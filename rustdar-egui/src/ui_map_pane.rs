use crate::actions::GuiAction;
use crate::legend_ramp;
use crate::overlay_cache::{
    current_quantized_zoom, draw_overlay_texture, plan_overlay_texture, viewport_geo_bounds,
};
use crate::pane::{PaneState, RadarImageData, TimeMode};
use crate::point_painter::EguiPointPainter;
use rustdar_overlays::render::draw::{DrawPointContext, HoverContext};
use rustdar_overlays::render::overlay_state::{
    OverlayItem, OverlayLegend, OverlayRegistry, RenderMode, Signed, Surface,
};
use rustdar_units::{HailSizeUnit, UserPreferences};
use std::sync::Arc;

use crate::tile_source::HttpsTiles;
use rustdar_geo::KM_PER_DEGREE_LAT;
use rustdar_radar::get_color_for_value;
use rustdar_radar::hca::MeltingLayerSource;
use rustdar_radar::hover::{HoverSource, Reading};
use rustdar_radar::sites::RadarSite;
use rustdar_source::id::{LayerId, known};
use rustdar_source::product::FieldId;
use rustdar_source::time::TimeAxis;

use super::super::map_overlays::{OverlayDrawContext, draw_tile_layer, is_pos_blocked};
use rustdar_radar::fields as radar_fields;

/// Which of a pane's surfaces one call to [`render_pane_map_content`] paints.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum PaneSurfaces {
    GroundAndGlass,
    /// A 3D pane's off-screen floor strip: geography only — chrome down here
    /// would be mirrored onto the floor.
    GroundOnly,
}

impl PaneSurfaces {
    /// Whether this pass paints `surface` — the handler-declared [`Surface`].
    const fn paints(self, surface: Surface) -> bool {
        match self {
            Self::GroundAndGlass => true,
            Self::GroundOnly => matches!(surface, Surface::Ground),
        }
    }
}

/// Shared references needed for rendering a single pane's map content.
pub(super) struct PaneRenderCtx<'a> {
    pub pane_idx: usize,
    pub pane: &'a mut PaneState,
    pub overlays: &'a mut OverlayRegistry,
    pub user_location: Option<(f64, f64)>,
    pub user_heading: Option<f32>,
    pub user_fix: Option<rustdar_location::Fix>,
    pub label_tiles: &'a mut Option<HttpsTiles>,
    /// How many slippy zoom levels deeper than this pane's own zoom its raster
    /// tile layers should fetch — see
    /// [`draw_tile_layer`](super::super::map_overlays::draw_tile_layer).
    pub tile_zoom_bias: u8,
    pub actions: &'a mut Vec<GuiAction>,
    pub pane_rect: egui::Rect,
    /// Which halves of the pane's content this pass is for. See
    /// [`PaneSurfaces`].
    pub surfaces: PaneSurfaces,
    /// Whether this frame's color scale bars run along the bottom edge
    /// (`true`) or the right edge (`false`). Resolved once per map panel.
    pub horizontal_color_scale: bool,
    /// The lowest screen y the colour-scale legend may draw on: the map's
    /// bottom edge, less whatever the phone shell's bottom bar covered.
    pub color_scale_floor: f32,
    pub pointer_available: bool,
    /// Rects of chrome painted over the map with no egui layer of its own.
    pub excluded_rects: Vec<egui::Rect>,
    /// Screen position of an active long-press (for the radar value tooltip),
    /// or `None`. Only the touch pipeline ever produces one.
    pub long_press_pos: Option<egui::Pos2>,
    /// Screen position of a confirmed overlay click/tap, or `None` if no overlay
    /// click occurred this frame. On desktop this comes from egui's `any_click()`;
    /// on Android from the deferred single-tap detector.
    pub overlay_click_pos: Option<egui::Pos2>,
    /// Set by every handler that **acts** on
    /// [`overlay_click_pos`](Self::overlay_click_pos) — the consumption half of
    /// the fade trigger in `ui_fade.rs`.
    pub click_consumed: &'a mut bool,
    pub preferences: &'a UserPreferences,
    /// The kinds this pane dispatched, in the order they painted, with the
    /// egui layer each **arm** painted into. Two kinds on *different* layers
    /// composite in `GraphicLayers::drain`'s order, not in this sequence.
    #[cfg(test)]
    pub paint_order: Vec<(LayerId, egui::LayerId)>,
}

/// Render the map content for a single pane (SPC/NWS overlays, radar image,
/// city labels, radar sites, user location).
pub(super) fn render_pane_map_content(
    ui: &mut egui::Ui,
    projector: &walkers::Projector,
    zoom: f64,
    ctx: &mut PaneRenderCtx<'_>,
) {
    ctx.pane.hydrate_layer_states(ctx.overlays, ctx.pane_idx);

    // Cleared every frame and re-set by the radar arm below. That arm is the
    // only writer, and it runs only while Radar is enabled and has an image.
    ctx.pane.hover_value = None;

    // Sites take priority over the overlays beneath them. Kept out of
    // `ctx.excluded_rects`, which `handle_radar_site_interactions` reads
    // itself: with the icons in there, every site click was self-blocked.
    let visible_sites = visible_radar_sites(ui, projector, zoom, ctx.pane);
    // What the overlays *under* the sites must not be clicked through.
    let overlay_excluded_rects: Vec<egui::Rect> = ctx
        .excluded_rects
        .iter()
        .copied()
        .chain(visible_sites.iter().map(|s| s.icon_rect))
        .collect();

    // RadarSites requires `allocate_rect` (&mut ui), so it is deferred to Phase 2.
    {
        let overlay_ctx = OverlayDrawContext::new(
            ui,
            projector,
            ctx.pointer_available,
            ctx.pane_rect,
            &overlay_excluded_rects,
            ctx.overlay_click_pos,
        );

        let mut selected: Vec<Arc<dyn OverlayItem>> = Vec::new();
        // The stale-image notice, deferred out of the Radar arm's position:
        // it must read over every overlay drawn after the radar.
        let mut pending_notice: Option<(FieldId, f32)> = None;
        let mut melting_layer_caveat: Option<MeltingLayerSource> = None;

        let draw_order: Vec<LayerId> = ctx.pane.draw_order_vec();
        for id in &draw_order {
            if !ctx.pane.is_overlay_enabled(id) {
                continue;
            }
            // An id with no registered handler is RETAINED in the list and
            // skipped at draw, so a newer build's layer keeps its place.
            let Some(handler) = ctx.overlays.handler_by_id(id) else {
                continue;
            };
            // The ground/glass split: a pass not painting this layer's surface
            // skips the arm entirely, so it also skips the paint-order record.
            if !ctx.surfaces.paints(handler.surface()) {
                continue;
            }
            // Every arm below paints through `ui.painter()` — the pane's own
            // paint list — so submission order IS `draw_order`.
            #[cfg(test)]
            let mut painted_layer = ui.painter().layer_id();
            match id {
                id if *id == known::RADAR => {
                    if ctx.pane.loop_state().is_active() {
                        if let Some(img) = ctx.pane.active_image().cloned() {
                            render_radar_overlay(
                                ui,
                                projector,
                                &img,
                                ctx.pane,
                                ctx.pane_rect,
                                ctx.preferences,
                            );
                        }
                    } else {
                        let meta_snapshot = ctx
                            .pane
                            .overlay_cache(id)
                            .and_then(|c| c.current())
                            .and_then(|tex| tex.radar_meta.as_ref())
                            .map(|m| {
                                (
                                    m.lat,
                                    m.lon,
                                    m.max_range_km,
                                    std::sync::Arc::clone(&m.hover),
                                )
                            });

                        if let Some(tex) = ctx.pane.overlay_cache(id).and_then(|c| c.current()) {
                            let screen_rect = ui.max_rect();
                            draw_overlay_texture(ui.painter(), projector, tex, screen_rect);
                        }

                        if let Some((lat, lon, extent_km, hover)) = meta_snapshot {
                            render_radar_range_ring(ui, projector, lat, lon, extent_km);
                            update_pane_hover_value_from_meta(
                                ui,
                                projector,
                                &RadarHoverData {
                                    hover: &hover,
                                    lat,
                                    lon,
                                },
                                ctx.pane,
                                ctx.pane_rect,
                                ctx.preferences,
                            );
                        }
                    }

                    // The pixels above are not the selection every other label
                    // on this pane is describing — say which product they are.
                    pending_notice = ctx.pane.stale_image_on_screen();
                    // And what the classification behind them is standing on,
                    // when nobody measured it. Never both `Some`.
                    melting_layer_caveat = ctx
                        .pane
                        .displayed_melting_layer_source()
                        .filter(|source| !source.is_measured());
                }
                id if *id == known::CITY_LABELS => {
                    if let Some(ltiles) = ctx.label_tiles.as_mut() {
                        draw_tile_layer(ui, projector, zoom, ltiles, ctx.tile_zoom_bias);
                    }
                }
                id if *id == known::RADAR_SITES => {
                    if let Some(tex) = ctx.pane.overlay_cache(id).and_then(|c| c.current()) {
                        let screen_rect = ui.max_rect();
                        draw_overlay_texture(ui.painter(), projector, tex, screen_rect);
                    }
                    handle_radar_site_interactions(ui, zoom, &visible_sites, ctx);
                }
                id if *id == known::USER_LOCATION => {
                    if let Some((user_lat, user_lon)) = ctx.user_location {
                        render_user_location(
                            ui,
                            projector,
                            user_lat,
                            user_lon,
                            ctx.user_heading,
                            ctx.user_fix.as_ref(),
                        );
                    }
                }
                // Color scale legend (screen-space HUD) — painted through the
                // pane's own paint list, so `draw_order` genuinely places it.
                id if *id == known::COLOR_SCALE => {
                    let painter = ui.painter().with_clip_rect(ctx.pane_rect);
                    #[cfg(test)]
                    {
                        painted_layer = painter.layer_id();
                    }
                    render_color_scales(
                        &painter,
                        clear_of_bottom_chrome(ui.max_rect(), ctx.color_scale_floor),
                        ctx.horizontal_color_scale,
                        ctx.pane_idx,
                        ctx.pane,
                        ctx.overlays,
                        ctx.preferences,
                    );
                }
                _ => match handler.render_mode() {
                    RenderMode::Texture => {
                        // Shared, not mutable: the clickable set is only asked
                        // for if a click needs resolving.
                        let overlays = &*ctx.overlays;
                        selected.extend(overlay_ctx.draw_overlay(
                            ctx.pane.overlay_cache(id),
                            overlays.map_labels(id),
                            || overlays.clickable_items(id, &ctx.pane.layer_ref(ctx.pane_idx, id)),
                        ));
                    }
                    RenderMode::PerFramePoint => {
                        selected.extend(render_per_frame_overlay(
                            ui,
                            projector,
                            &PerFrameOverlayCtx {
                                overlays: ctx.overlays,
                                id,
                                zoom,
                                prefs: ctx.preferences,
                                overlay_click_pos: ctx.overlay_click_pos,
                                excluded_rects: &overlay_excluded_rects,
                                pane_rect: ctx.pane_rect,
                            },
                        ));
                    }
                    _ => {}
                },
            }
            #[cfg(test)]
            ctx.paint_order.push((id.clone(), painted_layer));
        }

        // The deferred stale-image notice, submitted after every kind so
        // nothing in `draw_order` can paint over it. Glass: a floor strip does
        // not draw it — `Gui::draw_volume_glass` does instead.
        if let Some((on_screen, elevation)) = pending_notice
            && ctx.surfaces.paints(Surface::Glass)
        {
            let notice_painter = ui.painter().with_clip_rect(ctx.pane_rect);
            draw_pending_render_notice(
                &notice_painter,
                ctx.pane_rect,
                // The pill row's measured clearance, not the one-row
                // constant: a narrow pane wraps the row.
                crate::ui::pills::pill_row_clearance(ui.ctx(), ctx.pane_idx),
                &on_screen,
                elevation,
            );
        }

        // The other half of the same plate — mutually exclusive with the
        // notice above, so they cannot stack.
        if let Some(source) = melting_layer_caveat
            && ctx.surfaces.paints(Surface::Glass)
        {
            let notice_painter = ui.painter().with_clip_rect(ctx.pane_rect);
            draw_melting_layer_notice(
                &notice_painter,
                ctx.pane_rect,
                crate::ui::pills::pill_row_clearance(ui.ctx(), ctx.pane_idx),
                source,
            );
        }

        if !selected.is_empty() {
            ctx.overlays.selected_overlays = selected;
            ctx.overlays.selected_overlay_page = 0;
            *ctx.click_consumed = true;
        }

        {
            let hover_pos = ui.ctx().pointer_hover_pos();
            ctx.pane.overlay_hover_value = None;
            if let Some(pos) = hover_pos
                && ctx.pane_rect.contains(pos)
                && !ui
                    .ctx()
                    .layer_id_at(pos)
                    .is_some_and(|l| l.order > egui::Order::Background)
            {
                let map_pos = projector.unproject(egui::vec2(pos.x, pos.y));
                let hover_lat = map_pos.y();
                let hover_lon = map_pos.x();
                for id in &draw_order {
                    if ctx.pane.is_overlay_enabled(id)
                        && let Some(text) = ctx.overlays.hover_value_at(
                            id,
                            hover_lat,
                            hover_lon,
                            &ctx.pane.layer_ref(ctx.pane_idx, id),
                        )
                    {
                        ctx.pane.overlay_hover_value = Some(text);
                        break;
                    }
                }
            }
        }

        let screen_rect = ui.max_rect();
        let viewport_bounds = viewport_geo_bounds(projector, screen_rect);
        let qzoom = current_quantized_zoom(zoom);
        // As much overdraw as the adapter's texture limit allows; egui only
        // `debug_assert!`s the bound, so exceeding it is a wgpu validation error.
        let max_texture_side = ui.ctx().input(|i| i.max_texture_side) as u32;
        // In physical pixels, not points: an overlay sized in points is one
        // texel per `ppp²` physical pixels.
        let tex_plan =
            plan_overlay_texture(screen_rect, max_texture_side, ui.ctx().pixels_per_point());
        // The frame's clock, for the settle test: `needs_rerender` calls the
        // gesture settled once the zoom has been still for `SETTLE_REPAINT_DELAY`.
        let now = ui.input(|i| i.time);

        // Whether any overlay on this pane is showing a texture rasterised at a
        // zoom other than the map's — i.e. whether a settle render is still owed.
        let mut settle_owed = false;

        // The live theme, read once per frame and mixed into every overlay's
        // cache token below — a theme flip re-rasterizes on the next frame.
        let is_dark = ui.ctx().global_style().visuals.dark_mode;

        let texture_ids: Vec<LayerId> = ctx
            .overlays
            .handlers()
            .filter(|h| h.render_mode() == RenderMode::Texture)
            .map(|h| h.id())
            .collect();
        for id in &texture_ids {
            // Radar rendering is driven by product/elevation changes (not viewport),
            // handled by dispatch_pane_renders() in the platform crate.
            if *id == known::RADAR {
                continue;
            }
            let enabled = ctx.pane.is_overlay_enabled(id);
            let token = overlay_cache_token(ctx.overlays, ctx.pane_idx, ctx.pane, id, is_dark);
            let has_data = ctx
                .overlays
                .has_data(id, &ctx.pane.layer_ref(ctx.pane_idx, id));
            let cache = ctx.pane.overlay_cache_mut(id);
            // Asked on every frame the overlay is live, and not gated on
            // `render_in_flight`: a skipped frame is missing from the settle clock.
            let stale = enabled
                && has_data
                && cache.needs_rerender(token, zoom, now, &viewport_bounds, &tex_plan);
            if stale && !cache.render_in_flight {
                ctx.actions.push(GuiAction::RenderOverlay {
                    pane_idx: ctx.pane_idx,
                    overlay_kind: id.clone(),
                    geo_bounds: viewport_bounds,
                    texture: tex_plan,
                    data_generation: token,
                    zoom: qzoom,
                });
            }
            // `enabled && has_data` and not just `enabled`. A repaint asked for
            // on a frame that cannot dispatch anything is a 10 Hz wakeup nothing
            // can satisfy.
            if enabled && has_data && cache.zoom_is_stale(zoom) {
                settle_owed = true;
            }
            if !enabled {
                cache.clear();
            }
        }

        // Ask for one more frame while any overlay is still at the wrong zoom.
        if settle_owed {
            ui.ctx()
                .request_repaint_after(crate::overlay_cache::SETTLE_REPAINT_DELAY);
        }
    }

    // Long-press tooltip: show the radar value above the finger. Reached only
    // when the touch pipeline ran this frame (`InteractionState`).
    if let Some(touch_pos) = ctx.long_press_pos
        && ctx.pane_rect.contains(touch_pos)
    {
        let raw_meta = ctx
            .pane
            .overlay_cache(&known::RADAR)
            .and_then(|c| c.current())
            .and_then(|tex| tex.radar_meta.as_ref())
            .map(|m| (m.lat, m.lon, std::sync::Arc::clone(&m.hover)));
        if let Some((lat, lon, hover)) = raw_meta {
            draw_long_press_tooltip(
                ui,
                projector,
                &hover,
                lat,
                lon,
                touch_pos,
                ctx.pane,
                ctx.preferences,
            );
        } else if let Some(img) = ctx.pane.active_image().cloned() {
            draw_long_press_tooltip(
                ui,
                projector,
                &img.hover,
                img.lat,
                img.lon,
                touch_pos,
                ctx.pane,
                ctx.preferences,
            );
        }
    }
}

/// The token a texture overlay's cached raster is keyed by: it moves exactly
/// when the picture would be different.
fn overlay_cache_token(
    overlays: &OverlayRegistry,
    pane_idx: usize,
    pane: &PaneState,
    id: &LayerId,
    is_dark: bool,
) -> u64 {
    let base = if *id == known::RADAR_SITES {
        pane.radar_sites_render_gen
    } else {
        // Pane-aware since WO-M10b: two panes filtering the same layer
        // differently draw different pictures, and one token for both would be
        // one texture for both.
        overlays.content_signature(id, &pane.layer_ref(pane_idx, id))
    };
    let themed = is_dark && overlays.theme_sensitive(id);
    base ^ if themed { 0x9E37_79B9_7F4A_7C15 } else { 0 } ^ as_of_term(overlays, pane, id)
}

/// **The as-of half of the cache token, and it is `0` on a live pane.**
///
/// An [`TimeAxis::EventLifetime`] layer's picture is *which items are valid at
/// the depicted instant*, so a scrubbed pane must not be handed the texture
/// the live pane rasterized. It keys on the instant **quantized** by the
/// layer's own quantum rather than on the raw instant, so dragging the
/// scrubber re-uses rasters instead of minting one per frame.
///
/// Under [`TimeMode::Live`] this is `0` and the token is byte-for-byte what it
/// was before WO-E7c — which is what keeps a live pane's one-second lightning
/// quantum from re-rasterizing it every second. The `Live` fast path also
/// costs one enum test: the registry walk below only runs on a scrubbed pane.
///
/// It is mixed into `data_generation`, which is part of the key
/// `group_overlay_renders` shares one raster across panes on — so two panes on
/// two instants get two rasters without anything else having to know.
fn as_of_term(overlays: &OverlayRegistry, pane: &PaneState, id: &LayerId) -> u64 {
    let TimeMode::AsOf(instant) = pane.time.mode else {
        return 0;
    };
    let Some(handler) = overlays.handlers().find(|h| h.id() == *id) else {
        return 0;
    };
    if !matches!(handler.time_axis(), TimeAxis::EventLifetime) {
        return 0;
    }
    // Hashed rather than mixed raw: the bucket is a small integer and adjacent
    // buckets must not land on adjacent tokens beside a content signature.
    let bucket = crate::pane::as_of_bucket(instant, handler.as_of_quantum());
    let mut hasher = std::hash::DefaultHasher::new();
    std::hash::Hash::hash(&bucket, &mut hasher);
    std::hash::Hasher::finish(&hasher)
}

/// Render the radar image overlay, range ring, and hover tooltip (loop playback
/// path).
fn render_radar_overlay(
    ui: &egui::Ui,
    projector: &walkers::Projector,
    img: &RadarImageData,
    pane: &mut PaneState,
    pane_rect: egui::Rect,
    prefs: &UserPreferences,
) {
    ui.painter().image(
        img.texture.id(),
        crate::overlay_cache::placed_rect(projector, &img.placed),
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        egui::Color32::WHITE,
    );

    render_radar_range_ring(ui, projector, img.lat, img.lon, img.max_range_km);
    update_pane_hover_value_from_meta(
        ui,
        projector,
        &RadarHoverData {
            hover: &img.hover,
            lat: img.lat,
            lon: img.lon,
        },
        pane,
        pane_rect,
        prefs,
    );
}

/// Draw only the range ring for a radar site (used with overlay-cache rendering).
fn render_radar_range_ring(
    ui: &egui::Ui,
    projector: &walkers::Projector,
    lat: f64,
    lon: f64,
    extent_km: f64,
) {
    let radar_center = projector.project(walkers::lat_lon(lat, lon)).to_pos2();
    let north_edge = projector
        .project(walkers::lat_lon(lat + extent_km / KM_PER_DEGREE_LAT, lon))
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

/// The picture's gates and the site they were measured from — what a hover
/// query needs.
struct RadarHoverData<'a> {
    hover: &'a HoverSource,
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

    if ui
        .ctx()
        .layer_id_at(hover_pos)
        .is_some_and(|l| l.order > egui::Order::Background)
    {
        pane.last_hover_pos = None;
        pane.hover_value = None;
        return;
    }

    // Recomputed every frame the pointer is over the pane, stationary or not:
    // `render_pane_map_content` clears `hover_value` at its top.
    pane.last_hover_pos = Some(hover_pos);

    let screen_vec = egui::vec2(hover_pos.x, hover_pos.y);
    let map_pos = projector.unproject(screen_vec);

    pane.hover_value = Some(super::compute_hover_info_raw(
        radar.hover,
        &super::HoverInput {
            site_lat: radar.lat,
            site_lon: radar.lon,
            hover_lat: map_pos.y(),
            hover_lon: map_pos.x(),
        },
        &pane.selected_product(),
        prefs,
    ));
}

/// A hover readout pinned to the pointer, on a layer that cannot claim it.
fn map_hover_tooltip(
    ctx: &egui::Context,
    id: egui::Id,
    pos: egui::Pos2,
    width: Option<f32>,
    content: impl FnOnce(&mut egui::Ui),
) {
    egui::Area::new(id)
        .order(egui::Order::Tooltip)
        .interactable(false)
        .constrain(true)
        .fixed_pos(pos)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_max_width(width.unwrap_or_else(|| ui.spacing().tooltip_width));
                content(ui);
            });
        });
}

/// One radar site that landed near enough to this pane to matter, with the
/// projection already done.
struct VisibleSite {
    /// The row itself, not its position in the table: a table resolved at
    /// runtime can change length, so an index would name a different radar.
    site: &'static RadarSite,
    /// Screen position of the site marker's centre.
    screen: egui::Pos2,
    /// The clickable icon box around `screen`.
    icon_rect: egui::Rect,
}

/// Project the radar site table once, keeping the sites within a 100 px margin
/// of this pane. Empty when the layer is off.
fn visible_radar_sites(
    ui: &egui::Ui,
    projector: &walkers::Projector,
    zoom: f64,
    pane: &PaneState,
) -> Vec<VisibleSite> {
    if !pane.is_overlay_enabled(&known::RADAR_SITES) {
        return Vec::new();
    }
    // The margin is what lets a site just off the edge still draw its label and
    // take a click on the icon straddling the boundary.
    let near = ui.max_rect().expand(100.0);
    let icon_size = (10.0 + zoom as f32 * 2.0).clamp(8.0, 24.0);
    visible_sites_in(
        rustdar_radar::sites::radars(),
        near,
        icon_size,
        |lat, lon| projector.project(walkers::lat_lon(lat, lon)).to_pos2(),
    )
}

/// The walk itself, over whichever table it is handed. The table is an argument
/// rather than a global read so a test can hand it two tables of different
/// lengths.
fn visible_sites_in(
    rows: &'static [RadarSite],
    near: egui::Rect,
    icon_size: f32,
    project: impl Fn(f64, f64) -> egui::Pos2,
) -> Vec<VisibleSite> {
    let mut visible = Vec::new();
    for site in rows {
        let screen = project(site.lat, site.lon);
        if !near.contains(screen) {
            continue;
        }
        visible.push(VisibleSite {
            site,
            screen,
            icon_rect: egui::Rect::from_center_size(screen, egui::vec2(icon_size, icon_size)),
        });
    }
    visible
}

/// Per-frame radar site label rendering and interaction detection.
fn handle_radar_site_interactions(
    ui: &egui::Ui,
    zoom: f64,
    sites: &[VisibleSite],
    ctx: &mut PaneRenderCtx<'_>,
) {
    // Destructuring borrows the fields disjointly, so `pane` and `actions` stay
    // mutable while `excluded_rects` is read.
    let PaneRenderCtx {
        pane,
        actions,
        pane_idx,
        preferences: prefs,
        overlay_click_pos,
        pane_rect,
        excluded_rects,
        click_consumed,
        ..
    } = ctx;
    let pane_idx = *pane_idx;
    let pane_rect = *pane_rect;

    let zoom_f32 = zoom as f32;
    let icon_size = (10.0 + zoom_f32 * 2.0).clamp(8.0, 24.0);
    let font_size = (icon_size * 0.6).clamp(8.0, 12.0);

    let hover_pos = ui.ctx().pointer_hover_pos();
    let click_pos = *overlay_click_pos;

    let is_dark = ui.ctx().global_style().visuals.dark_mode;
    let text_color = if is_dark {
        egui::Color32::WHITE
    } else {
        egui::Color32::BLACK
    };

    for site in sites {
        let radar_site = site.site;
        let site_screen = site.screen;
        let icon_rect = site.icon_rect;

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

        if let Some(pos) = click_pos
            && icon_rect.contains(pos)
            && !is_pos_blocked(ui.ctx(), pos, pane_rect, excluded_rects)
        {
            pane.loading_site = Some(radar_site.name.to_string());
            pane.radar_sites_render_gen = pane.radar_sites_render_gen.wrapping_add(1);
            actions.push(GuiAction::SwitchRadarSite {
                site: radar_site.name.to_string(),
                pane_idx,
            });
            **click_consumed = true;
        }

        if let Some(pos) = hover_pos
            && icon_rect.contains(pos)
            && !is_pos_blocked(ui.ctx(), pos, pane_rect, excluded_rects)
        {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            // The feedhorn, not the ground: it is the figure a published
            // station record quotes as the radar's elevation.
            let elev_str = match radar_site.height_ft(rustdar_radar::sites::Datum::Feedhorn) {
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
            map_hover_tooltip(
                ui.ctx(),
                egui::Id::new(("site_tooltip", radar_site.name)),
                pos,
                None,
                |tooltip_ui| {
                    tooltip_ui.label(tooltip_text);
                },
            );
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
    fix: Option<&rustdar_location::Fix>,
) {
    let user_screen = projector
        .project(walkers::lat_lon(user_lat, user_lon))
        .to_pos2();

    let screen_rect = ui.max_rect();
    if !screen_rect.expand(50.0).contains(user_screen) {
        return;
    }

    let blue = egui::Color32::from_rgb(30, 130, 255);

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

    if let Some(fix) = fix {
        let dot_rect = egui::Rect::from_center_size(user_screen, egui::vec2(28.0, 28.0));
        if let Some(hover_pos) = ui.ctx().pointer_hover_pos()
            && dot_rect.contains(hover_pos)
        {
            map_hover_tooltip(
                ui.ctx(),
                egui::Id::new("gps_fix_tooltip"),
                hover_pos,
                None,
                |tooltip_ui| {
                    tooltip_ui.label(format!(
                        "Lat: {:.5}°  Lon: {:.5}°",
                        fix.point.lat, fix.point.lon
                    ));
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
pub(super) const SCALE_BAR_WIDTH: f32 = 20.0;
/// Margin from pane edge in logical pixels.
const SCALE_MARGIN: f32 = 16.0;
/// Extra margin reserved for the unit title above/beside the bar.
const SCALE_TITLE_MARGIN: f32 = 16.0;
/// Font size for value labels.
const SCALE_FONT_SIZE: f32 = 11.0;
/// Font size for the unit title label.
pub(super) const SCALE_TITLE_FONT_SIZE: f32 = 12.0;
/// Outline offset for text shadow.
const SHADOW_OFFSET: f32 = 1.0;
/// Minimum pixel spacing between labels before thinning kicks in.
const MIN_LABEL_SPACING: f32 = 14.0;
/// Gap between two stacked colour-scale bars, logical pixels: the room the
/// inner one's value labels are read in.
const SCALE_STACK_GAP: f32 = 40.0;
/// How thick a fold marker is across the bar's long axis, logical pixels.
const FOLD_TICK_THICKNESS: f32 = 2.0;
/// How far a fold marker sticks out past each face of the bar, logical pixels.
const FOLD_TICK_OVERHANG: f32 = 3.0;
/// Side of the range-folded key swatch, logical pixels — small enough to stand
/// in the [`SCALE_MARGIN`] past the end of the bar, and nowhere near the
/// 20-point bar width the strip classifier looks for.
const RF_SWATCH_SIZE: f32 = 10.0;
/// What the range-folded swatch is labelled, matching the two-letter form the
/// hydrometeor classification bar already uses for its own folded class.
const RF_SWATCH_LABEL: &str = "RF";
/// Baseline-to-baseline distance for the fold annotation stacked under the unit
/// title, logical pixels — [`SCALE_FONT_SIZE`] plus the shadow's own offset.
const FOLD_TITLE_LINE: f32 = SCALE_FONT_SIZE + SHADOW_OFFSET + 1.0;
/// The gap between a **vertical** bar's inner face and the value labels read
/// against it, logical pixels. Drawn `RIGHT_CENTER` at this offset.
const SCALE_LABEL_GAP: f32 = 4.0;
/// The gap between a **horizontal** bar's top edge and the value labels read
/// against it, logical pixels. Drawn `CENTER_BOTTOM` at this offset.
const SCALE_LABEL_LIFT: f32 = 2.0;
/// How far in from the pane's own edge the fold annotation is hung, logical
/// pixels.
const FOLD_TITLE_INSET: f32 = 2.0;

/// How far in from the pane edge it stands on the colour-scale block reaches,
/// logical pixels — `0.0` when this pane draws no legend at all.
pub(super) fn color_scale_gutter(
    measure: &egui::Painter,
    pane_rect: egui::Rect,
    horizontal: bool,
    pane_idx: usize,
    pane: &PaneState,
    overlays: &OverlayRegistry,
    prefs: &UserPreferences,
) -> f32 {
    // The same gate `Gui::draw_volume_glass` and the `ColorScale` arm put in
    // front of `render_color_scales`: layer off, nothing painted, no gutter.
    if !pane.is_overlay_enabled(&known::COLOR_SCALE) {
        return 0.0;
    }
    let product = pane.selected_product();
    let legend = crate::field_facts::facts(&product).scale;
    if legend.thresholds.len() < 2 {
        return 0.0;
    }
    // And the "pane too small" bail both painters take, restated from the same
    // expressions so a pane that draws no bar reserves no room for one.
    let bar_length = if horizontal {
        pane_rect.width() - SCALE_MARGIN * 2.0
    } else {
        pane_rect.height() - SCALE_MARGIN * 2.0 - SCALE_TITLE_MARGIN
    };
    if bar_length < 40.0 {
        return 0.0;
    }

    // The radar bar stands on the margin; each stacked overlay bar stands one
    // bar-and-gap further in. Every one is measured, not the innermost alone.
    let view = pane.view(pane_idx);
    let ticks = memoized_ticks(measure.ctx(), pane, prefs);
    let mut reach = legend_block_reach(
        measure,
        horizontal,
        0.0,
        &ticks,
        crate::field_facts::unit_label(&product, prefs),
    );
    let mut offset = 0.0;
    for id in pane.draw_order() {
        if *id == known::COLOR_SCALE || !pane.is_overlay_enabled(id) {
            continue;
        }
        let Some(overlay) = overlays.legend(id, &view.layer(id)) else {
            continue;
        };
        if overlay.items.thresholds.len() < 2 {
            continue;
        }
        offset += SCALE_BAR_WIDTH + SCALE_STACK_GAP;
        let ticks = memoized_overlay_ticks(measure.ctx(), id, &overlay);
        reach = reach.max(legend_block_reach(
            measure,
            horizontal,
            offset,
            &ticks,
            overlay.items.unit_label,
        ));
    }
    let mut gutter = SCALE_MARGIN + reach;

    // The legend's second line is hung off the pane's own edge rather than off
    // a bar, so it is a floor under the whole gutter. Read through
    // `legend_second_line`, the same function the painter draws from.
    if !horizontal && let Some(line) = legend_second_line(pane, prefs) {
        gutter = gutter.max(FOLD_TITLE_INSET + laid_out_width(measure, &line, SCALE_FONT_SIZE));
    }
    gutter
}

/// How wide `text` lays out at `size`, logical pixels.
fn laid_out_width(measure: &egui::Painter, text: &str, size: f32) -> f32 {
    measure
        .layout_no_wrap(
            text.to_owned(),
            egui::FontId::proportional(size),
            egui::Color32::WHITE,
        )
        .rect
        .width()
}

/// How far in from the pane edge one bar's block reaches: the bar itself, the
/// value labels read against it, and the unit title centred on it.
fn legend_block_reach(
    measure: &egui::Painter,
    horizontal: bool,
    offset: f32,
    ticks: &[String],
    title: &str,
) -> f32 {
    let past_the_bar = if horizontal {
        let row = measure
            .layout_no_wrap(
                "0".to_owned(),
                egui::FontId::proportional(SCALE_FONT_SIZE),
                egui::Color32::WHITE,
            )
            .rect
            .height();
        SCALE_LABEL_LIFT + row
    } else {
        // Every threshold, not the drawn subset: `MIN_LABEL_SPACING` thinning
        // drops labels a short bar has no room for, and re-deriving which
        // survived would be a second copy of the painter's arithmetic.
        let widest = ticks
            .iter()
            .map(|tick| laid_out_width(measure, tick, SCALE_FONT_SIZE))
            .fold(0.0_f32, f32::max);
        let title = laid_out_width(measure, title, SCALE_TITLE_FONT_SIZE);
        (SCALE_LABEL_GAP + widest).max((title - SCALE_BAR_WIDTH) / 2.0)
    };
    offset + SCALE_BAR_WIDTH + past_the_bar
}

/// The part of `pane_rect` the colour scale has *not* claimed: where a pane's
/// floating chrome may sit without printing through a legend. See
/// [`color_scale_gutter`].
pub(super) fn color_scale_free_rect(
    measure: &egui::Painter,
    pane_rect: egui::Rect,
    horizontal: bool,
    pane_idx: usize,
    pane: &PaneState,
    overlays: &OverlayRegistry,
    prefs: &UserPreferences,
) -> egui::Rect {
    let gutter = color_scale_gutter(
        measure, pane_rect, horizontal, pane_idx, pane, overlays, prefs,
    );
    let mut free = pane_rect;
    if horizontal {
        // Bars along the bottom; the titles sit beside them, on the same edge.
        free.max.y -= gutter;
    } else {
        free.max.x -= gutter;
    }
    // A pane too small for both keeps its own rect rather than an inverted one.
    if free.width() < 1.0 || free.height() < 1.0 {
        return pane_rect;
    }
    free
}

/// The generic tick form: whole numbers bare, one decimal otherwise. Short is
/// the point — a tick label sits in the margin beside a 20px bar.
fn short_tick(value: f32) -> String {
    if value.fract().abs() < 0.01 {
        format!("{value:.0}")
    } else {
        format!("{value:.1}")
    }
}

/// Every value label `render_color_scale` writes beside `product`'s bar, in
/// order, before `MIN_LABEL_SPACING` thinning. The **formatting**, not the
/// per-frame answer: [`memoized_ticks`] calls this on a miss.
pub(super) fn legend_ticks(product: &FieldId, prefs: &UserPreferences) -> Vec<String> {
    crate::field_facts::facts(product)
        .scale
        .thresholds
        .iter()
        .map(|&(value, _)| format_legend_value(product, value, prefs))
        .collect()
}

/// [`legend_ticks`], formatted at most once per preferences change. The version
/// key is the preferences themselves rather than a hash: a collision would show
/// as a bar labelled in the wrong unit.
fn memoized_ticks(
    ctx: &egui::Context,
    pane: &PaneState,
    prefs: &UserPreferences,
) -> std::sync::Arc<Vec<String>> {
    let product = pane.selected_product();
    legend_ramp::labels(
        ctx,
        // The memo key is still (field, prefs); the field half is a `FieldId`
        // rather than the enum since WO-E9e, and `FieldId` hashes by its bytes.
        egui::Id::new(("rustdar::legend_ticks::radar", product.as_str())),
        prefs.clone(),
        || legend_ticks(&product, prefs),
    )
}

/// An overlay bar's value labels, formatted at most once per legend signature.
fn memoized_overlay_ticks(
    ctx: &egui::Context,
    id: &LayerId,
    legend: &Signed<OverlayLegend>,
) -> std::sync::Arc<Vec<String>> {
    legend_ramp::labels(
        ctx,
        egui::Id::new(("rustdar::legend_ticks::overlay", id.as_str())),
        legend.signature,
        || {
            legend
                .items
                .thresholds
                .iter()
                .map(|&(value, _)| format!("{value:.0}"))
                .collect()
        },
    )
}

/// The baked ramp for `pane`'s radar colour bar. Sampled through
/// [`get_color_for_value`] at the legend's own values, so the bar is the
/// palette's answer; the palette is a compile-time table, so the ramp is
/// [`legend_ramp::IMMUTABLE`].
fn radar_ramp(ctx: &egui::Context, pane: &PaneState, horizontal: bool) -> egui::TextureHandle {
    let product = pane.selected_product();
    let scale = crate::field_facts::facts(&product).scale;
    let min = scale.min_value;
    let range = scale.max_value - min;
    // The palette is keyed by the radar layer's own field type, so the id is
    // resolved once here rather than per sample. A field this build does not
    // register has no palette to bake, and the ramp falls back to the default
    // field's — the same fallback `field_facts::facts` takes, for the same
    // reason.
    let ramp_product = radar_fields::product_for(&product)
        .or_else(|| radar_fields::product_for(&radar_fields::known::REFLECTIVITY))
        .expect("the default field is registered by the radar crate");
    legend_ramp::ramp(
        ctx,
        egui::Id::new(("rustdar::legend_ramp::radar", product.as_str(), horizontal)),
        legend_ramp::IMMUTABLE,
        "legend_ramp_radar",
        horizontal,
        |t| {
            let (r, g, b, a) = get_color_for_value(ramp_product, min + t * range);
            [r, g, b, a]
        },
    )
}

/// Format a legend label value. For HHC uses category names; for others, a short numeric string.
///
/// **The conversion is the registry's, the precision is the bar's.** The value
/// is converted by the field's own [`Quantity`](rustdar_units::Quantity) —
/// which is where WO-E9a put the unit each field's numbers live in — while how
/// many decimals survive is a property of a 20 px colour bar, not of the field,
/// so it stays here. The arms below therefore compare field *identity*, which
/// after WO-E9e is a `FieldId` rather than a source's enum; `FieldId` is an
/// open string, so these are comparisons and not `match` patterns.
fn format_legend_value(product: &FieldId, value: f32, prefs: &UserPreferences) -> String {
    use radar_fields::known;

    // The one discrete domain: the RPG's own displayed codes from `hc.lgd`.
    if *product == known::HYDROMETEOR_CLASSIFICATION {
        return match value as u16 {
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
            // `hc.lgd`'s own displayed code for melting snow.
            130 => "MS".into(),
            140 => "UK".into(),
            150 => "RF".into(),
            _ => format!("{value:.0}"),
        };
    }

    let converted = crate::field_facts::facts(product)
        .quantity
        .convert(value, prefs);

    // Speeds and echo tops: whole numbers, in the reader's own unit. Both
    // echo-tops fields are titled off `HeightUnit::kilo_suffix` and read out
    // through `convert_kft_to_kilo`, which is what `Quantity::HeightKft` does.
    if *product == known::VELOCITY
        || *product == known::STORM_RELATIVE_VELOCITY
        || *product == known::SPECTRUM_WIDTH
        || *product == known::ECHO_TOPS
        || *product == known::ECHO_TOPS_INTERPOLATED
    {
        return format!("{converted:.0}");
    }

    if *product == known::PRECIPITATION_RATE {
        return if converted < 1.0 {
            format!("{converted:.2}")
        } else {
            format!("{converted:.1}")
        };
    }

    // The ramp's stops are the NWS quarter-inch reporting steps; the ticks
    // are whatever unit the reader thinks in. Inches keep the generic short
    // form; cm and mm take the unit's own precision, which keeps `25.40`
    // off a 20px bar.
    if *product == known::MAX_EXPECTED_HAIL_SIZE {
        return match prefs.hail_size {
            HailSizeUnit::Inches => short_tick(converted),
            unit => {
                let decimals = unit.decimals();
                format!("{converted:.decimals$}")
            }
        };
    }

    // The remaining arms print the raw value: every one of them is a
    // `Quantity::Unitless` field, whose `convert` is the identity, so
    // `converted` and `value` are the same number here.
    if *product == known::CORRELATION_COEFFICIENT {
        return format!("{value:.2}");
    }
    if *product == known::DIFFERENTIAL_REFLECTIVITY
        || *product == known::SPECIFIC_DIFFERENTIAL_PHASE
    {
        return format!("{value:.1}");
    }
    short_tick(value)
}

// ── Pending-render notice ─────────────────────────────────────────────────

/// Font size of the pending-render notice. The color scale's title size, so the
/// notice reads as part of the same chrome rather than as an alert.
const PENDING_FONT_SIZE: f32 = 12.0;
/// Padding inside the notice's backing plate.
const PENDING_PADDING: egui::Vec2 = egui::vec2(8.0, 3.0);

/// What a pane says while the image on screen is not yet the product and tilt it
/// has selected — the one piece of information nothing else on the pane carries.
fn pending_render_notice(product: &FieldId, elevation: f32) -> String {
    format!(
        "\u{27f3} showing {} {:.1}\u{b0}",
        crate::field_facts::name(product),
        elevation
    )
}

/// Draw the notice across the top of the pane, over the imagery. Non-blocking:
/// the stale image stays fully visible and undimmed. Wrapped rather than
/// clipped — the longest product name is wider than a pane in a six-way split.
pub(super) fn draw_pending_render_notice(
    painter: &egui::Painter,
    pane_rect: egui::Rect,
    top_margin: f32,
    product: &FieldId,
    elevation: f32,
) {
    draw_top_notice(
        painter,
        pane_rect,
        top_margin,
        pending_render_notice(product, elevation),
    );
}

/// What a pane says when the classification on screen is standing on a melting
/// layer nobody measured for the volume it is drawn from — only for the two
/// unmeasured sources, see [`MeltingLayerSource::is_measured`]. Same plate and
/// colour as the pending-render notice, and no icon: every calm enough glyph
/// (`ⓘ`, `ℹ`) is missing from egui's proportional family.
fn melting_layer_notice(source: MeltingLayerSource) -> String {
    source.caption().to_owned()
}

/// Draw the melting-layer qualification across the top of the pane. Shares the
/// pending notice's position and cannot collide with it:
/// [`PaneState::displayed_melting_layer_source`] is gated through
/// `stale_image_on_screen`.
pub(super) fn draw_melting_layer_notice(
    painter: &egui::Painter,
    pane_rect: egui::Rect,
    top_margin: f32,
    source: MeltingLayerSource,
) {
    draw_top_notice(painter, pane_rect, top_margin, melting_layer_notice(source));
}

/// The rounded plate every top-of-pane notice is drawn on. Non-blocking: the
/// imagery stays fully visible and undimmed. Wrapped rather than clipped.
fn draw_top_notice(painter: &egui::Painter, pane_rect: egui::Rect, top_margin: f32, text: String) {
    let font = egui::FontId::proportional(PENDING_FONT_SIZE);
    let wrap_width = (pane_rect.width() - SCALE_MARGIN * 2.0 - PENDING_PADDING.x * 2.0).max(1.0);
    let galley = painter.layout(text, font, egui::Color32::WHITE, wrap_width);
    let plate = egui::Rect::from_center_size(
        egui::pos2(
            pane_rect.center().x,
            pane_rect.top() + top_margin + galley.size().y / 2.0 + PENDING_PADDING.y,
        ),
        galley.size() + PENDING_PADDING * 2.0,
    );
    painter.rect_filled(plate, 4.0, egui::Color32::from_black_alpha(200));
    painter.galley(plate.min + PENDING_PADDING, galley, egui::Color32::WHITE);
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
    painter.text(pos, anchor, text, font, egui::Color32::WHITE);
}

/// Every colour-scale legend a pane shows: the radar product's own bar, and
/// one more for each enabled overlay that carries a legend of its own.
pub(super) fn render_color_scales(
    painter: &egui::Painter,
    pane_rect: egui::Rect,
    horizontal: bool,
    pane_idx: usize,
    pane: &PaneState,
    overlays: &OverlayRegistry,
    prefs: &UserPreferences,
) {
    render_color_scale(painter, pane_rect, horizontal, pane, prefs);
    render_overlay_color_scales(painter, pane_rect, horizontal, pane_idx, pane, overlays);
}

/// The part of `pane_rect` the colour-scale legend may draw in: the pane, less
/// whatever the phone shell's bottom bar covers.
pub(super) fn clear_of_bottom_chrome(pane_rect: egui::Rect, floor: f32) -> egui::Rect {
    if !floor.is_finite() || floor >= pane_rect.bottom() {
        return pane_rect;
    }
    egui::Rect::from_min_max(
        pane_rect.min,
        egui::pos2(pane_rect.right(), floor.max(pane_rect.top())),
    )
}

pub(super) fn render_color_scale(
    painter: &egui::Painter,
    pane_rect: egui::Rect,
    horizontal: bool,
    pane: &PaneState,
    prefs: &UserPreferences,
) {
    let product = pane.selected_product();
    let legend = crate::field_facts::facts(&product).scale;
    if legend.thresholds.len() < 2 {
        return;
    }

    // Orientation follows the map panel's shape, not the platform: a portrait
    // panel gets horizontal bars along the bottom, a landscape one vertical
    // bars on the right. See `pane::ColorScaleOrientation`.
    let bar_length = if horizontal {
        pane_rect.width() - SCALE_MARGIN * 2.0
    } else {
        pane_rect.height() - SCALE_MARGIN * 2.0 - SCALE_TITLE_MARGIN
    };

    if bar_length < 40.0 {
        return; // pane too small
    }

    let bar_rect = if horizontal {
        let left = pane_rect.left() + SCALE_MARGIN;
        let bottom = pane_rect.bottom() - SCALE_MARGIN;
        let top = bottom - SCALE_BAR_WIDTH;
        egui::Rect::from_min_max(egui::pos2(left, top), egui::pos2(left + bar_length, bottom))
    } else {
        // Vertical bar along the right, origin at bottom-right
        let right = pane_rect.right() - SCALE_MARGIN;
        let left = right - SCALE_BAR_WIDTH;
        let bottom = pane_rect.bottom() - SCALE_MARGIN;
        let top = bottom - bar_length;
        egui::Rect::from_min_max(egui::pos2(left, top), egui::pos2(right, bottom))
    };

    let min_val = legend.min_value;
    let max_val = legend.max_value;
    let range = max_val - min_val;
    if range.abs() < f32::EPSILON {
        return;
    }

    let n = legend.thresholds.len();

    if legend.is_gradient {
        // Gradient scales: one image over a ramp baked once per product.
        // See `crate::legend_ramp`.
        painter.image(
            radar_ramp(painter.ctx(), pane, horizontal).id(),
            bar_rect,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );
    } else {
        // Discrete scales: equal-sized blocks, one per threshold. Left as
        // blocks on purpose: these are hard edges at exact fractions of the
        // bar, and a stretched `NEAREST` texture would move each boundary.
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

    // --- Fold markers: where the picture on the glass wraps ---
    let folds_at = pane
        .displayed_nyquist_ms()
        .filter(|ms| ms.is_finite() && *ms > 0.0);
    if let Some(nyquist_ms) = folds_at {
        for value in fold_marker_positions(nyquist_ms as f32, min_val, max_val)
            .into_iter()
            .flatten()
        {
            let t = (value - min_val) / range;
            let marker = if horizontal {
                egui::Rect::from_min_size(
                    egui::pos2(
                        bar_rect.left() + t * bar_rect.width() - FOLD_TICK_THICKNESS / 2.0,
                        bar_rect.top() - FOLD_TICK_OVERHANG,
                    ),
                    egui::vec2(
                        FOLD_TICK_THICKNESS,
                        SCALE_BAR_WIDTH + FOLD_TICK_OVERHANG * 2.0,
                    ),
                )
            } else {
                egui::Rect::from_min_size(
                    egui::pos2(
                        bar_rect.left() - FOLD_TICK_OVERHANG,
                        bar_rect.bottom() - t * bar_rect.height() - FOLD_TICK_THICKNESS / 2.0,
                    ),
                    egui::vec2(
                        SCALE_BAR_WIDTH + FOLD_TICK_OVERHANG * 2.0,
                        FOLD_TICK_THICKNESS,
                    ),
                )
            };
            // The same dark backing `draw_shadowed_text` gives every label on
            // this bar: a bare white line reads as a highlight over mid green.
            painter.rect_filled(
                marker.expand(1.0),
                0.0,
                egui::Color32::from_black_alpha(200),
            );
            painter.rect_filled(marker, 0.0, egui::Color32::WHITE);
        }
    }

    let label_font = egui::FontId::proportional(SCALE_FONT_SIZE);
    let title_font = egui::FontId::proportional(SCALE_TITLE_FONT_SIZE);

    let mut label_positions: Vec<(f32, &str)> = Vec::new();
    let tick_text = memoized_ticks(painter.ctx(), pane, prefs);
    for ((i, &(val, _)), text) in legend.thresholds.iter().enumerate().zip(tick_text.iter()) {
        let pixel_pos = if legend.is_gradient {
            let t = (val - min_val) / range;
            if horizontal {
                bar_rect.left() + t * bar_rect.width()
            } else {
                bar_rect.bottom() - t * bar_rect.height()
            }
        } else {
            let t = i as f32 / n as f32;
            if horizontal {
                bar_rect.left() + t * bar_rect.width()
            } else {
                bar_rect.bottom() - t * bar_rect.height()
            }
        };
        label_positions.push((pixel_pos, text));
    }

    let mut prev_pos: Option<f32> = None;
    let thinned: Vec<(f32, &str)> = label_positions
        .iter()
        .filter(|(pos, _)| {
            if let Some(prev) = prev_pos
                && (pos - prev).abs() < MIN_LABEL_SPACING
            {
                return false;
            }
            prev_pos = Some(*pos);
            true
        })
        .copied()
        .collect();

    for (pixel_pos, text) in &thinned {
        if horizontal {
            let pos = egui::pos2(*pixel_pos, bar_rect.top() - SCALE_LABEL_LIFT);
            draw_shadowed_text(
                painter,
                pos,
                egui::Align2::CENTER_BOTTOM,
                text,
                label_font.clone(),
            );
        } else {
            let pos = egui::pos2(bar_rect.left() - SCALE_LABEL_GAP, *pixel_pos);
            draw_shadowed_text(
                painter,
                pos,
                egui::Align2::RIGHT_CENTER,
                text,
                label_font.clone(),
            );
        }
    }

    // --- Title: unit label above the bar (desktop) or under it (mobile),
    //     with velocity's fold annotation on the line after it ---
    let unit = crate::field_facts::unit_label(&product, prefs);
    let fold_line = legend_second_line(pane, prefs);
    if horizontal {
        // Under the bar's left end, reading left to right: `mph  folds ±50`.
        let title_pos = egui::pos2(pane_rect.left() + 2.0, bar_rect.bottom() + 1.0);
        draw_shadowed_text(
            painter,
            title_pos,
            egui::Align2::LEFT_TOP,
            unit,
            title_font.clone(),
        );
        if let Some(line) = &fold_line {
            // Measured rather than reserved, because the gap between the two
            // has to look the same after `m/s` as after `km/h`.
            let unit_width = painter
                .layout_no_wrap(unit.to_owned(), title_font, egui::Color32::WHITE)
                .rect
                .width();
            draw_shadowed_text(
                painter,
                title_pos + egui::vec2(unit_width + 6.0, 0.0),
                egui::Align2::LEFT_TOP,
                line,
                label_font.clone(),
            );
        }
    } else {
        // Two lines stacked above the bar, unit on top. `SCALE_TITLE_MARGIN`
        // reserves 16 points and the pane's edge gives the second line 16 more.
        let stacked = fold_line.as_ref().map_or(0.0, |_| FOLD_TITLE_LINE);
        let title_pos = egui::pos2(bar_rect.center().x, bar_rect.top() - 4.0 - stacked);
        draw_shadowed_text(
            painter,
            title_pos,
            egui::Align2::CENTER_BOTTOM,
            unit,
            title_font,
        );
        if let Some(line) = &fold_line {
            // Hung off the pane's own edge rather than centred on the bar:
            // `folds ±229` is 52 points over a 20-point bar 16 points in.
            draw_shadowed_text(
                painter,
                egui::pos2(pane_rect.right() - FOLD_TITLE_INSET, bar_rect.top() - 4.0),
                egui::Align2::RIGHT_BOTTOM,
                line,
                label_font.clone(),
            );
        }
    }

    // --- The range-folded key ---
    if range_folded_is_painted(&product, pane) {
        // In both orientations the key stands past the end of the bar, in the
        // pane's bottom-right corner, label reading outward from the swatch —
        // a label on the bar's own side prints through the ±80 tick.
        let (swatch, label_pos, label_anchor) = if horizontal {
            let swatch = egui::Rect::from_min_size(
                egui::pos2(
                    bar_rect.right() + (SCALE_MARGIN - RF_SWATCH_SIZE) / 2.0,
                    bar_rect.center().y - RF_SWATCH_SIZE / 2.0,
                ),
                egui::Vec2::splat(RF_SWATCH_SIZE),
            );
            (
                swatch,
                egui::pos2(swatch.center().x, swatch.bottom() + 1.0),
                egui::Align2::CENTER_TOP,
            )
        } else {
            let swatch = egui::Rect::from_min_size(
                egui::pos2(
                    bar_rect.center().x - RF_SWATCH_SIZE / 2.0,
                    bar_rect.bottom() + (SCALE_MARGIN - RF_SWATCH_SIZE) / 2.0,
                ),
                egui::Vec2::splat(RF_SWATCH_SIZE),
            );
            (
                swatch,
                egui::pos2(swatch.right() + 3.0, swatch.center().y),
                egui::Align2::LEFT_CENTER,
            )
        };
        let (r, g, b, a) = rustdar_radar::RANGE_FOLDED;
        painter.rect_filled(
            swatch,
            0.0,
            egui::Color32::from_rgba_unmultiplied(r, g, b, a),
        );
        draw_shadowed_text(
            painter,
            label_pos,
            label_anchor,
            RF_SWATCH_LABEL,
            label_font,
        );
    }
}

/// Which ends of the fold, in the ramp's own m/s domain, have a place on the
/// bar — both, or neither.
fn fold_marker_positions(nyquist_ms: f32, min_val: f32, max_val: f32) -> Option<[f32; 2]> {
    if !nyquist_ms.is_finite() || nyquist_ms <= 0.0 {
        return None;
    }
    if -nyquist_ms < min_val || nyquist_ms > max_val {
        return None;
    }
    Some([-nyquist_ms, nyquist_ms])
}

/// Where this pane's picture folds, in the unit the reader chose — the legend's
/// second line. Converted through `rustdar-units`, which moves neither the ramp
/// nor the marker: those are positioned in the palette's own m/s domain.
fn fold_title_line(nyquist_ms: f64, prefs: &UserPreferences) -> String {
    let converted = prefs.speed.convert_from_ms(nyquist_ms as f32);
    format!("folds \u{b1}{converted:.0}")
}

/// What this pane's storm-relative picture was shifted by: the vector, then one
/// short word for where it came from — `SRM 32 kt @ 240\u{b0} (NWS)`. See
/// [`rustdar_radar::srv::StormMotionSource::tag`]. The direction is a compass
/// bearing and stays in degrees, three digits wide.
fn srm_title_line(motion: rustdar_radar::srv::SrvMotion, prefs: &UserPreferences) -> String {
    let speed = prefs.speed.convert_from_knots(motion.speed_kt);
    format!(
        "SRM {speed:.0} {} @ {:03.0}\u{b0} ({})",
        prefs.speed.suffix(),
        motion.direction_deg,
        motion.source.tag(),
    )
}

/// The legend's second line — under the unit title on a right-edge bar and
/// after it on a bottom-edge one — or `None`.
fn legend_second_line(pane: &PaneState, prefs: &UserPreferences) -> Option<String> {
    if let Some(nyquist_ms) = pane
        .displayed_nyquist_ms()
        .filter(|ms| ms.is_finite() && *ms > 0.0)
    {
        return Some(fold_title_line(nyquist_ms, prefs));
    }
    pane.displayed_storm_motion()
        .filter(|motion| motion.speed_kt.is_finite() && motion.direction_deg.is_finite())
        .map(|motion| srm_title_line(motion, prefs))
}

/// Whether the purple [`rustdar_radar::RANGE_FOLDED`] can appear in this pane's
/// picture, and therefore needs a key beside it.
fn range_folded_is_painted(product: &FieldId, pane: &PaneState) -> bool {
    // SRV is rasterized from `srv::compute_srv_grid`'s finished `f32` field,
    // whose NaNs are skipped, so there is no purple on an SRV raster to key.
    //
    // A comparison rather than a `matches!`: a `FieldId` is an open string, so
    // its consts are not patterns. The two fields named are the same two.
    (*product == radar_fields::known::VELOCITY || *product == radar_fields::known::SPECTRUM_WIDTH)
        && pane.is_map()
}

/// Render color scale legends for overlay layers that provide their own legend
/// (e.g. model data CIN). Drawn to the left of the radar color scale.
fn render_overlay_color_scales(
    painter: &egui::Painter,
    pane_rect: egui::Rect,
    // Same panel-wide orientation as the radar color scale.
    horizontal: bool,
    pane_idx: usize,
    pane: &PaneState,
    overlays: &OverlayRegistry,
) {
    let view = pane.view(pane_idx);
    // Offset each overlay legend to the left of (vertical) or above
    // (horizontal) the radar scale.
    let mut bar_offset = 0;

    for id in pane.draw_order() {
        if !pane.is_overlay_enabled(id) || *id == known::COLOR_SCALE {
            continue;
        }
        let Some(legend) = overlays.legend(id, &view.layer(id)) else {
            continue;
        };
        if legend.items.thresholds.len() < 2 {
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
            egui::Rect::from_min_max(egui::pos2(left, top), egui::pos2(left + bar_length, bottom))
        } else {
            let right = pane_rect.right() - SCALE_MARGIN - offset_px;
            let left = right - SCALE_BAR_WIDTH;
            let bottom = pane_rect.bottom() - SCALE_MARGIN;
            let top = bottom - bar_length;
            egui::Rect::from_min_max(egui::pos2(left, top), egui::pos2(right, bottom))
        };

        let min_val = legend.items.min_value;
        let max_val = legend.items.max_value;
        let range = max_val - min_val;
        if range.abs() < f32::EPSILON {
            continue;
        }

        // Always gradient for overlay legends — one image over a ramp baked
        // once per legend signature. See `crate::legend_ramp`.
        let thresholds = &legend.items.thresholds;
        painter.image(
            legend_ramp::ramp(
                painter.ctx(),
                egui::Id::new(("rustdar::legend_ramp::overlay", id.as_str(), horizontal)),
                legend.signature,
                "legend_ramp_overlay",
                horizontal,
                |t| {
                    let [r, g, b] = interpolate_legend_color(thresholds, min_val + t * range);
                    [r, g, b, 255]
                },
            )
            .id(),
            bar_rect,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );

        let label_font = egui::FontId::proportional(SCALE_FONT_SIZE);
        let title_font = egui::FontId::proportional(SCALE_TITLE_FONT_SIZE);

        let tick_text = memoized_overlay_ticks(painter.ctx(), id, &legend);
        let mut label_positions: Vec<(f32, &str)> = Vec::new();
        for (&(val, _), text) in legend.items.thresholds.iter().zip(tick_text.iter()) {
            let t = (val - min_val) / range;
            let pixel_pos = if horizontal {
                bar_rect.left() + t * bar_rect.width()
            } else {
                bar_rect.bottom() - t * bar_rect.height()
            };
            label_positions.push((pixel_pos, text));
        }

        let mut prev_pos: Option<f32> = None;
        let thinned: Vec<(f32, &str)> = label_positions
            .iter()
            .filter(|(pos, _)| {
                if let Some(prev) = prev_pos
                    && (pos - prev).abs() < MIN_LABEL_SPACING
                {
                    return false;
                }
                prev_pos = Some(*pos);
                true
            })
            .copied()
            .collect();

        for (pixel_pos, text) in &thinned {
            if horizontal {
                let pos = egui::pos2(*pixel_pos, bar_rect.top() - SCALE_LABEL_LIFT);
                draw_shadowed_text(
                    painter,
                    pos,
                    egui::Align2::CENTER_BOTTOM,
                    text,
                    label_font.clone(),
                );
            } else {
                let pos = egui::pos2(bar_rect.left() - SCALE_LABEL_GAP, *pixel_pos);
                draw_shadowed_text(
                    painter,
                    pos,
                    egui::Align2::RIGHT_CENTER,
                    text,
                    label_font.clone(),
                );
            }
        }

        let unit = legend.items.unit_label;
        if horizontal {
            // Under its own bar, for the reason the radar bar's title is: 12
            // points is not enough to lay `kg/m²` out in, and the pane's clip
            // rect turns the shortfall into a cut-off label.
            let title_pos = egui::pos2(pane_rect.left() + 2.0, bar_rect.bottom() + 1.0);
            draw_shadowed_text(painter, title_pos, egui::Align2::LEFT_TOP, unit, title_font);
        } else {
            let title_pos = egui::pos2(bar_rect.center().x, bar_rect.top() - 4.0);
            draw_shadowed_text(
                painter,
                title_pos,
                egui::Align2::CENTER_BOTTOM,
                unit,
                title_font,
            );
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
    id: &'a LayerId,
    zoom: f64,
    prefs: &'a UserPreferences,
    /// Pre-filtered click position (dialog clicks already stripped).
    /// See `PaneRenderCtx::overlay_click_pos` and the pre-filter in `ui_map.rs`.
    overlay_click_pos: Option<egui::Pos2>,
    excluded_rects: &'a [egui::Rect],
    pane_rect: egui::Rect,
}

/// Per-frame rendering for point overlays (e.g. METAR station model plots).
fn render_per_frame_overlay(
    ui: &egui::Ui,
    projector: &walkers::Projector,
    pf: &PerFrameOverlayCtx<'_>,
) -> Vec<Arc<dyn OverlayItem>> {
    let points = pf.overlays.per_frame_points(pf.id);
    if points.is_empty() {
        return Vec::new();
    }

    let zoom_f32 = pf.zoom as f32;
    let is_dark = ui.ctx().global_style().visuals.dark_mode;
    let draw_ctx = DrawPointContext {
        zoom: zoom_f32,
        is_dark,
    };
    let hit_radius = pf.overlays.point_hit_radius(pf.id, zoom_f32);
    let hover_ctx = HoverContext { prefs: pf.prefs };

    let screen_rect = ui.max_rect();
    let margin = hit_radius + 40.0; // extra margin for station model elements
    let expanded = screen_rect.expand(margin);
    // Pre-compute viewport geo-bounds (with margin) so we can skip the
    // expensive Mercator projection for points that are clearly off-screen.
    let geo_bounds = viewport_geo_bounds(projector, expanded);

    let painter = ui.painter();

    // Blocked-ness is a property of the *position*, not of the point tested
    // against it, so it is settled once here. Per-station it was ~41,000 rect
    // tests and 200 egui memory-lock acquisitions per pane per frame.
    let blocked = |pos: egui::Pos2| is_pos_blocked(ui.ctx(), pos, pf.pane_rect, pf.excluded_rects);
    let hover_pos = ui.ctx().pointer_hover_pos().filter(|&p| !blocked(p));
    let click_pos = pf.overlay_click_pos.filter(|&p| !blocked(p));

    let mut selected = Vec::new();
    let mut closest_hover: Option<(f32, u32)> = None; // (distance², id)

    for pt in points {
        // Fast geo-bounds rejection before the costly projection.
        if !geo_bounds.contains_point(pt.lat, pt.lon) {
            continue;
        }

        let screen = projector
            .project(walkers::lat_lon(pt.lat, pt.lon))
            .to_pos2();

        if !expanded.contains(screen) {
            continue;
        }

        let mut ep = EguiPointPainter {
            painter,
            center: screen,
        };
        pf.overlays.draw_point(pf.id, pt.id, &mut ep, &draw_ctx);

        // Click detection — layer blocking already applied by pre-filter in ui_map.rs.
        if let Some(click_pos) = click_pos {
            let dx = click_pos.x - screen.x;
            let dy = click_pos.y - screen.y;
            if dx * dx + dy * dy <= hit_radius * hit_radius {
                selected.push(pt.selection.clone());
            }
        }

        // Hover detection — a blocked cursor was already dropped above.
        if let Some(hp) = hover_pos {
            let dx = hp.x - screen.x;
            let dy = hp.y - screen.y;
            let d2 = dx * dx + dy * dy;
            if d2 <= hit_radius * hit_radius
                && closest_hover.is_none_or(|(best_d2, _)| d2 < best_d2)
            {
                closest_hover = Some((d2, pt.id));
            }
        }
    }

    if let Some((_, id)) = closest_hover
        && let Some(hp) = hover_pos
        && let Some(text) = pf.overlays.hover_text(pf.id, id, &hover_ctx)
    {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        map_hover_tooltip(
            ui.ctx(),
            egui::Id::new(("per_frame_overlay_hover", pf.id.as_str())),
            hp,
            Some(400.0),
            |tooltip_ui| {
                tooltip_ui.label(text);
            },
        );
    }

    selected
}

/// Vertical offset (points) from the touch point to the tooltip centre, so the
/// tooltip sits above the finger rather than under it.
const TOOLTIP_OFFSET_Y: f32 = 60.0;

/// Draw a floating tooltip above the finger during a long press, showing the
/// radar value at the touched position. Reached only from the touch pipeline.
#[allow(clippy::too_many_arguments)]
fn draw_long_press_tooltip(
    ui: &egui::Ui,
    projector: &walkers::Projector,
    hover: &HoverSource,
    lat: f64,
    lon: f64,
    touch_pos: egui::Pos2,
    pane: &PaneState,
    prefs: &UserPreferences,
) {
    // The same question the pointer readout asks, through the same two
    // functions: where is this point from the radar, and what did the render
    // paint there.
    let map_pos = projector.unproject(egui::vec2(touch_pos.x, touch_pos.y));
    let (azimuth, ground_km) =
        rustdar_geo::site_bearing_range_km(lat, lon, map_pos.y(), map_pos.x());

    let text = match hover.read(azimuth, ground_km) {
        Reading::Value(value) => {
            crate::field_facts::format_value(&pane.selected_product(), value, prefs)
        }
        Reading::Unpainted => "No data".to_string(),
        Reading::NotResident => "No value held for this frame".to_string(),
    };

    let tooltip_pos = egui::pos2(touch_pos.x, touch_pos.y - TOOLTIP_OFFSET_Y);

    let painter = ui.painter();
    let font = egui::FontId::proportional(14.0);
    let galley = painter.layout_no_wrap(text, font, egui::Color32::WHITE);
    let text_size = galley.size();
    let padding = egui::vec2(8.0, 4.0);
    let bg_rect = egui::Rect::from_center_size(tooltip_pos, text_size + padding * 2.0);

    painter.rect_filled(bg_rect, 4.0, egui::Color32::from_black_alpha(200));
    painter.galley(bg_rect.min + padding, galley, egui::Color32::WHITE);
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustdar_units::SpeedUnit;

    /// The ticks `render_color_scale` would paint for `product`, in order.
    fn ticks(product: &FieldId, prefs: &UserPreferences) -> Vec<String> {
        legend_ticks(product, prefs)
    }

    /// Every legend tick string and every colour-bar unit title, for every
    /// registered field under three unit preferences, against literals
    /// **captured from the build before WO-E9e re-keyed this file**.
    ///
    /// The order's Reopen-1:1 rule is that a bar's labels render byte-identical
    /// across the move off the product enum, and this is the only thing that
    /// can say so: the conversion now runs through the field's own
    /// [`rustdar_units::Quantity::convert`] instead of a hand-written arm per
    /// product, and the unit title through `Quantity::suffix` instead of the
    /// enum's `unit_label`. The expectations below were **measured** at
    /// `ed5a1f9b` and pasted in, not derived from the new code, so a formula
    /// that is consistently wrong in both spellings cannot pass.
    #[test]
    fn every_tick_and_unit_string_is_what_it_was_before_the_field_ids() {
        let sets: [(&str, UserPreferences); 3] = [
            ("default", UserPreferences::default()),
            (
                "metric",
                UserPreferences {
                    speed: SpeedUnit::MetersPerSec,
                    height: rustdar_units::HeightUnit::Meters,
                    precip_rate: rustdar_units::PrecipRateUnit::MillimetersPerHour,
                    hail_size: HailSizeUnit::Centimeters,
                    ..UserPreferences::default()
                },
            ),
            (
                "mm",
                UserPreferences {
                    hail_size: HailSizeUnit::Millimeters,
                    ..UserPreferences::default()
                },
            ),
        ];
        // (preference set, field id, the ticks joined by `|`, the unit title)
        const EXPECTED: [(&str, &str, &str, &str); 51] = [
            (
                "default",
                "Reflectivity",
                "0|2.5|5|7.5|10|15|20|25|30|35|40|45|50|55|60|65|70|75|80|85|90|95",
                "dBZ",
            ),
            (
                "default",
                "Velocity",
                "-81|-69|-58|-46|-35|-23|-12|-0|0|12|23|35|46|58|69|81",
                "mph",
            ),
            ("default", "SpectrumWidth", "0|5|9|14|18|23", "mph"),
            (
                "default",
                "DifferentialPhase",
                "0|15|30|45|60|75|90|105|120|135|150|165|180|195|210|225|240|255|270|285|300|315|330|345",
                "°",
            ),
            (
                "default",
                "CorrelationCoefficient",
                "0.45|0.55|0.75|0.80|0.90|0.96|0.98",
                "CC",
            ),
            (
                "default",
                "DifferentialReflectivity",
                "-2.0|-1.0|0.0|0.2|1.0|1.5|2.0|2.5|3.0|4.0|5.0|5.5",
                "dB",
            ),
            (
                "default",
                "StormRelativeVelocity",
                "-81|-69|-58|-46|-35|-23|-12|-0|0|12|23|35|46|58|69|81",
                "mph",
            ),
            (
                "default",
                "SpecificDifferentialPhase",
                "-2.0|-1.0|-0.5|0.0|1.0|1.5|2.0|2.5|3.0|4.0|5.0|6.0|6.5",
                "°/km",
            ),
            (
                "default",
                "EchoTops",
                "5|10|15|20|25|30|35|40|45|50|55|60",
                "kft",
            ),
            (
                "default",
                "EchoTopsInterpolated",
                "5|10|15|20|25|30|35|40|45|50|55|60",
                "kft",
            ),
            (
                "default",
                "VerticallyIntegratedLiquid",
                "1|5|10|15|20|25|30|35|40|50|60",
                "kg/m²",
            ),
            (
                "default",
                "VilDensity",
                "0.5|1|1.5|2|2.5|3|3.5|4|4.5|5|6",
                "g/m³",
            ),
            (
                "default",
                "ProbabilityOfSevereHail",
                "10|20|30|40|50|60|70|80|90|100",
                "%",
            ),
            (
                "default",
                "MaxExpectedHailSize",
                "0.2|0.5|0.8|1|1.2|1.5|1.8|2|2.5|3|3.5|4",
                "in",
            ),
            (
                "default",
                "HydrometeorClassification",
                "Bio|AP|IC|DS|WS|RA|HR|BD|GR|HA|LH|GH|MS|UK|RF",
                "HHC",
            ),
            (
                "default",
                "PrecipitationRate",
                "0.01|0.10|0.25|0.50|1.0|2.0|3.0|4.0|6.0|8.0|12.0",
                "in/hr",
            ),
            (
                "default",
                "NormalizedRotation",
                "-2|-1|-1.0|-0.2|0.2|1.0|1|1.5|1.5|2.0|2|2.5|2.5|3.0|3",
                "NROT",
            ),
            (
                "metric",
                "Reflectivity",
                "0|2.5|5|7.5|10|15|20|25|30|35|40|45|50|55|60|65|70|75|80|85|90|95",
                "dBZ",
            ),
            (
                "metric",
                "Velocity",
                "-36|-31|-26|-21|-15|-10|-5|-0|0|5|10|15|21|26|31|36",
                "m/s",
            ),
            ("metric", "SpectrumWidth", "0|2|4|6|8|10", "m/s"),
            (
                "metric",
                "DifferentialPhase",
                "0|15|30|45|60|75|90|105|120|135|150|165|180|195|210|225|240|255|270|285|300|315|330|345",
                "°",
            ),
            (
                "metric",
                "CorrelationCoefficient",
                "0.45|0.55|0.75|0.80|0.90|0.96|0.98",
                "CC",
            ),
            (
                "metric",
                "DifferentialReflectivity",
                "-2.0|-1.0|0.0|0.2|1.0|1.5|2.0|2.5|3.0|4.0|5.0|5.5",
                "dB",
            ),
            (
                "metric",
                "StormRelativeVelocity",
                "-36|-31|-26|-21|-15|-10|-5|-0|0|5|10|15|21|26|31|36",
                "m/s",
            ),
            (
                "metric",
                "SpecificDifferentialPhase",
                "-2.0|-1.0|-0.5|0.0|1.0|1.5|2.0|2.5|3.0|4.0|5.0|6.0|6.5",
                "°/km",
            ),
            ("metric", "EchoTops", "2|3|5|6|8|9|11|12|14|15|17|18", "km"),
            (
                "metric",
                "EchoTopsInterpolated",
                "2|3|5|6|8|9|11|12|14|15|17|18",
                "km",
            ),
            (
                "metric",
                "VerticallyIntegratedLiquid",
                "1|5|10|15|20|25|30|35|40|50|60",
                "kg/m²",
            ),
            (
                "metric",
                "VilDensity",
                "0.5|1|1.5|2|2.5|3|3.5|4|4.5|5|6",
                "g/m³",
            ),
            (
                "metric",
                "ProbabilityOfSevereHail",
                "10|20|30|40|50|60|70|80|90|100",
                "%",
            ),
            (
                "metric",
                "MaxExpectedHailSize",
                "0.6|1.3|1.9|2.5|3.2|3.8|4.4|5.1|6.3|7.6|8.9|10.2",
                "cm",
            ),
            (
                "metric",
                "HydrometeorClassification",
                "Bio|AP|IC|DS|WS|RA|HR|BD|GR|HA|LH|GH|MS|UK|RF",
                "HHC",
            ),
            (
                "metric",
                "PrecipitationRate",
                "0.25|2.5|6.3|12.7|25.4|50.8|76.2|101.6|152.4|203.2|304.8",
                "mm/hr",
            ),
            (
                "metric",
                "NormalizedRotation",
                "-2|-1|-1.0|-0.2|0.2|1.0|1|1.5|1.5|2.0|2|2.5|2.5|3.0|3",
                "NROT",
            ),
            (
                "mm",
                "Reflectivity",
                "0|2.5|5|7.5|10|15|20|25|30|35|40|45|50|55|60|65|70|75|80|85|90|95",
                "dBZ",
            ),
            (
                "mm",
                "Velocity",
                "-81|-69|-58|-46|-35|-23|-12|-0|0|12|23|35|46|58|69|81",
                "mph",
            ),
            ("mm", "SpectrumWidth", "0|5|9|14|18|23", "mph"),
            (
                "mm",
                "DifferentialPhase",
                "0|15|30|45|60|75|90|105|120|135|150|165|180|195|210|225|240|255|270|285|300|315|330|345",
                "°",
            ),
            (
                "mm",
                "CorrelationCoefficient",
                "0.45|0.55|0.75|0.80|0.90|0.96|0.98",
                "CC",
            ),
            (
                "mm",
                "DifferentialReflectivity",
                "-2.0|-1.0|0.0|0.2|1.0|1.5|2.0|2.5|3.0|4.0|5.0|5.5",
                "dB",
            ),
            (
                "mm",
                "StormRelativeVelocity",
                "-81|-69|-58|-46|-35|-23|-12|-0|0|12|23|35|46|58|69|81",
                "mph",
            ),
            (
                "mm",
                "SpecificDifferentialPhase",
                "-2.0|-1.0|-0.5|0.0|1.0|1.5|2.0|2.5|3.0|4.0|5.0|6.0|6.5",
                "°/km",
            ),
            (
                "mm",
                "EchoTops",
                "5|10|15|20|25|30|35|40|45|50|55|60",
                "kft",
            ),
            (
                "mm",
                "EchoTopsInterpolated",
                "5|10|15|20|25|30|35|40|45|50|55|60",
                "kft",
            ),
            (
                "mm",
                "VerticallyIntegratedLiquid",
                "1|5|10|15|20|25|30|35|40|50|60",
                "kg/m²",
            ),
            (
                "mm",
                "VilDensity",
                "0.5|1|1.5|2|2.5|3|3.5|4|4.5|5|6",
                "g/m³",
            ),
            (
                "mm",
                "ProbabilityOfSevereHail",
                "10|20|30|40|50|60|70|80|90|100",
                "%",
            ),
            (
                "mm",
                "MaxExpectedHailSize",
                "6|13|19|25|32|38|44|51|64|76|89|102",
                "mm",
            ),
            (
                "mm",
                "HydrometeorClassification",
                "Bio|AP|IC|DS|WS|RA|HR|BD|GR|HA|LH|GH|MS|UK|RF",
                "HHC",
            ),
            (
                "mm",
                "PrecipitationRate",
                "0.01|0.10|0.25|0.50|1.0|2.0|3.0|4.0|6.0|8.0|12.0",
                "in/hr",
            ),
            (
                "mm",
                "NormalizedRotation",
                "-2|-1|-1.0|-0.2|0.2|1.0|1|1.5|1.5|2.0|2|2.5|2.5|3.0|3",
                "NROT",
            ),
        ];
        assert_eq!(
            EXPECTED.len(),
            sets.len() * radar_fields::known::ALL.len(),
            "the table must cover every registered field in every preference \
             set, or a field could drop out of it with nothing going red",
        );
        for (label, field, want_ticks, want_unit) in EXPECTED {
            let id = FieldId::from_static(field);
            assert!(
                radar_fields::known::ALL.contains(&id),
                "{field} is not a field this crate has a const for",
            );
            let (_, prefs) = sets
                .iter()
                .find(|(name, _)| *name == label)
                .expect("a preference set named in the table");
            assert_eq!(
                ticks(&id, prefs).join("|"),
                want_ticks,
                "{field} under {label} preferences: the colour bar's labels moved",
            );
            assert_eq!(
                crate::field_facts::unit_label(&id, prefs),
                want_unit,
                "{field} under {label} preferences: the colour bar's unit title moved",
            );
        }
    }

    /// The MEHS colour bar is labelled in the user's hail-size unit; its stops
    /// are authored in inches, so the colours must not move.
    #[test]
    fn the_mehs_colour_bar_is_labelled_in_the_users_hail_size_unit() {
        let expected = [
            (
                HailSizeUnit::Inches,
                [
                    "0.2", "0.5", "0.8", "1", "1.2", "1.5", "1.8", "2", "2.5", "3", "3.5", "4",
                ],
            ),
            (
                HailSizeUnit::Centimeters,
                [
                    "0.6", "1.3", "1.9", "2.5", "3.2", "3.8", "4.4", "5.1", "6.3", "7.6", "8.9",
                    "10.2",
                ],
            ),
            (
                HailSizeUnit::Millimeters,
                [
                    "6", "13", "19", "25", "32", "38", "44", "51", "64", "76", "89", "102",
                ],
            ),
        ];
        for (unit, labels) in expected {
            let prefs = UserPreferences {
                hail_size: unit,
                ..UserPreferences::default()
            };
            assert_eq!(
                ticks(&radar_fields::known::MAX_EXPECTED_HAIL_SIZE, &prefs),
                labels,
                "{unit:?} ticks",
            );
        }

        // The stops themselves are untouched by the preference: this is a
        // relabelling, not a repalettising.
        let inch_stops: Vec<f32> =
            crate::field_facts::facts(&radar_fields::known::MAX_EXPECTED_HAIL_SIZE)
                .scale
                .thresholds
                .iter()
                .map(|&(v, _)| v)
                .collect();
        assert_eq!(
            inch_stops,
            [
                0.25, 0.5, 0.75, 1.0, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0, 3.5, 4.0
            ],
            "the palette's stops are inches whatever the preference says",
        );
    }

    /// A tick and the hover readout are the same number in the same unit.
    #[test]
    fn a_mehs_tick_and_the_hover_readout_are_the_same_number() {
        for unit in [HailSizeUnit::Centimeters, HailSizeUnit::Millimeters] {
            let prefs = UserPreferences {
                hail_size: unit,
                ..UserPreferences::default()
            };
            let product = radar_fields::known::MAX_EXPECTED_HAIL_SIZE;
            for &(stop, _) in &crate::field_facts::facts(&product).scale.thresholds {
                let tick = format_legend_value(&product, stop, &prefs);
                assert_eq!(
                    crate::field_facts::format_value(&product, stop, &prefs),
                    format!(
                        "MEHS: {tick} {}",
                        crate::field_facts::unit_label(&product, &prefs)
                    ),
                    "{unit:?} at the {stop} in stop",
                );
            }
        }
    }

    /// Every other product's ticks are unchanged, and no product picked up a
    /// hail-size conversion.
    #[test]
    fn no_other_products_ticks_moved() {
        let prefs = UserPreferences {
            hail_size: HailSizeUnit::Millimeters,
            ..UserPreferences::default()
        };
        let default = UserPreferences::default();
        for product in radar_fields::known::ALL.iter() {
            if *product == radar_fields::known::MAX_EXPECTED_HAIL_SIZE {
                continue;
            }
            assert_eq!(
                ticks(product, &prefs),
                ticks(product, &default),
                "{product:?} reads the hail-size preference and should not",
            );
        }
        // And the generic form itself, which every unnamed product falls back
        // to: whole numbers bare, one decimal otherwise.
        assert_eq!(short_tick(4.0), "4");
        assert_eq!(short_tick(0.25), "0.2");
        assert_eq!(short_tick(-1.5), "-1.5");
    }

    /// **Both** echo-tops bars are labelled in the user's height unit; their
    /// stops are authored in kft and both are titled off `HeightUnit::kilo_suffix`.
    #[test]
    fn both_echo_tops_bars_are_labelled_in_the_users_height_unit() {
        use rustdar_units::HeightUnit;

        let feet = UserPreferences {
            height: HeightUnit::Feet,
            ..UserPreferences::default()
        };
        let metres = UserPreferences {
            height: HeightUnit::Meters,
            ..UserPreferences::default()
        };
        for product in [
            radar_fields::known::ECHO_TOPS,
            radar_fields::known::ECHO_TOPS_INTERPOLATED,
        ] {
            assert_eq!(
                ticks(&product, &feet).last().map(String::as_str),
                Some("60"),
                "{product:?} in feet is the bar as it has always been labelled",
            );
            assert_eq!(
                ticks(&product, &metres).last().map(String::as_str),
                Some("18"),
                "{product:?} in metres is labelled in kft: 60 kft is 18 km",
            );
            // And the number on the bar is the number the readout gives for the
            // same stop, to the tick's own precision.
            let top = crate::field_facts::facts(&product)
                .scale
                .thresholds
                .last()
                .expect("the echo-tops ramp has stops")
                .0;
            let readout = crate::field_facts::format_value(&product, top, &metres);
            assert!(
                readout.contains("18.3 km"),
                "{product:?} reads out {readout:?} for the stop its bar calls \
                 {:?}",
                ticks(&product, &metres).last(),
            );
        }
    }

    /// The velocity ramp's own reach, m/s — what a fold marker has to fall
    /// inside to be drawable.
    fn velocity_bounds() -> (f32, f32) {
        let legend = crate::field_facts::facts(&radar_fields::known::VELOCITY).scale;
        (legend.min_value, legend.max_value)
    }

    /// A real Doppler declaration that sits **inside** the bar, m/s: KTLX's
    /// 0.5° cut on 2026-08-11 at 10:09. A WSR-88D and not a TDWR — a TDWR
    /// declares `nyquist_velocity = 0` on every cut.
    const INSIDE_THE_BAR_MS: f32 = 23.84;

    /// A declaration past the end of the bar, m/s — wider than KFFC cut 12's
    /// 62.94, the fastest measured, and the widest speed the velocity moment
    /// itself encodes (±63.5 m/s in half-metre steps).
    const PAST_THE_BAR_MS: f32 = 63.5;

    /// The fold annotation is the declared limit converted, and nothing else —
    /// every user-facing number goes through `rustdar-units`.
    #[test]
    fn the_fold_annotation_is_the_declared_limit_in_the_users_speed_unit() {
        // 23.84 m/s in each unit, rounded as the annotation rounds: 53.33 mph,
        // 85.82 km/h, 46.34 kt.
        let expected = [
            (SpeedUnit::Mph, "folds \u{b1}53"),
            (SpeedUnit::MetersPerSec, "folds \u{b1}24"),
            (SpeedUnit::KilometersPerHour, "folds \u{b1}86"),
            (SpeedUnit::Knots, "folds \u{b1}46"),
        ];
        for (speed, line) in expected {
            let prefs = UserPreferences {
                speed,
                ..UserPreferences::default()
            };
            assert_eq!(
                fold_title_line(f64::from(INSIDE_THE_BAR_MS), &prefs),
                line,
                "{speed:?}",
            );
        }
    }

    /// Both ends of a fold inside the ramp are marked; a declaration past its
    /// reach is marked nowhere. The off-scale answer is *nothing*, not a marker
    /// parked at the end.
    #[test]
    fn a_fold_off_the_end_of_the_ramp_is_marked_nowhere() {
        let (min_val, max_val) = velocity_bounds();
        assert!(
            (max_val - 36.01).abs() < 0.01 && (min_val + 36.01).abs() < 0.01,
            "the velocity ramp moved: it now spans {min_val}..{max_val} m/s, \
             so the fixtures below no longer describe one fold inside the bar \
             and one past it",
        );

        assert_eq!(
            fold_marker_positions(INSIDE_THE_BAR_MS, min_val, max_val),
            Some([-INSIDE_THE_BAR_MS, INSIDE_THE_BAR_MS]),
            "a fold inside the bar must be marked at both ends",
        );
        assert_eq!(
            fold_marker_positions(PAST_THE_BAR_MS, min_val, max_val),
            None,
            "a {PAST_THE_BAR_MS} m/s fold was marked on a bar that stops at \
             36.01",
        );
        // Exactly at the end still counts — that marker is drawable, and it is
        // the boundary a clamp would have been written around.
        assert_eq!(
            fold_marker_positions(max_val, min_val, max_val),
            Some([-max_val, max_val]),
        );
        // A declaration of zero or a non-finite one describes no fold at all.
        // Zero is the live case — every TDWR declares it.
        for absurd in [0.0, -22.0, f32::NAN, f32::INFINITY] {
            assert_eq!(
                fold_marker_positions(absurd, min_val, max_val),
                None,
                "{absurd} was taken for a fold limit",
            );
        }
    }

    /// A projection with no map in it: one screen point per degree, so a
    /// row's coordinates and the pixel it lands on are the same numbers.
    fn degrees_as_pixels(lat: f64, lon: f64) -> egui::Pos2 {
        egui::pos2(lon as f32, lat as f32)
    }

    /// Everything, so the walk's on-screen filter never decides anything here.
    fn everywhere() -> egui::Rect {
        egui::Rect::from_min_max(egui::pos2(-400.0, -400.0), egui::pos2(400.0, 400.0))
    }

    /// A [`VisibleSite`] keeps naming its own radar after the table grows
    /// under it — the table is resolved at runtime, so an index would not.
    #[test]
    fn a_visible_site_names_its_own_radar_after_the_table_grows() {
        let position = |lat_udeg, lon_udeg| rustdar_radar::site_position::SitePosition {
            lat_udeg,
            lon_udeg,
            site_height_m: 100,
            tower_height_m: 20,
        };
        // Three radars in the empty South Pacific, and a smaller table holding
        // only the first. The binary carries no radars at all.
        let learned = rustdar_radar::sites::SiteFix::Learned;
        // `ZZZA` rather than `ZZZZ`: arrivals are sorted by identifier, so the
        // incumbent must sort first for the zip below to compare like with like.
        let incumbent = ("ZZZA", learned(position(-29_000_000, -139_000_000)));
        let smaller = rustdar_radar::sites::build_table([incumbent]);
        let bigger = rustdar_radar::sites::build_table([
            incumbent,
            ("ZZZY", learned(position(-30_000_000, -140_000_000))),
            ("ZZZX", learned(position(-31_000_000, -141_000_000))),
        ]);
        assert_eq!(
            bigger.rows().len(),
            smaller.rows().len() + 2,
            "precondition: the two tables must be different lengths, or this \
             test cannot tell an index from a reference",
        );

        let walked = |rows| visible_sites_in(rows, everywhere(), 18.0, degrees_as_pixels);

        // Every site the smaller table produced is still named by the larger
        // one's walk, at the same place.
        let before = walked(smaller.rows());
        let after = walked(bigger.rows());
        assert!(
            !before.is_empty(),
            "the smaller table must produce some visible sites"
        );
        for (old, new) in before.iter().zip(after.iter()) {
            assert_eq!(
                old.site.name, new.site.name,
                "a row changed identity when the table grew",
            );
            assert_eq!(old.screen, new.screen);
        }

        // And every row names the radar whose coordinates put it there, which
        // is the property an index cannot promise across two tables.
        for visible in &after {
            assert_eq!(
                visible.screen,
                degrees_as_pixels(visible.site.lat, visible.site.lon),
                "{} is drawn somewhere other than its own position",
                visible.site.name,
            );
        }

        // The arrivals are among them, reachable and named.
        let names: Vec<&str> = after.iter().map(|v| v.site.name).collect();
        assert!(names.contains(&"ZZZY"), "got {names:?}");
        assert!(names.contains(&"ZZZX"));
    }
}

#[path = "ui_map_pane/raster_registration_tests.rs"]
#[cfg(test)]
mod raster_registration_tests;

#[path = "ui_map_pane/theme_flip_tests.rs"]
#[cfg(test)]
mod theme_flip_tests;

#[path = "ui_map_pane/as_of_token_tests.rs"]
#[cfg(test)]
mod as_of_token_tests;
