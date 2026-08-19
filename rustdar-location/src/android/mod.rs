//! The Android arm of the facade: the location runtime permission and the
//! `LocationHelper` fix path, over JNI, owned by rustdar-location since
//! WO-RL-4 (seam ruling 6 — no remote location arm lives anywhere else).
//!
//! # The init seam
//!
//! [`init`] is called ONCE per `android_main` from the shell's android entry,
//! with the process `JavaVM` and a global ref to the real `Activity`. It
//! stashes this module's own [`JAVA`] context and registers
//! `com.rustdar.LocationHelper` through the app ClassLoader. [`deinit`] clears
//! the context when that `android_main` returns, releasing the Activity ref
//! exactly as the shell releases its own.
//!
//! # The hook inversion died here — FOR LOCATION ONLY
//!
//! Before WO-RL-4 the location calls reached the app's bridge as injected `fn`
//! pointers (`set_location_hooks`), under the rule written in the shell's
//! `rustdar/src/android/mod.rs`. That rule STANDS for insets, theme and back:
//! their consumer is `PlatformBridge`, declared in rustdar-frontend, which
//! must compile for targets that have never heard of JNI. Location's consumer
//! is no longer the bridge at all — it is this crate's own facade, the JNI
//! calls are made directly by [`AndroidBackend`], and the portability contract
//! is carried by the crate boundary itself: the facade's default build has no
//! `jni` in it, the arm is feature-fenced, and the app side still holds only a
//! [`LocationFacade`](crate::LocationFacade).
//!
//! # The JNI context is this module's own
//!
//! [`JAVA`], [`with_env`] and [`with_activity`] are deliberate twins of the
//! shell's (`rustdar/src/android/mod.rs`), which serves its insets/theme/back/
//! compass concerns — the facade owns its env-attach helper (the WO-RL-4
//! order's words) rather than reaching into an app shell it must not depend
//! on. The shell's copy carries the full rationale for every decision here
//! (why not `ndk_context`, why a `Mutex<Option<Arc<_>>>` and not a `OnceLock`,
//! why the calls run off the UI thread); keep the two in step.

pub(crate) mod location;
pub(crate) mod permissions;

use crate::provider::{LocationProvider, Wake, drain_latest};

/// The process `JavaVM` and a global reference to the real `Activity`,
/// recaptured by [`init`] at the top of every `android_main`. See the module
/// note — the shell's `JAVA` doc carries the full rationale.
static JAVA: std::sync::Mutex<Option<std::sync::Arc<JavaContext>>> = std::sync::Mutex::new(None);

/// See [`JAVA`].
pub(crate) struct JavaContext {
    vm: jni::vm::JavaVM,
    /// A *global* reference, so it stays valid on the `gps-location` thread.
    /// A local ref would be scoped to the `android_main` frame that created it.
    activity: jni::objects::Global<jni::objects::JObject<'static>>,
}

/// Clone the current [`JAVA`] context out of the lock, recovering from
/// poisoning rather than reporting "no Activity": treating a poisoned lock as
/// an absent context would silently take the GPS permission prompt out for
/// the rest of the process.
pub(crate) fn java_context() -> Option<std::sync::Arc<JavaContext>> {
    JAVA.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// Attach the calling thread to the JVM and run `f`.
///
/// `None` if [`init`] has not run yet (or [`deinit`] has cleared the last
/// Activity), or if the thread could not be attached. Every caller degrades
/// to a default rather than propagating a failure.
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

/// Install this module's JNI context and register `com.rustdar.LocationHelper`.
///
/// Called ONCE per `android_main`, from the shell's android entry, inside the
/// same JNI setup that stashes the shell's own context. An overwrite on every
/// call, exactly like the shell's `set_java_context`: dropping the previous
/// entry is what releases the global ref pinning the previous Activity —
/// deferred past any helper still holding its `Arc`, so the ref is never
/// deleted under a live JNI call.
///
/// The class MUST be resolved through the app's `ClassLoader` rather than
/// `FindClass` — `android_main` runs on a thread attached with no Java frames
/// on the stack, where `FindClass` resolves against the *system* loader,
/// which knows nothing about classes packaged in the app. The registration is
/// idempotent on the Java side (`LocationHelper.register` re-stashing the new
/// Activity is the point); the loaded class lands in
/// [`location::LOCATION_CLASS`], which is a process-wide `OnceLock` because a
/// class resolved through the app loader serves every Activity instance.
pub fn init(vm: jni::vm::JavaVM, activity: jni::objects::Global<jni::objects::JObject<'static>>) {
    *JAVA
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) =
        Some(std::sync::Arc::new(JavaContext { vm, activity }));

    // Register LocationHelper now, through the app loader, so the
    // subscription class is resolvable before the gate's first request.
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

        let name = env.new_string("com.rustdar.LocationHelper").ok()?;
        let cls_obj = env
            .call_method(
                &loader,
                jni_str!("loadClass"),
                jni_sig!("(Ljava/lang/String;)Ljava/lang/Class;"),
                &[JValue::from(&name)],
            )
            .and_then(|v| v.l())
            .inspect_err(|e| log::warn!("Could not load com.rustdar.LocationHelper: {e:?}"))
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
                log::info!("com.rustdar.LocationHelper registered");
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
/// Activity's global ref. The mirror of [`init`]; the shell calls both at the
/// same places it manages its own context.
pub fn deinit() {
    *JAVA
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
}

/// The Android arm: permission tri-state and the `LocationHelper` fix path,
/// all direct JNI calls into this module's own context.
///
/// Constructed by the shell's android entry AFTER [`init`], handed to the app
/// inside a [`LocationFacade`](crate::LocationFacade).
pub struct AndroidBackend {
    /// What the app last said about how many times it has asked, kept for the
    /// permission query to read. Android is the one platform that cannot tell
    /// "never asked" from "permanently denied" without it — see
    /// [`permissions::location_permission_status`].
    attempts: u8,
    /// Fixes from the 10 s poll thread, once [`set_wake`](LocationProvider::set_wake)
    /// has started it.
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

    /// Starts the 10 s poll thread with the app's wake. Before WO-RL-4 the
    /// shell's entry started this thread before `run_app` and handed the
    /// receiver to the bridge; the thread's own 3 s startup grace and its
    /// `location_active` check make the slightly later start equivalent — no
    /// fix can flow until the gate has requested delivery anyway.
    fn set_wake(&mut self, wake: Wake) {
        if self.fixes.is_some() {
            // Refuse rather than half-apply, like the shell's theme detector:
            // a second thread would double-poll the provider.
            log::warn!("location poll thread already started; ignoring the second wake");
            return;
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        location::start_location_thread(sender, move || wake());
        self.fixes = Some(receiver);
    }
}
