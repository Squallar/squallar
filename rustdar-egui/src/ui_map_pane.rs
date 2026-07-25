use crate::actions::GuiAction;
use crate::overlay_cache::{
    viewport_geo_bounds, current_quantized_zoom, draw_overlay_texture,
    OVERDRAW_FRACTION,
};
use crate::point_painter::EguiPointPainter;
use rustdar_overlays::render::draw::{DrawPointContext, HoverContext};
use std::sync::Arc;
use rustdar_overlays::render::overlay_state::{OverlayRegistry, OverlayKind, OverlayItem, RenderMode};
use crate::pane::{PaneState, RadarImageData};
use rustdar_units::UserPreferences;

use rustdar_radar::{get_color_for_value, get_legend_scale};
use rustdar_radar::sites::RADARS;
use rustdar_radar::types::{MAX_RANGE_KM, ImageBounds, RadarProduct};
use walkers::HttpTiles;

use super::super::map_overlays::{OverlayDrawContext, draw_label_tiles_overlay, is_pos_blocked};

/// Shared references needed for rendering a single pane's map content.
pub(super) struct PaneRenderCtx<'a> {
    pub pane_idx: usize,
    pub pane: &'a mut PaneState,
    pub overlays: &'a mut OverlayRegistry,
    pub user_location: Option<(f64, f64)>,
    pub user_heading: Option<f32>,
    pub user_fix: Option<rustdar_gps::GpsFix>,
    pub label_tiles: &'a mut Option<HttpTiles>,
    pub actions: &'a mut Vec<GuiAction>,
    pub pane_rect: egui::Rect,
    /// Whether this frame's color scale bars run along the bottom edge
    /// (`true`) or the right edge (`false`). Resolved once for the whole map
    /// panel by `ColorScaleOrientation`, so every pane agrees.
    pub horizontal_color_scale: bool,
    pub pointer_available: bool,
    pub excluded_rects: Vec<egui::Rect>,
    /// On Android, the screen position of an active long-press (for radar value tooltip).
    #[cfg(target_os = "android")]    pub long_press_pos: Option<egui::Pos2>,
    /// Screen position of a confirmed overlay click/tap, or `None` if no overlay
    /// click occurred this frame. On desktop this comes from egui's `any_click()`;
    /// on Android from the deferred single-tap detector.
    pub overlay_click_pos: Option<egui::Pos2>,
    /// User unit and timezone preferences.
    pub preferences: &'a UserPreferences,
}

/// Render the map content for a single pane (SPC/NWS overlays, radar image,
/// city labels, radar sites, user location).
pub(super) fn render_pane_map_content(
    ui: &mut egui::Ui,
    projector: &walkers::Projector,
    zoom: f64,
    ctx: &mut PaneRenderCtx<'_>,
) {
    // Load this pane's overlay config snapshot so handler queries
    // (clickable_items, hover_value_at, per_frame_points, etc.) reflect
    // the per-pane settings.
    if !ctx.pane.overlay_configs.is_empty() {
        ctx.overlays.load_pane_configs(&ctx.pane.overlay_configs);
    }

    // Pre-compute radar site icon rects so overlay click detection can
    // skip clicks that land on a site marker (sites take priority).
    if ctx.pane.is_overlay_enabled(OverlayKind::RadarSites) {
        let screen_rect = ui.max_rect();
        let icon_size = (10.0 + zoom as f32 * 2.0).clamp(8.0, 24.0);
        for site in &RADARS {
            let pos = projector
                .project(walkers::lat_lon(site.lat, site.lon))
                .to_pos2();
            if screen_rect.expand(100.0).contains(pos) {
                ctx.excluded_rects.push(egui::Rect::from_center_size(
                    pos,
                    egui::vec2(icon_size, icon_size),
                ));
            }
        }
    }

    // --- Phase 1: immutable-ui work (ordered layer dispatch) ---
    // RadarSites requires `allocate_rect` (&mut ui), so it is deferred to Phase 2.
    {
        let overlay_ctx = OverlayDrawContext::new(
            ui,
            projector,
            ctx.pointer_available,
            ctx.pane_rect,
            &ctx.excluded_rects,
            ctx.overlay_click_pos,
        );

        let mut selected: Vec<Arc<dyn OverlayItem>> = Vec::new();

        let draw_order: Vec<OverlayKind> = ctx.pane.draw_order.clone();
        for &kind in &draw_order {
            if !ctx.pane.is_overlay_enabled(kind) {
                continue;
            }
            match kind {
                // Radar image layer — special handling for loop playback
                OverlayKind::Radar => {
                    // Loop playback: draw the active loop frame instead
                    if ctx.pane.loop_state.is_active() {
                        if let Some(img) = ctx.pane.active_image().cloned() {
                            render_radar_overlay(ui, projector, &img, ctx.pane, ctx.pane_rect, ctx.preferences);
                        }
                    } else {
                        // Extract metadata before drawing (avoids borrow conflict)
                        let meta_snapshot = ctx.pane.overlay_cache(OverlayKind::Radar)
                            .and_then(|c| c.current.as_ref())
                            .and_then(|tex| tex.radar_meta.as_ref())
                            .map(|m| (m.lat, m.lon, m.max_range_km, std::sync::Arc::clone(&m.value_data)));

                        if let Some(tex) = ctx.pane.overlay_cache(OverlayKind::Radar).and_then(|c| c.current.as_ref()) {
                            let screen_rect = ui.max_rect();
                            draw_overlay_texture(ui.painter(), projector, tex, screen_rect);
                        }

                        // Per-frame: range ring + hover value from radar metadata
                        if let Some((lat, lon, _max_range_km, value_data)) = meta_snapshot {
                            render_radar_range_ring(ui, projector, lat, lon);
                            update_pane_hover_value_from_meta(
                                ui, projector,
                                &RadarHoverData { value_data: &value_data, lat, lon },
                                ctx.pane, ctx.pane_rect, ctx.preferences,
                            );
                        }
                    }
                }
                // City label tiles — walkers tile layer
                OverlayKind::CityLabels => {
                    if let Some(ltiles) = ctx.label_tiles.as_mut() {
                        draw_label_tiles_overlay(ui, projector, zoom, ltiles);
                    }
                }
                // Radar sites: texture + per-frame interactions (text labels, clicks)
                OverlayKind::RadarSites => {
                    if let Some(tex) = ctx.pane.overlay_cache(kind).and_then(|c| c.current.as_ref()) {
                        let screen_rect = ui.max_rect();
                        draw_overlay_texture(ui.painter(), projector, tex, screen_rect);
                    }
                    handle_radar_site_interactions(
                        ui,
                        projector,
                        zoom,
                        ctx.pane,
                        ctx.actions,
                        ctx.pane_idx,
                        ctx.preferences,
                        ctx.overlay_click_pos,
                        ctx.pane_rect,
                        &ctx.excluded_rects,
                    );
                }
                // User location blue dot
                OverlayKind::UserLocation => {
                    if let Some((user_lat, user_lon)) = ctx.user_location {
                        render_user_location(ui, projector, user_lat, user_lon, ctx.user_heading, ctx.user_fix.as_ref());
                    }
                }
                // Color scale legend (screen-space HUD).
                // Draw on a foreground layer so overlay textures can never
                // paint over the bars regardless of egui shape batching.
                OverlayKind::ColorScale => {
                    let fg_layer = egui::LayerId::new(
                        egui::Order::Background,
                        ui.id().with("color_scale"),
                    );
                    let mut fg_painter = ui.ctx().layer_painter(fg_layer);
                    fg_painter.set_clip_rect(ctx.pane_rect);
                    let pane_rect = ui.max_rect();
                    render_color_scale(
                        &fg_painter,
                        pane_rect,
                        ctx.horizontal_color_scale,
                        ctx.pane,
                        ctx.preferences,
                    );
                    render_overlay_color_scales(
                        &fg_painter,
                        pane_rect,
                        ctx.horizontal_color_scale,
                        ctx.pane,
                        ctx.overlays,
                    );
                }
                // All other overlays dispatched by render mode
                _ => {
                    match ctx.overlays.render_mode(kind) {
                        Some(RenderMode::Texture) => {
                            let items = ctx.overlays.clickable_items(kind);
                            selected.extend(overlay_ctx.draw_overlay(
                                ctx.pane.overlay_cache(kind),
                                &items,
                            ));
                        }
                        Some(RenderMode::PerFramePoint) => {
                            selected.extend(render_per_frame_overlay(
                                ui,
                                projector,
                                &PerFrameOverlayCtx {
                                    overlays: ctx.overlays,
                                    kind,
                                    zoom,
                                    prefs: ctx.preferences,
                                    overlay_click_pos: ctx.overlay_click_pos,
                                    excluded_rects: &ctx.excluded_rects,
                                    pane_rect: ctx.pane_rect,
                                },
                            ));
                        }
                        _ => {}
                    }
                }
            }
        }

        if !selected.is_empty() {
            ctx.overlays.selected_overlays = selected;
            ctx.overlays.selected_overlay_page = 0;
        }

        // --- Check overlay hover values (model data, etc.) ---
        {
            let hover_pos = ui.ctx().pointer_hover_pos();
            ctx.pane.overlay_hover_value = None;
            if let Some(pos) = hover_pos
                && ctx.pane_rect.contains(pos)
                && !ui.ctx().layer_id_at(pos).is_some_and(|l| l.order > egui::Order::Background) {
                    let map_pos = projector.unproject(egui::vec2(pos.x, pos.y));
                    let hover_lat = map_pos.y();
                    let hover_lon = map_pos.x();
                    for &kind in &draw_order {
                        if ctx.pane.is_overlay_enabled(kind)
                            && let Some(text) = ctx.overlays.hover_value_at(kind, hover_lat, hover_lon) {
                                ctx.pane.overlay_hover_value = Some(text);
                                break;
                            }
                    }
                }
        }

        // --- Check if any texture overlays need background re-rendering ---
        let screen_rect = ui.max_rect();
        let viewport_bounds = viewport_geo_bounds(projector, screen_rect);
        let qzoom = current_quantized_zoom(zoom);
        // Compute render dimensions with overdraw
        let w = (screen_rect.width() * (1.0 + 2.0 * OVERDRAW_FRACTION)) as u32;
        let h = (screen_rect.height() * (1.0 + 2.0 * OVERDRAW_FRACTION)) as u32;

        for &kind in OverlayKind::all() {
            if ctx.overlays.render_mode(kind) != Some(RenderMode::Texture) {
                continue;
            }
            // Radar rendering is driven by product/elevation changes (not viewport),
            // handled by dispatch_pane_renders() in the platform crate.
            if kind == OverlayKind::Radar {
                continue;
            }
            let enabled = ctx.pane.is_overlay_enabled(kind);
            let data_gen = if kind == OverlayKind::RadarSites {
                ctx.pane.radar_sites_render_gen
            } else {
                ctx.overlays.data_generation(kind)
            };
            let has_data = ctx.overlays.has_data(kind);
            let cache = ctx.pane.overlay_cache_mut(kind);
            if enabled
                && has_data
                && !cache.render_in_flight
                && cache.needs_rerender(data_gen, qzoom, &viewport_bounds)
            {
                ctx.actions.push(GuiAction::RenderOverlay {
                    pane_idx: ctx.pane_idx,
                    overlay_kind: kind,
                    geo_bounds: viewport_bounds,
                    width: w,
                    height: h,
                    data_generation: data_gen,
                    zoom: qzoom,
                });
            }
            if !enabled {
                cache.current = None;
            }
        }
    }
    // overlay_ctx (and its shared borrow of ui) is dropped here

    // Mobile long-press tooltip: show radar value above the finger
    #[cfg(target_os = "android")]
    if let Some(touch_pos) = ctx.long_press_pos {
        if ctx.pane_rect.contains(touch_pos) {
            // Try overlay cache meta first (non-loop static render), then loop frame
            let raw_meta = ctx.pane.overlay_cache(OverlayKind::Radar)
                .and_then(|c| c.current.as_ref())
                .and_then(|tex| tex.radar_meta.as_ref())
                .map(|m| (m.lat, m.lon, std::sync::Arc::clone(&m.value_data)));
            if let Some((lat, lon, value_data)) = raw_meta {
                crate::ui::mobile::draw_long_press_tooltip_raw(
                    ui, projector, &value_data, lat, lon, touch_pos, ctx.pane, ctx.preferences,
                );
            } else if let Some(img) = ctx.pane.active_image().cloned() {
                crate::ui::mobile::draw_long_press_tooltip(ui, projector, &img, touch_pos, ctx.pane, ctx.preferences);
            }
        }
    }
}

/// Render the radar image overlay, range ring, and hover tooltip (loop playback path) (loop playback path).
fn render_radar_overlay(
    ui: &egui::Ui,
    projector: &walkers::Projector,
    img: &RadarImageData,
    pane: &mut PaneState,
    pane_rect: egui::Rect,
    prefs: &UserPreferences,
) {
    let bounds = ImageBounds::from_radar_site(img.lat, img.lon);

    let nw = projector
        .project(walkers::lat_lon(bounds.max_lat, bounds.min_lon))
        .to_pos2();
    let se = projector
        .project(walkers::lat_lon(bounds.min_lat, bounds.max_lon))
        .to_pos2();
    let rect = egui::Rect::from_two_pos(nw, se);

    ui.painter().image(
        img.texture.id(),
        rect,
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        egui::Color32::WHITE,
    );

    render_radar_range_ring(ui, projector, img.lat, img.lon);
    update_pane_hover_value_from_meta(
        ui, projector,
        &RadarHoverData { value_data: &img.value_data, lat: img.lat, lon: img.lon },
        pane, pane_rect, prefs,
    );
}

/// Draw only the range ring for a radar site (used with overlay-cache rendering).
fn render_radar_range_ring(
    ui: &egui::Ui,
    projector: &walkers::Projector,
    lat: f64,
    lon: f64,
) {
    let radar_center = projector
        .project(walkers::lat_lon(lat, lon))
        .to_pos2();
    let north_edge = projector
        .project(walkers::lat_lon(lat + MAX_RANGE_KM / 111.32, lon))
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

/// Radar value data and site location for hover queries.
struct RadarHoverData<'a> {
    value_data: &'a [f32],
    lat: f64,
    lon: f64,
}

/// Update hover value using radar metadata from the overlay cache.
fn update_pane_hover_value_from_meta(
    ui: &egui::Ui,
    projector: &walkers::Projector,
    radar: &RadarHoverData<'_>,
    pane: &mut PaneState,
    pane_rect: egui::Rect,
    prefs: &UserPreferences,
) {
    let bounds = ImageBounds::from_radar_site(radar.lat, radar.lon);
    let nw = projector
        .project(walkers::lat_lon(bounds.max_lat, bounds.min_lon))
        .to_pos2();
    let se = projector
        .project(walkers::lat_lon(bounds.min_lat, bounds.max_lon))
        .to_pos2();
    let image_rect = egui::Rect::from_two_pos(nw, se);

    let Some(hover_pos) = ui.ctx().pointer_hover_pos() else {
        pane.last_hover_pos = None;
        pane.hover_value = None;
        return;
    };

    if !pane_rect.contains(hover_pos) {
        pane.last_hover_pos = None;
        pane.hover_value = None;
        return;
    };

    // Suppress hover when cursor is over a floating dialog or popup window.
    if ui.ctx().layer_id_at(hover_pos).is_some_and(|l| l.order > egui::Order::Background) {
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

        pane.hover_value = Some(super::compute_hover_info_raw(
            radar.value_data,
            &super::HoverInput {
                site_lat: radar.lat,
                site_lon: radar.lon,
                hover_lat: map_pos.y(),
                hover_lon: map_pos.x(),
                hover_pos,
                rect: image_rect,
            },
            pane.selected_product,
            prefs,
        ));
    }
}

/// Per-frame radar site label rendering and interaction detection.
///
/// The site circles and background pills are in the background-rasterized
/// texture; this function draws text labels (tiny-skia cannot render text)
/// and handles interactive hits (clicks → site switch, hover → tooltip/cursor).
///
/// `overlay_click_pos` must be taken from `PaneRenderCtx::overlay_click_pos`
/// (pre-filtered — dialog clicks are already stripped). Never pass a raw
/// `ctx.input()` click position here.
fn handle_radar_site_interactions(
    ui: &egui::Ui,
    projector: &walkers::Projector,
    zoom: f64,
    pane: &mut PaneState,
    actions: &mut Vec<GuiAction>,
    pane_idx: usize,
    prefs: &UserPreferences,
    overlay_click_pos: Option<egui::Pos2>,
    pane_rect: egui::Rect,
    excluded_rects: &[egui::Rect],
) {
    let screen_rect = ui.max_rect();
    let zoom_f32 = zoom as f32;
    let icon_size = (10.0 + zoom_f32 * 2.0).clamp(8.0, 24.0);
    let font_size = (icon_size * 0.6).clamp(8.0, 12.0);

    let hover_pos = ui.ctx().pointer_hover_pos();
    let click_pos = overlay_click_pos;

    let is_dark = ui.ctx().global_style().visuals.dark_mode;
    let text_color = if is_dark {
        egui::Color32::WHITE
    } else {
        egui::Color32::BLACK
    };

    for radar_site in &RADARS {
        let site_screen = projector
            .project(walkers::lat_lon(radar_site.lat, radar_site.lon))
            .to_pos2();

        if !screen_rect.expand(100.0).contains(site_screen) {
            continue;
        }

        // Draw the text label below the marker (background pill is in the texture)
        if zoom >= 5.0 {
            let text_pos = egui::pos2(site_screen.x, site_screen.y + icon_size / 2.0 + 3.0);
            ui.painter().text(
                text_pos,
                egui::Align2::CENTER_TOP,
                radar_site.name,
                egui::FontId::monospace(font_size),
                text_color,
            );
        }

        let icon_rect =
            egui::Rect::from_center_size(site_screen, egui::vec2(icon_size, icon_size));

        if let Some(pos) = click_pos
            && icon_rect.contains(pos)
            && !is_pos_blocked(ui.ctx(), pos, pane_rect, excluded_rects) {
                pane.loading_site = Some(radar_site.name.to_string());
                pane.radar_sites_render_gen = pane.radar_sites_render_gen.wrapping_add(1);
                actions.push(GuiAction::SwitchRadarSite { site: radar_site.name.to_string(), pane_idx });
            }

        if let Some(pos) = hover_pos
            && icon_rect.contains(pos)
            && !is_pos_blocked(ui.ctx(), pos, pane_rect, excluded_rects) {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                let elev_str = match radar_site.elev {
                    Some(e) => {
                        let converted = prefs.height.convert_from_feet(e as f32);
                        format!("{:.0} {}", converted, prefs.height.suffix())
                    }
                    None => "N/A".to_string(),
                };
                let tooltip_text = format!(
                    "{}\nLat: {:.3}°, Lon: {:.3}°\nElev: {}",
                    radar_site.name, radar_site.lat, radar_site.lon, elev_str
                );
                egui::Tooltip::always_open(
                    ui.ctx().clone(),
                    ui.layer_id(),
                    egui::Id::new(("site_tooltip", radar_site.name)),
                    egui::PopupAnchor::Pointer,
                )
                .show(|tooltip_ui| { tooltip_ui.label(tooltip_text); });
            }
    }
}

/// Draw user location blue dot indicator with optional heading wedge and hover popup.
fn render_user_location(
    ui: &egui::Ui,
    projector: &walkers::Projector,
    user_lat: f64,
    user_lon: f64,
    heading: Option<f32>,
    fix: Option<&rustdar_gps::GpsFix>,
) {
    let user_screen = projector
        .project(walkers::lat_lon(user_lat, user_lon))
        .to_pos2();

    let screen_rect = ui.max_rect();
    if !screen_rect.expand(50.0).contains(user_screen) {
        return;
    }

    let blue = egui::Color32::from_rgb(30, 130, 255);

    // Draw heading wedge behind the dot if a heading is available
    if let Some(heading_deg) = heading {
        let wedge_radius = 28.0;
        let half_angle = 22.5_f32.to_radians(); // 45° total wedge
        let center_rad = (heading_deg - 90.0).to_radians(); // egui: 0° = right

        let num_segments = 16;
        let mut points = Vec::with_capacity(num_segments + 2);
        points.push(user_screen);
        for i in 0..=num_segments {
            let t = i as f32 / num_segments as f32;
            let angle = center_rad - half_angle + t * 2.0 * half_angle;
            points.push(egui::pos2(
                user_screen.x + wedge_radius * angle.cos(),
                user_screen.y + wedge_radius * angle.sin(),
            ));
        }

        let wedge_color = egui::Color32::from_rgba_unmultiplied(30, 130, 255, 140);
        let wedge_stroke = egui::Color32::from_rgba_unmultiplied(30, 130, 255, 200);
        ui.painter().add(egui::Shape::convex_polygon(
            points,
            wedge_color,
            egui::Stroke::new(1.0, wedge_stroke),
        ));
    }

    // Blue dot (same as before)
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
    ui.painter().circle_filled(user_screen, 7.0, blue);

    // Hover/tap popup with fix details
    if let Some(fix) = fix {
        let dot_rect = egui::Rect::from_center_size(user_screen, egui::vec2(28.0, 28.0));
        if let Some(hover_pos) = ui.ctx().pointer_hover_pos()
            && dot_rect.contains(hover_pos) {
                egui::Tooltip::always_open(
                    ui.ctx().clone(),
                    ui.layer_id(),
                    egui::Id::new("gps_fix_tooltip"),
                    egui::PopupAnchor::Pointer,
                )
                .show(|tooltip_ui| {
                        tooltip_ui.label(format!("Lat: {:.5}°  Lon: {:.5}°", fix.latitude, fix.longitude));
                        if let Some(alt) = fix.altitude_m {
                            tooltip_ui.label(format!("Alt: {:.0} m", alt));
                        }
                        if let Some(speed) = fix.speed_mps {
                            let speed_kts = speed * 1.94384;
                            tooltip_ui.label(format!("Speed: {:.1} m/s ({:.1} kts)", speed, speed_kts));
                        }
                        if let Some(hdg) = fix.heading_deg {
                            tooltip_ui.label(format!("Course: {:.0}°", hdg));
                        }
                        if let Some(sats) = fix.satellites {
                            tooltip_ui.label(format!("Sats: {}", sats));
                        }
                        tooltip_ui.label(format!("Fix: {}", fix.fix_quality.label()));
                        if let Some(hdop) = fix.hdop {
                            tooltip_ui.label(format!("HDOP: {:.1}", hdop));
                        }
                    },
                );
            }
    }
}

// ── Color scale legend ────────────────────────────────────────────────────

/// Bar width in logical pixels.
const SCALE_BAR_WIDTH: f32 = 20.0;
/// Margin from pane edge in logical pixels.
const SCALE_MARGIN: f32 = 16.0;
/// Extra margin reserved for the unit title above/beside the bar.
const SCALE_TITLE_MARGIN: f32 = 16.0;
/// Font size for value labels.
const SCALE_FONT_SIZE: f32 = 11.0;
/// Font size for the unit title label.
const SCALE_TITLE_FONT_SIZE: f32 = 12.0;
/// Outline offset for text shadow.
const SHADOW_OFFSET: f32 = 1.0;
/// Minimum pixel spacing between labels before thinning kicks in.
const MIN_LABEL_SPACING: f32 = 14.0;

/// Format a legend label value. For HHC uses category names; for others, a short numeric string.
fn format_legend_value(product: RadarProduct, value: f32, prefs: &UserPreferences) -> String {
    match product {
        RadarProduct::HydrometeorClassification => {
            match value as u16 {
                10 => "Bio".into(),
                20 => "AP".into(),
                30 => "IC".into(),
                40 => "DS".into(),
                50 => "WS".into(),
                60 => "RA".into(),
                70 => "HR".into(),
                80 => "BD".into(),
                90 => "GR".into(),
                100 => "HA".into(),
                110 => "LH".into(),
                120 => "GH".into(),
                140 => "UK".into(),
                150 => "RF".into(),
                _ => format!("{value:.0}"),
            }
        }
        RadarProduct::Velocity | RadarProduct::StormRelativeVelocity => {
            let converted = prefs.speed.convert_from_ms(value);
            format!("{converted:.0}")
        }
        RadarProduct::SpectrumWidth => {
            let converted = prefs.speed.convert_from_ms(value);
            format!("{converted:.0}")
        }
        RadarProduct::EchoTops => {
            let converted = prefs.height.convert_kft_to_kilo(value);
            format!("{converted:.0}")
        }
        RadarProduct::PrecipitationRate => {
            let converted = prefs.precip_rate.convert_from_in_per_hr(value);
            if converted < 1.0 { format!("{converted:.2}") }
            else { format!("{converted:.1}") }
        }
        RadarProduct::CorrelationCoefficient => format!("{value:.2}"),
        RadarProduct::DifferentialReflectivity
        | RadarProduct::SpecificDifferentialPhase => format!("{value:.1}"),
        _ => {
            if value.fract().abs() < 0.01 { format!("{value:.0}") }
            else { format!("{value:.1}") }
        }
    }
}

/// Draw text with a dark shadow for readability on the map.
fn draw_shadowed_text(
    painter: &egui::Painter,
    pos: egui::Pos2,
    anchor: egui::Align2,
    text: &str,
    font: egui::FontId,
) {
    painter.text(
        pos + egui::vec2(SHADOW_OFFSET, SHADOW_OFFSET),
        anchor,
        text,
        font.clone(),
        egui::Color32::from_black_alpha(200),
    );
    painter.text(
        pos,
        anchor,
        text,
        font,
        egui::Color32::WHITE,
    );
}

/// Render the color scale legend bar for the current pane's radar product.
///
/// `horizontal` is the panel-wide orientation resolved by
/// `pane::ColorScaleOrientation` — deliberately *not* recomputed from
/// `pane_rect` here, so that every pane in the grid draws its bars on the same
/// edge and dragging a divider cannot flip them.
fn render_color_scale(
    painter: &egui::Painter,
    pane_rect: egui::Rect,
    horizontal: bool,
    pane: &PaneState,
    prefs: &UserPreferences,
) {
    let product = pane.selected_product;
    let legend = get_legend_scale(product);
    if legend.thresholds.len() < 2 {
        return;
    }

    // Orientation follows the map panel's shape, not the platform (a grid can
    // be any shape on any target): a portrait panel gets horizontal bars along
    // the bottom, a landscape one vertical bars on the right, so the bar spans
    // the shorter axis and its 20px thickness eats into the longer one.
    // See `pane::ColorScaleOrientation`.
    let bar_length = if horizontal {
        pane_rect.width() - SCALE_MARGIN * 2.0
    } else {
        pane_rect.height() - SCALE_MARGIN * 2.0 - SCALE_TITLE_MARGIN
    };

    if bar_length < 40.0 {
        return; // pane too small
    }

    // Compute bar rect
    let bar_rect = if horizontal {
        // Horizontal bar along the bottom, origin at bottom-left
        let left = pane_rect.left() + SCALE_MARGIN;
        let bottom = pane_rect.bottom() - SCALE_MARGIN;
        let top = bottom - SCALE_BAR_WIDTH;
        egui::Rect::from_min_max(
            egui::pos2(left, top),
            egui::pos2(left + bar_length, bottom),
        )
    } else {
        // Vertical bar along the right, origin at bottom-right
        let right = pane_rect.right() - SCALE_MARGIN;
        let left = right - SCALE_BAR_WIDTH;
        let bottom = pane_rect.bottom() - SCALE_MARGIN;
        let top = bottom - bar_length;
        egui::Rect::from_min_max(
            egui::pos2(left, top),
            egui::pos2(right, bottom),
        )
    };

    let min_val = legend.min_value;
    let max_val = legend.max_value;
    let range = max_val - min_val;
    if range.abs() < f32::EPSILON {
        return;
    }

    let n = legend.thresholds.len();

    if legend.is_gradient {
        // Gradient scales: per-pixel sampling for smooth interpolation.
        let steps = bar_length.ceil() as usize;
        for i in 0..steps {
            let t = i as f32 / (steps - 1).max(1) as f32;
            let value = min_val + t * range;
            let (r, g, b, a) = get_color_for_value(product, value);
            if a == 0 { continue; }
            let color = egui::Color32::from_rgb(r, g, b);
            // Use 2px wide strips to avoid sub-pixel gaps
            if horizontal {
                let x = bar_rect.left() + t * bar_rect.width();
                let strip = egui::Rect::from_min_size(
                    egui::pos2(x, bar_rect.top()),
                    egui::vec2(2.0, SCALE_BAR_WIDTH),
                );
                painter.rect_filled(strip, 0.0, color);
            } else {
                let y = bar_rect.bottom() - t * bar_rect.height();
                let strip = egui::Rect::from_min_size(
                    egui::pos2(bar_rect.left(), y - 1.0),
                    egui::vec2(SCALE_BAR_WIDTH, 2.0),
                );
                painter.rect_filled(strip, 0.0, color);
            }
        }
    } else {
        // Discrete scales: equal-sized blocks, one per threshold.
        for i in 0..n {
            let (_, rgb) = legend.thresholds[i];
            let color = egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]);

            let t0 = i as f32 / n as f32;
            let t1 = (i + 1) as f32 / n as f32;

            if horizontal {
                let x0 = bar_rect.left() + t0 * bar_rect.width();
                let x1 = bar_rect.left() + t1 * bar_rect.width();
                let strip = egui::Rect::from_min_max(
                    egui::pos2(x0, bar_rect.top()),
                    egui::pos2(x1, bar_rect.bottom()),
                );
                painter.rect_filled(strip, 0.0, color);
            } else {
                let y0 = bar_rect.bottom() - t0 * bar_rect.height();
                let y1 = bar_rect.bottom() - t1 * bar_rect.height();
                let strip = egui::Rect::from_min_max(
                    egui::pos2(bar_rect.left(), y1),
                    egui::pos2(bar_rect.right(), y0),
                );
                painter.rect_filled(strip, 0.0, color);
            }
        }
    }

    // --- Labels: draw threshold values alongside the bar ---
    let label_font = egui::FontId::proportional(SCALE_FONT_SIZE);
    let title_font = egui::FontId::proportional(SCALE_TITLE_FONT_SIZE);

    let mut label_positions: Vec<(f32, String)> = Vec::new();
    for (i, &(val, _)) in legend.thresholds.iter().enumerate() {
        let pixel_pos = if legend.is_gradient {
            // Gradient: value-proportional positioning
            let t = (val - min_val) / range;
            if horizontal {
                bar_rect.left() + t * bar_rect.width()
            } else {
                bar_rect.bottom() - t * bar_rect.height()
            }
        } else {
            // Discrete: index-based positioning (bottom/left edge of each block)
            let t = i as f32 / n as f32;
            if horizontal {
                bar_rect.left() + t * bar_rect.width()
            } else {
                bar_rect.bottom() - t * bar_rect.height()
            }
        };
        let text = format_legend_value(product, val, prefs);
        label_positions.push((pixel_pos, text));
    }

    // Filter out labels that are too close to the previous one
    let mut prev_pos: Option<f32> = None;
    let thinned: Vec<(f32, &str)> = label_positions.iter().filter(|(pos, _)| {
        if let Some(prev) = prev_pos
            && (pos - prev).abs() < MIN_LABEL_SPACING {
                return false;
            }
        prev_pos = Some(*pos);
        true
    }).map(|(pos, text)| (*pos, text.as_str())).collect();

    for (pixel_pos, text) in &thinned {
        if horizontal {
            // Labels above the bar
            let pos = egui::pos2(*pixel_pos, bar_rect.top() - 2.0);
            draw_shadowed_text(painter, pos, egui::Align2::CENTER_BOTTOM, text, label_font.clone());
        } else {
            // Labels to the left of the bar
            let pos = egui::pos2(bar_rect.left() - 4.0, *pixel_pos);
            draw_shadowed_text(painter, pos, egui::Align2::RIGHT_CENTER, text, label_font.clone());
        }
    }

    // --- Title: unit label above the bar (desktop) or to the left (mobile) ---
    let unit = product.unit_label(prefs);
    if horizontal {
        let title_pos = egui::pos2(bar_rect.left() - 4.0, bar_rect.center().y);
        draw_shadowed_text(painter, title_pos, egui::Align2::RIGHT_CENTER, unit, title_font);
    } else {
        let title_pos = egui::pos2(bar_rect.center().x, bar_rect.top() - 4.0);
        draw_shadowed_text(painter, title_pos, egui::Align2::CENTER_BOTTOM, unit, title_font);
    }
}

/// Render color scale legends for overlay layers that provide their own legend
/// (e.g. model data CIN). Drawn to the left of the radar color scale.
fn render_overlay_color_scales(
    painter: &egui::Painter,
    pane_rect: egui::Rect,
    // Same panel-wide orientation as the radar color scale.
    horizontal: bool,
    pane: &PaneState,
    overlays: &OverlayRegistry,
) {
    // Offset each overlay legend to the left of (vertical) or above
    // (horizontal) the radar scale.
    let mut bar_offset = 0;

    for &kind in &pane.draw_order {
        if !pane.is_overlay_enabled(kind) || kind == OverlayKind::ColorScale {
            continue;
        }
        let Some(legend) = overlays.legend(kind) else {
            continue;
        };
        if legend.thresholds.len() < 2 {
            continue;
        }

        bar_offset += 1;
        let offset_px = bar_offset as f32 * (SCALE_BAR_WIDTH + 40.0);

        let bar_length = if horizontal {
            pane_rect.width() - SCALE_MARGIN * 2.0
        } else {
            pane_rect.height() - SCALE_MARGIN * 2.0 - SCALE_TITLE_MARGIN
        };
        if bar_length < 40.0 {
            continue;
        }

        let bar_rect = if horizontal {
            let left = pane_rect.left() + SCALE_MARGIN;
            let bottom = pane_rect.bottom() - SCALE_MARGIN - offset_px;
            let top = bottom - SCALE_BAR_WIDTH;
            egui::Rect::from_min_max(
                egui::pos2(left, top),
                egui::pos2(left + bar_length, bottom),
            )
        } else {
            let right = pane_rect.right() - SCALE_MARGIN - offset_px;
            let left = right - SCALE_BAR_WIDTH;
            let bottom = pane_rect.bottom() - SCALE_MARGIN;
            let top = bottom - bar_length;
            egui::Rect::from_min_max(
                egui::pos2(left, top),
                egui::pos2(right, bottom),
            )
        };

        let min_val = legend.min_value;
        let max_val = legend.max_value;
        let range = max_val - min_val;
        if range.abs() < f32::EPSILON {
            continue;
        }

        // Always gradient for overlay legends.
        let steps = bar_length.ceil() as usize;
        for i in 0..steps {
            let t = i as f32 / (steps - 1).max(1) as f32;
            let value = min_val + t * range;
            let color = interpolate_legend_color(&legend.thresholds, value);
            let [r, g, b] = color;
            if horizontal {
                let x = bar_rect.left() + t * bar_rect.width();
                let strip = egui::Rect::from_min_size(
                    egui::pos2(x, bar_rect.top()),
                    egui::vec2(2.0, SCALE_BAR_WIDTH),
                );
                painter.rect_filled(strip, 0.0, egui::Color32::from_rgb(r, g, b));
            } else {
                let y = bar_rect.bottom() - t * bar_rect.height();
                let strip = egui::Rect::from_min_size(
                    egui::pos2(bar_rect.left(), y - 1.0),
                    egui::vec2(SCALE_BAR_WIDTH, 2.0),
                );
                painter.rect_filled(strip, 0.0, egui::Color32::from_rgb(r, g, b));
            }
        }

        // Labels
        let label_font = egui::FontId::proportional(SCALE_FONT_SIZE);
        let title_font = egui::FontId::proportional(SCALE_TITLE_FONT_SIZE);

        let mut label_positions: Vec<(f32, String)> = Vec::new();
        for &(val, _) in &legend.thresholds {
            let t = (val - min_val) / range;
            let pixel_pos = if horizontal {
                bar_rect.left() + t * bar_rect.width()
            } else {
                bar_rect.bottom() - t * bar_rect.height()
            };
            label_positions.push((pixel_pos, format!("{val:.0}")));
        }

        let mut prev_pos: Option<f32> = None;
        let thinned: Vec<(f32, &str)> = label_positions.iter().filter(|(pos, _)| {
            if let Some(prev) = prev_pos
                && (pos - prev).abs() < MIN_LABEL_SPACING
            {
                return false;
            }
            prev_pos = Some(*pos);
            true
        }).map(|(pos, text)| (*pos, text.as_str())).collect();

        for (pixel_pos, text) in &thinned {
            if horizontal {
                let pos = egui::pos2(*pixel_pos, bar_rect.top() - 2.0);
                draw_shadowed_text(painter, pos, egui::Align2::CENTER_BOTTOM, text, label_font.clone());
            } else {
                let pos = egui::pos2(bar_rect.left() - 4.0, *pixel_pos);
                draw_shadowed_text(painter, pos, egui::Align2::RIGHT_CENTER, text, label_font.clone());
            }
        }

        // Title
        let unit = legend.unit_label;
        if horizontal {
            let title_pos = egui::pos2(bar_rect.left() - 4.0, bar_rect.center().y);
            draw_shadowed_text(painter, title_pos, egui::Align2::RIGHT_CENTER, unit, title_font);
        } else {
            let title_pos = egui::pos2(bar_rect.center().x, bar_rect.top() - 4.0);
            draw_shadowed_text(painter, title_pos, egui::Align2::CENTER_BOTTOM, unit, title_font);
        }
    }
}

/// Interpolate an RGB color from a sorted threshold list for a given value.
fn interpolate_legend_color(thresholds: &[(f32, [u8; 3])], value: f32) -> [u8; 3] {
    if thresholds.is_empty() {
        return [0, 0, 0];
    }
    if value <= thresholds[0].0 {
        return thresholds[0].1;
    }
    if value >= thresholds[thresholds.len() - 1].0 {
        return thresholds[thresholds.len() - 1].1;
    }
    for i in 1..thresholds.len() {
        if value <= thresholds[i].0 {
            let (v0, c0) = thresholds[i - 1];
            let (v1, c1) = thresholds[i];
            let t = if (v1 - v0).abs() < f32::EPSILON {
                0.0
            } else {
                (value - v0) / (v1 - v0)
            };
            return [
                (c0[0] as f32 + (c1[0] as f32 - c0[0] as f32) * t) as u8,
                (c0[1] as f32 + (c1[1] as f32 - c0[1] as f32) * t) as u8,
                (c0[2] as f32 + (c1[2] as f32 - c0[2] as f32) * t) as u8,
            ];
        }
    }
    thresholds[thresholds.len() - 1].1
}

/// Context for per-frame point overlay rendering.
struct PerFrameOverlayCtx<'a> {
    overlays: &'a OverlayRegistry,
    kind: OverlayKind,
    zoom: f64,
    prefs: &'a UserPreferences,
    /// Pre-filtered click position (dialog clicks already stripped).
    /// See `PaneRenderCtx::overlay_click_pos` and the pre-filter in `ui_map.rs`.
    overlay_click_pos: Option<egui::Pos2>,
    excluded_rects: &'a [egui::Rect],
    pane_rect: egui::Rect,
}

/// Per-frame rendering for point overlays (e.g. METAR station model plots).
///
/// Projects each point onto the screen, culls off-screen points, calls the
/// handler's `draw_point()` via an `EguiPointPainter`, and handles click/hover
/// detection using the handler-provided hit radius.
fn render_per_frame_overlay(
    ui: &egui::Ui,
    projector: &walkers::Projector,
    pf: &PerFrameOverlayCtx<'_>,
) -> Vec<Arc<dyn OverlayItem>> {
    let points = pf.overlays.per_frame_points(pf.kind);
    if points.is_empty() {
        return Vec::new();
    }

    let zoom_f32 = pf.zoom as f32;
    let is_dark = ui.ctx().global_style().visuals.dark_mode;
    let draw_ctx = DrawPointContext { zoom: zoom_f32, is_dark };
    let hit_radius = pf.overlays.point_hit_radius(pf.kind, zoom_f32);
    let hover_ctx = HoverContext { prefs: pf.prefs };

    let screen_rect = ui.max_rect();
    let margin = hit_radius + 40.0; // extra margin for station model elements
    let expanded = screen_rect.expand(margin);
    // Pre-compute viewport geo-bounds (with margin) so we can skip the
    // expensive Mercator projection for points that are clearly off-screen.
    let geo_bounds = viewport_geo_bounds(projector, expanded);

    let painter = ui.painter();
    let hover_pos = ui.ctx().pointer_hover_pos();

    let mut selected = Vec::new();
    let mut closest_hover: Option<(f32, u32)> = None; // (distance², id)

    for pt in points {
        // Fast geo-bounds rejection before the costly projection.
        if pt.lat < geo_bounds.min_lat
            || pt.lat > geo_bounds.max_lat
            || pt.lon < geo_bounds.min_lon
            || pt.lon > geo_bounds.max_lon
        {
            continue;
        }

        let screen = projector
            .project(walkers::lat_lon(pt.lat, pt.lon))
            .to_pos2();

        if !expanded.contains(screen) {
            continue;
        }

        // Draw the point
        let mut ep = EguiPointPainter {
            painter,
            center: screen,
        };
        pf.overlays.draw_point(pf.kind, pt.id, &mut ep, &draw_ctx);

        // Click detection — layer blocking already applied by pre-filter in ui_map.rs.
        if let Some(click_pos) = pf.overlay_click_pos {
            let dx = click_pos.x - screen.x;
            let dy = click_pos.y - screen.y;
            if dx * dx + dy * dy <= hit_radius * hit_radius
                && !is_pos_blocked(ui.ctx(), click_pos, pf.pane_rect, pf.excluded_rects) {
                    selected.push(pt.selection.clone());
                }
        }

        // Hover detection — skip if cursor is over a dialog or outside the pane.
        if let Some(hp) = hover_pos
            && !is_pos_blocked(ui.ctx(), hp, pf.pane_rect, pf.excluded_rects) {
                let dx = hp.x - screen.x;
                let dy = hp.y - screen.y;
                let d2 = dx * dx + dy * dy;
                if d2 <= hit_radius * hit_radius
                    && closest_hover.is_none_or(|(best_d2, _)| d2 < best_d2) {
                        closest_hover = Some((d2, pt.id));
                    }
            }
    }

    // Show tooltip for closest hovered point
    if let Some((_, id)) = closest_hover
        && let Some(text) = pf.overlays.hover_text(pf.kind, id, &hover_ctx) {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            egui::Tooltip::always_open(
                ui.ctx().clone(),
                ui.layer_id(),
                egui::Id::new(("per_frame_overlay_hover", pf.kind as u8)),
                egui::PopupAnchor::Pointer,
            )
            .width(400.0)
            .show(|tooltip_ui| {
                tooltip_ui.label(text);
            });
        }

    selected
}
