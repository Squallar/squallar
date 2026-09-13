//! **What decides that a site's radar data is wanted at all.**
//!
//! Four places used to answer that question and none of them asked whether any
//! pane was drawing radar: [`Gui::live_sites`], which the chunk feed, both
//! notification subscriptions and the archive push all work in; the archive
//! cadence's own inline copy of that walk in `check_auto_polls`; the session's
//! first fetch in the same function; and `App::check_archive_for`, which reuses
//! `live_sites`. On the archived FLOOR legs of 2026-09-07 a single pane booted
//! with all eighteen layers at `enabled: false` still subscribed to KTLX chunk
//! *and* archive notifications one second after boot, downloaded a 6.9 MB
//! volume, and two seconds after boot read `still scans 103,303,680 B` and a
//! `chunk feed` climbing from 17.3 MB past 95 MB.
//!
//! The predicate they now share is [`PaneState::needs_radar_data`], which is
//! [`PaneState::is_overlay_enabled`] with [`known::RADAR`] — the same accessor
//! the draw path asks — widened by exactly one case, pinned below.

use super::*;
use squallar_source::id::known;

/// **The boundary that matters more than the bug.**
///
/// `is_overlay_enabled` is `slot(id).is_some_and(|slot| slot.enabled)`, so a
/// pane whose Radar slot has never been minted answers **false**. If a fresh
/// install were in that state, gating the fetch on it would give a first-run
/// user no radar at all — a far worse defect than the one being fixed.
///
/// It is not in that state, and this is the pin: `Gui::new` ends with
/// `initialize_pane_enabled`, and so does `set_pane_count` for every pane it
/// mints and `load_ui_config` after every load, so a pane always reaches its
/// first frame holding a Radar slot at the handler's own default.
#[test]
fn a_first_run_pane_holds_a_radar_slot_and_asks_for_its_data() {
    let gui = Gui::new();
    let pane = gui.pane(0).expect("a fresh Gui has a pane");

    assert!(
        pane.is_overlay_enabled(&known::RADAR),
        "a first-run pane's Radar slot is not enabled, so gating the fetch on \
         it would leave a new install with no radar data at all. Either \
         `Gui::new` stopped calling `initialize_pane_enabled` or the radar \
         handler's `default_enabled` went false",
    );
    assert!(
        pane.needs_radar_data(),
        "a first-run pane does not ask for radar data",
    );
    assert!(
        gui.live_sites().iter().any(|s| s == pane.site()),
        "a first-run pane's site is not live, so nothing would fetch it a \
         volume: {:?}",
        gui.live_sites(),
    );
}

/// **And a pane the user opens later, which is now a different mechanism.**
///
/// Until `65a86dbc3` a grown pane got its stack from the layer-link fan-out —
/// new panes were born linked, so the active pane's whole stack was copied
/// onto them within a frame or two. That commit starts `layer_link` **off**,
/// which takes the fan-out away and leaves `initialize_pane_enabled` as the
/// only thing that puts layers on a new pane. `Gui::set_pane_count` calls it
/// two lines after it pushes, so the seeding still happens — but it is now
/// load-bearing where it used to be belt-and-braces, and this lane's fetch
/// gate reads exactly what it writes.
///
/// So the first-run pin above is no longer the whole boundary: "the app
/// started" and "the user opened a second pane" reach an enabled Radar slot
/// by different routes now, and only one of them was covered.
#[test]
fn a_pane_opened_later_also_holds_a_radar_slot_and_asks_for_its_data() {
    let mut gui = Gui::new();
    gui.set_pane_count_for_test(2);
    gui.pane_mut(1)
        .expect("the layout just grew to two panes")
        .set_site("KOUN".to_string());
    gui.pane_mut(1).expect("pane 1").set_viewing_live(true);

    let grown = gui.pane(1).expect("pane 1");
    assert!(
        grown.is_overlay_enabled(&known::RADAR),
        "a pane opened after startup holds no enabled Radar slot, so this \
         lane's fetch gate would leave it with no radar data at all. With \
         `layer_link` starting off there is no fan-out to cover for a missing \
         `initialize_pane_enabled`",
    );
    assert!(
        grown.needs_radar_data(),
        "a pane opened after startup does not ask for radar data",
    );
    assert!(
        gui.live_sites().iter().any(|s| s == "KOUN"),
        "the second pane's site is not live, so nothing would fetch it a \
         volume: {:?}",
        gui.live_sites(),
    );
}

/// The defect: a map pane with the layer off kept its site live.
#[test]
fn a_map_pane_with_radar_off_is_not_a_live_site() {
    let mut gui = Gui::new();
    {
        let pane = gui.pane_mut(0).expect("a fresh Gui has a pane");
        pane.set_site("KTLX".to_string());
        pane.set_viewing_live(true);
        assert!(
            pane.is_map(),
            "premise: a fresh pane must be a map pane, or this test is about \
             the section case below instead",
        );
    }
    assert_eq!(
        gui.live_sites(),
        vec!["KTLX".to_string()],
        "premise: the pane must be live before the layer is switched off, or \
         the assertion after it is satisfied by it never having been live",
    );

    gui.pane_mut(0)
        .expect("pane 0")
        .set_overlay_enabled(known::RADAR, false);

    assert_eq!(
        gui.live_sites(),
        Vec::<String>::new(),
        "a map pane with its radar layer switched off still reports its site \
         as live, so both notification sockets open for it and the chunk feed \
         keeps assembling volumes nothing draws",
    );
}

/// **The one case the fetch question is wider than the draw question.**
///
/// A cross-section pane reads the volume without the map ever drawing a radar
/// image over its tiles, and the Radar toggle does not speak for it —
/// `render_radar_controls` hides the product picker only for map panes for
/// exactly this reason. Gating the fetch on the raw flag would starve it.
#[test]
fn a_pane_that_draws_no_map_still_needs_its_volume() {
    let mut gui = Gui::new();
    let pane = gui.pane_mut(0).expect("a fresh Gui has a pane");
    pane.set_site("KTLX".to_string());
    pane.set_viewing_live(true);
    pane.set_view(squallar_radar::types::RenderView::CrossSection);
    pane.set_overlay_enabled(known::RADAR, false);

    assert!(
        !pane.is_map(),
        "premise: the conversion did not take, so the assertion below is the \
         map case again",
    );
    assert!(
        !pane.is_overlay_enabled(&known::RADAR),
        "premise: the flag must really be off, or `needs_radar_data` is \
         answering the easy way",
    );
    assert!(
        pane.needs_radar_data(),
        "a cross-section pane stopped needing its volume because a toggle \
         about what the *map* draws was switched off; it draws no map",
    );
    assert_eq!(
        gui.live_sites(),
        vec!["KTLX".to_string()],
        "the site feeding a cross-section pane went dark",
    );
}

/// And the layer coming back on makes the site live again — the half that
/// keeps the gate from being a one-way door.
#[test]
fn switching_the_layer_back_on_makes_the_site_live_again() {
    let mut gui = Gui::new();
    {
        let pane = gui.pane_mut(0).expect("a fresh Gui has a pane");
        pane.set_site("KTLX".to_string());
        pane.set_viewing_live(true);
        pane.set_overlay_enabled(known::RADAR, false);
    }
    assert_eq!(
        gui.live_sites(),
        Vec::<String>::new(),
        "premise: the site must really have gone quiet first",
    );

    gui.pane_mut(0)
        .expect("pane 0")
        .set_overlay_enabled(known::RADAR, true);

    assert_eq!(
        gui.live_sites(),
        vec!["KTLX".to_string()],
        "switching the radar layer back on left the site quiet, so nothing \
         would ever fetch it a volume again",
    );
}
