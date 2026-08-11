//! Push notification of new real-time chunks, over a WebSocket.
//!
//! The chunk feed's latency is bound by its poll interval, not by the bucket —
//! measured at a median 4 s against a 5 s interval. `nexrad-aws-notifier` bridges
//! the NEXRAD SNS topic to a per-station WebSocket, so a chunk can be fetched the
//! moment it exists instead of on the next tick.
//!
//! # A notification names the object, so it drives the fetch outright
//!
//! The message carries `path` — the complete bucket key. That is the whole
//! difference between an early wake-up and a shortcut: the volume's
//! `YYYYMMDD-HHMMSS` start time is part of the object name and cannot be derived
//! from the numeric fields, so without it a listing would still be needed to
//! learn the key. With it, [`rustdar_radar::chunks::ChunkPoller::fetch_notified`]
//! goes straight to a `GET`.
//!
//! What that retires, per site: the ~11-request cold-start discovery search, the
//! directory listing every round, and the rollover probe — the notification's own
//! volume start time says which volume a chunk belongs to.
//!
//! # Degradation is the absence of a feature, not a second path
//!
//! The periodic poll never stops. It simply finds nothing new while
//! notifications are doing the work, so it backs off to its quiet interval and
//! sits there as a gap-filler for a dropped socket or a missed message.
//!
//! So there is no fallback to get right: if the service is unreachable, the
//! endpoint is wrong, the socket drops, or the network blocks it, no
//! notifications arrive and the timer carries the site exactly as it does with
//! the feature switched off.
//!
//! A message this build cannot fully read degrades one step rather than to
//! nothing: an unparseable `path` still yields the station, which brings that
//! site's next round forward.
//!
//! # No async
//!
//! `ewebsock` hands back a `WsReceiver` with a non-blocking `try_recv`, and its
//! reader lives on its own thread natively and on the browser's event loop on
//! web. So this drains once a frame like every other channel in this crate, with
//! no executor and no `MaybeSend` gymnastics.

use std::collections::HashMap;

use ewebsock::{WsEvent, WsMessage, WsReceiver, WsSender};
use rustdar_radar::chunks::{ChunkId, VolumeIndex};

/// Backoff after a failed or dropped connection, doubling to a ceiling.
///
/// A ceiling, never a limit: retries continue for as long as the setting is on
/// and the site is live. A service that is down for an hour is exactly the case
/// this has to survive, and giving up would leave the site silently on the
/// slower path with nothing to say so.
const RECONNECT_BASE: std::time::Duration = std::time::Duration::from_secs(5);
const RECONNECT_MAX: std::time::Duration = std::time::Duration::from_secs(300);

/// How long a socket may sit in [`LinkState::Connecting`] before it is torn down
/// and retried.
///
/// `ewebsock` reports failures by event, so an ordinary refusal already lands as
/// `Error` or `Closed`. This covers the case with no event at all: a handshake
/// black-holed by a proxy, or a browser that neither opens nor rejects. Without
/// it such a socket sits in `subs` forever and the reconnect loop skips it,
/// because "already subscribed" and "still trying" are the same state there.
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// A chunk the service says now exists.
///
/// Carries the full [`ChunkId`] when the message named the object, which is what
/// lets a notification drive a direct `GET` — no listing, no discovery, no
/// rollover probe. Falls back to the loose fields when it did not, so an older
/// or changed service still buys the early wake-up.
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
///
/// `volume` and `chunk` arrive as strings rather than numbers. `path` is the
/// complete bucket key — `{site}/{volume}/{name}` — which is the whole reason
/// this can skip listing: the volume's `YYYYMMDD-HHMMSS` start time is part of
/// the object name and is not derivable from the numeric fields.
///
/// Everything except `station` is optional so a service that changes shape
/// degrades to a wake-up rather than going silent.
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
        // `path` absent but the pieces present: rebuild it. Kept because the two
        // fields are redundant in the protocol and either could be the one that
        // survives a future change.
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
///
/// Both matter, for different reasons. Chunks are the low-latency live path.
/// Archive volumes are what the 60-second auto-poll is looking for — the
/// fallback when chunks are off or retired, the source for panes parked on
/// historic data, and what feeds loop frames — so pushing those too takes the
/// fallback from "up to a minute late" to "as soon as it is published".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Feed {
    Chunk,
    Archive,
}

impl Feed {
    pub(crate) const ALL: [Feed; 2] = [Feed::Chunk, Feed::Archive];

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
    ///
    /// Carries only the site on purpose. The archive path already knows how to
    /// find the newest volume, including the previous-day fallback and the
    /// `_MDM` sidecars, and reusing that is worth far more than saving one
    /// listing on an event that fires about once every five minutes.
    Archive { site: String },
}

/// Parse one message according to which stream it arrived on.
///
/// The two share a `station` field and nothing else that matters, so the stream
/// decides the shape rather than the payload being sniffed.
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
    /// When this socket was opened, so [`CONNECT_TIMEOUT`] can tell a handshake
    /// still in progress from one that will never finish.
    since: web_time::Instant,
}

/// Per-site, per-feed subscriptions to the notifier service.
#[derive(Default)]
pub struct ChunkNotifier {
    subs: HashMap<(String, Feed), Subscription>,
    /// Subscriptions waiting out a backoff, kept out of `subs` so a dead socket
    /// is not held open.
    backoff: HashMap<(String, Feed), (u32, web_time::Instant)>,
}

impl ChunkNotifier {
    pub fn new() -> Self {
        Self::default()
    }

    /// Open a subscription to every `feed` for every site in `sites`, drop the
    /// rest, and retry anything that is due.
    ///
    /// Called every frame, which is what makes reconnection unconditional: there
    /// is no separate retry task to stall, and a socket that dropped is simply a
    /// key that is missing again on the next pass.
    ///
    /// `feeds` is narrowed rather than the whole call being skipped when the live
    /// chunk feed is off — see [`Feed::Archive`], which is worth pushing exactly
    /// when chunks are not.
    ///
    /// `wake` is called from the socket's own thread on every event, so the frame
    /// loop does not sleep through a notification.
    pub fn sync_sites(
        &mut self,
        sites: &[String],
        feeds: &[Feed],
        endpoint: &str,
        wake: impl Fn() + Send + Sync + Clone + 'static,
    ) {
        let wanted =
            |site: &String, feed: &Feed| sites.iter().any(|s| s == site) && feeds.contains(feed);
        self.subs.retain(|(site, feed), _| wanted(site, feed));
        self.backoff.retain(|(site, feed), _| wanted(site, feed));

        // A handshake that never resolves would otherwise be indistinguishable
        // from a healthy subscription: the loop below skips every key already in
        // `subs`, so without this "connecting" is a state a socket can never
        // leave and the site never reconnects.
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
    ///
    /// Reconnection runs from [`Self::sync_sites`], which only runs on a frame,
    /// so without a term of its own it would inherit whatever unrelated work
    /// happened to be keeping frames coming: turn auto-poll off with the socket
    /// down and it would never be retried.
    ///
    /// This half is an *unconditional* re-arm and stays one, because it is
    /// bounded: [`CONNECT_TIMEOUT`] is 30 s, after which `sync_sites` tears the
    /// socket down and the wait becomes a backoff, which is scheduled rather
    /// than spun on ([`Self::next_retry_delay`]). Thirty seconds of frames is
    /// worth pinning down eventually — nothing about a handshake needs the
    /// display's refresh rate — but it ends on its own, which is the property
    /// the backoff half did not have.
    pub fn handshake_pending(&self) -> bool {
        self.subs.values().any(|s| s.state == LinkState::Connecting)
    }

    /// How long until some subscription's backoff is up, or `None` when none is
    /// waiting one out.
    ///
    /// This used to be half of a boolean the frame loop re-armed on
    /// unconditionally, and that was the app's last permanent spinner. The
    /// backoff doubles from 5 s to a 300 s ceiling and *never gives up* — by
    /// design, since a service that is down for an hour is the case it exists
    /// to survive — so for anyone who cannot reach the notifier at all
    /// (offline, a restrictive network, the service down) `backoff` is
    /// non-empty for the entire session. Re-arming a redraw on that is the
    /// same defect the auto-poll re-arm had, for a whole class of users: five
    /// minutes of drawing at the display's refresh rate to make one connection
    /// attempt.
    ///
    /// The retry itself is unchanged. `sync_sites` still decides what is due;
    /// this only says when to bring it a frame.
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
        // process default, and this is the call that installs it. Cheap and
        // idempotent; called here so a session that somehow reaches a socket
        // before any S3 request still has one.
        rustdar_radar::tls::init();

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
        let shift = failures.saturating_sub(1).min(6);
        let delay = (RECONNECT_BASE * (1 << shift)).min(RECONNECT_MAX);
        self.backoff.insert(
            (site.to_string(), feed),
            (failures, web_time::Instant::now() + delay),
        );
    }

    /// Take everything the sockets have said since the last frame.
    ///
    /// Unparseable messages are dropped rather than treated as failures: the
    /// service may grow fields or event kinds this build has never heard of, and
    /// the worst case for ignoring one is the five-second timer firing instead.
    pub fn drain(&mut self) -> Vec<Notified> {
        let mut out = Vec::new();
        let mut dropped: Vec<((String, Feed), u32)> = Vec::new();

        for ((site, feed), sub) in &mut self.subs {
            while let Some(event) = sub.receiver.try_recv() {
                match event {
                    WsEvent::Opened => {
                        log::info!("{site}: {feed:?} notifications connected");
                        sub.state = LinkState::Open;
                        sub.failures = 0;
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
                        sub.state = LinkState::Down;
                        dropped.push(((site.clone(), *feed), sub.failures + 1));
                        break;
                    }
                    WsEvent::Closed => {
                        log::info!("{site}: {feed:?} notification socket closed");
                        sub.state = LinkState::Down;
                        dropped.push(((site.clone(), *feed), sub.failures + 1));
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
    #[cfg(test)]
    pub(crate) fn backdate_handshake(&mut self, site: &str, feed: Feed, by: std::time::Duration) {
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
        assert_eq!(id.kind(), rustdar_radar::chunks::ChunkKind::Intermediate);
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
    ///
    /// The endpoint is unreachable on purpose: what is under test is the
    /// bookkeeping either way, and every site must end up accounted for — either
    /// subscribed or waiting out a retry, never silently dropped.
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
    ///
    /// `sync_sites` skips every key already in `subs`, so a socket that neither
    /// opens nor fails — a black-holed handshake, a gateway that accepts the
    /// connection and says nothing — would otherwise occupy its slot forever and
    /// the site would never reconnect. Kills a mutation that drops the
    /// `CONNECT_TIMEOUT` sweep.
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
    ///
    /// They were one boolean, re-armed on unconditionally. The halves behave
    /// nothing alike: a handshake resolves or times out inside
    /// [`CONNECT_TIMEOUT`], while a backoff doubles to a five-minute ceiling
    /// and never gives up — so on a machine that cannot reach the notifier at
    /// all, that boolean was true for the entire session and the app drew at
    /// the display's refresh rate for as long as it ran. Hence a *duration*
    /// for the backoff, which the loop sleeps through.
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
