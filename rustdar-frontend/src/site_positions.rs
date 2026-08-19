//! What earlier volumes taught this install about where the radars are.
//!
//! `rustdar_radar::types::ScanInfo::from_scan` prefers a volume's own stated
//! position over the compiled-in table, which makes every site the user opens
//! self-correcting *for as long as that volume is in memory*. This is what
//! makes it survive the process: the position learned from a volume is handed
//! to the config store the moment it is learned, and handed back to `from_scan`
//! on the next run, so a site stays corrected and the map centres correctly on
//! a radar the user opened last week and has not re-downloaded yet.
//!
//! Handed to, not written by. This is reached from applying a volume, which is
//! a frame, so it takes the store's ordinary deferred `store` and a process
//! that dies within milliseconds of learning a position can still lose it. That
//! is the right trade here and not at the two device memos: those are written
//! off a lost rendering surface, where the process dying next is the expected
//! case rather than a coincidence.
//!
//! # Why this is not a field on `UiConfig`
//!
//! Same reason as [`rustdar_location::LOCATION_MEMO_KEY`], which is
//! the precedent this follows. `App::autosave_config` writes the UI config on
//! a 3 s timer behind a string compare, and everything the user has configured
//! rides in that one blob. Two consequences, both bad: a position learned in
//! the last three seconds of a session is lost, and — much worse — a single
//! unreadable value in the blob costs *every setting* on the next load.
//!
//! Its own key removes both. The blast radius of a corrupt entry is this map,
//! the app degrades to the compiled-in table, and nothing else notices.
//!
//! # Why not the `zone_cache_dir` pattern
//!
//! `nws::zones` caches to a TTL'd directory, which is `None` on wasm — and
//! wasm is exactly where this matters most, because a browser tab has no
//! filesystem to fall back on and re-downloads everything on every visit. A
//! [`KvStore`] is the one persistence every platform has.
//!
//! There is also no TTL here, and that is deliberate: a reported position does
//! not go stale. It is a step function that steps once a decade, and the
//! entries are ~60 bytes each against a network of ~200 radars.
//!
//! # The conflict policy is "the fresh volume wins", unconditionally
//!
//! No averaging, no quorum, no "two out of three". Across 18 diverse sites at
//! 2019, 2022 and 2026 the reported position is bit-identical, span 0.0 m — so
//! a disagreement is not noise to be smoothed out, it is a re-survey or a
//! relocation, and the newer value is simply the right one. A vote would keep
//! a relocated radar at its old site for as many years as it stood there,
//! which is the `RKSG` failure that prompted all of this.
//!
//! **The 18-site, three-epoch reading is not reproducible from this tree.**
//! Nothing here fetches an archive, and no fixture holds those volumes. The
//! apparatus for it is `harness/sweep_site_epochs.sh` on branch
//! `campaign/site-position-probe`, which sweeps one Level II prefix per site
//! per epoch and exists precisely to read drift between epochs — but that
//! branch kept the scripts and not the output, so re-running produces a new
//! reading rather than confirming this one. Treat "bit-identical, span 0.0 m"
//! as a dated observation of eighteen sites, not as a property of the archive.
//!
//! The policy does not actually need it, which is the useful thing to know if
//! it is ever re-measured and comes back dirtier: "the fresh volume wins" is
//! chosen because a vote cannot represent a relocation at all, and that
//! argument holds however noisy the corpus turns out to be. The policy itself
//! is pinned in-tree by `a_fresh_volume_wins_and_a_repeat_writes_nothing`.

use rustdar_kv::KvStore;
use rustdar_radar::site_position::SitePosition;
use rustdar_radar::sites::SiteFix;
use std::collections::BTreeMap;

/// Key the learned positions are persisted under.
pub const SITE_POSITIONS_KEY: &str = "site_positions";

/// How many sites may be remembered.
///
/// The WSR-88D and TDWR networks together are 207 radars and this is not
/// expected to grow past that in any decade. The cap exists so that a store
/// which somehow accumulates junk keys cannot grow without bound in a
/// browser's `localStorage`, where the whole origin shares a few megabytes:
/// four times the real network, and reaching it means something is wrong
/// rather than that a user has been busy.
const MAX_REMEMBERED_SITES: usize = 800;

/// Every position this install has learned, by ICAO.
///
/// A `BTreeMap` rather than a `HashMap` so the serialized form is stable:
/// two runs that learn the same things write byte-identical JSON, which makes
/// the blob diffable by a human and comparable by a test.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SitePositions {
    known: BTreeMap<String, SitePosition>,
}

impl SitePositions {
    /// Read what is remembered, or start with nothing.
    ///
    /// **Called before the first volume is decoded**, which is the ordering
    /// the 1:1-on-reopen rule needs: a learned position may only be applied
    /// before a pane's first paint, never as a correction that shifts a pane
    /// the user is already looking at. Consulting it inside
    /// `ScanInfo::from_scan` — before the `ScanInfo` exists, let alone before
    /// it is drawn — is what keeps that true, and it only works if this has
    /// already loaded.
    ///
    /// An unreadable blob is logged and dropped rather than propagated. The
    /// cost is one session on the compiled-in table, which is where the app
    /// was before this existed.
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

    /// What was learned about `site`, if anything.
    pub fn get(&self, site: &str) -> Option<SitePosition> {
        self.known.get(site).copied()
    }

    /// How many sites are remembered.
    pub fn len(&self) -> usize {
        self.known.len()
    }

    /// Whether nothing is remembered.
    pub fn is_empty(&self) -> bool {
        self.known.is_empty()
    }

    /// Every remembered site, as `(ICAO, position)`.
    ///
    /// This is how a cache that lives in the *frontend* overlays a table that
    /// lives in `rustdar-radar`, without `rustdar-radar` learning that this
    /// crate exists. The radar crate publishes
    /// [`sites::resolve`](rustdar_radar::sites::resolve), which takes exactly
    /// this shape — a borrowed name beside a plain [`SitePosition`], both of
    /// them already `rustdar-radar`'s own vocabulary — and the frontend hands
    /// it what it loaded. The dependency still points one way; only data
    /// crosses.
    ///
    /// Borrowed rather than cloned because `resolve` keeps a name only for a
    /// site the compiled-in seed has never heard of, and usually there are
    /// none.
    pub fn iter(&self) -> impl Iterator<Item = (&str, SitePosition)> {
        self.known
            .iter()
            .map(|(site, position)| (site.as_str(), *position))
    }

    /// The same entries, labelled with the authority they carry.
    ///
    /// This is what [`sites::resolve`](rustdar_radar::sites::resolve) takes now
    /// that more than one source can speak. A caller chains this onto the
    /// fetched catalogue's fixes and hands the stream over as one; the label is
    /// what makes the order of that chain irrelevant, so neither source has to
    /// know the other exists.
    ///
    /// [`SiteFix::Learned`] and not a bare position because these came from a
    /// volume: they carry a Volume Data Block's two separately-reported
    /// heights, and outrank anything the network can say.
    pub fn fixes(&self) -> impl Iterator<Item = (&str, SiteFix)> {
        self.iter()
            .map(|(site, position)| (site, SiteFix::Learned(position)))
    }

    /// Remember what a volume just said, and write it out **now**.
    ///
    /// Returns whether anything changed, which is also whether a write was
    /// attempted: re-learning the same position on every volume of a session
    /// would otherwise mean a `localStorage` write every five minutes per
    /// pane for a value that has not moved.
    ///
    /// Synchronous, not deferred to the autosave tick, for the reason in the
    /// module note — and because the case that matters most is the one that
    /// ends the session: a user who opens a site and then closes the app is
    /// exactly who this is for.
    ///
    /// A failed write is logged and dropped. A full `localStorage` must not
    /// stop the map from working, and the cost of losing it is that the site
    /// is learned again the next time it is opened.
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
        // Cannot fail — every field of a `SitePosition` is an `i32`, which is
        // the whole reason it is one. Handled rather than unwrapped because a
        // panic here would be a panic in the middle of applying a volume.
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
