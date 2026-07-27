// The `jni-typecheck` feature compiles everything below except `android_main`
// for the host, so the JNI bodies get type-checked without an NDK. See the
// feature's comment in Cargo.toml.
#![cfg(any(target_os = "android", feature = "jni-typecheck"))]
// Under that feature there is no `android_main`, so every helper it is the sole
// caller of looks dead. They are not; they just have no host entry point.
#![cfg_attr(not(target_os = "android"), allow(dead_code))]

//! Android entry point for Rustdar Platform
//!
//! This crate provides the Android-specific entry point and configuration
//! for the Rustdar radar visualization application.

#[cfg(target_os = "android")]
use winit::platform::android::activity::AndroidApp;

// ---------------------------------------------------------------------------
// JNI location helper functions (Android only)
// ---------------------------------------------------------------------------

/// The process `JavaVM` and a global reference to the real `Activity`,
/// recaptured at the top of every [`android_main`].
///
/// **Deliberately not `ndk_context`.** `ndk_context::android_context().context()`
/// is not the Activity on the `android-activity` version this build pins:
///
/// * 0.5 called `initialize_android_context(jvm, activity)` with
///   `activity = (*na).clazz` — the `NativeActivity` itself.
/// * 0.6 calls `initialize_android_context(vm, app_global)` where `app_global`
///   is `get_application(env, jni_activity)`, i.e. **`Activity.getApplication()`**
///   (`android-activity-0.6.1/src/init.rs`, `init_android_main_thread`).
///
/// The jni 0.22 bump pinned this: `rustls-platform-verifier 0.7` needs
/// `jni ^0.22`, and `android-activity` only accepts `jni 0.22` from 0.6.1.
///
/// The distinction is load-bearing, and it is not the situational
/// "may be the Application after suspend/resume" the old comments here claimed.
/// Under 0.6 it is the Application from the very first call, on every API level,
/// and it never changes. Three methods this module needs are declared on
/// `Activity` and simply do not exist on `Application`:
///
/// | method              | used by                     | symptom when called on Application |
/// |---------------------|-----------------------------|------------------------------------|
/// | `getWindow`         | [`get_system_insets`]       | insets always `(0,0,0,0)`, so the map's `excluded_rects` ignore cutouts and nav bars |
/// | `moveTaskToBack`    | [`move_task_to_back`]       | back-to-minimise dead, on every API level and by either dispatch route |
/// | `requestPermissions`| [`request_location_permission`] | the location dialog is never shown, so GPS stays off |
///
/// `getResources`, `getSystemService` and `checkSelfPermission` are `Context`
/// methods and would work off either object. They use the Activity too, so
/// there is one answer to "which object is this", and because the Activity's
/// `Resources` track the current configuration where the Application's need not.
///
/// Holding this ourselves is also what makes the graceful degradation these
/// helpers document *achievable*. `ndk_context::android_context()` is
/// `ANDROID_CONTEXT.expect("android context was not initialized")`: a helper
/// built on it cannot fall back when the context is missing, because it has
/// already panicked. An empty slot here yields `None` instead, which matters
/// because these run on the GPS and compass threads where an unwind takes the
/// feature out silently.
///
/// # Replaceable, not write-once
///
/// `android_main` is tied to the lifetime of the *Activity*, not the process
/// -- [`EVENT_LOOP_PROXY`] pins the android-activity contract and became
/// replaceable for exactly this reason. This slot was a `OnceLock`, whose
/// `set` no-ops on the second Activity: every helper in the table above kept
/// calling the *destroyed* Activity, and the stale global ref pinned that
/// Activity for the rest of the process. So: a `Mutex<Option<Arc<_>>>`,
/// overwritten at the top of every `android_main` and cleared when one
/// returns. The `Arc` is for the readers -- a helper mid-call on the GPS or
/// compass thread keeps the context it started with until it returns, and the
/// old Activity's global ref drops with the last such clone rather than out
/// from under a live JNI call.
///
/// # These calls do not run on the Android main thread
///
/// Worth stating plainly, because until this fix the three Activity-only
/// bridges bailed out before reaching Java at all, and now they do not: none of
/// them runs on the UI thread. `get_system_insets` is called from the winit
/// event-loop thread, and the location bridges from the `gps-location` thread.
/// The consequences are per-call and are documented at each of them; the shared
/// part is that "it compiled and the object is right" is not the same as "the
/// framework expects this call here", and neither of those is testable without
/// a device.
static JAVA: std::sync::Mutex<Option<std::sync::Arc<JavaContext>>> = std::sync::Mutex::new(None);

/// See [`JAVA`].
struct JavaContext {
    vm: jni::vm::JavaVM,
    /// A *global* reference, so it stays valid on the GPS and compass threads.
    /// A local ref would be scoped to the `android_main` frame that created it.
    activity: jni::objects::Global<jni::objects::JObject<'static>>,
}

/// Clone the current [`JAVA`] context out of the lock, recovering from
/// poisoning rather than reporting "no Activity". Same reasoning as
/// [`event_loop_proxy`]: nothing under this lock can panic today, and treating
/// a poisoned lock as an absent context would silently take insets,
/// back-to-minimise and the GPS permission prompt out for the rest of the
/// process.
fn java_context() -> Option<std::sync::Arc<JavaContext>> {
    JAVA.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// Install (or clear) the [`JAVA`] context.
///
/// An overwrite on every `android_main`, exactly like [`set_event_loop_proxy`]:
/// dropping the previous entry is what releases the global ref pinning the
/// previous Activity -- deferred past any helper still holding its `Arc`, so
/// the ref is never deleted under a live JNI call.
fn set_java_context(context: Option<JavaContext>) {
    *JAVA
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = context.map(std::sync::Arc::new);
}

/// Attach the calling thread to the JVM and run `f`.
///
/// `None` if [`android_main`] has not reached its JNI setup block yet (or the
/// last Activity has been torn down and the next has not stashed its context),
/// or if the thread could not be attached. Every caller degrades to a default
/// rather than propagating a failure.
fn with_env<T>(f: impl FnOnce(&mut jni::Env<'_>) -> T) -> Option<T> {
    let java = java_context()?;
    java.vm
        .attach_current_thread(|env| -> jni::errors::Result<T> { Ok(f(env)) })
        .ok()
}

/// As [`with_env`], but also hands `f` the [`Activity`][JAVA] global ref.
fn with_activity<T>(
    f: impl FnOnce(&mut jni::Env<'_>, &jni::objects::JObject<'static>) -> T,
) -> Option<T> {
    let java = java_context()?;
    java.vm
        .attach_current_thread(|env| -> jni::errors::Result<T> { Ok(f(env, &java.activity)) })
        .ok()
}

/// Check whether the app holds either location runtime permission.
///
/// FINE **or** COARSE: since Android 12 the permission dialog offers
/// "approximate only", which grants COARSE and denies FINE. That is still a
/// usable grant -- the network provider serves fixes under COARSE alone (see
/// the per-provider fallback in [`last_known_location_with`]) -- so treating
/// FINE as the only "yes" would read a user who already answered as
/// unpermissioned and burn the bounded re-requests in
/// [`start_location_thread`] against them.
fn has_location_permission() -> bool {
    use jni::objects::JValue;
    use jni::{jni_sig, jni_str};

    // checkSelfPermission is a Context method, so the Activity serves fine.
    with_activity(|env, activity| -> jni::errors::Result<bool> {
        for permission in [
            "android.permission.ACCESS_FINE_LOCATION",
            "android.permission.ACCESS_COARSE_LOCATION",
        ] {
            let perm = env.new_string(permission)?;
            let granted = env
                .call_method(
                    activity,
                    jni_str!("checkSelfPermission"),
                    jni_sig!("(Ljava/lang/String;)I"),
                    &[JValue::from(&perm)],
                )?
                .i()?;
            if granted == 0 {
                return Ok(true); // PERMISSION_GRANTED == 0
            }
        }
        Ok(false)
    })
    .and_then(Result::ok)
    .unwrap_or(false)
}

/// Request the location runtime permissions (FINE together with COARSE).
///
/// Shows the system permission dialog. The result is asynchronous; poll
/// [`has_location_permission`] afterwards to check the outcome.
///
/// Returns whether the JNI call was actually made. That is not the same as
/// "the user granted it" — it is the caller's cue that the request happened at
/// all, so a failure to reach `Activity.requestPermissions` is not mistaken for
/// a dialog the user dismissed. See [`start_location_thread`].
///
/// # This is called off the main thread, and that is not a supported context
///
/// `Activity.requestPermissions` goes on to `startActivityForResult` and sets
/// `mHasCurrentPermissionsRequest` without synchronisation. The framework
/// expects both on the UI thread; this runs on the `gps-location` thread. It is
/// not a `checkThread()` assertion, so it does not throw — it is simply outside
/// what the framework guarantees, and whether the dialog appears can depend on
/// where the Activity is in its lifecycle when the call lands.
///
/// **That is why the caller retries, and why the retry must not be
/// "simplified" to a single attempt.** A `false` here is a request that did not
/// happen; treating it as a request the user declined is exactly the bug this
/// replaced. See the bounded loop in [`start_location_thread`].
fn request_location_permission() -> bool {
    use jni::objects::JValue;
    use jni::{jni_sig, jni_str};

    // requestPermissions() is Activity-only -- see [`JAVA`] for why this used to
    // be reached through `ndk_context` and therefore never ran.
    let result = with_activity(|env, activity| -> jni::errors::Result<()> {
        // FINE and COARSE together, and the pairing is load-bearing: from
        // Android 12 -- which a targetSdk 34 build is squarely under -- a
        // request for ACCESS_FINE_LOCATION *alone* is silently discarded by
        // the framework: no dialog, no callback, because the user must be
        // offered the "approximate" downgrade alongside it. Each discarded
        // call still counted against MAX_PERMISSION_REQUESTS in
        // [`start_location_thread`], so both bounded attempts burned with the
        // user never once asked. Both permissions are declared in the
        // manifest.
        let fine = env.new_string("android.permission.ACCESS_FINE_LOCATION")?;
        let coarse = env.new_string("android.permission.ACCESS_COARSE_LOCATION")?;
        let string_class = env.find_class(jni_str!("java/lang/String"))?;
        let perm_array = env.new_object_array(2, &string_class, &fine)?;
        perm_array.set_element(env, 1, &coarse)?;

        env.call_method(
            activity,
            jni_str!("requestPermissions"),
            jni_sig!("([Ljava/lang/String;I)V"),
            &[JValue::from(&perm_array), JValue::Int(1)],
        )?;
        Ok(())
    });

    match result {
        Some(Ok(())) => true,
        Some(Err(e)) => {
            log::warn!("requestPermissions failed: {e:?}");
            false
        }
        None => {
            log::warn!("requestPermissions: no Activity yet, or JNI attach failed");
            false
        }
    }
}

/// Try to retrieve the device's last known GPS location via `LocationManager`.
/// Returns a [`GpsFix`] on success or `None` if unavailable.
///
/// "Last known" is whatever the providers last produced for *any* client;
/// LocationHelper's subscription (see [`start_location_updates`]) is what
/// keeps them producing once permission is granted, and this poll doubles as
/// the fallback when that subscription could not be established.
fn get_last_known_location() -> Option<rustdar_gps::GpsFix> {
    with_activity(last_known_location_with).flatten()
}

/// Body of [`get_last_known_location`], split out so it can keep using `?` on
/// `Option` inside the `Env` closure that jni 0.22's attachment API requires.
fn last_known_location_with(
    env: &mut jni::Env<'_>,
    activity: &jni::objects::JObject<'_>,
) -> Option<rustdar_gps::GpsFix> {
    use jni::objects::JValue;
    use jni::{jni_sig, jni_str};

    // LocationManager lm = context.getSystemService("location");
    // getSystemService is a Context method, so the Activity works here.
    let service_name = env.new_string("location").ok()?;
    let lm = env
        .call_method(
            activity,
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

/// JClass for com.rustdar.LocationHelper, loaded once via the app class loader.
///
/// A `OnceLock`, unlike [`JAVA`]: this is a *class* resolved through the
/// process-wide app ClassLoader, so the same object serves every Activity
/// instance and there is nothing to replace. Same shape as [`COMPASS_CLASS`].
static LOCATION_CLASS: std::sync::OnceLock<jni::objects::Global<jni::objects::JClass<'static>>> =
    std::sync::OnceLock::new();

/// Ask LocationHelper to begin real location updates (`LocationHelper.start()`).
///
/// `getLastKnownLocation` is passive: it reports the fix some location client
/// caused a provider to produce, and on a device where no other app happens to
/// be requesting location it stays null forever -- permission or not.
/// `start()` makes this app that client: LocationHelper subscribes a
/// do-nothing listener on the main looper, which is what switches the
/// providers on, and the existing [`get_last_known_location`] poll reads the
/// fixes they then produce. That split -- Java holds the subscription, Rust
/// keeps all the fix extraction -- is the simplest mechanism that covers
/// minSdk 28 through targetSdk 34: `requestLocationUpdates(String, long,
/// float, LocationListener, Looper)` exists and is undeprecated across the
/// whole range (`getCurrentLocation` is API 30+), and a `LocationListener` is
/// a Java interface Rust cannot implement without a DEX class anyway, so the
/// listener lives in LocationHelper.java beside the CompassHelper it mirrors.
///
/// Returns whether the call reached Java, so the caller retries a miss --
/// helper class not registered, JNI attach failure -- on its next pass instead
/// of giving live updates up for the process. Safe to deliver more than once:
/// `start()` is idempotent.
fn start_location_updates() -> bool {
    use jni::objects::JClass;
    use jni::{jni_sig, jni_str};

    let Some(global_ref) = LOCATION_CLASS.get() else {
        return false;
    };

    with_env(|env| {
        let cls: &JClass<'static> = global_ref;
        env.call_static_method(cls, jni_str!("start"), jni_sig!("()V"), &[])
            .inspect_err(|e| log::warn!("LocationHelper.start() failed: {e:?}"))
            .is_ok()
    })
    .unwrap_or(false)
}

/// Start a background thread that polls GPS location and sends updates
/// through the provided channel. Also handles permission requests, and -- once
/// a permission is granted -- switches real location updates on via
/// [`start_location_updates`], without which the poll below can read null
/// forever on a device where no other app is requesting location.
fn start_location_thread(sender: std::sync::mpsc::Sender<rustdar_gps::GpsFix>) {
    std::thread::Builder::new()
        .name("gps-location".into())
        .spawn(move || {
            // Let the app fully initialise before doing JNI work
            std::thread::sleep(std::time::Duration::from_secs(3));

            // Counts requests that *actually reached* Activity.requestPermissions,
            // not attempts. The old code set a `permission_requested` flag after the
            // first call whether or not anything happened, and because that call was
            // reaching the Application rather than the Activity (see [`JAVA`]) the
            // dialog was never shown and never retried: GPS was off for the life of
            // the process unless the permission was granted from Settings.
            //
            // Bounded, because Android stops showing the dialog after the user has
            // declined twice and silently auto-denies from then on. Retrying past
            // that is pure noise.
            //
            // Retrying *up to* it is not belt-and-braces, and this is the part not
            // to collapse back into a single call: `request_location_permission`
            // reaches `Activity.requestPermissions` from this thread, which is not
            // the UI thread and not a context the framework supports (see that
            // function). The first attempt fires three seconds in and can land
            // before the Activity is resumed. The counter only advances when the
            // call actually got through, so a dropped attempt costs one more pass
            // of this loop rather than the whole feature.
            const MAX_PERMISSION_REQUESTS: u32 = 2;
            let mut requests_made = 0u32;

            // Whether `LocationHelper.start()` has been delivered. Tracked so the
            // call is made once per grant rather than every 10 s; retried while
            // `false` because a miss (helper class not registered, JNI attach
            // failure) must not cost the process live updates -- the call is
            // idempotent on the Java side.
            let mut updates_started = false;

            loop {
                if has_location_permission() {
                    if !updates_started {
                        updates_started = start_location_updates();
                    }
                    if let Some(fix) = get_last_known_location()
                        && sender.send(fix).is_err()
                    {
                        break; // channel closed
                    }
                } else if requests_made < MAX_PERMISSION_REQUESTS {
                    log::info!(
                        "Requesting ACCESS_FINE_LOCATION + ACCESS_COARSE_LOCATION permissions"
                    );
                    if request_location_permission() {
                        requests_made += 1;
                    }
                }

                std::thread::sleep(std::time::Duration::from_secs(10));
            }
        })
        .expect("failed to spawn gps-location thread");
}

/// Query the system window insets (status bar, navigation bar) in physical pixels.
/// Returns (top, bottom, left, right) inset values.
///
/// # Called from the winit thread, not the UI thread
///
/// `View`/`ViewRootImpl` are main-thread-only by contract. `getWindowInsets`
/// happens not to `checkThread()`, so this does not throw — but what it returns
/// is whatever `ViewRootImpl` last computed, not a fresh measurement. Before
/// the first layout that is the null this function already guards for, which is
/// exactly why [`android_main`] defers the query to a callback fired on the
/// first `resumed()` rather than calling it at startup.
///
/// The practical consequence is a stale read after a configuration change until
/// the next layout, not a wrong-object read. Prior to the [`JAVA`] fix this
/// returned `(0,0,0,0)` unconditionally, so any real value is new behaviour
/// that has not been observed on a device.
pub fn get_system_insets() -> (f32, f32, f32, f32) {
    with_activity(system_insets_with).unwrap_or((0.0, 0.0, 0.0, 0.0))
}

/// Body of [`get_system_insets`], split out so it can `return` early from inside
/// the `Env` closure that jni 0.22's attachment API requires.
fn system_insets_with(
    env: &mut jni::Env<'_>,
    activity: &jni::objects::JObject<'_>,
) -> (f32, f32, f32, f32) {
    use jni::objects::JValue;
    use jni::{jni_sig, jni_str};

    // `activity` is the real Activity -- see [`JAVA`]. This used to take
    // whatever `ndk_context` held and guard with `is_instance_of(_, Activity)`,
    // which on android-activity 0.6 is the Application and so failed every
    // single time: the insets returned here were unconditionally (0,0,0,0), on
    // every API level, and the map laid its excluded_rects out against a
    // full-bleed rect that ignored cutouts and the navigation bar.
    //
    // Activity.getWindow().getDecorView().getRootWindowInsets()
    let window = match env.call_method(
        activity,
        jni_str!("getWindow"),
        jni_sig!("()Landroid/view/Window;"),
        &[],
    ) {
        Ok(w) => match w.l() {
            Ok(w) => w,
            Err(_) => return (0.0, 0.0, 0.0, 0.0),
        },
        Err(_) => return (0.0, 0.0, 0.0, 0.0),
    };
    let decor = match env.call_method(
        &window,
        jni_str!("getDecorView"),
        jni_sig!("()Landroid/view/View;"),
        &[],
    ) {
        Ok(v) => match v.l() {
            Ok(v) => v,
            Err(_) => return (0.0, 0.0, 0.0, 0.0),
        },
        Err(_) => return (0.0, 0.0, 0.0, 0.0),
    };
    let insets_obj = match env.call_method(
        &decor,
        jni_str!("getRootWindowInsets"),
        jni_sig!("()Landroid/view/WindowInsets;"),
        &[],
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
        let type_mask =
            match env.call_static_method(&type_class, jni_str!("systemBars"), jni_sig!("()I"), &[])
            {
                Ok(v) => match v.i() {
                    Ok(v) => v,
                    Err(_) => return get_legacy_insets(env, &insets_obj),
                },
                Err(_) => return get_legacy_insets(env, &insets_obj),
            };
        let insets_result = env.call_method(
            &insets_obj,
            jni_str!("getInsets"),
            jni_sig!("(I)Landroid/graphics/Insets;"),
            &[JValue::Int(type_mask)],
        );
        match insets_result {
            Ok(val) => {
                let insets = match val.l() {
                    Ok(i) if !i.is_null() => i,
                    _ => return get_legacy_insets(env, &insets_obj),
                };
                let t = env
                    .get_field(&insets, jni_str!("top"), jni_sig!("I"))
                    .map(|v| v.i().unwrap_or(0))
                    .unwrap_or(0);
                let b = env
                    .get_field(&insets, jni_str!("bottom"), jni_sig!("I"))
                    .map(|v| v.i().unwrap_or(0))
                    .unwrap_or(0);
                let l = env
                    .get_field(&insets, jni_str!("left"), jni_sig!("I"))
                    .map(|v| v.i().unwrap_or(0))
                    .unwrap_or(0);
                let r = env
                    .get_field(&insets, jni_str!("right"), jni_sig!("I"))
                    .map(|v| v.i().unwrap_or(0))
                    .unwrap_or(0);
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
fn get_legacy_insets(
    env: &mut jni::Env<'_>,
    insets_obj: &jni::objects::JObject<'_>,
) -> (f32, f32, f32, f32) {
    use jni::{jni_sig, jni_str};

    let top = env
        .call_method(
            insets_obj,
            jni_str!("getSystemWindowInsetTop"),
            jni_sig!("()I"),
            &[],
        )
        .map(|v| v.i().unwrap_or(0))
        .unwrap_or(0);
    let bottom = env
        .call_method(
            insets_obj,
            jni_str!("getSystemWindowInsetBottom"),
            jni_sig!("()I"),
            &[],
        )
        .map(|v| v.i().unwrap_or(0))
        .unwrap_or(0);
    let left = env
        .call_method(
            insets_obj,
            jni_str!("getSystemWindowInsetLeft"),
            jni_sig!("()I"),
            &[],
        )
        .map(|v| v.i().unwrap_or(0))
        .unwrap_or(0);
    let right = env
        .call_method(
            insets_obj,
            jni_str!("getSystemWindowInsetRight"),
            jni_sig!("()I"),
            &[],
        )
        .map(|v| v.i().unwrap_or(0))
        .unwrap_or(0);
    (top as f32, bottom as f32, left as f32, right as f32)
}

/// Get the Android API level.
///
/// Takes the caller's `Env` rather than attaching its own: jni 0.22 attachments
/// push a JNI stack frame, and nesting one inside `system_insets_with` would put
/// the local references it is holding out of the top frame.
fn android_api_level(env: &mut jni::Env<'_>) -> i32 {
    use jni::{jni_sig, jni_str};

    let Ok(build_class) = env.find_class(jni_str!("android/os/Build$VERSION")) else {
        return 0;
    };
    env.get_static_field(&build_class, jni_str!("SDK_INT"), jni_sig!("I"))
        .map(|v| v.i().unwrap_or(0))
        .unwrap_or(0)
}

/// Get the display density (pixels per dp) for converting physical to logical pixels.
fn get_display_density() -> f32 {
    use jni::{jni_sig, jni_str};

    // getResources is a Context method, so this worked on the Application too --
    // but the Activity's Resources are the ones that track the current
    // configuration, which is what a density reading wants.
    with_activity(|env, activity| -> jni::errors::Result<f32> {
        // activity.getResources().getDisplayMetrics().density
        let resources = env
            .call_method(
                activity,
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
    .and_then(Result::ok)
    .unwrap_or(1.0)
}

/// Read the OS dark-theme preference out of `Configuration.uiMode`.
///
/// Handed to the Android `PlatformBridge` by [`android_main`] rather than called
/// from it. `rustdar-platform`, which owns that bridge, is
/// `#![deny(unsafe_code)]` with one scoped `allow` on the iOS entry symbol, so
/// JNI stays out of it by policy — and it could not borrow one from this crate
/// anyway, because this crate is the cdylib that depends on *it*. Injecting a
/// `fn()` is the same inversion `set_insets_querier` and `set_back_handler`
/// already use.
///
/// `dark-light` answers this on every other platform. It compiles for Android
/// and returns a wrong answer there, which is why this exists at all.
///
/// Every failure path answers `false` (light) rather than propagating, and here
/// that fallback is reachable rather than decorative: [`with_activity`] yields
/// `None` before [`android_main`] has stashed the Activity, and the theme poll
/// thread can call this at any time. This used to read `ndk_context`, where the
/// equivalent guard could never fire — `ndk_context::android_context()` panics
/// on its own `expect` if the context was never initialised, one line before
/// anything got to check a pointer.
pub fn detect_dark_theme() -> bool {
    use jni::{jni_sig, jni_str};

    // `getResources()` is declared on `Context`, so this resolved off the
    // Application that `ndk_context` hands out and was the one bridge in this
    // file that had never been broken by it. It goes through the Activity now
    // for the same reason the others do (see [`JAVA`]) -- one answer to "which
    // object is this" -- and with a small gain: the Activity's `Resources`
    // track the current configuration, which is exactly what is being read.
    let ui_mode = with_activity(|env, activity| -> jni::errors::Result<i32> {
        // Context.getResources().getConfiguration().uiMode
        let resources = env
            .call_method(
                activity,
                jni_str!("getResources"),
                jni_sig!("()Landroid/content/res/Resources;"),
                &[],
            )?
            .l()?;
        let configuration = env
            .call_method(
                &resources,
                jni_str!("getConfiguration"),
                jni_sig!("()Landroid/content/res/Configuration;"),
                &[],
            )?
            .l()?;

        env.get_field(&configuration, jni_str!("uiMode"), jni_sig!("I"))?
            .i()
    });

    // `Option<Result<_>>`: the outer `None` is "no Activity yet, or the thread
    // would not attach", the inner `Err` is a JNI failure. Both mean light.
    let Some(Ok(ui_mode)) = ui_mode else {
        return false;
    };

    // Configuration.UI_MODE_NIGHT_MASK / UI_MODE_NIGHT_YES.
    const UI_MODE_NIGHT_MASK: i32 = 0x30;
    const UI_MODE_NIGHT_YES: i32 = 0x20;

    (ui_mode & UI_MODE_NIGHT_MASK) == UI_MODE_NIGHT_YES
}

/// Minimize the app by calling `Activity.moveTaskToBack(true)` via JNI.
///
/// This keeps the app alive in recents with a proper thumbnail instead of
/// killing the process (which leaves a white box in recents).
///
/// This is where *every* back press that the UI did not want ends up, whichever
/// route it came in by: `KEYCODE_BACK` off the native input queue, or
/// `OnBackInvokedDispatcher` through [`Java_com_rustdar_BackHandler_nativeBackPressed`].
/// Both reach `App::resolve_back_press`, which asks the UI first and only then
/// calls `PlatformBridge::handle_back` — which is this.
///
/// `handle_back` reports `true` as soon as a handler is installed, so a no-op
/// here reads to the frontend as a handled press and the button does nothing at
/// all — which is what happened while this was reaching the Application instead
/// of the Activity. See [`JAVA`].
pub fn move_task_to_back() {
    use jni::objects::JValue;
    use jni::{jni_sig, jni_str};

    let called = with_activity(|env, activity| {
        match env.call_method(
            activity,
            jni_str!("moveTaskToBack"),
            jni_sig!("(Z)Z"),
            &[JValue::Bool(true)],
        ) {
            Ok(_) => log::info!("App moved to background"),
            Err(e) => log::warn!("moveTaskToBack failed: {e:?}"),
        }
    });
    if called.is_none() {
        log::warn!("moveTaskToBack: no Activity yet, or JNI attach failed");
    }
}

// ---------------------------------------------------------------------------
// Predictive back (BackHandler.java)
// ---------------------------------------------------------------------------

/// A back press from `OnBackInvokedDispatcher`, waiting to be spent.
///
/// Written on the Java UI thread by [`Java_com_rustdar_BackHandler_nativeBackPressed`]
/// and taken on the `android_main` thread by [`take_back_press`]. A flag rather
/// than a count: two presses the loop never got between are one press as far as
/// the user is concerned, and collapsing them here is cheaper than dismissing
/// two layers for one gesture.
///
/// Process-global, and `android_main` is not: see [`set_event_loop_proxy`],
/// which clears this so a press parked against a dead loop is not spent by the
/// next one.
static BACK_PRESS_PENDING: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Wakes the winit loop for [`BACK_PRESS_PENDING`], or `None` when there is no
/// loop to wake.
///
/// **Replaceable, not write-once.** `android_main` is tied to the lifetime of
/// the *Activity*, not the process: android-activity's own docs say it "may be
/// called multiple times, for each `Activity` instance". `run_app` consumes the
/// `EventLoop` and drops the receiver behind the proxy, so a second
/// `android_main` must install a live one over the dead one. A `OnceLock` here
/// would keep the corpse, `send_event` would fail forever, and every back press
/// for the rest of the process would fall through to the Java minimise — the
/// exact bug this route exists to remove, silently reinstated.
///
/// The `Mutex` is for that replacement and nothing else. `EventLoopProxy<()>`
/// is both `Send` and `Sync` here (`mpsc::Sender<T: Send>` has been `Sync`
/// since Rust 1.72, and `AndroidAppWaker` declares both), so a static needs no
/// lock to *hold* one — only to swap one.
///
/// Empty until [`android_main`] builds the loop, which is after
/// [`register_java_helper`] has already registered `BackHandler`; presses in
/// that window are what the Java side's fallback covers.
#[cfg(target_os = "android")]
static EVENT_LOOP_PROXY: std::sync::Mutex<Option<winit::event_loop::EventLoopProxy<()>>> =
    std::sync::Mutex::new(None);

/// Pins the "not there to make the proxy shareable" half of the note above.
///
/// An earlier draft of it said `mpsc::Sender` was `Send` but not `Sync`, which
/// stopped being true in Rust 1.72 and was simply wrong about the lock's reason
/// to exist. If this ever fails the lock has acquired a second reason, and the
/// note needs rewriting rather than the assertion deleting.
#[cfg(target_os = "android")]
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<winit::event_loop::EventLoopProxy<()>>();
};

/// Take the proxy slot, recovering from poisoning rather than reporting "no
/// loop".
///
/// Nothing under this lock can panic today, so poisoning means some future
/// caller does. Treating that as "no event loop" would silently downgrade every
/// later back press to a bare minimise, which is precisely the failure this
/// module is about; the proxy itself is unaffected by an unwind elsewhere, so
/// the guard is worth taking.
#[cfg(target_os = "android")]
fn event_loop_proxy()
-> std::sync::MutexGuard<'static, Option<winit::event_loop::EventLoopProxy<()>>> {
    EVENT_LOOP_PROXY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Install (or clear) the proxy back presses are handed to.
///
/// Clears [`BACK_PRESS_PENDING`] with it: on a second `android_main` a press
/// may be parked against the loop that just died, and the new loop's first
/// `about_to_wait` would otherwise spend it on a layer the user never asked to
/// close.
#[cfg(target_os = "android")]
fn set_event_loop_proxy(proxy: Option<winit::event_loop::EventLoopProxy<()>>) {
    *event_loop_proxy() = proxy;
    BACK_PRESS_PENDING.store(false, std::sync::atomic::Ordering::Release);
}

/// Park a back press and wake the event loop for it.
///
/// `false` means there is no live loop to hand it to, and the Java caller
/// minimises for itself — which is what it used to do unconditionally.
fn post_back_press() -> bool {
    #[cfg(target_os = "android")]
    {
        use std::sync::atomic::Ordering;

        let slot = event_loop_proxy();
        let Some(proxy) = slot.as_ref() else {
            log::warn!("back press with no event loop installed yet; minimising in Java");
            return false;
        };

        // Parked before the wake, because the wake is what comes back for it.
        BACK_PRESS_PENDING.store(true, Ordering::Release);
        // `send_event` queues *and* wakes. Waking alone would not do: winit's
        // Android backend discards a `PollEvent::Wake` unless the loop is
        // running *and* a redraw or a user event is already outstanding
        // (`!self.running || (!pending_redraw && !has_incoming())`), so a bare
        // wake is dropped outright while paused and dropped when idle.
        if proxy.send_event(()).is_ok() {
            return true;
        }

        // The loop closed between the check and the send. The flag stays set on
        // purpose: clearing it here would also erase a *previous* press that
        // parked successfully and has not been drained. Nothing will drain it
        // now, and `set_event_loop_proxy` clears it when a loop next appears.
        log::warn!("the event loop is gone; minimising in Java");
    }
    false
}

/// Take the parked back press, if there is one.
///
/// Injected into `AndroidPlatform` as its `poll_back_press`, and read from
/// `App::about_to_wait` on the loop the wake above interrupted.
fn take_back_press() -> bool {
    BACK_PRESS_PENDING.swap(false, std::sync::atomic::Ordering::Acquire)
}

/// `BackHandler.nativeBackPressed()` — the predictive-back gesture, arriving on
/// the Java UI thread.
///
/// Returns whether the press reached the Rust funnel. `false` is the Java
/// side's cue to minimise for itself; [`post_back_press`] logs which of the two
/// reasons it was.
///
/// Deliberately does nothing but park and wake. This is not the `android_main`
/// thread, so it cannot touch `App`, and the framework is waiting on it, so it
/// must not block. The decision — dismiss a layer, or minimise — belongs to
/// `App::resolve_back_press`, exactly as it does for `KEYCODE_BACK`.
///
/// Raw pointers rather than `jni::Env`/`JClass`: neither is used, and the raw
/// signature is the JNI ABI with nothing in between to get wrong.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_rustdar_BackHandler_nativeBackPressed(
    _env: *mut jni::sys::JNIEnv,
    _class: jni::sys::jclass,
) -> jni::sys::jboolean {
    if post_back_press() {
        jni::sys::JNI_TRUE
    } else {
        jni::sys::JNI_FALSE
    }
}

// ---------------------------------------------------------------------------
// Compass heading via JNI (CompassHelper.java)
// ---------------------------------------------------------------------------

/// JClass for com.rustdar.CompassHelper, loaded once via the app class loader.
///
/// jni 0.22: `Global` is generic over the Java type it references, so this keeps
/// its `JClass`-ness and no longer needs an unsafe re-wrap to call statics on it.
static COMPASS_CLASS: std::sync::OnceLock<jni::objects::Global<jni::objects::JClass<'static>>> =
    std::sync::OnceLock::new();

/// Read the current compass heading from CompassHelper.getHeading().
/// Returns `None` if the class wasn't loaded or no reading is available yet.
fn get_compass_heading() -> Option<f32> {
    use jni::objects::JClass;
    use jni::{jni_sig, jni_str};

    let global_ref = COMPASS_CLASS.get()?;

    let heading = with_env(|env| {
        let cls: &JClass<'static> = global_ref;
        env.call_static_method(cls, jni_str!("getHeading"), jni_sig!("()F"), &[])
            .and_then(|v| v.f())
            .ok()
    })
    .flatten()?;

    if heading < 0.0 {
        None // -1 means no reading yet
    } else {
        Some(heading)
    }
}

/// Start a background thread that polls the compass heading every 200ms and
/// sends updates through the provided channel.
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
        })
        .expect("failed to spawn compass-heading thread");
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
            // here; actually *started* lazily from that thread, once it sees
            // the runtime permission granted. See `start_location_updates`.
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
    let mut platform_app = rustdar_frontend::app::App::new(Box::new(
        rustdar_platform_lib::platform::create_platform(),
    ));

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
