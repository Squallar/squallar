//! Android support: the JNI bridges and the `android_main` entry point, the
//! fourth OS arm beside the `os_location` providers the other three share.
//! Each concern has its own file; the location arm lives in
//! `rustdar_location::android`, which [`entry`] initialises once.
//!
//! # The injection is a rule, not a leftover (READ BEFORE "SIMPLIFYING")
//!
//! `android_main` installs `set_theme_detector` / `set_insets_querier` /
//! `set_back_handler` / `set_back_press_taker` as injected `fn` pointers even
//! though callee and caller share this crate: `PlatformBridge` is declared in
//! rustdar-app, which must compile for targets that have never heard of JNI,
//! the bridge structs stay `deny(unsafe_code)`-clean and host-testable, and
//! the JNI surface stays confined to these cfg(android) modules. Location is
//! not covered — its consumer is in the same crate as the JNI arm.

pub mod back;
pub mod compass;
pub mod density;
mod entry;
pub mod insets;
pub mod task_to_back;
pub mod theme;

/// The process `JavaVM` and a global reference to the real `Activity`,
/// recaptured at the top of every [`android_main`].
///
/// **Deliberately not `ndk_context`.** Under the pinned android-activity 0.6,
/// `ndk_context::android_context().context()` is `Activity.getApplication()`
/// from the very first call and on every API level. Three methods this module
/// needs are declared on `Activity` and do not exist on `Application`:
///
/// | method              | used by                     | symptom when called on Application |
/// |---------------------|-----------------------------|------------------------------------|
/// | `getWindow`         | [`get_system_insets`]       | insets always `(0,0,0,0)` |
/// | `moveTaskToBack`    | [`move_task_to_back`]       | back-to-minimise dead |
/// | `requestPermissions`| [`request_location_permission`] | the location dialog never shows |
///
/// Holding it ourselves also makes the graceful degradation achievable:
/// `ndk_context::android_context()` panics when the context is missing, and
/// these run on the GPS and compass threads where an unwind is silent.
///
/// **Replaceable, not write-once.** `android_main` is tied to the lifetime of
/// the *Activity*, so a `OnceLock` would leave every helper calling the
/// destroyed Activity and pin it in memory. The `Arc` is for the readers: the
/// old global ref drops with the last clone, not under a live JNI call.
static JAVA: std::sync::Mutex<Option<std::sync::Arc<JavaContext>>> = std::sync::Mutex::new(None);

/// See [`JAVA`].
struct JavaContext {
    vm: jni::vm::JavaVM,
    /// A *global* reference, so it stays valid on the GPS and compass threads.
    activity: jni::objects::Global<jni::objects::JObject<'static>>,
}

/// Clone the current [`JAVA`] context out of the lock, recovering from
/// poisoning rather than reporting "no Activity": treating a poisoned lock as
/// an absent context would take insets, back and the GPS prompt out for good.
fn java_context() -> Option<std::sync::Arc<JavaContext>> {
    JAVA.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// Install (or clear) the [`JAVA`] context. An overwrite on every
/// `android_main`: dropping the previous entry releases the global ref pinning
/// the previous Activity, deferred past any helper still holding its `Arc`.
fn set_java_context(context: Option<JavaContext>) {
    *JAVA
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = context.map(std::sync::Arc::new);
}

/// Attach the calling thread to the JVM and run `f`; `None` before the JNI
/// setup block, or if the thread could not be attached. Every caller degrades
/// to a default.
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

/// Load one of our own Java helper classes through the app's `ClassLoader` and
/// invoke its static `register(Activity)`, returning a global ref to the class.
/// It *must* go through `loader`: `android_main` runs on a thread attached
/// with no Java frames, where `FindClass` uses the system class loader.
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
    // `JClass::from(JObject)` conversion, and it borrows rather than consumes.
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
