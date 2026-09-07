//! **A latest the chunk landing files for a site no pane watches live is a
//! volume nothing else holds — and `still scans` names it.**
//!
//! This is the arm the plain `resident_scan_bytes` could never move on: the
//! archive drain files its latest beside the merge base (one `Arc`, free
//! under de-duplication), but the chunk landing returns before either store
//! is touched, so its latest is the cache's alone. Before the third store was
//! folded into the level, this volume was priced by no family at all.

use super::super::App;
use super::super::tests::headless;
use super::volume_close_tests::closing_round;
use crate::platform_double::TestBridge;

const SITE: &str = "KTLX";

/// A pane on [`SITE`] that is parked, not live, so the landing takes the
/// latest-cache arm.
fn app_parked_on_site() -> App {
    let mut app = headless(TestBridge::desktop());
    let pane = app.gui.pane_mut(0).expect("a headless app has a pane");
    pane.set_site(SITE.to_string());
    pane.viewing_live = false;
    app
}

#[test]
fn a_latest_nothing_else_holds_adds_exactly_its_own_bytes_to_the_still_level() {
    let mut app = app_parked_on_site();
    assert!(
        !app.any_pane_live_for_site(SITE),
        "fixture: a live pane would put the volume on screen, not in the cache",
    );

    app.apply_chunk_outcome(SITE, &closing_round(5));

    let (latest, _, _, _) = app
        .latest_cached_scans
        .get(SITE)
        .expect("a whole closed volume for a parked site is filed as its latest");
    assert!(
        app.volumes.base_for(SITE).is_none() && app.volumes.newest_still_for(SITE).is_none(),
        "fixture: the landing also installed the volume in the inventory, so \
         this is the shared case and not the sole-holder one",
    );
    let one = squallar_radar::scan_size::scan_bytes(latest) as u64;
    assert!(one > 0, "fixture: a volume priced at nothing");

    assert_eq!(
        app.volumes.latest_price(SITE) as u64,
        one,
        "the landing filed the latest without pricing it",
    );
    assert_eq!(
        app.volumes.resident_scan_bytes(),
        0,
        "fixture: the inventory's own stores must be empty for the arithmetic below",
    );
    assert_eq!(
        app.still_scan_level(),
        one,
        "a latest nothing else holds was priced by no family: the level reads \
         the two inventory stores alone",
    );
}

/// **The row leaves with the entry.** `evict_unshown_scans` empties the cache
/// of a site no pane shows and the price row goes with it, so a later latest
/// for the same site cannot be read at a stale price.
#[test]
fn evicting_the_latest_forgets_its_price() {
    let mut app = app_parked_on_site();
    app.apply_chunk_outcome(SITE, &closing_round(5));
    assert!(app.volumes.latest_price(SITE) > 0, "precondition: priced");

    // Move the pane off the site: KTLX is now unshown.
    app.gui
        .pane_mut(0)
        .expect("a pane")
        .set_site("KOUN".to_string());
    app.evict_unshown_scans();

    assert!(
        !app.latest_cached_scans.contains_key(SITE),
        "precondition: the eviction emptied the cache of the unshown site",
    );
    assert_eq!(
        app.volumes.latest_price(SITE),
        0,
        "the evicted site's price row was left behind",
    );
    assert_eq!(app.still_scan_level(), 0);
}
