//! Android support: the JNI bridges and the `android_main` entry point for the
//! rustdar radar visualization application, folded in from the retired android
//! entry crate (WO-RP-2). This module tree is the fourth OS arm,
//! beside the `os_location` providers the other three share.
//!
//! Shared JNI plumbing lives here; each concern has its own file:
//! [`permissions`], [`location`], [`insets`], [`density`], [`theme`],
//! [`task_to_back`], [`back`], [`compass`], and the [`entry`] point.
//!
//! # The injection is a rule, not a leftover (READ BEFORE "SIMPLIFYING")
//!
//! `android_main` installs `set_theme_detector` / `set_insets_querier` /
//! `set_back_handler` / `set_back_press_taker` / `set_location_hooks` as
//! injected `fn` pointers even though, since the fold, callee and caller share
//! this crate. The injection is the frontend portability contract, not a
//! crate-graph workaround: `PlatformBridge` is declared in rustdar-frontend,
//! which must compile for targets that have never heard of JNI; the bridge
//! structs stay `deny(unsafe_code)`-clean and host-testable (TestBridge
//! injects the same fn pointers); and the JNI surface stays confined to these
//! cfg(android) modules. Do NOT "simplify" `set_*` into direct calls from
//! `AndroidPlatform` -- that couples the bridge to JNI symbols, breaks the
//! host double's parity with production wiring, and un-writes the
//! one-setter-carries-all-four half-install guard.

pub mod back;
pub mod compass;
pub mod density;
mod entry;
pub mod insets;
pub mod location;
pub mod permissions;
pub mod task_to_back;
pub mod theme;

// ---------------------------------------------------------------------------
// Shared JNI plumbing (the process JavaVM + Activity, and the class loader)
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
/// `back::event_loop_proxy`: nothing under this lock can panic today, and treating
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
