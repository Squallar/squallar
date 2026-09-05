//! Rasterize overlay polygons to RGBA textures using tiny-skia.

use std::collections::HashSet;

use squallar_geo::lat_rad_to_mercator_y;
use tiny_skia::{Color, FillRule, LineCap, Paint, PathBuilder, Pixmap, Stroke, Transform};

use std::sync::Arc;

use crate::nws::alert::AlertCategory;
use crate::render::overlay_state::{HitItems, OverlayItem};
use crate::spc::colors::{md_fill_color, md_stroke_color};
use crate::spc::reports::StormReportKind;
use crate::types::OverlayFeature;
use squallar_geo::{GeoBounds, GeoPolygonRing};

/// Occupied cell index to the item indices drawn into it.
///
/// **Not hashed with `RandomState`.** The key is `qy * width + qx`, computed
/// here from a pixel the rasterizer just drew, and it is bounded by the
/// texture's own quarter-resolution grid — nothing outside this process picks
/// it, and no feed can widen the key space past `width * height` however many
/// features it sends. What `RandomState` buys is per-process seeding against an
/// attacker who chooses keys; there is no such attacker at this key, and
/// `HitCells::record` runs once per drawn point. `FxHashMap` is a multiply and
/// a rotate instead of SipHash-1-3.
///
/// Iteration order changes with the hasher, and one place cares: the reply wire
/// in [`crate::render::jobs::encode_overlay_out`], which already sorts by cell
/// index for exactly this reason and is unaffected. Every other reader takes
/// `values()`, `len()` or `is_empty()`.
pub type HitCellMap = rustc_hash::FxHashMap<u32, Vec<u32>>;

/// The portable half of click detection: which quarter-resolution cells the
/// rasterizer drew which item **indices** into — positions in its input list,
/// so both halves must come from one order.
#[derive(Debug, Clone, PartialEq)]
pub struct HitCells {
    pub width: u32,
    pub height: u32,
    pub cells: HitCellMap,
}

impl HitCells {
    pub fn new(full_width: u32, full_height: u32) -> Self {
        Self {
            width: full_width.div_ceil(4),
            height: full_height.div_ceil(4),
            cells: HitCellMap::default(),
        }
    }

    pub fn record(&mut self, px: f32, py: f32, item_id: u32) {
        // **A float-to-int `as` cast saturates**: -12.0 and NaN both become 0,
        // which passes the bound below as cell column 0 rather than failing it.
        // A stamp whose disc lies entirely off the left or top edge therefore
        // recorded its hits against the edge cells, and a click on that edge
        // answered an item that is nowhere near the pointer. Reachable: GLM
        // culls a flash only at `px < -base_size` while its hit disc is
        // `0.72 * base_size` at the widest, so a flash just inside the cull
        // draws no ink at all and still recorded a column of hits.
        //
        // Only the near edges need the guard. A coordinate past `width` casts
        // to a number past `width`, and infinity saturates to `u32::MAX`, so
        // the `>=` tests below already reject those.
        if px.is_nan() || py.is_nan() || px < 0.0 || py < 0.0 {
            return;
        }
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
    /// The items a cell's recorded indices name, **positionally**.
    ///
    /// This was a `HashMap<u32, Arc<dyn OverlayItem>>` whose keys were exactly
    /// `0..items.len()` — a dense range hashed with SipHash to find a slot the
    /// index already named. A positional lookup answers the same question with
    /// a bounds check, so the whole build hashes nothing.
    ///
    /// A [`HitItems::Rows`] clone is that vector of pointers and one refcount
    /// bump each; a [`HitItems::Slab`] clone is one refcount bump, and its
    /// items are built by the handful a click actually names.
    items: HitItems,
}

impl HitMap {
    /// `items.get(i)` **must** answer the item whose row travelled at position
    /// `i` of the described input, because a cell records positions and
    /// nothing else.
    pub fn from_cells(cells: HitCells, items: &HitItems) -> Self {
        Self {
            cells,
            items: items.clone(),
        }
    }

    pub fn hit_test(&self, u: f32, v: f32) -> Vec<Arc<dyn OverlayItem>> {
        self.cells
            .ids_at(u, v)
            .iter()
            .filter_map(|id| self.items.get(*id as usize))
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

/// Whether any pixel of a **premultiplied** RGBA buffer would change the frame
/// it is drawn on.
///
/// **Exact, not a sample.** Premultiplication is what makes it exact: a pixel
/// that contributes nothing has zero in all four bytes, so "no non-zero byte"
/// and "paints nothing" are the same statement. A sampled or strided version
/// would miss a picture whose only ink is one polygon, which is the ordinary
/// shape of an alerts raster.
///
/// **Short-circuits.** A picture with ink in its first row costs a handful of
/// loads; only a picture with no ink at all pays the whole pass, and that is
/// the reading this exists to take.
///
/// **It lives here, below the wire, because the wire needs it.** The answer is
/// what decides whether a reply carries a picture-sized payload at all
/// ([`RasterizeOutput::settle_blank`]), and the encoder is in this crate;
/// `squallar_egui::overlay_cache::ledger` — which is where the reading was
/// first taken and where its prose still lives — re-exports this one function
/// rather than keeping a second spelling of it. Two predicates that can
/// disagree about "is this blank" would be a picture uploaded against a pane
/// told to clear.
pub fn has_ink(rgba: &[u8]) -> bool {
    rgba.iter().any(|&b| b != 0)
}

pub struct RasterizeOutput {
    /// The picture's premultiplied bytes — **empty when `blank` is `Some`**.
    pub rgba: Vec<u8>,
    pub hit_cells: Option<HitCells>,
    pub alpha: AlphaMode,
    /// `Some(len)` when this raster has been judged and had no ink in it:
    /// `rgba` is then empty and `len` is the byte length the picture *would*
    /// have had.
    ///
    /// **Written in exactly one place**, [`Self::settle_blank`], which the job
    /// funnel's output stage calls once per reply after the premultiply. Every
    /// other producer leaves it `None`, which says "the pixels are in `rgba`"
    /// and is what an unjudged raster means; nothing downstream re-decides.
    ///
    /// The length rather than a bare flag, because it is the term the arrival
    /// checks the answer's size by: a handler answering the wrong size is a
    /// failed render, and a blank has to be separable from one.
    pub blank: Option<u32>,
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
            .field("blank", &self.blank)
            .finish()
    }
}

impl RasterizeOutput {
    /// Give up a buffer that cannot change the frame it would be drawn on,
    /// keeping the length it would have had.
    ///
    /// **Its precondition is premultiplied bytes**, which is why the funnel
    /// calls it after the premultiply and not at the end of a rasterizer: a
    /// straight buffer may carry non-zero colour under a zero alpha, and
    /// [`has_ink`] would call that ink.
    ///
    /// Idempotent, and it never un-settles: a raster already judged blank is
    /// left alone, and one with ink keeps every byte.
    pub fn settle_blank(&mut self) {
        if self.blank.is_some() || has_ink(&self.rgba) {
            return;
        }
        if let Ok(len) = u32::try_from(self.rgba.len()) {
            self.blank = Some(len);
            self.rgba = Vec::new();
        }
    }
}

impl squallar_source::job::JobOut for RasterizeOutput {
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

    fn discard_blank_rasters(&mut self) {
        self.settle_blank();
    }
}

/// Web Mercator's own limit, from [`squallar_geo`].
const MAX_MERCATOR_LAT: f64 = squallar_geo::MERCATOR_LAT_LIMIT_DEG;

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

squallar_source::impl_job_input!(OutlooksInput);

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
            blank: None,
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
        blank: None,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiscussionPaint {
    pub md_type: crate::spc::discussion::MdType,
    pub polygon: squallar_geo::GeoPolygon,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiscussionsInput {
    pub discussions: Vec<DiscussionPaint>,
    pub device_scale: f32,
}

squallar_source::impl_job_input!(DiscussionsInput);

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
            blank: None,
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
        blank: None,
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

squallar_source::impl_job_input!(AlertsInput);

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
            blank: None,
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
        blank: None,
    }
}

/// One station, as the coverage wash needs it: a position and nothing else.
///
/// **No name and no role.** The wash is the network's, not any one station's,
/// so which radar the pane is on cannot change a texel of it — which is what
/// lets two panes at the same viewport share one raster, and what keeps the
/// input free of the pane read the old sites job needed.
#[derive(Debug, Clone, PartialEq)]
pub struct CoverageSite {
    pub lat: f64,
    pub lon: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CoverageInput {
    pub sites: Vec<CoverageSite>,
    pub device_scale: f32,
}

squallar_source::impl_job_input!(CoverageInput);

/// A WSR-88D's nominal coverage radius, 230 km, in degrees of latitude.
///
/// 230 km rather than the 460 km the long-range reflectivity sweep reaches:
/// 230 is the range every base product shares, so it is the distance at which
/// "is this storm inside a radar's coverage" has one answer instead of one per
/// product.
/// **Public because there is exactly one of it.** The selected station's ring
/// is painted per frame by `squallar_egui::site_marker`, in points off the live
/// projector, while the network-wide coverage wash below is ground in a raster.
/// Two painters, one radius: a second spelling in the frontend is what
/// `geodesy_one_definition` exists to refuse, and a 230/111.32 written from
/// memory is ~250 m wrong on this ring.
pub const COVERAGE_RADIUS_DEG_LAT: f64 = 230.0 / squallar_geo::KM_PER_DEGREE_LAT;

/// The coverage wash's outline width, in texels before density. A hairline: it
/// is the edge of the covered region, not a ring around a station.
const COVERAGE_EDGE_WIDTH: f32 = 1.0;

/// **Where the radar network can see, as ground.**
///
/// Every station's 230 km disc, filled **as one path under the non-zero winding
/// rule**, so the overlaps merge into a single region instead of stacking into
/// 160 outlines. That is the difference between this and what it replaced: the
/// old raster stroked each station's ring separately, and at continental zoom
/// the result was a mesh of intersecting circles with the map invisible under
/// it. One filled region has one edge — the boundary of national coverage —
/// which is the thing the 230 km figure was chosen to answer.
///
/// **Nothing about the pane reaches this.** No station is coloured for being
/// current or loading, because the wash is the network's; the markers say which
/// radar the pane is on, in screen space, and the selected station's own ring is
/// painted there too.
pub fn rasterize_radar_coverage(
    input: &CoverageInput,
    bounds: &GeoBounds,
    width: u32,
    height: u32,
) -> RasterizeOutput {
    let CoverageInput {
        sites,
        device_scale,
    } = input;
    let scale = sane_device_scale(*device_scale);
    let Some(mut pixmap) = Pixmap::new(width, height) else {
        log::error!(
            "Pixmap allocation failed in rasterize_radar_coverage ({}×{})",
            width,
            height
        );
        return RasterizeOutput {
            rgba: vec![0u8; (width * height * 4) as usize],
            hit_cells: None,
            alpha: AlphaMode::Premultiplied,
            blank: None,
        };
    };
    let mb = MercatorBounds::from_geo(bounds);
    let w = width as f32;
    let h = height as f32;

    let mut pb = PathBuilder::new();
    for site in sites {
        // Into the viewport's frame first: the catalogue folds longitude into
        // [-180, 180] while `bounds` is unfolded; 4 of 208 stations are east.
        let lon = mb.nearest_lon(site.lon);
        let (px, py) = mb.project(site.lat, lon, w, h);

        // The radius in texels, taken by projecting a point one coverage radius
        // due north of the station and measuring. Web Mercator is conformal, so
        // a circle this small comes back a circle rather than an ellipse, and
        // the north offset is a faithful radius in every direction. Latitude
        // scaling is therefore handled for free: the same 230 km is more texels
        // at Nome than at Key West, which is what the ground looks like on this
        // projection.
        let (_, py_north) = mb.project(site.lat + COVERAGE_RADIUS_DEG_LAT, lon, w, h);
        let radius = (py - py_north).abs();

        // Cull on the disc, not on the station: a radar whose antenna is off the
        // texture still covers ground that is on it.
        if px < -radius || px > w + radius || py < -radius || py > h + radius {
            continue;
        }
        // A sub-texel disc contributes nothing a reader can see and `push_circle`
        // is happy to build a degenerate one, so it is dropped here rather than
        // left for the rasterizer to round away.
        if !radius.is_finite() || radius < 1.0 {
            continue;
        }

        pb.push_circle(px, py, radius);
    }

    if let Some(path) = pb.finish() {
        let mut paint = Paint {
            anti_alias: true,
            ..Default::default()
        };

        // The wash. Faint on purpose: this draws over the whole eastern half of
        // the country at continental zoom, and anything a reader has to see
        // *through* has failed at being a basemap annotation.
        paint.set_color(Color::from_rgba8(100, 150, 255, 38));
        // **Winding and not even-odd**, and this is the whole construction: under
        // even-odd two overlapping discs cancel to a hole where coverage is
        // doubled, which says the opposite of the truth. Winding merges them.
        pixmap.fill_path(
            &path,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );

        paint.set_color(Color::from_rgba8(100, 150, 255, 160));
        let stroke = Stroke {
            width: COVERAGE_EDGE_WIDTH * scale,
            ..Stroke::default()
        };
        pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
    }

    RasterizeOutput {
        rgba: pixmap.take(),
        hit_cells: None,
        alpha: AlphaMode::Premultiplied,
        blank: None,
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
    /// The instant the report happened ([`StormReport::valid`]), for the
    /// as-of cull below; `None` draws at every instant — a report is never
    /// dropped for want of a readable time.
    ///
    /// [`StormReport::valid`]: crate::spc::reports::StormReport::valid
    pub valid: Option<chrono::NaiveDateTime>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReportsInput {
    /// Behind an `Arc` so the page can hold one built row set across the
    /// dispatches whose only moving terms are the scalars below; the wire
    /// carries the rows, never the sharing.
    pub reports: Arc<Vec<ReportPaint>>,
    pub zoom: f64,
    pub is_dark: bool,
    pub device_scale: f32,
    /// The instant the picture **depicts**, captured at dispatch
    /// (`RasterizeContext::as_of`) and carried on the wire — the worker culls
    /// against this and never against a clock of its own, the same rule as
    /// [`GlmStrikesInput::now`].
    pub as_of: chrono::NaiveDateTime,
}

squallar_source::impl_job_input!(ReportsInput);

#[derive(Debug, Clone, PartialEq)]
pub struct MetarInput {
    /// The observations, in the handler's own order — **row `i` is
    /// `hit_items()[i]`**, the same index contract every hit-mapped row keeps.
    /// Behind an `Arc` so the page can hold one built row set across the
    /// dispatches whose only moving terms are the scalars below; the wire
    /// carries the rows, never the sharing.
    pub obs: Arc<Vec<crate::metar::types::MetarOb>>,
    pub zoom: f64,
    pub is_dark: bool,
    pub device_scale: f32,
}

squallar_source::impl_job_input!(MetarInput);

/// The station models, drawn into a picture instead of onto the frame thread.
///
/// Everything except the text: see [`PixmapPointPainter`] for why the five
/// texts a station draws stay behind, and what the 41 shapes that move were
/// costing.
pub fn rasterize_metar_stations(
    input: &MetarInput,
    bounds: &GeoBounds,
    width: u32,
    height: u32,
) -> RasterizeOutput {
    let MetarInput {
        obs,
        zoom,
        is_dark,
        device_scale,
    } = input;
    let scale = sane_device_scale(*device_scale);
    let Some(mut pixmap) = Pixmap::new(width, height) else {
        log::error!(
            "Pixmap allocation failed in rasterize_metar_stations ({}x{})",
            width,
            height
        );
        return RasterizeOutput {
            rgba: vec![0u8; (width * height * 4) as usize],
            hit_cells: None,
            alpha: AlphaMode::Premultiplied,
            blank: None,
        };
    };
    let mb = MercatorBounds::from_geo(bounds);
    let (w, h) = (width as f32, height as f32);
    let ctx = crate::render::draw::DrawPointContext {
        zoom: *zoom as f32,
        is_dark: *is_dark,
    };
    let hit_radius = crate::render::station_model::hit_radius_for_zoom(*zoom as f32) * scale;
    let mut hit_cells = HitCells::new(width, height);

    for (idx, ob) in obs.iter().enumerate() {
        // Into the viewport's frame first, as every point row does.
        let lon = mb.nearest_lon(ob.lon);
        let (px, py) = mb.project(ob.lat, lon, w, h);
        // A station model reaches well past its centre — wind barbs, cloud
        // cover and the pressure group all sit off to one side — so the slack
        // is generous. A symbol clipped at the edge is a drawing artifact; a
        // symbol culled at the edge is a missing station.
        let slack = 60.0 * scale;
        if px < -slack || px > w + slack || py < -slack || py > h + slack {
            continue;
        }
        {
            let mut painter = PixmapPointPainter {
                pixmap: &mut pixmap,
                center: (px, py),
            };
            // Text is a no-op in this painter, but the draw still asks for
            // it; building it here is per station per PICTURE, in the worker.
            let text = crate::render::station_model::StationText::of(ob);
            crate::render::station_model::draw_metar_station(ob, &text, &mut painter, &ctx);
        }
        // The station's position in the input list **is** its id, the same
        // contract `hit_items` is index-aligned against. Stepped by 4 because
        // the cell grid is quarter resolution.
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
        blank: None,
    }
}

/// A [`PointPainter`](crate::render::draw::PointPainter) that draws into a
/// `tiny_skia` pixmap, so a station model can be rasterized OFF the frame
/// thread.
///
/// **Why this exists.** The METAR station model emits 46 shapes per station —
/// 28 lines, 7 stroked circles, 6 filled circles, 4 polygons, 5 texts — and a
/// scene D leg carries 799 observations. Drawn through `egui` that is ~36,750
/// shapes per frame of which ~28,000 are STROKED, and a stroked path is the
/// tessellator's expensive case: it feathers geometry along both edges. The
/// measured cost was 98,815 vertices and 401,072 indices staged EVERY FRAME,
/// with `epaint::tessellator::stroke_and_fill_path` the single largest symbol
/// in the app at 11.75 %, for observations that change every twenty minutes.
///
/// **`text` is deliberately a no-op here.** `tiny_skia` has no font support and
/// nothing in the worker can lay out a galley, so the five texts a station
/// draws stay on the frame thread where the font atlas is. That is the split:
/// the 41 geometric shapes — every stroked one — become a picture, and only
/// the text remains. Moving the text as well needs a font rasterizer in the
/// worker and is a separate question with its own dependency decision.
struct PixmapPointPainter<'a> {
    pixmap: &'a mut Pixmap,
    /// The station's own position in pixels; every offset is relative to it.
    center: (f32, f32),
}

impl PixmapPointPainter<'_> {
    fn at(&self, offset: [f32; 2]) -> (f32, f32) {
        (self.center.0 + offset[0], self.center.1 + offset[1])
    }

    /// `PointPainter` speaks straight (unmultiplied) RGBA, which is what
    /// `from_rgba8` takes.
    fn paint_of(color: [u8; 4]) -> Paint<'static> {
        Paint {
            shader: tiny_skia::Shader::SolidColor(Color::from_rgba8(
                color[0], color[1], color[2], color[3],
            )),
            anti_alias: true,
            ..Default::default()
        }
    }
}

impl crate::render::draw::PointPainter for PixmapPointPainter<'_> {
    fn circle_filled(&mut self, offset: [f32; 2], radius: f32, color: [u8; 4]) {
        let (x, y) = self.at(offset);
        let mut pb = PathBuilder::new();
        pb.push_circle(x, y, radius);
        if let Some(path) = pb.finish() {
            self.pixmap.fill_path(
                &path,
                &Self::paint_of(color),
                FillRule::Winding,
                Transform::identity(),
                None,
            );
        }
    }

    fn circle_stroke(&mut self, offset: [f32; 2], radius: f32, color: [u8; 4], width: f32) {
        let (x, y) = self.at(offset);
        let mut pb = PathBuilder::new();
        pb.push_circle(x, y, radius);
        if let Some(path) = pb.finish() {
            self.pixmap.stroke_path(
                &path,
                &Self::paint_of(color),
                &Stroke {
                    width,
                    line_cap: LineCap::Round,
                    ..Default::default()
                },
                Transform::identity(),
                None,
            );
        }
    }

    /// No-op: see the type's own note. The frame thread still draws the text.
    fn text(
        &mut self,
        _offset: [f32; 2],
        _text: &str,
        _color: [u8; 4],
        _size: f32,
        _anchor: crate::render::draw::TextAnchor,
    ) {
    }

    fn line(&mut self, from: [f32; 2], to: [f32; 2], color: [u8; 4], width: f32) {
        let (x0, y0) = self.at(from);
        let (x1, y1) = self.at(to);
        let mut pb = PathBuilder::new();
        pb.move_to(x0, y0);
        pb.line_to(x1, y1);
        if let Some(path) = pb.finish() {
            self.pixmap.stroke_path(
                &path,
                &Self::paint_of(color),
                &Stroke {
                    width,
                    line_cap: LineCap::Round,
                    ..Default::default()
                },
                Transform::identity(),
                None,
            );
        }
    }

    fn filled_polygon(&mut self, points: &[[f32; 2]], color: [u8; 4]) {
        let Some((first, rest)) = points.split_first() else {
            return;
        };
        let (x0, y0) = self.at(*first);
        let mut pb = PathBuilder::new();
        pb.move_to(x0, y0);
        for p in rest {
            let (x, y) = self.at(*p);
            pb.line_to(x, y);
        }
        pb.close();
        if let Some(path) = pb.finish() {
            self.pixmap.fill_path(
                &path,
                &Self::paint_of(color),
                FillRule::Winding,
                Transform::identity(),
                None,
            );
        }
    }
}

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
        as_of,
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
            blank: None,
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
        // **A report later than the depicted instant has not happened yet**
        // (`TimeAxis::EventLifetime`): the picture at `as_of` is which of
        // today's reports have already happened. The cull lives HERE and not
        // in `paint_input`, because a row's position is its hit-map id
        // (`HitMap::from_cells`): dropping rows at the handler would renumber
        // every row after the gap and hand hovers to the wrong reports. A
        // culled row travels, draws nothing and records no cells — absent
        // from the picture, aligned in the map. `None` passes: a report is
        // never dropped for want of a readable time.
        if report.valid.is_some_and(|valid| valid > *as_of) {
            continue;
        }
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
        blank: None,
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

squallar_source::impl_job_input!(GlmStrikesInput);

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
            blank: None,
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

        // **A flash later than the depicted instant has not happened yet.**
        // A cull and not a clamp. The clamp this replaces - `.max(0)` on the
        // age - read a future flash as age *zero*, which is the peak of
        // `time_decay_color`'s ramp, so a scrubbed pane drew tomorrow's
        // strikes at full brightness beside today's. Nothing survives the
        // clamp to guard: this subtraction is exact integer arithmetic on two
        // `NaiveDateTime`s, so past the cull it cannot be negative.
        if flash.time > *now {
            continue;
        }
        let age_secs = (*now - flash.time).num_milliseconds() as f64 / 1000.0;
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
        // **Clamped into the texture, as the station and report loops are.**
        // Unclamped, `(px - r) as i32` truncates *toward zero*, so a bolt
        // within `r` of the west or north edge sampled at negative texel
        // coordinates — which `HitCells::record` then saturated back into
        // column or row 0. The answer came out nearly right by that accident
        // and not by the arithmetic: over a 5400-position sweep of the four
        // edges, 52 positions recorded a cell that the clamped sampling
        // reaches directly and the unclamped one only reached through the
        // cast. Sampling inside the texture is what makes the cast's
        // behaviour stop mattering.
        let min_x = (px - r).max(0.0) as i32;
        let max_x = ((px + r) as i32).min(width as i32 - 1);
        let min_y = (py - r).max(0.0) as i32;
        let max_y = ((py + r) as i32).min(height as i32 - 1);
        let mut sy = min_y;
        let sy_end = max_y;
        while sy <= sy_end {
            let mut sx = min_x;
            let sx_end = max_x;
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
        blank: None,
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
use crate::render::gridded::ValuesRef;

fn merc_y_to_lat(merc_y: f64) -> f64 {
    squallar_geo::mercator_y_to_lat_rad(merc_y).to_degrees()
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
    // reach stops describing it. On a Lambert grid that costs the `j` axis too:
    // a row is not a parallel, both indices are axes of the projection plane,
    // and `detect_longitude_wrap` reports a break found stepping along *either*
    // of them, so neither axis has a window left. Measured, not argued: with
    // only `i` widened and `j` narrowed, both wrapping-Lambert suites below
    // still fail.
    //
    // Where the rows *are* parallels the wrap reaches one axis only, and
    // `index_bounds` answers each on its own terms: the latitude bracket
    // always, and the column pair when the box does not walk over the axis's
    // own end. GMGSI is 15,000,000 points and this is the whole of its window.
    if coords.wraps_longitude() && !coords.rows_are_parallels() {
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
    // A cell `cell_deg` **degrees** tall is `cell_deg / cos(lat)` of Mercator
    // **y**, and the two agree only at the equator. Spending the pad as though
    // they were the same reaches `cos(lat)` of the way it means to: 0.71 at
    // 45 N, 0.31 at 72 N — GMGSI's own top row — and `low`/`high` below only
    // hand back one index of slack, so past about 65 N the shortfall is more
    // than a whole cell and a row that paints is left unprojected.
    let merc_scale = edge_lat
        .min(MAX_MERCATOR_LAT)
        .to_radians()
        .cos()
        .max(f64::MIN_POSITIVE);
    let merc_pad = CELL_REACH * cell_deg.to_radians() / merc_scale
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
// `Window` is ~240 B against `Whole`'s 8 (it carries `GridCoords` and the
// tagged `GridValues`, not a `Vec<f32>`). It is built once per job and handed
// to `DescribedJob::new`, which boxes it; a `Box` here would be a second
// pointer to chase on every raster read for nothing saved.
#[allow(clippy::large_enum_variant)]
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

squallar_source::impl_job_input!(GriddedInput);

/// The wire form of a gridded raster: the field's identity, the grid's shape
/// and coordinates, the [`IndexWindow`] its values were cut to, and those
/// values alone.
#[derive(Debug, Clone, PartialEq)]
pub struct GridWindow {
    /// **A field identity, not a source's own enum.** The raster resolves it
    /// through [`crate::render::gridded::field_paint`] and refuses what that
    /// does not answer, so a second gridded source needs no arm here.
    pub field: squallar_source::product::FieldId,
    /// The **full grid's** shape, which `win` and `coords` index against.
    pub ni: usize,
    pub nj: usize,
    pub coords: crate::hrrr::GridCoords,
    /// The window `values` covers, computed at the dispatch and carried.
    pub win: IndexWindow,
    /// Row-major within `win`: point `(i, j)` of the grid is
    /// `values[(j - win.j0) * (win.i1 - win.i0) + (i - win.i0)]`.
    ///
    /// **In the sending grid's own storage width**, which is what keeps the
    /// worker's copy the same size as the page's: a mosaic that is 16-bit
    /// codes resident travels as codes and is held as codes here, and the
    /// widening happens one value at a time at [`GriddedInput::value_at`].
    pub values: crate::render::gridded::GridValues,
}

impl GriddedInput {
    /// The field being drawn. The whole-grid arm reads it off the model's own
    /// registration, which is where the persisted spelling already lives.
    pub fn field(&self) -> &squallar_source::product::FieldId {
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
    /// below take this rather than repeating an arm each. A
    /// [`ValuesRef`] rather than a `&[f32]`: the storage width is the grid's
    /// business and none of the raster's.
    fn whole_values(&self) -> Option<(ValuesRef<'_>, usize)> {
        match self {
            Self::Whole(grid) => Some((ValuesRef::F32(&grid.values), grid.ni)),
            Self::Resident(grid) => Some((grid.values.view(), grid.ni)),
            Self::Window(_) => None,
        }
    }

    /// How this input's values are stored, whichever arm holds them.
    ///
    /// What the wire tags itself with, and what the encoders dispatch on.
    pub fn values_ref(&self) -> ValuesRef<'_> {
        match self {
            Self::Whole(grid) => ValuesRef::F32(&grid.values),
            Self::Resident(grid) => grid.values.view(),
            Self::Window(window) => window.values.view(),
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
    ///
    /// **The one place a stored value becomes an `f32`.** The whole rest of
    /// `rasterize_gridded` reads coordinates, so a narrower store costs exactly
    /// this call: a bounds-checked index plus, on the scaled arm, two flops and
    /// a walk of at most `MAX_NAN_CODES` reserved codes — strictly less than
    /// the `partition_point` the `color_for_value` on the next line already
    /// pays. Nothing here allocates.
    fn value_at(&self, i: usize, j: usize) -> Option<f32> {
        if let Some((values, stride)) = self.whole_values() {
            return values.get(j * stride + i);
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
    }

    /// One row of `win`'s values as `f32`; the whole arm pads with NaN where
    /// its vector runs short, which paints nothing. A callback per row so the
    /// encoder writes straight from the grid's storage.
    ///
    /// **The `f32`-store reader.** A grid whose values are stored narrower has
    /// no `&[f32]` to lend and is written by
    /// [`Self::for_each_window_row_raw`] instead; this yields nothing for one,
    /// and the only production caller — `GriddedJob::encode` — dispatches on
    /// [`Self::values_ref`] rather than calling this blind. Expanding here
    /// instead would put a mosaic-row buffer on the **frame thread**, which is
    /// where `JobRequest::to_bytes` runs.
    pub fn for_each_window_row(&self, win: &IndexWindow, mut f: impl FnMut(&[f32])) {
        if win.is_empty() {
            return;
        }
        if let Some((values, stride)) = self.whole_values() {
            let ValuesRef::F32(values) = values else {
                return;
            };
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
        let ValuesRef::F32(values) = window.values.view() else {
            return;
        };
        let carried = &window.win;
        let row_w = carried.i1 - carried.i0;
        for j in win.j0..win.j1 {
            let row = (j - carried.j0) * row_w;
            f(&values[row + (win.i0 - carried.i0)..row + (win.i1 - carried.i0)]);
        }
    }

    /// **The points inside `win` that carry no reading**, as indices into the
    /// window the encoder is about to write — `win.area()`-space, row-major,
    /// the same space the payload's values are in.
    ///
    /// Empty for every store but the byte one, which is the only one whose
    /// missing points are a property of *where* a value sits rather than of
    /// the code it carries: `ScaledU16` reserves codes, and an `f32` says NaN
    /// in the value itself. See `gridded::ByteCodes`.
    ///
    /// **Bounded work on the frame thread.** `JobRequest::to_bytes` runs at the
    /// dispatch site, so this is at most `MAX_ABSENT_POINTS` divisions and
    /// comparisons — no walk over the window, and nothing proportional to the
    /// grid. A per-point representation of the same fact could not be cut to a
    /// strided window without one.
    ///
    /// The order survives the cut: the indices arrive ascending, and both
    /// spaces are row-major over the same rows, so the mapping is monotone —
    /// which is what `ByteCodes::new` demands at the far end.
    pub fn absent_in_window(&self, win: &IndexWindow) -> Vec<u32> {
        // Where this store's index space starts, and how wide its rows are.
        // A resident grid is indexed from the grid's own origin; a window that
        // has already been cut is indexed from its own.
        let (absent, i0, j0, stride) = match self {
            Self::Resident(grid) => match grid.values.view() {
                ValuesRef::Bytes(b) => (b.absent(), 0, 0, grid.ni),
                ValuesRef::F32(_) | ValuesRef::Scaled(_) => return Vec::new(),
            },
            Self::Window(window) => match window.values.view() {
                ValuesRef::Bytes(b) => (
                    b.absent(),
                    window.win.i0,
                    window.win.j0,
                    window.win.i1.saturating_sub(window.win.i0),
                ),
                ValuesRef::F32(_) | ValuesRef::Scaled(_) => return Vec::new(),
            },
            Self::Whole(_) => return Vec::new(),
        };
        if absent.is_empty() || win.is_empty() || stride == 0 {
            return Vec::new();
        }
        let row_w = win.i1 - win.i0;
        absent
            .iter()
            .filter_map(|&k| {
                let k = k as usize;
                let j = j0 + k / stride;
                let i = i0 + k % stride;
                (i >= win.i0 && i < win.i1 && j >= win.j0 && j < win.j1)
                    .then(|| ((j - win.j0) * row_w + (i - win.i0)) as u32)
            })
            .collect()
    }

    /// One row of `win`'s values as **stored bytes**, in the storage's own
    /// width — the shape the wire carries and the transport lends.
    ///
    /// **No expansion, no scratch, no allocation.** That is the point rather
    /// than a detail: this runs inside `JobRequest::to_bytes` on the frame
    /// thread, so a per-row widening buffer here would trade the footprint win
    /// for frame time. Each row is handed out as a borrow of the grid's own
    /// allocation.
    ///
    /// A row that runs past the end yields **nothing at all** rather than a
    /// short run or a fabricated pad. The far end checks the payload length
    /// against the window the head names, so a short write is refused as a
    /// length mismatch — which is the honest answer for a values vector that
    /// does not match the shape beside it, and is unreachable for a decoded
    /// grid anyway (`parse_grib2_raw_in` refuses any other count).
    pub fn for_each_window_row_raw(&self, win: &IndexWindow, mut f: impl FnMut(&[u8])) {
        if win.is_empty() {
            return;
        }
        if let Some((values, stride)) = self.whole_values() {
            for j in win.j0..win.j1 {
                let Some(bytes) = values.sample_bytes(j * stride + win.i0..j * stride + win.i1)
                else {
                    return;
                };
                f(bytes);
            }
            return;
        }
        let Self::Window(window) = self else {
            return;
        };
        let carried = &window.win;
        let row_w = carried.i1 - carried.i0;
        let values = window.values.view();
        for j in win.j0..win.j1 {
            let row = (j - carried.j0) * row_w;
            let Some(bytes) =
                values.sample_bytes(row + (win.i0 - carried.i0)..row + (win.i1 - carried.i0))
            else {
                return;
            };
            f(bytes);
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
            blank: None,
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
            blank: None,
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
            blank: None,
        };
    }
    let win_w = win.i1 - win.i0;

    // One row of the window, projected. The cell loop reads each neighbour
    // several times, so the row it is on and the two beside it are pre-projected
    // rather than recomputed per read.
    let project_row = |j: usize, row: &mut Vec<(f32, f32)>| {
        row.clear();
        row.reserve(win_w);
        for i in win.i0..win.i1 {
            match coords.at(j * ni + i) {
                Some((lat, lon)) => row.push(mb.project(lat, lon, w, h)),
                None => row.push((f32::NAN, f32::NAN)),
            }
        }
    };

    // **Three rows, never the window.** Sizing a cell reads `(i±1, j)` and
    // `(i, j±1)` and nothing else, so row `j` needs `j-1` and `j+1` beside it
    // and no more. Holding the whole window instead cost a `(f32, f32)` per
    // grid point, and the window is what *zoom* moves: a tight view over one
    // state projects a few thousand points, a view zoomed out until a CONUS
    // mosaic fits projects all 24 500 000 of them — 196 MB in one infallible
    // `Vec::with_capacity`, which on wasm32 is `handle_alloc_error` against the
    // 1 GiB module ceiling and a trap that nothing unwinds. The band is 168 KB
    // at that same width. Gated by `tests/gridded_projection_band.rs`.
    //
    // `band[j % 3]` holds grid row `j`: the loop advances one row at a time and
    // the three live rows are consecutive, so their residues never collide.
    let mut band: [Vec<(f32, f32)>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    let mut projected_to: Option<usize> = None;

    let draw = win.interior(ni, nj);
    for j in draw.j0..draw.j1 {
        // Every read below is inside `win`; `interior` is what guarantees it,
        // and it is what bounds this range to rows the window really carries.
        let lo = j.saturating_sub(1).max(win.j0);
        let hi = (j + 1).min(win.j1 - 1);
        let first = match projected_to {
            Some(done) if done + 1 > lo => done + 1,
            _ => lo,
        };
        for row in first..=hi {
            project_row(row, &mut band[row % 3]);
        }
        projected_to = Some(hi);

        let here = &band[j % 3];
        // `j - 1` and `j + 1` modulo three, read behind the same `j > 0` and
        // `j + 1 < nj` guards the whole-window buffer was read behind.
        let above = &band[(j + 2) % 3];
        let below = &band[(j + 1) % 3];
        let at = |i: usize| here[i - win.i0];

        for i in draw.i0..draw.i1 {
            let Some(value) = input.value_at(i, j) else {
                continue;
            };
            let color = paint.color_for_value(value);
            if color[3] == 0 {
                continue;
            }

            let (cx, cy) = at(i);
            if cx.is_nan() || cy.is_nan() {
                continue;
            }

            // Half-extents from neighbour spacing. 0.55, not 0.50: a slight
            // overlap hides seams between adjacent cells.
            let dx_left = if i > 0 {
                let (nx, _) = at(i - 1);
                ((cx - nx).abs() * 0.55).max(0.5)
            } else if i + 1 < ni {
                let (nx, _) = at(i + 1);
                ((nx - cx).abs() * 0.55).max(0.5)
            } else {
                1.0
            };
            let dx_right = if i + 1 < ni {
                let (nx, _) = at(i + 1);
                ((nx - cx).abs() * 0.55).max(0.5)
            } else {
                dx_left
            };
            let dy_up = if j > 0 {
                let (_, ny) = above[i - win.i0];
                ((cy - ny).abs() * 0.55).max(0.5)
            } else if j + 1 < nj {
                let (_, ny) = below[i - win.i0];
                ((ny - cy).abs() * 0.55).max(0.5)
            } else {
                1.0
            };
            let dy_down = if j + 1 < nj {
                let (_, ny) = below[i - win.i0];
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
        blank: None,
    }
}

#[cfg(test)]
mod alpha_tests;

#[cfg(test)]
mod dateline_tests;

#[cfg(test)]
mod glm_energy_tests;

#[cfg(test)]
mod glm_time_tests;

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
mod wrapping_window_tests;

#[cfg(test)]
mod device_scale_tests;

#[cfg(test)]
mod sites_marker_tests;

#[cfg(test)]
mod hit_cells_tests;
