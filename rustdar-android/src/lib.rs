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

/// Wrap the process `JavaVM` that `ndk_context` holds.
///
/// jni 0.22 made `JavaVM::from_raw` infallible, but it *asserts* the pointer is
/// non-null where 0.21 returned a `Result`. Checking here keeps the pre-0.22
/// behaviour of falling back to a default instead of panicking if `ndk_context`
/// has not been initialised yet -- these helpers run on background threads where
/// a panic would silently take out GPS or compass polling.
#[cfg(target_os = "android")]
fn android_vm() -> Option<jni::vm::JavaVM> {
    let vm = ndk_context::android_context().vm();
    if vm.is_null() {
        return None;
    }
    // SAFETY: non-null, and `ndk_context` guarantees it is the process JavaVM.
    Some(unsafe { jni::vm::JavaVM::from_raw(vm.cast()) })
}

/// Check whether the app has been granted ACCESS_FINE_LOCATION.
#[cfg(target_os = "android")]
fn has_location_permission() -> bool {
    use jni::objects::{JObject, JValue};
    use jni::{jni_sig, jni_str};

    let ctx = ndk_context::android_context();
    let Some(vm) = android_vm() else { return false };
    let context = ctx.context();

    vm.attach_current_thread(|env| -> jni::errors::Result<bool> {
        let activity = unsafe { JObject::from_raw(env, context.cast()) };

        let perm = env.new_string("android.permission.ACCESS_FINE_LOCATION")?;
        let granted = env
            .call_method(
                &activity,
                jni_str!("checkSelfPermission"),
                jni_sig!("(Ljava/lang/String;)I"),
                &[JValue::from(&perm)],
            )?
            .i()?;
        Ok(granted == 0) // PERMISSION_GRANTED == 0
    })
    .unwrap_or(false)
}

/// Request the ACCESS_FINE_LOCATION runtime permission.
/// This shows the system permission dialog. The result is asynchronous;
/// poll `has_location_permission()` afterwards to check the outcome.
#[cfg(target_os = "android")]
fn request_location_permission() {
    use jni::objects::{JObject, JValue};
    use jni::{jni_sig, jni_str};

    let ctx = ndk_context::android_context();
    let Some(vm) = android_vm() else { return };
    let context = ctx.context();

    let _ = vm.attach_current_thread(|env| -> jni::errors::Result<()> {
        let activity = unsafe { JObject::from_raw(env, context.cast()) };

        // requestPermissions() is Activity-only; context may be Application after resume.
        let activity_class = env.find_class(jni_str!("android/app/Activity"))?;
        if !env.is_instance_of(&activity, &activity_class).unwrap_or(false) {
            return Ok(());
        }

        let perm_str = env.new_string("android.permission.ACCESS_FINE_LOCATION")?;
        let string_class = env.find_class(jni_str!("java/lang/String"))?;
        let perm_array = env.new_object_array(1, &string_class, &perm_str)?;

        let _ = env.call_method(
            &activity,
            jni_str!("requestPermissions"),
            jni_sig!("([Ljava/lang/String;I)V"),
            &[JValue::from(&perm_array), JValue::Int(1)],
        );
        Ok(())
    });
}

/// Try to retrieve the device's last known GPS location via `LocationManager`.
/// Returns a [`GpsFix`] on success or `None` if unavailable.
#[cfg(target_os = "android")]
fn get_last_known_location() -> Option<rustdar_gps::GpsFix> {
    let ctx = ndk_context::android_context();
    let vm = android_vm()?;
    let context = ctx.context();

    vm.attach_current_thread(|env| -> jni::errors::Result<Option<rustdar_gps::GpsFix>> {
        Ok(last_known_location_with(env, context))
    })
    .ok()
    .flatten()
}

/// Body of [`get_last_known_location`], split out so it can keep using `?` on
/// `Option` inside the `Env` closure that jni 0.22's attachment API requires.
#[cfg(target_os = "android")]
fn last_known_location_with(
    env: &mut jni::Env<'_>,
    context: *mut std::ffi::c_void,
) -> Option<rustdar_gps::GpsFix> {
    use jni::objects::{JObject, JValue};
    use jni::{jni_sig, jni_str};

    let activity = unsafe { JObject::from_raw(env, context.cast()) };

    // LocationManager lm = context.getSystemService("location");
    let service_name = env.new_string("location").ok()?;
    let lm = env
        .call_method(
            &activity,
            jni_str!("getSystemService"),
            jni_sig!("(Ljava/lang/String;)Ljava/lang/Object;"),
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
            jni_str!("getLastKnownLocation"),
            jni_sig!("(Ljava/lang/String;)Landroid/location/Location;"),
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
            .call_method(&location, jni_str!("getLatitude"), jni_sig!("()D"), &[])
            .ok()?
            .d()
            .ok()?;
        let lon = env
            .call_method(&location, jni_str!("getLongitude"), jni_sig!("()D"), &[])
            .ok()?
            .d()
            .ok()?;

        // Sanity check – (0, 0) is almost certainly a default/invalid value
        if lat.abs() < 0.001 && lon.abs() < 0.001 {
            continue;
        }

        // Extract extended fix data
        let altitude_m = env
            .call_method(&location, jni_str!("getAltitude"), jni_sig!("()D"), &[])
            .and_then(|v| v.d())
            .ok()
            .filter(|_| {
                env.call_method(&location, jni_str!("hasAltitude"), jni_sig!("()Z"), &[])
                    .and_then(|v| v.z())
                    .unwrap_or(false)
            });

        let speed_mps = env
            .call_method(&location, jni_str!("getSpeed"), jni_sig!("()F"), &[])
            .and_then(|v| v.f())
            .ok()
            .filter(|_| {
                env.call_method(&location, jni_str!("hasSpeed"), jni_sig!("()Z"), &[])
                    .and_then(|v| v.z())
                    .unwrap_or(false)
            })
            .map(|s| s as f64);

        let heading_deg = env
            .call_method(&location, jni_str!("getBearing"), jni_sig!("()F"), &[])
            .and_then(|v| v.f())
            .ok()
            .filter(|_| {
                env.call_method(&location, jni_str!("hasBearing"), jni_sig!("()Z"), &[])
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
                if let Some(fix) = get_last_known_location()
                    && sender.send(fix).is_err()
                {
                    break; // channel closed
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
    let ctx = ndk_context::android_context();
    let Some(vm) = android_vm() else {
        return (0.0, 0.0, 0.0, 0.0);
    };
    let context = ctx.context();

    vm.attach_current_thread(|env| -> jni::errors::Result<(f32, f32, f32, f32)> {
        Ok(system_insets_with(env, context))
    })
    .unwrap_or((0.0, 0.0, 0.0, 0.0))
}

/// Body of [`get_system_insets`], split out so it can `return` early from inside
/// the `Env` closure that jni 0.22's attachment API requires.
#[cfg(target_os = "android")]
fn system_insets_with(
    env: &mut jni::Env<'_>,
    context: *mut std::ffi::c_void,
) -> (f32, f32, f32, f32) {
    use jni::objects::{JObject, JValue};
    use jni::{jni_sig, jni_str};

    let activity = unsafe { JObject::from_raw(env, context.cast()) };

    // After suspend/resume, ndk_context may return the Application instead of
    // the Activity. getWindow() only exists on Activity, so bail out early.
    let Ok(activity_class) = env.find_class(jni_str!("android/app/Activity")) else {
        return (0.0, 0.0, 0.0, 0.0);
    };
    if !env.is_instance_of(&activity, &activity_class).unwrap_or(false) {
        log::warn!("get_system_insets: context is not an Activity, skipping");
        return (0.0, 0.0, 0.0, 0.0);
    }

    // Activity.getWindow().getDecorView().getRootWindowInsets()
    let window = match env.call_method(&activity, jni_str!("getWindow"), jni_sig!("()Landroid/view/Window;"), &[]) {
        Ok(w) => match w.l() { Ok(w) => w, Err(_) => return (0.0, 0.0, 0.0, 0.0) },
        Err(_) => return (0.0, 0.0, 0.0, 0.0),
    };
    let decor = match env.call_method(&window, jni_str!("getDecorView"), jni_sig!("()Landroid/view/View;"), &[]) {
        Ok(v) => match v.l() { Ok(v) => v, Err(_) => return (0.0, 0.0, 0.0, 0.0) },
        Err(_) => return (0.0, 0.0, 0.0, 0.0),
    };
    let insets_obj = match env.call_method(
        &decor, jni_str!("getRootWindowInsets"), jni_sig!("()Landroid/view/WindowInsets;"), &[]
    ) {
        Ok(i) => match i.l() {
            Ok(i) if !i.is_null() => i,
            _ => return (0.0, 0.0, 0.0, 0.0),
        },
        Err(_) => return (0.0, 0.0, 0.0, 0.0),
    };

    // On API 30+, use getInsets(WindowInsets.Type.systemBars())
    // On older APIs, use getSystemWindowInset*()
    let (top, bottom, left, right) = if android_api_level(env) >= 30 {
        // WindowInsets.Type.systemBars() returns a bitmask
        let type_class = match env.find_class(jni_str!("android/view/WindowInsets$Type")) {
            Ok(c) => c,
            Err(_) => return get_legacy_insets(env, &insets_obj),
        };
        let type_mask = match env.call_static_method(&type_class, jni_str!("systemBars"), jni_sig!("()I"), &[]) {
            Ok(v) => match v.i() { Ok(v) => v, Err(_) => return get_legacy_insets(env, &insets_obj) },
            Err(_) => return get_legacy_insets(env, &insets_obj),
        };
        let insets_result = env.call_method(
            &insets_obj, jni_str!("getInsets"), jni_sig!("(I)Landroid/graphics/Insets;"),
            &[JValue::Int(type_mask)],
        );
        match insets_result {
            Ok(val) => {
                let insets = match val.l() {
                    Ok(i) if !i.is_null() => i,
                    _ => return get_legacy_insets(env, &insets_obj),
                };
                let t = env.get_field(&insets, jni_str!("top"), jni_sig!("I")).map(|v| v.i().unwrap_or(0)).unwrap_or(0);
                let b = env.get_field(&insets, jni_str!("bottom"), jni_sig!("I")).map(|v| v.i().unwrap_or(0)).unwrap_or(0);
                let l = env.get_field(&insets, jni_str!("left"), jni_sig!("I")).map(|v| v.i().unwrap_or(0)).unwrap_or(0);
                let r = env.get_field(&insets, jni_str!("right"), jni_sig!("I")).map(|v| v.i().unwrap_or(0)).unwrap_or(0);
                (t as f32, b as f32, l as f32, r as f32)
            }
            Err(_) => get_legacy_insets(env, &insets_obj),
        }
    } else {
        get_legacy_insets(env, &insets_obj)
    };

    (top, bottom, left, right)
}

/// Fallback for Android < API 30: use deprecated getSystemWindowInset*() methods.
#[cfg(target_os = "android")]
fn get_legacy_insets(env: &mut jni::Env<'_>, insets_obj: &jni::objects::JObject<'_>) -> (f32, f32, f32, f32) {
    use jni::{jni_sig, jni_str};

    let top = env.call_method(insets_obj, jni_str!("getSystemWindowInsetTop"), jni_sig!("()I"), &[])
        .map(|v| v.i().unwrap_or(0)).unwrap_or(0);
    let bottom = env.call_method(insets_obj, jni_str!("getSystemWindowInsetBottom"), jni_sig!("()I"), &[])
        .map(|v| v.i().unwrap_or(0)).unwrap_or(0);
    let left = env.call_method(insets_obj, jni_str!("getSystemWindowInsetLeft"), jni_sig!("()I"), &[])
        .map(|v| v.i().unwrap_or(0)).unwrap_or(0);
    let right = env.call_method(insets_obj, jni_str!("getSystemWindowInsetRight"), jni_sig!("()I"), &[])
        .map(|v| v.i().unwrap_or(0)).unwrap_or(0);
    (top as f32, bottom as f32, left as f32, right as f32)
}

/// Get the Android API level.
///
/// Takes the caller's `Env` rather than attaching its own: jni 0.22 attachments
/// push a JNI stack frame, and nesting one inside `system_insets_with` would put
/// the local references it is holding out of the top frame.
#[cfg(target_os = "android")]
fn android_api_level(env: &mut jni::Env<'_>) -> i32 {
    use jni::{jni_sig, jni_str};

    let Ok(build_class) = env.find_class(jni_str!("android/os/Build$VERSION")) else { return 0 };
    env.get_static_field(&build_class, jni_str!("SDK_INT"), jni_sig!("I"))
        .map(|v| v.i().unwrap_or(0))
        .unwrap_or(0)
}

/// Get the display density (pixels per dp) for converting physical to logical pixels.
#[cfg(target_os = "android")]
fn get_display_density() -> f32 {
    use jni::objects::JObject;
    use jni::{jni_sig, jni_str};

    let ctx = ndk_context::android_context();
    let Some(vm) = android_vm() else { return 1.0 };
    let context = ctx.context();

    vm.attach_current_thread(|env| -> jni::errors::Result<f32> {
        let activity = unsafe { JObject::from_raw(env, context.cast()) };

        // activity.getResources().getDisplayMetrics().density
        let resources = env
            .call_method(
                &activity,
                jni_str!("getResources"),
                jni_sig!("()Landroid/content/res/Resources;"),
                &[],
            )?
            .l()?;
        let metrics = env
            .call_method(
                &resources,
                jni_str!("getDisplayMetrics"),
                jni_sig!("()Landroid/util/DisplayMetrics;"),
                &[],
            )?
            .l()?;
        env.get_field(&metrics, jni_str!("density"), jni_sig!("F"))?
            .f()
    })
    .unwrap_or(1.0)
}

/// Minimize the app by calling Activity.moveTaskToBack(true) via JNI.
/// This keeps the app alive in recents with a proper thumbnail instead
/// of killing the process (which leaves a white box in recents).
#[cfg(target_os = "android")]
pub fn move_task_to_back() {
    use jni::objects::{JObject, JValue};
    use jni::{jni_sig, jni_str};

    let ctx = ndk_context::android_context();
    let Some(vm) = android_vm() else {
        log::warn!("moveTaskToBack: failed to get JavaVM");
        return;
    };
    let context = ctx.context();

    let attached = vm.attach_current_thread(|env| -> jni::errors::Result<()> {
        let activity = unsafe { JObject::from_raw(env, context.cast()) };

        // moveTaskToBack() is Activity-only; context may be Application after resume.
        let activity_class = env.find_class(jni_str!("android/app/Activity"))?;
        if !env.is_instance_of(&activity, &activity_class).unwrap_or(false) {
            log::warn!("moveTaskToBack: context is not an Activity, skipping");
            return Ok(());
        }

        match env.call_method(
            &activity,
            jni_str!("moveTaskToBack"),
            jni_sig!("(Z)Z"),
            &[JValue::Bool(true)],
        ) {
            Ok(_) => log::info!("App moved to background"),
            Err(e) => log::warn!("moveTaskToBack failed: {:?}", e),
        }
        Ok(())
    });
    if attached.is_err() {
        log::warn!("moveTaskToBack: failed to attach JNI thread");
    }
}

// ---------------------------------------------------------------------------
// Compass heading via JNI (CompassHelper.java)
// ---------------------------------------------------------------------------

/// JClass for com.rustdar.CompassHelper, loaded once via the app class loader.
///
/// jni 0.22: `Global` is generic over the Java type it references, so this keeps
/// its `JClass`-ness and no longer needs an unsafe re-wrap to call statics on it.
#[cfg(target_os = "android")]
static COMPASS_CLASS: std::sync::OnceLock<jni::objects::Global<jni::objects::JClass<'static>>> =
    std::sync::OnceLock::new();

/// Read the current compass heading from CompassHelper.getHeading().
/// Returns `None` if the class wasn't loaded or no reading is available yet.
#[cfg(target_os = "android")]
fn get_compass_heading() -> Option<f32> {
    use jni::objects::JClass;
    use jni::{jni_sig, jni_str};

    let global_ref = COMPASS_CLASS.get()?;
    let vm = android_vm()?;

    let heading = vm
        .attach_current_thread(|env| -> jni::errors::Result<f32> {
            let cls: &JClass<'static> = global_ref;
            env.call_static_method(cls, jni_str!("getHeading"), jni_sig!("()F"), &[])?
                .f()
        })
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
            if let Some(heading) = get_compass_heading()
                && sender.send(heading).is_err()
            {
                break; // channel closed
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
    env: &mut jni::Env<'_>,
    loader: &jni::objects::JObject<'_>,
    activity: &jni::objects::JObject<'_>,
    class_name: &str,
) -> Option<jni::objects::Global<jni::objects::JClass<'static>>> {
    use jni::objects::{JClass, JValue};
    use jni::{jni_sig, jni_str};

    let name = env.new_string(class_name).ok()?;
    let cls_obj = env
        .call_method(
            loader,
            jni_str!("loadClass"),
            jni_sig!("(Ljava/lang/String;)Ljava/lang/Class;"),
            &[JValue::from(&name)],
        )
        .and_then(|v| v.l())
        .inspect_err(|e| log::warn!("Could not load {}: {:?}", class_name, e))
        .ok()?;

    // jni 0.22: `cast_local` is the checked replacement for the old
    // `JClass::from(JObject)` conversion, and it borrows rather than consumes,
    // so the global ref can be taken from the typed handle directly.
    let cls = env.cast_local::<JClass>(cls_obj).ok()?;
    let global = env.new_global_ref(&cls).ok();

    match env.call_static_method(
        &cls,
        jni_str!("register"),
        jni_sig!("(Landroid/app/Activity;)V"),
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

            // Back gesture on API 33+: back bypasses the native input queue and
            // goes through OnBackInvokedDispatcher. Unhandled, NativeActivity
            // calls finish() and the process dies; the helper minimises instead.
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