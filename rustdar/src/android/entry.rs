// Whole-file gate: everything here names winit's `AndroidApp`, which only
// exists on the android target.
#![cfg(target_os = "android")]

//! `android_main`: the entry point NativeActivity dlsym()s out of
//! `librustdar_native.so`, and all the wiring it installs.

use super::back::{set_event_loop_proxy, take_back_press};
use super::compass::{COMPASS_CLASS, start_compass_thread};
use super::density::get_display_density;
use super::insets::get_system_insets;
use super::task_to_back::move_task_to_back;
use super::theme::detect_dark_theme;
use super::{JavaContext, register_java_helper, set_java_context};
use winit::platform::android::activity::AndroidApp;

#[unsafe(no_mangle)]
#[allow(unsafe_code, reason = "the C ABI symbol NativeActivity dlsym()s")]
fn android_main(app: AndroidApp) {
    use winit::platform::android::EventLoopBuilderExtAndroid;

    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_tag("rustdar"),
    );

    log::info!("Starting Rustdar Platform (Android)");

    // Install the rustls crypto provider before anything can open a socket.
    // Redundant, but it keeps the choice at a predictable point.
    rustdar_app::tls::init();

    // rustls-platform-verifier reaches Android's TrustManager over JNI; without
    // this, every HTTPS connection fails. `init_with_env` derives the class
    // loader from `Context.getClassLoader()`.
    #[allow(
        unsafe_code,
        reason = "wrapping the raw JavaVM/Activity pointers android-activity hands us"
    )]
    {
        use jni::objects::JObject;
        use jni::vm::JavaVM;
        use jni::{jni_sig, jni_str};

        // jni 0.22: `from_raw` is infallible and registers the process-wide
        // `JavaVM` singleton.
        let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr() as *mut jni::sys::JavaVM) };
        let activity_ptr = app.activity_as_ptr() as jni::sys::jobject;

        vm.attach_current_thread(|env| -> jni::errors::Result<()> {
            // Two handles onto the same jobject: `JObject::from_raw` wraps the
            // pointer without taking ownership, and `init_with_env` consumes one.
            let context = unsafe { JObject::from_raw(env, activity_ptr) };
            let activity = unsafe { JObject::from_raw(env, activity_ptr) };

            // Stash the Activity for the JNI helpers, *before* anything that can
            // fail: this is the only point where it is available. See [`JAVA`].
            // Two global refs, one per context; a `Global` is owned, not `Clone`.
            match (env.new_global_ref(&activity), env.new_global_ref(&activity)) {
                (Ok(global), Ok(location_global)) => {
                    // An *overwrite*, not a first write: one `android_main` per
                    // Activity, and a stale context points every helper at a
                    // destroyed Activity and pins it. See [`JAVA`].
                    set_java_context(Some(JavaContext {
                        vm: vm.clone(),
                        activity: global,
                    }));
                    // The facade's JNI arm registers com.rustdar.LocationHelper.
                    rustdar_location::android::init(vm.clone(), location_global);
                }
                // Not fatal on its own, but insets, back-to-minimise and the
                // location prompt are all gone.
                (Err(e), _) | (_, Err(e)) => log::error!(
                    "Could not take a global ref to the Activity ({e:?}); \
                     window insets, back-to-minimise and the GPS permission \
                     prompt will be unavailable"
                ),
            }

            rustls_platform_verifier::android::init_with_env(env, context)
                .expect("Failed to initialize rustls-platform-verifier");
            log::info!("rustls-platform-verifier initialized");

            let loader = env
                .call_method(
                    &activity,
                    jni_str!("getClassLoader"),
                    jni_sig!("()Ljava/lang/ClassLoader;"),
                    &[],
                )
                .and_then(|v| v.l())
                .expect("Context.getClassLoader() failed");

            // Predictive back. Once the app opts in, back bypasses the native
            // input queue and goes through OnBackInvokedDispatcher; unhandled,
            // NativeActivity calls finish() and the process dies. Registered
            // before the event loop exists, so presses in that window take the
            // callback's own minimise.
            register_java_helper(env, &loader, &activity, "com.rustdar.BackHandler");

            if let Some(cls) =
                register_java_helper(env, &loader, &activity, "com.rustdar.CompassHelper")
            {
                let _ = COMPASS_CLASS.set(cls);
            }

            Ok(())
        })
        .expect("Failed to attach JNI thread");
    }

    // internal_data_path() returns .../files; its parent is the app root, so
    // parent/cache gives getCacheDir(), which shows as clearable "Cache".
    let android_zone_cache = app
        .internal_data_path()
        .and_then(|p| p.parent().map(|root| root.join("cache").join("zones")));

    let android_config_dir = app.internal_data_path().map(|p| p.join("config"));

    let event_loop = winit::event_loop::EventLoop::builder()
        .with_android_app(app)
        .build()
        .expect("Failed to create event loop");

    // The predictive-back callback runs on the UI thread and cannot touch the
    // App; this is how it reaches the loop that can. Installed before
    // `run_app` — until it is, `post_back_press` reports `false`. An
    // *overwrite*: a previous proxy is attached to a consumed `EventLoop`.
    set_event_loop_proxy(Some(event_loop.create_proxy()));

    let mut platform_app = rustdar_app::app::App::new(
        Box::new(crate::platform::create_platform()),
        crate::platform::create_location(),
    );

    platform_app.set_back_handler(move_task_to_back);

    // ...and where a press from OnBackInvokedDispatcher is collected.
    platform_app.set_back_press_taker(take_back_press);

    if let Some(cache_path) = android_zone_cache {
        platform_app.set_zone_cache_dir(cache_path);
    }

    if let Some(config_path) = android_config_dir {
        platform_app.set_config_dir(config_path);
    }

    // getRootWindowInsets() can return null before the first layout, so the
    // query is deferred to the first resumed() call via a callback.
    platform_app.set_insets_querier(|| {
        let (top, bottom, left, right) = get_system_insets();
        let density = get_display_density();
        let density = if density > 0.0 { density } else { 1.0 };
        let result = (
            top / density,
            bottom / density,
            left / density,
            right / density,
        );
        log::info!(
            "Safe area insets (logical): top={}, bottom={}, left={}, right={}",
            result.0,
            result.1,
            result.2,
            result.3
        );
        result
    });

    // NativeActivity never emits WindowEvent::ThemeChanged, so the bridge polls.
    platform_app.set_theme_detector(detect_dark_theme);

    // The compass thread asks for the frame its value will be read on through a
    // handle that is *empty right now* — no window until the first `resumed()`.

    let (heading_sender, heading_receiver) = std::sync::mpsc::channel();
    let heading_waker = platform_app.redraw_waker();
    start_compass_thread(heading_sender, move || heading_waker.wake());
    platform_app.set_heading_receiver(heading_receiver);

    if let Err(e) = event_loop.run_app(&mut platform_app) {
        log::error!("Application error: {}", e);
    }

    // `run_app` consumed the loop, so the proxy above is a corpse. Dropping it
    // means a back press before the next `android_main` minimises rather than
    // looking like a delivery failure.
    set_event_loop_proxy(None);

    // Same for the Activity context: the object behind [`JAVA`] is a destroyed
    // Activity from here, so let the global ref go and let the helpers degrade.
    set_java_context(None);
    rustdar_location::android::deinit();
}
