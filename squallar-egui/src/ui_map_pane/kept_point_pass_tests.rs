//! **What a pass whose build is still current does not do again.**
//!
//! [`render_per_frame_overlay`]'s memo used to keep one thing — the tessellated
//! text — and re-derive everything under it every frame: the whole point list
//! folded to the pane's turn, geo-culled, and every survivor projected. The key
//! that decides whether the mesh is still good carries the projector and the
//! culling window, so a pass that finds it held would reach the same survivors
//! at the same places. It reads them off the build instead.
//!
//! **Denominator.** One pane, one layer, one pass per assertion. Nothing here
//! is a frame figure.

use super::*;
use squallar_overlays::render::draw::MapPoint;
use squallar_overlays::render::overlay_state::{
    FetchPayload, OverlayHandler, OverlayItem, OverlayRegistry, PaneRef, PopupContent, RenderMode,
    Surface,
};
use squallar_source::time::TimeAxis;
use std::sync::Arc;

const CANVAS: egui::Rect = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(800.0, 600.0));
const CAMERA: (f64, f64) = (35.33, -97.28);

/// The selection a point carries, and the only thing a click here can return.
#[derive(Debug)]
struct Sel(&'static str);

impl OverlayItem for Sel {
    fn layer_id(&self) -> LayerId {
        known::METAR
    }
    fn popup_content(&self, _prefs: &UserPreferences) -> PopupContent {
        PopupContent {
            title: self.0.to_owned(),
            accent_rgb: [0, 0, 0],
            width: 1.0,
            sections: Vec::new(),
            actions: Vec::new(),
        }
    }
    fn matches(&self, _other: &dyn OverlayItem) -> bool {
        false
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// A point layer that draws text and declares a picture, so the pass takes the
/// `text_only` arm the memo lives on.
struct TextPoints {
    points: Vec<MapPoint>,
}

impl TextPoints {
    /// `offsets` are degrees from the camera; a point at `(0.0, 0.0)` lands at
    /// the middle of the canvas.
    fn at(offsets: &[(f64, f64)]) -> Self {
        Self {
            points: offsets
                .iter()
                .enumerate()
                .map(|(i, (dlat, dlon))| MapPoint {
                    lat: CAMERA.0 + dlat,
                    lon: CAMERA.1 + dlon,
                    id: i as u32,
                    selection: Arc::new(Sel(NAMES[i])) as Arc<dyn OverlayItem>,
                })
                .collect(),
        }
    }
}

const NAMES: [&str; 3] = ["one", "two", "three"];

impl OverlayHandler for TextPoints {
    fn id(&self) -> LayerId {
        known::METAR
    }
    fn surface(&self) -> Surface {
        Surface::Glass
    }
    fn draw_order_weight(&self) -> u32 {
        100
    }
    fn display_name(&self) -> &str {
        "TextPoints"
    }
    fn render_mode(&self) -> RenderMode {
        RenderMode::TextureAndPoint
    }
    /// **Held across the swap below on purpose.** The generation is what says
    /// the data moved; a fixture that bumped it would be testing the rebuild
    /// path rather than the kept one.
    fn data_generation(&self) -> u64 {
        7
    }
    fn has_data(&self, _pane: &PaneRef<'_>) -> bool {
        true
    }
    fn is_fetching(&self) -> bool {
        false
    }
    fn set_fetching(&mut self, _fetching: bool, _pane: &PaneRef<'_>) {}
    fn fetch_time(&self) -> Option<web_time::Instant> {
        None
    }
    fn apply_fetch_result(&mut self, _result: FetchPayload, _pane: &PaneRef<'_>) {}
    fn retain_selections(&self, _selections: &mut Vec<Arc<dyn OverlayItem>>, _pane: &PaneRef<'_>) {}
    fn time_axis(&self) -> TimeAxis {
        TimeAxis::Live
    }
    fn per_frame_points(&self) -> &[MapPoint] {
        &self.points
    }
    fn point_hit_radius(&self, _zoom: f32) -> f32 {
        12.0
    }
    /// A picture, which is what makes the pass `text_only`.
    fn job_codec(&self) -> Option<&'static squallar_source::job::JobCodec> {
        squallar_overlays::render::jobs::JOB_CODECS.first()
    }
    fn draw_point(
        &self,
        id: u32,
        painter: &mut dyn squallar_overlays::render::draw::PointPainter,
        _ctx: &squallar_overlays::render::draw::DrawPointContext,
    ) {
        painter.text(
            [0.0, 0.0],
            NAMES[id as usize],
            [255, 255, 255, 255],
            11.0,
            squallar_overlays::render::draw::TextAnchor::Center,
        );
    }
}

/// What one pass produced: the selections it resolved and the shapes it added.
struct Pass {
    selected: Vec<Arc<dyn OverlayItem>>,
    shapes: Vec<egui::epaint::ClippedShape>,
}

/// Run one point pass over `offsets`, clicking at `click`.
fn pass(
    ctx: &egui::Context,
    galleys: &mut walkers::GalleyCache,
    meshes: &mut crate::point_painter::PointTextMeshes,
    offsets: &[(f64, f64)],
    click: Option<egui::Pos2>,
) -> Pass {
    pass_at(ctx, galleys, meshes, offsets, click, 7.0)
}

/// [`pass`], at a chosen zoom — the term of the key a wheel moves.
fn pass_at(
    ctx: &egui::Context,
    galleys: &mut walkers::GalleyCache,
    meshes: &mut crate::point_painter::PointTextMeshes,
    offsets: &[(f64, f64)],
    click: Option<egui::Pos2>,
    zoom: f64,
) -> Pass {
    let overlays = OverlayRegistry::with_handlers(vec![
        Box::new(TextPoints::at(offsets)) as Box<dyn OverlayHandler>
    ]);
    let mut memory = walkers::MapMemory::default();
    memory.set_zoom(zoom).expect("a zoom walkers accepts");
    let projector = walkers::Projector::new(CANVAS, &memory, walkers::lat_lon(CAMERA.0, CAMERA.1));
    let preferences = UserPreferences::default();

    ctx.begin_pass(egui::RawInput {
        screen_rect: Some(CANVAS),
        ..Default::default()
    });
    let ui = egui::Ui::new(
        ctx.clone(),
        egui::Id::new("kept_point_pass"),
        egui::UiBuilder::new()
            .layer_id(egui::LayerId::background())
            .max_rect(CANVAS),
    );
    let selected = render_per_frame_overlay(
        galleys,
        meshes,
        &ui,
        &projector,
        &PerFrameOverlayCtx {
            pane_idx: 0,
            overlays: &overlays,
            id: &known::METAR,
            zoom,
            prefs: &preferences,
            overlay_click_pos: click,
            excluded_rects: &[],
            pane_rect: CANVAS,
        },
    );
    Pass {
        selected,
        shapes: ctx.end_pass().shapes,
    }
}

/// Where the middle point of [`ON_PANE`] lands, so a click there hits it.
fn centre_of_pane() -> egui::Pos2 {
    CANVAS.center()
}

/// Three points on the glass; the first is at the camera, under the click.
const ON_PANE: [(f64, f64); 3] = [(0.0, 0.0), (0.10, 0.10), (-0.10, -0.10)];
/// The same three, moved most of the way round the world. Every one of them is
/// culled by a pass that walks the list.
const OFF_PANE: [(f64, f64); 3] = [(0.0, 140.0), (0.10, 140.10), (-0.10, 139.90)];

/// Every byte a tessellated pass puts in front of the renderer, in order.
fn digest(ctx: &egui::Context, shapes: Vec<egui::epaint::ClippedShape>) -> Vec<String> {
    ctx.tessellate(shapes, ctx.pixels_per_point())
        .iter()
        .map(|p| match &p.primitive {
            egui::epaint::Primitive::Mesh(mesh) => format!(
                "clip=({:?},{:?},{:?},{:?}) tex={:?} indices={:?} vertices={:?}",
                p.clip_rect.min.x.to_bits(),
                p.clip_rect.min.y.to_bits(),
                p.clip_rect.max.x.to_bits(),
                p.clip_rect.max.y.to_bits(),
                mesh.texture_id,
                mesh.indices,
                mesh.vertices
                    .iter()
                    .map(|v| (
                        v.pos.x.to_bits(),
                        v.pos.y.to_bits(),
                        v.uv.x.to_bits(),
                        v.uv.y.to_bits(),
                        v.color.to_array(),
                    ))
                    .collect::<Vec<_>>(),
            ),
            egui::epaint::Primitive::Callback(_) => "callback".to_owned(),
        })
        .collect()
}

/// **The fixture must be able to tell the two passes apart.** A pass that walks
/// [`OFF_PANE`] resolves no click at all, so the second assertion below cannot
/// pass by accident of the click missing everything.
#[test]
fn the_fixture_separates_a_walked_pass_from_a_kept_one() {
    let ctx = egui::Context::default();
    let mut galleys = walkers::GalleyCache::default();
    let mut meshes = crate::point_painter::PointTextMeshes::default();

    let on = pass(
        &ctx,
        &mut galleys,
        &mut meshes,
        &ON_PANE,
        Some(centre_of_pane()),
    );
    assert_eq!(
        on.selected.len(),
        1,
        "the click must reach one on-pane point"
    );

    // A fresh memo, so this is a walk and not a kept read.
    let mut fresh = crate::point_painter::PointTextMeshes::default();
    let off = pass(
        &ctx,
        &mut galleys,
        &mut fresh,
        &OFF_PANE,
        Some(centre_of_pane()),
    );
    assert!(
        off.selected.is_empty(),
        "a walked pass over the moved points must resolve nothing"
    );
}

/// **A pass whose build is still current never walks the list.**
///
/// The data is swapped underneath it to somewhere no cull would keep, with the
/// generation held — the one thing that would tell the memo to rebuild. A pass
/// that re-walked would find nothing to click; this one clicks the point its
/// build wrote down, which is only possible from the kept positions.
///
/// And the glass is the same glass: the second pass's tessellated stream is
/// byte for byte the first's, clip rects, texture ids, indices and vertices.
#[test]
fn a_kept_build_answers_the_click_and_draws_the_same_bytes() {
    let ctx = egui::Context::default();
    let mut galleys = walkers::GalleyCache::default();
    let mut meshes = crate::point_painter::PointTextMeshes::default();

    // Warm the font atlas, so the second pass cannot differ by a glyph that
    // was not there yet.
    let _ = pass(&ctx, &mut galleys, &mut meshes, &ON_PANE, None);
    let mut meshes = crate::point_painter::PointTextMeshes::default();

    let build = pass(
        &ctx,
        &mut galleys,
        &mut meshes,
        &ON_PANE,
        Some(centre_of_pane()),
    );
    assert_eq!(meshes.builds(), 1, "the first pass must build");
    assert_eq!(build.selected.len(), 1);

    let kept = pass(
        &ctx,
        &mut galleys,
        &mut meshes,
        &OFF_PANE,
        Some(centre_of_pane()),
    );
    assert_eq!(
        meshes.hits(),
        1,
        "the second pass must be answered from the build"
    );
    assert_eq!(
        meshes.builds(),
        1,
        "the second pass must not have rebuilt anything"
    );
    assert_eq!(
        kept.selected.len(),
        1,
        "a kept pass hit-tests off the positions its build wrote down"
    );

    assert_eq!(
        digest(&ctx, kept.shapes),
        digest(&ctx, build.shapes),
        "a kept pass must put the same bytes in front of the renderer"
    );
}

/// **A rebuild fills the buffers the build it replaces left behind.**
///
/// A zoom is what makes the second pass a rebuild rather than a hit — it is a
/// term of the key, and it is what the wheel half of the gesture moves. The
/// first build has nothing to take and must say so; the second must take
/// **all three** — the mesh's buffers, the shape list's, and the culled-point
/// list's — which is the whole of the saving.
///
/// The point list is counted here because it was the one this pin did not
/// cover: the mesh and the shape list were recycled and the list of points the
/// cull kept was minted from `Vec::new()` on every rebuilding frame of a pan,
/// while the previous build's — already exactly the right size — went back to
/// the allocator underneath it. A pin that names two of three buffers reads
/// green on a pass that leaks the third.
#[test]
fn a_rebuild_takes_the_previous_builds_buffers() {
    let ctx = egui::Context::default();
    let mut galleys = walkers::GalleyCache::default();
    let mut meshes = crate::point_painter::PointTextMeshes::default();

    let _ = pass_at(&ctx, &mut galleys, &mut meshes, &ON_PANE, None, 7.0);
    assert_eq!(meshes.builds(), 1);
    assert_eq!(
        meshes.recycled_meshes(),
        0,
        "the first build has nothing to take"
    );
    assert_eq!(meshes.recycled_shapes(), 0);
    assert_eq!(meshes.recycled_points(), 0);

    let _ = pass_at(&ctx, &mut galleys, &mut meshes, &ON_PANE, None, 7.5);
    assert_eq!(meshes.builds(), 2, "a moved zoom is a rebuild, not a hit");
    assert_eq!(
        meshes.recycled_meshes(),
        1,
        "the rebuild asked the allocator for a mesh it already owned"
    );
    assert_eq!(
        meshes.recycled_shapes(),
        1,
        "the rebuild collected into a fresh shape list"
    );
    assert_eq!(
        meshes.recycled_points(),
        1,
        "the rebuild grew a fresh list of culled points while the previous \
         build's went back to the allocator"
    );

    // **And the pass FILLED it**, which the counter above cannot say: a pass
    // that retires the buffer and then collects into `Vec::new()` anyway bumps
    // that counter and reads green. So the parked list is given a capacity no
    // fresh `Vec` growing to this fixture's handful of points could reach, and
    // the next rebuild has to still be holding it.
    meshes
        .stored_points_mut(0, &known::METAR)
        .expect("the build stored a list")
        .reserve(4096);
    let _ = pass_at(&ctx, &mut galleys, &mut meshes, &ON_PANE, None, 8.0);
    assert_eq!(meshes.builds(), 3, "a moved zoom is a rebuild, not a hit");
    let held = meshes
        .stored_points_mut(0, &known::METAR)
        .expect("the rebuild stored a list")
        .capacity();
    assert!(
        held >= 4096,
        "the rebuild stored a list of capacity {held}: it collected into a \
         fresh `Vec` and gave the retired buffer back to the allocator"
    );
}
