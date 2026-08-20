//! The arm seam: what the facade needs from a location provider.
//!
//! One trait, five real implementors, all inside this crate. Test doubles MAY
//! implement it outside the crate — a double is not an arm — which is why it is
//! `pub` while `LocationBridge` is `pub(crate)`.

use crate::fix::Fix;
use crate::permission::LocationPermission;

/// Asks the event loop for a frame, so something a provider pushed while the
/// loop was parked is actually seen. `Arc<dyn …>` because providers hand it to
/// session threads, callbacks and poll threads.
pub type Wake = std::sync::Arc<dyn Fn() + Send + Sync + 'static>;

/// What the facade drives. Every method defaults to the honest answer of a
/// platform with no such capability.
///
/// The permission verbs keep the contracts written on the gate's seam
/// (`bridge.rs`): `permission` must be cheap and non-blocking on the frame path;
/// `request`'s `bool` is a hint only Android can make true; `stop` is an off
/// switch for the stream, never a revocation.
pub trait LocationProvider {
    /// Required — the arm-less answer is
    /// [`LocationPermission::Unavailable`], never a silent default.
    fn permission(&self) -> LocationPermission;

    fn request(&mut self) -> bool;

    fn stop(&mut self);

    fn active(&self) -> bool {
        false
    }

    /// How many times this install has asked — Android's tri-state needs it.
    fn set_attempts(&mut self, _attempts: u8) {}

    /// The newest fix waiting, from every source this arm runs; a desktop arm
    /// arbitrates serial against the OS service itself, see
    /// [`prefer_fix`](crate::prefer_fix).
    fn poll_fix(&mut self) -> Option<Fix> {
        None
    }

    fn settings_available(&self) -> bool {
        false
    }

    fn open_settings(&mut self) {}

    /// Install the app's wake and bring the arm up, prompting nobody and
    /// delivering nothing. Called once, when the app installs its redraw waker.
    fn set_wake(&mut self, _wake: Wake) {}

    /// A no-op on every arm with no serial port (web, Android, iOS).
    fn start_serial(&mut self, _config: &rustdar_nmea_serial::SerialConfig) {}

    fn stop_serial(&mut self) {}

    /// Whether a serial reader is running. Distinct from [`active`](Self::active):
    /// a dongle the user plugged in is not covered by the OS permission, and the
    /// revocation path reads this to leave the serial dot alone.
    fn serial_active(&self) -> bool {
        false
    }
}

/// Drain all pending messages from `rx`, returning the last one (if any).
///
/// Sensor channels are state, not events: only the newest value matters, and
/// draining keeps a fast producer from building a backlog the UI walks through
/// one frame at a time.
#[allow(
    dead_code,
    reason = "used by the provider arms; which of them is compiled varies by \
              feature and target, and a build with none (the default) still \
              carries the helper they share"
)]
pub(crate) fn drain_latest<T>(rx: &std::sync::mpsc::Receiver<T>) -> Option<T> {
    let mut latest = None;
    while let Ok(val) = rx.try_recv() {
        latest = Some(val);
    }
    latest
}

/// The arm for a build with no location service at all. Also what tests start
/// from.
pub struct UnavailableProvider;

impl LocationProvider for UnavailableProvider {
    fn permission(&self) -> LocationPermission {
        LocationPermission::Unavailable
    }

    fn request(&mut self) -> bool {
        false
    }

    fn stop(&mut self) {}
}
