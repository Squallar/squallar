use crate::overlay_cache::{OverlayTextureData, draw_overlay_texture, geo_point_in_feature};
use crate::tile_source::HttpsTiles;
use squallar_geo::{lat_to_tile_y, lon_to_tile_x, tile_to_lat, tile_to_lon};
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
    let tile_zoom = (zoom.round() as u8)
        .saturating_add(zoom_bias)
        .min(tiles.source_max_zoom());
    let n = 2u32.pow(tile_zoom as u32);
    if n == 0 {
        return;
    }

    let screen_rect = ui.max_rect();

    let nw = projector.unproject(egui::vec2(screen_rect.left(), screen_rect.top()));
    let se = projector.unproject(egui::vec2(screen_rect.right(), screen_rect.bottom()));

    // walkers Position: x = longitude, y = latitude
    let min_lon = nw.x().min(se.x());
    let max_lon = nw.x().max(se.x());
    let max_lat = nw.y().max(se.y());
    let min_lat = nw.y().min(se.y());

    let min_tx = lon_to_tile_x(min_lon, tile_zoom);
    let max_tx = (lon_to_tile_x(max_lon, tile_zoom) + 1).min(n - 1);
    let min_ty = lat_to_tile_y(max_lat, tile_zoom); // higher lat → smaller tile y
    let max_ty = (lat_to_tile_y(min_lat, tile_zoom) + 1).min(n - 1);

    for ty in min_ty..=max_ty {
        for tx in min_tx..=max_tx {
            let tile_id = TileId {
                x: tx,
                y: ty,
                zoom: tile_zoom,
            };

            if let Some(twuv) = tiles.at(tile_id) {
                let nw_lon = tile_to_lon(tx, tile_zoom);
                let nw_lat = tile_to_lat(ty, tile_zoom);
                let se_lon = tile_to_lon(tx + 1, tile_zoom);
                let se_lat = tile_to_lat(ty + 1, tile_zoom);

                let rect = crate::overlay_cache::geo_corner_rect(
                    projector,
                    (nw_lat, nw_lon),
                    (se_lat, se_lon),
                );

                match twuv.tile {
                    Tile::Raster(ref tex) => {
                        ui.painter()
                            .image(tex.id(), rect, twuv.uv, egui::Color32::WHITE);
                    }
                    // Unreachable today. `tile_source` fetches PNGs, so every
                    // tile it yields decodes as a raster; this arm exists only
                    // because enabling `walkers/mvt` put `Tile::Vector` on the
                    // enum.
                    //
                    // It is deliberately not `=> {}`. An empty arm here is the
                    // silent-partial-success shape: the pane would under-draw
                    // with nothing in the log and nothing on the glass, and a
                    // green board would keep hiding it for as long as it took
                    // somebody to notice a missing tile by eye. So this is loud
                    // three ways -- a marker the eye cannot miss, an error in
                    // the log, and a tripped assertion in tests and dev builds.
                    //
                    // `debug_assert!` and not `assert!`, because "loud" must
                    // stop short of panicking in front of a user: in release
                    // the marker and the log carry it. Magenta because that is
                    // already this stack's did-not-render colour -- walkers
                    // paints it from `style::Color::evaluate` when a colour
                    // expression fails.
                    Tile::Vector(_) => {
                        debug_assert!(
                            false,
                            "overlay tile source produced a vector tile at {tile_id:?}; \
                             this pane draws rasters only"
                        );
                        log::error!(
                            "overlay tile source produced a vector tile at {tile_id:?}; \
                             this pane draws rasters only, so that tile is not drawn"
                        );
                        ui.painter().rect_filled(rect, 0.0, egui::Color32::MAGENTA);
                    }
                }
            }
        }
    }
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
