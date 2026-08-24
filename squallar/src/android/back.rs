//! Predictive back (`BackHandler.kt`): the claim this app publishes, the parked
//! press, and the one JNI symbol it exports.

/// A back press from `OnBackInvokedDispatcher`, waiting to be spent. Written on
/// the Java UI thread by the JNI symbol below and taken on the `android_main`
/// thread by [`take_back_press`]. A flag rather than a count: two presses the
/// loop never got between are one press to the user. [`set_back_waker`] clears
/// it so a press parked against a dead loop is not spent by the next.
static BACK_PRESS_PENDING: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// How a parked press asks for the loop iteration that will spend it, or `None`
/// when there is no window to ask through.
///
/// A clone of the app's `RedrawWaker`, the same handle the location and compass
/// threads wake with, and for the same reason: `Window::request_redraw` sets
/// `redraw_flag` *before* waking the looper, so the wake survives where a bare
/// `PollEvent::Wake` is dropped — see the note on `start_location_thread` in
/// squallar-location. An `EventLoopProxy::send_event` would surface as
/// `ApplicationHandler::user_event`, which `App` does not implement.
///
/// **Replaceable, not write-once.** `android_main` is tied to the lifetime of
/// the *Activity*, so a `OnceLock` would keep the corpse and every later press
/// would park against a waker with nothing behind it. The `Mutex` is for that
/// replacement and nothing else.
#[cfg(target_os = "android")]
static BACK_WAKER: std::sync::Mutex<Option<squallar_app::platform::RedrawWaker>> =
    std::sync::Mutex::new(None);

/// Take the waker slot, recovering from poisoning rather than reporting "no
/// loop": that would leave every later back press parked and never spent.
#[cfg(target_os = "android")]
fn back_waker() -> std::sync::MutexGuard<'static, Option<squallar_app::platform::RedrawWaker>> {
    BACK_WAKER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Install (or clear) the waker back presses are handed to. Clears
/// [`BACK_PRESS_PENDING`] with it: on a second `android_main` a press may be
/// parked against the loop that just died.
#[cfg(target_os = "android")]
pub(super) fn set_back_waker(waker: Option<squallar_app::platform::RedrawWaker>) {
    *back_waker() = waker;
    BACK_PRESS_PENDING.store(false, std::sync::atomic::Ordering::Release);
}

/// Park a back press and ask for the frame that will spend it.
///
/// Nothing to report back: the Kotlin side has no decision left to make. A
/// press that arrives before the waker is installed still parks, and
/// `about_to_wait` collects it on the first iteration after that.
fn post_back_press() {
    BACK_PRESS_PENDING.store(true, std::sync::atomic::Ordering::Release);

    #[cfg(target_os = "android")]
    {
        let waker = back_waker().clone();
        match waker {
            // Dropped before waking: `notify_redraw` catches the unwind X11's
            // copy raises once the loop has closed, and a `Mutex` poisoned
            // under the guard would silently swallow every later wake.
            Some(waker) => waker.wake(),
            None => log::warn!("back press with no window yet; parked for the next iteration"),
        }
    }
}

/// Take the parked back press, if there is one. Injected into
/// `AndroidPlatform` as its `poll_back_press`, read from `App::about_to_wait`.
pub(super) fn take_back_press() -> bool {
    BACK_PRESS_PENDING.swap(false, std::sync::atomic::Ordering::Acquire)
}

/// Publish the app's claim on the next back press: `true` while the UI has
/// something a press would close, `false` otherwise. Injected into
/// `AndroidPlatform` as its `set_back_claimed` sink, pushed from the end of a
/// frame and only on a change.
///
/// Registering the callback is the *only* truthful way to claim a press —
/// `onBackInvoked()` returns void, so a press once taken cannot be handed back
/// — and staying unregistered is what lets the platform draw its own
/// back-to-home preview. A claim that has gone stale is still safe: the press
/// routes into `App::resolve_back_press`, which minimises through
/// `PlatformBridge::handle_back` when it finds nothing to dismiss.
///
/// **Dormant today, and deliberately so.** The manifest does not set
/// `android:enableOnBackInvokedCallback`, so nothing ever invokes the callback
/// this registers and every real press arrives as KEYCODE_BACK instead. The
/// claim is still published, still edge-triggered and still true, because the
/// opt-in becomes mandatory at targetSdk 36 and the reason it is off is a
/// separate, measured bug: `android_main` does not run for a second Activity,
/// so an app the platform backgrounds cannot be reopened (WO-RP-SPIKE leg 4b).
pub(super) fn report_back_claim(claimed: bool) {
    use jni::objects::{JClass, JValue};
    use jni::{jni_sig, jni_str};

    let Some(global_ref) = BACK_CLASS.get() else {
        return;
    };

    super::with_env(|env| {
        let cls: &JClass<'static> = global_ref;
        let _ = env
            .call_static_method(
                cls,
                jni_str!("setClaimed"),
                jni_sig!("(Z)V"),
                &[JValue::Bool(claimed)],
            )
            .inspect_err(|e| log::warn!("BackHandler.setClaimed() failed: {e:?}"));
    });
}

/// JClass for app.squallar.BackHandler, loaded once via the app class loader.
/// A `OnceLock`: one class resolved through the app ClassLoader serves every
/// Activity, and `BackHandler` re-stashes the Activity itself on each
/// `register`.
pub(super) static BACK_CLASS: std::sync::OnceLock<
    jni::objects::Global<jni::objects::JClass<'static>>,
> = std::sync::OnceLock::new();

/// `BackHandler.nativeBackPressed()` — the predictive-back gesture, on the Java
/// UI thread. Deliberately does nothing but park and wake: this is not the
/// `android_main` thread, so it cannot touch `App`, and the framework is
/// waiting on it. Raw pointers because neither `jni::Env` nor `JClass` is used.
#[unsafe(no_mangle)]
#[allow(unsafe_code, reason = "the JNI ABI symbol BackHandler binds")]
pub extern "system" fn Java_app_squallar_BackHandler_nativeBackPressed(
    _env: *mut jni::sys::JNIEnv,
    _class: jni::sys::jclass,
) {
    post_back_press();
}
