//! The Android arm of the facade: the location runtime permission and the
//! `LocationHelper` fix path, over JNI.
//!
//! [`init`] is called ONCE per `android_main` from the shell's android entry,
//! with the process `JavaVM` and a global ref to the real `Activity`; [`deinit`]
//! clears the context when that `android_main` returns.
//!
//! [`JAVA`], [`with_env`] and [`with_activity`] are deliberate twins of the
//! shell's (`squallar/src/android/mod.rs`), whose copies carry the full rationale.
//! Keep the two in step.

pub(crate) mod location;
pub(crate) mod permissions;

use crate::provider::{LocationProvider, Wake, drain_latest};

/// The process `JavaVM` and a global reference to the real `Activity`,
/// recaptured by [`init`] at the top of every `android_main`.
static JAVA: std::sync::Mutex<Option<std::sync::Arc<JavaContext>>> = std::sync::Mutex::new(None);

pub(crate) struct JavaContext {
    vm: jni::vm::JavaVM,
    /// A *global* reference, so it stays valid on the `gps-location` thread; a
    /// local ref would be scoped to the `android_main` frame that created it.
    activity: jni::objects::Global<jni::objects::JObject<'static>>,
}

/// Clone the current [`JAVA`] context out of the lock, recovering from poisoning
/// rather than reporting "no Activity" — that would silently take the GPS
/// permission prompt out for the rest of the process.
pub(crate) fn java_context() -> Option<std::sync::Arc<JavaContext>> {
    JAVA.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// Attach the calling thread to the JVM and run `f`. `None` if [`init`] has not
/// run yet, or the thread could not be attached; every caller degrades to a
/// default rather than propagating a failure.
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

/// Install this module's JNI context and register `app.squallar.LocationHelper`.
///
/// Called ONCE per `android_main`. An overwrite on every call: dropping the
/// previous entry releases the global ref pinning the previous Activity,
/// deferred past any helper still holding its `Arc`.
///
/// The class MUST be resolved through the app's `ClassLoader` and not
/// `FindClass` — `android_main` runs on a thread with no Java frames on the
/// stack, where `FindClass` resolves against the *system* loader.
pub fn init(vm: jni::vm::JavaVM, activity: jni::objects::Global<jni::objects::JObject<'static>>) {
    *JAVA
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) =
        Some(std::sync::Arc::new(JavaContext { vm, activity }));

    // Registered now so the class is resolvable before the gate's first request.
    let registered = with_activity(|env, activity| {
        use jni::objects::{JClass, JValue};
        use jni::{jni_sig, jni_str};

        let loader = env
            .call_method(
                activity,
                jni_str!("getClassLoader"),
                jni_sig!("()Ljava/lang/ClassLoader;"),
                &[],
            )
            .and_then(|v| v.l())
            .inspect_err(|e| log::warn!("Context.getClassLoader() failed: {e:?}"))
            .ok()?;

        let name = env.new_string("app.squallar.LocationHelper").ok()?;
        let cls_obj = env
            .call_method(
                &loader,
                jni_str!("loadClass"),
                jni_sig!("(Ljava/lang/String;)Ljava/lang/Class;"),
                &[JValue::from(&name)],
            )
            .and_then(|v| v.l())
            .inspect_err(|e| log::warn!("Could not load app.squallar.LocationHelper: {e:?}"))
            .ok()?;

        // jni 0.22: `cast_local` is the checked replacement for the old
        // `JClass::from(JObject)` conversion.
        let cls = env.cast_local::<JClass>(cls_obj).ok()?;
        let global = env.new_global_ref(&cls).ok();

        match env.call_static_method(
            &cls,
            jni_str!("register"),
            jni_sig!("(Landroid/app/Activity;)V"),
            &[JValue::from(activity)],
        ) {
            Ok(_) => {
                log::info!("app.squallar.LocationHelper registered");
                global
            }
            Err(e) => {
                log::warn!("LocationHelper.register() failed: {e:?}");
                None
            }
        }
    })
    .flatten();

    if let Some(cls) = registered {
        let _ = location::LOCATION_CLASS.set(cls);
    }
}

/// Clear the JNI context when an `android_main` returns, releasing the
/// Activity's global ref. The mirror of [`init`].
pub fn deinit() {
    *JAVA
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
}

/// The Android arm: permission tri-state and the `LocationHelper` fix path.
/// Constructed by the shell's android entry AFTER [`init`].
pub struct AndroidBackend {
    /// Android is the one platform that cannot tell "never asked" from
    /// "permanently denied" without it.
    attempts: u8,
    /// Fixes from the 10 s poll thread, once `set_wake` has started it.
    fixes: Option<std::sync::mpsc::Receiver<crate::Fix>>,
}

impl Default for AndroidBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl AndroidBackend {
    pub fn new() -> Self {
        Self {
            attempts: 0,
            fixes: None,
        }
    }
}

impl LocationProvider for AndroidBackend {
    fn permission(&self) -> crate::LocationPermission {
        permissions::location_permission_status(self.attempts)
    }

    fn request(&mut self) -> bool {
        location::request_location()
    }

    fn stop(&mut self) {
        location::stop_location();
    }

    fn active(&self) -> bool {
        location::location_active()
    }

    fn set_attempts(&mut self, attempts: u8) {
        self.attempts = attempts;
    }

    fn poll_fix(&mut self) -> Option<crate::Fix> {
        self.fixes.as_ref().and_then(drain_latest)
    }

    /// Starts the 10 s poll thread with the app's wake. Its 3 s startup grace
    /// and `location_active` check mean no fix can flow until the gate has
    /// requested delivery anyway.
    fn set_wake(&mut self, wake: Wake) {
        if self.fixes.is_some() {
            // Refuse rather than half-apply: a second thread would double-poll.
            log::warn!("location poll thread already started; ignoring the second wake");
            return;
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        location::start_location_thread(sender, move || wake());
        self.fixes = Some(receiver);
    }
}
