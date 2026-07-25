//! The seam between the portable app and whatever OS it is running on.
//!
//! Only the *trait* lives here. Every concrete implementation lives beside the
//! entry point that constructs it (`rustdar-platform` for desktop and Android),
//! because this crate must build for targets whose bridges it has never heard
//! of — and because a crate that named them would have to depend on the crate
//! that depends on it.

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
/// Returns the spawn error rather than panicking, so a bridge can degrade to its
/// synchronous path instead of taking the process down.
#[cfg(not(target_arch = "wasm32"))]
pub fn spawn_state_poller<T, F>(
    name: &str,
    interval: std::time::Duration,
    read: F,
) -> std::io::Result<std::sync::mpsc::Receiver<T>>
where
    T: Send + 'static,
    F: Fn() -> T + Send + 'static,
{
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            if sender.send(read()).is_err() {
                return;
            }
            loop {
                std::thread::sleep(interval);
                if sender.send(read()).is_err() {
                    break;
                }
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

    /// Request application exit. Returns `true` if the platform requires
    /// `std::process::exit` (Android), `false` for normal event-loop exit.
    fn needs_process_exit(&self) -> bool;

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
    /// and `set_back_handler` already use, and it is what lets `rustdar-platform`
    /// keep `#![forbid(unsafe_code)]`.
    ///
    /// Desktop and web answer `detect_dark_theme` themselves and ignore this.
    fn set_theme_detector(&mut self, _detector: fn() -> bool) {}

    /// Start the desktop serial GPS reader (no-op on Android).
    fn start_gps(&mut self, _config: &rustdar_gps::GpsConfig) {}

    /// Stop the desktop serial GPS reader (no-op on Android).
    fn stop_gps(&mut self) {}

    /// Whether the desktop serial GPS reader is currently running.
    fn gps_active(&self) -> bool { false }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;

    /// The consumer has nothing to show until the first sample lands, so it must
    /// not wait out an interval first.
    #[test]
    fn poller_sends_an_initial_sample_without_waiting() {
        let rx = spawn_state_poller("test-initial", Duration::from_secs(3600), || true).unwrap();

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
        let rx =
            spawn_state_poller("test-change", Duration::from_millis(5), move || {
                probe.load(Ordering::Relaxed)
            })
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

    /// The regression this exists for: a send-on-change loop never retries after
    /// the value settles, so it never observes the disconnect and runs forever.
    /// Dropping the receiver must stop the thread even though nothing changed.
    #[test]
    fn poller_exits_when_the_receiver_is_dropped_and_the_value_never_changes() {
        // The closure owns a Sender; the thread dropping the closure on exit is
        // what disconnects this probe channel. That makes "thread exited"
        // observable without sleeping on a guess.
        let (probe_tx, probe_rx) = std::sync::mpsc::channel();
        let rx = spawn_state_poller("test-exit", Duration::from_millis(5), move || {
            let _ = probe_tx.send(());
            true // deliberately constant
        })
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
            "poller must stop once its receiver is dropped, even if the sampled \
             value never changes"
        );
    }

    /// The thread must stop calling the detector after it exits — a leaked
    /// thread would keep a JVM attachment alive on Android.
    #[test]
    fn poller_stops_sampling_after_exit() {
        let calls = Arc::new(AtomicUsize::new(0));
        let probe = Arc::clone(&calls);
        let rx = spawn_state_poller("test-quiesce", Duration::from_millis(5), move || {
            probe.fetch_add(1, Ordering::SeqCst);
            true
        })
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
