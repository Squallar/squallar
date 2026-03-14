use crate::actions::{GuiAction, OverlayRenderKind};
use crate::overlay_cache::{
    viewport_geo_bounds, current_quantized_zoom, OVERDRAW_FRACTION,
};
use rustdar_overlays::render::layers::LayerKind;
use rustdar_overlays::render::overlay_state::OverlayData;
use crate::pane::{PaneState, RadarImageData};
use rustdar_radar::sites::RADARS;
use rustdar_radar::types::{ImageBounds, MAX_RANGE_KM};
use walkers::HttpTiles;

use super::super::map_overlays::{OverlayDrawContext, draw_label_tiles_overlay};

/// Shared references needed for rendering a single pane's map content.
pub(super) struct PaneRenderCtx<'a> {
    pub pane_idx: usize,
    pub pane: &'a mut PaneState,
    pub overlays: &'a mut OverlayData,
    pub radar_image: &'a Option<RadarImageData>,
    pub user_location: Option<(f64, f64)>,
    pub label_tiles: &'a mut Option<HttpTiles>,
    pub actions: &'a mut Vec<GuiAction>,
    pub pane_rect: egui::Rect,
    pub pointer_available: bool,
    pub is_dark_theme: bool,
    pub scan_info_site_name: Option<&'a str>,
    pub loading_site: &'a mut Option<String>,
    pub excluded_rects: Vec<egui::Rect>,
    pub is_zoom_dragging: bool,
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
            ctx.pointer_available,
            ctx.pane_rect,
            &ctx.excluded_rects,
            ctx.is_zoom_dragging,
        );

        // Draw SPC outlook textures (below radar)
        overlay_ctx.draw_spc_overlays(
            &ctx.pane.layers,
            &ctx.pane.spc_overlay_texture,
        );

        // Overlay radar data if available
        if ctx.pane.layers.is_enabled(LayerKind::Radar) {
            if let Some(img) = ctx.radar_image {
                render_radar_overlay(ui, projector, img, ctx.pane, ctx.pane_rect);
            }
        }

        // Draw SPC Mesoscale Discussion textures + labels
        let clicked_md = overlay_ctx.draw_spc_discussions(
            &ctx.pane.layers,
            &ctx.overlays.spc_discussions.data,
            &ctx.pane.spc_md_texture,
        );
        if let Some(idx) = clicked_md {
            ctx.overlays.selected_md = Some(idx);
        }

        // Draw NWS alert textures
        let clicked_alert = overlay_ctx.draw_nws_alerts(
            &ctx.pane.layers,
            &ctx.overlays.nws_alerts.data,
            &ctx.overlays.hidden_alerts,
            &ctx.pane.nws_alert_texture,
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

        // --- Check if any overlays need background re-rendering ---
        let screen_rect = ui.max_rect();
        let viewport_bounds = viewport_geo_bounds(projector, screen_rect);
        let qzoom = current_quantized_zoom(zoom);
        // Compute render dimensions with overdraw
        let w = (screen_rect.width() * (1.0 + 2.0 * OVERDRAW_FRACTION)) as u32;
        let h = (screen_rect.height() * (1.0 + 2.0 * OVERDRAW_FRACTION)) as u32;

        // SPC outlooks
        {
            let any_spc_enabled = ctx.pane.layers.spc_layers_for_day()
                .iter()
                .any(|lk| ctx.pane.layers.is_enabled(*lk));
            let data_gen = ctx.overlays.combined_spc_data_generation();
            if any_spc_enabled
                && !ctx.pane.spc_overlay_texture.render_in_flight
                && ctx.pane.spc_overlay_texture.needs_rerender(data_gen, qzoom, &viewport_bounds)
            {
                ctx.actions.push(GuiAction::RenderOverlay {
                    pane_idx: ctx.pane_idx,
                    overlay_kind: OverlayRenderKind::SpcOutlook,
                    geo_bounds: viewport_bounds.clone(),
                    width: w,
                    height: h,
                    data_generation: data_gen,
                    zoom: qzoom,
                });
            }
        }

        // SPC Mesoscale Discussions
        if ctx.pane.layers.is_enabled(LayerKind::SpcMesoscaleDiscussions)
            && !ctx.overlays.spc_discussions.data.is_empty()
            && !ctx.pane.spc_md_texture.render_in_flight
            && ctx.pane.spc_md_texture.needs_rerender(
                ctx.overlays.spc_discussions.data_generation,
                qzoom,
                &viewport_bounds,
            )
        {
            ctx.actions.push(GuiAction::RenderOverlay {
                pane_idx: ctx.pane_idx,
                overlay_kind: OverlayRenderKind::SpcDiscussions,
                geo_bounds: viewport_bounds.clone(),
                width: w,
                height: h,
                data_generation: ctx.overlays.spc_discussions.data_generation,
                zoom: qzoom,
            });
        }

        // NWS alerts
        if ctx.pane.layers.any_nws_enabled()
            && !ctx.overlays.nws_alerts.data.is_empty()
            && !ctx.pane.nws_alert_texture.render_in_flight
            && ctx.pane.nws_alert_texture.needs_rerender(
                ctx.overlays.nws_alerts.data_generation,
                qzoom,
                &viewport_bounds,
            )
        {
            ctx.actions.push(GuiAction::RenderOverlay {
                pane_idx: ctx.pane_idx,
                overlay_kind: OverlayRenderKind::NwsAlerts,
                geo_bounds: viewport_bounds,
                width: w,
                height: h,
                data_generation: ctx.overlays.nws_alerts.data_generation,
                zoom: qzoom,
            });
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

/// Update the hover value for a pane based on cursor position over the radar image.
///
/// Only recomputes when the cursor moves more than 0.5px from the last position.
/// Clears hover state when the cursor leaves the pane.
fn update_pane_hover_value(
    ui: &egui::Ui,
    projector: &walkers::Projector,
    img: &RadarImageData,
    pane: &mut PaneState,
    pane_rect: egui::Rect,
    image_rect: egui::Rect,
) {
    let Some(hover_pos) = ui.ctx().pointer_hover_pos() else {
        pane.last_hover_pos = None;
        pane.hover_value = None;
        return;
    };

    if !pane_rect.contains(hover_pos) {
        pane.last_hover_pos = None;
        pane.hover_value = None;
        return;
    }

    let pos_changed = pane
        .last_hover_pos
        .map(|last| (last - hover_pos).length() > 0.5)
        .unwrap_or(true);
    pane.last_hover_pos = Some(hover_pos);

    if pos_changed {
        let screen_vec = egui::vec2(hover_pos.x, hover_pos.y);
        let map_pos = projector.unproject(screen_vec);

        pane.hover_value = Some(super::compute_hover_info(
            img,
            map_pos.y(),
            map_pos.x(),
            hover_pos,
            image_rect,
            pane.selected_product,
        ));
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

    update_pane_hover_value(ui, projector, img, pane, pane_rect, rect);

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
            .is_some_and(|s| s == radar_site.name);

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

        draw_site_marker(ui, site_screen, icon_size, icon_color, radar_site.name, font_size, is_dark_theme);

        if response.hovered() {
            show_site_tooltip(response, radar_site);
        }
    }
}

/// Draw a single radar site marker (filled circle with outline and label).
fn draw_site_marker(
    ui: &egui::Ui,
    center: egui::Pos2,
    icon_size: f32,
    color: egui::Color32,
    name: &str,
    font_size: f32,
    is_dark_theme: bool,
) {
    ui.painter()
        .circle_filled(center, icon_size / 2.0, color);
    ui.painter().circle_stroke(
        center,
        icon_size / 2.0,
        egui::Stroke::new(1.5, egui::Color32::WHITE),
    );

    let text_color = if is_dark_theme {
        egui::Color32::WHITE
    } else {
        egui::Color32::BLACK
    };
    let text_pos = egui::pos2(center.x, center.y + icon_size / 2.0 + 3.0);
    ui.painter().text(
        text_pos,
        egui::Align2::CENTER_TOP,
        name,
        egui::FontId::monospace(font_size),
        text_color,
    );
}

/// Show a tooltip with site coordinates and elevation.
fn show_site_tooltip(response: egui::Response, site: &rustdar_radar::sites::RadarSite) {
    let elev_str = match site.elev {
        Some(e) => format!("{} ft", e),
        None => "N/A".to_string(),
    };
    response.on_hover_text(format!(
        "{}\nLat: {:.3}°, Lon: {:.3}°\nElev: {}",
        site.name, site.lat, site.lon, elev_str
    ));
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
