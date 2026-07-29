//! Push notification of new real-time chunks, over a WebSocket.
//!
//! The chunk feed's latency is bound by its poll interval, not by the bucket —
//! measured at a median 4 s against a 5 s interval. `nexrad-aws-notifier` bridges
//! the NEXRAD SNS topic to a per-station WebSocket, so a chunk can be fetched the
//! moment it exists instead of on the next tick.
//!
//! # This nudges the poller; it does not replace it
//!
//! A notification marks a site **due for a round now**, and the ordinary
//! [`rustdar_radar::chunks::ChunkPoller`] does the rest — listing, selecting,
//! downloading, assembling. Nothing about the polling path changes.
//!
//! That is what makes degradation free rather than a feature. If the service is
//! unreachable, the socket drops, the endpoint is wrong, or the user is on a
//! network that blocks it, no notifications arrive and the five-second timer
//! fires exactly as it does today. There is no fallback path to get right
//! because there is no second path — only an early wake-up that may or may not
//! come.
//!
//! It also leaves the traffic win on the table: each round still lists the
//! volume directory. Skipping that needs the volume's `YYYYMMDD-HHMMSS` prefix,
//! which the notification does not carry (it has the station, the rotating volume
//! index, the sequence and the type), so a listing is still needed once per
//! volume to learn it. That is a later increment.
//!
//! # No async
//!
//! `ewebsock` hands back a `WsReceiver` with a non-blocking `try_recv`, and its
//! reader lives on its own thread natively and on the browser's event loop on
//! web. So this drains once a frame like every other channel in this crate, with
//! no executor and no `MaybeSend` gymnastics.

use std::collections::HashMap;

use ewebsock::{WsEvent, WsMessage, WsReceiver, WsSender};
use rustdar_radar::chunks::{ChunkKind, VolumeIndex};

/// Backoff after a failed or dropped connection, doubling to a ceiling.
const RECONNECT_BASE: std::time::Duration = std::time::Duration::from_secs(5);
const RECONNECT_MAX: std::time::Duration = std::time::Duration::from_secs(300);

/// A chunk the service says now exists.
///
/// Not enough to build the object key on its own — that also needs the volume's
/// start time, which is in the name and not in the message — so this is a
/// *nudge*, not a fetch instruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkAvailable {
    pub site: String,
    pub volume: VolumeIndex,
    pub sequence: u16,
    pub kind: ChunkKind,
}

/// One notification message, exactly as the service sends it.
///
/// `volume` and `chunk` arrive as strings rather than numbers, so they are taken
/// as such and parsed here.
#[derive(serde::Deserialize)]
struct Notification {
    station: String,
    volume: String,
    chunk: String,
    #[serde(rename = "chunkType")]
    chunk_type: String,
}

impl Notification {
    fn into_available(self) -> Option<ChunkAvailable> {
        Some(ChunkAvailable {
            volume: VolumeIndex::new(self.volume.parse().ok()?)?,
            sequence: self.chunk.parse().ok()?,
            kind: match self.chunk_type.as_str() {
                "S" => ChunkKind::Start,
                "I" => ChunkKind::Intermediate,
                "E" => ChunkKind::End,
                _ => return None,
            },
            site: self.station,
        })
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
}

/// Per-site subscriptions to the notifier service.
#[derive(Default)]
pub struct ChunkNotifier {
    subs: HashMap<String, Subscription>,
    /// Sites whose subscription is waiting out a backoff, kept out of `subs` so
    /// a dead socket is not held open.
    backoff: HashMap<String, (u32, web_time::Instant)>,
}

impl ChunkNotifier {
    pub fn new() -> Self {
        Self::default()
    }

    /// Open a subscription for every site in `sites`, drop the rest.
    ///
    /// `wake` is called from the socket's own thread on every event, so the frame
    /// loop does not sleep through a notification.
    pub fn sync_sites(
        &mut self,
        sites: &[String],
        endpoint: &str,
        wake: impl Fn() + Send + Sync + Clone + 'static,
    ) {
        self.subs.retain(|site, _| sites.iter().any(|s| s == site));
        self.backoff
            .retain(|site, _| sites.iter().any(|s| s == site));

        let now = web_time::Instant::now();
        for site in sites {
            if self.subs.contains_key(site) {
                continue;
            }
            if let Some((_, retry_at)) = self.backoff.get(site)
                && now < *retry_at
            {
                continue;
            }
            let failures = self.backoff.remove(site).map(|(n, _)| n).unwrap_or(0);
            self.connect(site, endpoint, failures, wake.clone());
        }
    }

    fn connect(
        &mut self,
        site: &str,
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
            "{}/ws/events/nexrad-chunk/{site}",
            endpoint.trim_end_matches('/')
        );
        match ewebsock::connect_with_wakeup(url.clone(), ewebsock::Options::default(), wake) {
            Ok((sender, receiver)) => {
                log::info!("{site}: subscribing to chunk notifications at {url}");
                self.subs.insert(
                    site.to_string(),
                    Subscription {
                        _sender: sender,
                        receiver,
                        state: LinkState::Connecting,
                        failures,
                    },
                );
            }
            Err(e) => {
                log::warn!("{site}: could not open a notification socket: {e}");
                self.schedule_retry(site, failures + 1);
            }
        }
    }

    fn schedule_retry(&mut self, site: &str, failures: u32) {
        let shift = failures.saturating_sub(1).min(6);
        let delay = (RECONNECT_BASE * (1 << shift)).min(RECONNECT_MAX);
        self.backoff.insert(
            site.to_string(),
            (failures, web_time::Instant::now() + delay),
        );
    }

    /// Take everything the sockets have said since the last frame.
    ///
    /// Unparseable messages are dropped rather than treated as failures: the
    /// service may grow fields or event kinds this build has never heard of, and
    /// the worst case for ignoring one is the five-second timer firing instead.
    pub fn drain(&mut self) -> Vec<ChunkAvailable> {
        let mut out = Vec::new();
        let mut dropped: Vec<(String, u32)> = Vec::new();

        for (site, sub) in &mut self.subs {
            while let Some(event) = sub.receiver.try_recv() {
                match event {
                    WsEvent::Opened => {
                        log::info!("{site}: chunk notifications connected");
                        sub.state = LinkState::Open;
                        sub.failures = 0;
                    }
                    WsEvent::Message(WsMessage::Text(text)) => {
                        match serde_json::from_str::<Notification>(&text)
                            .ok()
                            .and_then(Notification::into_available)
                        {
                            Some(available) => out.push(available),
                            None => log::debug!("{site}: ignoring notification {text}"),
                        }
                    }
                    // Pings, pongs and binary frames are not part of this
                    // protocol; the transport handles keepalive itself.
                    WsEvent::Message(_) => {}
                    WsEvent::Error(e) => {
                        log::warn!("{site}: notification socket error: {e}");
                        sub.state = LinkState::Down;
                        dropped.push((site.clone(), sub.failures + 1));
                        break;
                    }
                    WsEvent::Closed => {
                        log::info!("{site}: notification socket closed");
                        sub.state = LinkState::Down;
                        dropped.push((site.clone(), sub.failures + 1));
                        break;
                    }
                }
            }
        }

        for (site, failures) in dropped {
            self.subs.remove(&site);
            self.schedule_retry(&site, failures);
        }
        out
    }

    /// Whether any site's socket is currently open, for the status bar.
    pub fn any_open(&self) -> bool {
        self.subs.values().any(|s| s.state == LinkState::Open)
    }

    pub fn state_for(&self, site: &str) -> LinkState {
        match self.subs.get(site) {
            Some(sub) => sub.state,
            None => LinkState::Down,
        }
    }

    #[cfg(test)]
    pub(crate) fn subscription_count(&self) -> usize {
        self.subs.len()
    }

    #[cfg(test)]
    pub(crate) fn is_backing_off(&self, site: &str) -> bool {
        self.backoff.contains_key(site)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> Option<ChunkAvailable> {
        serde_json::from_str::<Notification>(json)
            .ok()
            .and_then(Notification::into_available)
    }

    /// The service's own documented shape, with `volume` and `chunk` as strings.
    #[test]
    fn a_notification_parses_into_a_chunk_identity() {
        let got = parse(
            r#"{"station":"KJAX","volume":"415","chunk":"25","chunkType":"I","l2Version":"V06"}"#,
        )
        .expect("parses");
        assert_eq!(
            got,
            ChunkAvailable {
                site: "KJAX".to_string(),
                volume: VolumeIndex::new(415).unwrap(),
                sequence: 25,
                kind: ChunkKind::Intermediate,
            }
        );
    }

    #[test]
    fn every_chunk_type_letter_is_understood() {
        for (letter, want) in [
            ("S", ChunkKind::Start),
            ("I", ChunkKind::Intermediate),
            ("E", ChunkKind::End),
        ] {
            let json =
                format!(r#"{{"station":"KTLX","volume":"1","chunk":"1","chunkType":"{letter}"}}"#);
            assert_eq!(parse(&json).expect("parses").kind, want);
        }
    }

    /// A message this build cannot read is dropped, not fatal. The service may
    /// grow fields or event kinds, and the cost of ignoring one is the ordinary
    /// five-second timer firing instead.
    #[test]
    fn an_unreadable_notification_is_dropped_rather_than_fatal() {
        for bad in [
            "",
            "not json",
            r#"{"station":"KTLX"}"#,
            r#"{"station":"KTLX","volume":"0","chunk":"1","chunkType":"I"}"#, // outside 1..=999
            r#"{"station":"KTLX","volume":"abc","chunk":"1","chunkType":"I"}"#,
            r#"{"station":"KTLX","volume":"1","chunk":"1","chunkType":"X"}"#,
        ] {
            assert!(parse(bad).is_none(), "{bad:?} should not parse");
        }
    }

    /// Extra fields are ignored, so the service can add them without a client
    /// release.
    #[test]
    fn unknown_fields_do_not_break_a_notification() {
        assert!(
            parse(
                r#"{"station":"KTLX","volume":"1","chunk":"1","chunkType":"I","somethingNew":42}"#
            )
            .is_some()
        );
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
        n.sync_sites(&sites, "wss://127.0.0.1:1", || {});
        for site in &sites {
            assert!(
                n.state_for(site) != LinkState::Down || n.is_backing_off(site),
                "{site} is neither subscribed nor scheduled to retry"
            );
        }

        n.sync_sites(&[], "wss://127.0.0.1:1", || {});
        assert_eq!(n.subscription_count(), 0, "a socket outlived its site");
        assert!(!n.is_backing_off("KTLX"), "backoff outlived the site");
    }

    /// Re-syncing the same sites does not churn their sockets — otherwise every
    /// frame would tear down and rebuild every connection.
    #[test]
    fn re_syncing_the_same_sites_keeps_their_sockets() {
        let mut n = ChunkNotifier::new();
        let sites = ["KTLX".to_string()];
        n.sync_sites(&sites, "wss://127.0.0.1:1", || {});
        let before = n.subscription_count();
        for _ in 0..5 {
            n.sync_sites(&sites, "wss://127.0.0.1:1", || {});
        }
        assert_eq!(n.subscription_count(), before);
    }

    /// A site with no subscription reports `Down`, which is what makes the
    /// status bar say "polling" rather than claiming a link it does not have.
    #[test]
    fn an_unsubscribed_site_reports_down() {
        let n = ChunkNotifier::new();
        assert_eq!(n.state_for("KTLX"), LinkState::Down);
        assert!(!n.any_open());
    }
}
