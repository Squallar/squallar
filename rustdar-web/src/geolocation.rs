//! The browser's Geolocation and Permissions APIs standing in for the serial
//! GPS reader and for an OS permission service.
//!
//! Nothing downstream is serial-aware: a source pushes into a `Sender<Fix>`
//! and hands the `Receiver` to [`PlatformBridge::set_gps_fix_receiver`]. Android
//! already does this over JNI.
//!
//! # What is in here and what is in `bridge.rs`
//!
//! Everything below that decides *what a browser's answer means* is a plain
//! function over plain values, compiled and tested on the host. Everything that
//! *asks the browser* is `#[cfg(target_arch = "wasm32")]` and untestable here —
//! there is no wasm test runner in this repo. That split is not stylistic:
//! `fix_from_coords` was written this way because a swapped latitude and
//! longitude is silently valid, and the permission mapping has the same shape.
//! Mapping `Unavailable` where `Prompt` belongs is a browser on which location
//! never works again, and it throws nothing and logs nothing.
//!
//! [`PlatformBridge::set_gps_fix_receiver`]: rustdar_frontend::platform::PlatformBridge::set_gps_fix_receiver

use rustdar_location::{Fix, LocationPermission};

/// Build a [`Fix`] from a browser `GeolocationCoordinates`.
///
/// Separated from the `web_sys` call so the mapping is testable on the host: a
/// swapped latitude and longitude is silently valid, whereas the `web_sys` call
/// either works or throws.
///
/// Satellite count and HDOP stay `None` — the browser has neither. `heading` and
/// `speed` stay `None` when the device is stationary rather than defaulting to
/// zero, which would rotate a heading-up map to a fabricated bearing.
///
/// `accuracy` is not optional in the browser's API — `GeolocationCoordinates`
/// declares it non-nullable and every implementation fills it in — so it is
/// taken as an `f64` and always carried. It is the one field here with a reader
/// beyond the settings pane: `App::upgrade_provisional_site` uses it to refuse
/// a fix too coarse to improve on the timezone guess it would replace.
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
        // `from_device_position` is what sets `fix_quality`; a struct literal
        // would drift from whatever it decides a fused platform fix means.
        ..Fix::from_device_position(latitude, longitude)
    }
}

/// What to believe on a browser with no `navigator.permissions`.
///
/// **The one place in this file where a missing browser API must not be
/// [`Unavailable`] or [`Unknown`],** and the reasoning is worth spelling out
/// because both of the wrong answers are silent.
///
/// Safari shipped the Permissions API only in 16.4; iOS Safari and every
/// WebKit-shelled browser on iOS before that have `navigator.geolocation` and no
/// `navigator.permissions`. On such a browser the *only* way to learn the
/// permission state is to ask for a position and see what comes back, which is
/// exactly what [`Prompt`] licenses the gate to do — once, bounded, and
/// remembered.
///
/// * [`Unknown`] means "the platform has not answered *yet*", so the gate waits
///   forever for an answer that has no mechanism to arrive. Location is dead on
///   that browser and the settings pane sits on "checking…".
/// * [`Unavailable`] means "there is no location service here", which is false —
///   `navigator.geolocation` is right there and works — and the pane would tell
///   the user their browser cannot do something it can.
///
/// [`Prompt`]: LocationPermission::Prompt
/// [`Unknown`]: LocationPermission::Unknown
/// [`Unavailable`]: LocationPermission::Unavailable
pub const WITHOUT_PERMISSIONS_API: LocationPermission = LocationPermission::Prompt;

/// Decode a `PermissionState` string from `navigator.permissions.query`.
///
/// The spec names exactly three states and browsers do not invent others, so the
/// fallback arm is for the shapes that are not a state at all: a browser too old
/// for `PermissionStatus.state` (Chrome ≤ 43 called it `status`) reads as an
/// absent value, and so does anything the descriptor confused. Those land on
/// [`WITHOUT_PERMISSIONS_API`] for its reasons, not on [`Unknown`] — an
/// unrecognised string is a browser that will never say more, not one that has
/// not spoken yet.
///
/// [`Unknown`]: LocationPermission::Unknown
pub fn permission_from_state(state: &str) -> LocationPermission {
    match state {
        "granted" => LocationPermission::Granted,
        "denied" => LocationPermission::Denied,
        "prompt" => LocationPermission::Prompt,
        _ => WITHOUT_PERMISSIONS_API,
    }
}

/// Compose the page's raw facts into the state the gate and the settings pane
/// read.
///
/// `queried` is whatever the Permissions API has produced so far — [`Unknown`]
/// while its promise is in flight, [`WITHOUT_PERMISSIONS_API`] on a browser that
/// has none, and [`permission_from_state`]'s answer once it lands.
///
/// The two availability terms outrank it, and both are permanent:
///
/// * **No `navigator.geolocation`.** Nothing on this browser can produce a
///   position, so there is no privilege to grant.
/// * **An insecure context.** `getCurrentPosition` and `watchPosition` are
///   specified to fail with `PERMISSION_DENIED` on a page that is not a secure
///   context, whatever the user says — and *that* is the trap: without this
///   term the app would map a plain-HTTP page onto [`Denied`], point the user at
///   their browser's site settings, and send them looking for a switch that
///   would not have helped. `Unavailable` is the honest answer, and the fix is
///   to serve the page over HTTPS.
///
/// [`Unknown`]: LocationPermission::Unknown
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

/// The browser's IANA timezone, e.g. `"America/Denver"`.
///
/// `Intl.DateTimeFormat().resolvedOptions().timeZone` is the whole mechanism:
/// no permission, no prompt, no network, and an answer before the first frame.
/// It is the only "where is this user" signal a page gets for free, which is why
/// it is worth the coarse resolution — see [`location_hint`] for what that
/// resolution is and is not good for.
///
/// Reached through `js_sys::Reflect` rather than a typed `web_sys` binding
/// because `ResolvedDateTimeFormatOptions` is an anonymous object in the spec
/// and `web_sys` exposes it as a bare `Object`.
///
/// [`location_hint`]: rustdar_frontend::location_hint
#[cfg(target_arch = "wasm32")]
pub fn browser_timezone() -> Option<String> {
    use wasm_bindgen::JsValue;

    let resolved = js_sys::Intl::DateTimeFormat::default().resolved_options();
    let zone = js_sys::Reflect::get(&resolved, &JsValue::from_str("timeZone")).ok()?;
    // A browser too old for the `timeZone` key returns `undefined`, whose
    // `as_string` is `None` — the same miss as any other absent value.
    let zone = zone.as_string()?;
    // An empty string is not a zone, and would otherwise reach the anchor table
    // as a lookup that misses in a way that looks deliberate.
    (!zone.is_empty()).then_some(zone)
}

// ── The browser half ────────────────────────────────────────────────────
//
// Everything below talks to `web_sys` and exists only on wasm32. The decisions
// it makes are all delegated to the plain functions above.

/// The app's redraw waker, behind a slot the bridge refills.
///
/// `PlatformBridge::set_redraw_waker` is called from `App::with_instance`, i.e.
/// *after* `WebPlatform::new` has already started the permission query — so a
/// waker captured at construction would be a private empty one that the app's
/// later install never reaches. `RedrawWaker` is itself a shared slot, but
/// installing a *different* waker replaces the handle rather than filling it,
/// which is why this second indirection is needed and the type's own one is not
/// enough.
#[cfg(target_arch = "wasm32")]
pub type SharedWaker = std::rc::Rc<std::cell::RefCell<rustdar_frontend::platform::RedrawWaker>>;

/// The cell every browser callback writes the permission state into.
#[cfg(target_arch = "wasm32")]
pub type PermissionCell = std::rc::Rc<std::cell::Cell<LocationPermission>>;

/// Ask for a frame, without holding the borrow across the call.
///
/// `wake` ends in `Window::request_redraw`, which on this backend is
/// `requestAnimationFrame` and re-enters nothing here — but a `RefCell` borrow
/// held across a callback into the app is the kind of thing that becomes a panic
/// two refactors later, and the clone is a pointer copy.
#[cfg(target_arch = "wasm32")]
fn wake(waker: &SharedWaker) {
    let waker = waker.borrow().clone();
    waker.wake();
}

/// `navigator.geolocation`, if this page can ever have a position at all.
///
/// `None` is [`browser_permission`]'s `has_geolocation = false`: a browser with
/// no Geolocation API. The secure-context term is checked by the caller against
/// the same window, because it is a property of the *page* rather than of the
/// object.
#[cfg(target_arch = "wasm32")]
pub fn geolocation() -> Option<web_sys::Geolocation> {
    web_sys::window().and_then(|w| w.navigator().geolocation().ok())
}

/// Whether the page is a secure context. See [`browser_permission`].
#[cfg(target_arch = "wasm32")]
pub fn is_secure_context() -> bool {
    // A page with no `window` has already failed harder than this; `false` keeps
    // the answer on the conservative side of a state nothing can reach.
    web_sys::window().is_some_and(|w| w.is_secure_context())
}

/// A live `watchPosition`, cancelled by dropping it.
///
/// Same shape as `rustdar_nmea_serial`'s `SerialGpsReader` — started, then dropped to
/// stop — and it is a *handle* rather than a `forget()`ten subscription for a
/// reason the user can see: the settings pane
/// has a **Turn off** button, the gate calls `stop_location` from it and from
/// the revocation arm, and a no-op there is the app disagreeing with its own
/// control while the browser's location indicator stays lit.
///
/// # Why the closures are fields
///
/// The three available shapes are not equally good and the middle one is a use
/// after free:
///
/// 1. `forget()` the closures and never cancel. Safe, leaks two closures per
///    page, and makes `stop_location` a lie.
/// 2. `forget()` the closures and call `clearWatch` anyway. This is the worst of
///    the three — it stops the callbacks *and* keeps their memory, so it has the
///    leak of (1) with none of its honesty.
/// 3. Own them, and cancel the watch before dropping them. What this is.
///
/// The ordering in [`Drop`] is what makes (3) safe rather than a dangling
/// callback: `clearWatch` removes the entry from the browser's watch list
/// synchronously, so once it has returned no further call into the closures is
/// possible, and only then do the fields drop. Rust runs `Drop::drop` before it
/// drops fields, so the order is structural rather than a convention someone has
/// to remember.
#[cfg(target_arch = "wasm32")]
pub struct LocationWatch {
    /// The same object `watchPosition` was called on. `clearWatch` ids are
    /// scoped to it, and holding it is what lets [`Drop`] cancel without going
    /// back through `window()`.
    geolocation: web_sys::Geolocation,
    id: i32,
    /// Held, not `forget()`ten. See the type note.
    _on_success: wasm_bindgen::prelude::Closure<dyn FnMut(web_sys::Position)>,
    /// Held, not `forget()`ten. See the type note.
    _on_error: wasm_bindgen::prelude::Closure<dyn FnMut(web_sys::PositionError)>,
}

#[cfg(target_arch = "wasm32")]
impl Drop for LocationWatch {
    fn drop(&mut self) {
        // Before the closures. See the type note: this is the whole safety
        // argument for owning them.
        self.geolocation.clear_watch(self.id);
        log::debug!("geolocation watch {} cancelled", self.id);
    }
}

/// Start watching the browser's position, pushing every reading into `sender`.
///
/// **Not called at page load.** It is called from
/// `WebPlatform::request_location`, which the gate reaches only from a state
/// that licenses a prompt — because on the web this call *is* the prompt, and
/// prompting on first paint with no user gesture is the audited defect this
/// whole path exists to fix. A page that has never been granted location now
/// paints, settles, and only then asks.
///
/// `watchPosition`, not `getCurrentPosition`: the fix channel is state —
/// `drain_latest` keeps only the newest value — so a stream is what it expects.
///
/// # `permission`
///
/// The error callback writes a refusal back into the same cell the Permissions
/// API feeds. On a browser that has `navigator.permissions` this is redundant
/// with the `change` event; on the browsers [`WITHOUT_PERMISSIONS_API`] is for
/// it is the *only* denial signal that exists, and without it the settings pane
/// would keep offering "Use my location" to a user the browser has already
/// refused.
///
/// # `waker`
///
/// The success callback is invoked by the browser, not by the event loop, and
/// the loop is on `ControlFlow::Wait`. `App::poll_platform_state` drains this
/// channel only while rendering, so a reading that lands with the page idle
/// waits for the next pointer move or resize.
///
/// `request_redraw` on the web backend is `requestAnimationFrame`, so this is
/// one frame per reading, not a poll.
#[cfg(target_arch = "wasm32")]
pub fn watch_position(
    geolocation: &web_sys::Geolocation,
    sender: std::sync::mpsc::Sender<Fix>,
    permission: PermissionCell,
    waker: SharedWaker,
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
            // A closed receiver means the app is gone; the watch dies with the page.
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
            // Only `PERMISSION_DENIED` is a statement about permission.
            // `POSITION_UNAVAILABLE` and `TIMEOUT` are a device that cannot see
            // anything right now, which is not a refusal and must not be
            // rendered as one.
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

/// A live subscription to `PermissionStatus.change`.
///
/// # Why the `PermissionStatus` is a field
///
/// It is not decoration. The event fires on *that object*, and the object is
/// only reachable from JS for as long as something holds it — a `PermissionStatus`
/// nothing references is collectable, and a collected one stops delivering
/// `change`. Holding it in the wasm heap is what keeps the subscription alive,
/// and it is also what [`Drop`] needs in order to take the handler off again.
#[cfg(target_arch = "wasm32")]
pub struct PermissionWatch {
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
/// Fills `permission` when the promise settles and again on every `change`, and
/// parks a [`PermissionWatch`] in `subscription` so the second half keeps
/// working. Returns immediately: the query is a Promise, and until it settles
/// the cell keeps whatever it started with — [`LocationPermission::Unknown`],
/// which is the state that makes the gate *wait* rather than ask.
///
/// # Why this is worth doing at all when `watchPosition` would answer too
///
/// Because `watchPosition` answers by prompting. This is the only way to learn
/// that the user has already said yes (start delivering, no dialog) or already
/// said no (say so, offer no button) without putting a dialog in front of
/// somebody who has settled the question.
///
/// # Why the `change` subscription is not an optional extra
///
/// It is the push side of a revocation. The gate's poll goes quiet once a grant
/// is in hand and delivering — that state is terminal by design — so a
/// permission the user revokes in browser site settings while the tab is
/// foreground and the settings window closed is noticed by *nothing else*. One
/// event listener closes that gap for the whole platform.
#[cfg(target_arch = "wasm32")]
pub fn query_permission(
    permission: PermissionCell,
    subscription: std::rc::Rc<std::cell::RefCell<Option<PermissionWatch>>>,
    waker: SharedWaker,
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

    // `{ name: "geolocation" }`. Built by hand rather than through
    // `web_sys::PermissionDescriptor` so the crate needs no extra dictionary
    // feature for a two-field object.
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
        // A `permissions` object that rejects the geolocation descriptor is a
        // browser that cannot answer, which is the same situation as not having
        // the API at all.
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
/// that would show it.
///
/// Read reflectively rather than through `PermissionStatus::state()`, which
/// returns a `web_sys` enum: going through the string is what lets the whole
/// decision live in [`permission_from_state`], where it is testable on the host.
/// The same reasoning as `browser_timezone` above, and the same `js_sys::Reflect`
/// route.
#[cfg(target_arch = "wasm32")]
fn adopt_state(
    status: &web_sys::PermissionStatus,
    permission: &PermissionCell,
    waker: &SharedWaker,
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

#[cfg(test)]
mod tests {
    use super::*;
    use rustdar_location::FixQuality;

    /// Nothing downstream would notice a transposition: both are plain `f64` and
    /// a swapped pair is a valid location, just the wrong one.
    #[test]
    fn latitude_and_longitude_keep_their_places() {
        let fix = fix_from_coords(35.25, -97.5, 30.0, None, None, None);
        assert_eq!(fix.point.lat, 35.25);
        assert_eq!(fix.point.lon, -97.5);
    }

    /// Not `Fix::default()`, whose quality is `FixQuality::None` — the "no
    /// fix yet" state, whose coordinates mean nothing. The map does not read
    /// the quality at all (`ui_map.rs` draws the dot from latitude and
    /// longitude alone), so what a defaulted quality would break is the
    /// *site* upgrade: `FixQuality::can_relocate` refuses it, and a browser
    /// reading would silently stop refining the opening site.
    ///
    /// `Device` and not `Gps`, which is what this used to launder every reading
    /// into. A browser position is whatever the platform fused — satellites on a
    /// phone, Wi-Fi indoors, an IP lookup on a desktop — and the label the
    /// settings pane shows is read straight off this field, so `Gps` was a claim
    /// the page cannot support. Both variants relocate, so the behaviour that
    /// changed is what the user is told.
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

    /// The browser is the one source in this app that reports an accuracy, and
    /// `upgrade_provisional_site` is built to read it. Dropping it on the floor
    /// — which this did until the permission work — means every browser fix,
    /// including a 25 km IP lookup, spends the provisional site unconditionally.
    #[test]
    fn a_browser_fix_carries_the_accuracy_the_browser_reported() {
        let fix = fix_from_coords(35.25, -97.5, 1200.0, None, None, None);
        assert_eq!(fix.accuracy_m, Some(1200.0));
    }

    /// The optional fields are carried through rather than dropped.
    #[test]
    fn altitude_speed_and_heading_are_carried_through() {
        let fix = fix_from_coords(35.25, -97.5, 30.0, Some(390.0), Some(12.5), Some(275.0));
        assert_eq!(fix.altitude_m, Some(390.0));
        assert_eq!(fix.speed_mps, Some(12.5));
        assert_eq!(fix.heading_deg, Some(275.0));
    }

    /// Substituting zero for a stationary device's absent heading is
    /// indistinguishable from "moving due north" and rotates a heading-up map.
    #[test]
    fn absent_motion_stays_absent_rather_than_becoming_zero() {
        let fix = fix_from_coords(35.25, -97.5, 30.0, None, None, None);
        assert_eq!(fix.speed_mps, None);
        assert_eq!(fix.heading_deg, None);
        assert_eq!(fix.altitude_m, None);
    }

    /// The browser supplies neither, and the UI shows them verbatim.
    #[test]
    fn fields_the_browser_does_not_supply_are_not_invented() {
        let fix = fix_from_coords(35.25, -97.5, 30.0, Some(390.0), Some(12.5), Some(275.0));
        assert_eq!(fix.satellites, None);
        assert_eq!(fix.hdop, None);
    }

    // ── The permission mapping ──────────────────────────────────────────

    /// The three states the spec names, which are the whole of the happy path.
    #[test]
    fn the_three_permission_states_map_to_their_counterparts() {
        assert_eq!(
            permission_from_state("granted"),
            LocationPermission::Granted
        );
        assert_eq!(permission_from_state("denied"), LocationPermission::Denied);
        assert_eq!(permission_from_state("prompt"), LocationPermission::Prompt);
    }

    /// Fallback one. A browser with `navigator.geolocation` and no
    /// `navigator.permissions` — Safari before 16.4, and every browser on iOS
    /// that shipped against it — must be treated as *un-asked*, because asking
    /// is the only way that browser can ever answer.
    ///
    /// `Unknown` would leave the gate waiting for a resolution that has no
    /// mechanism to arrive, and `Unavailable` would tell the user their browser
    /// cannot do something it does perfectly well. Both fail silently, which is
    /// why this is pinned rather than left to the reader of the constant.
    #[test]
    fn a_browser_with_no_permissions_api_is_treated_as_never_asked() {
        assert_eq!(WITHOUT_PERMISSIONS_API, LocationPermission::Prompt);
        assert_eq!(
            browser_permission(true, true, WITHOUT_PERMISSIONS_API),
            LocationPermission::Prompt,
            "a browser that can be asked was reported as one that cannot"
        );
    }

    /// Fallback two. No Geolocation API at all is the one genuinely permanent
    /// answer, and the settings pane says so instead of offering a button.
    #[test]
    fn a_browser_with_no_geolocation_api_has_no_location_service() {
        assert_eq!(
            browser_permission(false, true, LocationPermission::Granted),
            LocationPermission::Unavailable,
            "a browser with no Geolocation API was reported as able to deliver"
        );
    }

    /// The other half of fallback two, and the one that would have been read as
    /// a user's decision. `watchPosition` on a plain-HTTP page fails with
    /// `PERMISSION_DENIED` no matter what the user says, so mapping the page
    /// through the permission state would point them at browser site settings
    /// for something only HTTPS fixes.
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

    /// Fallback three. The query is a Promise and there is a window — the first
    /// frames of every page load — in which nothing is known. That window must
    /// read as `Unknown`, which is the one state that makes the gate do nothing
    /// at all: it is what stops the app prompting on first paint.
    #[test]
    fn a_permission_query_still_in_flight_reads_as_unknown() {
        assert_eq!(
            browser_permission(true, true, LocationPermission::Unknown),
            LocationPermission::Unknown,
            "a page whose permission query has not settled claimed to know the \
             answer, which is how a prompt lands on first paint"
        );
    }

    /// A settled query outranks nothing and is simply carried through — the
    /// availability terms above are the only things allowed to overwrite it.
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

    // ── What the browser half does with all of that ─────────────────────
    //
    // `entry.rs` and `bridge.rs` are `#[cfg(target_arch = "wasm32")]` and there
    // is no wasm test runner in this repo, so the three properties below are
    // pinned by source probes. Same technique the frontend uses for claims about
    // code only a frame can reach (`app_chunks.rs`), and the same one this
    // crate's `tests/pwa_assets.rs` already uses to read `src/worker_port.rs`.
    // Each of these three has a failure mode that is silent on a device.

    /// **The audited defect, pinned at the one place it was written.**
    ///
    /// `entry::start` used to open a channel and call the geolocation watch on
    /// it before the first frame, so the browser's permission dialog appeared on
    /// first paint, with no user gesture, before the page had shown the user
    /// anything. `watchPosition` *is* the prompt on this platform: there is no
    /// way to start a watch quietly, which is why the only defence is not
    /// calling it — and why the assertion is about the entry point's source
    /// rather than about a flag somebody could set.
    ///
    /// The prompt now happens from `WebPlatform::request_location`, which
    /// `rustdar_location::LocationGate` reaches only from a state that
    /// licenses one.
    #[test]
    fn nothing_asks_the_browser_for_a_position_at_page_load() {
        let entry = include_str!("entry.rs");
        let start = entry
            .find("pub fn start()")
            .map(|i| &entry[i..])
            .expect("entry::start is gone");
        for asked in ["watch_position", "start_watch", "request_location"] {
            assert!(
                !start.contains(asked),
                "the browser entry point calls {asked} at boot, so the page \
                 prompts for location on first paint with no user gesture"
            );
        }
    }

    /// The three fallbacks above are only worth anything if the bridge routes
    /// its answer through them. A `location_permission` that read the cell
    /// directly would report `Unknown` on a browser with no Permissions API and
    /// wait for ever, and the tests here would all still pass.
    #[test]
    fn the_bridge_reports_the_permission_this_module_maps() {
        let bridge = include_str!("bridge.rs");
        let query = bridge
            .find("fn location_permission(")
            .map(|i| &bridge[i..])
            .expect("WebPlatform::location_permission is gone");
        assert!(
            query.contains("geolocation::browser_permission("),
            "the web bridge answers the permission question without the \
             fallbacks, so a browser with no Permissions API is dead and \
             nothing here would notice"
        );
    }

    /// The closure-ownership decision, made visible. The settings pane has a
    /// **Turn off** button wired through `stop_location`, and the gate calls the
    /// same method on a revocation — so a version that kept `forget()` and let
    /// this be a no-op would leave the browser's location indicator lit and the
    /// dot moving after the user had said stop three different ways.
    #[test]
    fn turning_location_off_really_cancels_the_watch() {
        let bridge = include_str!("bridge.rs");
        let stop = bridge
            .find("fn stop_location(")
            .map(|i| &bridge[i..])
            .expect("WebPlatform::stop_location is gone");
        assert!(
            stop.contains("self.watch.take()"),
            "stop_location does not drop the watch, so the off switch does not \
             switch anything off"
        );

        // Sliced at the test module, and not only for tidiness: this probe
        // names the call it is looking for, so a whole-file search would find
        // its own needle and pass for ever.
        let source = include_str!("geolocation.rs");
        let shipped = source
            .split_once("#[cfg(test)]")
            .map(|(before, _)| before)
            .expect("the test module marker moved");
        assert!(
            !shipped.contains(&format!(".{}()", "forget")),
            "a browser callback was leaked with forget(), which is what made \
             cancelling the watch unsafe in the first place"
        );
        let drop_impl = shipped
            .find("impl Drop for LocationWatch")
            .map(|i| &shipped[i..])
            .expect("LocationWatch stopped cancelling itself");
        assert!(
            drop_impl.contains("clear_watch"),
            "dropping the watch frees its closures without telling the browser, \
             so the next callback calls into freed wasm memory"
        );
    }
}
