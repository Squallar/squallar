//! Where the fetched network catalogue is kept between launches.
//!
//! The cache is read synchronously in `App::new`, ahead of any frame, and that
//! is what resolves the table. The fetch runs detached afterwards, writes the
//! cache, and is applied on the next launch — except a launch that read no
//! catalogue, which adopts the first one it fetches in that same session.
//!
//! Integers throughout, for the reason [`squallar_radar::site_position`] gives:
//! `serde_json` writes a non-finite float as `null` and `null` then fails to
//! deserialize on the next load.

use squallar_kv::KvStore;
use squallar_radar::catalogue::SiteCatalogue;

pub const SITE_CATALOGUE_KEY: &str = "site_catalogue";

/// How many radars a stored catalogue may carry.
///
/// The live listing is ~210; four times that means the stored blob is junk.
const MAX_CATALOGUE_SITES: usize = 800;

/// Read the cached catalogue, or an empty one.
///
/// Called before the first volume is decoded and before the first frame. An
/// empty result is what arms `App::catalogue_pending`.
pub fn load(store: Option<&dyn KvStore>) -> SiteCatalogue {
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
        Err(e) => {
            log::warn!("ignoring an unreadable cached site catalogue: {e}");
            SiteCatalogue::default()
        }
    }
}

/// Hand `catalogue` to the store when it lands, replacing whatever is there.
///
/// Returns whether a write was attempted, which is also whether anything
/// changed. A failed write is logged and dropped.
pub fn store_if_changed(
    store: Option<&dyn KvStore>,
    cached: &SiteCatalogue,
    fetched: &SiteCatalogue,
) -> bool {
    if cached == fetched {
        return false;
    }
    let Some(store) = store else {
        return false;
    };
    // Cannot fail — every field is an `i32` or a `String`.
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
    log::info!(
        "site catalogue queued for caching: {} radars",
        fetched.len()
    );
    true
}

#[cfg(test)]
mod tests;
