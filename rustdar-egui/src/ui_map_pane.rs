use crate::actions::GuiAction;
use crate::overlay_cache::{
    current_quantized_zoom, draw_overlay_texture, plan_overlay_texture, viewport_geo_bounds,
};
use crate::pane::{PaneState, RadarImageData};
use crate::point_painter::EguiPointPainter;
use rustdar_overlays::render::draw::{DrawPointContext, HoverContext};
use rustdar_overlays::render::overlay_state::{
    OverlayItem, OverlayKind, OverlayRegistry, RenderMode,
};
use rustdar_units::{HailSizeUnit, UserPreferences};
use std::sync::Arc;

use crate::tile_source::HttpsTiles;
use rustdar_radar::sites::RADARS;
use rustdar_radar::types::{ImageBounds, MAX_RANGE_KM, RadarProduct};
use rustdar_radar::{get_color_for_value, get_legend_scale};

use super::super::map_overlays::{OverlayDrawContext, draw_tile_layer, is_pos_blocked};

/// Shared references needed for rendering a single pane's map content.
pub(super) struct PaneRenderCtx<'a> {
    pub pane_idx: usize,
    pub pane: &'a mut PaneState,
    pub overlays: &'a mut OverlayRegistry,
    pub user_location: Option<(f64, f64)>,
    pub user_heading: Option<f32>,
    pub user_fix: Option<rustdar_gps::GpsFix>,
    pub label_tiles: &'a mut Option<HttpsTiles>,
    /// How many slippy zoom levels deeper than this pane's own zoom its raster
    /// tile layers should fetch — see
    /// [`draw_tile_layer`](super::super::map_overlays::draw_tile_layer).
    ///
    /// Non-zero only for a pane some 3D pane is standing on, and only while the
    /// mirror was actually sized to show the extra detail. Resolved once per
    /// pane by `Gui::tile_zoom_bias_for_pane`.
    pub tile_zoom_bias: u8,
    pub actions: &'a mut Vec<GuiAction>,
    pub pane_rect: egui::Rect,
    /// Whether this frame's color scale bars run along the bottom edge
    /// (`true`) or the right edge (`false`). Resolved once for the whole map
    /// panel by `ColorScaleOrientation`, so every pane agrees.
    pub horizontal_color_scale: bool,
    pub pointer_available: bool,
    /// Rects of chrome painted over the map with no egui layer of its own.
    /// Clicks there are not map clicks. Empty since the top bar replaced the
    /// hamburger — everything left is a panel or a floating layer, which
    /// `is_pos_blocked` catches without plumbing — but the mechanism stays for
    /// the next painted-in-pane chrome (see `ShellOutput::excluded_rects`).
    /// Map content that is itself clickable does **not** belong here — see
    /// `visible_sites` in `render_pane_map_content`.
    pub excluded_rects: Vec<egui::Rect>,
    /// Screen position of an active long-press (for the radar value tooltip),
    /// or `None`. Only the touch pipeline ever produces one.
    pub long_press_pos: Option<egui::Pos2>,
    /// Screen position of a confirmed overlay click/tap, or `None` if no overlay
    /// click occurred this frame. On desktop this comes from egui's `any_click()`;
    /// on Android from the deferred single-tap detector.
    pub overlay_click_pos: Option<egui::Pos2>,
    /// Set by every handler that **acts** on
    /// [`overlay_click_pos`](Self::overlay_click_pos) — an overlay feature
    /// hit, a radar-site icon click. One flag for the whole frame's pane
    /// loop, owned by `render_panes`: the fade trigger (`ui_fade.rs`) is a
    /// click nothing consumed, and this is the consumption half of it. See the CONVENTION
    /// comment in `ui_map.rs`.
    pub click_consumed: &'a mut bool,
    /// User unit and timezone preferences.
    pub preferences: &'a UserPreferences,
    /// Everything the 3D region drag needs. See [`RegionCtx`].
    pub region: RegionCtx<'a>,
    /// The kinds this pane dispatched, in the order they painted, with the
    /// egui layer each **arm** painted into. The layer is the honest half:
    /// the sequence alone restates the loop, but two kinds on *different*
    /// layers composite in `GraphicLayers::drain`'s order — same-`Order`
    /// non-area layers drain in hash order, egui's own "safety net" — not in
    /// this sequence, which is exactly how the old color-scale sub-layer
    /// ignored `draw_order`. One paint list is what makes the sequence the
    /// truth.
    ///
    /// What the layer record covers, exactly: the arm's *own* painter — the
    /// loop's `ui.painter()` default, overwritten by an arm that constructs
    /// another (the ColorScale arm does). That is the seam the old bug lived
    /// on, and the seam contract 95 pins. Below it the record is on honour:
    /// a paint helper that built its own layer painter *internally* would
    /// not be reflected here, and no test claims otherwise.
    #[cfg(test)]
    pub paint_order: Vec<(OverlayKind, egui::LayerId)>,
}

/// The region drag's slice of a pane's render context.
///
/// Grouped rather than four more flat fields because they are one feature and
/// because three of them are `&mut` borrows of `Gui` that only this feature
/// touches — keeping them together is what makes it obvious at the construction
/// site that nothing else is reaching into the pane loop's borrow.
pub(super) struct RegionCtx<'a> {
    /// Whether the pick-a-region mode is on. While it is, a drag on this pane
    /// draws a box instead of panning the map.
    pub armed: bool,
    /// The drag in flight, on this pane or another. Shared across panes because
    /// there is one pointer: a drag that started on pane 0 and wandered over
    /// pane 1 is still pane 0's, which is what `RegionDrag::pane_idx` is
    /// checked for.
    pub drag: &'a mut Option<crate::ui_region::RegionDrag>,
    /// Where a committed region is left for the pane loop to finish and
    /// `Gui::apply_pending_region` to act on.
    pub pending: &'a mut Option<crate::ui_region::PendingRegion>,
    /// Every 3D pane's committed region with the map pane it was dragged on, so
    /// this pane can draw the ones that belong to it.
    ///
    /// Gathered before the pane loop, because inside the loop's `mem::take` a 3D
    /// pane's slot reads as a default map pane with no region at all.
    pub committed: &'a [(usize, crate::pane::VolumeRegion)],
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

    // Cleared every frame and re-set by the radar arm below, exactly as
    // `overlay_hover_value` is. The radar arm is the only writer, and it only
    // runs while Radar is enabled, in `draw_order`, and has an image — clearing
    // only inside that arm left the last readout frozen in the status bar
    // whenever any of those stopped being true.
    ctx.pane.hover_value = None;

    // Sites take priority over the overlays beneath them, so those skip a click
    // that lands on an icon. Kept out of `ctx.excluded_rects`, which
    // `handle_radar_site_interactions` reads itself: with the icons in there,
    // every site click was blocked by its own icon.
    //
    // Projected **once**. This used to build the rect list and then throw the
    // projections away, leaving `handle_radar_site_interactions` to walk all
    // 207 sites again and re-derive the identical `icon_rect` — two Mercator
    // passes over the site table per map pane per frame for one answer. The
    // list is now the answer, and the interaction pass reads it.
    let visible_sites = visible_radar_sites(ui, projector, zoom, ctx.pane);
    // What the overlays *under* the sites must not be clicked through.
    let overlay_excluded_rects: Vec<egui::Rect> = ctx
        .excluded_rects
        .iter()
        .copied()
        .chain(visible_sites.iter().map(|s| s.icon_rect))
        .collect();

    // --- Phase 1: immutable-ui work (ordered layer dispatch) ---
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
        // it must read over every overlay drawn after the radar, and with the
        // whole pane on one paint list "over everything" means "submitted
        // after the loop" — not a sub-layer, whose compositing order against
        // the pane's list is egui's hash-order safety net (see `paint_order`).
        let mut pending_notice: Option<(RadarProduct, f32)> = None;

        let draw_order: Vec<OverlayKind> = ctx.pane.draw_order.clone();
        for &kind in &draw_order {
            if !ctx.pane.is_overlay_enabled(kind) {
                continue;
            }
            // Every arm below paints through `ui.painter()` — the pane's own
            // paint list — so submission order IS `draw_order`. The layer is
            // recorded per arm, from the arm's own painter (one that builds
            // another overwrites the default), so an *arm* moved onto a
            // sub-layer fails the paint-order pin rather than silently
            // leaving its stacking to egui's hash-order layer drain. The
            // record stops at the arm seam — a helper's internal painter is
            // not reflected; see `PaneRenderCtx::paint_order`.
            #[cfg(test)]
            let mut painted_layer = ui.painter().layer_id();
            match kind {
                // Radar image layer — special handling for loop playback
                OverlayKind::Radar => {
                    // Loop playback: draw the active loop frame instead
                    if ctx.pane.loop_state.is_active() {
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
                        // Extract metadata before drawing (avoids borrow conflict)
                        let meta_snapshot = ctx
                            .pane
                            .overlay_cache(OverlayKind::Radar)
                            .and_then(|c| c.current.as_ref())
                            .and_then(|tex| tex.radar_meta.as_ref())
                            .map(|m| {
                                (
                                    m.lat,
                                    m.lon,
                                    m.max_range_km,
                                    std::sync::Arc::clone(&m.value_data),
                                )
                            });

                        if let Some(tex) = ctx
                            .pane
                            .overlay_cache(OverlayKind::Radar)
                            .and_then(|c| c.current.as_ref())
                        {
                            let screen_rect = ui.max_rect();
                            draw_overlay_texture(ui.painter(), projector, tex, screen_rect);
                        }

                        // Per-frame: range ring + hover value from radar metadata
                        if let Some((lat, lon, _max_range_km, value_data)) = meta_snapshot {
                            render_radar_range_ring(ui, projector, lat, lon);
                            update_pane_hover_value_from_meta(
                                ui,
                                projector,
                                &RadarHoverData {
                                    value_data: &value_data,
                                    lat,
                                    lon,
                                },
                                ctx.pane,
                                ctx.pane_rect,
                                ctx.preferences,
                            );
                        }
                    }

                    // The pixels above are not the selection every other label on
                    // this pane is already describing — say which product they
                    // are. Decided inside the Radar arm, so it appears only while
                    // the radar layer is actually on screen and only for a pane
                    // that has an image to disown; *painted after the loop*, so
                    // an overlay drawn later in `draw_order` cannot paint over
                    // the notice — deferred submission on the pane's own paint
                    // list, not a sub-layer (see `PaneRenderCtx::paint_order`).
                    //
                    // Not branched on the datasource, and it must never be: this
                    // is the same call for a Level II and a Level III product,
                    // and `PaneState::stale_image_on_screen` answers from the
                    // same texture metadata either way.
                    pending_notice = ctx.pane.stale_image_on_screen();
                }
                // City label tiles — the same projector-driven tile pass the
                // basemap goes through, at the same bias, so the names sit on
                // the roads they name at every level.
                OverlayKind::CityLabels => {
                    if let Some(ltiles) = ctx.label_tiles.as_mut() {
                        draw_tile_layer(ui, projector, zoom, ltiles, ctx.tile_zoom_bias);
                    }
                }
                // Radar sites: texture + per-frame interactions (text labels, clicks)
                OverlayKind::RadarSites => {
                    if let Some(tex) = ctx
                        .pane
                        .overlay_cache(kind)
                        .and_then(|c| c.current.as_ref())
                    {
                        let screen_rect = ui.max_rect();
                        draw_overlay_texture(ui.painter(), projector, tex, screen_rect);
                    }
                    handle_radar_site_interactions(ui, zoom, &visible_sites, ctx);
                }
                // User location blue dot
                OverlayKind::UserLocation => {
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
                // pane's own paint list at this loop position, like every
                // other kind, so `draw_order` genuinely places it: the old
                // dedicated sub-layer composited in `GraphicLayers::drain`'s
                // hash order regardless of the loop, which is why moving City
                // Labels above the Color Scale used to change nothing. Within
                // one paint list submission order is paint order — egui
                // batches without reordering — so "later in `draw_order`"
                // now means "on top" for the bars exactly as it does for a
                // texture.
                OverlayKind::ColorScale => {
                    let painter = ui.painter().with_clip_rect(ctx.pane_rect);
                    #[cfg(test)]
                    {
                        painted_layer = painter.layer_id();
                    }
                    let pane_rect = ui.max_rect();
                    render_color_scale(
                        &painter,
                        pane_rect,
                        ctx.horizontal_color_scale,
                        ctx.pane,
                        ctx.preferences,
                    );
                    render_overlay_color_scales(
                        &painter,
                        pane_rect,
                        ctx.horizontal_color_scale,
                        ctx.pane,
                        ctx.overlays,
                    );
                }
                // All other overlays dispatched by render mode
                _ => match ctx.overlays.render_mode(kind) {
                    Some(RenderMode::Texture) => {
                        // Shared, not mutable: the labels are borrowed out of
                        // the handler for the length of the draw, and the
                        // clickable set is only asked for if a click needs
                        // resolving — see `OverlayDrawContext::draw_overlay`.
                        let overlays = &*ctx.overlays;
                        selected.extend(overlay_ctx.draw_overlay(
                            ctx.pane.overlay_cache(kind),
                            overlays.map_labels(kind),
                            || overlays.clickable_items(kind),
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
                                excluded_rects: &overlay_excluded_rects,
                                pane_rect: ctx.pane_rect,
                            },
                        ));
                    }
                    _ => {}
                },
            }
            #[cfg(test)]
            ctx.paint_order.push((kind, painted_layer));
        }

        // The deferred stale-image notice, submitted after every kind so
        // nothing in `draw_order` can paint over it — see the Radar arm.
        if let Some((on_screen, elevation)) = pending_notice {
            let notice_painter = ui.painter().with_clip_rect(ctx.pane_rect);
            draw_pending_render_notice(
                &notice_painter,
                ctx.pane_rect,
                // The pill row's measured clearance, not the one-row
                // constant: a narrow pane wraps the row (M9-18).
                crate::ui::pills::pill_row_clearance(ui.ctx(), ctx.pane_idx),
                on_screen,
                elevation,
            );
        }

        if !selected.is_empty() {
            ctx.overlays.selected_overlays = selected;
            ctx.overlays.selected_overlay_page = 0;
            // A feature answered this frame's click — the consumption half
            // of the fade trigger (see `PaneRenderCtx::click_consumed`).
            *ctx.click_consumed = true;
        }

        // --- Check overlay hover values (model data, etc.) ---
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
                for &kind in &draw_order {
                    if ctx.pane.is_overlay_enabled(kind)
                        && let Some(text) = ctx.overlays.hover_value_at(kind, hover_lat, hover_lon)
                    {
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
        // Compute render dimensions with as much overdraw as the adapter's texture
        // limit allows. `max_texture_side` is `max_texture_dimension_2d`, handed to
        // egui when the renderer was built; egui only `debug_assert!`s the bound in
        // `load_texture`, so a release build that ignored it would reach
        // `Device::create_texture` and fail as a wgpu validation error at runtime.
        let max_texture_side = ui.ctx().input(|i| i.max_texture_side) as u32;
        let tex_plan = plan_overlay_texture(screen_rect, max_texture_side);

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
                    texture: tex_plan,
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

    // Long-press tooltip: show the radar value above the finger. Reached only
    // when the touch pipeline ran this frame, which is now a runtime decision
    // (`InteractionState`) rather than a target one — a touchscreen laptop and
    // a phone browser both get here.
    if let Some(touch_pos) = ctx.long_press_pos
        && ctx.pane_rect.contains(touch_pos)
    {
        // Try overlay cache meta first (non-loop static render), then loop frame
        let raw_meta = ctx
            .pane
            .overlay_cache(OverlayKind::Radar)
            .and_then(|c| c.current.as_ref())
            .and_then(|tex| tex.radar_meta.as_ref())
            .map(|m| (m.lat, m.lon, std::sync::Arc::clone(&m.value_data)));
        if let Some((lat, lon, value_data)) = raw_meta {
            draw_long_press_tooltip(
                ui,
                projector,
                &value_data,
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
                &img.value_data,
                img.lat,
                img.lon,
                touch_pos,
                ctx.pane,
                ctx.preferences,
            );
        }
    }

    // Last, so the box is drawn over everything the pane put under it. A region
    // outline hidden behind a reflectivity texture is a region the user cannot
    // aim.
    handle_region_drag(ui, projector, ctx);
}

/// Stroke width of a region box, points.
const REGION_STROKE: f32 = 1.5;

/// Advance the region drag on this pane and draw whatever boxes belong to it.
///
/// # Why the pointer is read raw here
///
/// `ui_input`'s convention is that map handlers consume
/// `PaneRenderCtx::overlay_click_pos` and never `ctx.input()` directly, and that
/// convention is about **clicks** — a confirmed tap, deferred past a double-tap
/// window and filtered against floating dialogs. A drag is not a click: it has a
/// press frame, a stream of moved frames and a release frame, none of which a
/// confirmed-tap position can express. The dialog gate that convention exists to
/// enforce is applied here explicitly, on the press, via [`is_pos_blocked`].
///
/// This runs inside `Map::show`, and it has to: the projector is the only thing
/// that can turn a pointer position into the ground under it, and it exists
/// nowhere else. That is also what makes the geographic anchor possible — see the
/// `ui_region` module doc for why a pixel anchor would be wrong.
fn handle_region_drag(ui: &egui::Ui, projector: &walkers::Projector, ctx: &mut PaneRenderCtx<'_>) {
    // Committed boxes first, and regardless of the armed mode: a 3D pane's region
    // is drawn on the map it came from for as long as it exists, which is the
    // honest answer to "where is that volume from". Unarmed is the common case, so
    // this is also the only work most frames do here.
    for (_, region) in ctx
        .region
        .committed
        .iter()
        .filter(|(source, _)| *source == ctx.pane_idx)
    {
        draw_region_box(
            ui,
            projector,
            region.centre(),
            region.half_width_km(),
            egui::Color32::from_rgb(120, 200, 255),
            false,
        );
    }

    if !ctx.region.armed {
        // A drag that was in flight on this pane when the mode went off is
        // already gone — the menu clears it — so there is nothing to tidy.
        return;
    }

    let (pressed, down, released, pos) = ui.ctx().input(|i| {
        (
            i.pointer.primary_pressed(),
            i.pointer.primary_down(),
            i.pointer.primary_released(),
            i.pointer.interact_pos(),
        )
    });

    let to_geo = |p: egui::Pos2| {
        let position = projector.unproject(egui::vec2(p.x, p.y));
        crate::pane::GeoPoint {
            lat: position.y(),
            lon: position.x(),
        }
    };

    // The press frame. Gated on this pane's own rect and on the dialog layer, so
    // arming the mode and then using the menu does not start a drag behind it.
    if pressed
        && let Some(pos) = pos
        && ctx.pane_rect.contains(pos)
        && !is_pos_blocked(ui.ctx(), pos, ctx.pane_rect, &ctx.excluded_rects)
    {
        *ctx.region.drag = crate::ui_region::RegionDrag::begin(ctx.pane_idx, to_geo(pos));
    }

    // Everything below belongs to *this* pane's drag only. A drag that started
    // next door is that pane's to advance, draw and commit, even while the
    // pointer is over this one.
    let Some(drag) = ctx
        .region
        .drag
        .as_mut()
        .filter(|d| d.pane_idx() == ctx.pane_idx)
    else {
        return;
    };

    // Not gated on the pane's rect: dragging past the edge of a pane to make a
    // big box is ordinary, and stopping the box growing at the boundary would
    // read as the gesture having been dropped.
    if down && let Some(pos) = pos {
        drag.extend_to(to_geo(pos));
    }

    let drag = *drag;
    draw_region_box(
        ui,
        projector,
        drag.centre(),
        drag.half_width_km(),
        crate::ui_region::REGION_ARM_COLOR,
        true,
    );
    draw_region_hint(ui, projector, drag);

    if released {
        // Taken either way: a discarded drag must not survive into the next
        // press, or the second box would be measured from the first one's centre.
        *ctx.region.drag = None;
        match drag.commit() {
            Some(region) => {
                *ctx.region.pending = Some(crate::ui_region::PendingRegion {
                    source_pane: ctx.pane_idx,
                    region,
                });
            }
            // Too small to be honoured rather than clamped. Silently discarded,
            // and the mode stays armed — see `RegionDrag::commit`.
            None => log::debug!(
                "3D region drag was {:.1} km across, below the resampler's minimum; discarded",
                2.0 * drag.half_width_km(),
            ),
        }
    }
}

/// Draw one region box in geographic terms.
///
/// The corners are projected rather than the centre offset by a pixel radius, so
/// the box stays over the same ground at every zoom — the same thing
/// `overlay_texture_rect` does for an overlay's bounds, and for the same reason.
///
/// A degenerate box — a zero half-width before the pointer has moved, or a polar
/// centre — draws nothing rather than a dot, because a dot under the cursor at
/// the start of every drag reads as a stray click artefact.
fn draw_region_box(
    ui: &egui::Ui,
    projector: &walkers::Projector,
    centre: crate::pane::GeoPoint,
    half_width_km: f64,
    color: egui::Color32,
    filled: bool,
) {
    let Some(rect) = region_screen_rect(projector, centre, half_width_km) else {
        return;
    };
    if filled {
        ui.painter()
            .rect_filled(rect, 0.0, color.gamma_multiply(0.12));
    }
    ui.painter().rect_stroke(
        rect,
        0.0,
        egui::Stroke::new(REGION_STROKE, color),
        egui::StrokeKind::Middle,
    );
}

/// A region's screen rect, or `None` for one that has no square to draw.
fn region_screen_rect(
    projector: &walkers::Projector,
    centre: crate::pane::GeoPoint,
    half_width_km: f64,
) -> Option<egui::Rect> {
    if !(half_width_km.is_finite() && half_width_km > 0.0) {
        return None;
    }
    let (nw, se) = crate::ui_region::corners_for(centre, half_width_km)?;
    let nw = projector
        .project(walkers::lat_lon(nw.lat, nw.lon))
        .to_pos2();
    let se = projector
        .project(walkers::lat_lon(se.lat, se.lon))
        .to_pos2();
    Some(egui::Rect::from_two_pos(nw, se))
}

/// The width and per-cell resolution of the box being dragged, over its top edge.
///
/// The resolution is the whole reason to pick a region — the grid's cell count is
/// fixed, so a tighter box spends the same cells over less ground — and it is
/// invisible unless it is said. Shown *during* the drag rather than after, so the
/// choice is made against the number rather than discovered after a 155 ms
/// rebuild.
fn draw_region_hint(
    ui: &egui::Ui,
    projector: &walkers::Projector,
    drag: crate::ui_region::RegionDrag,
) {
    let Some(rect) = region_screen_rect(projector, drag.centre(), drag.half_width_km()) else {
        return;
    };
    let Some(text) = region_hint_text(drag.centre(), drag.half_width_km()) else {
        return;
    };
    let galley = ui.painter().layout_no_wrap(
        text,
        egui::FontId::proportional(12.0),
        egui::Color32::from_rgb(20, 20, 20),
    );
    let origin = egui::pos2(rect.left(), rect.top() - galley.size().y - 4.0);
    ui.painter().rect_filled(
        egui::Rect::from_min_size(origin, galley.size()).expand(3.0),
        2.0,
        crate::ui_region::REGION_ARM_COLOR,
    );
    ui.painter()
        .galley(origin, galley, egui::Color32::PLACEHOLDER);
}

/// The hint's text for a drag standing at `half_width_km` about `centre`, or
/// `None` off Earth, where there is no box to describe.
///
/// Through `VolumeRegion`, so the figures shown are the ones that will be
/// resampled: it clamps the half-width to the resampler's range, and a hint
/// reading 4 km over a box that would become 10 km is a hint that lies.
/// `RegionDrag::extend_to` caps the drag at the same ceiling, so today the two
/// agree before this runs — but the hint's honesty is this routing, not that
/// cap, and it is what the test on this function pins.
fn region_hint_text(centre: crate::pane::GeoPoint, half_width_km: f64) -> Option<String> {
    let region = crate::pane::VolumeRegion::new(centre, half_width_km)?;
    let cells = rustdar_radar::voxel::default_shape().nx;
    Some(match region.resolution_km(cells) {
        Some(km) => format!("{:.0} km - {km:.2} km/cell", 2.0 * region.half_width_km()),
        None => format!("{:.0} km", 2.0 * region.half_width_km()),
    })
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
        ui,
        projector,
        &RadarHoverData {
            value_data: &img.value_data,
            lat: img.lat,
            lon: img.lon,
        },
        pane,
        pane_rect,
        prefs,
    );
}

/// Draw only the range ring for a radar site (used with overlay-cache rendering).
fn render_radar_range_ring(ui: &egui::Ui, projector: &walkers::Projector, lat: f64, lon: f64) {
    let radar_center = projector.project(walkers::lat_lon(lat, lon)).to_pos2();
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
    // `render_pane_map_content` clears `hover_value` at its top, so a value
    // cached behind a did-the-pointer-move gate would blank the readout the
    // moment the mouse rested.
    pane.last_hover_pos = Some(hover_pos);

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

/// A hover readout pinned to the pointer, on a layer that cannot claim it.
///
/// `egui::Tooltip` puts its `Area` up **interactable**, so `layer_id_at`
/// reports it at the pointer it is anchored to and the dialog gate
/// (`filter_dialog_blocked`, `is_pos_blocked`) then throws away the click that
/// follows. Hovering a thing on the map is what made it unclickable.
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
///
/// The single product of the site table walk: both consumers — the exclusion
/// list the overlays under the sites are hit-tested against, and the label /
/// click / hover pass — read this rather than re-projecting.
struct VisibleSite {
    /// Index into [`RADARS`].
    index: usize,
    /// Screen position of the site marker's centre.
    screen: egui::Pos2,
    /// The clickable icon box around `screen`.
    icon_rect: egui::Rect,
}

/// Project the radar site table once, keeping the sites within a 100 px margin
/// of this pane.
///
/// Empty when the layer is off, which is the same condition the sites arm of
/// the draw loop runs under — nothing downstream pays for a walk whose result
/// would be discarded.
fn visible_radar_sites(
    ui: &egui::Ui,
    projector: &walkers::Projector,
    zoom: f64,
    pane: &PaneState,
) -> Vec<VisibleSite> {
    if !pane.is_overlay_enabled(OverlayKind::RadarSites) {
        return Vec::new();
    }
    // The margin is what lets a site just off the edge still draw its label and
    // take a click on the icon straddling the boundary — see the harness's
    // `a_click_outside_the_pane_does_not_reach_a_site_icon_straddling_its_edge`.
    let near = ui.max_rect().expand(100.0);
    let icon_size = (10.0 + zoom as f32 * 2.0).clamp(8.0, 24.0);
    let mut visible = Vec::new();
    for (index, site) in RADARS.iter().enumerate() {
        let screen = projector
            .project(walkers::lat_lon(site.lat, site.lon))
            .to_pos2();
        if !near.contains(screen) {
            continue;
        }
        visible.push(VisibleSite {
            index,
            screen,
            icon_rect: egui::Rect::from_center_size(screen, egui::vec2(icon_size, icon_size)),
        });
    }
    visible
}

/// Per-frame radar site label rendering and interaction detection.
///
/// The site circles and background pills are in the background-rasterized
/// texture; this function draws text labels (tiny-skia cannot render text)
/// and handles interactive hits (clicks → site switch, hover → tooltip/cursor).
///
/// `sites` is [`visible_radar_sites`]' output for this pane and frame: the
/// projection and the on-screen test are already done, so this walks only the
/// sites that can be seen.
///
/// `overlay_click_pos` must be taken from `PaneRenderCtx::overlay_click_pos`
/// (pre-filtered — dialog clicks are already stripped). Never pass a raw
/// `ctx.input()` click position here.
fn handle_radar_site_interactions(
    ui: &egui::Ui,
    zoom: f64,
    sites: &[VisibleSite],
    ctx: &mut PaneRenderCtx<'_>,
) {
    // Everything below used to arrive as seven separate parameters, all of
    // them copied out of `PaneRenderCtx` at the single call site. Destructuring
    // borrows the fields disjointly, so `pane` and `actions` stay mutable while
    // `excluded_rects` is read.
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
        let radar_site = &RADARS[site.index];
        let site_screen = site.screen;
        let icon_rect = site.icon_rect;

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
            // An icon answered this frame's click — the consumption half of
            // the fade trigger (see `PaneRenderCtx::click_consumed`).
            **click_consumed = true;
        }

        if let Some(pos) = hover_pos
            && icon_rect.contains(pos)
            && !is_pos_blocked(ui.ctx(), pos, pane_rect, excluded_rects)
        {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            // The feedhorn, not the ground: it is the figure a published
            // station record quotes as the radar's elevation, so it is the
            // one a reader can check this tooltip against.
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
                        fix.latitude, fix.longitude
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

/// The generic tick form: whole numbers bare, one decimal otherwise. Short is
/// the point — a tick label sits in the margin beside a 20px bar.
fn short_tick(value: f32) -> String {
    if value.fract().abs() < 0.01 {
        format!("{value:.0}")
    } else {
        format!("{value:.1}")
    }
}

/// Format a legend label value. For HHC uses category names; for others, a short numeric string.
///
/// Values arrive in the unit `get_color_for_value` takes, which for several
/// products is not the unit the user asked for: velocity's ramp is authored in
/// mph and sampled in m/s, MEHS's in inches (`rustdar-radar`'s `palette.rs`).
/// The colours stay where the palette put them and the *ticks* are converted
/// here, so a preference change relabels the bar without recolouring it.
fn format_legend_value(product: RadarProduct, value: f32, prefs: &UserPreferences) -> String {
    match product {
        RadarProduct::HydrometeorClassification => match value as u16 {
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
        },
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
            if converted < 1.0 {
                format!("{converted:.2}")
            } else {
                format!("{converted:.1}")
            }
        }
        // The ramp's stops are the NWS quarter-inch reporting steps; the ticks
        // are whatever unit the reader thinks in. Inches keep the generic short
        // form the bar has always been labelled with (¼-in stops as .2 / .5 /
        // .8), because widening every label by two characters to spell out a
        // precision the palette's own stops already imply costs margin the
        // labels do not have; cm and mm take the unit's own precision, which is
        // what keeps `25.40` off a 20px bar and makes each tick the same number
        // the hover readout gives for that value.
        RadarProduct::MaxExpectedHailSize => {
            let converted = prefs.hail_size.convert_from_inches(value);
            match prefs.hail_size {
                HailSizeUnit::Inches => short_tick(converted),
                unit => {
                    let decimals = unit.decimals();
                    format!("{converted:.decimals$}")
                }
            }
        }
        RadarProduct::CorrelationCoefficient => format!("{value:.2}"),
        RadarProduct::DifferentialReflectivity | RadarProduct::SpecificDifferentialPhase => {
            format!("{value:.1}")
        }
        _ => short_tick(value),
    }
}

// ── Pending-render notice ─────────────────────────────────────────────────

/// Font size of the pending-render notice. The color scale's title size, so the
/// notice reads as part of the same chrome rather than as an alert.
const PENDING_FONT_SIZE: f32 = 12.0;
/// Padding inside the notice's backing plate.
const PENDING_PADDING: egui::Vec2 = egui::vec2(8.0, 3.0);

/// What a pane says while the image on screen is not yet the product and tilt it
/// has selected.
///
/// It names what is **on screen**, which is the one piece of information nothing
/// else on the pane carries: the color scale, the tilt picker, the hover readout
/// and the status bar's data line have all already moved to the new selection, so
/// the pixels are the only thing left unlabelled. "Loading Velocity" would repeat
/// what the legend beside it already says and still leave the user unable to tell
/// what they are looking at.
///
/// **One wording for both datasources.** The situation is identical whichever
/// side a product is fetched from — a Level II render takes as long as it takes,
/// a Level III one additionally waits for its object to land — and a notice that
/// differed, or appeared for only one of them, would be a way to read the
/// datasource off the screen. That is exactly the tell the uniform data line was
/// introduced to remove.
fn pending_render_notice(product: RadarProduct, elevation: f32) -> String {
    format!("\u{27f3} showing {} {:.1}\u{b0}", product.name(), elevation)
}

/// Draw the notice across the top of the pane, over the imagery.
///
/// Deliberately non-blocking: the stale image stays fully visible and
/// undimmed. Somebody watching weather must never lose the picture to a
/// progress indicator — the picture is still real data, just not the field
/// they last asked for, and one product's echoes are better than none.
///
/// Wrapped rather than clipped, because the longest product name is wider than a
/// pane in a six-way split and a truncated notice about a mislabelled image would
/// be its own small lie.
fn draw_pending_render_notice(
    painter: &egui::Painter,
    pane_rect: egui::Rect,
    top_margin: f32,
    product: RadarProduct,
    elevation: f32,
) {
    let font = egui::FontId::proportional(PENDING_FONT_SIZE);
    let wrap_width = (pane_rect.width() - SCALE_MARGIN * 2.0 - PENDING_PADDING.x * 2.0).max(1.0);
    let galley = painter.layout(
        pending_render_notice(product, elevation),
        font,
        egui::Color32::WHITE,
        wrap_width,
    );
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

/// Render the color scale legend bar for the current pane's radar product.
///
/// `horizontal` is the panel-wide orientation resolved by
/// `pane::ColorScaleOrientation` — deliberately *not* recomputed from
/// `pane_rect` here, so that every pane in the grid draws its bars on the same
/// edge and dragging a divider cannot flip them.
pub(super) fn render_color_scale(
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
        // Gradient scales: per-pixel sampling for smooth interpolation.
        let steps = bar_length.ceil() as usize;
        for i in 0..steps {
            let t = i as f32 / (steps - 1).max(1) as f32;
            let value = min_val + t * range;
            let (r, g, b, a) = get_color_for_value(product, value);
            if a == 0 {
                continue;
            }
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
        .map(|(pos, text)| (*pos, text.as_str()))
        .collect();

    for (pixel_pos, text) in &thinned {
        if horizontal {
            // Labels above the bar
            let pos = egui::pos2(*pixel_pos, bar_rect.top() - 2.0);
            draw_shadowed_text(
                painter,
                pos,
                egui::Align2::CENTER_BOTTOM,
                text,
                label_font.clone(),
            );
        } else {
            // Labels to the left of the bar
            let pos = egui::pos2(bar_rect.left() - 4.0, *pixel_pos);
            draw_shadowed_text(
                painter,
                pos,
                egui::Align2::RIGHT_CENTER,
                text,
                label_font.clone(),
            );
        }
    }

    // --- Title: unit label above the bar (desktop) or to the left (mobile) ---
    let unit = product.unit_label(prefs);
    if horizontal {
        let title_pos = egui::pos2(bar_rect.left() - 4.0, bar_rect.center().y);
        draw_shadowed_text(
            painter,
            title_pos,
            egui::Align2::RIGHT_CENTER,
            unit,
            title_font,
        );
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
            egui::Rect::from_min_max(egui::pos2(left, top), egui::pos2(left + bar_length, bottom))
        } else {
            let right = pane_rect.right() - SCALE_MARGIN - offset_px;
            let left = right - SCALE_BAR_WIDTH;
            let bottom = pane_rect.bottom() - SCALE_MARGIN;
            let top = bottom - bar_length;
            egui::Rect::from_min_max(egui::pos2(left, top), egui::pos2(right, bottom))
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
            .map(|(pos, text)| (*pos, text.as_str()))
            .collect();

        for (pixel_pos, text) in &thinned {
            if horizontal {
                let pos = egui::pos2(*pixel_pos, bar_rect.top() - 2.0);
                draw_shadowed_text(
                    painter,
                    pos,
                    egui::Align2::CENTER_BOTTOM,
                    text,
                    label_font.clone(),
                );
            } else {
                let pos = egui::pos2(bar_rect.left() - 4.0, *pixel_pos);
                draw_shadowed_text(
                    painter,
                    pos,
                    egui::Align2::RIGHT_CENTER,
                    text,
                    label_font.clone(),
                );
            }
        }

        // Title
        let unit = legend.unit_label;
        if horizontal {
            let title_pos = egui::pos2(bar_rect.left() - 4.0, bar_rect.center().y);
            draw_shadowed_text(
                painter,
                title_pos,
                egui::Align2::RIGHT_CENTER,
                unit,
                title_font,
            );
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
    let draw_ctx = DrawPointContext {
        zoom: zoom_f32,
        is_dark,
    };
    let hit_radius = pf.overlays.point_hit_radius(pf.kind, zoom_f32);
    let hover_ctx = HoverContext { prefs: pf.prefs };

    let screen_rect = ui.max_rect();
    let margin = hit_radius + 40.0; // extra margin for station model elements
    let expanded = screen_rect.expand(margin);
    // Pre-compute viewport geo-bounds (with margin) so we can skip the
    // expensive Mercator projection for points that are clearly off-screen.
    let geo_bounds = viewport_geo_bounds(projector, expanded);

    let painter = ui.painter();

    // Blocked-ness is a property of the *position*, not of the point being
    // tested against it, so it is settled once here rather than inside the
    // loop. It used to be evaluated per visible station: each call scans the
    // excluded rects — up to 207 radar-site icons — and takes egui's memory
    // lock through `layer_id_at`, so 200 stations meant 200 lock acquisitions
    // and ~41,000 rect tests per pane per frame, every one of them returning
    // the same answer. `&&` short-circuits left to right, which is why the
    // cheap distance test in front of it did not save any of them.
    let blocked = |pos: egui::Pos2| is_pos_blocked(ui.ctx(), pos, pf.pane_rect, pf.excluded_rects);
    let hover_pos = ui.ctx().pointer_hover_pos().filter(|&p| !blocked(p));
    let click_pos = pf.overlay_click_pos.filter(|&p| !blocked(p));

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

    // Show tooltip for closest hovered point
    if let Some((_, id)) = closest_hover
        && let Some(hp) = hover_pos
        && let Some(text) = pf.overlays.hover_text(pf.kind, id, &hover_ctx)
    {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        map_hover_tooltip(
            ui.ctx(),
            egui::Id::new(("per_frame_overlay_hover", pf.kind as u8)),
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
/// radar value at the touched position.
///
/// Reached only from the touch pipeline. It used to live in `ui_mobile.rs`
/// behind `cfg(target_os = "android")`, which is why it was unreachable on a
/// touchscreen laptop and in a phone browser; the gate is now the runtime
/// modality, so this is plain platform-independent drawing code.
#[allow(clippy::too_many_arguments)]
fn draw_long_press_tooltip(
    ui: &egui::Ui,
    projector: &walkers::Projector,
    value_data: &[f32],
    lat: f64,
    lon: f64,
    touch_pos: egui::Pos2,
    pane: &PaneState,
    prefs: &UserPreferences,
) {
    use rustdar_radar::types::{IMAGE_SIZE, ImageBounds};

    let bounds = ImageBounds::from_radar_site(lat, lon);

    let nw = projector
        .project(walkers::lat_lon(bounds.max_lat, bounds.min_lon))
        .to_pos2();
    let se = projector
        .project(walkers::lat_lon(bounds.min_lat, bounds.max_lon))
        .to_pos2();
    let image_rect = egui::Rect::from_two_pos(nw, se);

    // Compute pixel coordinates inside the radar image
    let frac_x = (touch_pos.x - image_rect.left()) / image_rect.width();
    let frac_y = (touch_pos.y - image_rect.top()) / image_rect.height();
    let px = (frac_x * IMAGE_SIZE as f32) as i32;
    let py = (frac_y * IMAGE_SIZE as f32) as i32;

    let mut text = String::new();
    if px >= 0 && px < IMAGE_SIZE as i32 && py >= 0 && py < IMAGE_SIZE as i32 {
        let pixel_idx = py as usize * IMAGE_SIZE + px as usize;
        if pixel_idx < value_data.len() {
            let value = value_data[pixel_idx];
            if !value.is_nan() {
                text = pane.selected_product.format_value(value, prefs);
            }
        }
    }

    if text.is_empty() {
        text = "No data".into();
    }

    // Position tooltip above the finger
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

    /// The ticks `render_color_scale` would paint for `product`, in order.
    fn ticks(product: RadarProduct, prefs: &UserPreferences) -> Vec<String> {
        get_legend_scale(product)
            .thresholds
            .iter()
            .map(|&(value, _)| format_legend_value(product, value, prefs))
            .collect()
    }

    /// The MEHS colour bar is labelled in the user's hail-size unit.
    ///
    /// Its stops are authored in inches (`palette.rs`'s `MEHS` table), which is
    /// also the unit `get_color_for_value` is sampled in, so the
    /// *colours* must not move — only the numbers written beside them. Same
    /// arrangement velocity has had all along: an mph table sampled in m/s with
    /// the ticks converted at the label.
    ///
    /// The inches row is today's labelling unchanged, quarter-inch stops and
    /// all: nobody who has not opened the settings dialog sees a different bar.
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
                ticks(RadarProduct::MaxExpectedHailSize, &prefs),
                labels,
                "{unit:?} ticks",
            );
        }

        // The stops themselves are untouched by the preference: this is a
        // relabelling, not a repalettising.
        let inch_stops: Vec<f32> = get_legend_scale(RadarProduct::MaxExpectedHailSize)
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
    ///
    /// The failure this rules out is the half-converted one: a readout in
    /// millimetres beside a bar still labelled in inches, where every number on
    /// screen is individually right and the pane as a whole lies about the size
    /// of the hail. Asserted by rebuilding the readout out of the tick, so the
    /// two cannot drift apart in precision either.
    ///
    /// Inches are excluded on purpose: the ramp's ticks have always been the
    /// generic short form (`1.2` for the 1.25 in stop) while the readout gives
    /// hundredths, and this fix does not renumber the default bar.
    #[test]
    fn a_mehs_tick_and_the_hover_readout_are_the_same_number() {
        for unit in [HailSizeUnit::Centimeters, HailSizeUnit::Millimeters] {
            let prefs = UserPreferences {
                hail_size: unit,
                ..UserPreferences::default()
            };
            let product = RadarProduct::MaxExpectedHailSize;
            for &(stop, _) in &get_legend_scale(product).thresholds {
                let tick = format_legend_value(product, stop, &prefs);
                assert_eq!(
                    product.format_value(stop, &prefs),
                    format!("MEHS: {tick} {}", product.unit_label(&prefs)),
                    "{unit:?} at the {stop} in stop",
                );
            }
        }
    }

    /// Every other product's ticks are exactly what they were: the shared
    /// `short_tick` helper the MEHS arm was factored out of still answers for
    /// them, and no product picked up a hail-size conversion on the way past.
    #[test]
    fn no_other_products_ticks_moved() {
        let prefs = UserPreferences {
            hail_size: HailSizeUnit::Millimeters,
            ..UserPreferences::default()
        };
        let default = UserPreferences::default();
        for &product in RadarProduct::all() {
            if product == RadarProduct::MaxExpectedHailSize {
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

    /// The drag hint prints the box that will commit, not the raw drag.
    ///
    /// Fed a half-width far past the resampler's maximum — the raw value a
    /// long drag would carry if `RegionDrag::extend_to`'s cap were ever lost —
    /// the hint must read the clamped 460 km, because that is the box
    /// `VolumeRegion::new` will commit. A hint formatted from its raw input
    /// would read 800 km over a square that resamples 460, with a per-cell
    /// figure flattered by the same ratio: the number the region drag exists
    /// to show, wrong exactly when the user is at the control's stop.
    #[test]
    fn the_drag_hint_reads_the_clamped_width_past_the_cap() {
        let centre = crate::pane::GeoPoint {
            lat: 35.3,
            lon: -97.3,
        };
        let max = rustdar_radar::voxel::MAX_HALF_WIDTH_KM;
        let cells = rustdar_radar::voxel::default_shape().nx;
        let text = region_hint_text(centre, 400.0).expect("a hint for a point on Earth");
        assert_eq!(
            text,
            format!(
                "{:.0} km - {:.2} km/cell",
                2.0 * max,
                2.0 * max / cells as f64,
            ),
            "a 400 km half-width drag must be shown as the {:.0} km box it \
             commits, not the 800 km it painted",
            2.0 * max,
        );
    }
}
