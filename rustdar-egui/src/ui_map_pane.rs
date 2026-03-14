use crate::actions::GuiAction;
use crate::layers::LayerKind;
use crate::overlay_state::OverlayData;
use crate::pane::{PaneState, RadarImageData};
use rustdar_radar::sites::RADARS;
use rustdar_radar::types::{ImageBounds, MAX_RANGE_KM};
use walkers::HttpTiles;

use super::super::map_overlays::{OverlayDrawContext, draw_label_tiles_overlay};

/// Shared references needed for rendering a single pane's map content.
pub(super) struct PaneRenderCtx<'a> {
    pub pane: &'a mut PaneState,
    pub overlays: &'a mut OverlayData,
    pub radar_image: &'a Option<RadarImageData>,
    pub user_location: Option<(f64, f64)>,
    pub label_tiles: &'a mut Option<HttpTiles>,
    pub actions: &'a mut Vec<GuiAction>,
    pub pane_rect: egui::Rect,
    pub pointer_available: bool,
    pub is_dark_theme: bool,
    pub current_theme_is_dark: bool,
    pub scan_info_site_name: Option<&'a str>,
    pub loading_site: &'a mut Option<String>,
}

/// Render the map content for a single pane (SPC/NWS overlays, radar image,
/// city labels, radar sites, user location).
pub(super) fn render_pane_map_content(
    ui: &mut egui::Ui,
    projector: &walkers::Projector,
    zoom: f64,
    ctx: &mut PaneRenderCtx<'_>,
) {
    // --- Phase 1: immutable-ui work (overlays, radar image, labels) ---
    {
        let overlay_ctx = OverlayDrawContext::new(
            ui,
            projector,
            zoom,
            ctx.current_theme_is_dark,
            ctx.pointer_available,
        );

        // Draw SPC outlook polygons (below radar)
        overlay_ctx.draw_spc_overlays(
            &ctx.pane.layers,
            &ctx.overlays.spc_outlooks.data,
            &mut ctx.pane.spc_overlay_caches,
            &ctx.overlays.spc_data_generation,
        );

        // Overlay radar data if available
        if ctx.pane.layers.is_enabled(LayerKind::Radar) {
            if let Some(img) = ctx.radar_image {
                render_radar_overlay(ui, projector, img, ctx.pane, ctx.pane_rect);
            }
        }

        // Draw SPC Mesoscale Discussion polygons
        let clicked_md = overlay_ctx.draw_spc_discussions(
            &ctx.pane.layers,
            &ctx.overlays.spc_discussions.data,
            &mut ctx.pane.spc_md_overlay_cache,
            ctx.overlays.spc_discussions.data_generation,
        );
        if let Some(idx) = clicked_md {
            ctx.overlays.selected_md = Some(idx);
        }

        // Draw NWS alert polygons
        let clicked_alert = overlay_ctx.draw_nws_alerts(
            &ctx.pane.layers,
            &ctx.overlays.nws_alerts.data,
            &ctx.overlays.hidden_alerts,
            &mut ctx.pane.nws_overlay_cache,
            ctx.overlays.nws_alerts.data_generation,
        );
        if let Some(idx) = clicked_alert {
            ctx.overlays.selected_alert = Some(idx);
        }

        // Draw label-only tiles on top of the radar overlay
        if ctx.pane.layers.is_enabled(LayerKind::CityLabels) {
            if let Some(ltiles) = ctx.label_tiles.as_mut() {
                draw_label_tiles_overlay(ui, projector, zoom, ltiles);
            }
        }
    }
    // overlay_ctx (and its shared borrow of ui) is dropped here

    // --- Phase 2: mutable-ui work (radar sites need allocate_rect) ---
    if ctx.pane.layers.is_enabled(LayerKind::RadarSites) {
        render_radar_sites(
            ui,
            projector,
            zoom,
            ctx.is_dark_theme,
            ctx.scan_info_site_name,
            ctx.loading_site,
            ctx.actions,
        );
    }

    // Draw user location indicator (blue dot)
    if let Some((user_lat, user_lon)) = ctx.user_location {
        render_user_location(ui, projector, user_lat, user_lon);
    }
}

/// Render the radar image overlay, range ring, and hover tooltip.
fn render_radar_overlay(
    ui: &egui::Ui,
    projector: &walkers::Projector,
    img: &RadarImageData,
    pane: &mut PaneState,
    pane_rect: egui::Rect,
) {
    let bounds = pane
        .cached_image_bounds
        .unwrap_or_else(|| ImageBounds::from_radar_site(img.lat, img.lon));

    let nw = projector
        .project(walkers::lat_lon(bounds.max_lat, bounds.min_lon))
        .to_pos2();
    let se = projector
        .project(walkers::lat_lon(bounds.min_lat, bounds.max_lon))
        .to_pos2();
    let rect = egui::Rect::from_two_pos(nw, se);

    // Hover: only compute for the pane the cursor is in
    if let Some(hover_pos) = ui.ctx().pointer_hover_pos() {
        if pane_rect.contains(hover_pos) {
            let pos_changed = pane
                .last_hover_pos
                .map(|last| (last - hover_pos).length() > 0.5)
                .unwrap_or(true);
            pane.last_hover_pos = Some(hover_pos);

            if pos_changed {
                let screen_vec = egui::vec2(hover_pos.x, hover_pos.y);
                let map_pos = projector.unproject(screen_vec);
                let hover_lat = map_pos.y();
                let hover_lon = map_pos.x();

                pane.hover_value = Some(super::compute_hover_info(
                    img,
                    hover_lat,
                    hover_lon,
                    hover_pos,
                    rect,
                    pane.selected_product,
                ));
            }
        } else {
            // Cursor not in this pane
            pane.last_hover_pos = None;
            pane.hover_value = None;
        }
    } else {
        pane.last_hover_pos = None;
        pane.hover_value = None;
    }

    // Draw the radar image overlay
    ui.painter().image(
        img.texture.id(),
        rect,
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        egui::Color32::WHITE,
    );

    // Draw a light grey circle showing the radar range
    let radar_center = projector
        .project(walkers::lat_lon(img.lat, img.lon))
        .to_pos2();
    let north_edge = projector
        .project(walkers::lat_lon(
            img.lat + MAX_RANGE_KM / 111.32,
            img.lon,
        ))
        .to_pos2();
    let range_radius_pixels = (radar_center.y - north_edge.y).abs();
    ui.painter().circle_stroke(
        radar_center,
        range_radius_pixels,
        egui::Stroke::new(
            1.0,
            egui::Color32::from_rgba_unmultiplied(150, 150, 150, 80),
        ),
    );
}

/// Draw NEXRAD radar site icons on the map.
fn render_radar_sites(
    ui: &mut egui::Ui,
    projector: &walkers::Projector,
    zoom: f64,
    is_dark_theme: bool,
    scan_info_site_name: Option<&str>,
    loading_site: &mut Option<String>,
    actions: &mut Vec<GuiAction>,
) {
    let screen_rect = ui.max_rect();
    let zoom_f32 = zoom as f32;
    let icon_size = (10.0 + zoom_f32 * 2.0).clamp(8.0, 24.0);
    let font_size = (icon_size * 0.6).clamp(8.0, 12.0);

    for radar_site in &RADARS {
        let site_screen = projector
            .project(walkers::lat_lon(radar_site.lat, radar_site.lon))
            .to_pos2();

        if !screen_rect.expand(100.0).contains(site_screen) {
            continue;
        }

        let is_current_site = scan_info_site_name == Some(radar_site.name);
        let is_loading = loading_site
            .as_ref()
            .map(|s| s == radar_site.name)
            .unwrap_or(false);

        let icon_color = if is_loading {
            egui::Color32::from_rgb(160, 32, 240)
        } else if is_current_site {
            egui::Color32::from_rgb(255, 100, 100)
        } else {
            egui::Color32::from_rgb(100, 150, 255)
        };

        let icon_rect =
            egui::Rect::from_center_size(site_screen, egui::vec2(icon_size, icon_size));

        let response = ui.allocate_rect(icon_rect, egui::Sense::click());

        if response.clicked() {
            *loading_site = Some(radar_site.name.to_string());
            actions.push(GuiAction::SwitchRadarSite(radar_site.name.to_string()));
        }

        ui.painter()
            .circle_filled(site_screen, icon_size / 2.0, icon_color);
        ui.painter().circle_stroke(
            site_screen,
            icon_size / 2.0,
            egui::Stroke::new(1.5, egui::Color32::WHITE),
        );

        let text_color = if is_dark_theme {
            egui::Color32::WHITE
        } else {
            egui::Color32::BLACK
        };

        let text_pos = egui::pos2(site_screen.x, site_screen.y + icon_size / 2.0 + 3.0);

        ui.painter().text(
            text_pos,
            egui::Align2::CENTER_TOP,
            radar_site.name,
            egui::FontId::monospace(font_size),
            text_color,
        );

        if response.hovered() {
            let elev_str = match radar_site.elev {
                Some(e) => format!("{} ft", e),
                None => "N/A".to_string(),
            };
            let tooltip_text = format!(
                "{}\nLat: {:.3}°, Lon: {:.3}°\nElev: {}",
                radar_site.name, radar_site.lat, radar_site.lon, elev_str
            );
            response.on_hover_text(tooltip_text);
        }
    }
}

/// Draw user location blue dot indicator.
fn render_user_location(
    ui: &egui::Ui,
    projector: &walkers::Projector,
    user_lat: f64,
    user_lon: f64,
) {
    let user_screen = projector
        .project(walkers::lat_lon(user_lat, user_lon))
        .to_pos2();

    let screen_rect = ui.max_rect();
    if screen_rect.expand(50.0).contains(user_screen) {
        ui.painter().circle_filled(
            user_screen,
            14.0,
            egui::Color32::from_rgba_unmultiplied(30, 130, 255, 40),
        );
        ui.painter().circle_stroke(
            user_screen,
            7.0,
            egui::Stroke::new(2.5, egui::Color32::WHITE),
        );
        ui.painter().circle_filled(
            user_screen,
            7.0,
            egui::Color32::from_rgb(30, 130, 255),
        );
    }
}
