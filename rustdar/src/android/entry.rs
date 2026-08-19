// Whole-file gate rather than per-item: everything here names winit's
// `AndroidApp`, which only exists on the android target -- the host
// `jni-typecheck` builds compile every sibling module but not this one.
#![cfg(target_os = "android")]

//! `android_main`: the entry point NativeActivity dlsym()s out of
//! `librustdar_native.so`, and all the wiring it installs.

use super::back::{set_event_loop_proxy, take_back_press};
use super::compass::{COMPASS_CLASS, start_compass_thread};
use super::density::get_display_density;
use super::insets::get_system_insets;
use super::location::{
    LOCATION_CLASS, location_active, request_location, start_location_thread, stop_location,
};
use super::permissions::location_permission_status;
use super::task_to_back::move_task_to_back;
use super::theme::detect_dark_theme;
use super::{JavaContext, register_java_helper, set_java_context};
use winit::platform::android::activity::AndroidApp;

/// Android main entry point
///
/// This function is called by the Android runtime when the app starts.
/// It initializes logging and starts the main application loop.
#[unsafe(no_mangle)]
#[allow(unsafe_code, reason = "the C ABI symbol NativeActivity dlsym()s")]
fn android_main(app: AndroidApp) {
    use winit::platform::android::EventLoopBuilderExtAndroid;

    // Initialize Android logging
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_tag("rustdar"),
    );

    log::info!("Starting Rustdar Platform (Android)");

    // Install the rustls crypto provider before anything can open a socket.
    // Belt-and-braces -- every client constructor calls this too (see
    // `rustdar_radar::tls`) -- but doing it first here keeps the choice of
    // provider at a predictable point rather than leaving it to whichever
    // background thread fetches first.
    rustdar_frontend::tls::init();

    // Initialize rustls-platform-verifier for TLS certificate verification.
    // reqwest uses it to reach Android's TrustManager over JNI; without this,
    // every HTTPS connection fails.
    //
    // `init_with_env` derives the class loader itself, from
    // `Context.getClassLoader()`. Under the old cargo-apk build that loader was
    // useless: cargo-apk emitted a purely native APK with no classes.dex, so the
    // app loader had never heard of the verifier's Kotlin classes, and this
    // function had to hand-roll a `PathClassLoader` over the APK's own sourceDir
    // and hand it to `init_with_refs`. The Gradle build packages a real DEX
    // (`android:hasCode="true"`), so the app loader is now the correct one and
    // that whole workaround is gone.
    #[allow(
        unsafe_code,
        reason = "wrapping the raw JavaVM/Activity pointers android-activity hands us"
    )]
    {
        use jni::objects::JObject;
        use jni::vm::JavaVM;
        use jni::{jni_sig, jni_str};

        // jni 0.22: `from_raw` is infallible and registers the process-wide
        // `JavaVM` singleton; the environment is only handed out as a `&mut Env`
        // borrowed for the body of the attachment closure.
        let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr() as *mut jni::sys::JavaVM) };
        let activity_ptr = app.activity_as_ptr() as jni::sys::jobject;

        vm.attach_current_thread(|env| -> jni::errors::Result<()> {
            // Two handles onto the same jobject. `JObject::from_raw` only wraps
            // the pointer -- it takes no ownership and deletes no local reference
            // when dropped -- and `init_with_env` consumes the handle it is
            // passed, so the second is what the helper registrations below use to
            // reach the Activity.
            let context = unsafe { JObject::from_raw(env, activity_ptr) };
            let activity = unsafe { JObject::from_raw(env, activity_ptr) };

            // Stash the Activity for the JNI helpers above, *before* anything
            // that can fail: this is the only point in this `android_main`
            // where the real Activity is available. `ndk_context` cannot substitute for
            // it -- android-activity 0.6 registers `Activity.getApplication()`
            // there, and `getWindow` / `moveTaskToBack` / `requestPermissions`
            // do not exist on `Application`. See [`JAVA`].
            match env.new_global_ref(&activity) {
                Ok(global) => {
                    // `JavaVM` is a handle onto a process-wide singleton, so the
                    // clone is the same VM this closure is attached through.
                    //
                    // An *overwrite*, not a first write, for the same reason as
                    // `set_event_loop_proxy` below: one `android_main` per
                    // Activity instance, and the context a previous instance
                    // left behind both points every helper at a destroyed
                    // Activity and pins that Activity in memory. See [`JAVA`].
                    set_java_context(Some(JavaContext {
                        vm: vm.clone(),
                        activity: global,
                    }));
                }
                // Not fatal on its own -- TLS and the event loop still work --
                // but insets, back-to-minimise and the location permission
                // prompt are all gone, so say which.
                Err(e) => log::error!(
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
            // NativeActivity calls finish() and the process dies. The helper
            // registers a callback that hands the press to
            // `Java_com_rustdar_BackHandler_nativeBackPressed` instead of
            // deciding anything itself. Registered here, before the event loop
            // exists, so presses in that window take the callback's own
            // minimise — see `post_back_press`.
            register_java_helper(env, &loader, &activity, "com.rustdar.BackHandler");

            if let Some(cls) =
                register_java_helper(env, &loader, &activity, "com.rustdar.CompassHelper")
            {
                let _ = COMPASS_CLASS.set(cls);
            }

            // Holds the location-update subscription that keeps the providers
            // producing fixes for the gps-location thread's poll. Registered
            // here; actually *started* later, by the permission gate, once it
            // sees the runtime permission granted. See `start_location_updates`
            // and `request_location`.
            if let Some(cls) =
                register_java_helper(env, &loader, &activity, "com.rustdar.LocationHelper")
            {
                let _ = LOCATION_CLASS.set(cls);
            }
            Ok(())
        })
        .expect("Failed to attach JNI thread");
    }

    // Derive the Android cache directory for zone geometry caching.
    // internal_data_path() returns .../files; its parent is the app root,
    // so parent/cache gives us getCacheDir() which shows as clearable
    // "Cache" in Android app settings.
    let android_zone_cache = app
        .internal_data_path()
        .and_then(|p| p.parent().map(|root| root.join("cache").join("zones")));

    // Derive the config directory before `app` is moved into the event loop.
    let android_config_dir = app.internal_data_path().map(|p| p.join("config"));

    // Create event loop with Android app
    let event_loop = winit::event_loop::EventLoop::builder()
        .with_android_app(app)
        .build()
        .expect("Failed to create event loop");

    // The predictive-back callback runs on the UI thread and cannot touch the
    // App; this is how it reaches the loop that can. Installed before
    // `run_app` — until it is, `post_back_press` reports `false` and the Java
    // side minimises for itself.
    //
    // An *overwrite*, not a first write: this function runs once per Activity
    // instance, not once per process, and the proxy left behind by a previous
    // instance is attached to an `EventLoop` that `run_app` has already
    // consumed. See [`EVENT_LOOP_PROXY`].
    set_event_loop_proxy(Some(event_loop.create_proxy()));

    // Create and run the platform app
    let mut platform_app =
        rustdar_frontend::app::App::new(Box::new(crate::platform::create_platform()));

    // Wire up Android back button to minimize instead of exit
    platform_app.set_back_handler(move_task_to_back);

    // ...and the other half of it: where a press that came in through
    // OnBackInvokedDispatcher rather than the input queue is collected.
    platform_app.set_back_press_taker(take_back_press);

    // Set zone geometry cache directory for persistent caching
    if let Some(cache_path) = android_zone_cache {
        platform_app.set_zone_cache_dir(cache_path);
    }

    // Set config directory for UI config persistence on Android.
    if let Some(config_path) = android_config_dir {
        platform_app.set_config_dir(config_path);
    }

    // Set up a callback to query system bar insets when the window is ready.
    // getRootWindowInsets() can return null before the first layout, so we
    // defer the query to the first resumed() call via a callback.
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

    // Hand the bridge the JNI theme read. NativeActivity never emits
    // WindowEvent::ThemeChanged, so the bridge polls this to notice a
    // light/dark switch; it also answers the initial query on the first frame.
    platform_app.set_theme_detector(detect_dark_theme);

    // ...and the four location calls, for the same reason and by the same
    // inversion. Since the fold, callee and caller share this crate -- the
    // injection survives ON PURPOSE: it is the frontend portability contract,
    // not a crate-graph workaround (the rule, in full, is in this module
    // tree's doc -- see `android/mod.rs`; do not "simplify" these into direct
    // calls from AndroidPlatform).
    //
    // All four at once, deliberately: a half-installed set has no symptom. A
    // bridge with `query` and no `request` reports `Prompt` forever and never
    // asks, which is indistinguishable from a user who has not been asked yet.
    //
    // Before `run_app`, so the window in which `AndroidPlatform` answers
    // `Unavailable` for want of hooks closes before the first frame -- that
    // answer is terminal, and a gate that saw it would stop polling for good.
    platform_app.set_location_hooks(rustdar_frontend::platform::LocationHooks {
        query: location_permission_status,
        request: request_location,
        stop: stop_location,
        active: location_active,
    });

    // Both threads below ask for the frame their value will be read on through
    // a handle that is *empty right now* -- the window does not exist until the
    // first `resumed()`, which is inside `run_app`. That is the whole reason it
    // is a slot they share rather than a window handle each takes a copy of;
    // see `rustdar_frontend::platform::RedrawWaker`.

    // Start GPS location polling thread and wire it to the app
    let (location_sender, location_receiver) = std::sync::mpsc::channel();
    let location_waker = platform_app.redraw_waker();
    start_location_thread(location_sender, move || location_waker.wake());
    platform_app.set_gps_fix_receiver(location_receiver);

    // Start compass heading thread and wire it to the app
    let (heading_sender, heading_receiver) = std::sync::mpsc::channel();
    let heading_waker = platform_app.redraw_waker();
    start_compass_thread(heading_sender, move || heading_waker.wake());
    platform_app.set_heading_receiver(heading_receiver);

    if let Err(e) = event_loop.run_app(&mut platform_app) {
        log::error!("Application error: {}", e);
    }

    // `run_app` consumed the loop, so the proxy above is now a corpse whose
    // `send_event` can only fail. Dropping it means a back press between here
    // and the next `android_main` reports "no loop installed" and minimises,
    // rather than looking like a delivery failure. Usually unreachable —
    // `needs_process_exit` takes Android out through `process::exit` — but this
    // is where a plain Activity teardown arrives.
    set_event_loop_proxy(None);

    // Same for the Activity context: from here until the next `android_main`
    // the object behind [`JAVA`] is a destroyed Activity, so let the global
    // ref go and let the helpers degrade to their documented defaults instead
    // of calling into the corpse.
    set_java_context(None);
}
