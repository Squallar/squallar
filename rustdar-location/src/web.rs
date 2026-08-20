//! The browser arm of the facade: the Geolocation and Permissions APIs standing
//! in for the serial GPS reader and for an OS permission service.
//!
//! Everything that decides what a browser's answer *means* is a plain function
//! tested on the host; everything that asks the browser is wasm32-only and
//! untestable here. A swapped latitude and longitude, or `Unavailable` where
//! `Prompt` belongs, is silently valid.

use crate::{Fix, LocationPermission};

/// Build a [`Fix`] from a browser `GeolocationCoordinates`. Separated from the
/// `web_sys` call so the mapping is testable on the host.
///
/// `heading` and `speed` stay `None` when the device is stationary rather than
/// defaulting to zero, which would rotate a heading-up map to a fabricated
/// bearing. `accuracy` is non-nullable in the browser's API and always carried.
pub fn fix_from_coords(
    latitude: f64,
    longitude: f64,
    accuracy_m: f64,
    altitude_m: Option<f64>,
    speed_mps: Option<f64>,
    heading_deg: Option<f64>,
) -> Fix {
    Fix {
        altitude_m,
        speed_mps,
        heading_deg,
        accuracy_m: Some(accuracy_m),
        ..Fix::from_device_position(latitude, longitude)
    }
}

/// What to believe on a browser with no `navigator.permissions`.
///
/// Safari shipped the Permissions API only in 16.4, so iOS Safari before that
/// has `navigator.geolocation` and no `navigator.permissions`, where the only
/// way to learn the permission state is to ask for a position. `Unknown` would
/// wait for an answer with no mechanism to arrive; `Unavailable` would claim the
/// browser cannot do what it can.
pub const WITHOUT_PERMISSIONS_API: LocationPermission = LocationPermission::Prompt;

/// Decode a `PermissionState` string from `navigator.permissions.query`. The
/// spec names exactly three states, so the fallback arm is for shapes that are
/// not a state at all (Chrome ≤ 43 called the property `status`) — an
/// unrecognised string is a browser that will never say more.
pub fn permission_from_state(state: &str) -> LocationPermission {
    match state {
        "granted" => LocationPermission::Granted,
        "denied" => LocationPermission::Denied,
        "prompt" => LocationPermission::Prompt,
        _ => WITHOUT_PERMISSIONS_API,
    }
}

/// Compose the page's raw facts into the state the gate and the settings pane
/// read. `queried` is whatever the Permissions API has produced so far.
///
/// The two availability terms outrank it and both are permanent: no
/// `navigator.geolocation`, and an insecure context — `watchPosition` is
/// specified to fail with `PERMISSION_DENIED` there whatever the user says, so
/// mapping that onto `Denied` would send them hunting for a switch that cannot
/// help.
pub fn browser_permission(
    has_geolocation: bool,
    is_secure_context: bool,
    queried: LocationPermission,
) -> LocationPermission {
    if !has_geolocation || !is_secure_context {
        return LocationPermission::Unavailable;
    }
    queried
}

// Everything below talks to `web_sys` and exists only on wasm32.

/// The app's wake, behind a slot the facade refills: `set_wake` arrives from the
/// app *after* [`WebBackend::new`] has started the permission query, so a wake
/// captured at construction would never be reached. `None` is a no-op.
#[cfg(target_arch = "wasm32")]
pub(crate) type SharedWake = std::rc::Rc<std::cell::RefCell<Option<crate::Wake>>>;

/// The cell every browser callback writes the permission state into.
#[cfg(target_arch = "wasm32")]
pub(crate) type PermissionCell = std::rc::Rc<std::cell::Cell<LocationPermission>>;

/// Ask for a frame without holding the `RefCell` borrow across the call.
#[cfg(target_arch = "wasm32")]
fn wake(waker: &SharedWake) {
    let wake = waker.borrow().clone();
    if let Some(wake) = wake {
        wake();
    }
}

/// `navigator.geolocation`, if this page can ever have a position at all. `None`
/// is [`browser_permission`]'s `has_geolocation = false`; the secure-context
/// term is a property of the *page* and is checked by the caller.
#[cfg(target_arch = "wasm32")]
pub(crate) fn geolocation() -> Option<web_sys::Geolocation> {
    web_sys::window().and_then(|w| w.navigator().geolocation().ok())
}

/// Whether the page is a secure context. See [`browser_permission`].
#[cfg(target_arch = "wasm32")]
pub(crate) fn is_secure_context() -> bool {
    web_sys::window().is_some_and(|w| w.is_secure_context())
}

/// A live `watchPosition`, cancelled by dropping it.
///
/// A *handle* rather than a `forget()`ten subscription because the settings pane
/// has a **Turn off** button and a no-op there leaves the browser's location
/// indicator lit. The [`Drop`] ordering is what makes owning the closures safe:
/// `clearWatch` removes the entry from the browser's watch list synchronously,
/// and Rust runs `Drop::drop` before it drops fields.
#[cfg(target_arch = "wasm32")]
pub(crate) struct LocationWatch {
    /// The object `watchPosition` was called on; `clearWatch` ids are scoped to it.
    geolocation: web_sys::Geolocation,
    id: i32,
    /// Held, not `forget()`ten.
    _on_success: wasm_bindgen::prelude::Closure<dyn FnMut(web_sys::Position)>,
    /// Held, not `forget()`ten.
    _on_error: wasm_bindgen::prelude::Closure<dyn FnMut(web_sys::PositionError)>,
}

#[cfg(target_arch = "wasm32")]
impl Drop for LocationWatch {
    fn drop(&mut self) {
        // Before the closures. See the type note.
        self.geolocation.clear_watch(self.id);
        log::debug!("geolocation watch {} cancelled", self.id);
    }
}

/// Start watching the browser's position, pushing every reading into `sender`.
///
/// **Not called at page load.** It is called from `WebBackend::request`, which
/// the gate reaches only from a state that licenses a prompt — on the web this
/// call *is* the prompt.
///
/// `watchPosition`, not `getCurrentPosition`: the fix channel is state
/// (`drain_latest` keeps only the newest value), so a stream is what it wants.
///
/// The error callback writes a refusal into the same cell the Permissions API
/// feeds — on the browsers [`WITHOUT_PERMISSIONS_API`] is for, the only denial
/// signal there is. `waker` exists because the success callback is invoked by
/// the browser while the loop sits on `ControlFlow::Wait`.
#[cfg(target_arch = "wasm32")]
pub(crate) fn watch_position(
    geolocation: &web_sys::Geolocation,
    sender: std::sync::mpsc::Sender<Fix>,
    permission: PermissionCell,
    waker: SharedWake,
) -> Option<LocationWatch> {
    use wasm_bindgen::JsCast;
    use wasm_bindgen::prelude::Closure;

    let fix_waker = waker.clone();
    let on_success =
        Closure::<dyn FnMut(web_sys::Position)>::new(move |position: web_sys::Position| {
            let coords = position.coords();
            let fix = fix_from_coords(
                coords.latitude(),
                coords.longitude(),
                coords.accuracy(),
                coords.altitude(),
                coords.speed(),
                coords.heading(),
            );
            // A closed receiver means the app is gone.
            if sender.send(fix).is_err() {
                log::debug!("GPS receiver dropped; geolocation updates have nowhere to go");
                return;
            }
            wake(&fix_waker);
        });

    let on_error =
        Closure::<dyn FnMut(web_sys::PositionError)>::new(move |error: web_sys::PositionError| {
            // Denial is the common case and is not an application error.
            log::info!(
                "geolocation unavailable (code {}): {}",
                error.code(),
                error.message()
            );
            // Only `PERMISSION_DENIED` is a statement about permission;
            // `POSITION_UNAVAILABLE` and `TIMEOUT` are not refusals.
            if error.code() == web_sys::PositionError::PERMISSION_DENIED {
                permission.set(LocationPermission::Denied);
                wake(&waker);
            }
        });

    let options = web_sys::PositionOptions::new();
    options.set_enable_high_accuracy(true);

    let id = match geolocation.watch_position_with_error_callback_and_options(
        on_success.as_ref().unchecked_ref(),
        Some(on_error.as_ref().unchecked_ref()),
        &options,
    ) {
        Ok(id) => id,
        Err(e) => {
            log::warn!("failed to start watching position: {e:?}");
            return None;
        }
    };

    Some(LocationWatch {
        geolocation: geolocation.clone(),
        id,
        _on_success: on_success,
        _on_error: on_error,
    })
}

/// A live subscription to `PermissionStatus.change`. The `PermissionStatus` is a
/// field because the event fires on *that object*, and one nothing references is
/// collectable — a collected one stops delivering `change`.
#[cfg(target_arch = "wasm32")]
pub(crate) struct PermissionWatch {
    status: web_sys::PermissionStatus,
    /// Held for the same reason [`LocationWatch`]'s are.
    _on_change: wasm_bindgen::prelude::Closure<dyn FnMut()>,
}

#[cfg(target_arch = "wasm32")]
impl Drop for PermissionWatch {
    fn drop(&mut self) {
        // Before the closure, exactly as in `LocationWatch`.
        self.status.set_onchange(None);
    }
}

/// Ask the Permissions API about geolocation, and keep listening.
///
/// Fills `permission` when the promise settles and again on every `change`.
/// Returns immediately: until it settles the cell keeps
/// [`LocationPermission::Unknown`], the state that makes the gate wait.
///
/// Worth doing even though `watchPosition` would answer too, because
/// `watchPosition` answers by prompting. The `change` subscription is the push
/// side of a revocation: the gate's poll goes quiet once a grant is in hand.
#[cfg(target_arch = "wasm32")]
pub(crate) fn query_permission(
    permission: PermissionCell,
    subscription: std::rc::Rc<std::cell::RefCell<Option<PermissionWatch>>>,
    waker: SharedWake,
) {
    use wasm_bindgen::JsCast;
    use wasm_bindgen::JsValue;
    use wasm_bindgen::prelude::Closure;

    let Some(permissions) = web_sys::window().and_then(|w| w.navigator().permissions().ok()) else {
        log::info!(
            "no navigator.permissions on this browser; treating location as \
             un-asked so the app can still offer it"
        );
        permission.set(WITHOUT_PERMISSIONS_API);
        return;
    };

    // `{ name: "geolocation" }`, built by hand so the crate needs no extra
    // `web_sys` dictionary feature for a two-field object.
    let descriptor = js_sys::Object::new();
    if js_sys::Reflect::set(
        &descriptor,
        &JsValue::from_str("name"),
        &JsValue::from_str("geolocation"),
    )
    .is_err()
    {
        permission.set(WITHOUT_PERMISSIONS_API);
        return;
    }

    let promise = match permissions.query(&descriptor) {
        Ok(promise) => promise,
        // A browser that rejects the geolocation descriptor cannot answer, which
        // is the same as not having the API.
        Err(e) => {
            log::info!("navigator.permissions.query rejected the descriptor: {e:?}");
            permission.set(WITHOUT_PERMISSIONS_API);
            return;
        }
    };

    wasm_bindgen_futures::spawn_local(async move {
        let settled = match wasm_bindgen_futures::JsFuture::from(promise).await {
            Ok(settled) => settled,
            Err(e) => {
                log::info!("navigator.permissions.query failed: {e:?}");
                permission.set(WITHOUT_PERMISSIONS_API);
                wake(&waker);
                return;
            }
        };
        let status: web_sys::PermissionStatus = settled.unchecked_into();
        adopt_state(&status, &permission, &waker);

        let on_change = Closure::<dyn FnMut()>::new({
            let status = status.clone();
            let permission = permission.clone();
            let waker = waker.clone();
            move || adopt_state(&status, &permission, &waker)
        });
        status.set_onchange(Some(on_change.as_ref().unchecked_ref()));
        *subscription.borrow_mut() = Some(PermissionWatch {
            status,
            _on_change: on_change,
        });
    });
}

/// Read `status.state` and put what it means in the cell, asking for the frame
/// that would show it. Read reflectively rather than through
/// `PermissionStatus::state()`, whose `web_sys` enum would move the decision out
/// of [`permission_from_state`], where it is testable on the host.
#[cfg(target_arch = "wasm32")]
fn adopt_state(
    status: &web_sys::PermissionStatus,
    permission: &PermissionCell,
    waker: &SharedWake,
) {
    use wasm_bindgen::JsValue;

    let state = js_sys::Reflect::get(status, &JsValue::from_str("state"))
        .ok()
        .and_then(|state| state.as_string());
    let next = state
        .as_deref()
        .map_or(WITHOUT_PERMISSIONS_API, permission_from_state);
    if permission.replace(next) == next {
        return;
    }
    log::info!("browser location permission is now {next:?}");
    wake(waker);
}

/// The browser arm: the permission cell, its `change` subscription, and the
/// live watch.
#[cfg(target_arch = "wasm32")]
pub struct WebBackend {
    /// `None` until the watch starts.
    fixes: Option<std::sync::mpsc::Receiver<Fix>>,
    /// Resolved once. `None` is the permanent
    /// [`Unavailable`](LocationPermission::Unavailable) half of
    /// [`browser_permission`].
    geolocation: Option<web_sys::Geolocation>,
    /// Read once — it cannot change for the life of a document.
    secure_context: bool,
    /// What `navigator.permissions.query` has produced so far. An `Rc<Cell<_>>`
    /// because three browser callbacks write it and none can hold a
    /// `&mut WebBackend`, while `permission` is a `&self` getter on the frame
    /// path, which a `Cell` serves without a lock.
    permission: PermissionCell,
    /// The `change` subscription, once the query has settled. Underscored
    /// because holding it *is* its job: the async task that fills it drops its
    /// own handle when it finishes.
    _permission_watch: std::rc::Rc<std::cell::RefCell<Option<PermissionWatch>>>,
    /// Dropping it is what `stop` does.
    watch: Option<LocationWatch>,
    /// See [`SharedWake`].
    waker: SharedWake,
}

#[cfg(target_arch = "wasm32")]
impl Default for WebBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(target_arch = "wasm32")]
impl WebBackend {
    pub fn new() -> Self {
        use std::cell::{Cell, RefCell};
        use std::rc::Rc;

        let permission: PermissionCell = Rc::new(Cell::new(LocationPermission::Unknown));
        let permission_watch = Rc::new(RefCell::new(None));
        let waker: SharedWake = Rc::new(RefCell::new(None));

        // Started here rather than lazily: the query asks what the browser
        // already knows *without prompting*, so a returning user gets a dot and
        // no dialog.
        query_permission(
            Rc::clone(&permission),
            Rc::clone(&permission_watch),
            Rc::clone(&waker),
        );

        Self {
            fixes: None,
            geolocation: geolocation(),
            secure_context: is_secure_context(),
            permission,
            _permission_watch: permission_watch,
            watch: None,
            waker,
        }
    }
}

#[cfg(target_arch = "wasm32")]
impl crate::LocationProvider for WebBackend {
    /// [`browser_permission`]'s composition. Cheap by the gate seam's contract:
    /// two `Copy` reads and a `Cell` load.
    fn permission(&self) -> LocationPermission {
        browser_permission(
            self.geolocation.is_some(),
            self.secure_context,
            self.permission.get(),
        )
    }

    /// Start delivering, prompting if the browser needs to. On the web the ask
    /// and the subscription are the same call, which is why the gate seam has
    /// one method. `true` means the watch was registered, not that the user
    /// agreed; `false` is reserved for the watch failing to register.
    fn request(&mut self) -> bool {
        let Some(geolocation) = self.geolocation.as_ref() else {
            return false;
        };
        // Idempotent: the gate calls this on every poll until `active` agrees,
        // and a second `watchPosition` would leak the first.
        if self.watch.is_some() {
            return true;
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        let Some(watch) = watch_position(
            geolocation,
            sender,
            std::rc::Rc::clone(&self.permission),
            std::rc::Rc::clone(&self.waker),
        ) else {
            return false;
        };
        self.watch = Some(watch);
        self.fixes = Some(receiver);
        true
    }

    /// Really stops, because [`LocationWatch`] really cancels.
    fn stop(&mut self) {
        // The receiver goes with the watch; leaving it would hand the app one
        // more reading that arrived just before the cancel.
        self.fixes = None;
        if self.watch.take().is_some() {
            log::info!("browser location updates stopped");
        }
    }

    fn active(&self) -> bool {
        self.watch.is_some()
    }

    fn poll_fix(&mut self) -> Option<Fix> {
        self.fixes.as_ref().and_then(crate::provider::drain_latest)
    }

    /// Fills the slot every browser callback wakes through. See [`SharedWake`].
    fn set_wake(&mut self, wake: crate::Wake) {
        *self.waker.borrow_mut() = Some(wake);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FixQuality;

    #[test]
    fn latitude_and_longitude_keep_their_places() {
        let fix = fix_from_coords(35.25, -97.5, 30.0, None, None, None);
        assert_eq!(fix.point.lat, 35.25);
        assert_eq!(fix.point.lon, -97.5);
    }

    /// `Device` and not `Gps`: a browser position is whatever the platform
    /// fused, and the settings pane reads this field straight off. Not
    /// `Fix::default()`, whose `FixQuality::None` would fail `can_relocate`.
    #[test]
    fn a_reading_counts_as_a_gps_fix() {
        let fix = fix_from_coords(35.25, -97.5, 30.0, None, None, None);
        assert!(
            matches!(fix.fix_quality, FixQuality::Device),
            "{:?}",
            fix.fix_quality
        );
        assert!(
            fix.fix_quality.can_relocate(),
            "a browser fix stopped being allowed to refine the opening site"
        );
    }

    /// `upgrade_provisional_site` reads the accuracy; dropping it lets a 25 km IP
    /// lookup spend the provisional site unconditionally.
    #[test]
    fn a_browser_fix_carries_the_accuracy_the_browser_reported() {
        let fix = fix_from_coords(35.25, -97.5, 1200.0, None, None, None);
        assert_eq!(fix.accuracy_m, Some(1200.0));
    }

    #[test]
    fn altitude_speed_and_heading_are_carried_through() {
        let fix = fix_from_coords(35.25, -97.5, 30.0, Some(390.0), Some(12.5), Some(275.0));
        assert_eq!(fix.altitude_m, Some(390.0));
        assert_eq!(fix.speed_mps, Some(12.5));
        assert_eq!(fix.heading_deg, Some(275.0));
    }

    #[test]
    fn absent_motion_stays_absent_rather_than_becoming_zero() {
        let fix = fix_from_coords(35.25, -97.5, 30.0, None, None, None);
        assert_eq!(fix.speed_mps, None);
        assert_eq!(fix.heading_deg, None);
        assert_eq!(fix.altitude_m, None);
    }

    #[test]
    fn fields_the_browser_does_not_supply_are_not_invented() {
        let fix = fix_from_coords(35.25, -97.5, 30.0, Some(390.0), Some(12.5), Some(275.0));
        assert_eq!(fix.satellites, None);
        assert_eq!(fix.hdop, None);
    }

    #[test]
    fn the_three_permission_states_map_to_their_counterparts() {
        assert_eq!(
            permission_from_state("granted"),
            LocationPermission::Granted
        );
        assert_eq!(permission_from_state("denied"), LocationPermission::Denied);
        assert_eq!(permission_from_state("prompt"), LocationPermission::Prompt);
    }

    /// A browser with `navigator.geolocation` and no `navigator.permissions`
    /// (Safari before 16.4) must be treated as *un-asked*: asking is the only way
    /// it can ever answer, and both wrong answers fail silently.
    #[test]
    fn a_browser_with_no_permissions_api_is_treated_as_never_asked() {
        assert_eq!(WITHOUT_PERMISSIONS_API, LocationPermission::Prompt);
        assert_eq!(
            browser_permission(true, true, WITHOUT_PERMISSIONS_API),
            LocationPermission::Prompt,
            "a browser that can be asked was reported as one that cannot"
        );
    }

    #[test]
    fn a_browser_with_no_geolocation_api_has_no_location_service() {
        assert_eq!(
            browser_permission(false, true, LocationPermission::Granted),
            LocationPermission::Unavailable,
            "a browser with no Geolocation API was reported as able to deliver"
        );
    }

    /// `watchPosition` on a plain-HTTP page fails with `PERMISSION_DENIED` no
    /// matter what the user says.
    #[test]
    fn an_insecure_page_reports_no_location_service_rather_than_a_denial() {
        assert_eq!(
            browser_permission(true, false, LocationPermission::Prompt),
            LocationPermission::Unavailable
        );
        assert_eq!(
            browser_permission(true, false, LocationPermission::Denied),
            LocationPermission::Unavailable,
            "an http:// page was reported as a permission the user could undo"
        );
    }

    /// The query is a Promise, and the window before it settles must read as
    /// `Unknown` — what stops the app prompting on first paint.
    #[test]
    fn a_permission_query_still_in_flight_reads_as_unknown() {
        assert_eq!(
            browser_permission(true, true, LocationPermission::Unknown),
            LocationPermission::Unknown,
            "a page whose permission query has not settled claimed to know the \
             answer, which is how a prompt lands on first paint"
        );
    }

    #[test]
    fn a_settled_permission_is_reported_as_the_browser_gave_it() {
        for state in [
            LocationPermission::Granted,
            LocationPermission::Denied,
            LocationPermission::Prompt,
        ] {
            assert_eq!(browser_permission(true, true, state), state);
        }
    }

    // The `web_sys` half is wasm-only and there is no wasm test runner here, so
    // the two properties below are pinned by source probes over this file's own
    // shipped half. Each has a failure mode that is silent on a device.

    /// Sliced at the test module: these probes name the calls they look for, so a
    /// whole-file search would find its own needles and pass for ever.
    fn shipped() -> &'static str {
        include_str!("web.rs")
            .split_once("#[cfg(test)]")
            .map(|(before, _)| before)
            .expect("the test module marker moved")
    }

    /// A `permission` that read the cell directly would report `Unknown` on a
    /// browser with no Permissions API and wait for ever, and every test here
    /// would still pass.
    #[test]
    fn the_arm_reports_the_permission_this_module_maps() {
        let query = shipped()
            .find("fn permission(&self)")
            .map(|i| &shipped()[i..])
            .expect("WebBackend::permission is gone");
        assert!(
            query.contains("browser_permission("),
            "the web arm answers the permission question without the \
             fallbacks, so a browser with no Permissions API is dead and \
             nothing here would notice"
        );
    }

    /// A version that kept `forget()` and let this be a no-op would leave the
    /// browser's location indicator lit after the user said stop.
    #[test]
    fn turning_location_off_really_cancels_the_watch() {
        let stop = shipped()
            .find("fn stop(&mut self)")
            .map(|i| &shipped()[i..])
            .expect("WebBackend::stop is gone");
        assert!(
            stop.contains("self.watch.take()"),
            "stop does not drop the watch, so the off switch does not \
             switch anything off"
        );

        assert!(
            !shipped().contains(&format!(".{}()", "forget")),
            "a browser callback was leaked with forget(), which is what made \
             cancelling the watch unsafe in the first place"
        );
        let drop_impl = shipped()
            .find("impl Drop for LocationWatch")
            .map(|i| &shipped()[i..])
            .expect("LocationWatch stopped cancelling itself");
        assert!(
            drop_impl.contains("clear_watch"),
            "dropping the watch frees its closures without telling the browser, \
             so the next callback calls into freed wasm memory"
        );
    }
}
