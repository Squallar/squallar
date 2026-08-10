//! The provider for a target with no OS location service.
//!
//! Not a placeholder that will be deleted: wasm32 and any OS rustdar has not
//! learned about select this permanently, and "there is nothing to read here"
//! is a real answer rather than a missing one. [`OsLocationReader::start`]
//! returns `None`, which the bridge already treats as "no fixes from this
//! source" — the same path a machine with no serial port takes.
#![allow(
    dead_code,
    reason = "the arm that compiles this has no caller for `start`; the type is \
              named as a field type by `platform.rs` and the constructor is what \
              a real provider fills in. `-D warnings` runs only on the host cfg \
              today, so without this the first cross-target lint row added would \
              fail on a file that is behaving correctly."
)]

use super::{OsLocationProvider, OsLocationSink};

/// A location session on a target that has none.
///
/// [`SerialGpsReader`]: rustdar_gps::SerialGpsReader
pub struct OsLocationReader {
    /// Uninhabited in practice — `start` never returns a value — but a
    /// zero-field struct would let a real provider be added without anyone
    /// noticing this one still had no state to stop.
    _private: (),
}

/// Every method below is unreachable by construction: `start` never hands
/// anyone a value to call them on. They are written out anyway, and they are
/// what keeps `platform.rs` free of `#[cfg(target_os = ...)]` — a bridge that
/// had to name a target to ask for a permission would put the arm table in two
/// places, and the second copy is the one that gets a new OS added to it
/// wrongly.
impl OsLocationProvider for OsLocationReader {
    /// Always `None`: this target has no location service to subscribe to.
    ///
    /// The whole [`OsLocationSink`] is dropped here, which closes the fix
    /// channel — exactly what the caller's drain expects from a source that will
    /// never produce. The waker and the permission callback go with it, for the
    /// same reason: a provider that never produces a fix has no frame to ask for
    /// and no permission to announce.
    fn start(_sink: OsLocationSink) -> Option<Self> {
        log::debug!("no OS location provider is compiled for this target");
        None
    }

    fn request(&mut self) -> bool {
        false
    }

    fn stop(&mut self) {}

    fn active(&self) -> bool {
        false
    }
}
