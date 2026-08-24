use crate::par::*;

use crate::l3_values::{build_eet_lut, build_vil_lut, decode_legacy_thresholds, l3_physical_value};
use crate::palette::get_color_for_value;
use crate::types;
use nexrad_model::data::{DataMoment, Radial, Scan};
use std::f64::consts::PI;
use std::sync::atomic::{AtomicU64, Ordering};

pub mod polar;

/// Pre-computed Web Mercator projection constants, derived from
/// [`types::ImageBounds`] so the pixel grid aligns with the bounds the UI gets.
struct MercatorProjection {
    sin_radar_lat: f64,
    cos_radar_lat: f64,
    center_px: f64,
    merc_y_top: f64,
    merc_y_scale: f64,
    /// Pixels per radian of longitude east of the site.
    lon_rad_to_px: f64,
    /// The image's scale, pixels per kilometre east-west at the site.
    px_per_km: f64,
    /// The half-width this raster covers, km — [`types::plan_view_extent_km`]
    /// of the sweep's own reach.
    extent_km: f64,
    /// The image's side, pixels — [`types::raster_side_px`]'s answer for this
    /// extent and this caller's ceiling.
    side_px: usize,
}

/// The longest step `render_gate` will take on either of its axes once it has
/// to raise the sample count, in pixels.
///
/// `render_gate` walks a lattice of sample points and drops each on the pixel
/// it lands in. For the raster to come out whole, every pixel square under the
/// gate has to receive at least one sample — and the property that guarantees
/// that is the lattice's **covering radius**, the furthest any point in the
/// plane can be from the nearest sample. A unit square's inradius is 0.5, so a
/// point within 0.5 of a square's centre is inside that square; a lattice whose
/// covering radius is under half a pixel therefore cannot miss one.
///
/// It is a bound on each *step*, not on the radius: two steps at 0.7 give
/// `hypot(0.7, 0.7) = 0.9899`, a covering radius of **0.495 px**, which is
/// under the bar with 1 % of margin. Holding each step at 0.5 instead would
/// buy nothing and cost 2× the samples on both axes.
const SAMPLE_STEP_PX: f64 = 0.7;

impl MercatorProjection {
    fn from_bounds(
        radar_lat: f64,
        bounds: &types::ImageBounds,
        extent_km: f64,
        side_px: usize,
    ) -> Self {
        let (sin_radar_lat, cos_radar_lat) = radar_lat.to_radians().sin_cos();
        let px_per_km = side_px as f64 / (2.0 * extent_km);
        Self {
            sin_radar_lat,
            cos_radar_lat,
            center_px: side_px as f64 / 2.0,
            merc_y_top: bounds.mercator_y_max,
            merc_y_scale: side_px as f64 / (bounds.mercator_y_max - bounds.mercator_y_min),
            lon_rad_to_px: squallar_geo::EARTH_RADIUS_KM * cos_radar_lat * px_per_km,
            px_per_km,
            extent_km,
            side_px,
        }
    }

    /// The pixel a point `ground_range_km` out at bearing `(sin_az, cos_az)`
    /// lands in, as fractional column and row.
    #[inline]
    fn pixel_at(&self, sin_az: f64, cos_az: f64, sin_d: f64, cos_d: f64) -> (f64, f64) {
        let sin_lat =
            (self.sin_radar_lat * cos_d + self.cos_radar_lat * sin_d * cos_az).clamp(-1.0, 1.0);
        let dlon =
            (sin_az * sin_d * self.cos_radar_lat).atan2(cos_d - self.sin_radar_lat * sin_lat);
        let merc_y = squallar_geo::mercator_y_from_sin_lat(sin_lat);
        (
            self.center_px + dlon * self.lon_rad_to_px,
            (self.merc_y_top - merc_y) * self.merc_y_scale,
        )
    }

    /// Paint one gate over the ground footprint [`GateSpan`] names.
    fn render_gate(
        &self,
        bufs: &RenderBuffers,
        ctx: &RadialContext,
        span: GateSpan,
        value: f32,
        from: GateId,
    ) {
        bufs.polar
            .paint(from, ctx.azimuth_deg, ctx.az_half_spacing, value);

        let GateSpan {
            near_km: range_start,
            far_km: range_end,
        } = span;
        let range_km = 0.5 * (range_start + range_end);

        let mut num_range_samples = ((range_end - range_start) * self.px_per_km).ceil() as i32 + 2;
        let mut num_az_samples =
            ((ctx.az_half_spacing * 2.0 * range_km * PI / 180.0) * self.px_per_km).ceil() as i32
                + 2;
        {
            let len_r = (range_end - range_start) * self.px_per_km;
            // The gate's *outer* row, not its centre: the arc lengthens with
            // range across the gate's own depth, so the widest row is the one
            // the guarantee has to hold for. The count itself still comes from
            // the centre, so a gate that is already covered is untouched.
            let len_t = ctx.az_half_spacing * 2.0 * range_end * PI / 180.0 * self.px_per_km;
            let step_r = len_r / num_range_samples.max(1) as f64;
            let step_t = len_t / num_az_samples.max(1) as f64;
            if step_r.hypot(step_t) >= 1.0 {
                num_range_samples = num_range_samples.max((len_r / SAMPLE_STEP_PX).ceil() as i32);
                num_az_samples = num_az_samples.max((len_t / SAMPLE_STEP_PX).ceil() as i32);
            }
        }
        let inv_num_range = 1.0 / num_range_samples.max(1) as f64;
        let inv_num_az = 1.0 / num_az_samples.max(1) as f64;

        let cell = RenderBuffers::cell(write_key(from), value);

        for r_step in 0..num_range_samples {
            let r = range_start + (range_end - range_start) * (r_step as f64 * inv_num_range);
            let (sin_d, cos_d) = (r / squallar_geo::EARTH_RADIUS_KM).sin_cos();

            for az_step in 0..num_az_samples {
                let t = az_step as f64 * inv_num_az;
                let sin_az = ctx.sin_az_start + ctx.sin_az_delta * t;
                let cos_az = ctx.cos_az_start + ctx.cos_az_delta * t;

                let (px, py) = self.pixel_at(sin_az, cos_az, sin_d, cos_d);
                let px_i = px as i32;
                let py_i = py as i32;

                if px_i >= 0
                    && px_i < self.side_px as i32
                    && py_i >= 0
                    && py_i < self.side_px as i32
                {
                    let pixel_idx = py_i as usize * self.side_px + px_i as usize;
                    bufs.claim(pixel_idx, cell);
                }
            }
        }
    }
}

/// The ground footprint of one gate: the two ranges its wedge runs between.
///
/// Half-open in the same sense [`MercatorProjection::render_gate`]'s sample
/// walk is — `[near_km, far_km)` — so that a boundary shared with the next gate
/// belongs to the gate outside it, and [`polar::PolarGeometry::pick`] agrees.
#[derive(Clone, Copy, Debug, PartialEq)]
struct GateSpan {
    /// The inner edge, ground km.
    near_km: f64,
    /// The outer edge, ground km. Never less than `near_km`.
    far_km: f64,
}

/// The ground edges of a radial's gates, in order, sharing each boundary
/// between the gate inside it and the gate outside it.
fn gate_ground_edges(
    first_gate_slant_km: f64,
    gate_interval_slant_km: f64,
    gates: usize,
    to_ground: impl Fn(f64) -> f64,
) -> impl Iterator<Item = GateSpan> {
    let half = gate_interval_slant_km / 2.0;
    let mut near_km = to_ground(first_gate_slant_km - half);
    (0..gates).map(move |j| {
        let far_km = to_ground(first_gate_slant_km + (j as f64 + 0.5) * gate_interval_slant_km);
        let span = GateSpan { near_km, far_km };
        near_km = far_km;
        span
    })
}

/// Pre-computed azimuth sin/cos values for a single radial strip.
struct RadialContext {
    sin_az_start: f64,
    cos_az_start: f64,
    sin_az_delta: f64,
    cos_az_delta: f64,
    az_half_spacing: f64,
    /// The radial's own azimuth, degrees, kept unencoded beside the sines and
    /// cosines the paint loop reads.
    azimuth_deg: f64,
}

impl RadialContext {
    /// A half-width below zero is not a narrow wedge, it is an inside-out one:
    /// `render_gate` derives its azimuth sample count from this number, so a
    /// negative value makes `0..count` empty and the radial silently paints
    /// nothing — a whole sweep can disappear without a line in the log. The
    fn new(azimuth_deg: f64, az_half_spacing_deg: f64) -> Self {
        debug_assert!(
            az_half_spacing_deg.is_finite(),
            "radial at {azimuth_deg}° was handed a non-finite half-width \
             ({az_half_spacing_deg})"
        );
        let az_half_spacing_deg = az_half_spacing_deg.max(0.0);
        let az_start_rad = (azimuth_deg - az_half_spacing_deg) * PI / 180.0;
        let az_end_rad = (azimuth_deg + az_half_spacing_deg) * PI / 180.0;
        let (sin_az_start, cos_az_start) = az_start_rad.sin_cos();
        let (sin_az_end, cos_az_end) = az_end_rad.sin_cos();
        Self {
            sin_az_start,
            cos_az_start,
            sin_az_delta: sin_az_end - sin_az_start,
            cos_az_delta: cos_az_end - cos_az_start,
            az_half_spacing: az_half_spacing_deg,
            azimuth_deg,
        }
    }
}

/// One atomic cell per output pixel: `(write_key << 32) | value_bits`.
///
/// Now there is one cell, so there is no pair to tear, and it is claimed with
/// `fetch_max` rather than a store. `fetch_max` is a set operation: the result
/// is the greatest claim, whatever order the claims arrive in. With
/// [`write_key`] ranking claims radial-major, gate-minor, the greatest claim is
/// the one a single-threaded radial-major render would have written last — so
/// the parallel result *is* the sequential result, not merely a stable one.
struct RenderBuffers {
    /// Borrowed from [`POOLED_CELLS`] for the length of one render and handed
    /// back by [`Self::into_output`].
    cells: Vec<AtomicU64>,
    /// The gates themselves, at the resolution the radar measured them,
    /// recorded as [`MercatorProjection::render_gate`] paints them.
    polar: polar::PolarBuffers,
    product: types::RadarProduct,
}

/// The one cell buffer this process keeps between plan-view renders.
static POOLED_CELLS: std::sync::Mutex<Option<Vec<AtomicU64>>> = std::sync::Mutex::new(None);

/// How much larger than the render asking for it a carried cell buffer may be
/// before [`RenderBuffers::checkout`] drops it instead of shrinking it.
const CELL_POOL_REUSE_FACTOR: usize = 4;

/// The one RGBA texture this process keeps between plan-view renders, and the
/// one value grid, in two slots that fill and empty independently.
static POOLED_IMAGE: std::sync::Mutex<Option<Vec<u8>>> = std::sync::Mutex::new(None);

/// See [`POOLED_IMAGE`], which documents both slots.
static POOLED_VALUES: std::sync::Mutex<Option<Vec<f32>>> = std::sync::Mutex::new(None);

/// The texture slot, with a poisoned lock read as a live one.
fn image_pool() -> std::sync::MutexGuard<'static, Option<Vec<u8>>> {
    POOLED_IMAGE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The value-grid slot. See [`image_pool`].
fn values_pool() -> std::sync::MutexGuard<'static, Option<Vec<f32>>> {
    POOLED_VALUES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A zeroed texture of exactly `len` bytes — the pool's if it has one.
///
/// Because it genuinely is not overwritten. [`RenderBuffers::into_output`]'s
/// colouring pass has **no `else` arm**: a pixel whose value is `NaN` and whose
/// bits are not [`RANGE_FOLDED_BITS`] is left exactly as the buffer delivered
/// it, and every render leaves most of the raster that way — the corners
/// outside the disc, the gaps between radials, the whole of a sweep that
/// paints nothing. `vec![0u8; n]` is what made those pixels transparent, and a
/// pooled buffer that skipped this would show the previous render's echoes
/// through the new one's empty sky.
fn checkout_image(len: usize) -> Vec<u8> {
    // Bound to a `let`, and deliberately **not** written as the scrutinee of
    // the `match` below. A guard produced inside a scrutinee temporary lives to
    // the end of the whole `match`, so `match image_pool().take() { .. }` would
    // hold the pool lock across the fallback allocation and the zero-fill.
    let taken = image_pool().take();
    match taken {
        Some(mut image) => {
            image.clear();
            image.resize(len, 0u8);
            image
        }
        None => vec![0u8; len],
    }
}

/// An empty value grid with the pool's capacity if it has one.
fn checkout_values() -> Vec<f32> {
    // See [`checkout_image`] for why this is a `let` and not a receiver.
    let taken = values_pool().take();
    let mut values = taken.unwrap_or_default();
    values.clear();
    values
}

/// Offer a finished RGBA texture back for the next plan-view render to draw
/// into.
///
/// Call it where the texture stops being needed — after it has been copied into
/// whatever the display layer holds — and not before. What arrives is *dead*:
/// this takes ownership, and the next render will overwrite every byte.
pub fn recycle_image(image: Vec<u8>) {
    if image.capacity() == 0 {
        return;
    }
    let mut pool = image_pool();
    if pool.is_none() {
        *pool = Some(image);
    }
}

/// Offer a finished value grid back. See [`recycle_image`], which this mirrors
/// exactly.
pub fn recycle_values(values: Vec<f32>) {
    if values.capacity() == 0 {
        return;
    }
    let mut pool = values_pool();
    if pool.is_none() {
        *pool = Some(values);
    }
}

impl RenderBuffers {
    fn new(product: types::RadarProduct, side_px: usize, shape: polar::PolarShape) -> Self {
        Self {
            cells: Self::checkout(side_px * side_px),
            polar: polar::PolarBuffers::new(shape),
            product,
        }
    }

    /// Take the pooled buffer resized to `n` cells, or build one if this is the
    /// first render or a second render is already holding it. See
    /// [`POOLED_CELLS`].
    ///
    /// The pool's invariant is that every cell it holds is [`Self::EMPTY`].
    /// [`Self::into_output`] is the only path that puts a buffer back and it
    /// establishes that.
    fn checkout(n: usize) -> Vec<AtomicU64> {
        // Bound to a `let`, and deliberately **not** written as the `match`
        // scrutinee. A guard produced in a scrutinee lives to the end of the
        // match, so `match Self::pool().take()` would hold the pool lock across
        // the fallback allocation below.
        let pooled = Self::pool().take();
        match pooled {
            // Carried buffers are kept only while they are near the size being
            // asked for. `resize_with` never returns capacity, so a slot that
            // once held the largest raster this device allows would hold that
            // allocation for the life of the process.
            Some(cells) if cells.len() <= n.saturating_mul(CELL_POOL_REUSE_FACTOR) => {
                let mut cells = cells;
                cells.resize_with(n, || AtomicU64::new(Self::EMPTY));
                cells
            }
            _ => (0..n).map(|_| AtomicU64::new(Self::EMPTY)).collect(),
        }
    }

    /// Offer a drained buffer back to the pool, keeping it only if the slot is
    /// free. See [`POOLED_CELLS`] for why the slot is one and not many.
    fn recycle(cells: Vec<AtomicU64>) {
        let mut pool = Self::pool();
        if pool.is_none() {
            *pool = Some(cells);
        }
    }

    /// The pool, with a poisoned lock read as a live one.
    ///
    /// **What the lock covers is one `Option::take` in [`Self::checkout`] and
    /// one `is_none` plus a move-assign in [`Self::recycle`], and nothing
    /// else.**
    fn pool() -> std::sync::MutexGuard<'static, Option<Vec<AtomicU64>>> {
        POOLED_CELLS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// No gate has claimed this pixel. Distinct from every real cell because
    /// [`write_key`] never yields 0.
    const EMPTY: u64 = 0;

    /// Pack a gate's claim. The key takes the high bits so `fetch_max` orders
    /// by it and not by the value riding along in the low ones.
    #[inline]
    fn cell(key: u32, value: f32) -> u64 {
        ((key as u64) << 32) | value.to_bits() as u64
    }

    /// Give `cell` the pixel if it outranks whatever holds it.
    #[inline]
    fn claim(&self, pixel_idx: usize, cell: u64) {
        self.cells[pixel_idx].fetch_max(cell, Ordering::Relaxed);
    }

    /// Pixels per colouring task. Big enough that rayon's per-task overhead
    /// vanishes against the palette lookups.
    const COLOR_CHUNK: usize = 16 * 1024;

    /// Split the cells into the RGBA texture and the value grid, give the
    /// drained buffer back to [`POOLED_CELLS`], and hand back the extent they
    /// were painted at so that whatever places the picture places it on the
    /// same ground the gates were projected onto.
    ///
    /// A carried buffer has to go back to the pool [`Self::EMPTY`] everywhere,
    /// or the next render inherits whatever pixels this one painted that it
    /// does not.
    fn into_output(self, extent_km: f64) -> SweepRender {
        let Self {
            mut cells,
            polar,
            product,
        } = self;
        let mut value_data = checkout_values();
        value_data.extend(cells.iter_mut().map(|a| {
            match std::mem::replace(a.get_mut(), Self::EMPTY) {
                Self::EMPTY => f32::NAN,
                cell => f32::from_bits(cell as u32),
            }
        }));
        Self::recycle(cells);
        let mut image = checkout_image(value_data.len() * 4);
        image
            .par_chunks_mut(4 * Self::COLOR_CHUNK)
            .zip(value_data.par_chunks_mut(Self::COLOR_CHUNK))
            .for_each(|(px, vals)| {
                for (px, v) in px.chunks_exact_mut(4).zip(vals) {
                    if !v.is_nan() {
                        let c = get_color_for_value(product, *v);
                        px.copy_from_slice(&[c.0, c.1, c.2, c.3]);
                    } else if v.to_bits() == RANGE_FOLDED_BITS {
                        let c = crate::palette::RANGE_FOLDED;
                        px.copy_from_slice(&[c.0, c.1, c.2, c.3]);
                        *v = f32::NAN;
                    }
                }
            });
        SweepRender {
            image,
            max_range_km: extent_km,
            values: value_data,
            polar: polar.into_field(),
            nyquist_ms: None,
            melting_layer_source: None,
            storm_motion: None,
        }
    }
}

/// The bit pattern a range-folded gate claims its pixel with.
///
/// The RDA reports three things about a gate and Level II encodes all three:
/// a value, "below threshold", and **range folded** — the gate's echo came back
/// from beyond the waveform's unambiguous range, so the radar has a return and
/// cannot say how far away it was.
const RANGE_FOLDED_BITS: u32 = 0x7FC0_0F1D;

/// The value a range-folded gate carries through the fill loop.
const RANGE_FOLDED_SENTINEL: f32 = f32::from_bits(RANGE_FOLDED_BITS);

/// One finished plan-view raster.
pub struct SweepRender {
    /// RGBA, `side²` pixels; the side is derivable from the length and
    /// deliberately not restated (see `crate::types::raster_side_px`).
    pub image: Vec<u8>,
    /// The half-width the raster was **projected** at, km — where its corners
    /// go on the ground, not how far the data reached. See
    /// [`render_with_projection`].
    pub max_range_km: f64,
    /// Per-pixel values, `f32::NAN` where nothing was painted **and** where a
    /// range-folded gate was (see [`RANGE_FOLDED_BITS`]).
    pub values: Vec<f32>,
    /// The gates behind those pixels, at the resolution the radar measured
    /// them, and the geometry to find one under a point.
    pub polar: polar::PolarField,
    /// Where the rendered sweep's cut declared its velocity folds, m/s.
    ///
    /// A property of the **sweep**, not of the product drawn from it.
    pub nyquist_ms: Option<f64>,
    /// Which melting layer the hybrid hydrometeor classification was computed
    /// against, or `None` for every other product — nothing else has one.
    pub melting_layer_source: Option<crate::hca::MeltingLayerSource>,
    /// The storm motion vector the storm-relative velocity was computed
    /// against, or `None` for every other product — nothing else applies one.
    pub storm_motion: Option<crate::srv::SrvMotion>,
}

impl SweepRender {
    /// Stamp the fold limit of the sweep this raster was drawn from.
    fn declaring(mut self, nyquist_ms: Option<f64>) -> Self {
        self.nyquist_ms = nyquist_ms;
        self
    }

    /// Stamp which melting layer the classification stood on.
    fn classified_against(mut self, source: crate::hca::MeltingLayerSource) -> Self {
        self.melting_layer_source = Some(source);
        self
    }

    /// Stamp the storm motion vector the storm-relative field was shifted by.
    fn moved_by(mut self, motion: crate::srv::SrvMotion) -> Self {
        self.storm_motion = Some(motion);
        self
    }
}

/// Which gate a claim came from.
#[derive(Clone, Copy)]
struct GateId {
    radial: usize,
    gate: usize,
}

/// Rank a gate's write the way a single-threaded, radial-major render would:
/// radial index first, gate index within it second. `fetch_max` over these is
/// order-independent, so the parallel result is the sequential one.
///
/// Never 0, so [`RenderBuffers::EMPTY`] stays unambiguous. Saturates: past
/// 65535 radials or 65534 gates some writes rank equally, which stays
/// deterministic (`fetch_max` is a set operation) but stops matching the
/// sequential order.
#[inline]
fn write_key(from: GateId) -> u32 {
    let r = from.radial.min(0xFFFF) as u32;
    let g = from.gate.min(0xFFFE) as u32;
    (r << 16) | (g + 1)
}

/// What the plan-view render paints for one decoded gate, or `None` where it
/// paints nothing at all.
pub fn painted_moment_value(value: nexrad_model::data::MomentValue) -> Option<f32> {
    use nexrad_model::data::MomentValue;
    match value {
        MomentValue::Value(v) if v < 999.0 => Some(v),
        MomentValue::Value(_) => None,
        MomentValue::RangeFolded => Some(RANGE_FOLDED_SENTINEL),
        MomentValue::BelowThreshold => None,
    }
}

/// One gate of `moment`, decoded, reached by indexing rather than by walking
/// every gate before it.
///
/// **Because `nth` on that iterator is a walk.** `iter()` is
/// `chunks_exact(word).map(..).map(..)`, and `Map` overrides `next`,
/// `size_hint`, `try_fold` and `fold` — not `nth`, and not `advance_by`. So
/// `nth(n)` falls through to the default, which is `next()` called `n + 1`
/// times, and *both* decode closures run on every gate skipped.
#[inline]
pub fn moment_value_at(
    moment: &nexrad_model::data::MomentData,
    gate: usize,
) -> Option<nexrad_model::data::MomentValue> {
    use nexrad_model::data::MomentValue;

    let bytes = moment.raw_values();
    // Anything other than 16 is one byte per gate, which is how the model's own
    // `raw_gate_values` reads it. `raw_values().len()` is authoritative for how
    // many gates there are, not `gate_count()`, for the same reason.
    let step = if moment.data_word_size() == 16 { 2 } else { 1 };
    let start = gate.checked_mul(step)?;
    let word = bytes.get(start..start.checked_add(step)?)?;

    #[cfg(test)]
    polar::note_gate_reads((word.len() / step) as u64);

    let raw = if step == 2 {
        u16::from_be_bytes([word[0], word[1]])
    } else {
        u16::from(word[0])
    };

    // The model's decode, from `MomentData::iter`, including the exact
    // `scale == 0.0` comparison: the value comes from a binary format where
    // IEEE 754 zero is stored literally, and a zero scale means the raw words
    // *are* the values, so 0 and 1 are ordinary numbers and not status codes.
    let scale = moment.scale();
    if scale == 0.0 {
        return Some(MomentValue::Value(raw as f32));
    }
    Some(match raw {
        0 => MomentValue::BelowThreshold,
        1 => MomentValue::RangeFolded,
        _ => MomentValue::Value((raw as f32 - moment.offset()) / scale),
    })
}

/// Which of `scan`'s sweeps a plan-view render of `product` at
/// `elevation_angle` would draw, by index.
pub fn sweep_index_for(
    scan: &Scan,
    product: types::RadarProduct,
    elevation_angle: f32,
) -> Option<usize> {
    let owner = find_sweep_owner(scan, product, elevation_angle)?;
    scan.sweeps().iter().position(|s| std::ptr::eq(s, owner))
}

/// How near a sweep's elevation has to sit to a requested one to count as it.
pub const ELEVATION_WINDOW: f64 = 0.1;

/// The available elevation angle (rounded to 0.1°) closest to
/// `target_elevation` that carries this product. The loop renderer uses it to
/// snap the selected elevation to what each historical scan actually holds.
pub fn find_closest_elevation(
    scan: &Scan,
    product: types::RadarProduct,
    target_elevation: f32,
) -> Option<f32> {
    scan.sweeps()
        .iter()
        .filter_map(|sweep| {
            let radials = sweep.radials();
            let elevation = crate::volumetric::sweep_elevation_deg(radials)?;
            let rounded = (elevation * 10.0).round() as f32 / 10.0;
            radials
                .iter()
                .any(|r| product.get_moment(r).is_some())
                .then_some(rounded)
        })
        .min_by(|a, b| ((*a - target_elevation).abs()).total_cmp(&((*b - target_elevation).abs())))
}

/// Find the newest sweep in `elevation_angle`'s tilt *family* that carries
/// the requested product's moment data.
///
/// Searched newest-first: SAILS volumes carry several cuts of the low tilts,
/// minutes apart, and the last one in the scan is the most recent. The
/// reference display shows the newest cut too — cursor samples of its NROT
/// correlate at 0.95 with the matching cut and near zero with the stale ones.
///
/// Sweeps are compared on [`crate::volumetric::sweep_elevation_deg`] — the
/// **median** of the sweep's radials — and the window is a tight 0.1°.
///
/// Within the family, non-Doppler products prefer the newest sweep *without*
/// a velocity moment: a split cut's Doppler half repeats a short-range copy
/// of the surveillance moments, and the reference display draws reflectivity
/// from the surveillance half.
/// Upper tilts are single merged cuts carrying everything, so the preference
/// falls back to any sweep with the product's moment.
pub(crate) fn find_sweep(
    scan: &Scan,
    product: types::RadarProduct,
    elevation_angle: f32,
) -> Option<&[Radial]> {
    find_sweep_owner(scan, product, elevation_angle).map(nexrad_model::data::Sweep::radials)
}

/// [`find_sweep`], answering the `Sweep` rather than its radials.
///
/// **Every radial, not the first one.** Whether a sweep carries a product, and
/// whether it is a split cut's Doppler half, are properties of the *sweep*, and
/// asking them of `radials.first()` let one radial answer for all 720 of them.
///
/// **The two questions do not take the same answer, though.** "Does this sweep
/// carry the product" is `any`, because one surviving radial is proof that it
/// does. "Is this the split cut's Doppler half" is not: `any` there lets one
/// stray velocity radial declare a *surveillance* cut to be the Doppler one,
/// which hides it from the preference below.
pub(crate) fn find_sweep_owner(
    scan: &Scan,
    product: types::RadarProduct,
    elevation_angle: f32,
) -> Option<&nexrad_model::data::Sweep> {
    let newest = |surveillance_only: bool| {
        scan.sweeps().iter().rev().find(|sweep| {
            let radials = sweep.radials();
            crate::volumetric::sweep_elevation_deg(radials)
                .map(|elevation| {
                    (elevation - f64::from(elevation_angle)).abs() < ELEVATION_WINDOW
                        && radials.iter().any(|r| product.get_moment(r).is_some())
                        && !(surveillance_only && sweep_carries_velocity(radials))
                })
                .unwrap_or(false)
        })
    };
    match product {
        types::RadarProduct::Velocity
        | types::RadarProduct::SpectrumWidth
        | types::RadarProduct::NormalizedRotation
        | types::RadarProduct::StormRelativeVelocity => newest(false),
        _ => newest(true).or_else(|| newest(false)),
    }
}

/// Whether a sweep is a split cut's **Doppler half** — most of its radials
/// carry velocity.
///
/// A majority rather than `any` or `all`, because the question is about the
/// sweep and both extremes let one radial decide it.
fn sweep_carries_velocity(radials: &[Radial]) -> bool {
    let carrying = radials.iter().filter(|r| r.velocity().is_some()).count();
    carrying * 2 > radials.len()
}

/// The widest wedge any radial is ever painted at, degrees.
///
/// 2.0° caps every such pathology while touching nothing real: Level II
/// declares 0.5° or 1.0° and nothing else, the RDA has no third resolution, so
/// no sweep this display has ever drawn comes within a factor of two of the
/// cap.
const MAX_WEDGE_DEG: f64 = 2.0;

/// How wide to paint one Level II radial, degrees, given what it declares and
/// what the sweep around it measures.
///
/// A radial declares its own azimuth resolution on the wire and has since
/// Message 31 — 0.5° for a super-res cut, 1.0° otherwise — and that declaration
/// is the honest answer to "how much sky does this sample stand for".
///
/// This is the **floor** under a wedge and no longer the whole of it:
/// [`l2_wedge_half_widths_deg`] finishes the answer.
fn l2_wedge_width_deg(declared_deg: f64, median_step_deg: f64) -> f64 {
    let base = if declared_deg > 0.0 {
        declared_deg
    } else {
        median_step_deg
    };
    base.min(crate::azimuth::MAX_ADJACENT_GAP_STEPS * median_step_deg)
        .min(MAX_WEDGE_DEG)
}

/// Half the sky each radial of a sweep is painted over, degrees — one per
/// radial, in the order they were handed in.
///
/// A radial's sample stands for the sky from halfway to the radial before it to
/// halfway to the radial after — because that sky *was measured*, by whichever
/// of the two was dwelling on it while the antenna crossed it. The declared
/// width stays as a floor, so no wedge is narrower than it is today, and the
/// reach is held under [`crate::azimuth::MAX_ADJACENT_GAP_STEPS`], which is
/// already this crate's statement of how far past a sweep's own spacing a
/// consumer may reach before it is inventing coverage.
fn l2_wedge_half_widths_deg(
    azimuths_deg: &[f64],
    declared_deg: &[f64],
    median_step_deg: f64,
) -> Vec<f64> {
    let n = azimuths_deg.len();
    let base: Vec<f64> = (0..n)
        .map(|i| l2_wedge_width_deg(declared_deg[i], median_step_deg))
        .collect();
    let mut half: Vec<f64> = base.iter().map(|w| w / 2.0).collect();
    if n < 2 {
        return half;
    }
    // Azimuth order, not the order the radials arrived in: a sweep is stored
    // as it was collected, so a cut that begins mid-circle wraps, and a
    // radial's neighbours are its neighbours *in azimuth*.
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|a, b| azimuths_deg[*a].total_cmp(&azimuths_deg[*b]));
    for k in 0..n {
        let i = order[k];
        let prev = azimuths_deg[order[(k + n - 1) % n]];
        let next = azimuths_deg[order[(k + 1) % n]];
        // `rem_euclid` closes the circle, so the first and last radials in
        // azimuth order are neighbours like any other pair.
        let reach = crate::azimuth::MAX_ADJACENT_GAP_STEPS * base[i];
        for gap in [
            (azimuths_deg[i] - prev).rem_euclid(360.0),
            (next - azimuths_deg[i]).rem_euclid(360.0),
        ] {
            if gap <= reach {
                half[i] = half[i].max(gap / 2.0);
            }
        }
    }
    half
}

/// How wide to paint one row of a derived polar grid, degrees.
///
/// NROT, SRV and KDP are computed onto grids of their own, and a grid row
/// carries an azimuth but no declared resolution — the declaration belongs to
/// the radial the row was computed *from*, and deliberately is not threaded
/// through, since a derived value spans whatever its input span was rather than
/// one radial's. So these rows are painted at the sweep's measured median step,
/// capped by [`MAX_WEDGE_DEG`] for the two-radial reason given there.
fn derived_grid_wedge_deg(azimuths_deg: &[f64]) -> f64 {
    crate::azimuth::median_azimuth_step_deg(azimuths_deg.iter().copied())
        .unwrap_or(1.0)
        .min(MAX_WEDGE_DEG)
}

/// [`derived_grid_wedge_deg`] finished the way
/// [`l2_wedge_half_widths_deg`] finishes a sweep's: half-widths that meet
/// where the rows wobble, and stop where one is missing.
fn derived_grid_half_widths_deg(azimuths_deg: &[f64]) -> Vec<f64> {
    let median =
        crate::azimuth::median_azimuth_step_deg(azimuths_deg.iter().copied()).unwrap_or(1.0);
    let declared = vec![derived_grid_wedge_deg(azimuths_deg); azimuths_deg.len()];
    l2_wedge_half_widths_deg(azimuths_deg, &declared, median)
}

/// How far apart two radials' reaches may be before the sweep is reported as
/// disagreeing with itself, km.
///
/// Within one cut the RDA declares one gate count per moment, and real volumes
/// hold to it exactly: walked over 102 sweeps and all six moments of the
/// opening volume at KTLX, KDMX, KMPX, KAMX, KFTG and TJUA — plains, upper
/// midwest, coastal, mountain, tropical — every sweep's radials agreed on their
/// reach to the last bit, spread **0.000 km** everywhere.
const RADIAL_REACH_DISAGREEMENT_KM: f64 = 1.0;

/// How far the sweep's data actually reaches, km: the **greatest** reach among
/// the radials carrying this product's moment, or 0 if none do.
fn compute_max_range(radials: &[Radial], product: types::RadarProduct) -> f64 {
    let mut reach = f64::NEG_INFINITY;
    let mut shortest = f64::INFINITY;
    for radial in radials {
        let Some(moment) = product.get_moment(radial) else {
            continue;
        };
        let km = moment.first_gate_range_km()
            + f64::from(moment.gate_count()) * moment.gate_interval_km();
        reach = reach.max(km);
        shortest = shortest.min(km);
    }
    if reach == f64::NEG_INFINITY {
        return 0.0;
    }
    if reach - shortest > RADIAL_REACH_DISAGREEMENT_KM {
        log::warn!(
            "{product:?}: this sweep's radials do not agree on how far they reach — \
             {shortest:.1}km to {reach:.1}km, a spread of {:.1}km; rendering to the \
             longest",
            reach - shortest
        );
    }
    reach
}

/// How far apart this sweep's gates are along a radial, km — the **finest**
/// spacing any radial carrying the product declares.
///
/// Zero when no radial carries the product, which is the same answer
/// [`compute_max_range`] gives for that sweep and which
/// [`types::data_limited_side_px`] reads as "this says nothing about sampling".
fn compute_gate_interval_km(radials: &[Radial], product: types::RadarProduct) -> f64 {
    let mut finest = f64::INFINITY;
    for radial in radials {
        let Some(moment) = product.get_moment(radial) else {
            continue;
        };
        let km = moment.gate_interval_km();
        if km > 0.0 {
            finest = finest.min(km);
        }
    }
    if finest.is_finite() { finest } else { 0.0 }
}

/// Where a sweep's gates start and how many of them there are, for the product
/// being drawn — the two halves of [`polar::PolarShape`] that
/// [`compute_max_range`] and [`compute_gate_interval_km`] do not already answer.
///
/// Both taken the way those two take theirs: the **nearest** first gate and the
/// **most** gates any radial carrying the product declares, so the polar field
/// is at least as large as every radial the fill will walk. A shape smaller
/// than the walk loses the tail of a radial's readout, which `PolarBuffers::
/// paint` will `debug_assert` on but cannot recover from.
///
/// Zeroes where no radial carries the product, which is the same answer its two
/// neighbours give for that sweep.
fn compute_gate_span(radials: &[Radial], product: types::RadarProduct) -> (f64, usize) {
    let mut first_km = f64::INFINITY;
    let mut gates = 0usize;
    for radial in radials {
        let Some(moment) = product.get_moment(radial) else {
            continue;
        };
        first_km = first_km.min(moment.first_gate_range_km());
        gates = gates.max(moment.gate_count() as usize);
    }
    (if first_km.is_finite() { first_km } else { 0.0 }, gates)
}

/// The polar shape of a 1° × 1 km volume grid — the layout
/// [`crate::volumetric`] computes echo tops, VIL density and the hail pair
/// onto, and the one three of the paths below share.
///
/// Gate 0's centre sits half a bin out because that is where those fills paint
/// it: each calls `render_gate` at `r + 0.5` for row index `r`, so the first
/// bin spans `[0, 1)` km and is centred at 0.5.
fn volume_grid_shape(rows: usize, range_bins: usize) -> polar::PolarShape {
    polar::PolarShape {
        radials: rows,
        gates: range_bins,
        first_gate_slant_km: crate::volumetric::RANGE_BIN_KM / 2.0,
        gate_interval_slant_km: crate::volumetric::RANGE_BIN_KM,
        elevation_deg: None,
    }
}

/// The factor between the slant range a sweep's gates are measured at and the
/// ground range they sit over: `cos e` of the sweep's **median** elevation.
///
/// The ground range a gate sits over is the spherical arc
/// `Rₑ·asin(r·cos e/(Rₑ + h))`, and `cos e` is its small-angle limit with the
/// curvature term dropped.
///
/// Two answers are not the median's. `None` is an empty sweep, which paints
/// nothing whichever factor it is handed. A non-finite median is a corrupt
/// angle, and 1.0 draws that sweep where the RDA said it was measured, which
/// is a better failure than `cos NaN` collapsing all of it onto the site.
fn sweep_elevation_deg_or_flat(radials: &[Radial]) -> f64 {
    match crate::volumetric::sweep_elevation_deg(radials) {
        Some(e) if e.is_finite() => e,
        // A sweep that will not say where it pointed is drawn where it was
        // measured rather than collapsed onto the site, which is what a zero
        // elevation gives: the arc is the identity to within 8 nm at 460 km.
        _ => 0.0,
    }
}

/// The **mean** foreshortening over a radial that reaches `slant_reach_km`: the
/// ground arc it ends at, divided by the slant range it took to get there.
///
/// Gate positions do not come through here. They come from
/// [`gate_ground_edges`], which evaluates the arc per boundary.
fn sweep_ground_factor(slant_reach_km: f64, elevation_deg: f64) -> f64 {
    if slant_reach_km <= 0.0 {
        return 1.0;
    }
    (crate::beam::ground_range_km(slant_reach_km, elevation_deg) / slant_reach_km).clamp(0.0, 1.0)
}

/// How far a field's samples go along a radial and how far apart they are, both
/// in the ground coordinate the caller paints in.
#[derive(Clone, Copy)]
struct FieldRadial {
    /// How far the outermost sample reaches, km. `0.0` where no radial carries
    /// the product, which [`types::plan_view_extent_km`] reads as "a picture of
    /// nothing" and answers with the fallback extent.
    reach_km: f64,
    /// The distance between two consecutive samples along a radial, km. A
    /// non-positive figure says nothing about sampling, and
    /// [`types::data_limited_side_px`] answers it with the display's calibrated
    /// scale rather than dividing by it.
    sample_km: f64,
    /// The polar layout the fill will walk — how many radials, how many gates
    /// each, and where gate 0 sits.
    shape: polar::PolarShape,
}

/// Project a field onto the image, at the extent its own data asks for.
///
/// So the returned figure is the **extent of the picture**, not the reach of
/// the data. Below the floor those differ — a 40 km Level III product is drawn
/// on a 230 km frame — and the picture's extent is the one a consumer can do
/// anything with: it is what says where the corners of the texture go and
/// which pixel a hover lands in.
///
/// `reach_km` is measured in **the coordinate its caller paints in**, which
/// for the four per-tilt paths is a ground range: they have already folded
/// [`sweep_ground_factor`] into both the reach and every gate.
///
/// `side_ceiling_px` is the largest side the caller will accept; the extent,
/// that ceiling and `sample_km` together give the raster's own side through
/// [`types::raster_side_px`], which is the second half of the geometry and the
/// only half this crate cannot decide alone.
///
/// `sample_km` is how far apart this field's samples are along a radial, in the
/// same ground coordinate `reach_km` is in — so the four per-tilt paths fold
/// [`sweep_ground_factor`] into it exactly as they fold it into the reach.
fn render_with_projection(
    radar_lat: f64,
    radar_lon: f64,
    field: FieldRadial,
    product: types::RadarProduct,
    side_ceiling_px: usize,
    label: &str,
    fill: impl FnOnce(&MercatorProjection, &RenderBuffers),
) -> SweepRender {
    let FieldRadial {
        reach_km,
        sample_km,
        shape,
    } = field;
    let extent_km = types::plan_view_extent_km(reach_km);
    let side_px = types::raster_side_px(extent_km, side_ceiling_px, sample_km);
    let bounds = types::ImageBounds::from_radar_site(radar_lat, radar_lon, extent_km);
    let proj = MercatorProjection::from_bounds(radar_lat, &bounds, extent_km, side_px);
    let bufs = RenderBuffers::new(product, side_px, shape);

    fill(&proj, &bufs);

    let output = bufs.into_output(extent_km);
    log::info!(
        "{} rendering complete: data reaches {:.1}km, projected at ±{:.1}km \
         onto {side_px}² px ({:.2} px/km)",
        label,
        reach_km,
        output.max_range_km,
        proj.px_per_km,
    );
    output
}

/// Render radar data to an image projected for geographic display; see
/// [`SweepRender`] for what comes back.
///
/// The volume declares nothing, so the velocity products' dealiaser estimates
/// its fold limit off the sweep and [`SweepRender::nyquist_ms`] is `None` — the
/// answer for a caller holding only model types, which is every caller of this
/// short form. [`render_radar_to_image_full`] takes the table.
pub fn render_radar_to_image(
    data: &Scan,
    elevation_angle: f32,
    product: types::RadarProduct,
    radar_lat: f64,
    radar_lon: f64,
) -> Option<SweepRender> {
    render_radar_to_image_full(
        data,
        elevation_angle,
        product,
        radar_lat,
        radar_lon,
        crate::srv::MotionInputs::default(),
        None,
        None,
        &crate::nyquist::DeclaredNyquist::empty(),
    )
}

/// [`render_radar_to_image`] from a [`RenderInput`] instead of a `Scan`.
pub fn render_from(input: &crate::render_input::RenderInput) -> Option<SweepRender> {
    render_from_sized(input, types::IMAGE_SIZE)
}

/// [`render_from`] at a caller-chosen side ceiling.
pub fn render_from_sized(
    input: &crate::render_input::RenderInput,
    side_ceiling_px: usize,
) -> Option<SweepRender> {
    render_radar_to_image_full_sized(
        &input.to_scan(),
        input.elevation(),
        input.product(),
        input.radar_lat(),
        input.radar_lon(),
        input.storm_motion(),
        input.env_heights_km_msl(),
        input.melting_layer_product().map(|o| o.as_slice()),
        &input.declared_nyquist(),
        side_ceiling_px,
    )
}

/// [`render_radar_to_image`] plus the two render parameters: the storm
/// motion override, in knots and degrees-from — read by storm-relative
/// velocity alone; `None` is "no override" and SRV applies the next rung of
/// its chain ([`crate::srv`]) — and
/// the environmental 0 °C / −20 °C heights in km MSL, read by the products
/// [`types::RadarProduct::reads_env_heights`] names: the hail pair, whose
/// field is undefined without them so `None` renders nothing
/// ([`crate::hail`]), and the hybrid classification, which answers `None` by
/// falling back to the operational adaptation defaults and so draws a
/// *different* picture rather than no picture ([`render_hhc_to_image`], 30
/// lines below).
///
/// The environmental wind profile NROT's and SRV's dealiasers seed from is
/// not a parameter: it is fit from the volume's own velocity tilts
/// ([`crate::velocity::volume_wind_profile`]).
/// `declared_nyquist` is what each cut said about where its velocity folds
/// ([`crate::nyquist::DeclaredNyquist`]). NROT and SRV dealias, and this is the
/// interval they fold around; the sweep's own value also comes back in
/// [`SweepRender::nyquist_ms`]. Pass an empty table for a volume that declared
/// nothing and the dealiaser estimates.
#[allow(clippy::too_many_arguments)]
pub fn render_radar_to_image_full(
    data: &Scan,
    elevation_angle: f32,
    product: types::RadarProduct,
    radar_lat: f64,
    radar_lon: f64,
    motion: crate::srv::MotionInputs,
    env_heights_km_msl: Option<(f64, f64)>,
    melting_layer_product: Option<&[u8]>,
    declared_nyquist: &crate::nyquist::DeclaredNyquist,
) -> Option<SweepRender> {
    render_radar_to_image_full_sized(
        data,
        elevation_angle,
        product,
        radar_lat,
        radar_lon,
        motion,
        env_heights_km_msl,
        melting_layer_product,
        declared_nyquist,
        types::IMAGE_SIZE,
    )
}

/// [`render_radar_to_image_full`] at a caller-chosen side ceiling. See
/// [`types::raster_side_px`] for what a ceiling is and why the caller owns it.
#[allow(clippy::too_many_arguments)]
pub fn render_radar_to_image_full_sized(
    data: &Scan,
    elevation_angle: f32,
    product: types::RadarProduct,
    radar_lat: f64,
    radar_lon: f64,
    motion: crate::srv::MotionInputs,
    env_heights_km_msl: Option<(f64, f64)>,
    melting_layer_product: Option<&[u8]>,
    declared_nyquist: &crate::nyquist::DeclaredNyquist,
    side_ceiling_px: usize,
) -> Option<SweepRender> {
    if product == types::RadarProduct::EchoTopsInterpolated {
        return render_echo_tops_interp_to_image(data, radar_lat, radar_lon, side_ceiling_px);
    }

    if matches!(
        product,
        types::RadarProduct::ProbabilityOfSevereHail | types::RadarProduct::MaxExpectedHailSize
    ) {
        return render_hail_to_image(
            data,
            product,
            radar_lat,
            radar_lon,
            env_heights_km_msl,
            side_ceiling_px,
        );
    }

    if product == types::RadarProduct::HydrometeorClassification {
        return render_hhc_to_image(
            data,
            radar_lat,
            radar_lon,
            env_heights_km_msl,
            melting_layer_product,
            side_ceiling_px,
        );
    }

    // The owner and not just its radials: the declared Nyquist table is keyed
    // by the RDA's `elevation_number`, and a `Sweep` is where that number is
    // authoritative.
    let owner = find_sweep_owner(data, product, elevation_angle)?;
    let radials = owner.radials();
    let nyquist_ms = declared_nyquist.get(owner.elevation_number());

    if product == types::RadarProduct::NormalizedRotation {
        return render_nrot_to_image(
            data,
            radials,
            radar_lat,
            radar_lon,
            nyquist_ms,
            side_ceiling_px,
        );
    }

    if product == types::RadarProduct::StormRelativeVelocity {
        return render_srv_to_image(
            data,
            radials,
            radar_lat,
            radar_lon,
            motion,
            nyquist_ms,
            side_ceiling_px,
        );
    }

    // The stand-in for an unmeasurable sweep is 1.0° because that is the
    // coarser of the two resolutions the RDA has, so a sweep too degenerate to
    // measure is painted as if it were the wider one rather than as a spoke.
    let median_step = crate::azimuth::median_azimuth_step_deg(
        radials.iter().map(|r| f64::from(r.azimuth_angle_degrees())),
    )
    .unwrap_or(1.0);
    // Slant out of `compute_max_range`, ground into the projection: how far
    // the *data* goes is a property of the sweep, how wide the *picture* is
    // has to be the ground it covers.
    let sweep_elevation = sweep_elevation_deg_or_flat(radials);
    let slant_reach_km = compute_max_range(radials, product);
    let cos_e = sweep_ground_factor(slant_reach_km, sweep_elevation);
    let ground_reach_km = slant_reach_km * cos_e;
    let ground_sample_km = compute_gate_interval_km(radials, product) * cos_e;
    let (first_gate_slant_km, gate_count) = compute_gate_span(radials, product);

    // Half-widths for the whole sweep at once, because each one depends on
    // where its neighbours in azimuth landed; see `l2_wedge_half_widths_deg`.
    let azimuths_deg: Vec<f64> = radials
        .iter()
        .map(|r| f64::from(r.azimuth_angle_degrees()))
        .collect();
    let declared_deg: Vec<f64> = radials
        .iter()
        .map(|r| f64::from(r.azimuth_spacing_degrees()))
        .collect();
    let half_widths = l2_wedge_half_widths_deg(&azimuths_deg, &declared_deg, median_step);
    let output = render_with_projection(
        radar_lat,
        radar_lon,
        FieldRadial {
            reach_km: ground_reach_km,
            sample_km: ground_sample_km,
            shape: polar::PolarShape {
                radials: radials.len(),
                gates: gate_count,
                first_gate_slant_km,
                gate_interval_slant_km: compute_gate_interval_km(radials, product),
                elevation_deg: Some(sweep_elevation),
            },
        },
        product,
        side_ceiling_px,
        "Radar",
        |proj, bufs| {
            radials
                .par_iter()
                .enumerate()
                .for_each(|(radial_idx, radial)| {
                    let azimuth = radial.azimuth_angle_degrees() as f64;
                    let ctx = RadialContext::new(azimuth, half_widths[radial_idx]);

                    if let Some(moment) = product.get_moment(radial) {
                        let first_gate_range = moment.first_gate_range_km();
                        let gate_size = moment.gate_interval_km();

                        let edges = gate_ground_edges(
                            first_gate_range,
                            gate_size,
                            moment.gate_count() as usize,
                            |slant_km| crate::beam::ground_range_km(slant_km, sweep_elevation),
                        );
                        // `iter`, not `values`: the latter is `iter().collect()`
                        // and this walk is strictly sequential.
                        for ((gate_idx, moment_value), span) in moment.iter().enumerate().zip(edges)
                        {
                            // The gate's **inner** edge, because a gate that
                            // starts outside the picture is the first one with
                            // nothing to contribute; the conversion is monotone,
                            // so the break still short-circuits.
                            if span.near_km > proj.extent_km {
                                break;
                            }

                            let Some(scaled_value) = painted_moment_value(moment_value) else {
                                continue;
                            };

                            let from = GateId {
                                radial: radial_idx,
                                gate: gate_idx,
                            };
                            proj.render_gate(bufs, &ctx, span, scaled_value, from);
                        }
                    }
                });
        },
    );
    Some(output.declaring(nyquist_ms))
}

/// Render NROT (Normalized Rotation): azimuthal shear derived from Level II
/// velocity, normalized by range to remove beam broadening and scaled to a
/// unitless field where >1.0 is significant and >2.5 extreme.
fn render_nrot_to_image(
    scan: &Scan,
    radials: &[Radial],
    radar_lat: f64,
    radar_lon: f64,
    declared_nyquist_ms: Option<f64>,
    side_ceiling_px: usize,
) -> Option<SweepRender> {
    let num_radials = radials.len();
    if num_radials < 3 {
        return None;
    }

    let vg = crate::velocity::grid(radials)?;

    let sweep_elevation = sweep_elevation_deg_or_flat(radials);
    let slant_reach_km = vg.first_gate_range_km + vg.gate_count as f64 * vg.gate_interval_km;
    let cos_e = sweep_ground_factor(slant_reach_km, sweep_elevation);
    let ground_reach_km = slant_reach_km * cos_e;
    let half_widths = derived_grid_half_widths_deg(&vg.azimuths_deg);

    // The physics keeps the first radial's angle. It is the same number the
    // shear normalization has always divided by, and the two are not
    // interchangeable: `sweep_ground_factor`'s median is where the sweep is
    // *drawn*, this is what the sweep was *computed* at.
    let elevation_deg = radials
        .first()
        .map(|r| r.elevation_angle_degrees() as f64)
        .unwrap_or(0.5);
    let profile = crate::velocity::volume_wind_profile(scan);
    let nrot_grid = crate::nrot::compute_nrot_grid_with_profile(
        &vg.sweep(declared_nyquist_ms),
        elevation_deg,
        profile.as_ref(),
    );

    let output = render_with_projection(
        radar_lat,
        radar_lon,
        FieldRadial {
            reach_km: ground_reach_km,
            sample_km: vg.gate_interval_km * cos_e,
            shape: polar::PolarShape {
                radials: nrot_grid.len(),
                gates: nrot_grid.iter().map(Vec::len).max().unwrap_or(0),
                first_gate_slant_km: vg.first_gate_range_km,
                gate_interval_slant_km: vg.gate_interval_km,
                elevation_deg: Some(sweep_elevation),
            },
        },
        types::RadarProduct::NormalizedRotation,
        side_ceiling_px,
        "NROT",
        |proj, bufs| {
            nrot_grid.par_iter().enumerate().for_each(|(i, nrot_row)| {
                let ctx = RadialContext::new(vg.azimuths_deg[i], half_widths[i]);

                let edges = gate_ground_edges(
                    vg.first_gate_range_km,
                    vg.gate_interval_km,
                    nrot_row.len(),
                    |slant_km| crate::beam::ground_range_km(slant_km, sweep_elevation),
                );
                for ((j, &nrot_val), span) in nrot_row.iter().enumerate().zip(edges) {
                    if nrot_val.is_nan() {
                        continue;
                    }

                    if span.near_km > proj.extent_km {
                        break;
                    }

                    // Sub-threshold shear must not claim the pixel at all, or
                    // it would outrank a real return from a lower radial.
                    // `into_output` would colour it transparent either way, so
                    // this has to happen here, not there.
                    let scaled_value = nrot_val as f32;
                    let color =
                        get_color_for_value(types::RadarProduct::NormalizedRotation, scaled_value);
                    if color.3 == 0 {
                        continue;
                    }

                    let from = GateId { radial: i, gate: j };
                    proj.render_gate(bufs, &ctx, span, scaled_value, from);
                }
            });
        },
    );
    Some(output.declaring(declared_nyquist_ms))
}

/// Render storm-relative velocity derived locally from Level II: the sweep's
/// velocity dealiased under the Coverage profile, plus the storm-motion
/// correction — a user override when one is set, otherwise the RPG's own
/// vector for the volume, otherwise the derived rung the reader chose (the
/// 0–6 km mean wind by default). Values are m/s, like every
/// Level II velocity field, so the palette and `format_value` read them
/// unchanged. See [`crate::srv`].
///
/// `None` when no vector exists at all — no override and a wind profile too
/// hollow for even the mean-wind fallback.
#[allow(clippy::too_many_arguments)]
fn render_srv_to_image(
    scan: &Scan,
    radials: &[Radial],
    radar_lat: f64,
    radar_lon: f64,
    motion: crate::srv::MotionInputs,
    declared_nyquist_ms: Option<f64>,
    side_ceiling_px: usize,
) -> Option<SweepRender> {
    if radials.len() < 3 {
        return None;
    }
    let elevation_deg = radials
        .first()
        .map(|r| r.elevation_angle_degrees() as f64)
        .unwrap_or(0.5);
    let profile = crate::velocity::volume_wind_profile(scan);
    // The RPG's vector inside `motion` is already paired to this volume by the
    // caller — see `render_dispatch::rpg_storm_motion_for`.
    let motion = motion.resolve(profile.as_ref())?;
    log::info!(
        "SRV {elevation_deg:.1}°: {:.1} kt from {:.1}° ({:?})",
        motion.speed_kt,
        motion.direction_deg,
        motion.source,
    );
    let grid = crate::srv::compute_srv_grid(
        radials,
        elevation_deg,
        profile.as_ref(),
        &motion,
        declared_nyquist_ms,
    )?;

    // As in NROT: the dealiaser above keeps the first radial's angle, this is
    // where the finished field is *placed*.
    let sweep_elevation = sweep_elevation_deg_or_flat(radials);
    let slant_reach_km = grid.first_gate_range_km + grid.gate_count as f64 * grid.gate_interval_km;
    let cos_e = sweep_ground_factor(slant_reach_km, sweep_elevation);
    let ground_reach_km = slant_reach_km * cos_e;
    let half_widths = derived_grid_half_widths_deg(&grid.azimuths_deg);
    let output = render_with_projection(
        radar_lat,
        radar_lon,
        FieldRadial {
            reach_km: ground_reach_km,
            sample_km: grid.gate_interval_km * cos_e,
            shape: polar::PolarShape {
                radials: grid.values.len(),
                gates: grid.values.iter().map(Vec::len).max().unwrap_or(0),
                first_gate_slant_km: grid.first_gate_range_km,
                gate_interval_slant_km: grid.gate_interval_km,
                elevation_deg: Some(sweep_elevation),
            },
        },
        types::RadarProduct::StormRelativeVelocity,
        side_ceiling_px,
        "SRV",
        |proj, bufs| {
            grid.values.par_iter().enumerate().for_each(|(i, row)| {
                let ctx = RadialContext::new(grid.azimuths_deg[i], half_widths[i]);
                let edges = gate_ground_edges(
                    grid.first_gate_range_km,
                    grid.gate_interval_km,
                    row.len(),
                    |slant_km| crate::beam::ground_range_km(slant_km, sweep_elevation),
                );
                for ((j, &value), span) in row.iter().enumerate().zip(edges) {
                    if value.is_nan() {
                        continue;
                    }
                    if span.near_km > proj.extent_km {
                        break;
                    }
                    let from = GateId { radial: i, gate: j };
                    proj.render_gate(bufs, &ctx, span, value as f32, from);
                }
            });
        },
    );
    Some(output.declaring(declared_nyquist_ms).moved_by(motion))
}

/// Render interpolated echo tops: the whole reflectivity volume reduced to a
/// 1° × 1 km polar grid of threshold-crossing heights, painted with the echo
/// tops palette. Tilt-independent — every elevation request renders the same
/// volume product.
pub fn render_echo_tops_interp_to_image(
    scan: &Scan,
    radar_lat: f64,
    radar_lon: f64,
    side_ceiling_px: usize,
) -> Option<SweepRender> {
    let grid = crate::volumetric::compute_echo_tops(scan);
    let max_range = grid.range_bins as f64;
    let output = render_with_projection(
        radar_lat,
        radar_lon,
        FieldRadial {
            reach_km: max_range,
            sample_km: crate::volumetric::RANGE_BIN_KM,
            shape: volume_grid_shape(grid.values.len(), grid.range_bins),
        },
        types::RadarProduct::EchoTopsInterpolated,
        side_ceiling_px,
        "Radar",
        |proj, bufs| {
            grid.values.par_iter().enumerate().for_each(|(az, row)| {
                let ctx = RadialContext::new(az as f64 + 0.5, 0.5);
                for (r, v) in row.iter().enumerate() {
                    if v.is_nan() {
                        continue;
                    }
                    let from = GateId {
                        radial: az,
                        gate: r,
                    };
                    let span = GateSpan {
                        near_km: r as f64,
                        far_km: r as f64 + 1.0,
                    };
                    proj.render_gate(bufs, &ctx, span, *v, from);
                }
            });
        },
    );
    Some(output)
}

/// Render VIL density from the RPG's own two published products for one
/// volume — Digital VIL (134) over Enhanced Echo Tops (135), see
/// [`crate::vild`] — as a 1° × 1 km polar grid in g/m³ painted with the
/// VIL-density palette.
///
/// `None` where the pair cannot make a field — a mismatched volume above all,
/// which is refused rather than painted ([`crate::vild::Refusal`]).
pub fn render_derived_vild_to_image(
    dvl: &nexrad_level3::model::Level3Message,
    eet: &nexrad_level3::model::Level3Message,
    radar_lat: f64,
    radar_lon: f64,
) -> Option<SweepRender> {
    render_derived_vild_to_image_sized(dvl, eet, radar_lat, radar_lon, types::IMAGE_SIZE)
}

/// [`render_derived_vild_to_image`] at a caller-chosen side ceiling.
pub fn render_derived_vild_to_image_sized(
    dvl: &nexrad_level3::model::Level3Message,
    eet: &nexrad_level3::model::Level3Message,
    radar_lat: f64,
    radar_lon: f64,
    side_ceiling_px: usize,
) -> Option<SweepRender> {
    let grid = match crate::vild::compute_vild(dvl, eet) {
        Ok(grid) => grid,
        Err(refusal) => {
            log::info!("VIL density: nothing to render — {refusal:?}");
            return None;
        }
    };
    let max_range = grid.range_bins as f64;
    let output = render_with_projection(
        radar_lat,
        radar_lon,
        FieldRadial {
            reach_km: max_range,
            sample_km: crate::volumetric::RANGE_BIN_KM,
            shape: volume_grid_shape(grid.values.len(), grid.range_bins),
        },
        types::RadarProduct::VilDensity,
        side_ceiling_px,
        "Radar",
        |proj, bufs| {
            grid.values.par_iter().enumerate().for_each(|(az, row)| {
                let ctx = RadialContext::new(az as f64 + 0.5, 0.5);
                for (r, v) in row.iter().enumerate() {
                    if v.is_nan() {
                        continue;
                    }
                    let from = GateId {
                        radial: az,
                        gate: r,
                    };
                    let span = GateSpan {
                        near_km: r as f64,
                        far_km: r as f64 + 1.0,
                    };
                    proj.render_gate(bufs, &ctx, span, *v, from);
                }
            });
        },
    );
    Some(output)
}

/// The site height every render path anchors its MSL heights on: the
/// **feedhorn**, not the ground under the tower.
///
/// Zero, which makes every height this render produces one above the antenna
/// rather than above sea level. That is the only honest answer available: it
/// is not a claim that the radar is at sea level, it is the absence of an MSL
/// datum to add.
fn render_site_height_ft(lat: f64, lon: f64) -> f64 {
    crate::eet::radar_height_ft_near(lat, lon, crate::sites::Datum::Feedhorn).unwrap_or(0.0)
}

/// Render one of the derived hail products ([`crate::hail`]): POSH in %,
/// or MEHS converted from the field's mm into **inches** — the palette's,
/// legend's and hover's unit — on a 1° × 1 km polar grid. Tilt-independent:
/// every elevation request renders the same volume product.
///
/// `env_heights_km_msl` is the per-site 0 °C / −20 °C pair
/// ([`crate::sounding::EnvHeights`], km MSL). **`None` renders nothing** —
/// `compute_hail` has no field without an environment, and this seam turns
/// that into the ordinary "no data" answer rather than a zero-filled grid
/// pretending to be one.
pub fn render_hail_to_image(
    scan: &Scan,
    product: types::RadarProduct,
    radar_lat: f64,
    radar_lon: f64,
    env_heights_km_msl: Option<(f64, f64)>,
    side_ceiling_px: usize,
) -> Option<SweepRender> {
    let Some((h0c_km_msl, hm20c_km_msl)) = env_heights_km_msl else {
        log::info!("{product:?}: no environmental heights — nothing to render");
        return None;
    };
    let env = crate::sounding::EnvHeights {
        h0c_km_msl,
        hm20c_km_msl,
        fetched_at: chrono::Utc::now(),
    };
    let radar_height_ft = render_site_height_ft(radar_lat, radar_lon);
    // The observing antenna's beam, not the WSR-88D's: this caps the ceiling
    // layer, and a TDWR's 0.55° is not the fleet's 0.95°. See
    // `hail::layer_bounds_km`.
    let beamwidth_deg = crate::beam::half_power_beamwidth_deg_near(radar_lat, radar_lon);
    let grids = crate::hail::compute_hail(scan, Some(&env), radar_height_ft, beamwidth_deg)?;
    const MM_PER_IN: f32 = 25.4;
    let (grid, unit_scale) = match product {
        types::RadarProduct::MaxExpectedHailSize => (grids.mehs_mm, 1.0 / MM_PER_IN),
        _ => (grids.posh, 1.0),
    };
    let max_range = grid.range_bins as f64;
    let output = render_with_projection(
        radar_lat,
        radar_lon,
        FieldRadial {
            reach_km: max_range,
            sample_km: crate::volumetric::RANGE_BIN_KM,
            shape: volume_grid_shape(grid.values.len(), grid.range_bins),
        },
        product,
        side_ceiling_px,
        "Radar",
        |proj, bufs| {
            grid.values.par_iter().enumerate().for_each(|(az, row)| {
                let ctx = RadialContext::new(az as f64 + 0.5, 0.5);
                for (r, v) in row.iter().enumerate() {
                    if v.is_nan() {
                        continue;
                    }
                    let from = GateId {
                        radial: az,
                        gate: r,
                    };
                    let span = GateSpan {
                        near_km: r as f64,
                        far_km: r as f64 + 1.0,
                    };
                    proj.render_gate(bufs, &ctx, span, *v * unit_scale, from);
                }
            });
        },
    );
    Some(output)
}

/// Render the locally derived Hybrid Hydrometeor Classification
/// ([`crate::hhc::compute_hhc`]): the whole volume's per-tilt
/// classification composited down the hybrid scan, a 1° × 0.25 km polar
/// grid of class codes painted with the HHC palette. Tilt-independent —
/// every elevation request renders the same volume product.
///
/// `melting_layer_product` is the RPG's own Melting Layer object (Level III
/// 166, `N0M`) **for this volume**, and it is the answer whenever it is
/// there. [`crate::hca::resolve_melting_layer`] owns the chain below it.
///
/// `env_heights_km_msl` is the sounding's (0 °C, −20 °C) pair. It still
/// places the HSDA's wet-bulb regimes, and its 0 °C height is the chain's
/// third rung; `None` falls back to the operational adaptation defaults,
/// exactly as the RPG runs without environmental data.
pub fn render_hhc_to_image(
    scan: &Scan,
    radar_lat: f64,
    radar_lon: f64,
    env_heights_km_msl: Option<(f64, f64)>,
    melting_layer_product: Option<&[u8]>,
    side_ceiling_px: usize,
) -> Option<SweepRender> {
    let radar_km_msl = render_site_height_ft(radar_lat, radar_lon) * 0.0003048;
    let params = crate::kdp::KdpParams {
        isdp_est_deg: crate::kdp::estimate_volume_isdp(scan),
        ..crate::kdp::KdpParams::render_fallback()
    };
    let hsda = match env_heights_km_msl {
        Some((h0c, hm20c)) => crate::hca::HsdaHeights::from_env_heights(h0c, hm20c, radar_km_msl),
        None => crate::hca::HsdaHeights::operational_defaults(radar_km_msl),
    };
    // The object's own decoder, not a second description of the product. A
    // decode failure is not fatal here: it drops this rung of
    // `resolve_melting_layer`'s chain and the next one answers.
    let rpg_layer = melting_layer_product.and_then(|bytes| {
        match nexrad_level3::decode::decode_product(bytes) {
            Ok(message) => Some(message),
            Err(e) => {
                log::warn!("Could not decode the melting layer product: {e}");
                None
            }
        }
    });

    let all: Vec<&[nexrad_model::data::Radial]> =
        scan.sweeps().iter().map(|s| s.radials()).collect();
    let dp: Vec<&[nexrad_model::data::Radial]> = all
        .iter()
        .copied()
        .filter(|r| {
            r.first()
                .map(|x| x.differential_phase().is_some())
                .unwrap_or(false)
        })
        .collect();
    let cappi = crate::hca::build_refl_cappi(&dp);
    let ml_sweeps: Vec<&[nexrad_model::data::Radial]> = dp
        .iter()
        .copied()
        .filter(|r| {
            r.first()
                .map(|x| (4.0..=10.0).contains(&f64::from(x.elevation_angle_degrees())))
                .unwrap_or(false)
        })
        .collect();
    let ml = crate::hca::resolve_melting_layer(
        rpg_layer.as_ref(),
        &ml_sweeps,
        &params,
        env_heights_km_msl.map(|(h0c, _)| h0c),
        radar_km_msl,
        &hsda,
        Some(&cappi),
    );
    let melting_layer_source = ml.source;
    let tilts = crate::hhc::volume_tilts(&all);
    let grid = crate::hhc::compute_hhc(&tilts, &params, &ml, &hsda, Some(&cappi))?;

    let max_gates = grid.values.iter().map(Vec::len).max().unwrap_or(0);
    let max_range = grid.first_gate_km + max_gates as f64 * grid.gate_interval_km;
    let output = render_with_projection(
        radar_lat,
        radar_lon,
        FieldRadial {
            reach_km: max_range,
            sample_km: grid.gate_interval_km,
            shape: polar::PolarShape {
                radials: grid.values.len(),
                gates: max_gates,
                first_gate_slant_km: grid.first_gate_km,
                gate_interval_slant_km: grid.gate_interval_km,
                elevation_deg: None,
            },
        },
        types::RadarProduct::HydrometeorClassification,
        side_ceiling_px,
        "Radar",
        |proj, bufs| {
            grid.values.par_iter().enumerate().for_each(|(az, row)| {
                let ctx = RadialContext::new(az as f64 + 0.5, 0.5);
                for (r, &v) in row.iter().enumerate() {
                    if v.is_nan() {
                        continue;
                    }
                    let range_km = grid.first_gate_km + r as f64 * grid.gate_interval_km;
                    let from = GateId {
                        radial: az,
                        gate: r,
                    };
                    let half = grid.gate_interval_km / 2.0;
                    let span = GateSpan {
                        near_km: range_km - half,
                        far_km: range_km + half,
                    };
                    proj.render_gate(bufs, &ctx, span, v, from);
                }
            });
        },
    );
    Some(output.classified_against(melting_layer_source))
}

/// Render the locally derived Specific Differential Phase
/// ([`crate::kdp::compute_kdp`]) for the tilt family nearest
/// `elevation_angle`: the sweep is picked with the same tilt-family rule as
/// the differential phase moment it derives from (surveillance cut
/// preferred), and the recombined 1° × 0.25 km field paints with the KDP
/// palette.
///
/// `params` carries the radial-header quantities a decoded `Scan` lacks —
/// [`crate::kdp::KdpParams::from_archive`] when the caller holds the raw
/// file, `KdpParams::default()` (the documented estimator fallback)
/// otherwise.
pub fn render_derived_kdp_to_image(
    scan: &Scan,
    elevation_angle: f32,
    radar_lat: f64,
    radar_lon: f64,
    params: &crate::kdp::KdpParams,
    side_ceiling_px: usize,
) -> Option<SweepRender> {
    let radials = find_sweep(
        scan,
        types::RadarProduct::DifferentialPhase,
        elevation_angle,
    )?;
    let derived = crate::kdp::compute_kdp(radials, params)?;
    let n_radials = derived.values.len();
    if n_radials == 0 {
        return None;
    }
    let max_gates = derived.values.iter().map(Vec::len).max().unwrap_or(0);
    // KDP is a range derivative of ΦDP, so its grid keeps the differential
    // phase sweep's own gate spacing and reaches where that sweep reached —
    // and is placed on the ground the same way that sweep's moments are.
    let sweep_elevation = sweep_elevation_deg_or_flat(radials);
    let slant_reach_km = derived.first_gate_km + max_gates as f64 * derived.gate_interval_km;
    let cos_e = sweep_ground_factor(slant_reach_km, sweep_elevation);
    let ground_reach_km = slant_reach_km * cos_e;
    let half_widths = derived_grid_half_widths_deg(&derived.azimuths_deg);

    let output = render_with_projection(
        radar_lat,
        radar_lon,
        FieldRadial {
            reach_km: ground_reach_km,
            sample_km: derived.gate_interval_km * cos_e,
            shape: polar::PolarShape {
                radials: n_radials,
                gates: max_gates,
                first_gate_slant_km: derived.first_gate_km,
                gate_interval_slant_km: derived.gate_interval_km,
                elevation_deg: Some(sweep_elevation),
            },
        },
        types::RadarProduct::SpecificDifferentialPhase,
        side_ceiling_px,
        "KDP",
        |proj, bufs| {
            derived.values.par_iter().enumerate().for_each(|(i, row)| {
                let ctx = RadialContext::new(derived.azimuths_deg[i], half_widths[i]);
                let edges = gate_ground_edges(
                    derived.first_gate_km,
                    derived.gate_interval_km,
                    row.len(),
                    |slant_km| crate::beam::ground_range_km(slant_km, sweep_elevation),
                );
                for ((j, &v), span) in row.iter().enumerate().zip(edges) {
                    if v.is_nan() {
                        continue;
                    }
                    if span.near_km > proj.extent_km {
                        break;
                    }
                    let from = GateId { radial: i, gate: j };
                    proj.render_gate(bufs, &ctx, span, v, from);
                }
            });
        },
    );
    Some(output)
}

/// Render a Level III radial product, as [`render_radar_to_image`] does for a
/// Level II `Scan`.
///
/// For digital products `physical = (gate_byte - offset) / scale`. A `lut`
/// overrides that and indexes on the gate value directly, covering legacy 4-bit
/// products (16 entries) and VIL (256 entries).
#[allow(clippy::too_many_arguments)]
pub fn render_level3_radial_to_image(
    radial_packet: &nexrad_level3::model::RadialPacket,
    product: types::RadarProduct,
    radar_lat: f64,
    radar_lon: f64,
    scale: f32,
    offset: f32,
    lut: Option<&[f32]>,
    side_ceiling_px: usize,
) -> Option<SweepRender> {
    render_level3_radial_with_gate_km(
        radial_packet,
        radial_packet.gate_interval_km(),
        product,
        radar_lat,
        radar_lon,
        scale,
        offset,
        lut,
        side_ceiling_px,
    )
}

/// [`render_level3_radial_to_image`] with the gate spacing chosen by the
/// caller. The message path passes the PDB's product-code override — some
/// products' packet-16 scale-factor halfword does not carry the gate size
/// (see `ProductDescriptionBlock::range_gate_km`) — so the first gate's range
/// is also re-derived from `first_range_bin` at the chosen spacing rather
/// than taken from the packet.
///
/// [`RadialPacket::gate_range_km`] answers the range of a gate's **centre**,
/// which is the half-gate-shifted reading ICD 2620001AD fixes (Appendix E:
/// "Range to the center of the first bin"). That is the coordinate
/// `Projection::render_gate` wants — it paints `range_km ± gate_interval/2`.
/// The reach handed to the projection stays an *edge*
/// ([`RadialPacket::reach_km`]): the last gate's outer boundary, not its
/// centre.
///
/// The sweep rasterizers above turn a gate's slant range into the ground
/// range under it. This one deliberately does not, and the reason is that a
/// Level III bin is **already** the RPG's answer about where something is,
/// not a measurement this display is placing:
///
/// * The RPG bins on the ground itself. Its own generators carry `cos(elev)`
///   as a *display* constant rather than as a range correction —
///   `dualpol8bit.c` writes `cos(elev)·1000` into the packet-16 scale-factor
///   halfword, which `nexrad_level3::model::ProductDescriptionBlock::
///   `range_gate_km` documents and overrides.
#[allow(clippy::too_many_arguments)]
fn render_level3_radial_with_gate_km(
    radial_packet: &nexrad_level3::model::RadialPacket,
    gate_interval: f64,
    product: types::RadarProduct,
    radar_lat: f64,
    radar_lon: f64,
    scale: f32,
    offset: f32,
    lut: Option<&[f32]>,
    side_ceiling_px: usize,
) -> Option<SweepRender> {
    if radial_packet.radials.is_empty() {
        return None;
    }

    let num_bins = radial_packet.num_range_bins as usize;
    let actual_max_range = radial_packet.reach_km(num_bins, gate_interval);

    let radials = &radial_packet.radials;

    let output = render_with_projection(
        radar_lat,
        radar_lon,
        FieldRadial {
            reach_km: actual_max_range,
            sample_km: gate_interval,
            shape: polar::PolarShape {
                radials: radials.len(),
                gates: num_bins,
                // The same centre the gate loop below paints gate 0 at, which
                // is what this field has to agree with.
                first_gate_slant_km: radial_packet.gate_range_km(0, gate_interval),
                gate_interval_slant_km: gate_interval,
                elevation_deg: None,
            },
        },
        product,
        side_ceiling_px,
        "Level III",
        |proj, bufs| {
            radials
                .par_iter()
                .enumerate()
                .for_each(|(radial_idx, radial_run)| {
                    let azimuth =
                        radial_run.start_angle as f64 + radial_run.angle_delta as f64 / 2.0;
                    let ctx = RadialContext::new(azimuth, radial_run.angle_delta as f64 / 2.0);

                    let bins_to_render = radial_run.gate_values.len().min(num_bins);
                    for (gate_idx, &gate_value) in
                        radial_run.gate_values[..bins_to_render].iter().enumerate()
                    {
                        if gate_value <= 1 {
                            continue;
                        }

                        let physical_value =
                            l3_physical_value(gate_value, product, scale, offset, lut);
                        if physical_value.is_nan() || physical_value >= 999.0 {
                            continue;
                        }

                        let range_km = radial_packet.gate_range_km(gate_idx, gate_interval);
                        if range_km > proj.extent_km {
                            break;
                        }

                        let from = GateId {
                            radial: radial_idx,
                            gate: gate_idx,
                        };
                        let half = gate_interval / 2.0;
                        let span = GateSpan {
                            near_km: range_km - half,
                            far_km: range_km + half,
                        };
                        proj.render_gate(bufs, &ctx, span, physical_value, from);
                    }
                });
        },
    );
    Some(output)
}

/// Render a storm-relative velocity field derived from dealiased Level III
/// velocity. See [`crate::srm`].
pub fn render_derived_srm_to_image(
    derived: &crate::srm::DerivedSrm,
    radar_lat: f64,
    radar_lon: f64,
    side_ceiling_px: usize,
) -> Option<SweepRender> {
    render_level3_radial_to_image(
        &derived.packet,
        types::RadarProduct::StormRelativeVelocity,
        radar_lat,
        radar_lon,
        derived.scale,
        derived.offset,
        None,
        side_ceiling_px,
    )
}

/// Render a Level III message, taking the radial packet, scale/offset and LUT
/// out of its symbology and product description blocks. Keeps every
/// nexrad-level3 internal out of the callers.
pub fn render_level3_message_to_image(
    l3_msg: &nexrad_level3::model::Level3Message,
    product: types::RadarProduct,
    radar_lat: f64,
    radar_lon: f64,
) -> Option<SweepRender> {
    render_level3_message_to_image_sized(l3_msg, product, radar_lat, radar_lon, types::IMAGE_SIZE)
}

/// [`render_level3_message_to_image`] at a caller-chosen side ceiling.
pub fn render_level3_message_to_image_sized(
    l3_msg: &nexrad_level3::model::Level3Message,
    product: types::RadarProduct,
    radar_lat: f64,
    radar_lon: f64,
    side_ceiling_px: usize,
) -> Option<SweepRender> {
    use nexrad_level3::model::DataPacket;

    let radial_packet = l3_msg.symbology.as_ref().and_then(|sym| {
        sym.layers.iter().find_map(|layer| {
            layer.packets.iter().find_map(|pkt| {
                if let DataPacket::DigitalRadial(rp) = pkt {
                    Some(rp)
                } else {
                    None
                }
            })
        })
    });

    let rp = match radial_packet {
        Some(rp) => {
            log::debug!(
                "L3 {:?}: radials={}, bins={}, legacy={}, scale_factor={}",
                product,
                rp.radials.len(),
                rp.num_range_bins,
                rp.is_legacy,
                rp.scale_factor
            );
            rp
        }
        None => {
            log::warn!("L3 {:?}: no radial packet found in symbology!", product);
            return None;
        }
    };

    // Prefer the XDR scale/offset from packet 28 attributes: PDB thresholds do
    // not encode IEEE floats for some products (134 DVL, 135 EET).
    let scale = rp.xdr_data_scale.unwrap_or_else(|| l3_msg.pdb.data_scale());
    let offset = rp
        .xdr_data_offset
        .unwrap_or_else(|| l3_msg.pdb.data_offset());
    let product_lut = build_vil_lut(&l3_msg.pdb).or_else(|| build_eet_lut(&l3_msg.pdb));
    let legacy_lut;
    let lut: Option<&[f32]> = if product_lut.is_some() {
        product_lut.as_deref()
    } else if rp.is_legacy {
        legacy_lut = decode_legacy_thresholds(&l3_msg.pdb);
        Some(legacy_lut.as_slice())
    } else {
        None
    };

    log::debug!(
        "L3 {:?}: rendering with scale={}, offset={}, legacy={}, lut_len={:?}, xdr_scale={:?}, xdr_offset={:?}",
        product,
        scale,
        offset,
        rp.is_legacy,
        lut.map(|l| l.len()),
        rp.xdr_data_scale,
        rp.xdr_data_offset
    );

    // The packet's own gate spacing with the PDB's product-code override —
    // 99/154/163's scale-factor halfword lies about the gate size, and the
    // twin-comparison path already prefers the PDB the same way.
    let gate_interval = crate::twin::compare::gate_km(&l3_msg.pdb, rp);
    render_level3_radial_with_gate_km(
        rp,
        gate_interval,
        product,
        radar_lat,
        radar_lon,
        scale,
        offset,
        lut,
        side_ceiling_px,
    )
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
