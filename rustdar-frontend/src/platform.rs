//! The seam between the portable app and whatever OS it is running on.
//!
//! Only the *trait* lives here. Every concrete implementation lives beside the
//! entry point that constructs it (`rustdar-platform` for desktop and Android),
//! because this crate must build for targets whose bridges it has never heard
//! of — and because a crate that named them would have to depend on the crate
//! that depends on it.

/// What a [`RedrawWaker`] fires, once there is a window to fire it at.
///
/// `Arc` rather than `Box` so [`RedrawWaker::wake`] can clone it out of the slot
/// and drop the guard *before* calling it; see the poisoning note there.
type WakeFn = std::sync::Arc<dyn Fn() + Send + Sync>;

/// A handle a foreign thread uses to ask the event loop for a frame.
///
/// # The gap this closes
///
/// The app runs on `ControlFlow::Wait`, and `App::poll_platform_state` — the
/// one thing that drains the sensor and theme channels — runs only from
/// `handle_redraw`, i.e. only on a `RedrawRequested` frame. A value pushed into
/// one of those channels by a thread the loop knows nothing about is therefore
/// invisible until something *else* happens to produce a frame. Five producers
/// share the gap: the serial GPS reader, Android's location and compass
/// threads, the Android theme poller ([`spawn_state_poller`]), and the
/// browser's `watchPosition` callback.
///
/// It is masked today by auto-poll, which defaults on and is persisted, so
/// `handle_redraw` re-arms a redraw every frame and the loop free-runs at the
/// refresh rate — which is a power bug on a handheld, not a fix. Turn
/// auto-refresh off, and the latency on a GPS fix or a light/dark switch is
/// unbounded.
///
/// # Why a redraw request and not the event-loop proxy
///
/// `rustdar-android` already keeps an [`EventLoopProxy`] for predictive back,
/// and it is right there. It stays there, and this is not it:
///
/// * `EventLoopProxy::send_event` (winit 0.30 has no `wake_up`; that is 0.31)
///   delivers to `ApplicationHandler::user_event`, which `App` does not
///   override — so a proxy wake produces an iteration, not a frame, and the
///   channel drain lives on the frame. Back gets away with this because
///   `about_to_wait` is where it is collected; a sensor value is not.
/// * A proxy belongs to one `EventLoop`, and only the entry point that built
///   the loop has one. `rustdar-platform`, `rustdar-web` and this crate would
///   each need their own plumbing for it. `Window` is `Send + Sync` on every
///   backend and `App` already holds one.
/// * `Window::request_redraw` wakes a parked loop everywhere: an X11 redraw
///   channel send, a Wayland ping, `RedrawWindow` on Windows, a main-thread
///   dispatch on macOS/iOS, `requestAnimationFrame` in the browser. On Android
///   it is *stronger* than a bare wake — it sets `redraw_flag` before
///   `waker.wake()`, and the backend drops a `PollEvent::Wake` unless a redraw
///   or a user event is already outstanding, so the flag is what survives.
///
/// Measured against a real winit 0.30.13 loop on `ControlFlow::Wait` under
/// X11, poked from a thread it knew nothing about. Idle: one opening frame,
/// then nothing. One `request_redraw` from that thread: `RedrawRequested`
/// delivered 29–43 µs later, over three runs. Two `send_event`s from it: two
/// `user_event`s, two `about_to_wait`s, and **zero** `RedrawRequested`.
///
/// This is also the shape the rest of the crate already uses: `offload`'s jobs
/// "send on an `mpsc::Sender` and call `notify_redraw`", and
/// `ChunkNotifier::sync_sites` takes a `wake` for exactly this reason. Sensors
/// are the case it was never applied to, because they are wired *before a
/// window exists*.
///
/// # Why a slot, and why it empties
///
/// `DesktopPlatform::start_gps`, `android_main` and the browser entry all hand
/// their producer a waker while `App::window` is still `None`, so a snapshot of
/// the window would be a snapshot of nothing. Every handle is a clone of one
/// `Arc`, so filling the slot in `create_window` reaches producers that took
/// their copy at startup.
///
/// `App::suspended` empties it, and that is not tidiness. Suspend sets
/// `window = None` and `state = None` precisely so no wgpu surface outlives the
/// destroyed window; a slot that *survived* would leave five sensor threads
/// holding an `Arc<Window>` whose `ANativeWindow` is gone. Surviving is the
/// bug, not the virtue. `resumed` refills it through `create_window`.
///
/// # What is in the slot
///
/// The action, not the window. The window is what `App` captures in it — the
/// `notify_redraw` call is written there and pinned by a source probe — and
/// keeping the indirection is what makes this type's own guarantees checkable
/// on a host, where no test in this repo can build a `Window`: that a wake
/// before `install` is a no-op, that `detach` really drops what it was holding,
/// and that an unwinding wake does not silence the next one.
///
/// [`EventLoopProxy`]: winit::event_loop::EventLoopProxy
#[derive(Clone, Default)]
pub struct RedrawWaker {
    slot: std::sync::Arc<std::sync::Mutex<Option<WakeFn>>>,
}

/// Pins what the producers rely on: one waker is shared by five threads, and
/// two of them (the serial reader, the theme poller) take it by clone into a
/// `std::thread::spawn`. Losing any of the three turns a wiring change into a
/// compile error here rather than a redesign at the call sites.
const _: () = {
    const fn assert_shareable<T: Send + Sync + Clone>() {}
    assert_shareable::<RedrawWaker>();
};

impl std::fmt::Debug for RedrawWaker {
    /// Says whether the slot is filled, which is the only thing about a waker
    /// anyone can inspect — the contents are a closure.
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
    /// A waker with no window behind it yet. Waking is a no-op until one
    /// arrives, which is the state every producer is handed its copy in.
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
    ///
    /// See the type's note: this is what `App::suspended` calls, and the point
    /// of it is the *drop*, not the silence.
    pub(crate) fn detach(&self) {
        *self
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }

    /// Ask the event loop for a frame, if there is a window to ask through.
    ///
    /// # Two rules about the lock
    ///
    /// The guard is dropped before the call. `notify_redraw` wraps
    /// `request_redraw` in `catch_unwind` because X11's copy panics once the
    /// loop has closed — and a `std::sync::Mutex` poisoned by an unwind *under
    /// the guard* would make every later `unwrap()` here fail, silently
    /// dropping every subsequent wake from every producer. That is strictly
    /// worse than the bug this type exists to fix, so the unwind is arranged to
    /// happen where no guard is held.
    ///
    /// The `unwrap_or_else` is then belt to that braces, and deliberately not
    /// removed as unreachable: it is what keeps a *future* panic under the lock
    /// from re-introducing the same silence. Same reasoning as
    /// `rustdar_android::event_loop_proxy`, which recovers for the same reason.
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
/// Sensor and theme channels are state, not events: only the newest value
/// matters and the ones behind it are already stale. Draining rather than
/// taking one per frame keeps a fast producer from building a backlog the UI
/// then walks through one frame at a time.
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
/// today: NativeActivity never emits `WindowEvent::ThemeChanged`, so re-reading
/// `Configuration.uiMode` is the *only* way a light/dark switch is ever noticed.
///
/// Every sample is sent, not just the ones that differ from the last. Two
/// reasons, and the second is the load-bearing one:
///
///   * Nothing downstream sees the repeats. [`drain_latest`] collapses the
///     backlog to one value per frame and the consumers compare against their
///     own cached copy before acting.
///   * It is what lets the thread notice the receiver is gone. A loop that only
///     sends on change stops calling `send` once the value settles, and a
///     disconnected `mpsc::Sender` is *only* observable by trying to send — so
///     on a device whose theme never changes, such a thread would poll forever
///     and keep a permanent JVM attachment alive for a bridge that no longer
///     exists. (`attach_current_thread` in jni 0.22 is the permanent attach;
///     `attach_current_thread_for_scope` is the scoped one.)
///
/// The first sample is sent immediately rather than after `interval`: the
/// consumer has no value at all until it arrives.
///
/// `wake` is what makes a sample visible at all. The theme channel is drained by
/// `App::poll_platform_state`, which runs only on a frame, and under
/// `ControlFlow::Wait` this thread is the only thing that knows there is a
/// reason to draw one — so without it a light/dark switch waits for the next
/// unrelated event, which with auto-refresh off may never come.
///
/// **Only a change wakes**, and the asymmetry with "every sample is sent" above
/// is the point. Sending unconditionally is what lets the thread notice a
/// dropped receiver; *waking* unconditionally would be a full frame — egui pass,
/// texture sampling, present — every `interval` for the life of the process, on
/// the one platform this poller exists for and the one where that is a battery
/// cost rather than a rounding error. Nothing is lost by holding back: the
/// consumer compares against its own cached copy anyway (`App::adopt_theme`
/// returns `false` and requests nothing when the reading has not moved), so an
/// unchanged sample has no frame to justify.
///
/// Returns the spawn error rather than panicking, so a bridge can degrade to its
/// synchronous path instead of taking the process down.
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
            // `None` until the first read, and that is what makes the first
            // sample count as a change: the consumer has no value at all yet.
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
/// Keeps `#[cfg(target_os = "android")]` blocks out of `app.rs`.
pub trait PlatformBridge {
    /// Poll for theme changes from the OS. Returns `Some(is_dark)` when
    /// a change is detected, `None` otherwise.
    fn poll_theme(&mut self) -> Option<bool>;

    /// Poll for GPS fix updates. Returns the latest [`GpsFix`](rustdar_gps::GpsFix) if available.
    fn poll_gps_fix(&mut self) -> Option<rustdar_gps::GpsFix>;

    /// Poll for compass heading updates. Returns degrees (0–360) if available.
    fn poll_heading(&mut self) -> Option<f32>;

    /// Query system bar insets (top, bottom, left, right) in logical pixels.
    fn query_insets(&self) -> Option<(f32, f32, f32, f32)>;

    /// Handle the back button press. Returns `true` if the platform consumed
    /// the event (e.g. Android moveTaskToBack), `false` if the app should exit.
    fn handle_back(&self) -> bool;

    /// Take a back press the platform delivered *outside* the window's input
    /// queue, if one is waiting.
    ///
    /// Android's `OnBackInvokedDispatcher` is the only source. Once the app
    /// opts in to predictive back — explicitly, or by targeting an SDK that
    /// opts in for it — back stops arriving as `KEYCODE_BACK` and is handed to
    /// a Java callback on the UI thread instead, which is not the thread `App`
    /// lives on. The callback therefore parks the press and wakes the loop, and
    /// this is where the loop picks it up; from there it takes the same route
    /// as Escape and as legacy back. See `BackHandler.java`.
    ///
    /// Consuming, like [`InputHandler::take_back_out_press`]: this is polled
    /// every loop iteration, and a non-consuming read would spend one press on
    /// every layer the UI has.
    ///
    /// Defaulted because no other platform has a second delivery route: the
    /// desktop, iOS and the browser all deliver back (or Escape) as an input
    /// event and nothing else.
    ///
    /// [`InputHandler::take_back_out_press`]: crate::input::InputHandler::take_back_out_press
    fn poll_back_press(&mut self) -> bool {
        false
    }

    /// Set the reader for [`poll_back_press`](Self::poll_back_press) (Android
    /// only, no-op elsewhere).
    ///
    /// Injected for the same reason [`set_theme_detector`](Self::set_theme_detector)
    /// is: the flag it reads is written by a JNI entry point, and that lives in
    /// `rustdar-android`, a crate this one cannot depend on.
    fn set_back_press_taker(&mut self, _taker: fn() -> bool) {}

    /// Detect the current system dark theme preference.
    fn detect_dark_theme(&self) -> bool;

    /// Set a callback for back-button behavior.
    fn set_back_handler(&mut self, handler: fn());

    /// Set persistent cache directory for zone geometries.
    fn set_zone_cache_dir(&mut self, dir: std::path::PathBuf);

    /// Get the zone cache directory.
    fn zone_cache_dir(&self) -> Option<&std::path::Path>;

    /// Set the config directory for UI config persistence.
    fn set_config_dir(&mut self, dir: std::path::PathBuf);

    /// Where to persist UI configuration, or `None` if this platform has not
    /// been told yet (Android learns its data path only after startup).
    ///
    /// Returns a store rather than a directory so the trait carries no
    /// filesystem assumption: a web bridge hands back a `localStorage` backend,
    /// which has no path to return.
    fn config_store(&self) -> Option<Box<dyn rustdar_egui::config_store::ConfigStore>>;

    /// This device's IANA timezone name, e.g. `"America/Denver"`.
    ///
    /// Used to pick a starting radar site on a first run, before any
    /// configuration exists and without asking for a location permission. See
    /// [`crate::location_hint`] for what that buys and what it does not.
    ///
    /// The default is `None`, which leaves the caller on its compiled-in
    /// default. A platform that cannot answer is not an error — it is a platform
    /// where the old behaviour stands.
    fn iana_timezone(&self) -> Option<String> {
        None
    }

    /// Request application exit. Returns `true` if the platform requires
    /// `std::process::exit` (Android), `false` for normal event-loop exit.
    fn needs_process_exit(&self) -> bool;

    /// Whether quitting is something this platform lets an app do at all.
    ///
    /// `false` on iOS: calling `exit()` is an App Store rejection, and UIKit's
    /// run loop never unwinds back to `run_app`'s caller, so `event_loop.exit()`
    /// would leave the app running with its exit path already taken.
    fn supports_exit(&self) -> bool {
        true
    }

    /// Adjust the attributes the main window is created with.
    ///
    /// Defaulted because only the web bridge has anything to add: winit's web
    /// backend has to be told which `<canvas>` the window *is* before the window
    /// exists, and that element is a `web_sys` type this crate cannot name
    /// without taking a browser dependency on every target. Returning the
    /// attributes unchanged is the correct behaviour everywhere else, so this is
    /// a hook rather than a required method.
    fn window_attributes(
        &self,
        attributes: winit::window::WindowAttributes,
    ) -> winit::window::WindowAttributes {
        attributes
    }

    /// Hand the bridge the handle its own background threads wake the loop
    /// with.
    ///
    /// Called once from `App::with_instance`, which is before any window exists
    /// — deliberately, and it is why [`RedrawWaker`] is a slot rather than a
    /// window. A bridge that starts a producer at construction (Android's theme
    /// poller, started from `set_theme_detector` during `android_main`) or on
    /// demand from a UI action (`start_gps`) has nowhere else to get one: the
    /// trait's other methods carry no window, and by the time `start_gps` is
    /// called the waker has to already be in the bridge's hands.
    ///
    /// A concrete type rather than `impl Fn()` because a `Box<dyn
    /// PlatformBridge>` cannot hold a generic method.
    ///
    /// Defaulted: iOS has no pollable channels yet, and the web bridge's one
    /// producer is wired by the entry point rather than by the bridge.
    fn set_redraw_waker(&mut self, _waker: RedrawWaker) {}

    /// Set a receiver for GPS fix updates (Android only, no-op on desktop).
    fn set_gps_fix_receiver(&mut self, _receiver: std::sync::mpsc::Receiver<rustdar_gps::GpsFix>) {}

    /// Set a receiver for compass heading updates (Android only, no-op on desktop).
    fn set_heading_receiver(&mut self, _receiver: std::sync::mpsc::Receiver<f32>) {}

    /// Set a callback that queries system bar insets (Android only, no-op on desktop).
    fn set_insets_querier(&mut self, _querier: fn() -> (f32, f32, f32, f32)) {}

    /// Set a callback that reads the OS dark-theme preference (Android only,
    /// no-op elsewhere).
    ///
    /// Android reads this over JNI, which needs `unsafe` and the process
    /// `JavaVM`. Both live in `rustdar-android`, the cdylib entry point — which
    /// depends on the bridge's own crate, so the bridge can never call into it
    /// directly. Injecting the reader is the same inversion `set_insets_querier`
    /// and `set_back_handler` already use, and it is what keeps JNI out of
    /// `rustdar-platform`: that crate is `#![deny(unsafe_code)]`, and its one
    /// scoped `allow` is the iOS entry symbol, not this.
    ///
    /// Desktop and web answer `detect_dark_theme` themselves and ignore this.
    fn set_theme_detector(&mut self, _detector: fn() -> bool) {}

    /// Start the desktop serial GPS reader (no-op on Android).
    fn start_gps(&mut self, _config: &rustdar_gps::GpsConfig) {}

    /// Stop the desktop serial GPS reader (no-op on Android).
    fn stop_gps(&mut self) {}

    /// Whether the desktop serial GPS reader is currently running.
    fn gps_active(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── RedrawWaker ─────────────────────────────────────────────────────
    //
    // No test in this repo can build a `winit::Window` — `headless()` has none
    // and the loop is never run — so what `App` puts in the slot is pinned by a
    // source probe in `app.rs`. What is checkable here is everything else the
    // type promises, and all three of those promises have a failure mode that
    // is silent on a device.

    /// A counting wake, and the count. `Arc<AtomicUsize>` rather than a `Cell`
    /// because the slot's contents must be `Send + Sync`, which is the whole
    /// reason a sensor thread can hold one.
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

    /// The reason this is a slot and not a window.
    ///
    /// `DesktopPlatform::start_gps`, `android_main` and the browser entry all
    /// hand a producer its waker while `App::window` is still `None`. A
    /// snapshot taken then would be a snapshot of nothing, and the producer
    /// would go on holding it for the life of the process.
    #[test]
    fn a_waker_handed_out_before_the_window_exists_still_finds_it() {
        let waker = RedrawWaker::new();
        // What a sensor thread takes at startup: a clone, of an empty waker.
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

    /// Waking before there is anything to wake must be quiet, not fatal: it is
    /// the normal state between process start and the first `resumed`, and the
    /// serial reader can be several seconds into a port probe by then.
    #[test]
    fn a_wake_with_no_window_yet_is_a_no_op() {
        RedrawWaker::new().wake();
    }

    /// `App::suspended` clears `window` and `state` so no wgpu surface outlives
    /// the destroyed window. A slot handed to five sensor threads that survived
    /// that would hold an `Arc<Window>` whose `ANativeWindow` is gone — so the
    /// *drop* is the assertion here, not the silence.
    #[test]
    fn a_waker_stops_holding_the_window_once_the_app_is_suspended() {
        let waker = RedrawWaker::new();
        // Stands in for the `Arc<Window>` the installed closure captures.
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
        // And a producer that kept its copy across the suspend is merely quiet.
        waker.clone().wake();
    }

    /// The failure mode a `Mutex` in `wake` would have introduced.
    ///
    /// `notify_redraw` wraps `request_redraw` in `catch_unwind` because X11's
    /// panics once the loop has closed. Unwinding *under* a held guard poisons
    /// the mutex, and then every later `lock().unwrap()` — from every one of
    /// the five producers — panics or silently gives up. That is strictly worse
    /// than the bug being fixed, so `wake` releases the guard before it calls.
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

    /// Only the newest value survives a drain — that is the whole point of it.
    #[test]
    fn drain_latest_returns_the_newest_value() {
        let (tx, rx) = std::sync::mpsc::channel();
        for v in [1, 2, 3] {
            tx.send(v).unwrap();
        }

        assert_eq!(drain_latest(&rx), Some(3));
    }

    /// A drained channel must report empty rather than replaying the last value.
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

    // ── spawn_state_poller ──────────────────────────────────────────────
    //
    // The Android theme bridge is the only caller and cannot run under test, so
    // these pin the parts of it that are plain Rust.
    //
    // Carries the *same* cfg as the function: wasm32 has no threads, so the
    // definition is absent there and ungated callers here broke
    // `--all-targets` on that target while the lib arm stayed green.
    #[cfg(not(target_arch = "wasm32"))]
    mod poller {
        use super::super::{RedrawWaker, spawn_state_poller};
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use std::time::Duration;

        /// A waker whose wakes can be counted, standing in for the one the
        /// window fills in. See the `RedrawWaker` tests above for why no test
        /// here can use a real window.
        fn counted() -> (RedrawWaker, Arc<AtomicUsize>) {
            let waker = RedrawWaker::new();
            let count = Arc::new(AtomicUsize::new(0));
            let probe = Arc::clone(&count);
            waker.install(move || {
                probe.fetch_add(1, Ordering::SeqCst);
            });
            (waker, count)
        }

        /// The consumer has nothing to show until the first sample lands, so it
        /// must not wait out an interval first.
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

        /// A flipped value must reach the consumer.
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

        /// The theme is Android's producer here, and a send alone reaches
        /// nothing: `poll_theme` is drained by `poll_platform_state`, which runs
        /// only on a frame. With auto-refresh off, a light/dark switch on a
        /// waker-less poller waits for the next unrelated event.
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
            // The wake is fired after the send the loop will drain, so by the
            // time the flipped value is in hand it has either happened or is
            // about to. Poll rather than sleep on a guess.
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

        /// The other half of that rule, and the one that costs battery.
        ///
        /// Every sample is sent whether or not it moved — that is what lets the
        /// thread notice a dropped receiver — but waking on every sample would
        /// be a whole frame every two seconds forever on Android, for a reading
        /// `adopt_theme` then discards as unchanged.
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
            // Long enough for ~40 more samples at a 5 ms interval.
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

        /// The regression this exists for: a send-on-change loop never retries
        /// after the value settles, so it never observes the disconnect and runs
        /// forever. Dropping the receiver must stop the thread even though
        /// nothing changed.
        #[test]
        fn poller_exits_when_the_receiver_is_dropped_and_the_value_never_changes() {
            // The closure owns a Sender; the thread dropping the closure on exit
            // is what disconnects this probe channel. That makes "thread exited"
            // observable without sleeping on a guess.
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

        /// The thread must stop calling the detector after it exits — a leaked
        /// thread would keep a JVM attachment alive on Android.
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

            // Let it notice the disconnect, then confirm the count has settled.
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
