//! The armed offline-download pick, driven through the real UI.
//!
//! The suite next door (`region_pick_tests`) drives the *other* arm of the
//! same drag and is untouched by this one — which is the point: one gesture,
//! two arms, and neither one's behaviour is a special case inside the other.

use super::*;
use crate::input_harness::InputHarness;
use crate::ui_download_area::DetailLevel;

const FRAME_DT: f64 = 1.0 / 60.0;

/// The shipped archive's ceiling, used where a test needs the probe to have
/// one. Seeded rather than read, because the harness has no network; the
/// ceiling actually coming off the header is pinned in
/// `ui_download_area::tests::the_deepest_level_stores_to_the_archives_own_ceiling`.
const SEEDED_CEILING: u8 = 14;

/// Where the level list drew, so a click lands on the panel's row rather than
/// on the box chip that carries the same words.
fn panel_rect(h: &InputHarness) -> egui::Rect {
    h.area_rect(egui::Id::new("download_area_panel"))
        .expect("the level list draws while a box is picked")
}

/// The panel row whose text carries `needle` — the rows read
/// "<level>  -  <size>", so an exact match would pin the composition rather
/// than the row.
fn panel_row(h: &InputHarness, needle: &str) -> egui::Rect {
    let panel = panel_rect(h);
    h.painted_text_rects()
        .into_iter()
        .find(|(rect, text)| text.contains(needle) && panel.contains(rect.center()))
        .map(|(rect, _)| rect)
        .unwrap_or_else(|| {
            panic!(
                "no panel row carries {needle:?}; drawn: {:?}",
                h.painted_text_strings_in(panel)
            )
        })
}

fn harness() -> InputHarness {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.load_scan("KTLX");
    h.warm_up();
    h
}

/// Two points on pane `idx` a drag between will make a box of roughly
/// `half_km` half-width: the pane's centre, and the ground `half_km` north of
/// it, projected back to the screen.
fn centre_and_corner(h: &InputHarness, idx: usize, half_km: f64) -> (egui::Pos2, egui::Pos2) {
    let centre_pos = h.pane_rects()[idx].center();
    let centre = h.ground_at(idx, centre_pos);
    let corner = squallar_geo::GeoPoint {
        lat: centre.y() + half_km / squallar_geo::KM_PER_DEGREE_LAT,
        lon: centre.x(),
    };
    (centre_pos, h.screen_of(idx, corner))
}

/// **The happy path**: arming the mode and dragging a square gives a picked
/// box, and the mode disarms itself once it has one.
#[test]
fn a_drag_on_an_armed_map_pane_picks_the_ground_to_download() {
    let mut h = harness();
    h.set_download_pick_armed(true);
    h.warm_up();

    let (centre, corner) = centre_and_corner(&h, 0, 60.0);
    h.drag_download_area(centre, corner);

    let picked = h
        .download_pick()
        .expect("a drag on an armed map pane must pick a box");
    assert!(
        (picked.half_width_km - 60.0).abs() < 3.0,
        "the box committed at {} km half-width rather than the 60 km dragged",
        picked.half_width_km
    );
    assert!(
        !h.download_pick_armed(),
        "the mode stayed armed after committing a box, so the next pan would draw another"
    );
    assert!(
        picked
            .area_spec(SEEDED_CEILING, DetailLevel::EveryStreet)
            .is_some(),
        "the picked box describes no area"
    );
}

/// **The whole reason the drag was decoupled.** A box the voxel resampler's
/// bounds would refuse — under its 10 km floor — is an ordinary download area,
/// driven through the same real gesture on the same real map.
#[test]
fn a_box_under_the_resamplers_floor_downloads_but_does_not_pick_a_3d_region() {
    let half_km = squallar_radar::voxel::MIN_HALF_WIDTH_KM / 4.0;

    let mut refused = harness();
    refused.set_region_pick_armed(true);
    refused.warm_up();
    let (centre, corner) = centre_and_corner(&refused, 0, half_km);
    refused.drag_region(centre, corner);
    assert!(
        refused.volume_region(1).is_none() && refused.volume_region(0).is_none(),
        "the 3D pick accepted a box under the resampler's own minimum"
    );

    let mut accepted = harness();
    accepted.set_download_pick_armed(true);
    accepted.warm_up();
    let (centre, corner) = centre_and_corner(&accepted, 0, half_km);
    accepted.drag_download_area(centre, corner);
    let picked = accepted
        .download_pick()
        .expect("the download arm must accept a town-sized box the resampler would refuse");
    assert!(
        picked.half_width_km < squallar_radar::voxel::MIN_HALF_WIDTH_KM,
        "the box committed at {} km, which is not under the floor this test is about",
        picked.half_width_km
    );
}

/// The three modal drags are mutually exclusive: arming any one un-arms the
/// other two.
#[test]
fn arming_the_download_pick_disarms_the_other_two_drags() {
    let mut h = harness();

    h.set_section_draw_armed(true);
    h.set_download_pick_armed(true);
    h.warm_up();
    assert!(h.download_pick_armed() && !h.section_draw_armed());

    h.set_region_pick_armed(true);
    h.warm_up();
    assert!(
        h.region_pick_armed() && !h.download_pick_armed(),
        "arming the 3D pick left the download arm lit as well"
    );

    h.set_download_pick_armed(true);
    h.warm_up();
    assert!(
        h.download_pick_armed() && !h.region_pick_armed(),
        "arming the download pick left the 3D arm lit as well"
    );
}

/// The armed chip names the download arm's own gesture, not the 3D pick's.
#[test]
fn the_armed_chip_names_the_download_gesture() {
    let mut h = harness();
    h.set_download_pick_armed(true);
    h.warm_up();

    let pane = h.pane_rects()[0];
    assert!(
        h.text_painted_in(pane, crate::ui_download_area::DOWNLOAD_ARM_HINT),
        "the armed download mode drew no hint chip; painted: {:?}",
        h.painted_text_strings_in(pane)
    );
    assert!(
        !h.text_painted_in(pane, REGION_ARM_HINT),
        "the download arm drew the 3D pick's hint"
    );
}

/// The level list draws all three names, each beside the exact figure the
/// probe measured for it, and picking one moves the selection.
#[test]
fn the_level_list_draws_three_depths_with_the_exact_figure_beside_each() {
    let mut h = harness();
    h.set_download_pick_armed(true);
    h.warm_up();
    let (centre, corner) = centre_and_corner(&h, 0, 60.0);
    h.drag_download_area(centre, corner);

    h.seed_download_size(SEEDED_CEILING, DetailLevel::CitiesAndHighways, 12_000_000);
    h.seed_download_size(SEEDED_CEILING, DetailLevel::TownsAndMainRoads, 47_000_000);
    h.seed_download_size(SEEDED_CEILING, DetailLevel::EveryStreet, 310_000_000);
    h.warm_up();

    let screen = h.screen_rect();
    let drawn = h.painted_text_strings_in(screen).join("\n");
    for level in crate::ui_download_area::DETAIL_LEVELS {
        assert!(
            drawn.contains(level.label()),
            "the level list did not draw {:?}; drawn:\n{drawn}",
            level.label()
        );
    }
    for figure in ["12 MB", "47 MB", "310 MB"] {
        assert!(
            drawn.contains(figure),
            "the level list did not draw the exact figure {figure}; drawn:\n{drawn}"
        );
    }
    assert!(
        drawn.contains(DETAIL_LEVEL_HEADING) && drawn.contains(DECIMAL_SIZES_NOTE),
        "the list drew sizes without naming the denominator once above them"
    );
}

/// **Quota-short: quantities and an action, never an apology.** The panel
/// states what the chosen level needs, what the origin has, and offers the
/// deepest level that fits.
#[test]
fn a_level_that_will_not_fit_shows_both_quantities_and_a_level_that_does() {
    let mut h = harness();
    h.set_download_pick_armed(true);
    h.warm_up();
    let (centre, corner) = centre_and_corner(&h, 0, 60.0);
    h.drag_download_area(centre, corner);

    h.seed_download_size(SEEDED_CEILING, DetailLevel::CitiesAndHighways, 12_000_000);
    h.seed_download_size(SEEDED_CEILING, DetailLevel::TownsAndMainRoads, 47_000_000);
    h.seed_download_size(SEEDED_CEILING, DetailLevel::EveryStreet, 310_000_000);
    h.set_download_quota(Some(crate::basemap_download::OfflineQuota {
        usage: Some(20_000_000),
        quota: Some(200_000_000),
    }));
    h.warm_up();

    // The chosen level fits 180 MB free, so nothing is short yet.
    let screen = h.screen_rect();
    assert!(
        !h.painted_text_strings_in(screen)
            .join("\n")
            .contains("free"),
        "a level that fits was reported short"
    );

    // Choose the deepest level, which does not.
    let deepest = panel_row(&h, DetailLevel::EveryStreet.label());
    h.mouse_click(deepest.center());
    h.warm_up();

    let drawn = h.painted_text_strings_in(screen).join("\n");
    assert_eq!(
        h.download_detail(),
        DetailLevel::EveryStreet,
        "clicking a level did not select it"
    );
    for quantity in ["310 MB", "180 MB"] {
        assert!(
            drawn.contains(quantity),
            "the shortfall did not state {quantity}; drawn:\n{drawn}"
        );
    }
    assert!(
        drawn.contains(&crate::ui_download_area::shortfall_action_label(
            DetailLevel::TownsAndMainRoads
        )),
        "the shortfall offered no level that fits; drawn:\n{drawn}"
    );
    for apology in ["sorry", "unfortunately", "cannot be downloaded"] {
        assert!(
            !drawn.to_lowercase().contains(apology),
            "the shortfall reads as an apology rather than as quantities: {drawn}"
        );
    }

    // And the action is an action: taking it moves the selection to the level
    // that fits.
    let action = panel_row(
        &h,
        &crate::ui_download_area::shortfall_action_label(DetailLevel::TownsAndMainRoads),
    );
    h.mouse_click(action.center());
    h.warm_up();
    assert_eq!(
        h.download_detail(),
        DetailLevel::TownsAndMainRoads,
        "the offered action did not switch to the level that fits"
    );
}

/// Backing out clears the box, and the box only — a *finished* area's record
/// is not the box's to drop.
#[test]
fn backing_out_clears_the_picked_box() {
    let mut h = harness();
    h.set_download_pick_armed(true);
    h.warm_up();
    let (centre, corner) = centre_and_corner(&h, 0, 60.0);
    h.drag_download_area(centre, corner);
    assert!(h.download_pick().is_some());

    assert!(
        h.gui_mut().dismiss_top_layer(),
        "back had something to close"
    );
    h.frames_for(2, FRAME_DT);
    assert!(
        h.download_pick().is_none(),
        "back left the picked box on the map"
    );
}

/// **Reopen is exactly 1:1**: the arm, the box and the level all come back.
#[test]
fn the_arm_the_box_and_the_level_all_survive_a_reopen() {
    let mut h = harness();
    h.set_download_pick_armed(true);
    h.warm_up();
    let (centre, corner) = centre_and_corner(&h, 0, 60.0);
    h.drag_download_area(centre, corner);
    h.seed_download_size(SEEDED_CEILING, DetailLevel::EveryStreet, 310_000_000);
    h.warm_up();
    let row = panel_row(&h, DetailLevel::EveryStreet.label());
    h.mouse_click(row.center());
    h.warm_up();
    h.set_download_pick_armed(true);
    h.warm_up();

    let picked = h.download_pick().expect("a box");
    let store = squallar_kv::MemoryKvStore::default();
    h.gui_mut().save_ui_config(&store);

    let mut reopened = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    assert!(
        reopened.gui_mut().load_ui_config(&store),
        "the saved config must load"
    );
    reopened.warm_up();

    assert_eq!(
        reopened.download_pick(),
        Some(picked),
        "the picked box did not survive a reopen"
    );
    assert_eq!(
        reopened.download_detail(),
        DetailLevel::EveryStreet,
        "the chosen detail level did not survive a reopen"
    );
    assert!(
        reopened.download_pick_armed(),
        "the armed download mode did not survive a reopen, so the window did not reopen as \
         it was left"
    );
}
