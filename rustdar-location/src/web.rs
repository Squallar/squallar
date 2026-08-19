//! The browser arm of the facade: the Geolocation and Permissions APIs
//! standing in for the serial GPS reader and for an OS permission service.
//! Owned by rustdar-location since WO-RL-4 (seam ruling 6); `WebBackend`
//! (wasm-only, below) is
//! what rustdar-web's entry hands to the app inside a
//! [`LocationFacade`](crate::LocationFacade).
//!
//! # What is host-testable and what is wasm-only
//!
//! Everything below that decides *what a browser's answer means* is a plain
//! function over plain values, compiled and tested on the host. Everything that
//! *asks the browser* is `#[cfg(target_arch = "wasm32")]` and untestable here —
//! there is no wasm test runner in this repo. That split is not stylistic:
//! `fix_from_coords` was written this way because a swapped latitude and
//! longitude is silently valid, and the permission mapping has the same shape.
//! Mapping `Unavailable` where `Prompt` belongs is a browser on which location
//! never works again, and it throws nothing and logs nothing.

use crate::{Fix, LocationPermission};

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

// ── The browser half ────────────────────────────────────────────────────
//
// Everything below talks to `web_sys` and exists only on wasm32. The decisions
// it makes are all delegated to the plain functions above.

/// The app's wake, behind a slot the facade refills.
///
/// The facade's `set_wake` arrives from the app *after* [`WebBackend::new`]
/// has already started the permission query — so a wake captured at
/// construction would be an empty one that the app's later install never
/// reaches. `None` (before the install) is a no-op wake: nothing can be
/// looking at a frame that early.
#[cfg(target_arch = "wasm32")]
pub(crate) type SharedWake = std::rc::Rc<std::cell::RefCell<Option<crate::Wake>>>;

/// The cell every browser callback writes the permission state into.
#[cfg(target_arch = "wasm32")]
pub(crate) type PermissionCell = std::rc::Rc<std::cell::Cell<LocationPermission>>;

/// Ask for a frame, without holding the borrow across the call.
///
/// The wake ends in `Window::request_redraw`, which on this backend is
/// `requestAnimationFrame` and re-enters nothing here — but a `RefCell` borrow
/// held across a callback into the app is the kind of thing that becomes a panic
/// two refactors later, and the clone is a pointer copy.
#[cfg(target_arch = "wasm32")]
fn wake(waker: &SharedWake) {
    let wake = waker.borrow().clone();
    if let Some(wake) = wake {
        wake();
    }
}

/// `navigator.geolocation`, if this page can ever have a position at all.
///
/// `None` is [`browser_permission`]'s `has_geolocation = false`: a browser with
/// no Geolocation API. The secure-context term is checked by the caller against
/// the same window, because it is a property of the *page* rather than of the
/// object.
#[cfg(target_arch = "wasm32")]
pub(crate) fn geolocation() -> Option<web_sys::Geolocation> {
    web_sys::window().and_then(|w| w.navigator().geolocation().ok())
}

/// Whether the page is a secure context. See [`browser_permission`].
#[cfg(target_arch = "wasm32")]
pub(crate) fn is_secure_context() -> bool {
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
pub(crate) struct LocationWatch {
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
/// `WebBackend::request`, which the gate reaches only from a state
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
/// live watch — the location half of rustdar-web's old `WebPlatform`,
/// collapsed into the facade at WO-RL-4.
#[cfg(target_arch = "wasm32")]
pub struct WebBackend {
    /// Fixes pushed by the geolocation watch. `None` until the watch starts.
    fixes: Option<std::sync::mpsc::Receiver<Fix>>,
    /// `navigator.geolocation`, resolved once. `None` is the permanent
    /// [`Unavailable`](LocationPermission::Unavailable) half of
    /// [`browser_permission`].
    geolocation: Option<web_sys::Geolocation>,
    /// Whether the page is a secure context, read once — it cannot change for
    /// the life of a document.
    secure_context: bool,
    /// What `navigator.permissions.query` has produced so far.
    ///
    /// Behind an `Rc<Cell<_>>` because three different browser callbacks write
    /// it and none of them can hold a `&mut WebBackend`: the query's promise
    /// resolution, the `PermissionStatus` `change` event, and `watchPosition`'s
    /// error callback. `permission` is a `&self` getter on the frame path,
    /// which a `Cell` serves without a lock — the same constraint that makes
    /// Windows' arm an `AtomicI32`, one thread down.
    permission: PermissionCell,
    /// The `change` subscription, once the query has settled.
    ///
    /// Underscored because holding it *is* its job: the async task that fills
    /// it drops its own handle when it finishes, so this is the reference that
    /// keeps the `PermissionStatus` and its handler alive for the life of the
    /// page. See [`PermissionWatch`].
    _permission_watch: std::rc::Rc<std::cell::RefCell<Option<PermissionWatch>>>,
    /// The live `watchPosition`, or `None` when nothing is being delivered.
    /// Dropping it is what `stop` does.
    watch: Option<LocationWatch>,
    /// The app's wake, behind a slot the browser callbacks share. See
    /// [`SharedWake`].
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

        // Started here rather than lazily, and it is the whole reason the app
        // can be honest about location on the first frame: the query asks what
        // the browser already knows *without prompting*, so a user who granted
        // location on a previous visit gets a blue dot and no dialog, and one
        // who refused gets told so rather than being asked again.
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
    /// The composition is [`browser_permission`]'s; see it for why each
    /// fallback is the state it is.
    ///
    /// Cheap by the gate seam's contract: two reads of `Copy` fields and a
    /// `Cell` load. The Promise, the `change` event and the watch's error
    /// callback all resolve *into* that cell on their own schedule.
    fn permission(&self) -> LocationPermission {
        browser_permission(
            self.geolocation.is_some(),
            self.secure_context,
            self.permission.get(),
        )
    }

    /// Start delivering, prompting if the browser needs to.
    ///
    /// On the web the ask and the subscription are the same call — there is no
    /// way to prompt without also subscribing — which is why the gate seam has
    /// one method and not two.
    ///
    /// The `bool` is the honest one for this platform: `true` means the watch
    /// was registered, not that the user agreed. A refusal arrives later on the
    /// error callback, which writes `Denied` into the same cell
    /// [`permission`](Self::permission) reads. `false` is reserved for the
    /// watch failing to register at all, which leaves the gate free to retry
    /// within its bound.
    fn request(&mut self) -> bool {
        let Some(geolocation) = self.geolocation.as_ref() else {
            return false;
        };
        // Idempotent: the gate calls this from the `Granted` arm on every poll
        // until `active` agrees, and a second `watchPosition` would be a
        // second subscription with the first one leaked.
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
    ///
    /// The alternative — `forget()`ing the watch's closures and making this a
    /// documented no-op — was rejected once the settings pane grew a **Turn
    /// off** button: the gate calls this from that button, from the revocation
    /// arm and from the app's own disabled switch, and a no-op would leave the
    /// browser's location indicator lit and the dot moving after the user had
    /// said stop three different ways.
    fn stop(&mut self) {
        // The receiver goes with the watch. Leaving it would let `poll_fix`
        // hand the app one more reading that arrived between the last frame and
        // the cancel, which is a dot re-appearing after the user turned it off.
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

    /// Fills the slot every browser callback wakes through. Arrives from the
    /// app after [`WebBackend::new`] has already started the permission query
    /// — the reason [`SharedWake`] is a refillable slot.
    fn set_wake(&mut self, wake: crate::Wake) {
        *self.waker.borrow_mut() = Some(wake);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FixQuality;

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
    // The `web_sys` half above is `#[cfg(target_arch = "wasm32")]` and there
    // is no wasm test runner in this repo, so the two properties below are
    // pinned by source probes over this file's own shipped half. Same
    // technique the app side uses for claims about code only a frame can
    // reach. Each has a failure mode that is silent on a device. (The third
    // probe of this family — that the page's entry point never starts a watch
    // at boot — is ABOUT rustdar-web's entry source and stayed there when the
    // arm moved in at WO-RL-4.)

    /// This file's shipped half, sliced at the test module — and not only for
    /// tidiness: these probes name the calls they are looking for, so a
    /// whole-file search would find its own needles and pass for ever.
    fn shipped() -> &'static str {
        include_str!("web.rs")
            .split_once("#[cfg(test)]")
            .map(|(before, _)| before)
            .expect("the test module marker moved")
    }

    /// The three fallbacks above are only worth anything if the arm routes
    /// its answer through them. A `permission` that read the cell directly
    /// would report `Unknown` on a browser with no Permissions API and wait
    /// for ever, and the tests here would all still pass.
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

    /// The closure-ownership decision, made visible. The settings pane has a
    /// **Turn off** button wired through the gate's stop, and the gate calls
    /// the same verb on a revocation — so a version that kept `forget()` and
    /// let this be a no-op would leave the browser's location indicator lit
    /// and the dot moving after the user had said stop three different ways.
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
