use walkers::{HttpTiles, Texture, TileId, Tiles};
use crate::overlay_cache::{
    OverlayTextureCache, draw_overlay_texture, geo_point_in_feature,
};
use rustdar_overlays::render::overlay_state::{ClickableItem, SelectedOverlay};
use crate::tiles::{lon_to_tile_x, lat_to_tile_y, tile_to_lon, tile_to_lat};

// ---------------------------------------------------------------------------
/// Shared context for overlay drawing operations.
///
/// Bundles the common parameters (UI handle, map projector, click detection
/// state) that every overlay drawing function needs.
pub(super) struct OverlayDrawContext<'a> {
    ui: &'a egui::Ui,
    projector: &'a walkers::Projector,
    screen_rect: egui::Rect,
    // Pre-computed click state (shared by discussion + alert drawing).
    overlay_click_pos: Option<egui::Pos2>,
    click_on_ui: bool,
    pointer_available: bool,
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
        let click_on_ui = overlay_click_pos.is_some_and(|p| {
            !pane_rect.contains(p)
                || excluded_rects.iter().any(|r| r.contains(p))
                || ui.ctx()
                    .layer_id_at(p)
                    .is_some_and(|l| l.order > egui::Order::Background)
        });

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
    /// This is fully generic — the caller provides the texture cache and the
    /// pre-built `ClickableItem` list from `OverlayKind::clickable_items()`.
    /// Returns `SelectedOverlay` IDs for all items whose polygons contain the
    /// click point.
    pub fn draw_overlay(
        &self,
        texture: Option<&OverlayTextureCache>,
        items: &[ClickableItem<'_>],
    ) -> Vec<SelectedOverlay> {
        // 1. Draw the pre-rasterized texture if available
        if let Some(ref tex) = texture.and_then(|c| c.current.as_ref()) {
            draw_overlay_texture(self.ui.painter(), self.projector, tex, self.screen_rect);
        }

        // 2. Draw map labels
        let painter = self.ui.painter();
        for item in items {
            if let Some(ref label) = item.label {
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
        }

        // 3. Click hit-testing
        if !self.pointer_available || self.click_on_ui {
            return Vec::new();
        }
        let Some(click_pos) = self.overlay_click_pos else {
            return Vec::new();
        };

        // If a hit buffer is available, use it for pixel-perfect detection.
        if let Some(ref tex) = texture.and_then(|c| c.current.as_ref()) {
            if let Some(ref hit_map) = tex.hit_map {
                let rect = crate::overlay_cache::overlay_texture_rect(self.projector, tex, self.screen_rect);
                if rect.width() > 0.0 && rect.height() > 0.0 {
                    let u = (click_pos.x - rect.left()) / rect.width();
                    let v = (click_pos.y - rect.top()) / rect.height();
                    return hit_map.hit_test(u, v).into_iter().cloned().collect();
                }
            }
        }

        // Fall back to geographic polygon containment.
        let geo = self
            .projector
            .unproject(egui::vec2(click_pos.x, click_pos.y));
        let lat = geo.y();
        let lon = geo.x();

        let mut hits = Vec::new();
        for item in items {
            let hit = item.features.iter().any(|f| {
                geo_point_in_feature(lat, lon, f)
            });
            if hit {
                hits.push(item.id.clone());
            }
        }
        hits
    }
}

/// Draw label-only map tiles on top of the radar overlay.
///
/// Uses the same slippy-map tile grid that walkers uses internally so the
/// labels align pixel-perfectly with the base map. Only tiles that intersect
/// the current viewport are fetched / drawn.
pub(super) fn draw_label_tiles_overlay(
    ui: &egui::Ui,
    projector: &walkers::Projector,
    zoom: f64,
    tiles: &mut HttpTiles,
) {
    let tile_zoom = zoom.round() as u8;
    let n = 2u32.pow(tile_zoom as u32);
    if n == 0 {
        return;
    }

    let screen_rect = ui.max_rect();

    // Determine the visible geographic bounds by unprojecting screen corners.
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
                // Tile geographic corners
                let nw_lon = tile_to_lon(tx, tile_zoom);
                let nw_lat = tile_to_lat(ty, tile_zoom);
                let se_lon = tile_to_lon(tx + 1, tile_zoom);
                let se_lat = tile_to_lat(ty + 1, tile_zoom);

                let nw_screen = projector
                    .project(walkers::lat_lon(nw_lat, nw_lon))
                    .to_pos2();
                let se_screen = projector
                    .project(walkers::lat_lon(se_lat, se_lon))
                    .to_pos2();
                let rect = egui::Rect::from_two_pos(nw_screen, se_screen);

                let Texture::Raster(ref tex) = twuv.texture;
                ui.painter().image(tex.id(), rect, twuv.uv, egui::Color32::WHITE);
            }
        }
    }
}
