use nexrad_model::data::{DataMoment, Radial, Scan};
#[cfg(not(target_arch = "wasm32"))]
use rayon::prelude::*;

#[cfg(target_arch = "wasm32")]
use seq_fallback::*;

/// Sequential stand-ins for the two rayon entry points this module uses.
///
/// wasm32-unknown-unknown is single-threaded: rayon compiles there but cannot
/// build a thread pool. Keeping the call sites identical avoids cfg'ing four
/// rasterization loops, which would then drift. The closures need no changes —
/// rayon requires `Fn + Send + Sync`, strictly stronger than the `FnMut` these
/// want.
///
/// This is a cfg split, **not** a removal: rasterization is the hot path on
/// desktop, and this fallback silently becoming the native arm is a large
/// regression that no test catches.
#[cfg(target_arch = "wasm32")]
mod seq_fallback {
    /// Stands in for `rayon::prelude::ParallelSlice::par_iter`. Implemented on
    /// `[T]` only; `Vec<T>` reaches it through deref.
    pub trait ParIterFallback<T> {
        fn par_iter<'a>(&'a self) -> impl Iterator<Item = &'a T>
        where
            T: 'a;
    }

    impl<T> ParIterFallback<T> for [T] {
        fn par_iter<'a>(&'a self) -> impl Iterator<Item = &'a T>
        where
            T: 'a,
        {
            self.iter()
        }
    }

    /// Stands in for `rayon::iter::IntoParallelIterator::into_par_iter`.
    pub trait IntoParIterFallback {
        type Item;
        fn into_par_iter(self) -> impl Iterator<Item = Self::Item>;
    }

    impl IntoParIterFallback for std::ops::Range<usize> {
        type Item = usize;
        fn into_par_iter(self) -> impl Iterator<Item = usize> {
            self
        }
    }

    /// Stands in for `rayon::slice::ParallelSlice::par_chunks`.
    pub trait ParChunksFallback<T> {
        fn par_chunks<'a>(&'a self, n: usize) -> impl Iterator<Item = &'a [T]>
        where
            T: 'a;
    }

    impl<T> ParChunksFallback<T> for [T] {
        fn par_chunks<'a>(&'a self, n: usize) -> impl Iterator<Item = &'a [T]>
        where
            T: 'a,
        {
            self.chunks(n)
        }
    }

    /// Stands in for `rayon::slice::ParallelSliceMut::par_chunks_mut`.
    pub trait ParChunksMutFallback<T> {
        fn par_chunks_mut<'a>(&'a mut self, n: usize) -> impl Iterator<Item = &'a mut [T]>
        where
            T: 'a;
    }

    impl<T> ParChunksMutFallback<T> for [T] {
        fn par_chunks_mut<'a>(&'a mut self, n: usize) -> impl Iterator<Item = &'a mut [T]>
        where
            T: 'a,
        {
            self.chunks_mut(n)
        }
    }
}

use crate::l3_values::{build_eet_lut, build_vil_lut, decode_legacy_thresholds, l3_physical_value};
use crate::palette::get_color_for_value;
use crate::types;
use std::f64::consts::PI;
use std::sync::atomic::{AtomicU64, Ordering};

// ── Shared rendering infrastructure ──────────────────────────────────────────

/// Pre-computed Web Mercator projection constants, derived from
/// [`types::ImageBounds`] so the pixel grid aligns with the bounds the UI gets.
struct MercatorProjection {
    radar_lat_rad: f64,
    cos_radar_lat: f64,
    center_px: f64,
    merc_y_top: f64,
    merc_y_scale: f64,
}

impl MercatorProjection {
    fn from_bounds(radar_lat: f64, bounds: &types::ImageBounds) -> Self {
        let radar_lat_rad = radar_lat.to_radians();
        Self {
            radar_lat_rad,
            cos_radar_lat: radar_lat_rad.cos(),
            center_px: types::IMAGE_SIZE as f64 / 2.0,
            merc_y_top: bounds.mercator_y_max,
            merc_y_scale: types::IMAGE_SIZE as f64
                / (bounds.mercator_y_max - bounds.mercator_y_min),
        }
    }

    fn render_gate(
        &self,
        bufs: &RenderBuffers,
        ctx: &RadialContext,
        range_km: f64,
        gate_interval: f64,
        value: f32,
        from: GateId,
    ) {
        let range_start = range_km - gate_interval / 2.0;
        let range_end = range_km + gate_interval / 2.0;

        let num_range_samples =
            ((range_end - range_start) * types::PIXELS_PER_KM).ceil() as i32 + 2;
        let num_az_samples = ((ctx.az_half_spacing * 2.0 * range_km * PI / 180.0)
            * types::PIXELS_PER_KM)
            .ceil() as i32
            + 2;
        let inv_num_range = 1.0 / num_range_samples.max(1) as f64;
        let inv_num_az = 1.0 / num_az_samples.max(1) as f64;

        let cell = RenderBuffers::cell(write_key(from), value);

        for r_step in 0..num_range_samples {
            let r = range_start + (range_end - range_start) * (r_step as f64 * inv_num_range);
            let dy_center = r * ctx.cos_az_center;
            let dest_lat_rad = self.radar_lat_rad + dy_center / types::EARTH_RADIUS_KM;
            let cos_correction = self.cos_radar_lat / dest_lat_rad.cos();

            for az_step in 0..num_az_samples {
                let t = az_step as f64 * inv_num_az;
                let sin_az = ctx.sin_az_start + ctx.sin_az_delta * t;
                let cos_az = ctx.cos_az_start + ctx.cos_az_delta * t;

                let dx_km = r * sin_az;
                let dy_km = r * cos_az;
                let px_i = (self.center_px + dx_km * cos_correction * types::PIXELS_PER_KM) as i32;
                let dest_lat_rad = self.radar_lat_rad + dy_km / types::EARTH_RADIUS_KM;
                let dest_merc_y = types::lat_rad_to_mercator_y(dest_lat_rad);
                let py_i = ((self.merc_y_top - dest_merc_y) * self.merc_y_scale) as i32;

                if px_i >= 0
                    && px_i < types::IMAGE_SIZE as i32
                    && py_i >= 0
                    && py_i < types::IMAGE_SIZE as i32
                {
                    let pixel_idx = py_i as usize * types::IMAGE_SIZE + px_i as usize;
                    bufs.claim(pixel_idx, cell);
                }
            }
        }
    }
}

/// Pre-computed azimuth sin/cos values for a single radial strip.
struct RadialContext {
    cos_az_center: f64,
    sin_az_start: f64,
    cos_az_start: f64,
    sin_az_delta: f64,
    cos_az_delta: f64,
    az_half_spacing: f64,
}

impl RadialContext {
    fn new(azimuth_deg: f64, az_half_spacing_deg: f64) -> Self {
        let az_start_rad = (azimuth_deg - az_half_spacing_deg) * PI / 180.0;
        let az_end_rad = (azimuth_deg + az_half_spacing_deg) * PI / 180.0;
        let cos_az_center = (azimuth_deg * PI / 180.0).cos();
        let (sin_az_start, cos_az_start) = az_start_rad.sin_cos();
        let (sin_az_end, cos_az_end) = az_end_rad.sin_cos();
        Self {
            cos_az_center,
            sin_az_start,
            cos_az_start,
            sin_az_delta: sin_az_end - sin_az_start,
            cos_az_delta: cos_az_end - cos_az_start,
            az_half_spacing: az_half_spacing_deg,
        }
    }
}

/// One atomic cell per output pixel: `(write_key << 32) | value_bits`.
///
/// `render_gate` runs under a `par_iter` over radials, and two radials
/// routinely claim the same pixel — but *not* because their footprints overlap
/// in continuous space. They tile: `t` runs over `[0, 1)`, so a gate samples a
/// strict subset of `[range_start, range_end)`, and the `+2` on the sample
/// counts raises sample *density*, never extent. They collide because those
/// footprints are quantized onto a pixel grid nothing aligns them to — inside
/// ~26 km a 0.5° radial's arc is narrower than one pixel, and at any range the
/// truncating cast drops neighbouring wedges into the same cell. The L2 path
/// adds a second source: `compute_azimuth_spacing` hands every radial the
/// *average* half-width, so radials packed tighter than average overlap for
/// real. A fixture whose wedges tile exactly still contends over 271 pixels;
/// see `overlapping_radials_contend_for_pixels`.
///
/// Neither claimant is more correct — the rasterizer never computes subpixel
/// coverage — so the tie is arbitrary, and the only question is whether it gets
/// resolved *stably*.
///
/// It used to be resolved by the race. Two relaxed stores per sample, one to an
/// image buffer and one to a value buffer, last writer wins. That cost two
/// things:
///
///   * The render was not reproducible. Over 12 runs of a 720 × 1200 L3 sweep
///     at `IMAGE_SIZE` 2048 on 32 threads: 12 distinct hashes, ~16 k of 3.3 M
///     painted pixels differing per pair, 53 k in the union. Invisible, in
///     fairness — 91% of those differed by ≤ 0.5 dBZ (one data level), none by
///     more than 5 dBZ, and no pixel flipped between opaque and transparent.
///   * The image and value stores were a *pair*, and nothing kept them
///     together. One radial could win the colour while another won the value,
///     leaving a pixel no radial ever wrote: measured, rare, real — 3 such
///     pixels over 12 runs of that sweep.
///
/// Now there is one cell, so there is no pair to tear, and it is claimed with
/// `fetch_max` rather than a store. `fetch_max` is a set operation: the result
/// is the greatest claim, whatever order the claims arrive in. With
/// [`write_key`] ranking claims radial-major, gate-minor, the greatest claim is
/// the one a single-threaded radial-major render would have written last — so
/// the parallel result *is* the sequential result, not merely a stable one.
/// Checked against the pre-change rasterizer compiled in alongside this one:
/// 0 differing bytes and 0 differing values over all 4,194,304 pixels, on both
/// a smooth field and an adversarial one. Note the suite cannot re-check that
/// on its own — `parallel_matches_single_thread` compares this code against
/// its own single-threaded self, which `fetch_max` makes true by construction.
///
/// ## What determinism costs
///
/// It is not free. `AtomicU64::fetch_max` has no x86-64 instruction behind it;
/// it lowers to a `lock cmpxchgq` retry loop, which needs the line exclusively
/// and cannot coalesce in the store buffer, so the cost climbs with thread
/// count. Three variants compiled into one binary and interleaved in one
/// process, 30 samples each, same 720 × 1200 sweep. **Medians** — the minimum
/// is actively misleading here, because `fetch_max` widens the distribution
/// instead of shifting it, and min-of-N reports the run that got lucky:
///
/// | `IMAGE_SIZE` 2048           |  1 thr | 8 thr | 16 thr | 32 thr |
/// |-----------------------------|-------:|------:|-------:|-------:|
/// | 2 × `AtomicU32`, store      |  395.8 |  73.2 |   51.1 |   42.9 |
/// | 1 × `AtomicU32`, store      |  394.6 |  67.0 |   45.2 |   37.9 |
/// | 1 × `AtomicU64`, `fetch_max`|  413.4 |  72.3 |   52.9 |   52.1 |
///
/// At 32 threads that is +21% against the old layout, and the spread tells the
/// story better than the median: 41.7 / 42.9 / 44.0 (min/median/max) before,
/// 37.0 / 52.1 / 64.7 now. At `IMAGE_SIZE` 1024 single-threaded — the web arm's
/// operating point — it is a wash: 201.7 / 195.5 / 200.3.
///
/// The middle row is why the cell was collapsed at all, and it is separable
/// from the keying: a single `AtomicU32` holding just the value bits ends the
/// tearing outright, with nothing left to tear against, and is the fastest of
/// the three everywhere. Determinism is what the third row buys and the third
/// row's price.
///
/// Colour is derived in `into_output` rather than stored per gate. That is
/// *more* palette work, not less — ~663 k gates reach the 230 km break against
/// 3.3 M painted pixels at 2048² — but it is parallel and off the fill loop,
/// and it removes a whole store per sample from the loop that is actually hot.
/// Leaving that pass serial costs more than everything else here put together.
///
/// ## Earlier measurement, still standing
///
/// The atomics are *not* load-bearing on wasm32, so cfg-splitting that arm to a
/// plain buffer looks like a free win. It was measured per component, not
/// assumed, against a real KTLX 0.5° reflectivity sweep (720 radials × 1832
/// gates) at `IMAGE_SIZE` 1024, release, rasterizer isolated from WebGL/winit.
/// It predates the collapse to one cell, so the store counts are the old
/// paired ones and it measured relaxed *stores*, not the RMW the fill loop now
/// runs. Nothing here re-measures that in a browser; what carries over is only
/// that atomics-vs-plain was ~1% of the frame when it was measured.
///
/// | what                                    | Firefox | Chromium |
/// |-----------------------------------------|--------:|---------:|
/// | whole render                            |  233 ms |   261 ms |
/// | 28 M relaxed `store` vs plain `Vec<u32>`| 39 / 37 |  47 / 48 |
/// | `into_output` shape, atomic vs plain    | 0.8/0.4 |  0.7/0.3 |
/// | `RenderBuffers::new`, atomic vs plain   | 0.2/0.3 |  0.3/0.2 |
///
/// ~2.5 ms of a 233 ms frame — about 1%, the same 1% in both browsers. Built and
/// measured end to end too, with the wasm arm on `Vec<Cell<u32>>`: Firefox
/// 233 → 230 ms, Chromium 261 → 262 ms, byte-identical image. A 1% return does
/// not pay for two divergent buffer types under one hot loop.
///
/// Those same numbers dispose of the theory that Firefox's `radar-render`
/// penalty came from these atomics: Firefox rasterizes this sweep *faster* than
/// Chromium, and relaxed atomic stores cost it 5% over plain ones.
///
/// Most of the frame is the per-sample `(π/4 + lat/2).tan().ln()` in
/// `types::lat_rad_to_mercator_y`: 28 M of those cost 660 ms in Firefox and
/// 597 ms in Chromium against 29 ms and 37 ms for the same loop without them.
/// Reducing it means changing the arithmetic every output pixel depends on, so
/// it cannot be done bit-identically. Firefox's reported 5.7× `radar-render`
/// penalty was a measurement artifact — re-measured on a pinned sweep it is a
/// 159 ms *minimum* against Chromium's 174 ms, a matched-pair median ratio of
/// 0.88; see `rustdar-web`'s crate docs for the medians and the method.
struct RenderBuffers {
    cells: Vec<AtomicU64>,
    /// Only `into_output` needs it, but it has to be the product the gates were
    /// coloured against, so it is captured at construction rather than passed
    /// back in.
    product: types::RadarProduct,
}

impl RenderBuffers {
    fn new(product: types::RadarProduct) -> Self {
        let n = types::IMAGE_SIZE * types::IMAGE_SIZE;
        Self {
            cells: (0..n).map(|_| AtomicU64::new(Self::EMPTY)).collect(),
            product,
        }
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

    /// Split the cells into the RGBA texture and the value grid.
    ///
    /// Colour is derived here rather than stored per sample: it is a pure
    /// function of the value at every call site, so keeping it in the cell
    /// would only give it a second chance to disagree. Deriving it is also
    /// less work — one lookup per pixel instead of one per gate — but only
    /// once the pass is parallel. Serial, it dominates the whole render.
    fn into_output(self, actual_max_range: f64) -> (Vec<u8>, f64, Vec<f32>) {
        let product = self.product;
        let value_data: Vec<f32> = self
            .cells
            .iter()
            .map(|a| match a.load(Ordering::Relaxed) {
                Self::EMPTY => f32::NAN,
                cell => f32::from_bits(cell as u32),
            })
            .collect();
        let mut image = vec![0u8; value_data.len() * 4];
        image
            .par_chunks_mut(4 * Self::COLOR_CHUNK)
            .zip(value_data.par_chunks(Self::COLOR_CHUNK))
            .for_each(|(px, vals)| {
                for (px, &v) in px.chunks_exact_mut(4).zip(vals) {
                    if !v.is_nan() {
                        let c = get_color_for_value(product, v);
                        px.copy_from_slice(&[c.0, c.1, c.2, c.3]);
                    }
                }
            });
        let max_range = if actual_max_range > 0.0 {
            actual_max_range
        } else {
            types::MAX_RANGE_KM
        };
        (image, max_range, value_data)
    }
}

/// Which gate a claim came from. Named fields rather than two `usize`
/// arguments: three call sites build one of these, and transposing them would
/// reorder the tie-break silently on whichever path got it wrong.
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
/// sequential order. No NEXRAD product comes close — 720 radials and 1832
/// gates is the widest sweep.
#[inline]
fn write_key(from: GateId) -> u32 {
    let r = from.radial.min(0xFFFF) as u32;
    let g = from.gate.min(0xFFFE) as u32;
    (r << 16) | (g + 1)
}

// ── Sweep / azimuth helpers ──────────────────────────────────────────────────

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
            let r = sweep.radials().first()?;
            let rounded = (r.elevation_angle_degrees() * 10.0).round() / 10.0;
            product.get_moment(r).is_some().then_some(rounded)
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
/// The window is a wide 0.3° rather than an exact match because split-cut
/// surveillance sweeps drift well off the nominal tilt (0.32°/0.63° cuts for
/// a requested 0.5°), and they carry the full-range reflectivity and dual-pol
/// fields. Doppler cuts sit within 0.05° of nominal, so velocity-family
/// products keep picking the same sweep they always did.
///
/// Within the family, non-Doppler products prefer the newest sweep *without*
/// a velocity moment: a split cut's Doppler half repeats a short-range copy
/// of the surveillance moments, and the reference display draws reflectivity
/// from the surveillance half (measured on a KLOT SAILS volume: the 0.63°
/// surveillance cut's painted mask matches the reference at IoU 0.73 /
/// area ratio 0.98, against 0.69 / 0.89 for the newer 0.53° Doppler cut).
/// Upper tilts are single merged cuts carrying everything, so the preference
/// falls back to any sweep with the product's moment.
/// `pub(crate)` for [`crate::render_input`], which has to make this exact
/// choice against the whole volume so the one sweep it carries is the one
/// `find_sweep` reaches again on the reconstructed scan.
pub(crate) fn find_sweep(
    scan: &Scan,
    product: types::RadarProduct,
    elevation_angle: f32,
) -> Option<&[Radial]> {
    let newest = |surveillance_only: bool| {
        scan.sweeps().iter().rev().find_map(|sweep| {
            let matches = sweep
                .radials()
                .first()
                .map(|r| {
                    (r.elevation_angle_degrees() - elevation_angle).abs() < 0.3
                        && product.get_moment(r).is_some()
                        && !(surveillance_only && r.velocity().is_some())
                })
                .unwrap_or(false);
            matches.then(|| sweep.radials())
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

/// Average azimuth spacing (degrees) between consecutive Level II radials.
fn compute_azimuth_spacing(radials: &[Radial]) -> f64 {
    let mut prev_azimuth: Option<f64> = None;
    let mut spacing_sum = 0.0f64;
    let mut spacing_count = 0u32;
    for radial in radials {
        let az = radial.azimuth_angle_degrees() as f64;
        if let Some(prev) = prev_azimuth {
            let mut diff = az - prev;
            if diff < -180.0 {
                diff += 360.0;
            } else if diff > 180.0 {
                diff -= 360.0;
            }
            spacing_sum += diff;
            spacing_count += 1;
        }
        prev_azimuth = Some(az);
    }
    if spacing_count > 0 {
        spacing_sum / spacing_count as f64
    } else {
        1.0
    }
}

/// Maximum range (km) derived from the first radial that carries the given
/// product's moment data.
fn compute_max_range(radials: &[Radial], product: types::RadarProduct) -> f64 {
    radials
        .iter()
        .find_map(|radial| {
            let moment = product.get_moment(radial)?;
            let gate_count = moment.gate_count() as usize;
            Some(moment.first_gate_range_km() + gate_count as f64 * moment.gate_interval_km())
        })
        .unwrap_or(0.0)
}

fn render_with_projection(
    radar_lat: f64,
    radar_lon: f64,
    actual_max_range: f64,
    product: types::RadarProduct,
    label: &str,
    fill: impl FnOnce(&MercatorProjection, &RenderBuffers),
) -> (Vec<u8>, f64, Vec<f32>) {
    let bounds = types::ImageBounds::from_radar_site(radar_lat, radar_lon);
    let proj = MercatorProjection::from_bounds(radar_lat, &bounds);
    let bufs = RenderBuffers::new(product);

    fill(&proj, &bufs);

    let (image, max_range, value_data) = bufs.into_output(actual_max_range);
    log::info!(
        "{} rendering complete: actual_max_range={:.1}km, using max_range={:.1}km",
        label,
        actual_max_range,
        max_range
    );
    (image, max_range, value_data)
}

// ── Public rendering functions ───────────────────────────────────────────────

/// Render radar data to an image projected for geographic display. Returns
/// `(RGBA pixels, max_range_km, per-pixel values)`; a value is `f32::NAN` where
/// there is no data.
pub fn render_radar_to_image(
    data: &Scan,
    elevation_angle: f32,
    product: types::RadarProduct,
    radar_lat: f64,
    radar_lon: f64,
) -> Option<(Vec<u8>, f64, Vec<f32>)> {
    render_radar_to_image_full(
        data,
        elevation_angle,
        product,
        radar_lat,
        radar_lon,
        None,
        None,
    )
}

/// [`render_radar_to_image`] from a [`RenderInput`] instead of a `Scan`.
///
/// This is the entry point for a caller that does not hold the volume — the
/// browser's rasterization worker, which is handed
/// [`RenderInput::to_bytes`](crate::render_input::RenderInput::to_bytes) over a
/// message port because a decoded `Scan` is tens of megabytes and a `RenderInput`
/// is one sweep.
///
/// It reconstructs a `Scan` and runs the ordinary renderer, so there is one
/// rasterizer rather than two that could disagree about a pixel; see
/// [`crate::render_input`] for why the reconstruction is exact.
pub fn render_from(input: &crate::render_input::RenderInput) -> Option<(Vec<u8>, f64, Vec<f32>)> {
    render_radar_to_image_full(
        &input.to_scan(),
        input.elevation(),
        input.product(),
        input.radar_lat(),
        input.radar_lon(),
        input.storm_motion_override(),
        input.env_heights_km_msl(),
    )
}

/// [`render_radar_to_image`] plus the two render parameters: the storm
/// motion override, in knots and degrees-from — read by storm-relative
/// velocity alone; `None` is "no override" and SRV applies the Bunkers
/// right-mover from the volume's own wind profile ([`crate::srv`]) — and
/// the environmental 0 °C / −20 °C heights in km MSL, read by the hail pair
/// alone; `None` there means the hail field is undefined and renders
/// nothing ([`crate::hail`]).
///
/// The environmental wind profile NROT's and SRV's dealiasers seed from is
/// not a parameter: it is fit from the volume's own velocity tilts
/// ([`build_wind_profile`]). The RPG's NVW product used to be an alternate
/// source, until the local VAD fit was validated against the RPG's own
/// dealiased velocity and the fetch dropped.
pub fn render_radar_to_image_full(
    data: &Scan,
    elevation_angle: f32,
    product: types::RadarProduct,
    radar_lat: f64,
    radar_lon: f64,
    storm_motion_override: Option<(f32, f32)>,
    env_heights_km_msl: Option<(f64, f64)>,
) -> Option<(Vec<u8>, f64, Vec<f32>)> {
    if product == types::RadarProduct::EchoTopsInterpolated {
        return render_echo_tops_interp_to_image(data, radar_lat, radar_lon);
    }

    if product == types::RadarProduct::VilDensity {
        return render_vil_density_to_image(data, radar_lat, radar_lon);
    }

    if matches!(
        product,
        types::RadarProduct::ProbabilityOfSevereHail | types::RadarProduct::MaxExpectedHailSize
    ) {
        return render_hail_to_image(data, product, radar_lat, radar_lon, env_heights_km_msl);
    }

    if product == types::RadarProduct::HydrometeorClassification {
        return render_hhc_to_image(data, radar_lat, radar_lon, env_heights_km_msl);
    }

    let radials = find_sweep(data, product, elevation_angle)?;

    if product == types::RadarProduct::NormalizedRotation {
        return render_nrot_to_image(data, radials, radar_lat, radar_lon);
    }

    if product == types::RadarProduct::StormRelativeVelocity {
        return render_srv_to_image(data, radials, radar_lat, radar_lon, storm_motion_override);
    }

    let avg_azimuth_spacing = compute_azimuth_spacing(radials);
    let actual_max_range = compute_max_range(radials, product);

    let output = render_with_projection(
        radar_lat,
        radar_lon,
        actual_max_range,
        product,
        "Radar",
        |proj, bufs| {
            radials
                .par_iter()
                .enumerate()
                .for_each(|(radial_idx, radial)| {
                    let azimuth = radial.azimuth_angle_degrees() as f64;
                    let ctx = RadialContext::new(azimuth, avg_azimuth_spacing / 2.0);

                    if let Some(moment) = product.get_moment(radial) {
                        let first_gate_range = moment.first_gate_range_km();
                        let gate_size = moment.gate_interval_km();

                        for (gate_idx, moment_value) in moment.values().iter().enumerate() {
                            let range_km = first_gate_range + (gate_idx as f64 * gate_size);
                            if range_km > types::MAX_RANGE_KM {
                                break;
                            }

                            let scaled_value = match moment_value {
                                nexrad_model::data::MomentValue::Value(v) => *v,
                                _ => continue,
                            };
                            if scaled_value >= 999.0 || scaled_value.is_nan() {
                                continue;
                            }

                            let from = GateId {
                                radial: radial_idx,
                                gate: gate_idx,
                            };
                            proj.render_gate(bufs, &ctx, range_km, gate_size, scaled_value, from);
                        }
                    }
                });
        },
    );
    Some(output)
}

/// Render NROT (Normalized Rotation): azimuthal shear derived from Level II
/// velocity, normalized by range to remove beam broadening and scaled to a
/// unitless field where >1.0 is significant and >2.5 extreme.
fn render_nrot_to_image(
    scan: &Scan,
    radials: &[Radial],
    radar_lat: f64,
    radar_lon: f64,
) -> Option<(Vec<u8>, f64, Vec<f32>)> {
    let num_radials = radials.len();
    if num_radials < 3 {
        return None;
    }

    let vg = build_velocity_grid(radials)?;

    let actual_max_range = vg.first_gate_range_km + vg.gate_count as f64 * vg.gate_interval_km;
    let avg_spacing_deg = 360.0 / num_radials as f64;

    let elevation_deg = radials
        .first()
        .map(|r| r.elevation_angle_degrees() as f64)
        .unwrap_or(0.5);
    let profile = build_wind_profile(scan);
    let nrot_grid = crate::nrot::compute_nrot_grid_with_profile(
        &crate::nrot::VelocitySweep {
            vel_grid: &vg.vel_grid,
            azimuths_deg: &vg.azimuths_deg,
            gate_count: vg.gate_count,
            first_gate_range_km: vg.first_gate_range_km,
            gate_interval_km: vg.gate_interval_km,
        },
        elevation_deg,
        profile.as_ref(),
    );

    let output = render_with_projection(
        radar_lat,
        radar_lon,
        actual_max_range,
        types::RadarProduct::NormalizedRotation,
        "NROT",
        |proj, bufs| {
            nrot_grid.par_iter().enumerate().for_each(|(i, nrot_row)| {
                let ctx = RadialContext::new(vg.azimuths_deg[i], avg_spacing_deg / 2.0);

                for (j, &nrot_val) in nrot_row.iter().enumerate() {
                    if nrot_val.is_nan() {
                        continue;
                    }

                    let range_km = vg.first_gate_range_km + j as f64 * vg.gate_interval_km;
                    if range_km > types::MAX_RANGE_KM {
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
                    proj.render_gate(
                        bufs,
                        &ctx,
                        range_km,
                        vg.gate_interval_km,
                        scaled_value,
                        from,
                    );
                }
            });
        },
    );
    Some(output)
}

/// Render storm-relative velocity derived locally from Level II: the sweep's
/// velocity dealiased under the Coverage profile, plus the storm-motion
/// correction — a user override when one is set, otherwise the Bunkers
/// right-mover from the volume's wind profile. Values are m/s, like every
/// Level II velocity field, so the palette and `format_value` read them
/// unchanged. See [`crate::srv`].
///
/// `None` when no vector exists at all — no override and a wind profile too
/// hollow for even the mean-wind fallback — because painting base velocity
/// under a storm-relative label is the failure the old Level III path
/// refused too (it waited for an `N0S`).
fn render_srv_to_image(
    scan: &Scan,
    radials: &[Radial],
    radar_lat: f64,
    radar_lon: f64,
    storm_motion_override: Option<(f32, f32)>,
) -> Option<(Vec<u8>, f64, Vec<f32>)> {
    if radials.len() < 3 {
        return None;
    }
    let elevation_deg = radials
        .first()
        .map(|r| r.elevation_angle_degrees() as f64)
        .unwrap_or(0.5);
    let profile = build_wind_profile(scan);
    let user = storm_motion_override.and_then(|(speed_kt, direction_deg)| {
        crate::srv::SrvMotion::user_override(speed_kt, direction_deg)
    });
    let motion = crate::srv::storm_motion(profile.as_ref(), user)?;
    log::info!(
        "SRV {elevation_deg:.1}°: {:.1} kt from {:.1}° ({:?})",
        motion.speed_kt,
        motion.direction_deg,
        motion.source,
    );
    let grid = crate::srv::compute_srv_grid(radials, elevation_deg, profile.as_ref(), &motion)?;

    let actual_max_range =
        grid.first_gate_range_km + grid.gate_count as f64 * grid.gate_interval_km;
    let avg_spacing_deg = 360.0 / grid.values.len().max(1) as f64;
    let output = render_with_projection(
        radar_lat,
        radar_lon,
        actual_max_range,
        types::RadarProduct::StormRelativeVelocity,
        "SRV",
        |proj, bufs| {
            grid.values.par_iter().enumerate().for_each(|(i, row)| {
                let ctx = RadialContext::new(grid.azimuths_deg[i], avg_spacing_deg / 2.0);
                for (j, &value) in row.iter().enumerate() {
                    if value.is_nan() {
                        continue;
                    }
                    let range_km = grid.first_gate_range_km + j as f64 * grid.gate_interval_km;
                    if range_km > types::MAX_RANGE_KM {
                        break;
                    }
                    let from = GateId { radial: i, gate: j };
                    proj.render_gate(
                        bufs,
                        &ctx,
                        range_km,
                        grid.gate_interval_km,
                        value as f32,
                        from,
                    );
                }
            });
        },
    );
    Some(output)
}

/// Velocity as a 2D grid (azimuth × range).
struct VelocityGrid {
    vel_grid: Vec<Vec<f64>>,
    azimuths_deg: Vec<f64>,
    gate_count: usize,
    first_gate_range_km: f64,
    gate_interval_km: f64,
}

/// Fit the volume wind profile from every velocity tilt in the scan
fn build_wind_profile(scan: &Scan) -> Option<crate::nrot::WindProfile> {
    let mut builder = crate::nrot::WindProfileBuilder::new();
    for sweep in scan.sweeps() {
        let radials = sweep.radials();
        let Some(first) = radials.first() else {
            continue;
        };
        if first.velocity().is_none() || radials.len() < 3 {
            continue;
        }
        let Some(vg) = build_velocity_grid(radials) else {
            continue;
        };
        builder.add_sweep(
            &crate::nrot::VelocitySweep {
                vel_grid: &vg.vel_grid,
                azimuths_deg: &vg.azimuths_deg,
                gate_count: vg.gate_count,
                first_gate_range_km: vg.first_gate_range_km,
                gate_interval_km: vg.gate_interval_km,
            },
            first.elevation_angle_degrees() as f64,
        );
    }
    builder.finish()
}

fn build_velocity_grid(radials: &[Radial]) -> Option<VelocityGrid> {
    let first_vel = radials.iter().find_map(|r| r.velocity())?;
    let gate_count = first_vel.gate_count() as usize;
    let first_gate_range_km = first_vel.first_gate_range_km();
    let gate_interval_km = first_vel.gate_interval_km();

    let mut vel_grid: Vec<Vec<f64>> = Vec::with_capacity(radials.len());
    let mut azimuths_deg: Vec<f64> = Vec::with_capacity(radials.len());

    for radial in radials.iter() {
        azimuths_deg.push(radial.azimuth_angle_degrees() as f64);
        let mut gates = vec![f64::NAN; gate_count];
        if let Some(moment) = radial.velocity() {
            for (j, val) in moment.values().iter().enumerate().take(gate_count) {
                if let nexrad_model::data::MomentValue::Value(v) = val
                    && !v.is_nan()
                    && *v < 999.0
                {
                    gates[j] = *v as f64;
                }
            }
        }
        vel_grid.push(gates);
    }

    Some(VelocityGrid {
        vel_grid,
        azimuths_deg,
        gate_count,
        first_gate_range_km,
        gate_interval_km,
    })
}

/// Render interpolated echo tops: the whole reflectivity volume reduced to a
/// 1° × 1 km polar grid of threshold-crossing heights, painted with the echo
/// tops palette. Tilt-independent — every elevation request renders the same
/// volume product.
pub fn render_echo_tops_interp_to_image(
    scan: &Scan,
    radar_lat: f64,
    radar_lon: f64,
) -> Option<(Vec<u8>, f64, Vec<f32>)> {
    let grid = crate::volumetric::compute_echo_tops(scan);
    let max_range = grid.range_bins as f64;
    let output = render_with_projection(
        radar_lat,
        radar_lon,
        max_range,
        types::RadarProduct::EchoTopsInterpolated,
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
                    proj.render_gate(bufs, &ctx, r as f64 + 0.5, 1.0, *v, from);
                }
            });
        },
    );
    Some(output)
}

/// Render VIL density: local Digital VIL over local echo tops
/// ([`crate::vil::compute_vil_density`]), a 1° × 1 km polar grid in g/m³
/// painted with the VIL-density palette. Tilt-independent — every elevation
/// request renders the same volume product. The site height the echo top's
/// MSL datum needs comes from the nearest-site table, as the EET render
/// path's does.
pub fn render_vil_density_to_image(
    scan: &Scan,
    radar_lat: f64,
    radar_lon: f64,
) -> Option<(Vec<u8>, f64, Vec<f32>)> {
    let radar_height_ft = crate::eet::radar_height_ft_near(radar_lat, radar_lon);
    let grid = crate::vil::compute_vil_density(scan, radar_height_ft);
    let max_range = grid.range_bins as f64;
    let output = render_with_projection(
        radar_lat,
        radar_lon,
        max_range,
        types::RadarProduct::VilDensity,
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
                    proj.render_gate(bufs, &ctx, r as f64 + 0.5, 1.0, *v, from);
                }
            });
        },
    );
    Some(output)
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
/// pretending to be one. The site height that resolves the MSL heights to
/// the beam's ARL datum comes from the nearest-site table, as the VIL
/// density render path's does.
pub fn render_hail_to_image(
    scan: &Scan,
    product: types::RadarProduct,
    radar_lat: f64,
    radar_lon: f64,
    env_heights_km_msl: Option<(f64, f64)>,
) -> Option<(Vec<u8>, f64, Vec<f32>)> {
    let Some((h0c_km_msl, hm20c_km_msl)) = env_heights_km_msl else {
        log::info!("{product:?}: no environmental heights — nothing to render");
        return None;
    };
    let env = crate::sounding::EnvHeights {
        h0c_km_msl,
        hm20c_km_msl,
        fetched_at: chrono::Utc::now(),
    };
    let radar_height_ft = crate::eet::radar_height_ft_near(radar_lat, radar_lon);
    let grids = crate::hail::compute_hail(scan, Some(&env), radar_height_ft)?;
    const MM_PER_IN: f32 = 25.4;
    let (grid, unit_scale) = match product {
        types::RadarProduct::MaxExpectedHailSize => (grids.mehs_mm, 1.0 / MM_PER_IN),
        _ => (grids.posh, 1.0),
    };
    let max_range = grid.range_bins as f64;
    let output = render_with_projection(
        radar_lat,
        radar_lon,
        max_range,
        product,
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
                    proj.render_gate(bufs, &ctx, r as f64 + 0.5, 1.0, *v * unit_scale, from);
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
/// `env_heights_km_msl` is the sounding's (0 °C, −20 °C) pair; `None`
/// falls back to the operational adaptation defaults, exactly as the RPG
/// runs without environmental data. The radar height comes from the
/// nearest-site table, as the EET render path's does; the radial-header
/// parameters a decoded `Scan` cannot carry come from
/// [`crate::kdp::KdpParams::render_fallback`] (fleet-typical `dbz0`/atmos —
/// without a `dbz0` the SNR gate reads every gate as no-echo and the
/// product would be blank) with the initial phase from the volume's own
/// estimator, the same fallback family the KDP render arm documents.
pub fn render_hhc_to_image(
    scan: &Scan,
    radar_lat: f64,
    radar_lon: f64,
    env_heights_km_msl: Option<(f64, f64)>,
) -> Option<(Vec<u8>, f64, Vec<f32>)> {
    let radar_km_msl = crate::eet::radar_height_ft_near(radar_lat, radar_lon) * 0.0003048;
    let params = crate::kdp::KdpParams {
        isdp_est_deg: crate::kdp::estimate_volume_isdp(scan),
        ..crate::kdp::KdpParams::render_fallback()
    };
    let (h0c, hsda) = match env_heights_km_msl {
        Some((h0c, hm20c)) => (
            h0c,
            crate::hca::HsdaHeights::from_env_heights(h0c, hm20c, radar_km_msl),
        ),
        None => (
            crate::hca::DEFAULT_HEIGHT_0_KM_MSL,
            crate::hca::HsdaHeights::operational_defaults(radar_km_msl),
        ),
    };
    let default_top_arl = (h0c - radar_km_msl).max(0.0);

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
    let ml =
        crate::hca::detect_melting_layer(&ml_sweeps, &params, default_top_arl, &hsda, Some(&cappi));
    let tilts = crate::hhc::volume_tilts(&all);
    let grid = crate::hhc::compute_hhc(&tilts, &params, &ml, &hsda, Some(&cappi))?;

    let max_gates = grid.values.iter().map(Vec::len).max().unwrap_or(0);
    let max_range = grid.first_gate_km + max_gates as f64 * grid.gate_interval_km;
    let output = render_with_projection(
        radar_lat,
        radar_lon,
        max_range,
        types::RadarProduct::HydrometeorClassification,
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
                    proj.render_gate(bufs, &ctx, range_km, grid.gate_interval_km, v, from);
                }
            });
        },
    );
    Some(output)
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
) -> Option<(Vec<u8>, f64, Vec<f32>)> {
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
    let actual_max_range = derived.first_gate_km + max_gates as f64 * derived.gate_interval_km;
    let avg_spacing_deg = 360.0 / n_radials as f64;

    let output = render_with_projection(
        radar_lat,
        radar_lon,
        actual_max_range,
        types::RadarProduct::SpecificDifferentialPhase,
        "KDP",
        |proj, bufs| {
            derived.values.par_iter().enumerate().for_each(|(i, row)| {
                let ctx = RadialContext::new(derived.azimuths_deg[i], avg_spacing_deg / 2.0);
                for (j, &v) in row.iter().enumerate() {
                    if v.is_nan() {
                        continue;
                    }
                    let range_km = derived.first_gate_km + j as f64 * derived.gate_interval_km;
                    if range_km > types::MAX_RANGE_KM {
                        break;
                    }
                    let from = GateId { radial: i, gate: j };
                    proj.render_gate(bufs, &ctx, range_km, derived.gate_interval_km, v, from);
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
pub fn render_level3_radial_to_image(
    radial_packet: &nexrad_level3::model::RadialPacket,
    product: types::RadarProduct,
    radar_lat: f64,
    radar_lon: f64,
    scale: f32,
    offset: f32,
    lut: Option<&[f32]>,
) -> Option<(Vec<u8>, f64, Vec<f32>)> {
    render_level3_radial_with_gate_km(
        radial_packet,
        radial_packet.gate_interval_km(),
        product,
        radar_lat,
        radar_lon,
        scale,
        offset,
        lut,
    )
}

/// [`render_level3_radial_to_image`] with the gate spacing chosen by the
/// caller. The message path passes the PDB's product-code override — some
/// products' packet-16 scale-factor halfword does not carry the gate size
/// (see `ProductDescriptionBlock::range_gate_km`) — so the first gate's range
/// is also re-derived from `first_range_bin` at the chosen spacing rather
/// than taken from the packet.
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
) -> Option<(Vec<u8>, f64, Vec<f32>)> {
    if radial_packet.radials.is_empty() {
        return None;
    }

    let first_gate_range = radial_packet.first_range_bin as f64 * gate_interval;
    let num_bins = radial_packet.num_range_bins as usize;
    let actual_max_range = first_gate_range + num_bins as f64 * gate_interval;

    let radials = &radial_packet.radials;

    let output = render_with_projection(
        radar_lat,
        radar_lon,
        actual_max_range,
        product,
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

                        let range_km = first_gate_range + gate_idx as f64 * gate_interval;
                        if range_km > types::MAX_RANGE_KM {
                            break;
                        }

                        let from = GateId {
                            radial: radial_idx,
                            gate: gate_idx,
                        };
                        proj.render_gate(bufs, &ctx, range_km, gate_interval, physical_value, from);
                    }
                });
        },
    );
    Some(output)
}

/// Render a storm-relative velocity field derived from dealiased Level III
/// velocity. See [`crate::srm`].
///
/// Separate from [`render_level3_message_to_image`] because the derived packet
/// is not what any product on the wire looks like: its gate values are knots on
/// a scale this crate chose, and its gate spacing comes from the source
/// product's code rather than from the packet.
pub fn render_derived_srm_to_image(
    derived: &crate::srm::DerivedSrm,
    radar_lat: f64,
    radar_lon: f64,
) -> Option<(Vec<u8>, f64, Vec<f32>)> {
    render_level3_radial_to_image(
        &derived.packet,
        types::RadarProduct::StormRelativeVelocity,
        radar_lat,
        radar_lon,
        derived.scale,
        derived.offset,
        None,
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
) -> Option<(Vec<u8>, f64, Vec<f32>)> {
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
    )
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use nexrad_level3::model::{Level3Message, ProductDescriptionBlock, RadialPacket, RadialRun};
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    const LAT: f64 = 35.3333;
    const LON: f64 = -97.2778;
    const SCALE: f32 = 2.0;
    const OFFSET: f32 = 66.0;
    const PRODUCT: types::RadarProduct = types::RadarProduct::Reflectivity;
    const N_RADIALS: usize = 360;
    const N_BINS: usize = 600;

    /// A spatially coherent reflectivity field — storm cores placed in (x, y)
    /// rather than a per-radial pattern, so neighbouring radials agree about
    /// as much as real ones do. `silence` drops one radial without renumbering
    /// the rest, which [`overlapping_radials_contend_for_pixels`] needs.
    fn packet(silence: Option<usize>) -> RadialPacket {
        let radials = (0..N_RADIALS)
            .map(|i| {
                let az = (i as f64).to_radians();
                let (s, c) = az.sin_cos();
                let gate_values = (0..N_BINS)
                    .map(|j| {
                        if silence == Some(i) {
                            return 0; // a gate value <= 1 is skipped
                        }
                        let r = j as f64 * 0.25;
                        let (x, y) = (r * s, r * c);
                        let core = |cx: f64, cy: f64, w: f64, amp: f64| {
                            let d2 = (x - cx).powi(2) + (y - cy).powi(2);
                            amp * (-d2 / (2.0 * w * w)).exp()
                        };
                        let dbz = 20.0
                            + core(40.0, 60.0, 18.0, 55.0)
                            + core(-70.0, -30.0, 25.0, 45.0)
                            + core(10.0, -90.0, 12.0, 60.0)
                            + 6.0 * (x / 30.0).sin() * (y / 30.0).cos();
                        ((dbz * SCALE as f64 + OFFSET as f64).round() as i64).clamp(2, 250) as u16
                    })
                    .collect();
                RadialRun {
                    start_angle: i as f32,
                    angle_delta: 1.0,
                    gate_values,
                }
            })
            .collect();
        RadialPacket {
            first_range_bin: 0,
            num_range_bins: N_BINS as u16,
            i_center: 0,
            j_center: 0,
            scale_factor: 4.0,
            is_legacy: false,
            xdr_data_scale: None,
            xdr_data_offset: None,
            radials,
        }
    }

    fn render(p: &RadialPacket) -> (Vec<u8>, Vec<f32>) {
        let (image, _, values) =
            render_level3_radial_to_image(p, PRODUCT, LAT, LON, SCALE, OFFSET, None).unwrap();
        (image, values)
    }

    fn digest(image: &[u8], values: &[f32]) -> u64 {
        let mut h = DefaultHasher::new();
        image.hash(&mut h);
        for v in values {
            v.to_bits().hash(&mut h);
        }
        h.finish()
    }

    /// The fixture has to actually paint, or everything below passes vacuously.
    #[test]
    fn fixture_covers_a_realistic_share_of_the_image() {
        let (image, values) = render(&packet(None));
        let painted = image.chunks_exact(4).filter(|px| px[3] != 0).count();
        let disc = std::f64::consts::PI * (N_BINS as f64 * 0.25 * types::PIXELS_PER_KM).powi(2);
        assert!(
            (painted as f64) > disc * 0.9 && (painted as f64) < disc * 1.1,
            "painted {painted}, expected about {disc:.0} for a {N_BINS}-gate disc"
        );
        assert!(values.iter().any(|v| !v.is_nan()));
    }

    /// Every value has to survive the trip through the cell exactly. The key
    /// shares those 64 bits, so anything that lets it reach the low half shows
    /// up here as a value the packet could not have encoded.
    #[test]
    fn values_round_trip_through_the_cell_unaltered() {
        let (_, values) = render(&packet(None));
        for &v in values.iter().filter(|v| !v.is_nan()) {
            let gate = v * SCALE + OFFSET;
            assert!(
                gate.fract() == 0.0 && (2.0..=250.0).contains(&gate),
                "value {v} is not (gate - {OFFSET}) / {SCALE} for any gate the fixture wrote"
            );
        }
    }

    /// Pins the *direction* of the tie-break, not just its stability. Two
    /// adjacent radials, the earlier one deliberately carrying the **larger**
    /// value: wherever both reach a pixel the later radial must take it, purely
    /// because it is later. Ranking by anything else — value, gate index, a
    /// constant key — hands some of those pixels to radial 0 instead.
    ///
    /// `both`, `only_first` and `only_second` are the value grids with both
    /// radials, with the second silenced, and with the first silenced.
    fn assert_later_radial_wins(
        both: &[f32],
        only_first: &[f32],
        only_second: &[f32],
        first_value: f32,
    ) {
        let contested = only_first
            .iter()
            .zip(only_second)
            .filter(|(a, b)| !a.is_nan() && !b.is_nan())
            .count();
        assert!(
            contested > 20,
            "only {contested} pixels are reached by both radials; this fixture cannot \
             observe the tie-break"
        );

        let stolen = both
            .iter()
            .zip(only_second)
            .filter(|(got, second)| !second.is_nan() && **got == first_value)
            .count();
        assert_eq!(
            stolen, 0,
            "{stolen} of {contested} contested pixels kept radial 0's value even though \
             radial 1 reached them; the later radial is no longer winning"
        );
    }

    #[test]
    fn level3_later_radial_wins_a_contested_pixel() {
        // Radial 0 carries the larger value on purpose.
        fn two_radials(first: u16, second: u16) -> RadialPacket {
            let run = |start: f32, gate: u16| RadialRun {
                start_angle: start,
                angle_delta: 1.0,
                gate_values: vec![gate; N_BINS],
            };
            RadialPacket {
                first_range_bin: 0,
                num_range_bins: N_BINS as u16,
                i_center: 0,
                j_center: 0,
                scale_factor: 4.0,
                is_legacy: false,
                xdr_data_scale: None,
                xdr_data_offset: None,
                radials: vec![run(90.0, first), run(91.0, second)],
            }
        }
        let grid = |first, second| render(&two_radials(first, second)).1;
        assert_later_radial_wins(
            &grid(200, 100),
            &grid(200, 0),
            &grid(0, 100),
            (200.0 - OFFSET) / SCALE,
        );
    }

    /// Guards the premise of the two determinism tests: radials really do land
    /// on each other's pixels, so a racy rasterizer would have something to
    /// race over. Silencing radial `k` hands every pixel it owned to whichever
    /// lower-keyed radial also wrote there, so a pixel painted both times but
    /// holding different values is one that two radials contended for.
    #[test]
    fn overlapping_radials_contend_for_pixels() {
        let (_, full) = render(&packet(None));
        let (_, cut) = render(&packet(Some(N_RADIALS / 2)));

        let contested = full
            .iter()
            .zip(&cut)
            .filter(|(a, b)| !a.is_nan() && !b.is_nan() && a.to_bits() != b.to_bits())
            .count();

        assert!(
            contested > 100,
            "only {contested} pixels contended; the fixture has stopped overlapping and \
             the determinism tests prove nothing"
        );
    }

    /// The property this module exists to pin: ten renders of one sweep across
    /// the whole rayon pool agree byte for byte.
    #[test]
    fn parallel_render_is_deterministic() {
        assert!(
            rayon::current_num_threads() > 1,
            "single-threaded pool: this test cannot observe a race"
        );
        let p = packet(None);
        let first = {
            let (i, v) = render(&p);
            digest(&i, &v)
        };
        for run in 1..10 {
            let (i, v) = render(&p);
            assert_eq!(digest(&i, &v), first, "render {run} differs from render 0");
        }
    }

    /// Stability alone would let the parallel path settle on an answer of its
    /// own. It has to settle on the sequential one.
    #[test]
    fn parallel_matches_single_thread() {
        let p = packet(None);
        let (i, v) = render(&p);
        let parallel = digest(&i, &v);

        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap();
        let sequential = pool.install(|| {
            let (i, v) = render(&p);
            digest(&i, &v)
        });

        assert_eq!(parallel, sequential);
    }

    /// Colour and value come out of one cell, so they cannot come from
    /// different gates. Two separate buffers used to let them.
    #[test]
    fn colour_agrees_with_value_at_every_pixel() {
        let (image, values) = render(&packet(None));
        for (idx, (px, &v)) in image.chunks_exact(4).zip(&values).enumerate() {
            let expected = if v.is_nan() {
                (0, 0, 0, 0)
            } else {
                get_color_for_value(PRODUCT, v)
            };
            assert_eq!(
                (px[0], px[1], px[2], px[3]),
                expected,
                "pixel {idx} holds a colour its value did not produce (value {v})"
            );
        }
    }

    // ── Level II and NROT ────────────────────────────────────────────────────
    //
    // These paths build their own keys and hand their own product to
    // `RenderBuffers`, and none of the Level III tests above reach them.

    const L2_ELEVATION: f32 = 0.5;

    /// A one-sweep Level II scan, one radial per entry in `gates`, spaced 1°
    /// from 90°. A radial whose byte is 0 decodes as below-threshold and is
    /// skipped, which silences it without renumbering the rest.
    fn l2_scan(gates: &[u8], velocity: bool) -> Scan {
        use nexrad_model::data::{
            MomentData, PulseWidth, RadialStatus, Sweep, VolumeCoveragePattern,
        };
        let radials = gates
            .iter()
            .enumerate()
            .map(|(i, &byte)| {
                let moment =
                    MomentData::from_fixed_point(600, 0, 250, 8, SCALE, OFFSET, vec![byte; 600]);
                let (refl, vel) = if velocity {
                    (None, Some(moment))
                } else {
                    (Some(moment), None)
                };
                Radial::new(
                    0,
                    i as u16,
                    90.0 + i as f32,
                    1.0,
                    RadialStatus::IntermediateRadialData,
                    1,
                    L2_ELEVATION,
                    refl,
                    vel,
                    None,
                    None,
                    None,
                    None,
                    None,
                )
            })
            .collect();
        Scan::new(
            VolumeCoveragePattern::new(
                212,
                0,
                0.5,
                PulseWidth::Short,
                false,
                0,
                false,
                0,
                false,
                false,
                0,
                false,
                false,
                Vec::new(),
            ),
            vec![Sweep::new(1, radials)],
        )
    }

    fn render_l2(gates: &[u8], product: types::RadarProduct) -> (Vec<u8>, Vec<f32>) {
        let scan = l2_scan(gates, product != types::RadarProduct::Reflectivity);
        let (image, _, values) =
            render_radar_to_image(&scan, L2_ELEVATION, product, LAT, LON).unwrap();
        (image, values)
    }

    #[test]
    fn level2_later_radial_wins_a_contested_pixel() {
        let grid = |g: &[u8]| render_l2(g, PRODUCT).1;
        assert_later_radial_wins(
            &grid(&[200, 100]),
            &grid(&[200, 0]),
            &grid(&[0, 100]),
            (200.0 - OFFSET) / SCALE,
        );
    }

    #[test]
    fn level2_colour_agrees_with_value_at_every_pixel() {
        let (image, values) = render_l2(&[200, 100, 180, 120], PRODUCT);
        assert!(
            values.iter().any(|v| !v.is_nan()),
            "level II fixture painted nothing"
        );
        for (px, &v) in image.chunks_exact(4).zip(&values) {
            let want = if v.is_nan() {
                (0, 0, 0, 0)
            } else {
                get_color_for_value(PRODUCT, v)
            };
            assert_eq!((px[0], px[1], px[2], px[3]), want);
        }
    }

    /// A velocity field with enough azimuthal shear to survive the LLSD fit,
    /// the range normalization, and the ±0.25 display threshold, so
    /// `render_nrot_to_image` actually paints.
    fn nrot_scan(n_radials: usize) -> Scan {
        use nexrad_model::data::{
            MomentData, PulseWidth, RadialStatus, Sweep, VolumeCoveragePattern,
        };
        let radials = (0..n_radials)
            .map(|i| {
                let theta = i as f64 / n_radials as f64 * std::f64::consts::TAU;
                // Byte 129 is 0 m/s at scale 2 / offset 129; ±8 cycles of
                // ±35 m/s gives shear well past the 0.5 display threshold.
                let ms = 35.0 * (8.0 * theta).sin();
                let byte = (129.0 + ms * 2.0).round().clamp(2.0, 254.0) as u8;
                Radial::new(
                    0,
                    i as u16,
                    i as f32 * (360.0 / n_radials as f32),
                    360.0 / n_radials as f32,
                    RadialStatus::IntermediateRadialData,
                    1,
                    L2_ELEVATION,
                    None,
                    Some(MomentData::from_fixed_point(
                        400,
                        0,
                        250,
                        8,
                        2.0,
                        129.0,
                        vec![byte; 400],
                    )),
                    None,
                    None,
                    None,
                    None,
                    None,
                )
            })
            .collect();
        Scan::new(
            VolumeCoveragePattern::new(
                212,
                0,
                0.5,
                PulseWidth::Short,
                false,
                0,
                false,
                0,
                false,
                false,
                0,
                false,
                false,
                Vec::new(),
            ),
            vec![Sweep::new(1, radials)],
        )
    }

    /// NROT hands `RenderBuffers` its own product literal, far from where
    /// `into_output` applies it. Rendering NROT through the reflectivity
    /// palette would look plausible and fail nothing else.
    #[test]
    fn nrot_colour_comes_from_the_nrot_palette() {
        let scan = nrot_scan(360);
        let (image, _, values) = render_radar_to_image(
            &scan,
            L2_ELEVATION,
            types::RadarProduct::NormalizedRotation,
            LAT,
            LON,
        )
        .unwrap();

        let painted = image.chunks_exact(4).filter(|px| px[3] != 0).count();
        assert!(
            painted > 10_000,
            "NROT fixture painted only {painted} pixels"
        );

        for (px, &v) in image.chunks_exact(4).zip(&values) {
            let want = if v.is_nan() {
                (0, 0, 0, 0)
            } else {
                get_color_for_value(types::RadarProduct::NormalizedRotation, v)
            };
            assert_eq!((px[0], px[1], px[2], px[3]), want);
        }
    }

    /// The NROT grid is indexed (azimuth, gate) like the others, and its key
    /// has to agree.
    ///
    /// Known gap: transposing this path's [`GateId`] survives the suite. The
    /// L2 and L3 equivalents die to their `later_radial_wins` tests, which need
    /// two adjacent radials carrying known, very different values — NROT has no
    /// such handle, since every value is an LLSD fit over its neighbours and
    /// the median filter deletes anything isolated enough to control. The
    /// named fields are the mitigation: a transposition there has to be
    /// written out in full rather than slipped in as argument order.
    #[test]
    fn nrot_render_is_deterministic() {
        let scan = nrot_scan(360);
        let once = || {
            let (image, _, values) = render_radar_to_image(
                &scan,
                L2_ELEVATION,
                types::RadarProduct::NormalizedRotation,
                LAT,
                LON,
            )
            .unwrap();
            digest(&image, &values)
        };
        let first = once();
        for run in 1..6 {
            assert_eq!(once(), first, "NROT render {run} differs from render 0");
        }

        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap();
        assert_eq!(
            pool.install(once),
            first,
            "NROT parallel differs from sequential"
        );
    }

    #[test]
    fn write_key_ranks_radial_major_and_never_reads_as_empty() {
        let k = |radial, gate| write_key(GateId { radial, gate });
        assert!(k(0, 0) > 0);
        assert!(k(0, 1) > k(0, 0));
        assert!(k(1, 0) > k(0, N_BINS));
        assert!(k(719, 1831) > k(718, 1831));
    }

    /// A minimal Level III message around one digital radial packet whose
    /// scale-factor halfword claims ~1 km gates — what product 163 really
    /// carries on the wire, where that halfword is the scan projection
    /// constant and not a gate size.
    fn message_with_lying_scale_factor(product_code: i16, bins: usize) -> Level3Message {
        use nexrad_level3::model::{DataLayer, MessageHeader, SymbologyBlock};

        let packet = RadialPacket {
            first_range_bin: 0,
            num_range_bins: bins as u16,
            i_center: 0,
            j_center: 0,
            // ~1 km per gate if believed.
            scale_factor: 0.999,
            is_legacy: false,
            xdr_data_scale: Some(SCALE),
            xdr_data_offset: Some(OFFSET),
            radials: (0..N_RADIALS)
                .map(|i| RadialRun {
                    start_angle: i as f32,
                    angle_delta: 1.0,
                    gate_values: vec![100; bins],
                })
                .collect(),
        };
        Level3Message {
            header: MessageHeader {
                message_code: product_code,
                date_of_message: 20661,
                time_of_message: 0,
                message_length: 0,
                source_id: 0,
                destination_id: 0,
                number_of_blocks: 3,
            },
            pdb: ProductDescriptionBlock {
                block_divider: -1,
                latitude: LAT,
                longitude: LON,
                height: 1000,
                product_code,
                operational_mode: 2,
                vcp: 212,
                sequence_number: 0,
                volume_scan_number: 1,
                volume_scan_date: 20661,
                volume_scan_time: 0,
                generation_date: 20661,
                generation_time: 0,
                product_specific_1: 0,
                product_specific_2: 0,
                elevation_number: 1,
                product_specific_3: 5,
                thresholds: [0; 16],
                product_specific_47_53: [0; 7],
                version: 0,
                spot_blank: 0,
                symbology_offset: 60,
                graphic_offset: 0,
                tabular_offset: 0,
            },
            symbology: Some(SymbologyBlock {
                block_id: 1,
                block_length: 0,
                num_layers: 1,
                layers: vec![DataLayer {
                    layer_length: 0,
                    packets: vec![nexrad_level3::model::DataPacket::DigitalRadial(packet)],
                }],
            }),
        }
    }

    /// Product 163's packet says ~1 km per gate; the ICD says 0.25 km. The
    /// display path has to prefer the PDB's override the way the
    /// twin-comparison path does, or the on-screen KDP field draws 4× too
    /// far out. A product without an override keeps the packet's own value.
    #[test]
    fn message_path_prefers_the_pdb_gate_spacing_over_the_packets() {
        const BINS: usize = 40;

        let (_, max_range, _) = render_level3_message_to_image(
            &message_with_lying_scale_factor(163, BINS),
            types::RadarProduct::SpecificDifferentialPhase,
            LAT,
            LON,
        )
        .unwrap();
        assert!(
            (max_range - BINS as f64 * 0.25).abs() < 1e-9,
            "163 must render at the ICD's 0.25 km spacing, got a max range of {max_range} km \
             from {BINS} gates"
        );

        let (_, max_range, _) = render_level3_message_to_image(
            &message_with_lying_scale_factor(94, BINS),
            PRODUCT,
            LAT,
            LON,
        )
        .unwrap();
        let packet_km = 1.0 / 0.999_f32 as f64;
        assert!(
            (max_range - BINS as f64 * packet_km).abs() < 1e-9,
            "a product with no PDB override must keep the packet's spacing, got a max range \
             of {max_range} km from {BINS} gates"
        );
    }
}
