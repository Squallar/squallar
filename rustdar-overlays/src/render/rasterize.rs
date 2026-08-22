//! Rasterize overlay polygons to RGBA textures using tiny-skia.

use std::collections::{HashMap, HashSet};

use rustdar_geo::lat_rad_to_mercator_y;
use tiny_skia::{Color, FillRule, LineCap, Paint, PathBuilder, Pixmap, Stroke, Transform};

use std::sync::Arc;

use crate::nws::alert::AlertCategory;
use crate::render::overlay_state::OverlayItem;
use crate::spc::colors::{md_fill_color, md_stroke_color};
use crate::spc::reports::StormReportKind;
use crate::types::OverlayFeature;
use rustdar_geo::{GeoBounds, GeoPolygonRing};

/// The portable half of click detection: which quarter-resolution cells the
/// rasterizer drew which item **indices** into — positions in its input list,
/// so both halves must come from one order.
#[derive(Debug, Clone, PartialEq)]
pub struct HitCells {
    pub width: u32,
    pub height: u32,
    pub cells: HashMap<u32, Vec<u32>>,
}

impl HitCells {
    pub fn new(full_width: u32, full_height: u32) -> Self {
        Self {
            width: full_width.div_ceil(4),
            height: full_height.div_ceil(4),
            cells: HashMap::new(),
        }
    }

    pub fn record(&mut self, px: f32, py: f32, item_id: u32) {
        let qx = (px as u32) / 4;
        let qy = (py as u32) / 4;
        if qx >= self.width || qy >= self.height {
            return;
        }
        let idx = qy * self.width + qx;
        let ids = self
            .cells
            .entry(idx)
            .or_insert_with(|| Vec::with_capacity(1));
        if !ids.contains(&item_id) {
            ids.push(item_id);
        }
    }

    pub fn ids_at(&self, u: f32, v: f32) -> &[u32] {
        if !(0.0..=1.0).contains(&u) || !(0.0..=1.0).contains(&v) {
            return &[];
        }
        let qx = ((u * self.width as f32) as u32).min(self.width.saturating_sub(1));
        let qy = ((v * self.height as f32) as u32).min(self.height.saturating_sub(1));
        let idx = qy * self.width + qx;
        self.cells.get(&idx).map_or(&[], Vec::as_slice)
    }

    pub fn max_id(&self) -> Option<u32> {
        self.cells.values().flatten().copied().max()
    }
}

#[derive(Clone)]
pub struct HitMap {
    cells: HitCells,
    id_map: HashMap<u32, Arc<dyn OverlayItem>>,
}

impl HitMap {
    /// `items[i]` **must** be the item whose row travelled at position `i` of
    /// the described input, because a cell records positions and nothing else.
    pub fn from_cells(cells: HitCells, items: &[Arc<dyn OverlayItem>]) -> Self {
        Self {
            cells,
            id_map: items
                .iter()
                .enumerate()
                .map(|(i, item)| (i as u32, Arc::clone(item)))
                .collect(),
        }
    }

    pub fn hit_test(&self, u: f32, v: f32) -> Vec<Arc<dyn OverlayItem>> {
        self.cells
            .ids_at(u, v)
            .iter()
            .filter_map(|id| self.id_map.get(id).cloned())
            .collect()
    }
}

/// Which of the two RGBA conventions a rasterizer's bytes are written in;
/// picking the wrong one shifts every translucent colour. tiny-skia output is
/// [`Self::Premultiplied`], [`rasterize_gridded`] is [`Self::Straight`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlphaMode {
    Premultiplied,
    Straight,
}

pub struct RasterizeOutput {
    pub rgba: Vec<u8>,
    pub hit_cells: Option<HitCells>,
    pub alpha: AlphaMode,
}

impl std::fmt::Debug for RasterizeOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RasterizeOutput")
            .field("rgba_len", &self.rgba.len())
            .field(
                "hit_cells_occupied",
                &self.hit_cells.as_ref().map(|cells| cells.cells.len()),
            )
            .field("alpha", &self.alpha)
            .finish()
    }
}

impl rustdar_source::job::JobOut for RasterizeOutput {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn into_any(self: Box<Self>) -> Box<dyn std::any::Any> {
        self
    }

    fn straight_rasters_mut(&mut self) -> Vec<&mut [u8]> {
        match self.alpha {
            AlphaMode::Premultiplied => Vec::new(),
            AlphaMode::Straight => {
                self.alpha = AlphaMode::Premultiplied;
                vec![&mut self.rgba]
            }
        }
    }
}

/// Web Mercator's own limit, from [`rustdar_geo`].
const MAX_MERCATOR_LAT: f64 = rustdar_geo::MERCATOR_LAT_LIMIT_DEG;

#[derive(Debug, Clone, Copy)]
pub(crate) struct MercatorBounds {
    min_lon: f64,
    max_lon: f64,
    merc_y_min: f64, // south edge
    merc_y_max: f64, // north edge
}

impl MercatorBounds {
    pub(crate) fn from_geo(bounds: &GeoBounds) -> Self {
        let clamped_min = bounds.min_lat.clamp(-MAX_MERCATOR_LAT, MAX_MERCATOR_LAT);
        let clamped_max = bounds.max_lat.clamp(-MAX_MERCATOR_LAT, MAX_MERCATOR_LAT);
        Self {
            min_lon: bounds.min_lon,
            max_lon: bounds.max_lon,
            merc_y_min: lat_rad_to_mercator_y(clamped_min.to_radians()),
            merc_y_max: lat_rad_to_mercator_y(clamped_max.to_radians()),
        }
    }

    /// Bring a longitude into this box's own frame: `[min_lon, min_lon + 360)`.
    #[inline]
    pub(crate) fn wrap_lon(&self, lon: f64) -> f64 {
        self.min_lon + (lon - self.min_lon).rem_euclid(360.0)
    }

    /// The whole multiple of 360° carrying a datum spanning `[min_lon, max_lon]`
    /// to its representation *nearest* this box — a rigid translation, only defined
    /// for a datum inside a half-turn.
    #[inline]
    pub(crate) fn lon_shift(&self, min_lon: f64, max_lon: f64) -> f64 {
        crate::render::geo::lon_shift(min_lon, max_lon, self.min_lon, self.max_lon)
    }

    #[inline]
    pub(crate) fn nearest_lon(&self, lon: f64) -> f64 {
        lon + self.lon_shift(lon, lon)
    }

    /// To texture pixel coordinates. Longitude is mapped linearly and no shift
    /// is applied here, so each caller states its own frame.
    #[inline]
    pub(crate) fn project(&self, lat: f64, lon: f64, w: f32, h: f32) -> (f32, f32) {
        let lon_frac = (lon - self.min_lon) / (self.max_lon - self.min_lon);
        let merc_y = lat_rad_to_mercator_y(lat.to_radians());
        let merc_frac = (merc_y - self.merc_y_min) / (self.merc_y_max - self.merc_y_min);
        let px = (lon_frac * w as f64) as f32;
        // Y is inverted: top of texture = north = max Mercator Y.
        let py = ((1.0 - merc_frac) * h as f64) as f32;
        (px, py)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct OutlooksInput {
    pub features: Vec<OverlayFeature>,
    pub hatch_color: [u8; 4],
    pub device_scale: f32,
}

rustdar_source::impl_job_input!(OutlooksInput);

pub fn rasterize_spc_outlooks(
    input: &OutlooksInput,
    bounds: &GeoBounds,
    width: u32,
    height: u32,
) -> RasterizeOutput {
    let OutlooksInput {
        features,
        hatch_color,
        device_scale,
    } = input;
    let scale = sane_device_scale(*device_scale);
    let Some(mut pixmap) = Pixmap::new(width, height) else {
        log::error!(
            "Pixmap allocation failed in rasterize_spc_outlooks ({}×{})",
            width,
            height
        );
        return RasterizeOutput {
            rgba: vec![0u8; (width * height * 4) as usize],
            hit_cells: None,
            alpha: AlphaMode::Premultiplied,
        };
    };
    let mb = MercatorBounds::from_geo(bounds);
    let w = width as f32;
    let h = height as f32;

    // Two passes: hatching must go over every fill, including later features.
    for feature in features {
        draw_feature(&mut pixmap, feature, &mb, w, h, scale);
    }
    crate::render::hatch::draw_hatch_pass(&mut pixmap, features, &mb, w, h, *hatch_color);

    RasterizeOutput {
        rgba: pixmap.take(),
        hit_cells: None,
        alpha: AlphaMode::Premultiplied,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiscussionPaint {
    pub md_type: crate::spc::discussion::MdType,
    pub polygon: rustdar_geo::GeoPolygon,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiscussionsInput {
    pub discussions: Vec<DiscussionPaint>,
    pub device_scale: f32,
}

rustdar_source::impl_job_input!(DiscussionsInput);

pub fn rasterize_spc_discussions(
    input: &DiscussionsInput,
    bounds: &GeoBounds,
    width: u32,
    height: u32,
) -> RasterizeOutput {
    let DiscussionsInput {
        discussions,
        device_scale,
    } = input;
    let scale = sane_device_scale(*device_scale);
    let Some(mut pixmap) = Pixmap::new(width, height) else {
        log::error!(
            "Pixmap allocation failed in rasterize_spc_discussions ({}×{})",
            width,
            height
        );
        return RasterizeOutput {
            rgba: vec![0u8; (width * height * 4) as usize],
            hit_cells: None,
            alpha: AlphaMode::Premultiplied,
        };
    };
    let mb = MercatorBounds::from_geo(bounds);
    let w = width as f32;
    let h = height as f32;

    for md in discussions {
        let fill_rgba = md_fill_color(&md.md_type);
        let stroke_rgba = md_stroke_color(&md.md_type);

        for ring in &md.polygon {
            if ring.len() < 3 {
                continue;
            }
            // No longitude shift here: `spc::discussion::parse_coord_token`
            // drops any point outside `(-140.0..=-50.0)`.
            let pts: Vec<(f32, f32)> = ring
                .iter()
                .map(|&(lat, lon)| mb.project(lat, lon, w, h))
                .collect();
            if let Some(path) = build_polygon_path(&pts) {
                fill_path(&mut pixmap, &path, fill_rgba, FillRule::Winding);
                let sw = scaled_stroke_width(&path, 2.0, scale);
                stroke_path(&mut pixmap, &path, stroke_rgba, sw);
            }
        }
    }

    RasterizeOutput {
        rgba: pixmap.take(),
        hit_cells: None,
        alpha: AlphaMode::Premultiplied,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AlertPaint {
    pub id: String,
    pub category: AlertCategory,
    pub features: Arc<Vec<OverlayFeature>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AlertsInput {
    pub alerts: Vec<AlertPaint>,
    pub enabled_categories: Vec<AlertCategory>,
    pub hidden_ids: HashSet<String>,
    pub device_scale: f32,
}

rustdar_source::impl_job_input!(AlertsInput);

pub fn rasterize_nws_alerts(
    input: &AlertsInput,
    bounds: &GeoBounds,
    width: u32,
    height: u32,
) -> RasterizeOutput {
    let AlertsInput {
        alerts,
        enabled_categories,
        hidden_ids,
        device_scale,
    } = input;
    let scale = sane_device_scale(*device_scale);
    let Some(mut pixmap) = Pixmap::new(width, height) else {
        log::error!(
            "Pixmap allocation failed in rasterize_nws_alerts ({}×{})",
            width,
            height
        );
        return RasterizeOutput {
            rgba: vec![0u8; (width * height * 4) as usize],
            hit_cells: None,
            alpha: AlphaMode::Premultiplied,
        };
    };
    let mb = MercatorBounds::from_geo(bounds);
    let w = width as f32;
    let h = height as f32;

    for alert in alerts {
        if !enabled_categories.contains(&alert.category) || hidden_ids.contains(&alert.id) {
            continue;
        }
        for feature in alert.features.iter() {
            draw_feature(&mut pixmap, feature, &mb, w, h, scale);
        }
    }

    RasterizeOutput {
        rgba: pixmap.take(),
        hit_cells: None,
        alpha: AlphaMode::Premultiplied,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RadarSiteInfo {
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    pub is_current: bool,
    pub is_loading: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SitesInput {
    pub sites: Vec<RadarSiteInfo>,
    pub zoom: f64,
    pub is_dark: bool,
    pub device_scale: f32,
}

rustdar_source::impl_job_input!(SitesInput);

pub fn rasterize_radar_sites(
    input: &SitesInput,
    bounds: &GeoBounds,
    width: u32,
    height: u32,
) -> RasterizeOutput {
    let SitesInput {
        sites,
        zoom,
        is_dark,
        device_scale,
    } = input;
    let (zoom, is_dark, device_scale) = (*zoom, *is_dark, *device_scale);
    let scale = sane_device_scale(device_scale);
    let Some(mut pixmap) = Pixmap::new(width, height) else {
        log::error!(
            "Pixmap allocation failed in rasterize_radar_sites ({}×{})",
            width,
            height
        );
        return RasterizeOutput {
            rgba: vec![0u8; (width * height * 4) as usize],
            hit_cells: None,
            alpha: AlphaMode::Premultiplied,
        };
    };
    let mb = MercatorBounds::from_geo(bounds);
    let w = width as f32;
    let h = height as f32;

    let zoom_f32 = zoom as f32;
    let radius = ((5.0 + zoom_f32).clamp(4.0, 12.0)).max(1.0) * scale;
    let stroke_w = (radius * 0.3).clamp(0.5 * scale, 2.0 * scale);

    let text_bg = if is_dark {
        Color::from_rgba8(0, 0, 0, 140)
    } else {
        Color::from_rgba8(255, 255, 255, 140)
    };

    for site in sites {
        // Into the viewport's frame first: the catalogue folds longitude into
        // [-180, 180] while `bounds` is unfolded; 4 of 208 stations are east.
        let lon = mb.nearest_lon(site.lon);
        let (px, py) = mb.project(site.lat, lon, w, h);
        // 50 points of slack so a site just off-texture still contributes its label.
        let slack = 50.0 * scale;
        if px < -slack || px > w + slack || py < -slack || py > h + slack {
            continue;
        }

        let fill = if site.is_loading {
            Color::from_rgba8(160, 32, 240, 255) // purple
        } else if site.is_current {
            Color::from_rgba8(255, 100, 100, 255) // red
        } else {
            Color::from_rgba8(100, 150, 255, 255) // blue
        };

        let mut pb = PathBuilder::new();
        pb.push_circle(px, py, radius);
        if let Some(path) = pb.finish() {
            let mut paint = Paint::default();
            paint.set_color(fill);
            paint.anti_alias = true;
            pixmap.fill_path(
                &path,
                &paint,
                FillRule::Winding,
                Transform::identity(),
                None,
            );

            paint.set_color(Color::from_rgba8(255, 255, 255, 255));
            let stroke = Stroke {
                width: stroke_w,
                ..Stroke::default()
            };
            pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
        }

        // tiny-skia cannot render text: only the background pill is baked in,
        // and egui draws the label over it per frame.
        if zoom >= 5.0 {
            let label_w = (site.name.len() as f32 * 5.5 + 4.0) * scale;
            let label_h = 10.0 * scale;
            let lx = px - label_w / 2.0;
            let ly = py + radius + 2.0 * scale;
            let mut pb = PathBuilder::new();
            if let Some(rect) = tiny_skia::Rect::from_xywh(lx, ly, label_w, label_h) {
                pb.push_rect(rect);
            }
            if let Some(path) = pb.finish() {
                let mut paint = Paint::default();
                paint.set_color(text_bg);
                paint.anti_alias = true;
                pixmap.fill_path(
                    &path,
                    &paint,
                    FillRule::Winding,
                    Transform::identity(),
                    None,
                );
            }
        }
    }

    RasterizeOutput {
        rgba: pixmap.take(),
        hit_cells: None,
        alpha: AlphaMode::Premultiplied,
    }
}

/// Tornado: funnel, i.e. an inverted triangle.
fn draw_tornado_symbol(pixmap: &mut Pixmap, px: f32, py: f32, r: f32, color: Color) {
    let s = r * 0.6; // symbol half-size
    let mut pb = PathBuilder::new();
    pb.move_to(px - s, py - s * 0.7);
    pb.line_to(px + s, py - s * 0.7);
    pb.line_to(px, py + s * 0.9);
    pb.close();
    if let Some(path) = pb.finish() {
        let mut paint = Paint::default();
        paint.set_color(color);
        paint.anti_alias = true;
        pixmap.fill_path(
            &path,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }
}

/// Hail: filled core with four radiating ticks.
fn draw_hail_symbol(pixmap: &mut Pixmap, px: f32, py: f32, r: f32, color: Color) {
    let core = r * 0.3;
    let tick_inner = r * 0.35;
    let tick_outer = r * 0.65;
    let stroke_w = (r * 0.18).clamp(0.5, 1.5);

    let mut pb = PathBuilder::new();
    pb.push_circle(px, py, core);
    if let Some(path) = pb.finish() {
        let mut paint = Paint::default();
        paint.set_color(color);
        paint.anti_alias = true;
        pixmap.fill_path(
            &path,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }

    let mut paint = Paint::default();
    paint.set_color(color);
    paint.anti_alias = true;
    let stroke = Stroke {
        width: stroke_w,
        line_cap: LineCap::Round,
        ..Stroke::default()
    };
    let diag = std::f32::consts::FRAC_1_SQRT_2;
    let offsets: [(f32, f32); 4] = [(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)];
    for (dx, dy) in offsets {
        let (adx, ady) = if dx.abs() > 0.5 {
            (dx, diag * dy.signum().max(0.3))
        } else {
            (diag * dx.signum().max(0.3), dy)
        };
        let _ = ady;
        let _ = adx;
        let mut pb = PathBuilder::new();
        pb.move_to(px + dx * tick_inner, py + dy * tick_inner);
        pb.line_to(px + dx * tick_outer, py + dy * tick_outer);
        if let Some(path) = pb.finish() {
            pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
        }
    }
}

/// Wind: right-pointing chevron.
fn draw_wind_symbol(pixmap: &mut Pixmap, px: f32, py: f32, r: f32, color: Color) {
    let s = r * 0.55;
    let stroke_w = (r * 0.22).clamp(0.5, 2.0);

    let mut pb = PathBuilder::new();
    pb.move_to(px - s * 0.5, py - s);
    pb.line_to(px + s * 0.5, py);
    pb.line_to(px - s * 0.5, py + s);
    if let Some(path) = pb.finish() {
        let mut paint = Paint::default();
        paint.set_color(color);
        paint.anti_alias = true;
        let stroke = Stroke {
            width: stroke_w,
            line_cap: LineCap::Round,
            ..Stroke::default()
        };
        pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReportPaint {
    pub kind: StormReportKind,
    pub lat: f64,
    pub lon: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReportsInput {
    pub reports: Vec<ReportPaint>,
    pub zoom: f64,
    pub is_dark: bool,
    pub device_scale: f32,
}

rustdar_source::impl_job_input!(ReportsInput);

/// Tornado = red, hail = green, wind = blue. Below a 5 px radius the symbols
/// fall back to filled dots.
pub fn rasterize_storm_reports(
    input: &ReportsInput,
    bounds: &GeoBounds,
    width: u32,
    height: u32,
) -> RasterizeOutput {
    let ReportsInput {
        reports,
        zoom,
        is_dark,
        device_scale,
    } = input;
    let (zoom, is_dark) = (*zoom, *is_dark);
    let scale = sane_device_scale(*device_scale);
    let Some(mut pixmap) = Pixmap::new(width, height) else {
        log::error!(
            "Pixmap allocation failed in rasterize_storm_reports ({}×{})",
            width,
            height
        );
        return RasterizeOutput {
            rgba: vec![0u8; (width * height * 4) as usize],
            hit_cells: None,
            alpha: AlphaMode::Premultiplied,
        };
    };
    let mb = MercatorBounds::from_geo(bounds);
    let w = width as f32;
    let h = height as f32;

    let zoom_f32 = zoom as f32;
    let radius = (3.0 + zoom_f32 * 0.5).clamp(3.0, 10.0) * scale;
    let stroke_w = (radius * 0.3).clamp(0.5 * scale, 2.0 * scale);
    let hit_radius = radius + stroke_w;

    let outline = if is_dark {
        Color::from_rgba8(255, 255, 255, 220)
    } else {
        Color::from_rgba8(40, 40, 40, 220)
    };

    let mut hit_cells = HitCells::new(width, height);

    for (idx, report) in reports.iter().enumerate() {
        // Into the viewport's frame first — see `rasterize_radar_sites`.
        let lon = mb.nearest_lon(report.lon);
        let (px, py) = mb.project(report.lat, lon, w, h);
        let slack = 20.0 * scale;
        if px < -slack || px > w + slack || py < -slack || py > h + slack {
            continue;
        }

        let fill = match report.kind {
            StormReportKind::Tornado => Color::from_rgba8(220, 40, 40, 220),
            StormReportKind::Hail => Color::from_rgba8(40, 180, 40, 220),
            StormReportKind::Wind => Color::from_rgba8(40, 80, 220, 220),
        };

        let use_symbol = radius >= 5.0;

        let mut pb = PathBuilder::new();
        pb.push_circle(px, py, radius);
        if let Some(path) = pb.finish() {
            let mut paint = Paint {
                anti_alias: true,
                ..Paint::default()
            };

            if use_symbol {
                paint.set_color(fill);
                let stroke = Stroke {
                    width: stroke_w,
                    ..Stroke::default()
                };
                pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);

                match report.kind {
                    StormReportKind::Tornado => {
                        draw_tornado_symbol(&mut pixmap, px, py, radius, fill)
                    }
                    StormReportKind::Hail => draw_hail_symbol(&mut pixmap, px, py, radius, fill),
                    StormReportKind::Wind => draw_wind_symbol(&mut pixmap, px, py, radius, fill),
                }
            } else {
                paint.set_color(fill);
                pixmap.fill_path(
                    &path,
                    &paint,
                    FillRule::Winding,
                    Transform::identity(),
                    None,
                );

                paint.set_color(outline);
                let stroke = Stroke {
                    width: stroke_w,
                    ..Stroke::default()
                };
                pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
            }
        }

        // The report's position in the input list **is** its id.
        let item_id = idx as u32;
        let min_x = (px - hit_radius).max(0.0) as i32;
        let max_x = ((px + hit_radius) as i32).min(width as i32 - 1);
        let min_y = (py - hit_radius).max(0.0) as i32;
        let max_y = ((py + hit_radius) as i32).min(height as i32 - 1);
        let r2 = hit_radius * hit_radius;
        let mut sy = min_y;
        while sy <= max_y {
            let mut sx = min_x;
            while sx <= max_x {
                let dx = sx as f32 - px;
                let dy = sy as f32 - py;
                if dx * dx + dy * dy <= r2 {
                    hit_cells.record(sx as f32, sy as f32, item_id);
                }
                sx += 4;
            }
            sy += 4;
        }
    }

    RasterizeOutput {
        rgba: pixmap.take(),
        hit_cells: Some(hit_cells),
        alpha: AlphaMode::Premultiplied,
    }
}

fn draw_lightning_bolt(pixmap: &mut Pixmap, cx: f32, cy: f32, size: f32, rgba: [u8; 4]) {
    let s = size * 0.5;
    let mut pb = PathBuilder::new();
    pb.move_to(cx - s * 0.1, cy - s);
    pb.line_to(cx + s * 0.35, cy - s);
    pb.line_to(cx + s * 0.05, cy - s * 0.15);
    pb.line_to(cx + s * 0.35, cy - s * 0.15);
    pb.line_to(cx - s * 0.15, cy + s);
    pb.line_to(cx + s * 0.05, cy + s * 0.15);
    pb.line_to(cx - s * 0.25, cy + s * 0.15);
    pb.close();

    let Some(path) = pb.finish() else { return };
    let paint = tiny_skia::Paint {
        shader: tiny_skia::Shader::SolidColor(tiny_skia::Color::from_rgba8(
            rgba[0], rgba[1], rgba[2], rgba[3],
        )),
        anti_alias: true,
        ..Default::default()
    };
    pixmap.fill_path(
        &path,
        &paint,
        tiny_skia::FillRule::Winding,
        tiny_skia::Transform::identity(),
        None,
    );
}

/// Age ramp: white → yellow → orange → red, in thirds of `window_secs`.
fn time_decay_color(age_secs: f64, window_secs: f64, is_dark: bool) -> [u8; 4] {
    let t = (age_secs / window_secs).clamp(0.0, 1.0) as f32;
    let (r, g, b) = if t < 0.33 {
        let f = t / 0.33;
        (255, (255.0 - f * 30.0) as u8, (255.0 - f * 200.0) as u8)
    } else if t < 0.66 {
        let f = (t - 0.33) / 0.33;
        (255, (225.0 - f * 90.0) as u8, (55.0 - f * 55.0) as u8)
    } else {
        let f = (t - 0.66) / 0.34;
        ((255.0 - f * 55.0) as u8, (135.0 - f * 85.0) as u8, 0)
    };
    let alpha = if is_dark { 230 } else { 200 };
    [r, g, b, alpha]
}

/// GLM radiant energy → 0…1 bolt-size channel.
///
/// The 1e-16…1e-12 J clamp window covers event and group energies but not all
/// flash energies. Input must be CF-unpacked. `None` draws at the midpoint and
/// must not collapse to either end.
fn energy_size_scale(energy: Option<f32>) -> f32 {
    match energy {
        Some(e) => (e.log10().clamp(-16.0, -12.0) + 16.0) / 4.0,
        None => 0.5,
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FlashPaint {
    pub lat: f64,
    pub lon: f64,
    /// UTC. Aged against [`GlmStrikesInput::now`] for the fade ramp and cull.
    pub time: chrono::NaiveDateTime,
    /// Radiant energy in joules; sizes the bolt. `None` means unknown.
    pub energy: Option<f32>,
}

/// A flash's position in `flashes` is its hit-map id.
#[derive(Debug, Clone, PartialEq)]
pub struct GlmStrikesInput {
    pub flashes: Vec<FlashPaint>,
    pub zoom: f64,
    pub is_dark: bool,
    /// Flashes older than this many seconds are dropped; younger ones fade
    /// through [`time_decay_color`]'s ramp over it.
    pub time_window_secs: f64,
    /// **The page's clock at dispatch, never the worker's** — flash age is
    /// `now - flash.time`, so a worker's own clock would render another picture.
    pub now: chrono::NaiveDateTime,
    pub device_scale: f32,
}

rustdar_source::impl_job_input!(GlmStrikesInput);

pub fn rasterize_glm_strikes(
    input: &GlmStrikesInput,
    bounds: &GeoBounds,
    width: u32,
    height: u32,
) -> RasterizeOutput {
    let GlmStrikesInput {
        flashes,
        zoom,
        is_dark,
        time_window_secs,
        now,
        device_scale,
    } = input;
    let Some(mut pixmap) = Pixmap::new(width, height) else {
        log::error!(
            "Pixmap allocation failed in rasterize_glm_strikes ({}×{})",
            width,
            height
        );
        return RasterizeOutput {
            rgba: vec![0u8; (width * height * 4) as usize],
            hit_cells: None,
            alpha: AlphaMode::Premultiplied,
        };
    };
    let mb = MercatorBounds::from_geo(bounds);
    let w = width as f32;
    let h = height as f32;
    let mut hit_cells = HitCells::new(width, height);

    // ~12 points at zoom 6, clamped to 6-20 points, then taken into texels.
    let zoom_f32 = *zoom as f32;
    let base_size = (zoom_f32 * 2.0).clamp(6.0, 20.0) * sane_device_scale(*device_scale);

    for (i, flash) in flashes.iter().enumerate() {
        // Into the viewport's frame before either test: the flash carries a
        // folded longitude and `bounds` carries an unfolded one.
        let lon = mb.wrap_lon(flash.lon);
        if flash.lat < bounds.min_lat || flash.lat > bounds.max_lat || lon > bounds.max_lon {
            continue;
        }

        let age_secs = (*now - flash.time).num_milliseconds().max(0) as f64 / 1000.0;
        if age_secs > *time_window_secs {
            continue;
        }

        let (px, py) = mb.project(flash.lat, lon, w, h);
        if px < -base_size || px > w + base_size || py < -base_size || py > h + base_size {
            continue;
        }

        let bolt_size = base_size * (0.8 + energy_size_scale(flash.energy) * 0.4);

        let rgba = time_decay_color(age_secs, *time_window_secs, *is_dark);
        draw_lightning_bolt(&mut pixmap, px, py, bolt_size, rgba);

        let item_id = i as u32;
        let r = bolt_size * 0.6;
        let r2 = r * r;
        let mut sy = (py - r) as i32;
        let sy_end = (py + r) as i32;
        while sy <= sy_end {
            let mut sx = (px - r) as i32;
            let sx_end = (px + r) as i32;
            while sx <= sx_end {
                let dx = sx as f32 - px;
                let dy = sy as f32 - py;
                if dx * dx + dy * dy <= r2 {
                    hit_cells.record(sx as f32, sy as f32, item_id);
                }
                sx += 4;
            }
            sy += 4;
        }
    }

    RasterizeOutput {
        rgba: pixmap.take(),
        hit_cells: Some(hit_cells),
        alpha: AlphaMode::Premultiplied,
    }
}

fn draw_feature(
    pixmap: &mut Pixmap,
    feature: &OverlayFeature,
    mb: &MercatorBounds,
    w: f32,
    h: f32,
    scale: f32,
) {
    // Geo-AABB cull before any projection work, shifted into the texture's
    // frame first: `OverlayFeature::geo_bounds` is the raw GeoJSON extent while
    // a dateline viewport arrives as e.g. -195..-165.
    if let Some(ref fb) = feature.geo_bounds {
        let tb = GeoBounds {
            min_lat: merc_y_to_lat(mb.merc_y_min),
            max_lat: merc_y_to_lat(mb.merc_y_max),
            min_lon: mb.min_lon,
            max_lon: mb.max_lon,
        };
        let shift = mb.lon_shift(fb.min_lon, fb.max_lon);
        let shifted = GeoBounds {
            min_lon: fb.min_lon + shift,
            max_lon: fb.max_lon + shift,
            ..*fb
        };
        if !shifted.intersects(&tb) {
            return;
        }
    }

    for polygon in &feature.polygons {
        let Some(projected) = project_polygon(polygon, mb, w, h) else {
            continue;
        };
        if let Some((path, rule)) = build_filled_polygon_path(&projected.exterior, &projected.holes)
        {
            fill_path(pixmap, &path, feature.fill_rgba, rule);
            if feature.stroke_rgba[3] > 0 {
                let sw = scaled_stroke_width(&path, 1.5, scale);
                stroke_path(pixmap, &path, feature.stroke_rgba, sw);
            }
        }
    }
}

/// Thins the stroke below a 40-point minimum dimension, so a small polygon is
/// not swallowed by its own outline. Points in, texels per point as `scale`.
fn scaled_stroke_width(path: &tiny_skia::Path, base: f32, scale: f32) -> f32 {
    let b = path.bounds();
    let min_dim = b.width().min(b.height());
    (min_dim / 40.0 * base).clamp(0.5 * scale, base * scale)
}

/// A device scale that can be multiplied by: a zero draws nothing and a `NaN`
/// makes `Rect::from_xywh` return `None`, so the floor is one texel per point.
fn sane_device_scale(device_scale: f32) -> f32 {
    if device_scale.is_finite() {
        device_scale.max(1.0)
    } else {
        1.0
    }
}

/// Interior rings below this projected area (square pixels) are dropped: such a
/// ring encloses nothing, but the stroke would draw it as a hairline scratch.
const MIN_HOLE_AREA_PX: f32 = 0.25;

/// Interior rings thinner than this *on average* — twice the area over the
/// perimeter — are dropped as well: the worst measured spreads 0.39 px² over
/// 31.1 px of rim. A quarter of a pixel drops 78 of the 2,040 rings above the
/// area floor.
const MIN_HOLE_WIDTH_PX: f32 = 0.25;

fn ring_area_px(pts: &[(f32, f32)]) -> f32 {
    let n = pts.len();
    if n < 3 {
        return 0.0;
    }
    let mut twice = 0.0;
    for i in 0..n {
        let (x1, y1) = pts[i];
        let (x2, y2) = pts[(i + 1) % n];
        twice += x1 * y2 - x2 * y1;
    }
    (twice * 0.5).abs()
}

fn ring_perimeter_px(pts: &[(f32, f32)]) -> f32 {
    let n = pts.len();
    if n < 2 {
        return 0.0;
    }
    let mut total = 0.0;
    for i in 0..n {
        let (x1, y1) = pts[i];
        let (x2, y2) = pts[(i + 1) % n];
        total += ((x2 - x1).powi(2) + (y2 - y1).powi(2)).sqrt();
    }
    total
}

pub(crate) fn hole_is_drawable(pts: &[(f32, f32)]) -> bool {
    let area = ring_area_px(pts);
    let perimeter = ring_perimeter_px(pts);
    // `2 * area >= width * perimeter` rather than a division, so a ring of zero
    // perimeter cannot produce a NaN.
    area >= MIN_HOLE_AREA_PX && 2.0 * area >= MIN_HOLE_WIDTH_PX * perimeter
}

/// A GeoJSON polygon projected to texture pixels: the exterior ring and the
/// interior rings [`hole_is_drawable`] keeps — one definition of the interior,
/// so the fill and the hatch mask cannot disagree.
pub(crate) struct ProjectedPolygon {
    pub(crate) exterior: Vec<(f32, f32)>,
    pub(crate) holes: Vec<Vec<(f32, f32)>>,
}

/// The rigid longitude shift that carries `ring` into `mb`'s frame, from the
/// ring's own longitude extent. Zero for an empty ring.
fn ring_lon_shift(ring: &[(f64, f64)], mb: &MercatorBounds) -> f64 {
    match crate::render::geo::ring_lon_extent(ring) {
        Some((min_lon, max_lon)) => mb.lon_shift(min_lon, max_lon),
        None => 0.0,
    }
}

/// `None` when the exterior ring is too short to enclose anything. The whole
/// polygon is translated by one shared multiple of 360°, taken from the
/// *exterior* ring: sharing it is what makes the move rigid.
pub(crate) fn project_polygon(
    polygon: &[GeoPolygonRing],
    mb: &MercatorBounds,
    w: f32,
    h: f32,
) -> Option<ProjectedPolygon> {
    let exterior_ring = polygon.first()?;
    if exterior_ring.len() < 3 {
        return None;
    }
    let shift = ring_lon_shift(exterior_ring, mb);
    let project = |ring: &[(f64, f64)]| -> Vec<(f32, f32)> {
        strip_closing_dup(ring)
            .iter()
            .map(|&(lat, lon)| mb.project(lat, lon + shift, w, h))
            .collect()
    };
    // `polygon[1..]` are interior rings — holes.
    let holes = polygon[1..]
        .iter()
        .filter(|ring| ring.len() >= 3)
        .map(|ring| project(ring))
        .filter(|pts| hole_is_drawable(pts))
        .collect();
    Some(ProjectedPolygon {
        exterior: project(exterior_ring),
        holes,
    })
}

fn push_ring(pb: &mut PathBuilder, pts: &[(f32, f32)]) {
    pb.move_to(pts[0].0, pts[0].1);
    for &(x, y) in &pts[1..] {
        pb.line_to(x, y);
    }
    pb.close();
}

pub(crate) fn build_polygon_path(pts: &[(f32, f32)]) -> Option<tiny_skia::Path> {
    if pts.len() < 3 {
        return None;
    }
    let mut pb = PathBuilder::new();
    push_ring(&mut pb, pts);
    let path = pb.finish()?;
    let b = path.bounds();
    if b.width() < 0.1 || b.height() < 0.1 {
        return None;
    }
    Some(path)
}

/// A whole GeoJSON polygon — exterior ring plus interior rings — as one path,
/// with the fill rule it must be filled under.
///
/// **Even-odd, because that is what the hit test computes.**
/// `geo_point_in_feature` counts ray crossings per ring and never looks at
/// orientation. Measured over a 7,015-zone NWS cache, two of 2,064 interior
/// rings wind *with* their exterior and another 2,515 have zero area. The
/// exterior alone keeps `Winding`, since 4.7% of rings self-intersect.
pub(crate) fn build_filled_polygon_path(
    exterior: &[(f32, f32)],
    holes: &[Vec<(f32, f32)>],
) -> Option<(tiny_skia::Path, FillRule)> {
    let exterior_path = build_polygon_path(exterior)?;
    if holes.is_empty() {
        return Some((exterior_path, FillRule::Winding));
    }
    let mut pb = PathBuilder::new();
    pb.push_path(&exterior_path);
    for hole in holes {
        if hole.len() >= 3 {
            push_ring(&mut pb, hole);
        }
    }
    let path = pb.finish()?;
    Some((path, FillRule::EvenOdd))
}

fn fill_path(pixmap: &mut Pixmap, path: &tiny_skia::Path, rgba: [u8; 4], rule: FillRule) {
    if rgba[3] == 0 {
        return;
    }
    let mut paint = Paint::default();
    paint.set_color(Color::from_rgba8(rgba[0], rgba[1], rgba[2], rgba[3]));
    paint.anti_alias = true;
    pixmap.fill_path(path, &paint, rule, Transform::identity(), None);
}

fn stroke_path(pixmap: &mut Pixmap, path: &tiny_skia::Path, rgba: [u8; 4], width: f32) {
    let mut paint = Paint::default();
    paint.set_color(Color::from_rgba8(rgba[0], rgba[1], rgba[2], rgba[3]));
    paint.anti_alias = true;
    let stroke = Stroke {
        width,
        line_cap: LineCap::Round,
        ..Stroke::default()
    };
    pixmap.stroke_path(path, &paint, &stroke, Transform::identity(), None);
}

pub(crate) fn strip_closing_dup(ring: &[(f64, f64)]) -> &[(f64, f64)] {
    if ring.len() > 3 && ring.first() == ring.last() {
        &ring[..ring.len() - 1]
    } else {
        ring
    }
}

use crate::hrrr::HrrrGridData;

fn merc_y_to_lat(merc_y: f64) -> f64 {
    rustdar_geo::mercator_y_to_lat_rad(merc_y).to_degrees()
}

/// Half-open `(i, j)` ranges of the grid the rasterizer touches.
///
/// Carried on the wire rather than recomputed on the far side: the window math
/// runs through libm, and a re-derivation could land one index off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexWindow {
    pub i0: usize,
    pub i1: usize,
    pub j0: usize,
    pub j1: usize,
}

impl IndexWindow {
    pub fn is_empty(&self) -> bool {
        self.i0 >= self.i1 || self.j0 >= self.j1
    }

    pub fn area(&self) -> usize {
        if self.is_empty() {
            0
        } else {
            (self.i1 - self.i0) * (self.j1 - self.j0)
        }
    }

    /// This window cut down to the grid it indexes, so a window off a message
    /// port can never name a point past the grid that arrived beside it.
    fn clamped(&self, ni: usize, nj: usize) -> Self {
        Self {
            i0: self.i0.min(ni),
            i1: self.i1.min(ni),
            j0: self.j0.min(nj),
            j1: self.j1.min(nj),
        }
    }

    /// The cells this window can *draw*: one ring in from the edge, because
    /// sizing a cell reads its four neighbours, and only a neighbour inside the
    /// window has been projected.
    fn interior(&self, ni: usize, nj: usize) -> Self {
        Self {
            i0: self.i0 + usize::from(self.i0 > 0),
            i1: self.i1 - usize::from(self.i1 < ni),
            j0: self.j0 + usize::from(self.j0 > 0),
            j1: self.j1 - usize::from(self.j1 < nj),
        }
    }
}

/// How far outside the texture a grid point can be and still paint into it.
/// `0.55` cells is the overlap the cell loop applies; the rest is headroom.
const CELL_REACH: f64 = 0.75;

/// The same reach in pixels; the half-extent has a `0.5` px floor. Up from 1.5.
const PIXEL_REACH: f64 = 2.0;

/// `bounds` widened by `lon_pad` degrees and `merc_pad` of Mercator `y`.
///
/// Latitude is padded in Mercator, the axis the texture's pixels are linear in.
fn grow_bounds(bounds: &GeoBounds, lon_pad: f64, merc_pad: f64) -> GeoBounds {
    let mb = MercatorBounds::from_geo(bounds);
    GeoBounds {
        min_lon: bounds.min_lon - lon_pad,
        max_lon: bounds.max_lon + lon_pad,
        min_lat: merc_y_to_lat(mb.merc_y_min - merc_pad),
        max_lat: merc_y_to_lat(mb.merc_y_max + merc_pad),
    }
}

/// Which grid points [`rasterize_gridded`] must project: the whole grid
/// unless the coordinates can name a narrower window — see
/// [`crate::hrrr::GridCoords::index_bounds`].
///
/// A point *outside* the texture still paints into it, so the **box** is grown
/// before the index range is taken — a Lambert row is not a parallel. The pad is
/// **absolute**, not a fraction of the box.
fn projection_window(
    coords: &crate::hrrr::GridCoords,
    ni: usize,
    nj: usize,
    bounds: &GeoBounds,
    width: u32,
    height: u32,
) -> IndexWindow {
    let full = IndexWindow {
        i0: 0,
        i1: ni,
        j0: 0,
        j1: nj,
    };

    // A grid with a longitude discontinuity — anti-meridian or the cone's own
    // seam — has an `i` neighbour most of a turn away, so the "0.55 of a cell"
    // reach stops describing it.
    if coords.wraps_longitude() {
        return full;
    }

    // `cos` at the box's own extreme latitude: the only cells that can reach
    // the texture sit within a cell of the box.
    let edge_lat = bounds.min_lat.abs().max(bounds.max_lat.abs());
    let Some(cell_deg) = coords.cell_span_degrees(edge_lat) else {
        return full;
    };
    let mb = MercatorBounds::from_geo(bounds);
    let lon_pad = CELL_REACH * cell_deg
        + PIXEL_REACH * (bounds.max_lon - bounds.min_lon) / width.max(1) as f64;
    let merc_pad = CELL_REACH * cell_deg.to_radians()
        + PIXEL_REACH * (mb.merc_y_max - mb.merc_y_min) / height.max(1) as f64;

    let grown = grow_bounds(bounds, lon_pad, merc_pad);
    let Some((fi0, fi1, fj0, fj1)) = coords.index_bounds(&grown, ni, nj) else {
        return full;
    };

    // One more cell each way so every drawn cell's four neighbours are
    // projected too — an unprojected neighbour resizes the cell.
    let low = |f: f64, n: usize| (f.floor() - 1.0).max(0.0).min(n as f64) as usize;
    let high = |f: f64, n: usize| (f.ceil() + 1.0).max(0.0).min(n as f64) as usize;

    IndexWindow {
        i0: low(fi0, ni),
        i1: high(fi1, ni),
        j0: low(fj0, nj),
        j1: high(fj1, nj),
    }
}

/// What [`rasterize_gridded`] reads. The HRRR values vector is 1,905,141
/// `f32` — **7.62 MB** — and the raster only reads the points inside its own
/// [`projection_window`], so this is an enum over how much of the grid is in
/// hand: [`Self::Whole`] carries it by `Arc`, [`Self::Window`] carries the
/// window and exactly its values (the wire).
#[derive(Debug, Clone, PartialEq)]
pub enum GriddedInput {
    Whole(std::sync::Arc<HrrrGridData>),
    /// A whole grid held by a source that carries **no source-specific enum**
    /// into the raster — the shape a second gridded source registers in. Same
    /// posture as [`Self::Whole`] (hold it all, cut at encode), without the
    /// model's own type in the signature.
    Resident(std::sync::Arc<crate::render::gridded::ResidentGrid>),
    Window(GridWindow),
}

rustdar_source::impl_job_input!(GriddedInput);

/// The wire form of a gridded raster: the field's identity, the grid's shape
/// and coordinates, the [`IndexWindow`] its values were cut to, and those
/// values alone.
#[derive(Debug, Clone, PartialEq)]
pub struct GridWindow {
    /// **A field identity, not a source's own enum.** The raster resolves it
    /// through [`crate::render::gridded::field_paint`] and refuses what that
    /// does not answer, so a second gridded source needs no arm here.
    pub field: rustdar_source::product::FieldId,
    /// The **full grid's** shape, which `win` and `coords` index against.
    pub ni: usize,
    pub nj: usize,
    pub coords: crate::hrrr::GridCoords,
    /// The window `values` covers, computed at the dispatch and carried.
    pub win: IndexWindow,
    /// Row-major within `win`: point `(i, j)` of the grid is
    /// `values[(j - win.j0) * (win.i1 - win.i0) + (i - win.i0)]`.
    pub values: Vec<f32>,
}

impl GriddedInput {
    /// The field being drawn. The whole-grid arm reads it off the model's own
    /// registration, which is where the persisted spelling already lives.
    pub fn field(&self) -> &rustdar_source::product::FieldId {
        match self {
            Self::Whole(grid) => &crate::hrrr::fields::spec(grid.parameter).id,
            Self::Resident(grid) => &grid.field,
            Self::Window(window) => &window.field,
        }
    }

    pub fn shape(&self) -> (usize, usize) {
        match self {
            Self::Whole(grid) => (grid.ni, grid.nj),
            Self::Resident(grid) => (grid.ni, grid.nj),
            Self::Window(window) => (window.ni, window.nj),
        }
    }

    pub fn coords(&self) -> &crate::hrrr::GridCoords {
        match self {
            Self::Whole(grid) => &grid.coords,
            Self::Resident(grid) => &grid.coords,
            Self::Window(window) => &window.coords,
        }
    }

    /// The values of a grid held **whole**, with the row stride they are
    /// indexed by — `None` for the windowed arm, which carries only a cut.
    ///
    /// The two whole arms differ in nothing the raster reads, so the readers
    /// below take this rather than repeating an arm each.
    fn whole_values(&self) -> Option<(&[f32], usize)> {
        match self {
            Self::Whole(grid) => Some((&grid.values, grid.ni)),
            Self::Resident(grid) => Some((&grid.values, grid.ni)),
            Self::Window(_) => None,
        }
    }

    /// The index window a raster of `bounds` may draw from: computed for a whole
    /// grid, **carried** for a window.
    pub fn window_for(&self, bounds: &GeoBounds, width: u32, height: u32) -> IndexWindow {
        let (ni, nj) = self.shape();
        match self {
            Self::Window(window) => window.win.clamped(ni, nj),
            _ => projection_window(self.coords(), ni, nj, bounds, width, height),
        }
    }

    /// The value at grid point `(i, j)`, or `None` where there is none to read
    /// — past a short values vector, or outside the carried window.
    fn value_at(&self, i: usize, j: usize) -> Option<f32> {
        if let Some((values, stride)) = self.whole_values() {
            return values.get(j * stride + i).copied();
        }
        let Self::Window(window) = self else {
            return None;
        };
        let win = &window.win;
        if i < win.i0 || i >= win.i1 || j < win.j0 || j >= win.j1 {
            return None;
        }
        window
            .values
            .get((j - win.j0) * (win.i1 - win.i0) + (i - win.i0))
            .copied()
    }

    /// One row of `win`'s values, exactly as the wire writes them; the whole
    /// arm pads with NaN where its vector runs short, which paints nothing.
    /// A callback per row so the encoder writes straight from the grid's storage.
    pub fn for_each_window_row(&self, win: &IndexWindow, mut f: impl FnMut(&[f32])) {
        if win.is_empty() {
            return;
        }
        if let Some((values, stride)) = self.whole_values() {
            let mut padded: Vec<f32> = Vec::new();
            for j in win.j0..win.j1 {
                let start = j * stride + win.i0;
                let end = j * stride + win.i1;
                if end <= values.len() {
                    f(&values[start..end]);
                } else {
                    padded.clear();
                    padded.extend((start..end).map(|k| values.get(k).copied().unwrap_or(f32::NAN)));
                    f(&padded);
                }
            }
            return;
        }
        let Self::Window(window) = self else {
            return;
        };
        let carried = &window.win;
        let row_w = carried.i1 - carried.i0;
        for j in win.j0..win.j1 {
            let row = (j - carried.j0) * row_w;
            f(&window.values[row + (win.i0 - carried.i0)..row + (win.i1 - carried.i0)]);
        }
    }
}

/// Writes pixels directly rather than through tiny-skia: one filled rectangle
/// per grid point, sized from its neighbour spacing.
pub fn rasterize_gridded(
    input: &GriddedInput,
    bounds: &GeoBounds,
    width: u32,
    height: u32,
) -> RasterizeOutput {
    let size = (width * height * 4) as usize;
    let mut rgba = vec![0u8; size];
    let (ni, nj) = input.shape();
    let coords = input.coords();

    let empty = input.whole_values().is_some_and(|(v, _)| v.is_empty());
    if empty || width == 0 || height == 0 || ni == 0 || nj == 0 {
        return RasterizeOutput {
            rgba,
            hit_cells: None,
            alpha: AlphaMode::Straight,
        };
    }

    // Resolved once, outside the cell loop, and **refused** rather than
    // defaulted: a field this build does not register is a newer build's, and
    // painting it through some other field's colours would be a silent misread.
    let Some(paint) = crate::render::gridded::field_paint(input.field()) else {
        return RasterizeOutput {
            rgba,
            hit_cells: None,
            alpha: AlphaMode::Straight,
        };
    };

    let mb = MercatorBounds::from_geo(bounds);
    let w = width as f32;
    let h = height as f32;

    // Only points that can influence a pixel of *this* texture are projected;
    // `coords.at` over all 1.9 M points was two thirds of this function's cost.
    let win = input.window_for(bounds, width, height);
    if win.is_empty() {
        return RasterizeOutput {
            rgba,
            hit_cells: None,
            alpha: AlphaMode::Straight,
        };
    }
    let win_w = win.i1 - win.i0;

    // Pre-project once: the cell loop reads each neighbour several times.
    let mut px_coords: Vec<(f32, f32)> = Vec::with_capacity(win_w * (win.j1 - win.j0));
    for j in win.j0..win.j1 {
        for i in win.i0..win.i1 {
            match coords.at(j * ni + i) {
                Some((lat, lon)) => px_coords.push(mb.project(lat, lon, w, h)),
                None => px_coords.push((f32::NAN, f32::NAN)),
            }
        }
    }
    // Every read below is inside `win`; `interior` is what guarantees it.
    let at = |i: usize, j: usize| px_coords[(j - win.j0) * win_w + (i - win.i0)];

    let draw = win.interior(ni, nj);
    for j in draw.j0..draw.j1 {
        for i in draw.i0..draw.i1 {
            let Some(value) = input.value_at(i, j) else {
                continue;
            };
            let color = paint.color_for_value(value);
            if color[3] == 0 {
                continue;
            }

            let (cx, cy) = at(i, j);
            if cx.is_nan() || cy.is_nan() {
                continue;
            }

            // Half-extents from neighbour spacing. 0.55, not 0.50: a slight
            // overlap hides seams between adjacent cells.
            let dx_left = if i > 0 {
                let (nx, _) = at(i - 1, j);
                ((cx - nx).abs() * 0.55).max(0.5)
            } else if i + 1 < ni {
                let (nx, _) = at(i + 1, j);
                ((nx - cx).abs() * 0.55).max(0.5)
            } else {
                1.0
            };
            let dx_right = if i + 1 < ni {
                let (nx, _) = at(i + 1, j);
                ((nx - cx).abs() * 0.55).max(0.5)
            } else {
                dx_left
            };
            let dy_up = if j > 0 {
                let (_, ny) = at(i, j - 1);
                ((cy - ny).abs() * 0.55).max(0.5)
            } else if j + 1 < nj {
                let (_, ny) = at(i, j + 1);
                ((ny - cy).abs() * 0.55).max(0.5)
            } else {
                1.0
            };
            let dy_down = if j + 1 < nj {
                let (_, ny) = at(i, j + 1);
                ((ny - cy).abs() * 0.55).max(0.5)
            } else {
                dy_up
            };

            let x0 = ((cx - dx_left) as i32).max(0);
            let y0 = ((cy - dy_up) as i32).max(0);
            let x1 = ((cx + dx_right) as i32).min(width as i32 - 1);
            let y1 = ((cy + dy_down) as i32).min(height as i32 - 1);

            for y in y0..=y1 {
                let row_offset = (y as u32 * width * 4) as usize;
                for x in x0..=x1 {
                    let offset = row_offset + (x as u32 * 4) as usize;
                    // Overwrite — no blending between adjacent grid cells.
                    rgba[offset] = color[0];
                    rgba[offset + 1] = color[1];
                    rgba[offset + 2] = color[2];
                    rgba[offset + 3] = color[3];
                }
            }
        }
    }

    RasterizeOutput {
        rgba,
        hit_cells: None,
        alpha: AlphaMode::Straight,
    }
}

#[cfg(test)]
mod alpha_tests;

#[cfg(test)]
mod dateline_tests;

#[cfg(test)]
mod glm_energy_tests;

#[cfg(test)]
mod hole_tests;

#[cfg(test)]
pub(crate) mod lambert_fixture;

#[cfg(test)]
mod model_nan_tests;

#[cfg(test)]
mod model_window_tests;

#[cfg(test)]
mod projection_window_tests;

#[cfg(test)]
mod device_scale_tests;
