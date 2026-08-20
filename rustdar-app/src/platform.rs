//! The seam between the portable app and whatever OS it is running on.
//!
//! Only the trait lives here. Every concrete implementation lives beside the
//! entry point that constructs it, because this crate must build for targets
//! whose bridges it has never heard of.

/// What a [`RedrawWaker`] fires, once there is a window to fire it at. `Arc`
/// rather than `Box` so [`RedrawWaker::wake`] can drop the guard before calling.
type WakeFn = std::sync::Arc<dyn Fn() + Send + Sync>;

/// A handle a foreign thread uses to ask the event loop for a frame.
///
/// The app runs on `ControlFlow::Wait` and `App::poll_platform_state` — the one
/// thing that drains the sensor and theme channels — runs only from
/// `handle_redraw`, so a value pushed by a thread the loop knows nothing about is
/// invisible until something else produces a frame. Five producers share the gap.
///
/// A redraw request and not the event-loop proxy: `EventLoopProxy::send_event`
/// (winit 0.30 has no `wake_up`) delivers to `ApplicationHandler::user_event`,
/// which `App` does not override, so it produces an iteration and not a frame.
/// Measured against a real winit 0.30.13 loop on `ControlFlow::Wait` under X11:
/// one `request_redraw` from a foreign thread delivered `RedrawRequested` in
/// 29–43 µs over three runs; two `send_event`s produced zero.
///
/// A slot rather than a window because producers are handed a waker while
/// `App::window` is still `None`. `App::suspended` empties it so no sensor thread
/// outlives the destroyed window; `resumed` refills it.
#[derive(Clone, Default)]
pub struct RedrawWaker {
    slot: std::sync::Arc<std::sync::Mutex<Option<WakeFn>>>,
}

const _: () = {
    const fn assert_shareable<T: Send + Sync + Clone>() {}
    assert_shareable::<RedrawWaker>();
};

impl std::fmt::Debug for RedrawWaker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let attached = self
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some();
        f.debug_struct("RedrawWaker")
            .field("attached", &attached)
            .finish()
    }
}

impl RedrawWaker {
    /// A waker with no window behind it yet; waking is a no-op until one arrives.
    pub fn new() -> Self {
        Self::default()
    }

    /// Install `wake` as what every outstanding handle fires.
    pub(crate) fn install(&self, wake: impl Fn() + Send + Sync + 'static) {
        *self
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(std::sync::Arc::new(wake));
    }

    /// Empty the slot, dropping whatever it was holding — the window included.
    pub(crate) fn detach(&self) {
        *self
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }

    /// Ask the event loop for a frame, if there is a window to ask through.
    ///
    /// The guard is dropped before the call: `notify_redraw` wraps
    /// `request_redraw` in `catch_unwind` because X11's copy panics once the loop
    /// has closed, and a `Mutex` poisoned by an unwind under the guard would
    /// silently drop every subsequent wake from every producer.
    pub fn wake(&self) {
        let wake = self
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(wake) = wake {
            wake();
        }
    }
}

/// Drain all pending messages from `rx`, returning the last one (if any).
///
/// Sensor and theme channels are state, not events: only the newest value matters.
pub fn drain_latest<T>(rx: &std::sync::mpsc::Receiver<T>) -> Option<T> {
    let mut latest = None;
    while let Ok(val) = rx.try_recv() {
        latest = Some(val);
    }
    latest
}

/// Spawn a named thread that samples `read` every `interval` and forwards the
/// result, until the returned `Receiver` is dropped.
///
/// For state a platform only exposes by polling. Android's theme is the one case
/// today: NativeActivity never emits `WindowEvent::ThemeChanged`.
///
/// Every sample is sent, not just the ones that differ: that is what lets the
/// thread notice the receiver is gone, since a disconnected `mpsc::Sender` is
/// only observable by trying to send. Only a change wakes, since waking on every
/// sample would be a full frame every `interval` for the life of the process.
#[cfg(not(target_arch = "wasm32"))]
pub fn spawn_state_poller<T, F>(
    name: &str,
    interval: std::time::Duration,
    read: F,
    wake: RedrawWaker,
) -> std::io::Result<std::sync::mpsc::Receiver<T>>
where
    T: Clone + PartialEq + Send + 'static,
    F: Fn() -> T + Send + 'static,
{
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            // `None` until the first read, which makes the first sample a change.
            let mut last: Option<T> = None;
            loop {
                let sample = read();
                let changed = last.as_ref() != Some(&sample);
                if sender.send(sample.clone()).is_err() {
                    break;
                }
                last = Some(sample);
                if changed {
                    wake.wake();
                }
                std::thread::sleep(interval);
            }
        })?;
    Ok(receiver)
}

/// Platform-specific behavior abstracted behind a common trait.
pub trait PlatformBridge {
    /// Poll for theme changes from the OS. `Some(is_dark)` on a change.
    fn poll_theme(&mut self) -> Option<bool>;

    /// Poll for compass heading updates. Returns degrees (0–360) if available.
    fn poll_heading(&mut self) -> Option<f32>;

    /// Query system bar insets (top, bottom, left, right) in logical pixels.
    fn query_insets(&self) -> Option<(f32, f32, f32, f32)>;

    /// Handle the back button press. Returns `true` if the platform consumed
    /// the event (e.g. Android moveTaskToBack), `false` if the app should exit.
    fn handle_back(&self) -> bool;

    /// Take a back press the platform delivered outside the window's input queue.
    ///
    /// Android's `OnBackInvokedDispatcher` is the only source: once the app opts in
    /// to predictive back, back is handed to a Java callback on the UI thread,
    /// which parks the press and wakes the loop. Consuming, because this is polled
    /// every loop iteration.
    fn poll_back_press(&mut self) -> bool {
        false
    }

    /// Set the reader for [`poll_back_press`](Self::poll_back_press) (Android only).
    ///
    /// Injected because the flag it reads is written by a JNI entry point in the
    /// `rustdar` crate's cfg(android) back module.
    fn set_back_press_taker(&mut self, _taker: fn() -> bool) {}

    /// Tell the platform whether the next back press has something to close.
    ///
    /// Android's predictive-back dispatcher only lets an app decline a press by
    /// not being registered when it arrives — `onBackInvoked()` returns void —
    /// and declining is what buys the system's own back-to-home preview. So the
    /// claim has to be published *before* the press, and it has to be true at
    /// every transition that opens or closes something. Pushed on change only,
    /// at the end of a frame; a per-frame JNI hop is what this shape avoids.
    ///
    /// Every other platform ignores it: back is a key there, answered when it
    /// arrives — and so, today, does Android, which has not opted into the
    /// dispatcher (see `BackHandler.kt` and the manifest for the measurement
    /// that keeps it opted out). The claim is published anyway so that the day
    /// the opt-in becomes mandatory nothing but a manifest attribute changes.
    fn set_back_claimed(&mut self, _claimed: bool) {}

    /// Set the sink [`set_back_claimed`](Self::set_back_claimed) forwards to
    /// (Android only).
    ///
    /// Injected for the same reason as
    /// [`set_back_press_taker`](Self::set_back_press_taker): the far end is a
    /// JNI static call in the `rustdar` crate's cfg(android) back module, and
    /// this trait has to compile for targets that never heard of JNI.
    fn set_back_claim_reporter(&mut self, _reporter: fn(bool)) {}

    fn detect_dark_theme(&self) -> bool;

    fn set_back_handler(&mut self, handler: fn());

    fn set_zone_cache_dir(&mut self, dir: std::path::PathBuf);

    fn zone_cache_dir(&self) -> Option<&std::path::Path>;

    fn set_config_dir(&mut self, dir: std::path::PathBuf);

    /// Where this platform persists small blobs, or `None` if the platform has not
    /// been told where yet (Android learns its data path only after startup).
    /// A store rather than a directory, so a web bridge can hand back
    /// `localStorage`.
    fn kv(&self) -> Option<Box<dyn rustdar_kv::KvStore>>;

    /// This device's IANA timezone name, e.g. `"America/Denver"`.
    ///
    /// Used to pick a starting radar site on a first run, without asking for a
    /// location permission — see [`crate::location_hint`].
    fn iana_timezone(&self) -> Option<String> {
        None
    }

    /// Request application exit. Returns `true` if the platform requires
    /// `std::process::exit` (Android), `false` for normal event-loop exit.
    fn needs_process_exit(&self) -> bool;

    /// Whether quitting is something this platform lets an app do at all.
    /// `false` on iOS: `exit()` is an App Store rejection, and UIKit's run loop
    /// never unwinds back to `run_app`'s caller.
    fn supports_exit(&self) -> bool {
        true
    }

    /// Adjust the attributes the main window is created with.
    ///
    /// Only the web bridge has anything to add: winit's web backend must be told
    /// which `<canvas>` the window is before it exists.
    fn window_attributes(
        &self,
        attributes: winit::window::WindowAttributes,
    ) -> winit::window::WindowAttributes {
        attributes
    }

    /// Hand the bridge the handle its own background threads wake the loop with.
    ///
    /// Called once from `App::with_instance`, before any window exists — which is
    /// why [`RedrawWaker`] is a slot rather than a window.
    fn set_redraw_waker(&mut self, _waker: RedrawWaker) {}

    /// Set a receiver for compass heading updates (Android only, no-op on desktop).
    fn set_heading_receiver(&mut self, _receiver: std::sync::mpsc::Receiver<f32>) {}

    /// Set a callback that queries system bar insets (Android only, no-op on desktop).
    fn set_insets_querier(&mut self, _querier: fn() -> (f32, f32, f32, f32)) {}

    /// Set a callback that reads the OS dark-theme preference (Android only).
    ///
    /// Android reads this over JNI, which needs `unsafe` and the process
    /// `JavaVM`; both stay in the `rustdar` crate's cfg(android) modules because
    /// this crate must compile for targets that have never heard of JNI.
    fn set_theme_detector(&mut self, _detector: fn() -> bool) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    // No test here can build a `winit::Window`; what `App` puts in the slot is
    // pinned by a source probe in `app.rs`.

    /// `Arc<AtomicUsize>` because the slot's contents must be `Send + Sync`.
    fn counting_wake(waker: &RedrawWaker) -> std::sync::Arc<std::sync::atomic::AtomicUsize> {
        let count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let probe = std::sync::Arc::clone(&count);
        waker.install(move || {
            probe.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        count
    }

    fn woke(count: &std::sync::atomic::AtomicUsize) -> usize {
        count.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Producers are handed their waker while `App::window` is still `None`.
    #[test]
    fn a_waker_handed_out_before_the_window_exists_still_finds_it() {
        let waker = RedrawWaker::new();
        let held_by_the_producer = waker.clone();
        held_by_the_producer.wake();

        let woken = counting_wake(&waker);

        held_by_the_producer.wake();
        assert_eq!(
            woke(&woken),
            1,
            "the copy taken before the window existed never saw it appear, so \
             every fix that producer ever sends is invisible until something \
             else draws a frame"
        );
    }

    /// Waking before there is anything to wake must be quiet, not fatal.
    #[test]
    fn a_wake_with_no_window_yet_is_a_no_op() {
        RedrawWaker::new().wake();
    }

    /// `App::suspended` clears `window` and `state`, so the drop is the assertion.
    #[test]
    fn a_waker_stops_holding_the_window_once_the_app_is_suspended() {
        let waker = RedrawWaker::new();
        let window = std::sync::Arc::new(());
        let held = std::sync::Arc::clone(&window);
        waker.install(move || {
            let _ = &held;
        });
        assert_eq!(std::sync::Arc::strong_count(&window), 2);

        waker.detach();

        assert_eq!(
            std::sync::Arc::strong_count(&window),
            1,
            "the window is still referenced from the slot after a suspend, so \
             the surface it belongs to outlives it"
        );
        waker.clone().wake();
    }

    /// `notify_redraw` wraps `request_redraw` in `catch_unwind` because X11's
    /// panics once the loop has closed; unwinding under a held guard would poison
    /// the mutex, so `wake` releases the guard before it calls.
    #[test]
    fn a_panicking_wake_does_not_silence_later_ones() {
        let waker = RedrawWaker::new();
        waker.install(|| panic!("request_redraw on a closed X11 loop"));

        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| waker.wake()));
        assert!(unwound.is_err(), "the fixture did not actually panic");

        let woken = counting_wake(&waker);
        waker.wake();
        assert_eq!(
            woke(&woken),
            1,
            "one unwinding wake poisoned the slot, so every producer's every \
             later wake is dropped"
        );
    }

    #[test]
    fn drain_latest_returns_the_newest_value() {
        let (tx, rx) = std::sync::mpsc::channel();
        for v in [1, 2, 3] {
            tx.send(v).unwrap();
        }

        assert_eq!(drain_latest(&rx), Some(3));
    }

    #[test]
    fn drain_latest_is_empty_once_drained() {
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(7).unwrap();

        assert_eq!(drain_latest(&rx), Some(7));
        assert_eq!(drain_latest(&rx), None, "the value must not be replayed");
    }

    #[test]
    fn drain_latest_on_an_empty_channel_is_none() {
        let (_tx, rx) = std::sync::mpsc::channel::<u8>();
        assert_eq!(drain_latest(&rx), None);
    }

    // wasm32 has no threads, so the definition is absent there.
    #[cfg(not(target_arch = "wasm32"))]
    mod poller {
        use super::super::{RedrawWaker, spawn_state_poller};
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use std::time::Duration;

        fn counted() -> (RedrawWaker, Arc<AtomicUsize>) {
            let waker = RedrawWaker::new();
            let count = Arc::new(AtomicUsize::new(0));
            let probe = Arc::clone(&count);
            waker.install(move || {
                probe.fetch_add(1, Ordering::SeqCst);
            });
            (waker, count)
        }

        #[test]
        fn poller_sends_an_initial_sample_without_waiting() {
            let rx = spawn_state_poller(
                "test-initial",
                Duration::from_secs(3600),
                || true,
                RedrawWaker::new(),
            )
            .unwrap();

            assert_eq!(
                rx.recv_timeout(Duration::from_secs(5)),
                Ok(true),
                "first sample must not be delayed by one interval"
            );
        }

        #[test]
        fn poller_reports_a_change() {
            let state = Arc::new(AtomicBool::new(false));
            let probe = Arc::clone(&state);
            let rx = spawn_state_poller(
                "test-change",
                Duration::from_millis(5),
                move || probe.load(Ordering::Relaxed),
                RedrawWaker::new(),
            )
            .unwrap();

            assert_eq!(rx.recv_timeout(Duration::from_secs(5)), Ok(false));
            state.store(true, Ordering::Relaxed);

            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            loop {
                match rx.recv_timeout(Duration::from_secs(5)) {
                    Ok(true) => break,
                    Ok(false) => assert!(
                        std::time::Instant::now() < deadline,
                        "poller never reported the flipped value"
                    ),
                    Err(e) => panic!("poller stopped early: {e:?}"),
                }
            }
        }

        #[test]
        fn a_theme_change_arriving_while_the_app_is_idle_asks_for_a_frame() {
            let (waker, woke) = counted();
            let state = Arc::new(AtomicBool::new(false));
            let probe = Arc::clone(&state);
            let rx = spawn_state_poller(
                "test-wake",
                Duration::from_millis(5),
                move || probe.load(Ordering::Relaxed),
                waker,
            )
            .unwrap();

            assert_eq!(rx.recv_timeout(Duration::from_secs(5)), Ok(false));
            let before = woke.load(Ordering::SeqCst);
            state.store(true, Ordering::Relaxed);

            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while rx.recv_timeout(Duration::from_secs(5)) != Ok(true) {
                assert!(
                    std::time::Instant::now() < deadline,
                    "poller never reported the flipped value"
                );
            }
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while woke.load(Ordering::SeqCst) <= before {
                assert!(
                    std::time::Instant::now() < deadline,
                    "the theme changed and nothing asked for the frame that \
                     would show it"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        #[test]
        fn an_unchanged_reading_does_not_ask_for_a_frame() {
            let (waker, woke) = counted();
            let rx = spawn_state_poller(
                "test-quiet",
                Duration::from_millis(5),
                || true, // deliberately constant
                waker,
            )
            .unwrap();

            assert_eq!(rx.recv_timeout(Duration::from_secs(5)), Ok(true));
            std::thread::sleep(Duration::from_millis(200));
            assert!(
                rx.try_recv().is_ok(),
                "the poller stopped sending, so this proves nothing about waking"
            );

            assert_eq!(
                woke.load(Ordering::SeqCst),
                1,
                "a reading that never changed woke the loop anyway, which on \
                 Android is a frame every interval for the life of the process"
            );
        }

        #[test]
        fn poller_exits_when_the_receiver_is_dropped_and_the_value_never_changes() {
            let (probe_tx, probe_rx) = std::sync::mpsc::channel();
            let rx = spawn_state_poller(
                "test-exit",
                Duration::from_millis(5),
                move || {
                    let _ = probe_tx.send(());
                    true // deliberately constant
                },
                RedrawWaker::new(),
            )
            .unwrap();

            assert_eq!(rx.recv_timeout(Duration::from_secs(5)), Ok(true));
            drop(rx);

            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            let mut exited = false;
            while std::time::Instant::now() < deadline {
                match probe_rx.recv_timeout(Duration::from_millis(50)) {
                    Ok(()) => continue,
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                        exited = true;
                        break;
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                }
            }
            assert!(
                exited,
                "poller must stop once its receiver is dropped, even if the \
                 sampled value never changes"
            );
        }

        #[test]
        fn poller_stops_sampling_after_exit() {
            let calls = Arc::new(AtomicUsize::new(0));
            let probe = Arc::clone(&calls);
            let rx = spawn_state_poller(
                "test-quiesce",
                Duration::from_millis(5),
                move || {
                    probe.fetch_add(1, Ordering::SeqCst);
                    true
                },
                RedrawWaker::new(),
            )
            .unwrap();

            assert_eq!(rx.recv_timeout(Duration::from_secs(5)), Ok(true));
            drop(rx);

            std::thread::sleep(Duration::from_millis(200));
            let settled = calls.load(Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(200));

            assert_eq!(
                calls.load(Ordering::SeqCst),
                settled,
                "detector was still being called after the receiver was dropped"
            );
        }
    }
}
