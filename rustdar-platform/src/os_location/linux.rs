//! `org.freedesktop.portal.Location`, over `ashpd`, and nothing else.
//!
//! # Why the portal and not GeoClue directly
//!
//! This provider used to speak to `org.freedesktop.GeoClue2` on the system bus.
//! It reached a position that way, and it was still the wrong call:
//!
//! * **The portal's `disable-location` is a user preference, and going around
//!   it is going around the user.** `org.freedesktop.impl.portal.Lockdown`
//!   carries a `disable-location` property that a desktop binds to its own
//!   "location services" switch. A client that talks to geoclue directly never
//!   reads it, so it answers a question the user has already answered — and
//!   answers it the other way. Honouring that switch is the point of this file,
//!   *including* on the machines where it means location does not work.
//! * **Inside a Flatpak there is no system bus to talk to.** The direct path
//!   caps packaging at raw binaries for ever; the portal is the interface a
//!   sandbox is expected to have.
//! * **The portal is the sanctioned interface.** GeoClue's own D-Bus API is
//!   what the portal is built on top of, not what applications are meant to
//!   call.
//!
//! **There is no fallback.** A portal that refuses is reported as a refusal and
//! a portal that is absent is reported as absent; neither reaches for geoclue
//! behind the portal's back, because doing so is the defect this file exists to
//! remove.
//!
//! # Why this is a thread and not a few calls
//!
//! `ashpd` has no blocking API — every call is a future — and two of them can
//! take arbitrarily long:
//!
//! * `Start()` is a portal *request*: on a sandboxed install the portal puts an
//!   access dialog up and the response signal does not arrive until a human
//!   answers it. On the frame thread that is an unbounded UI freeze.
//! * The session then lives as long as the user leaves it on, delivering
//!   `LocationUpdated` signals.
//!
//! So the whole session runs on a thread of its own, and that thread drives the
//! futures with a bare [`futures_lite::future::block_on`]. No executor is set
//! up and none is needed: `async-io` is already in this graph — `ashpd`'s
//! `async-std` feature selects `zbus/async-io` — and its reactor starts itself
//! on first use.
//!
//! # What a fix from this actually is
//!
//! Measured on the development machine, through the portal: **25 km** accuracy,
//! described by geoclue as `GeoIP (ichnaea)`. That is an IP lookup, not a GPS
//! one. Every fix therefore leaves here as
//! [`FixQuality::Device`](rustdar_gps::FixQuality::Device) carrying its
//! [`accuracy_m`](GpsFix::accuracy_m): the site-upgrade gate reads that field,
//! and a 25 km circle is comfortably good enough to pick between WSR-88D sites
//! ~200 km apart while being useless for anything finer.
//!
//! # Network egress
//!
//! The portal does not find positions itself — it proxies to the same geoclue
//! daemon this file used to call. So the disclosure is unchanged: if geoclue
//! resolves the position by **Wi-Fi** rather than by IP, it POSTs the BSSIDs of
//! the access points it can see to `api.beacondb.net`. That is geoclue's own
//! request, made under its own configuration — rustdar neither makes nor can
//! suppress it — but it happens because this app asked, so it is disclosed
//! here, in `packaging/linux/README.md` and in the settings pane.
//!
//! # The refusal users will actually hit
//!
//! `xdg-desktop-portal-gtk` implements `Lockdown` by reading the GSettings key
//! `org.gnome.system.location enabled`, which **defaults to `false`** and has a
//! UI only on GNOME. On a stock KDE machine with that backend installed — a
//! very ordinary Arch install — every session is refused before it starts, with
//! `org.freedesktop.portal.Error.NotAllowed: Location services disabled`. That
//! is [`Denied`](LocationPermission::Denied), not
//! [`Unavailable`](LocationPermission::Unavailable), and [`explain`] names the
//! key that turns it on. See `packaging/linux/README.md`.

use futures_channel::oneshot;
use futures_lite::{Stream, StreamExt as _};

use ashpd::desktop::ResponseError;
use ashpd::desktop::location::{Accuracy, Location, LocationProxy};

/// `zbus`, reached through `ashpd`'s own re-export rather than through a
/// dependency of ours.
///
/// This file names `zbus` only to classify errors `ashpd` hands back.
/// Depending on the crate directly to spell those types would put a second
/// version constraint on a crate whose compiled feature set the whole Linux
/// graph shares, in exchange for no code at all.
use ashpd::zbus;

use rustdar_gps::{GpsFix, LocationPermission};

use super::{OsLocationProvider, OsLocationSink};

// No `DESKTOP_ID` constant any more, and its absence is the change this file
// is about. GeoClue's `Start()` refused to run without a `DesktopId` the
// registered agent could resolve to a `<id>.desktop`, so the id was part of
// asking for a location and had to be pinned in code. The portal identifies an
// unsandboxed caller with `xdp_app_info_is_host` and asks for no app id at
// all, so nothing in this provider has one to declare. The `.desktop` file is
// still shipped and still matters — for the launcher, the icon, window
// grouping and a future Flatpak — and the tests still pin its
// contents; see `packaging/linux/README.md`.

/// Altitude's "unknown" sentinel, the minimum double value, which `ashpd`
/// spells `-f64::MAX` and this file spells [`f64::MIN`]. They are the same
/// number; only one of them is checkable by eye, and a test pins that
/// they have not drifted apart.
const ALTITUDE_UNKNOWN: f64 = f64::MIN;

// ── The reader ──────────────────────────────────────────────────────────

/// The portal location provider: a sink to talk back through, and a session
/// when one is running.
///
/// The two-phase split the contract asks for falls out of this file's own
/// constraints rather than being imposed on it. Bringing the provider up cannot
/// call anything, because the first call that would answer "may we?" is also
/// the call that can put a dialog in front of a human; so [`start`] does nothing
/// but say [`Prompt`] and wait to be asked, and everything real happens in
/// [`request`], on the thread [`Session`] owns.
///
/// [`start`]: OsLocationProvider::start
/// [`request`]: OsLocationProvider::request
/// [`Prompt`]: rustdar_gps::LocationPermission::Prompt
pub struct OsLocationReader {
    /// Cloned into each session. Cheap: a `Sender` and two `Arc`s.
    sink: OsLocationSink,
    session: Option<Session>,
}

/// A running portal location session, stopped by dropping it.
///
/// Same shape as [`SerialGpsReader`]: a value whose drop is the off switch.
///
/// [`SerialGpsReader`]: rustdar_gps::SerialGpsReader
struct Session {
    /// The reader's end of the session thread's cancellation.
    ///
    /// A `oneshot` and not the `mpsc` the GeoClue session used, because the
    /// thing being woken is a *future* rather than a blocking iterator: the
    /// thread parks in `block_on` on a race between the portal's two signal
    /// streams and this receiver, and only a future can lose that race. Nothing
    /// is ever sent — dropping the sender resolves the receiver, which is
    /// exactly the "the reader went away" the thread has to notice — and
    /// [`is_canceled`] reads back the other direction, which is what makes
    /// [`active`] tell a live session from a thread that has already given up.
    ///
    /// [`is_canceled`]: oneshot::Sender::is_canceled
    /// [`active`]: OsLocationProvider::active
    stop: oneshot::Sender<()>,
}

impl OsLocationProvider for OsLocationReader {
    /// Always `Some` on Linux, and deliberately so: nothing that can fail is
    /// attempted here, because nothing that could fail may be attempted here.
    /// A `None` would mean "we know there is no portal", and finding that out
    /// costs a round trip on the session bus, on the frame thread, before the
    /// first frame.
    ///
    /// The initial report is [`Prompt`](LocationPermission::Prompt) and it is
    /// made synchronously, on the caller's thread, before this returns. Nobody
    /// has been asked, which is the honest description, and it is the one state
    /// in which asking is legitimate — `Unknown` would leave the gate waiting
    /// for an answer that only asking can produce, and the settings pane would
    /// read "Checking…" for the life of the process.
    fn start(sink: OsLocationSink) -> Option<Self> {
        (sink.report)(LocationPermission::Prompt);
        Some(Self {
            sink,
            session: None,
        })
    }

    /// Open a portal location session on a thread of its own.
    ///
    /// `CreateSession`, `Start` and the signal loop all run on the spawned
    /// thread, because `Start` is a portal request whose response signal can sit
    /// on an access dialog for as long as the user takes to answer it — on the
    /// frame thread that is the unbounded freeze `AUDIT.md` P0-2 was.
    ///
    /// `true` unconditionally: the thread was spawned, which is all this side
    /// can know. Every answer that matters arrives later, through
    /// [`OsLocationSink::report`].
    fn request(&mut self) -> bool {
        // `active` and not `session.is_some()`: a session whose thread has
        // already exited — refused, or ended by the portal — must not make this
        // a no-op that leaves the button dead for the life of the process.
        if self.active() {
            return true;
        }

        let (stop, stopped) = oneshot::channel();
        let sink = self.sink.clone();

        std::thread::Builder::new()
            .name("os-location-portal".into())
            .spawn(move || futures_lite::future::block_on(run_session(stopped, &sink)))
            .expect("failed to spawn os-location-portal thread");

        self.session = Some(Session { stop });
        true
    }

    /// End the session. The permission the bridge holds is untouched — this is
    /// an off switch for the stream, and the portal has no way to hand a grant
    /// back even if it were one.
    ///
    /// Dropping the [`Session`] drops the `oneshot::Sender`, which resolves the
    /// receiver the session thread is racing against; the thread then closes
    /// the portal session and exits.
    fn stop(&mut self) {
        self.session = None;
    }

    /// Whether a session thread is still running.
    ///
    /// Reads the far end of the cancellation channel rather than merely
    /// `is_some`, so a session the *portal* ended — a refusal, a `Closed`
    /// signal, a portal restart — stops claiming to be live without anything
    /// having to tell this side. Cheap enough for the frame path: one atomic
    /// load.
    fn active(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| !session.stop.is_canceled())
    }

    // No `settings_available` / `open_settings` override, and that is a
    // decision rather than an omission.
    //
    // Windows has `ms-settings:privacy-location`, a documented URI with a page
    // behind it. Linux has no equivalent: what the portal actually reads is the
    // GSettings key `org.gnome.system.location enabled`, GNOME renders it under
    // Settings → Privacy, and **KDE has no page for it at all** — which is
    // precisely the desktop where the default refuses. A button that opened
    // `gnome-control-center` on a Plasma machine would either fail or launch an
    // application the user does not have; a button that shelled out to
    // `gsettings` would be this app quietly changing a system-wide preference
    // it was not asked to change.
    //
    // So the answer is the trait's default, `false`, and the sentence that
    // would have gone next to the button goes in the settings pane instead —
    // where it can name the key and the exact command, which is the thing that
    // actually works on every desktop. See `ui_settings.rs`.
}

// ── The session ─────────────────────────────────────────────────────────

/// Create a portal session, start it, and forward every location it reports
/// until the session ends or the reader stops it.
///
/// The three things this says back to the app — a fix, a request for the frame
/// that will show it, a change of permission — arrive bundled as an
/// [`OsLocationSink`], which is the contract's whole parameter list.
async fn run_session(stopped: oneshot::Receiver<()>, out: &OsLocationSink) {
    let report = &out.report;

    // Cheap: a proxy build plus one `version` property read, and no prompt.
    //
    // It answers only one of the two "no portal" questions. A frontend that is
    // running but has no backend for `Location` fails here, as
    // `Error::PortalNotFound`; a bus with no `org.freedesktop.portal.Desktop`
    // on it at all does *not* — `ashpd` swallows that reply and assumes version
    // 1 — so it surfaces from `create_session` below as a bare `ServiceUnknown`.
    // See [`classify_bus`], which is where both end up as `Unavailable`.
    let proxy = match LocationProxy::new().await {
        Ok(proxy) => proxy,
        Err(e) => {
            report(classify(&e));
            log::warn!("no location portal: {}", explain(&e));
            return;
        }
    };

    // `Accuracy::Exact` is the top of the portal's **0–5** scale, which is a
    // different enum from GeoClue's non-contiguous 0–8 despite the overlapping
    // numbers — the portal maps between them. Asking for the top costs nothing:
    // the portal clamps to what this caller is allowed and geoclue clamps again
    // to what the machine can do (`STREET` here).
    //
    // This is also where the lockdown refusal lands: `CreateSession` checks
    // `disable-location` before it builds anything.
    let session = match proxy
        .create_session(None, None, Some(Accuracy::Exact))
        .await
    {
        Ok(session) => session,
        Err(e) => {
            report(classify(&e));
            log::warn!("the location portal refused a session: {}", explain(&e));
            return;
        }
    };
    log::debug!("portal location session at {session:?}");

    // Both subscriptions go up *before* `Start`, and neither is optional.
    //
    // `LocationUpdated` because the portal emits it as soon as geoclue has a
    // cached position, which can be inside the `Start` call; a stream opened
    // afterwards would miss that one and wait minutes for the next.
    //
    // `Closed` because the portal closes the session itself on any non-zero
    // response — so on the refusal path the signal can arrive before `Start`
    // has even returned.
    let updates = match proxy.receive_location_updated().await {
        Ok(updates) => updates,
        Err(e) => {
            report(classify(&e));
            log::warn!("could not subscribe to portal locations: {}", explain(&e));
            // Both of these bail out *after* `CreateSession` succeeded, so
            // unlike every failure above them they own a session that the
            // portal has not closed and nothing else will.
            close(&session).await;
            return;
        }
    };
    let closed = match session.receive_closed().await {
        Ok(closed) => closed,
        Err(e) => {
            report(classify(&e));
            log::warn!("could not watch the portal session: {}", explain(&e));
            close(&session).await;
            return;
        }
    };

    // The call that can sit on a human. Everything above is round trips of a
    // millisecond or two; this one returns when the portal's access dialog is
    // answered — and on an unsandboxed install there is no dialog, so it
    // returns at once with `EXACT` already granted.
    //
    // Two failures, not one: `start` itself faults (the lockdown is checked a
    // second time here), and the request's *response* carries a refusal. Both
    // are `ashpd::Error` and both are classified the same way.
    match proxy.start(&session, None).await {
        Ok(request) => {
            if let Err(e) = request.response() {
                report(classify(&e));
                log::warn!("the location portal refused to start: {}", explain(&e));
                close(&session).await;
                return;
            }
        }
        Err(e) => {
            report(classify(&e));
            log::warn!(
                "could not start the portal location session: {}",
                explain(&e)
            );
            close(&session).await;
            return;
        }
    }

    report(LocationPermission::Granted);
    log::info!("portal location session started");

    let ending = watch(updates, closed, stopped, out).await;
    log::info!("portal location session ended: {ending:?}");
    if let Some(permission) = ending.report() {
        // The push half of the permission story. Nothing polled would ever see
        // this: the gate stops asking once the answer is `Granted` and delivery
        // is live, so on a foreground desktop with the settings window shut
        // this is the only thing that knows delivery has ended.
        report(permission);
    }
    close(&session).await;
}

/// Close the portal session, best effort.
///
/// Called on every path that got as far as creating one, including the ones
/// that then failed: a session left open holds a GeoClue client alive inside
/// the portal for the life of the process. On the refusal paths the portal has
/// already closed it — it does that itself for any non-zero response — which is
/// why a failure here is `debug` and not `warn`.
async fn close<T: ashpd::desktop::SessionPortal>(session: &ashpd::desktop::Session<'_, T>) {
    if let Err(e) = session.close().await {
        log::debug!("closing the portal location session failed: {e}");
    }
}

/// Why a session stopped.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Ending {
    /// The reader asked, by dropping the [`Session`].
    Stopped,
    /// The portal closed the session, or the signal stream carrying it ended.
    Closed,
    /// The consumer went away: the fix channel is closed, so there is nobody
    /// left to deliver to and nobody left to tell.
    ConsumerGone,
}

impl Ending {
    /// What the bridge has to be told, if anything.
    ///
    /// A deliberate stop reports nothing — the bridge asked for it and its
    /// stored permission is still true — and neither does a dead consumer,
    /// because the app is shutting down and there is nothing left to render it.
    ///
    /// A session the portal ended reports [`Prompt`] and **not** [`Denied`]:
    /// the portal closes sessions for reasons that are not decisions (its own
    /// restart, a backend going away, geoclue dropping the client), a refusal
    /// has already been reported from the `Start` path before delivery ever
    /// began, and `Denied` is terminal for the gate. Telling a user to go and
    /// undo a decision nobody made is the advice `Denied` exists to avoid.
    ///
    /// [`Prompt`]: LocationPermission::Prompt
    /// [`Denied`]: LocationPermission::Denied
    fn report(self) -> Option<LocationPermission> {
        match self {
            Self::Closed => Some(LocationPermission::Prompt),
            Self::Stopped | Self::ConsumerGone => None,
        }
    }
}

/// Forward every location the portal sends until something ends the session.
///
/// Three futures race on every turn of the loop, and all three have to be in
/// the race rather than checked between events: a session that only woke for
/// locations would ignore a stop until the *next* fix, which on a machine with
/// nothing moving is minutes away.
///
/// Each is re-created per iteration, which is sound because all three are
/// cancel-safe — `next()` on a zbus signal stream leaves the message queued,
/// and polling a `oneshot::Receiver` only parks a waker.
///
/// The order is the priority, because [`futures_lite::future::or`] polls in
/// order and takes the first that is ready: **ending the session outranks
/// delivering a fix**. "Turn off" is a button somebody just pressed and has to
/// take effect now, while a position that is already sitting in the stream is
/// still there on the next poll — and if that poll never comes it is because
/// the session ended, which is the case where nobody wanted it.
async fn watch(
    mut updates: impl Stream<Item = Location> + Unpin,
    mut closed: impl Stream<Item = ()> + Unpin,
    mut stopped: oneshot::Receiver<()>,
    out: &OsLocationSink,
) -> Ending {
    loop {
        // The reader dropping its `Session`, which drops the sender. The
        // `Result` is `Err(Canceled)` on exactly that path and `Ok` only if
        // something ever sends, which nothing does.
        let halted = async {
            let _ = (&mut stopped).await;
            Event::Done(Ending::Stopped)
        };
        // The portal's own `Closed`, and likewise its stream ending.
        let shut = async {
            closed.next().await;
            Event::Done(Ending::Closed)
        };
        // A location, or the stream that carries them ending — which is the
        // connection going away, and is the same news as a close.
        let fix = async {
            updates
                .next()
                .await
                .map_or(Event::Done(Ending::Closed), Event::Fix)
        };

        match futures_lite::future::or(futures_lite::future::or(halted, shut), fix).await {
            Event::Fix(location) => {
                if !publish(&location, out) {
                    return Ending::ConsumerGone;
                }
            }
            Event::Done(ending) => return ending,
        }
    }
}

/// One turn of [`watch`]'s race.
///
/// An enum rather than three futures with three output types, because
/// [`futures_lite::future::or`] races futures that agree on one.
enum Event {
    Fix(Location),
    Done(Ending),
}

/// Hand one location to the consumer. `false` when the consumer is gone and the
/// session should stop.
fn publish(location: &Location, out: &OsLocationSink) -> bool {
    let Some(fix) = fix_from_location(location) else {
        log::warn!("a portal location had no usable coordinates");
        return true;
    };
    log::debug!(
        "OS location fix: {:.4}, {:.4} (±{} m){}",
        fix.latitude,
        fix.longitude,
        fix.accuracy_m.map_or("?".to_owned(), |a| format!("{a:.0}")),
        location
            .description()
            .map_or(String::new(), |d| format!(" from {d}"))
    );
    if out.fixes.send(fix).is_err() {
        log::info!("OS location fix channel closed, stopping the portal session");
        return false;
    }
    // The send and the wake are one step. A wake that gets separated from its
    // send is a fix sitting in a channel nothing will draw a frame to drain.
    (out.wake)();
    true
}

// ── Error classification ────────────────────────────────────────────────

/// What a failed portal call means for the permission the UI shows.
///
/// The point is that the outcomes need different sentences. A missing portal
/// and a refusing one both fail every call this file makes, and telling a user
/// to go and turn something back on when the frontend was never installed is
/// worse than saying nothing.
fn classify(error: &ashpd::Error) -> LocationPermission {
    match error {
        // The lockdown switch, raised by `CreateSession` and again by `Start`.
        // A preference somebody set, so a decision, so `Denied` — see
        // [`explain`] for why that is the right half of the pair even though
        // "your system settings" is not where most desktops keep it.
        ashpd::Error::Portal(ashpd::PortalError::NotAllowed(_)) => LocationPermission::Denied,
        // Response code 1. On a sandboxed install this is the access dialog
        // answered "Deny", or a `NONE` already sitting in the permission store
        // from the last time it was.
        ashpd::Error::Response(ResponseError::Cancelled) => LocationPermission::Denied,
        // Response code 2: the portal accepted the request and then could not
        // carry it out — most often because its own GeoClue client failed to
        // start. Nobody decided anything and nothing is missing, so this is the
        // one that must stay retryable.
        ashpd::Error::Response(ResponseError::Other) => LocationPermission::Prompt,
        // A portals frontend is running but nothing behind it implements
        // `org.freedesktop.portal.Location`.
        ashpd::Error::PortalNotFound(_) => LocationPermission::Unavailable,
        ashpd::Error::Zbus(e) => classify_bus(e),
        // Transport-level trouble and everything unrecognised. Not a decision,
        // so not `Denied`, and recoverable, so not `Unavailable`.
        _ => LocationPermission::Prompt,
    }
}

/// The same question for the errors that arrive as bare D-Bus faults.
///
/// Split out because it is the arm with the interesting mistakes in it:
/// `ashpd` reports "there is no `org.freedesktop.portal.Desktop` on this bus"
/// as an ordinary method error, and reading that as a hiccup would leave the
/// settings pane offering a button that can never work.
fn classify_bus(error: &zbus::Error) -> LocationPermission {
    match error {
        zbus::Error::MethodError(name, _, _) => match name.as_str() {
            // Nothing owns `org.freedesktop.portal.Desktop`: no portals
            // frontend is installed, or its service file is not where the bus
            // looks.
            "org.freedesktop.DBus.Error.ServiceUnknown"
            | "org.freedesktop.DBus.Error.NameHasNoOwner" => LocationPermission::Unavailable,
            // A bus policy, not a portal decision, but still a refusal that
            // clicking again will not change.
            "org.freedesktop.DBus.Error.AccessDenied" => LocationPermission::Denied,
            _ => LocationPermission::Prompt,
        },
        // No session bus reachable at all: a container with no `/run/user`
        // socket bind, a service started outside a session. There is no portal
        // to grant here and no setting that would add one.
        zbus::Error::Address(_) | zbus::Error::InputOutput(_) => LocationPermission::Unavailable,
        _ => LocationPermission::Prompt,
    }
}

/// The same error, as a sentence with the fix in it.
///
/// The lockdown case gets its own paragraph and it is the important one,
/// because it is the *default* state of a machine that has
/// `xdg-desktop-portal-gtk` installed: that backend answers `disable-location`
/// from `org.gnome.system.location enabled`, which ships `false`. The message
/// names the key, because on any desktop other than GNOME there is no page to
/// send the user to and "check your system settings" is advice that dead-ends.
fn explain(error: &ashpd::Error) -> String {
    match error {
        ashpd::Error::Portal(ashpd::PortalError::NotAllowed(_)) => format!(
            "{error}. The desktop's location switch is off. \
             xdg-desktop-portal-gtk reads it from the GSettings key \
             `org.gnome.system.location enabled`, which defaults to false and \
             has a UI only on GNOME; `gsettings set org.gnome.system.location \
             enabled true` turns it on anywhere."
        ),
        ashpd::Error::Response(ResponseError::Cancelled) => {
            format!("{error}. The location request was refused.")
        }
        ashpd::Error::Response(ResponseError::Other) => format!(
            "{error}. The portal accepted the request and could not carry it \
             out; its own GeoClue client most likely failed to start. Trying \
             again is reasonable."
        ),
        ashpd::Error::PortalNotFound(_) => format!(
            "{error}. Install a portals backend that implements it — \
             `xdg-desktop-portal-gnome`, `-gtk` or `-kde` — alongside \
             `xdg-desktop-portal` itself."
        ),
        _ if classify(error) == LocationPermission::Unavailable => format!(
            "{error}. No xdg-desktop-portal is reachable on the session bus; \
             the `xdg-desktop-portal` package is probably not installed."
        ),
        _ => error.to_string(),
    }
}

// ── Decoding ────────────────────────────────────────────────────────────

/// One `LocationUpdated` payload as plain numbers, after `ashpd` has stripped
/// the sentinels it knows about.
///
/// The seam that keeps the interesting half of this file testable without a
/// bus. Everything below this line is arithmetic and range checks over a
/// `Reading`; everything above it is D-Bus. The split is why the tests at the
/// bottom run on a CI machine with no session bus, no portal and no geoclue.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Reading {
    latitude: f64,
    longitude: f64,
    /// The 68% confidence radius in metres.
    accuracy_m: f64,
    /// Metres above sea level, or `None` for the documented sentinel.
    altitude_m: Option<f64>,
    /// Metres per second, or `None` for the documented sentinel.
    speed_mps: Option<f64>,
    /// Degrees clockwise from north, or `None` for the documented sentinel.
    heading_deg: Option<f64>,
    /// Whole seconds since the epoch.
    ///
    /// The wire type is `(tt)` — seconds and microseconds — but `ashpd`'s
    /// [`Location::timestamp`] returns a `Duration` built from the seconds
    /// alone. Nothing here needs the other half: the interface XML warns twice
    /// that this value may not increase monotonically and may be very old (a
    /// cached fix keeps the timestamp of the measurement), so nothing in this
    /// app decides *freshness* from it — the settings pane's "last fix" line
    /// counts from when the fix arrived — and a microsecond on a reading that
    /// may be an hour stale is not a quantity anybody can use.
    timestamp_s: u64,
}

impl From<&Location> for Reading {
    /// The whole D-Bus-facing half of decoding: seven fields, each named the
    /// same thing on both sides.
    fn from(location: &Location) -> Self {
        Self {
            latitude: location.latitude(),
            longitude: location.longitude(),
            accuracy_m: location.accuracy(),
            altitude_m: location.altitude(),
            speed_mps: location.speed(),
            heading_deg: location.heading(),
            timestamp_s: location.timestamp().as_secs(),
        }
    }
}

/// A portal location as a [`GpsFix`], or `None` when it does not carry a usable
/// position at all.
///
/// **`Description` is not read into the fix.** The interface says outright that
/// applications should not rely on it because not every source provides one,
/// and nothing in this app has a place to put a string. It is logged, because
/// on this machine it is the one field that says whether the position came from
/// an IP lookup or a Wi-Fi one.
fn fix_from_location(location: &Location) -> Option<GpsFix> {
    fix_from_reading(&Reading::from(location))
}

/// The half of [`fix_from_location`] that a test can reach.
///
/// `from_device_position` is what sets `FixQuality::Device`, and it stays
/// `Device` however small `accuracy_m` turns out to be. That is not an
/// omission: the portal never names its source in a field an app should act on,
/// so a 5 m fix from a USB receiver and a 5 m fix from a very good Wi-Fi
/// database arrive identically. `Gps` is a claim about *where the number came
/// from*, not about how tight it is, and promoting on accuracy would put "GPS"
/// in the UI beside a position that came from a router lookup. The accuracy is
/// not discarded — it travels in `accuracy_m`, which is what the
/// provisional-site upgrade actually gates on.
fn fix_from_reading(reading: &Reading) -> Option<GpsFix> {
    if !reading.latitude.is_finite() || !reading.longitude.is_finite() {
        return None;
    }
    Some(GpsFix {
        altitude_m: reading.altitude_m.and_then(decode_altitude),
        speed_mps: reading.speed_mps.and_then(decode_speed),
        heading_deg: reading.heading_deg.and_then(decode_heading),
        accuracy_m: decode_accuracy(reading.accuracy_m),
        timestamp: timestamp_from_epoch(reading.timestamp_s),
        ..GpsFix::from_device_position(reading.latitude, reading.longitude)
    })
}

/// Altitude, unless it is the sentinel `ashpd` should already have removed.
///
/// Compared with `==` rather than with `<`, and that is the whole reason this
/// is a function. A negative altitude is an ordinary reading — the Dead Sea
/// shore is about -430 m — so the sign test that is right for speed and
/// heading would silently delete every below-sea-level position.
fn decode_altitude(raw: f64) -> Option<f64> {
    (raw != ALTITUDE_UNKNOWN && raw.is_finite()).then_some(raw)
}

/// Speed, unless it is meaningless.
fn decode_speed(raw: f64) -> Option<f64> {
    // Not only the sentinel, which `ashpd` has already taken out. A negative
    // ground speed is meaningless whatever value a backend gives it, and a zero
    // carries nothing a consumer can act on — `HeadingSource::Auto` reads speed
    // to decide whether a bearing is trustworthy, and "stationary" is what
    // `None` already says.
    (raw > 0.0 && raw.is_finite()).then_some(raw)
}

/// Heading, unless it is off the compass.
fn decode_heading(raw: f64) -> Option<f64> {
    // Zero is *inside* the range and must stay there — due north is the one
    // bearing a truthiness test would silently delete.
    (0.0..=360.0).contains(&raw).then_some(raw)
}

/// The 68% confidence radius in metres.
///
/// The interface documents no sentinel for this one, so the only filter is the
/// one that is true by construction: a negative radius is not a radius. Zero is
/// kept — some backends report an exact position that way, and the consumer
/// treats a small number as good news.
fn decode_accuracy(raw: f64) -> Option<f64> {
    (raw >= 0.0 && raw.is_finite()).then_some(raw)
}

/// Epoch seconds as a `NaiveDateTime`.
///
/// # Carried, not trusted
///
/// See [`Reading::timestamp_s`]. This exists to fill the field in honestly
/// rather than to be compared against, and the guard is for a value a *backend*
/// can produce rather than one the wire can: `u64` seconds runs to the year 584
/// billion and `DateTime::from_timestamp` refuses anything outside chrono's
/// range.
fn timestamp_from_epoch(seconds: u64) -> Option<chrono::NaiveDateTime> {
    let seconds = i64::try_from(seconds).ok()?;
    chrono::DateTime::from_timestamp(seconds, 0).map(|t| t.naive_utc())
}

#[cfg(test)]
mod tests;
