//! **The draw fork** (WI-6): which picture a non-radar textured layer puts on
//! the map.
//!
//! End-to-end and not hand-armed — every assertion below reads the textured
//! quad the frame really painted inside the pane rect, through the same
//! `render_map_pane` a user sees. The identity of the texture is what is read,
//! never "something was drawn": the whole defect is a *wrong* picture on the
//! glass, and a wrong picture is still a picture.

use super::{InputHarness, PaintedImage};
use crate::overlay_cache::OverlayTextureData;
use crate::pane::{LayerTimeState, LoopFrame, LoopFrameImage, LoopPhase, TimeMode};
use squallar_source::id::{LayerId, known};

/// The forecast layer this work item exists for. Registered in every build and
/// `RenderMode::Texture`, so the generic arm of `render_map_pane` is what draws
/// it — which is the arm under test.
pub(super) const LAYER: LayerId = known::MODEL_DATA;

pub(super) fn ts(minutes: i64) -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 8, 22)
        .unwrap()
        .and_hms_opt(12, 0, 0)
        .unwrap()
        + chrono::Duration::minutes(minutes)
}

/// A 1x1 texture named `name`, placed over whatever ground pane 0 is showing.
///
/// The placement is read back off the live projector rather than invented, so
/// the quad really does land inside the pane rect — a raster placed off-screen
/// is dropped by `draw_overlay_texture` and every assertion below would then
/// pass over an empty list.
pub(super) fn raster(h: &InputHarness, name: &str) -> OverlayTextureData {
    let rect = h.pane_rects()[0];
    let nw = h.ground_at(0, rect.min);
    let se = h.ground_at(0, rect.max);
    let texture = h.ctx.load_texture(
        name.to_owned(),
        egui::ColorImage::filled([1, 1], egui::Color32::RED),
        egui::TextureOptions::default(),
    );
    OverlayTextureData {
        texture,
        placed: squallar_geo::PlacedRaster::of(squallar_geo::GeoBounds {
            min_lat: nw.y().min(se.y()),
            max_lat: nw.y().max(se.y()),
            min_lon: nw.x().min(se.x()),
            max_lon: nw.x().max(se.x()),
        }),
        data_generation: 0,
        render_zoom: 0,
        width: 1,
        height: 1,
        radar_meta: None,
        hit_map: None,
    }
}

/// Which textures the last frame painted inside pane 0.
pub(super) fn painted(h: &InputHarness) -> Vec<egui::TextureId> {
    h.painted_images_in(h.pane_rects()[0])
        .iter()
        .map(|image: &PaintedImage| image.texture)
        .collect()
}

/// The forecast layer on, its **live** raster in the cache, and a three-frame
/// timeline of its own beside it — the state a model loop reaches once its
/// frames have landed.
///
/// The live raster is deliberately a *different* picture from every frame: it
/// stands for the instant the layer last rasterized for the map, and leaving it
/// on the glass while the playhead sits elsewhere is the defect this item
/// closes.
pub(super) fn model_loop(h: &mut InputHarness) -> (egui::TextureId, Vec<egui::TextureId>) {
    h.gui_mut().enable_overlay_for_test(&LAYER);
    h.warm_up();

    let live = raster(h, "live");
    let live_id = live.texture.id();
    h.gui_mut().panes_mut()[0]
        .overlay_cache_mut(&LAYER)
        .show(live);

    let frames: Vec<OverlayTextureData> = (0..3).map(|i| raster(h, &format!("frame{i}"))).collect();
    let frame_ids: Vec<egui::TextureId> = frames.iter().map(|f| f.texture.id()).collect();

    let mut ls = LayerTimeState::new();
    ls.phase = LoopPhase::Playing;
    ls.frames = frames
        .into_iter()
        .enumerate()
        .map(|(i, image)| LoopFrame {
            timestamp: ts(i as i64 * 15),
            image: Some(LoopFrameImage::Overlay(image)),
            render_in_flight: false,
            render_failed: false,
        })
        .collect();
    *h.gui_mut().panes_mut()[0].time_state_mut(&LAYER) = ls;
    h.warm_up();
    (live_id, frame_ids)
}

/// Move pane 0's clock to `at` and run a frame.
pub(super) fn scrub_to(h: &mut InputHarness, at: chrono::NaiveDateTime) {
    h.gui_mut().panes_mut()[0].set_time_mode(TimeMode::AsOf(at));
    h.warm_up();
}

/// **The correctness pin of the whole loop-generalisation effort.**
///
/// A forecast layer that is animating paints the frame under its own playhead,
/// and paints a *different* one when the playhead moves. Without the fork the
/// live cache answers every time and the previous hour's forecast stays on the
/// glass, unlabelled, presented as the answer — which is why the assertion is
/// on the texture's identity and not on "a quad was painted".
///
/// **Floor A** — replace `overlay_texture_on_screen`'s body with
/// `self.overlay_cache(id).and_then(|c| c.current())` (i.e. delete the fork) and
/// every stamp below paints `live`: three failures, one per playhead position.
#[test]
fn a_forecast_loop_paints_the_frame_under_its_playhead_and_not_the_last_raster() {
    let mut h = InputHarness::new();
    let (live, frames) = model_loop(&mut h);

    // Non-triviality for the fixture itself: four distinct pictures, or
    // "identity changed" below could be one texture reused.
    let mut distinct: Vec<egui::TextureId> = frames.clone();
    distinct.push(live);
    distinct.sort_by_key(|id| format!("{id:?}"));
    distinct.dedup();
    assert_eq!(
        distinct.len(),
        4,
        "fixture: the live raster and the three frames must be four different \
         textures"
    );

    let mut seen = Vec::new();
    for (i, expected) in frames.iter().enumerate() {
        scrub_to(&mut h, ts(i as i64 * 15));
        let drawn = painted(&h);
        assert!(
            drawn.contains(expected),
            "the playhead is on frame {i} and its picture was not painted; \
             painted {drawn:?}"
        );
        assert!(
            !drawn.contains(&live),
            "the playhead is on frame {i} but the layer's LIVE raster is on the \
             glass — that is the previous instant presented as the answer"
        );
        seen.push(*expected);
    }

    seen.dedup();
    assert_eq!(
        seen.len(),
        3,
        "the drawn texture must CHANGE with the playhead; a fork that always \
         answered one frame would satisfy every assertion above"
    );
}

/// **Floor C's half: a frame with no picture yet fabricates nothing.**
///
/// The loading *state* is WI-7's; what is drawn is this item's, and the
/// boundary is exactly that. A frame whose `image` is `None` must leave the map
/// empty of this layer — not fall back to the live raster, and not hold the
/// previous frame. The same is asserted for a clock sitting before every frame
/// the layer holds, which is `qualifying_frame`'s `None` reaching the painter.
#[test]
fn a_frame_with_no_picture_yet_paints_nothing_at_all() {
    let mut h = InputHarness::new();
    let (live, frames) = model_loop(&mut h);

    scrub_to(&mut h, ts(15));
    assert!(
        painted(&h).contains(&frames[1]),
        "fixture: frame 1 is what a loaded playhead paints"
    );

    // Take frame 1's picture away and leave everything else alone.
    h.gui_mut().panes_mut()[0].time_state_mut(&LAYER).frames[1].image = None;
    h.warm_up();
    let drawn = painted(&h);
    for (i, id) in frames.iter().enumerate() {
        assert!(
            !drawn.contains(id),
            "frame {i}'s picture is on the glass while the playhead sits on a \
             frame that has none"
        );
    }
    assert!(
        !drawn.contains(&live),
        "a frame that has not arrived was papered over with the live raster"
    );

    // And a clock before every frame the layer holds: nothing qualifies, so
    // nothing is drawn — the oldest frame is not a fallback.
    h.gui_mut().panes_mut()[0].time_state_mut(&LAYER).frames[1].image =
        Some(LoopFrameImage::Overlay(raster(&h, "restored")));
    scrub_to(&mut h, ts(-60));
    let drawn = painted(&h);
    assert!(
        !drawn.contains(&live) && !drawn.contains(&frames[0]),
        "a clock before every frame drew something anyway: {drawn:?}"
    );
}

/// **Non-triviality: a pane that is not animating this layer is unchanged.**
///
/// The live raster is what a still pane has always painted, and the fork must
/// not have made "always paint the loop frame" true. Both halves are read: the
/// layer inactive, and the layer holding frames it is simply not animating.
#[test]
fn a_layer_that_is_not_animating_still_paints_its_live_raster() {
    let mut h = InputHarness::new();
    let (live, frames) = model_loop(&mut h);
    scrub_to(&mut h, ts(15));

    h.gui_mut().panes_mut()[0].time_state_mut(&LAYER).phase = LoopPhase::Inactive;
    h.warm_up();

    let drawn = painted(&h);
    assert!(
        drawn.contains(&live),
        "a pane that is not animating stopped painting its live raster: {drawn:?}"
    );
    for (i, id) in frames.iter().enumerate() {
        assert!(
            !drawn.contains(id),
            "frame {i} is on the glass on a pane that is not animating"
        );
    }
}

/// **Floor B: an overlay frame is not one of radar's render views.**
///
/// `LayerTimeState::view` is `PlanView` on a fresh state, so an overlay
/// timeline satisfies every `self.view == RenderView::PlanView` gate in
/// `pane.rs` by construction. What keeps a finished radar render out of an
/// overlay frame — and an overlay picture out of radar's accessors — is that
/// the picture itself refuses to name a `RenderView`.
///
/// **Floor B mutation** — make `view()` answer `Some(RenderView::PlanView)` for
/// `Overlay` and this fails on its first assertion.
#[test]
fn an_overlay_frame_is_not_a_radar_render_view() {
    let ctx = egui::Context::default();
    let texture = ctx.load_texture(
        "b",
        egui::ColorImage::filled([1, 1], egui::Color32::RED),
        egui::TextureOptions::default(),
    );
    let overlay = LoopFrameImage::Overlay(OverlayTextureData {
        texture,
        placed: squallar_geo::PlacedRaster::of(squallar_geo::GeoBounds {
            min_lat: 30.0,
            max_lat: 40.0,
            min_lon: -100.0,
            max_lon: -90.0,
        }),
        data_generation: 0,
        render_zoom: 0,
        width: 1,
        height: 1,
        radar_meta: None,
        hit_map: None,
    });

    assert_eq!(
        overlay.view(),
        None,
        "an overlay frame named a radar render view, so a radar render result \
         can be filed into it"
    );
    assert!(
        overlay.plan_view().is_none() && overlay.section().is_none() && overlay.volume().is_none(),
        "a radar consumer was handed an overlay frame"
    );
    assert!(
        overlay.overlay().is_some(),
        "and its own consumer refused it"
    );

    // The converse: radar's own shapes must not answer the overlay accessor.
    let plan = LoopFrameImage::PlanView(crate::pane::RadarImageData {
        texture: ctx.load_texture(
            "p",
            egui::ColorImage::filled([1, 1], egui::Color32::BLUE),
            egui::TextureOptions::default(),
        ),
        lat: 35.0,
        lon: -97.0,
        placed: squallar_geo::PlacedRaster::of(squallar_geo::GeoBounds {
            min_lat: 30.0,
            max_lat: 40.0,
            min_lon: -100.0,
            max_lon: -90.0,
        }),
        hover: std::sync::Arc::new(squallar_radar::hover::HoverSource::empty()),
        max_range_km: 100.0,
        nyquist_ms: None,
        melting_layer_source: None,
        storm_motion: None,
    });
    assert_eq!(
        plan.view(),
        Some(squallar_radar::types::RenderView::PlanView),
        "radar's own shapes still name their view"
    );
    assert!(
        plan.overlay().is_none(),
        "a radar plan view answered the overlay accessor"
    );
}
