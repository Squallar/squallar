//! `org.freedesktop.portal.Location`, over `ashpd`, and nothing else. No
//! fallback to GeoClue directly: only the portal reads
//! `impl.portal.Lockdown`'s `disable-location`, and inside a Flatpak there is
//! no system bus to reach geoclue on.
//!
//! The whole session runs on its own thread: `ashpd` has no blocking API and
//! `Start()` does not return until a human answers the access dialog.
//!
//! Measured through the portal here: 25 km, `GeoIP (ichnaea)`, so every fix
//! leaves as [`FixQuality::Device`](crate::FixQuality::Device) carrying its
//! [`accuracy_m`](Fix::accuracy_m).
//!
//! Egress: if geoclue resolves by Wi-Fi rather than IP it POSTs the visible
//! BSSIDs to `api.beacondb.net` — squallar neither makes nor can suppress that
//! request. Also disclosed in `packaging/linux/README.md` and the settings pane.
//!
//! `xdg-desktop-portal-gtk` implements `Lockdown` from the GSettings key
//! `org.gnome.system.location enabled`, which defaults to `false` and has a UI
//! only on GNOME, so a stock KDE machine refuses every session — with
//! [`Denied`](LocationPermission::Denied), not `Unavailable`.

use futures_channel::oneshot;
use futures_lite::{Stream, StreamExt as _};

use ashpd::desktop::ResponseError;
use ashpd::desktop::location::{Accuracy, Location, LocationProxy};

/// `zbus` through `ashpd`'s re-export: named here only to classify errors.
use ashpd::zbus;

use crate::{Fix, LocationPermission};

use super::{OsLocationProvider, OsLocationSink};

/// The portal location provider: a sink to talk back through, and a session
/// when one is running. Bringing it up cannot call anything, because the first
/// call that would answer "may we?" is also the one that can put a dialog in
/// front of a human — so `start` only says `Prompt`.
pub struct OsLocationReader {
    sink: OsLocationSink,
    session: Option<Session>,
}

/// A running portal location session, stopped by dropping it.
struct Session {
    /// The reader's end of the session thread's cancellation. A `oneshot`
    /// because the thread parks in `block_on` on a race between the portal's
    /// signal streams and this receiver, and only a future can lose that race.
    /// Nothing is ever sent — dropping the sender resolves the receiver.
    stop: oneshot::Sender<()>,
}

impl OsLocationProvider for OsLocationReader {
    /// Always `Some`: finding out whether a portal exists costs a session-bus
    /// round trip on the frame thread before the first frame. The initial report
    /// is [`Prompt`](LocationPermission::Prompt), made synchronously — `Unknown`
    /// would leave the gate waiting for an answer only asking can produce.
    fn start(sink: OsLocationSink) -> Option<Self> {
        (sink.report)(LocationPermission::Prompt);
        Some(Self {
            sink,
            session: None,
        })
    }

    /// Open a portal location session on a thread of its own, because `Start`'s
    /// response signal can sit on an access dialog for as long as the user
    /// takes. `true` means only that the thread was spawned.
    fn request(&mut self) -> bool {
        // A session whose thread has already exited must not make this a no-op
        // that leaves the button dead.
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

    /// End the session — an off switch for the stream, not a revocation.
    /// Dropping the [`Session`] drops the sender, which resolves the receiver
    /// the session thread is racing against.
    fn stop(&mut self) {
        self.session = None;
    }

    /// Whether a session thread is still running. Reads the far end of the
    /// cancellation channel, so a session the *portal* ended stops claiming to
    /// be live.
    fn active(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| !session.stop.is_canceled())
    }

    // No `settings_available` / `open_settings`: the only page that exists is
    // GNOME's, and KDE — the desktop where the default refuses — has none. The
    // settings pane names the GSettings key and the command instead.
}

/// Create a portal session, start it, and forward every location it reports
/// until the session ends or the reader stops it.
async fn run_session(stopped: oneshot::Receiver<()>, out: &OsLocationSink) {
    let report = &out.report;

    // Answers only one of the two "no portal" questions: a bus with no
    // `org.freedesktop.portal.Desktop` on it does *not* fail here, because
    // `ashpd` assumes version 1, so it surfaces from `create_session` as a bare
    // `ServiceUnknown`. See [`classify_bus`].
    let proxy = match LocationProxy::new().await {
        Ok(proxy) => proxy,
        Err(e) => {
            report(classify(&e));
            log::warn!("no location portal: {}", explain(&e));
            return;
        }
    };

    // `Accuracy::Exact` is the top of the portal's 0–5 scale, a different enum
    // from GeoClue's non-contiguous 0–8 despite the overlapping numbers; both
    // layers clamp. Also where the lockdown refusal lands.
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

    // Both subscriptions go up *before* `Start`: the portal emits
    // `LocationUpdated` as soon as geoclue has a cached position, possibly
    // inside the `Start` call, and closes the session itself on any non-zero
    // response, possibly before `Start` returns.
    let updates = match proxy.receive_location_updated().await {
        Ok(updates) => updates,
        Err(e) => {
            report(classify(&e));
            log::warn!("could not subscribe to portal locations: {}", explain(&e));
            // Unlike every failure above, this path owns a session the portal
            // has not closed.
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

    // The call that can sit on a human. Two failures, not one: `start` itself
    // faults (the lockdown is checked again here), and the request's *response*
    // carries a refusal.
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
        // The push half: the gate stops asking once the answer is `Granted` and
        // delivery is live, so nothing polled sees this.
        report(permission);
    }
    close(&session).await;
}

/// Close the portal session, best effort. A session left open holds a GeoClue
/// client alive inside the portal for the life of the process. On the refusal
/// paths the portal has already closed it, hence `debug` and not `warn`.
async fn close<T: ashpd::desktop::SessionPortal>(session: &ashpd::desktop::Session<'_, T>) {
    if let Err(e) = session.close().await {
        log::debug!("closing the portal location session failed: {e}");
    }
}

/// Why a session stopped.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Ending {
    Stopped,
    Closed,
    ConsumerGone,
}

impl Ending {
    /// What the bridge has to be told, if anything. A session the portal ended
    /// reports `Prompt` and **not** `Denied`: the portal closes sessions for
    /// reasons that are not decisions, a real refusal has already been reported
    /// from the `Start` path, and `Denied` is terminal.
    fn report(self) -> Option<LocationPermission> {
        match self {
            Self::Closed => Some(LocationPermission::Prompt),
            Self::Stopped | Self::ConsumerGone => None,
        }
    }
}

/// Forward every location the portal sends until something ends the session.
///
/// All three futures must be in the race rather than checked between events: a
/// session that only woke for locations would ignore a stop until the next fix,
/// minutes away. Re-created per iteration, which is sound because all three are
/// cancel-safe; the order is the priority, so ending outranks delivering.
async fn watch(
    mut updates: impl Stream<Item = Location> + Unpin,
    mut closed: impl Stream<Item = ()> + Unpin,
    mut stopped: oneshot::Receiver<()>,
    out: &OsLocationSink,
) -> Ending {
    loop {
        // The `Result` is `Err(Canceled)` on exactly the dropped-sender path.
        let halted = async {
            let _ = (&mut stopped).await;
            Event::Done(Ending::Stopped)
        };
        let shut = async {
            closed.next().await;
            Event::Done(Ending::Closed)
        };
        // A location, or the stream ending — the same news as a close.
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

/// One turn of [`watch`]'s race. An enum because
/// [`futures_lite::future::or`] races futures that agree on one output type.
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
        fix.point.lat,
        fix.point.lon,
        fix.accuracy_m.map_or("?".to_owned(), |a| format!("{a:.0}")),
        location
            .description()
            .map_or(String::new(), |d| format!(" from {d}"))
    );
    if out.fixes.send(fix).is_err() {
        log::info!("OS location fix channel closed, stopping the portal session");
        return false;
    }
    // The send and the wake are one step: a wake separated from its send is a
    // fix nothing will draw a frame to drain.
    (out.wake)();
    true
}

/// What a failed portal call means for the permission the UI shows. A missing
/// portal and a refusing one both fail every call, and need different sentences.
fn classify(error: &ashpd::Error) -> LocationPermission {
    match error {
        // The lockdown switch, raised by `CreateSession` and again by `Start`.
        ashpd::Error::Portal(ashpd::PortalError::NotAllowed(_)) => LocationPermission::Denied,
        // Response code 1: the dialog answered "Deny", or a stored `NONE`.
        ashpd::Error::Response(ResponseError::Cancelled) => LocationPermission::Denied,
        // Response code 2: accepted then not carried out, most often the
        // portal's own GeoClue client. Must stay retryable.
        ashpd::Error::Response(ResponseError::Other) => LocationPermission::Prompt,
        // A portals frontend is running but nothing implements `Location`.
        ashpd::Error::PortalNotFound(_) => LocationPermission::Unavailable,
        ashpd::Error::Zbus(e) => classify_bus(e),
        // Not a decision, so not `Denied`; recoverable, so not `Unavailable`.
        _ => LocationPermission::Prompt,
    }
}

/// The same question for errors that arrive as bare D-Bus faults. `ashpd`
/// reports "there is no `org.freedesktop.portal.Desktop` on this bus" as an
/// ordinary method error, which read as a hiccup would offer a dead button.
fn classify_bus(error: &zbus::Error) -> LocationPermission {
    match error {
        zbus::Error::MethodError(name, _, _) => match name.as_str() {
            // No portals frontend is installed.
            "org.freedesktop.DBus.Error.ServiceUnknown"
            | "org.freedesktop.DBus.Error.NameHasNoOwner" => LocationPermission::Unavailable,
            // A bus policy, not a portal decision, but still a refusal.
            "org.freedesktop.DBus.Error.AccessDenied" => LocationPermission::Denied,
            _ => LocationPermission::Prompt,
        },
        // No session bus reachable at all.
        zbus::Error::Address(_) | zbus::Error::InputOutput(_) => LocationPermission::Unavailable,
        _ => LocationPermission::Prompt,
    }
}

/// The same error, as a sentence with the fix in it. The lockdown case is the
/// *default* state of a machine with `xdg-desktop-portal-gtk` installed, and the
/// message names the GSettings key because only GNOME has a page for it.
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

/// One `LocationUpdated` payload as plain numbers, after `ashpd` has stripped
/// the sentinels it knows about. The seam that keeps decoding testable without
/// a bus: everything below is arithmetic, everything above is D-Bus.
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
    /// Whole seconds since the epoch. The wire type is `(tt)` but `ashpd`'s
    /// [`Location::timestamp`] returns the seconds alone. The interface XML
    /// warns this value may not increase monotonically and may be very old, so
    /// nothing here decides *freshness* from it.
    timestamp_s: u64,
}

impl From<&Location> for Reading {
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

/// A portal location as a [`Fix`], or `None` when it carries no usable position.
/// `Description` is not read into the fix — the interface says not to rely on it
/// — but it is logged, being the field that says IP lookup versus Wi-Fi.
fn fix_from_location(location: &Location) -> Option<Fix> {
    fix_from_reading(&Reading::from(location))
}

/// The half of [`fix_from_location`] that a test can reach. Stays
/// `FixQuality::Device` however small `accuracy_m` is: the portal never names
/// its source in a field an app should act on, so a 5 m fix from a USB receiver
/// and one from a Wi-Fi database arrive identically. The accuracy travels in
/// `accuracy_m`, which is what the site upgrade gates on.
fn fix_from_reading(reading: &Reading) -> Option<Fix> {
    if !reading.latitude.is_finite() || !reading.longitude.is_finite() {
        return None;
    }
    Some(Fix {
        altitude_m: reading.altitude_m.and_then(decode_altitude),
        speed_mps: reading.speed_mps.and_then(decode_speed),
        heading_deg: reading.heading_deg.and_then(decode_heading),
        accuracy_m: decode_accuracy(reading.accuracy_m),
        timestamp: timestamp_from_epoch(reading.timestamp_s),
        ..Fix::from_device_position(reading.latitude, reading.longitude)
    })
}

/// Altitude, unless it is the sentinel `ashpd` should already have removed.
///
/// The band and not an equality against the sentinel: `-G_MAXDOUBLE` is one
/// spelling of "unknown" and a portal backend answering with another is the
/// same reading. A negative altitude is still an ordinary one — the Dead Sea
/// shore is about -430 m — which is why the band's floor is below it.
fn decode_altitude(raw: f64) -> Option<f64> {
    crate::plausible::altitude_m(raw)
}

/// Speed, unless it is meaningless.
fn decode_speed(raw: f64) -> Option<f64> {
    // Stricter than the shared floor by one value: a zero carries nothing a
    // consumer can act on here — `HeadingSource::Auto` reads this field to
    // decide whether a bearing is trustworthy.
    crate::plausible::speed_mps(raw).filter(|&speed| speed > 0.0)
}

/// Heading, unless it is off the compass.
fn decode_heading(raw: f64) -> Option<f64> {
    crate::plausible::heading_deg(raw)
}

/// The 68% confidence radius in metres. No documented sentinel, so the only
/// filter is that a negative radius is not a radius; zero is kept.
fn decode_accuracy(raw: f64) -> Option<f64> {
    (raw >= 0.0 && raw.is_finite()).then_some(raw)
}

/// Epoch seconds as a `NaiveDateTime`. See [`Reading::timestamp_s`]: carried,
/// not trusted. The guard is for a value a backend can produce — `u64` seconds
/// runs well past chrono's range.
fn timestamp_from_epoch(seconds: u64) -> Option<chrono::NaiveDateTime> {
    let seconds = i64::try_from(seconds).ok()?;
    chrono::DateTime::from_timestamp(seconds, 0).map(|t| t.naive_utc())
}

#[cfg(test)]
mod tests;
