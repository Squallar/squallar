//! The desktop/mobile OS arm of the facade: the provider from this module's
//! arm table, the permission it last reported, the channel it delivers on —
//! and, behind the `serial` feature, the serial GPS reader beside it.
//!
//! This is the shell's old `OsLocation` wiring (rustdar/src/platform.rs),
//! collapsed into the facade at WO-RL-4 with the serial verbs that used to
//! live on `DesktopPlatform`. `DesktopPlatform` and `IosPlatform` construct
//! one [`OsBackend`] each and hand it to the app inside a
//! [`LocationFacade`](crate::LocationFacade); nothing OS-specific leaks out —
//! the arm table in `mod.rs` is still the entire `cfg` surface.

use super::{OsLocationProvider as _, OsLocationReader, OsLocationSink};
use crate::provider::{LocationProvider, Wake, drain_latest};

/// The OS location service and everything the facade keeps beside it.
pub struct OsBackend {
    /// The platform's own location service.
    ///
    /// Built once, from [`set_wake`](LocationProvider::set_wake), and held for
    /// the life of the facade. **Not** dropped by `stop`: on two of the three
    /// platforms this value *is* the permission watcher, and a revocation made
    /// in system settings while delivery is off would go unnoticed if it were
    /// torn down with the stream. `None` means this target has no provider at
    /// all, which [`permission`](LocationProvider::permission) renders as
    /// `Unavailable`.
    provider: Option<OsLocationReader>,
    /// What the provider last reported.
    ///
    /// An atomic and not a `Cell`, because it is written from whatever thread
    /// the provider is given — a portal session thread, a WinRT RPC thread,
    /// the main run loop — and read from `permission`, which is a `&self`
    /// getter on the frame path. That rules out a `Cell` (not `Send`), a
    /// `Receiver` (cannot be drained through `&self`) and a `Mutex` (a lock
    /// on the frame path). See [`crate::encode_permission`].
    ///
    /// **It deliberately outlives every session.** A revocation arrives as a
    /// permission change *and* stops delivery, and the gate responds to
    /// `Denied` by calling stop — so a state that lived inside the thing
    /// being stopped would evaporate at exactly the moment it started to
    /// matter, the arm would fall back to "nobody has been asked", and the
    /// app would ask again straight into the refusal it just received.
    state: std::sync::Arc<std::sync::atomic::AtomicU8>,
    /// Receives fixes from the provider.
    ///
    /// A channel of its own rather than a second sender into the serial
    /// reader's, because on desktop the two sources have to be told apart:
    /// [`poll_fix`](LocationProvider::poll_fix) picks between them (see
    /// [`crate::prefer_fix`]) and cannot do that once they are merged.
    ///
    /// `None` until a provider exists, and never dropped afterwards — the
    /// provider keeps the matching `Sender` for the life of the facade, so the
    /// channel survives a stop and is reused by the next start.
    fixes: Option<std::sync::mpsc::Receiver<crate::Fix>>,
    /// Active serial GPS source (dropped to stop). The wrapper from
    /// [`crate::serial`]: the transport parses in its own vocabulary, and that
    /// module is where sentences become fixes.
    #[cfg(feature = "serial")]
    serial: Option<crate::serial::SerialFixReader>,
    /// Receives GPS fixes from the serial reader thread.
    #[cfg(feature = "serial")]
    serial_fixes: Option<std::sync::mpsc::Receiver<crate::Fix>>,
    /// The app's wake, stashed by [`set_wake`](LocationProvider::set_wake) —
    /// handed to the provider's sink and to every serial reader started
    /// afterwards, so a fix arriving while the loop is parked gets a frame to
    /// be shown on.
    wake: Option<Wake>,
}

impl Default for OsBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl OsBackend {
    /// No provider yet. Constructing one needs the app's waker, which does not
    /// exist when a platform shell is built; see
    /// [`set_wake`](LocationProvider::set_wake).
    pub fn new() -> Self {
        Self {
            provider: None,
            state: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(crate::encode_permission(
                crate::LocationPermission::Unknown,
            ))),
            fixes: None,
            #[cfg(feature = "serial")]
            serial: None,
            #[cfg(feature = "serial")]
            serial_fixes: None,
            wake: None,
        }
    }
}

impl LocationProvider for OsBackend {
    /// Whatever the provider last reported, or `Unavailable` when this build
    /// has none.
    ///
    /// `Unavailable` and not `Unknown` for the no-provider case, and the
    /// difference matters. `Unknown` means "ask again shortly", so the gate
    /// would poll an arm that is never going to answer and the settings pane
    /// would sit on "Checking…" for the life of the process. `Unavailable` is
    /// the truth: this build has no OS location provider, the pane says so,
    /// and nothing spins.
    fn permission(&self) -> crate::LocationPermission {
        match self.provider {
            Some(_) => {
                crate::decode_permission(self.state.load(std::sync::atomic::Ordering::Relaxed))
            }
            None => crate::LocationPermission::Unavailable,
        }
    }

    fn request(&mut self) -> bool {
        self.provider
            .as_mut()
            .is_some_and(OsLocationReader::request)
    }

    /// Stop the stream and discard anything already in it.
    ///
    /// The drain is the part that is easy to leave out and is not optional:
    /// this is the path a *revoked* permission takes, and a fix that was in
    /// flight when consent was withdrawn must not land on the map one frame
    /// later. The receiver itself stays — the provider still holds the
    /// matching `Sender`, and a later `request` delivers down the same
    /// channel.
    ///
    /// The permission is left exactly as the provider last reported it. This
    /// is an off switch, not a revocation, and no platform lets an app hand a
    /// permission back.
    fn stop(&mut self) {
        let was_active = self.active();
        if let Some(provider) = self.provider.as_mut() {
            provider.stop();
        }
        if let Some(receiver) = self.fixes.as_ref() {
            let _ = drain_latest(receiver);
        }
        if was_active {
            log::info!("OS location delivery stopped");
        }
    }

    fn active(&self) -> bool {
        self.provider.as_ref().is_some_and(OsLocationReader::active)
    }

    /// Drains **both** sources every time, not the first one that answers.
    ///
    /// Draining conditionally would leave the loser's channel filling up: the
    /// OS provider pushes on its own schedule and nothing else empties it, so
    /// a serial fix arriving first would build an unbounded backlog behind it
    /// that later surfaces as minutes-old positions. See [`crate::prefer_fix`]
    /// for which one wins and why it is not simply "serial".
    fn poll_fix(&mut self) -> Option<crate::Fix> {
        let os = self.fixes.as_ref().and_then(drain_latest);
        #[cfg(feature = "serial")]
        {
            let serial = self.serial_fixes.as_ref().and_then(drain_latest);
            crate::prefer_fix(serial, os)
        }
        #[cfg(not(feature = "serial"))]
        os
    }

    /// Asked once, at startup, and before any provider exists — which is why
    /// the arm trait's is an associated function rather than a method.
    fn settings_available(&self) -> bool {
        OsLocationReader::settings_available()
    }

    fn open_settings(&mut self) {
        if let Some(provider) = self.provider.as_mut() {
            provider.open_settings();
        }
    }

    /// Bring the provider up.
    ///
    /// Called from the facade when the app installs its redraw waker and
    /// nowhere else, which is the only point where both of its requirements
    /// hold: the provider must exist before the gate's first
    /// `location_permission()` (the app installs the waker immediately after
    /// constructing itself and before any frame), and the app's real waker
    /// must be in hand — a wake captured at construction would fire into a
    /// slot nothing ever fills.
    ///
    /// Prompts nobody. That is the contract's first phase; see
    /// the crate-internal `OsLocationProvider::start` contract in `mod.rs`.
    fn set_wake(&mut self, wake: Wake) {
        let (fixes, receiver) = std::sync::mpsc::channel();
        let reported = std::sync::Arc::clone(&self.state);
        let report_waker = wake.clone();
        self.provider = OsLocationReader::start(OsLocationSink {
            fixes,
            wake: wake.clone(),
            // Store *and* wake. The store is what the frame path reads; the
            // wake is what gets a frame drawn for it to be read on, which
            // under `ControlFlow::Wait` is not otherwise going to happen —
            // a permission change is exactly the kind of event nothing else
            // is causing a redraw for.
            report: std::sync::Arc::new(move |permission| {
                reported.store(
                    crate::encode_permission(permission),
                    std::sync::atomic::Ordering::Relaxed,
                );
                report_waker();
            }),
        });
        // Only keep the receiver if something is going to push into it; an
        // open one would make `poll_fix` drain a channel forever.
        self.fixes = self.provider.is_some().then_some(receiver);
        self.wake = Some(wake);
    }

    #[cfg(feature = "serial")]
    fn start_serial(&mut self, config: &rustdar_nmea_serial::SerialConfig) {
        // Stop any existing reader first.
        self.stop_serial();
        let Some(wake) = self.wake.clone() else {
            // The waker is taken at set_wake because a fix is invisible until
            // a frame drains it; a reader started before that moment would
            // push into the void. The app installs the waker before any UI
            // exists, so nothing user-reachable can get here.
            log::warn!("serial GPS requested before the wake was installed; not started");
            return;
        };
        let (tx, rx) = std::sync::mpsc::channel();
        // The reader is a thread of its own, and `poll_fix` is drained only
        // on a frame: under `ControlFlow::Wait` a fix it pushes while the app
        // is idle is invisible until something else happens to draw one.
        if let Some(reader) = crate::serial::SerialFixReader::start(config, tx, move || wake()) {
            self.serial = Some(reader);
            self.serial_fixes = Some(rx);
            log::info!("Desktop serial GPS reader started");
        } else {
            log::warn!("No GPS port found — serial GPS not started");
        }
    }

    #[cfg(feature = "serial")]
    fn stop_serial(&mut self) {
        if self.serial.take().is_some() {
            log::info!("Desktop serial GPS reader stopped");
        }
        self.serial_fixes = None;
    }

    #[cfg(feature = "serial")]
    fn serial_active(&self) -> bool {
        self.serial.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LocationPermission as P;

    /// An arm whose provider has not been built has nothing to report, and
    /// `Unavailable` is the honest answer: it is the state the settings pane
    /// renders as "not available on this platform", and the gate treats it as
    /// terminal rather than spinning on "Checking…".
    #[test]
    fn a_backend_with_no_provider_yet_reports_unavailable() {
        let backend = OsBackend::new();
        assert_eq!(backend.permission(), P::Unavailable);
        assert!(!backend.active());
    }

    /// The contract's first phase, pinned on whichever arm this host compiles.
    ///
    /// Two rules, and both have been got wrong by a provider already. Bringing
    /// one up must **not** start delivering — that is `request`'s job and the
    /// user's decision — and it must leave the arm with something the gate can
    /// act on, which `Unavailable` is not: the gate reads it as terminal and
    /// never asks again, so a provider that came up and reported nothing would
    /// be a location service the app could never turn on.
    #[test]
    fn bringing_a_provider_up_reports_a_state_and_delivers_nothing() {
        let mut backend = OsBackend::new();
        backend.set_wake(std::sync::Arc::new(|| {}));

        assert!(
            !backend.active(),
            "`set_wake` delivered without anybody asking"
        );
        assert_eq!(
            backend.permission() == P::Unavailable,
            backend.provider.is_none(),
            "`Unavailable` must mean no provider and nothing else"
        );
    }
}
