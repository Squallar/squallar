use crate::overlay_cache::{OverlayTextureData, draw_overlay_texture, geo_point_in_feature};
use crate::tile_source::{GroundPiece, HttpsTiles};
use squallar_overlays::render::overlay_state::{ClickableItem, OverlayItem};
use squallar_overlays::types::OverlayLabel;
use std::sync::Arc;
use walkers::{Tile, TileId};

// ---------------------------------------------------------------------------
/// Shared context for overlay drawing operations.
///
/// Holds no `&Ui`. The layer walk sets the `Ui`'s opacity around every arm,
/// and a shared borrow kept across the whole loop would make that a borrow
/// error, so the painter to draw with arrives per call instead.
pub(super) struct OverlayDrawContext<'a> {
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
        ui: &egui::Ui,
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
    ///
    /// `painter` is the walk's `ui.painter()`, carrying the layer's opacity.
    pub fn draw_overlay<'i>(
        &self,
        painter: &egui::Painter,
        texture: Option<&OverlayTextureData>,
        labels: &[OverlayLabel],
        items: impl FnOnce() -> Vec<ClickableItem<'i>>,
    ) -> Vec<Arc<dyn OverlayItem>> {
        if let Some(tex) = texture {
            draw_overlay_texture(painter, self.projector, tex, self.screen_rect);
        }

        // Once for the list: the turn this pane is looking at. A label is
        // written in the folded +/-180 frame and the pane's centre is not, so a
        // pane panned past the antimeridian projects every one of them a world
        // off the glass and the `contains` below culls the lot.
        let turn = crate::overlay_cache::pane_turn_lon(self.projector);
        for label in labels {
            let screen_pos = self
                .projector
                .project(walkers::lat_lon(
                    label.lat,
                    squallar_geo::fold_lon_near(label.lon, turn),
                ))
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

    // egui's own frame counter, so the renderer can tell one frame's ground
    // draws from the next without a clock or a frame callback of its own --
    // and the pass the snap decision below is stepped for.
    let pass_nr = ui.ctx().cumulative_pass_nr();

    // The tile-sharpness rung, decided once per source per pass from the
    // levels the last pass left (`tile_source::snap`): a snapped source is
    // asked for the whole zoom below the fractional one and drawn scaled by
    // `Projector::tile_rect`, exactly as an unsnapped source's tiles are drawn
    // scaled between whole zooms. Integers; nothing is measured here.
    let snapped = tiles.snap_for_pass(pass_nr);
    let tile_zoom = crate::tiles::tile_zoom_for(zoom, snapped, zoom_bias, source_max_zoom);

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
                    // The net's columns wrap exactly as the drawn ones do; a
                    // net cell is only ever asked for, never placed, so the
                    // grid column is the whole of what it needs.
                    x: squallar_geo::wrap_tile_x(tx, net_zoom),
                    y: ty,
                    zoom: net_zoom,
                });
            }
        }
    }

    // What this pass wants of the source: the cells the walk below draws plus
    // the net just asked for. The cache's floor follows it, so nothing on the
    // glass is ever evicted for history, and the cells the channel refused
    // last pass are asked for now, ahead of the walk's own head. After the
    // net, before the grid, on purpose — see `HttpsTiles::note_wanted`.
    tiles.note_wanted(
        pass_nr,
        span.tiles(),
        crate::tiles::warm_net_cells(span, tile_zoom),
    );

    // What this pass would have wanted **unsnapped**, for the rung's release
    // gate: the span itself while the two levels agree, else a second span at
    // the level `round` picks -- priced by the source, never asked for. The
    // ancestor net is in both, so the set the source would return to is the
    // set it left, net and all.
    let unsnapped_zoom = crate::tiles::tile_zoom_for(zoom, false, zoom_bias, source_max_zoom);
    if unsnapped_zoom == tile_zoom {
        tiles.note_unsnapped(
            pass_nr,
            span.tiles(),
            crate::tiles::warm_net_cells(span, tile_zoom),
        );
    } else {
        let unsnapped = crate::tiles::tile_span(projector, ui.max_rect(), unsnapped_zoom);
        tiles.note_unsnapped(
            pass_nr,
            unsnapped.tiles(),
            crate::tiles::warm_net_cells(unsnapped, unsnapped_zoom),
        );
    }

    // Once for the layer, not once per tile: a `Context` read lock and a
    // divide, against a span that holds up to 84 cells.
    let feathering = crate::tile_mesh::feathering_of(ui.ctx());

    // The layer's opacity, as the walk set it on this `Ui` before calling
    // here. egui applies it to every CPU-placed shape as they are added; the
    // GPU-drawn runs get it through `GroundMeshes`, because a paint callback
    // is the one shape the painter cannot tint.
    let opacity = ui.painter().opacity();

    // Accumulated across every cell below, so the collision test the caller
    // makes is one test against the whole pane; see [`paint_labels`].
    let mut labels: Vec<walkers::Text> = Vec::new();

    // Falsified per cell below; `tile_zoom` is already clamped to the
    // source's deepest level, so an inexact answer is a tile that has not
    // arrived, never one the source cannot serve.
    let mut exact = true;

    // Two walks over the span, not one. The first asks the source for every
    // cell and paints nothing but the vector tiles' background rectangles --
    // all of them, ahead of every tile's geometry, under the pane's clip
    // rather than each tile's own, so epaint tessellates the lot into ONE
    // primitive where the per-tile clip opened one per tile (45 on a
    // 1920x1080 pane: the ground's largest remaining primitive source once
    // its callbacks were batched). The second walk draws the tiles. Why the
    // hoist changes no pixel is `hoist_background`'s to say.
    let mut answered: Vec<(egui::Rect, GroundPiece, Background)> = Vec::with_capacity(span.tiles());
    // Every hoisted rectangle of this pass, in one mesh rather than one
    // `Shape::Mesh` and one `Painter::add` each. See
    // [`crate::tile_mesh::HoistedBackgrounds`] for why that changes no
    // vertex, and `hoist_background` for why the hoist is legal at all.
    let mut backgrounds = crate::tile_mesh::HoistedBackgrounds::default();
    // One clip for both walks: the grid draws every cell under the pane's own
    // rect, which is what lets a run of them merge at all.
    let pane_clip = ui.painter().clip_rect();
    for ty in span.north..=span.south {
        for tx in span.west..=span.east {
            // **Two different columns, and that is the wrap.** `tx` is the
            // column the viewport is looking at and may be off either end of
            // the grid; the tile that covers it is the one a whole turn away,
            // which is what the source is asked for. Drawing the wrapped column
            // where the wrapped column *is* would put it a world off the glass.
            let tile_id = TileId {
                x: squallar_geo::wrap_tile_x(tx, tile_zoom),
                y: ty,
                zoom: tile_zoom,
            };

            let piece = tiles.ground_at(tile_id);
            exact &= piece.as_ref().is_some_and(|piece| piece.uv == FULL_TILE_UV);
            let Some(piece) = piece else {
                continue;
            };

            // Affine, not geographic. This used to spell the tile's two corners as
            // latitudes and longitudes and hand them to `geo_corner_rect`, which
            // projected them straight back: `tile_to_lat` is `sinh`/`atan` and
            // `Projector::project` is `tan`/`asinh`, exact inverses, four transcendental
            // pairs per tile to arrive at a rect that is a linear function of
            // `(x, y, zoom)`. `tests::the_affine_tile_rect_agrees_with_the_geographic_round_trip`
            // holds the two answers together.
            let rect = projector.tile_rect_at(tx, ty, tile_zoom);

            let background = match &piece.tile {
                Tile::Vector(shapes) => {
                    if hoist_background(
                        &mut backgrounds,
                        shapes,
                        rect,
                        piece.uv,
                        ui.pixels_per_point(),
                        pane_clip,
                    ) {
                        Background::Hoisted
                    } else {
                        Background::Inline
                    }
                }
                // A raster tile has no background rectangle to take.
                Tile::Raster(_) => Background::Inline,
            };
            answered.push((rect, piece, background));
        }
    }

    if let Some(shape) = backgrounds.finish() {
        ui.painter().add(shape);
    }

    // Every raster cell of this pass, in one mesh per consecutive run of one
    // atlas page rather than one `Shape::Mesh` and one `Painter::add` each.
    // See [`crate::tile_mesh::RasterQuads`] for why that changes no vertex and
    // for the two things that end a run.
    let mut quads = crate::tile_mesh::RasterQuads::default();
    let mut quad_count: u64 = 0;
    let mut quad_meshes: u64 = 0;
    for (rect, piece, background) in answered {
        match piece.tile {
            // `window_of` and not `piece.uv`: the tile may be a slot
            // of a shared texture, and the ancestor window is a
            // window of the TILE. It is the identity for a tile with
            // a texture to itself.
            Tile::Raster(ref raster) => {
                quad_count += 1;
                if let Some(run) = quads.push(
                    raster.id(),
                    rect,
                    raster.window_of(piece.uv),
                    egui::Color32::WHITE,
                    pane_clip,
                ) {
                    ui.painter().add(run);
                    quad_meshes += 1;
                }
            }
            Tile::Vector(ref shapes) => {
                // The run this tile interrupts goes in ahead of its geometry,
                // because that is where those quads went in before.
                if let Some(run) = quads.take() {
                    ui.painter().add(run);
                    quad_meshes += 1;
                }
                paint_vector_tile(
                    ui.painter(),
                    shapes,
                    GroundMeshes {
                        meshes: piece.meshes.as_ref(),
                        painter: ground,
                        pass_nr,
                        feathering,
                        opacity,
                    },
                    rect,
                    piece.uv,
                    &mut labels,
                    background,
                );
            }
        }
    }
    if let Some(run) = quads.finish() {
        ui.painter().add(run);
        quad_meshes += 1;
    }
    if quad_count > 0 {
        crate::tile_mesh::ledger::note_raster_quads(quad_count, quad_meshes);
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
    pixels_per_point: f32,
) -> egui::Shape {
    let galley = text.galley_cached(ctx, galleys, pixels_per_point);

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

/// Place one shape on the CPU: count it, transform it, and file it as geometry
/// or as a deferred label.
///
/// A free function because both the planned walk and the un-planned fallback
/// must do exactly this and must not drift — the fallback is what a tile with
/// no plan takes, and a divergence between the two would be a difference
/// nothing draws attention to.
fn place_one(
    shape: &walkers::ShapeOrText,
    placement: egui::emath::TSTransform,
    rect: egui::Rect,
    counted: &mut Counted,
    placed: &mut Vec<egui::Shape>,
    labels: &mut Vec<walkers::Text>,
) {
    match shape {
        walkers::ShapeOrText::Shape(egui::Shape::Mesh(mesh)) => {
            counted.mesh_vertices += mesh.vertices.len() as u64;
        }
        walkers::ShapeOrText::Shape(egui::Shape::Path(path)) => {
            counted.path_points += path.points.len() as u64;
        }
        _ => {}
    }
    // **A label is culled on its anchor before it is placed, not after.**
    // An MVT tile carries every label whose feature reaches it, buffer
    // included, so most of the names in a tile's shape list are anchored in a
    // neighbour and belong to that neighbour's pass. `ShapeOrText::placed`
    // builds a whole new value to answer where one point lands, and the test
    // below then dropped ~88% of them unread: measured on scene A at 1920x1080
    // with real vector tiles, 3,843 text placements per frame of which 3,374
    // were cloned and discarded -- an `Arc<str>` bump and its matching drop,
    // two atomics on a line every tile of the pane shares, plus the value
    // copy, for a name nothing was going to draw.
    //
    // The anchor is one affine on one point and it is the same arithmetic
    // `placed` would apply, so the surviving label is the value that spelling
    // produced, built here instead of copied. The geometry arm is untouched:
    // its shapes have no anchor to cull on and every one of them draws.
    //
    // **A planned tile reaches this with most of them already gone.** An
    // anchor outside the tile's own extent fails this test under every `uv`
    // and every camera, so `tile_mesh::build_plan` emits no step for it at all
    // and this runs only for the labels whose answer the frame actually
    // decides -- the ones inside the extent but outside a stretched ancestor's
    // window. The test stays here unchanged because it is also the un-planned
    // walk's, and because it is what settles that remainder.
    if let walkers::ShapeOrText::Text(text) = shape {
        let position = placement.scaling * text.position + placement.translation;
        if !rect.contains(position) {
            return;
        }
        counted.label_anchors += 1;
        let mut text = text.clone();
        text.position = position;
        labels.push(text);
        return;
    }
    match shape.placed(placement) {
        walkers::ShapeOrText::Shape(shape) => placed.push(shape),
        // `placed` maps `Text` to `Text`, and the arm above took every one of
        // them before this match was reached.
        walkers::ShapeOrText::Text(_) => {}
    }
}

/// Draw a declined **stroke** run from the buffers it was already tessellated
/// into, instead of putting its paths back through epaint.
///
/// **This is the floor strip's cut.** A `GroundOnly` pass has no painter to
/// hand a run to — its primitives are copied into the mirror with every
/// callback swapped for an empty mesh
/// ([`PaneRenderCtx::ground_mesh_painter`](super::pane_render::PaneRenderCtx::ground_mesh_painter)) —
/// so every run of every tile it draws fell through to
/// [`place_run_on_cpu`], and a dense tile is hundreds of `Shape::Path`s that
/// `Context::tessellate` walked again on every frame of a gesture. The
/// geometry those paths tessellate to was already computed once, at tile
/// build, in extent space; all that is per-frame about it is the affine, and
/// [`TileMeshes::placed_stroke_mesh`](crate::tile_mesh::TileMeshes::placed_stroke_mesh)
/// applies it.
///
/// **The feathering test is the same one the renderer makes**, and it is here
/// for the same reason: the offsets are wrong-width roads under any other
/// `pixels_per_point`, and a tile whose flatten has not caught up with a
/// display change goes back to placing its paths until it does. Fills are not
/// taken — a fill run's shape is already a `Shape::Mesh`, so placing it costs
/// one transform either way and there is nothing here to win.
///
/// `false` when nothing was drawn and the caller must place the run's shapes.
fn place_run_as_mesh(
    run: crate::tile_mesh::MeshRun,
    meshes: &crate::tile_mesh::TileMeshes,
    ground: &GroundMeshes<'_>,
    placement: egui::emath::TSTransform,
    counted: &mut Counted,
    placed: &mut Vec<egui::Shape>,
) -> bool {
    if run.kind != crate::tile_mesh::RunKind::Stroke || meshes.feathering() != ground.feathering {
        return false;
    }
    let place = crate::tile_mesh::Placement {
        scale: placement.scaling,
        translation: [placement.translation.x, placement.translation.y],
    };
    let Some(mesh) = meshes.placed_stroke_mesh(run, place) else {
        return false;
    };
    counted.stroke_run_meshes += 1;
    counted.stroke_mesh_vertices += mesh.vertices.len() as u64;
    placed.push(egui::Shape::mesh(mesh));
    true
}

/// Place every shape a declined run would have drawn, each at its own place
/// among the shapes.
///
/// **A `Text` in the span is skipped, and that is the whole of the subtlety.**
/// A label whose anchor falls inside a stroke run's span is not drawn by the
/// run, so `tile_mesh::build_plan` settles it on its own — a `PlanStep::Place`
/// where the anchor can draw, and nothing at all where it cannot
/// (`tile_mesh::anchor_is_off_the_tile`). Placing it here as well would push a
/// label into `labels` twice and lay it out twice, or put back one the plan
/// has already ruled out. Nothing else can be in a span — a fill run is one
/// mesh, and any shape that draws closes a stroke run at flatten time
/// (`tile_mesh::flatten`) — so skipping `Text` leaves exactly the geometry the
/// run was going to draw.
fn place_run_on_cpu(
    run: crate::tile_mesh::MeshRun,
    shapes: &[walkers::ShapeOrText],
    placement: egui::emath::TSTransform,
    rect: egui::Rect,
    counted: &mut Counted,
    placed: &mut Vec<egui::Shape>,
    labels: &mut Vec<walkers::Text>,
) {
    for at in run.shape_index..run.shape_index + run.shape_span {
        let Some(shape) = shapes.get(at as usize) else {
            continue;
        };
        if matches!(shape, walkers::ShapeOrText::Text(_)) {
            continue;
        }
        place_one(shape, placement, rect, counted, placed, labels);
    }
}

/// Whether this install can draw `run` from the GPU on this frame.
///
/// The feathering test is the `pixels_per_point` guard; see
/// [`RunCursor::take_at`], which states why bit equality is the right
/// comparison. It is a property of the **tile**, not of the run, so a
/// mismatch declines every stroke run of the tile and no fill run of it.
fn run_is_drawable(
    run: crate::tile_mesh::MeshRun,
    meshes: &crate::tile_mesh::TileMeshes,
    ground: &GroundMeshes<'_>,
) -> bool {
    if ground.painter.is_none() {
        return false;
    }
    match run.kind {
        // **A tile whose fill bytes an EARLIER store took.** The buffers this
        // run would draw from live in a store that has been dropped — a
        // surface lost and rebuilt — and the store this frame draws through
        // has nothing to make them resident with, so handing it the run would
        // draw nothing at all. Declining puts the tile's own `Shape::Mesh`
        // back on the CPU path, which is where it was before any of this and
        // is what a build with no wgpu renderer gets. See
        // `tile_mesh::TileMeshes::fills_epoch`.
        crate::tile_mesh::RunKind::Fill => meshes.fill_runs_drawable(),
        crate::tile_mesh::RunKind::Stroke => meshes.feathering() == ground.feathering,
    }
}

/// One paint callback for `runs[first..first + count]`, drawn in that order.
fn issue_run_batch(
    meshes: &std::sync::Arc<crate::tile_mesh::TileMeshes>,
    ground: &GroundMeshes<'_>,
    first: usize,
    count: usize,
    placement: egui::emath::TSTransform,
    piece: egui::Rect,
) -> Option<egui::Shape> {
    let painter = ground.painter?;
    let payload = painter.payload(crate::tile_mesh::GroundDraw {
        meshes,
        first_run: first,
        run_count: count,
        place: crate::tile_mesh::Placement {
            scale: placement.scaling,
            translation: [placement.translation.x, placement.translation.y],
        },
        opacity: ground.opacity,
        pass_nr: ground.pass_nr,
    })?;
    Some(egui::Shape::Callback(egui::epaint::PaintCallback {
        // The **piece**, which is what egui turns into a viewport and refuses
        // when it is degenerate. The draw replaces that viewport with the
        // whole screen, because the geometry is placed in screen points by the
        // uniform exactly as the CPU path places it; the clip that makes a
        // stretched ancestor draw only the quarter that belongs to this tile
        // is egui's scissor, taken from the clip rect the painter carries.
        //
        // Every run of the batch shares this rect, because they are all runs
        // of the same tile at the same placement -- which is what lets them
        // share one callback at all.
        rect: piece,
        callback: payload,
    }))
}

/// Issue `runs[first..first + count]` as **as few paint callbacks as their
/// drawability allows**, placing on the CPU every run the renderer declines.
///
/// A callback forces a primitive boundary unconditionally, so one callback per
/// run was one primitive, one draw and one state reset per run. Every run of a
/// tile shares the tile's placement, clip rect and buffers, so a contiguous
/// drawable span of them is one callback and the frame records one boundary
/// for the tile instead of one per run.
///
/// **Order is preserved by construction, not by argument.** The batch draws
/// its runs in index order, which is `shape_index` order
/// (`Flattening::finish` sorts them and `runs_are_in_shape_order` holds it to
/// that), so what covers what inside a batch is what covered what before it.
/// Between batches, order is preserved because a batch is only ever a
/// contiguous span of `PlanStep::Runs`, which `build_plan` opens afresh the
/// moment anything the ground phase *places* comes between two runs; a `Text`
/// does not, because it is deferred to the label phase and pushes nothing
/// into the primitive list. A declined run splits the span at exactly itself,
/// so its own geometry still draws between the runs before and after it.
#[allow(clippy::too_many_arguments)]
fn take_run_batch(
    first: usize,
    count: usize,
    meshes: &std::sync::Arc<crate::tile_mesh::TileMeshes>,
    ground: &GroundMeshes<'_>,
    placement: egui::emath::TSTransform,
    rect: egui::Rect,
    shapes: &[walkers::ShapeOrText],
    counted: &mut Counted,
    placed: &mut Vec<egui::Shape>,
    labels: &mut Vec<walkers::Text>,
) {
    // **A layer at zero opacity places nothing, on either path.** egui's
    // `Painter::add` turns every shape a painter at 0.0 is handed into
    // `Shape::Noop` -- the `Shape::Callback` this batch would return
    // included, which is why the uniform never gets the chance to draw at 0.0
    // either. Placing them anyway minted a `GroundDraw` payload and counted a
    // mesh or stroke draw per run in `tile_mesh::ledger`, so a basemap at 0%
    // reported ground draws that never reached a GPU: an always-on instrument
    // over-reporting, in the one case where the truthful figure is zero.
    //
    // Nothing else leaves through here: the shapes are the Noops, and the
    // label phase is fed by `paint_vector_tile`'s own text walk rather than
    // by this fn, which skips `ShapeOrText::Text` outright.
    if ground.opacity == 0.0 {
        return;
    }
    let end = first.saturating_add(count).min(meshes.runs().len());
    let mut at = first.min(end);
    while at < end {
        let mut reach = at;
        while reach < end && run_is_drawable(meshes.runs()[reach], meshes, ground) {
            reach += 1;
        }
        if reach > at {
            if let Some(callback) = issue_run_batch(meshes, ground, at, reach - at, placement, rect)
            {
                for run in &meshes.runs()[at..reach] {
                    match run.kind {
                        crate::tile_mesh::RunKind::Fill => counted.mesh_draws += 1,
                        crate::tile_mesh::RunKind::Stroke => counted.stroke_draws += 1,
                    }
                }
                placed.push(callback);
            } else {
                // The renderer refused the span outright. Every run in it goes
                // back on the CPU, in order, exactly as a per-run decline does.
                for index in at..reach {
                    let run = meshes.runs()[index];
                    if place_run_as_mesh(run, meshes, ground, placement, counted, placed) {
                        continue;
                    }
                    place_run_on_cpu(run, shapes, placement, rect, counted, placed, labels);
                }
            }
            at = reach;
            continue;
        }
        let run = meshes.runs()[at];
        if !place_run_as_mesh(run, meshes, ground, placement, counted, placed) {
            place_run_on_cpu(run, shapes, placement, rect, counted, placed, labels);
        }
        at += 1;
    }
}

/// The shape a vector tile's background rectangle is, when it is one:
/// `mvt::render` pushes the style's `background` layer before anything it
/// reads a feature for, so it is shape 0 of every styled tile.
const BACKGROUND_SHAPE: usize = 0;

/// Where a vector tile's background rectangle is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Background {
    /// Inside the tile's own walk, placed and clipped like every other shape.
    Inline,
    /// Already drawn by the caller, ahead of every tile's geometry, from
    /// [`hoist_background`]; the tile's walk skips [`BACKGROUND_SHAPE`].
    Hoisted,
}

impl Background {
    /// Whether the caller has already drawn shape `index`.
    fn already_drew(self, index: usize) -> bool {
        self == Self::Hoisted && index == BACKGROUND_SHAPE
    }
}

/// A vector tile's background rectangle, if the ground walk may draw it out of
/// the tile -- see [`crate::tile_mesh::is_hoistable_background`] -- placed
/// against the whole tile exactly as the tile's own walk would place it, then
/// cut to `piece` as the hard mesh that paints what the clipped rectangle
/// painted ([`crate::tile_mesh::background_within`]).
///
/// **Shape [`BACKGROUND_SHAPE`] and nothing else.** Hoisting a shape ahead of
/// every tile's geometry preserves draw order only for a shape nothing in its
/// own tile draws under, and within a tile that is the first shape alone.
/// Across tiles it is what the cut buys: a rectangle cut to its piece paints
/// no pixel of any other piece -- tiles partition the pane
/// (`Projector::tile_rect` is affine in the tile index, so neighbours share an
/// edge bit for bit), and every neighbour's geometry is scissored to *its*
/// piece by the same `round()` this rectangle's edges take -- so it commutes
/// with the geometry of every tile but its own, and its own is still drawn
/// after it. A label is drawn after every tile, as it always was.
///
/// `None` leaves the shape to the tile's clipped walk: a tile whose first
/// shape is not a plain fill, or a background that misses its piece.
fn hoist_background(
    into: &mut crate::tile_mesh::HoistedBackgrounds,
    shapes: &[walkers::ShapeOrText],
    piece: egui::Rect,
    uv: egui::Rect,
    pixels_per_point: f32,
    clip: egui::Rect,
) -> bool {
    let Some(first) = shapes.get(BACKGROUND_SHAPE) else {
        return false;
    };
    let walkers::ShapeOrText::Shape(egui::Shape::Rect(background)) = first else {
        return false;
    };
    if !crate::tile_mesh::is_hoistable_background(background) {
        return false;
    }
    // `placed`, not a hand-written transform: the same arithmetic `place_one`
    // applies to every shape the tile's own walk draws.
    let placement = walkers::mvt::placement(full_rect_of_clipped_tile(piece, uv));
    let walkers::ShapeOrText::Shape(egui::Shape::Rect(placed)) = first.placed(placement) else {
        return false;
    };
    into.push(&placed, piece, pixels_per_point, clip)
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
/// that was asked for. The one shape that does not take that clip is the
/// background rectangle when `background` is [`Background::Hoisted`]: the
/// caller has already drawn it, cut to the piece rather than clipped to it,
/// ahead of every tile -- see [`hoist_background`] -- and this walk skips it.
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
    background: Background,
) {
    let painter = painter.with_clip_rect(rect);

    let full = full_rect_of_clipped_tile(rect, uv);
    let placement = walkers::mvt::placement(full);

    // Accumulated and written once per tile per counter, not once per shape:
    // a dense tile is hundreds of shapes and these are `static` atomics.
    let mut counted = Counted::default();

    let mut runs = GroundMeshes::runs();

    // **The walk is precomputed; this only applies the placement.** Which index
    // opens a run, which span a run covers, which shapes the CPU still has to
    // place and which labels can never be drawn by this tile at all are a pure
    // function of (tile, style epoch) and were settled by
    // `tile_mesh::build_plan` off the frame thread. See `PlanStep`.
    //
    // The guard is the shape count: `TileMeshes` can also come from
    // `flatten_meshes`/`flatten_paths`, which never saw a shape list, and a
    // plan built for a different list would draw the wrong tile. A mismatch
    // falls back to the full walk below rather than trusting it.
    let planned = ground
        .meshes
        .and_then(|meshes| meshes.plan().map(|plan| (meshes, plan)))
        .filter(|(_, plan)| plan.matches(shapes.len()));

    // **What this tile will hand the painter, not how many shapes it holds.**
    // Every run of a planned tile is one callback and every other step of one
    // is a label, which is deferred to `paint_labels` and pushes nothing here;
    // measured on the native rig's scene A, a vector tile placed exactly ONE
    // shape while reserving room for 316. That is 41 buffers a pane-frame,
    // each tens of kilobytes, asked of the allocator and handed straight back.
    // `TilePlan::shape_slots` settles the figure where the plan is settled,
    // off the frame thread. A tile with no plan has nothing to ask, so it
    // keeps the shape count the un-planned walk really can fill.
    let mut placed: Vec<egui::Shape> =
        Vec::with_capacity(planned.map_or(shapes.len(), |(_, plan)| plan.shape_slots()));

    if let Some((meshes, plan)) = planned {
        for step in plan.steps() {
            match *step {
                crate::tile_mesh::PlanStep::Runs { first, count } => {
                    take_run_batch(
                        first as usize,
                        count as usize,
                        meshes,
                        &ground,
                        placement,
                        rect,
                        shapes,
                        &mut counted,
                        &mut placed,
                        labels,
                    );
                }
                crate::tile_mesh::PlanStep::Place(index) => {
                    if background.already_drew(index as usize) {
                        continue;
                    }
                    if let Some(shape) = shapes.get(index as usize) {
                        place_one(shape, placement, rect, &mut counted, &mut placed, labels);
                    }
                }
            }
        }
    } else {
        for (index, shape) in shapes.iter().enumerate() {
            if let Some((callback, kind)) = runs.take_at(index, &ground, placement, rect) {
                match kind {
                    crate::tile_mesh::RunKind::Fill => counted.mesh_draws += 1,
                    crate::tile_mesh::RunKind::Stroke => counted.stroke_draws += 1,
                }
                placed.push(callback);
                continue;
            }
            if runs.covers(index)
                && matches!(shape, walkers::ShapeOrText::Shape(egui::Shape::Path(_)))
            {
                continue;
            }
            if background.already_drew(index) {
                continue;
            }
            place_one(shape, placement, rect, &mut counted, &mut placed, labels);
        }
    }

    counted.ground_shapes = placed.len() as u64;
    counted.ground_shape_slots = placed.capacity() as u64;
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
    stroke_run_meshes: u64,
    stroke_mesh_vertices: u64,
    /// Shapes handed to the painter — the parent the rest are cuts of, set
    /// from the list's length after the walk rather than incremented, so
    /// nothing can count itself into it twice.
    ground_shapes: u64,
    /// Slots reserved to hold them, read off the same vector's capacity for
    /// the same reason.
    ground_shape_slots: u64,
}

impl Counted {
    fn report(self) {
        use crate::tile_mesh::ledger;
        ledger::note_mesh_vertices_placed(self.mesh_vertices);
        ledger::note_path_points_placed(self.path_points);
        ledger::note_label_anchors_placed(self.label_anchors);
        ledger::note_mesh_draws(self.mesh_draws);
        ledger::note_stroke_draws(self.stroke_draws);
        ledger::note_stroke_run_meshes(self.stroke_run_meshes, self.stroke_mesh_vertices);
        ledger::note_ground_shapes(self.ground_shapes, self.ground_shape_slots);
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
    /// The painter's opacity **this frame**, 0-1, from
    /// `ui.painter().opacity()`, handed to the renderer with every run. The
    /// CPU-placed shapes of the same tile get it from egui as they are added;
    /// a `Shape::Callback` is the one shape `Painter::add` cannot tint, so the
    /// runs would draw at full strength under a dimmed layer without it.
    opacity: f32,
}

impl GroundMeshes<'_> {
    /// Nothing to draw from the GPU: the whole tile takes the CPU path.
    #[cfg(test)]
    const CPU_ONLY: GroundMeshes<'static> = GroundMeshes {
        meshes: None,
        painter: None,
        pass_nr: 0,
        feathering: 0.0,
        opacity: 1.0,
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
        ground.painter?;
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
        if !run_is_drawable(run, meshes, ground) {
            return None;
        }
        let shape = issue_run_batch(meshes, ground, self.next - 1, 1, placement, piece)?;
        self.covered_to = index + run.shape_span as usize;
        Some((shape, run.kind))
    }

    /// Whether an already-issued run has drawn the shape at `index`.
    fn covers(&self, index: usize) -> bool {
        index < self.covered_to
    }
}

/// Lay every label this pane collected out against **one** [`OccupiedAreas`],
/// and paint the ones that survived.
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
///
/// **The solve is skipped outright on a pane whose labels have not moved.**
/// [`solve_labels`] reads the list and the fonts and nothing else, so `cache`
/// answers with the geometry it produced last time whenever this pane hands
/// over the same list under the same glyph raster — which, on a map nobody is
/// touching, is every frame after the first. See [`crate::label_cache`] for
/// what is in the key and what is deliberately not.
///
/// **What is kept and re-added is one tessellated mesh, not the shape list.**
/// A shape list is re-walked by `Context::tessellate` on every frame it is
/// added on, and on the native rig's scene A that is 430.2 text shapes and
/// 14,663 glyph vertices per pane-frame, each vertex arriving at the value it
/// already had. Tessellating the solve where it is made costs the same
/// vertices once and a copy thereafter, and it is
/// [`crate::point_painter::tessellate_text_shapes`] that does it, so the two
/// text paths a pane has cannot drift apart.
///
/// **A solve fills the retired solve's buffers**, rather than asking the
/// allocator for another ~850 kB on every frame a pan re-solves on. See
/// [`crate::label_cache::LabelCache::recycle`], which is also where the
/// argument that nothing else can still be holding them lives.
///
/// Placed under the pane's own painter, so the clip and the layer opacity
/// still reach it exactly as they reached the shapes. **Below full opacity the
/// tint is made once per factor rather than once per frame**, because
/// `Painter::add` tints a `Shape::Mesh` through `Arc::make_mut` and a memo
/// still holding the `Arc` turns that into a deep clone of the whole mesh on
/// every frame; [`LabelCache::paint`](crate::label_cache::LabelCache::paint)
/// and [`crate::point_painter::add_kept_mesh`] carry the argument that doing
/// it there is the same picture.
pub(super) fn paint_labels(
    painter: &egui::Painter,
    labels: Vec<walkers::Text>,
    galleys: &mut walkers::GalleyCache,
    cache: &mut crate::label_cache::LabelCache,
    pane_idx: usize,
) {
    if labels.is_empty() {
        return;
    }
    let key = crate::label_cache::LabelKey::new(painter.ctx(), galleys);
    let mesh = match cache.lookup(pane_idx, key, &labels) {
        Some(kept) => kept,
        None => {
            crate::tile_mesh::ledger::note_label_solve();
            let placed = solve_labels(painter.ctx(), &labels, galleys);
            // The solve this one replaces owns a buffer of exactly the right
            // size; see `LabelCache::recycle`.
            let recycled = cache.recycle(pane_idx);
            let mesh =
                crate::point_painter::tessellate_text_shapes_into(painter.ctx(), placed, recycled);
            cache.store(pane_idx, key, labels, mesh.clone());
            mesh
        }
    };
    cache.paint(pane_idx, painter, mesh);
}

/// The label phase itself: lay every name out, and hand back the shapes that
/// survived, in paint order.
///
/// Pure in its inputs — the context's fonts, the list, and the memo it lays
/// out through — which is the property [`paint_labels`]' memo stands on.
pub(super) fn solve_labels(
    ctx: &egui::Context,
    labels: &[walkers::Text],
    galleys: &mut walkers::GalleyCache,
) -> Vec<egui::Shape> {
    // **Once for the solve.** `Context::pixels_per_point` is `Context::write`,
    // and the galley memo used to take it per label — see
    // `walkers::Text::galley_cached`. It cannot differ between two labels of
    // one pass.
    let pixels_per_point = ctx.pixels_per_point();
    let mut occupied = walkers::OccupiedAreas::new();
    // Where each name has already been drawn, so a fragmented river is named
    // once per stretch of screen rather than once per OSM way. See
    // [`MIN_REPEAT_DISTANCE`].
    //
    // Borrowed from `labels`, never owned: the map is built and dropped inside
    // this call, so a name that draws costs a hash of its bytes and no
    // refcount traffic at all.
    let mut placed_names: std::collections::HashMap<&std::sync::Arc<str>, Vec<egui::Pos2>> =
        std::collections::HashMap::new();

    let mut placed: Vec<egui::Shape> = Vec::with_capacity(labels.len());

    for text in labels {
        let position = text.position;

        if placed_names.get(&text.text).is_some_and(|anchors| {
            anchors
                .iter()
                .any(|at| at.distance(position) < MIN_REPEAT_DISTANCE)
        }) {
            continue;
        }

        let shape = lay_out_label(ctx, text, &mut occupied, galleys, pixels_per_point);

        // Only a label that actually drew claims the spot. A name suppressed by
        // the collision test must not stop the same name drawing further along,
        // or one river losing a contest at a crowded confluence would be
        // silenced across the whole viewport.
        if !matches!(shape, egui::Shape::Noop) {
            placed_names.entry(&text.text).or_default().push(position);
            placed.push(shape);
        }
    }

    placed
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
        let _ledger = ledger_guard();
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

    /// **A snapped source is asked for the whole zoom below the fractional one,
    /// an unsnapped one for the rounded zoom, and each for its own ancestor
    /// net** — the apply half of the tile-sharpness rung, driven through the
    /// real decision rather than a test hook: the scene rung arms the source
    /// and on the fifteenth armed pass the walk asks one level up. At zoom
    /// 13.5 `round` gives 14 and `floor` 13, the half of every zoom the rung
    /// exists for; the counts are read off the source's own pass tally, since
    /// a six-slot request channel refuses most of a span and what is *cached*
    /// after one pass says nothing about what was *asked* for. What the
    /// source would draw unsnapped stays tallied while it is snapped — the
    /// release gate's input — and equals what it drew before the snap.
    #[test]
    fn a_snapped_source_is_asked_for_the_whole_zoom_and_its_net_the_same_way() {
        let _ledger = ledger_guard();
        use crate::tile_source::snap::TILE_SNAP_DWELL_PASSES;
        squallar_radar::tls::init();

        let ctx = egui::Context::default();
        let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1920.0, 1080.0));
        let zoom = 13.5;

        let mut memory = walkers::MapMemory::default();
        memory
            .set_zoom(zoom)
            .expect("zoom 13.5 is in walkers' range");
        let projector = walkers::Projector::new(canvas, &memory, walkers::lat_lon(35.33, -97.28));

        let mut tiles = crate::tile_source::HttpsTiles::with_client(
            DeadSource,
            ctx.clone(),
            reqwest::Client::builder()
                .build()
                .expect("the test client should build"),
        );

        let step = crate::tiles::WARM_ANCESTOR_STEPS;
        let rounded = zoom.round() as u8;
        let floored = zoom.floor() as u8;
        assert_eq!(
            rounded,
            floored + 1,
            "fixture: the half step must separate the two rules"
        );
        let sharp = crate::tiles::tile_span(&projector, canvas, rounded);
        let whole = crate::tiles::tile_span(&projector, canvas, floored);
        let sharp_set = (sharp.tiles(), crate::tiles::warm_net_cells(sharp, rounded));
        let whole_set = (whole.tiles(), crate::tiles::warm_net_cells(whole, floored));
        assert!(
            whole_set.0 < sharp_set.0 && whole_set.1 > 0,
            "fixture: the whole zoom must want fewer cells ({whole_set:?} against \
             {sharp_set:?}) and still want a net"
        );

        let draw = |tiles: &mut crate::tile_source::HttpsTiles| {
            ctx.begin_pass(egui::RawInput {
                screen_rect: Some(canvas),
                ..Default::default()
            });
            let ui = egui::Ui::new(
                ctx.clone(),
                egui::Id::new("draw_tile_layer_snap"),
                egui::UiBuilder::new()
                    .layer_id(egui::LayerId::background())
                    .max_rect(canvas),
            );
            draw_tile_layer(&ui, &projector, zoom, tiles, 0, None);
            let _ = ctx.end_pass();
        };

        // Sharp: two passes so the tally has a whole pass to report.
        draw(&mut tiles);
        draw(&mut tiles);
        assert!(
            !tiles.snapped(),
            "a source with no input snapped on its own"
        );
        assert_eq!(
            tiles.wanted_for_test(),
            sharp_set,
            "unsnapped, the walk asked at the rounded level"
        );
        assert_eq!(
            tiles.unsnapped_for_test(),
            sharp_set,
            "unsnapped, the counterfactual is the walk itself"
        );

        // The rung arms; the dwell holds the level for fourteen more passes.
        tiles.set_whole_zoom_rung(true);
        for _ in 0..TILE_SNAP_DWELL_PASSES - 1 {
            draw(&mut tiles);
        }
        assert!(!tiles.snapped(), "snapped before the dwell had counted");
        assert_eq!(
            tiles.wanted_for_test(),
            sharp_set,
            "the level moved under the dwell"
        );

        // The fifteenth armed pass snaps and asks at the whole zoom, with the
        // net one more step down; the pass after reports it as a whole pass.
        draw(&mut tiles);
        assert!(
            tiles.snapped(),
            "the dwell passed and the source did not snap"
        );
        draw(&mut tiles);
        assert_eq!(
            tiles.wanted_for_test(),
            whole_set,
            "snapped, the walk did not ask at the whole zoom"
        );
        assert_eq!(
            tiles.unsnapped_for_test(),
            sharp_set,
            "snapped, the set the source would return to is not the set it left"
        );
        // And the net at the snapped level was really asked for: put as a
        // marker, or refused by a channel the earlier passes' asks still fill
        // and queued to go first next pass -- either is the ask; whether the
        // IO task has drained a dead source's slots by now is not the pass's.
        let net_zoom = floored - step;
        for ty in (whole.north >> step)..=(whole.south >> step) {
            for tx in (whole.west >> step)..=(whole.east >> step) {
                let net = TileId {
                    x: squallar_geo::wrap_tile_x(tx, net_zoom),
                    y: ty,
                    zoom: net_zoom,
                };
                assert!(
                    tiles.asked_or_queued_for_test(net),
                    "the snapped pass did not ask for its ancestor net at {net:?}: the net was traded"
                );
            }
        }
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
        let _ledger = ledger_guard();
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
                    x: squallar_geo::wrap_tile_x(tx, net_zoom),
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

    /// [`shapes_of_one_pass`] under a `Ui` whose opacity was set before the
    /// draw -- what the layer walk does around a layer's arm. A sibling
    /// rather than a parameter so the helper every test above uses stays the
    /// untouched harness the full-opacity arm is compared against.
    fn shapes_of_one_pass_at(
        ctx: &egui::Context,
        canvas: egui::Rect,
        opacity: f32,
        draw: impl FnOnce(&egui::Ui),
    ) -> Vec<egui::epaint::ClippedShape> {
        ctx.begin_pass(egui::RawInput {
            screen_rect: Some(canvas),
            ..Default::default()
        });
        let mut ui = egui::Ui::new(
            ctx.clone(),
            egui::Id::new("vector_seam"),
            egui::UiBuilder::new()
                .layer_id(egui::LayerId::background())
                .max_rect(canvas),
        );
        ui.set_opacity(opacity);
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
        let _ledger = ledger_guard();
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
            x: squallar_geo::wrap_tile_x(span.west, tile_zoom),
            y: span.north,
            zoom: tile_zoom,
        };
        tiles.put_for_test(tile_id, extent_spanning_tile());
        let rect = projector.tile_rect(tile_id);

        let mut placed = Vec::new();
        let shapes = shapes_of_one_pass(&ctx, canvas, |ui| {
            let labels = draw_tile_layer(ui, &projector, zoom, &mut tiles, 0, None).labels;
            // The pane's `CityLabels` arm, which is where the deferred labels
            // are painted; without it this pass draws ground and no names.
            placed = solve_and_paint(ui, labels);
        });

        // NON-VACUITY, and the specific thing that would have passed before:
        // the old arm emitted exactly one `rect_filled` in MAGENTA. So "some
        // shape was emitted" is not the assertion -- the fill has to be the
        // colour the tile carried, at the tile's own rect. It arrives as the
        // hoisted background (`tile_mesh::background_within`): a hard mesh on
        // the tile's rect rounded to pixels.
        let fill = egui::Color32::from_rgb(0x10, 0x20, 0x30);
        let expected = {
            use egui::emath::GuiRounding as _;
            rect.round_to_pixels(1.0)
        };
        let painted_fill = shapes.iter().any(|clipped| {
            matches!(
                &clipped.shape,
                egui::Shape::Mesh(m)
                    if m.vertices.len() == 4
                        && m.vertices.iter().all(|v| v.color == fill)
                        && (m.calc_bounds().min - expected.min).length() < 0.01
                        && (m.calc_bounds().max - expected.max).length() < 0.01
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
        let label = placed
            .iter()
            .find_map(|s| match s {
                egui::Shape::Text(t) => Some(t),
                _ => None,
            })
            .expect("the tile's label did not reach the label phase");
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
        let _ledger = ledger_guard();
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
        let _ledger = ledger_guard();
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
                Background::Inline,
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

    /// Solve `labels` for this pane and paint them, and hand back the shapes
    /// the phase produced.
    ///
    /// **The shapes are what the assertions below read, and the mesh is what
    /// the glass gets.** `paint_labels` adds one `Shape::Mesh`, so a pass's
    /// own shape list no longer carries a `TextShape` to inspect; every
    /// placement, collision, wrapping and repeat rule is a property of
    /// `solve_labels`' output, which is what this returns. The one thing that
    /// says the painted mesh *is* those shapes is
    /// `the_painted_mesh_is_what_the_solved_shapes_tessellate_to`.
    ///
    /// Both are run, and the second run is the shipped `paint_labels`, so
    /// every test below still drives the real path end to end.
    fn solve_and_paint(ui: &egui::Ui, labels: Vec<Text>) -> Vec<egui::Shape> {
        let solved = solve_labels(ui.ctx(), &labels, &mut walkers::GalleyCache::default());
        paint_labels(
            ui.painter(),
            labels,
            &mut walkers::GalleyCache::default(),
            &mut crate::label_cache::LabelCache::default(),
            0,
        );
        solved
    }

    /// Draw `tiles` as `(shapes, rect)` pieces through the ground phase, then
    /// run the one label phase over everything they deferred -- which is what
    /// `draw_tile_layer` does across a span, in miniature.
    fn ground_then_labels(
        ui: &egui::Ui,
        tiles: &[(Vec<ShapeOrText>, egui::Rect)],
    ) -> Vec<egui::Shape> {
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
                Background::Inline,
            );
        }
        solve_and_paint(ui, labels)
    }

    fn text_count(shapes: &[egui::Shape]) -> usize {
        shapes
            .iter()
            .filter(|s| matches!(s, egui::Shape::Text(_)))
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

    /// **A label is culled on its own anchor, and the cull is what does it.**
    ///
    /// An MVT tile carries every name whose feature reaches it, buffer
    /// included, so most of the labels in a tile's shape list are anchored in
    /// a neighbour and belong to that neighbour's pass. `place_one` drops
    /// those and keeps the rest placed by the same affine.
    ///
    /// **This is a unit test of `place_one` and not of a rendered pass on
    /// purpose.** The two tile-seam tests below draw the duplicate case end to
    /// end and stay green with this cull disabled outright — `solve_labels`'
    /// own repeat-distance rule catches the duplicate name downstream — so
    /// they pin the pixel and cannot pin this. Reached directly, the branch
    /// has nothing standing in front of it.
    #[test]
    fn a_label_anchored_outside_its_tile_is_dropped_before_it_is_placed() {
        // The tile drawn 256 points wide at (100, 50), which is the scale
        // `walkers::mvt::placement` builds for a tile of `EXTENT` units.
        let side = 256.0;
        let placement = egui::emath::TSTransform::new(egui::vec2(100.0, 50.0), side / EXTENT);
        let rect = egui::Rect::from_min_size(egui::pos2(100.0, 50.0), egui::vec2(side, side));

        // Inside its own tile; and west of its own origin, which is what a
        // neighbour's place looks like in this tile's buffer.
        let inside = label_at(EXTENT / 2.0, "Topeka");
        let outside = label_at(-EXTENT * 0.01, "Lawrence");

        let mut counted = Counted::default();
        let mut placed: Vec<egui::Shape> = Vec::new();
        let mut labels: Vec<Text> = Vec::new();
        for shape in [&inside, &outside] {
            place_one(
                shape,
                placement,
                rect,
                &mut counted,
                &mut placed,
                &mut labels,
            );
        }

        assert_eq!(
            labels.iter().map(|t| &*t.text).collect::<Vec<_>>(),
            ["Topeka"],
            "the label anchored in a neighbouring tile was kept by this tile's pass"
        );
        assert_eq!(
            counted.label_anchors, 1,
            "the anchor count follows the list"
        );
        assert!(placed.is_empty(), "a label is not geometry");

        // **And the survivor is the value the old spelling produced.** The
        // cull moved in front of `ShapeOrText::placed`; what it hands on must
        // still be that call's answer, or the reorder moved a pixel.
        let walkers::ShapeOrText::Text(expected) = inside.placed(placement) else {
            panic!("`placed` maps a `Text` to a `Text`");
        };
        assert_eq!(labels[0].position, expected.position, "the anchor moved");
        assert_eq!(labels[0].font_size, expected.font_size);
        assert_eq!(labels[0].text_color, expected.text_color);
        assert_eq!(labels[0].angle, expected.angle);
    }

    /// **Two labels claiming the same screen: one wins.** The collision is what
    /// `OccupiedAreas` is for, and a seam that dropped it would draw legible
    /// text on top of legible text and still look plausible in a screenshot.
    #[test]
    fn overlapping_labels_collide_and_only_one_is_drawn() {
        let _ledger = ledger_guard();
        let ctx = egui::Context::default();
        let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(256.0, 256.0));

        // Same point, twice.
        let mut shapes = Vec::new();
        let _ = shapes_of_one_pass(&ctx, canvas, |ui| {
            shapes = ground_then_labels(
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
        let mut shapes = Vec::new();
        let _ = shapes_of_one_pass(&ctx, canvas, |ui| {
            shapes = ground_then_labels(
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
        let _ledger = ledger_guard();
        let ctx = egui::Context::default();
        let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));

        let west = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(256.0, 256.0));
        let east = egui::Rect::from_min_size(egui::pos2(256.0, 0.0), egui::vec2(256.0, 256.0));

        // The place sits just inside the WEST tile's eastern edge, so the west
        // tile owns the anchor and the east tile carries it only in its buffer
        // -- a negative extent coordinate, west of its own origin, which lands
        // on the same screen point.
        let mut shapes = Vec::new();
        let _ = shapes_of_one_pass(&ctx, canvas, |ui| {
            shapes = ground_then_labels(
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
        let _ledger = ledger_guard();
        let ctx = egui::Context::default();
        let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));

        let west = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(256.0, 256.0));
        let east = egui::Rect::from_min_size(egui::pos2(256.0, 0.0), egui::vec2(256.0, 256.0));

        let mut shapes = Vec::new();
        let _ = shapes_of_one_pass(&ctx, canvas, |ui| {
            shapes = ground_then_labels(
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
        let _ledger = ledger_guard();
        let ctx = egui::Context::default();
        let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        let tile = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(256.0, 256.0));

        let mut solved = Vec::new();
        let shapes = shapes_of_one_pass(&ctx, canvas, |ui| {
            solved = ground_then_labels(ui, &[(vec![label_at(EXTENT * 0.99, "Topeka")], tile)]);
        });
        assert_eq!(text_count(&solved), 1, "the label was not placed at all");

        // The label's glyphs reach the painter as the phase's one mesh, and it
        // is the CLIP it arrived under that this test is about.
        let label = shapes
            .iter()
            .find(|c| matches!(&c.shape, egui::Shape::Mesh(m) if is_glyph_mesh(m)))
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
        let _ledger = ledger_guard();
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

    /// **The label solve settles the galley memo on the CONTEXT's
    /// `pixels_per_point`, and that is a live gate because the solve now reads
    /// it itself.**
    ///
    /// `walkers::Text::galley_cached` used to take `Context::pixels_per_point`
    /// per label — `Context::write`, an exclusive lock on the whole context and
    /// a probe of its viewport table, 459 times a solve on the native rig's
    /// scene A — for a number that is fixed for the pass. It is now the
    /// caller's to read once, which puts the *choice of value* on the caller
    /// and creates a hazard nothing had before: a caller that passes a stale or
    /// invented figure leaves the memo settled on a scale the display is not at
    /// and serves galleys rasterized for another one, which is glyph geometry
    /// at the wrong size on a real display change.
    ///
    /// So this drives the shipped `solve_labels` across a `pixels_per_point`
    /// change and requires the memo to have been dropped and re-laid at the
    /// new scale — and requires the glyphs to actually move, so a solve that
    /// answered from the stale table cannot pass by drawing the same thing.
    /// Pinning the value `solve_labels` reads to a constant makes it red.
    #[test]
    fn the_label_solve_settles_the_galley_memo_on_the_contexts_own_scale() {
        let _ledger = ledger_guard();
        let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, SCREEN);
        let ctx = egui::Context::default();
        let mut galleys = walkers::GalleyCache::default();
        let names = vec![label("Washita River", egui::pos2(200.0, 300.0))];

        // Through the viewport, which is how a real display change arrives.
        let pass_at = |ppp: Option<f32>| {
            let mut input = egui::RawInput {
                screen_rect: Some(canvas),
                ..Default::default()
            };
            if let Some(ppp) = ppp {
                input
                    .viewports
                    .get_mut(&input.viewport_id)
                    .expect("the root viewport is in every RawInput")
                    .native_pixels_per_point = Some(ppp);
            }
            input
        };

        let solve = |ctx: &egui::Context, galleys: &mut walkers::GalleyCache| {
            let placed = solve_labels(ctx, &names, galleys);
            assert_eq!(
                placed.len(),
                1,
                "fixture: the one name must place, or this compares nothing"
            );
            match &placed[0] {
                egui::Shape::Text(text) => text.galley.size(),
                other => panic!("the label placed as {other:?} rather than text"),
            }
        };

        ctx.begin_pass(pass_at(None));
        let one = solve(&ctx, &mut galleys);
        let laid_out_at_one = galleys.layouts();
        let _ = ctx.end_pass();
        assert_eq!(
            laid_out_at_one, 1,
            "fixture: the first solve lays the name out"
        );

        ctx.begin_pass(pass_at(Some(2.0)));
        assert_eq!(
            ctx.pixels_per_point(),
            2.0,
            "fixture: the display change did not reach the context"
        );
        let two = solve(&ctx, &mut galleys);
        let laid_out_at_two = galleys.layouts();
        let _ = ctx.end_pass();

        assert_eq!(
            laid_out_at_two, 2,
            "the memo answered the moved display from the table it built at the \
             old scale: {laid_out_at_two} layouts across the two passes, not 2"
        );
        assert_ne!(
            one, two,
            "fixture: the galley measures the same at both scales, so a stale \
             answer would be indistinguishable from a fresh one here"
        );
    }

    /// **A kept galley memo paints what a fresh one paints, frame after
    /// frame.**
    ///
    /// The unit tests in `walkers::text` hold the galley identity; this holds
    /// the one that matters on the glass, through the real `paint_labels`:
    /// three passes over the same names, one cache carried across all of them,
    /// against three passes each with its own. Every glyph vertex the pass put
    /// on the painter must match in position, atlas UV and colour, in order —
    /// a memo that answered a stale galley, or that changed placement order,
    /// shows up here.
    ///
    /// The labels move between passes, because that is the case the memo is
    /// built for: panning re-uses every entry, so a kept cache must still lay
    /// the frame out from scratch positionally while re-using the glyphs.
    #[test]
    fn a_kept_galley_cache_paints_what_a_fresh_one_paints() {
        let _ledger = ledger_guard();
        let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, SCREEN);
        let names = ["Washita River", "Oklahoma City", "Lake Thunderbird"];
        let offsets = [0.0_f32, 17.0, -23.0];

        let describe = painted_glyph_vertices;

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
                paint_labels(
                    ui.painter(),
                    labels,
                    &mut kept,
                    &mut crate::label_cache::LabelCache::default(),
                    0,
                );
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
                paint_labels(
                    ui.painter(),
                    labels,
                    &mut walkers::GalleyCache::default(),
                    &mut crate::label_cache::LabelCache::default(),
                    0,
                );
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

    /// **A kept label solve paints what a fresh solve paints, frame after
    /// frame.**
    ///
    /// The sibling above holds the *galley* memo, which is a memo of glyphs
    /// and is re-consulted every frame. This holds the memo of the whole
    /// phase: three passes over an UNMOVED label list against one
    /// [`crate::label_cache::LabelCache`], and three passes each with its own,
    /// which is the arm that solves every time. Every glyph vertex the pass
    /// put on the painter must match in position, atlas UV and colour.
    ///
    /// The labels do not move between passes here, and that is the difference
    /// from the sibling: this is the case the phase memo exists for, and the
    /// one a map nobody is touching is in on every frame.
    #[test]
    fn a_kept_label_solve_paints_what_a_fresh_solve_paints() {
        let _ledger = ledger_guard();
        let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, SCREEN);
        // Crowded on purpose: the third and fifth sit on top of the first and
        // fourth, so two of the five lose the collision contest and the
        // comparison covers the refusals as well as the placements.
        let names: [(&str, egui::Pos2); 5] = [
            ("Washita River", egui::pos2(200.0, 100.0)),
            ("Oklahoma City", egui::pos2(560.0, 100.0)),
            ("Lake Thunderbird", egui::pos2(204.0, 104.0)),
            ("Norman", egui::pos2(200.0, 400.0)),
            ("Moore", egui::pos2(203.0, 403.0)),
        ];
        let labels = || -> Vec<Text> { names.iter().map(|(n, at)| label(n, *at)).collect() };

        let describe = painted_glyph_vertices;

        const PASSES: u64 = 3;

        let ctx_kept = egui::Context::default();
        let mut galleys = walkers::GalleyCache::default();
        let mut cache = crate::label_cache::LabelCache::default();
        let mut kept_frames = Vec::new();
        for _ in 0..PASSES {
            let shapes = shapes_of_one_pass(&ctx_kept, canvas, |ui| {
                paint_labels(ui.painter(), labels(), &mut galleys, &mut cache, 0);
            });
            kept_frames.push(describe(&shapes));
        }

        let ctx_fresh = egui::Context::default();
        let mut fresh_frames = Vec::new();
        for _ in 0..PASSES {
            let shapes = shapes_of_one_pass(&ctx_fresh, canvas, |ui| {
                paint_labels(
                    ui.painter(),
                    labels(),
                    &mut walkers::GalleyCache::default(),
                    &mut crate::label_cache::LabelCache::default(),
                    0,
                );
            });
            fresh_frames.push(describe(&shapes));
        }

        assert_eq!(kept_frames, fresh_frames);
        assert!(
            !kept_frames[0].is_empty(),
            "the fixture drew no labels, so the comparison proves nothing",
        );
        // The fixture property, read where it is countable: the frames above
        // are glyph vertices, so "fewer than five" is a statement about the
        // phase's shapes and is made against them.
        let mut placed = Vec::new();
        let _ = shapes_of_one_pass(&egui::Context::default(), canvas, |ui| {
            placed = solve_labels(ui.ctx(), &labels(), &mut walkers::GalleyCache::default());
        });
        assert!(
            text_shapes(&placed).len() < names.len(),
            "the fixture placed every label, so the comparison never covers a refusal",
        );

        // **The cut itself.** One solve for the three passes, and the two
        // later ones did not reach the galley memo at all: the whole phase --
        // galley probe, repeat-name table, oriented rectangles and the
        // bucketed collision search -- did not run.
        assert_eq!(cache.solves(), 1, "the label set was solved more than once");
        assert_eq!(cache.hits(), PASSES - 1);
        let touched = galleys.layouts() + galleys.hits();
        assert_eq!(
            touched,
            names.len() as u64,
            "a kept frame reached the galley memo, so the phase ran again",
        );
    }

    /// **The mesh `paint_labels` paints is the solved shapes, vertex for
    /// vertex.**
    ///
    /// Every placement, collision, wrapping and repeat assertion in this
    /// module reads [`solve_labels`]' shapes. This is the one thing that says
    /// what reaches the glass is those shapes: one arm adds them to a painter
    /// and lets `Context::tessellate` walk them, which is what this path did
    /// before it kept a mesh; the other is the shipped `paint_labels`.
    /// Positions, atlas UVs, colours and indices must match in order.
    ///
    /// **The fixture has to carry three properties or it cannot reach the
    /// thing it measures.** The atlas is warmed first, because a pass that
    /// grows it changes every normalised UV and the comparison would be a
    /// comparison of atlases rather than of paths. The names are placed well
    /// inside the canvas, because the direct arm culls a text row against the
    /// painter's clip and [`crate::point_painter::tessellate_text_shapes`]
    /// tessellates under `Rect::EVERYTHING` — the two agree exactly on a row
    /// the clip keeps, and only there. And more than one name is placed, at
    /// more than one row, so the comparison covers ordering and not just a
    /// single galley.
    #[test]
    fn the_painted_mesh_is_what_the_solved_shapes_tessellate_to() {
        let _ledger = ledger_guard();
        let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, SCREEN);
        let ctx = egui::Context::default();
        let names = ["Washita River", "Oklahoma City", "Lake Thunderbird", "Enid"];
        let labels = || -> Vec<Text> {
            names
                .iter()
                .enumerate()
                .map(|(i, n)| label(n, egui::pos2(300.0, 120.0 + 90.0 * i as f32)))
                .collect()
        };

        // Warm the atlas with every glyph both arms use, so neither grows it
        // under the other.
        let _ = shapes_of_one_pass(&ctx, canvas, |ui| {
            let _ = solve_labels(ui.ctx(), &labels(), &mut walkers::GalleyCache::default());
        });

        // The direct arm: the phase's shapes, added and tessellated by egui.
        let mut solved = Vec::new();
        let direct_pass = shapes_of_one_pass(&ctx, canvas, |ui| {
            solved = solve_labels(ui.ctx(), &labels(), &mut walkers::GalleyCache::default());
            ui.painter().extend(solved.clone());
        });
        assert_eq!(
            text_shapes(&solved).len(),
            names.len(),
            "fixture: not every name placed, so the comparison is narrower \
             than it reads"
        );
        let mut direct = egui::Mesh::default();
        for prim in ctx.tessellate(direct_pass, ctx.pixels_per_point()) {
            if let egui::epaint::Primitive::Mesh(m) = prim.primitive {
                direct.append(m);
            }
        }

        // The kept arm: `paint_labels`, which tessellates the same shapes once.
        let painted = shapes_of_one_pass(&ctx, canvas, |ui| {
            paint_labels(
                ui.painter(),
                labels(),
                &mut walkers::GalleyCache::default(),
                &mut crate::label_cache::LabelCache::default(),
                0,
            );
        });
        let mut kept = egui::Mesh::default();
        for prim in ctx.tessellate(painted, ctx.pixels_per_point()) {
            if let egui::epaint::Primitive::Mesh(m) = prim.primitive {
                kept.append(m);
            }
        }

        assert!(!direct.is_empty(), "the direct arm tessellated nothing");
        let describe = |m: &egui::Mesh| -> Vec<(egui::Pos2, egui::Pos2, egui::Color32)> {
            m.vertices.iter().map(|v| (v.pos, v.uv, v.color)).collect()
        };
        assert_eq!(describe(&kept), describe(&direct));
        assert_eq!(kept.indices, direct.indices);
    }

    /// **A repack under a map nobody is touching does not re-paint the old
    /// atlas.**
    ///
    /// This is the defect `8f2e1bb0f` fixed for the galley memo, asked of the
    /// memo above it, and it bites harder here: a kept mesh carries the atlas
    /// coordinates twice over — the glyph's texel, and the atlas size it was
    /// divided by — so it is wrong after a *growth* as well as after a repack,
    /// where a kept galley would still have been right. Nothing gated
    /// [`crate::label_cache::LabelKey`]'s raster terms before this: setting
    /// `atlas_generation` to a constant left the whole board green.
    ///
    /// **The property the fixture needs** is the one that file's header names:
    /// enough distinct glyphs to drive egui past its 80 % repack mark, a label
    /// solved before that and asked for again after, and — the part that makes
    /// it a *memo* test — a label list that never moves, so the phase memo
    /// answers from its entry on every frame in between and the raster terms
    /// are the only thing that can invalidate it.
    ///
    /// The reference is solved and tessellated in the SAME pass, through a
    /// cache holding nothing, so the two differ only where the memo served
    /// geometry the current atlas no longer matches.
    #[test]
    fn a_repack_under_a_still_map_does_not_paint_the_old_atlas() {
        let _ledger = ledger_guard();
        let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, SCREEN);
        let ctx = egui::Context::default();
        let mut galleys = walkers::GalleyCache::default();
        let mut cache = crate::label_cache::LabelCache::default();
        let names = || vec![label("Washita River", egui::pos2(200.0, 300.0))];

        // Fresh atlas entries, the way panning onto new tiles makes them.
        let mut size = 6.0f32;
        let burn = |ctx: &egui::Context, size: &mut f32| {
            for _ in 0..4 {
                *size += 0.25;
                let mut job = egui::text::LayoutJob::default();
                job.append(
                    "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789",
                    0.0,
                    egui::TextFormat {
                        font_id: egui::FontId::proportional(*size),
                        color: egui::Color32::WHITE,
                        ..Default::default()
                    },
                );
                ctx.fonts_mut(|f| f.layout_job(job));
            }
        };
        let stamp =
            |ctx: &egui::Context| ctx.fonts(|f| (f.font_image_size(), f.font_atlas_fill_ratio()));

        // One pass so the context has fonts to read.
        let _ = shapes_of_one_pass(&ctx, canvas, |_| {});

        let mut repacked = false;
        let mut grew = false;
        for _ in 0..4000 {
            let before = stamp(&ctx);
            let mut reference = None;
            let painted = shapes_of_one_pass(&ctx, canvas, |ui| {
                // The head of the pass, where the shell checks the raster.
                galleys.begin_frame(ui.ctx());
                paint_labels(ui.painter(), names(), &mut galleys, &mut cache, 0);
                reference = crate::point_painter::tessellate_text_shapes(
                    ui.ctx(),
                    solve_labels(ui.ctx(), &names(), &mut walkers::GalleyCache::default()),
                );
                burn(ui.ctx(), &mut size);
            });

            let reference = reference.expect("the reference solve drew the label");
            let expected: Vec<_> = reference
                .vertices
                .iter()
                .map(|v| (v.pos, v.uv, v.color))
                .collect();
            assert_eq!(
                painted_glyph_vertices(&painted),
                expected,
                "the pane painted geometry the current atlas no longer matches"
            );

            let now = stamp(&ctx);
            repacked |= now.1 < before.1;
            grew |= now.0 != before.0;
            if repacked && grew {
                break;
            }
        }

        assert!(repacked, "fixture: never drove a repack");
        assert!(grew, "fixture: never grew the atlas");
        assert!(
            cache.solves() > 1,
            "fixture: an unmoved label list was re-solved by something other \
             than the raster, so the comparison is not about the raster"
        );
        assert!(
            cache.hits() > 0,
            "fixture: the memo never answered, so nothing was held across \
             anything"
        );
    }

    /// **A solve fills the buffers of the solve it replaces.**
    ///
    /// A pane being panned re-solves on nearly half its frames, and a solve
    /// that starts from `Mesh::default()` takes ~850 kB from the allocator and
    /// gives it back a frame later, every one of those frames. This holds both
    /// halves of the reuse: that `Arc::try_unwrap` really does find the kept
    /// mesh unique by the next pass — which is the fact the whole thing stands
    /// on, and the one that would silently stop being true if the painter's
    /// clone outlived its frame — and that what comes back carries the
    /// capacity rather than an emptied husk. `Mesh::clear` produces the husk;
    /// that is why `recycle` does not call it.
    #[test]
    fn a_moved_label_set_refills_the_retired_mesh() {
        let _ledger = ledger_guard();
        let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, SCREEN);
        let ctx = egui::Context::default();
        let mut galleys = walkers::GalleyCache::default();
        let mut cache = crate::label_cache::LabelCache::default();
        let at = |dx: f32| vec![label("Washita River", egui::pos2(200.0 + dx, 300.0))];

        for dx in [0.0_f32, 11.0, 22.0] {
            let _ = shapes_of_one_pass(&ctx, canvas, |ui| {
                paint_labels(ui.painter(), at(dx), &mut galleys, &mut cache, 0);
            });
        }

        assert_eq!(cache.solves(), 3, "fixture: the list must move every pass");
        assert_eq!(
            cache.recycled(),
            2,
            "a solve asked the allocator for a mesh the retired solve already \
             owned; `Arc::try_unwrap` is finding the kept mesh still shared"
        );

        let retired = cache.recycle(0);
        assert!(retired.is_empty(), "a recycled mesh must come back emptied");
        assert!(
            retired.vertices.capacity() > 0 && retired.indices.capacity() > 0,
            "the retired mesh came back with no buffers ({} verts, {} indices \
             of capacity), so every solve still allocates",
            retired.vertices.capacity(),
            retired.indices.capacity(),
        );
    }

    /// The memo is a memo, not a latch: a label set that moved is solved
    /// again, and a set that came back is solved again after it.
    #[test]
    fn a_moved_label_set_is_solved_again() {
        let _ledger = ledger_guard();
        let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, SCREEN);
        let ctx = egui::Context::default();
        let mut galleys = walkers::GalleyCache::default();
        let mut cache = crate::label_cache::LabelCache::default();

        let at = |dx: f32| vec![label("Washita River", egui::pos2(200.0 + dx, 300.0))];

        for dx in [0.0_f32, 0.0, 11.0, 11.0, 0.0] {
            let _ = shapes_of_one_pass(&ctx, canvas, |ui| {
                paint_labels(ui.painter(), at(dx), &mut galleys, &mut cache, 0);
            });
        }

        // Three distinct lists in that run of five: 0, 11, and 0 again.
        assert_eq!(cache.solves(), 3, "a moved label set was answered stale");
        assert_eq!(cache.hits(), 2);
    }

    /// Two panes handing over the same names keep two solves, because the
    /// entry is per pane. A single-entry memo would thrash between them and
    /// solve on every frame; a memo that ignored the pane would paint one
    /// pane's labels into the other.
    #[test]
    fn each_pane_keeps_its_own_solve() {
        let _ledger = ledger_guard();
        let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, SCREEN);
        let ctx = egui::Context::default();
        let mut galleys = walkers::GalleyCache::default();
        let mut cache = crate::label_cache::LabelCache::default();

        let labels = || vec![label("Washita River", egui::pos2(200.0, 300.0))];
        for _ in 0..3 {
            for pane in 0..2 {
                let _ = shapes_of_one_pass(&ctx, canvas, |ui| {
                    paint_labels(ui.painter(), labels(), &mut galleys, &mut cache, pane);
                });
            }
        }
        assert_eq!(cache.solves(), 2, "one solve per pane, not one per frame");
        assert_eq!(cache.hits(), 4);
    }

    /// Every `TextShape` the label phase produced, reaching inside a haloed
    /// label's `Shape::Vec`.
    fn text_shapes(shapes: &[egui::Shape]) -> Vec<&egui::epaint::TextShape> {
        fn walk<'a>(shape: &'a egui::Shape, into: &mut Vec<&'a egui::epaint::TextShape>) {
            match shape {
                egui::Shape::Text(text) => into.push(text),
                egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| walk(s, into)),
                _ => {}
            }
        }
        let mut found = Vec::new();
        for shape in shapes {
            walk(shape, &mut found);
        }
        found
    }

    /// Whether a mesh carries glyphs rather than flat fill.
    ///
    /// A ground fill's vertices all sample epaint's white texel; a glyph's
    /// sample the font atlas. Both meshes carry `TextureId::Managed(0)`, so
    /// the UV is what separates them — which is what keeps a shape-kind
    /// sequence able to say "and then the label" now that the label is a mesh.
    fn is_glyph_mesh(mesh: &egui::Mesh) -> bool {
        mesh.vertices.iter().any(|v| v.uv != egui::epaint::WHITE_UV)
    }

    /// The vertices `paint_labels` put on the painter, in order.
    ///
    /// A stronger reading than the `(text, pos, colour)` triples this
    /// replaced: it is the geometry itself, so a memo that answered a galley
    /// addressing the wrong atlas texels is a difference here and was not
    /// there.
    fn painted_glyph_vertices(
        shapes: &[egui::epaint::ClippedShape],
    ) -> Vec<(egui::Pos2, egui::Pos2, egui::Color32)> {
        shapes
            .iter()
            .filter_map(|c| match &c.shape {
                egui::Shape::Mesh(m) if is_glyph_mesh(m) => Some(m),
                _ => None,
            })
            .flat_map(|m| m.vertices.iter().map(|v| (v.pos, v.uv, v.color)))
            .collect()
    }

    /// **`text-max-width` wraps, and the collision box covers the block.**
    ///
    /// The defect: no wrapping existed anywhere in walkers, so a style's
    /// `text-max-width` was parsed by nothing and every label drew as one run.
    #[test]
    fn a_long_name_wraps_to_its_styled_width() {
        let _ledger = ledger_guard();
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
        let _ledger = ledger_guard();
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
        let _ledger = ledger_guard();
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
        let mut shapes = Vec::new();
        let _ = shapes_of_one_pass(&ctx, canvas, |ui| {
            shapes = solve_and_paint(ui, distinct);
        });
        assert_eq!(
            text_shapes(&shapes).len(),
            4,
            "fixture: these four anchors must not collide, or the assertion \
             below is about the overlap rule instead of the repeat rule"
        );

        let crowded: Vec<Text> = anchors.iter().map(|at| label("Rio Grande", *at)).collect();
        let mut shapes = Vec::new();
        let _ = shapes_of_one_pass(&ctx, canvas, |ui| {
            shapes = solve_and_paint(ui, crowded);
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
        let mut shapes = Vec::new();
        let _ = shapes_of_one_pass(&ctx, canvas, |ui| {
            shapes = solve_and_paint(ui, spread);
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
        let _ledger = ledger_guard();
        let ctx = egui::Context::default();
        let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));

        let mut solved = Vec::new();
        let shapes = shapes_of_one_pass(&ctx, canvas, |ui| {
            solved = solve_and_paint(ui, vec![label("Washita River", egui::pos2(400.0, 300.0))]);
        });

        let texts = text_shapes(&solved);
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
        let _ledger = ledger_guard();
        let ctx = egui::Context::default();
        let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));

        // `place_city_dot_z7`, the layer this name actually draws through, asks
        // for `text-max-width: 8`.
        let long = label(LONG_NAME, egui::pos2(400.0, 300.0)).with_wrapping(Some(8.0), None);
        let mut shapes = Vec::new();
        let _ = shapes_of_one_pass(&ctx, canvas, |ui| {
            shapes = solve_and_paint(ui, vec![long]);
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

            let mut shapes = Vec::new();
            let _ = shapes_of_one_pass(&ctx, canvas, |ui| {
                shapes = solve_and_paint(ui, vec![text]);
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
        let _ledger = ledger_guard();
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
        let _ledger = ledger_guard();
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
                    x: squallar_geo::wrap_tile_x(tx, tile_zoom),
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
    // **Every test in this module takes `ledger_guard()`, first thing.**
    //
    // `tile_mesh::ledger`'s counters are process-global and are written by
    // production `emit`, so a test does not have to *read* them to disturb
    // one that does — painting a tile is enough. The lock used to be taken
    // only by the tests that read totals, and the nineteen that merely paint
    // ran beside them: a sibling's label was counted into a reader's figure
    // and `a_declined_run_does_not_place_a_label_in_its_span_twice` saw
    // `label_anchors_placed` of 2 where it required 1.
    //
    // **It was invisible on an idle box.** Serialized
    // (`--test-threads=1`) all 31 pass; run as a filtered subset under load
    // 15-19 twelve fail, and the same tests pass in a full-lib run on a quiet
    // machine, which is why five runs on one box and three on another called
    // it green while `cargo test --workspace` reddened. Scheduling decided
    // the outcome, so this is a race, and a race is the regression.
    // -----------------------------------------------------------------

    static LEDGER: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// The module's turn at the process-global ledger, with a poisoned lock
    /// read as a live one.
    ///
    /// **Poison-tolerant on purpose.** These tests share
    /// `tile_mesh::ledger`'s counters, so one failing test used to poison the
    /// lock and every sibling then died on `PoisonError` instead of running:
    /// one real assertion arrived buried under ten failures that said nothing
    /// about themselves. A panicking test has already failed and reported;
    /// it must not decide the outcome of the others.
    fn ledger_guard() -> std::sync::MutexGuard<'static, ()> {
        LEDGER
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// A painter that hands back a payload for every span of runs it is asked
    /// about, and remembers what it was asked: the tile, the span as
    /// `(first_run, run_count)`, the placement and the opacity.
    #[derive(Default)]
    struct RecordingPainter {
        asked: std::sync::Mutex<Vec<Asked>>,
    }

    /// One thing [`RecordingPainter`] was asked for: the tile's id, the span
    /// as `(first_run, run_count)`, the placement and the opacity.
    type Asked = (u64, (usize, usize), crate::tile_mesh::Placement, f32);

    impl crate::tile_mesh::TileMeshPainter for RecordingPainter {
        fn payload(
            &self,
            draw: crate::tile_mesh::GroundDraw<'_>,
        ) -> Option<std::sync::Arc<dyn std::any::Any + Send + Sync>> {
            self.asked
                .lock()
                .expect("the recorder is not poisoned")
                .push((
                    draw.meshes.id(),
                    (draw.first_run, draw.run_count),
                    draw.place,
                    draw.opacity,
                ));
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

    /// A tile whose label's anchor falls **between two paths of one stroke
    /// run**, so the run's span covers the label.
    ///
    /// `flatten` lets a `Text` sit inside a span deliberately — it neither
    /// opens nor closes a run, because the ground phase defers every label —
    /// which is exactly the arrangement that makes a declined run's CPU
    /// fallback able to place the label a second time.
    fn a_tile_with_a_label_inside_a_stroke_span() -> Vec<ShapeOrText> {
        let line = |from: f32, to: f32| {
            ShapeOrText::Shape(egui::Shape::line(
                vec![egui::pos2(from, from), egui::pos2(to, to)],
                egui::Stroke::new(2.0, egui::Color32::GREEN),
            ))
        };
        vec![
            line(0.0, EXTENT / 2.0),
            ShapeOrText::Text(Text::new(
                egui::pos2(EXTENT / 2.0, EXTENT / 2.0),
                "Monaco".to_owned(),
                12.0,
                egui::Color32::WHITE,
                0.0,
            )),
            line(EXTENT / 2.0, EXTENT),
        ]
    }

    /// **A declined run does not place the label inside its span a second
    /// time.**
    ///
    /// The plan emits a `Place` step for a `Text` inside a run's span, because
    /// the run does not draw it. When the renderer declines the run, its span
    /// goes back on the CPU — and if that fallback placed the whole span it
    /// would place that same label again, from the two paths' own step. The
    /// label would then be laid out twice, collide with itself, and draw
    /// doubled on every frame a display change left the tile flattened at the
    /// wrong feathering.
    ///
    /// RED before the fix: `label_anchors_placed` reads 2.
    #[test]
    fn a_declined_run_does_not_place_a_label_in_its_span_twice() {
        let _ledger = ledger_guard();

        let shapes = a_tile_with_a_label_inside_a_stroke_span();
        let flat = crate::tile_mesh::flatten(&shapes, FEATHERING);
        // Non-triviality: the fixture is only the case under test if the two
        // paths really are one run whose span reaches over the label.
        assert_eq!(flat.runs().len(), 1, "the fixture is one stroke run");
        assert_eq!(
            (flat.runs()[0].shape_index, flat.runs()[0].shape_span),
            (0, 3),
            "the run's span does not cover the label, so nothing here is the \
             double-place case"
        );

        let painter: std::sync::Arc<dyn crate::tile_mesh::TileMeshPainter> =
            std::sync::Arc::new(RecordingPainter::default());

        // The control: flattened and drawn at the same feathering, the run is
        // drawn from the GPU and the label places once.
        let (_, drawn) = one_ground_pass_of(shapes.clone(), Some(&painter), FEATHERING, FEATHERING);
        assert_eq!(drawn.stroke_draws, 1, "the control did not draw the run");
        assert_eq!(drawn.label_anchors_placed, 1);

        // The case: a display change leaves the tile flattened at another
        // feathering, so the run is declined and its span falls back.
        let (_, declined) =
            one_ground_pass_of(shapes, Some(&painter), FEATHERING, FEATHERING / 2.0);
        assert_eq!(
            declined.stroke_draws, 0,
            "the run was drawn after all, so the fallback under test did not run"
        );
        assert!(
            declined.path_points_placed > 0,
            "the declined run placed no path points, so its span did not fall \
             back to the CPU at all"
        );
        assert_eq!(
            declined.label_anchors_placed, 1,
            "the declined run placed the label inside its span a second time; \
             it is drawn twice, over itself"
        );
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
        one_ground_pass_of(a_styled_tile(), painter, flattened_at, drawn_at)
    }

    /// [`one_ground_pass_at`] over a caller's shape list.
    fn one_ground_pass_of(
        shapes: Vec<ShapeOrText>,
        painter: Option<&std::sync::Arc<dyn crate::tile_mesh::TileMeshPainter>>,
        flattened_at: f32,
        drawn_at: f32,
    ) -> (
        Vec<egui::epaint::ClippedShape>,
        crate::tile_mesh::ledger::Totals,
    ) {
        one_ground_pass_dimmed(shapes, painter, flattened_at, drawn_at, 1.0)
    }

    /// [`one_ground_pass_of`] with the ground told a layer opacity, which is
    /// what `draw_tile_layer` reads off the painter and hands across.
    fn one_ground_pass_dimmed(
        shapes: Vec<ShapeOrText>,
        painter: Option<&std::sync::Arc<dyn crate::tile_mesh::TileMeshPainter>>,
        flattened_at: f32,
        drawn_at: f32,
        opacity: f32,
    ) -> (
        Vec<egui::epaint::ClippedShape>,
        crate::tile_mesh::ledger::Totals,
    ) {
        let ctx = egui::Context::default();
        let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(256.0, 256.0));
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
                    opacity,
                },
                rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                &mut labels,
                Background::Inline,
            );
            paint_labels(
                ui.painter(),
                labels.clone(),
                &mut walkers::GalleyCache::default(),
                &mut crate::label_cache::LabelCache::default(),
                0,
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
        let _ledger = ledger_guard();

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
        let _ledger = ledger_guard();

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
                egui::Shape::Mesh(m) if is_glyph_mesh(m) => "glyphs",
                egui::Shape::Mesh(_) => "mesh",
                other => panic!("unexpected shape {other:?}"),
            })
            .collect();
        assert_eq!(
            kinds,
            vec!["rect", "callback", "path", "callback", "glyphs"],
            "the fills did not draw in the positions the style put them in"
        );
    }

    /// Every callback carries the placement the CPU path would have placed
    /// by, and the runs it was asked for — the two things that decide which
    /// geometry lands where.
    ///
    /// The two fill runs here are **not** batched together, and that is the
    /// point of taking this reading at [`NO_FEATHERING`]: the stroke between
    /// them is refused by the flatten and placed on the CPU, so a drawing
    /// shape sits between the runs and `build_plan` opens a second batch. The
    /// batched reading is
    /// `a_stroke_run_becomes_a_callback_at_its_own_position`.
    #[test]
    fn each_ground_draw_carries_its_own_run_and_the_cpu_paths_placement() {
        let _ledger = ledger_guard();

        let recorder = std::sync::Arc::new(RecordingPainter::default());
        let painter: std::sync::Arc<dyn crate::tile_mesh::TileMeshPainter> = recorder.clone();
        let _ = one_ground_pass(Some(&painter));

        let asked = recorder.asked.lock().expect("not poisoned").clone();
        assert_eq!(asked.len(), 2);
        assert_eq!(
            asked[0].1,
            (0, 1),
            "the first run was not asked for as run 0 alone"
        );
        assert_eq!(asked[1].1, (1, 1));
        assert_eq!(asked[0].0, asked[1].0, "the two runs are one tile's");

        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(256.0, 256.0));
        let expected = crate::tile_mesh::Placement::of(rect);
        for (_, _, place, _) in &asked {
            assert_eq!(
                *place, expected,
                "a ground draw was placed by something other than \
                 `mvt::placement` over the whole tile"
            );
        }
    }

    /// **Every ground draw carries the ground's opacity, and at full opacity
    /// it carries exactly 1.0.** The renderer applies the value in its
    /// uniform, so a ground that read the painter but did not hand the number
    /// across would draw its runs at full strength under a dimmed layer while
    /// the CPU-placed shapes beside them dimmed. Both arms: the dimmed one
    /// pins the hand-over, the full one pins that a layer nobody dimmed asks
    /// for nothing but 1.0.
    #[test]
    fn every_ground_draw_carries_the_grounds_opacity() {
        let _ledger = ledger_guard();

        for opacity in [1.0_f32, 0.5] {
            let recorder = std::sync::Arc::new(RecordingPainter::default());
            let painter: std::sync::Arc<dyn crate::tile_mesh::TileMeshPainter> = recorder.clone();
            let (_, totals) = one_ground_pass_dimmed(
                a_styled_tile(),
                Some(&painter),
                FEATHERING,
                FEATHERING,
                opacity,
            );
            // Non-vacuity: the runs went to the GPU, so the recorder was asked.
            assert_eq!((totals.mesh_draws, totals.stroke_draws), (2, 1));
            let asked = recorder.asked.lock().expect("not poisoned").clone();
            assert!(!asked.is_empty(), "no ground draw was asked for");
            for (_, span, _, carried) in &asked {
                assert_eq!(
                    *carried, opacity,
                    "the draw of runs {span:?} did not carry the ground's \
                     opacity {opacity}"
                );
            }
        }
    }

    /// **A layer at zero opacity counts no ground draw, because it makes
    /// none.**
    ///
    /// egui's `Painter::add` turns every shape a painter at 0.0 is handed
    /// into `Shape::Noop`, and a `Shape::Callback` is no exception -- so a
    /// basemap the user has dragged to 0% draws nothing whichever path its
    /// runs take. What it used to do anyway was mint the `GroundDraw` payload
    /// and count a mesh or stroke draw per run, which put draws that never
    /// reached a GPU into an always-on counter (`tile_mesh::ledger`), in the
    /// one case where the honest figure is zero.
    ///
    /// Both halves, because skipping the GPU path by declining the runs would
    /// have been *worse* than leaving it alone: the decline route places
    /// every declined run on the CPU instead, so the vertices and points
    /// would have moved into the ledger's other counters rather than out of
    /// it.
    #[test]
    fn a_ground_at_zero_opacity_counts_no_draw_on_either_path() {
        let _ledger = ledger_guard();

        let pass = |opacity: f32| {
            let recorder = std::sync::Arc::new(RecordingPainter::default());
            let painter: std::sync::Arc<dyn crate::tile_mesh::TileMeshPainter> = recorder.clone();
            let (_, totals) = one_ground_pass_dimmed(
                a_styled_tile(),
                Some(&painter),
                FEATHERING,
                FEATHERING,
                opacity,
            );
            let asked = recorder.asked.lock().expect("not poisoned").len();
            (totals, asked)
        };

        // The control: at full strength this tile's runs really do go to the
        // GPU and really are counted, so the zeroes below are the guard's
        // doing and not an empty fixture.
        let (full, full_asked) = pass(1.0);
        assert_eq!(
            (full.mesh_draws, full.stroke_draws),
            (2, 1),
            "control: an opaque tile counted no ground draw, so this fixture \
             has no path for a transparent one to skip",
        );
        assert!(full_asked > 0, "control: the renderer was never asked");

        let (clear, clear_asked) = pass(0.0);
        assert_eq!(
            (clear.mesh_draws, clear.stroke_draws),
            (0, 0),
            "a layer at 0% opacity counted {} mesh and {} stroke draws that \
             egui turned into `Noop` before any of them reached a GPU",
            clear.mesh_draws,
            clear.stroke_draws,
        );
        assert_eq!(
            clear_asked, 0,
            "the renderer was asked for {clear_asked} ground-draw payloads \
             for a layer at 0% opacity, and egui discards every one of them",
        );
        assert_eq!(
            (clear.mesh_vertices_placed, clear.path_points_placed),
            (full.mesh_vertices_placed, full.path_points_placed),
            "the skipped runs went to the CPU instead of nowhere: the \
             ledger's placed counters moved rather than its draw counters \
             going away, which is more work for the same invisible tile",
        );
        assert_eq!(
            clear.label_anchors_placed, full.label_anchors_placed,
            "the guard took the tile's labels with it: they are placed by the \
             text walk, not by the run batch, and a layer at 0% still defers \
             them for the label phase to drop",
        );
    }

    /// One layer pass over a tile with runs, with the ground draws recorded:
    /// the shapes the pass emitted, the tile's rect and what the renderer was
    /// asked. `Some(opacity)` runs it under a `Ui` set to that opacity;
    /// `None` runs it through [`shapes_of_one_pass`], the harness that never
    /// heard of opacity, which is the control the full arm is compared to.
    fn a_tile_layer_pass_at(
        opacity: Option<f32>,
    ) -> (Vec<egui::epaint::ClippedShape>, egui::Rect, Vec<Asked>) {
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
        let span = crate::tiles::tile_span(&projector, canvas, tile_zoom);
        let tile_id = TileId {
            x: squallar_geo::wrap_tile_x(span.west, tile_zoom),
            y: span.north,
            zoom: tile_zoom,
        };
        tiles.put_for_test(tile_id, Tile::Vector(std::sync::Arc::new(a_styled_tile())));
        let rect = projector.tile_rect(tile_id);

        let recorder = std::sync::Arc::new(RecordingPainter::default());
        let painter: std::sync::Arc<dyn crate::tile_mesh::TileMeshPainter> = recorder.clone();
        let draw = |ui: &egui::Ui| {
            let labels =
                draw_tile_layer(ui, &projector, zoom, &mut tiles, 0, Some(&painter)).labels;
            paint_labels(
                ui.painter(),
                labels,
                &mut walkers::GalleyCache::default(),
                &mut crate::label_cache::LabelCache::default(),
                0,
            );
        };
        let shapes = match opacity {
            Some(opacity) => shapes_of_one_pass_at(&ctx, canvas, opacity, draw),
            None => shapes_of_one_pass(&ctx, canvas, draw),
        };
        let asked = recorder.asked.lock().expect("not poisoned").clone();
        (shapes, rect, asked)
    }

    /// The kind of every shape a pass emitted, in order, with `Noop` named so
    /// a shape egui dropped for being invisible is a difference and not a gap.
    fn kinds_of(shapes: &[egui::epaint::ClippedShape]) -> Vec<&'static str> {
        shapes
            .iter()
            .map(|clipped| match &clipped.shape {
                egui::Shape::Noop => "noop",
                egui::Shape::Rect(_) => "rect",
                egui::Shape::Mesh(m) if is_glyph_mesh(m) => "glyphs",
                egui::Shape::Mesh(_) => "mesh",
                egui::Shape::Callback(_) => "callback",
                egui::Shape::Path(_) => "path",
                other => panic!("unexpected shape {other:?}"),
            })
            .collect()
    }

    /// **`draw_tile_layer` reads the layer's opacity off the painter it was
    /// handed, and a tile at half opacity is dimmed on both of its paths.**
    ///
    /// The walk sets the `Ui`'s opacity around a layer's arm; this is the
    /// arm. Under a `Ui` at 0.5 every ground draw carries 0.5 -- the read
    /// this pins is `ui.painter().opacity()`, and a ground filled from a
    /// literal 1.0 would record 1.0 here -- while the tile's hoisted
    /// background, which egui places on the CPU, arrives with every vertex at
    /// `gamma_multiply(0.5)` of its colour: the two paths dim by the same
    /// operation, which is what makes a vector layer's opacity one number and
    /// not a fill drawn at one strength under a ground at another. The
    /// sequence of shapes is the same as at 1.0, so a dim changes colours
    /// and nothing about what is drawn or where.
    ///
    /// The other arm: under a `Ui` at 1.0 every draw carries 1.0 and the
    /// shapes are the ones the untouched harness emits, so a layer nobody
    /// dimmed paints exactly what it painted before opacity existed.
    #[test]
    fn a_tile_layer_at_half_opacity_dims_its_runs_and_its_ground_alike() {
        let _ledger = ledger_guard();
        let fill = egui::Color32::from_rgb(0x10, 0x20, 0x30);

        // The vertex colour of the tile's hoisted background: a mesh of four
        // vertices whose bounds are the tile's rect rounded to pixels, which
        // is how `a_vector_tile_reaches_the_painter_placed_on_its_own_rect`
        // finds it too.
        let background_colour = |shapes: &[egui::epaint::ClippedShape], rect: egui::Rect| {
            use egui::emath::GuiRounding as _;
            let expected = rect.round_to_pixels(1.0);
            shapes
                .iter()
                .find_map(|clipped| match &clipped.shape {
                    egui::Shape::Mesh(m)
                        if m.vertices.len() == 4
                            && (m.calc_bounds().min - expected.min).length() < 0.01
                            && (m.calc_bounds().max - expected.max).length() < 0.01 =>
                    {
                        Some(m.vertices[0].color)
                    }
                    _ => None,
                })
                .expect("the tile's hoisted background did not reach the painter")
        };

        let (full, rect, asked_full) = a_tile_layer_pass_at(Some(1.0));
        assert!(!asked_full.is_empty(), "no run reached the renderer");
        for (_, span, _, carried) in &asked_full {
            assert_eq!(
                *carried, 1.0,
                "runs {span:?} at full opacity carried {carried}"
            );
        }
        assert_eq!(background_colour(&full, rect), fill);
        assert!(
            full.iter()
                .any(|c| matches!(c.shape, egui::Shape::Callback(_))),
            "no callback was emitted, so the runs were placed on the CPU and \
             the opacity carried above was never going to be applied"
        );
        // Shape for shape what the untouched harness emits. Compared as
        // `Debug` text rather than by `==`: a `PaintCallback` is equal only
        // to itself by pointer, and each pass mints its own payload.
        let (untouched, _, asked_untouched) = a_tile_layer_pass_at(None);
        assert_eq!(
            format!("{full:?}"),
            format!("{untouched:?}"),
            "a Ui at 1.0 painted something other than a Ui that was never \
             told an opacity"
        );
        // Minus the tile id: every `put_for_test` flattens afresh and
        // `TileMeshes::id` is minted per flatten, so two passes over the
        // same fixture never share one. What must agree is what was asked
        // for and at what strength.
        let sans_id = |asked: &[Asked]| -> Vec<((usize, usize), crate::tile_mesh::Placement, f32)> {
            asked
                .iter()
                .map(|(_, span, place, o)| (*span, *place, *o))
                .collect()
        };
        assert_eq!(sans_id(&asked_untouched), sans_id(&asked_full));

        let (half, half_rect, asked_half) = a_tile_layer_pass_at(Some(0.5));
        assert_eq!(half_rect, rect);
        assert_eq!(
            asked_half.len(),
            asked_full.len(),
            "a dim changed what went to the GPU"
        );
        for (_, span, _, carried) in &asked_half {
            assert_eq!(
                *carried, 0.5,
                "runs {span:?} under a Ui at 0.5 carried {carried}: \
                 `draw_tile_layer` is not reading the painter's opacity"
            );
        }
        assert_eq!(
            background_colour(&half, rect),
            fill.gamma_multiply(0.5),
            "the CPU-placed background was not dimmed by the painter, so the \
             two paths of one tile would draw at different strengths"
        );
        assert_eq!(
            kinds_of(&half),
            kinds_of(&full),
            "a dim changed what was drawn, not only how strongly"
        );
    }

    /// **No stroke point is placed on the CPU while a renderer can draw it.**
    ///
    /// The stroke half of
    /// `ground_fills_stop_being_placed_on_the_cpu_while_labels_still_place`,
    /// and **RED on the unmodified baseline**, where the flatten had no stroke
    /// arm at all and every stroke point was copied on the frame thread every
    /// frame.
    ///
    /// **The `without` arm is a tile with no stroke runs**, and it has to be:
    /// a painterless pass over a tile that *has* them no longer places its
    /// points either, because it draws them from the buffers they were already
    /// tessellated into (`place_run_as_mesh`). What still places points is a
    /// tile flattened where the stroke arm produced no run at all, which
    /// [`NO_FEATHERING`] gives, so the zero below remains a move and not an
    /// absence.
    #[test]
    fn ground_strokes_stop_being_placed_on_the_cpu() {
        let _ledger = ledger_guard();

        let (_, without) = one_ground_pass_at(None, NO_FEATHERING, FEATHERING);
        assert!(
            without.path_points_placed > 0,
            "non-triviality: the CPU path placed no stroke points either, so \
             the zero below would prove nothing"
        );
        assert_eq!(without.stroke_draws, 0, "no painter, no stroke draws");
        assert_eq!(
            without.stroke_run_meshes, 0,
            "non-triviality: the `without` arm materialised a run, so its \
             points were never the CPU-placed ones this compares against"
        );

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

    /// Paths in the dense fixture's single stroke run — enough that the shape
    /// count this run collapses to is unmistakably a collapse and not noise.
    /// A real city-core tile carries 708 of them
    /// (`tile_mesh::fixture_tests`); this is the same shape in miniature.
    const DENSE_RUN_PATHS: usize = 64;

    /// A tile whose ground is one fill and then a **run of many** strokes,
    /// with a label after them.
    ///
    /// Nothing that draws sits between the paths, so `flatten` puts all of
    /// them in one run — which is what a styled city tile does and what
    /// [`a_styled_tile`]'s single path cannot show.
    fn a_tile_with_a_dense_stroke_run() -> Vec<ShapeOrText> {
        let mut mesh = egui::epaint::Mesh::default();
        mesh.add_rect_with_uv(
            egui::Rect::from_min_size(egui::pos2(8.0, 8.0), egui::vec2(64.0, 64.0)),
            egui::Rect::from_min_max(egui::epaint::WHITE_UV, egui::epaint::WHITE_UV),
            egui::Color32::RED,
        );
        let mut shapes = vec![ShapeOrText::Shape(egui::Shape::Mesh(mesh.into()))];
        for step in 0..DENSE_RUN_PATHS {
            let at = 16.0 + step as f32;
            shapes.push(ShapeOrText::Shape(egui::Shape::line(
                vec![egui::pos2(at, 0.0), egui::pos2(at, EXTENT)],
                egui::Stroke::new(DENSE_RUN_WIDTH, egui::Color32::GREEN),
            )));
        }
        shapes.push(ShapeOrText::Text(Text::new(
            egui::pos2(EXTENT / 2.0, EXTENT / 2.0),
            "Monaco".to_owned(),
            12.0,
            egui::Color32::WHITE,
            0.0,
        )));
        shapes
    }

    /// The dense fixture's stroke width, in points. Thick enough at
    /// [`FEATHERING`] to take epaint's thick branch, which is the one a real
    /// road takes.
    const DENSE_RUN_WIDTH: f32 = 2.0;

    /// **A pass with no painter draws its stroke runs from the buffers they
    /// were already tessellated into, instead of putting their paths back
    /// through the tessellator.**
    ///
    /// **The floor strip's cut.** A `GroundOnly` pass has no painter — its
    /// primitives are copied into the mirror with every callback swapped for
    /// an empty mesh — so every run of every tile it draws used to fall back
    /// to placing the run's `Shape::Path`s, and `Context::tessellate` walked
    /// all of them again on every frame of a gesture. The geometry was already
    /// computed once, at tile build.
    ///
    /// **The before arm is the same tile with the same runs**, declined by the
    /// feathering guard rather than by having no runs, so what it reports is
    /// exactly what this pass did before this cut.
    ///
    /// **It fails in both directions.** A cut that simply stopped drawing
    /// would take `ground_shapes` to one and pass a count assertion; the
    /// vertex equality below is against epaint's own arithmetic for this
    /// stroke — [`crate::tile_mesh::stroke::vertices_per_path_point`] times
    /// the points the paths carry — so a run that lost its geometry, or drew
    /// it twice, moves it.
    ///
    /// RED on the unmodified baseline: `stroke_run_meshes` reads 0 and
    /// `ground_shapes` reads the path count.
    #[test]
    fn a_painterless_pass_draws_a_stroke_run_from_the_buffers_it_was_tessellated_into() {
        let _ledger = ledger_guard();

        let shapes = a_tile_with_a_dense_stroke_run();
        let flat = crate::tile_mesh::flatten(&shapes, FEATHERING);
        let runs: Vec<crate::tile_mesh::MeshRun> = flat
            .runs()
            .iter()
            .filter(|run| run.kind == crate::tile_mesh::RunKind::Stroke)
            .copied()
            .collect();
        assert_eq!(
            runs.len(),
            1,
            "fixture: the paths did not flatten into one stroke run, so the \
             collapse below is not the collapse under test"
        );
        assert_eq!(
            runs[0].shape_span as usize, DENSE_RUN_PATHS,
            "fixture: the run covers {} of the {DENSE_RUN_PATHS} paths",
            runs[0].shape_span
        );

        // Before: the same tile, the same runs, declined by the feathering
        // guard — which is what every painterless pass did to every run.
        let (_, before) = one_ground_pass_of(shapes.clone(), None, FEATHERING, FEATHERING / 2.0);
        assert_eq!(
            before.stroke_run_meshes, 0,
            "the before arm materialised a run, so it is not the before arm"
        );
        assert!(
            before.path_points_placed > 0,
            "non-triviality: the before arm placed no stroke points, so the \
             zero below would prove nothing"
        );
        assert_eq!(
            before.ground_shapes,
            DENSE_RUN_PATHS as u64 + 1,
            "the before arm handed the tessellator {} shapes, not the fill \
             plus one per path",
            before.ground_shapes
        );

        let (emitted, after) = one_ground_pass_of(shapes, None, FEATHERING, FEATHERING);

        assert_eq!(
            after.path_points_placed, 0,
            "stroke points are still being copied on the frame thread"
        );
        assert_eq!(
            after.stroke_run_meshes, 1,
            "the run was not drawn from the buffers it was tessellated into"
        );
        assert_eq!(
            after.ground_shapes, 2,
            "the ground handed the tessellator {} shapes; the tile is a fill \
             and a run, so it is two",
            after.ground_shapes
        );

        // The picture, not the count: every vertex the paths tessellate to is
        // in the mesh, by epaint's own arithmetic for this stroke.
        let per_point = u64::from(crate::tile_mesh::stroke::vertices_per_path_point(
            DENSE_RUN_WIDTH,
            FEATHERING,
        ));
        assert_eq!(
            after.stroke_mesh_vertices,
            before.path_points_placed * per_point,
            "the run's mesh carries {} vertices against the {} epaint \
             tessellates {} path points to",
            after.stroke_mesh_vertices,
            before.path_points_placed * per_point,
            before.path_points_placed
        );
        assert_eq!(
            after.label_anchors_placed, before.label_anchors_placed,
            "the label stopped placing, so the figures above are a tile pass \
             that did not run"
        );

        // And it draws where the style put it: the fill, then the run over it.
        let kinds: Vec<&'static str> = emitted
            .iter()
            .map(|clipped| match &clipped.shape {
                egui::Shape::Mesh(m) if is_glyph_mesh(m) => "glyphs",
                egui::Shape::Mesh(_) => "mesh",
                egui::Shape::Path(_) => "path",
                other => panic!("unexpected shape {other:?}"),
            })
            .collect();
        assert_eq!(
            kinds,
            vec!["mesh", "mesh", "glyphs"],
            "the run did not draw as one mesh in the fill's wake"
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
        let _ledger = ledger_guard();

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

    /// **A stroke run draws where the style put it, and the batch keeps it
    /// there.** The fixture's fill, stroke, fill are three consecutive runs
    /// with nothing the ground phase draws between them, so they are one
    /// callback — and the order the road draws in is now carried by the run
    /// span inside that callback rather than by three primitives' positions.
    ///
    /// Both halves are asserted, because either alone would pass a defect:
    /// the shape sequence alone would not notice a batch that drew its runs
    /// in the wrong order, and the span alone would not notice a batch pushed
    /// before the background rectangle.
    #[test]
    fn a_stroke_run_becomes_a_callback_at_its_own_position() {
        let _ledger = ledger_guard();

        let recorder = std::sync::Arc::new(RecordingPainter::default());
        let painter: std::sync::Arc<dyn crate::tile_mesh::TileMeshPainter> = recorder.clone();
        let (with, totals) = one_ground_pass_at(Some(&painter), FEATHERING, FEATHERING);

        let kinds: Vec<&'static str> = with
            .iter()
            .map(|clipped| match &clipped.shape {
                egui::Shape::Rect(_) => "rect",
                egui::Shape::Callback(_) => "callback",
                egui::Shape::Path(_) => "path",
                egui::Shape::Mesh(m) if is_glyph_mesh(m) => "glyphs",
                egui::Shape::Mesh(_) => "mesh",
                other => panic!("unexpected shape {other:?}"),
            })
            .collect();
        assert_eq!(
            kinds,
            vec!["rect", "callback", "glyphs"],
            "the tile's three runs are not one callback sitting where the \
             first of them sat, after the background rectangle"
        );

        // Non-triviality: one callback is only a win if it still draws all
        // three runs. Two fills and one stroke went to the GPU.
        assert_eq!((totals.mesh_draws, totals.stroke_draws), (2, 1));

        let asked = recorder.asked.lock().expect("not poisoned").clone();
        assert_eq!(asked.len(), 1, "the three runs were not one ask");
        assert_eq!(
            asked[0].1,
            (0, 3),
            "the batch does not cover runs 0, 1 and 2 in that order, so the \
             fixture's fill, stroke, fill order did not survive: the stroke \
             must be the middle draw, not before or after both"
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

        let _ledger = ledger_guard();
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
                .filter(|c| matches!(&c.shape, egui::Shape::Mesh(m) if !is_glyph_mesh(m)))
                .count(),
            2,
            "a refused run was not placed exactly once"
        );
    }

    // -----------------------------------------------------------------
    // The hoisted background rectangles.
    //
    // `draw_tile_layer` draws every vector tile's background ahead of every
    // tile's geometry, under the pane's clip, so epaint makes one primitive
    // of them where the per-tile clip made one each. Pixel parity with the
    // clipped arrangement is the GPU suite's to hold
    // (`squallar-gpu/tests/tile_mesh_gpu.rs`); what is pinned here is the
    // arrangement itself: where the rectangles sit in the shape list, what
    // clip they carry, and that the tessellator really merges them.
    // -----------------------------------------------------------------

    use egui::emath::GuiRounding as _;

    /// The zoom every fixture below is drawn at: whole, so the tile side is
    /// exactly 256 points and a 512-point pane spans more than one cell.
    const HOIST_ZOOM: f64 = 6.0;

    /// A styled tile in miniature whose ground the CPU path draws as real
    /// geometry: the background rectangle first, then one opaque quad over
    /// the whole extent.
    fn a_tile_with_a_quad(background: egui::Color32) -> Tile {
        let mut quad = egui::epaint::Mesh::default();
        quad.add_rect_with_uv(
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(EXTENT, EXTENT)),
            egui::Rect::from_min_max(egui::epaint::WHITE_UV, egui::epaint::WHITE_UV),
            egui::Color32::RED,
        );
        Tile::Vector(std::sync::Arc::new(vec![
            ShapeOrText::Shape(egui::Shape::rect_filled(
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(EXTENT, EXTENT)),
                0.0,
                background,
            )),
            ShapeOrText::Shape(egui::Shape::Mesh(quad.into())),
        ]))
    }

    /// A 512-point pane at [`HOIST_ZOOM`] over `DeadSource`, and the cells its
    /// span walks, in walk order (north to south, west to east).
    fn a_pane_and_its_cells() -> (
        egui::Context,
        egui::Rect,
        walkers::Projector,
        HttpsTiles,
        Vec<TileId>,
    ) {
        squallar_radar::tls::init();
        let ctx = egui::Context::default();
        let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(512.0, 512.0));
        let mut memory = walkers::MapMemory::default();
        memory
            .set_zoom(HOIST_ZOOM)
            .expect("zoom 6 is in walkers' range");
        let projector = walkers::Projector::new(canvas, &memory, walkers::lat_lon(35.33, -97.28));
        let tiles = HttpsTiles::with_client(
            DeadSource,
            ctx.clone(),
            reqwest::Client::builder()
                .build()
                .expect("the test client should build"),
        );
        let tile_zoom = HOIST_ZOOM.round() as u8;
        let span = crate::tiles::tile_span(&projector, canvas, tile_zoom);
        let mut cells = Vec::new();
        for ty in span.north..=span.south {
            for tx in span.west..=span.east {
                cells.push(TileId {
                    x: squallar_geo::wrap_tile_x(tx, tile_zoom),
                    y: ty,
                    zoom: tile_zoom,
                });
            }
        }
        assert!(
            cells.len() > 1,
            "fixture: one cell cannot show a merge or a neighbour"
        );
        (ctx, canvas, projector, tiles, cells)
    }

    /// One ground pass over the pane, returning the shapes it emitted and the
    /// clip the pane's own painter carried.
    fn one_pane_pass(
        ctx: &egui::Context,
        canvas: egui::Rect,
        projector: &walkers::Projector,
        tiles: &mut HttpsTiles,
    ) -> (Vec<egui::epaint::ClippedShape>, egui::Rect) {
        let pane_clip = std::cell::Cell::new(egui::Rect::NOTHING);
        let shapes = shapes_of_one_pass(ctx, canvas, |ui| {
            pane_clip.set(ui.clip_rect());
            draw_tile_layer(ui, projector, HOIST_ZOOM, tiles, 0, None);
        });
        (shapes, pane_clip.get())
    }

    /// An inline background: the `Shape::Rect` the tile's own walk places.
    fn is_fill(clipped: &egui::epaint::ClippedShape, fill: egui::Color32) -> bool {
        matches!(&clipped.shape, egui::Shape::Rect(r) if r.fill == fill && r.stroke.is_empty())
    }

    /// A solid four-vertex mesh in one colour: the hoisted form of a
    /// background (`tile_mesh::background_within`) -- and also the fixture's
    /// quads, which is why every colour below is distinct.
    fn is_solid_quad(clipped: &egui::epaint::ClippedShape, colour: egui::Color32) -> bool {
        matches!(
            &clipped.shape,
            egui::Shape::Mesh(m) if m.vertices.len() == 4 && m.vertices.iter().all(|v| v.color == colour)
        )
    }

    /// The quads of the walk's leading background mesh, in the order they were
    /// added -- four vertices each, so quad `i` is `vertices[4i..4i+4]`.
    ///
    /// **The arrangement the assertions below read.** Until 2026-09-09 each
    /// tile's background was its own `Shape::Mesh` and each was read off the
    /// shape list directly; they are now appended to one mesh in walk order
    /// (`tile_mesh::HoistedBackgrounds`), which is the arrangement epaint
    /// tessellated them into either way. Every geometric claim the per-shape
    /// reads made -- walk order, one quad per tile, each on its own tile's
    /// pixel-rounded rect, in that tile's colour -- is made here against the
    /// quads instead.
    fn background_quads(clipped: &egui::epaint::ClippedShape) -> Vec<(egui::Rect, egui::Color32)> {
        let egui::Shape::Mesh(mesh) = &clipped.shape else {
            panic!("{:?} is not the batched background mesh", clipped.shape)
        };
        assert_eq!(
            mesh.vertices.len() % 4,
            0,
            "the background mesh holds {} vertices, not whole hard rectangles",
            mesh.vertices.len()
        );
        (0..mesh.vertices.len() / 4)
            .map(|quad| {
                let corners = &mesh.vertices[4 * quad..4 * quad + 4];
                let colour = corners[0].color;
                assert!(
                    corners.iter().all(|v| v.color == colour),
                    "background quad {quad} is not one colour"
                );
                let mut rect = egui::Rect::NOTHING;
                for corner in corners {
                    rect.extend_with(corner.pos);
                }
                (rect, colour)
            })
            .collect()
    }

    fn within(a: egui::Rect, b: egui::Rect, tolerance: f32) -> bool {
        (a.min - b.min).length() < tolerance && (a.max - b.max).length() < tolerance
    }

    /// **Every tile's background rectangle leads the walk, under the pane's
    /// clip, and epaint makes one mesh of them.** Under the per-tile clip each
    /// was its own primitive -- one per tile, per pane, per frame, and the
    /// ground's largest remaining primitive source once its callbacks were
    /// batched.
    ///
    /// Three things are pinned and each alone would pass a defect: that the
    /// rectangles come first and carry the pane's clip (else they are still
    /// one primitive each); that every tile's geometry still follows them
    /// under its own clip (else the hoist changed what draws over what); and
    /// that the tessellator really folds them into one primitive, which is
    /// the count the whole thing exists for.
    #[test]
    fn the_background_rectangles_lead_the_walk_as_one_mesh() {
        let _ledger = ledger_guard();
        let (ctx, canvas, projector, mut tiles, cells) = a_pane_and_its_cells();
        let background = egui::Color32::from_rgb(0x10, 0x20, 0x30);
        for cell in &cells {
            tiles.put_for_test(*cell, a_tile_with_a_quad(background));
        }

        let (shapes, pane_clip) = one_pane_pass(&ctx, canvas, &projector, &mut tiles);
        let n = cells.len();
        assert!(
            shapes.len() > 1,
            "the walk emitted {} shapes for {n} tiles",
            shapes.len()
        );

        // ONE shape leads the walk and it carries the `n` backgrounds, in walk
        // order, each the hard quad on its own tile's pixel-rounded rect, and
        // it is under the pane's clip rather than any tile's.
        assert_eq!(
            shapes[0].clip_rect, pane_clip,
            "the background mesh carries a clip of its own"
        );
        let quads = background_quads(&shapes[0]);
        assert_eq!(
            quads.len(),
            n,
            "the background mesh holds {} quads for {n} tiles",
            quads.len()
        );
        for ((bounds, colour), cell) in quads.iter().zip(&cells) {
            assert_eq!(
                *colour, background,
                "the quad for {cell:?} is not the background colour"
            );
            let rect = projector.tile_rect(*cell).round_to_pixels(1.0);
            assert!(
                within(*bounds, rect, 0.01),
                "the background for {cell:?} sits at {bounds:?}, not on its tile {rect:?}"
            );
        }

        // No tile placed its background a second time, in either form, and
        // every tile's quad follows the backgrounds under that tile's own clip.
        let later = shapes[1..]
            .iter()
            .filter(|clipped| is_fill(clipped, background) || is_solid_quad(clipped, background))
            .count();
        assert_eq!(
            later, 0,
            "{later} background rectangles were also placed inside their tiles"
        );
        let tile_quads: Vec<&egui::epaint::ClippedShape> = shapes[1..]
            .iter()
            .filter(|clipped| is_solid_quad(clipped, egui::Color32::RED))
            .collect();
        assert_eq!(
            tile_quads.len(),
            n,
            "{} quads followed the backgrounds for {n} tiles",
            tile_quads.len()
        );
        for (quad, cell) in tile_quads.iter().zip(&cells) {
            assert_eq!(
                quad.clip_rect,
                projector.tile_rect(*cell).intersect(pane_clip),
                "the quad for {cell:?} lost its tile's clip"
            );
        }

        // The tessellator's own answer: one primitive for the lot, the quads
        // still one each. A hard rectangle is four vertices.
        let primitives = ctx.tessellate(shapes, 1.0);
        assert_eq!(primitives[0].clip_rect, pane_clip);
        let egui::epaint::Primitive::Mesh(first) = &primitives[0].primitive else {
            panic!("the first primitive is a callback")
        };
        assert_eq!(
            first.vertices.len(),
            4 * n,
            "the first primitive holds {} vertices, not the {} of {n} hard \
             rectangles: the backgrounds did not merge",
            first.vertices.len(),
            4 * n
        );
        assert_eq!(
            primitives.len(),
            1 + n,
            "{} primitives for {n} tiles; expected one mesh of backgrounds and \
             one clipped quad per tile",
            primitives.len()
        );
    }

    /// **A stretched ancestor's background is cut to its piece, not clipped
    /// to it -- and that cut is what the per-tile clip was for.** `HttpsTiles`
    /// answers an unanswered cell with a shallower tile and the `uv` window of
    /// it that covers the cell; placed against the whole ancestor, that tile's
    /// background reaches over three sibling cells. Under the pane's clip
    /// nothing else stops it.
    #[test]
    fn a_stretched_ancestor_s_background_is_cut_to_its_piece() {
        let _ledger = ledger_guard();
        let (ctx, canvas, projector, mut tiles, cells) = a_pane_and_its_cells();
        let background = egui::Color32::from_rgb(0x10, 0x20, 0x30);
        let ancestor = egui::Color32::from_rgb(0x70, 0x10, 0x10);
        let hole = cells[0];
        for cell in &cells[1..] {
            tiles.put_for_test(*cell, a_tile_with_a_quad(background));
        }
        tiles.put_for_test(
            TileId {
                x: hole.x / 2,
                y: hole.y / 2,
                zoom: hole.zoom - 1,
            },
            a_tile_with_a_quad(ancestor),
        );

        // Non-triviality: the hole IS answered through a window of the
        // ancestor, and the whole ancestor placed against that window reaches
        // past the piece -- else the cut is the identity and this proves
        // nothing.
        let piece = projector.tile_rect(hole);
        let answered = tiles
            .ground_at(hole)
            .expect("the ancestor answers the hole");
        assert_ne!(
            answered.uv, FULL_TILE_UV,
            "fixture: the hole was answered exactly"
        );
        let full = full_rect_of_clipped_tile(piece, answered.uv);
        assert!(
            full.width() > 1.5 * piece.width() && full.height() > 1.5 * piece.height(),
            "fixture: the placed ancestor {full:?} does not reach past the piece {piece:?}"
        );

        let (shapes, pane_clip) = one_pane_pass(&ctx, canvas, &projector, &mut tiles);
        // Nowhere but the batched background mesh: not as a clipped rectangle
        // inside its own tile, and not as a mesh of its own after it.
        let elsewhere = shapes[1..]
            .iter()
            .filter(|clipped| is_fill(clipped, ancestor) || is_solid_quad(clipped, ancestor))
            .count();
        assert_eq!(
            elsewhere, 0,
            "the ancestor's background was also drawn {elsewhere} times inside a tile"
        );
        assert_eq!(
            shapes[0].clip_rect, pane_clip,
            "the background mesh kept a clip of its own"
        );
        let quads = background_quads(&shapes[0]);
        let at: Vec<usize> = quads
            .iter()
            .enumerate()
            .filter(|(_, (_, colour))| *colour == ancestor)
            .map(|(at, _)| at)
            .collect();
        assert_eq!(
            at.len(),
            1,
            "the ancestor's background is {} of the mesh's quads",
            at.len()
        );
        assert_eq!(
            at[0], 0,
            "the hole is the walk's first cell, so its background leads; it sat at {}",
            at[0]
        );
        let piece = piece.round_to_pixels(1.0);
        assert!(
            within(quads[0].0, piece, 0.01),
            "the ancestor's background covers {:?}; the piece it may cover is {piece:?}",
            quads[0].0
        );
    }

    /// **Only a plain fill drawn first is taken out of its tile.** A stroked
    /// rectangle paints across its own edges, so cutting it to the piece is
    /// not the clip; and a rectangle that is not the tile's first shape has
    /// geometry under it that must stay under it. Both keep the tile's own
    /// clipped walk, exactly as before.
    #[test]
    fn a_background_that_is_not_a_plain_first_fill_stays_inside_its_tile() {
        let _ledger = ledger_guard();
        let (ctx, canvas, projector, mut tiles, cells) = a_pane_and_its_cells();
        let extent = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(EXTENT, EXTENT));
        let stroked = egui::Color32::from_rgb(0x10, 0x20, 0x30);
        let second = egui::Color32::from_rgb(0x30, 0x20, 0x10);

        // Cell 0: a stroked rectangle first.
        tiles.put_for_test(
            cells[0],
            Tile::Vector(std::sync::Arc::new(vec![ShapeOrText::Shape(
                egui::Shape::rect_stroke(
                    extent,
                    0.0,
                    egui::Stroke::new(2.0, stroked),
                    egui::StrokeKind::Inside,
                ),
            )])),
        );
        // Cell 1: a quad first, then a plain fill over it.
        let mut quad = egui::epaint::Mesh::default();
        quad.add_rect_with_uv(
            extent,
            egui::Rect::from_min_max(egui::epaint::WHITE_UV, egui::epaint::WHITE_UV),
            egui::Color32::RED,
        );
        tiles.put_for_test(
            cells[1],
            Tile::Vector(std::sync::Arc::new(vec![
                ShapeOrText::Shape(egui::Shape::Mesh(quad.into())),
                ShapeOrText::Shape(egui::Shape::rect_filled(extent, 0.0, second)),
            ])),
        );

        let (shapes, pane_clip) = one_pane_pass(&ctx, canvas, &projector, &mut tiles);

        let strokes: Vec<&egui::epaint::ClippedShape> = shapes
            .iter()
            .filter(|c| matches!(&c.shape, egui::Shape::Rect(r) if r.stroke.color == stroked))
            .collect();
        assert_eq!(
            strokes.len(),
            1,
            "the stroked rectangle drew {} times",
            strokes.len()
        );
        assert_eq!(
            strokes[0].clip_rect,
            projector.tile_rect(cells[0]).intersect(pane_clip),
            "a stroked rectangle was taken out of its tile's clip"
        );

        let fills: Vec<(usize, &egui::epaint::ClippedShape)> = shapes
            .iter()
            .enumerate()
            .filter(|(_, c)| is_fill(c, second))
            .collect();
        assert_eq!(
            fills.len(),
            1,
            "the second shape's fill drew {} times",
            fills.len()
        );
        assert_eq!(
            fills[0].1.clip_rect,
            projector.tile_rect(cells[1]).intersect(pane_clip),
            "a rectangle with geometry under it was taken out of its tile's clip"
        );
        let quad_at = shapes
            .iter()
            .position(|c| is_solid_quad(c, egui::Color32::RED))
            .expect("the quad drew");
        assert!(
            quad_at < fills[0].0,
            "the fill at {} was drawn before the quad at {quad_at} that is under it",
            fills[0].0
        );
    }

    /// `Background::Hoisted` is the caller's word that shape 0 is drawn: the
    /// tile's walk places everything else and that rectangle never -- on the
    /// planned walk and on the un-planned fallback alike, since either can be
    /// the one a frame takes.
    #[test]
    fn a_hoisted_background_is_not_placed_again_by_its_tile() {
        let _ledger = ledger_guard();
        let shapes = a_styled_tile();
        let flat = std::sync::Arc::new(crate::tile_mesh::flatten(&shapes, FEATHERING));
        assert!(
            flat.plan().is_some(),
            "fixture: the flattened tile carries a plan"
        );
        let ctx = egui::Context::default();
        let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(256.0, 256.0));

        // (rectangles, meshes, paths, labels) one ground pass emits.
        let counts = |ground: GroundMeshes<'_>, background: Background| {
            let mut labels = Vec::new();
            let emitted = shapes_of_one_pass(&ctx, canvas, |ui| {
                paint_vector_tile(
                    ui.painter(),
                    &shapes,
                    ground,
                    rect,
                    FULL_TILE_UV,
                    &mut labels,
                    background,
                );
            });
            let count =
                |pick: fn(&egui::Shape) -> bool| emitted.iter().filter(|c| pick(&c.shape)).count();
            (
                count(|s| matches!(s, egui::Shape::Rect(_))),
                count(|s| matches!(s, egui::Shape::Mesh(_))),
                count(|s| matches!(s, egui::Shape::Path(_))),
                labels.len(),
            )
        };
        // The planned walk: a plan, every run declined (no painter), so the
        // fills and the stroke are placed on the CPU through the plan's steps.
        let planned = GroundMeshes {
            meshes: Some(&flat),
            painter: None,
            pass_nr: 1,
            feathering: FEATHERING,
            opacity: 1.0,
        };
        // **The two arms differ by one shape KIND and by nothing else.** The
        // planned arm has the tile's flattened buffers, so its declined stroke
        // run is materialised into the mesh those buffers already hold
        // (`place_run_as_mesh`) and arrives as a third `Shape::Mesh` where the
        // un-planned arm, which has no buffers to materialise from, still
        // places the `Shape::Path`. Same geometry either way; what this test
        // is about is the rectangle, and in both arms hoisting removes exactly
        // it.
        for (name, ground, meshes, paths) in [
            ("planned", planned, 3, 0),
            ("un-planned", GroundMeshes::CPU_ONLY, 2, 1),
        ] {
            assert_eq!(
                counts(ground, Background::Inline),
                (1, meshes, paths, 1),
                "{name}: the control, inline, does not draw the tile as itself"
            );
            assert_eq!(
                counts(ground, Background::Hoisted),
                (0, meshes, paths, 1),
                "{name}: hoisted, the tile drew its background a second time or \
                 lost something else"
            );
        }
    }

    /// **The planned walk and the un-planned walk defer the same labels, at
    /// every `uv`.**
    ///
    /// `tile_mesh::build_plan` drops the steps for labels anchored off the
    /// tile, so the planned walk never sees them and the un-planned one culls
    /// them itself at `place_one`. That is only sound if the two lists are the
    /// same list — and the failure it would produce is a *missing name*,
    /// which no pixel comparison of a tile's ground draws would notice and
    /// which `solve_labels`' repeat-distance rule would happily paper over at
    /// a seam.
    ///
    /// The `uv` arms are the point. A window is what a stretched ancestor
    /// draws while the deeper tile is still arriving, and it is the one input
    /// the plan cannot know: an anchor inside the extent but outside the
    /// window is still the frame's to answer, and must survive the plan to
    /// reach it. Every quadrant is run, so a cull that quietly resolved
    /// against the wrong window would have to agree with `place_one` on all
    /// four to pass.
    #[test]
    fn the_planned_and_unplanned_walks_defer_the_same_labels() {
        let _ledger = ledger_guard();
        let quarter =
            |x: f32, y: f32| egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(0.5, 0.5));

        // A city core, three names in its buffer (one off each edge and one
        // off a corner), and two more inside the extent but well apart, so
        // every `uv` quadrant below keeps a different subset.
        let mut shapes = a_styled_tile();
        for (at, name) in [
            ((EXTENT / 2.0, EXTENT / 2.0), "Monaco"),
            ((-EXTENT * 0.02, EXTENT / 2.0), "Nice"),
            ((EXTENT / 2.0, EXTENT * 1.02), "Menton"),
            ((EXTENT * 1.03, -EXTENT * 0.03), "Cannes"),
            ((EXTENT * 0.2, EXTENT * 0.2), "Fontvieille"),
            ((EXTENT * 0.8, EXTENT * 0.8), "Larvotto"),
        ] {
            shapes.push(ShapeOrText::Text(Text::new(
                egui::pos2(at.0, at.1),
                name.to_owned(),
                12.0,
                egui::Color32::WHITE,
                0.0,
            )));
        }
        let flat = std::sync::Arc::new(crate::tile_mesh::flatten(&shapes, FEATHERING));
        assert!(
            flat.plan().is_some(),
            "fixture: the flattened tile carries a plan"
        );
        let planned = GroundMeshes {
            meshes: Some(&flat),
            painter: None,
            pass_nr: 1,
            feathering: FEATHERING,
            opacity: 1.0,
        };

        let ctx = egui::Context::default();
        let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        let piece = egui::Rect::from_min_size(egui::pos2(37.0, 61.0), egui::vec2(256.0, 256.0));

        let deferred = |ground: GroundMeshes<'_>, uv: egui::Rect| {
            let mut labels = Vec::new();
            let _ = shapes_of_one_pass(&ctx, canvas, |ui| {
                paint_vector_tile(
                    ui.painter(),
                    &shapes,
                    ground,
                    piece,
                    uv,
                    &mut labels,
                    Background::Inline,
                );
            });
            labels
                .into_iter()
                .map(|text| (text.text.to_string(), text.position))
                .collect::<Vec<_>>()
        };

        let mut kept_somewhere = 0usize;
        for uv in [
            FULL_TILE_UV,
            quarter(0.0, 0.0),
            quarter(0.5, 0.0),
            quarter(0.0, 0.5),
            quarter(0.5, 0.5),
        ] {
            let unplanned = deferred(GroundMeshes::CPU_ONLY, uv);
            kept_somewhere += unplanned.len();
            assert_eq!(
                deferred(planned, uv),
                unplanned,
                "at uv {uv:?} the planned walk deferred a different label list                  than the walk it stands in for"
            );
        }

        // A non-triviality floor: two lists that were both empty would agree
        // for the wrong reason, and this test's failure mode is a name that is
        // never deferred at all.
        assert!(
            kept_somewhere >= 6,
            "the fixture deferred only {kept_somewhere} labels across five uv \
             windows, so the agreement above is not about labels that draw"
        );
    }
}
