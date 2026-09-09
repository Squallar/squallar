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

/// Push one archive volume down the real channel and drain it — a Refresh or
/// scrub fetch, which the drain puts on screen.
fn land_one_archive_volume(app: &mut App, site: &str, timestamp: chrono::NaiveDateTime) {
    land_archive(app, site, timestamp, false);
}

/// The same volume arriving from the auto-poll, which the drain files in
/// `latest_cached_scans` for a site no pane is watching live.
fn land_one_auto_poll_volume(app: &mut App, site: &str, timestamp: chrono::NaiveDateTime) {
    land_archive(app, site, timestamp, true);
}

/// The chunk feed's shape: an assembled volume with no compressed half behind
/// it, which is the one arrival the archive trade cannot reach.
fn land_one_volume_with_no_archive(app: &mut App, site: &str, timestamp: chrono::NaiveDateTime) {
    land_archive_inner(app, site, timestamp, false, None);
}

/// **Hand the pane the picture the arrival owes it.**
///
/// `RenderDispatcher::reset_panes_for_site` clears `last_rendered` for every
/// pane on the site and the arrival path calls it before it files anything, so
/// a volume that has just landed is one no pane has drawn — and
/// `App::release_unneeded_base_gates` will not take a volume in that state,
/// because this pass runs ahead of `App::dispatch_pane_renders` in the frame
/// and the release would be undone by `ensure_base_whole` one dispatch later.
/// Every scene below that expects a release is therefore a scene where the
/// picture exists, which is what the application reaches a frame or two after
/// the arrival.
fn the_pane_has_its_picture(app: &mut App) {
    app.render.pane_render[0].last_rendered =
        Some((squallar_radar::types::RadarProduct::Reflectivity, 0.5));
}

fn land_archive(app: &mut App, site: &str, timestamp: chrono::NaiveDateTime, is_auto_poll: bool) {
    // The compressed half of the same arrival. It shares nothing with the
    // volume, so the allocation identities this module asserts are untouched
    // by it.
    land_archive_inner(
        app,
        site,
        timestamp,
        is_auto_poll,
        Some(std::sync::Arc::new(vec![0u8; 64])),
    );
}

fn land_archive_inner(
    app: &mut App,
    site: &str,
    timestamp: chrono::NaiveDateTime,
    is_auto_poll: bool,
    archive: Option<std::sync::Arc<Vec<u8>>>,
) {
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
                archive,
            }),
            is_auto_poll,
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

/// **The two clocks, and the volume the sweep evicted from under its pane.**
///
/// The archive drain files an arrival in the loop cache under the second it
/// was fetched by (the S3 key's clock), and puts the volume's own first radial
/// — to the millisecond — on the pane as `scan_info.timestamp`. Those differ
/// on essentially every real volume. `evict_unneeded_loop_scans` kept "what a
/// pane is parked at" by comparing the pane's clock against the cache key, so
/// the parked volume read as unwanted, left the loop cache, and the still
/// inventory became its sole owner — until the next loop downloaded it again.
///
/// Red on the tree before the identity fix; the precondition below is what
/// makes it about the two clocks rather than about eviction in general.
#[test]
fn a_parked_volume_survives_the_loop_sweep_however_the_archive_keyed_it() {
    let mut app = app_on_site();
    land_one_archive_volume(&mut app, SITE, at(0));

    let parked_at = app
        .gui
        .pane(0)
        .and_then(|p| p.scan_info.as_ref().map(|i| i.timestamp))
        .expect("the drain put scan info on the pane");
    assert_ne!(
        parked_at,
        at(0),
        "fixture: the pane's clock and the fetch's clock coincide, so this \
         scene cannot show the two-clock defect",
    );
    assert!(
        app.loop_mgr.get_cached(SITE, &at(0)).is_some(),
        "precondition: the arrival was filed in the loop cache under the fetch clock",
    );

    app.evict_unneeded_loop_scans();

    assert!(
        app.loop_mgr.get_cached(SITE, &at(0)).is_some(),
        "the volume the pane is parked on was evicted from the loop cache \
         because its key ({}) was compared against the pane's clock ({parked_at})",
        at(0),
    );
}

/// **An auto-poll's latest that is also the site's merge base adds nothing
/// to `still scans`**: the drain files one `Arc<Scan>` in both, and the level
/// de-duplicates by allocation, so the third holder is free.
#[test]
fn a_latest_that_is_the_merge_base_adds_nothing_to_the_still_level() {
    let mut app = app_on_site();
    app.gui.pane_mut(0).expect("a pane").viewing_live = false;
    land_one_auto_poll_volume(&mut app, SITE, at(0));

    let (latest, _, _, _) = app
        .latest_cached_scans
        .get(SITE)
        .expect("an auto-poll for a site no pane watches live is filed as its latest");
    let (base, _) = app
        .volumes
        .base_for(SITE)
        .expect("the drain installed the base");
    assert!(
        Arc::ptr_eq(latest, &base),
        "fixture: the latest and the base are different allocations, so this \
         is not the shared case",
    );
    let one = squallar_radar::scan_size::scan_bytes(latest) as u64;
    assert!(one > 0, "fixture: a volume priced at nothing");

    assert_eq!(
        app.volumes.latest_price(SITE) as u64,
        one,
        "the drain filed the latest without pricing it",
    );
    assert_eq!(
        app.still_scan_level(),
        one,
        "a volume held as both base and latest was charged twice",
    );
    assert_eq!(
        app.still_scan_level(),
        app.volumes.resident_scan_bytes() as u64,
        "the shared latest moved the level the stores alone already read",
    );
}

/// **`radar shared` names the whole of one arrival**, because one arrival is
/// one allocation the still side and the loop cache both price.
///
/// # What an input must carry for the defect to appear
///
/// **One `Arc<Scan>` reachable from both sides.** A fixture that files a
/// volume into the inventory and a *different* one into the loop cache can
/// run green on every assertion here and say nothing at all: it has no
/// sharing to measure, so a `radar_shared_level` hard-coded to zero would
/// satisfy it. That is why this drives `App::poll_data_channels` rather than
/// filling the stores, and why the `Arc::ptr_eq` below is a precondition and
/// not a conclusion — the whole finding rests on the arrival path putting one
/// allocation in three holders, which only the arrival path can demonstrate.
///
/// # Why it matters
///
/// The census family table adds `still scans` and `loop scans`. On a scene
/// where each pane is parked on a fetched volume this test says that sum is
/// about double what emptying both would free, so a memory target read off it
/// is a target against a quantity that is not on the heap.
///
/// TAMPER: return 0 from `App::radar_shared_level` and the sum goes back to
/// double; return `still_scan_level()` and the loop-only control below fails.
#[test]
fn the_shared_level_names_the_whole_of_one_arrival() {
    let mut app = app_on_site();
    land_one_archive_volume(&mut app, SITE, at(0));

    let (base, _) = app.volumes.base_for(SITE).expect("the base is resident");
    let (cached, _) = app
        .loop_mgr
        .get_cached(SITE, &at(0))
        .expect("every arrival is filed in the loop download cache");
    assert!(
        Arc::ptr_eq(&base, cached),
        "fixture: the two sides hold different allocations, so there is no \
         sharing here for this figure to measure",
    );
    let one = squallar_radar::scan_size::scan_bytes(&base) as u64;
    assert!(one > 0, "fixture: a volume priced at nothing");

    assert_eq!(
        app.still_scan_level(),
        one,
        "precondition: the still side prices this arrival at one volume",
    );
    assert_eq!(
        app.loop_mgr.cached_scan_bytes() as u64,
        one,
        "precondition: the loop cache prices the same arrival at one volume",
    );
    assert_eq!(
        app.radar_shared_level(),
        one,
        "the two families name 2 x {one} B between them and this says none of \
         it is shared, so the census's decoded-volume total stays double the \
         allocation",
    );
}

/// **A volume only the loop cache holds is not shared**, and one only the
/// still side holds is not either — per volume, never as a fraction of the
/// pair.
///
/// The control the test above cannot be read without, and it is built so that
/// **no simpler expression than the union passes it**. The scene holds three
/// distinct allocations arranged so the four figures a wrong implementation
/// would most plausibly return are all different from the right one:
///
/// * two volumes on the still side, one of them evicted from the loop cache;
/// * two volumes in the loop cache, one of them the loop's own frame;
/// * exactly ONE allocation on both sides.
///
/// So `still scans` reads two volumes, `loop scans` reads two, their minimum
/// reads two, and the answer is one. Returning either family, or the smaller
/// of them, fails here; only a walk that compares allocations passes.
///
/// The fixture volumes all price alike, which is why the discrimination is
/// built out of *counts* of distinct allocations rather than out of sizes: a
/// scene where the shared volume happened to be half the cache could not tell
/// "the shared one" from "half of it".
#[test]
fn only_the_allocation_on_both_sides_is_shared() {
    let mut app = app_on_site();
    land_one_archive_volume(&mut app, SITE, at(0));
    land_one_archive_volume(&mut app, OTHER, at(0));

    let (shared, _) = app.volumes.base_for(SITE).expect("KTLX has a base");
    let (still_only, _) = app.volumes.base_for(OTHER).expect("KOUN has a base");
    let one = squallar_radar::scan_size::scan_bytes(&shared) as u64;
    assert!(one > 0, "fixture: a volume priced at nothing");
    assert_eq!(
        squallar_radar::scan_size::scan_bytes(&still_only) as u64,
        one,
        "fixture: the two arrivals price differently, so the counts below \
         are not the arithmetic this reads them as",
    );

    // The other site's volume leaves the loop cache and stays on the still
    // side: the still-only class, which the drain cannot produce on its own
    // because `append_scan_to_active_loops` files every arrival in both.
    let dropped = app.loop_mgr.retain_scans(|site, _, _| site == SITE);
    assert_eq!(dropped.len(), 1, "fixture: the wrong site's volume left");
    drop(dropped);

    // A frame the loop downloaded for itself: in the loop cache and in no
    // still-side holder.
    let loop_only = crate::volume_fixture::ready_scan();
    app.loop_mgr
        .cache_scan(SITE, at(5), (Arc::clone(&loop_only), Default::default()));

    assert_eq!(
        app.still_scan_level(),
        2 * one,
        "precondition: the still side holds two distinct volumes",
    );
    assert_eq!(
        app.loop_mgr.cached_scan_bytes() as u64,
        2 * one,
        "precondition: the loop cache holds two distinct volumes",
    );
    assert!(
        Arc::strong_count(&still_only) >= 2 && Arc::strong_count(&loop_only) >= 2,
        "precondition: the two unshared volumes are still held by their one \
         side apiece",
    );

    assert_eq!(
        app.radar_shared_level(),
        one,
        "the shared level is not the ONE allocation both sides name: it read \
         {} B against a still side of {} B and a loop cache of {} B, so it is \
         reporting a family rather than measuring an overlap",
        app.radar_shared_level(),
        app.still_scan_level(),
        app.loop_mgr.cached_scan_bytes(),
    );
}

/// **A drain arrival's archive survives the residency sweep**, so the volume
/// it came out of can be traded for it instead of sitting resident forever.
///
/// # The defect, measured on the real drain
///
/// `retain_archives` asked one question — "does a live loop frame name this
/// ADDRESS" — on a premise that was true for the loop's own downloads and
/// false for the archive drain's. The drain files a pane fetch, an auto-poll
/// and an adjacent-volume nudge under the second the S3 key names, and puts
/// the volume's own first radial on the pane. Those instants are equal on 0
/// of the 171 local Archive II volumes.
///
/// So the archive was filed on arrival and swept one pass later. What that
/// cost is not the download: `evict_decoded_except` refuses to evict a volume
/// with **no archive behind it**, so the sweep manufactured exactly the
/// un-evictable 33.7-82.7 MiB volume that
/// `archive_less_volumes_hold_the_decoded_ceiling_shut` describes — holding
/// the decoded ceiling shut against the loop's own frames, for the life of the
/// process.
///
/// # What an input must carry for the guard to be reachable
///
/// Three arrangements, and the first is the one a hand-built fixture destroys
/// without noticing:
///
/// 1. **The two clocks must actually DIFFER.** A `ScanInfo` written by hand at
///    the same round timestamp the archive is filed under makes the address
///    and the identity equal, the old address-only predicate answers
///    correctly, and the fixture is structurally unable to see the bug. Only
///    the real drain produces the split, which is why this test drives
///    `poll_data_channels`. Asserted below rather than assumed.
/// 2. **No OTHER route may name the address.** If the pane's loop frames
///    named this moment, `keep` would already be true and the new clause would
///    never run — green, and about nothing.
/// 3. **A live loop on the site**, or the drain never files the archive at all
///    (`append_scan_to_active_loops` gates that on `is_looping`) and there is
///    nothing for the sweep to keep or drop.
///
/// The consequence is asserted where it is spent — the volume becomes
/// tradeable — and by `Arc::strong_count`, not by a store row: the still
/// inventory holds the same allocation, so the count falling by exactly one is
/// what says the loop cache really let go.
#[test]
fn a_drain_arrivals_archive_survives_the_sweep_that_only_knew_its_address() {
    let mut app = app_on_site();
    // (3) A live loop on the site, whose frames are elsewhere — see (2).
    app.loop_mgr.set_plan(
        0,
        squallar_radar::loop_downloads::FramePlan::new(SITE.to_string(), vec![at(30), at(35)]),
    );
    land_one_archive_volume(&mut app, SITE, at(0));

    let parked_at = app
        .gui
        .pane(0)
        .and_then(|p| p.scan_info.as_ref().map(|i| i.timestamp))
        .expect("the drain put scan info on the pane");
    // (1) The clocks differ, which is the whole defect class.
    assert_ne!(
        parked_at,
        at(0),
        "fixture: the address and the identity coincide, so an address-only \
         predicate answers this scene correctly and the fixture cannot reach \
         the bug",
    );
    // (2) Nothing else names the address.
    assert!(
        !app.gui
            .pane(0)
            .expect("a pane")
            .time_state(&squallar_source::id::known::RADAR)
            .frames
            .iter()
            .any(|f| f.timestamp == at(0)),
        "fixture: a loop frame names this address, so `keep` is already true \
         and the two-clock clause is never exercised",
    );
    assert!(
        app.loop_mgr.has_archive(SITE, &at(0)),
        "precondition: the drain filed the compressed half",
    );

    app.evict_unneeded_loop_scans();

    assert!(
        app.loop_mgr.has_archive(SITE, &at(0)),
        "the archive was swept because the sweep only knew the address ({}) \
         and the pane is parked at the identity ({parked_at}); the volume it \
         came out of is now un-evictable",
        at(0),
    );

    // And what the archive is FOR: the volume can now be traded for it.
    let volume = Arc::clone(
        &app.loop_mgr
            .get_cached(SITE, &at(0))
            .expect("the parked volume is still cached")
            .0,
    );
    let before = Arc::strong_count(&volume);
    drop(app.loop_mgr.evict_decoded_except(|_, _, _| false));
    assert_eq!(
        Arc::strong_count(&volume),
        before - 1,
        "the loop cache did not let go of the volume, so keeping the archive \
         bought nothing",
    );
}

/// **An archive no clock names is still swept.** The control: the fix widens
/// `retain_archives` by exactly one clause, and a predicate that had instead
/// become "keep everything" would pass the test above and would turn the
/// compressed cache into a second unbounded holder.
#[test]
fn an_archive_neither_clock_names_is_still_swept() {
    let mut app = app_on_site();
    app.loop_mgr.set_plan(
        0,
        squallar_radar::loop_downloads::FramePlan::new(SITE.to_string(), vec![at(30), at(35)]),
    );
    land_one_archive_volume(&mut app, SITE, at(0));
    // A moment nothing is parked at and no frame names, with neither half of
    // its identity known to any pane.
    app.loop_mgr
        .cache_archive(SITE, at(45), std::sync::Arc::new(vec![0u8; 4096]));
    assert!(
        app.loop_mgr.has_archive(SITE, &at(45)),
        "precondition: the unnamed archive is here to be swept",
    );

    app.evict_unneeded_loop_scans();

    assert!(
        !app.loop_mgr.has_archive(SITE, &at(45)),
        "an archive no pane is parked at and no live frame names survived, so \
         the compressed cache is retaining on a predicate that cannot say no",
    );
    assert!(
        app.loop_mgr.has_archive(SITE, &at(0)),
        "and the parked one went with it",
    );
}

/// **Emptying the still store frees nothing on a live pane, and the merge
/// base is why** — so a still-side withdrawal cannot pay on any scene whose
/// panes are live.
///
/// # What this settles, and what it cost to find out
///
/// `still scans` reads 403.8 MiB on the HEAVY6 arm and 96.9 on REST1, which
/// invites the reading that there is a still-side cache to reclaim. There is
/// not. **Both** arrival paths `Arc::clone` ONE decoded volume into the still
/// store and into the site's merge base in a single statement — the archive
/// drain in `App::poll_data_channels`, and the chunk feed in
/// `App::land_chunk_outcome` when a closed volume completes — so on a live
/// pane the two stores name one allocation and the still store's copy is a
/// refcount, not bytes.
///
/// The residency policy leaves no slack either: `retain_still`'s `wanted` is
/// "what a pane is parked at, plus the newest per shown site", so the store
/// already holds only what is on screen.
///
/// **So the withdrawable set is the still entries that are NOT their site's
/// base, and on a live pane that set is empty.** What `still scans` actually
/// prices on those arms is the merge bases, and the base is not free to go:
/// `section_source_refusal`, `App::extract_current_volume`,
/// `App::dispatch_section_renders`' extract closure,
/// `App::current_ladder_fingerprint` and `App::current_volume_stamp` all read
/// it — the first three for the section cut's GATES and the last two for its
/// structure and its times only. A withdrawal there is a different change
/// from this one.
///
/// Memoising the last two was tried and refuted
/// (`squallar_radar::current::tests::the_fingerprint_and_the_stamp_move_with_the_overlay_while_the_base_stands`):
/// both are functions of the LIVE overlay, which advances every sealed sweep.
/// What they are not is functions of a gate, so the route that remains is
/// `squallar_radar::skeleton::VolumeSkeleton` — structure resident, arrays
/// released, measured at 3.18 % of a VCP-212-shaped volume.
///
/// # Why the assertion is on the refcount
///
/// A store row is not a byte. `still_count` would fall to zero here and the
/// heap would not move at all, which is the exact reading this test exists to
/// refuse. `Arc::strong_count` is the only thing that can tell them apart.
///
/// **The fixture must land through the real drain** for the same reason every
/// test in this module does: a store filled by hand can put two DIFFERENT
/// volumes in the two stores, and then emptying one really would free
/// something — green, and about a scene the application never builds.
#[test]
fn emptying_the_still_store_frees_nothing_while_the_base_holds_the_same_volume() {
    let mut app = app_on_site();
    land_one_archive_volume(&mut app, SITE, at(0));

    let (base, _) = app.volumes.base_for(SITE).expect("the base is resident");
    let at_moment = app
        .volumes
        .newest_still_for(SITE)
        .expect("the arrival installed a still");
    let (still, _) = app
        .volumes
        .still_for(SITE, at_moment)
        .expect("the still is resident");
    assert!(
        Arc::ptr_eq(&still, &base),
        "fixture: the drain installed two different volumes, so emptying the \
         still store would free something and this test is about a scene the \
         app does not build",
    );
    let one = squallar_radar::scan_size::scan_bytes(&base) as u64;
    assert_eq!(
        app.still_scan_level(),
        one,
        "precondition: the level prices the shared allocation once",
    );

    // Everything the still store holds, handed back owned and dropped: the
    // strongest form of "the still side let go".
    let dropped = app.volumes.retain_still(&|_, _| false);
    assert_eq!(dropped.len(), 1, "fixture: the still store held one volume");
    drop(dropped);
    drop(still);

    assert!(
        app.volumes.holds_no_still(),
        "precondition: the still store really is empty",
    );
    assert!(
        Arc::strong_count(&base) > 1,
        "the allocation was freed by emptying the still store alone, so a \
         still-side withdrawal would pay after all and this test is stale",
    );
    assert_eq!(
        app.still_scan_level(),
        one,
        "`still scans` did not fall when the still store emptied, which is \
         the point: what that row prices on a live pane is the merge base, \
         and the base has readers the still does not",
    );
}

/// **The withdrawal will not release a base with no way back**, and the
/// archive drain's own arrival is the case that has one.
///
/// # What an input must carry for the refusal to be reachable
///
/// A base that HAS an archive and one that does not, in the same shape of
/// scene, or the test cannot tell a policy that checks from one that never
/// releases anything. Both arms are driven through `poll_data_channels`; the
/// difference is one `cache_archive`.
///
/// The archive is looked up by the volume's IDENTITY, not by the address it
/// was filed under, and those are equal on 0 of the 171 local Archive II
/// volumes — so a lookup that used the address would find nothing here and
/// this test would pass for the wrong reason. The precondition below asserts
/// the two clocks differ, which is what makes the identity route load-bearing.
#[test]
fn a_base_with_no_archive_keeps_its_gates() {
    let mut app = app_on_site();
    land_one_volume_with_no_archive(&mut app, SITE, at(0));
    the_pane_has_its_picture(&mut app);
    let collected = app
        .volumes
        .base_collected_at(SITE)
        .expect("the drain installed a base");
    assert_ne!(
        collected,
        at(0),
        "fixture: the base's identity equals the archive's address, so an \
         address-keyed lookup would work and the identity route is untested",
    );
    assert!(
        app.loop_mgr.archive_for_identity(SITE, collected).is_none(),
        "precondition: this arm is the no-archive one",
    );

    app.evict_unshown_scans();

    assert!(
        app.volumes.base_has_gates(SITE),
        "a base with nothing to decode from was released, so the section cut \
         has no way back at all — which is the chunk feed's every volume, \
         filed with `archive: None`",
    );
}

/// **A site any pane reads gates from keeps them** — the cross-section and the
/// 3D resample are the two readers a skeleton cannot serve.
///
/// The control for the release below: without it, a policy that released
/// unconditionally would pass every other test here and would blank a section
/// pane.
#[test]
fn a_site_with_a_section_pane_keeps_its_gates() {
    let mut app = app_on_site();
    land_one_archive_volume(&mut app, SITE, at(0));
    the_pane_has_its_picture(&mut app);
    let collected = app.volumes.base_collected_at(SITE).expect("a base");
    // Give it the way back, so the ONLY thing standing between this base and
    // a release is the section pane.
    app.loop_mgr
        .cache_archive(SITE, at(0), std::sync::Arc::new(vec![0u8; 4096]));
    assert!(
        app.loop_mgr.archive_for_identity(SITE, collected).is_some(),
        "precondition: the way back is present, so a refusal here is about \
         the reader and not about the archive",
    );
    app.gui
        .pane_mut(0)
        .expect("a pane")
        .set_kind(squallar_egui::pane::PaneKind::CrossSection);
    assert!(
        app.gui.pane(0).expect("a pane").cross_section().is_some(),
        "fixture: the pane shows no section, so there is no gate reader here",
    );

    app.evict_unshown_scans();

    assert!(
        app.volumes.base_has_gates(SITE),
        "the gates were released under a section pane, which cuts from them",
    );
}

/// **With a way back and no gate reader, the base's gates go — and the
/// structure that replaces them still answers the frame-thread readers.**
///
/// The release is asserted on `Arc::strong_count`: the still store holds the
/// same allocation on a live pane, so the inventory's own row is not evidence
/// that anything was let go. What this shows is the base's REFERENCE dropping,
/// which is the half of the joint release this change builds.
///
/// And the readers are asserted AFTER the release, through the same entry
/// points the application calls, because a release that satisfied every
/// byte-shaped assertion and left `current_volume_stamp` answering `None`
/// would blank every site's displayed time.
#[test]
fn a_base_nothing_reads_gates_from_is_released_and_still_answers() {
    let mut app = app_on_site();
    land_one_archive_volume(&mut app, SITE, at(0));
    the_pane_has_its_picture(&mut app);
    let (base, _) = app.volumes.base_for(SITE).expect("the base is resident");
    app.loop_mgr
        .cache_archive(SITE, at(0), std::sync::Arc::new(vec![0u8; 4096]));

    let stamp_before = app.current_volume_stamp(SITE);
    let print_before =
        app.current_ladder_fingerprint(SITE, squallar_radar::types::RadarProduct::Reflectivity);
    assert!(
        stamp_before.is_some(),
        "fixture: the stamp reads None before the release, so an equality \
         after it would be None == None",
    );
    app.evict_unshown_scans();

    assert!(
        !app.volumes.base_has_gates(SITE),
        "the base kept its gates although nothing reads them and an archive \
         is held",
    );
    // **The REFCOUNT is not asserted here, and the reason is worth writing
    // down.** The app path hands a released volume to
    // `squallar_worker::offload::discard`, whose queue is a process-global
    // shared by every test in this binary — so `Arc::strong_count` at this
    // point measures how far that lane has drained, not whether this base let
    // go. Measured: this assertion passed run alone and failed in a batch with
    // the other tests in this module, which is the shared-state trap and not a
    // defect in the release.
    //
    // The refcount claim belongs where it is deterministic, and it is made
    // there: `volume_inventory::base_gate_release_tests::releasing_the_gates_hands_the_volume_back_and_keeps_the_structure`
    // calls the inventory directly, takes the volume back owned, drops it, and
    // requires the count to fall to exactly one.
    let _ = &base;
    assert!(
        app.volumes.base_for(SITE).is_none(),
        "a gate reader can still reach the released base",
    );
    assert!(
        app.volumes.base_skeleton_bytes() > 0,
        "the released base prices its structure at nothing",
    );

    assert_eq!(
        app.current_volume_stamp(SITE),
        stamp_before,
        "the displayed data-through time changed when the gates went, and it \
         is a function of collection times that a skeleton preserves exactly",
    );
    assert_eq!(
        app.current_ladder_fingerprint(SITE, squallar_radar::types::RadarProduct::Reflectivity),
        print_before,
        "the section's re-cut key moved when the gates went, which would \
         re-cut every transition against a key that cannot see its own data",
    );
}

/// **A gate reader asks for exactly one restore, not one a frame.**
///
/// `ensure_base_whole` re-derives on the STATE — "does this holder have
/// gates?" — so it is asked again on every pass while the answer stays no.
/// The in-flight mark is what keeps that from dispatching a decode per frame,
/// and without it a section pane on a released base would queue a 33.7-82.7
/// MiB decode every frame it was shown.
#[test]
fn a_released_base_is_restored_once_however_often_it_is_asked() {
    let mut app = app_on_site();
    land_one_archive_volume(&mut app, SITE, at(0));
    the_pane_has_its_picture(&mut app);
    app.loop_mgr
        .cache_archive(SITE, at(0), std::sync::Arc::new(vec![0u8; 4096]));
    app.evict_unshown_scans();
    assert!(
        app.volumes.base_is_released(SITE),
        "precondition: the base is released, which is the only state that asks",
    );
    // **The loop cache must not still hold the volume**, or the restore is
    // free and no decode is dispatched at all — which is the right behaviour
    // and the wrong fixture for this test. Measured: without this the ask
    // dispatched 0 decodes because `restore_released_bases` could take the
    // whole volume straight out of the cache.
    drop(app.loop_mgr.evict_decoded_except(|_, _, _| false));
    assert!(
        app.loop_mgr.get_cached(SITE, &at(0)).is_none(),
        "precondition: the cache still holds the volume, so a decode is not \
         the way back and the guard under test is never reached",
    );
    assert!(
        app.loop_mgr.has_archive(SITE, &at(0)),
        "precondition: the archive went with the moments, so there is nothing \
         to decode from",
    );

    for _ in 0..5 {
        app.ensure_base_whole(SITE);
    }

    // **Counted at the dispatch, because neither of the two obvious proxies
    // works.** The in-flight set is keyed by site, so its length is 1 whether
    // the guard is there or not — measured, on a tamper that deleted the guard
    // and left a length assertion green. The replies are 0 either way, because
    // the job funnel does not run in a headless test.
    let dispatched = app.base_restore_dispatches.get();
    assert_eq!(
        dispatched, 1,
        "five asks dispatched {dispatched} decodes, so a shown section pane \
         queues a whole-volume decode every frame it is drawn",
    );
}

/// **The joint release: all four holders let go of one allocation, and
/// `still scans` goes to zero for it.**
///
/// This is the cut. Releasing fewer frees nothing — both arrival paths clone
/// ONE `Arc<Scan>` into the base, the still store and the loop download cache,
/// so three holders would remain and `live_bytes` would not move. Each holder
/// is asserted individually against what IT needs, never the set against a
/// total: a release that dropped three and kept the loop cache's would satisfy
/// any figure-shaped assertion and free nothing.
///
/// The byte claim is `still_scan_level()`, which is what the census publishes
/// as `still scans` and is deterministic here — unlike `Arc::strong_count` at
/// this level, which measures the process-global deferred-drop lane's
/// occupancy and is asserted in `base_gate_release_tests` instead.
///
/// **No gate reader can obtain a gateless volume**, asserted rather than left
/// to the type: `still_for` and `base_for` must answer `None`, not `Some` with
/// empty buffers. That is the whole reason the skeleton lives in its own field
/// behind its own type — the four readers that consume an empty buffer as real
/// data (the raster fill that paints nothing and returns `Some`, the code
/// plane refusal logged at `info!` and fallen through, `velocity::grid`'s
/// all-`NaN` sweep sized from the scalar count, and `estimate_fold_limit`
/// disarming by returning `None`) are unreachable only while that holds.
#[test]
fn the_joint_release_drops_every_holder_of_one_allocation() {
    let mut app = app_on_site();
    land_one_archive_volume(&mut app, SITE, at(0));
    the_pane_has_its_picture(&mut app);
    app.loop_mgr
        .cache_archive(SITE, at(0), std::sync::Arc::new(vec![0u8; 4096]));

    let (base, _) = app.volumes.base_for(SITE).expect("the base is resident");
    let parked = app
        .volumes
        .newest_still_for(SITE)
        .expect("the arrival installed a still");
    assert!(
        Arc::ptr_eq(
            &app.volumes.still_for(SITE, parked).expect("a still").0,
            &base
        ),
        "fixture: the still and the base are different allocations, so this \
         scene is not the one the joint release exists for",
    );
    assert!(
        app.loop_mgr
            .get_cached(SITE, &at(0))
            .is_some_and(|(scan, _)| Arc::ptr_eq(scan, &base)),
        "fixture: the loop cache holds a different volume",
    );
    let one = app.still_scan_level();
    assert!(one > 0, "fixture: a volume priced at nothing");

    app.evict_unshown_scans();

    // Each holder, against what IT needs.
    assert!(
        app.volumes.base_for(SITE).is_none(),
        "the base still answers gate readers",
    );
    assert!(
        app.volumes.still_for(SITE, parked).is_none(),
        "the still store still holds the allocation, so the base's release \
         freed nothing",
    );
    assert!(
        !app.latest_cached_scans.contains_key(SITE),
        "the per-site latest still holds the allocation",
    );
    assert!(
        app.loop_mgr.get_cached(SITE, &at(0)).is_none(),
        "the loop download cache still holds the allocation, which is three \
         holders released and no bytes freed",
    );
    // The way back survived the release.
    assert!(
        app.loop_mgr.has_archive(SITE, &at(0)),
        "the archive went with the volume, so nothing can decode it back",
    );
    assert!(
        app.volumes.base_is_released(SITE),
        "the base kept no structure, so the frame-thread readers have nothing",
    );

    assert_eq!(
        app.still_scan_level(),
        0,
        "`still scans` did not fall to zero for a volume every holder let go",
    );
    assert!(
        app.volumes.base_skeleton_bytes() > 0,
        "the structure that replaced it is priced at nothing",
    );
}

/// **What was released comes back whole**, base and still together, off the
/// archive the release kept.
///
/// A release with no way back is a blank pane, so this drives the real restore:
/// the decode lands in the loop cache and `restore_released_bases` takes it
/// from there. Asserted on the GATES, not on presence — a restore that
/// reinstalled a structurally plausible volume with different values would
/// satisfy every count here and be a wrong picture.
#[test]
fn a_jointly_released_volume_comes_back_whole() {
    use nexrad_model::data::DataMoment;

    let mut app = app_on_site();
    land_one_archive_volume(&mut app, SITE, at(0));
    the_pane_has_its_picture(&mut app);
    app.loop_mgr
        .cache_archive(SITE, at(0), std::sync::Arc::new(vec![0u8; 4096]));
    let (base, _) = app.volumes.base_for(SITE).expect("a base");
    let collected = app.volumes.base_collected_at(SITE).expect("a base");
    let gates: Vec<Vec<u8>> = base
        .sweeps()
        .iter()
        .flat_map(nexrad_model::data::Sweep::radials)
        .filter_map(nexrad_model::data::Radial::reflectivity)
        .map(|m| m.raw_values().to_vec())
        .collect();
    assert!(
        gates.iter().any(|g| !g.is_empty()),
        "fixture: no gates to compare"
    );

    app.evict_unshown_scans();
    assert!(
        app.volumes.base_is_released(SITE) && app.volumes.still_for(SITE, collected).is_none(),
        "precondition: the joint release happened",
    );

    // The volume arrives back in the loop cache — the one place a decode
    // files it — and the state-derived pass takes it from there.
    app.loop_mgr
        .cache_scan(SITE, at(0), (Arc::clone(&base), Default::default()));
    app.restore_released_bases();

    let (restored, _) = app
        .volumes
        .base_for(SITE)
        .expect("the base did not come back, so nothing that needs gates can be served");
    assert!(
        app.volumes.still_for(SITE, collected).is_some(),
        "the base came back without the still, so the plan-view render asks \
         forever for a volume nothing will reinstall",
    );
    let after: Vec<Vec<u8>> = restored
        .sweeps()
        .iter()
        .flat_map(nexrad_model::data::Sweep::radials)
        .filter_map(nexrad_model::data::Radial::reflectivity)
        .map(|m| m.raw_values().to_vec())
        .collect();
    assert_eq!(after, gates, "the restored volume is not the one released");
    assert!(
        app.still_scan_level() > 0,
        "the restored volume is priced at nothing",
    );
}

/// **The arrival path leaves the withdrawal a way back, on the site shape the
/// application actually boots into: nothing looping.**
///
/// This is the test the withdrawal never had, and its absence is why the cut
/// landed dormant. Every release scene above hands the loop cache an archive
/// with a `cache_archive` call of its own, so all of them were green while
/// `App::append_scan_to_active_loops` was filing the compressed half only on a
/// site `LoopDownloadManager::is_looping` said yes to — which is no site at
/// boot. `archive_for_identity` then answered `None` for every base the drain
/// installed, `release_unneeded_base_gates` took its `continue` every time,
/// and the measurement said so: `base skeletons` 0 B on all 530 census ticks
/// of a 5-leg run, `still scans` peaking at 101,596,480 B on every leg, byte
/// for byte what the tree read before the withdrawal existed.
///
/// The `is_looping` assertion is the load-bearing one. Without it a fixture
/// that happened to arm a loop would pass against the old code and this would
/// be a test of nothing.
#[test]
fn a_boot_arrival_leaves_the_withdrawal_a_way_back_with_no_loop_running() {
    let mut app = app_on_site();
    land_one_archive_volume(&mut app, SITE, at(0));

    assert!(
        !app.loop_mgr.is_looping(SITE),
        "fixture: a loop is running, so the archive would be filed by the old \
         rule too and this asserts nothing",
    );
    let collected = app
        .volumes
        .base_collected_at(SITE)
        .expect("the drain installed a base");
    assert_ne!(
        collected,
        at(0),
        "fixture: the base's identity equals the address the archive was filed \
         under, so an address-keyed lookup would answer and the identity route \
         this depends on is untested",
    );
    assert!(
        app.loop_mgr.archive_for_identity(SITE, collected).is_some(),
        "the arrival that installed the base filed no archive for it, so \
         `release_unneeded_base_gates` can never find a way back and the \
         joint release is dead code on every live pane",
    );
}

/// **The withdrawal fires end to end off the drain alone**, with nothing
/// filed by hand.
///
/// The one above shows the way back exists; this shows the whole policy runs
/// on it. Deliberately no `cache_archive` call anywhere: the only compressed
/// bytes in this process are the ones the arrival carried, and if the
/// residency pass sweeps them before the withdrawal is asked — it runs first,
/// and asks a two-clock question the drain's arrivals used to fail — nothing
/// is released and this reddens.
///
/// The byte claim is `still_scan_level`, the figure the census publishes,
/// rather than `Arc::strong_count`, which measures the process-global
/// deferred-drop lane and is not deterministic here.
#[test]
fn the_drain_alone_arms_the_joint_release() {
    let mut app = app_on_site();
    land_one_archive_volume(&mut app, SITE, at(0));
    the_pane_has_its_picture(&mut app);

    let parked = app
        .volumes
        .newest_still_for(SITE)
        .expect("the arrival installed a still");
    let before = app.still_scan_level();
    assert!(before > 0, "fixture: a volume priced at nothing");

    app.evict_unshown_scans();

    assert!(
        app.volumes.base_is_released(SITE),
        "the base kept its gates on an arrival that carried its own archive, \
         so the withdrawal is still unreachable from the application's own \
         path",
    );
    assert!(
        app.volumes.still_for(SITE, parked).is_none(),
        "the still store still holds the allocation, so the release freed \
         nothing",
    );
    assert!(
        app.loop_mgr.get_cached(SITE, &at(0)).is_none(),
        "the loop download cache still holds the allocation",
    );
    assert!(
        app.loop_mgr.has_archive(SITE, &at(0)),
        "the residency pass swept the way back before the withdrawal was \
         asked, so a section pane on this site has nothing to wait for",
    );
    assert_eq!(
        app.still_scan_level(),
        0,
        "`still scans` did not fall for a volume every holder let go",
    );
    assert!(
        app.volumes.base_skeleton_bytes() > 0,
        "the structure that replaced it is priced at nothing",
    );
}

/// **A released base's way back survives the pane moving off the volume it
/// came from.**
///
/// The scene is the one every session opens with and then leaves within a
/// minute: the boot volume lands, the pane draws it, the withdrawal takes the
/// gates against the archive as its way back — and then a newer volume
/// arrives and the pane parks on that one instead. From that moment the
/// archive the released base is keyed to is named by nothing
/// `evict_unneeded_loop_scans` asks: no loop frame names it, and `parked` is
/// the new volume. It was swept, and the base stayed released on a skeleton
/// with nothing to decode back — the state `App::ensure_base_whole` logs at
/// `warn` as one "the withdrawal that released it should not have".
///
/// Measured on five 420 s single-pane legs of 2026-09-09 before this: `loop
/// archives` fell to 0 within seconds on all five while `base skeletons`
/// stood at 1.93 MiB for the rest of the leg — a released base that could not
/// be made whole again for ~400 s, on 4 of the 5.
///
/// The pane is moved by writing `scan_info.timestamp`, which is the field the
/// drain itself writes and the field `parked` is built from, so this asks the
/// same question the application asks.
///
/// TAMPER: drop the `released_ways_back` clause from `retain_archives` and
/// this reddens on its last assertion alone.
#[test]
fn a_released_bases_way_back_survives_the_pane_moving_off_it() {
    let mut app = app_on_site();
    land_one_archive_volume(&mut app, SITE, at(0));
    the_pane_has_its_picture(&mut app);
    app.evict_unshown_scans();

    assert!(
        app.volumes.base_is_released(SITE),
        "fixture: nothing was withdrawn, so there is no way back to lose",
    );
    let collected = app
        .volumes
        .base_collected_at(SITE)
        .expect("a released base is still keyed to its volume");
    assert!(
        app.loop_mgr.has_archive(SITE, &at(0)),
        "fixture: the way back was already gone before the pane moved",
    );

    // The pane parks on a moment that is neither the archive's address nor
    // the base's identity — what a newer arrival does to it.
    let moved_to = at(9);
    assert_ne!(
        moved_to,
        at(0),
        "fixture: the pane did not move off the address"
    );
    assert_ne!(
        Some(moved_to),
        Some(collected),
        "fixture: the pane did not move off the identity",
    );
    app.gui
        .pane_mut(0)
        .expect("a pane")
        .scan_info
        .as_mut()
        .expect("the drain put scan info on the pane")
        .timestamp = moved_to;

    app.evict_unshown_scans();

    assert!(
        app.volumes.base_is_released(SITE),
        "fixture: the base took its gates back on its own, so this scene no \
         longer asks whether the way back survived",
    );
    assert!(
        app.loop_mgr.has_archive(SITE, &at(0)),
        "the residency pass took the way back out from under a base that is \
         still released, so `ensure_base_whole` can never make it whole and a \
         section pane on this site waits forever",
    );
}

/// **A volume no pane has drawn yet is not taken**, however releasable it
/// otherwise looks.
///
/// The frame order is the whole of the reason. `App::evict_unshown_scans` runs
/// before `App::dispatch_pane_renders` in `App::handle_redraw`, so on the
/// frame an arrival lands the withdrawal would always be first: it would take
/// the still the drain had just installed, the dispatch would find nothing,
/// and `ensure_base_whole` would spend a whole volume decode rebuilding bytes
/// this process decoded milliseconds earlier — a first paint delayed by a
/// decode, on every arrival, to free nothing that stays freed.
///
/// TAMPER: drop `picture_owed` from `release_unneeded_base_gates`' guard and
/// this reddens while every other test in this module stays green, which is
/// what makes the hold-back gated rather than assumed.
#[test]
fn a_base_no_pane_has_drawn_yet_keeps_its_gates() {
    let mut app = app_on_site();
    land_one_archive_volume(&mut app, SITE, at(0));

    let collected = app.volumes.base_collected_at(SITE).expect("a base");
    assert!(
        app.loop_mgr.archive_for_identity(SITE, collected).is_some(),
        "precondition: the way back is present, so a refusal here is about \
         the picture and not about the archive",
    );
    assert!(
        app.render.pane_render[0].last_rendered.is_none(),
        "fixture: the pane already has a picture, so nothing is owed and this \
         test is about a state the arrival path does not build",
    );

    app.evict_unshown_scans();

    assert!(
        app.volumes.base_has_gates(SITE),
        "the volume was taken out from under a pane that has not drawn it, so \
         the next dispatch pays a decode for bytes this process just had",
    );

    // And it goes as soon as the picture exists, so the hold-back is a wait
    // and never a permanent refusal.
    the_pane_has_its_picture(&mut app);
    app.evict_unshown_scans();
    assert!(
        app.volumes.base_is_released(SITE),
        "the hold-back outlived the picture it was waiting for, which is a \
         withdrawal that never fires dressed as one that does",
    );
}
