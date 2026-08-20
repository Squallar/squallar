//! Predictive back (BackHandler.java): the parked press, the event-loop wake,
//! and the one JNI symbol this app exports.

/// A back press from `OnBackInvokedDispatcher`, waiting to be spent. Written on
/// the Java UI thread by the JNI symbol below and taken on the `android_main`
/// thread by [`take_back_press`]. A flag rather than a count: two presses the
/// loop never got between are one press to the user. [`set_event_loop_proxy`]
/// clears it so a press parked against a dead loop is not spent by the next.
static BACK_PRESS_PENDING: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Wakes the winit loop for [`BACK_PRESS_PENDING`], or `None` when there is no
/// loop to wake.
///
/// **Replaceable, not write-once.** `android_main` is tied to the lifetime of
/// the *Activity*, and `run_app` consumes the `EventLoop`, so a `OnceLock`
/// would keep the corpse and every later back press would fall through to the
/// Java minimise. The `Mutex` is for that replacement and nothing else.
///
/// The sensor threads use `Window::request_redraw` instead: they need a
/// *frame*, a back press needs an *iteration*, and `send_event` guarantees
/// one where a bare `PollEvent::Wake` is dropped when nothing is outstanding.
#[cfg(target_os = "android")]
static EVENT_LOOP_PROXY: std::sync::Mutex<Option<winit::event_loop::EventLoopProxy<()>>> =
    std::sync::Mutex::new(None);

/// Pins the "not there to make the proxy shareable" half of the note above. If
/// this fails the lock has acquired a second reason, and the note needs
/// rewriting rather than the assertion deleting.
#[cfg(target_os = "android")]
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<winit::event_loop::EventLoopProxy<()>>();
};

/// Take the proxy slot, recovering from poisoning rather than reporting "no
/// loop": that would downgrade every later back press to a bare minimise.
#[cfg(target_os = "android")]
fn event_loop_proxy()
-> std::sync::MutexGuard<'static, Option<winit::event_loop::EventLoopProxy<()>>> {
    EVENT_LOOP_PROXY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Install (or clear) the proxy back presses are handed to. Clears
/// [`BACK_PRESS_PENDING`] with it: on a second `android_main` a press may be
/// parked against the loop that just died.
#[cfg(target_os = "android")]
pub(super) fn set_event_loop_proxy(proxy: Option<winit::event_loop::EventLoopProxy<()>>) {
    *event_loop_proxy() = proxy;
    BACK_PRESS_PENDING.store(false, std::sync::atomic::Ordering::Release);
}

/// Park a back press and wake the event loop for it. `false` means there is no
/// live loop to hand it to, and the Java caller minimises for itself.
fn post_back_press() -> bool {
    #[cfg(target_os = "android")]
    {
        use std::sync::atomic::Ordering;

        let slot = event_loop_proxy();
        let Some(proxy) = slot.as_ref() else {
            log::warn!("back press with no event loop installed yet; minimising in Java");
            return false;
        };

        BACK_PRESS_PENDING.store(true, Ordering::Release);
        // `send_event` queues *and* wakes. Waking alone would not do: winit's
        // Android backend discards a `PollEvent::Wake` unless the loop is
        // running and something is already outstanding.
        if proxy.send_event(()).is_ok() {
            return true;
        }

        // The loop closed between the check and the send. The flag stays set:
        // clearing it would erase a *previous* press that parked and has not
        // been drained.
        log::warn!("the event loop is gone; minimising in Java");
    }
    false
}

/// Take the parked back press, if there is one. Injected into
/// `AndroidPlatform` as its `poll_back_press`, read from `App::about_to_wait`.
pub(super) fn take_back_press() -> bool {
    BACK_PRESS_PENDING.swap(false, std::sync::atomic::Ordering::Acquire)
}

/// `BackHandler.nativeBackPressed()` — the predictive-back gesture, on the Java
/// UI thread. Returns whether the press reached the Rust funnel; `false` is
/// the Java side's cue to minimise for itself. Deliberately does nothing but
/// park and wake: this is not the `android_main` thread, so it cannot touch
/// `App`, and the framework is waiting on it. Raw pointers because neither
/// `jni::Env` nor `JClass` is used.
#[unsafe(no_mangle)]
#[allow(unsafe_code, reason = "the JNI ABI symbol BackHandler binds")]
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
