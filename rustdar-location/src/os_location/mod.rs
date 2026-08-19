//! The seam between the platform bridges and whatever location service the OS
//! offers.
//!
//! Windows has `Geolocator` and `AppCapability`, Apple has `CLLocationManager`,
//! Linux has xdg-desktop-portal — first-class services rustdar had never asked.
//! Each gets
//! a private module here exposing one type, `OsLocationReader`, implementing one
//! trait, [`OsLocationProvider`]. That trait is the contract, it is declared
//! once, and the compiler checks every arm against it.
//!
//! **This module is the entire `cfg` surface.** Nothing outside it names a
//! target, and no provider file carries a `cfg` naming a *different* target than
//! the arm that selects it. A per-OS `cfg` spread across call sites is how a
//! build ends up compiling two providers, or none, on a target nobody tested.
//!
//! Landing a provider touches this file, the provider's own file, and that
//! target's `os-providers`-fenced dependency block in
//! `rustdar-location/Cargo.toml`. **Nothing else** — not the shell's
//! `platform.rs`, not any `lib.rs`. The three providers that landed here each
//! discovered independently that the older wording ("a one-line change here")
//! was false, because the older contract was not one contract: each of them had
//! to add a parameter `unsupported` did not take, and each therefore had to
//! reach into the shell's `platform.rs` to call it. [`OsLocationProvider`] is
//! what makes the claim true, and the claim is checkable: every `target_os`
//! left in the shell's `platform.rs` says which *bridge* exists, and not one of
//! them says anything about how that bridge does location.
//!
//! Since WO-RL-3 the arms live here in the facade rather than in the shell —
//! every remote location arm belongs to rustdar-location (seam ruling 6) —
//! and since WO-RL-4 the shell's wiring does too: `OsBackend` (backend.rs, re-exported here) is the arm
//! the desktop and iOS shells hand to the app, and the
//! `OsLocationReader`/`OsLocationProvider`/`OsLocationSink` seam is
//! crate-internal.

mod backend;
#[cfg(target_os = "linux")]
mod linux;
mod unsupported;

pub use backend::OsBackend;

/// CoreLocation, for macOS and iOS.
///
/// Declared unconditionally, unlike the arm below that selects it: the half of
/// that file which decides what a `CLAuthorizationStatus` means and which of a
/// `CLLocation`'s components are sentinels names no Objective-C type at all,
/// and the `cargo test` that has to run those decisions is a Linux one. The
/// CoreLocation half carries the same `cfg` this module's Apple arm does.
mod apple;

// The arm table. Written on `target_os` and never on `unix` or `target_family`,
// because both of those get it wrong in a way that compiles: Android *is*
// `unix` and would take the Linux arm despite having a completely different
// location API reached over JNI, and iOS is `unix` but not `macos` while
// sharing macOS's CoreLocation. `target_os` is the only axis on which these
// four are actually distinct.
//
// The `not(...)` arm is not a leftover. wasm32 lands there — it is neither
// Android nor iOS, so it selects `DesktopPlatform` and therefore compiles this
// module, and `clippy.yaml` runs `cargo check --workspace --target
// wasm32-unknown-unknown` over it on every push. A browser tab genuinely has no
// OS location service (the page's own Geolocation API is the *web bridge's*
// business, not this one's), so `unsupported` is the right answer there and not
// a stub.

/// `org.freedesktop.portal.Location` over `ashpd`, on a thread that drives its
/// futures itself. Never GeoClue directly: the portal's `disable-location` is a
/// user preference, and going around it is going around the user.
#[cfg(target_os = "linux")]
use linux as provider;

/// `AppCapability` for the state, `Geolocator` to prompt, an MTA worker to keep
/// `RequestAccessAsync` off the frame thread.
///
/// Compiled under `test` as well as on Windows, and that is not a convenience.
/// The half of that file worth testing is pure — which `AppCapabilityAccessStatus`
/// means "ask", which `PositionSource` is a satellite, how a null `IReference`
/// decodes — and every one of those failures is silent on a device: a wrong arm
/// renders the wrong control or the wrong fix quality and throws nothing. CI has
/// no Windows runner to run tests on, so the mapping is written against plain
/// `i32`s, pinned to the real bindings by `const` assertions that only a Windows
/// build evaluates, and exercised on the Linux host that `cargo test` runs on.
#[cfg(any(target_os = "windows", test))]
mod windows;

/// `self::`, because a bare `windows` here is ambiguous with the crate of that
/// name — uniform paths would find both and refuse to choose.
#[cfg(target_os = "windows")]
use self::windows as provider;

/// One `apple.rs` with two `#[cfg]` islands — `CLLocationManager` is the same
/// on both, but macOS constructs its bridge after `NSApplication` exists and
/// may not be running from a bundle at all, while iOS constructs it before
/// `UIApplicationMain` has run.
#[cfg(any(target_os = "macos", target_os = "ios"))]
use apple as provider;

/// Everything else, wasm32 included.
#[cfg(not(any(
    target_os = "linux",
    target_os = "windows",
    target_os = "macos",
    target_os = "ios"
)))]
use unsupported as provider;

pub(crate) use provider::OsLocationReader;

// ── The provider contract ───────────────────────────────────────────────

/// Asks the event loop for a frame, so that something a provider pushed while
/// the loop was parked is actually seen.
///
/// `Arc<dyn …>` and not `impl Fn`, because two providers hand it to more than
/// one place: Linux clones it into every session thread it starts, Windows
/// clones it into every `StartDelivery` command. `Send + Sync` is what
/// `RedrawWaker` already guarantees — `rustdar-app` pins that with a
/// `const` assertion — so requiring it here costs nothing and is what makes the
/// clone legal.
pub(crate) type RedrawWake = crate::provider::Wake;

/// Announces a permission the app did not ask for.
pub(crate) type ReportPermission =
    std::sync::Arc<dyn Fn(crate::LocationPermission) + Send + Sync + 'static>;

/// The three ways a provider talks back to the app, and the only three.
///
/// One struct rather than three parameters because they travel together through
/// every layer of every provider and are never used apart — Linux had already
/// bundled exactly these into a private `Consumer` before this trait existed —
/// and because a fourth thing, if one is ever needed, is then a field rather
/// than a fourth edit to four arms.
///
/// `Clone` is load-bearing: a provider that can be stopped and started again
/// needs to hand a fresh copy to each session it runs.
#[derive(Clone)]
pub(crate) struct OsLocationSink {
    /// Where fixes go. `DesktopPlatform` drains this alongside the serial
    /// reader's and picks between them; see [`crate::prefer_fix`].
    pub fixes: std::sync::mpsc::Sender<crate::Fix>,
    /// See [`RedrawWake`].
    pub wake: RedrawWake,
    /// See [`ReportPermission`] and the note on [`OsLocationProvider::start`].
    pub report: ReportPermission,
}

/// One shape for every arm of the table above.
///
/// # Why this is a trait and not a convention
///
/// It used to be a convention: `unsupported.rs` carried a signature and each
/// provider was expected to match it. Three providers landed and none of them
/// did — Linux needed a permission callback, Apple needed a waker and four more
/// methods, Windows needed a waker and routed around the mismatch with a
/// differently-shaped constructor of its own. Nothing caught any of that,
/// because nothing was comparing them. A trait is compared by the compiler.
///
/// # Why the parameter is not a `SerialConfig`
///
/// The old signature took one (a `GpsConfig`, then), inherited from
/// `SerialGpsReader::start` (rustdar-nmea-serial, an optional dependency this
/// doc cannot link across), whose
/// job it is to open a serial port. That config carries a port name and a baud
/// rate: settings for a piece of hardware the user
/// plugged in. A portal session, a WinRT `Geolocator` and a `CLLocationManager`
/// have none of those and can use none of them — every provider that landed
/// ignored the argument, and one of them said so in a doc comment. What a
/// location session actually needs is somewhere to put a fix, a way to ask for
/// the frame that will show it, and a way to say the permission changed. That
/// is [`OsLocationSink`], and it is the whole parameter list.
///
/// # The two phases, and why there are two
///
/// [`start`](Self::start) brings the provider up. It **must not prompt and must
/// not deliver**: it runs once, from `set_redraw_waker`, before the first frame.
/// [`request`](Self::request) is the user-visible act — prompt if the platform
/// prompts, then deliver.
///
/// Splitting them is what lets all three providers keep the behaviour they were
/// each built for. Windows' `AppCapability` watcher and Apple's
/// `locationManagerDidChangeAuthorization:` have to be live *before* anything is
/// asked, because they are what answers "may we?", and have to stay live *after*
/// delivery stops, or a revocation made in system settings is never noticed.
/// Linux's `Start()` can sit on an agent dialog for as long as the user takes,
/// so it cannot be part of bringing the provider up at all. One phase would have
/// forced two of the three to lie.
///
/// # The permission lives in the bridge, not in here
///
/// Providers **push** through [`OsLocationSink::report`]; nothing asks them.
/// There is no `permission()` on this trait, and that is deliberate: the gate
/// answers `Denied` by calling `stop_location`, so any state kept inside the
/// value being stopped evaporates at exactly the moment it starts to matter.
/// `DesktopPlatform` holds one atomic that outlives every session, and
/// `report` is what writes it.
///
/// A provider is expected to report its true initial state during `start`, or to
/// leave it deliberately at [`Unknown`] — the one state that neither asks nor
/// concludes — when it genuinely does not know yet. Linux reports [`Prompt`]
/// (nobody has been asked, and asking is the only way to find out); Apple
/// reports the real `CLAuthorizationStatus`, which it can read synchronously;
/// Windows leaves it `Unknown` until its worker's first `CheckAccess`, which is
/// what the settings pane's "Checking…" exists for.
///
/// [`Unknown`]: crate::LocationPermission::Unknown
/// [`Prompt`]: crate::LocationPermission::Prompt
pub(crate) trait OsLocationProvider: Sized {
    /// Bring the provider up, prompting nobody and delivering nothing.
    ///
    /// `None` means this build or this machine has no location service to
    /// subscribe to — which is not the same as the user having said no, and the
    /// bridge renders it as [`Unavailable`] rather than as a refusal.
    ///
    /// [`Unavailable`]: crate::LocationPermission::Unavailable
    fn start(sink: OsLocationSink) -> Option<Self>;

    /// Prompt if the platform needs prompting, and start delivering.
    ///
    /// The `bool` is the hint `LocationBridge::request_location` documents, and
    /// nothing durable may hang off it: two of the three platforms cannot tell
    /// whether the ask reached a human.
    fn request(&mut self) -> bool;

    /// Stop delivering. Never revokes — no platform offers an app a way to hand
    /// a permission back — and never tears down the permission watcher, which
    /// is the thing that would notice a change made while delivery is off.
    fn stop(&mut self);

    /// Whether fixes are being delivered right now. Not "granted": those are
    /// different states and the settings pane shows both.
    fn active(&self) -> bool;

    /// Whether this platform has a location settings page worth offering.
    ///
    /// An associated function and not a method, because it is a property of the
    /// build rather than of any state — and because `App::new` asks it once,
    /// before `set_redraw_waker` has run, so at a point where no provider has
    /// been constructed yet. A `&self` answer would be `false` on Windows
    /// forever, which is a button that never appears.
    fn settings_available() -> bool {
        false
    }

    /// Open the system location settings. Fire and forget; must not block.
    fn open_settings(&mut self) {}
}
