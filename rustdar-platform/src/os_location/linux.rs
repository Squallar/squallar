//! GeoClue2 over `zbus`, on the one connection its objects will answer to.
//!
//! # Why this is a thread and not a few calls
//!
//! Three properties of the service force the shape, and each of them was
//! measured rather than assumed:
//!
//! * **`Location` objects are peer-scoped.** Reading `Latitude` on a location
//!   path over a *different* `zbus::Connection` than the one that called
//!   `GetClient` comes back `Access denied` — GeoClue's own check, not a bus
//!   policy. So the connection is not a detail to be recreated per call; it is
//!   the session, and everything from `GetClient` to the last property read
//!   happens over the same one.
//! * **`Client` lifetime is bound to that connection and `GetClient` is
//!   per-peer**, which is also why none of this can be poked at with `busctl`
//!   or `gdbus`: each invocation is a new peer and gets a different client.
//! * **`Start()` can block on a human.** Where a geoclue agent is registered
//!   (GNOME's `gnome-shell` is one) it puts a dialog up and the method does not
//!   return until it is answered. On the frame thread that is an unbounded UI
//!   freeze, which is the same hazard the Windows provider is forbidden from
//!   reintroducing. So the whole session — connect, `GetClient`, `Start`, and
//!   the signal loop — runs here.
//!
//! # What a fix from this actually is
//!
//! Measured on the development machine: `AvailableAccuracyLevel = 6`
//! (`STREET`, the ceiling this install can offer) and a real accuracy of
//! **25 km** — an IP/ichnaea lookup, not a GPS one. That is why every fix
//! leaves here as [`FixQuality::Device`](rustdar_gps::FixQuality::Device) and
//! carries its [`accuracy_m`](GpsFix::accuracy_m): the site-upgrade gate reads that field,
//! and a 25 km circle is comfortably good enough to pick between WSR-88D sites
//! ~200 km apart while being useless for anything finer.
//!
//! # Network egress
//!
//! If GeoClue resolves the position by **Wi-Fi** rather than by IP, it POSTs
//! the BSSIDs of the access points it can see to `api.beacondb.net`. That is
//! geoclue's own request, made under its own configuration — rustdar neither
//! makes nor can suppress it — but it happens because this app asked, so it is
//! disclosed here, in `packaging/linux/README.md` and in the settings pane.

use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};

use rustdar_gps::{GpsConfig, GpsFix, LocationPermission};
use zbus::blocking::{Connection, MessageIterator};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Structure, Value};

/// The bus name, object paths and interfaces this provider speaks.
const GEOCLUE_BUS: &str = "org.freedesktop.GeoClue2";
const MANAGER_PATH: &str = "/org/freedesktop/GeoClue2/Manager";
const MANAGER_IFACE: &str = "org.freedesktop.GeoClue2.Manager";
const CLIENT_IFACE: &str = "org.freedesktop.GeoClue2.Client";
const LOCATION_IFACE: &str = "org.freedesktop.GeoClue2.Location";
const PROPERTIES_IFACE: &str = "org.freedesktop.DBus.Properties";

/// The basename of the `.desktop` file, without the extension.
///
/// `Start()` fails without it — the daemon requires `DesktopId` to be set — and
/// a registered agent resolves `<DesktopId>.desktop` through `GDesktopAppInfo`
/// to find the name and icon it shows the user. It must equal the file shipped
/// by `packaging/linux/`, which in turn matches `ios/project.yml`'s bundle id
/// and the Android `applicationId`.
pub const DESKTOP_ID: &str = "dev.mcswain.rustdar";

/// `GCLUE_ACCURACY_LEVEL_EXACT`, from `gclue-enums.h`.
///
/// The scale is **not** contiguous — `NONE = 0`, `COUNTRY = 1`, `CITY = 4`,
/// `NEIGHBORHOOD = 5`, `STREET = 6`, `EXACT = 8` — and it is unrelated to the
/// xdg desktop portal's 0–5 accuracy argument, which is a different enum with
/// overlapping numbers. Asking for the top of the scale costs nothing: geoclue
/// clamps to what the machine can actually do (6 here) and the agent may clamp
/// it further.
const ACCURACY_LEVEL_EXACT: u32 = 8;

/// Altitude's "unknown" sentinel, `-1.7976931348623157e+308`, spelled the way
/// the interface XML spells it: *minimum double value*. Written as
/// [`f64::MIN`] rather than as the literal because they are the same number and
/// only one of them is checkable by eye.
const ALTITUDE_UNKNOWN: f64 = f64::MIN;

/// Speed and heading share one sentinel, documented on both properties.
const SPEED_HEADING_UNKNOWN: f64 = -1.0;

/// The bus's own interface, for the one signal that says geoclue itself has
/// gone away.
const DBUS_BUS: &str = "org.freedesktop.DBus";
const DBUS_PATH: &str = "/org/freedesktop/DBus";

// ── The reader ──────────────────────────────────────────────────────────

/// A running GeoClue2 session, stopped by dropping it.
///
/// Same shape as [`SerialGpsReader`]: a constructor that returns `None` when
/// there is nothing to read, and a value whose drop is the off switch.
///
/// [`SerialGpsReader`]: rustdar_gps::SerialGpsReader
pub struct OsLocationReader {
    /// Dropping (or sending on) this tells the session thread the stop was
    /// deliberate. It is *not* what unblocks it — see [`Closer`] — but it is
    /// what lets the thread tell "the app asked" from "the bus went away",
    /// which are reported very differently.
    stop_signal: mpsc::Sender<()>,
    /// The connection, once the thread has one, so this end can close it.
    closer: Arc<Closer>,
}

impl OsLocationReader {
    /// Start a GeoClue2 session on its own thread.
    ///
    /// Always `Some` on Linux, and deliberately so: nothing that can fail is
    /// attempted here. The system bus connect, `GetClient`, the `.desktop`
    /// preflight and `Start()` all run on the spawned thread, because the last
    /// of those can sit on an agent dialog for as long as the user takes to
    /// answer it. A `None` here would mean "we know there is no service", and
    /// this side cannot know that without making the very call it must not
    /// make.
    ///
    /// # `wake`
    ///
    /// The frontend's event loop runs on `ControlFlow::Wait` and drains the fix
    /// channel only while drawing a frame, so a fix pushed from this thread
    /// while the app is idle is invisible until something unrelated happens to
    /// draw one — with auto-refresh off, possibly never. Same parameter, same
    /// reason, as `SerialGpsReader::start`.
    ///
    /// # `report`
    ///
    /// How a permission change gets back to the bridge, and it is the half of
    /// this provider that closes a hole polling cannot. `Granted && active` is
    /// a terminal state for the gate, so a revocation made outside the app —
    /// the agent turning us off, the session ending — is *never* observed by
    /// asking. GeoClue tells us instead, by setting `Client.Active` false, and
    /// this callback is how that becomes a non-granted permission in the UI.
    pub fn start(
        _config: &GpsConfig,
        fixes: mpsc::Sender<GpsFix>,
        wake: impl Fn() + Send + 'static,
        report: impl Fn(LocationPermission) + Send + 'static,
    ) -> Option<Self> {
        let (stop_tx, stop_rx) = mpsc::channel();
        let closer = Arc::new(Closer::default());
        let thread_closer = Arc::clone(&closer);

        std::thread::Builder::new()
            .name("os-location-geoclue".into())
            .spawn(move || {
                preflight_desktop_file();
                let out = Consumer {
                    fixes: &fixes,
                    wake: &wake,
                    report: &report,
                };
                run_session(&thread_closer, &stop_rx, &out);
            })
            .expect("failed to spawn os-location-geoclue thread");

        Some(Self {
            stop_signal: stop_tx,
            closer,
        })
    }
}

impl Drop for OsLocationReader {
    /// Stop the session, in the one order that makes the reason legible.
    ///
    /// The stop message goes first so that by the time the signal iterator
    /// returns `None`, [`should_stop`] already answers `true` and the thread
    /// reports nothing. Closing first would race: the thread could see the
    /// stream end before the sender was dropped, conclude the bus had gone
    /// away, and push a spurious permission change into a bridge that is
    /// shutting the reader down on purpose.
    fn drop(&mut self) {
        let _ = self.stop_signal.send(());
        self.closer.close();
    }
}

/// The shared handle on the session's connection.
///
/// Exists to solve one narrow problem: the connection is created on the session
/// thread, but the thing that unblocks that thread — closing it — has to be
/// reachable from the side that drops the reader. `Connection` is `Clone` over
/// an `Arc` and `close()` takes `self` by value, so a clone parked here is
/// exactly enough.
///
/// The `closed` flag is in the same lock as the connection rather than beside
/// it, which is what makes the start/stop race safe: a reader dropped before
/// the thread has finished connecting finds nothing to close, and the thread's
/// [`adopt`](Closer::adopt) then reports that it is already too late instead of
/// parking a connection nobody will ever close.
#[derive(Default)]
struct Closer(Mutex<CloserState>);

#[derive(Default)]
struct CloserState {
    conn: Option<Connection>,
    closed: bool,
}

impl CloserState {
    /// Whether a connection handed over now would ever be closed again.
    ///
    /// The whole start/stop race in one predicate, and the reason it is a
    /// predicate: a test can reach this without a bus, while
    /// [`adopt`](Closer::adopt) needs a real `Connection` to park.
    fn accepts(&self) -> bool {
        !self.closed
    }
}

impl Closer {
    /// Hand the session's connection over, or learn that the reader is already
    /// gone. `false` means "stop now"; the caller still owns the connection and
    /// closes it itself.
    fn adopt(&self, conn: &Connection) -> bool {
        let mut state = self.lock();
        let open = state.accepts();
        if open {
            state.conn = Some(conn.clone());
        }
        open
    }

    /// Close the connection if there is one, and remember that we did.
    ///
    /// The close happens with the lock released. `Connection::close` shuts the
    /// socket down in both directions and that is quick, but it is still I/O,
    /// and holding a lock across I/O that the other side may be waiting on is
    /// how a stop turns into a hang.
    fn close(&self) {
        let conn = {
            let mut state = self.lock();
            state.closed = true;
            state.conn.take()
        };
        if let Some(conn) = conn
            && let Err(e) = conn.close()
        {
            log::debug!("closing the GeoClue connection failed: {e}");
        }
    }

    /// Same `PoisonError::into_inner` recovery as `RedrawWaker`, and for the
    /// same reason: a panic under this lock must not turn every later stop into
    /// a silent no-op that leaks the thread and leaves the client started.
    fn lock(&self) -> std::sync::MutexGuard<'_, CloserState> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Whether the session thread must exit: an explicit stop `()` arrived, or the
/// [`OsLocationReader`] holding the sender was dropped (`Disconnected`). Only
/// an empty-but-connected channel means keep running — a bare
/// `try_recv().is_ok()` would read the drop as "keep going" and leak the
/// thread, which here holds a started GeoClue client and a live bus connection.
///
/// Deliberately a copy of `rustdar_gps::serial`'s predicate rather than a
/// shared helper: it is three tokens of logic and one paragraph of reasoning,
/// and the reasoning is what is worth having twice.
fn should_stop(stop_rx: &mpsc::Receiver<()>) -> bool {
    !matches!(stop_rx.try_recv(), Err(mpsc::TryRecvError::Empty))
}

// ── The session ─────────────────────────────────────────────────────────

/// The three things the session says to the rest of the app: a fix, a request
/// for the frame that will show it, and a change of permission.
///
/// Bundled rather than passed as three parameters because they travel together
/// through every layer of the session and are never used apart — and because a
/// signal loop needs a connection, a client, a stop channel and a
/// de-duplication cursor as well, which is more arguments than clippy will sit
/// still for.
struct Consumer<'a, W: Fn(), R: Fn(LocationPermission)> {
    fixes: &'a mpsc::Sender<GpsFix>,
    wake: &'a W,
    report: &'a R,
}

/// Connect, claim a client, start it, and forward every location it reports
/// until the reader is dropped or the bus goes away.
fn run_session(
    closer: &Closer,
    stop_rx: &mpsc::Receiver<()>,
    out: &Consumer<'_, impl Fn(), impl Fn(LocationPermission)>,
) {
    let report = out.report;
    // The **system** bus, and this is not a detail to get wrong: geoclue ships
    // `/usr/share/dbus-1/system-services/org.freedesktop.GeoClue2.service` and
    // nothing under `services/`, so a session-bus connection answers
    // `ServiceUnknown: The name is not activatable` — which reads exactly like
    // "geoclue is not installed" on a machine where it is running fine. (The
    // *agent* half of geoclue is the part that lives in the user session; this
    // side never talks to it directly.) Peer scoping is unaffected: `GetClient`
    // is per-connection either way, which is why this one connection is held
    // for the whole session.
    let conn = match Connection::system() {
        Ok(conn) => conn,
        Err(e) => {
            // No system bus reachable: a container without `/run/dbus`, a
            // sandbox with no socket bind. There is no location service to
            // grant here and no setting that would add one.
            log::warn!("no D-Bus system bus, so no OS location service: {e}");
            report(LocationPermission::Unavailable);
            return;
        }
    };

    if !closer.adopt(&conn) {
        // Stopped between `start` returning and this thread connecting.
        let _ = conn.close();
        return;
    }
    if should_stop(stop_rx) {
        return;
    }

    let client = match get_client(&conn) {
        Ok(path) => path,
        Err(e) => {
            report(classify(&e));
            log::warn!("GeoClue2 GetClient failed: {}", explain(&e));
            return;
        }
    };
    log::debug!("GeoClue2 client at {client}");

    if let Err(e) = configure_client(&conn, &client) {
        report(classify(&e));
        log::warn!("could not configure the GeoClue2 client: {}", explain(&e));
        return;
    }

    // The signal subscription is set up *before* `Start()`, as the interface
    // XML instructs: geoclue may emit `LocationUpdated` from inside that call
    // if it already has a position cached, and a rule added afterwards would
    // miss it and wait minutes for the next one.
    let signals = match subscribe(&conn, &client) {
        Ok(signals) => signals,
        Err(e) => {
            report(classify(&e));
            log::warn!("could not subscribe to GeoClue2 signals: {}", explain(&e));
            return;
        }
    };

    if should_stop(stop_rx) {
        return;
    }

    // The call that can sit on a human. Everything above is round trips of a
    // millisecond or two; this one returns when the user answers the agent.
    if let Err(e) = call(&conn, &client, CLIENT_IFACE, "Start") {
        report(classify(&e));
        log::warn!("GeoClue2 Start() failed: {}", explain(&e));
        return;
    }
    report(LocationPermission::Granted);
    log::info!("GeoClue2 location session started as {DESKTOP_ID}");

    // Whatever geoclue already knew, before waiting on a signal for it — a
    // cached position turns "location is on" from a minutes-long wait into an
    // immediate dot. The property is "/" until the first fix ever, which
    // `current_location` reads as nothing to do.
    let mut published = None;
    if let Some(path) = current_location(&conn, &client) {
        if !publish(&conn, &path, out) {
            return;
        }
        published = Some(path);
    }

    watch(signals, &conn, &client, published, stop_rx, out);
}

/// Block on the message stream, forwarding fixes and noticing revocation, until
/// something ends it.
///
/// `last` carries the `Location` object already published, if any, so the same
/// object is not fetched and delivered twice. It has to: geoclue sets
/// `Client.Location` *and* emits `LocationUpdated` for the same fix, so the
/// read done before this loop and the loop's first signal are routinely the
/// same object, and each duplicate costs a round trip, a channel send and a
/// full frame.
fn watch(
    signals: MessageIterator,
    conn: &Connection,
    client: &str,
    mut last: Option<OwnedObjectPath>,
    stop_rx: &mpsc::Receiver<()>,
    out: &Consumer<'_, impl Fn(), impl Fn(LocationPermission)>,
) {
    let report = out.report;
    for message in signals {
        let message = match message {
            Ok(message) => message,
            Err(e) => {
                log::debug!("GeoClue2 signal stream error: {e}");
                continue;
            }
        };
        match route(&message, client) {
            Signal::NewLocation(path) => {
                if !is_new(last.as_ref(), &path) {
                    continue;
                }
                if !publish(conn, &path, out) {
                    return;
                }
                last = Some(path);
            }
            Signal::Revoked => {
                // The push half of the permission story. Nothing polled would
                // ever see this: the gate stops asking once the answer is
                // `Granted` and delivery is live, so on a foreground desktop
                // with the settings window shut this signal is the *only* thing
                // that knows consent was withdrawn.
                log::info!("GeoClue2 stopped this client; location is no longer granted");
                report(LocationPermission::Denied);
                return;
            }
            Signal::ServiceGone => {
                // The daemon went away and took our client with it. Not a
                // decision anybody made, so not `Denied` — but delivery has
                // stopped and the app must stop claiming otherwise.
                log::warn!("the GeoClue2 service exited; location stopped");
                report(LocationPermission::Prompt);
                return;
            }
            Signal::Ignore => {}
        }
    }

    if should_stop(stop_rx) {
        log::debug!("GeoClue2 session stopping (reader dropped)");
        return;
    }

    // The stream ended and nobody here asked for that: the bus went away, or
    // the connection was dropped under us. Not a denial — there is nothing for
    // the user to turn back on — so this reports the state that leaves the
    // settings button working rather than the one that reads as a decision.
    log::warn!("the D-Bus connection ended unexpectedly; location stopped");
    report(LocationPermission::Prompt);
}

/// Whether a `Location` object is one this session has not already read.
///
/// GeoClue sets `Client.Location` **and** emits `LocationUpdated` for the same
/// fix, so the property read done before the loop and the loop's first signal
/// routinely name the same object. Each duplicate costs a `GetAll` round trip,
/// a channel send and a full frame — the wake is the expensive part — for a
/// position the consumer already has.
fn is_new(last: Option<&OwnedObjectPath>, candidate: &OwnedObjectPath) -> bool {
    last != Some(candidate)
}

/// What one message off the stream means to this session.
#[derive(Debug, PartialEq, Eq)]
enum Signal {
    /// A new `Location` object to read.
    NewLocation(OwnedObjectPath),
    /// GeoClue stopped this client: consent has been withdrawn.
    Revoked,
    /// GeoClue itself released its bus name, taking the client with it.
    ServiceGone,
    /// Not addressed to this session.
    Ignore,
}

/// Decide what a message means, without reading anything off the bus.
///
/// Split out of [`watch`] because it is the part with the interesting mistakes
/// in it and the part a test can reach: `watch` needs a live `MessageIterator`
/// and therefore a bus, while this takes a `Message` a test can build. Every
/// guard below is one that failed silently in the wrong direction —
/// mis-attributing another app's revocation to this one, or missing our own.
///
/// The stream it filters is **unfiltered by design** (see [`subscribe`]), so it
/// carries this connection's own method replies and every other geoclue client
/// on the machine. Both are rejected here: replies by message type, everything
/// else by the object path it claims to be about.
fn route(message: &zbus::message::Message, client: &str) -> Signal {
    if message.message_type() != zbus::message::Type::Signal {
        return Signal::Ignore;
    }
    let header = message.header();
    let member = header.member().map(|m| m.as_str());
    let interface = header.interface().map(|i| i.as_str());
    let path = header.path().map(|p| p.as_str());
    let ours = path == Some(client);

    match (interface, member) {
        (Some(CLIENT_IFACE), Some("LocationUpdated")) if ours => {
            // `(old, new)`; only the new path is worth a round trip.
            match message
                .body()
                .deserialize::<(OwnedObjectPath, OwnedObjectPath)>()
            {
                Ok((_, new)) => Signal::NewLocation(new),
                Err(_) => {
                    log::debug!("LocationUpdated with an unexpected body");
                    Signal::Ignore
                }
            }
        }
        (Some(PROPERTIES_IFACE), Some("PropertiesChanged")) if ours => {
            if client_went_inactive(message) {
                Signal::Revoked
            } else {
                Signal::Ignore
            }
        }
        (Some(DBUS_BUS), Some("NameOwnerChanged")) if path == Some(DBUS_PATH) => {
            if geoclue_vanished(message) {
                Signal::ServiceGone
            } else {
                Signal::Ignore
            }
        }
        // Every other signal the two match rules let through — mostly other
        // apps' geoclue clients, which is what the `ours` guard is protecting
        // against. At `trace` because it is also the only way to confirm from
        // outside that the rules and the filter are wired up at all.
        _ => {
            log::trace!(
                "ignoring {}.{} on {}",
                interface.unwrap_or("?"),
                member.unwrap_or("?"),
                path.unwrap_or("?")
            );
            Signal::Ignore
        }
    }
}

/// Read one `Location` object and hand the fix to the consumer. `false` when
/// the consumer is gone and the session should stop.
fn publish(
    conn: &Connection,
    path: &OwnedObjectPath,
    out: &Consumer<'_, impl Fn(), impl Fn(LocationPermission)>,
) -> bool {
    let props = match location_properties(conn, path) {
        Ok(props) => props,
        Err(e) => {
            log::warn!("could not read GeoClue2 location {path}: {}", explain(&e));
            return true;
        }
    };
    let Some(fix) = fix_from_properties(&props) else {
        log::warn!("GeoClue2 location {path} had no usable coordinates");
        return true;
    };
    log::debug!(
        "OS location fix: {:.4}, {:.4} (±{} m)",
        fix.latitude,
        fix.longitude,
        fix.accuracy_m.map_or("?".to_owned(), |a| format!("{a:.0}"))
    );
    if out.fixes.send(fix).is_err() {
        log::info!("OS location fix channel closed, stopping the GeoClue2 session");
        return false;
    }
    // The send and the wake are one step. A wake that gets separated from its
    // send is a fix sitting in a channel nothing will draw a frame to drain.
    (out.wake)();
    true
}

// ── D-Bus calls ─────────────────────────────────────────────────────────

/// One method call with no arguments and no interesting reply.
fn call(conn: &Connection, path: &str, iface: &str, method: &str) -> zbus::Result<()> {
    conn.call_method(Some(GEOCLUE_BUS), path, Some(iface), method, &())?;
    Ok(())
}

/// `Manager.GetClient` — the per-peer client object this connection owns.
fn get_client(conn: &Connection) -> zbus::Result<String> {
    let reply = conn.call_method(
        Some(GEOCLUE_BUS),
        MANAGER_PATH,
        Some(MANAGER_IFACE),
        "GetClient",
        &(),
    )?;
    Ok(reply.body().deserialize::<OwnedObjectPath>()?.to_string())
}

/// Set the two properties `Start()` will not run without.
fn configure_client(conn: &Connection, client: &str) -> zbus::Result<()> {
    set_property(conn, client, "DesktopId", Value::from(DESKTOP_ID))?;
    set_property(
        conn,
        client,
        "RequestedAccuracyLevel",
        Value::from(ACCURACY_LEVEL_EXACT),
    )
}

fn set_property(conn: &Connection, client: &str, name: &str, value: Value<'_>) -> zbus::Result<()> {
    conn.call_method(
        Some(GEOCLUE_BUS),
        client,
        Some(PROPERTIES_IFACE),
        "Set",
        &(CLIENT_IFACE, name, value),
    )?;
    Ok(())
}

/// Ask the bus for the three signals this session has to see, and return the
/// one iterator they all arrive on.
///
/// # Why the rules are registered by hand
///
/// `MessageIterator::for_match_rule` is the tidier call and it takes exactly
/// one rule, filtered client-side. Two are needed here and there is nothing in
/// a blocking API to wait on two iterators at once — so the rules go on the
/// connection with `AddMatch` and the single unfiltered iterator does its own
/// filtering in [`watch`]. That filtering is not optional: an unfiltered stream
/// carries every other app's geoclue traffic and our own method replies.
///
/// The first rule is scoped by **path and not by member**, which is what makes
/// one rule cover two interfaces: `Client.LocationUpdated` and the
/// `Properties.PropertiesChanged` that announces revocation arrive on the same
/// object.
///
/// The second is the one the client-path rule cannot express. `geoclue.service`
/// is D-Bus activated and exits when it has no clients; if it is restarted or
/// crashes, our client object disappears **with no signal on its own path** and
/// no further location ever arrives. Nothing else in this file would notice —
/// the connection is to the bus, not to geoclue, so the stream stays open — and
/// the app would sit on a stale blue dot claiming a live grant. `arg0` narrows
/// it to geoclue's name so this is not a subscription to every name change on
/// the system bus.
fn subscribe(conn: &Connection, client: &str) -> zbus::Result<MessageIterator> {
    let bus = zbus::blocking::fdo::DBusProxy::new(conn)?;
    bus.add_match_rule(
        zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender(GEOCLUE_BUS)?
            .path(client)?
            .build(),
    )?;
    bus.add_match_rule(
        zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender(DBUS_BUS)?
            .interface(DBUS_BUS)?
            .member("NameOwnerChanged")?
            .add_arg(GEOCLUE_BUS)?
            .build(),
    )?;
    Ok(MessageIterator::from(conn))
}

/// Whether a `NameOwnerChanged` says geoclue has released its name.
///
/// The body is `(name, old_owner, new_owner)`; an empty *new* owner is the
/// service going away, and a non-empty one is it coming back — which is not the
/// same thing as our client coming back, since a restarted daemon has no memory
/// of it.
fn geoclue_vanished(message: &zbus::message::Message) -> bool {
    message
        .body()
        .deserialize::<(String, String, String)>()
        .is_ok_and(|(name, _, new_owner)| name == GEOCLUE_BUS && new_owner.is_empty())
}

/// `Client.Location`, or `None` when geoclue has not found one yet.
fn current_location(conn: &Connection, client: &str) -> Option<OwnedObjectPath> {
    let reply = conn
        .call_method(
            Some(GEOCLUE_BUS),
            client,
            Some(PROPERTIES_IFACE),
            "Get",
            &(CLIENT_IFACE, "Location"),
        )
        .ok()?;
    let path = reply.body().deserialize::<OwnedValue>().ok()?;
    let path = OwnedObjectPath::try_from(path).ok()?;
    // "/" is D-Bus's null object path, which this property carries until the
    // first fix.
    (path.as_str() != "/").then_some(path)
}

/// Every property of one `Location` object, in **one** round trip.
///
/// `GetAll` rather than seven `Get`s: the seven would be seven blocking round
/// trips per fix on a thread that is otherwise idle, for a reply the daemon
/// assembles from one struct either way.
fn location_properties(
    conn: &Connection,
    path: &OwnedObjectPath,
) -> zbus::Result<HashMap<String, OwnedValue>> {
    let reply = conn.call_method(
        Some(GEOCLUE_BUS),
        path,
        Some(PROPERTIES_IFACE),
        "GetAll",
        &(LOCATION_IFACE,),
    )?;
    reply.body().deserialize::<HashMap<String, OwnedValue>>()
}

/// Whether a `PropertiesChanged` on the client path says the client stopped.
///
/// Reads both halves of the signal. `Active` moving to `false` is the ordinary
/// revocation; `Active` being *invalidated* is the same statement made without
/// a value, and a reader that only looked at the changed map would miss it.
fn client_went_inactive(message: &zbus::message::Message) -> bool {
    let Ok((iface, changed, invalidated)) =
        message
            .body()
            .deserialize::<(String, HashMap<String, OwnedValue>, Vec<String>)>()
    else {
        return false;
    };
    if iface != CLIENT_IFACE {
        return false;
    }
    if invalidated.iter().any(|name| name == "Active") {
        return true;
    }
    changed
        .get("Active")
        .and_then(|v| bool::try_from(v.clone()).ok())
        .is_some_and(|active| !active)
}

// ── Error classification ────────────────────────────────────────────────

/// What a failed GeoClue2 call means for the permission the UI shows.
///
/// The point is that the three outcomes need three different sentences. A
/// missing service and a refused one both fail every call this file makes, and
/// telling a user to look in system settings for a switch that was never
/// installed is worse than saying nothing.
fn classify(error: &zbus::Error) -> LocationPermission {
    match error {
        zbus::Error::MethodError(name, _, _) => match name.as_str() {
            // The agent said no, or there is an agent and no `.desktop` file
            // for it to resolve us by. `preflight_desktop_file` has already
            // logged which of those it is, if it could tell.
            "org.freedesktop.DBus.Error.AccessDenied" => LocationPermission::Denied,
            // Nothing owns the name: geoclue is not installed, or its
            // service file is not where the bus looks.
            "org.freedesktop.DBus.Error.ServiceUnknown"
            | "org.freedesktop.DBus.Error.NameHasNoOwner" => LocationPermission::Unavailable,
            _ => LocationPermission::Prompt,
        },
        // Transport-level trouble. Not a decision, so not `Denied`, and
        // recoverable, so not `Unavailable`.
        _ => LocationPermission::Prompt,
    }
}

/// The same error, as a sentence with the fix in it.
fn explain(error: &zbus::Error) -> String {
    match classify(error) {
        LocationPermission::Denied => format!(
            "{error}. GeoClue refused this app. Either the location agent was \
             answered with No, or it could not find {DESKTOP_ID}.desktop — see \
             packaging/linux/README.md for how to install it."
        ),
        LocationPermission::Unavailable => format!(
            "{error}. GeoClue2 is not running and could not be started; the \
             `geoclue` package is probably not installed."
        ),
        _ => error.to_string(),
    }
}

/// Look for `<DESKTOP_ID>.desktop` where a geoclue agent will look for it, and
/// say what to do if it is missing.
///
/// Runs *before* the first round trip, because the alternative is a bare
/// `AccessDenied` from `Start()` — a message that names neither the file nor
/// the directory it belongs in, on a failure whose whole cause is a missing
/// file. The agent resolves the id through `GDesktopAppInfo`, which searches
/// `$XDG_DATA_HOME/applications` then each of `$XDG_DATA_DIRS`, so those are
/// the directories checked here, with the same defaults the spec gives.
///
/// It does not install anything and it does not stop the session. An app that
/// writes into the user's `applications/` directory to get itself a permission
/// is doing something the user did not ask for, and the file is genuinely
/// optional on installs with no agent registered — which is how this arm was
/// first measured working.
fn preflight_desktop_file() {
    let file = format!("{DESKTOP_ID}.desktop");
    if desktop_search_path(
        std::env::var_os("XDG_DATA_HOME"),
        std::env::var_os("HOME"),
        std::env::var_os("XDG_DATA_DIRS"),
    )
    .into_iter()
    .any(|dir| dir.join(&file).is_file())
    {
        return;
    }
    log::warn!(
        "{file} is not installed under XDG_DATA_HOME or XDG_DATA_DIRS. \
         GeoClue's Start() needs a DesktopId it can resolve, so where a \
         location agent is registered (GNOME's shell is one) this session \
         will be refused with AccessDenied. Install it with \
         `make -C packaging/linux install-user`."
    );
}

/// Every `applications/` directory a desktop-file lookup searches, in the order
/// it searches them.
///
/// Takes the three environment values rather than reading them, which is what
/// makes the search order testable: `std::env::set_var` is `unsafe` in this
/// edition and unsound alongside a parallel test runner, so a function that
/// read the environment itself could only ever be tested against whatever the
/// machine happened to have set.
///
/// The rules are the XDG basedir spec's, which is what `GDesktopAppInfo`
/// implements: `$XDG_DATA_HOME` first, defaulting to `~/.local/share`, then
/// each entry of `$XDG_DATA_DIRS`, defaulting to `/usr/local/share:/usr/share`.
/// An *empty* value counts as unset, not as an empty list — the spec says so,
/// and getting it wrong means a user with `XDG_DATA_DIRS=` installed to
/// `/usr/share` and gets told nothing is there.
fn desktop_search_path(
    data_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
    data_dirs: Option<std::ffi::OsString>,
) -> Vec<std::path::PathBuf> {
    let home = data_home
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| home.map(|h| std::path::PathBuf::from(h).join(".local/share")));

    let dirs = data_dirs
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    let dirs = dirs.to_string_lossy().into_owned();

    home.into_iter()
        .chain(dirs.split(':').filter(|d| !d.is_empty()).map(Into::into))
        .map(|dir: std::path::PathBuf| dir.join("applications"))
        .collect()
}

// ── Decoding ────────────────────────────────────────────────────────────

/// A `Location` object's properties as a [`GpsFix`], or `None` when it does not
/// carry a position at all.
///
/// Latitude and longitude are the only required fields. Everything else is
/// sentinel-encoded or documented as unreliable, and a location without
/// coordinates is not a location.
///
/// **`Description` is not read.** The XML says outright that applications
/// should not rely on it because not every source provides one, and nothing in
/// this app has a place to put a string anyway.
fn fix_from_properties(props: &HashMap<String, OwnedValue>) -> Option<GpsFix> {
    let latitude = number(props, "Latitude")?;
    let longitude = number(props, "Longitude")?;
    let accuracy_m = number(props, "Accuracy").and_then(decode_accuracy);

    // `from_device_position` is what sets `FixQuality::Device`, and it stays
    // `Device` however small `accuracy_m` turns out to be. That is not an
    // omission: GeoClue never names its source, so a 5 m fix from a USB
    // receiver and a 5 m fix from a very good Wi-Fi database are the same
    // D-Bus reply. `Gps` is a claim about *where the number came from*, not
    // about how tight it is, and promoting on accuracy would put "GPS" in the
    // UI beside a position that came from a router lookup. The accuracy is not
    // discarded — it travels in `accuracy_m`, which is what the
    // provisional-site upgrade actually gates on.
    Some(GpsFix {
        altitude_m: number(props, "Altitude").and_then(decode_altitude),
        speed_mps: number(props, "Speed").and_then(decode_speed),
        heading_deg: number(props, "Heading").and_then(decode_heading),
        accuracy_m,
        timestamp: props.get("Timestamp").and_then(decode_timestamp),
        ..GpsFix::from_device_position(latitude, longitude)
    })
}

/// One `d` property, or `None` when it is absent or not a double.
fn number(props: &HashMap<String, OwnedValue>, name: &str) -> Option<f64> {
    f64::try_from(props.get(name)?).ok()
}

/// Altitude, unless it is the documented sentinel.
///
/// Compared with `==` rather than with `<`, and that is the whole reason this
/// is a function. A negative altitude is an ordinary reading — the Dead Sea
/// shore is about -430 m — so the sign test that is right for speed and
/// heading would silently delete every below-sea-level position.
fn decode_altitude(raw: f64) -> Option<f64> {
    (raw != ALTITUDE_UNKNOWN).then_some(raw)
}

/// Speed, unless unknown.
fn decode_speed(raw: f64) -> Option<f64> {
    if raw == SPEED_HEADING_UNKNOWN {
        return None;
    }
    // Not only the sentinel. A negative ground speed is meaningless whatever
    // value a backend gives it, and a zero carries nothing a consumer can act
    // on — `HeadingSource::Auto` reads speed to decide whether a GPS bearing is
    // trustworthy, and "stationary" is what `None` already says.
    (raw > 0.0).then_some(raw)
}

/// Heading, unless unknown.
fn decode_heading(raw: f64) -> Option<f64> {
    if raw == SPEED_HEADING_UNKNOWN {
        return None;
    }
    // Zero is *inside* the range and must stay there — due north is the one
    // bearing a truthiness test would silently delete.
    (0.0..=360.0).contains(&raw).then_some(raw)
}

/// The 68% confidence radius in metres.
///
/// The XML documents no sentinel for this one, so the only filter is the one
/// that is true by construction: a negative radius is not a radius. Zero is
/// kept — some backends report an exact position that way, and the consumer
/// treats a small number as good news.
fn decode_accuracy(raw: f64) -> Option<f64> {
    (raw >= 0.0 && raw.is_finite()).then_some(raw)
}

/// The `(tt)` timestamp — seconds and microseconds since the epoch — as a
/// `NaiveDateTime`.
///
/// # Carried, not trusted
///
/// The interface XML warns twice about this value: geoclue cannot guarantee it
/// increases monotonically, because a backend may not respect that, and it can
/// be very old, because a cached location keeps the timestamp of the
/// measurement. So nothing in this app decides *freshness* from it — the
/// settings pane's "last fix" line counts from when the fix arrived here — and
/// this exists to fill in the field honestly rather than to be compared
/// against.
fn decode_timestamp(value: &OwnedValue) -> Option<chrono::NaiveDateTime> {
    let fields = <&Structure<'_>>::try_from(value).ok()?;
    let [seconds, micros] = fields.fields() else {
        return None;
    };
    let seconds = u64::try_from(seconds).ok()?;
    let micros = u64::try_from(micros).ok()?;
    timestamp_from_epoch(seconds, micros)
}

/// The arithmetic half of [`decode_timestamp`], away from D-Bus types.
///
/// Both guards are for values a *backend* can produce rather than for values
/// the wire can: `u64` seconds runs to the year 584 billion and
/// `DateTime::from_timestamp` refuses anything outside chrono's range, while a
/// microsecond field at or above a million is a unit mistake at the source
/// that would otherwise roll silently into the next second.
fn timestamp_from_epoch(seconds: u64, micros: u64) -> Option<chrono::NaiveDateTime> {
    let seconds = i64::try_from(seconds).ok()?;
    let nanos = u32::try_from(micros.min(999_999) * 1_000).ok()?;
    chrono::DateTime::from_timestamp(seconds, nanos).map(|t| t.naive_utc())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustdar_gps::FixQuality;

    // Every test here runs with no session bus, no geoclue and no D-Bus of any
    // kind. That is not incidental: CI has none of those, and the parts of this
    // file worth testing — what a sentinel means, what a timestamp is, when the
    // thread stops — are exactly the parts that never touch the wire.

    // ── should_stop ─────────────────────────────────────────────────────

    #[test]
    fn a_live_and_silent_stop_channel_keeps_the_session_running() {
        let (_stop_tx, stop_rx) = mpsc::channel::<()>();
        assert!(!should_stop(&stop_rx));
    }

    #[test]
    fn an_explicit_stop_message_ends_the_session() {
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        stop_tx.send(()).unwrap();
        assert!(should_stop(&stop_rx));
    }

    /// Dropping the reader drops the sender, and on this provider a thread that
    /// read `Disconnected` as "keep going" would leak a started GeoClue client
    /// and a bus connection for the life of the process. Same trap as the
    /// serial reader's, with a worse prize.
    #[test]
    fn a_dropped_reader_ends_the_session_without_sending_anything() {
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        drop(stop_tx);
        assert!(should_stop(&stop_rx));
    }

    // ── Sentinels ───────────────────────────────────────────────────────

    /// The sentinel is spelled as `f64::MIN` here and as
    /// `-1.7976931348623157e+308` in the interface XML. If those ever stop
    /// being the same number, every altitude geoclue reports becomes garbage
    /// rather than absent.
    #[test]
    fn the_altitude_sentinel_is_the_number_the_interface_documents() {
        assert_eq!(ALTITUDE_UNKNOWN, -1.797_693_134_862_315_7e308);
    }

    #[test]
    fn an_unknown_altitude_is_absent_rather_than_enormous() {
        assert_eq!(decode_altitude(ALTITUDE_UNKNOWN), None);
    }

    /// The regression this function exists for. Altitude is the one field whose
    /// sentinel is *not* a sign test, and reusing the speed rule here would
    /// discard every valid reading below sea level.
    #[test]
    fn an_altitude_below_sea_level_is_a_reading_and_not_a_sentinel() {
        assert_eq!(decode_altitude(-430.0), Some(-430.0));
    }

    #[test]
    fn an_ordinary_altitude_survives() {
        assert_eq!(decode_altitude(357.0), Some(357.0));
    }

    /// Spelled out rather than compared against itself: every other test in
    /// this section names the constant, so a sign flip here would pass all of
    /// them while turning "1 m/s" into "unknown" on every fix.
    #[test]
    fn the_speed_and_heading_sentinel_is_the_number_the_interface_documents() {
        assert_eq!(SPEED_HEADING_UNKNOWN, -1.0);
    }

    #[test]
    fn an_unknown_speed_is_absent_rather_than_reversing() {
        assert_eq!(decode_speed(SPEED_HEADING_UNKNOWN), None);
        assert_eq!(decode_speed(-0.5), None);
    }

    /// A stationary receiver says nothing about a bearing, and
    /// `HeadingSource::Auto` reads exactly this field to decide whether to
    /// trust one. `Some(0.0)` and `None` are the same statement, and only one
    /// of them stays true after a unit conversion or a comparison.
    #[test]
    fn a_stationary_reading_is_no_speed_rather_than_a_speed_of_zero() {
        assert_eq!(decode_speed(0.0), None);
    }

    #[test]
    fn a_real_speed_survives() {
        assert_eq!(decode_speed(3.5), Some(3.5));
    }

    #[test]
    fn an_unknown_heading_is_absent_rather_than_pointing_backwards() {
        assert_eq!(decode_heading(SPEED_HEADING_UNKNOWN), None);
    }

    /// Due north is 0 degrees and shares no digits with the sentinel, so a
    /// truthiness test here would delete the one bearing users notice.
    #[test]
    fn a_heading_of_due_north_is_kept() {
        assert_eq!(decode_heading(0.0), Some(0.0));
    }

    #[test]
    fn a_heading_outside_the_compass_is_rejected() {
        assert_eq!(decode_heading(360.5), None);
    }

    #[test]
    fn a_negative_accuracy_radius_is_not_a_radius() {
        assert_eq!(decode_accuracy(-1.0), None);
    }

    /// The measured value on the development machine, which must pass: the
    /// site-upgrade gate rejects only the absurd, and 25 km beats the timezone
    /// guess it replaces by an order of magnitude.
    #[test]
    fn the_twenty_five_kilometre_fix_this_machine_reports_is_kept() {
        assert_eq!(decode_accuracy(25_000.0), Some(25_000.0));
    }

    #[test]
    fn an_infinite_accuracy_radius_is_rejected() {
        assert_eq!(decode_accuracy(f64::INFINITY), None);
    }

    // ── Timestamps ──────────────────────────────────────────────────────

    #[test]
    fn the_epoch_converts_to_the_epoch() {
        let t = timestamp_from_epoch(0, 0).expect("the epoch is representable");
        assert_eq!(t.and_utc().timestamp(), 0);
        assert_eq!(t.and_utc().timestamp_subsec_nanos(), 0);
    }

    /// The microsecond half is the field that is easy to get wrong by three
    /// orders of magnitude in either direction.
    #[test]
    fn the_microsecond_half_of_the_timestamp_becomes_nanoseconds() {
        let t = timestamp_from_epoch(1_700_000_000, 500_000).expect("representable");
        assert_eq!(t.and_utc().timestamp(), 1_700_000_000);
        assert_eq!(t.and_utc().timestamp_subsec_nanos(), 500_000_000);
    }

    /// A backend that puts nanoseconds, or a whole second, in the microsecond
    /// field must not roll the reading into the following second.
    #[test]
    fn a_microsecond_field_of_a_whole_second_does_not_advance_the_clock() {
        let t = timestamp_from_epoch(1_700_000_000, 1_000_000).expect("representable");
        assert_eq!(t.and_utc().timestamp(), 1_700_000_000);
        assert!(t.and_utc().timestamp_subsec_nanos() < 1_000_000_000);
    }

    #[test]
    fn a_timestamp_beyond_the_representable_range_is_absent_rather_than_wrapped() {
        assert_eq!(timestamp_from_epoch(u64::MAX, 0), None);
    }

    /// The wire type is `(tt)`, a struct of two `u64`s, and the two halves are
    /// in an order nothing in the signature enforces. Decoding a real one is
    /// the only way to find out that they were read the wrong way round.
    #[test]
    fn the_two_halves_of_the_wire_timestamp_are_read_in_the_documented_order() {
        let wire = OwnedValue::try_from(zbus::zvariant::Value::from(
            zbus::zvariant::Structure::from((1_700_000_000u64, 250_000u64)),
        ))
        .expect("a (tt) value");

        let decoded = decode_timestamp(&wire).expect("a timestamp");

        assert_eq!(decoded.and_utc().timestamp(), 1_700_000_000);
        assert_eq!(decoded.and_utc().timestamp_subsec_nanos(), 250_000_000);
    }

    /// Anything that is not a `(tt)` leaves the field empty rather than
    /// producing an epoch-shaped lie.
    #[test]
    fn a_timestamp_of_the_wrong_shape_is_absent() {
        assert_eq!(decode_timestamp(&OwnedValue::from(17u64)), None);
    }

    // ── The whole reply ─────────────────────────────────────────────────

    /// A `GetAll` reply, built the way the daemon sends one.
    fn reply(pairs: &[(&str, OwnedValue)]) -> HashMap<String, OwnedValue> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect()
    }

    fn double(v: f64) -> OwnedValue {
        OwnedValue::from(v)
    }

    /// The shape this machine actually answers with: a real position, a 25 km
    /// circle, and a sentinel in every other numeric field.
    #[test]
    fn the_reply_this_machine_sends_becomes_a_device_fix_with_its_accuracy() {
        let props = reply(&[
            ("Latitude", double(35.4676)),
            ("Longitude", double(-97.5164)),
            ("Accuracy", double(25_000.0)),
            ("Altitude", double(ALTITUDE_UNKNOWN)),
            ("Speed", double(SPEED_HEADING_UNKNOWN)),
            ("Heading", double(SPEED_HEADING_UNKNOWN)),
        ]);

        let fix = fix_from_properties(&props).expect("a position");

        assert_eq!(fix.latitude, 35.4676);
        assert_eq!(fix.longitude, -97.5164);
        assert_eq!(fix.accuracy_m, Some(25_000.0));
        assert_eq!(fix.fix_quality, FixQuality::Device);
        assert_eq!(fix.altitude_m, None);
        assert_eq!(fix.speed_mps, None);
        assert_eq!(fix.heading_deg, None);
    }

    /// The gate downstream reads `accuracy_m`, so losing it is not a cosmetic
    /// omission: an accuracy-less fix is treated as trustworthy and spends the
    /// provisional site permanently.
    #[test]
    fn a_fix_without_its_accuracy_is_not_what_this_provider_emits() {
        let props = reply(&[
            ("Latitude", double(35.4676)),
            ("Longitude", double(-97.5164)),
            ("Accuracy", double(25_000.0)),
        ]);
        assert!(
            fix_from_properties(&props)
                .expect("a position")
                .accuracy_m
                .is_some()
        );
    }

    /// The honesty rule, pinned on the real path because it is the one somebody
    /// will be tempted to "improve": accuracy says how tight the circle is and
    /// says nothing at all about whether a satellite was involved. The variant
    /// also has to be one the site upgrade acts on, or the whole arm draws a
    /// dot and never refines anything.
    #[test]
    fn even_a_five_metre_fix_is_reported_as_a_device_fix_and_not_as_gps() {
        let props = reply(&[
            ("Latitude", double(35.4676)),
            ("Longitude", double(-97.5164)),
            ("Accuracy", double(5.0)),
        ]);
        let fix = fix_from_properties(&props).expect("a position");
        assert_eq!(fix.fix_quality, FixQuality::Device);
        assert!(fix.fix_quality.can_relocate());
    }

    /// The timestamp comes off the same reply as everything else, and a
    /// conversion that never reaches the fix is a field that silently stays
    /// `None` for every position this provider ever emits.
    #[test]
    fn the_replys_timestamp_reaches_the_fix() {
        let stamp = OwnedValue::try_from(zbus::zvariant::Value::from(
            zbus::zvariant::Structure::from((1_700_000_000u64, 0u64)),
        ))
        .expect("a (tt) value");
        let props = reply(&[
            ("Latitude", double(35.4676)),
            ("Longitude", double(-97.5164)),
            ("Accuracy", double(25_000.0)),
            ("Timestamp", stamp),
        ]);

        let fix = fix_from_properties(&props).expect("a position");

        assert_eq!(
            fix.timestamp.map(|t| t.and_utc().timestamp()),
            Some(1_700_000_000)
        );
    }

    #[test]
    fn a_reply_with_no_coordinates_is_not_a_fix() {
        assert!(fix_from_properties(&reply(&[("Accuracy", double(25_000.0))])).is_none());
        assert!(fix_from_properties(&reply(&[("Latitude", double(35.4676))])).is_none());
    }

    /// Real backends do report movement, and a fix that dropped it would lose
    /// the only heading source a desktop has.
    #[test]
    fn a_moving_fix_keeps_its_speed_and_heading() {
        let props = reply(&[
            ("Latitude", double(35.4676)),
            ("Longitude", double(-97.5164)),
            ("Accuracy", double(12.0)),
            ("Altitude", double(357.0)),
            ("Speed", double(13.4)),
            ("Heading", double(271.0)),
        ]);

        let fix = fix_from_properties(&props).expect("a position");

        assert_eq!(fix.altitude_m, Some(357.0));
        assert_eq!(fix.speed_mps, Some(13.4));
        assert_eq!(fix.heading_deg, Some(271.0));
    }

    // ── Error classification ────────────────────────────────────────────

    fn method_error(name: &str) -> zbus::Error {
        zbus::Error::MethodError(
            name.try_into().expect("a well-formed error name"),
            None,
            zbus::message::Message::method_call("/", "Whatever")
                .expect("a path and a member")
                .build(&())
                .expect("an empty body"),
        )
    }

    /// A refusal is a decision, so the pane offers system settings rather than
    /// a button that would be refused again.
    #[test]
    fn a_refusal_from_the_agent_reads_as_a_denial() {
        assert_eq!(
            classify(&method_error("org.freedesktop.DBus.Error.AccessDenied")),
            LocationPermission::Denied
        );
    }

    /// Both spellings of "nobody is there". A machine with no geoclue has no
    /// switch to turn on, and telling its user to look for one is the advice
    /// `Unavailable` exists to avoid.
    #[test]
    fn a_missing_geoclue_reads_as_unavailable_and_not_as_a_denial() {
        assert_eq!(
            classify(&method_error("org.freedesktop.DBus.Error.ServiceUnknown")),
            LocationPermission::Unavailable
        );
        assert_eq!(
            classify(&method_error("org.freedesktop.DBus.Error.NameHasNoOwner")),
            LocationPermission::Unavailable
        );
    }

    /// Anything else is a hiccup, not a verdict. `Denied` and `Unavailable` are
    /// both terminal for the gate, so classifying an unrecognised fault as
    /// either would end location for the session over a timeout.
    #[test]
    fn an_unrecognised_fault_leaves_the_user_able_to_try_again() {
        assert_eq!(
            classify(&method_error("org.freedesktop.DBus.Error.NoReply")),
            LocationPermission::Prompt
        );
        assert_eq!(
            classify(&zbus::Error::InvalidReply),
            LocationPermission::Prompt
        );
    }

    /// The log line for a refusal has to name the file and where to get it,
    /// because "AccessDenied" on its own describes a missing `.desktop` entry
    /// and a user saying no identically.
    #[test]
    fn the_denial_message_names_the_desktop_file_that_would_fix_it() {
        let message = explain(&method_error("org.freedesktop.DBus.Error.AccessDenied"));
        assert!(message.contains("dev.mcswain.rustdar.desktop"), "{message}");
        assert!(message.contains("packaging/linux"), "{message}");
    }

    #[test]
    fn the_unavailable_message_names_the_package_that_is_missing() {
        let message = explain(&method_error("org.freedesktop.DBus.Error.ServiceUnknown"));
        assert!(message.contains("geoclue"), "{message}");
    }

    // ── Revocation ──────────────────────────────────────────────────────

    fn properties_changed_on(
        path: &str,
        iface: &str,
        changed: &[(&str, OwnedValue)],
        invalidated: &[&str],
    ) -> zbus::message::Message {
        let changed: HashMap<String, OwnedValue> = changed
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect();
        let invalidated: Vec<String> = invalidated.iter().map(|s| (*s).to_owned()).collect();
        zbus::message::Message::signal(path, PROPERTIES_IFACE, "PropertiesChanged")
            .expect("a path, an interface and a member")
            .build(&(iface, changed, invalidated))
            .expect("a well-formed body")
    }

    fn properties_changed(
        iface: &str,
        changed: &[(&str, OwnedValue)],
        invalidated: &[&str],
    ) -> zbus::message::Message {
        properties_changed_on(OURS, iface, changed, invalidated)
    }

    /// The push signal the whole revocation story rests on. Polling cannot see
    /// this: `Granted` plus live delivery is terminal for the gate, so with the
    /// settings window shut nothing would ever ask again.
    #[test]
    fn the_client_being_turned_off_is_seen_as_a_revocation() {
        assert!(client_went_inactive(&properties_changed(
            CLIENT_IFACE,
            &[("Active", OwnedValue::from(false))],
            &[],
        )));
    }

    /// D-Bus lets a property change be announced without its new value, and a
    /// reader that only looked in the changed map would treat that as nothing
    /// happening.
    #[test]
    fn an_invalidated_active_property_is_also_a_revocation() {
        assert!(client_went_inactive(&properties_changed(
            CLIENT_IFACE,
            &[],
            &["Active"],
        )));
    }

    #[test]
    fn a_client_that_just_became_active_is_not_a_revocation() {
        assert!(!client_went_inactive(&properties_changed(
            CLIENT_IFACE,
            &[("Active", OwnedValue::from(true))],
            &[],
        )));
    }

    /// The match rule is scoped by path, not by interface, so signals for other
    /// interfaces on the client path arrive here too and must not be read as a
    /// verdict on ours.
    #[test]
    fn a_property_change_on_another_interface_is_not_a_revocation() {
        assert!(!client_went_inactive(&properties_changed(
            LOCATION_IFACE,
            &[("Active", OwnedValue::from(false))],
            &[],
        )));
    }

    fn name_owner_changed(name: &str, old: &str, new: &str) -> zbus::message::Message {
        zbus::message::Message::signal(DBUS_PATH, DBUS_BUS, "NameOwnerChanged")
            .expect("a path, an interface and a member")
            .build(&(name, old, new))
            .expect("a well-formed body")
    }

    /// The case no signal on our own client path can express: geoclue is D-Bus
    /// activated and exits when it has no clients, and a restart takes our
    /// client with it silently. Without this the app would keep a stale dot on
    /// the map and go on claiming a live grant.
    #[test]
    fn the_geoclue_service_releasing_its_name_stops_the_session() {
        assert!(geoclue_vanished(&name_owner_changed(
            GEOCLUE_BUS,
            ":1.42",
            ""
        )));
    }

    /// The same signal fires when it comes *back*, and a reader that did not
    /// look at the new owner would treat the restart as a second death — and,
    /// worse, would have treated the very first appearance as one too.
    #[test]
    fn the_geoclue_service_appearing_is_not_a_death() {
        assert!(!geoclue_vanished(&name_owner_changed(
            GEOCLUE_BUS,
            "",
            ":1.43"
        )));
    }

    /// `arg0` narrows the match rule, but the rule is a bus-side optimisation
    /// and not a guarantee about what arrives here.
    #[test]
    fn some_other_service_disappearing_is_not_geoclue_disappearing() {
        assert!(!geoclue_vanished(&name_owner_changed(
            "org.freedesktop.Avahi",
            ":1.7",
            ""
        )));
    }

    #[test]
    fn an_unrelated_property_change_is_not_a_revocation() {
        assert!(!client_went_inactive(&properties_changed(
            CLIENT_IFACE,
            &[("DistanceThreshold", OwnedValue::from(100u32))],
            &[],
        )));
    }

    // ── Routing ─────────────────────────────────────────────────────────
    //
    // The stream `route` filters is deliberately unfiltered, so these are the
    // tests that stand between "this session" and "every geoclue client on the
    // machine". Each guard here failed silently in one direction or the other
    // before it existed.

    const OURS: &str = "/org/freedesktop/GeoClue2/Client/1";
    const THEIRS: &str = "/org/freedesktop/GeoClue2/Client/2";

    fn location_updated(client: &str, old: &str, new: &str) -> zbus::message::Message {
        zbus::message::Message::signal(client, CLIENT_IFACE, "LocationUpdated")
            .expect("a path, an interface and a member")
            .build(&(
                OwnedObjectPath::try_from(old).expect("an object path"),
                OwnedObjectPath::try_from(new).expect("an object path"),
            ))
            .expect("a well-formed body")
    }

    #[test]
    fn our_own_location_update_is_a_new_position_to_read() {
        let message = location_updated(OURS, "/", "/org/freedesktop/GeoClue2/Location/3");
        assert_eq!(
            route(&message, OURS),
            Signal::NewLocation(
                OwnedObjectPath::try_from("/org/freedesktop/GeoClue2/Location/3")
                    .expect("an object path")
            )
        );
    }

    /// Another application's client is on the same bus, emits the same signals,
    /// and its position is not ours to report.
    #[test]
    fn another_applications_location_update_is_ignored() {
        let message = location_updated(THEIRS, "/", "/org/freedesktop/GeoClue2/Location/9");
        assert_eq!(route(&message, OURS), Signal::Ignore);
    }

    #[test]
    fn our_client_being_stopped_is_a_revocation() {
        let message = properties_changed_on(
            OURS,
            CLIENT_IFACE,
            &[("Active", OwnedValue::from(false))],
            &[],
        );
        assert_eq!(route(&message, OURS), Signal::Revoked);
    }

    /// The worst of the mis-attributions: another app's user revoking their
    /// permission would have turned ours off too.
    #[test]
    fn another_applications_client_being_stopped_is_not_our_revocation() {
        let message = properties_changed_on(
            THEIRS,
            CLIENT_IFACE,
            &[("Active", OwnedValue::from(false))],
            &[],
        );
        assert_eq!(route(&message, OURS), Signal::Ignore);
    }

    #[test]
    fn the_service_releasing_its_name_ends_the_session() {
        assert_eq!(
            route(&name_owner_changed(GEOCLUE_BUS, ":1.42", ""), OURS),
            Signal::ServiceGone
        );
    }

    #[test]
    fn the_service_reappearing_does_not_end_the_session() {
        assert_eq!(
            route(&name_owner_changed(GEOCLUE_BUS, "", ":1.43"), OURS),
            Signal::Ignore
        );
    }

    /// Method replies to this connection's own calls arrive on the same stream
    /// — they have no interface and no member, and a router that only matched
    /// on those would fall through to whatever its catch-all did.
    #[test]
    fn this_connections_own_method_replies_are_not_signals() {
        let call = zbus::message::Message::method_call("/", "Whatever")
            .expect("a path and a member")
            .build(&())
            .expect("an empty body");
        assert_eq!(route(&call, OURS), Signal::Ignore);
    }

    /// `NameOwnerChanged` is the bus's own announcement and only the bus object
    /// is entitled to make it. Anything else emitting the same
    /// interface/member pair with a convincing body is an impersonation, and
    /// acting on it would end a perfectly live session.
    #[test]
    fn a_name_change_announced_by_something_other_than_the_bus_is_ignored() {
        let impostor =
            zbus::message::Message::signal("/somewhere/else", DBUS_BUS, "NameOwnerChanged")
                .expect("a path, an interface and a member")
                .build(&(GEOCLUE_BUS, ":1.42", ""))
                .expect("a well-formed body");
        assert_eq!(route(&impostor, OURS), Signal::Ignore);
    }

    // ── De-duplication ──────────────────────────────────────────────────

    fn location(n: u32) -> OwnedObjectPath {
        OwnedObjectPath::try_from(format!("/org/freedesktop/GeoClue2/Location/{n}"))
            .expect("an object path")
    }

    /// GeoClue announces the same fix twice at startup — once by setting
    /// `Client.Location`, once by signal — and each duplicate costs a round
    /// trip and a full frame for a position the app already has.
    #[test]
    fn the_object_already_read_is_not_read_again() {
        assert!(!is_new(Some(&location(3)), &location(3)));
    }

    #[test]
    fn a_different_object_is_a_new_position() {
        assert!(is_new(Some(&location(3)), &location(4)));
        assert!(is_new(None, &location(3)));
    }

    // ── The desktop-file search path ────────────────────────────────────

    fn search(data_home: Option<&str>, home: Option<&str>, data_dirs: Option<&str>) -> Vec<String> {
        desktop_search_path(
            data_home.map(Into::into),
            home.map(Into::into),
            data_dirs.map(Into::into),
        )
        .iter()
        .map(|p| p.display().to_string())
        .collect()
    }

    /// The order matters as much as the membership: a per-user file must be
    /// found even on a machine that also has a system-wide one.
    #[test]
    fn the_users_own_data_directory_is_searched_before_the_system_ones() {
        assert_eq!(
            search(Some("/home/u/.local/share"), None, Some("/usr/share")),
            [
                "/home/u/.local/share/applications",
                "/usr/share/applications"
            ]
        );
    }

    /// `install-user` writes to `~/.local/share` and most sessions never set
    /// `XDG_DATA_HOME`, so this fallback is the common case rather than the
    /// exotic one.
    #[test]
    fn an_unset_data_home_falls_back_to_the_directory_under_home() {
        let path = search(None, Some("/home/u"), Some("/usr/share"));
        assert_eq!(
            path.first().map(String::as_str),
            Some("/home/u/.local/share/applications")
        );
    }

    /// The spec's own rule, and the one that bites: an exported-but-empty
    /// variable means "use the default", not "search nowhere". Reading it as an
    /// empty list tells a user with the file in `/usr/share` that it is
    /// missing.
    #[test]
    fn an_empty_environment_value_means_the_default_and_not_an_empty_list() {
        assert_eq!(
            search(Some(""), Some("/home/u"), Some("")),
            [
                "/home/u/.local/share/applications",
                "/usr/local/share/applications",
                "/usr/share/applications",
            ]
        );
    }

    /// A container with neither variable still gets the system directories,
    /// which is where a packaged install would have put the file.
    #[test]
    fn a_process_with_no_environment_still_searches_the_system_directories() {
        assert_eq!(
            search(None, None, None),
            ["/usr/local/share/applications", "/usr/share/applications"]
        );
    }

    /// Trailing and doubled separators are ordinary in a hand-edited
    /// `XDG_DATA_DIRS`, and an empty entry would otherwise become the relative
    /// path `applications`, searched against whatever directory the app happens
    /// to have been launched from.
    #[test]
    fn empty_entries_in_the_directory_list_do_not_become_relative_paths() {
        assert_eq!(
            search(None, None, Some("/a::/b:")),
            ["/a/applications", "/b/applications"]
        );
    }

    // ── Stopping ────────────────────────────────────────────────────────

    /// The start/stop race: `start` returns before the thread has a connection,
    /// so a reader dropped in that window has nothing to close. What must not
    /// happen is the thread then parking a connection nobody will ever close —
    /// which would leave a started GeoClue client alive for the life of the
    /// process.
    #[test]
    fn a_reader_dropped_before_its_thread_connects_refuses_the_connection() {
        let closer = Closer::default();
        assert!(
            closer.lock().accepts(),
            "a fresh session must be allowed to hand its connection over"
        );

        closer.close();

        assert!(
            !closer.lock().accepts(),
            "a connection handed over after the stop would never be closed"
        );
    }

    // ── Identity ────────────────────────────────────────────────────────

    /// Three things have to agree on this string — this constant, the file
    /// `packaging/linux/` installs, and the agent's `GDesktopAppInfo` lookup —
    /// and only the first two are checkable here. It also matches the iOS
    /// bundle id and the Android `applicationId`, which is why it is not
    /// something shorter.
    #[test]
    fn the_desktop_id_matches_the_file_the_packaging_installs() {
        let entry = include_str!("../../../packaging/linux/dev.mcswain.rustdar.desktop");
        assert!(entry.starts_with("[Desktop Entry]"), "{entry}");
        assert_eq!(DESKTOP_ID, "dev.mcswain.rustdar");
    }

    /// The agent shows the user this name and this icon; a `.desktop` file
    /// missing either resolves to a permission prompt for something nameless.
    #[test]
    fn the_desktop_entry_carries_the_name_and_icon_an_agent_will_show() {
        let entry = include_str!("../../../packaging/linux/dev.mcswain.rustdar.desktop");
        assert!(entry.contains("\nName=Rustdar"), "{entry}");
        // A bare identifier, not a path: the icon theme spec resolves this
        // against the installed hicolor sizes, and a path would pin one.
        assert!(entry.contains("\nIcon=dev.mcswain.rustdar\n"), "{entry}");
    }
}
