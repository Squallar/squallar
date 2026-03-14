use std::collections::HashSet;
use walkers::{HttpTiles, Texture, TileId, Tiles};
use rustdar_overlays::render::layers::LayerKind;
use crate::overlay_cache::{
    OverlayTextureCache, draw_overlay_texture, geo_point_in_feature,
};
use rustdar_overlays::spc::discussion::SpcDiscussion;
use rustdar_overlays::spc::colors::md_stroke_color;
use rustdar_overlays::nws::alert::NwsAlert;
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

    /// Unproject a screen click position to geographic coordinates and test
    /// whether it falls inside any feature polygon.
    fn _clicked_feature_geo(
        &self,
        features: &[rustdar_overlays::types::OverlayFeature],
    ) -> Option<usize> {
        if !self.pointer_available || self.click_on_ui {
            return None;
        }
        let click_pos = self.overlay_click_pos?;
        let geo = self.projector.unproject(egui::vec2(click_pos.x, click_pos.y));
        let lat = geo.y();
        let lon = geo.x();

        for (idx, feature) in features.iter().enumerate() {
            if geo_point_in_feature(lat, lon, feature) {
                return Some(idx);
            }
        }
        None
    }

    /// Draw SPC convective outlook overlays (texture-based).
    pub fn draw_spc_overlays(
        &self,
        _layers: &rustdar_overlays::render::layers::LayerManager,
        spc_texture: &OverlayTextureCache,
    ) {
        if let Some(ref tex) = spc_texture.current {
            draw_overlay_texture(self.ui.painter(), self.projector, tex, self.screen_rect);
        }
    }

    /// Draw SPC Mesoscale Discussion overlays (texture + labels + click).
    ///
    /// Returns indices of all discussions whose polygons contain the click point.
    pub fn draw_spc_discussions(
        &self,
        layers: &rustdar_overlays::render::layers::LayerManager,
        discussions: &[SpcDiscussion],
        md_texture: &OverlayTextureCache,
    ) -> Vec<usize> {
        if !layers.is_enabled(LayerKind::SpcMesoscaleDiscussions) || discussions.is_empty() {
            return Vec::new();
        }

        // Draw texture
        if let Some(ref tex) = md_texture.current {
            draw_overlay_texture(self.ui.painter(), self.projector, tex, self.screen_rect);
        }

        // MD labels: still draw as egui text (cheap)
        let painter = self.ui.painter();
        for md in discussions {
            if md.polygon.is_empty() {
                continue;
            }
            // Compute centroid from first ring
            let ring = &md.polygon[0];
            if ring.is_empty() {
                continue;
            }
            let n = ring.len() as f64;
            let cx: f64 = ring.iter().map(|&(_, lon)| lon).sum::<f64>() / n;
            let cy: f64 = ring.iter().map(|&(lat, _)| lat).sum::<f64>() / n;
            let screen_pos = self.projector.project(walkers::lat_lon(cy, cx)).to_pos2();
            if self.screen_rect.contains(screen_pos) {
                let [sr, sg, sb, sa] = md_stroke_color(&md.md_type);
                let color = egui::Color32::from_rgba_unmultiplied(sr, sg, sb, sa);
                painter.text(
                    screen_pos,
                    egui::Align2::CENTER_CENTER,
                    format!("MD {}", md.number),
                    egui::FontId::proportional(11.0),
                    color,
                );
            }
        }

        // Click detection in geo-coordinates
        if !self.pointer_available || self.click_on_ui {
            return Vec::new();
        }
        let Some(click_pos) = self.overlay_click_pos else { return Vec::new() };
        let geo = self.projector.unproject(egui::vec2(click_pos.x, click_pos.y));
        let lat = geo.y();
        let lon = geo.x();
        let mut hits = Vec::new();
        for (idx, md) in discussions.iter().enumerate() {
            for ring in &md.polygon {
                if ring.len() < 3 {
                    continue;
                }
                let ring_sp: Vec<rustdar_overlays::types::ScreenPoint> = ring
                    .iter()
                    .map(|&(rlat, rlon)| {
                        rustdar_overlays::types::ScreenPoint::new(
                            rlon as f32,
                            lat_rad_to_mercator_y(rlat.to_radians()) as f32,
                        )
                    })
                    .collect();
                let point = rustdar_overlays::types::ScreenPoint::new(
                    lon as f32,
                    lat_rad_to_mercator_y(lat.to_radians()) as f32,
                );
                if rustdar_overlays::render::geo::point_in_polygon(point, &ring_sp) {
                    hits.push(idx);
                    break; // one hit per MD is enough
                }
            }
        }
        hits
    }

    /// Draw NWS weather alert overlays (texture-based).
    ///
    /// Returns indices of all alerts whose polygons contain the click point.
    pub fn draw_nws_alerts(
        &self,
        layers: &rustdar_overlays::render::layers::LayerManager,
        nws_alerts: &[NwsAlert],
        hidden_alerts: &HashSet<String>,
        alert_texture: &OverlayTextureCache,
    ) -> Vec<usize> {
        if !layers.any_nws_enabled() || nws_alerts.is_empty() {
            return Vec::new();
        }

        // Draw texture
        if let Some(ref tex) = alert_texture.current {
            draw_overlay_texture(self.ui.painter(), self.projector, tex, self.screen_rect);
        }

        // Click detection in geo-coordinates
        if !self.pointer_available || self.click_on_ui {
            return Vec::new();
        }
        let Some(click_pos) = self.overlay_click_pos else { return Vec::new() };
        let geo = self.projector.unproject(egui::vec2(click_pos.x, click_pos.y));
        let lat = geo.y();
        let lon = geo.x();

        let enabled_categories = layers.enabled_nws_categories();
        let mut hits = Vec::new();
        for (alert_idx, alert) in nws_alerts.iter().enumerate() {
            if !enabled_categories.contains(&alert.category)
                || hidden_alerts.contains(&alert.id)
            {
                continue;
            }
            for feature in &alert.features {
                if geo_point_in_feature(lat, lon, feature) {
                    hits.push(alert_idx);
                    break; // one hit per alert is enough
                }
            }
        }
        hits
    }
}

/// Convert latitude (radians) to Web Mercator Y (for geo click detection).
#[inline]
fn lat_rad_to_mercator_y(lat_rad: f64) -> f64 {
    (std::f64::consts::PI / 4.0 + lat_rad / 2.0).tan().ln()
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
