//! The browser's [`PlatformBridge`], counterpart of
//! `rustdar_platform::platform::DesktopPlatform`. Most of the trait is
//! capabilities a tab does not have, so most of this file is honest `None`s.

use crate::geolocation;
use rustdar_frontend::platform::{PlatformBridge, RedrawWaker, drain_latest};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use winit::platform::web::WindowAttributesExtWebSys;

const DARK_SCHEME_QUERY: &str = "(prefers-color-scheme: dark)";

pub struct WebPlatform {
    /// The canvas the winit window is bound to. Held because
    /// `window_attributes` is called after construction, on `resumed`.
    canvas: web_sys::HtmlCanvasElement,
    /// Fixes pushed by the geolocation watch. `None` until the watch starts.
    gps_fix_receiver: Option<std::sync::mpsc::Receiver<rustdar_location::Fix>>,
    /// Last theme reported to the app, so `poll_theme` can answer "changed?"
    /// rather than "what is it?".
    last_theme: Option<bool>,
    /// `navigator.geolocation`, resolved once. `None` is the permanent
    /// [`Unavailable`](rustdar_location::LocationPermission::Unavailable) half of
    /// [`geolocation::browser_permission`].
    geolocation: Option<web_sys::Geolocation>,
    /// Whether the page is a secure context, read once — it cannot change for
    /// the life of a document.
    secure_context: bool,
    /// What `navigator.permissions.query` has produced so far.
    ///
    /// Behind an `Rc<Cell<_>>` because three different browser callbacks write
    /// it and none of them can hold a `&mut WebPlatform`: the query's promise
    /// resolution, the `PermissionStatus` `change` event, and `watchPosition`'s
    /// error callback. `location_permission` is a `&self` getter on the frame
    /// path, which a `Cell` serves without a lock — the same constraint that
    /// makes Windows' bridge an `AtomicI32`, one thread down.
    permission: geolocation::PermissionCell,
    /// The `change` subscription, once the query has settled.
    ///
    /// Underscored because holding it *is* its job: the async task that fills
    /// it drops its own handle when it finishes, so this is the reference that
    /// keeps the `PermissionStatus` and its handler alive for the life of the
    /// page. See [`geolocation::PermissionWatch`].
    _permission_watch: Rc<RefCell<Option<geolocation::PermissionWatch>>>,
    /// The live `watchPosition`, or `None` when nothing is being delivered.
    /// Dropping it is what `stop_location` does.
    watch: Option<geolocation::LocationWatch>,
    /// The app's waker, behind a slot the browser callbacks share. See
    /// [`geolocation::SharedWaker`].
    waker: geolocation::SharedWaker,
}

impl WebPlatform {
    pub fn new(canvas: web_sys::HtmlCanvasElement) -> Self {
        let permission: geolocation::PermissionCell =
            Rc::new(Cell::new(rustdar_location::LocationPermission::Unknown));
        let permission_watch = Rc::new(RefCell::new(None));
        let waker: geolocation::SharedWaker = Rc::new(RefCell::new(RedrawWaker::new()));

        // Started here rather than lazily, and it is the whole reason the app
        // can be honest about location on the first frame: the query asks what
        // the browser already knows *without prompting*, so a user who granted
        // location on a previous visit gets a blue dot and no dialog, and one
        // who refused gets told so rather than being asked again.
        geolocation::query_permission(
            Rc::clone(&permission),
            Rc::clone(&permission_watch),
            Rc::clone(&waker),
        );

        Self {
            canvas,
            gps_fix_receiver: None,
            last_theme: None,
            geolocation: geolocation::geolocation(),
            secure_context: geolocation::is_secure_context(),
            permission,
            _permission_watch: permission_watch,
            watch: None,
            waker,
        }
    }
}

impl PlatformBridge for WebPlatform {
    fn poll_theme(&mut self) -> Option<bool> {
        let current = self.detect_dark_theme();
        // Report only transitions. The app bumps every cached texture when this
        // returns `Some`, so answering unconditionally would re-render the whole
        // UI on every frame.
        if self.last_theme == Some(current) {
            return None;
        }
        self.last_theme = Some(current);
        Some(current)
    }

    fn detect_dark_theme(&self) -> bool {
        // A browser too old for `matchMedia`, or a failed query, is treated as
        // light — the same default the desktop bridge falls back to.
        web_sys::window()
            .and_then(|w| w.match_media(DARK_SCHEME_QUERY).ok().flatten())
            .is_some_and(|list| list.matches())
    }

    fn poll_gps_fix(&mut self) -> Option<rustdar_location::Fix> {
        self.gps_fix_receiver.as_ref().and_then(drain_latest)
    }

    // No `set_gps_fix_receiver`, deliberately, and its absence is load-bearing.
    // The entry point used to open the channel and start the watch at boot;
    // this bridge now owns both, and `request_location` replaces the receiver
    // every time it starts a watch. An override here would let an outside
    // producer swap the receiver out from under a live watch, which is a fix
    // stream with nowhere to go and no symptom.

    /// Taken so the geolocation and permission callbacks can ask for the frame
    /// their value is read on. Under `ControlFlow::Wait` a reading that lands
    /// with the page idle is otherwise invisible until something else draws.
    ///
    /// This arrives from `App::with_instance`, i.e. *after* `new` has already
    /// started the permission query — which is why the waker lives behind a
    /// slot the callbacks share rather than being captured by value.
    fn set_redraw_waker(&mut self, waker: RedrawWaker) {
        *self.waker.borrow_mut() = waker;
    }

    /// `DeviceOrientationEvent` needs a secure context and, on iOS, a separate
    /// gesture-gated permission. `HeadingSource` already falls back to the GPS
    /// bearing when no compass reports.
    fn poll_heading(&mut self) -> Option<f32> {
        None
    }

    /// A tab has no system bars to inset around.
    fn query_insets(&self) -> Option<(f32, f32, f32, f32)> {
        None
    }

    /// Nothing consumes a back gesture, so the app's own handling stands.
    fn handle_back(&self) -> bool {
        false
    }

    fn set_back_handler(&mut self, _handler: fn()) {}

    /// No filesystem, so no zone cache. The overlay layer treats the absence as
    /// "fetch every time" and the browser's HTTP cache sits underneath.
    fn set_zone_cache_dir(&mut self, _dir: std::path::PathBuf) {}

    fn zone_cache_dir(&self) -> Option<&std::path::Path> {
        None
    }

    /// Inert, not an oversight: `localStorage` is available from the first
    /// frame, which is why `kv` never returns `None` for "not told
    /// where yet" the way the Android bridge does.
    fn set_config_dir(&mut self, _dir: std::path::PathBuf) {}

    fn iana_timezone(&self) -> Option<String> {
        crate::geolocation::browser_timezone()
    }

    /// There is no process to exit; the event loop stopping is all there is.
    fn needs_process_exit(&self) -> bool {
        false
    }

    // ── Platform location service ───────────────────────────────────────
    //
    // The browser's is the one location service that was already wired in this
    // repo, and it was wired the wrong way round: `entry::start` called
    // `watchPosition` unconditionally at boot, so the page prompted on first
    // paint with no user gesture, and a refusal was an `info!` line the app
    // could neither see nor act on. The permission is asked about here, before
    // anything is asked *for*, and the watch is started only from
    // `request_location` — which the gate reaches only from a state that
    // licenses it.

    fn window_attributes(
        &self,
        attributes: winit::window::WindowAttributes,
    ) -> winit::window::WindowAttributes {
        // No `with_inner_size`, deliberately. winit's web backend reports
        // `inner_size()` from a cell written only by its ResizeObserver, so the
        // size is zero for the first frame or two either way (the zero-size
        // guard in `App::handle_redraw` is the actual fix) — and setting it
        // writes an inline pixel `width`/`height` that outranks the stylesheet's
        // `width: 100%`, pinning the canvas to its startup size forever.
        attributes
            .with_canvas(Some(self.canvas.clone()))
            // Otherwise the browser also handles events egui already consumed:
            // scrolling the map scrolls the page, dragging selects text.
            .with_prevent_default(true)
            // The canvas is already in the document; appending adds a second one
            // that nothing has sized.
            .with_append(false)
    }
}

impl rustdar_location::LocationBridge for WebPlatform {
    fn kv(&self) -> Option<Box<dyn rustdar_kv::KvStore>> {
        crate::kv::LocalStorageKvStore::new()
            .map(|store| Box::new(store) as Box<dyn rustdar_kv::KvStore>)
    }

    /// The composition is [`geolocation::browser_permission`]'s; see it for why
    /// each fallback is the state it is.
    ///
    /// Cheap by the trait's contract: two reads of `Copy` fields and a `Cell`
    /// load. The Promise, the `change` event and the watch's error callback all
    /// resolve *into* that cell on their own schedule.
    fn location_permission(&self) -> rustdar_location::LocationPermission {
        geolocation::browser_permission(
            self.geolocation.is_some(),
            self.secure_context,
            self.permission.get(),
        )
    }

    /// Start delivering, prompting if the browser needs to.
    ///
    /// On the web the ask and the subscription are the same call — there is no
    /// way to prompt without also subscribing — which is why the trait has one
    /// method and not two.
    ///
    /// The `bool` is the honest one for this platform: `true` means the watch
    /// was registered, not that the user agreed. A refusal arrives later on the
    /// error callback, which writes `Denied` into the same cell
    /// `location_permission` reads. `false` is reserved for the watch failing
    /// to register at all, which leaves the gate free to retry within its
    /// bound.
    fn request_location(&mut self) -> bool {
        let Some(geolocation) = self.geolocation.as_ref() else {
            return false;
        };
        // Idempotent: the gate calls this from the `Granted` arm on every poll
        // until `location_active` agrees, and a second `watchPosition` would be
        // a second subscription with the first one leaked.
        if self.watch.is_some() {
            return true;
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        let Some(watch) = geolocation::watch_position(
            geolocation,
            sender,
            Rc::clone(&self.permission),
            Rc::clone(&self.waker),
        ) else {
            return false;
        };
        self.watch = Some(watch);
        self.gps_fix_receiver = Some(receiver);
        true
    }

    /// Really stops, because [`geolocation::LocationWatch`] really cancels.
    ///
    /// The alternative — `forget()`ing the watch's closures and making this a
    /// documented no-op — was rejected once the settings pane grew a **Turn
    /// off** button: the gate calls this from that button, from the revocation
    /// arm and from the app's own disabled switch, and a no-op would leave the
    /// browser's location indicator lit and the dot moving after the user had
    /// said stop three different ways.
    fn stop_location(&mut self) {
        // The receiver goes with the watch. Leaving it would let `poll_gps_fix`
        // hand the app one more reading that arrived between the last frame and
        // the cancel, which is a dot re-appearing after the user turned it off.
        self.gps_fix_receiver = None;
        if self.watch.take().is_some() {
            log::info!("browser location updates stopped");
        }
    }

    fn location_active(&self) -> bool {
        self.watch.is_some()
    }
}
