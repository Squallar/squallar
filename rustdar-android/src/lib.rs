#![cfg(target_os = "android")]

//! Android entry point for Rustdar Platform
//! 
//! This crate provides the Android-specific entry point and configuration
//! for the Rustdar radar visualization application.

#[cfg(target_os = "android")]
use winit::platform::android::activity::AndroidApp;

// ---------------------------------------------------------------------------
// JNI location helper functions (Android only)
// ---------------------------------------------------------------------------

/// Check whether the app has been granted ACCESS_FINE_LOCATION.
#[cfg(target_os = "android")]
fn has_location_permission() -> bool {
    use jni::objects::JObject;
    use jni::JavaVM;

    let ctx = ndk_context::android_context();
    let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) };
    let Ok(vm) = vm else { return false };
    let Ok(mut env) = vm.attach_current_thread() else { return false };
    let activity = unsafe { JObject::from_raw(ctx.context().cast()) };

    let Ok(perm) = env.new_string("android.permission.ACCESS_FINE_LOCATION") else { return false };
    let result = env.call_method(
        &activity,
        "checkSelfPermission",
        "(Ljava/lang/String;)I",
        &[jni::objects::JValue::from(&perm)],
    );
    match result {
        Ok(val) => val.i().unwrap_or(-1) == 0, // PERMISSION_GRANTED == 0
        Err(_) => false,
    }
}

/// Request the ACCESS_FINE_LOCATION runtime permission.
/// This shows the system permission dialog. The result is asynchronous;
/// poll `has_location_permission()` afterwards to check the outcome.
#[cfg(target_os = "android")]
fn request_location_permission() {
    use jni::objects::{JObject, JValue};
    use jni::JavaVM;

    let ctx = ndk_context::android_context();
    let Ok(vm) = (unsafe { JavaVM::from_raw(ctx.vm().cast()) }) else { return };
    let Ok(mut env) = vm.attach_current_thread() else { return };
    let activity = unsafe { JObject::from_raw(ctx.context().cast()) };

    // requestPermissions() is Activity-only; context may be Application after resume.
    let Ok(activity_class) = env.find_class("android/app/Activity") else { return };
    if !env.is_instance_of(&activity, activity_class).unwrap_or(false) { return }

    let Ok(perm_str) = env.new_string("android.permission.ACCESS_FINE_LOCATION") else { return };
    let Ok(string_class) = env.find_class("java/lang/String") else { return };
    let Ok(perm_array) = env.new_object_array(1, &string_class, &perm_str) else { return };

    let _ = env.call_method(
        &activity,
        "requestPermissions",
        "([Ljava/lang/String;I)V",
        &[JValue::from(&perm_array), JValue::from(1i32)],
    );
}

/// Try to retrieve the device's last known GPS location via `LocationManager`.
/// Returns a [`GpsFix`] on success or `None` if unavailable.
#[cfg(target_os = "android")]
fn get_last_known_location() -> Option<rustdar_gps::GpsFix> {
    use jni::objects::{JObject, JValue};
    use jni::JavaVM;

    let ctx = ndk_context::android_context();
    let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) }.ok()?;
    let mut env = vm.attach_current_thread().ok()?;
    let activity = unsafe { JObject::from_raw(ctx.context().cast()) };

    // LocationManager lm = context.getSystemService("location");
    let service_name = env.new_string("location").ok()?;
    let lm = env
        .call_method(
            &activity,
            "getSystemService",
            "(Ljava/lang/String;)Ljava/lang/Object;",
            &[JValue::from(&service_name)],
        )
        .ok()?
        .l()
        .ok()?;
    if lm.is_null() {
        return None;
    }

    // Try GPS first, then network provider as fallback
    for provider in &["gps", "network"] {
        let provider_str = env.new_string(provider).ok()?;
        let location = env.call_method(
            &lm,
            "getLastKnownLocation",
            "(Ljava/lang/String;)Landroid/location/Location;",
            &[JValue::from(&provider_str)],
        );
        // getLastKnownLocation throws SecurityException without permission
        let location = match location {
            Ok(val) => val.l().ok()?,
            Err(_) => continue,
        };
        if location.is_null() {
            continue;
        }

        let lat = env
            .call_method(&location, "getLatitude", "()D", &[])
            .ok()?
            .d()
            .ok()?;
        let lon = env
            .call_method(&location, "getLongitude", "()D", &[])
            .ok()?
            .d()
            .ok()?;

        // Sanity check – (0, 0) is almost certainly a default/invalid value
        if lat.abs() < 0.001 && lon.abs() < 0.001 {
            continue;
        }

        // Extract extended fix data
        let altitude_m = env
            .call_method(&location, "getAltitude", "()D", &[])
            .and_then(|v| v.d())
            .ok()
            .filter(|_| {
                env.call_method(&location, "hasAltitude", "()Z", &[])
                    .and_then(|v| v.z())
                    .unwrap_or(false)
            });

        let speed_mps = env
            .call_method(&location, "getSpeed", "()F", &[])
            .and_then(|v| v.f())
            .ok()
            .filter(|_| {
                env.call_method(&location, "hasSpeed", "()Z", &[])
                    .and_then(|v| v.z())
                    .unwrap_or(false)
            })
            .map(|s| s as f64);

        let heading_deg = env
            .call_method(&location, "getBearing", "()F", &[])
            .and_then(|v| v.f())
            .ok()
            .filter(|_| {
                env.call_method(&location, "hasBearing", "()Z", &[])
                    .and_then(|v| v.z())
                    .unwrap_or(false)
            })
            .map(|b| b as f64);

        let fix_quality = if *provider == "gps" {
            rustdar_gps::FixQuality::Gps
        } else {
            rustdar_gps::FixQuality::Estimated
        };

        return Some(rustdar_gps::GpsFix {
            latitude: lat,
            longitude: lon,
            altitude_m,
            speed_mps,
            heading_deg,
            satellites: None, // Not available from getLastKnownLocation
            fix_quality,
            hdop: None,
            timestamp: None,
        });
    }
    None
}

/// Start a background thread that polls GPS location and sends updates
/// through the provided channel. Also handles permission requests.
#[cfg(target_os = "android")]
fn start_location_thread(sender: std::sync::mpsc::Sender<rustdar_gps::GpsFix>) {
    std::thread::Builder::new()
        .name("gps-location".into())
        .spawn(move || {
        // Let the app fully initialise before doing JNI work
        std::thread::sleep(std::time::Duration::from_secs(3));

        let mut permission_requested = false;

        loop {
            if has_location_permission() {
                if let Some(fix) = get_last_known_location() {
                    if sender.send(fix).is_err() {
                        break; // channel closed
                    }
                }
            } else if !permission_requested {
                log::info!("Requesting ACCESS_FINE_LOCATION permission");
                request_location_permission();
                permission_requested = true;
            }

            std::thread::sleep(std::time::Duration::from_secs(10));
        }
    }).expect("failed to spawn gps-location thread");
}

/// Query the system window insets (status bar, navigation bar) in physical pixels.
/// Returns (top, bottom, left, right) inset values.
#[cfg(target_os = "android")]
pub fn get_system_insets() -> (f32, f32, f32, f32) {
    use jni::objects::{JObject, JValue};
    use jni::JavaVM;

    let ctx = ndk_context::android_context();
    let Ok(vm) = (unsafe { JavaVM::from_raw(ctx.vm().cast()) }) else {
        return (0.0, 0.0, 0.0, 0.0);
    };
    let Ok(mut env) = vm.attach_current_thread() else {
        return (0.0, 0.0, 0.0, 0.0);
    };
    let activity = unsafe { JObject::from_raw(ctx.context().cast()) };

    // After suspend/resume, ndk_context may return the Application instead of
    // the Activity. getWindow() only exists on Activity, so bail out early.
    let Ok(activity_class) = env.find_class("android/app/Activity") else {
        return (0.0, 0.0, 0.0, 0.0);
    };
    if !env.is_instance_of(&activity, activity_class).unwrap_or(false) {
        log::warn!("get_system_insets: context is not an Activity, skipping");
        return (0.0, 0.0, 0.0, 0.0);
    }

    // Activity.getWindow().getDecorView().getRootWindowInsets()
    let window = match env.call_method(&activity, "getWindow", "()Landroid/view/Window;", &[]) {
        Ok(w) => match w.l() { Ok(w) => w, Err(_) => return (0.0, 0.0, 0.0, 0.0) },
        Err(_) => return (0.0, 0.0, 0.0, 0.0),
    };
    let decor = match env.call_method(&window, "getDecorView", "()Landroid/view/View;", &[]) {
        Ok(v) => match v.l() { Ok(v) => v, Err(_) => return (0.0, 0.0, 0.0, 0.0) },
        Err(_) => return (0.0, 0.0, 0.0, 0.0),
    };
    let insets_obj = match env.call_method(
        &decor, "getRootWindowInsets", "()Landroid/view/WindowInsets;", &[]
    ) {
        Ok(i) => match i.l() {
            Ok(i) if !i.is_null() => i,
            _ => return (0.0, 0.0, 0.0, 0.0),
        },
        Err(_) => return (0.0, 0.0, 0.0, 0.0),
    };

    // On API 30+, use getInsets(WindowInsets.Type.systemBars())
    // On older APIs, use getSystemWindowInset*()
    let (top, bottom, left, right) = if android_api_level() >= 30 {
        // WindowInsets.Type.systemBars() returns a bitmask
        let type_class = match env.find_class("android/view/WindowInsets$Type") {
            Ok(c) => c,
            Err(_) => return get_legacy_insets(&mut env, &insets_obj),
        };
        let type_mask = match env.call_static_method(type_class, "systemBars", "()I", &[]) {
            Ok(v) => match v.i() { Ok(v) => v, Err(_) => return get_legacy_insets(&mut env, &insets_obj) },
            Err(_) => return get_legacy_insets(&mut env, &insets_obj),
        };
        let insets_result = env.call_method(
            &insets_obj, "getInsets", "(I)Landroid/graphics/Insets;",
            &[JValue::from(type_mask)],
        );
        match insets_result {
            Ok(val) => {
                let insets = match val.l() {
                    Ok(i) if !i.is_null() => i,
                    _ => return get_legacy_insets(&mut env, &insets_obj),
                };
                let t = env.get_field(&insets, "top", "I").map(|v| v.i().unwrap_or(0)).unwrap_or(0);
                let b = env.get_field(&insets, "bottom", "I").map(|v| v.i().unwrap_or(0)).unwrap_or(0);
                let l = env.get_field(&insets, "left", "I").map(|v| v.i().unwrap_or(0)).unwrap_or(0);
                let r = env.get_field(&insets, "right", "I").map(|v| v.i().unwrap_or(0)).unwrap_or(0);
                (t as f32, b as f32, l as f32, r as f32)
            }
            Err(_) => get_legacy_insets(&mut env, &insets_obj),
        }
    } else {
        get_legacy_insets(&mut env, &insets_obj)
    };

    (top, bottom, left, right)
}

/// Fallback for Android < API 30: use deprecated getSystemWindowInset*() methods.
#[cfg(target_os = "android")]
fn get_legacy_insets(env: &mut jni::AttachGuard<'_>, insets_obj: &jni::objects::JObject<'_>) -> (f32, f32, f32, f32) {
    let top = env.call_method(insets_obj, "getSystemWindowInsetTop", "()I", &[])
        .map(|v| v.i().unwrap_or(0)).unwrap_or(0);
    let bottom = env.call_method(insets_obj, "getSystemWindowInsetBottom", "()I", &[])
        .map(|v| v.i().unwrap_or(0)).unwrap_or(0);
    let left = env.call_method(insets_obj, "getSystemWindowInsetLeft", "()I", &[])
        .map(|v| v.i().unwrap_or(0)).unwrap_or(0);
    let right = env.call_method(insets_obj, "getSystemWindowInsetRight", "()I", &[])
        .map(|v| v.i().unwrap_or(0)).unwrap_or(0);
    (top as f32, bottom as f32, left as f32, right as f32)
}

/// Get the Android API level.
#[cfg(target_os = "android")]
fn android_api_level() -> i32 {
    use jni::JavaVM;
    let ctx = ndk_context::android_context();
    let Ok(vm) = (unsafe { JavaVM::from_raw(ctx.vm().cast()) }) else { return 0 };
    let Ok(mut env) = vm.attach_current_thread() else { return 0 };
    let Ok(build_class) = env.find_class("android/os/Build$VERSION") else { return 0 };
    env.get_static_field(build_class, "SDK_INT", "I")
        .map(|v| v.i().unwrap_or(0))
        .unwrap_or(0)
}

/// Get the display density (pixels per dp) for converting physical to logical pixels.
#[cfg(target_os = "android")]
fn get_display_density() -> f32 {
    use jni::objects::JObject;
    use jni::JavaVM;

    let ctx = ndk_context::android_context();
    let Ok(vm) = (unsafe { JavaVM::from_raw(ctx.vm().cast()) }) else { return 1.0 };
    let Ok(mut env) = vm.attach_current_thread() else { return 1.0 };
    let activity = unsafe { JObject::from_raw(ctx.context().cast()) };

    // activity.getResources().getDisplayMetrics().density
    let resources = match env.call_method(&activity, "getResources", "()Landroid/content/res/Resources;", &[]) {
        Ok(r) => match r.l() { Ok(r) => r, Err(_) => return 1.0 },
        Err(_) => return 1.0,
    };
    let metrics = match env.call_method(&resources, "getDisplayMetrics", "()Landroid/util/DisplayMetrics;", &[]) {
        Ok(m) => match m.l() { Ok(m) => m, Err(_) => return 1.0 },
        Err(_) => return 1.0,
    };
    env.get_field(&metrics, "density", "F")
        .map(|v| v.f().unwrap_or(1.0))
        .unwrap_or(1.0)
}

/// Minimize the app by calling Activity.moveTaskToBack(true) via JNI.
/// This keeps the app alive in recents with a proper thumbnail instead
/// of killing the process (which leaves a white box in recents).
#[cfg(target_os = "android")]
pub fn move_task_to_back() {
    use jni::objects::JObject;
    use jni::JavaVM;

    let ctx = ndk_context::android_context();
    let Ok(vm) = (unsafe { JavaVM::from_raw(ctx.vm().cast()) }) else {
        log::warn!("moveTaskToBack: failed to get JavaVM");
        return;
    };
    let Ok(mut env) = vm.attach_current_thread() else {
        log::warn!("moveTaskToBack: failed to attach JNI thread");
        return;
    };
    let activity = unsafe { JObject::from_raw(ctx.context().cast()) };

    // moveTaskToBack() is Activity-only; context may be Application after resume.
    let Ok(activity_class) = env.find_class("android/app/Activity") else { return };
    if !env.is_instance_of(&activity, activity_class).unwrap_or(false) {
        log::warn!("moveTaskToBack: context is not an Activity, skipping");
        return;
    }

    match env.call_method(&activity, "moveTaskToBack", "(Z)Z", &[jni::objects::JValue::from(true)]) {
        Ok(_) => log::info!("App moved to background"),
        Err(e) => log::warn!("moveTaskToBack failed: {:?}", e),
    }
}

// ---------------------------------------------------------------------------
// Compass heading via JNI (CompassHelper.java)
// ---------------------------------------------------------------------------

/// JClass for com.rustdar.CompassHelper, loaded once via PathClassLoader.
#[cfg(target_os = "android")]
static COMPASS_CLASS: std::sync::OnceLock<jni::objects::GlobalRef> = std::sync::OnceLock::new();

/// Read the current compass heading from CompassHelper.getHeading().
/// Returns `None` if the class wasn't loaded or no reading is available yet.
#[cfg(target_os = "android")]
fn get_compass_heading() -> Option<f32> {
    use jni::JavaVM;

    let global_ref = COMPASS_CLASS.get()?;
    let ctx = ndk_context::android_context();
    let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) };
    let Ok(vm) = vm else { return None };
    let Ok(mut env) = vm.attach_current_thread() else { return None };

    let cls = jni::objects::JClass::from(unsafe {
        jni::objects::JObject::from_raw(global_ref.as_obj().as_raw())
    });
    let heading = env.call_static_method(cls, "getHeading", "()F", &[])
        .ok()?
        .f()
        .ok()?;
    if heading < 0.0 {
        None // -1 means no reading yet
    } else {
        Some(heading)
    }
}

/// Start a background thread that polls the compass heading every 200ms and
/// sends updates through the provided channel.
#[cfg(target_os = "android")]
fn start_compass_thread(sender: std::sync::mpsc::Sender<f32>) {
    std::thread::Builder::new()
        .name("compass-heading".into())
        .spawn(move || {
        // Wait for CompassHelper to be initialized
        std::thread::sleep(std::time::Duration::from_secs(4));

        loop {
            if let Some(heading) = get_compass_heading() {
                if sender.send(heading).is_err() {
                    break; // channel closed
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }).expect("failed to spawn compass-heading thread");
}

/// Load one of our own Java helper classes through the app's [`ClassLoader`] and
/// invoke its static `register(Activity)`.
///
/// Returns a global ref to the loaded class so the caller can keep calling static
/// methods on it later (see [`COMPASS_CLASS`]).
///
/// The class *must* be resolved through `loader` rather than `JNIEnv::find_class`.
/// `android_main` runs on a thread that `android-activity` attached to the JVM
/// with no Java frames on the stack, and in that situation JNI `FindClass`
/// resolves against the *system* class loader — which knows nothing about classes
/// packaged in the app. `Context.getClassLoader()` is the app loader and does.
///
/// [`ClassLoader`]: <https://developer.android.com/reference/java/lang/ClassLoader>
#[cfg(target_os = "android")]
fn register_java_helper(
    env: &mut jni::AttachGuard<'_>,
    loader: &jni::objects::JObject<'_>,
    activity: &jni::objects::JObject<'_>,
    class_name: &str,
) -> Option<jni::objects::GlobalRef> {
    use jni::objects::{JClass, JValue};

    let name = env.new_string(class_name).ok()?;
    let cls_obj = env
        .call_method(
            loader,
            "loadClass",
            "(Ljava/lang/String;)Ljava/lang/Class;",
            &[JValue::from(&name)],
        )
        .and_then(|v| v.l())
        .inspect_err(|e| log::warn!("Could not load {}: {:?}", class_name, e))
        .ok()?;

    // Taken before the value is consumed by the JClass conversion below.
    let global = env.new_global_ref(&cls_obj).ok();

    match env.call_static_method(
        JClass::from(cls_obj),
        "register",
        "(Landroid/app/Activity;)V",
        &[JValue::from(activity)],
    ) {
        Ok(_) => {
            log::info!("{} registered", class_name);
            global
        }
        Err(e) => {
            log::warn!("{}.register() failed: {:?}", class_name, e);
            None
        }
    }
}

/// Android main entry point
///
/// This function is called by the Android runtime when the app starts.
/// It initializes logging and starts the main application loop.
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
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
    {
        use jni::JavaVM;
        use jni::objects::JObject;

        let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr() as *mut jni::sys::JavaVM) }
            .expect("Failed to get JavaVM");
        let mut env = vm
            .attach_current_thread()
            .expect("Failed to attach JNI thread");

        // Two handles onto the same jobject. `JObject::from_raw` only wraps the
        // pointer -- it takes no ownership and deletes no local reference when
        // dropped -- and `init_with_env` consumes the handle it is passed, so the
        // second is what the helper registrations below use to reach the Activity.
        let context = unsafe { JObject::from_raw(app.activity_as_ptr() as jni::sys::jobject) };
        let activity = unsafe { JObject::from_raw(app.activity_as_ptr() as jni::sys::jobject) };

        rustls_platform_verifier::android::init_with_env(&mut env, context)
            .expect("Failed to initialize rustls-platform-verifier");
        log::info!("rustls-platform-verifier initialized");

        let loader = env
            .call_method(&activity, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])
            .and_then(|v| v.l())
            .expect("Context.getClassLoader() failed");

        // Back gesture on API 33+: back bypasses the native input queue and goes
        // through OnBackInvokedDispatcher. Unhandled, NativeActivity calls
        // finish() and the process dies; the helper minimises instead.
        register_java_helper(&mut env, &loader, &activity, "com.rustdar.BackHandler");

        if let Some(cls) =
            register_java_helper(&mut env, &loader, &activity, "com.rustdar.CompassHelper")
        {
            let _ = COMPASS_CLASS.set(cls);
        }
    }

    // Derive the Android cache directory for zone geometry caching.
    // internal_data_path() returns .../files; its parent is the app root,
    // so parent/cache gives us getCacheDir() which shows as clearable
    // "Cache" in Android app settings.
    let android_zone_cache = app
        .internal_data_path()
        .and_then(|p| p.parent().map(|root| root.join("cache").join("zones")));

    // Derive the config directory before `app` is moved into the event loop.
    let android_config_dir = app
        .internal_data_path()
        .map(|p| p.join("config"));

    // Create event loop with Android app
    let event_loop = winit::event_loop::EventLoop::builder()
        .with_android_app(app)
        .build()
        .expect("Failed to create event loop");

    // Create and run the platform app
    let mut platform_app = rustdar_frontend::app::App::new(Box::new(
        rustdar_platform_lib::platform::create_platform(),
    ));

    // Wire up Android back button to minimize instead of exit
    platform_app.set_back_handler(move_task_to_back);

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
        let result = (top / density, bottom / density, left / density, right / density);
        log::info!("Safe area insets (logical): top={}, bottom={}, left={}, right={}", result.0, result.1, result.2, result.3);
        result
    });

    // Start GPS location polling thread and wire it to the app
    let (location_sender, location_receiver) = std::sync::mpsc::channel();
    start_location_thread(location_sender);
    platform_app.set_gps_fix_receiver(location_receiver);

    // Start compass heading thread and wire it to the app
    let (heading_sender, heading_receiver) = std::sync::mpsc::channel();
    start_compass_thread(heading_sender);
    platform_app.set_heading_receiver(heading_receiver);
    
    if let Err(e) = event_loop.run_app(&mut platform_app) {
        log::error!("Application error: {}", e);
    }
}