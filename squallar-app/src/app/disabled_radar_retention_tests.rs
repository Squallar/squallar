//! **A pane that needs no radar data retains none of it.**
//!
//! The fetch half landed on 2026-09-07: a pane with its radar layer switched
//! off stopped opening its site's notification sockets, stopped assembling a
//! chunk feed and stopped pulling the archive on the cadence, all through
//! `Gui::live_sites` asking `PaneState::needs_radar_data`. Nothing asked that
//! question of the *retention*, so a pane that already had a volume when the
//! user switched the layer off went on naming its site and its moment to
//! every store the eviction pass walks — the decoded still, the base, the
//! auto-poll's cached copy and the site's cached extractions — and kept them
//! for the life of the process. Measured at 48.4 MiB of `still scans` per
//! pane on the campaign's one-pane arm, held for a layer drawing nothing.
//!
//! **Both stores are asked, because the one `Arc` is in both.** The still
//! inventory and the loop download cache hold the same allocation
//! (`scan_ownership_tests` pins that), so dropping it from one frees no bytes
//! at all. The `scan_info` half of `evict_unneeded_loop_scans`' retention is
//! asked the same question; its loop walk is not, so a site a live loop is
//! still deriving from keeps everything it kept before.
//!
//! Every test here drives the real arrival (`App::poll_data_channels`) and the
//! real eviction pass, and reads the inventory's own byte figure — the one the
//! census publishes as `still scans`.

use super::tests::n_pane_app;
use super::*;

const SITE: &str = "KTLX";

fn at(minute: u32) -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 9, 8)
        .expect("a real date")
        .and_hms_opt(18, minute, 0)
        .expect("a real time")
}

/// A headless app with `panes` panes, all on [`SITE`] and watching live.
fn app_on_site(panes: usize) -> App {
    let mut app = n_pane_app(panes, SITE);
    for idx in 0..panes {
        let pane = app.gui.pane_mut(idx).expect("the layout just made it");
        pane.viewing_live = true;
        assert!(
            pane.is_overlay_enabled(&squallar_source::id::known::RADAR),
            "premise: a pane that has saved nothing draws radar. If a fresh \
             pane read as switched off, the eviction below would be throwing \
             away a first-run pane's volume — a far worse defect than the one \
             it fixes.",
        );
    }
    app
}

/// Push one archive volume down the real channel and drain it.
fn land_one_volume(app: &mut App) {
    let scan = Arc::try_unwrap(crate::volume_fixture::ready_scan())
        .unwrap_or_else(|_| unreachable!("ready_scan hands out a fresh Arc"));
    let generation = app.render.fetch_generation_for(SITE);
    app.channels
        .scan_sender
        .send(crate::channels::ScanResponse {
            generation,
            site: SITE.to_string(),
            requester: crate::channels::FetchRequester::Site,
            result: Ok(crate::channels::ScanData {
                scan,
                declared_nyquist: Default::default(),
                site: SITE.to_string(),
                timestamp: at(0),
                archive: Some(std::sync::Arc::new(vec![0u8; 64])),
            }),
            is_auto_poll: false,
        })
        .expect("the app holds the receiver");
    app.poll_data_channels();
}

/// Switch pane `idx`'s radar layer on or off.
fn draw_radar(app: &mut App, idx: usize, on: bool) {
    app.gui
        .pane_mut(idx)
        .expect("the pane exists")
        .set_overlay_enabled(squallar_source::id::known::RADAR, on);
}

/// What the inventory is holding, in bytes — the figure the census publishes
/// as `still scans`. Bytes and not an entry count: the claim is about memory,
/// and an emptied store still answers a count of zero if it never held
/// anything.
fn resident(app: &App) -> usize {
    app.volumes.resident_scan_bytes()
}

/// **The volume of a pane that stops drawing radar is let go.**
///
/// **Floor — delete the `needs_radar_data` arm** from `evict_unshown_scans`'
/// pane walk: the figure after the switch-off equals the figure before it, to
/// the byte.
#[test]
fn a_pane_that_stops_drawing_radar_lets_go_of_its_volume() {
    let mut app = app_on_site(1);
    land_one_volume(&mut app);
    let held = resident(&app);
    assert!(
        held > 0,
        "premise: the arrival installed a volume with real radials in it. A \
         zero here makes every assertion below vacuous.",
    );
    // **The allocation itself, not a store's figure.** One arrival is filed
    // in the inventory's two stores AND in the loop download cache, and all
    // three hold the same `Arc` (`scan_ownership_tests`), so a store emptying
    // is not bytes going back. This clone is the instrument that can tell
    // those apart.
    let (volume, _) = app
        .volumes
        .base_for(SITE)
        .expect("the arrival installed the base");
    assert!(
        app.loop_mgr.get_cached(SITE, &at(0)).is_some(),
        "premise: every arrival is filed in the loop download cache too, and \
         that is the holder a still-inventory figure cannot see",
    );
    // **The DROP in the count, not the count.** Some of this workspace's
    // process-global state outlives a test — the derive memo among it — so a
    // whole-suite run can carry an owner this scene never created, and an
    // absolute `== 1` reads red in a full run and green when filtered. What
    // this change is answerable for is the three owners it removes.
    let owners_before = Arc::strong_count(&volume);

    draw_radar(&mut app, 0, false);
    app.evict_unshown_scans();

    assert_eq!(
        resident(&app),
        0,
        "the pane kept its whole decoded volume after the user switched the \
         radar layer off — {held} B for a layer that draws nothing, held for \
         the life of the process because nothing but a site change ever asks \
         again.",
    );
    assert_eq!(
        app.volumes.still_count(),
        0,
        "the still store kept an entry whose bytes it no longer charges for",
    );
    assert!(
        app.loop_mgr.get_cached(SITE, &at(0)).is_none(),
        "the loop download cache is still holding the volume the inventory \
         just gave up. Both hold the same allocation, so this is the \
         difference between a figure falling and a heap falling.",
    );
    let owners_after = owners_once_freed(&volume, owners_before - 3);
    assert!(
        owners_before >= owners_after + 3,
        "the app let go of only {} of the three owners one arrival has — the \
         base, the still and the loop download cache all hold the same \
         allocation, and while any one of them is still an owner not a byte \
         of the volume has gone back ({owners_before} owners before, \
         {owners_after} after; the survivor is this test's own clone).",
        owners_before - owners_after,
    );
}

/// `volume`'s owner count once the eviction's frees have actually happened, or
/// the count as it stood at the deadline.
///
/// **The eviction does not free anything itself, and a count read on the
/// instruction after it is racing.** `evict_unshown_scans` hands every evicted
/// volume to `squallar_worker::offload::discard`, which on native routes to the
/// pool's free lane — the whole point being that a multi-GiB teardown must not
/// land on the frame thread (`app::tests` red-gates a return to `retain` for
/// exactly that reason). So the three owners go away on ANOTHER THREAD, some
/// time after the call returns.
///
/// Read straight through, this test failed on **8 of 25 unmodified-tree runs**
/// of `cargo test -p squallar-app --lib` (2026-09-08, and the same on the tree
/// that added this helper: 10 of 25 before, 0 of 25 after). It reported "the
/// app let go of only 1 of the three owners", which is a true statement about
/// an instant that had not finished happening.
///
/// **Polls for the property, and never asserts on the clock.** The deadline is
/// only how long it is willing to be wrong before it says so; it returns the
/// live count either way, so a genuine retention regression still fails on the
/// caller's own assertion and its own message rather than on a timeout.
fn owners_once_freed(volume: &Arc<nexrad_model::data::Scan>, target: usize) -> usize {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let owners = Arc::strong_count(volume);
        if owners <= target || std::time::Instant::now() >= deadline {
            return owners;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

/// **The control, and the more important half of the pair**: a pane that draws
/// radar keeps everything.
///
/// The over-filtering guard. `is_overlay_enabled` answers `false` for a slot
/// that was never minted, so a retention gated carelessly on it throws away
/// the volume of a pane that has simply never been configured — the
/// fresh-install case, which is a far worse defect than the waste being
/// removed. The premise in [`app_on_site`] pins that pane from the front; this
/// pins what it keeps.
#[test]
fn a_pane_that_draws_radar_keeps_its_volume() {
    let mut app = app_on_site(1);
    land_one_volume(&mut app);
    let held = resident(&app);

    app.evict_unshown_scans();

    assert_eq!(
        resident(&app),
        held,
        "the eviction took the volume of a pane that is drawing it. Every \
         frame runs this pass, so this is a pane that can never hold a \
         volume at all.",
    );
}

/// **Two panes on one site part company only when neither wants it.**
///
/// The retention is a union over panes, and a per-pane predicate is the shape
/// that gets that wrong: a linked or split pane still showing the site must
/// keep the volume its sibling stopped asking for.
#[test]
fn a_sibling_pane_still_drawing_radar_keeps_the_shared_volume() {
    let mut app = app_on_site(2);
    land_one_volume(&mut app);
    let held = resident(&app);
    assert!(held > 0, "premise: the arrival installed a volume");

    draw_radar(&mut app, 0, false);
    app.evict_unshown_scans();
    assert_eq!(
        resident(&app),
        held,
        "one pane switching its radar layer off took the volume out from \
         under a sibling that is still drawing it",
    );

    draw_radar(&mut app, 1, false);
    app.evict_unshown_scans();
    assert_eq!(
        resident(&app),
        0,
        "with neither pane drawing radar the volume is still resident",
    );
}

/// **The way back is the fetch's own.**
///
/// Nothing here re-acquires a volume by itself; what makes the drop payable is
/// that the site rejoins the set every radar fetch works in the moment the
/// layer comes back on. `Gui::live_sites` is that set — the chunk feed, both
/// notification subscriptions and the archive cadence are all driven from it —
/// and it asks the very predicate the retention now asks.
///
/// **Floor — pin `live_sites` to the enabled flag alone** rather than
/// `needs_radar_data`, or leave the site out of it: the way back is closed and
/// the volume this pass drops never comes back at all.
#[test]
fn switching_radar_back_on_puts_the_site_back_in_the_fetch_set() {
    let mut app = app_on_site(1);
    land_one_volume(&mut app);

    draw_radar(&mut app, 0, false);
    app.evict_unshown_scans();
    assert!(
        app.gui.live_sites().is_empty(),
        "premise: with the layer off the site is fetched for by nobody, which \
         is what makes dropping the volume a refetch rather than a churn",
    );

    draw_radar(&mut app, 0, true);
    assert_eq!(
        app.gui.live_sites(),
        vec![SITE.to_string()],
        "the layer came back on and its site did not rejoin the fetch set: \
         the pane would sit with no volume and nothing on any path would go \
         and get one.",
    );
}
