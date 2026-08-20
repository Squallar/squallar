//! CoreLocation, shared by macOS and iOS.
//!
//! The decoding half — `CLAuthorizationStatus` → [`LocationPermission`],
//! `CLLocation` → [`Fix`] — is pure arithmetic and compiles on every target, so
//! the `cargo test` that actually runs (a Linux one) can reach it.
//!
//! macOS constructs this after `EventLoop::new()` but may not be running from a
//! bundle, and CoreLocation refuses — silently — to prompt for an unbundled
//! executable, so macOS checks first. iOS constructs this before
//! `UIApplicationMain`, so there is no *running* run loop yet; hence the
//! synchronous initial status read.
//!
//! iOS has no `UIBackgroundModes: location` in `packaging/ios/Info.plist`, so
//! delivery stops when the app backgrounds with no callback to observe it.
//! `allowsBackgroundLocationUpdates` **throws** without the matching entitlement.

// Every CoreLocation binding in `objc2-core-location` 0.3.2 is an `unsafe fn`.
// Each block below has its own SAFETY note.
#![allow(
    unsafe_code,
    reason = "every objc2-core-location binding is an `unsafe fn`; the blocks \
              below are the whole surface and each has its own SAFETY note"
)]
// The decoding half is compiled on every target so its tests run on the host.
#![cfg_attr(
    not(any(target_os = "macos", target_os = "ios")),
    allow(
        dead_code,
        reason = "the decoding half compiles everywhere so `cargo test` can \
                  reach it; its only non-test caller is behind the Apple cfg"
    )
)]

use crate::{Fix, LocationPermission};

/// The `CLAuthorizationStatus` constants as raw `c_int`, so the mapping is
/// reachable from a host test. `live` asserts them against the real constants.
mod status {
    pub const NOT_DETERMINED: i32 = 0;
    pub const RESTRICTED: i32 = 1;
    pub const DENIED: i32 = 2;
    pub const AUTHORIZED_ALWAYS: i32 = 3;
    pub const AUTHORIZED_WHEN_IN_USE: i32 = 4;
}

/// Decode a raw `CLAuthorizationStatus` into the app's model.
///
/// `==` and not `match`: `CLAuthorizationStatus` is a `repr(transparent)`
/// newtype over `c_int` with associated constants, so a `match` can never be
/// exhaustive. Taking the `i32` also makes this callable without CoreLocation.
///
/// `Restricted` maps to `Denied`, not `Unavailable`, which would claim the
/// platform has no location service. An unrecognised value is `Unknown`.
fn permission_from_status(raw: i32) -> LocationPermission {
    if raw == status::NOT_DETERMINED {
        LocationPermission::Prompt
    } else if raw == status::AUTHORIZED_ALWAYS || raw == status::AUTHORIZED_WHEN_IN_USE {
        // Coarse/fine and always/when-in-use both collapse here.
        LocationPermission::Granted
    } else if raw == status::DENIED || raw == status::RESTRICTED {
        LocationPermission::Denied
    } else {
        LocationPermission::Unknown
    }
}

/// Everything a `CLLocation` carries that this app has a field for. A struct
/// rather than seven positional `f64`s: a transposed pair would type-check.
#[derive(Debug, Clone, Copy)]
struct LocationComponents {
    latitude: f64,
    longitude: f64,
    /// Metres relative to sea level. **Not** sign-sentinelled.
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
/// not `NaN` and not a flag. `< 0.0` and never `== -1.0`: the headers document
/// the rule by sign, and `-1.0` is only what the simulator happens to use.
fn valid_component(v: f64) -> Option<f64> {
    if v < 0.0 { None } else { Some(v) }
}

/// Decode a position report, or reject it.
///
/// `_LocationEssentials.framework`'s `CLLocationEssentials.h` documents exactly
/// four components as "negative if invalid": `horizontalAccuracy`,
/// `verticalAccuracy`, `course` and `speed`. **`altitude` is not one of them** —
/// it can be negative below sea level — so `verticalAccuracy` gates it. A
/// negative `horizontalAccuracy` means the *coordinate* is invalid, so it
/// rejects the whole report: `accuracy_m: None` reads downstream as passing.
fn fix_from_components(c: LocationComponents) -> Option<Fix> {
    let horizontal_accuracy_m = valid_component(c.horizontal_accuracy_m)?;
    Some(Fix {
        // Gated on `verticalAccuracy`; the altitude's own sign says nothing.
        altitude_m: valid_component(c.vertical_accuracy_m).map(|_| c.altitude_m),
        speed_mps: valid_component(c.speed_mps),
        heading_deg: valid_component(c.course_deg),
        accuracy_m: Some(horizontal_accuracy_m),
        // `CLLocation::timestamp` is when the fix was *measured*; the settings
        // pane times fix age from arrival instead.
        timestamp: None,
        // `Device`, not `Gps`: CoreLocation fuses GNSS, Wi-Fi and cell.
        ..Fix::from_device_position(c.latitude, c.longitude)
    })
}

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
    use crate::{Fix, LocationPermission};
    #[cfg(target_os = "macos")]
    use objc2_foundation::NSBundle;

    use super::super::{OsLocationProvider, OsLocationSink, RedrawWake, ReportPermission};

    /// A renumbering in a future `objc2-core-location` fails the build here
    /// rather than mapping "denied" onto "granted" at runtime.
    const _: () = {
        assert!(CLAuthorizationStatus::NotDetermined.0 == status::NOT_DETERMINED);
        assert!(CLAuthorizationStatus::Restricted.0 == status::RESTRICTED);
        assert!(CLAuthorizationStatus::Denied.0 == status::DENIED);
        assert!(CLAuthorizationStatus::AuthorizedAlways.0 == status::AUTHORIZED_ALWAYS);
        assert!(CLAuthorizationStatus::AuthorizedWhenInUse.0 == status::AUTHORIZED_WHEN_IN_USE);
    };

    /// How far the device must move before CoreLocation reports again. The
    /// default (`kCLDistanceFilterNone`) is a wake per second on a phone.
    const DISTANCE_FILTER_M: f64 = 10.0;

    /// State shared between the reader and its delegate. `Rc` and `Cell`, not
    /// `Arc` and atomics: the delegate class is `MainThreadOnly`,
    /// `CLLocationManager` is neither `Send` nor `Sync`, and CoreLocation
    /// delivers on the run loop the manager was created on.
    struct Shared {
        fixes: Sender<Fix>,
        /// Asks the event loop for a frame: winit parks in `ControlFlow::Wait`
        /// and a run-loop source firing produces no `RedrawRequested`.
        wake: RedrawWake,
        /// Publishes an authorisation change into the bridge's atomic.
        /// `locationManagerDidChangeAuthorization:` is a genuine push.
        report: ReportPermission,
        /// A local copy, because neither [`sync_updates`] nor `request` can read
        /// the bridge's atomic.
        permission: Cell<LocationPermission>,
        /// Survives a denial, so a later grant resumes on its own.
        wants_updates: Cell<bool>,
        updating: Cell<bool>,
    }

    impl Shared {
        fn set_permission(&self, permission: LocationPermission) {
            if self.permission.replace(permission) != permission {
                log::info!("CoreLocation authorization is now {permission:?}");
                (self.report)(permission);
                (self.wake)();
            }
        }
    }

    /// Start or stop delivery so that it matches what the app asked for and what
    /// the OS allows. One place, called from both the authorisation callback and
    /// [`OsLocationReader::request`]: a first run starts from the callback, a
    /// returning user is already `Granted` and gets no further callback.
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

    /// Pull the seven numbers out of a `CLLocation`. Reading only; the decisions
    /// are [`fix_from_components`]'s.
    fn components_of(location: &CLLocation) -> LocationComponents {
        // SAFETY: plain property reads on a live `CLLocation`, none of which has
        // a documented precondition or can fail.
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
        // which makes the `Cell`s in `Shared` sound without a lock.
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
                        // A negative `horizontalAccuracy`: the coordinate is
                        // meaningless. Common on a cold start, so `debug`.
                        log::debug!("CoreLocation reported a fix with an invalid coordinate");
                        continue;
                    };
                    if shared.fixes.send(fix).is_err() {
                        // The bridge dropped its receiver.
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
                // deployment floor is set in `.cargo/config.toml`; without it
                // this is a `doesNotRecognizeSelector:` at runtime.
                let status = unsafe { manager.authorizationStatus() };
                shared.set_permission(permission_from_status(status.0));
                // Delivery starts *here*: `requestWhenInUseAuthorization`
                // returns long before the user has looked at the dialog.
                sync_updates(manager, shared);
            }

            #[unsafe(method(locationManager:didFailWithError:))]
            fn did_fail_with_error(&self, manager: &CLLocationManager, error: &NSError) {
                let shared = self.ivars();
                let domain = error.domain();
                let code = error.code();
                // SAFETY: reading an `extern "C"` static `NSString` the
                // framework defines. The domain is checked because `code` is
                // only a `CLError` inside `kCLErrorDomain`.
                let is_cl_error = &*domain == unsafe { kCLErrorDomain };
                if is_cl_error && code == CLError::Denied.0 {
                    // The only error that is a *state*: the user said no, or
                    // Location Services is off system-wide. Every other code is
                    // transient and CoreLocation keeps trying on its own.
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

    /// A live `CLLocationManager` and the delegate it reports to. The field
    /// order is load-bearing: `setDelegate:` is a **weak** property, so a
    /// delegate whose only strong handle was a local would be deallocated when
    /// that frame returned and every callback would go to a zeroed weak
    /// reference — presenting as "the OS never calls us back".
    pub struct OsLocationReader {
        manager: Retained<CLLocationManager>,
        /// Never read; the `Retained` *is* the point.
        _delegate: Retained<Delegate>,
        shared: Rc<Shared>,
    }

    impl OsLocationProvider for OsLocationReader {
        /// Bring up CoreLocation without asking the user anything.
        ///
        /// Constructing a `CLLocationManager` and giving it a delegate does not
        /// prompt — only [`request`](Self::request) does — and it has to happen
        /// at startup: the gate asks `location_permission()` on the first frame
        /// and a missing provider answers `Unavailable`, which is terminal.
        fn start(sink: OsLocationSink) -> Option<Self> {
            let OsLocationSink {
                fixes,
                wake,
                report,
            } = sink;
            let Some(mtm) = MainThreadMarker::new() else {
                // A wiring bug: nothing constructs a bridge off the main thread.
                log::error!("the OS location provider was built off the main thread");
                return None;
            };

            // macOS only: on iOS the executable is always inside a .app.
            #[cfg(target_os = "macos")]
            if NSBundle::mainBundle().bundleIdentifier().is_none() {
                // `requestWhenInUseAuthorization` needs a bundle identifier and
                // returns `void` either way, so reporting no provider is honest.
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

            // SAFETY: `CLLocationManager` is `AnyThread`, so `new()` takes no
            // marker despite being `unsafe`. Constructed on the main thread
            // deliberately: that decides which run loop delivers the callbacks.
            let manager = unsafe { CLLocationManager::new() };

            // SAFETY: the delegate outlives the manager (both are fields of the
            // struct returned below, in that order), and
            // `ProtocolObject::from_ref` only erases the type.
            unsafe {
                manager.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
                manager.setDesiredAccuracy(kCLLocationAccuracyBest);
                manager.setDistanceFilter(DISTANCE_FILTER_M);
            }

            // Read synchronously as well as from the callback: assigning a
            // delegate only *schedules* one authorisation callback, and on iOS
            // the run loop it is scheduled on has not started yet.
            //
            // SAFETY: the instance property; see the callback's note.
            let status = unsafe { manager.authorizationStatus() };
            let initial = permission_from_status(status.0);
            shared.permission.set(initial);
            // Reported unconditionally, not through `set_permission`: the atomic
            // starts at `Unknown`, so a genuinely `NotDetermined` status would
            // otherwise produce no report at all.
            (shared.report)(initial);
            log::info!("CoreLocation provider ready; authorization is {initial:?}");

            Some(Self {
                manager,
                _delegate: delegate,
                shared,
            })
        }

        /// Prompt if the user has never been asked, and start delivering. The
        /// `bool` is a hint: `requestWhenInUseAuthorization` returns `void` and
        /// fails silently, so `false` means only that there was nothing to ask.
        fn request(&mut self) -> bool {
            self.shared.wants_updates.set(true);
            match self.shared.permission.get() {
                LocationPermission::Prompt | LocationPermission::Unknown => {
                    // SAFETY: no compile-time preconditions. The runtime one —
                    // an Info.plist usage string — is checked by the OS.
                    unsafe { self.manager.requestWhenInUseAuthorization() };
                    log::info!("asked CoreLocation for when-in-use authorization");
                    true
                }
                LocationPermission::Granted => {
                    // Already authorised, so no callback is coming to start us.
                    sync_updates(&self.manager, &self.shared);
                    true
                }
                LocationPermission::Denied | LocationPermission::Unavailable => false,
            }
        }

        /// Stop delivering. Cannot give the permission back, and leaves the
        /// manager and delegate alive so a change made in System Settings still
        /// reaches `locationManagerDidChangeAuthorization:`.
        fn stop(&mut self) {
            self.shared.wants_updates.set(false);
            sync_updates(&self.manager, &self.shared);
        }

        /// Whether `startUpdatingLocation` is outstanding — not "a fix arrived
        /// recently"; see the module note on iOS backgrounding.
        fn active(&self) -> bool {
            self.shared.updating.get()
        }
    }

    impl Drop for OsLocationReader {
        /// Dropping the reader stops the stream rather than leaving it to
        /// deallocation: CoreLocation is entitled to deliver one more callback
        /// before the last `Retained` goes, to a delegate whose `Sender` is gone.
        fn drop(&mut self) {
            self.shared.wants_updates.set(false);
            sync_updates(&self.manager, &self.shared);
            // SAFETY: clearing a weak property, so no callback can reach a
            // delegate whose `Shared` has already lost its `Sender`.
            unsafe { self.manager.setDelegate(None) };
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub use corelocation::OsLocationReader;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FixQuality;

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
        // `altitude` is not one of the four sign-sentinelled components.
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
        let fix = fix_from_components(LocationComponents {
            course_deg: -1.0,
            ..valid()
        })
        .expect("course does not invalidate the coordinate");
        assert_eq!(fix.heading_deg, None);
    }

    #[test]
    fn the_sentinel_test_is_on_the_sign_and_not_on_the_value_minus_one() {
        assert_eq!(valid_component(-0.5), None);
        assert_eq!(valid_component(-1.0), None);
        assert_eq!(valid_component(f64::MIN), None);
    }

    #[test]
    fn a_zero_reading_is_a_reading_and_not_a_sentinel() {
        // Kills a `<=` where the headers say `<`: speed 0 and course 0 are real.
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
        let fix = fix_from_components(valid()).expect("every component was valid");
        assert_eq!(fix.fix_quality, FixQuality::Device);
        assert_eq!(
            fix.satellites, None,
            "CoreLocation reports no satellite count"
        );
        assert_eq!(fix.hdop, None, "CoreLocation reports no HDOP");
    }
}
