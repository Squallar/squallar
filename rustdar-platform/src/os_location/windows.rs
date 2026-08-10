//! Windows: `AppCapability` for the state, `Geolocator` to prompt and to
//! deliver.
//!
//! Modelled on Chromium's `system_geolocation_source_win.cc`, which is worth
//! copying rather than reasoning from the docs because it solves the same
//! problem this file has: a **desktop Win32 process with no appx package
//! identity**. Almost everything written about `Windows.Devices.Geolocation`
//! assumes a packaged app, and the two differ in exactly the places that decide
//! whether location works at all.
//!
//! # Three traps, and what this file does instead
//!
//! **The state does not come from `Geolocator`.** `GeolocationAccessStatus` is
//! about the *stream*, and `Geolocator::LocationStatus` is worse: it reports
//! `NotInitialized` until something has called `GetGeopositionAsync` or
//! subscribed to `PositionChanged`, so reading it to decide whether to ask for
//! permission answers a question about liveness with a word that looks like a
//! permission. The state comes from `AppCapability::Create("location")` +
//! `CheckAccess`, which is a property of the app and answers before anything has
//! been started.
//!
//! **`NotDeclaredByApp` means "ask", not "no".** rustdar ships no appx manifest
//! and therefore declares no `<DeviceCapability Name="location">`, so "not
//! declared by app" is a literal, permanent description of this binary. Reading
//! it as a denial would render no button, never call `RequestAccessAsync`, and
//! so never create the OS-side entry that would have changed the answer —
//! Windows location dead forever and indistinguishable from a user who said no.
//! See [`permission_from_slot`].
//!
//! **Nothing here may block the frame thread.** On Windows 11 24H2 and later an
//! unpackaged process *does* get a one-time system prompt, and that prompt
//! blocks on a human. `RequestAccessAsync().join()` on the event loop's thread
//! would be a UI freeze bounded only by how long the user takes to read it — the
//! P0 hang this project has already had once. Every WinRT call in this file
//! happens on one worker thread instead, and the only thing that crosses back to
//! the frame path is one byte in an atomic — written by the contract's `report`
//! callback, never read by anything here.
//!
//! # Why one worker thread and not several
//!
//! COM apartments. A thread that never calls `CoInitializeEx` is implicitly MTA
//! (windows-rs calls `CoIncrementMTAUsage` on the first activation failure and
//! retries — the `combase.dll` import that puts in the binary is verified), and
//! in an MTA a blocking wait is legal and completions arrive on RPC threads with
//! no message pump. In an STA — which the winit main thread may well be, since
//! it initialises OLE for drag and drop — the same wait blocks *without*
//! pumping and deadlocks. So there is one thread, it is never given an
//! apartment, and everything WinRT touches lives on it.
//!
//! It also settles a constraint that is easy to miss: `TypedEventHandler<T, U>`
//! is itself `!Send` and `!Sync` in windows-rs 0.62 — a bare `IUnknown` plus a
//! `PhantomData`, with no `unsafe impl` — even though `Geolocator` and
//! `AppCapability` are both `Send + Sync`. A delegate must be built and
//! registered on the same thread; only the `i64` token it yields may travel.
//! The closure *inside* it needs `Send` and not `Sync`, so a bare
//! `mpsc::Sender` is enough and no `Mutex` appears anywhere below.
//!
//! # Why the push subscription is not an optional extra
//!
//! `Granted` plus a live stream is a terminal state for the polling gate, so a
//! permission revoked in Settings while rustdar is in the foreground would never
//! be noticed by polling at all — the blue dot would sit there at a position the
//! user has just withdrawn consent for. `AppCapability::AccessChanged` is the
//! push signal that closes it. A slow poll runs alongside anyway, because the
//! event is not universally reliable and Chromium keeps a poll for the same
//! reason.
//!
//! # What is testable here, and how
//!
//! CI has no Windows runner. Everything above the WinRT calls is therefore
//! written as pure functions over plain `i32`s and tested on the Linux host —
//! the same split `rustdar_web::geolocation::fix_from_coords` uses, and for the
//! same reason: a swapped arm in a status mapping is silently valid, throws
//! nothing and logs nothing. The `i32`s are pinned to the real bindings by
//! `const` assertions inside the `live` module, which only a Windows build
//! compiles and therefore only a Windows build evaluates, so the two halves
//! cannot drift.
//!
//! **No `unsafe`.** The whole surface — activation, delegates, futures, `Uri`,
//! `Launcher` — is safe Rust in windows-rs 0.62, which matters because this
//! crate is `#![deny(unsafe_code)]`.

use rustdar_gps::{FixQuality, GpsFix, LocationPermission};

// ── The values this file maps between ───────────────────────────────────
//
// Written out as `i32` rather than used through the generated types, because
// the mapping is the part with a silent failure mode and the Linux host is
// where it can be run. `live` asserts each one against the binding it stands
// for, at compile time, so a windows-rs upgrade that renumbered anything would
// fail the Windows build rather than mis-render a permission.

/// `AppCapabilityAccessStatus::DeniedBySystem` — group policy, or the
/// machine-wide location switch is off.
pub const CAPABILITY_DENIED_BY_SYSTEM: i32 = 0;
/// `AppCapabilityAccessStatus::NotDeclaredByApp` — **the state this binary is
/// normally in.** See [`permission_from_slot`].
pub const CAPABILITY_NOT_DECLARED_BY_APP: i32 = 1;
/// `AppCapabilityAccessStatus::DeniedByUser`.
pub const CAPABILITY_DENIED_BY_USER: i32 = 2;
/// `AppCapabilityAccessStatus::UserPromptRequired`.
pub const CAPABILITY_USER_PROMPT_REQUIRED: i32 = 3;
/// `AppCapabilityAccessStatus::Allowed`.
pub const CAPABILITY_ALLOWED: i32 = 4;

/// `GeolocationAccessStatus::Unspecified` — the prompt was dismissed, or the
/// answer never arrived. Explicitly *not* a denial.
pub const ACCESS_UNSPECIFIED: i32 = 0;
/// `GeolocationAccessStatus::Allowed`.
pub const ACCESS_ALLOWED: i32 = 1;
/// `GeolocationAccessStatus::Denied`.
pub const ACCESS_DENIED: i32 = 2;

/// `PositionSource::Satellite` — the one source that earns
/// [`FixQuality::Gps`]. Everything else Windows fuses (cell, Wi-Fi, IP address,
/// its own `Default` tier) is [`FixQuality::Device`].
pub const SOURCE_SATELLITE: i32 = 1;

/// `PositionStatus` codes, for the one diagnostic line this file logs. Not a
/// permission signal — see the module note.
pub const POSITION_STATUS_READY: i32 = 0;
/// `PositionStatus::Initializing`.
pub const POSITION_STATUS_INITIALIZING: i32 = 1;
/// `PositionStatus::NoData`.
pub const POSITION_STATUS_NO_DATA: i32 = 2;
/// `PositionStatus::Disabled`.
pub const POSITION_STATUS_DISABLED: i32 = 3;
/// `PositionStatus::NotInitialized`.
pub const POSITION_STATUS_NOT_INITIALIZED: i32 = 4;
/// `PositionStatus::NotAvailable`.
pub const POSITION_STATUS_NOT_AVAILABLE: i32 = 5;

/// Nothing has been read yet.
///
/// Negative so it cannot collide with a real `AppCapabilityAccessStatus`, which
/// the SDK numbers from zero upwards. This is the value the shared slot is born
/// holding, and it decodes to [`LocationPermission::Unknown`] — "the platform
/// has not answered yet", which is the one state the gate responds to by
/// waiting rather than acting.
pub const SLOT_UNKNOWN: i32 = -1;

/// There is no `AppCapability` class on this machine.
///
/// Windows 10 before 1903 has no `Windows.Security.Authorization.AppCapabilityAccess`
/// at all, so activation fails with `REGDB_E_CLASSNOTREG` and there is no state
/// source this file is willing to trust in its place. Terminal: the settings
/// pane says location is not available here and stops asking.
pub const SLOT_UNAVAILABLE: i32 = -2;

/// How many consecutive `CheckAccess` failures before the slot stops saying
/// "not yet".
///
/// The failure being guarded against is an RPC hiccup, which is transient by
/// nature — so the first thing tried is simply asking again. But
/// [`Unknown`](LocationPermission::Unknown) is the state that *does nothing*,
/// so mapping a failure straight to it and leaving it there parks the settings
/// pane on "Checking…" for the life of the process and never offers the user a
/// button. Three tries at the poll cadence is a couple of seconds; after that
/// the honest answer is "we do not know, and you may as well try", which is
/// [`Prompt`](LocationPermission::Prompt).
pub const MAX_CONSECUTIVE_CHECK_FAILURES: u8 = 3;

/// The capability name `AppCapability::Create` is asked for.
pub const LOCATION_CAPABILITY: &str = "location";

/// Where `Denied` sends the user.
///
/// The **global** location switch, not a per-app entry: those do not exist for
/// desktop apps, so deep-linking to one would be a link to nothing. Launched
/// through `Launcher::LaunchUriAsync`, which needs no HWND, spawns no process
/// and flashes no console window — all three of which a `cmd /c start` would.
///
/// It is not a promise that Settings helps. The prompt can be suppressed
/// machine-wide (`ShowGlobalPrompts=0`) and the group policy `DisableLocation`
/// greys the toggle out entirely; the pane's wording does not claim otherwise.
pub const LOCATION_SETTINGS_URI: &str = "ms-settings:privacy-location";

// ── The mappings ────────────────────────────────────────────────────────

/// Decode the shared slot — an `AppCapabilityAccessStatus` or one of the two
/// sentinels above — into the app's own permission model.
///
/// # `NotDeclaredByApp` is [`Prompt`](LocationPermission::Prompt)
///
/// This is the single most consequential line in the Windows arm, and the
/// obvious reading of the name gets it backwards. A capability is "declared" by
/// an appx manifest. rustdar has no manifest, will not grow one for a plain
/// desktop build, and so is *permanently* undeclared — the status is a fact
/// about packaging, not about consent.
///
/// Read as a denial it would be self-sealing: the settings pane renders `Denied`
/// with no button, so `RequestAccessAsync` is never called, so Windows never
/// records an access entry for this executable, so `CheckAccess` goes on
/// answering `NotDeclaredByApp` forever. Location would be dead on every Windows
/// machine and would look exactly like a user who had said no. Chromium can map
/// it to a denial because Chromium ships an installer and usually has package
/// identity; that precedent does not transfer.
///
/// # Why the fallback arm is `Prompt` and not `Unknown`
///
/// `AppCapabilityAccessStatus` is a `#[repr(transparent)] struct(pub i32)` with
/// associated constants rather than a Rust enum, so an unlisted value is not
/// merely possible — the compiler cannot even warn about it, which is why the
/// `_` arm is mandatory. If Windows grows a sixth status, `Unknown` would wait
/// forever for a clarification that is never coming and `Denied` would brick the
/// arm. `Prompt` offers a button that either works or produces a definite answer
/// on the next `CheckAccess`, and failing towards "offer to ask" is the safe
/// direction throughout this file.
pub fn permission_from_slot(slot: i32) -> LocationPermission {
    match slot {
        SLOT_UNKNOWN => LocationPermission::Unknown,
        SLOT_UNAVAILABLE => LocationPermission::Unavailable,
        CAPABILITY_ALLOWED => LocationPermission::Granted,
        // Both reversible only by the user, in Settings, and both render the
        // same sentence plus the `ms-settings:` button. `DeniedBySystem` is
        // group policy or the machine-wide switch; the pane does not promise
        // the button will help, only where to look.
        CAPABILITY_DENIED_BY_USER | CAPABILITY_DENIED_BY_SYSTEM => LocationPermission::Denied,
        CAPABILITY_USER_PROMPT_REQUIRED | CAPABILITY_NOT_DECLARED_BY_APP => {
            LocationPermission::Prompt
        }
        _ => LocationPermission::Prompt,
    }
}

/// What a completed `Geolocator::RequestAccessAsync` said.
///
/// `Geolocator` and not `AppCapability::RequestAccessAsync`, deliberately: this
/// is the call that raises the Windows 11 24H2 one-time system prompt for an
/// unpackaged process, and it is the one Chromium's own location provider makes.
/// `AppCapability` is read for state and never asked.
///
/// [`Unspecified`](ACCESS_UNSPECIFIED) is
/// [`Unknown`](LocationPermission::Unknown) and **must not become a retry.** It
/// is what comes back when the prompt was dismissed rather than answered, or
/// when the request never resolved into a decision. Treating it as "ask again"
/// turns a dismissed dialog into a dialog that reappears, which is the exact
/// nagging behaviour the permission gate exists to make impossible.
pub fn permission_from_request_result(status: i32) -> LocationPermission {
    match status {
        ACCESS_ALLOWED => LocationPermission::Granted,
        ACCESS_DENIED => LocationPermission::Denied,
        ACCESS_UNSPECIFIED => LocationPermission::Unknown,
        // `GeolocationAccessStatus` is another `repr(transparent)` struct, so
        // this arm is mandatory. Same reasoning as `permission_from_slot`,
        // except that here the safe direction is "conclude nothing" — the next
        // `CheckAccess` is a fraction of a second away and is authoritative.
        _ => LocationPermission::Unknown,
    }
}

/// The slot value a finished access request justifies writing, or `None` for
/// "leave it and let `CheckAccess` answer".
///
/// The request result is written straight into the slot only so the settings
/// pane reacts to the user's click without waiting out a poll interval. It is
/// **not** a second source of truth: `CheckAccess` runs on the very next turn of
/// the worker loop and overwrites whatever this put there, so a seed that
/// disagrees with the OS self-corrects within one cadence instead of sticking.
///
/// That is why [`Unknown`](LocationPermission::Unknown) writes nothing rather
/// than [`SLOT_UNKNOWN`]. A dismissed prompt is not new information about the
/// permission, and blanking a state already known to be `Granted` would flip the
/// pane back to "Checking…" for no reason.
pub fn slot_after_request(status: i32) -> Option<i32> {
    match permission_from_request_result(status) {
        LocationPermission::Granted => Some(CAPABILITY_ALLOWED),
        LocationPermission::Denied => Some(CAPABILITY_DENIED_BY_USER),
        _ => None,
    }
}

/// The slot value a failed `CheckAccess` justifies writing, or `None` for
/// "leave it".
///
/// Two rules, and they exist for opposite reasons.
///
/// A failure never overwrites an answer that was once real: `CheckAccess` is an
/// RPC and one blip must not flip a granted permission into a prompt, or a
/// denial into a button. So anything other than [`SLOT_UNKNOWN`] is left alone,
/// however many failures pile up behind it.
///
/// A failure *before* any answer must not be sticky either, which is the harder
/// half. The slot starts at [`SLOT_UNKNOWN`], `Unknown` is the state the gate
/// responds to by waiting, and waiting on an RPC that is never going to answer
/// is a settings pane frozen on "Checking…" with no control on it. After
/// [`MAX_CONSECUTIVE_CHECK_FAILURES`] the slot falls back to
/// `UserPromptRequired`, which at worst offers a button that fails visibly.
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

/// How much to trust a position, from the source Windows names for it.
///
/// Only [`Satellite`](SOURCE_SATELLITE) is [`Gps`](FixQuality::Gps). Everything
/// else — cellular, Wi-Fi, IP address, `Unknown`, the `Default` tier and
/// `Obfuscated` — is a fused or inferred position that [`Device`](FixQuality::Device)
/// describes honestly. The distinction is not cosmetic: `FixQuality::Gps` is
/// what `App::upgrade_provisional_site` looks for, so calling an IP lookup a GPS
/// fix would let a city-scale guess spend the one-shot site upgrade.
///
/// `Obfuscated` deserves its own mention: Windows returns a deliberately
/// coarsened position there, and it arrives looking like any other. It lands on
/// `Device` with whatever `Accuracy` the OS reported, which is what the accuracy
/// gate downstream is for.
pub fn fix_quality_from_position_source(source: i32) -> FixQuality {
    if source == SOURCE_SATELLITE {
        FixQuality::Gps
    } else {
        FixQuality::Device
    }
}

/// Collapse the two-layer `Result` a WinRT `IReference<T>` field arrives in.
///
/// `Geocoordinate::Altitude()` and friends are `Result<IReference<f64>>`, not
/// `Result<Option<f64>>`: a null WinRT reference — the sensor did not report
/// this field — surfaces as an `Err` from the getter, and a genuine RPC failure
/// surfaces as an `Err` from the same getter. **The two are indistinguishable
/// without inspecting the HRESULT**, so this deliberately does not log: at a few
/// fixes a minute, a receiver with no altimeter would otherwise produce a steady
/// stream of warnings about a device working exactly as intended. What a missing
/// field costs is one `None` on an optional column of the settings readout, and
/// what a real RPC failure costs is the same — but the *position itself* comes
/// from `Latitude`/`Longitude`/`Accuracy`, which are plain `f64`s whose failure
/// propagates and is logged.
pub fn reference_value<R, T, E>(
    field: Result<R, E>,
    value: impl FnOnce(R) -> Result<T, E>,
) -> Option<T> {
    field.ok().and_then(|reference| value(reference).ok())
}

/// Build a [`GpsFix`] from what a `Geocoordinate` reports.
///
/// Separated from the WinRT reads for the reason the module note gives: this is
/// where a swapped latitude and longitude, or an accuracy landing in the
/// altitude column, would be silently valid.
///
/// `satellites` and `hdop` stay `None` — `GeocoordinateSatelliteData` exists but
/// is a set of dilution figures that only ever appear on a true GNSS fix, and
/// `accuracy_m` is the number every consumer here actually reads.
///
/// `timestamp` stays `None` too, matching the browser bridge. Windows does
/// report one, but nothing in the app reads `GpsFix::timestamp` from a
/// non-serial source: the settings readout deliberately uses when the fix
/// *arrived* rather than when it was measured.
pub fn fix_from_coordinate(
    latitude: f64,
    longitude: f64,
    accuracy_m: f64,
    altitude_m: Option<f64>,
    heading_deg: Option<f64>,
    speed_mps: Option<f64>,
    position_source: i32,
) -> GpsFix {
    GpsFix {
        altitude_m,
        speed_mps,
        heading_deg,
        accuracy_m: Some(accuracy_m),
        fix_quality: fix_quality_from_position_source(position_source),
        // `from_device_position` decides what a fused platform fix is; the
        // quality above is the one field this source can say more about than it
        // can.
        ..GpsFix::from_device_position(latitude, longitude)
    }
}

/// One word for a `PositionStatus`, for the diagnostic log line.
///
/// Not a permission signal, and this file never treats it as one: the status is
/// `NotInitialized` until something subscribes, so it says whether the *stream*
/// is alive. It is here because "granted, subscribed, and still no dot" is the
/// hardest thing to explain on a machine that is not the developer's, and
/// `Disabled` versus `NoData` is the whole answer.
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

// ── The live WinRT half ─────────────────────────────────────────────────

#[cfg(target_os = "windows")]
pub use live::OsLocationReader;

#[cfg(target_os = "windows")]
mod live {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicI32, Ordering};
    use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
    use std::time::Duration;

    use rustdar_gps::GpsFix;
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

    /// The bridge between the `i32`s the mapping layer is written against and
    /// the generated bindings. Evaluated at compile time on Windows only, which
    /// is the entire point: it costs nothing, it cannot be got wrong at runtime,
    /// and a windows-rs release that renumbered a status would fail this build
    /// instead of silently rendering the wrong control.
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

        // The sentinels rely on the SDK numbering its statuses from zero
        // upwards. If a negative one ever appeared they would alias a real
        // state, and `Unknown` or `Unavailable` would be reported for it.
        assert!(super::SLOT_UNKNOWN < 0 && super::SLOT_UNAVAILABLE < 0);
    };

    /// How often `CheckAccess` runs when nothing has asked it to.
    ///
    /// The fallback behind `AccessChanged`, not the primary path — Chromium
    /// keeps one too, because the event does not fire reliably on every build.
    /// Two seconds is far below any cadence a human notices for a permission
    /// they just changed in Settings, and it is one cheap in-process RPC.
    const POLL_INTERVAL: Duration = Duration::from_secs(2);

    /// `Geolocator::ReportInterval`, in milliseconds.
    ///
    /// A *minimum* interval, and a hint: the OS is free to report less often,
    /// and asking for a small number is asking the fused provider to keep
    /// higher-power sources warm. Ten seconds suits an application whose use for
    /// a position is choosing which radar site to open — the nearest one is
    /// ~200 km away, and nothing on screen moves when the user does.
    const REPORT_INTERVAL_MS: u32 = 10_000;

    /// `Geolocator::MovementThreshold`, in metres.
    ///
    /// Suppresses the stream of near-identical positions a stationary receiver
    /// otherwise emits. Generous for the same reason the report interval is: a
    /// hundred metres cannot change which radar site is nearest.
    const MOVEMENT_THRESHOLD_M: f64 = 100.0;

    /// Work for the WinRT thread. Everything here is something that must not
    /// happen on the frame thread.
    enum Command {
        /// Raise the system prompt, if this build of Windows has one.
        RequestAccess,
        /// Subscribe `PositionChanged` and start pushing fixes.
        StartDelivery {
            fixes: Sender<GpsFix>,
            wake: RedrawWake,
        },
        /// Unsubscribe.
        StopDelivery,
        /// Open the machine-wide location switch in Settings.
        OpenSettings,
    }

    /// The latest `AppCapabilityAccessStatus`, or one of the two sentinels, plus
    /// the callback that publishes every change of it.
    ///
    /// The `i32` and the `report` are one value and not two because they must
    /// never be written apart. Four places set this status — the first
    /// `CheckAccess`, the poll, the `AccessChanged` push, the answer to
    /// `RequestAccessAsync` — and a fifth will be added one day. Any of them
    /// that stored the integer and forgot to call `report` would leave the
    /// settings pane showing a permission the OS no longer agrees with, silently
    /// and for the life of the process. [`store`](Self::store) is the only way
    /// to write it, so there is nothing to forget.
    ///
    /// An atomic and not a `Cell`: it is written from the worker, from an RPC
    /// thread inside the `AccessChanged` handler, and read back by
    /// [`slot_after_check_failure`](super::slot_after_check_failure) on the
    /// worker. `Relaxed` throughout — nothing else is published alongside it, so
    /// there is no ordering for an acquire/release pair to establish.
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
    ///
    /// The two lifetimes are why the contract has two phases. Permission state
    /// has to be readable *before* anything starts — that is what decides
    /// whether to ask at all — and has to keep being watched after the user
    /// turns delivery off, so that a revocation made in Settings is still
    /// noticed. Delivery is the part that stops.
    pub struct OsLocationReader {
        /// Dropping this is what stops the worker — a disconnected channel ends
        /// its `recv_timeout` — so nothing needs an explicit shutdown command.
        commands: Sender<Command>,
        /// Cloned into each `StartDelivery`; the fix channel and the waker
        /// outlive any one subscription.
        sink: OsLocationSink,
        /// Whether a `StartDelivery` is outstanding. Not read back off the
        /// worker: a round trip to answer `location_active()` would put a
        /// channel handshake on the frame path.
        delivering: bool,
    }

    impl OsLocationProvider for OsLocationReader {
        /// Start the worker and begin watching the capability. Prompts nobody:
        /// `CheckAccess` is a read.
        ///
        /// Returns immediately, and the first `CheckAccess` has not happened
        /// yet, so the bridge's permission stays
        /// [`Unknown`](rustdar_gps::LocationPermission::Unknown) for a moment — which the
        /// gate is built to wait through, and is why `Unknown` is a state at
        /// all. This is the one arm that deliberately leaves the initial report
        /// unmade.
        fn start(sink: OsLocationSink) -> Option<Self> {
            let slot = Arc::new(Slot {
                status: AtomicI32::new(SLOT_UNKNOWN),
                report: Arc::clone(&sink.report),
            });
            let (commands, inbox) = channel();

            let worker_slot = Arc::clone(&slot);
            if let Err(e) = std::thread::Builder::new()
                .name("rustdar-os-location".to_owned())
                .spawn(move || worker(&worker_slot, &inbox))
            {
                // Out of threads or out of memory. Not `Unknown`: there is no
                // second chance coming, so the pane should say so rather than
                // wait.
                log::error!("could not start the Windows location worker: {e}");
                slot.store(SLOT_UNAVAILABLE);
            }

            Some(Self {
                commands,
                sink,
                delivering: false,
            })
        }

        /// Subscribe first, then ask.
        ///
        /// Both, and in that order, because they are one user-visible act. The
        /// gate calls this in two situations — never asked, and granted but not
        /// delivering — and neither would be served by a method that did only
        /// half of it. Subscribing before the answer arrives is deliberate: a
        /// `Geolocator` with no permission simply reports no positions, and
        /// having the subscription already in place is what makes a grant
        /// produce a fix immediately rather than a poll interval later.
        ///
        /// The `bool` says the request reached the worker. It cannot say more —
        /// whether a dialog appears is up to the Windows build, and whether it
        /// is answered is up to the user.
        fn request(&mut self) -> bool {
            if !self.delivering {
                // Optimistic by necessity: the `Geolocator` is constructed on
                // the worker, because a delegate must be registered on the
                // thread that owns the apartment, so whether it succeeded is not
                // knowable here without blocking — the one thing this file
                // exists to avoid. A failure shows up as a log line and no
                // fixes.
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
            // Best effort. A dead worker has already dropped its `Geolocator`,
            // which unsubscribes on its own.
            let _ = self.commands.send(Command::StopDelivery);
            self.delivering = false;
        }

        fn active(&self) -> bool {
            self.delivering
        }

        /// `ms-settings:privacy-location` is a documented URI that
        /// `Launcher::LaunchUriAsync` opens with no HWND, no spawned process and
        /// no console flash. See [`LOCATION_SETTINGS_URI`].
        fn settings_available() -> bool {
            true
        }

        fn open_settings(&mut self) {
            if self.commands.send(Command::OpenSettings).is_err() {
                log::warn!("the OS location worker is gone; cannot open system settings");
            }
        }
    }

    /// A registered `PositionChanged`/`StatusChanged` pair.
    ///
    /// Exists for its `Drop`: the tokens have to be handed back to the
    /// `Geolocator` they came from, and tying that to a value means the worker
    /// cannot forget on any of the paths that end delivery (an explicit stop,
    /// a dropped [`OsLocationReader`], a worker exiting on channel disconnect).
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

    /// The one thread that touches WinRT.
    ///
    /// It never calls `CoInitializeEx`, which is what makes it implicitly MTA
    /// once windows-rs performs its `CoIncrementMTAUsage` fallback on the first
    /// activation. That is load-bearing twice over: blocking on a future is only
    /// safe in an MTA, and event completions reach an MTA object on an RPC
    /// thread with no message pump to run.
    fn worker(slot: &Arc<Slot>, commands: &Receiver<Command>) {
        let capability = match AppCapability::Create(&HSTRING::from(LOCATION_CAPABILITY)) {
            Ok(capability) => capability,
            Err(e) => {
                // `REGDB_E_CLASSNOTREG` here is Windows 10 before 1903, which
                // has no `AppCapabilityAccess` namespace at all. Anything else
                // is a machine whose WinRT registration is broken. Neither has a
                // retry that would help, and the design refuses to fall back to
                // `GeolocationAccessStatus` for state.
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
                    // Idempotent. The gate can call `request_location` more than
                    // once before `location_active` flips, and two `Geolocator`s
                    // would mean two subscriptions and one leaked.
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

    /// The push half. Without it, a permission revoked in Settings while
    /// rustdar is in the foreground is invisible — see the module note.
    fn subscribe_access_changed(capability: &AppCapability, slot: &Arc<Slot>) -> Option<i64> {
        let slot = Arc::clone(slot);
        // Constructed and registered here, on the worker: `TypedEventHandler`
        // is `!Send`, so it could not have been built anywhere else and handed
        // over. The `i64` token it returns is the only part that travels.
        let handler = TypedEventHandler::<AppCapability, AppCapabilityAccessChangedEventArgs>::new(
            move |sender, _args| {
                // Runs on an RPC thread. The event carries no status, so the
                // capability has to be re-read; `AppCapability` is `Send + Sync`
                // and agile, so reading it from here is fine. `Slot::store`
                // pushes the change into the bridge from this thread too, which
                // is what makes a revocation made in Settings visible at all —
                // the gate stops polling once the answer is `Granted` and
                // delivery is live.
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
                // Not fatal, and not silent: the poll below still notices a
                // change, just up to `POLL_INTERVAL` late.
                log::warn!(
                    "could not watch Windows location access changes, \
                     falling back to polling alone: {e}"
                );
                None
            }
        }
    }

    /// The authoritative read. See [`slot_after_check_failure`] for why a
    /// failure here is not allowed to be sticky *or* destructive.
    ///
    /// [`slot_after_check_failure`]: super::slot_after_check_failure
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

    /// Blocks — on a human, if this build of Windows shows the prompt. That is
    /// exactly why it is here and not on the frame thread.
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
            // Deliberately no retry. A failed ask is a failed ask; the gate's
            // own attempt bound decides whether anything tries again, and this
            // file has no business deciding it a second time.
            Err(e) => log::warn!("the Windows location request failed: {e}"),
        }
    }

    /// Build the `Geolocator` and subscribe. Runs on the worker, because a
    /// delegate has to be registered on the thread that owns the apartment.
    fn start_delivery(fixes: Sender<GpsFix>, wake: RedrawWake) -> Option<Delivery> {
        let geolocator = match Geolocator::new() {
            Ok(geolocator) => geolocator,
            Err(e) => {
                log::warn!("could not create a Windows Geolocator: {e}");
                return None;
            }
        };

        // All three are hints the OS may ignore, and none of them is worth
        // failing over — a `Geolocator` at its defaults still reports positions.
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
                // A closed channel means the bridge has stopped draining. Not an
                // error worth logging at every report interval: the `Drop` that
                // caused it is on its way here as a `StopDelivery`.
                if fixes.send(fix).is_ok() {
                    // Under `ControlFlow::Wait` the loop is parked and nothing
                    // would drain the channel until some other event woke it.
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

    /// `Launcher::LaunchUriAsync` blocks until the shell has handed the URI off,
    /// which is another reason this runs on the worker.
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

    // ── permission_from_slot ────────────────────────────────────────────

    /// The line the whole arm turns on. `NotDeclaredByApp` is what an
    /// unpackaged Win32 binary reports *forever*, so reading it as a refusal
    /// renders no button, never asks, and never lets Windows record the entry
    /// that would have changed the answer.
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

    /// Both denials are reversible only by the user in Settings, which is what
    /// [`LocationPermission::Denied`] means — as distinct from `Unavailable`,
    /// where that advice leads nowhere.
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

    /// `AppCapabilityAccessStatus` is a `repr(transparent)` struct with
    /// associated constants, not an enum, so a value outside the five is a
    /// thing the compiler cannot rule out. `Unknown` would park the pane on
    /// "Checking…" and `Denied` would brick the arm; only `Prompt` degrades
    /// into something a user can act on.
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

    /// The sentinels are only safe because they cannot collide with a real
    /// status, and the two halves of that claim live in different files — the
    /// constants here, the SDK's numbering in `live`'s `const` assertions.
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

    // ── the transient-failure retry ─────────────────────────────────────

    /// The whole point of the retry: an RPC blip on the first read must not
    /// leave the app permanently "Checking…".
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

    /// Retrying forever is the other half of the same bug — the pane would sit
    /// on "Checking…" for the life of the process with no control on it.
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

    /// Pins the boundary itself: one failure short of the bound still waits.
    #[test]
    fn the_last_retry_before_the_bound_still_waits() {
        assert_eq!(
            slot_after_check_failure(SLOT_UNKNOWN, MAX_CONSECUTIVE_CHECK_FAILURES - 1),
            None
        );
    }

    /// A blip must never demote an answer that was once real. Flipping
    /// `Granted` to `Prompt` would re-offer a button for a permission already
    /// held; flipping `Denied` to `Prompt` would offer to ask a user who has
    /// said no.
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

    // ── the request result ──────────────────────────────────────────────

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

    /// A dismissed prompt is `Unspecified`, and the one thing it must not
    /// become is a reason to ask again — that is how a permission dialog turns
    /// into a dialog that keeps coming back.
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

    /// The seed the settings pane reacts to before the next poll lands. It has
    /// to decode back to what the request said, or clicking Allow would show
    /// the wrong thing for a cadence.
    #[test]
    fn a_seeded_request_result_decodes_to_what_the_user_answered() {
        let granted = slot_after_request(ACCESS_ALLOWED).expect("allowed seeds the slot");
        assert_eq!(permission_from_slot(granted), LocationPermission::Granted);

        let denied = slot_after_request(ACCESS_DENIED).expect("denied seeds the slot");
        assert_eq!(permission_from_slot(denied), LocationPermission::Denied);
    }

    // ── PositionSource → FixQuality ─────────────────────────────────────

    #[test]
    fn only_a_satellite_position_is_called_gps() {
        assert_eq!(
            fix_quality_from_position_source(SOURCE_SATELLITE),
            FixQuality::Gps
        );
    }

    /// Cellular(0), WiFi(2), IPAddress(3), Unknown(4), Default(5) and
    /// Obfuscated(6). Calling any of them `Gps` would hand a city-scale guess
    /// the site upgrade that `FixQuality::Gps` gates.
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

    // ── IReference unwrapping ───────────────────────────────────────────

    /// The shape that matters: `Altitude()` is `Result<IReference<f64>>`, so a
    /// field the sensor did not report arrives as an `Err` from the *getter*,
    /// not as a `None`.
    #[test]
    fn a_field_the_sensor_did_not_report_reads_as_absent() {
        let missing: Result<f64, ()> = Err(());
        assert_eq!(reference_value(missing, Ok::<f64, ()>), None);
    }

    /// The second layer: the reference exists but reading through it fails.
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

    // ── the fix ─────────────────────────────────────────────────────────

    /// Latitude and longitude swapped is silently valid — the reason this
    /// mapping is a separate function at all.
    #[test]
    fn a_windows_coordinate_becomes_a_fix_with_its_fields_in_the_right_columns() {
        let fix = fix_from_coordinate(35.25, -97.5, 65.0, Some(390.0), Some(271.5), Some(3.5), 2);

        assert_eq!(fix.latitude, 35.25);
        assert_eq!(fix.longitude, -97.5);
        assert_eq!(fix.accuracy_m, Some(65.0));
        assert_eq!(fix.altitude_m, Some(390.0));
        assert_eq!(fix.heading_deg, Some(271.5));
        assert_eq!(fix.speed_mps, Some(3.5));
        assert_eq!(fix.fix_quality, FixQuality::Device);
    }

    /// `accuracy_m` is what `App::upgrade_provisional_site` reads to refuse a
    /// fix too coarse to improve on the timezone guess. Dropping it would make
    /// a 25 km Wi-Fi fix indistinguishable from a metre-accurate one.
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

    /// Absent optional fields stay absent rather than defaulting to zero: a
    /// fabricated heading of 0° would rotate a heading-up map to due north.
    #[test]
    fn a_stationary_receiver_reports_no_heading_rather_than_north() {
        let fix = fix_from_coordinate(35.25, -97.5, 12.0, None, None, None, 0);
        assert_eq!(fix.heading_deg, None);
        assert_eq!(fix.speed_mps, None);
        assert_eq!(fix.altitude_m, None);
    }

    // ── the two strings Windows has to recognise ────────────────────────
    //
    // Change detectors, and deliberately so. Both are identifiers the OS
    // defines, neither is validated at compile time, and both fail *quietly*
    // when wrong: a mistyped capability name makes `AppCapability::Create`
    // return an error that reads exactly like "this machine is too old", and a
    // mistyped URI makes the settings button a no-op the user is left staring
    // at. There is nowhere else these could be caught.

    #[test]
    fn the_capability_asked_for_is_the_one_windows_defines() {
        assert_eq!(LOCATION_CAPABILITY, "location");
    }

    #[test]
    fn the_settings_uri_is_the_global_location_page() {
        assert_eq!(LOCATION_SETTINGS_URI, "ms-settings:privacy-location");
        assert!(
            !LOCATION_SETTINGS_URI.contains("rustdar"),
            "there is no per-app location page for a desktop app to deep-link to"
        );
    }

    // ── the diagnostic label ────────────────────────────────────────────

    /// Every `PositionStatus` has to say something distinct, because "granted,
    /// subscribed, still no dot" is the report this label exists to answer and
    /// `Disabled` versus `NoData` is the whole of that answer.
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
