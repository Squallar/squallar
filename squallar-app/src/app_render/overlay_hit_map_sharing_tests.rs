//! **One hit map per arriving raster, however many panes draw it.**
//!
//! A rasterizer answers one dispatch with one picture and one hit map, and
//! `poll_overlay_render_results` files that answer on every pane that still
//! wants it. The map used to be an owned
//! [`squallar_overlays::render::rasterize::HitMap`], so each of those panes
//! got a **deep copy** of an `FxHashMap<u32, Vec<u32>>` with one entry per
//! quarter-cell the layer touched — built on the frame thread, once per pane
//! per arrival, and freed there again when the next raster replaced it.
//!
//! Measured on scene E2 (KTLX, every layer on, a playing one-hour loop, one
//! destination pane, RTX 3090 / Vulkan on Xvfb), two legs an arm: **215 and
//! 205 deep copies a leg, moving 8.4 M and 16.0 M ids**, against none once the
//! handle is shared. The denominator is arrivals that CARRY a hit map — about
//! 17% of the 1224 and 1187 rasters that arrived — and one pane is the FLOOR,
//! because the copy was per destination pane. A campaign instrument that never
//! landed charged the copy **49% of the whole `Apply` pump walk**, worst single
//! copy 3489 us against a p99 interact-frame service bar of 4 ms.
//!
//! These suites hold the two halves of the remedy: the panes share one
//! allocation, and sharing it does not change what a click answers.

use super::*;
use squallar_geo::GeoBounds;
use squallar_overlays::render::overlay_state::HitItems;
use squallar_overlays::render::rasterize::{HitCells, HitMap};
use squallar_source::id::known;
use std::collections::BTreeSet;

/// The raster's pixel size. The hit grid is a quarter of it per axis, so this
/// is also what `HitCells::new` is handed.
const W: u32 = 8;
const H: u32 = 8;

/// Where the fixture records its two items, in raster pixels. Chosen in two
/// different quarter-cells so a click can tell them apart.
const FIRST_PX: (f32, f32) = (1.0, 1.0);
const SECOND_PX: (f32, f32) = (5.0, 5.0);

fn bounds() -> GeoBounds {
    GeoBounds {
        min_lat: 34.0,
        max_lat: 36.0,
        min_lon: -99.0,
        max_lon: -97.0,
    }
}

/// The UV a click at pixel `px` lands on, in `HitMap::hit_test`'s coordinates.
fn uv(px: (f32, f32)) -> (f32, f32) {
    (px.0 / W as f32, px.1 / H as f32)
}

#[derive(Debug)]
struct Stub(&'static str);

impl squallar_overlays::render::overlay_state::OverlayItem for Stub {
    fn layer_id(&self) -> squallar_source::id::LayerId {
        known::NWS_ALERTS
    }

    fn popup_content(
        &self,
        _prefs: &squallar_units::UserPreferences,
    ) -> squallar_overlays::render::overlay_state::PopupContent {
        squallar_overlays::render::overlay_state::PopupContent {
            title: self.0.to_owned(),
            accent_rgb: [80, 80, 80],
            width: 300.0,
            sections: Vec::new(),
            actions: Vec::new(),
        }
    }

    fn matches(&self, _other: &dyn squallar_overlays::render::overlay_state::OverlayItem) -> bool {
        false
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// The two names the fixture's items answer to, in the id order the cells
/// record — which is the order `HitMap::from_cells` zips on.
const NAMES: [&str; 2] = ["first", "second"];

/// One rasterizer's hit map: two items in two quarter-cells.
fn a_hit_map() -> Arc<HitMap> {
    let mut cells = HitCells::new(W, H);
    cells.record(FIRST_PX.0, FIRST_PX.1, 0);
    cells.record(SECOND_PX.0, SECOND_PX.1, 1);
    let items: HitItems = NAMES
        .iter()
        .copied()
        .map(|name| {
            Arc::new(Stub(name)) as Arc<dyn squallar_overlays::render::overlay_state::OverlayItem>
        })
        .collect();
    Arc::new(HitMap::from_cells(cells, &items))
}

/// One raster, answered to `pane_indices`, carrying `hit_map`.
fn deliver(
    app: &mut crate::app::App,
    ctx: &egui::Context,
    pane_indices: Vec<usize>,
    hit_map: Option<Arc<HitMap>>,
) {
    // The mark a real dispatch leaves: the poller files a raster only while
    // the cache is still waiting for that very dispatch.
    for &idx in &pane_indices {
        if let Some(pane) = app.gui.pane_mut(idx) {
            pane.overlay_cache_mut(&known::NWS_ALERTS).renders.record(
                squallar_egui::overlay_cache::RenderTicket::whole(7, bounds()),
            );
        }
    }
    let image = Arc::new(egui::ColorImage::from_rgba_unmultiplied(
        [W as usize, H as usize],
        &[9u8; (W * H * 4) as usize],
    ));
    app.channels
        .overlay_render_sender
        .send(crate::channels::OverlayRenderResponse {
            picture: Some(crate::channels::OverlayPicture::Painted(image)),
            geo_bounds: bounds(),
            overlay_kind: known::NWS_ALERTS,
            generation: 7,
            pane_indices,
            zoom: 32,
            hit_map,
            // The pane's live picture, not a loop frame's.
            frame: None,
        })
        .expect("the receiver lives on the App");
    app.poll_overlay_render_results(ctx);
}

/// The hit map the poller placed on `pane_idx`.
fn placed_map(app: &mut crate::app::App, pane_idx: usize) -> Arc<HitMap> {
    app.gui
        .pane_mut(pane_idx)
        .expect("pane exists")
        .overlay_cache_mut(&known::NWS_ALERTS)
        .current()
        .expect("the poller placed an overlay on this pane")
        .hit_map
        .clone()
        .expect("the delivered raster carried a hit map")
}

/// **The count gate.** Distinct allocations, against the number of panes that
/// took the raster — never against a literal 1, so the assertion states the
/// ratio it means.
#[test]
fn one_arriving_raster_leaves_one_hit_map_however_many_panes_draw_it() {
    let ctx = egui::Context::default();
    let panes = 3;
    let mut app = crate::app::tests::n_pane_app(panes, "KTLX");
    let arrived = a_hit_map();
    let arrived_at = Arc::as_ptr(&arrived);

    deliver(
        &mut app,
        &ctx,
        (0..panes).collect(),
        Some(Arc::clone(&arrived)),
    );

    // The handles are kept alive for the whole comparison: a pointer read off
    // a temporary would be compared after its allocation could have been
    // reused, and two rebuilt maps could then read as one.
    let held: Vec<Arc<HitMap>> = (0..panes).map(|idx| placed_map(&mut app, idx)).collect();
    assert_eq!(
        held.len(),
        panes,
        "premise: every pane the raster named took it",
    );
    let distinct: BTreeSet<*const HitMap> = held.iter().map(Arc::as_ptr).collect();
    assert_eq!(
        distinct.len(),
        1,
        "{panes} panes drew one arriving raster and hold {} hit maps between \
         them; one arriving map must leave one allocation, not one per pane",
        distinct.len(),
    );
    assert_eq!(
        distinct.into_iter().next().expect("just counted one"),
        arrived_at,
        "the map the panes hold is not the map that arrived, so it was rebuilt",
    );
}

/// **The correctness gate**, split from the count above on purpose: sharing
/// one allocation is worthless if a pane past the first answers a click
/// differently. Every pane is asked the same two clicks.
#[test]
fn every_pane_answers_a_click_with_the_item_the_rasterizer_recorded_there() {
    let ctx = egui::Context::default();
    let panes = 3;
    let mut app = crate::app::tests::n_pane_app(panes, "KTLX");

    deliver(&mut app, &ctx, (0..panes).collect(), Some(a_hit_map()));

    for (px, expected) in [(FIRST_PX, NAMES[0]), (SECOND_PX, NAMES[1])] {
        let (u, v) = uv(px);
        let answers: Vec<String> = (0..panes)
            .map(|idx| {
                let hits = placed_map(&mut app, idx).hit_test(u, v);
                assert_eq!(
                    hits.len(),
                    1,
                    "pane {idx} answered {} items at ({u}, {v}); the fixture \
                     recorded exactly one there",
                    hits.len(),
                );
                hits[0]
                    .as_any()
                    .downcast_ref::<Stub>()
                    .expect("the fixture's own item type came back")
                    .0
                    .to_owned()
            })
            .collect();
        let distinct: BTreeSet<&String> = answers.iter().collect();
        assert_eq!(
            distinct.len(),
            1,
            "the {panes} panes answered the click at ({u}, {v}) with {answers:?}; \
             one raster's hit map answers one question",
        );
        assert_eq!(
            answers[0], expected,
            "the click at ({u}, {v}) answered {} where the rasterizer recorded \
             {expected}",
            answers[0],
        );
    }
}

/// A raster with no hit map places none — the poller does not invent one, and
/// the `None` arm is what most layers take.
#[test]
fn a_raster_that_recorded_no_hits_places_no_hit_map() {
    let ctx = egui::Context::default();
    let mut app = crate::app::tests::n_pane_app(1, "KTLX");

    deliver(&mut app, &ctx, vec![0], None);

    assert!(
        app.gui
            .pane_mut(0)
            .expect("pane exists")
            .overlay_cache_mut(&known::NWS_ALERTS)
            .current()
            .expect("the poller placed an overlay on this pane")
            .hit_map
            .is_none(),
        "a raster that carried no hit map left one on the pane",
    );
}
