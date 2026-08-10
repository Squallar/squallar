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
    ///
    /// `wake` is the same third argument [`SerialGpsReader::start`] takes and
    /// exists for the same reason: a provider delivers on a schedule the event
    /// loop knows nothing about, and the loop parks in `ControlFlow::Wait`, so
    /// a fix pushed into the channel is invisible until something else happens
    /// to draw a frame. `report` is how a provider announces a permission
    /// change nobody polled for. Both are dropped here: a provider that never
    /// produces a fix has no frame to ask for and no permission to announce.
    ///
    /// [`SerialGpsReader::start`]: rustdar_gps::SerialGpsReader::start
    pub fn start(
        _config: &rustdar_gps::GpsConfig,
        _fixes: std::sync::mpsc::Sender<rustdar_gps::GpsFix>,
        _wake: impl Fn() + Send + 'static,
        _report: impl Fn(rustdar_gps::LocationPermission) + Send + 'static,
    ) -> Option<Self> {
        log::debug!("no OS location provider is compiled for this target");
        None
    }

    // ── The rest of the provider contract ───────────────────────────────
    //
    // Unreachable here — `start` never hands anyone a value to call them on —
    // and present anyway, because they are what keeps `platform.rs` free of
    // `#[cfg(target_os = ...)]`. A bridge that had to name a target to ask for
    // a permission would put the arm table in two places, and the second copy
    // is the one that gets a new OS added to it wrongly.

    /// What the OS says about this app's access to the user's location.
    pub fn permission(&self) -> rustdar_gps::LocationPermission {
        rustdar_gps::LocationPermission::Unavailable
    }

    /// Prompt if the platform needs prompting, and start delivering.
    pub fn request(&mut self) -> bool {
        false
    }

    /// Stop delivering. Never revokes; no platform offers that.
    pub fn stop(&mut self) {}

    /// Whether fixes are being delivered right now.
    pub fn active(&self) -> bool {
        false
    }
}
