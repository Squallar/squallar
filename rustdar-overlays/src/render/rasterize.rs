//! Rasterize overlay polygons to RGBA textures using tiny-skia.
//!
//! Runs on a background thread; the texture is then geo-positioned on the map
//! the same way a radar image is.

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

// ── Hit buffer types ─────────────────────────────────────────────────────

/// The portable half of click detection: which quarter-resolution cells the
/// rasterizer drew which item **indices** into. Plain data — this is what a
/// described hit-map render answers over a message port, where the trait
/// objects a click resolves to cannot follow.
///
/// An id here is the item's position in the rasterizer's own input list
/// (`ReportsInput::reports`, `GlmStrikesInput::flashes`). That makes the pair
/// `(HitCells, id_map)` meaningful only when the id_map was captured **from
/// the same list in the same order** — see [`HitMap::from_cells`], where the
/// zip happens and where that invariant is stated as the contract it is.
///
/// Stored at 1/4 resolution per axis, and sparsely, to keep memory and wire
/// size down — the same layout the pre-split `HitMap` used.
#[derive(Debug, Clone, PartialEq)]
pub struct HitCells {
    /// Quarter-resolution, not texture width.
    pub width: u32,
    pub height: u32,
    /// `qy * width + qx` → covering item indices. Occupied cells only.
    ///
    /// Public because this is a wire form: the codec in
    /// `rustdar_frontend::offload` reads and rebuilds it, and *it* polices the
    /// in-range invariant on untrusted bytes. Everything built through
    /// [`record`](Self::record) is in range by construction.
    pub cells: HashMap<u32, Vec<u32>>,
}

impl HitCells {
    /// Takes *full*-resolution dimensions and quarters them.
    pub fn new(full_width: u32, full_height: u32) -> Self {
        Self {
            width: full_width.div_ceil(4),
            height: full_height.div_ceil(4),
            cells: HashMap::new(),
        }
    }

    /// `(px, py)` is in full-resolution pixels.
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

    /// The item indices covering `(u, v)` — texture UVs in `[0, 1]`.
    pub fn ids_at(&self, u: f32, v: f32) -> &[u32] {
        if !(0.0..=1.0).contains(&u) || !(0.0..=1.0).contains(&v) {
            return &[];
        }
        let qx = ((u * self.width as f32) as u32).min(self.width.saturating_sub(1));
        let qy = ((v * self.height as f32) as u32).min(self.height.saturating_sub(1));
        let idx = qy * self.width + qx;
        self.cells.get(&idx).map_or(&[], Vec::as_slice)
    }

    /// The largest item index any cell records, or `None` for an empty map.
    /// What the page-side zip checks its id_map against — see
    /// [`HitMap::from_cells`].
    pub fn max_id(&self) -> Option<u32> {
        self.cells.values().flatten().copied().max()
    }
}

/// Click detection against the pixels the rasterizer actually drew: the
/// portable [`HitCells`] zipped with the page-side `id_map` of the trait
/// objects a hit resolves to.
///
/// The two halves used to be one struct filled in one function. They are
/// split because only one of them can cross a message port: the cells travel
/// back from the worker, the `Arc<dyn OverlayItem>`s never leave the page.
#[derive(Clone)]
pub struct HitMap {
    cells: HitCells,
    id_map: HashMap<u32, Arc<dyn OverlayItem>>,
}

impl HitMap {
    /// Zip the worker's cells with the items captured at dispatch.
    ///
    /// # The order-stability invariant
    ///
    /// `items[i]` **must** be the item whose row travelled at position `i` of
    /// the described input, because a cell records positions and nothing else.
    /// Both halves are built from one iteration of the handler's data — the
    /// input rows by the handler's `paint_input` helper, the items by its
    /// `hit_items` — and the wire codec appends and reads lists in order, so
    /// the invariant holds by construction. It is pinned rather than trusted:
    /// `rustdar_frontend::offload::tests` zips a deliberately shuffled id_map
    /// and asserts the hit comes back **wrong**, because a silent order
    /// mismatch here is a hover that names the wrong storm report — worse than
    /// no hit map at all.
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

    /// `(u, v)` are texture UVs in `[0, 1]`.
    pub fn hit_test(&self, u: f32, v: f32) -> Vec<Arc<dyn OverlayItem>> {
        self.cells
            .ids_at(u, v)
            .iter()
            .filter_map(|id| self.id_map.get(id).cloned())
            .collect()
    }
}

/// Which of the two RGBA conventions a rasterizer's bytes are written in.
///
/// egui has a constructor for each — `ColorImage::from_rgba_premultiplied` is a
/// copy, `from_rgba_unmultiplied` is a copy through a 64 KiB lookup table — and
/// picking the wrong one does not fail, it shifts every translucent colour. So
/// the convention is carried on [`RasterizeOutput`] rather than known by the
/// consumer: `rustdar_frontend::offload::execute`'s output stage reads it off
/// the value it was handed, converts the straight case inside the job, and
/// answers premultiplied-always — so the one page-side read is a
/// compute-nothing `from_rgba_premultiplied` that cannot be written to assume
/// the wrong thing.
///
/// It has to be per-rasterizer, not per-crate, because this module genuinely
/// produces both. Everything drawn through tiny-skia is [`Self::Premultiplied`]
/// — that is what a `Pixmap` holds, by definition. [`rasterize_model_data`]
/// bypasses tiny-skia and writes palette bytes into a buffer itself, so its
/// output is [`Self::Straight`]. A single global choice corrupts whichever half
/// it is wrong about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlphaMode {
    /// RGB already scaled by alpha — a `tiny_skia::Pixmap`'s own layout, and
    /// egui's `Color32` layout, which is why the pair needs no conversion.
    Premultiplied,
    /// RGB independent of alpha, as a colour table or a picker states it.
    Straight,
}

pub struct RasterizeOutput {
    /// `width × height × 4` bytes, in the convention [`Self::alpha`] names.
    pub rgba: Vec<u8>,
    /// The portable half of the hit map — item **indices**, never items. The
    /// consumer zips it with the id_map it captured at dispatch
    /// ([`HitMap::from_cells`]); a rasterizer that answers `Some` here is one
    /// whose kind resolves clicks by pixel rather than by polygon containment.
    pub hit_cells: Option<HitCells>,
    /// How to read [`Self::rgba`]. See [`AlphaMode`].
    pub alpha: AlphaMode,
}

/// Summarized by hand rather than derived: the job boundary's erasure seam
/// ([`rustdar_source::job::JobOut`]) requires `Debug`, and the derived form
/// would print every byte of a raster — 206.75 MiB at the desktop ceiling —
/// into any panic or test-failure message that formats one.
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

/// The reply half of the job boundary's erasure seam: a described overlay
/// render answers this type through the codec rows in
/// [`jobs`](crate::render::jobs).
impl rustdar_source::job::JobOut for RasterizeOutput {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn into_any(self: Box<Self>) -> Box<dyn std::any::Any> {
        self
    }

    /// The declared [`AlphaMode`] is the whole answer: a straight raster is
    /// handed over for conversion **and the declaration flips with it**, so
    /// the statement and the buffer cannot come to disagree — after the run
    /// funnel premultiplies what it is handed here, this output honestly
    /// declares what its bytes are, which is also the only convention the
    /// reply wire ever carries.
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

// ── Mercator projection helpers ──────────────────────────────────────────

/// Web Mercator's own limit, from [`rustdar_geo`] rather than spelled
/// again here.
///
/// It read `85.05`, under a comment claiming that was where the projection
/// diverges. Both halves were wrong. The limit is 85.051128779806°, so the
/// literal was 0.0011287798° — **125.51 m** of meridian — short of it; and the
/// clamp below is not a divergence guard, because
/// `rustdar_egui::overlay_cache::OverlayTexturePlan::coverage` has already
/// clamped these same `GeoBounds` to the true limit before the rasterizer ever
/// sees them, and stores that clamped rectangle as the texture's placement
/// bounds. So the only thing `85.05` could ever do was hold the rasterizer's
/// Y-range 125 m short of the Y-range the texture is *placed* between — the
/// picture drawn for one rectangle and pinned to another. Sub-pixel at CONUS
/// latitudes in a whole-world texture, and zero once the two agree, which is
/// the point of their being one constant.
const MAX_MERCATOR_LAT: f64 = rustdar_geo::MERCATOR_LAT_LIMIT_DEG;

/// Mercator Y for both edges is precomputed once per texture.
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

    /// Bring a longitude into this box's own 360° frame.
    ///
    /// The two sides of this comparison are written in different conventions
    /// and always have been. Point data is folded into [-180, 180] on the way
    /// in — GLM does it in `normalize_longitude`, because GOES-West's own
    /// coordinate offsets run past the antimeridian. The viewport is
    /// deliberately *not* folded: `OverlayCache::coverage` says so in as many
    /// words, "longitude is not [clamped], because the map wraps and a texture
    /// may legitimately straddle the antimeridian", so a view over the
    /// dateline arrives as e.g. `-195..-165`.
    ///
    /// Compared raw, a flash the fold placed at +172 fails `lon > max_lon`
    /// against a max of -165 and is dropped, and if it survived it would
    /// project 12 texture-widths off the side. Measured on real granules: of
    /// the 86 development-set flashes lying inside a 30° dateline viewport,
    /// 76 (88.4%) were dropped; 1 of 3 on the holdout. Only GOES-West can see
    /// this, so the GOES-East cases score a structural zero and say nothing.
    ///
    /// Returns a longitude in `[min_lon, min_lon + 360)`, so a caller that has
    /// shifted no longer needs the `< min_lon` half of a range test.
    #[inline]
    pub(crate) fn wrap_lon(&self, lon: f64) -> f64 {
        self.min_lon + (lon - self.min_lon).rem_euclid(360.0)
    }

    /// The whole multiple of 360° that carries a datum spanning
    /// `[min_lon, max_lon]` to its representation *nearest* this box.
    ///
    /// [`Self::wrap_lon`] answers a different question, and only the GLM
    /// strike path wants the one it answers. `wrap_lon` lands in
    /// `[min_lon, min_lon + 360)`, so a datum a degree *west* of `min_lon`
    /// comes back 359° east — off the far side rather than just off the near
    /// one. For a flash that is invisible: either way it fails the pixel
    /// window. For anything drawn with slack around the texture edge — a
    /// radar site keeps 50 points of it so a site just off-texture still
    /// contributes its label — it silently deletes the western margin. This
    /// picks the nearest representation instead, which is identity whenever
    /// the datum is already in frame and never moves anything across the
    /// texture to reach it.
    ///
    /// A shift is a *rigid translation*: applied to every vertex of a ring it
    /// moves the shape without redrawing it, which is the property a polygon
    /// needs and a per-vertex `wrap_lon` does not have. It is only rigid for a
    /// datum that fits in a half-turn, so a wider one gets no shift at all —
    /// see the guard. Measured against the live NWS zone corpus, nothing is
    /// excluded by that guard: the source is already cut at the antimeridian,
    /// e.g. marine zone `PKZ784` arrives as a MultiPolygon whose parts span
    /// `177.62..180.0` and `-180.0..-177.05` separately.
    #[inline]
    pub(crate) fn lon_shift(&self, min_lon: f64, max_lon: f64) -> f64 {
        crate::render::geo::lon_shift(min_lon, max_lon, self.min_lon, self.max_lon)
    }

    /// A single point's representation nearest this box. See [`Self::lon_shift`].
    #[inline]
    pub(crate) fn nearest_lon(&self, lon: f64) -> f64 {
        lon + self.lon_shift(lon, lon)
    }

    /// To texture pixel coordinates.
    ///
    /// Longitude is mapped linearly, so a `lon` outside this box's own 360°
    /// frame lands proportionally outside the texture and is culled by the
    /// caller's pixel-window test. Every caller whose data is folded into
    /// [-180, 180] must therefore state its frame first, and they do not all
    /// state it the same way:
    ///
    ///  * the GLM strike path uses [`Self::wrap_lon`], whose
    ///    `[min_lon, min_lon + 360)` guarantee its own `> max_lon` range test
    ///    is written against;
    ///  * the point overlays ([`rasterize_radar_sites`],
    ///    [`rasterize_storm_reports`]) use [`Self::nearest_lon`], which keeps
    ///    the western edge slack `wrap_lon` would delete;
    ///  * [`project_polygon`] shifts each polygon rigidly by
    ///    [`Self::lon_shift`], because a *per-vertex* wrap would move one
    ///    vertex across the frame and redraw the shape rather than move it.
    ///
    /// No shift is applied here, so that each of those stays the caller's own
    /// stated decision rather than something this function guesses.
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

// ── Public API ───────────────────────────────────────────────────────────
//
// Every entry point below draws through tiny-skia, so every `Vec<u8>` they
// return is a `Pixmap`'s own buffer: **premultiplied** alpha. They used to
// un-premultiply it on the way out, because egui's overlay upload called
// `ColorImage::from_rgba_unmultiplied`, which immediately multiplied it back.
// The pair cancelled — at 15 ms of division plus 7 ms of extra table lookup per
// 18.7 Mpx texture, and one lossy `u8` round trip, for a picture that was
// already in the layout `Color32` wants. The buffer is now handed over as it
// was drawn, and [`AlphaMode`] on [`RasterizeOutput`] is what tells the
// uploader so.
//
// The exception is [`rasterize_model_data`], which never went through
// tiny-skia and never called the conversion: it writes palette bytes with
// straight alpha, and says so.

/// Everything [`rasterize_spc_outlooks`] reads besides the raster's own
/// geometry, as one struct — the **wire form** of the outlook render, shaped
/// the way [`SitesInput`] is and for its reason: the described job decodes
/// back into the struct the direct call takes, so byte-identity between "over
/// a port" and "on this thread" is a property of the type.
///
/// `hatch_color` is resolved from the theme at *prepare* time and travels as
/// the resolved colour: the worker has no theme to consult, and re-deriving it
/// there would be a second statement of a page-side fact.
#[derive(Debug, Clone, PartialEq)]
pub struct OutlooksInput {
    /// In paint order, hatched features included — see
    /// `SpcOutlookHandler::features_in_paint_order`. The hatch pass reads each
    /// feature's own [`HatchPattern`](crate::types::HatchPattern) off this
    /// list, so nothing beyond the list and `hatch_color` travels for it.
    pub features: Vec<OverlayFeature>,
    /// Theme-dependent, so it cannot live in the feature.
    pub hatch_color: [u8; 4],
    /// See the `device_scale` note on
    /// [`RasterizeContext`](crate::render::overlay_state::RasterizeContext).
    pub device_scale: f32,
}

// The job boundary's erasure seam over this wire form: `as_any` answers
// `self`, `eq_dyn` is a downcast followed by the derived `==` above — value
// equality through the erased type, so the retry/discard machinery compares
// a described job the way the typed one would. One invocation beside each
// wire-form input; the codec rows live in `render::jobs`.
rustdar_source::impl_job_input!(OutlooksInput);

/// [`RasterizeOutput`] for the reason [`rasterize_radar_sites`] answers one:
/// with the outlook render a described job, `rustdar_frontend::offload`'s
/// `execute` calls this directly and needs the alpha convention stated by the
/// value rather than known by the caller. Premultiplied on every path.
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

    // Two passes: hatching must go over every fill, including fills drawn by
    // features later in the list.
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

/// The paint-relevant slice of one mesoscale discussion: what
/// [`rasterize_spc_discussions`] reads of an
/// [`SpcDiscussion`](crate::spc::discussion::SpcDiscussion), and nothing else.
/// The title, text and link stay page-side — they are popup content, and a
/// raster described over a message port should not carry prose it never draws.
#[derive(Debug, Clone, PartialEq)]
pub struct DiscussionPaint {
    /// Chooses the fill and stroke colours (`crate::spc::colors`).
    pub md_type: crate::spc::discussion::MdType,
    /// Every ring drawn as its own filled polygon — MDs carry no holes.
    pub polygon: rustdar_geo::GeoPolygon,
}

/// Everything [`rasterize_spc_discussions`] reads besides the raster's own
/// geometry — the **wire form** of the discussion render. See
/// [`OutlooksInput`].
#[derive(Debug, Clone, PartialEq)]
pub struct DiscussionsInput {
    pub discussions: Vec<DiscussionPaint>,
    /// See the `device_scale` note on
    /// [`RasterizeContext`](crate::render::overlay_state::RasterizeContext).
    pub device_scale: f32,
}

rustdar_source::impl_job_input!(DiscussionsInput);

/// [`RasterizeOutput`] for [`rasterize_spc_outlooks`]'s reason. Premultiplied
/// on every path.
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
            // No longitude shift here, unlike every other polygon path, and
            // that is a fact about the *parser* rather than a gap. A mesoscale
            // discussion's points are decoded from the `LAT...LON` block by
            // `spc::discussion::parse_coord_token`, which drops outright any
            // point failing `(-140.0..=-50.0).contains(&lon)`. So this ring
            // cannot hold a longitude within 40° of the antimeridian, and the
            // frame the viewport is in cannot matter to it. Widening that gate
            // means revisiting this call.
            let pts: Vec<(f32, f32)> = ring
                .iter()
                .map(|&(lat, lon)| mb.project(lat, lon, w, h))
                .collect();
            if let Some(path) = build_polygon_path(&pts) {
                // Every ring of an MD is drawn as its own filled polygon, so
                // there are no holes to honour here.
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

/// The paint-relevant slice of one alert: what [`rasterize_nws_alerts`] reads
/// of an [`NwsAlert`](crate::nws::alert::NwsAlert), and nothing else. The
/// headline, description, instruction and the rest stay page-side — popup
/// content, never drawn by this rasterizer, and not worth a codec on the
/// message port the described job crosses.
#[derive(Debug, Clone, PartialEq)]
pub struct AlertPaint {
    /// Tested against [`AlertsInput::hidden_ids`].
    pub id: String,
    /// Tested against [`AlertsInput::enabled_categories`].
    pub category: AlertCategory,
    /// The geometry, colours and (unused here) hatch of the alert's area.
    ///
    /// Shared with the owning
    /// [`NwsAlert`](crate::nws::alert::NwsAlert) — building a row refcounts
    /// the parse-time `Arc` instead of deep-cloning the national feed's
    /// geometry per raster dispatch. `PartialEq` stays value-based (`Arc`
    /// derefs through `==`): the wire round-trip compares a decoded copy in a
    /// fresh `Arc` against this one, and pointer identity would fail it.
    pub features: Arc<Vec<OverlayFeature>>,
}

/// Everything [`rasterize_nws_alerts`] reads besides the raster's own
/// geometry — the **wire form** of the alert render. See [`OutlooksInput`].
///
/// The category and hidden-id filters travel *with* the rows rather than
/// being applied before them, exactly as the argument list always took them:
/// which alerts paint stays this function's own decision, made identically on
/// the direct path and in a worker.
#[derive(Debug, Clone, PartialEq)]
pub struct AlertsInput {
    pub alerts: Vec<AlertPaint>,
    pub enabled_categories: Vec<AlertCategory>,
    pub hidden_ids: HashSet<String>,
    /// See the `device_scale` note on
    /// [`RasterizeContext`](crate::render::overlay_state::RasterizeContext).
    pub device_scale: f32,
}

rustdar_source::impl_job_input!(AlertsInput);

/// Renders only alerts in `enabled_categories` and not in `hidden_ids`.
///
/// [`RasterizeOutput`] for [`rasterize_spc_outlooks`]'s reason. Premultiplied
/// on every path.
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

/// Deliberately not `rustdar_radar`'s site type: keeps this crate decoupled.
#[derive(Debug, Clone, PartialEq)]
pub struct RadarSiteInfo {
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    pub is_current: bool,
    pub is_loading: bool,
}

/// Everything [`rasterize_radar_sites`] reads besides the raster's own
/// geometry, as one struct: the site rows and the three appearance inputs.
///
/// This is the **wire form** of the sites render. The struct exists so that a
/// job described over a message port and a direct call cannot come to take
/// different inputs: the sites codec row ([`crate::render::jobs::SitesJob`])
/// carries exactly this type, decodes back into it, and calls the same
/// function the direct path calls — byte-identity between the two paths is a
/// property of the type rather than of two argument lists kept in step.
///
/// The raster's own geometry — bounds, width, height — deliberately stays
/// outside: those are shared by every overlay kind and travel once on the
/// enclosing job, where one statement of them cannot disagree with another.
///
/// Derives what the job enum derives, for the reason [`GeoBounds`]'s
/// `PartialEq` states.
#[derive(Debug, Clone, PartialEq)]
pub struct SitesInput {
    pub sites: Vec<RadarSiteInfo>,
    /// The *actual* zoom — the quantized cache key divided back out — because
    /// the marker radius is a function of it.
    pub zoom: f64,
    pub is_dark: bool,
    /// Physical texels per point, as the texture plan counted them. See the
    /// `device_scale` note on [`RasterizeContext`](crate::render::overlay_state::RasterizeContext).
    pub device_scale: f32,
}

rustdar_source::impl_job_input!(SitesInput);

/// [`RasterizeOutput`] and not a bare buffer, unlike its neighbours, because
/// this is the one rasterizer whose caller is not an
/// [`OverlayHandler`](crate::render::overlay_state::OverlayHandler): `app_fetch`
/// invokes it directly, so there is no handler in between to state the alpha
/// convention on its behalf. Returning the mode with the bytes is what keeps
/// that call site from having to know it.
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
    // Chosen in points and converted to texels, rather than chosen in texels:
    // the clamps are what a dot should look like on a screen, not how many
    // samples of one it takes.
    let radius = ((5.0 + zoom_f32).clamp(4.0, 12.0)).max(1.0) * scale;
    let stroke_w = (radius * 0.3).clamp(0.5 * scale, 2.0 * scale);

    let text_bg = if is_dark {
        Color::from_rgba8(0, 0, 0, 140)
    } else {
        Color::from_rgba8(255, 255, 255, 140)
    };

    for site in sites {
        // Into the viewport's frame first: the catalogue validates longitude
        // into [-180, 180] (`rustdar_radar::catalogue`) while `bounds` is
        // unfolded, so over the dateline the two disagree by a turn. The
        // network really does straddle it — of the 208 stations
        // `api.weather.gov/radar/stations` lists, 4 are east-hemisphere
        // (PGUA 144.81E, RODN 127.91E, RKSG 127.29E, RKJK 126.62E) and the
        // westernmost is PAEC at -165.29.
        let lon = mb.nearest_lon(site.lon);
        let (px, py) = mb.project(site.lat, lon, w, h);
        // 50 points of slack so a site just off-texture still contributes its
        // label. In texels here, so the ground it stands for does not shrink
        // with density.
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
            // egui draws the glyphs over this pill at a fixed *point* size, so
            // the pill has to be that many points wide in texels or the text
            // overflows the background it is drawn on.
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

// ── Storm report symbol helpers ───────────────────────────────────────

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

/// The paint-relevant slice of one storm report: what
/// [`rasterize_storm_reports`] reads of a
/// [`StormReport`](crate::spc::reports::StormReport), and nothing else. The
/// time, magnitude, location and comments stay page-side — popup content the
/// raster never draws, and not worth bytes on the message port the described
/// job crosses.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReportPaint {
    /// Chooses the colour and the symbol.
    pub kind: StormReportKind,
    pub lat: f64,
    pub lon: f64,
}

/// Everything [`rasterize_storm_reports`] reads besides the raster's own
/// geometry — the **wire form** of the storm-reports render. See
/// [`OutlooksInput`] for the shape's reason.
///
/// A report's position in `reports` is its **hit-map id**: the cells this
/// render answers record indices into this list, and the page zips them with
/// an item list captured from the same handler data in the same order
/// ([`HitMap::from_cells`] states the invariant).
#[derive(Debug, Clone, PartialEq)]
pub struct ReportsInput {
    pub reports: Vec<ReportPaint>,
    /// The actual zoom — the marker radius is a function of it.
    pub zoom: f64,
    /// Picks the outline colour.
    pub is_dark: bool,
    /// See the `device_scale` note on
    /// [`RasterizeContext`](crate::render::overlay_state::RasterizeContext).
    pub device_scale: f32,
}

rustdar_source::impl_job_input!(ReportsInput);

/// Tornado = red, hail = green, wind = blue. Below a 5 px radius the symbols
/// are unreadable, so it falls back to filled dots.
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
    // Includes the stroke, so the outline itself is clickable.
    let hit_radius = radius + stroke_w;

    let outline = if is_dark {
        Color::from_rgba8(255, 255, 255, 220)
    } else {
        Color::from_rgba8(40, 40, 40, 220)
    };

    let mut hit_cells = HitCells::new(width, height);

    for (idx, report) in reports.iter().enumerate() {
        // Into the viewport's frame first — see `rasterize_radar_sites`. The
        // CSV parser accepts any longitude in [-180, 180] verbatim
        // (`spc::reports::parse_csv`) and `bounds` is unfolded. Measured, this
        // feed stays far from the seam: over the 70,022 located records of
        // SPC's 1950-2023 tornado archive the westernmost is -163.53 and none
        // is east-hemisphere, so this is a guard against the frame, not a
        // repair of an observed loss.
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

        // The report's position in the input list **is** its id — the page's
        // id_map is keyed the same way, which is the whole zip contract.
        let item_id = idx as u32;
        let min_x = (px - hit_radius).max(0.0) as i32;
        let max_x = ((px + hit_radius) as i32).min(width as i32 - 1);
        let min_y = (py - hit_radius).max(0.0) as i32;
        let max_y = ((py + hit_radius) as i32).min(height as i32 - 1);
        let r2 = hit_radius * hit_radius;
        // Step 4: one sample per quarter-res cell.
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

// ── GLM Lightning rasterization ──────────────────────────────────────────

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
/// flash energies: about a sixth of flashes in a sampled GOES-West granule
/// exceed 1e-12 J, so the largest share a size. Input must be CF-unpacked —
/// raw packed counts (tens to thousands) clamp to the top for every strike.
///
/// `None` draws at the midpoint and must not collapse to either end: 0.0
/// renders "unknown" as "weakest", 1.0 as "strongest".
fn energy_size_scale(energy: Option<f32>) -> f32 {
    match energy {
        Some(e) => (e.log10().clamp(-16.0, -12.0) + 16.0) / 4.0,
        None => 0.5,
    }
}

/// The paint-relevant slice of one GLM flash: what [`rasterize_glm_strikes`]
/// reads of a [`GlmFlash`], and nothing else — the satellite, level and area
/// stay page-side with the popup.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FlashPaint {
    pub lat: f64,
    pub lon: f64,
    /// UTC. Aged against [`GlmStrikesInput::now`] for the fade ramp and the
    /// window cull.
    pub time: chrono::NaiveDateTime,
    /// Radiant energy in joules; sizes the bolt. `None` means unknown — see
    /// [`energy_size_scale`].
    pub energy: Option<f32>,
}

/// Everything [`rasterize_glm_strikes`] reads besides the raster's own
/// geometry — the **wire form** of the lightning render. See
/// [`OutlooksInput`] for the shape's reason, and [`ReportsInput`] for the
/// hit-map id contract, which is the same here: a flash's position in
/// `flashes` is its hit-map id.
#[derive(Debug, Clone, PartialEq)]
pub struct GlmStrikesInput {
    pub flashes: Vec<FlashPaint>,
    pub zoom: f64,
    pub is_dark: bool,
    /// Flashes older than this many seconds are dropped; younger ones fade
    /// through [`time_decay_color`]'s ramp over it.
    pub time_window_secs: f64,
    /// **The page's clock at dispatch, never the worker's.** Flash age — the
    /// fade colour and the window cull — is `now - flash.time`, and a worker
    /// that re-read its own clock here would render a different picture than
    /// the direct call: parity between the two paths is only byte-exact
    /// because this value travels on the wire with the flashes it ages. The
    /// capture site is [`RasterizeContext::now`], filled by the dispatching
    /// pane; `rustdar_frontend::offload::tests` pins that a shifted `now`
    /// really does change the picture on a fixture whose flashes straddle the
    /// fade steps, so a worker re-derivation cannot pass the parity gate.
    ///
    /// [`RasterizeContext::now`]: crate::render::overlay_state::RasterizeContext::now
    pub now: chrono::NaiveDateTime,
    /// See the `device_scale` note on
    /// [`RasterizeContext`](crate::render::overlay_state::RasterizeContext).
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

        // The flash's position in the input list **is** its id — see
        // `rasterize_storm_reports`, whose contract this shares.
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

// ── Feature rendering ────────────────────────────────────────────────────

fn draw_feature(
    pixmap: &mut Pixmap,
    feature: &OverlayFeature,
    mb: &MercatorBounds,
    w: f32,
    h: f32,
    scale: f32,
) {
    // Geo-AABB cull before any projection work.
    //
    // Shifted into the texture's frame first, or the cull is where a Pacific
    // feature dies: `OverlayFeature::geo_bounds` is the raw GeoJSON extent —
    // Guam's zone `GUZ001` is 144.62..144.96 — and a dateline viewport arrives
    // as e.g. -195..-165, two rectangles that cannot overlap however close the
    // ground is. The shift is the feature's, not each polygon's, and it is
    // deliberately loose: a feature the source already cut at the antimeridian
    // pools into a ~360°-wide box, `lon_shift` declines it, and the box
    // intersects everything. That costs a projection pass this cull existed to
    // save and never drops a feature that should draw — the failure a cull may
    // have.
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
                // The path carries the holes as subpaths, so this outlines
                // them too: a hole's edge is as much a boundary of the feature
                // as the exterior is.
                let sw = scaled_stroke_width(&path, 1.5, scale);
                stroke_path(pixmap, &path, feature.stroke_rgba, sw);
            }
        }
    }
}

// ── Path building helpers ────────────────────────────────────────────────

/// Thins the stroke below a 40-point minimum dimension, so a small polygon is
/// not swallowed by its own outline. `base` is the width at close zoom, in
/// points; `scale` is texels per point.
///
/// Both the threshold and the clamps are in points and converted here, and the
/// middle term needs no conversion at all: `min_dim` grows with density and the
/// threshold grows with it, so the ratio is already right and only the bounds
/// it is held between have to move. Left unscaled, a polygon large enough to
/// take the full `base` would be outlined at `base` texels — half a point's
/// width on a 2x display — which is the case this whole parameter exists for,
/// since most polygons on screen are the large ones.
fn scaled_stroke_width(path: &tiny_skia::Path, base: f32, scale: f32) -> f32 {
    let b = path.bounds();
    let min_dim = b.width().min(b.height());
    (min_dim / 40.0 * base).clamp(0.5 * scale, base * scale)
}

/// A device scale that can be multiplied by, from one that may not be.
///
/// The value crosses a public API and reaches a marker radius and a
/// `tiny_skia::Rect`, where a zero draws nothing at all and a `NaN` makes
/// `Rect::from_xywh` return `None` — both of which are an overlay that silently
/// stops painting rather than an error anyone can read. Below one texel per
/// point there is nothing to gain and a sub-point marker to lose, so that is
/// the floor.
fn sane_device_scale(device_scale: f32) -> f32 {
    if device_scale.is_finite() {
        device_scale.max(1.0)
    } else {
        1.0
    }
}

/// Interior rings below this projected area (square pixels) are dropped.
///
/// They are not rare and they are not hypothetical. When this was written, RDP
/// simplification collapsed a small closed ring to a retracing out-and-back,
/// and 2,515 of the 4,579 interior rings in a full 7,015-zone cache had exactly
/// zero area — every one of them a three-point ring whose shoelace terms cancel
/// pairwise. Such a ring encloses nothing, so even-odd already ignores it for
/// the fill — but it is still a subpath, and the stroke would draw every one of
/// them as a hairline scratch across the zone.
///
/// [`simplify_ring`](crate::render::geo::simplify_ring) no longer produces
/// them: it tightens its own tolerance rather than flatten a ring, so what used
/// to arrive here as a zero-area sliver now arrives as the small real hole it
/// always was. That does not retire this floor — it is what the floor was
/// always for. A hole a third of a pixel across is still nothing anyone can
/// see, and deciding *that* needs the projection, which is why the decision is
/// here and not upstream. What changed is that it now judges honest geometry
/// instead of covering for a simplifier that had already destroyed it.
const MIN_HOLE_AREA_PX: f32 = 0.25;

/// Interior rings thinner than this *on average* — twice the area over the
/// perimeter — are dropped as well.
///
/// Area alone does not catch what it was written to catch. A ring can clear
/// [`MIN_HOLE_AREA_PX`] and still be a scratch: the worst in that same cache
/// (`forecast_PKZ785`) spreads 0.39 px² over 31.1 px of rim, so it is a fortieth
/// of a pixel wide and 15 px long. Even-odd removes a fortieth of a pixel of
/// alpha, which nobody can see; the stroke then draws its rim at full opacity,
/// which is a visible line across a zone that has no visible hole in it. The
/// two tests together are the claim: a ring is kept only if it is both big
/// enough and thick enough to be *seen* as a hole, not merely outlined as one.
///
/// A quarter of a pixel, measured: it drops 78 of the 2,040 rings that clear
/// the area floor and keeps 1,962. The largest ring it drops is 4.6 px² spread
/// over 41 px of rim; the largest it keeps anywhere near the boundary is
/// 20 px² over 145 px (`forecast_GMZ237`, 0.276 px wide), a cut-out big enough
/// to see. Every ring it drops was invisible before this change too, which
/// never drew an interior ring at all — so this can only decline to add a
/// scratch, never remove a hole the map has been showing.
const MIN_HOLE_WIDTH_PX: f32 = 0.25;

/// Unsigned shoelace area, so it says nothing about which way the ring winds.
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

/// Closed length of the ring, so the last vertex joins back to the first.
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

/// Whether an interior ring is worth cutting *and outlining* at the size it is
/// drawn. See [`MIN_HOLE_AREA_PX`] and [`MIN_HOLE_WIDTH_PX`].
pub(crate) fn hole_is_drawable(pts: &[(f32, f32)]) -> bool {
    let area = ring_area_px(pts);
    let perimeter = ring_perimeter_px(pts);
    // `2 * area >= width * perimeter` rather than a division, so a ring of zero
    // perimeter cannot produce a NaN. It cannot reach here anyway: zero
    // perimeter means zero area, which the first test already rejects.
    area >= MIN_HOLE_AREA_PX && 2.0 * area >= MIN_HOLE_WIDTH_PX * perimeter
}

/// A GeoJSON polygon projected to texture pixels: the exterior ring, and the
/// interior rings that [`hole_is_drawable`] keeps.
///
/// One definition of "where this polygon's interior is", so the fill
/// ([`draw_feature`]) and the hatch mask ([`crate::render::hatch`]) cannot
/// answer differently — which they did while only the fill honoured holes, the
/// hatch painting straight across a hole the fill had just cut.
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

/// `None` when the exterior ring is too short to enclose anything.
///
/// The whole polygon — exterior and holes together — is translated into the
/// viewport's longitude frame by one shared multiple of 360°, taken from the
/// *exterior* ring. Sharing it is what makes the move rigid: a hole shifted by
/// a different multiple than the ring it cuts would leave the polygon, and a
/// per-vertex [`MercatorBounds::wrap_lon`] would tear a ring in half at the
/// seam. See [`MercatorBounds::lon_shift`] for why the nearest representation
/// is the right one and when it declines to answer.
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
    // `polygon[1..]` are interior rings — holes. Dropping them painted a donut
    // as a solid blob while `geo_point_in_feature` counted the hole as outside,
    // so a click in the cut-out of a marine zone or a nested SPC outlook hit
    // nothing the eye could see was not there.
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

/// One closed subpath. Repeated calls on the same builder produce the
/// multi-subpath path a polygon with holes needs.
fn push_ring(pb: &mut PathBuilder, pts: &[(f32, f32)]) {
    pb.move_to(pts[0].0, pts[0].1);
    for &(x, y) in &pts[1..] {
        pb.line_to(x, y);
    }
    pb.close();
}

/// `None` for degenerate paths, which tiny-skia cannot fill.
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

/// A whole GeoJSON polygon — exterior ring plus its interior rings — as one
/// path, with the fill rule that path must be filled under.
///
/// **Even-odd, because that is what the hit test computes.**
/// `geo_point_in_feature` counts ray crossings per ring and never looks at
/// orientation, so even-odd is the rule that makes painted pixels and clicks
/// answer the same question. `Winding` would instead punch a hole only where
/// the interior ring runs against its exterior.
///
/// That distinction is not academic even though the producers mostly behave.
/// Measured over a full 7,015-zone NWS cache and eight archived SPC outlooks:
/// of 2,064 interior rings with any area at all, 2,062 wind against their
/// exterior as GeoJSON's right-hand rule asks — and two do not, so `Winding`
/// would leave those two solid. The other 2,515 interior rings in that cache
/// have *zero* area after simplification and so no orientation for a
/// winding rule to consult (see `MIN_HOLE_AREA_PX`). A rule that must ask
/// which way a ring turns has, for more than half of this data, nothing to
/// ask.
///
/// The exterior alone keeps `Winding`, the rule it has always been filled
/// under. The two rules agree on any simple ring, but RDP simplification
/// leaves 811 of the 17,306 rings in that cache (4.7%) self-intersecting, and
/// some of those enclose a doubly-wound region that even-odd would drop — 400
/// uniform samples of each such ring's bounding box find one in 46 of the 811.
/// Sampling can only under-count a lobe it never lands in, so 46 is a floor and
/// not a fraction. Holes are this change's business; re-cutting hole-free
/// coastlines is not.
///
/// **What that deferral costs.** 694 of the 12,727 exterior rings in that cache
/// (5.5%) self-intersect, and wherever such a ring carries no hole its fill
/// still reads `Winding` while `geo_point_in_feature` reads it even-odd: the
/// same paint/click disagreement this commit closes for holes, left open for
/// self-intersection. It is smaller — the two rules differ only on the
/// doubly-wound lobes an RDP crossing leaves behind, not on a whole enclave —
/// but it is the same bug, and it is still here.
///
/// **A second residual, of the same family.** RDP can also leave two interior
/// rings of one polygon overlapping: 99 such pairs in that cache, across 29
/// polygons, none nested, ~70 px² of overlap in total. Even-odd paints the
/// overlap (two hole crossings plus the exterior is odd) while the hit test's
/// `any(hole contains)` calls it outside. That is a strict improvement on what
/// this function did before — the region used to be painted along with both
/// holes, and is now painted alone — so it is recorded, not fixed.
pub(crate) fn build_filled_polygon_path(
    exterior: &[(f32, f32)],
    holes: &[Vec<(f32, f32)>],
) -> Option<(tiny_skia::Path, FillRule)> {
    // The degeneracy gate stays on the exterior, so a polygon that used to
    // draw still draws no matter what its holes look like — and so the
    // hole-free path below is bit-for-bit the one this function has always
    // returned.
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

// ── Utilities ────────────────────────────────────────────────────────────

/// Drops GeoJSON's closing duplicate vertex (last == first).
pub(crate) fn strip_closing_dup(ring: &[(f64, f64)]) -> &[(f64, f64)] {
    if ring.len() > 3 && ring.first() == ring.last() {
        &ring[..ring.len() - 1]
    } else {
        ring
    }
}

use crate::hrrr::HrrrGridData;

/// Returns degrees.
fn merc_y_to_lat(merc_y: f64) -> f64 {
    rustdar_geo::mercator_y_to_lat_rad(merc_y).to_degrees()
}

// ── Model data (HRRR) rasterization ──────────────────────────────────────

/// Half-open `(i, j)` ranges of the grid the rasterizer touches.
///
/// Public because it is on the wire: [`ModelWindow`] carries the window its
/// values were cut to, computed once at the dispatch and never recomputed on
/// the far side — the window math runs through libm (`index_bounds` projects),
/// and a worker that re-derived it could land one index off at an exact
/// boundary and read outside the values it was sent.
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

    /// Points inside the window.
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
    /// window has been projected. The grid's own edges need no ring — the
    /// sizing code already has a one-sided branch for them.
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
///
/// `0.55` cells is the deliberate overlap the cell loop applies; the rest is
/// headroom for that spacing varying with latitude across the box.
const CELL_REACH: f64 = 0.75;

/// The same reach, in pixels: the half-extent has a `0.5` px floor, and `as i32`
/// truncates toward zero, so a rect ending just left of `x = 0` still paints
/// column 0. Rounded up from 1.5.
const PIXEL_REACH: f64 = 2.0;

/// `bounds` widened by `lon_pad` degrees and `merc_pad` of Mercator `y`.
///
/// Latitude is padded in Mercator, not in degrees, because that is the axis the
/// texture's pixels are linear in.
fn grow_bounds(bounds: &GeoBounds, lon_pad: f64, merc_pad: f64) -> GeoBounds {
    let mb = MercatorBounds::from_geo(bounds);
    GeoBounds {
        min_lon: bounds.min_lon - lon_pad,
        max_lon: bounds.max_lon + lon_pad,
        min_lat: merc_y_to_lat(mb.merc_y_min - merc_pad),
        max_lat: merc_y_to_lat(mb.merc_y_max + merc_pad),
    }
}

/// Which grid points [`rasterize_model_data`] must project.
///
/// The whole grid unless the coordinates can name a narrower window — see
/// [`crate::hrrr::GridCoords::index_bounds`].
///
/// A point *outside* the texture still paints into it, so the box is grown
/// before the index range is taken. Two things about that growth are
/// load-bearing:
///
///  * it grows the **box**, not the index range. A Lambert row is not a
///    parallel, so the box's latitude edge bounds `i` as well as `j`: a point a
///    hair south of the box can sit at an `i` well outside the unwidened range
///    while its longitude — and hence its column of pixels — is squarely inside
///    the texture.
///  * the pad is **absolute** — [`CELL_REACH`] grid cells and [`PIXEL_REACH`]
///    texture pixels — not a fraction of the box. The rasterizer's reach is
///    0.55 of a *cell*, which does not shrink with the viewport. Scaling the pad
///    to the box instead blanks the overlay below ~0.3 cells per texture, i.e.
///    from about map zoom 19 up, where a single cell covers the whole texture.
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

    // A grid with a longitude discontinuity in it — across the anti-meridian or
    // across the cone's own seam — has an `i` neighbour most of a turn away, so
    // its cell is stretched right across the texture and the "0.55 of a cell"
    // reach this window is built on stops describing it. The *box* crossing the
    // seam is handled inside `index_bounds`; this is the same hazard from the
    // grid's side, which nothing there can see.
    if coords.wraps_longitude() {
        return full;
    }

    // `cos` is taken at the box's own extreme latitude: the only cells that can
    // reach the texture sit within a cell of the box, where the spacing is the
    // same to well inside the headroom in `CELL_REACH`.
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

/// What [`rasterize_model_data`] reads — the model-grid overlay's input, in
/// the two carries its size forces where every other kind needs one.
///
/// The HRRR values vector is 1,905,141 `f32` — **7.62 MB** — and the raster
/// only ever reads the points inside its own [`projection_window`]. So the
/// input is an enum over *how much of the grid is in hand*:
///
///  * [`Self::Whole`] is the dispatch's carry: the grid as fetched, by
///    `Arc`, so the native path (which moves this enum by value through the
///    pool and serialises nothing) pays a refcount where every other carry
///    would pay a memcpy.
///  * [`Self::Window`] is the wire's carry: the window and exactly its
///    values. `rustdar_frontend::offload`'s encoder cuts a `Whole` down to
///    this at `to_bytes` time — the one place that knows the texture's
///    bounds — and its decoder only ever produces this form.
///
/// Both arms run the one rasterizer, and the byte parity between them is
/// pinned twice: `render::rasterize::model_window_tests` proves a proper
/// subset window paints identically to the whole grid, and
/// `rustdar_frontend::offload::tests` proves it again through the codec.
#[derive(Debug, Clone, PartialEq)]
pub enum ModelDataInput {
    /// The grid as fetched, whole, by reference.
    Whole(std::sync::Arc<HrrrGridData>),
    /// The projection window and exactly its values — what travels.
    Window(ModelWindow),
}

rustdar_source::impl_job_input!(ModelDataInput);

/// The wire form of a model-grid raster: the grid's shape and coordinates,
/// the [`IndexWindow`] its values were cut to, and those values alone.
///
/// `coords` travels whole on either arm because its cost is nothing like the
/// values': the Lambert case — every real HRRR fetch — is a 104-byte constant
/// struct ([`crate::hrrr::lambert::LambertGridParts`]), and the explicit case
/// (materialised coordinate arrays; no production source produces one) never
/// has a proper-subset window to cut to, since [`projection_window`] can only
/// narrow a Lambert grid.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelWindow {
    pub parameter: crate::hrrr::ModelParameter,
    /// The **full grid's** shape — indices in `win` and `coords` are stated
    /// against it, exactly as they are against a whole grid.
    pub ni: usize,
    pub nj: usize,
    pub coords: crate::hrrr::GridCoords,
    /// The window `values` covers, computed at the dispatch and carried —
    /// see [`IndexWindow`] for why it is never recomputed here.
    pub win: IndexWindow,
    /// Row-major within `win`: point `(i, j)` of the grid is
    /// `values[(j - win.j0) * (win.i1 - win.i0) + (i - win.i0)]`.
    pub values: Vec<f32>,
}

impl ModelDataInput {
    pub fn parameter(&self) -> crate::hrrr::ModelParameter {
        match self {
            Self::Whole(grid) => grid.parameter,
            Self::Window(window) => window.parameter,
        }
    }

    /// The full grid's `(ni, nj)`.
    pub fn shape(&self) -> (usize, usize) {
        match self {
            Self::Whole(grid) => (grid.ni, grid.nj),
            Self::Window(window) => (window.ni, window.nj),
        }
    }

    pub fn coords(&self) -> &crate::hrrr::GridCoords {
        match self {
            Self::Whole(grid) => &grid.coords,
            Self::Window(window) => &window.coords,
        }
    }

    /// The index window a raster of `bounds` at `width` × `height` may draw
    /// from: computed for a whole grid, **carried** for a window — the
    /// window's values are all it has, so the window it was cut to is the
    /// only honest answer whatever bounds are asked about.
    pub fn window_for(&self, bounds: &GeoBounds, width: u32, height: u32) -> IndexWindow {
        let (ni, nj) = self.shape();
        match self {
            Self::Whole(grid) => projection_window(&grid.coords, ni, nj, bounds, width, height),
            Self::Window(window) => window.win.clamped(ni, nj),
        }
    }

    /// The value at grid point `(i, j)`, or `None` where there is none to
    /// read — past a short values vector on the whole arm (the same skip the
    /// rasterizer has always made there), or outside the carried window.
    fn value_at(&self, i: usize, j: usize) -> Option<f32> {
        match self {
            Self::Whole(grid) => grid.values.get(j * grid.ni + i).copied(),
            Self::Window(window) => {
                let win = &window.win;
                if i < win.i0 || i >= win.i1 || j < win.j0 || j >= win.j1 {
                    return None;
                }
                window
                    .values
                    .get((j - win.j0) * (win.i1 - win.i0) + (i - win.i0))
                    .copied()
            }
        }
    }

    /// One row of `win`'s values, exactly as the wire writes them: the whole
    /// arm slices its values vector (padding with NaN where the vector runs
    /// short of `ni × nj`, which paints nothing — `color_for_value(NAN)` is
    /// transparent, pinned by `model_nan_tests`), the window arm hands back
    /// the row it carries.
    ///
    /// A callback per row rather than a returned `Vec` so the encoder writes
    /// straight from the grid's own storage — one pass, no intermediate
    /// buffer of up to 7.62 MB.
    pub fn for_each_window_row(&self, win: &IndexWindow, mut f: impl FnMut(&[f32])) {
        if win.is_empty() {
            return;
        }
        match self {
            Self::Whole(grid) => {
                let mut padded: Vec<f32> = Vec::new();
                for j in win.j0..win.j1 {
                    let start = j * grid.ni + win.i0;
                    let end = j * grid.ni + win.i1;
                    if end <= grid.values.len() {
                        f(&grid.values[start..end]);
                    } else {
                        padded.clear();
                        padded.extend(
                            (start..end).map(|k| grid.values.get(k).copied().unwrap_or(f32::NAN)),
                        );
                        f(&padded);
                    }
                }
            }
            Self::Window(window) => {
                // `win` is this input's own window (what `window_for` answers),
                // stated relative to the carried one so a caller asking for a
                // clamped sub-window still gets rows of the width it asked for.
                let carried = &window.win;
                let row_w = carried.i1 - carried.i0;
                for j in win.j0..win.j1 {
                    let row = (j - carried.j0) * row_w;
                    f(&window.values[row + (win.i0 - carried.i0)..row + (win.i1 - carried.i0)]);
                }
            }
        }
    }
}

/// Writes pixels directly rather than going through tiny-skia: one filled
/// rectangle per grid point, sized from its neighbour spacing.
///
/// Takes the wire-form input the described job decodes back into
/// ([`ModelDataInput`]), the way [`SitesInput`] and its siblings are taken:
/// "over a port" and "on this thread" run the same function, and byte
/// identity between the whole grid and its window is a pinned property
/// rather than a hope.
pub fn rasterize_model_data(
    input: &ModelDataInput,
    bounds: &GeoBounds,
    width: u32,
    height: u32,
) -> RasterizeOutput {
    let size = (width * height * 4) as usize;
    let mut rgba = vec![0u8; size];
    let (ni, nj) = input.shape();
    let parameter = input.parameter();
    let coords = input.coords();

    let empty = matches!(input, ModelDataInput::Whole(grid) if grid.values.is_empty());
    if empty || width == 0 || height == 0 || ni == 0 || nj == 0 {
        return RasterizeOutput {
            rgba,
            hit_cells: None,
            alpha: AlphaMode::Straight,
        };
    }

    let mb = MercatorBounds::from_geo(bounds);
    let w = width as f32;
    let h = height as f32;

    // `coords.at` is the Lambert inverse for HRRR, and this loop used to run it
    // over all 1.9 M points — two thirds of this function's cost, on the
    // background render thread, re-paid on every zoom step and every third of a
    // viewport of pan (`OverlayTextureCache::needs_rerender`). Only points that
    // can influence a pixel of *this* texture are projected now — and for a
    // windowed input, only those points' values ever travelled at all.
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
            let color = parameter.color_for_value(value);
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
