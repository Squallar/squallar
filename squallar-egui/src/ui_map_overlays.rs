use crate::overlay_cache::{OverlayTextureData, draw_overlay_texture, geo_point_in_feature};
use crate::tile_source::HttpsTiles;
use squallar_overlays::render::overlay_state::{ClickableItem, OverlayItem};
use squallar_overlays::types::OverlayLabel;
use std::sync::Arc;
use walkers::{Tile, TileId};

// ---------------------------------------------------------------------------
/// Shared context for overlay drawing operations.
pub(super) struct OverlayDrawContext<'a> {
    ui: &'a egui::Ui,
    projector: &'a walkers::Projector,
    screen_rect: egui::Rect,
    // Pre-computed click state (shared by discussion + alert drawing).
    overlay_click_pos: Option<egui::Pos2>,
    click_on_ui: bool,
    pointer_available: bool,
}

/// Returns `true` when a screen-space position should be treated as "blocked"
/// by a floating dialog or non-map UI element, meaning map interactions at
/// that position must be suppressed.
pub(super) fn is_pos_blocked(
    ctx: &egui::Context,
    pos: egui::Pos2,
    pane_rect: egui::Rect,
    excluded_rects: &[egui::Rect],
) -> bool {
    !pane_rect.contains(pos)
        || excluded_rects.iter().any(|r| r.contains(pos))
        || ctx
            .layer_id_at(pos)
            .is_some_and(|l| l.order > egui::Order::Background)
}

impl<'a> OverlayDrawContext<'a> {
    pub fn new(
        ui: &'a egui::Ui,
        projector: &'a walkers::Projector,
        pointer_available: bool,
        pane_rect: egui::Rect,
        excluded_rects: &[egui::Rect],
        overlay_click_pos: Option<egui::Pos2>,
    ) -> Self {
        let screen_rect = ui.max_rect();

        // Suppress overlay clicks when the click position is outside
        // the map pane, on a floating UI element, or on a popup layer.
        let click_on_ui = overlay_click_pos
            .is_some_and(|p| is_pos_blocked(ui.ctx(), p, pane_rect, excluded_rects));

        Self {
            ui,
            projector,
            screen_rect,
            overlay_click_pos,
            click_on_ui,
            pointer_available,
        }
    }

    /// Draw a single overlay layer: texture, labels, and click detection.
    ///
    /// **The raster arrives resolved, not as a cache to read.** Which picture
    /// this layer shows is the pane's fork
    /// ([`PaneState::overlay_texture_on_screen`](crate::pane::PaneState::overlay_texture_on_screen)):
    /// a loop frame while the layer is animating, its live raster otherwise.
    /// Hit-testing below reads the same value, so what is clicked is always
    /// what was painted.
    pub fn draw_overlay<'i>(
        &self,
        texture: Option<&OverlayTextureData>,
        labels: &[OverlayLabel],
        items: impl FnOnce() -> Vec<ClickableItem<'i>>,
    ) -> Vec<Arc<dyn OverlayItem>> {
        if let Some(tex) = texture {
            draw_overlay_texture(self.ui.painter(), self.projector, tex, self.screen_rect);
        }

        let painter = self.ui.painter();
        for label in labels {
            let screen_pos = self
                .projector
                .project(walkers::lat_lon(label.lat, label.lon))
                .to_pos2();
            if self.screen_rect.contains(screen_pos) {
                let [r, g, b, a] = label.color;
                let color = egui::Color32::from_rgba_unmultiplied(r, g, b, a);
                painter.text(
                    screen_pos,
                    egui::Align2::CENTER_CENTER,
                    &label.text,
                    egui::FontId::proportional(11.0),
                    color,
                );
            }
        }

        if !self.pointer_available || self.click_on_ui {
            return Vec::new();
        }
        let Some(click_pos) = self.overlay_click_pos else {
            return Vec::new();
        };

        // If a hit buffer is available, use it for pixel-perfect detection.
        if let Some(tex) = texture
            && let Some(ref hit_map) = tex.hit_map
        {
            let rect = crate::overlay_cache::placed_rect(self.projector, &tex.placed);
            if rect.width() > 0.0 && rect.height() > 0.0 {
                let u = (click_pos.x - rect.left()) / rect.width();
                let v = (click_pos.y - rect.top()) / rect.height();
                return hit_map.hit_test(u, v);
            }
        }

        // Fall back to geographic polygon containment.
        let geo = self
            .projector
            .unproject(egui::vec2(click_pos.x, click_pos.y));
        let lat = geo.y();
        let lon = geo.x();

        let mut hits = Vec::new();
        for item in items() {
            let hit = item
                .features
                .iter()
                .any(|f| geo_point_in_feature(lat, lon, f));
            if hit {
                hits.push(item.item.clone());
            }
        }
        hits
    }
}

/// What one [`draw_tile_layer`] pass produced: the labels it deferred, and
/// whether the span it walked was fully answered.
pub(super) struct TileLayerPaint {
    /// The labels the tiles carried, deferred for the `CityLabels` arm.
    pub(super) labels: Vec<walkers::Text>,
    /// Whether every cell was answered with its exact tile. See
    /// [`TileCoverage`].
    pub(super) coverage: TileCoverage,
}

/// Whether one tile pass answered **every** cell of its span with the exact
/// tile at the requested zoom — no hole, no ancestor stretched over a gap, no
/// archive still waiting on its header.
///
/// A newtype over a private `bool`, in the shape of `GroundIsMesh` and for
/// the same reason: the only way to obtain a *complete* answer is to have
/// [`draw_tile_layer`] walk the span and measure it. A caller-composed
/// `true` — "the source exists, so it must be resolved" — is exactly the
/// belief that would freeze a 3D floor on stretched ancestors for ever, and
/// it does not typecheck against this.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct TileCoverage(bool);

impl TileCoverage {
    /// Whether the pass answered its whole span at the requested zoom.
    pub(super) fn complete(self) -> bool {
        self.0
    }
}

/// The `uv` window of a tile that was answered by *itself* rather than by an
/// ancestor: the whole texture. `interpolate_from_lower_zoom` at the tile's
/// own zoom produces exactly these bounds, so the compare is bit-exact.
const FULL_TILE_UV: egui::Rect =
    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));

/// Draw one slippy-map tile layer through the pane's own projector, and hand
/// back the labels it deferred plus whether the span was fully answered.
///
/// **The ground is painted here; the labels are not.** A vector tile carries
/// both, but they belong at different heights in the pane: the ground is the
/// bottom of the stack and the place names draw at the `CityLabels` layer's
/// position, above the weather. The caller paints them with [`paint_labels`]
/// when that layer's turn comes, and drops them when it is switched off.
///
/// `ground` is what draws a vector tile's tessellated fills from the GPU, or
/// `None` for a pass that must place them itself — see
/// [`PaneRenderCtx::ground_mesh_painter`](super::pane_render::PaneRenderCtx::ground_mesh_painter).
pub(super) fn draw_tile_layer(
    ui: &egui::Ui,
    projector: &walkers::Projector,
    zoom: f64,
    tiles: &mut HttpsTiles,
    zoom_bias: u8,
    ground: Option<&std::sync::Arc<dyn crate::tile_mesh::TileMeshPainter>>,
) -> TileLayerPaint {
    // Once for the layer, before the grid loop. `HttpsTiles::at` does not drain,
    // so this is the only thing that moves finished fetches into the cache --
    // and doing it per cell would repeat it once per tile in the span below.
    // Unconditional: the pump is bounded by its own time budget rather than by
    // whether a gesture is running, so tiles land while the map is moving.
    tiles.pump();

    // An archive source has not read its header yet on the first frames, so it
    // cannot say how deep it goes. Clamping to a stand-in number is what used to
    // seed `0/0/0` into the LRU and leave it there as the session's fallback
    // ancestor -- see `tile_source::MAX_ZOOM_UNKNOWN`. Drawing nothing for a
    // frame is the whole cost of not doing that; the IO task repaints when the
    // header lands. Incomplete, not vacuously complete: nothing was answered.
    let Some(source_max_zoom) = tiles.source_max_zoom() else {
        return TileLayerPaint {
            labels: Vec::new(),
            coverage: TileCoverage(false),
        };
    };

    let tile_zoom = (zoom.round() as u8)
        .saturating_add(zoom_bias)
        .min(source_max_zoom);

    let span = crate::tiles::tile_span(projector, ui.max_rect(), tile_zoom);

    // The ancestor net, and **before** the grid loop rather than after it.
    // These tiles are not drawn this frame; they are what
    // `cached_or_interpolated` stretches over the frame a zoom-out lands on,
    // and without them that frame is a hole in every cell -- see
    // `HttpsTiles::warm` and `tiles::WARM_ANCESTOR_STEPS`.
    //
    // The order is the whole of it. `request_once` sends on a
    // `channel(MAX_PARALLEL_DOWNLOADS)` and drops what will not fit, retrying
    // next frame; the span below is tens of cells against six slots, so it
    // fills the queue on every frame it has anything left to ask for. Asked
    // afterwards, the net -- four tiles -- was refused every time and never
    // arrived at all. Asked first it costs the visible level one frame of
    // delay on four of its tiles, which it was already going to spend, since
    // 84 asks never fit six slots either way.
    if let Some(net_zoom) = tile_zoom.checked_sub(crate::tiles::WARM_ANCESTOR_STEPS) {
        let step = crate::tiles::WARM_ANCESTOR_STEPS;
        for ty in (span.north >> step)..=(span.south >> step) {
            for tx in (span.west >> step)..=(span.east >> step) {
                tiles.warm(TileId {
                    x: tx,
                    y: ty,
                    zoom: net_zoom,
                });
            }
        }
    }

    // egui's own frame counter, so the renderer can tell one frame's ground
    // draws from the next without a clock or a frame callback of its own.
    let pass_nr = ui.ctx().cumulative_pass_nr();

    // Once for the layer, not once per tile: a `Context` read lock and a
    // divide, against a span that holds up to 84 cells.
    let feathering = crate::tile_mesh::feathering_of(ui.ctx());

    // Accumulated across every cell below, so the collision test the caller
    // makes is one test against the whole pane; see [`paint_labels`].
    let mut labels: Vec<walkers::Text> = Vec::new();

    // Falsified per cell below; `tile_zoom` is already clamped to the
    // source's deepest level, so an inexact answer is a tile that has not
    // arrived, never one the source cannot serve.
    let mut exact = true;

    for ty in span.north..=span.south {
        for tx in span.west..=span.east {
            let tile_id = TileId {
                x: tx,
                y: ty,
                zoom: tile_zoom,
            };

            let answered = tiles.ground_at(tile_id);
            exact &= answered
                .as_ref()
                .is_some_and(|piece| piece.uv == FULL_TILE_UV);
            if let Some(twuv) = answered {
                // Affine, not geographic. This used to spell the tile's two corners as
                // latitudes and longitudes and hand them to `geo_corner_rect`, which
                // projected them straight back: `tile_to_lat` is `sinh`/`atan` and
                // `Projector::project` is `tan`/`asinh`, exact inverses, four transcendental
                // pairs per tile to arrive at a rect that is a linear function of
                // `(x, y, zoom)`. `tests::the_affine_tile_rect_agrees_with_the_geographic_round_trip`
                // holds the two answers together.
                let rect = projector.tile_rect(tile_id);

                match twuv.tile {
                    Tile::Raster(ref tex) => {
                        ui.painter()
                            .image(tex.id(), rect, twuv.uv, egui::Color32::WHITE);
                    }
                    Tile::Vector(ref shapes) => {
                        paint_vector_tile(
                            ui.painter(),
                            shapes,
                            GroundMeshes {
                                meshes: twuv.meshes.as_ref(),
                                painter: ground,
                                pass_nr,
                                feathering,
                            },
                            rect,
                            twuv.uv,
                            &mut labels,
                        );
                    }
                }
            }
        }
    }

    TileLayerPaint {
        labels,
        coverage: TileCoverage(exact),
    }
}

/// The rect the *whole* tile would occupy, given the rect a `uv` sub-rectangle
/// of it was placed at.
///
/// `HttpsTiles::at` answers a deep tile with a shallower ancestor plus the `uv`
/// window of that ancestor which covers the tile asked for, and `rect` is where
/// that window goes. The shapes inside a vector tile are in extent coordinates
/// over the tile as a whole, so they have to be placed against the whole tile's
/// rect and then clipped back to `rect` -- a raster gets the same treatment for
/// free, because `Painter::image` takes the `uv` directly.
///
/// walkers computes this in `tiles::full_rect_of_clipped_tile`, which is
/// private. Ten lines of affine arithmetic, so it lives here rather than
/// widening the vendor delta.
fn full_rect_of_clipped_tile(rect: egui::Rect, uv: egui::Rect) -> egui::Rect {
    let full = egui::vec2(rect.width() / uv.width(), rect.height() / uv.height());
    let min = rect.min - egui::vec2(full.x * uv.min.x, full.y * uv.min.y);
    egui::Rect::from_min_size(min, full)
}

/// How far apart two labels reading the same name have to be before both draw.
///
/// **This is what stops a river being named six times in one viewport.** OSM
/// splits a way at every tag change, county line and confluence, so one
/// watercourse arrives as a dozen `LineString`s and each one asks for its own
/// label. Deduplicating by name outright would be wrong in the other
/// direction -- MapLibre repeats a line label every `symbol-spacing` points
/// precisely so a river crossing the whole screen is readable at both ends --
/// so the rule is a *distance*, and a name may repeat once it is far enough
/// away to be a second reading rather than a duplicate.
///
/// 300 points because that is what the committed styles ask for:
/// `waterway_label` sets `symbol-spacing: 300` and `watername_lake_line` sets
/// 350, against MapLibre's default of 250. A per-layer value would have to be
/// carried on every [`walkers::Text`] to reach here; the styles use two values
/// eleven points apart in effect, so one constant is the honest simplification
/// and this comment is the record of what it stands in for.
const MIN_REPEAT_DISTANCE: f32 = 300.0;

/// How many rows a wrapped label may occupy before it is not worth placing.
///
/// **A name that needs a third row is not a label, it is a paragraph.** OSM
/// carries the legal names of jointly-held areas, and at least one of them --
/// "Kiowa Indian Tribe, Comanche Nation, Apache Tribe, and Fort Sill Apache
/// Tribe", 77 characters -- has every one of its five OpenMapTiles name fields
/// set to that same string, so there is no shorter variant to select. Wrapping
/// it to its layer's `text-max-width` gives a 92x70 pt block that dominates the
/// view; unwrapped it measures 414.7 pt and spans the pane. Neither is a label.
///
/// Two rows and not one, because the ordinary long-ish names are two rows and
/// read fine: "Iowa Tribe of Oklahoma", "Seneca-Cayuga Nation". Those are the
/// cases this must not touch, and they are what sets the threshold.
///
/// This is a *length* rule and not a list. Any name anywhere in the world that
/// cannot be set in two rows is dropped, which is the behaviour of most maps --
/// they simply do not label that area at this zoom -- rather than a special case
/// for one feature that would leave the next one to be found by a user.
const MAX_LABEL_ROWS: usize = 2;

/// Lay one label out, and claim the area it needs.
///
/// Returns [`egui::Shape::Noop`] when the label is unplaceable, which happens
/// two ways: the area is already taken -- the collision rule, first label to ask
/// for a piece of screen keeps it -- or the name is too long to set in
/// [`MAX_LABEL_ROWS`].
///
/// The caller owns `occupied`, and [`paint_labels`] owns the only one there is
/// per pane.
///
/// The layout is [`walkers::Text`]'s own, so this phase and walkers' per-tile
/// draw cannot drift apart. The row cap is *not* pushed down there: it is our
/// cartographic policy about what is worth drawing, not a property of laying
/// text out, and walkers has no opinion about it.
fn lay_out_label(
    ctx: &egui::Context,
    text: &walkers::Text,
    occupied: &mut walkers::OccupiedAreas,
    galleys: &mut walkers::GalleyCache,
) -> egui::Shape {
    let galley = text.galley_cached(ctx, galleys);

    // Before `try_occupy`, so an unplaceable name does not first claim the
    // screen it was never going to be drawn on and evict a label that fits.
    if galley.rows.len() > MAX_LABEL_ROWS {
        return egui::Shape::Noop;
    }

    let area = walkers::text::OrientedRect::new(text.position, text.angle, galley.size());
    let top_left = area.top_left();

    if occupied.try_occupy(area) {
        text.shape(galley, top_left)
    } else {
        egui::Shape::Noop
    }
}

/// Paint one decoded vector tile.
///
/// `shapes` are in MVT extent units over the whole tile and are shared by every
/// pane that draws this tile, so nothing here mutates them:
/// [`walkers::ShapeOrText::placed`] returns a placed copy of the one shape it
/// is given.
///
/// **One copy of a shape is made, and only for the shapes the clip can show.**
/// This used to be `mvt::transformed`, which materialised a whole second
/// `Vec<ShapeOrText>` and then walked it in place — and the in-place walk hit
/// `Arc::make_mut` on every tessellated fill, which copied again because the
/// tile cache still held the original. Two deep copies of every shape in every
/// visible tile, every frame. Measured on the committed Monaco fixture's z14
/// tile, release build: 22.9 us to clone the cached `Tile` plus 135.2 us to
/// transform it — 158.1 us per tile per frame, against a viewport that holds up
/// to 84 tiles.
///
/// **The tessellated fills and the strokes do not take that copy at all where
/// a renderer can draw them.** Both were flattened once when the tile arrived
/// ([`crate::tile_mesh`]) and are drawn from a GPU buffer with the placement
/// as a uniform, so a run becomes one paint callback rather than a copy of its
/// geometry. Same fixture, same build: the two coalesced meshes were 12.63 us
/// of the tile's 26.61 us of placement and the 708 stroked paths beside them
/// were the other 13.51 us. A stroke's width is in screen points while its
/// geometry is in extent units, which is what used to keep it here; the offset
/// each vertex takes from its point is invariant under the placement, so it is
/// pre-computed and added in the shader ([`crate::tile_mesh::stroke`]).
/// `ground` being `None` (a floor strip, a raster tile, a build with no
/// renderer installed) puts every run back on this path unchanged, and so does
/// a tile flattened at a feathering this frame does not draw at.
///
/// **Nothing is culled here, and that was measured rather than assumed.** A
/// per-shape bounding-rect test against the clip looks like the obvious
/// companion to this, and it was tried: on the quarter-piece ancestor case it
/// dropped 738 shapes to 75 and still ran *slower* (35.9 us against 32.1 us),
/// because `mvt::render` folds a tile's fills into a couple of large meshes and
/// a large mesh both dominates the bounds pass and always intersects. epaint's
/// own `visual_bounding_rect` cull against the clip is what does this job, and
/// it does it after the tessellator rather than before this loop.
///
/// The placement is against the whole tile
/// ([`full_rect_of_clipped_tile`]) and the clip is against the piece, so an
/// ancestor stretched over a gap draws only the part that belongs to the tile
/// that was asked for.
///
/// **This is the ground phase: it paints geometry and defers every label.**
/// Text is pushed onto `labels` for [`paint_labels`] to lay out once the whole
/// grid has been walked, and is *not* painted through this function's clip —
/// a name whose glyphs straddle a tile boundary has to draw whole.
///
/// A label is taken from the tile whose piece its **anchor** falls in, and from
/// that tile only. Vector tiles carry a buffer, so the same place is present in
/// its neighbours' data too; without the anchor test each copy would be drawn,
/// and copies generalised at different zooms do not land close enough to be
/// collided away.
fn paint_vector_tile(
    painter: &egui::Painter,
    shapes: &[walkers::ShapeOrText],
    ground: GroundMeshes<'_>,
    rect: egui::Rect,
    uv: egui::Rect,
    labels: &mut Vec<walkers::Text>,
) {
    let painter = painter.with_clip_rect(rect);

    let full = full_rect_of_clipped_tile(rect, uv);
    let placement = walkers::mvt::placement(full);

    // Accumulated and written once per tile per counter, not once per shape:
    // a dense tile is hundreds of shapes and these are `static` atomics.
    let mut counted = Counted::default();

    let mut runs = GroundMeshes::runs();
    let mut placed: Vec<egui::Shape> = Vec::with_capacity(shapes.len());

    for (index, shape) in shapes.iter().enumerate() {
        // The runs are in shape order, so this walks them in step with the
        // shapes and never searches. A run whose shape the loop has passed
        // cannot exist; one it has not reached yet is simply not this shape's.
        if let Some((callback, kind)) = runs.take_at(index, &ground, placement, rect) {
            match kind {
                crate::tile_mesh::RunKind::Fill => counted.mesh_draws += 1,
                crate::tile_mesh::RunKind::Stroke => counted.stroke_draws += 1,
            }
            placed.push(callback);
            continue;
        }
        // A stroke run covers a *span* of consecutive paths and drew all of
        // them under the callback its first shape pushed. Only a path can be
        // inside a span — anything else that draws closes the run at flatten
        // time — so this cannot swallow a shape the run did not draw.
        if runs.covers(index) && matches!(shape, walkers::ShapeOrText::Shape(egui::Shape::Path(_)))
        {
            continue;
        }
        match shape {
            walkers::ShapeOrText::Shape(egui::Shape::Mesh(mesh)) => {
                counted.mesh_vertices += mesh.vertices.len() as u64;
            }
            walkers::ShapeOrText::Shape(egui::Shape::Path(path)) => {
                counted.path_points += path.points.len() as u64;
            }
            _ => {}
        }
        match shape.placed(placement) {
            walkers::ShapeOrText::Shape(shape) => placed.push(shape),
            walkers::ShapeOrText::Text(text) => {
                if rect.contains(text.position) {
                    counted.label_anchors += 1;
                    labels.push(text);
                }
            }
        }
    }

    counted.report();
    painter.extend(placed);
}

/// What one tile's ground phase placed, before it is reported.
#[derive(Default)]
struct Counted {
    mesh_vertices: u64,
    path_points: u64,
    label_anchors: u64,
    mesh_draws: u64,
    stroke_draws: u64,
}

impl Counted {
    fn report(self) {
        use crate::tile_mesh::ledger;
        ledger::note_mesh_vertices_placed(self.mesh_vertices);
        ledger::note_path_points_placed(self.path_points);
        ledger::note_label_anchors_placed(self.label_anchors);
        ledger::note_mesh_draws(self.mesh_draws);
        ledger::note_stroke_draws(self.stroke_draws);
    }
}

/// What a tile pass knows about drawing this tile's fills from the GPU: the
/// flattened buffers the tile arrived with, and the renderer that can draw
/// them. Either being absent is the CPU path, which is what a floor strip, a
/// raster tile and every unit test in this crate take.
#[derive(Clone, Copy)]
struct GroundMeshes<'a> {
    meshes: Option<&'a std::sync::Arc<crate::tile_mesh::TileMeshes>>,
    painter: Option<&'a std::sync::Arc<dyn crate::tile_mesh::TileMeshPainter>>,
    pass_nr: u64,
    /// The feathering **this frame** tessellates at, in points, from
    /// [`crate::tile_mesh::feathering_of`]. A stroke run whose tile was
    /// flattened at another value is declined; see [`RunCursor::take_at`].
    feathering: f32,
}

impl GroundMeshes<'_> {
    /// Nothing to draw from the GPU: the whole tile takes the CPU path.
    #[cfg(test)]
    const CPU_ONLY: GroundMeshes<'static> = GroundMeshes {
        meshes: None,
        painter: None,
        pass_nr: 0,
        feathering: 0.0,
    };

    /// A cursor over this tile's runs, in shape order.
    fn runs() -> RunCursor {
        RunCursor {
            next: 0,
            covered_to: 0,
        }
    }
}

/// The position in [`TileMeshes::runs`](crate::tile_mesh::TileMeshes::runs)
/// the shape walk has reached.
struct RunCursor {
    next: usize,
    /// One past the last shape an issued run has already drawn. A fill run
    /// reaches one shape; a stroke run reaches its whole span.
    covered_to: usize,
}

impl RunCursor {
    /// The paint callback for the shape at `index`, if that shape opens a run
    /// this install can draw from the GPU, and which kind of run it was.
    /// Advances past the run either way, so a run the renderer refuses falls
    /// through to CPU placement exactly once.
    fn take_at(
        &mut self,
        index: usize,
        ground: &GroundMeshes<'_>,
        placement: egui::emath::TSTransform,
        piece: egui::Rect,
    ) -> Option<(egui::Shape, crate::tile_mesh::RunKind)> {
        let meshes = ground.meshes?;
        let painter = ground.painter?;
        let run = *meshes.runs().get(self.next)?;
        if run.shape_index as usize != index {
            return None;
        }
        self.next += 1;
        // **The `pixels_per_point` guard.** Stroke offsets are baked at a
        // feathering, and drawing them under a different one paints
        // wrong-width roads. A tile whose flatten has not caught up with a
        // display change is not drawn wrong; its paths place on the CPU, as
        // they did before any of this, until the re-flatten
        // (`HttpsTiles::set_feathering`) lands. Bit equality is the right
        // test: both sides come from `tile_mesh::feathering_of`, so equal
        // inputs give equal bits and there is no tolerance to pick.
        if run.kind == crate::tile_mesh::RunKind::Stroke && meshes.feathering() != ground.feathering
        {
            return None;
        }
        let payload = painter.payload(crate::tile_mesh::GroundDraw {
            meshes,
            run: self.next - 1,
            place: crate::tile_mesh::Placement {
                scale: placement.scaling,
                translation: [placement.translation.x, placement.translation.y],
            },
            pass_nr: ground.pass_nr,
        })?;
        self.covered_to = index + run.shape_span as usize;
        Some((
            egui::Shape::Callback(egui::epaint::PaintCallback {
                // The **piece**, which is what egui turns into a viewport and
                // refuses when it is degenerate. The draw replaces that viewport
                // with the whole screen, because the geometry is placed in screen
                // points by the uniform exactly as the CPU path places it; the
                // clip that makes a stretched ancestor draw only the quarter that
                // belongs to this tile is egui's scissor, taken from the clip
                // rect the painter above carries.
                rect: piece,
                callback: payload,
            }),
            run.kind,
        ))
    }

    /// Whether an already-issued run has drawn the shape at `index`.
    fn covers(&self, index: usize) -> bool {
        index < self.covered_to
    }
}

/// Lay every label this pane collected out against **one** [`OccupiedAreas`].
///
/// walkers constructs its own inside the per-tile draw, so its collision test
/// cannot see across a tile seam and it draws a name once per tile that carries
/// it. One set of claimed areas for the whole pane is the fix, and it is why
/// the labels are a phase rather than part of the grid loop.
///
/// Called from the `CityLabels` arm of the pane's layer walk, so the names land
/// above the weather rather than under it, and the layer's toggle governs them
/// by simply not calling this.
///
/// The layout is finished before `extend` because laying a label out takes
/// `Context::fonts_mut` while `Painter::extend` holds the graphics lock;
/// interleaving them deadlocks.
pub(super) fn paint_labels(
    painter: &egui::Painter,
    labels: Vec<walkers::Text>,
    galleys: &mut walkers::GalleyCache,
) {
    if labels.is_empty() {
        return;
    }

    let mut occupied = walkers::OccupiedAreas::new();
    // Where each name has already been drawn, so a fragmented river is named
    // once per stretch of screen rather than once per OSM way. See
    // [`MIN_REPEAT_DISTANCE`].
    let mut placed_names: std::collections::HashMap<String, Vec<egui::Pos2>> =
        std::collections::HashMap::new();

    let mut placed: Vec<egui::Shape> = Vec::with_capacity(labels.len());

    for text in labels {
        let position = text.position;

        // Borrowed, never cloned: this runs per label per frame, and the name
        // is only ever moved into the map by a label that actually drew.
        if placed_names.get(&text.text).is_some_and(|anchors| {
            anchors
                .iter()
                .any(|at| at.distance(position) < MIN_REPEAT_DISTANCE)
        }) {
            continue;
        }

        let shape = lay_out_label(painter.ctx(), &text, &mut occupied, galleys);

        // Only a label that actually drew claims the spot. A name suppressed by
        // the collision test must not stop the same name drawing further along,
        // or one river losing a contest at a crowded confluence would be
        // silenced across the whole viewport.
        if !matches!(shape, egui::Shape::Noop) {
            placed_names.entry(text.text).or_default().push(position);
            placed.push(shape);
        }
    }

    painter.extend(placed);
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: egui::Vec2 = egui::vec2(800.0, 600.0);
    /// The pane, inset from the viewport on every side, so "outside the pane"
    /// and "off the screen" are different places.
    const PANE: egui::Rect =
        egui::Rect::from_min_max(egui::pos2(200.0, 80.0), egui::pos2(760.0, 520.0));

    /// A real context with a real floating `Area` at `dialog`, run for two
    /// passes so the area is registered whichever visibility rule egui applies.
    fn ctx_with_dialog(dialog: Option<egui::Rect>) -> egui::Context {
        let ctx = egui::Context::default();
        for _ in 0..2 {
            ctx.begin_pass(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, SCREEN)),
                ..Default::default()
            });
            if let Some(rect) = dialog {
                egui::Area::new(egui::Id::new("a_dialog"))
                    .order(egui::Order::Middle)
                    .fixed_pos(rect.min)
                    .interactable(true)
                    .show(&ctx, |ui| {
                        ui.allocate_exact_size(rect.size(), egui::Sense::click());
                    });
            }
            let _ = ctx.end_pass();
        }
        ctx
    }

    /// A source whose tiles can never arrive: a port nothing listens on, so a
    /// request fails at connect. This test counts drains, and a fetch that could
    /// succeed would only add a network to it.
    #[derive(Clone)]
    struct DeadSource;

    impl walkers::sources::TileSource for DeadSource {
        fn tile_url(&self, tile_id: TileId) -> String {
            format!(
                "http://127.0.0.1:1/{}/{}/{}.png",
                tile_id.zoom, tile_id.x, tile_id.y
            )
        }

        fn attribution(&self) -> walkers::sources::Attribution {
            walkers::sources::Attribution {
                text: "test",
                url: "http://127.0.0.1:1/",
                logo_light: None,
                logo_dark: None,
            }
        }
    }

    /// **The drain is per layer, not per tile.**
    ///
    /// `HttpsTiles::pump` is what moves finished fetches into the cache, and
    /// `draw_tile_layer` is its only caller. One layer must pump once, whatever
    /// number of grid cells the span turns out to hold — the defect this pins
    /// was one drain per cell, which at this canvas is a two-orders-of-magnitude
    /// difference on wasm32, where each drain reads `cumulative_pass_nr` under
    /// two `RwLock`s of the whole `Context`.
    ///
    /// The cell count is measured from the same `tile_span` the loop itself
    /// calls, never written down here: a literal would only hold the arithmetic
    /// this test did against itself.
    #[test]
    fn a_layer_drains_once_however_many_tiles_it_draws() {
        squallar_radar::tls::init();

        let ctx = egui::Context::default();
        let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1920.0, 1080.0));
        let zoom = 6.0;

        let mut memory = walkers::MapMemory::default();
        memory.set_zoom(zoom).expect("zoom 6 is in walkers' range");
        let projector = walkers::Projector::new(canvas, &memory, walkers::lat_lon(35.33, -97.28));

        let mut tiles = crate::tile_source::HttpsTiles::with_client(
            DeadSource,
            ctx.clone(),
            reqwest::Client::builder()
                .build()
                .expect("the test client should build"),
        );

        let tile_zoom = zoom.round() as u8;
        let cells = crate::tiles::tile_span(&projector, canvas, tile_zoom).tiles();
        assert!(
            cells > 1,
            "fixture: the span must name more than one cell, or per-cell and \
             per-layer are the same number"
        );

        ctx.begin_pass(egui::RawInput {
            screen_rect: Some(canvas),
            ..Default::default()
        });
        let ui = egui::Ui::new(
            ctx.clone(),
            egui::Id::new("draw_tile_layer_pump_count"),
            egui::UiBuilder::new()
                .layer_id(egui::LayerId::background())
                .max_rect(canvas),
        );

        let before = tiles.pumps();
        draw_tile_layer(&ui, &projector, zoom, &mut tiles, 0, None);
        let drains = tiles.pumps() - before;
        let _ = ctx.end_pass();

        assert_eq!(
            drains, 1,
            "a layer of {cells} cells drained {drains} times; one layer is one \
             drain, and {cells} is what a per-cell drain would have cost"
        );
    }

    /// **Drawing a layer asks for the ancestor net, and the net is small.**
    ///
    /// The call-site half of `HttpsTiles::warm`'s gate. Without this, a draw
    /// pass could stop requesting the net and only `tile_source`'s own suite —
    /// which calls `warm` by hand — would still be green, so the map would go
    /// black on a zoom-out with every test passing.
    ///
    /// Both halves are asserted because both can fail on their own: that every
    /// net tile covering the span was asked for (too few is a hole the
    /// zoom-out falls through), and that the net's size is the bound
    /// `tiles::tiles_resident_with_warm_net` sizes the LRU against (too many
    /// and the net evicts the glass it exists to back up).
    #[test]
    fn a_drawn_layer_asks_for_the_ancestor_net_and_no_more_than_its_bound() {
        squallar_radar::tls::init();

        let ctx = egui::Context::default();
        let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1920.0, 1080.0));
        let zoom = 6.0;

        let mut memory = walkers::MapMemory::default();
        memory.set_zoom(zoom).expect("zoom 6 is in walkers' range");
        let projector = walkers::Projector::new(canvas, &memory, walkers::lat_lon(35.33, -97.28));

        let mut tiles = crate::tile_source::HttpsTiles::with_client(
            DeadSource,
            ctx.clone(),
            reqwest::Client::builder()
                .build()
                .expect("the test client should build"),
        );

        let tile_zoom = zoom.round() as u8;
        let step = crate::tiles::WARM_ANCESTOR_STEPS;
        let net_zoom = tile_zoom
            .checked_sub(step)
            .expect("fixture: the drawn zoom must be deeper than the net");
        let span = crate::tiles::tile_span(&projector, canvas, tile_zoom);

        ctx.begin_pass(egui::RawInput {
            screen_rect: Some(canvas),
            ..Default::default()
        });
        let ui = egui::Ui::new(
            ctx.clone(),
            egui::Id::new("draw_tile_layer_warm_net"),
            egui::UiBuilder::new()
                .layer_id(egui::LayerId::background())
                .max_rect(canvas),
        );
        draw_tile_layer(&ui, &projector, zoom, &mut tiles, 0, None);
        let _ = ctx.end_pass();

        // Every net tile the drawn span sits under was asked for. The source
        // never answers, so a hit here is the *request* -- which is the thing
        // that has to happen ahead of the zoom-out, not the arrival.
        let mut net_tiles = 0_usize;
        for ty in (span.north >> step)..=(span.south >> step) {
            for tx in (span.west >> step)..=(span.east >> step) {
                net_tiles += 1;
                let net = TileId {
                    x: tx,
                    y: ty,
                    zoom: net_zoom,
                };
                assert!(
                    tiles.tile_is_cached(net),
                    "the draw pass did not ask for {net:?}, so a zoom-out over \
                     that cell has no ancestor to stretch and draws a hole",
                );
            }
        }
        assert!(
            net_tiles > 0,
            "fixture: the net must name at least one tile, or the loop above \
             asserted nothing"
        );

        // The net stays inside what the LRU was sized for. The bound counts
        // the drawn level too, so subtract what the span actually named.
        let bound = crate::tiles::tiles_resident_with_warm_net(canvas, 0, 1)
            - crate::tiles::tiles_resident_for(canvas, 0, 1);
        assert!(
            net_tiles <= bound,
            "the net asked for {net_tiles} tiles where the cache is sized for \
             {bound}: the net now evicts the glass it exists to back up",
        );
    }

    // -----------------------------------------------------------------------
    // The vector draw seam
    //
    // These drive the painter with hand-built `Tile::Vector` values rather
    // than an open archive, so the seam — placement, not rendering — is
    // pinned in isolation from the IO machinery that produces real tiles.
    // -----------------------------------------------------------------------

    use walkers::{ShapeOrText, Text};

    /// A tile whose shapes span the whole MVT extent, so every assertion below
    /// is about where the seam *put* them and not about what a renderer chose.
    ///
    /// `EXTENT` is `walkers::mvt`'s only supported layer extent. Spelled here
    /// because the constant is private there.
    const EXTENT: f32 = 4096.0;

    fn extent_spanning_tile() -> Tile {
        Tile::Vector(std::sync::Arc::new(vec![
            // Corner to corner: after placement this is the tile's own rect.
            ShapeOrText::Shape(egui::Shape::rect_filled(
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(EXTENT, EXTENT)),
                0.0,
                egui::Color32::from_rgb(0x10, 0x20, 0x30),
            )),
            // A label at the centre of the extent.
            ShapeOrText::Text(Text::new(
                egui::pos2(EXTENT / 2.0, EXTENT / 2.0),
                "Monaco".to_owned(),
                12.0,
                egui::Color32::WHITE,
                0.0,
            )),
        ]))
    }

    /// Every shape one pass emitted, with its clip rect.
    fn shapes_of_one_pass(
        ctx: &egui::Context,
        canvas: egui::Rect,
        draw: impl FnOnce(&egui::Ui),
    ) -> Vec<egui::epaint::ClippedShape> {
        ctx.begin_pass(egui::RawInput {
            screen_rect: Some(canvas),
            ..Default::default()
        });
        let ui = egui::Ui::new(
            ctx.clone(),
            egui::Id::new("vector_seam"),
            egui::UiBuilder::new()
                .layer_id(egui::LayerId::background())
                .max_rect(canvas),
        );
        draw(&ui);
        ctx.end_pass().shapes
    }

    /// **The seam draws.** A `Tile::Vector` handed to `draw_tile_layer` reaches
    /// the painter as real geometry, placed on the tile's own rect.
    ///
    /// This is the assertion the campaign never had: before it, the vector arm
    /// painted a magenta rectangle and tripped a `debug_assert!`, so the only
    /// thing a green board proved about `Tile::Vector` was that nothing
    /// produced one.
    #[test]
    fn a_vector_tile_reaches_the_painter_placed_on_its_own_rect() {
        squallar_radar::tls::init();

        let ctx = egui::Context::default();
        let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1920.0, 1080.0));
        let zoom = 6.0;

        let mut memory = walkers::MapMemory::default();
        memory.set_zoom(zoom).expect("zoom 6 is in walkers' range");
        let projector = walkers::Projector::new(canvas, &memory, walkers::lat_lon(35.33, -97.28));

        let mut tiles = crate::tile_source::HttpsTiles::with_client(
            DeadSource,
            ctx.clone(),
            reqwest::Client::builder()
                .build()
                .expect("the test client should build"),
        );

        // One cell of the span the loop will walk, filled with a vector tile.
        let tile_zoom = zoom.round() as u8;
        let span = crate::tiles::tile_span(&projector, canvas, tile_zoom);
        let tile_id = TileId {
            x: span.west,
            y: span.north,
            zoom: tile_zoom,
        };
        tiles.put_for_test(tile_id, extent_spanning_tile());
        let rect = projector.tile_rect(tile_id);

        let shapes = shapes_of_one_pass(&ctx, canvas, |ui| {
            let labels = draw_tile_layer(ui, &projector, zoom, &mut tiles, 0, None).labels;
            // The pane's `CityLabels` arm, which is where the deferred labels
            // are painted; without it this pass draws ground and no names.
            paint_labels(ui.painter(), labels, &mut walkers::GalleyCache::default());
        });

        // NON-VACUITY, and the specific thing that would have passed before:
        // the old arm emitted exactly one `rect_filled` in MAGENTA. So "some
        // shape was emitted" is not the assertion -- the fill has to be the
        // colour the tile carried, at the tile's own rect.
        let painted_fill = shapes.iter().any(|clipped| {
            matches!(
                &clipped.shape,
                egui::Shape::Rect(r)
                    if r.fill == egui::Color32::from_rgb(0x10, 0x20, 0x30)
                        && (r.rect.min - rect.min).length() < 0.01
                        && (r.rect.max - rect.max).length() < 0.01
            )
        });
        assert!(
            painted_fill,
            "the vector tile's extent-spanning fill did not arrive at {rect:?}; \
             shapes were {:?}",
            shapes.iter().map(|c| &c.shape).collect::<Vec<_>>()
        );

        assert!(
            !shapes.iter().any(
                |c| matches!(&c.shape, egui::Shape::Rect(r) if r.fill == egui::Color32::MAGENTA)
            ),
            "the did-not-render marker is still being painted"
        );

        // The label was laid out and placed, not dropped. `Text` becomes a
        // `TextShape`, which is what proves `lay_out_label` ran rather than the
        // variant being skipped.
        let label = shapes
            .iter()
            .find_map(|c| match &c.shape {
                egui::Shape::Text(t) => Some(t),
                _ => None,
            })
            .expect("the tile's label did not reach the painter");
        assert_eq!(label.galley.job.text, "Monaco");

        // The label sat at the centre of the extent, so it must sit at the
        // centre of the tile -- `OrientedRect::top_left` offsets it by half the
        // galley, so the comparison is against the galley's centre.
        //
        // `galley.rect.center()` and not `size() / 2.0`: labels are laid out
        // with `halign: Center` so that a wrapped name's rows are centred on
        // its anchor, which puts the galley's own origin on its centre line
        // rather than at its top-left corner. The two spellings agree only for
        // a left-aligned galley. **The asserted position did not move** -- this
        // is the same "at the tile centre" check, measured correctly for a
        // centred galley.
        let placed_centre = label.pos + label.galley.rect.center().to_vec2();
        assert!(
            (placed_centre - rect.center()).length() < 0.01,
            "the label landed at {placed_centre:?}, not the tile centre {:?}",
            rect.center()
        );
    }

    /// **A stretched ancestor is placed against the whole tile and clipped to
    /// the piece.** The `uv` window is what `HttpsTiles::at` answers a deep
    /// request with when only a shallower tile is cached, and a vector tile's
    /// shapes are in extent coordinates over the *whole* tile, so ignoring `uv`
    /// would squeeze a whole ancestor into a quarter of its area.
    #[test]
    fn a_uv_window_places_against_the_whole_tile_and_clips_to_the_piece() {
        let piece = egui::Rect::from_min_size(egui::pos2(100.0, 200.0), egui::vec2(64.0, 64.0));
        // The north-west quarter of the tile.
        let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(0.5, 0.5));

        let full = full_rect_of_clipped_tile(piece, uv);

        assert_eq!(full.min, piece.min, "the north-west corner is shared");
        assert!(
            (full.width() - 128.0).abs() < 1e-4 && (full.height() - 128.0).abs() < 1e-4,
            "a quarter window means a tile twice as wide, got {full:?}"
        );

        // The south-east quarter, which moves the origin as well as the size.
        let uv = egui::Rect::from_min_max(egui::pos2(0.5, 0.5), egui::pos2(1.0, 1.0));
        let full = full_rect_of_clipped_tile(piece, uv);
        assert_eq!(full.max, piece.max, "the south-east corner is shared");
        assert!(
            (full.min.x - 36.0).abs() < 1e-4 && (full.min.y - 136.0).abs() < 1e-4,
            "the whole tile starts a piece-width north-west of the piece, got {full:?}"
        );

        // And the identity window changes nothing.
        let whole = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        assert_eq!(full_rect_of_clipped_tile(piece, whole), piece);
    }

    /// The painter is clipped to the piece, so a shape that overhangs the tile
    /// cannot bleed over its neighbour.
    #[test]
    fn the_vector_painter_is_clipped_to_the_tile() {
        let ctx = egui::Context::default();
        let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        let rect = egui::Rect::from_min_size(egui::pos2(100.0, 100.0), egui::vec2(256.0, 256.0));

        let shapes = shapes_of_one_pass(&ctx, canvas, |ui| {
            paint_vector_tile(
                ui.painter(),
                &[ShapeOrText::Shape(egui::Shape::rect_filled(
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(EXTENT, EXTENT)),
                    0.0,
                    egui::Color32::RED,
                ))],
                GroundMeshes::CPU_ONLY,
                rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                &mut Vec::new(),
            );
        });

        let clipped: Vec<_> = shapes
            .iter()
            .filter(|c| matches!(&c.shape, egui::Shape::Rect(r) if r.fill == egui::Color32::RED))
            .collect();
        assert_eq!(clipped.len(), 1, "the fill was not painted exactly once");
        assert_eq!(
            clipped[0].clip_rect.intersect(rect),
            clipped[0].clip_rect,
            "the clip rect {:?} is not inside the tile {rect:?}",
            clipped[0].clip_rect
        );
    }

    /// Draw `tiles` as `(shapes, rect)` pieces through the ground phase, then
    /// run the one label phase over everything they deferred -- which is what
    /// `draw_tile_layer` does across a span, in miniature.
    fn ground_then_labels(ui: &egui::Ui, tiles: &[(Vec<ShapeOrText>, egui::Rect)]) {
        let whole = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        let mut labels = Vec::new();
        for (shapes, rect) in tiles {
            paint_vector_tile(
                ui.painter(),
                shapes,
                GroundMeshes::CPU_ONLY,
                *rect,
                whole,
                &mut labels,
            );
        }
        paint_labels(ui.painter(), labels, &mut walkers::GalleyCache::default());
    }

    fn text_count(shapes: &[egui::epaint::ClippedShape]) -> usize {
        shapes
            .iter()
            .filter(|c| matches!(&c.shape, egui::Shape::Text(_)))
            .count()
    }

    /// A label `x` across the extent, on the extent's horizontal midline.
    fn label_at(x: f32, name: &str) -> ShapeOrText {
        ShapeOrText::Text(Text::new(
            egui::pos2(x, EXTENT / 2.0),
            name.to_owned(),
            12.0,
            egui::Color32::WHITE,
            0.0,
        ))
    }

    /// **Two labels claiming the same screen: one wins.** The collision is what
    /// `OccupiedAreas` is for, and a seam that dropped it would draw legible
    /// text on top of legible text and still look plausible in a screenshot.
    #[test]
    fn overlapping_labels_collide_and_only_one_is_drawn() {
        let ctx = egui::Context::default();
        let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(256.0, 256.0));

        // Same point, twice.
        let shapes = shapes_of_one_pass(&ctx, canvas, |ui| {
            ground_then_labels(
                ui,
                &[(
                    vec![
                        label_at(EXTENT / 2.0, "Monaco"),
                        label_at(EXTENT / 2.0, "Monte-Carlo"),
                    ],
                    rect,
                )],
            );
        });
        assert_eq!(
            text_count(&shapes),
            1,
            "two labels at one point must collide to one"
        );

        // Far apart, and both survive -- the control that stops the assertion
        // above from passing because labels are simply never drawn.
        let shapes = shapes_of_one_pass(&ctx, canvas, |ui| {
            ground_then_labels(
                ui,
                &[(
                    vec![
                        label_at(EXTENT / 8.0, "Monaco"),
                        label_at(EXTENT * 7.0 / 8.0, "Monte-Carlo"),
                    ],
                    rect,
                )],
            );
        });
        assert_eq!(text_count(&shapes), 2, "labels a tile apart do not collide");
    }

    /// **The seam duplicate is gone: one `OccupiedAreas` spans the pane.**
    ///
    /// Two adjoining tiles that both carry the same place -- which is what a
    /// vector tile's buffer guarantees -- must put one name on the glass, not
    /// two. walkers builds its `OccupiedAreas` inside the per-tile draw, so its
    /// collision test cannot see the neighbour, and this is exactly the case it
    /// gets wrong.
    #[test]
    fn a_place_carried_by_two_adjoining_tiles_is_drawn_once() {
        let ctx = egui::Context::default();
        let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));

        let west = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(256.0, 256.0));
        let east = egui::Rect::from_min_size(egui::pos2(256.0, 0.0), egui::vec2(256.0, 256.0));

        // The place sits just inside the WEST tile's eastern edge, so the west
        // tile owns the anchor and the east tile carries it only in its buffer
        // -- a negative extent coordinate, west of its own origin, which lands
        // on the same screen point.
        let shapes = shapes_of_one_pass(&ctx, canvas, |ui| {
            ground_then_labels(
                ui,
                &[
                    (vec![label_at(EXTENT * 0.99, "Topeka")], west),
                    (vec![label_at(-EXTENT * 0.01, "Topeka")], east),
                ],
            );
        });

        assert_eq!(
            text_count(&shapes),
            1,
            "the same place carried by two tiles reached the glass twice"
        );
    }

    /// The control for the test above: **the ownership rule is not simply
    /// eating every label the second tile has.** A place genuinely inside the
    /// east tile still draws, so "one label" above is a rule about anchors and
    /// not a renderer that stopped drawing past the first tile.
    #[test]
    fn a_place_of_its_own_in_the_second_tile_still_draws() {
        let ctx = egui::Context::default();
        let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));

        let west = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(256.0, 256.0));
        let east = egui::Rect::from_min_size(egui::pos2(256.0, 0.0), egui::vec2(256.0, 256.0));

        let shapes = shapes_of_one_pass(&ctx, canvas, |ui| {
            ground_then_labels(
                ui,
                &[
                    (vec![label_at(EXTENT / 2.0, "Topeka")], west),
                    (vec![label_at(EXTENT / 2.0, "Lawrence")], east),
                ],
            );
        });

        assert_eq!(text_count(&shapes), 2, "each tile's own place must draw");
    }

    /// **A label is not clipped to the tile that carried it.** Its glyphs are
    /// laid out in the label phase, whose painter is the pane's, so a name
    /// wider than its distance to the seam draws whole instead of being cut in
    /// half -- which is the other half of what the per-tile draw got wrong.
    #[test]
    fn a_label_is_clipped_to_the_pane_and_not_to_its_tile() {
        let ctx = egui::Context::default();
        let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        let tile = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(256.0, 256.0));

        let shapes = shapes_of_one_pass(&ctx, canvas, |ui| {
            ground_then_labels(ui, &[(vec![label_at(EXTENT * 0.99, "Topeka")], tile)]);
        });

        let label = shapes
            .iter()
            .find(|c| matches!(&c.shape, egui::Shape::Text(_)))
            .expect("the label reached the painter");
        assert!(
            label.clip_rect.max.x > tile.max.x,
            "the label was clipped to its own tile ({:?} against {tile:?}), so a \
             name at the seam is still being cut in half",
            label.clip_rect
        );
    }

    /// **A styled `line-width` arrives on screen at that width.**
    ///
    /// The factor lives in `vendor/walkers/src/mvt.rs` and is
    /// `ONLY_SUPPORTED_EXTENT / TILE_SIDE_POINTS`; this is the end of the chain
    /// it was chosen for, so it is measured here rather than asserted there.
    /// Upstream's 4.0 puts this at 2.0 points instead of 8.0.
    #[test]
    fn a_styled_line_width_arrives_on_screen_at_that_width() {
        let style = walkers::Style::from_json(
            r##"{"layers":[{"type":"line","source-layer":"transportation",
                 "paint":{"line-color":"#ff0000","line-width":8}}]}"##,
        )
        .expect("the fixture style parses");

        let paint = style
            .layers
            .iter()
            .find_map(|layer| match layer {
                walkers::Layer::Line { paint, .. } => Some(paint),
                _ => None,
            })
            .expect("the fixture style has a line layer");

        let context = walkers::Context::new("LineString", Default::default(), 14);
        let mut shapes = Vec::new();
        walkers::render_line(
            &walkers::mvt::Geometry::LineString(vec![(0.0_f32, 0.0_f32), (EXTENT, EXTENT)].into()),
            &context,
            &mut shapes,
            paint,
        )
        .expect("a line string renders");

        // Placed onto a tile drawn at `TILE_SIDE_POINTS`, which is what a whole
        // zoom at bias 0 gives.
        let rect = egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::Vec2::splat(crate::tiles::TILE_SIDE_POINTS),
        );
        let placed = walkers::mvt::transformed(&shapes, rect);

        let width = placed
            .iter()
            .find_map(|s| match s {
                ShapeOrText::Shape(egui::Shape::Path(p)) => Some(p.stroke.width),
                _ => None,
            })
            .expect("the line reached the placed shapes as a path");

        assert!(
            (width - 8.0).abs() < 1e-3,
            "a style asking for 8 points drew {width}; the extent-to-points \
             factor in vendor/walkers/src/mvt.rs does not match \
             TILE_SIDE_POINTS = {}",
            crate::tiles::TILE_SIDE_POINTS
        );
    }
    // -----------------------------------------------------------------------
    // The label phase: wrapping, repetition and haloes
    // -----------------------------------------------------------------------

    /// The 77-character name that spanned the user's whole viewport as one
    /// line. Every one of its five OpenMapTiles name fields carries this exact
    /// string, so there is no shorter variant to select and wrapping is the
    /// only way it fits.
    const LONG_NAME: &str =
        "Kiowa Indian Tribe, Comanche Nation, Apache Tribe, and Fort Sill Apache Tribe";

    fn label(name: &str, at: egui::Pos2) -> Text {
        Text::new(at, name.to_owned(), 12.0, egui::Color32::WHITE, 0.0)
    }

    /// **A kept galley memo paints what a fresh one paints, frame after
    /// frame.**
    ///
    /// The unit tests in `walkers::text` hold the galley identity; this holds
    /// the one that matters on the glass, through the real `paint_labels`:
    /// three passes over the same names, one cache carried across all of them,
    /// against three passes each with its own. Every `TextShape` must match in
    /// position, colour and laid-out text — a memo that answered a stale
    /// galley, or that changed placement order, shows up here.
    ///
    /// The labels move between passes, because that is the case the memo is
    /// built for: panning re-uses every entry, so a kept cache must still lay
    /// the frame out from scratch positionally while re-using the glyphs.
    #[test]
    fn a_kept_galley_cache_paints_what_a_fresh_one_paints() {
        let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, SCREEN);
        let names = ["Washita River", "Oklahoma City", "Lake Thunderbird"];
        let offsets = [0.0_f32, 17.0, -23.0];

        let describe =
            |shapes: &[egui::epaint::ClippedShape]| -> Vec<(String, egui::Pos2, egui::Color32)> {
                text_shapes(shapes)
                    .iter()
                    .map(|t| (t.galley.text().to_owned(), t.pos, t.fallback_color))
                    .collect()
            };

        let ctx_kept = egui::Context::default();
        let mut kept = walkers::GalleyCache::default();
        let mut kept_frames = Vec::new();
        for dx in offsets {
            let labels: Vec<Text> = names
                .iter()
                .enumerate()
                .map(|(i, n)| label(n, egui::pos2(200.0 + dx, 100.0 + 90.0 * i as f32)))
                .collect();
            let shapes = shapes_of_one_pass(&ctx_kept, canvas, |ui| {
                paint_labels(ui.painter(), labels, &mut kept);
            });
            kept_frames.push(describe(&shapes));
        }

        let ctx_fresh = egui::Context::default();
        let mut fresh_frames = Vec::new();
        for dx in offsets {
            let labels: Vec<Text> = names
                .iter()
                .enumerate()
                .map(|(i, n)| label(n, egui::pos2(200.0 + dx, 100.0 + 90.0 * i as f32)))
                .collect();
            let shapes = shapes_of_one_pass(&ctx_fresh, canvas, |ui| {
                paint_labels(ui.painter(), labels, &mut walkers::GalleyCache::default());
            });
            fresh_frames.push(describe(&shapes));
        }

        assert_eq!(kept_frames, fresh_frames);
        assert!(
            !kept_frames[0].is_empty(),
            "the fixture drew no labels, so the comparison proves nothing",
        );
        // The memo did its job: three passes over three names, laid out once.
        assert_eq!(kept.layouts(), names.len() as u64);
        assert_eq!(kept.hits(), 2 * names.len() as u64);
    }

    /// Every `TextShape` a pass emitted, reaching inside a haloed label's
    /// `Shape::Vec`.
    fn text_shapes(shapes: &[egui::epaint::ClippedShape]) -> Vec<&egui::epaint::TextShape> {
        fn walk<'a>(shape: &'a egui::Shape, into: &mut Vec<&'a egui::epaint::TextShape>) {
            match shape {
                egui::Shape::Text(text) => into.push(text),
                egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| walk(s, into)),
                _ => {}
            }
        }
        let mut found = Vec::new();
        for clipped in shapes {
            walk(&clipped.shape, &mut found);
        }
        found
    }

    /// **`text-max-width` wraps, and the collision box covers the block.**
    ///
    /// The defect: no wrapping existed anywhere in walkers, so a style's
    /// `text-max-width` was parsed by nothing and every label drew as one run.
    #[test]
    fn a_long_name_wraps_to_its_styled_width() {
        let ctx = egui::Context::default();
        let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));

        ctx.begin_pass(egui::RawInput {
            screen_rect: Some(canvas),
            ..Default::default()
        });

        // NON-VACUITY / the control: unwrapped, this is the viewport-spanning
        // run the user is looking at.
        let unwrapped = label(LONG_NAME, egui::pos2(400.0, 300.0)).galley(&ctx);
        assert_eq!(unwrapped.rows.len(), 1, "the control must be a single run");
        assert!(
            unwrapped.size().x > 400.0,
            "the control is {} pt wide; it is supposed to be wider than the pane",
            unwrapped.size().x
        );

        // `place_city_dot_z7`, which is the layer this name actually draws
        // through, asks for `text-max-width: 8`.
        let wrapped = label(LONG_NAME, egui::pos2(400.0, 300.0))
            .with_wrapping(Some(8.0), None)
            .galley(&ctx);

        assert!(
            wrapped.rows.len() > 1,
            "an 8-em cap left the name on one row"
        );
        assert!(
            wrapped.size().x <= 8.0 * 12.0,
            "the wrapped block is {} pt wide, over the 96 pt the style asked for",
            wrapped.size().x
        );
        // The block is taller because it is narrower: nothing was dropped.
        assert!(
            wrapped.size().y > unwrapped.size().y,
            "the wrapped block is no taller than one row, so text went missing"
        );
        assert_eq!(
            wrapped.job.text, unwrapped.job.text,
            "wrapping must not change the text itself"
        );

        let _ = ctx.end_pass();
    }

    /// A short name is not padded out to the wrap width, so wrapping cannot
    /// silently inflate every collision box on the map.
    #[test]
    fn a_short_name_is_not_widened_by_its_wrap_limit() {
        let ctx = egui::Context::default();
        ctx.begin_pass(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            ..Default::default()
        });

        let galley = label("Enid", egui::pos2(100.0, 100.0))
            .with_wrapping(Some(10.0), None)
            .galley(&ctx);

        assert_eq!(galley.rows.len(), 1);
        assert!(
            galley.size().x < 10.0 * 12.0 * 0.5,
            "a four-letter name measured {} pt against a 120 pt cap",
            galley.size().x
        );
        let _ = ctx.end_pass();
    }

    /// **One river, one label per stretch of screen.**
    ///
    /// OSM splits a way at every tag change and confluence, so a watercourse
    /// arrives as many `LineString`s and each asks for its own label -- the
    /// user counted "Rio Grande" six times in one viewport. The rule is a
    /// distance and not a set, so a river crossing the whole pane is still
    /// named at both ends.
    #[test]
    fn a_river_split_into_fragments_is_named_once_per_stretch() {
        let ctx = egui::Context::default();
        let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 600.0));

        // Four fragments of one river. The spacing is chosen so the ordinary
        // overlap test cannot decide this: at 12 pt "Rio Grande" is about 63 pt
        // wide, so 80 pt apart the boxes do not touch, and the whole run is
        // 240 pt end to end, inside `MIN_REPEAT_DISTANCE`.
        let anchors: Vec<egui::Pos2> = (0..4)
            .map(|i| egui::pos2(100.0 + 80.0 * i as f32, 300.0))
            .collect();

        // THE CONTROL FIRST, because it establishes that these four positions
        // are ones the collision test lets through. Without it "one label"
        // below would also be what a plain overlap would have produced, and
        // this test would be evidence of nothing.
        let distinct: Vec<Text> = anchors
            .iter()
            .enumerate()
            .map(|(i, at)| label(&format!("River {i}"), *at))
            .collect();
        let shapes = shapes_of_one_pass(&ctx, canvas, |ui| {
            paint_labels(ui.painter(), distinct, &mut walkers::GalleyCache::default());
        });
        assert_eq!(
            text_shapes(&shapes).len(),
            4,
            "fixture: these four anchors must not collide, or the assertion \
             below is about the overlap rule instead of the repeat rule"
        );

        let crowded: Vec<Text> = anchors.iter().map(|at| label("Rio Grande", *at)).collect();
        let shapes = shapes_of_one_pass(&ctx, canvas, |ui| {
            paint_labels(ui.painter(), crowded, &mut walkers::GalleyCache::default());
        });
        assert_eq!(
            text_shapes(&shapes).len(),
            1,
            "four fragments of one river put four labels on the glass"
        );

        // THE CONTROL, in two directions at once. A name far enough away is a
        // second reading and must still draw, and two *different* names in the
        // same place must not be collapsed into one.
        let spread = vec![
            label("Rio Grande", egui::pos2(100.0, 300.0)),
            label(
                "Rio Grande",
                egui::pos2(100.0 + MIN_REPEAT_DISTANCE + 50.0, 300.0),
            ),
            // On its own row: near enough that a name-only rule would be
            // tempted by it, far enough that the ordinary overlap test -- which
            // is a different rule and is not what this control is about --
            // leaves it alone.
            label("Rio Salado", egui::pos2(140.0, 400.0)),
        ];
        let shapes = shapes_of_one_pass(&ctx, canvas, |ui| {
            paint_labels(ui.painter(), spread, &mut walkers::GalleyCache::default());
        });
        let drawn: Vec<&str> = text_shapes(&shapes)
            .iter()
            .map(|t| t.galley.job.text.as_str())
            .collect();
        assert_eq!(
            drawn.iter().filter(|t| **t == "Rio Grande").count(),
            2,
            "a river spanning the pane must be readable at both ends, got {drawn:?}"
        );
        assert!(
            drawn.contains(&"Rio Salado"),
            "a different river was eaten by the repeat rule, got {drawn:?}"
        );
    }

    /// **A label is exactly one text draw and no box.**
    ///
    /// Two halo approximations have shipped here and both were withdrawn after
    /// looking at them. `TextFormat::background` fills the galley's bounding
    /// rectangle, so a style's `text-halo-color` painted a translucent slab
    /// behind the whole label. Redrawing the glyphs at eight offsets around a
    /// circle -- the standard approximation short of a glyph atlas -- read as
    /// fuzzy and unevenly weighted, because each copy is alpha-blended
    /// anti-aliased text and the coverage stacks differently around different
    /// letter edges.
    ///
    /// This pins the withdrawal in both directions at once: not more than one
    /// draw, and not a rectangle either. A real SDF halo would make it go red,
    /// which is correct -- that would be a deliberate change, not a regression.
    #[test]
    fn a_label_is_one_draw_and_no_background_box() {
        let ctx = egui::Context::default();
        let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));

        let shapes = shapes_of_one_pass(&ctx, canvas, |ui| {
            paint_labels(
                ui.painter(),
                vec![label("Washita River", egui::pos2(400.0, 300.0))],
                &mut walkers::GalleyCache::default(),
            );
        });

        let texts = text_shapes(&shapes);
        assert_eq!(
            texts.len(),
            1,
            "a label drew {} times, not once",
            texts.len()
        );
        assert_eq!(
            texts[0].fallback_color,
            egui::Color32::WHITE,
            "the one draw is the text itself"
        );
        assert!(
            !shapes
                .iter()
                .any(|c| matches!(&c.shape, egui::Shape::Rect(_))),
            "a background rectangle is being painted behind the label"
        );
    }

    /// **A name too long to set in [`MAX_LABEL_ROWS`] is not drawn at all.**
    ///
    /// Wrapping it was tried and rejected on the glass: at its layer's
    /// `text-max-width` the tribal-nation name becomes a 92x70 pt block that
    /// dominates the view, and unwrapped it spans the pane. Most maps simply do
    /// not label that area at this zoom.
    #[test]
    fn a_name_too_long_to_set_in_two_rows_is_not_drawn() {
        let ctx = egui::Context::default();
        let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));

        // `place_city_dot_z7`, the layer this name actually draws through, asks
        // for `text-max-width: 8`.
        let long = label(LONG_NAME, egui::pos2(400.0, 300.0)).with_wrapping(Some(8.0), None);
        let shapes = shapes_of_one_pass(&ctx, canvas, |ui| {
            paint_labels(
                ui.painter(),
                vec![long],
                &mut walkers::GalleyCache::default(),
            );
        });
        assert!(
            text_shapes(&shapes).is_empty(),
            "the 77-character name was drawn anyway"
        );

        // THE CONTROLS, and they are what make the rule a length rule rather
        // than a ban on wrapping. Both of these genuinely wrap -- they are the
        // two-row tribal names sitting beside the long one in the same view --
        // and both must survive.
        for name in ["Iowa Tribe of Oklahoma", "Seneca-Cayuga Nation"] {
            let text = label(name, egui::pos2(400.0, 300.0)).with_wrapping(Some(8.0), None);
            let rows = text.galley(&ctx).rows.len();
            assert!(
                rows > 1,
                "fixture: {name:?} is supposed to wrap, and took {rows} row"
            );
            assert!(
                rows <= MAX_LABEL_ROWS,
                "fixture: {name:?} took {rows} rows, over the cap"
            );

            let shapes = shapes_of_one_pass(&ctx, canvas, |ui| {
                paint_labels(
                    ui.painter(),
                    vec![text],
                    &mut walkers::GalleyCache::default(),
                );
            });
            assert_eq!(
                text_shapes(&shapes).len(),
                1,
                "{name:?} wraps to two rows and must still be drawn"
            );
        }
    }

    /// Each of the three conditions must block **on its own**.
    #[test]
    fn each_condition_blocks_a_position_on_its_own() {
        let clear = egui::pos2(400.0, 300.0);
        assert!(
            PANE.contains(clear),
            "fixture: the control point is on the pane"
        );

        let excluded = egui::Rect::from_min_size(egui::pos2(220.0, 100.0), egui::vec2(48.0, 48.0));
        let on_excluded = excluded.center();
        let dialog = egui::Rect::from_min_size(egui::pos2(500.0, 350.0), egui::vec2(120.0, 90.0));
        let on_dialog = dialog.center();
        // Outside the pane but still on screen: the sidebar / status-bar case.
        let off_pane = egui::pos2(100.0, 300.0);

        let bare = ctx_with_dialog(None);
        let with_dialog = ctx_with_dialog(Some(dialog));

        assert!(
            !is_pos_blocked(&bare, clear, PANE, &[]),
            "a plain spot on the map must not be blocked, or every row below \
             passes for free"
        );

        assert!(
            is_pos_blocked(&bare, off_pane, PANE, &[]),
            "a position outside the pane must be blocked by the pane check \
             alone: nothing is excluded and no layer floats over it"
        );

        assert!(
            !bare
                .layer_id_at(on_excluded)
                .is_some_and(|l| l.order > egui::Order::Background),
            "fixture: nothing floats over the excluded rect, so only the \
             excluded-rect check can block it"
        );
        assert!(
            is_pos_blocked(&bare, on_excluded, PANE, &[excluded]),
            "a position on an excluded rect must be blocked by the excluded-rect \
             check alone"
        );
        assert!(
            !is_pos_blocked(&bare, on_excluded, PANE, &[]),
            "…and only because it was excluded: the same point with an empty \
             list must fall through"
        );

        assert!(
            PANE.contains(on_dialog),
            "fixture: the dialog sits over the pane, so only the layer check \
             can block it"
        );
        assert!(
            is_pos_blocked(&with_dialog, on_dialog, PANE, &[]),
            "a position on a floating layer must be blocked by the layer check \
             alone"
        );
        assert!(
            !is_pos_blocked(&bare, on_dialog, PANE, &[]),
            "…and only because of the layer: with no dialog open the same point \
             is ordinary map"
        );
    }

    /// **The coverage answer is a measurement of the span, not a belief about
    /// the source.** Complete only when every cell was answered by its exact
    /// tile: a hole, a stretched ancestor and a header still on its way are
    /// three different pending states and each must read incomplete — the
    /// floor-strip cache skips repaints on this answer, and a false `complete`
    /// freezes a 3D floor on whatever was on it.
    #[test]
    fn coverage_is_complete_only_when_every_cell_is_answered_exactly() {
        squallar_radar::tls::init();

        let ctx = egui::Context::default();
        let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(512.0, 512.0));
        let zoom = 6.0;
        let mut memory = walkers::MapMemory::default();
        memory.set_zoom(zoom).expect("zoom 6 is in walkers' range");
        let projector = walkers::Projector::new(canvas, &memory, walkers::lat_lon(35.33, -97.28));

        let mut tiles = crate::tile_source::HttpsTiles::with_client(
            DeadSource,
            ctx.clone(),
            reqwest::Client::builder()
                .build()
                .expect("the test client should build"),
        );

        let tile_zoom = zoom.round() as u8;
        let span = crate::tiles::tile_span(&projector, canvas, tile_zoom);

        // Nothing answered yet: incomplete.
        let empty = shapes_of_one_pass(&ctx, canvas, |ui| {
            let paint = draw_tile_layer(ui, &projector, zoom, &mut tiles, 0, None);
            assert!(
                !paint.coverage.complete(),
                "an unanswered span read complete; the strip cache would freeze \
                 an empty floor"
            );
        });
        drop(empty);

        // Every cell but one: still incomplete.
        let mut cells: Vec<TileId> = Vec::new();
        for ty in span.north..=span.south {
            for tx in span.west..=span.east {
                cells.push(TileId {
                    x: tx,
                    y: ty,
                    zoom: tile_zoom,
                });
            }
        }
        assert!(
            cells.len() > 1,
            "fixture: the span must hold more than one cell, or the hole case \
             and the empty case are the same case"
        );
        for cell in &cells[1..] {
            tiles.put_for_test(*cell, extent_spanning_tile());
        }
        shapes_of_one_pass(&ctx, canvas, |ui| {
            let paint = draw_tile_layer(ui, &projector, zoom, &mut tiles, 0, None);
            assert!(
                !paint.coverage.complete(),
                "a span with one hole read complete"
            );
        });

        // The hole answered by a stretched ancestor: still incomplete. The
        // ancestor keeps the glass populated, which is exactly why coverage
        // must not read it as the answer.
        let ancestor = TileId {
            x: cells[0].x / 2,
            y: cells[0].y / 2,
            zoom: tile_zoom - 1,
        };
        tiles.put_for_test(ancestor, extent_spanning_tile());
        shapes_of_one_pass(&ctx, canvas, |ui| {
            let paint = draw_tile_layer(ui, &projector, zoom, &mut tiles, 0, None);
            assert!(
                !paint.coverage.complete(),
                "a span answered through a stretched ancestor read complete"
            );
        });

        // The last exact tile lands: complete.
        tiles.put_for_test(cells[0], extent_spanning_tile());
        shapes_of_one_pass(&ctx, canvas, |ui| {
            let paint = draw_tile_layer(ui, &projector, zoom, &mut tiles, 0, None);
            assert!(
                paint.coverage.complete(),
                "a fully answered span read incomplete; the strip cache could \
                 never skip and the whole lever is dead"
            );
        });
    }

    // -----------------------------------------------------------------
    // The ground-mesh split.
    //
    // Every test below reads process-global counters
    // (`tile_mesh::ledger`), so they take one lock and reset inside it.
    // Cargo runs tests in threads of one process; without this they would
    // read each other's tiles.
    // -----------------------------------------------------------------

    static LEDGER: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// A painter that hands back a payload for every run it is asked about,
    /// and remembers what it was asked.
    #[derive(Default)]
    struct RecordingPainter {
        asked: std::sync::Mutex<Vec<(u64, usize, crate::tile_mesh::Placement)>>,
    }

    impl crate::tile_mesh::TileMeshPainter for RecordingPainter {
        fn payload(
            &self,
            draw: crate::tile_mesh::GroundDraw<'_>,
        ) -> Option<std::sync::Arc<dyn std::any::Any + Send + Sync>> {
            self.asked
                .lock()
                .expect("the recorder is not poisoned")
                .push((draw.meshes.id(), draw.run, draw.place));
            Some(std::sync::Arc::new(()))
        }
    }

    /// A tile whose ground is a background rect, two fill meshes with a
    /// stroke between them, and a label — the shape of a real styled tile in
    /// miniature.
    fn a_styled_tile() -> Vec<ShapeOrText> {
        let quad = |at: f32, colour: egui::Color32| {
            let mut mesh = egui::epaint::Mesh::default();
            mesh.add_rect_with_uv(
                egui::Rect::from_min_size(egui::pos2(at, at), egui::vec2(64.0, 64.0)),
                egui::Rect::from_min_max(egui::epaint::WHITE_UV, egui::epaint::WHITE_UV),
                colour,
            );
            ShapeOrText::Shape(egui::Shape::Mesh(mesh.into()))
        };
        vec![
            ShapeOrText::Shape(egui::Shape::rect_filled(
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(EXTENT, EXTENT)),
                0.0,
                egui::Color32::from_rgb(0x10, 0x20, 0x30),
            )),
            quad(100.0, egui::Color32::RED),
            ShapeOrText::Shape(egui::Shape::line(
                vec![egui::pos2(0.0, 0.0), egui::pos2(EXTENT, EXTENT)],
                egui::Stroke::new(2.0, egui::Color32::GREEN),
            )),
            quad(300.0, egui::Color32::BLUE),
            ShapeOrText::Text(Text::new(
                egui::pos2(EXTENT / 2.0, EXTENT / 2.0),
                "Monaco".to_owned(),
                12.0,
                egui::Color32::WHITE,
                0.0,
            )),
        ]
    }

    /// Feathering off, which puts every stroke on the CPU path — what the
    /// fill-only cases below were written against and still assert.
    const NO_FEATHERING: f32 = 0.0;

    /// One physical pixel at `pixels_per_point` 1, which is what puts the
    /// fixture's stroke on the GPU path.
    const FEATHERING: f32 = 1.0;

    /// Paint `a_styled_tile` once, with or without a painter, and answer the
    /// shapes it emitted and the counters it moved.
    ///
    /// **One `feathering` for both halves**, because that is the invariant
    /// the ground phase enforces: the value the tile was flattened at and the
    /// value the frame draws at. `flattened_at` differing from it is the
    /// display-change case, and `a_stroke_run_flattened_at_another_ppp_...`
    /// is what exercises it.
    fn one_ground_pass_at(
        painter: Option<&std::sync::Arc<dyn crate::tile_mesh::TileMeshPainter>>,
        flattened_at: f32,
        drawn_at: f32,
    ) -> (
        Vec<egui::epaint::ClippedShape>,
        crate::tile_mesh::ledger::Totals,
    ) {
        let ctx = egui::Context::default();
        let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(256.0, 256.0));
        let shapes = a_styled_tile();
        let meshes = std::sync::Arc::new(crate::tile_mesh::flatten(&shapes, flattened_at));

        crate::tile_mesh::ledger::reset();
        let mut labels = Vec::new();
        let emitted = shapes_of_one_pass(&ctx, canvas, |ui| {
            paint_vector_tile(
                ui.painter(),
                &shapes,
                GroundMeshes {
                    meshes: Some(&meshes),
                    painter,
                    pass_nr: 7,
                    feathering: drawn_at,
                },
                rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                &mut labels,
            );
            paint_labels(
                ui.painter(),
                labels.clone(),
                &mut walkers::GalleyCache::default(),
            );
        });
        (emitted, crate::tile_mesh::ledger::totals())
    }

    /// [`one_ground_pass_at`] with the strokes on the CPU, which is what the
    /// fill-only cases were written against.
    fn one_ground_pass(
        painter: Option<&std::sync::Arc<dyn crate::tile_mesh::TileMeshPainter>>,
    ) -> (
        Vec<egui::epaint::ClippedShape>,
        crate::tile_mesh::ledger::Totals,
    ) {
        one_ground_pass_at(painter, NO_FEATHERING, NO_FEATHERING)
    }

    /// **No ground fill vertex is placed on the CPU while a renderer can draw
    /// it — and the labels still place.**
    ///
    /// The zero is only readable beside the second figure: a tile pass that
    /// never ran would report zero for both. The strokes are held **off** the
    /// GPU path here by a zero feathering, so this case pins the fill half on
    /// its own; `ground_strokes_stop_being_placed_on_the_cpu` is the stroke
    /// half.
    ///
    /// RED on the unmodified baseline: with no painter, every fill vertex is
    /// placed every frame, which is the `without` half below.
    #[test]
    fn ground_fills_stop_being_placed_on_the_cpu_while_labels_still_place() {
        let _guard = LEDGER.lock().expect("the ledger lock is not poisoned");

        let (_, without) = one_ground_pass(None);
        assert!(
            without.mesh_vertices_placed > 0,
            "non-triviality: the CPU path placed no fill vertices either, so \
             the zero below would prove nothing"
        );
        assert_eq!(without.mesh_draws, 0, "no painter, no ground draws");

        let painter: std::sync::Arc<dyn crate::tile_mesh::TileMeshPainter> =
            std::sync::Arc::new(RecordingPainter::default());
        let (_, with) = one_ground_pass(Some(&painter));

        assert_eq!(
            with.mesh_vertices_placed, 0,
            "ground fills are still being placed on the frame thread"
        );
        assert_eq!(
            with.mesh_draws, 2,
            "the tile's two fill runs did not both become ground draws"
        );
        assert!(
            with.label_anchors_placed > 0,
            "the labels stopped placing, so the zero above is a tile pass that \
             did not run rather than a fill that moved to the GPU"
        );
        assert_eq!(
            with.path_points_placed, without.path_points_placed,
            "the strokes changed path; they are the CPU's either way"
        );
    }

    /// **A run is drawn where the style put it.** The callback replaces the
    /// mesh *in place* in the shape sequence, so a fill still draws under the
    /// stroke that was styled over it.
    #[test]
    fn a_fill_run_becomes_a_callback_at_its_own_position() {
        let _guard = LEDGER.lock().expect("the ledger lock is not poisoned");

        let painter: std::sync::Arc<dyn crate::tile_mesh::TileMeshPainter> =
            std::sync::Arc::new(RecordingPainter::default());
        let (with, _) = one_ground_pass(Some(&painter));

        // The tile's own primitives, in the order they were pushed: the
        // background rect, a callback, the stroke, a callback. (The label
        // phase adds a text shape after them.)
        let kinds: Vec<&'static str> = with
            .iter()
            .map(|clipped| match &clipped.shape {
                egui::Shape::Rect(_) => "rect",
                egui::Shape::Callback(_) => "callback",
                egui::Shape::Path(_) => "path",
                egui::Shape::Text(_) => "text",
                other => panic!("unexpected shape {other:?}"),
            })
            .collect();
        assert_eq!(
            kinds,
            vec!["rect", "callback", "path", "callback", "text"],
            "the fills did not draw in the positions the style put them in"
        );
    }

    /// Every callback carries the placement the CPU path would have placed
    /// by, and the run index it was asked for — the two things that decide
    /// which geometry lands where.
    #[test]
    fn each_ground_draw_carries_its_own_run_and_the_cpu_paths_placement() {
        let _guard = LEDGER.lock().expect("the ledger lock is not poisoned");

        let recorder = std::sync::Arc::new(RecordingPainter::default());
        let painter: std::sync::Arc<dyn crate::tile_mesh::TileMeshPainter> = recorder.clone();
        let _ = one_ground_pass(Some(&painter));

        let asked = recorder.asked.lock().expect("not poisoned").clone();
        assert_eq!(asked.len(), 2);
        assert_eq!(asked[0].1, 0, "the first run was not asked for as run 0");
        assert_eq!(asked[1].1, 1);
        assert_eq!(asked[0].0, asked[1].0, "the two runs are one tile's");

        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(256.0, 256.0));
        let expected = crate::tile_mesh::Placement::of(rect);
        for (_, _, place) in &asked {
            assert_eq!(
                *place, expected,
                "a ground draw was placed by something other than \
                 `mvt::placement` over the whole tile"
            );
        }
    }

    /// **No stroke point is placed on the CPU while a renderer can draw it.**
    ///
    /// The stroke half of
    /// `ground_fills_stop_being_placed_on_the_cpu_while_labels_still_place`,
    /// and **RED on the unmodified baseline**, where the flatten had no stroke
    /// arm at all and every stroke point was copied on the frame thread every
    /// frame. The `without` arm is the same tile at the same feathering with
    /// no painter, so the zero is a move and not an absence.
    #[test]
    fn ground_strokes_stop_being_placed_on_the_cpu() {
        let _guard = LEDGER.lock().expect("the ledger lock is not poisoned");

        let (_, without) = one_ground_pass_at(None, FEATHERING, FEATHERING);
        assert!(
            without.path_points_placed > 0,
            "non-triviality: the CPU path placed no stroke points either, so \
             the zero below would prove nothing"
        );
        assert_eq!(without.stroke_draws, 0, "no painter, no stroke draws");

        let painter: std::sync::Arc<dyn crate::tile_mesh::TileMeshPainter> =
            std::sync::Arc::new(RecordingPainter::default());
        let (_, with) = one_ground_pass_at(Some(&painter), FEATHERING, FEATHERING);

        assert_eq!(
            with.path_points_placed, 0,
            "stroke points are still being copied on the frame thread"
        );
        assert_eq!(
            with.stroke_draws, 1,
            "the tile's one stroke run did not become a ground draw"
        );
        assert!(
            with.label_anchors_placed > 0,
            "the labels stopped placing, so the zero above is a tile pass \
             that did not run rather than a stroke that moved to the GPU"
        );
    }

    /// **A tile flattened at another `pixels_per_point` is not drawn.**
    ///
    /// Stroke offsets are baked at a feathering, and feathering is
    /// `feathering_size_in_pixels / pixels_per_point`. Dragging the window to
    /// a different-DPI display would otherwise paint every road at the old
    /// display's width until the tile was evicted. The fills are unaffected —
    /// their vertices carry no screen-space quantity — and that asymmetry is
    /// what this pins.
    #[test]
    fn a_stroke_run_flattened_at_another_ppp_falls_back_to_the_cpu() {
        let _guard = LEDGER.lock().expect("the ledger lock is not poisoned");

        let painter: std::sync::Arc<dyn crate::tile_mesh::TileMeshPainter> =
            std::sync::Arc::new(RecordingPainter::default());

        // The control: flattened and drawn at one value, the strokes go to
        // the GPU.
        let (_, matched) = one_ground_pass_at(Some(&painter), FEATHERING, FEATHERING);
        assert_eq!(matched.path_points_placed, 0);
        assert_eq!(matched.stroke_draws, 1);

        // The display change: the tile is still the one flattened for the old
        // `pixels_per_point`, and the frame is drawing at the new one.
        let (_, mismatched) = one_ground_pass_at(Some(&painter), FEATHERING, FEATHERING / 2.0);
        assert_eq!(
            mismatched.stroke_draws, 0,
            "a tile flattened at another feathering was drawn from the GPU, \
             which paints its roads the width the old display asked for"
        );
        assert!(
            mismatched.path_points_placed > 0,
            "the strokes were neither drawn from the GPU nor placed on the \
             CPU, so they were not drawn at all"
        );
        assert_eq!(
            mismatched.mesh_draws, matched.mesh_draws,
            "the fills stopped drawing from the GPU too; only the strokes \
             carry a screen-space quantity"
        );
    }

    /// **A stroke run draws where the style put it.** The callback replaces
    /// the span *in place*, so a road still draws over the fill it was styled
    /// above and under the one styled above it.
    #[test]
    fn a_stroke_run_becomes_a_callback_at_its_own_position() {
        let _guard = LEDGER.lock().expect("the ledger lock is not poisoned");

        let painter: std::sync::Arc<dyn crate::tile_mesh::TileMeshPainter> =
            std::sync::Arc::new(RecordingPainter::default());
        let (with, _) = one_ground_pass_at(Some(&painter), FEATHERING, FEATHERING);

        let kinds: Vec<&'static str> = with
            .iter()
            .map(|clipped| match &clipped.shape {
                egui::Shape::Rect(_) => "rect",
                egui::Shape::Callback(_) => "callback",
                egui::Shape::Path(_) => "path",
                egui::Shape::Text(_) => "text",
                other => panic!("unexpected shape {other:?}"),
            })
            .collect();
        assert_eq!(
            kinds,
            vec!["rect", "callback", "callback", "callback", "text"],
            "the fixture's fill, stroke, fill order did not survive: the \
             stroke must be the middle callback, not before or after both"
        );
    }

    /// **A painter that refuses a run leaves it to the CPU, once.** The
    /// cursor advances either way, so a refusal is not a shape drawn twice
    /// and not a run silently skipped.
    #[test]
    fn a_refused_run_falls_back_to_cpu_placement_exactly_once() {
        struct Refuses;
        impl crate::tile_mesh::TileMeshPainter for Refuses {
            fn payload(
                &self,
                _draw: crate::tile_mesh::GroundDraw<'_>,
            ) -> Option<std::sync::Arc<dyn std::any::Any + Send + Sync>> {
                None
            }
        }

        let _guard = LEDGER.lock().expect("the ledger lock is not poisoned");
        let painter: std::sync::Arc<dyn crate::tile_mesh::TileMeshPainter> =
            std::sync::Arc::new(Refuses);
        let (shapes, totals) = one_ground_pass(Some(&painter));

        assert_eq!(totals.mesh_draws, 0);
        assert!(
            totals.mesh_vertices_placed > 0,
            "a refused run drew nothing at all"
        );
        assert_eq!(
            shapes
                .iter()
                .filter(|c| matches!(c.shape, egui::Shape::Mesh(_)))
                .count(),
            2,
            "a refused run was not placed exactly once"
        );
    }
}
