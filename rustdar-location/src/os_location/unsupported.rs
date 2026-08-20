//! The provider for a target with no OS location service. Not a placeholder:
//! wasm32 and any OS rustdar has not learned about select this permanently, and
//! [`OsLocationReader::start`] returns `None`.
#![allow(
    dead_code,
    reason = "the arm that compiles this has no caller for `start`; the type is \
              named as a field type by `platform.rs` and the constructor is what \
              a real provider fills in. `-D warnings` runs only on the host cfg \
              today, so without this the first cross-target lint row added would \
              fail on a file that is behaving correctly."
)]

use super::{OsLocationProvider, OsLocationSink};

pub struct OsLocationReader {
    /// A zero-field struct would let a real provider be added without anyone
    /// noticing this one still had no state to stop.
    _private: (),
}

/// Every method below is unreachable by construction, and written out anyway
/// because that is what keeps `platform.rs` free of `#[cfg(target_os = ...)]`.
impl OsLocationProvider for OsLocationReader {
    /// Always `None`. The whole [`OsLocationSink`] is dropped here, closing the
    /// fix channel — what the caller's drain expects from a source that will
    /// never produce.
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
