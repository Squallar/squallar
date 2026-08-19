//! The arm seam: what the facade needs from a location provider.
//!
//! One trait, five real implementors, all of them INSIDE this crate (seam
//! ruling 6 — no remote location arm lives anywhere else): the desktop OS
//! arms behind `os-providers` (which carry the serial reader behind `serial`),
//! the Android JNI arm behind `android-provider`, the browser arm behind
//! `web-provider`, and [`UnavailableProvider`] for a build with none. The app
//! side holds a [`LocationFacade`](crate::LocationFacade) and never names an
//! arm; the platform shells construct the arm for their target and hand it in.
//!
//! Test doubles MAY implement this trait outside the crate — a double is not
//! an arm. That is why it is `pub` while [`LocationBridge`](crate::bridge)
//! (the gate's narrower view) collapsed to `pub(crate)` at WO-RL-4.

use crate::fix::Fix;
use crate::permission::LocationPermission;

/// Asks the event loop for a frame, so that something a provider pushed while
/// the loop was parked is actually seen.
///
/// `Arc<dyn …>` and not `impl Fn`, because providers hand it to more than one
/// place — session threads, callbacks, poll threads. `Send + Sync` is what the
/// app's `RedrawWaker` already guarantees (rustdar-frontend pins that with a
/// `const` assertion), so requiring it here costs nothing and is what makes
/// the clone legal. On wasm there are no threads to send it across; the bound
/// is still satisfied and still harmless.
pub type Wake = std::sync::Arc<dyn Fn() + Send + Sync + 'static>;

/// What the facade drives. Every method defaults to the honest answer of a
/// platform with no such capability, so an arm implements exactly the verbs
/// its OS has.
///
/// The permission verbs keep the contracts written on the gate's seam (see
/// `bridge.rs`): `permission` must be cheap and non-blocking on the frame
/// path; `request`'s `bool` is a hint only Android can make true; `stop` is an
/// off switch for the stream, never a revocation.
pub trait LocationProvider {
    /// What the OS currently says about this app's access to the user's
    /// location. Required — every arm has an answer, and the arm-less answer
    /// is [`LocationPermission::Unavailable`], never a silent default.
    fn permission(&self) -> LocationPermission;

    /// Prompt if the platform prompts, and start delivering fixes.
    fn request(&mut self) -> bool;

    /// Stop delivering fixes (the stream, not the permission).
    fn stop(&mut self);

    /// Whether the OS service is delivering right now.
    fn active(&self) -> bool {
        false
    }

    /// How many times this install has asked — Android's tri-state needs it;
    /// everyone else ignores it.
    fn set_attempts(&mut self, _attempts: u8) {}

    /// The newest fix waiting, from every source this arm runs (a desktop arm
    /// arbitrates its serial reader against the OS service internally — see
    /// [`prefer_fix`](crate::prefer_fix)).
    fn poll_fix(&mut self) -> Option<Fix> {
        None
    }

    /// Whether this platform has a location settings page worth offering.
    fn settings_available(&self) -> bool {
        false
    }

    /// Open the system location settings. Fire and forget; must not block.
    fn open_settings(&mut self) {}

    /// Install the app's wake and bring the arm up (prompting nobody and
    /// delivering nothing — the two-phase contract in `os_location`). Called
    /// once, when the app installs its redraw waker; before it, an arm may
    /// answer [`LocationPermission::Unknown`].
    fn set_wake(&mut self, _wake: Wake) {}

    /// Start the serial GPS reader with this config. A no-op on every arm
    /// that has no serial port (web, Android, iOS) — mirroring the defaulted
    /// no-ops the old `PlatformBridge` serial verbs carried.
    fn start_serial(&mut self, _config: &rustdar_nmea_serial::SerialConfig) {}

    /// Drop the serial reader.
    fn stop_serial(&mut self) {}

    /// Whether a serial reader is running. Distinct from [`active`](Self::active):
    /// a dongle the user plugged in is not covered by the OS permission, and
    /// the app's revocation path reads this to leave the serial dot alone.
    fn serial_active(&self) -> bool {
        false
    }
}

/// Drain all pending messages from `rx`, returning the last one (if any).
///
/// Sensor channels are state, not events: only the newest value matters and
/// the ones behind it are already stale. Draining rather than taking one per
/// frame keeps a fast producer from building a backlog the UI then walks
/// through one frame at a time. (The app side keeps its own copy in
/// `rustdar_frontend::platform` for its theme/heading channels — a five-line
/// helper does not cross a crate boundary; keep them in step.)
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

/// The arm for a build with no location service at all: permission is
/// permanently [`Unavailable`](LocationPermission::Unavailable) and every
/// other verb keeps its default. Also what tests start from.
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
