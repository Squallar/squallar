//! Where the fetched network catalogue is kept between launches.
//!
//! [`rustdar_radar::catalogue`] decides *what* the catalogue is; this decides
//! where it lives on each platform and when it is read and written. The split
//! is the one [`crate::site_positions`] already makes and for the same reason:
//! [`ConfigStore`] is a `rustdar-egui` type, and `rustdar-radar` must not learn
//! that this crate exists.
//!
//! # The cache is what the app actually runs on
//!
//! The requirement above everything else here is that resolution happens
//! **before the first paint** — a name or a position arriving late adds a map
//! marker, adds a site-list row, and shifts a section's height datum under a
//! user who is already looking at them, and reopening has to be exactly 1:1.
//!
//! A network fetch cannot meet that. So it does not try: the cache is read
//! synchronously in `App::new`, beside the learned positions and ahead of any
//! frame, and *that* is what resolves the table. The fetch runs detached
//! afterwards, writes the cache, and is applied on the **next** launch, which
//! is why it has no deadline and no bearing on startup: it can take a minute,
//! or never finish, and the app is identical.
//!
//! # The one exception, and what makes it safe
//!
//! A launch that read *no* catalogue applies the first one it fetches in that
//! same session — `App::catalogue_pending` and
//! `App::adopt_the_first_catalogue`. Without it a launch with nothing cached
//! shows a site list of only the radars this install has decoded, and the
//! network appears a launch later; on the web, where a reload is the relaunch
//! nobody thinks to perform, that read as "there is one radar".
//!
//! It does not weaken the rule above. Every row present before that first
//! catalogue came from a learned position, `SiteFix::Learned` outranks
//! `Network`, and `sites::extended` settles rank before it builds a row — so
//! the first catalogue can only *add* rows. Nothing a user is looking at
//! moves. Every catalogue after it takes the next-launch path.
//!
//! # Its own key, written when the fetch lands
//!
//! Not `ui.json`. `App::autosave_config` writes that on a 3 s timer behind a
//! string compare, and everything the user has configured rides in the one
//! blob: a catalogue landing in the last three seconds of a session would be
//! lost, and — much worse — one unreadable value in that blob costs *every*
//! setting on the next load. Its own key bounds the blast radius of a corrupt
//! entry to this map, whereupon the app degrades to the seed and nothing else
//! notices.
//!
//! Integers throughout, for the reason [`rustdar_radar::site_position`] gives:
//! `serde_json` writes a non-finite float as `null` and `null` then fails to
//! deserialize on the *next* load, so one bad `f64` costs the whole record a
//! run after the bug. Every field of a [`CataloguePosition`] is an `i32`, so
//! there is nothing to filter and nothing to remember to filter.
//!
//! No TTL. A published station position is a step function that steps once a
//! decade, and the fetch already refreshes the cache on every launch that has a
//! network — so a TTL could only ever throw away a good answer for a worse one.

use rustdar_egui::config_store::ConfigStore;
use rustdar_radar::catalogue::SiteCatalogue;

/// Key the fetched catalogue is persisted under.
pub const SITE_CATALOGUE_KEY: &str = "site_catalogue";

/// How many radars a stored catalogue may carry.
///
/// The live listing is ~210. This is four times that: reaching it means the
/// stored blob is junk rather than that the network grew, and the cap exists so
/// a store that somehow accumulates entries cannot grow without bound in a
/// browser's `localStorage`, where the whole origin shares a few megabytes.
const MAX_CATALOGUE_SITES: usize = 800;

/// Read the cached catalogue, or an empty one.
///
/// **Called before the first volume is decoded and before the first frame** —
/// see the module note. An unreadable or implausible blob is logged and
/// dropped rather than propagated, and an empty result is not a degraded mode:
/// it is what arms `App::catalogue_pending`, so this session adopts the
/// catalogue it fetches rather than waiting for the next launch.
pub fn load(store: Option<&dyn ConfigStore>) -> SiteCatalogue {
    let Some(raw) = store.and_then(|store| store.load(SITE_CATALOGUE_KEY)) else {
        return SiteCatalogue::default();
    };
    match serde_json::from_str::<SiteCatalogue>(&raw) {
        Ok(catalogue) if catalogue.len() > MAX_CATALOGUE_SITES => {
            log::warn!(
                "ignoring a cached site catalogue of {} radars: the live \
                 network is ~210, and {MAX_CATALOGUE_SITES} is four times it",
                catalogue.len(),
            );
            SiteCatalogue::default()
        }
        Ok(catalogue) => catalogue,
        // Worth saying rather than silently starting over: until the next
        // fetch lands, this install is back on the compiled-in seed.
        Err(e) => {
            log::warn!("ignoring an unreadable cached site catalogue: {e}");
            SiteCatalogue::default()
        }
    }
}

/// Hand `catalogue` to the store **when it lands**, replacing whatever is there.
///
/// Returns whether a write was attempted, which is also whether anything
/// changed: a fetch that comes back with the catalogue already cached is the
/// ordinary case, and rewriting the same ~15 KB blob into `localStorage` on
/// every launch for a value that has not moved is a cost with no benefit.
///
/// Handed over when the fetch lands rather than deferred to the autosave tick,
/// for the reason in the module note — which is about *this key* not riding the
/// 3 s `UiConfig` blob, and is unaffected by the store queuing the bytes. This
/// is reached from a frame, so it takes the deferred `store` and a process that
/// dies moments later loses the catalogue. That costs one more launch on the
/// seed, which is why this is not one of the callers that pays for `store_now`.
///
/// A failed write is logged and dropped — a full `localStorage` must not stop
/// the map from working.
///
/// The return value says nothing about whether the caller should *use* the
/// catalogue: it did once, and a store that could not be written then discarded
/// a catalogue already in hand. See `App::poll_site_catalogue`.
pub fn store_if_changed(
    store: Option<&dyn ConfigStore>,
    cached: &SiteCatalogue,
    fetched: &SiteCatalogue,
) -> bool {
    if cached == fetched {
        return false;
    }
    let Some(store) = store else {
        return false;
    };
    // Cannot fail — every field is an `i32` or a `String`, which is the whole
    // reason they are. Handled rather than unwrapped because a panic here
    // would be a panic in a frame.
    let json = match serde_json::to_string(fetched) {
        Ok(json) => json,
        Err(e) => {
            log::warn!("could not serialize the fetched site catalogue: {e}");
            return false;
        }
    };
    if let Err(e) = store.store(SITE_CATALOGUE_KEY, &json) {
        log::warn!("could not persist the fetched site catalogue: {e}");
        return false;
    }
    // Says only what this function did. It used to add "applied on the next
    // launch", which the launch that adopts its first catalogue contradicts on
    // the very next line of the log. "Queued" is the same discipline one step
    // further: `store` hands the bytes to a writer thread and returns before
    // any of them are on disk, so "cached" would now be claiming an outcome
    // this function has not observed and will never be told about.
    log::info!(
        "site catalogue queued for caching: {} radars",
        fetched.len()
    );
    true
}

#[cfg(test)]
mod tests;
