//! What earlier volumes taught this install about where the radars are.
//!
//! `rustdar_radar::types::ScanInfo::from_scan` prefers a volume's own stated
//! position over the compiled-in table. The position learned from a volume is
//! handed to the config store the moment it is learned and handed back to
//! `from_scan` on the next run, so a site stays corrected across runs.
//!
//! Its own key, not a `UiConfig` field, so a corrupt entry costs this map and
//! not every setting. No TTL: a reported position is a step function that
//! steps once a decade.
//!
//! Conflict policy: the fresh volume wins, unconditionally. A vote cannot
//! represent a relocation, which is the `RKSG` failure that prompted this.
//! Pinned by `a_fresh_volume_wins_and_a_repeat_writes_nothing`.

use rustdar_kv::KvStore;
use rustdar_radar::site_position::SitePosition;
use rustdar_radar::sites::SiteFix;
use std::collections::BTreeMap;

pub const SITE_POSITIONS_KEY: &str = "site_positions";

/// How many sites may be remembered.
///
/// The WSR-88D and TDWR networks together are 207 radars; four times that
/// bounds a store accumulating junk keys in a browser's `localStorage`.
const MAX_REMEMBERED_SITES: usize = 800;

/// Every position this install has learned, by ICAO.
///
/// A `BTreeMap` so the serialized form is stable and byte-comparable.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SitePositions {
    known: BTreeMap<String, SitePosition>,
}

impl SitePositions {
/// Read what is remembered, or start with nothing.
///
/// Called before the first volume is decoded, which is the ordering the
/// 1:1-on-reopen rule needs: a learned position may only be applied before a
/// pane's first paint. An unreadable blob is logged and dropped.
    pub fn load(store: Option<&dyn KvStore>) -> Self {
        let Some(raw) = store.and_then(|store| store.load(SITE_POSITIONS_KEY)) else {
            return Self::default();
        };
        match serde_json::from_str::<BTreeMap<String, SitePosition>>(&raw) {
            Ok(known) => Self { known },
            // Worth saying rather than silently starting over: the entries
            // cannot be regenerated without re-downloading every site.
            Err(e) => {
                log::warn!("ignoring unreadable learned site positions: {e}");
                Self::default()
            }
        }
    }

    pub fn get(&self, site: &str) -> Option<SitePosition> {
        self.known.get(site).copied()
    }

    pub fn len(&self) -> usize {
        self.known.len()
    }

    pub fn is_empty(&self) -> bool {
        self.known.is_empty()
    }

/// Every remembered site, as `(ICAO, position)`.
    pub fn iter(&self) -> impl Iterator<Item = (&str, SitePosition)> {
        self.known
            .iter()
            .map(|(site, position)| (site.as_str(), *position))
    }

/// The same entries, labelled with the authority they carry, for
/// [`sites::resolve`](rustdar_radar::sites::resolve).
///
/// [`SiteFix::Learned`] because these came from a volume: they carry a Volume
/// Data Block's two separately-reported heights, and outrank the network.
    pub fn fixes(&self) -> impl Iterator<Item = (&str, SiteFix)> {
        self.iter()
            .map(|(site, position)| (site, SiteFix::Learned(position)))
    }

/// Remember what a volume just said, and write it out now.
///
/// Returns whether anything changed, which is also whether a write was
/// attempted. A failed write is logged and dropped.
    pub fn learn(
        &mut self,
        store: Option<&dyn KvStore>,
        site: &str,
        position: SitePosition,
    ) -> bool {
        if self.known.get(site) == Some(&position) {
            return false;
        }
        if !self.known.contains_key(site) && self.known.len() >= MAX_REMEMBERED_SITES {
            log::warn!(
                "not remembering {site}: {MAX_REMEMBERED_SITES} sites are already \
                 remembered, which is four times the real network",
            );
            return false;
        }
        self.known.insert(site.to_owned(), position);
        self.persist(store);
        true
    }

    fn persist(&self, store: Option<&dyn KvStore>) {
        let Some(store) = store else {
            return;
        };
        // Cannot fail — every field of a `SitePosition` is an `i32`.
        let json = match serde_json::to_string(&self.known) {
            Ok(json) => json,
            Err(e) => {
                log::warn!("could not serialize the learned site positions: {e}");
                return;
            }
        };
        if let Err(e) = store.store(SITE_POSITIONS_KEY, &json) {
            log::warn!("could not persist the learned site positions: {e}");
        }
    }
}

#[cfg(test)]
mod tests;
