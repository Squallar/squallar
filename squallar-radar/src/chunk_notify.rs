//! Push notification of new real-time chunks, over a WebSocket.

use std::collections::HashMap;

use crate::chunks::{ChunkId, VolumeIndex};
use ewebsock::{WsEvent, WsMessage, WsReceiver, WsSender};

/// Backoff after a failed or dropped connection, doubling to a ceiling.
const RECONNECT_BASE: std::time::Duration = std::time::Duration::from_secs(5);
const RECONNECT_MAX: std::time::Duration = std::time::Duration::from_secs(300);

/// How long a socket may sit in [`LinkState::Connecting`] before it is torn down
/// and retried.
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// How long a socket must stay open before its drop counts as a fresh failure
/// rather than a continuing one.
const STABLE_AFTER: std::time::Duration = std::time::Duration::from_secs(60);

/// Backoff for the nth consecutive failure, doubling to [`RECONNECT_MAX`].
fn retry_delay(failures: u32) -> std::time::Duration {
    let shift = failures.saturating_sub(1).min(6);
    (RECONNECT_BASE * (1 << shift)).min(RECONNECT_MAX)
}

/// The failure count to carry into the retry after a socket drops.
///
/// A socket that stayed open past [`STABLE_AFTER`] has shown the endpoint works,
/// so its drop starts the ramp over. One that closed straight after the
/// handshake has shown nothing: `ewebsock` reports `Opened` on the HTTP upgrade,
/// before any application traffic, so a server that accepts and then closes
/// would otherwise reset the count every cycle and retry at [`RECONNECT_BASE`]
/// forever.
fn failures_after_drop(state: LinkState, held_for: std::time::Duration, failures: u32) -> u32 {
    if state == LinkState::Open && held_for >= STABLE_AFTER {
        1
    } else {
        failures.saturating_add(1)
    }
}

/// A chunk the service says now exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChunkAvailable {
    /// The message named the object, so the key is known exactly.
    Identified(ChunkId),
    /// The message said only that *something* landed for this site.
    Site(String),
}

impl ChunkAvailable {
    pub fn site(&self) -> &str {
        match self {
            Self::Identified(id) => id.site(),
            Self::Site(site) => site,
        }
    }
}

/// One notification message, as the service sends it.
#[derive(serde::Deserialize)]
struct Notification {
    station: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    volume: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

impl Notification {
    fn into_available(self) -> Option<ChunkAvailable> {
        if self.station.is_empty() {
            return None;
        }
        // The key straight off the wire, which `ChunkId::from_key` already
        // parses — the same path a listing would have produced.
        if let Some(id) = self.path.as_deref().and_then(ChunkId::from_key) {
            return Some(ChunkAvailable::Identified(id));
        }
        // `path` absent but the pieces present: rebuild it.
        if let (Some(volume), Some(name)) = (self.volume.as_deref(), self.name.as_deref())
            && let Some(volume) = volume.parse().ok().and_then(VolumeIndex::new)
            && let Some(id) = ChunkId::parse(&self.station, volume, name)
        {
            return Some(ChunkAvailable::Identified(id));
        }
        Some(ChunkAvailable::Site(self.station))
    }
}

/// Which of the service's two streams a subscription is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Feed {
    Chunk,
    Archive,
}

impl Feed {
    /// `pub` rather than `pub(crate)`: the app crate's notification driver
    /// narrows from this set when the live feed is off.
    pub const ALL: [Feed; 2] = [Feed::Chunk, Feed::Archive];

    fn route(self) -> &'static str {
        match self {
            Self::Chunk => "nexrad-chunk",
            Self::Archive => "nexrad-archive",
        }
    }
}

/// Something the service said landed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notified {
    /// A real-time chunk, identified well enough to fetch directly or at least
    /// to wake its site.
    Chunk(ChunkAvailable),
    /// A completed archive volume exists for this site.
    Archive { site: String },
}

/// Parse one message according to which stream it arrived on.
fn parse_message(feed: Feed, text: &str) -> Option<Notified> {
    match feed {
        Feed::Chunk => serde_json::from_str::<Notification>(text)
            .ok()
            .and_then(Notification::into_available)
            .map(Notified::Chunk),
        Feed::Archive => {
            let n: Notification = serde_json::from_str(text).ok()?;
            (!n.station.is_empty()).then_some(Notified::Archive { site: n.station })
        }
    }
}

/// How a site's subscription is doing, for the status bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkState {
    Connecting,
    Open,
    /// Down, with a reconnect scheduled. Polling carries the site meanwhile.
    Down,
}

struct Subscription {
    /// Held only to keep the connection alive: dropping the sender closes it.
    /// Nothing is ever sent — the subscription is the URL.
    _sender: WsSender,
    receiver: WsReceiver,
    state: LinkState,
    failures: u32,
    /// When this socket entered `state`, so [`CONNECT_TIMEOUT`] can tell a
    /// handshake still in progress from one that will never finish, and
    /// [`STABLE_AFTER`] can tell a connection that held from one that did not.
    since: web_time::Instant,
}

/// Per-site, per-feed subscriptions to the notifier service.
#[derive(Default)]
pub struct ChunkNotifier {
    subs: HashMap<(String, Feed), Subscription>,
    /// Subscriptions waiting out a backoff, kept out of `subs` so a dead socket
    /// is not held open.
    backoff: HashMap<(String, Feed), (u32, web_time::Instant)>,
    /// The endpoint the live sockets were opened against. A socket is bound to
    /// the URL it dialled, so a changed endpoint has to be reconnected rather
    /// than left talking to the old host.
    endpoint: String,
}

impl ChunkNotifier {
    pub fn new() -> Self {
        Self::default()
    }

    /// Open a subscription to every `feed` for every site in `sites`, drop the
    /// rest, and retry anything that is due.
    pub fn sync_sites(
        &mut self,
        sites: &[String],
        feeds: &[Feed],
        endpoint: &str,
        wake: impl Fn() + Send + Sync + Clone + 'static,
    ) {
        if self.endpoint != endpoint {
            endpoint.clone_into(&mut self.endpoint);
            // Nothing here can be kept: every socket and every pending retry
            // belongs to the host that just stopped being the one in use.
            self.subs.clear();
            self.backoff.clear();
        }

        let wanted =
            |site: &String, feed: &Feed| sites.iter().any(|s| s == site) && feeds.contains(feed);
        self.subs.retain(|(site, feed), _| wanted(site, feed));
        self.backoff.retain(|(site, feed), _| wanted(site, feed));

        let stuck: Vec<(String, Feed)> = self
            .subs
            .iter()
            .filter(|(_, sub)| {
                sub.state == LinkState::Connecting && sub.since.elapsed() >= CONNECT_TIMEOUT
            })
            .map(|(key, _)| key.clone())
            .collect();
        for (site, feed) in stuck {
            log::warn!("{site}: {feed:?} notification socket never finished connecting; retrying");
            let failures = self
                .subs
                .remove(&(site.clone(), feed))
                .map_or(1, |s| s.failures + 1);
            self.schedule_retry(&site, feed, failures);
        }

        let now = web_time::Instant::now();
        for site in sites {
            for feed in feeds.iter().copied() {
                let key = (site.clone(), feed);
                if self.subs.contains_key(&key) {
                    continue;
                }
                if let Some((_, retry_at)) = self.backoff.get(&key)
                    && now < *retry_at
                {
                    continue;
                }
                let failures = self.backoff.remove(&key).map(|(n, _)| n).unwrap_or(0);
                self.connect(site, feed, endpoint, failures, wake.clone());
            }
        }
    }

    /// Whether a handshake is in flight, so the frame loop must keep coming
    /// until it resolves or times out.
    pub fn handshake_pending(&self) -> bool {
        self.subs.values().any(|s| s.state == LinkState::Connecting)
    }

    /// How long until some subscription's backoff is up, or `None` when none is
    /// waiting one out.
    pub fn next_retry_delay(&self) -> Option<std::time::Duration> {
        let now = web_time::Instant::now();
        self.backoff
            .values()
            .map(|&(_, retry_at)| retry_at.saturating_duration_since(now))
            .min()
    }

    fn connect(
        &mut self,
        site: &str,
        feed: Feed,
        endpoint: &str,
        failures: u32,
        wake: impl Fn() + Send + Sync + 'static,
    ) {
        // The provider `tungstenite` will reach for at handshake time is the
        // process default, and this is the call that installs it.
        crate::tls::init();

        let url = format!(
            "{}/ws/events/{}/{site}",
            endpoint.trim_end_matches('/'),
            feed.route()
        );
        match ewebsock::connect_with_wakeup(url.clone(), ewebsock::Options::default(), wake) {
            Ok((sender, receiver)) => {
                log::info!("{site}: subscribing to {:?} notifications at {url}", feed);
                self.subs.insert(
                    (site.to_string(), feed),
                    Subscription {
                        _sender: sender,
                        receiver,
                        state: LinkState::Connecting,
                        failures,
                        since: web_time::Instant::now(),
                    },
                );
            }
            Err(e) => {
                log::warn!("{site}: could not open a {feed:?} notification socket: {e}");
                self.schedule_retry(site, feed, failures + 1);
            }
        }
    }

    fn schedule_retry(&mut self, site: &str, feed: Feed, failures: u32) {
        self.backoff.insert(
            (site.to_string(), feed),
            (failures, web_time::Instant::now() + retry_delay(failures)),
        );
    }

    /// Take everything the sockets have said since the last frame.
    pub fn drain(&mut self) -> Vec<Notified> {
        let mut out = Vec::new();
        let mut dropped: Vec<((String, Feed), u32)> = Vec::new();

        for ((site, feed), sub) in &mut self.subs {
            while let Some(event) = sub.receiver.try_recv() {
                match event {
                    WsEvent::Opened => {
                        log::info!("{site}: {feed:?} notifications connected");
                        sub.state = LinkState::Open;
                        sub.since = web_time::Instant::now();
                    }
                    WsEvent::Message(WsMessage::Text(text)) => match parse_message(*feed, &text) {
                        Some(notified) => out.push(notified),
                        None => log::debug!("{site}: ignoring {feed:?} notification {text}"),
                    },
                    // Pings, pongs and binary frames are not part of this
                    // protocol; the transport handles keepalive itself.
                    WsEvent::Message(_) => {}
                    WsEvent::Error(e) => {
                        log::warn!("{site}: {feed:?} notification socket error: {e}");
                        let failures =
                            failures_after_drop(sub.state, sub.since.elapsed(), sub.failures);
                        sub.state = LinkState::Down;
                        dropped.push(((site.clone(), *feed), failures));
                        break;
                    }
                    WsEvent::Closed => {
                        log::info!("{site}: {feed:?} notification socket closed");
                        let failures =
                            failures_after_drop(sub.state, sub.since.elapsed(), sub.failures);
                        sub.state = LinkState::Down;
                        dropped.push(((site.clone(), *feed), failures));
                        break;
                    }
                }
            }
        }

        for ((site, feed), failures) in dropped {
            self.subs.remove(&(site.clone(), feed));
            self.schedule_retry(&site, feed, failures);
        }
        out
    }

    /// Whether any socket is currently open, for the status bar.
    pub fn any_open(&self) -> bool {
        self.subs.values().any(|s| s.state == LinkState::Open)
    }

    /// Whether this site's *chunk* socket is open — the one that decides whether
    /// the live path is being pushed.
    pub fn chunk_link_open(&self, site: &str) -> bool {
        self.subs
            .get(&(site.to_string(), Feed::Chunk))
            .is_some_and(|s| s.state == LinkState::Open)
    }

    pub fn state_for(&self, site: &str, feed: Feed) -> LinkState {
        match self.subs.get(&(site.to_string(), feed)) {
            Some(sub) => sub.state,
            None => LinkState::Down,
        }
    }

    #[cfg(test)]
    pub(crate) fn subscription_count(&self) -> usize {
        self.subs.len()
    }

    #[cfg(test)]
    pub(crate) fn is_backing_off(&self, site: &str, feed: Feed) -> bool {
        self.backoff.contains_key(&(site.to_string(), feed))
    }

    /// Age a socket's handshake, so [`CONNECT_TIMEOUT`] can be exercised without
    /// the test sleeping for it.
    #[cfg(any(test, feature = "test-support"))]
    pub fn backdate_handshake(&mut self, site: &str, feed: Feed, by: std::time::Duration) {
        if let Some(sub) = self.subs.get_mut(&(site.to_string(), feed)) {
            sub.since = sub.since.checked_sub(by).unwrap_or(sub.since);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> Option<ChunkAvailable> {
        match parse_message(Feed::Chunk, json)? {
            Notified::Chunk(available) => Some(available),
            Notified::Archive { .. } => None,
        }
    }

    /// The service's real message, and the property that matters: `path` is the
    /// complete bucket key, so the fetch needs no listing to find it.
    #[test]
    fn a_notification_names_the_object_outright() {
        let got = parse(
            r#"{"station":"KJAX","volume":"415","chunk":"25","chunkType":"I",
                "l2Version":"V06","name":"20240418-033635-025-I",
                "path":"KJAX/415/20240418-033635-025-I"}"#,
        )
        .expect("parses");
        let ChunkAvailable::Identified(id) = got else {
            panic!("a message carrying `path` must identify the object, not just the site");
        };
        assert_eq!(id.key(), "KJAX/415/20240418-033635-025-I");
        assert_eq!(id.site(), "KJAX");
        assert_eq!(id.volume(), VolumeIndex::new(415).unwrap());
        assert_eq!(id.sequence(), 25);
        assert_eq!(id.kind(), crate::chunks::ChunkKind::Intermediate);
        // The part no numeric field carries, and the reason `path` is what makes
        // listing unnecessary.
        assert_eq!(
            id.volume_time(),
            chrono::NaiveDate::from_ymd_opt(2024, 4, 18)
                .unwrap()
                .and_hms_opt(3, 36, 35)
                .unwrap()
        );
    }

    /// `path` and `volume`+`name` are redundant in the protocol, so either alone
    /// is enough and a future change that drops one is survivable.
    #[test]
    fn the_object_can_be_rebuilt_without_the_path_field() {
        let got = parse(r#"{"station":"KJAX","volume":"415","name":"20240418-033635-025-I"}"#)
            .expect("parses");
        let ChunkAvailable::Identified(id) = got else {
            panic!("volume + name is enough to name the object");
        };
        assert_eq!(id.key(), "KJAX/415/20240418-033635-025-I");
    }

    /// A message with nothing usable but the station degrades one step — to an
    /// early round for that site — rather than to nothing.
    #[test]
    fn a_message_without_a_usable_key_still_names_its_site() {
        for json in [
            r#"{"station":"KTLX"}"#,
            r#"{"station":"KTLX","path":"nonsense"}"#,
            r#"{"station":"KTLX","volume":"0","name":"20240418-033635-025-I"}"#,
            r#"{"station":"KTLX","volume":"415","name":"garbage"}"#,
        ] {
            assert_eq!(
                parse(json).expect("still parses"),
                ChunkAvailable::Site("KTLX".to_string()),
                "{json} should still wake its site"
            );
        }
    }

    /// Nothing usable at all is dropped rather than treated as a failure: the
    /// service may emit event kinds this build has never heard of, and the cost
    /// of ignoring one is the ordinary timer firing instead.
    #[test]
    fn an_unreadable_notification_is_dropped_rather_than_fatal() {
        for bad in ["", "not json", "{}", r#"{"station":""}"#, "[]"] {
            assert!(parse(bad).is_none(), "{bad:?} should not parse");
        }
    }

    /// Extra fields are ignored, so the service can add them without a client
    /// release.
    #[test]
    fn unknown_fields_do_not_break_a_notification() {
        assert!(
            parse(r#"{"station":"KTLX","path":"KTLX/1/20240418-033635-025-I","somethingNew":42}"#)
                .is_some()
        );
    }

    /// The archive stream's own shape. Only the station is taken: the archive
    /// path already knows how to find the newest volume, including the
    /// previous-day fallback and the `_MDM` sidecars, and reusing that is worth
    /// more than saving one listing on an event that fires every few minutes.
    #[test]
    fn an_archive_notification_names_its_site() {
        let got = parse_message(
            Feed::Archive,
            r#"{"station":"TBOS","path":"2024/04/18/TBOS/TBOS20240418_033635_V08"}"#,
        )
        .expect("parses");
        assert_eq!(
            got,
            Notified::Archive {
                site: "TBOS".to_string()
            }
        );
    }

    /// The two streams are told apart by which socket they arrived on, not by
    /// sniffing the payload — an archive message has no `volume` or `chunkType`
    /// and would otherwise fall through to the chunk parser's site-only arm.
    #[test]
    fn the_stream_decides_the_shape_not_the_payload() {
        let archive = r#"{"station":"TBOS","path":"2024/04/18/TBOS/TBOS20240418_033635_V08"}"#;
        assert!(matches!(
            parse_message(Feed::Archive, archive),
            Some(Notified::Archive { .. })
        ));

        let chunk = r#"{"station":"KJAX","path":"KJAX/415/20240418-033635-025-I"}"#;
        assert!(matches!(
            parse_message(Feed::Chunk, chunk),
            Some(Notified::Chunk(ChunkAvailable::Identified(_)))
        ));
    }

    /// An archive message with no station is dropped rather than waking a site
    /// named by the empty string.
    #[test]
    fn an_archive_notification_without_a_station_is_dropped() {
        assert!(parse_message(Feed::Archive, r#"{"path":"a/b/c/d/e"}"#).is_none());
        assert!(parse_message(Feed::Archive, r#"{"station":""}"#).is_none());
    }

    /// Sites nothing watches lose their socket, and their backoff with it.
    #[test]
    fn subscriptions_follow_the_live_sites() {
        let mut n = ChunkNotifier::new();
        let sites = ["KTLX".to_string(), "KOUN".to_string()];
        n.sync_sites(&sites, &Feed::ALL, "wss://127.0.0.1:1", || {});
        for site in &sites {
            for feed in [Feed::Chunk, Feed::Archive] {
                assert!(
                    n.state_for(site, feed) != LinkState::Down || n.is_backing_off(site, feed),
                    "{site}/{feed:?} is neither subscribed nor scheduled to retry"
                );
            }
        }

        n.sync_sites(&[], &Feed::ALL, "wss://127.0.0.1:1", || {});
        assert_eq!(n.subscription_count(), 0, "a socket outlived its site");
        assert!(
            !n.is_backing_off("KTLX", Feed::Chunk) && !n.is_backing_off("KTLX", Feed::Archive),
            "backoff outlived the site"
        );
    }

    /// `ewebsock` reports `Opened` on the HTTP upgrade, before any application
    /// traffic, so a server that accepts the connection and then closes it —
    /// bad route, unknown site, an error raised after the upgrade — used to
    /// reset the failure count every cycle and retry forever at the base delay.
    #[test]
    fn a_socket_that_closes_straight_after_the_handshake_still_backs_off() {
        let mut failures = 0;
        let mut delays = Vec::new();
        for _ in 0..5 {
            // Opened, then closed again immediately.
            failures = failures_after_drop(
                LinkState::Open,
                std::time::Duration::from_millis(50),
                failures,
            );
            delays.push(retry_delay(failures));
        }
        assert!(
            delays.windows(2).all(|w| w[1] > w[0]),
            "backoff never grew across repeated accept-then-close cycles: {delays:?}"
        );
        assert_eq!(delays[0], RECONNECT_BASE);
    }

    /// The counterweight: a connection that actually held must not be punished
    /// for a later drop, or a long-lived socket would come back slower every
    /// time the network hiccuped.
    #[test]
    fn a_socket_that_held_starts_the_ramp_over() {
        assert_eq!(
            retry_delay(failures_after_drop(LinkState::Open, STABLE_AFTER, 6)),
            RECONNECT_BASE,
            "a socket that stayed open past STABLE_AFTER must retry at the base delay"
        );
        // And a socket that never opened at all is always a continuing failure.
        assert!(
            failures_after_drop(LinkState::Connecting, STABLE_AFTER * 10, 3) > 3,
            "a handshake that never completed must not earn a clean slate"
        );
    }

    /// The delay is bounded, and never zero — a zero would spin the frame loop.
    #[test]
    fn the_retry_delay_is_bounded() {
        for failures in 0..64u32 {
            let d = retry_delay(failures);
            assert!(
                d >= RECONNECT_BASE && d <= RECONNECT_MAX,
                "retry_delay({failures}) = {d:?} outside [{RECONNECT_BASE:?}, {RECONNECT_MAX:?}]"
            );
        }
        assert_eq!(retry_delay(u32::MAX), RECONNECT_MAX);
    }

    /// A socket is bound to the URL it dialled, so editing the endpoint has to
    /// reconnect it — otherwise it keeps talking to the old host until the site
    /// changes or the connection happens to drop.
    #[test]
    fn changing_the_endpoint_reconnects_the_sockets() {
        let mut n = ChunkNotifier::new();
        let sites = ["KTLX".to_string()];
        n.sync_sites(&sites, &[Feed::Chunk], "wss://127.0.0.1:1", || {});
        assert_eq!(n.subscription_count(), 1);

        // Strand this socket on a backoff, so there is state belonging to the
        // old host for the endpoint change to have to clear.
        n.backdate_handshake("KTLX", Feed::Chunk, CONNECT_TIMEOUT + RECONNECT_BASE);
        n.sync_sites(&sites, &[Feed::Chunk], "wss://127.0.0.1:1", || {});
        assert!(
            n.is_backing_off("KTLX", Feed::Chunk),
            "precondition: the aged socket should be waiting out a retry"
        );

        n.sync_sites(&sites, &[Feed::Chunk], "wss://127.0.0.1:2", || {});
        assert!(
            !n.is_backing_off("KTLX", Feed::Chunk),
            "the old host's backoff outlived the endpoint change"
        );
        assert_eq!(
            n.subscription_count(),
            1,
            "a changed endpoint must dial the new host straight away"
        );
    }

    /// Re-syncing the same sites does not churn their sockets — otherwise every
    /// frame would tear down and rebuild every connection.
    #[test]
    fn re_syncing_the_same_sites_keeps_their_sockets() {
        let mut n = ChunkNotifier::new();
        let sites = ["KTLX".to_string()];
        n.sync_sites(&sites, &Feed::ALL, "wss://127.0.0.1:1", || {});
        let before = n.subscription_count();
        for _ in 0..5 {
            n.sync_sites(&sites, &Feed::ALL, "wss://127.0.0.1:1", || {});
        }
        assert_eq!(n.subscription_count(), before);
    }

    /// A handshake that never resolves must not become a permanent state.
    #[test]
    fn a_handshake_that_never_resolves_is_torn_down_and_retried() {
        let mut n = ChunkNotifier::new();
        let sites = ["KTLX".to_string()];
        n.sync_sites(&sites, &Feed::ALL, "wss://127.0.0.1:1", || {});
        // Counterweight: a handshake still within its window is left alone,
        // otherwise this would "pass" by reconnecting on every frame.
        n.sync_sites(&sites, &Feed::ALL, "wss://127.0.0.1:1", || {});
        assert!(
            !n.is_backing_off("KTLX", Feed::Chunk),
            "a fresh handshake was torn down early"
        );

        n.backdate_handshake("KTLX", Feed::Chunk, CONNECT_TIMEOUT + RECONNECT_BASE);
        n.sync_sites(&sites, &Feed::ALL, "wss://127.0.0.1:1", || {});
        assert!(
            n.is_backing_off("KTLX", Feed::Chunk),
            "a socket stuck connecting was never retried"
        );
    }

    /// The frame loop's two terms, and which is which. Reconnection only runs
    /// on a frame, so if neither reported anything while a socket was down,
    /// the retry would depend on unrelated work happening to keep the loop
    /// awake.
    #[test]
    fn a_pending_reconnect_is_visible_to_the_frame_loop() {
        let mut n = ChunkNotifier::new();
        assert!(
            !n.handshake_pending() && n.next_retry_delay().is_none(),
            "an idle notifier must let the loop sleep"
        );

        let sites = ["KTLX".to_string()];
        n.sync_sites(&sites, &Feed::ALL, "wss://127.0.0.1:1", || {});
        assert!(
            n.handshake_pending(),
            "a handshake in progress must keep the loop awake so it can time out"
        );

        n.backdate_handshake("KTLX", Feed::Chunk, CONNECT_TIMEOUT + RECONNECT_BASE);
        n.backdate_handshake("KTLX", Feed::Archive, CONNECT_TIMEOUT + RECONNECT_BASE);
        n.sync_sites(&sites, &Feed::ALL, "wss://127.0.0.1:1", || {});
        let delay = n
            .next_retry_delay()
            .expect("a socket waiting out a backoff must be scheduled for");
        assert!(
            !delay.is_zero() && delay <= RECONNECT_BASE,
            "the retry is scheduled {delay:?} out, which is not the backoff \
                 it is waiting on"
        );
        assert!(
            !n.handshake_pending(),
            "a socket that timed out and went to a backoff is still being \
                 counted as a handshake, so the loop spins through the whole \
                 backoff instead of sleeping it"
        );

        n.sync_sites(&[], &Feed::ALL, "wss://127.0.0.1:1", || {});
        assert!(
            !n.handshake_pending() && n.next_retry_delay().is_none(),
            "a retired site must not keep the loop awake forever"
        );
    }

    /// The backoff grows, and the wake grows with it. A `next_retry_delay`
    /// that answered a constant would be right on the first attempt and wake
    /// the app sixty times too often by the sixth.
    #[test]
    fn a_lengthening_backoff_lengthens_the_wake_it_asks_for() {
        let mut n = ChunkNotifier::new();

        n.schedule_retry("KTLX", Feed::Chunk, 1);
        let first = n.next_retry_delay().expect("a retry is scheduled");
        assert!(
            first > RECONNECT_BASE / 2 && first <= RECONNECT_BASE,
            "the first retry is {first:?}, not the base backoff"
        );

        n.schedule_retry("KTLX", Feed::Chunk, 4);
        let later = n.next_retry_delay().expect("a retry is still scheduled");
        assert!(
            later > first * 3,
            "the fourth failure asks for {later:?} against the first's \
                 {first:?}, so the backoff is not reaching the wake"
        );

        // And the soonest of several, not whichever the map iterates first.
        n.schedule_retry("KTLX", Feed::Archive, 1);
        let soonest = n.next_retry_delay().expect("two retries are scheduled");
        assert!(
            soonest <= RECONNECT_BASE,
            "the loop is sleeping past the sooner of two retries: {soonest:?}"
        );

        n.sync_sites(&[], &Feed::ALL, "wss://127.0.0.1:1", || {});
        assert_eq!(
            n.next_retry_delay(),
            None,
            "a retired site's backoff is still asking for frames"
        );
    }

    /// Turning the live chunk feed off narrows the subscriptions rather than
    /// dropping them: archive pushes are worth most exactly when chunks are not
    /// running, because the archive path is then the one carrying the site.
    #[test]
    fn archive_notifications_survive_the_chunk_feed_being_off() {
        let mut n = ChunkNotifier::new();
        let sites = ["KTLX".to_string()];
        n.sync_sites(&sites, &Feed::ALL, "wss://127.0.0.1:1", || {});
        assert_eq!(n.subscription_count(), 2);

        n.sync_sites(&sites, &[Feed::Archive], "wss://127.0.0.1:1", || {});
        assert_eq!(
            n.subscription_count(),
            1,
            "narrowing the feeds should leave exactly the archive socket"
        );
        assert!(
            !n.is_backing_off("KTLX", Feed::Chunk),
            "a de-subscribed feed should not keep retrying"
        );
        assert_ne!(
            n.state_for("KTLX", Feed::Archive),
            LinkState::Down,
            "the archive socket was dropped with the chunk feed"
        );
    }

    /// A site with no subscription reports `Down`, which is what makes the
    /// status bar say "polling" rather than claiming a link it does not have.
    #[test]
    fn an_unsubscribed_site_reports_down() {
        let n = ChunkNotifier::new();
        assert_eq!(n.state_for("KTLX", Feed::Chunk), LinkState::Down);
        assert!(!n.any_open());
        assert!(!n.chunk_link_open("KTLX"));
    }
}
