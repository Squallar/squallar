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

/// Draw one slippy-map tile layer through the pane's own projector.
pub(super) fn draw_tile_layer(
    ui: &egui::Ui,
    projector: &walkers::Projector,
    zoom: f64,
    tiles: &mut HttpsTiles,
    zoom_bias: u8,
) {
    // Once for the layer, before the grid loop. `HttpsTiles::at` does not drain,
    // so this is the only thing that moves finished fetches into the cache --
    // and doing it per cell would repeat it once per tile in the span below.
    tiles.pump();

    let tile_zoom = (zoom.round() as u8)
        .saturating_add(zoom_bias)
        .min(tiles.source_max_zoom());

    let span = crate::tiles::tile_span(projector, ui.max_rect(), tile_zoom);

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
                        paint_vector_tile(ui.painter(), shapes, rect, twuv.uv);
                    }
                }
            }
        }
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

/// Lay one label out, and claim the area it needs.
///
/// Returns [`egui::Shape::Noop`] when the area is already taken, which is the
/// collision rule: first label to ask for a piece of screen keeps it.
///
/// **`occupied` is per tile here, not per pane.** Labels therefore still
/// collide across a tile seam; fixing that is the label phase split, which is
/// its own change.
fn lay_out_label(
    ctx: &egui::Context,
    text: walkers::Text,
    occupied: &mut walkers::OccupiedAreas,
) -> egui::Shape {
    let mut job = egui::text::LayoutJob::default();
    job.append(
        &text.text,
        0.0,
        egui::TextFormat {
            font_id: egui::FontId::proportional(text.font_size),
            color: text.text_color,
            background: text.background_color,
            ..Default::default()
        },
    );

    let galley = ctx.fonts_mut(|fonts| fonts.layout_job(job));
    let area = walkers::text::OrientedRect::new(text.position, text.angle, galley.size());
    let top_left = area.top_left();

    if occupied.try_occupy(area) {
        egui::epaint::TextShape::new(top_left, galley, text.text_color)
            .with_angle(text.angle)
            .into()
    } else {
        egui::Shape::Noop
    }
}

/// Paint one decoded vector tile.
///
/// `shapes` are in MVT extent units over the whole tile and are shared by every
/// pane that draws this tile, so nothing here mutates them:
/// [`walkers::mvt::transformed`] returns a placed copy.
///
/// The placement is against the whole tile
/// ([`full_rect_of_clipped_tile`]) and the clip is against the piece, so an
/// ancestor stretched over a gap draws only the part that belongs to the tile
/// that was asked for.
///
/// The shapes are collected before `extend` because laying a label out takes
/// `Context::fonts_mut` while `Painter::extend` holds the graphics lock;
/// interleaving them deadlocks.
fn paint_vector_tile(
    painter: &egui::Painter,
    shapes: &[walkers::ShapeOrText],
    rect: egui::Rect,
    uv: egui::Rect,
) {
    let painter = painter.with_clip_rect(rect);
    let mut occupied = walkers::OccupiedAreas::new();

    let placed: Vec<egui::Shape> =
        walkers::mvt::transformed(shapes, full_rect_of_clipped_tile(rect, uv))
            .into_iter()
            .map(|shape_or_text| match shape_or_text {
                walkers::ShapeOrText::Shape(shape) => shape,
                walkers::ShapeOrText::Text(text) => {
                    lay_out_label(painter.ctx(), text, &mut occupied)
                }
            })
            .collect();

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
        Tile::Vector(vec![
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
        ])
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
            draw_tile_layer(ui, &projector, zoom, &mut tiles, 0);
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
        let placed_centre = label.pos + label.galley.size() / 2.0;
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

    /// **Two labels claiming the same screen: one wins.** The collision is what
    /// `OccupiedAreas` is for, and a seam that dropped it would draw legible
    /// text on top of legible text and still look plausible in a screenshot.
    #[test]
    fn overlapping_labels_collide_and_only_one_is_drawn() {
        let ctx = egui::Context::default();
        let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(256.0, 256.0));
        let whole = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));

        let at = |x: f32, name: &str| {
            ShapeOrText::Text(Text::new(
                egui::pos2(x, EXTENT / 2.0),
                name.to_owned(),
                12.0,
                egui::Color32::WHITE,
                egui::Color32::TRANSPARENT,
                0.0,
            ))
        };

        // Same point, twice.
        let shapes = shapes_of_one_pass(&ctx, canvas, |ui| {
            paint_vector_tile(
                ui.painter(),
                &[at(EXTENT / 2.0, "Monaco"), at(EXTENT / 2.0, "Monte-Carlo")],
                rect,
                whole,
            );
        });
        let drawn = shapes
            .iter()
            .filter(|c| matches!(&c.shape, egui::Shape::Text(_)))
            .count();
        assert_eq!(drawn, 1, "two labels at one point must collide to one");

        // Far apart, and both survive -- the control that stops the assertion
        // above from passing because labels are simply never drawn.
        let shapes = shapes_of_one_pass(&ctx, canvas, |ui| {
            paint_vector_tile(
                ui.painter(),
                &[
                    at(EXTENT / 8.0, "Monaco"),
                    at(EXTENT * 7.0 / 8.0, "Monte-Carlo"),
                ],
                rect,
                whole,
            );
        });
        let drawn = shapes
            .iter()
            .filter(|c| matches!(&c.shape, egui::Shape::Text(_)))
            .count();
        assert_eq!(drawn, 2, "labels a tile apart do not collide");
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
