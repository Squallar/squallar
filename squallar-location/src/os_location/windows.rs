//! Windows: `AppCapability` for the state, `Geolocator` to prompt and to
//! deliver. Modelled on Chromium's `system_geolocation_source_win.cc`, which
//! solves the same problem: a desktop Win32 process with no appx identity.
//!
//! State comes from `AppCapability::Create("location")` + `CheckAccess`, never
//! from `Geolocator`. `NotDeclaredByApp` means "ask", not "no" — see
//! [`permission_from_slot`].
//!
//! Nothing here may block the frame thread: `RequestAccessAsync().join()` waits
//! on a human. Every WinRT call runs on one worker thread that never calls
//! `CoInitializeEx` and is therefore implicitly MTA, where a blocking wait is
//! legal and completions arrive on RPC threads with no message pump.
//! `TypedEventHandler` is `!Send` in windows-rs 0.62: a delegate must be built
//! and registered on that thread, and only the `i64` token may travel.
//!
//! The mapping layer is pure functions over `i32`s so it is testable on a Linux
//! host; `live`'s `const` assertions pin them to the real bindings.

use crate::{Fix, FixQuality, LocationPermission};

/// `DeniedBySystem` — group policy, or the machine-wide switch is off.
pub const CAPABILITY_DENIED_BY_SYSTEM: i32 = 0;
/// `NotDeclaredByApp` — the normal state for this binary. See
/// [`permission_from_slot`].
pub const CAPABILITY_NOT_DECLARED_BY_APP: i32 = 1;
pub const CAPABILITY_DENIED_BY_USER: i32 = 2;
pub const CAPABILITY_USER_PROMPT_REQUIRED: i32 = 3;
pub const CAPABILITY_ALLOWED: i32 = 4;

/// `GeolocationAccessStatus::Unspecified` — the prompt was dismissed, or the
/// answer never arrived. Not a denial.
pub const ACCESS_UNSPECIFIED: i32 = 0;
pub const ACCESS_ALLOWED: i32 = 1;
pub const ACCESS_DENIED: i32 = 2;

/// `PositionSource::Satellite` — the one source that earns [`FixQuality::Gps`].
pub const SOURCE_SATELLITE: i32 = 1;

/// `PositionStatus` codes, for the one diagnostic line this file logs.
pub const POSITION_STATUS_READY: i32 = 0;
pub const POSITION_STATUS_INITIALIZING: i32 = 1;
pub const POSITION_STATUS_NO_DATA: i32 = 2;
pub const POSITION_STATUS_DISABLED: i32 = 3;
pub const POSITION_STATUS_NOT_INITIALIZED: i32 = 4;
pub const POSITION_STATUS_NOT_AVAILABLE: i32 = 5;

/// Nothing has been read yet. Negative so it cannot collide with a real
/// `AppCapabilityAccessStatus`, which the SDK numbers from zero upwards.
pub const SLOT_UNKNOWN: i32 = -1;

/// No `AppCapability` class on this machine — Windows 10 before 1903 has no
/// `AppCapabilityAccess` namespace. Terminal.
pub const SLOT_UNAVAILABLE: i32 = -2;

/// How many consecutive `CheckAccess` failures before the slot stops saying
/// "not yet". `Unknown` does nothing, so a stuck failure would park the settings
/// pane on "Checking…" for the life of the process.
pub const MAX_CONSECUTIVE_CHECK_FAILURES: u8 = 3;

/// The capability name `AppCapability::Create` is asked for.
pub const LOCATION_CAPABILITY: &str = "location";

/// Where `Denied` sends the user: the **global** location switch — desktop apps
/// have no per-app entry to deep-link to. `LaunchUriAsync` needs no HWND.
pub const LOCATION_SETTINGS_URI: &str = "ms-settings:privacy-location";

/// Decode the shared slot — an `AppCapabilityAccessStatus` or one of the two
/// sentinels — into the app's own permission model.
///
/// `NotDeclaredByApp` is [`Prompt`](LocationPermission::Prompt): a capability is
/// declared by an appx manifest and this binary has none, so the status is a
/// fact about packaging, not consent. Read as a denial it self-seals — no
/// button, so no request, so Windows never records an access entry. The `_` arm
/// is mandatory (a `repr(transparent)` struct, not an enum) and is `Prompt` for
/// the same reason.
pub fn permission_from_slot(slot: i32) -> LocationPermission {
    match slot {
        SLOT_UNKNOWN => LocationPermission::Unknown,
        SLOT_UNAVAILABLE => LocationPermission::Unavailable,
        CAPABILITY_ALLOWED => LocationPermission::Granted,
        // Both reversible only by the user in Settings, same sentence and button.
        CAPABILITY_DENIED_BY_USER | CAPABILITY_DENIED_BY_SYSTEM => LocationPermission::Denied,
        CAPABILITY_USER_PROMPT_REQUIRED | CAPABILITY_NOT_DECLARED_BY_APP => {
            LocationPermission::Prompt
        }
        _ => LocationPermission::Prompt,
    }
}

/// What a completed `Geolocator::RequestAccessAsync` said. `Geolocator` and not
/// `AppCapability::RequestAccessAsync`, because this is the call that raises the
/// Windows 11 24H2 one-time prompt for an unpackaged process.
///
/// [`Unspecified`](ACCESS_UNSPECIFIED) is `Unknown` and must not become a retry
/// — it is what a dismissed prompt returns.
pub fn permission_from_request_result(status: i32) -> LocationPermission {
    match status {
        ACCESS_ALLOWED => LocationPermission::Granted,
        ACCESS_DENIED => LocationPermission::Denied,
        ACCESS_UNSPECIFIED => LocationPermission::Unknown,
        // Mandatory arm; conclude nothing, the next `CheckAccess` decides.
        _ => LocationPermission::Unknown,
    }
}

/// The slot value a finished access request justifies writing, or `None` for
/// "leave it and let `CheckAccess` answer". The result is seeded only so the
/// settings pane reacts without waiting out a poll interval. `Unknown` writes
/// nothing rather than [`SLOT_UNKNOWN`], which would blank a known state.
pub fn slot_after_request(status: i32) -> Option<i32> {
    match permission_from_request_result(status) {
        LocationPermission::Granted => Some(CAPABILITY_ALLOWED),
        LocationPermission::Denied => Some(CAPABILITY_DENIED_BY_USER),
        _ => None,
    }
}

/// The slot value a failed `CheckAccess` justifies writing, or `None`. A failure
/// never overwrites an answer that was once real, and a failure before any
/// answer must not be sticky either — so after
/// [`MAX_CONSECUTIVE_CHECK_FAILURES`] the slot falls back to `UserPromptRequired`.
pub fn slot_after_check_failure(current: i32, consecutive_failures: u8) -> Option<i32> {
    if current != SLOT_UNKNOWN {
        return None;
    }
    if consecutive_failures >= MAX_CONSECUTIVE_CHECK_FAILURES {
        Some(CAPABILITY_USER_PROMPT_REQUIRED)
    } else {
        None
    }
}

/// Only [`Satellite`](SOURCE_SATELLITE) is [`Gps`](FixQuality::Gps); cellular,
/// Wi-Fi, IP address, `Default` and `Obfuscated` are all
/// [`Device`](FixQuality::Device). `App::upgrade_provisional_site` looks for
/// `Gps`, so a city-scale guess must not claim it.
pub fn fix_quality_from_position_source(source: i32) -> FixQuality {
    if source == SOURCE_SATELLITE {
        FixQuality::Gps
    } else {
        FixQuality::Device
    }
}

/// Collapse the two-layer `Result` a WinRT `IReference<T>` field arrives in.
/// `Geocoordinate::Altitude()` and friends are `Result<IReference<f64>>`: a null
/// reference and a genuine RPC failure both surface as `Err` from the getter and
/// are indistinguishable without the HRESULT, so this does not log.
pub fn reference_value<R, T, E>(
    field: Result<R, E>,
    value: impl FnOnce(R) -> Result<T, E>,
) -> Option<T> {
    field.ok().and_then(|reference| value(reference).ok())
}

/// Build a [`Fix`] from what a `Geocoordinate` reports. Separate from the WinRT
/// reads because a swapped latitude and longitude here is silently valid.
/// `satellites`, `hdop` and `timestamp` stay `None`.
pub fn fix_from_coordinate(
    latitude: f64,
    longitude: f64,
    accuracy_m: f64,
    altitude_m: Option<f64>,
    heading_deg: Option<f64>,
    speed_mps: Option<f64>,
    position_source: i32,
) -> Fix {
    Fix {
        altitude_m,
        speed_mps,
        heading_deg,
        accuracy_m: Some(accuracy_m),
        fix_quality: fix_quality_from_position_source(position_source),
        ..Fix::from_device_position(latitude, longitude)
    }
}

/// One word for a `PositionStatus`, for the diagnostic log line. Not a
/// permission signal: it is `NotInitialized` until something subscribes.
pub fn position_status_label(status: i32) -> &'static str {
    match status {
        POSITION_STATUS_READY => "ready",
        POSITION_STATUS_INITIALIZING => "initializing",
        POSITION_STATUS_NO_DATA => "no data",
        POSITION_STATUS_DISABLED => "disabled (location is off for this machine or app)",
        POSITION_STATUS_NOT_INITIALIZED => "not initialized (nothing has subscribed yet)",
        POSITION_STATUS_NOT_AVAILABLE => "not available on this device",
        _ => "unrecognised",
    }
}

#[cfg(target_os = "windows")]
pub use live::OsLocationReader;

#[cfg(target_os = "windows")]
mod live {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicI32, Ordering};
    use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
    use std::time::Duration;

    use crate::Fix;
    // `::windows`, because this module's own parent is called `windows` too.
    use ::windows::Devices::Geolocation::{
        Geolocator, PositionAccuracy, PositionChangedEventArgs, StatusChangedEventArgs,
    };
    use ::windows::Foundation::{TypedEventHandler, Uri};
    use ::windows::Security::Authorization::AppCapabilityAccess::{
        AppCapability, AppCapabilityAccessChangedEventArgs, AppCapabilityAccessStatus,
    };
    use ::windows::System::Launcher;
    use ::windows::core::HSTRING;

    use super::super::{OsLocationProvider, OsLocationSink, RedrawWake, ReportPermission};
    use super::{
        LOCATION_CAPABILITY, LOCATION_SETTINGS_URI, SLOT_UNAVAILABLE, SLOT_UNKNOWN,
        permission_from_request_result, permission_from_slot, position_status_label,
    };

    /// Pins the mapping layer's `i32`s to the generated bindings at compile time.
    const _: () = {
        use ::windows::Devices::Geolocation::GeolocationAccessStatus as Access;
        use ::windows::Devices::Geolocation::{PositionSource, PositionStatus};

        assert!(super::CAPABILITY_DENIED_BY_SYSTEM == AppCapabilityAccessStatus::DeniedBySystem.0);
        assert!(
            super::CAPABILITY_NOT_DECLARED_BY_APP == AppCapabilityAccessStatus::NotDeclaredByApp.0
        );
        assert!(super::CAPABILITY_DENIED_BY_USER == AppCapabilityAccessStatus::DeniedByUser.0);
        assert!(
            super::CAPABILITY_USER_PROMPT_REQUIRED
                == AppCapabilityAccessStatus::UserPromptRequired.0
        );
        assert!(super::CAPABILITY_ALLOWED == AppCapabilityAccessStatus::Allowed.0);

        assert!(super::ACCESS_UNSPECIFIED == Access::Unspecified.0);
        assert!(super::ACCESS_ALLOWED == Access::Allowed.0);
        assert!(super::ACCESS_DENIED == Access::Denied.0);

        assert!(super::SOURCE_SATELLITE == PositionSource::Satellite.0);

        assert!(super::POSITION_STATUS_READY == PositionStatus::Ready.0);
        assert!(super::POSITION_STATUS_INITIALIZING == PositionStatus::Initializing.0);
        assert!(super::POSITION_STATUS_NO_DATA == PositionStatus::NoData.0);
        assert!(super::POSITION_STATUS_DISABLED == PositionStatus::Disabled.0);
        assert!(super::POSITION_STATUS_NOT_INITIALIZED == PositionStatus::NotInitialized.0);
        assert!(super::POSITION_STATUS_NOT_AVAILABLE == PositionStatus::NotAvailable.0);

        // The sentinels rely on the SDK numbering its statuses from zero upwards.
        assert!(super::SLOT_UNKNOWN < 0 && super::SLOT_UNAVAILABLE < 0);
    };

    /// How often `CheckAccess` runs unasked — the fallback behind
    /// `AccessChanged`, which does not fire reliably on every build.
    const POLL_INTERVAL: Duration = Duration::from_secs(2);

    /// `Geolocator::ReportInterval`, in milliseconds: a *minimum*, and a hint.
    const REPORT_INTERVAL_MS: u32 = 10_000;

    /// `Geolocator::MovementThreshold`, in metres.
    const MOVEMENT_THRESHOLD_M: f64 = 100.0;

    /// Work for the WinRT thread: what must not happen on the frame thread.
    enum Command {
        RequestAccess,
        StartDelivery {
            fixes: Sender<Fix>,
            wake: RedrawWake,
        },
        StopDelivery,
        OpenSettings,
    }

    /// The latest `AppCapabilityAccessStatus`, or a sentinel, plus the callback
    /// that publishes every change of it.
    ///
    /// One value and not two because they must never be written apart: four
    /// places set the status, and any that forgot `report` would leave the pane
    /// stale for the life of the process, so [`store`](Self::store) is the only
    /// writer. Atomic because it is also written from an RPC thread.
    struct Slot {
        status: AtomicI32,
        report: ReportPermission,
    }

    impl Slot {
        fn store(&self, status: i32) {
            self.status.store(status, Ordering::Relaxed);
            (self.report)(permission_from_slot(status));
        }

        fn load(&self) -> i32 {
            self.status.load(Ordering::Relaxed)
        }
    }

    /// The Windows provider: an `AppCapability` watcher that runs for the life
    /// of the process, and a `Geolocator` subscription that comes and goes.
    /// Permission state must be readable before anything starts and keep being
    /// watched after delivery stops.
    pub struct OsLocationReader {
        /// Dropping this stops the worker — a disconnected channel ends its
        /// `recv_timeout`.
        commands: Sender<Command>,
        sink: OsLocationSink,
        /// Not read back off the worker: that would put a channel handshake on
        /// the frame path.
        delivering: bool,
    }

    impl OsLocationProvider for OsLocationReader {
        /// Start the worker and begin watching the capability. Prompts nobody,
        /// and returns before the first `CheckAccess`, so the permission stays
        /// [`Unknown`](crate::LocationPermission::Unknown) for a moment.
        fn start(sink: OsLocationSink) -> Option<Self> {
            let slot = Arc::new(Slot {
                status: AtomicI32::new(SLOT_UNKNOWN),
                report: Arc::clone(&sink.report),
            });
            let (commands, inbox) = channel();

            let worker_slot = Arc::clone(&slot);
            if let Err(e) = std::thread::Builder::new()
                .name("squallar-os-location".to_owned())
                .spawn(move || worker(&worker_slot, &inbox))
            {
                // No second chance is coming, so say so rather than wait.
                log::error!("could not start the Windows location worker: {e}");
                slot.store(SLOT_UNAVAILABLE);
            }

            Some(Self {
                commands,
                sink,
                delivering: false,
            })
        }

        /// Subscribe first, then ask — they are one user-visible act. A
        /// `Geolocator` with no permission simply reports no positions, and
        /// having the subscription in place is what makes a grant produce a fix
        /// immediately. The `bool` says only that the request reached the worker.
        fn request(&mut self) -> bool {
            if !self.delivering {
                // The `Geolocator` is constructed on the worker, so success is
                // not knowable here without blocking.
                self.delivering = self
                    .commands
                    .send(Command::StartDelivery {
                        fixes: self.sink.fixes.clone(),
                        wake: Arc::clone(&self.sink.wake),
                    })
                    .is_ok();
                if self.delivering {
                    log::info!("Windows location delivery requested");
                } else {
                    log::warn!("the OS location worker is gone; no fixes will arrive");
                }
            }
            self.commands.send(Command::RequestAccess).is_ok()
        }

        /// Unsubscribe, leaving the capability watcher running.
        fn stop(&mut self) {
            // Best effort; a dead worker has already dropped its `Geolocator`.
            let _ = self.commands.send(Command::StopDelivery);
            self.delivering = false;
        }

        fn active(&self) -> bool {
            self.delivering
        }

        /// See [`LOCATION_SETTINGS_URI`].
        fn settings_available() -> bool {
            true
        }

        fn open_settings(&mut self) {
            if self.commands.send(Command::OpenSettings).is_err() {
                log::warn!("the OS location worker is gone; cannot open system settings");
            }
        }
    }

    /// A registered `PositionChanged`/`StatusChanged` pair. Exists for its
    /// `Drop`: the tokens must be handed back to the `Geolocator` they came from.
    struct Delivery {
        geolocator: Geolocator,
        position_token: i64,
        status_token: Option<i64>,
    }

    impl Drop for Delivery {
        fn drop(&mut self) {
            if let Err(e) = self.geolocator.RemovePositionChanged(self.position_token) {
                log::debug!("could not unsubscribe from Windows position updates: {e}");
            }
            if let Some(token) = self.status_token
                && let Err(e) = self.geolocator.RemoveStatusChanged(token)
            {
                log::debug!("could not unsubscribe from Windows location status: {e}");
            }
        }
    }

    /// The one thread that touches WinRT. It never calls `CoInitializeEx`, which
    /// makes it implicitly MTA once windows-rs performs its `CoIncrementMTAUsage`
    /// fallback: blocking on a future is only safe in an MTA, and completions
    /// arrive on an RPC thread with no message pump.
    fn worker(slot: &Arc<Slot>, commands: &Receiver<Command>) {
        let capability = match AppCapability::Create(&HSTRING::from(LOCATION_CAPABILITY)) {
            Ok(capability) => capability,
            Err(e) => {
                // `REGDB_E_CLASSNOTREG` is Windows 10 before 1903. No retry
                // helps, and this file will not fall back to `Geolocator`.
                log::info!(
                    "no Windows AppCapability for {LOCATION_CAPABILITY} \
                     (0x{:08X}: {e}); location is unavailable on this machine",
                    e.code().0
                );
                slot.store(SLOT_UNAVAILABLE);
                return;
            }
        };

        let access_token = subscribe_access_changed(&capability, slot);

        let mut consecutive_failures: u8 = 0;
        let mut delivery: Option<Delivery> = None;

        loop {
            check_access(&capability, slot, &mut consecutive_failures);

            match commands.recv_timeout(POLL_INTERVAL) {
                Ok(Command::RequestAccess) => request_access(slot),
                Ok(Command::StartDelivery { fixes, wake }) => {
                    // Idempotent: two `Geolocator`s would leak one subscription.
                    if delivery.is_none() {
                        delivery = start_delivery(fixes, wake);
                    }
                }
                Ok(Command::StopDelivery) => delivery = None,
                Ok(Command::OpenSettings) => open_settings(),
                Err(RecvTimeoutError::Timeout) => {}
                // The `OsLocationReader` was dropped: the process is going away.
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }

        if let Some(token) = access_token
            && let Err(e) = capability.RemoveAccessChanged(token)
        {
            log::debug!("could not unsubscribe from Windows location access changes: {e}");
        }
    }

    /// The push half. Without it a permission revoked in Settings goes unnoticed.
    fn subscribe_access_changed(capability: &AppCapability, slot: &Arc<Slot>) -> Option<i64> {
        let slot = Arc::clone(slot);
        // Built and registered here: `TypedEventHandler` is `!Send`.
        let handler = TypedEventHandler::<AppCapability, AppCapabilityAccessChangedEventArgs>::new(
            move |sender, _args| {
                // Runs on an RPC thread; the event carries no status, so the
                // capability is re-read. `AppCapability` is agile.
                let status = sender.ok()?.CheckAccess()?;
                log::info!(
                    "Windows location access changed to {:?}",
                    permission_from_slot(status.0)
                );
                slot.store(status.0);
                Ok(())
            },
        );

        match capability.AccessChanged(&handler) {
            Ok(token) => Some(token),
            Err(e) => {
                // Not fatal: the poll still notices, up to `POLL_INTERVAL` late.
                log::warn!(
                    "could not watch Windows location access changes, \
                     falling back to polling alone: {e}"
                );
                None
            }
        }
    }

    /// The authoritative read. A failure here is neither sticky nor destructive —
    /// see [`slot_after_check_failure`](super::slot_after_check_failure).
    fn check_access(capability: &AppCapability, slot: &Slot, consecutive_failures: &mut u8) {
        match capability.CheckAccess() {
            Ok(status) => {
                *consecutive_failures = 0;
                slot.store(status.0);
            }
            Err(e) => {
                *consecutive_failures = consecutive_failures.saturating_add(1);
                log::debug!("Windows CheckAccess failed ({consecutive_failures} in a row): {e}");
                if let Some(value) =
                    super::slot_after_check_failure(slot.load(), *consecutive_failures)
                {
                    log::warn!(
                        "Windows CheckAccess has failed {consecutive_failures} times \
                         with no answer yet; offering to ask rather than waiting further"
                    );
                    slot.store(value);
                }
            }
        }
    }

    /// Blocks — on a human, if this build of Windows shows the prompt.
    fn request_access(slot: &Slot) {
        match Geolocator::RequestAccessAsync().and_then(|request| request.join()) {
            Ok(status) => {
                log::info!(
                    "Windows answered the location request with {:?}",
                    permission_from_request_result(status.0)
                );
                if let Some(value) = super::slot_after_request(status.0) {
                    slot.store(value);
                }
            }
            // No retry: the gate's attempt bound decides whether anything asks
            // again.
            Err(e) => log::warn!("the Windows location request failed: {e}"),
        }
    }

    /// Build the `Geolocator` and subscribe. Runs on the worker, because a
    /// delegate must be registered on the thread that owns the apartment.
    fn start_delivery(fixes: Sender<Fix>, wake: RedrawWake) -> Option<Delivery> {
        let geolocator = match Geolocator::new() {
            Ok(geolocator) => geolocator,
            Err(e) => {
                log::warn!("could not create a Windows Geolocator: {e}");
                return None;
            }
        };

        // Hints the OS may ignore; a `Geolocator` at its defaults still reports.
        if let Err(e) = geolocator.SetDesiredAccuracy(PositionAccuracy::High) {
            log::debug!("could not raise the Windows position accuracy: {e}");
        }
        if let Err(e) = geolocator.SetReportInterval(REPORT_INTERVAL_MS) {
            log::debug!("could not set the Windows report interval: {e}");
        }
        if let Err(e) = geolocator.SetMovementThreshold(MOVEMENT_THRESHOLD_M) {
            log::debug!("could not set the Windows movement threshold: {e}");
        }

        let position_handler =
            TypedEventHandler::<Geolocator, PositionChangedEventArgs>::new(move |_sender, args| {
                let coordinate = args.ok()?.Position()?.Coordinate()?;
                let fix = super::fix_from_coordinate(
                    coordinate.Latitude()?,
                    coordinate.Longitude()?,
                    coordinate.Accuracy()?,
                    super::reference_value(coordinate.Altitude(), |value| value.Value()),
                    super::reference_value(coordinate.Heading(), |value| value.Value()),
                    super::reference_value(coordinate.Speed(), |value| value.Value()),
                    coordinate.PositionSource()?.0,
                );
                // A closed channel means the bridge has stopped draining.
                if fixes.send(fix).is_ok() {
                    // Under `ControlFlow::Wait` nothing else would drain it.
                    wake();
                }
                Ok(())
            });

        let position_token = match geolocator.PositionChanged(&position_handler) {
            Ok(token) => token,
            Err(e) => {
                log::warn!("could not subscribe to Windows position updates: {e}");
                return None;
            }
        };

        let status_handler =
            TypedEventHandler::<Geolocator, StatusChangedEventArgs>::new(|_sender, args| {
                let status = args.ok()?.Status()?;
                log::info!(
                    "Windows Geolocator status: {}",
                    position_status_label(status.0)
                );
                Ok(())
            });
        let status_token = geolocator.StatusChanged(&status_handler).ok();

        log::info!("Windows location delivery started");
        Some(Delivery {
            geolocator,
            position_token,
            status_token,
        })
    }

    /// `LaunchUriAsync` blocks until the shell has taken the URI.
    fn open_settings() {
        let launched = Uri::CreateUri(&HSTRING::from(LOCATION_SETTINGS_URI))
            .and_then(|uri| Launcher::LaunchUriAsync(&uri))
            .and_then(|launch| launch.join());
        match launched {
            Ok(true) => log::info!("opened {LOCATION_SETTINGS_URI}"),
            Ok(false) => log::warn!("Windows declined to open {LOCATION_SETTINGS_URI}"),
            Err(e) => log::warn!("could not open {LOCATION_SETTINGS_URI}: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_capability_the_app_never_declared_is_a_prompt_and_not_a_denial() {
        assert_eq!(
            permission_from_slot(CAPABILITY_NOT_DECLARED_BY_APP),
            LocationPermission::Prompt,
            "an unpackaged build reports this on every machine; as a denial it \
             would render no button and never ask, permanently"
        );
    }

    #[test]
    fn an_allowed_capability_is_a_granted_permission() {
        assert_eq!(
            permission_from_slot(CAPABILITY_ALLOWED),
            LocationPermission::Granted
        );
    }

    #[test]
    fn a_capability_awaiting_a_prompt_asks_for_one() {
        assert_eq!(
            permission_from_slot(CAPABILITY_USER_PROMPT_REQUIRED),
            LocationPermission::Prompt
        );
    }

    #[test]
    fn a_denial_by_the_user_or_by_the_system_is_a_denial() {
        assert_eq!(
            permission_from_slot(CAPABILITY_DENIED_BY_USER),
            LocationPermission::Denied
        );
        assert_eq!(
            permission_from_slot(CAPABILITY_DENIED_BY_SYSTEM),
            LocationPermission::Denied
        );
    }

    #[test]
    fn a_slot_nobody_has_written_to_yet_reports_that_rather_than_guessing() {
        assert_eq!(
            permission_from_slot(SLOT_UNKNOWN),
            LocationPermission::Unknown
        );
    }

    #[test]
    fn a_machine_with_no_app_capability_class_is_unavailable_not_denied() {
        assert_eq!(
            permission_from_slot(SLOT_UNAVAILABLE),
            LocationPermission::Unavailable,
            "`Denied` would send the user to a Settings page that cannot help"
        );
    }

    /// `AppCapabilityAccessStatus` is a struct with associated constants, not an
    /// enum, so a sixth value cannot be ruled out.
    #[test]
    fn a_status_this_build_has_never_heard_of_still_offers_to_ask() {
        for unknown in [5, 6, 99, i32::MAX] {
            assert_eq!(
                permission_from_slot(unknown),
                LocationPermission::Prompt,
                "status {unknown} left the user with no way forward"
            );
        }
    }

    #[test]
    fn the_sentinels_cannot_be_mistaken_for_a_real_status() {
        for status in [
            CAPABILITY_DENIED_BY_SYSTEM,
            CAPABILITY_NOT_DECLARED_BY_APP,
            CAPABILITY_DENIED_BY_USER,
            CAPABILITY_USER_PROMPT_REQUIRED,
            CAPABILITY_ALLOWED,
        ] {
            assert_ne!(status, SLOT_UNKNOWN);
            assert_ne!(status, SLOT_UNAVAILABLE);
        }
        assert_ne!(SLOT_UNKNOWN, SLOT_UNAVAILABLE);
    }

    #[test]
    fn a_single_check_failure_before_any_answer_is_retried_rather_than_concluded() {
        assert_eq!(
            slot_after_check_failure(SLOT_UNKNOWN, 1),
            None,
            "one failed RPC is not an answer about anything"
        );
        assert_eq!(
            permission_from_slot(SLOT_UNKNOWN),
            LocationPermission::Unknown,
            "and the slot it leaves in place still means `keep looking`"
        );
    }

    #[test]
    fn check_failures_that_never_stop_eventually_offer_the_user_a_button() {
        assert_eq!(
            slot_after_check_failure(SLOT_UNKNOWN, MAX_CONSECUTIVE_CHECK_FAILURES),
            Some(CAPABILITY_USER_PROMPT_REQUIRED)
        );
        assert_eq!(
            permission_from_slot(CAPABILITY_USER_PROMPT_REQUIRED),
            LocationPermission::Prompt
        );
    }

    #[test]
    fn the_last_retry_before_the_bound_still_waits() {
        assert_eq!(
            slot_after_check_failure(SLOT_UNKNOWN, MAX_CONSECUTIVE_CHECK_FAILURES - 1),
            None
        );
    }

    #[test]
    fn a_check_failure_never_overwrites_an_answer_the_os_already_gave() {
        for answered in [
            CAPABILITY_ALLOWED,
            CAPABILITY_DENIED_BY_USER,
            CAPABILITY_DENIED_BY_SYSTEM,
            CAPABILITY_NOT_DECLARED_BY_APP,
            CAPABILITY_USER_PROMPT_REQUIRED,
        ] {
            assert_eq!(
                slot_after_check_failure(answered, MAX_CONSECUTIVE_CHECK_FAILURES * 10),
                None,
                "status {answered} was replaced by a transport failure"
            );
        }
    }

    #[test]
    fn an_allowed_request_grants_and_a_denied_one_denies() {
        assert_eq!(
            permission_from_request_result(ACCESS_ALLOWED),
            LocationPermission::Granted
        );
        assert_eq!(
            permission_from_request_result(ACCESS_DENIED),
            LocationPermission::Denied
        );
    }

    #[test]
    fn a_dismissed_prompt_concludes_nothing_and_asks_for_nothing() {
        assert_eq!(
            permission_from_request_result(ACCESS_UNSPECIFIED),
            LocationPermission::Unknown
        );
        assert_eq!(
            slot_after_request(ACCESS_UNSPECIFIED),
            None,
            "writing `Unknown` into the slot would blank a state already known"
        );
    }

    #[test]
    fn an_unrecognised_request_result_concludes_nothing() {
        for unknown in [3, 42, i32::MIN] {
            assert_eq!(
                permission_from_request_result(unknown),
                LocationPermission::Unknown
            );
            assert_eq!(slot_after_request(unknown), None);
        }
    }

    #[test]
    fn a_seeded_request_result_decodes_to_what_the_user_answered() {
        let granted = slot_after_request(ACCESS_ALLOWED).expect("allowed seeds the slot");
        assert_eq!(permission_from_slot(granted), LocationPermission::Granted);

        let denied = slot_after_request(ACCESS_DENIED).expect("denied seeds the slot");
        assert_eq!(permission_from_slot(denied), LocationPermission::Denied);
    }

    #[test]
    fn only_a_satellite_position_is_called_gps() {
        assert_eq!(
            fix_quality_from_position_source(SOURCE_SATELLITE),
            FixQuality::Gps
        );
    }

    /// Cellular(0), WiFi(2), IPAddress(3), Unknown(4), Default(5), Obfuscated(6).
    #[test]
    fn every_fused_or_inferred_source_is_a_device_fix() {
        for source in [0, 2, 3, 4, 5, 6, 7, -1] {
            assert_eq!(
                fix_quality_from_position_source(source),
                FixQuality::Device,
                "PositionSource {source} was reported as a satellite fix"
            );
        }
    }

    /// `Altitude()` is `Result<IReference<f64>>`, so an unreported field arrives
    /// as an `Err` from the *getter*, not as a `None`.
    #[test]
    fn a_field_the_sensor_did_not_report_reads_as_absent() {
        let missing: Result<f64, ()> = Err(());
        assert_eq!(reference_value(missing, Ok::<f64, ()>), None);
    }

    #[test]
    fn a_reference_that_will_not_read_reads_as_absent() {
        assert_eq!(
            reference_value(Ok::<f64, ()>(1.0), |_| Err::<f64, ()>(())),
            None
        );
    }

    #[test]
    fn a_field_the_sensor_did_report_survives_both_layers() {
        assert_eq!(
            reference_value(Ok::<f64, ()>(42.5), Ok::<f64, ()>),
            Some(42.5)
        );
    }

    #[test]
    fn a_windows_coordinate_becomes_a_fix_with_its_fields_in_the_right_columns() {
        let fix = fix_from_coordinate(35.25, -97.5, 65.0, Some(390.0), Some(271.5), Some(3.5), 2);

        assert_eq!(fix.point.lat, 35.25);
        assert_eq!(fix.point.lon, -97.5);
        assert_eq!(fix.accuracy_m, Some(65.0));
        assert_eq!(fix.altitude_m, Some(390.0));
        assert_eq!(fix.heading_deg, Some(271.5));
        assert_eq!(fix.speed_mps, Some(3.5));
        assert_eq!(fix.fix_quality, FixQuality::Device);
    }

    #[test]
    fn a_windows_fix_always_carries_the_accuracy_windows_reported() {
        let coarse = fix_from_coordinate(35.25, -97.5, 25_000.0, None, None, None, 3);
        assert_eq!(coarse.accuracy_m, Some(25_000.0));
    }

    #[test]
    fn a_satellite_coordinate_is_the_one_that_becomes_a_gps_fix() {
        let fix = fix_from_coordinate(35.25, -97.5, 4.0, None, None, None, SOURCE_SATELLITE);
        assert_eq!(fix.fix_quality, FixQuality::Gps);
        assert!(
            fix.fix_quality.can_relocate(),
            "a satellite fix must still be allowed to refine the radar site"
        );
    }

    #[test]
    fn a_stationary_receiver_reports_no_heading_rather_than_north() {
        let fix = fix_from_coordinate(35.25, -97.5, 12.0, None, None, None, 0);
        assert_eq!(fix.heading_deg, None);
        assert_eq!(fix.speed_mps, None);
        assert_eq!(fix.altitude_m, None);
    }

    // Change detectors by design: OS-defined identifiers, unvalidated at compile
    // time, and both fail quietly when wrong.

    #[test]
    fn the_capability_asked_for_is_the_one_windows_defines() {
        assert_eq!(LOCATION_CAPABILITY, "location");
    }

    #[test]
    fn the_settings_uri_is_the_global_location_page() {
        assert_eq!(LOCATION_SETTINGS_URI, "ms-settings:privacy-location");
        assert!(
            !LOCATION_SETTINGS_URI.contains("squallar"),
            "there is no per-app location page for a desktop app to deep-link to"
        );
    }

    #[test]
    fn every_position_status_is_described_distinctly() {
        let labels: Vec<&str> = (POSITION_STATUS_READY..=POSITION_STATUS_NOT_AVAILABLE)
            .map(position_status_label)
            .collect();
        let unrecognised = position_status_label(POSITION_STATUS_NOT_AVAILABLE + 1);

        for (i, label) in labels.iter().enumerate() {
            assert_ne!(
                *label, unrecognised,
                "PositionStatus {i} fell through to the catch-all arm"
            );
        }
        let mut sorted = labels.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), labels.len(), "two statuses read the same");
    }
}
