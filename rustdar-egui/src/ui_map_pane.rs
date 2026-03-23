use crate::actions::GuiAction;
use crate::overlay_cache::{
    viewport_geo_bounds, current_quantized_zoom, draw_overlay_texture,
    OVERDRAW_FRACTION,
};
use crate::point_painter::EguiPointPainter;
use rustdar_overlays::render::draw::{DrawPointContext, HoverContext};
use rustdar_overlays::render::layers::LayerKind;
use rustdar_overlays::render::overlay_state::{OverlayRegistry, OverlayKind, SelectedOverlay};
use crate::pane::{PaneState, RadarImageData};
use rustdar_units::UserPreferences;

use rustdar_radar::sites::RADARS;
use rustdar_radar::types::{MAX_RANGE_KM, ImageBounds};
use walkers::HttpTiles;

use super::super::map_overlays::{OverlayDrawContext, draw_label_tiles_overlay};

/// Shared references needed for rendering a single pane's map content.
pub(super) struct PaneRenderCtx<'a> {
    pub pane_idx: usize,
    pub pane: &'a mut PaneState,
    pub overlays: &'a mut OverlayRegistry,
    pub user_location: Option<(f64, f64)>,
    pub label_tiles: &'a mut Option<HttpTiles>,
    pub actions: &'a mut Vec<GuiAction>,
    pub pane_rect: egui::Rect,
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
    // Pre-compute radar site icon rects so overlay click detection can
    // skip clicks that land on a site marker (sites take priority).
    if ctx.pane.layers.is_enabled(LayerKind::RadarSites) {
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

        let mut selected: Vec<SelectedOverlay> = Vec::new();

        let draw_order: Vec<OverlayKind> = ctx.pane.draw_order.clone();
        for &kind in &draw_order {
            if !kind.is_enabled(&ctx.pane.layers) {
                continue;
            }
            match kind {
                // Texture-based overlays: draw texture + clickable items
                OverlayKind::SpcOutlook
                | OverlayKind::SpcDiscussions
                | OverlayKind::NwsAlerts
                | OverlayKind::StormReports => {
                    let items = ctx.overlays.clickable_items(kind, &ctx.pane.layers);
                    selected.extend(overlay_ctx.draw_overlay(
                        ctx.pane.overlay_cache(kind),
                        &items,
                    ));
                }
                // Per-frame point overlay: METAR station model plots
                OverlayKind::Metar => {
                    selected.extend(render_per_frame_overlay(
                        ui,
                        projector,
                        ctx.overlays,
                        kind,
                        zoom,
                        ctx.preferences,
                        ctx.overlay_click_pos,
                        &ctx.excluded_rects,
                    ));
                }
                // Radar image layer — drawn from overlay texture cache
                OverlayKind::Radar => {
                    // Loop playback: draw the active loop frame instead
                    if ctx.pane.loop_state.multi_frame {
                        if let Some(img) = ctx.pane.active_image().cloned() {
                            render_radar_overlay(ui, projector, &img, ctx.pane, ctx.pane_rect, ctx.preferences);
                        }
                    } else {
                        // Extract metadata before drawing (avoids borrow conflict)
                        let meta_snapshot = ctx.pane.overlay_cache(OverlayKind::Radar)
                            .and_then(|c| c.current.as_ref())
                            .and_then(|tex| tex.radar_meta.as_ref())
                            .map(|m| (m.lat, m.lon, m.max_range_km, std::sync::Arc::clone(&m.value_data)));

                        if let Some(ref tex) = ctx.pane.overlay_cache(OverlayKind::Radar).and_then(|c| c.current.as_ref()) {
                            let screen_rect = ui.max_rect();
                            draw_overlay_texture(ui.painter(), projector, tex, screen_rect);
                        }

                        // Per-frame: range ring + hover value from radar metadata
                        if let Some((lat, lon, _max_range_km, value_data)) = meta_snapshot {
                            render_radar_range_ring(ui, projector, lat, lon);
                            update_pane_hover_value_from_meta(
                                ui, projector, &value_data, lat, lon,
                                ctx.pane, ctx.pane_rect, ctx.preferences,
                            );
                        }
                    }
                }
                // City label tiles
                OverlayKind::CityLabels => {
                    if let Some(ltiles) = ctx.label_tiles.as_mut() {
                        draw_label_tiles_overlay(ui, projector, zoom, ltiles);
                    }
                }
                // Radar sites: texture + per-frame interactions
                OverlayKind::RadarSites => {
                    // Draw the pre-rasterized site circles texture
                    if let Some(ref tex) = ctx.pane.overlay_cache(kind).and_then(|c| c.current.as_ref()) {
                        let screen_rect = ui.max_rect();
                        draw_overlay_texture(ui.painter(), projector, tex, screen_rect);
                    }
                    // Per-frame: clicks, tooltips, hover cursor
                    handle_radar_site_interactions(
                        ui,
                        projector,
                        zoom,
                        ctx.pane,
                        ctx.actions,
                        ctx.pane_idx,
                        ctx.preferences,
                    );
                }
                // User location blue dot
                OverlayKind::UserLocation => {
                    if let Some((user_lat, user_lon)) = ctx.user_location {
                        render_user_location(ui, projector, user_lat, user_lon);
                    }
                }
            }
        }

        if !selected.is_empty() {
            ctx.overlays.selected_overlays = selected;
            ctx.overlays.selected_overlay_page = 0;
        }

        // --- Check if any texture overlays need background re-rendering ---
        let screen_rect = ui.max_rect();
        let viewport_bounds = viewport_geo_bounds(projector, screen_rect);
        let qzoom = current_quantized_zoom(zoom);
        // Compute render dimensions with overdraw
        let w = (screen_rect.width() * (1.0 + 2.0 * OVERDRAW_FRACTION)) as u32;
        let h = (screen_rect.height() * (1.0 + 2.0 * OVERDRAW_FRACTION)) as u32;

        for &kind in OverlayKind::texture_overlays() {
            // Radar rendering is driven by product/elevation changes (not viewport),
            // handled by dispatch_pane_renders() in the platform crate.
            if kind == OverlayKind::Radar {
                continue;
            }
            let enabled = kind.is_enabled(&ctx.pane.layers);
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
                    geo_bounds: viewport_bounds.clone(),
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
    update_pane_hover_value_from_meta(ui, projector, &img.value_data, img.lat, img.lon, pane, pane_rect, prefs);
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

/// Update hover value using radar metadata from the overlay cache.
fn update_pane_hover_value_from_meta(
    ui: &egui::Ui,
    projector: &walkers::Projector,
    value_data: &[f32],
    lat: f64,
    lon: f64,
    pane: &mut PaneState,
    pane_rect: egui::Rect,
    prefs: &UserPreferences,
) {
    let bounds = ImageBounds::from_radar_site(lat, lon);
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

    let pos_changed = pane
        .last_hover_pos
        .map(|last| (last - hover_pos).length() > 0.5)
        .unwrap_or(true);
    pane.last_hover_pos = Some(hover_pos);

    if pos_changed {
        let screen_vec = egui::vec2(hover_pos.x, hover_pos.y);
        let map_pos = projector.unproject(screen_vec);

        pane.hover_value = Some(super::compute_hover_info_raw(
            value_data,
            lat,
            lon,
            map_pos.y(),
            map_pos.x(),
            hover_pos,
            image_rect,
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
fn handle_radar_site_interactions(
    ui: &egui::Ui,
    projector: &walkers::Projector,
    zoom: f64,
    pane: &mut PaneState,
    actions: &mut Vec<GuiAction>,
    pane_idx: usize,
    prefs: &UserPreferences,
) {
    let screen_rect = ui.max_rect();
    let zoom_f32 = zoom as f32;
    let icon_size = (10.0 + zoom_f32 * 2.0).clamp(8.0, 24.0);
    let font_size = (icon_size * 0.6).clamp(8.0, 12.0);

    let hover_pos = ui.ctx().pointer_hover_pos();
    let click_pos = ui.ctx().input(|i| {
        if i.pointer.any_click() {
            i.pointer.interact_pos()
        } else {
            None
        }
    });

    let is_dark = ui.ctx().style().visuals.dark_mode;
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

        if let Some(pos) = click_pos {
            if icon_rect.contains(pos) {
                pane.loading_site = Some(radar_site.name.to_string());
                pane.radar_sites_render_gen = pane.radar_sites_render_gen.wrapping_add(1);
                actions.push(GuiAction::SwitchRadarSite { site: radar_site.name.to_string(), pane_idx });
            }
        }

        if let Some(pos) = hover_pos {
            if icon_rect.contains(pos) {
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
                #[allow(deprecated)]
                egui::show_tooltip_at_pointer(
                    ui.ctx(),
                    ui.layer_id(),
                    egui::Id::new(("site_tooltip", radar_site.name)),
                    |tooltip_ui| { tooltip_ui.label(tooltip_text); },
                );
            }
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

/// Per-frame rendering for point overlays (e.g. METAR station model plots).
///
/// Projects each point onto the screen, culls off-screen points, calls the
/// handler's `draw_point()` via an `EguiPointPainter`, and handles click/hover
/// detection using the handler-provided hit radius.
fn render_per_frame_overlay(
    ui: &egui::Ui,
    projector: &walkers::Projector,
    overlays: &OverlayRegistry,
    kind: OverlayKind,
    zoom: f64,
    prefs: &UserPreferences,
    overlay_click_pos: Option<egui::Pos2>,
    excluded_rects: &[egui::Rect],
) -> Vec<SelectedOverlay> {
    let points = overlays.per_frame_points(kind);
    if points.is_empty() {
        return Vec::new();
    }

    let zoom_f32 = zoom as f32;
    let is_dark = ui.ctx().style().visuals.dark_mode;
    let draw_ctx = DrawPointContext { zoom: zoom_f32, is_dark };
    let hit_radius = overlays.point_hit_radius(kind, zoom_f32);
    let hover_ctx = HoverContext { prefs };

    let screen_rect = ui.max_rect();
    let margin = hit_radius + 40.0; // extra margin for station model elements
    let expanded = screen_rect.expand(margin);

    let painter = ui.painter();
    let hover_pos = ui.ctx().pointer_hover_pos();

    let mut selected = Vec::new();
    let mut closest_hover: Option<(f32, u32)> = None; // (distance², id)

    for pt in points {
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
        overlays.draw_point(kind, pt.id, &mut ep, &draw_ctx);

        // Click detection
        if let Some(click_pos) = overlay_click_pos {
            let dx = click_pos.x - screen.x;
            let dy = click_pos.y - screen.y;
            if dx * dx + dy * dy <= hit_radius * hit_radius {
                let on_excluded = excluded_rects.iter().any(|r| r.contains(click_pos));
                if !on_excluded {
                    selected.push(pt.selection.clone());
                }
            }
        }

        // Hover detection
        if let Some(hp) = hover_pos {
            let dx = hp.x - screen.x;
            let dy = hp.y - screen.y;
            let d2 = dx * dx + dy * dy;
            if d2 <= hit_radius * hit_radius {
                if closest_hover.map_or(true, |(best_d2, _)| d2 < best_d2) {
                    closest_hover = Some((d2, pt.id));
                }
            }
        }
    }

    // Show tooltip for closest hovered point
    if let Some((_, id)) = closest_hover {
        if let Some(text) = overlays.hover_text(kind, id, &hover_ctx) {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            #[allow(deprecated)]
            egui::show_tooltip_at_pointer(
                ui.ctx(),
                ui.layer_id(),
                egui::Id::new(("per_frame_overlay_hover", kind as u8)),
                |tooltip_ui| {
                    tooltip_ui.set_max_width(400.0);
                    tooltip_ui.label(text);
                },
            );
        }
    }

    selected
}
