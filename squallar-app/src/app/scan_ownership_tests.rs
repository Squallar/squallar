//! **What one arriving volume is owned by, and what the inventory's figure
//! says about it** — over the real archive drain, not a hand-built store.
//!
//! One arrival puts ONE `Arc<Scan>` into three long-lived holders:
//! `VolumeInventory::base`, `VolumeInventory::still` and the loop download
//! cache. `resident_scan_bytes` used to charge the first two separately, so
//! the census's `still scans` family read about double what the inventory
//! held; and because the third holder keeps the volume a pane is parked on
//! (`App::evict_unneeded_loop_scans`' `parked` fallback), emptying the
//! inventory would free none of it.
//!
//! Everything here drives `App::poll_data_channels`, so the ownership it
//! asserts is the ownership the arrival path actually builds — a store filled
//! by hand cannot show which allocations a caller shares.

use super::tests::headless;
use super::*;
use crate::platform_double::TestBridge;

const SITE: &str = "KTLX";
const OTHER: &str = "KOUN";

fn at(minute: u32) -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 9, 7)
        .expect("a real date")
        .and_hms_opt(18, minute, 0)
        .expect("a real time")
}

/// A headless app with one pane on [`SITE`], watching live.
fn app_on_site() -> App {
    let mut app = headless(TestBridge::desktop());
    let pane = app.gui.pane_mut(0).expect("a headless app has a pane");
    pane.set_site(SITE.to_string());
    pane.viewing_live = true;
    app
}

/// The fixture volume, owned — `ready_scan` hands out an `Arc` and the
/// arrival channel carries the `Scan` itself.
///
/// A volume with real radials on purpose: `scan_bytes` of a volume with no
/// gates is the containers alone, and an arithmetic about double-charging
/// cannot be read off a figure that is nearly zero.
fn arriving_volume() -> nexrad_model::data::Scan {
    Arc::try_unwrap(crate::volume_fixture::ready_scan())
        .unwrap_or_else(|_| unreachable!("ready_scan hands out a fresh Arc"))
}

/// Push one archive volume down the real channel and drain it.
fn land_one_archive_volume(app: &mut App, site: &str, timestamp: chrono::NaiveDateTime) {
    let generation = app.render.fetch_generation_for(site);
    app.channels
        .scan_sender
        .send(crate::channels::ScanResponse {
            generation,
            site: site.to_string(),
            requester: crate::channels::FetchRequester::Site,
            result: Ok(crate::channels::ScanData {
                scan: arriving_volume(),
                declared_nyquist: Default::default(),
                site: site.to_string(),
                timestamp,
            }),
            is_auto_poll: false,
        })
        .expect("the app holds the receiver");
    app.poll_data_channels();
}

/// **One arrival is one allocation in both of the inventory's stores.**
///
/// The premise everything else here rests on, and the one a store filled by
/// hand cannot establish: `two_volumes()` in `volume_inventory`'s own tests
/// builds a distinct `Scan` per call, so no test in that module ever put one
/// allocation in both stores.
#[test]
fn one_arrival_puts_one_allocation_in_both_stores() {
    let mut app = app_on_site();
    land_one_archive_volume(&mut app, SITE, at(0));

    let at_moment = app
        .volumes
        .newest_still_for(SITE)
        .expect("the arrival installed a still");
    let (still, _) = app
        .volumes
        .still_for(SITE, at_moment)
        .expect("the still is resident");
    let (base, _) = app.volumes.base_for(SITE).expect("the base is resident");

    assert!(
        Arc::ptr_eq(&still, &base),
        "the arrival path installed two DIFFERENT volumes, so nothing below \
         is about de-duplication",
    );
}

/// **The volume both stores hold is charged once.**
///
/// Before the de-duplication this read `2 * one` on this exact scene — the
/// figure the census publishes as `still scans`, and one of the largest
/// entries in it.
#[test]
fn the_inventory_charges_one_arrival_once_and_not_twice() {
    let mut app = app_on_site();
    land_one_archive_volume(&mut app, SITE, at(0));

    let (volume, _) = app.volumes.base_for(SITE).expect("the base is resident");
    let one = squallar_radar::scan_size::scan_bytes(&volume);
    assert!(
        one > 0,
        "fixture: a volume priced at nothing cannot show a double charge",
    );

    assert_eq!(
        app.volumes.resident_scan_bytes(),
        one,
        "one allocation in two stores was charged twice",
    );
    assert_ne!(
        app.volumes.resident_scan_bytes(),
        2 * one,
        "the figure is still the plain sum of the two stores",
    );
}

/// **Emptying the inventory would free nothing**, because the loop download
/// cache holds the same allocation.
///
/// This is what the census's shared-ownership convention means in practice
/// and it is the reason a smaller `still scans` is not a smaller heap:
/// `App::append_scan_to_active_loops` files every arrival in the loop cache
/// unconditionally, and `evict_unneeded_loop_scans` keeps whatever a pane is
/// parked at even on a site with no Level II loop.
#[test]
fn the_loop_cache_holds_the_same_allocation_the_inventory_does() {
    let mut app = app_on_site();
    land_one_archive_volume(&mut app, SITE, at(0));

    let (base, _) = app.volumes.base_for(SITE).expect("the base is resident");
    // **The loop cache is keyed by the RESPONSE's timestamp**, which is what
    // `append_scan_to_active_loops` is handed, while the still store is keyed
    // by the volume's own first-radial moment (`scan_info.timestamp`). One
    // volume, two keys; this looks it up by the key its writer used.
    let (cached, _) = app
        .loop_mgr
        .get_cached(SITE, &at(0))
        .expect("every arrival is filed in the loop download cache");

    assert!(
        Arc::ptr_eq(&base, cached),
        "the loop cache holds a different volume, so the inventory's stores \
         may be its sole owners after all",
    );
    // Three long-lived owners — base, still, loop cache — plus the one clone
    // this test is holding.
    assert!(
        Arc::strong_count(&base) >= 4,
        "one arrival should be reachable from the base, the still and the \
         loop cache; the count is {}",
        Arc::strong_count(&base),
    );
}

/// **Two sites' arrivals are two volumes, and the figure says two.**
///
/// The control for the de-duplication: it collapses equal ALLOCATIONS, and a
/// rule that collapsed a whole store, or every entry sharing a key shape,
/// would read one volume here.
#[test]
fn two_sites_arrivals_are_two_volumes_on_the_figure() {
    let mut app = app_on_site();
    land_one_archive_volume(&mut app, SITE, at(0));
    land_one_archive_volume(&mut app, OTHER, at(0));

    let (first, _) = app.volumes.base_for(SITE).expect("KTLX has a base");
    let (second, _) = app.volumes.base_for(OTHER).expect("KOUN has a base");
    assert!(
        !Arc::ptr_eq(&first, &second),
        "fixture: the two sites landed one allocation, so there is only one \
         volume to count",
    );
    let one = squallar_radar::scan_size::scan_bytes(&first);
    assert_eq!(
        squallar_radar::scan_size::scan_bytes(&second),
        one,
        "fixture: the two arrivals must price the same for the multiple below",
    );

    assert_eq!(
        app.volumes.resident_scan_bytes(),
        2 * one,
        "two distinct arrivals were collapsed into one charge",
    );
}
