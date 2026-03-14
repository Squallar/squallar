use std::collections::{HashMap, HashSet};
use walkers::{HttpTiles, Texture, TileId, Tiles};
use crate::layers::LayerKind;
use crate::overlay_cache::{
    CachedFeature, OverlayLayerCache, ViewportKey, MeshAccumulator,
    build_cached_features, draw_cached_features,
};
use rustdar_overlays::spc::outlook::{OutlookDay, OutlookProduct, SpcOutlook};
use rustdar_overlays::spc::discussion::SpcDiscussion;
use rustdar_overlays::spc::colors::{md_fill_color, md_stroke_color};
use rustdar_overlays::nws::alert::NwsAlert;
use rustdar_overlays::types::HatchPattern;
use crate::tiles::{lon_to_tile_x, lat_to_tile_y, tile_to_lon, tile_to_lat};

// ---------------------------------------------------------------------------
/// Draw SPC convective outlook polygons on the map.
///
/// Uses cached projected + triangulated geometry to avoid per-frame O(n²)
/// ear-clip triangulation. The cache is invalidated on viewport changes
/// (pan/zoom) or when new SPC data arrives.
///
/// This is a free function (not a method on `Gui`) to avoid capturing the
/// entire `self` struct inside the map closure, which would conflict with
/// disjoint field borrows required by Rust's borrow checker.
pub(super) fn draw_spc_overlays(
    ui: &egui::Ui,
    projector: &walkers::Projector,
    zoom: f64,
    layers: &crate::layers::LayerManager,
    spc_outlooks: &HashMap<(OutlookDay, OutlookProduct), SpcOutlook>,
    current_theme_is_dark: bool,
    caches: &mut HashMap<(OutlookDay, OutlookProduct), OverlayLayerCache>,
    data_generations: &HashMap<(OutlookDay, OutlookProduct), u64>,
) {
    let screen_rect = ui.max_rect();
    let key = ViewportKey::from_projector_and_rect(projector, zoom, screen_rect);
    let day = layers.spc_day;

    let hatch_color = if current_theme_is_dark {
        egui::Color32::from_rgba_unmultiplied(200, 200, 200, 180)
    } else {
        egui::Color32::from_rgba_unmultiplied(60, 60, 60, 180)
    };

    // Iterate through enabled SPC layers
    for layer_kind in layers.spc_layers_for_day() {
        if !layers.is_enabled(layer_kind) {
            continue;
        }
        let Some(product) = layer_kind.to_outlook_product() else {
            continue;
        };
        let Some(outlook) = spc_outlooks.get(&(day, product)) else {
            continue;
        };

        let data_gen = data_generations.get(&(day, product)).copied().unwrap_or(0);
        let cache = caches.entry((day, product)).or_insert_with(OverlayLayerCache::new);

        // Rebuild on any viewport or data change. This is cheap because
        // O(n²) triangulation is pre-computed at fetch time — only O(n)
        // vertex projection runs here.
        if !cache.is_valid(&key, data_gen) {
            cache.features = build_cached_features(
                &outlook.features,
                projector,
                screen_rect,
                true,
            );
            cache.viewport_key = key;
            cache.data_generation = data_gen;
        }

        // Draw from cache — all geometry is batched into minimal painter calls
        draw_cached_features(
            ui.painter(),
            &cache.features,
            &outlook.features,
            screen_rect,
            hatch_color,
        );
    }
}

/// Draw SPC Mesoscale Discussion polygons on the map.
///
/// Uses cached projected geometry. On cache miss, temporary `OverlayFeature`
/// wrappers are built for each MD's polygon so `build_cached_features` can
/// pre-triangulate them.
///
/// Returns `Some(discussion_index)` if the user clicked on an MD polygon.
pub(super) fn draw_spc_discussions(
    ui: &egui::Ui,
    projector: &walkers::Projector,
    zoom: f64,
    layers: &crate::layers::LayerManager,
    discussions: &[SpcDiscussion],
    cache: &mut OverlayLayerCache,
    data_gen: u64,
    pointer_available: bool,
) -> Option<usize> {
    if !layers.is_enabled(LayerKind::SpcMesoscaleDiscussions) || discussions.is_empty() {
        return None;
    }

    // Pre-check: is the click on a foreground UI layer (e.g. side panel)?
    // NOTE: `layer_id_at` must be called OUTSIDE the `input()` closure to avoid
    // re-entrant read-locking on the egui Context's RwLock, which deadlocks when
    // a background writer (e.g. walkers tile IO calling `request_repaint`) is pending.
    let (any_click, click_pos) = ui.ctx().input(|i| {
        (i.pointer.any_click(), i.pointer.interact_pos())
    });
    let click_on_ui = any_click
        && click_pos.is_some_and(|p| {
            ui.ctx()
                .layer_id_at(p)
                .is_some_and(|l| l.order > egui::Order::Background)
        });

    let screen_rect = ui.max_rect();
    let key = ViewportKey::from_projector_and_rect(projector, zoom, screen_rect);

    // Rebuild on any viewport or data change
    if !cache.is_valid(&key, data_gen) {
        let temp_features: Vec<rustdar_overlays::types::OverlayFeature> = discussions
            .iter()
            .map(|md| {
                let fill = md_fill_color(&md.md_type);
                let stroke = md_stroke_color(&md.md_type);
                let polygons: Vec<Vec<Vec<(f64, f64)>>> = md
                    .polygon
                    .iter()
                    .map(|ring| vec![ring.clone()])
                    .collect();
                rustdar_overlays::types::OverlayFeature::new(
                    polygons,
                    fill,
                    stroke,
                    String::new(),
                    String::new(),
                    HatchPattern::None,
                )
            })
            .collect();

        cache.features = build_cached_features(
            &temp_features,
            projector,
            screen_rect,
            false,
        );
        cache.viewport_key = key;
        cache.data_generation = data_gen;
    }

    let mut clicked_index: Option<usize> = None;
    let painter = ui.painter();
    let mut acc = MeshAccumulator::new();

    for (md_idx, (md, cached_feat)) in discussions.iter().zip(cache.features.iter()).enumerate() {
        let [r, g, b, a] = md_fill_color(&md.md_type);
        let fill = egui::Color32::from_rgba_unmultiplied(r, g, b, a);
        let [sr, sg, sb, _sa] = md_stroke_color(&md.md_type);
        let stroke_color = egui::Color32::from_rgba_unmultiplied(sr, sg, sb, _sa);

        for cached_poly in &cached_feat.polygons {
            if !screen_rect.intersects(cached_poly.poly_rect) {
                continue;
            }

            acc.append_polygon(cached_poly, fill, stroke_color, 2.0);

            // MD number label at polygon centroid
            if !cached_poly.screen_pts.is_empty() {
                let cx = cached_poly.screen_pts.iter().map(|p| p.x).sum::<f32>()
                    / cached_poly.screen_pts.len() as f32;
                let cy = cached_poly.screen_pts.iter().map(|p| p.y).sum::<f32>()
                    / cached_poly.screen_pts.len() as f32;
                painter.text(
                    egui::pos2(cx, cy),
                    egui::Align2::CENTER_CENTER,
                    format!("MD {}", md.number),
                    egui::FontId::proportional(11.0),
                    stroke_color,
                );
            }

            // Click detection (SPC discussions)
            if pointer_available && !click_on_ui && clicked_index.is_none()
                && any_click
                && click_pos.is_some_and(|p| {
                    cached_poly.poly_rect.contains(p)
                        && crate::geo::point_in_polygon(p, &cached_poly.screen_pts)
                })
            {
                clicked_index = Some(md_idx);
            }
        }
    }

    acc.emit(painter);

    clicked_index
}

/// Draw NWS weather alert polygons on the map.
///
/// Uses cached projected geometry. The cache stores a flat list of
/// `CachedFeature`s built from all alerts' features in order, so the
/// drawing loop can reconstruct the alert→feature mapping cheaply.
///
/// Returns `Some(alert_index)` if the user clicked on an alert polygon,
/// allowing the caller to open a detail popup.
///
/// This is a free function (not a method on `Gui`) for the same borrow-checker
/// reasons as `draw_spc_overlays`.
pub(super) fn draw_nws_alerts(
    ui: &egui::Ui,
    projector: &walkers::Projector,
    zoom: f64,
    layers: &crate::layers::LayerManager,
    nws_alerts: &[NwsAlert],
    hidden_alerts: &HashSet<String>,
    cache: &mut OverlayLayerCache,
    data_gen: u64,
    pointer_available: bool,
) -> Option<usize> {
    if !layers.any_nws_enabled() || nws_alerts.is_empty() {
        return None;
    }

    // Pre-check: is the click on a foreground UI layer (e.g. side panel)?
    // NOTE: `layer_id_at` must be called OUTSIDE the `input()` closure to avoid
    // re-entrant read-locking on the egui Context's RwLock, which deadlocks when
    // a background writer (e.g. walkers tile IO calling `request_repaint`) is pending.
    let (any_click, click_pos) = ui.ctx().input(|i| {
        (i.pointer.any_click(), i.pointer.interact_pos())
    });
    let click_on_ui = any_click
        && click_pos.is_some_and(|p| {
            ui.ctx()
                .layer_id_at(p)
                .is_some_and(|l| l.order > egui::Order::Background)
        });

    let screen_rect = ui.max_rect();
    let key = ViewportKey::from_projector_and_rect(projector, zoom, screen_rect);

    // Rebuild on any viewport or data change.
    // Cache is built for ALL alerts (regardless of enabled categories) so that
    // layer toggles don't require an expensive cache rebuild.
    if !cache.is_valid(&key, data_gen) {
        let mut all_cached: Vec<CachedFeature> = Vec::new();
        for alert in nws_alerts.iter() {
            let cached = build_cached_features(
                &alert.features,
                projector,
                screen_rect,
                false,
            );
            all_cached.extend(cached);
        }
        cache.features = all_cached;
        cache.viewport_key = key;
        cache.data_generation = data_gen;
    }

    let enabled_categories = layers.enabled_nws_categories();
    let mut clicked_index: Option<usize> = None;
    let painter = ui.painter();
    let mut acc = MeshAccumulator::new();

    // Walk the flat cache in the same alert→feature order it was built.
    let mut flat_idx = 0;
    for (alert_idx, alert) in nws_alerts.iter().enumerate() {
        let skip = !enabled_categories.contains(&alert.category)
            || hidden_alerts.contains(&alert.id);
        for src_feature in &alert.features {
            if flat_idx >= cache.features.len() {
                break;
            }
            let cached_feat = &cache.features[flat_idx];
            flat_idx += 1;

            if skip {
                continue;
            }

            let [r, g, b, a] = src_feature.fill_rgba;
            let fill = egui::Color32::from_rgba_unmultiplied(r, g, b, a);
            let [sr, sg, sb, _sa] = src_feature.stroke_rgba;
            let stroke_color = egui::Color32::from_rgba_unmultiplied(sr, sg, sb, _sa);

            for cached_poly in &cached_feat.polygons {
                if !screen_rect.intersects(cached_poly.poly_rect) {
                    continue;
                }

                acc.append_polygon(cached_poly, fill, stroke_color, 2.0);

                // Click detection (NWS alerts)
                if pointer_available && !click_on_ui && clicked_index.is_none()
                    && any_click
                    && click_pos.is_some_and(|p| {
                        cached_poly.poly_rect.contains(p)
                            && crate::geo::point_in_polygon(p, &cached_poly.screen_pts)
                    })
                {
                    clicked_index = Some(alert_idx);
                }
            }
        }
    }

    acc.emit(painter);

    clicked_index
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
