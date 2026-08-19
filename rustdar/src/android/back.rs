//! Predictive back (BackHandler.java): the parked press, the event-loop wake,
//! and the one JNI symbol this app exports.

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
///
/// # Why the sensor threads do not use this
///
/// They wake the loop through `Window::request_redraw` instead (see
/// [`start_location_thread`]), and the split is about what each side needs
/// delivered rather than about which mechanism wakes harder.
///
/// A back press is collected in `App::about_to_wait`, which every dispatch
/// reaches, so it needs an *iteration* — and `send_event` is what guarantees
/// one here, because it queues a user event and the Android backend drops a
/// bare `PollEvent::Wake` when nothing is outstanding. A GPS fix, a heading or
/// a theme reading is drained by `App::poll_platform_state`, which runs only
/// from `handle_redraw`, so it needs a *frame*; `user_event` is not overridden
/// on `App` and would not produce one. `request_redraw` sets `redraw_flag`
/// before its own `waker.wake()`, so it survives the same drop this one
/// documents, and it is `Send + Sync` on every backend without a per-entry-point
/// proxy to thread through the three that build event loops.
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
pub(super) fn set_event_loop_proxy(proxy: Option<winit::event_loop::EventLoopProxy<()>>) {
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
pub(super) fn take_back_press() -> bool {
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
