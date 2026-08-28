use crate::overlay_cache::{OverlayTextureData, draw_overlay_texture, geo_point_in_feature};
use crate::tile_source::HttpsTiles;
use squallar_overlays::render::overlay_state::{ClickableItem, OverlayItem};
use squallar_overlays::types::OverlayLabel;
use std::sync::Arc;
use walkers::{Tile, TileId, Tiles};

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

/// Draw one slippy-map tile layer through the pane's own projector, and hand
/// back the labels it deferred.
///
/// **The ground is painted here; the labels are not.** A vector tile carries
/// both, but they belong at different heights in the pane: the ground is the
/// bottom of the stack and the place names draw at the `CityLabels` layer's
/// position, above the weather. The caller paints them with [`paint_labels`]
/// when that layer's turn comes, and drops them when it is switched off.
pub(super) fn draw_tile_layer(
    ui: &egui::Ui,
    projector: &walkers::Projector,
    zoom: f64,
    tiles: &mut HttpsTiles,
    zoom_bias: u8,
) -> Vec<walkers::Text> {
    // Once for the layer, before the grid loop. `HttpsTiles::at` does not drain,
    // so this is the only thing that moves finished fetches into the cache --
    // and doing it per cell would repeat it once per tile in the span below.
    tiles.pump();

    // An archive source has not read its header yet on the first frames, so it
    // cannot say how deep it goes. Clamping to a stand-in number is what used to
    // seed `0/0/0` into the LRU and leave it there as the session's fallback
    // ancestor -- see `tile_source::MAX_ZOOM_UNKNOWN`. Drawing nothing for a
    // frame is the whole cost of not doing that; the IO task repaints when the
    // header lands.
    let Some(source_max_zoom) = tiles.source_max_zoom() else {
        return Vec::new();
    };

    let tile_zoom = (zoom.round() as u8)
        .saturating_add(zoom_bias)
        .min(source_max_zoom);

    let span = crate::tiles::tile_span(projector, ui.max_rect(), tile_zoom);

    // Accumulated across every cell below, so the collision test the caller
    // makes is one test against the whole pane; see [`paint_labels`].
    let mut labels: Vec<walkers::Text> = Vec::new();

    for ty in span.north..=span.south {
        for tx in span.west..=span.east {
            let tile_id = TileId {
                x: tx,
                y: ty,
                zoom: tile_zoom,
            };

            if let Some(twuv) = tiles.at(tile_id) {
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
                        paint_vector_tile(ui.painter(), shapes, rect, twuv.uv, &mut labels);
                    }
                }
            }
        }
    }

    labels
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

/// Lay one label out, and claim the area it needs.
///
/// Returns [`egui::Shape::Noop`] when the area is already taken, which is the
/// collision rule: first label to ask for a piece of screen keeps it.
///
/// The caller owns `occupied`, and [`paint_labels`] owns the only one there is
/// per pane.
///
/// The wrapping and the halo are [`walkers::Text`]'s own, so this phase and
/// walkers' per-tile draw cannot drift apart.
fn lay_out_label(
    ctx: &egui::Context,
    text: &walkers::Text,
    occupied: &mut walkers::OccupiedAreas,
) -> egui::Shape {
    let galley = text.galley(ctx);
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
    rect: egui::Rect,
    uv: egui::Rect,
    labels: &mut Vec<walkers::Text>,
) {
    let painter = painter.with_clip_rect(rect);

    let placement = walkers::mvt::placement(full_rect_of_clipped_tile(rect, uv));

    let placed: Vec<egui::Shape> = shapes
        .iter()
        .filter_map(|shape| match shape.placed(placement) {
            walkers::ShapeOrText::Shape(shape) => Some(shape),
            walkers::ShapeOrText::Text(text) => {
                if rect.contains(text.position) {
                    labels.push(text);
                }
                None
            }
        })
        .collect();

    painter.extend(placed);
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
pub(super) fn paint_labels(painter: &egui::Painter, labels: Vec<walkers::Text>) {
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

        let shape = lay_out_label(painter.ctx(), &text, &mut occupied);

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
        draw_tile_layer(&ui, &projector, zoom, &mut tiles, 0);
        let drains = tiles.pumps() - before;
        let _ = ctx.end_pass();

        assert_eq!(
            drains, 1,
            "a layer of {cells} cells drained {drains} times; one layer is one \
             drain, and {cells} is what a per-cell drain would have cost"
        );
    }

    // -----------------------------------------------------------------------
    // The vector draw seam
    //
    // These run in a DEFAULT `cargo test --workspace`, on purpose.
    // `walkers/mvt` is on unconditionally, so `Tile::Vector` exists on every
    // build; `basemap-vector` gates the archive that produces one, not the
    // renderer that draws it. A seam test behind the feature would be invisible
    // to the workspace suite, which is exactly how the archive reader landed
    // with 13 tests and moved the total by zero.
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
                egui::Color32::TRANSPARENT,
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
            let labels = draw_tile_layer(ui, &projector, zoom, &mut tiles, 0);
            // The pane's `CityLabels` arm, which is where the deferred labels
            // are painted; without it this pass draws ground and no names.
            paint_labels(ui.painter(), labels);
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
            paint_vector_tile(ui.painter(), shapes, *rect, whole, &mut labels);
        }
        paint_labels(ui.painter(), labels);
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
            egui::Color32::TRANSPARENT,
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

        let context = walkers::Context::new("LineString".to_owned(), Default::default(), 14);
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
        Text::new(
            at,
            name.to_owned(),
            12.0,
            egui::Color32::WHITE,
            egui::Color32::TRANSPARENT,
            0.0,
        )
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
            paint_labels(ui.painter(), distinct);
        });
        assert_eq!(
            text_shapes(&shapes).len(),
            4,
            "fixture: these four anchors must not collide, or the assertion \
             below is about the overlap rule instead of the repeat rule"
        );

        let crowded: Vec<Text> = anchors.iter().map(|at| label("Rio Grande", *at)).collect();
        let shapes = shapes_of_one_pass(&ctx, canvas, |ui| {
            paint_labels(ui.painter(), crowded);
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
            paint_labels(ui.painter(), spread);
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

    /// **The halo is the glyphs redrawn around themselves, not a filled box.**
    ///
    /// What shipped before was the halo colour at half alpha in
    /// `TextFormat::background`, which egui paints across the galley's whole
    /// bounding rectangle -- the grey slab behind every river name.
    #[test]
    fn a_haloed_label_draws_glyphs_around_itself_and_no_box() {
        let ctx = egui::Context::default();
        let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));

        let haloed = Text::new(
            egui::pos2(400.0, 300.0),
            "Washita River".to_owned(),
            12.0,
            egui::Color32::WHITE,
            egui::Color32::BLACK,
            0.0,
        )
        .with_halo_width(1.0);

        let shapes = shapes_of_one_pass(&ctx, canvas, |ui| {
            paint_labels(ui.painter(), vec![haloed]);
        });

        let texts = text_shapes(&shapes);
        assert_eq!(
            texts.len(),
            9,
            "a haloed label is eight offset draws plus the glyphs themselves"
        );
        assert_eq!(
            texts
                .iter()
                .filter(|t| t.fallback_color == egui::Color32::WHITE)
                .count(),
            1,
            "exactly one draw is the text; the rest are halo"
        );

        // The halo draws are displaced from the text and by the styled width.
        let centre = texts
            .iter()
            .find(|t| t.fallback_color == egui::Color32::WHITE)
            .expect("the glyph draw")
            .pos;
        for halo in texts
            .iter()
            .filter(|t| t.fallback_color == egui::Color32::BLACK)
        {
            let offset = (halo.pos - centre).length();
            assert!(
                (offset - 1.0).abs() < 0.01,
                "a halo draw sits {offset} pt out, not the 1 pt the style asked for"
            );
        }

        // NO BOX. The old spelling put the halo colour in `TextFormat`, which
        // shows up as a filled rect behind the galley.
        assert!(
            !shapes
                .iter()
                .any(|c| matches!(&c.shape, egui::Shape::Rect(_))),
            "a background rectangle is still being painted behind the label"
        );

        // The control: with no halo width, one draw and nothing else -- so the
        // count above is a property of the halo and not of the painter.
        let bare = Text::new(
            egui::pos2(400.0, 300.0),
            "Washita River".to_owned(),
            12.0,
            egui::Color32::WHITE,
            egui::Color32::BLACK,
            0.0,
        );
        let shapes = shapes_of_one_pass(&ctx, canvas, |ui| {
            paint_labels(ui.painter(), vec![bare]);
        });
        assert_eq!(
            text_shapes(&shapes).len(),
            1,
            "a label whose style asks for no halo width still drew one"
        );
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
}
