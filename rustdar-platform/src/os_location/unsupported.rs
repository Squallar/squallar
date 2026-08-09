//! The provider for a target with no OS location service — and, for now, for
//! every target.
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

/// A running subscription to the platform's location service, stopped by
/// dropping it.
///
/// The shape is [`SerialGpsReader`]'s, deliberately: `DesktopPlatform` holds
/// one of each and drains one channel, and two readers with the same lifecycle
/// are two readers nobody has to remember the difference between.
///
/// [`SerialGpsReader`]: rustdar_gps::SerialGpsReader
pub struct OsLocationReader {
    /// Uninhabited in practice — `start` never returns a value — but a
    /// zero-field struct would let a real provider be added without anyone
    /// noticing this one still had no state to stop.
    _private: (),
}

impl OsLocationReader {
    /// Always `None`: this target has no location service to subscribe to.
    ///
    /// Takes the same arguments a real provider needs so that swapping one in
    /// touches only `mod.rs`. The `Sender` is dropped here, which closes the
    /// channel — exactly what the caller's drain expects from a source that
    /// will never produce.
    pub fn start(
        _config: &rustdar_gps::GpsConfig,
        _fixes: std::sync::mpsc::Sender<rustdar_gps::GpsFix>,
    ) -> Option<Self> {
        log::debug!("no OS location provider is compiled for this target");
        None
    }
}
