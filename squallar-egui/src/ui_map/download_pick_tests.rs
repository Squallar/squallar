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
        drawn.contains(DETAIL_LEVEL_HEADING),
        "the level list drew its figures without a heading over them"
    );
    // Users know what MB means. The panel states sizes, never their base.
    assert!(
        !drawn.contains("1,000,000") && !drawn.contains("decimal"),
        "the panel explained its byte denominator; drawn:\n{drawn}"
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

// ---------------------------------------------------------------------------
// The in-flight block
// ---------------------------------------------------------------------------

/// The bbox of the committed fixture, so a real engine has real bytes to move.
/// Only the id has to match the picked box — that is what the panel keys the
/// in-flight run on.
#[cfg(not(target_arch = "wasm32"))]
fn monaco_run_spec(area_id: String) -> crate::basemap_download::AreaSpec {
    crate::basemap_download::AreaSpec {
        area_id,
        west: 7.35,
        south: 43.70,
        east: 7.50,
        north: 43.78,
        max_zoom: 14,
    }
}

/// A source that answers `budget` reads and then parks forever.
///
/// The engine is serial, so a spent budget **freezes** the ledger: nothing
/// increments and no outcome lands, which is the only way to hold a run
/// genuinely in flight while the glass is read. A finished run would not do —
/// the frame settles it and the panel goes back to its start button.
///
/// The sibling in `ui_offline_areas::tests` is the same idea with a top-up
/// door it needs and this does not.
#[cfg(not(target_arch = "wasm32"))]
struct StalledSource {
    inner: crate::basemap_archive::FileRangeSource,
    budget: std::sync::atomic::AtomicI64,
}

#[cfg(not(target_arch = "wasm32"))]
impl crate::basemap_archive::RangeSource for StalledSource {
    async fn read_range(
        &self,
        offset: u64,
        length: usize,
    ) -> Result<Vec<u8>, crate::basemap_archive::RangeError> {
        use std::sync::atomic::Ordering;
        if self.budget.fetch_sub(1, Ordering::SeqCst) <= 0 {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        }
        self.inner.read_range(offset, length).await
    }
}

/// **This is the panel the user watched sit at "0 of 20 parts".** It now draws
/// a bar over the bytes with the exact byte figures beside it, and the segment
/// vocabulary appears nowhere on it.
///
/// The figures come from a real engine over the committed Monaco fixture, held
/// mid-run by a spent read budget so the reading is settled rather than raced.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn the_panels_in_flight_block_draws_bytes_and_never_a_part_count() {
    const MONACO: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/monaco.pmtiles");
    if std::fs::metadata(MONACO).is_err() {
        use std::io::Write as _;
        let mut stderr = std::io::stderr().lock();
        let _ = writeln!(
            stderr,
            "\n\
             ###########################################################################\n\
             ## SKIPPED, NOT PASSED: the_panels_in_flight_block_draws_bytes_and_never_a_part_count\n\
             ##   no PMTiles archive at squallar-egui/testdata/monaco.pmtiles\n\
             ##   this test asserted NOTHING.\n\
             ###########################################################################"
        );
        return;
    }

    let mut dir = std::env::temp_dir();
    dir.push(format!("squallar-download-panel-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);

    let mut h = harness();
    h.set_download_pick_armed(true);
    h.warm_up();
    let (centre, corner) = centre_and_corner(&h, 0, 60.0);
    h.drag_download_area(centre, corner);
    h.seed_download_size(SEEDED_CEILING, DetailLevel::TownsAndMainRoads, 47_000_000);
    h.warm_up();

    // The panel shows the one in-flight run whose id is the picked box's, so
    // the engine is started under exactly that id.
    let area_id = h
        .gui()
        .download_size
        .area_spec(h.gui().download_detail)
        .expect("a picked box has a spec")
        .area_id;
    let engine = crate::basemap_areas::ActiveDownload::start_with_segment_bytes(
        StalledSource {
            inner: crate::basemap_archive::FileRangeSource::open(std::path::Path::new(MONACO))
                .expect("the fixture opens"),
            // Enough to open the index, plan, and land some tile bytes; far
            // short of finishing the run.
            budget: std::sync::atomic::AtomicI64::new(8),
        },
        None::<(crate::basemap_archive::FileRangeSource, String)>,
        crate::basemap_download::FsSegmentStore::new(dir.clone()),
        monaco_run_spec(area_id),
        String::new(),
        h.ctx().clone(),
        120_000,
    );
    // Frozen is proved, not assumed: two readings a beat apart that agree.
    let start = std::time::Instant::now();
    let progress = loop {
        assert!(
            start.elapsed() < std::time::Duration::from_secs(30),
            "the fixture download never reached a held mid-run state: {:?}",
            engine.progress(),
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
        let first = engine.progress();
        if !first.denominator_known() || first.bytes_done.bytes() == 0 {
            continue;
        }
        std::thread::sleep(std::time::Duration::from_millis(150));
        if engine.progress() == first {
            break first;
        }
    };
    assert!(
        engine.outcome().is_none(),
        "the run finished; the panel would have settled it away",
    );

    h.gui_mut().active_download = Some(engine);
    h.warm_up();

    let drawn = h.painted_text_strings_in(panel_rect(&h));
    let bytes = crate::ui_download_area::progress_bytes_line(progress);
    assert!(
        drawn.iter().any(|text| text.contains(&bytes)),
        "the panel does not show the exact byte figures ({bytes:?}); drawn: {drawn:?}",
    );
    assert!(
        drawn.iter().any(|text| text.ends_with('%')),
        "the panel drew no bar over the bytes; drawn: {drawn:?}",
    );
    let vocabulary: Vec<&String> = drawn
        .iter()
        .filter(|text| {
            let folded = text.to_ascii_lowercase();
            folded.contains("part") || folded.contains("segment")
        })
        .collect();
    assert!(
        vocabulary.is_empty(),
        "the panel draws the segment vocabulary: {vocabulary:?}",
    );

    h.gui_mut().active_download = None;
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// The hillshade checkbox
// ---------------------------------------------------------------------------

/// The two halves' figures, chosen so their labels and their sum's label are
/// three different strings — without which "the figure moved" could pass on a
/// figure that did not.
const BASEMAP_FIGURE: u64 = 47_000_000;
const TERRAIN_FIGURE: u64 = 21_000_000;
const BASEMAP_LABEL: &str = "47 MB";
const COMBINED_LABEL: &str = "68 MB";

/// A harness with a box picked and both halves' figures in hand.
fn picked_with_both_figures() -> InputHarness {
    let mut h = harness();
    h.set_download_pick_armed(true);
    h.warm_up();
    let (centre, corner) = centre_and_corner(&h, 0, 60.0);
    h.drag_download_area(centre, corner);
    h.seed_download_size(
        SEEDED_CEILING,
        DetailLevel::TownsAndMainRoads,
        BASEMAP_FIGURE,
    );
    h.seed_download_terrain_size(
        SEEDED_CEILING,
        DetailLevel::TownsAndMainRoads,
        TERRAIN_FIGURE,
    );
    h.warm_up();
    h
}

/// Everything the download panel drew, as one string.
fn panel_text(h: &InputHarness) -> String {
    h.painted_text_strings_in(panel_rect(h)).join("\n")
}

/// **The cost of the hillshade is visible before it is spent.** The panel
/// carries a "Terrain shading" checkbox, and ticking it moves the level list's
/// exact figures to the combined one — not by a factor, but by the terrain
/// archive's own figure added.
#[test]
fn ticking_terrain_shading_moves_the_level_lists_exact_figure() {
    let mut h = picked_with_both_figures();

    assert!(
        !h.download_terrain_wanted(),
        "precondition: the terrain switch is off on a fresh map, so the checkbox \
         starts clear and the click below is a real change",
    );
    let before = panel_text(&h);
    assert!(
        before.contains(BASEMAP_LABEL),
        "the level list did not draw the basemap's own figure: {before}",
    );
    assert!(
        !before.contains(COMBINED_LABEL),
        "the level list drew the combined figure with the checkbox clear: {before}",
    );
    assert!(
        before.contains(TERRAIN_INCLUDE_LABEL),
        "the panel offers no way to ask for the hillshade: {before}",
    );

    let checkbox = panel_row(&h, TERRAIN_INCLUDE_LABEL);
    h.mouse_click(checkbox.center());
    h.warm_up();

    assert!(
        h.download_terrain_wanted(),
        "the checkbox did not take the click",
    );
    let after = panel_text(&h);
    assert!(
        after.contains(COMBINED_LABEL),
        "the figure did not move to the combined one when the checkbox was ticked, \
         which is the tell for an estimate: {after}",
    );
    assert!(
        !after.contains(BASEMAP_LABEL),
        "the level list still draws the basemap figure alone after the checkbox was \
         ticked: {after}",
    );

    // And back. Untick returns the basemap's own figure rather than a stale sum.
    h.mouse_click(checkbox.center());
    h.warm_up();
    let cleared = panel_text(&h);
    assert!(
        cleared.contains(BASEMAP_LABEL) && !cleared.contains(COMBINED_LABEL),
        "unticking kept the combined figure: {cleared}",
    );
}

/// **The default is what the user is actually looking at.** With terrain
/// shading switched on in the Base Map inspector, a box drawn afterwards
/// starts with the checkbox ticked and quotes the combined figure — nobody has
/// to remember to ask for what is already on the glass.
#[test]
fn the_checkbox_starts_where_the_terrain_switch_is() {
    let mut h = harness();
    h.set_terrain_from_basemap_inspector(true);
    h.warm_up();

    h.set_download_pick_armed(true);
    h.warm_up();
    let (centre, corner) = centre_and_corner(&h, 0, 60.0);
    h.drag_download_area(centre, corner);
    h.seed_download_size(
        SEEDED_CEILING,
        DetailLevel::TownsAndMainRoads,
        BASEMAP_FIGURE,
    );
    h.seed_download_terrain_size(
        SEEDED_CEILING,
        DetailLevel::TownsAndMainRoads,
        TERRAIN_FIGURE,
    );
    h.warm_up();

    assert!(
        h.download_terrain_wanted(),
        "the download did not follow the terrain switch the user has on",
    );
    let drawn = panel_text(&h);
    assert!(
        drawn.contains(COMBINED_LABEL),
        "the quoted figure does not include the shading the map is showing: {drawn}",
    );
}

/// A ticked checkbox is a **choice**, and it reopens exactly as it was left —
/// including against a terrain switch that says otherwise.
#[test]
fn the_terrain_choice_survives_a_reopen() {
    let mut h = picked_with_both_figures();
    let checkbox = panel_row(&h, TERRAIN_INCLUDE_LABEL);
    h.mouse_click(checkbox.center());
    h.warm_up();
    assert!(
        h.download_terrain_wanted(),
        "the checkbox did not take the click"
    );

    let store = squallar_kv::MemoryKvStore::default();
    h.gui_mut().save_ui_config(&store);

    let mut reopened = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    assert!(
        reopened.gui_mut().load_ui_config(&store),
        "the saved config must load"
    );
    reopened.warm_up();
    assert!(
        reopened.download_terrain_wanted(),
        "the hillshade choice did not survive a reopen, so the window did not reopen \
         as it was left",
    );
    assert!(
        !reopened.overlay_enabled_on(0, &known::TERRAIN),
        "precondition: the terrain switch is off in the reopened window, so the true \
         above is the CHOICE rather than the switch",
    );
}
