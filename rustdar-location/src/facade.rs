//! The one value the app holds for "where am I" (WO-RL-4).
//!
//! Composes the permission gate with whichever provider arm the platform
//! shell constructed, and is the whole app-facing surface: the old
//! `PlatformBridge` location/gps verb family died when this landed. The gate
//! drives the provider in-crate through the `pub(crate)` seam in `bridge.rs`;
//! nothing outside this crate can reach a provider around the gate's
//! one-prompt-line discipline.
//!
//! # Where the kv store comes from
//!
//! The gate persists its memo through [`rustdar_kv::KvStore`], and the ONE
//! owner of "where blobs live" is the platform bridge the app already holds —
//! Android learns its config dir after startup, and duplicating that state
//! here would let the two owners disagree. So the gate-driving methods take a
//! `kv` closure and the app passes `platform.kv()` through per call; the gate
//! still resolves it exactly once (its `resolve_store` is one-shot).

use crate::bridge::LocationBridge;
use crate::fix::Fix;
use crate::gate::{LocationGate, LocationStep};
use crate::permission::LocationPermission;
use crate::provider::{LocationProvider, UnavailableProvider, Wake};

/// A kv-store source, passed per call. See the module note.
pub type KvSource<'a> = &'a dyn Fn() -> Option<Box<dyn rustdar_kv::KvStore>>;

/// The gate's view of (provider + the caller's kv), composed per call.
///
/// This is what "the LocationBridge trait collapses internal" looks like: the
/// trait's six methods survive verbatim as the gate's seam, and this adapter
/// is its one production implementor — the provider answers five, the app's
/// kv closure answers the sixth.
struct GateSeam<'a> {
    provider: &'a mut dyn LocationProvider,
    kv: KvSource<'a>,
}

impl LocationBridge for GateSeam<'_> {
    fn location_permission(&self) -> LocationPermission {
        self.provider.permission()
    }

    fn request_location(&mut self) -> bool {
        self.provider.request()
    }

    fn stop_location(&mut self) {
        self.provider.stop();
    }

    fn location_active(&self) -> bool {
        self.provider.active()
    }

    fn set_location_attempts(&mut self, attempts: u8) {
        self.provider.set_attempts(attempts);
    }

    fn kv(&self) -> Option<Box<dyn rustdar_kv::KvStore>> {
        (self.kv)()
    }
}

/// Everything between "where am I" and the operating system, as one value.
pub struct LocationFacade {
    gate: LocationGate,
    provider: Box<dyn LocationProvider>,
}

impl LocationFacade {
    /// A facade over the arm the platform shell built for its target.
    pub fn new(provider: Box<dyn LocationProvider>) -> Self {
        Self {
            gate: LocationGate::new(),
            provider,
        }
    }

    /// A facade with no location service at all — the honest state for a
    /// build with no arm, and the state app-level tests start from.
    pub fn unavailable() -> Self {
        Self::new(Box::new(UnavailableProvider))
    }

    /// What the gate last observed. See [`LocationGate::permission`].
    pub fn permission(&self) -> LocationPermission {
        self.gate.permission()
    }

    /// Whether the OS service is delivering, as the gate last observed it.
    pub fn active(&self) -> bool {
        self.gate.active()
    }

    /// Drive the gate one step against the provider. Called once per frame
    /// from the app's platform poll; see `LocationGate::step` (crate-internal).
    pub fn step(&mut self, kv: KvSource<'_>, settings_open: bool) -> LocationStep {
        self.gate.step(
            &mut GateSeam {
                provider: self.provider.as_mut(),
                kv,
            },
            settings_open,
        )
    }

    /// The user said yes in the UI. See `LocationGate::enable` (crate-internal).
    pub fn enable(&mut self, kv: KvSource<'_>) {
        self.gate.enable(&mut GateSeam {
            provider: self.provider.as_mut(),
            kv,
        });
    }

    /// The user said stop in the UI. See `LocationGate::disable` (crate-internal).
    pub fn disable(&mut self, kv: KvSource<'_>) {
        self.gate.disable(&mut GateSeam {
            provider: self.provider.as_mut(),
            kv,
        });
    }

    /// Coming back to the foreground; see [`LocationGate::resumed`].
    pub fn resumed(&mut self) {
        self.gate.resumed();
    }

    /// Whether this platform has a location settings page worth offering.
    /// A property of the build — the app reads it once, at startup.
    pub fn settings_available(&self) -> bool {
        self.provider.settings_available()
    }

    /// Open the system location settings.
    pub fn open_settings(&mut self) {
        self.provider.open_settings();
    }

    /// The newest fix from every source the arm runs (OS service and serial
    /// reader alike — the arm arbitrates; see [`prefer_fix`](crate::prefer_fix)).
    pub fn poll_fix(&mut self) -> Option<Fix> {
        self.provider.poll_fix()
    }

    /// Install the app's wake and bring the arm up. Called once, where the
    /// app installs its redraw waker; prompts nobody and delivers nothing
    /// (the two-phase contract in `os_location`).
    pub fn set_wake(&mut self, wake: Wake) {
        self.provider.set_wake(wake);
    }

    /// Start the serial GPS reader. A no-op on arms without a serial port.
    pub fn start_serial(&mut self, config: &rustdar_nmea_serial::SerialConfig) {
        self.provider.start_serial(config);
    }

    /// Drop the serial reader.
    pub fn stop_serial(&mut self) {
        self.provider.stop_serial();
    }

    /// Whether a serial reader is running — the dongle's dot survives an OS
    /// permission denial precisely because this is a different question from
    /// [`active`](Self::active).
    pub fn serial_active(&self) -> bool {
        self.provider.serial_active()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The arm-less facade is permanently `Unavailable` — the state the gate
    /// treats as terminal — and never claims a stream or a serial reader.
    #[test]
    fn a_facade_with_no_arm_is_honestly_unavailable() {
        let mut facade = LocationFacade::unavailable();
        let step = facade.step(&|| None, false);
        assert_eq!(facade.permission(), LocationPermission::Unavailable);
        assert!(!facade.active());
        assert!(!facade.serial_active());
        assert!(!facade.settings_available());
        assert!(!step.revoked);
    }

    /// The seam really carries the caller's kv into the gate: the memo's
    /// enabled flag round-trips through the passed store on disable and flips
    /// back on enable (the facade is what wires `platform.kv()` to the gate
    /// since the bridge verbs died).
    #[test]
    fn the_gate_persists_through_the_kv_the_caller_passes() {
        use rustdar_kv::{KvStore, MemoryKvStore};
        use std::rc::Rc;

        /// The GateDouble sharing pattern: the test keeps the store the gate
        /// writes through.
        struct Shared(Rc<MemoryKvStore>);
        impl KvStore for Shared {
            fn load(&self, key: &str) -> Option<String> {
                self.0.load(key)
            }
            fn store(&self, key: &str, value: &str) -> Result<(), String> {
                self.0.store(key, value)
            }
        }

        let store = Rc::new(MemoryKvStore::default());
        let kv = {
            let store = Rc::clone(&store);
            move || Some(Box::new(Shared(Rc::clone(&store))) as Box<dyn KvStore>)
        };

        let mut facade = LocationFacade::unavailable();
        // The first step resolves the store, exactly as production frames do.
        let _ = facade.step(&kv, false);

        facade.disable(&kv);
        let raw = store
            .load(crate::LOCATION_MEMO_KEY)
            .expect("the memo was never written through the caller's kv");
        assert!(
            raw.contains("\"enabled\":false"),
            "disable did not persist through the passed store: {raw}"
        );

        facade.enable(&kv);
        let raw = store.load(crate::LOCATION_MEMO_KEY).expect("memo vanished");
        assert!(
            raw.contains("\"enabled\":true"),
            "enable did not persist through the passed store: {raw}"
        );
    }
}
