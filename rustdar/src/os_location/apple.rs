//! CoreLocation, shared by macOS and iOS.
//!
//! One file, two halves, and the split is not cosmetic.
//!
//! The **decoding half** — `CLAuthorizationStatus` → [`LocationPermission`],
//! `CLLocation` → [`Fix`] — is pure arithmetic over `f64`s and one `i32`,
//! and it compiles on every target. That is deliberate: it holds every rule
//! this provider can get wrong (which sentinel is a sentinel, which one is
//! not, what an unrecognised status means) and it is the only part testable
//! by the `cargo test` that actually runs, which is a Linux one.
//!
//! The **CoreLocation half** is behind the one `cfg` and is as thin as it can
//! be: message sends, and a delegate that hands its arguments straight to the
//! decoding half.
//!
//! # Two asymmetries between the platforms
//!
//! Both are marked with their own `cfg` below.
//!
//! * **macOS constructs this after `EventLoop::new()`**, so `NSApplication`
//!   exists — but it may not be running from a bundle at all (`cargo run`
//!   produces a bare Mach-O). CoreLocation refuses to prompt for an
//!   unbundled executable, silently, so macOS checks first.
//! * **iOS constructs this before `UIApplicationMain` has run.** The main
//!   thread is the main thread, so the `MainThreadMarker` is available; what
//!   is not available yet is a *running* run loop. Delegate callbacks are
//!   scheduled on it and arrive once UIKit starts spinning it, which is a few
//!   milliseconds later and needs no handling — but it does mean the first
//!   authorisation callback cannot be waited for, which is why the initial
//!   status is also read synchronously.
//!
//! # What this provider does not do
//!
//! iOS has no `UIBackgroundModes: location` in `packaging/ios/Info.plist`, so the OS
//! stops delivering the moment the app backgrounds and resumes when it
//! foregrounds. Nothing here can observe that: `-locationManager:
//! didUpdateLocations:` simply stops being called, and there is no "paused"
//! callback for it (`locationManagerDidPauseLocationUpdates:` is about
//! `pausesLocationUpdatesAutomatically`, which this provider never turns on).
//! So [`OsLocationReader::active`] keeps reporting `true` across a background
//! transition and the map keeps the last dot. That is the honest state of it:
//! the fix is stale, not wrong, and the settings pane's "last fix" line —
//! which is timed off arrival, not off the fix's own clock — is what tells
//! the user so.
//!
//! Fixing it properly means `allowsBackgroundLocationUpdates`, and that
//! property **throws** when set without the matching `UIBackgroundModes`
//! entitlement. It is not set here and must not be set without the plist key
//! landing first.

// Every CoreLocation binding in `objc2-core-location` 0.3.2 is an `unsafe fn`,
// including the ones that read like plain property getters and the constructor
// that reads like `Default::default()`. The blocks below are the entire
// CoreLocation surface of this crate and each carries its own SAFETY note.
//
// `objc2`'s `define_class!` contributes none of them: it expands `unsafe impl`
// and `#[unsafe(method(...))]` from an external macro, which the lint treats as
// out of scope.
#![allow(
    unsafe_code,
    reason = "every objc2-core-location binding is an `unsafe fn`; the blocks \
              below are the whole surface and each has its own SAFETY note"
)]
// The decoding half is compiled on every target so its tests run on the host,
// but only the Apple arm of `mod.rs` has a caller for it.
#![cfg_attr(
    not(any(target_os = "macos", target_os = "ios")),
    allow(
        dead_code,
        reason = "the decoding half compiles everywhere so `cargo test` can \
                  reach it; its only non-test caller is behind the Apple cfg"
    )
)]

use rustdar_location::{Fix, LocationPermission};

// ── The decoding half ───────────────────────────────────────────────────
//
// Compiled on every target. Nothing below this comment mentions Objective-C.

/// The `CLAuthorizationStatus` constants, as their raw `c_int` values.
///
/// Named here rather than matched against `CLAuthorizationStatus::` so that
/// the mapping — the part with the judgement in it — is reachable from a host
/// test. The Apple half asserts these against the real constants at compile
/// time, so a renumbering in a future SDK fails the build rather than the
/// behaviour.
mod status {
    pub const NOT_DETERMINED: i32 = 0;
    pub const RESTRICTED: i32 = 1;
    pub const DENIED: i32 = 2;
    pub const AUTHORIZED_ALWAYS: i32 = 3;
    pub const AUTHORIZED_WHEN_IN_USE: i32 = 4;
}

/// Decode a raw `CLAuthorizationStatus` into the app's model.
///
/// # Why `==` and not `match`
///
/// `CLAuthorizationStatus` is a `#[repr(transparent)]` newtype over `c_int`
/// with associated constants, not a Rust `enum`. A `match` on those constants
/// compiles, but it can never be exhaustive — the compiler still demands arms
/// for `(i32::MIN..=-1)` and `(5..=i32::MAX)` — so it buys none of the
/// exhaustiveness a real enum would while looking like it does. Taking the
/// `i32` and comparing is the same logic without the implied guarantee, and it
/// is what makes this function callable from a test on a machine with no
/// CoreLocation.
///
/// # The two judgement calls
///
/// **`Restricted` maps to `Denied`, not `Unavailable`.** It means an MDM
/// profile or Screen Time has taken the decision away from the user, so
/// "you can turn this back on in system settings" is not quite true. But
/// `Unavailable` says "this platform has no location service", which is
/// flatly false and renders no explanation at all; Location Services does
/// list the app, greyed out, with the reason. The less-wrong sentence wins.
///
/// **An unrecognised value is `Unknown`, not a denial.** `Unknown` is the
/// state that does nothing and looks again, so a status a future SDK adds
/// costs a poll cycle. Mapping it to `Denied` would render a permanent
/// "you said no" for a value the user never chose.
fn permission_from_status(raw: i32) -> LocationPermission {
    if raw == status::NOT_DETERMINED {
        LocationPermission::Prompt
    } else if raw == status::AUTHORIZED_ALWAYS || raw == status::AUTHORIZED_WHEN_IN_USE {
        // Coarse/fine and always/when-in-use both collapse here; see
        // `LocationPermission`'s own note for why nothing downstream can use
        // the distinction.
        LocationPermission::Granted
    } else if raw == status::DENIED || raw == status::RESTRICTED {
        LocationPermission::Denied
    } else {
        LocationPermission::Unknown
    }
}

/// Everything a `CLLocation` carries that this app has a field for, already
/// out of Objective-C.
///
/// A struct rather than seven positional arguments, every one of which is an
/// `f64` of metres or degrees: a transposed pair would type-check.
#[derive(Debug, Clone, Copy)]
struct LocationComponents {
    latitude: f64,
    longitude: f64,
    /// Metres relative to sea level. **Not** sign-sentinelled — see
    /// [`fix_from_components`].
    altitude_m: f64,
    /// Negative when the coordinate is invalid.
    horizontal_accuracy_m: f64,
    /// Negative when `altitude_m` is invalid.
    vertical_accuracy_m: f64,
    /// Negative when invalid.
    speed_mps: f64,
    /// Degrees true, negative when invalid.
    course_deg: f64,
}

/// CoreLocation signals "this component is invalid" with a *negative value*,
/// not with `NaN` and not with a separate flag.
///
/// `< 0.0` and never `== -1.0`. The headers document the rule by sign, and
/// `-1.0` is only the value the simulator happens to use; a device reporting
/// `-1.0000001` would sail through an equality test as a real reading.
fn valid_component(v: f64) -> Option<f64> {
    if v < 0.0 { None } else { Some(v) }
}

/// Decode a position report, or reject it.
///
/// # The four sentinels, and the field that is not one
///
/// `_LocationEssentials.framework`'s `CLLocationEssentials.h` documents
/// exactly four components as "negative if invalid": `horizontalAccuracy`,
/// `verticalAccuracy`, `course` and `speed`.
///
/// **`altitude` is not one of them.** The same header says it "can be positive
/// (above sea level) or negative (below sea level)", so filtering it on its
/// own sign would silently discard every fix from Death Valley, the Dead Sea,
/// the Netherlands below NAP, and any car park under one of them. The reading
/// that says whether the altitude means anything is `verticalAccuracy`, and
/// that is what gates it here.
///
/// # Why an invalid horizontal accuracy rejects the whole report
///
/// A negative `horizontalAccuracy` does not mean "the accuracy is unknown"; it
/// means the *coordinate* is invalid. Passing the latitude and longitude on
/// with `accuracy_m: None` would hand the app a position it was told not to
/// believe, in the one shape (`None`) that every consumer treats as "this
/// source does not report accuracy" and therefore as passing.
fn fix_from_components(c: LocationComponents) -> Option<Fix> {
    let horizontal_accuracy_m = valid_component(c.horizontal_accuracy_m)?;
    Some(Fix {
        // Present only when `verticalAccuracy` says the altitude is real; the
        // altitude's own sign says nothing about its validity.
        altitude_m: valid_component(c.vertical_accuracy_m).map(|_| c.altitude_m),
        speed_mps: valid_component(c.speed_mps),
        heading_deg: valid_component(c.course_deg),
        accuracy_m: Some(horizontal_accuracy_m),
        // Left `None` on purpose. `CLLocation::timestamp` is when the *fix was
        // measured* by the device's clock; the only thing that asks about fix
        // age is the settings pane, and it deliberately times from arrival
        // instead (see `Gui::user_fix_at`). Populating this would add a
        // `chrono` dependency to this crate to feed a field with no reader.
        timestamp: None,
        // `Device`, not `Gps`: CoreLocation fuses GNSS, Wi-Fi and cell and
        // does not say which won.
        ..Fix::from_device_position(c.latitude, c.longitude)
    })
}

// ── The CoreLocation half ───────────────────────────────────────────────

#[cfg(any(target_os = "macos", target_os = "ios"))]
mod corelocation {
    use super::{LocationComponents, fix_from_components, permission_from_status, status};

    use std::cell::Cell;
    use std::rc::Rc;
    use std::sync::mpsc::Sender;

    use objc2::rc::Retained;
    use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
    use objc2::{DefinedClass, MainThreadOnly, define_class};
    use objc2_core_location::{
        CLAuthorizationStatus, CLError, CLLocation, CLLocationManager, CLLocationManagerDelegate,
        kCLErrorDomain, kCLLocationAccuracyBest,
    };
    use objc2_foundation::{MainThreadMarker, NSArray, NSError};
    // macOS only, and so is its one use below.
    #[cfg(target_os = "macos")]
    use objc2_foundation::NSBundle;
    use rustdar_location::{Fix, LocationPermission};

    use super::super::{OsLocationProvider, OsLocationSink, RedrawWake, ReportPermission};

    /// The raw constants the decoding half is written against really are the
    /// ones this SDK ships. A renumbering in a future `objc2-core-location`
    /// fails the build here rather than mapping "denied" onto "granted" at
    /// runtime.
    const _: () = {
        assert!(CLAuthorizationStatus::NotDetermined.0 == status::NOT_DETERMINED);
        assert!(CLAuthorizationStatus::Restricted.0 == status::RESTRICTED);
        assert!(CLAuthorizationStatus::Denied.0 == status::DENIED);
        assert!(CLAuthorizationStatus::AuthorizedAlways.0 == status::AUTHORIZED_ALWAYS);
        assert!(CLAuthorizationStatus::AuthorizedWhenInUse.0 == status::AUTHORIZED_WHEN_IN_USE);
    };

    /// How far the device must move before CoreLocation reports again, in
    /// metres.
    ///
    /// The app picks a radar site among WSR-88Ds ~200 km apart and draws one
    /// dot. Ten metres is already far finer than either needs; the point of
    /// setting it at all is that the default (`kCLDistanceFilterNone`) reports
    /// *every* update, which on a phone is a wake per second forever.
    const DISTANCE_FILTER_M: f64 = 10.0;

    /// State shared between the reader and its delegate.
    ///
    /// `Rc` and `Cell`, not `Arc` and atomics: the delegate class is
    /// `MainThreadOnly`, `CLLocationManager` is neither `Send` nor `Sync`, and
    /// CoreLocation delivers on the run loop the manager was created on — which
    /// is the winit event loop, the same thread that reads these. The compiler
    /// enforces all of that, so a lock here would only cost.
    struct Shared {
        fixes: Sender<Fix>,
        /// Asks the event loop for a frame. Callbacks arrive on the main run
        /// loop, but winit parks in `ControlFlow::Wait` and a run-loop source
        /// firing produces no `RedrawRequested` on its own — so without this a
        /// fix sits in the channel until something else happens to draw.
        wake: RedrawWake,
        /// Publishes an authorisation change into the bridge's atomic.
        ///
        /// `locationManagerDidChangeAuthorization:` is a genuine push — a
        /// revocation made in System Settings with rustdar in the foreground
        /// reaches us without anyone polling for it — and this is where that
        /// becomes a permission the settings pane can render.
        report: ReportPermission,
        /// Last thing the OS said. A local copy of what `report` published,
        /// kept because [`sync_updates`] and [`OsLocationReader::request`] both
        /// branch on it and neither can read the bridge's atomic from here.
        permission: Cell<LocationPermission>,
        /// Whether the app has asked for delivery. Distinct from `updating`:
        /// this survives a denial, so a later grant resumes without the user
        /// pressing the button again.
        wants_updates: Cell<bool>,
        /// Whether `startUpdatingLocation` is currently outstanding.
        updating: Cell<bool>,
    }

    impl Shared {
        /// Publish a new authorisation status and give the gate a frame to
        /// notice it on.
        fn set_permission(&self, permission: LocationPermission) {
            if self.permission.replace(permission) != permission {
                log::info!("CoreLocation authorization is now {permission:?}");
                (self.report)(permission);
                (self.wake)();
            }
        }
    }

    /// Start or stop delivery so that it matches what the app asked for and
    /// what the OS allows.
    ///
    /// One place, called from both the authorisation callback and
    /// [`OsLocationReader::request`], because the two orders of events are
    /// equally normal: a first run prompts and starts from the callback, and a
    /// returning user is already `Granted` when the button is pressed and gets
    /// no further callback to start from.
    fn sync_updates(manager: &CLLocationManager, shared: &Shared) {
        let wanted =
            shared.wants_updates.get() && shared.permission.get() == LocationPermission::Granted;
        if wanted == shared.updating.get() {
            return;
        }
        if wanted {
            // SAFETY: no documented preconditions; the manager is live and we
            // are on the thread it was created on.
            unsafe { manager.startUpdatingLocation() };
            log::info!("CoreLocation updates started");
        } else {
            // SAFETY: as above.
            unsafe { manager.stopUpdatingLocation() };
            log::info!("CoreLocation updates stopped");
        }
        shared.updating.set(wanted);
    }

    /// Pull the seven numbers out of a `CLLocation`.
    ///
    /// Reading only; every decision about what they mean is
    /// [`fix_from_components`]'s, which is why this function has no branches.
    fn components_of(location: &CLLocation) -> LocationComponents {
        // SAFETY: plain property reads on a live `CLLocation`. None of them
        // has a documented precondition, and none of them can fail.
        unsafe {
            let coordinate = location.coordinate();
            LocationComponents {
                latitude: coordinate.latitude,
                longitude: coordinate.longitude,
                altitude_m: location.altitude(),
                horizontal_accuracy_m: location.horizontalAccuracy(),
                vertical_accuracy_m: location.verticalAccuracy(),
                speed_mps: location.speed(),
                course_deg: location.course(),
            }
        }
    }

    define_class!(
        // SAFETY:
        // - `NSObject` has no subclassing requirements.
        // - `Delegate` does not implement `Drop`.
        #[unsafe(super(NSObject))]
        // CoreLocation delivers on the run loop the manager was created on,
        // which is the main one. Declaring that is what makes the `Cell`s in
        // `Shared` sound without a lock.
        #[thread_kind = MainThreadOnly]
        #[ivars = Rc<Shared>]
        #[name = "RustdarCLLocationDelegate"]
        struct Delegate;

        // SAFETY: `NSObjectProtocol` has no safety requirements.
        unsafe impl NSObjectProtocol for Delegate {}

        // SAFETY: each selector is spelled exactly as `CLLocationManagerDelegate`
        // declares it and the Rust signatures match the protocol's.
        unsafe impl CLLocationManagerDelegate for Delegate {
            #[unsafe(method(locationManager:didUpdateLocations:))]
            fn did_update_locations(
                &self,
                _manager: &CLLocationManager,
                locations: &NSArray<CLLocation>,
            ) {
                let shared = self.ivars();
                let mut delivered = 0_usize;
                for location in locations.iter() {
                    let Some(fix) = fix_from_components(components_of(&location)) else {
                        // A negative `horizontalAccuracy`: CoreLocation is
                        // telling us the coordinate is meaningless. Common
                        // enough on a cold start to be `debug`, not `warn`.
                        log::debug!("CoreLocation reported a fix with an invalid coordinate");
                        continue;
                    };
                    if shared.fixes.send(fix).is_err() {
                        // The bridge dropped its receiver, i.e. it is going
                        // away. Nothing to recover; the manager stops when the
                        // reader drops.
                        log::debug!("nobody is listening for OS location fixes any more");
                        return;
                    }
                    delivered += 1;
                }
                if delivered > 0 {
                    (shared.wake)();
                }
            }

            #[unsafe(method(locationManagerDidChangeAuthorization:))]
            fn did_change_authorization(&self, manager: &CLLocationManager) {
                let shared = self.ivars();
                // SAFETY: the *instance* property, macOS 11 / iOS 14. The
                // deployment floor that makes this resolve is set in
                // `.cargo/config.toml`; without it this is a
                // `doesNotRecognizeSelector:` at runtime and nothing at build
                // time.
                let status = unsafe { manager.authorizationStatus() };
                shared.set_permission(permission_from_status(status.0));
                // Delivery starts *here*, not at the point the app asked. On a
                // first run `requestWhenInUseAuthorization` returns
                // immediately, long before the user has looked at the dialog.
                sync_updates(manager, shared);
            }

            #[unsafe(method(locationManager:didFailWithError:))]
            fn did_fail_with_error(&self, manager: &CLLocationManager, error: &NSError) {
                let shared = self.ivars();
                let domain = error.domain();
                let code = error.code();
                // SAFETY: reading an `extern "C"` static `NSString` the
                // framework defines; it is initialised before any CoreLocation
                // call can have been made. The domain is checked because
                // `code` is only a `CLError` inside `kCLErrorDomain` — the
                // same integer means something else in every other domain.
                let is_cl_error = &*domain == unsafe { kCLErrorDomain };
                if is_cl_error && code == CLError::Denied.0 {
                    // The only error that is a *state*, not a hiccup: the user
                    // said no, or Location Services is off system-wide. Every
                    // other code (`Network`, `LocationUnknown`) is transient
                    // and CoreLocation keeps trying on its own.
                    shared.set_permission(LocationPermission::Denied);
                    sync_updates(manager, shared);
                } else {
                    log::warn!(
                        "CoreLocation error {code} in {}: {}",
                        &*domain,
                        &*error.localizedDescription()
                    );
                }
            }
        }
    );

    impl Delegate {
        fn new(mtm: MainThreadMarker, shared: Rc<Shared>) -> Retained<Self> {
            let this = Self::alloc(mtm).set_ivars(shared);
            // SAFETY: `NSObject`'s designated initialiser, on a freshly
            // allocated instance whose ivars are already written.
            unsafe { objc2::msg_send![super(this), init] }
        }
    }

    /// A live `CLLocationManager` and the delegate it reports to.
    ///
    /// # The field order is load-bearing
    ///
    /// `-[CLLocationManager setDelegate:]` is a **weak** property: CoreLocation
    /// does not retain what it is given. A delegate whose only strong handle
    /// was a local in the constructor would be deallocated the moment that
    /// frame returned, and every callback would go to a zeroed weak reference —
    /// which presents as "the OS never calls us back", with no error anywhere.
    /// The `Retained` below is what keeps it alive, and it is declared after
    /// `manager` so it is dropped after it.
    pub struct OsLocationReader {
        manager: Retained<CLLocationManager>,
        /// Never read. The `Retained` *is* the point; see the note above.
        _delegate: Retained<Delegate>,
        shared: Rc<Shared>,
    }

    impl OsLocationProvider for OsLocationReader {
        /// Bring up CoreLocation without asking the user anything.
        ///
        /// Constructing a `CLLocationManager` and giving it a delegate does not
        /// prompt — only [`request`](Self::request) does — so this is safe to
        /// do at startup, and it has to be: the permission gate asks
        /// `location_permission()` on the first frame, and a provider that does
        /// not exist yet answers `Unavailable`, which the gate treats as
        /// terminal.
        ///
        /// `None` means this process cannot use CoreLocation at all, which is
        /// not the same as "the user said no".
        fn start(sink: OsLocationSink) -> Option<Self> {
            let OsLocationSink {
                fixes,
                wake,
                report,
            } = sink;
            let Some(mtm) = MainThreadMarker::new() else {
                // Nothing in this crate constructs a bridge off the main
                // thread, so this is a wiring bug rather than a condition.
                log::error!("the OS location provider was built off the main thread");
                return None;
            };

            // macOS only: on iOS the executable is always inside a .app, and a
            // false negative here would silently disable location on the one
            // platform where it cannot happen.
            #[cfg(target_os = "macos")]
            if NSBundle::mainBundle().bundleIdentifier().is_none() {
                // `-[CLLocationManager requestWhenInUseAuthorization]` needs a
                // bundle identifier to attribute the request to, and returns
                // `void` whether or not it got one. Reporting no provider at
                // all is the honest answer: the settings pane then says
                // location is unavailable instead of parking on a prompt that
                // will never appear.
                log::warn!(
                    "no bundle identifier: this is a bare executable, not a .app, and \
                     CoreLocation will not prompt for one. Build the bundle with \
                     `make -C macos` and run rustdar.app."
                );
                return None;
            }

            let shared = Rc::new(Shared {
                fixes,
                wake,
                report,
                permission: Cell::new(LocationPermission::Unknown),
                wants_updates: Cell::new(false),
                updating: Cell::new(false),
            });
            let delegate = Delegate::new(mtm, Rc::clone(&shared));

            // SAFETY: `CLLocationManager` is `AnyThread` — the binding carries
            // no `#[thread_kind = MainThreadOnly]`, so `new()` takes no marker
            // despite being `unsafe`. It is still constructed on the main
            // thread deliberately: that is what decides which run loop delivers
            // the callbacks, and `mtm` above proves which thread this is.
            let manager = unsafe { CLLocationManager::new() };

            // SAFETY: the delegate outlives the manager (both are fields of the
            // struct returned below, declared in that order), and
            // `ProtocolObject::from_ref` only erases the type. The two setters
            // take plain scalars.
            unsafe {
                manager.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
                manager.setDesiredAccuracy(kCLLocationAccuracyBest);
                manager.setDistanceFilter(DISTANCE_FILTER_M);
            }

            // Read the status synchronously as well as taking it from the
            // callback. Assigning a delegate schedules one
            // `locationManagerDidChangeAuthorization:` immediately, but it is
            // *scheduled* — on iOS the run loop it is scheduled on has not been
            // started yet — and `location_permission()` is asked before then.
            //
            // SAFETY: the instance property; see the callback for the
            // deployment-target note.
            let status = unsafe { manager.authorizationStatus() };
            let initial = permission_from_status(status.0);
            shared.permission.set(initial);
            // Reported unconditionally, not through `set_permission`: the
            // bridge's atomic starts at `Unknown`, and a returning user whose
            // status really is `NotDetermined` would otherwise see no report at
            // all — leaving the pane on "Checking…" until something else
            // changed. This is the initial report the contract asks for.
            (shared.report)(initial);
            log::info!("CoreLocation provider ready; authorization is {initial:?}");

            Some(Self {
                manager,
                _delegate: delegate,
                shared,
            })
        }

        /// Prompt if the user has never been asked, and start delivering.
        ///
        /// The `bool` is the hint `LocationBridge::request_location` documents,
        /// not a fact: `requestWhenInUseAuthorization` returns `void` and fails
        /// silently. `false` here means only that there was nothing to ask —
        /// the user has already refused — which is the one case this side can
        /// be sure of.
        fn request(&mut self) -> bool {
            self.shared.wants_updates.set(true);
            match self.shared.permission.get() {
                LocationPermission::Prompt | LocationPermission::Unknown => {
                    // SAFETY: no compile-time preconditions. The runtime one —
                    // an Info.plist usage string — is checked by the OS, which
                    // is why the macOS bundle ships both
                    // `NSLocationUsageDescription` and
                    // `NSLocationWhenInUseUsageDescription`.
                    unsafe { self.manager.requestWhenInUseAuthorization() };
                    log::info!("asked CoreLocation for when-in-use authorization");
                    true
                }
                LocationPermission::Granted => {
                    // Already authorised, so no further authorisation callback
                    // is coming to start us.
                    sync_updates(&self.manager, &self.shared);
                    true
                }
                LocationPermission::Denied | LocationPermission::Unavailable => false,
            }
        }

        /// Stop delivering. Does not, and cannot, give the permission back — and
        /// deliberately leaves the manager and its delegate alive, so a change
        /// made in System Settings while delivery is off still reaches
        /// `locationManagerDidChangeAuthorization:`.
        fn stop(&mut self) {
            self.shared.wants_updates.set(false);
            sync_updates(&self.manager, &self.shared);
        }

        /// Whether `startUpdatingLocation` is outstanding.
        ///
        /// Not "a fix arrived recently": see the module note on iOS
        /// backgrounding for the one case where those differ.
        fn active(&self) -> bool {
            self.shared.updating.get()
        }
    }

    impl Drop for OsLocationReader {
        /// Dropping the reader stops the stream, which is the contract every
        /// provider in this module shares with `SerialGpsReader`.
        ///
        /// Not left to deallocation. A `CLLocationManager` does stop updating
        /// when it is deallocated, but that is a statement about when the last
        /// `Retained` goes rather than about anything visible here — and
        /// CoreLocation is entitled to deliver one more callback in the
        /// meantime, to a delegate whose `Sender` has already been dropped. Two
        /// message sends buy an ordering that does not depend on refcount
        /// timing.
        fn drop(&mut self) {
            self.shared.wants_updates.set(false);
            sync_updates(&self.manager, &self.shared);
            // SAFETY: clearing a weak property. CoreLocation would zero it on
            // dealloc regardless; doing it here means no callback can be
            // delivered to a delegate whose `Shared` has already lost its
            // `Sender`.
            unsafe { self.manager.setDelegate(None) };
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub use corelocation::OsLocationReader;

// ── Tests for the decoding half ─────────────────────────────────────────
//
// These run on the host, which is a Linux one. That is the whole reason the
// decoding half does not name a CoreLocation type.

#[cfg(test)]
mod tests {
    use super::*;
    use rustdar_location::FixQuality;

    /// A fix with every component valid, for tests that vary one thing.
    fn valid() -> LocationComponents {
        LocationComponents {
            latitude: 35.25,
            longitude: -97.5,
            altitude_m: 380.0,
            horizontal_accuracy_m: 12.0,
            vertical_accuracy_m: 8.0,
            speed_mps: 3.5,
            course_deg: 271.0,
        }
    }

    #[test]
    fn a_status_nobody_has_answered_yet_is_the_one_state_that_may_prompt() {
        assert_eq!(permission_from_status(0), LocationPermission::Prompt);
    }

    #[test]
    fn a_restriction_imposed_on_the_user_still_reads_as_denied() {
        // Not `Unavailable`: the service exists and the app is listed in it.
        assert_eq!(permission_from_status(1), LocationPermission::Denied);
    }

    #[test]
    fn a_refusal_by_the_user_reads_as_denied() {
        assert_eq!(permission_from_status(2), LocationPermission::Denied);
    }

    #[test]
    fn always_and_when_in_use_both_collapse_to_granted() {
        assert_eq!(permission_from_status(3), LocationPermission::Granted);
        assert_eq!(permission_from_status(4), LocationPermission::Granted);
    }

    #[test]
    fn a_status_this_build_has_never_heard_of_is_unknown_rather_than_a_denial() {
        // `Unknown` does nothing and looks again; `Denied` would render a
        // permanent refusal the user never gave.
        for raw in [5, 6, -1, i32::MIN, i32::MAX] {
            assert_eq!(
                permission_from_status(raw),
                LocationPermission::Unknown,
                "status {raw} should not have been decoded"
            );
        }
    }

    #[test]
    fn a_fix_whose_horizontal_accuracy_is_negative_is_discarded_entirely() {
        // The negative accuracy means the *coordinate* is invalid, so passing
        // the position on with no accuracy would be handing the app a location
        // it was told not to believe.
        assert!(
            fix_from_components(LocationComponents {
                horizontal_accuracy_m: -1.0,
                ..valid()
            })
            .is_none()
        );
    }

    #[test]
    fn a_valid_report_keeps_its_horizontal_accuracy_as_the_confidence_radius() {
        let fix = fix_from_components(valid()).expect("every component was valid");
        assert_eq!(fix.accuracy_m, Some(12.0));
        assert_eq!(fix.point.lat, 35.25);
        assert_eq!(fix.point.lon, -97.5);
    }

    #[test]
    fn a_report_from_below_sea_level_keeps_its_negative_altitude() {
        // The defect this test exists for: `altitude` is not one of the four
        // sign-sentinelled components, and gating it on its own sign would
        // discard every valid reading below sea level.
        let fix = fix_from_components(LocationComponents {
            altitude_m: -86.0,
            ..valid()
        })
        .expect("a negative altitude is a reading, not a sentinel");
        assert_eq!(fix.altitude_m, Some(-86.0));
    }

    #[test]
    fn an_altitude_is_dropped_only_when_the_vertical_accuracy_says_it_is_invalid() {
        let fix = fix_from_components(LocationComponents {
            vertical_accuracy_m: -1.0,
            ..valid()
        })
        .expect("an invalid altitude does not invalidate the coordinate");
        assert_eq!(fix.altitude_m, None);
        assert_eq!(
            fix.accuracy_m,
            Some(12.0),
            "the horizontal reading was still good"
        );
    }

    #[test]
    fn a_negative_speed_is_a_sentinel_and_not_a_reading() {
        let fix = fix_from_components(LocationComponents {
            speed_mps: -1.0,
            ..valid()
        })
        .expect("speed does not invalidate the coordinate");
        assert_eq!(fix.speed_mps, None);
    }

    #[test]
    fn a_negative_course_is_a_sentinel_and_not_a_reading() {
        // Emitted whenever the device is stationary, so this is the common
        // case rather than an error one.
        let fix = fix_from_components(LocationComponents {
            course_deg: -1.0,
            ..valid()
        })
        .expect("course does not invalidate the coordinate");
        assert_eq!(fix.heading_deg, None);
    }

    #[test]
    fn the_sentinel_test_is_on_the_sign_and_not_on_the_value_minus_one() {
        // A device reporting -0.5 is reporting "invalid" just as much as one
        // reporting -1.0; an equality test would take it for a real reading.
        assert_eq!(valid_component(-0.5), None);
        assert_eq!(valid_component(-1.0), None);
        assert_eq!(valid_component(f64::MIN), None);
    }

    #[test]
    fn a_zero_reading_is_a_reading_and_not_a_sentinel() {
        // Kills a `<=` where the headers say `<`: a stationary device reports
        // speed 0, and due north is course 0.
        assert_eq!(valid_component(0.0), Some(0.0));
        let fix = fix_from_components(LocationComponents {
            speed_mps: 0.0,
            course_deg: 0.0,
            ..valid()
        })
        .expect("zeroes are valid readings");
        assert_eq!(fix.speed_mps, Some(0.0));
        assert_eq!(fix.heading_deg, Some(0.0));
    }

    #[test]
    fn every_fix_this_provider_emits_is_labelled_as_coming_from_the_device() {
        // Not `Gps`: CoreLocation fuses satellites, Wi-Fi and cells and never
        // says which one answered.
        let fix = fix_from_components(valid()).expect("every component was valid");
        assert_eq!(fix.fix_quality, FixQuality::Device);
        assert_eq!(
            fix.satellites, None,
            "CoreLocation reports no satellite count"
        );
        assert_eq!(fix.hdop, None, "CoreLocation reports no HDOP");
    }
}
