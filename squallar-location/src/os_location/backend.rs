//! The desktop/mobile OS arm of the facade: the provider from `mod.rs`'s arm
//! table, the permission it last reported, the channel it delivers on — and,
//! behind the `serial` feature, the serial GPS reader beside it.

use super::{OsLocationProvider as _, OsLocationReader, OsLocationSink};
use crate::provider::{LocationProvider, Wake, drain_latest};

pub struct OsBackend {
    /// Built once from `set_wake`. **Not** dropped by `stop`: on two of the
    /// three platforms this value *is* the permission watcher. `None` renders as
    /// `Unavailable`.
    provider: Option<OsLocationReader>,
    /// What the provider last reported. An atomic because it is written from
    /// whatever thread the provider is given and read from `permission`, a
    /// `&self` getter on the frame path; see [`crate::encode_permission`]. It
    /// deliberately outlives every session, because the gate answers `Denied` by
    /// calling stop.
    state: std::sync::Arc<std::sync::atomic::AtomicU8>,
    /// Its own channel and not a second sender into the serial reader's, because
    /// `poll_fix` has to pick between the two sources (see
    /// [`crate::prefer_fix`]) and cannot once they are merged.
    fixes: Option<std::sync::mpsc::Receiver<crate::Fix>>,
    /// Active serial GPS source (dropped to stop). See [`crate::serial`].
    #[cfg(feature = "serial")]
    serial: Option<crate::serial::SerialFixReader>,
    #[cfg(feature = "serial")]
    serial_fixes: Option<std::sync::mpsc::Receiver<crate::Fix>>,
    /// Stashed by `set_wake` and handed to the provider's sink and to every
    /// serial reader started afterwards.
    wake: Option<Wake>,
}

impl Default for OsBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl OsBackend {
    /// No provider yet: constructing one needs the app's waker, which does not
    /// exist when a platform shell is built.
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
    /// Whatever the provider last reported, or `Unavailable` when this build has
    /// none — `Unknown` would mean "ask again shortly", so the gate would poll an
    /// arm that is never going to answer.
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

    /// Stop the stream and discard anything already in it. The drain is not
    /// optional: this is the path a *revoked* permission takes, and a fix in
    /// flight when consent was withdrawn must not land one frame later.
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

    /// Drains **both** sources every time, not the first that answers: draining
    /// conditionally would leave the loser's channel filling into an unbounded
    /// backlog that later surfaces as minutes-old positions.
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

    /// Asked once, at startup, before any provider exists — which is why the arm
    /// trait's is an associated function.
    fn settings_available(&self) -> bool {
        OsLocationReader::settings_available()
    }

    fn open_settings(&mut self) {
        if let Some(provider) = self.provider.as_mut() {
            provider.open_settings();
        }
    }

    /// Bring the provider up, prompting nobody. Called when the app installs its
    /// redraw waker and nowhere else, the only point where both requirements
    /// hold: the provider must exist before the gate's first
    /// `location_permission()`, and the app's real waker must be in hand.
    fn set_wake(&mut self, wake: Wake) {
        let (fixes, receiver) = std::sync::mpsc::channel();
        let reported = std::sync::Arc::clone(&self.state);
        let report_waker = wake.clone();
        self.provider = OsLocationReader::start(OsLocationSink {
            fixes,
            wake: wake.clone(),
            // Store *and* wake: the store is what the frame path reads, the wake
            // is what gets a frame drawn under `ControlFlow::Wait`.
            report: std::sync::Arc::new(move |permission| {
                reported.store(
                    crate::encode_permission(permission),
                    std::sync::atomic::Ordering::Relaxed,
                );
                report_waker();
            }),
        });
        // An open receiver with nothing pushing would make `poll_fix` drain
        // a channel forever.
        self.fixes = self.provider.is_some().then_some(receiver);
        self.wake = Some(wake);
    }

    #[cfg(feature = "serial")]
    fn start_serial(&mut self, config: &squallar_nmea_serial::SerialConfig) {
        self.stop_serial();
        let Some(wake) = self.wake.clone() else {
            // A fix is invisible until a frame drains it, so a reader started
            // before the wake was installed would push into the void.
            log::warn!("serial GPS requested before the wake was installed; not started");
            return;
        };
        let (tx, rx) = std::sync::mpsc::channel();
        // Under `ControlFlow::Wait` a fix pushed while the app is idle is
        // invisible until something else draws.
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

    /// `Unavailable` is the honest answer for an arm whose provider has not been
    /// built: the gate treats it as terminal rather than spinning on "Checking…".
    #[test]
    fn a_backend_with_no_provider_yet_reports_unavailable() {
        let backend = OsBackend::new();
        assert_eq!(backend.permission(), P::Unavailable);
        assert!(!backend.active());
    }

    /// Bringing a provider up must **not** start delivering, and must leave the
    /// arm with something the gate can act on — which `Unavailable` is not.
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
