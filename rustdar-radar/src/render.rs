use nexrad_model::data::{DataMoment, Radial, Scan};
#[cfg(not(target_arch = "wasm32"))]
use rayon::prelude::*;

#[cfg(target_arch = "wasm32")]
use seq_fallback::*;

/// Sequential stand-ins for the two rayon entry points this module uses.
///
/// wasm32-unknown-unknown is single-threaded: rayon compiles there but cannot
/// build a thread pool, so the parallel iterators are not an option. This keeps
/// the *call sites* identical rather than cfg'ing four rasterization loops,
/// which is what would actually rot — the loops are the hot path and the two
/// copies would drift.
///
/// The closures need no changes: rayon requires `Fn + Send + Sync`, which is
/// strictly stronger than the `FnMut` these want, so anything that satisfied
/// rayon satisfies this.
///
/// Native keeps rayon. This is a cfg split, not a removal — radar
/// rasterization is the hot path on desktop and a sequential fallback silently
/// becoming the native arm would be a large regression that no test notices.
#[cfg(target_arch = "wasm32")]
mod seq_fallback {
    /// Stands in for `rayon::prelude::ParallelSlice::par_iter`.
    ///
    /// Implemented on `[T]` only; `Vec<T>` reaches it through deref.
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
}
use std::f64::consts::PI;
use std::sync::atomic::{AtomicU32, Ordering};
use crate::palette::get_color_for_value;
use crate::types;

// ── Shared rendering infrastructure ──────────────────────────────────────────

/// Pre-computed Web Mercator projection constants for a radar station.
///
/// Derived from [`types::ImageBounds`] so the rendered pixel grid aligns
/// exactly with the bounds reported to the UI layer.
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

    /// Render a single radar gate cell into the atomic buffers.
    fn render_gate(
        &self,
        bufs: &RenderBuffers,
        ctx: &RadialContext,
        range_km: f64,
        gate_interval: f64,
        value: f32,
        color: (u8, u8, u8, u8),
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

        let packed = u32::from_ne_bytes([color.0, color.1, color.2, color.3]);
        let value_bits = value.to_bits();

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
                let px_i =
                    (self.center_px + dx_km * cos_correction * types::PIXELS_PER_KM) as i32;
                let dest_lat_rad = self.radar_lat_rad + dy_km / types::EARTH_RADIUS_KM;
                let dest_merc_y = types::lat_rad_to_mercator_y(dest_lat_rad);
                let py_i = ((self.merc_y_top - dest_merc_y) * self.merc_y_scale) as i32;

                if px_i >= 0
                    && px_i < types::IMAGE_SIZE as i32
                    && py_i >= 0
                    && py_i < types::IMAGE_SIZE as i32
                {
                    let pixel_idx = py_i as usize * types::IMAGE_SIZE + px_i as usize;
                    bufs.image[pixel_idx].store(packed, Ordering::Relaxed);
                    bufs.values[pixel_idx].store(value_bits, Ordering::Relaxed);
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

/// Paired atomic image and value buffers for parallel rendering.
///
/// The atomics are load-bearing on native: `render_gate` is reached from a
/// `par_iter` over radials, and two radials routinely land on the same pixel.
///
/// They are *not* load-bearing on wasm32, which is single-threaded — the
/// `seq_fallback` above exists for exactly that reason — so a cfg-split to
/// `Vec<u32>` there is an obvious-looking win, and was measured rather than
/// assumed. It is not worth taking. Against a real KTLX 0.5° reflectivity
/// sweep (720 radials × 1832 gates) at `IMAGE_SIZE` 1024, in a release build
/// with the rasterizer isolated from WebGL and winit:
///
/// | what                                   | Firefox | Chromium |
/// |----------------------------------------|--------:|---------:|
/// | whole render                           |  233 ms |   261 ms |
/// | 28 M relaxed `store` vs plain `Vec<u32>`| 39 / 37 |  47 / 48 |
/// | `into_output` shape, atomic vs plain   | 0.8/0.4 |  0.7/0.3 |
/// | `RenderBuffers::new`, atomic vs plain  | 0.2/0.3 |  0.3/0.2 |
///
/// That totals roughly 2.5 ms of a 233 ms frame — about 1%, and the same 1% in
/// both browsers. The cost of the split is two divergent buffer types under one
/// hot loop; the return does not pay for it.
///
/// The same measurements dispose of the theory that Firefox's 5.7× penalty on
/// `radar-render` comes from these atomics. It does not: Firefox rasterizes
/// this sweep *faster* than Chromium does, and relaxed atomic stores cost it
/// 5% over plain ones. See `rustdar-web`'s crate docs for what is and is not
/// still open there.
///
/// Where the frame actually goes is the per-sample transcendental in
/// `types::lat_rad_to_mercator_y` — `(π/4 + lat/2).tan().ln()`, once per
/// azimuth sample. 28 M of those cost 660 ms in Firefox and 597 ms in Chromium
/// against 29 ms and 37 ms for the same loop without them, which puts this one
/// call at most of the render on both. Reducing it means changing the arithmetic,
/// and every pixel of the output is a function of that arithmetic, so it is not
/// a change that can be made bit-identical — hence not made here.
struct RenderBuffers {
    image: Vec<AtomicU32>,
    values: Vec<AtomicU32>,
}

impl RenderBuffers {
    fn new() -> Self {
        let n = types::IMAGE_SIZE * types::IMAGE_SIZE;
        Self {
            image: (0..n).map(|_| AtomicU32::new(0)).collect(),
            values: (0..n).map(|_| AtomicU32::new(f32::NAN.to_bits())).collect(),
        }
    }

    fn into_output(self, actual_max_range: f64) -> (Vec<u8>, f64, Vec<f32>) {
        let image: Vec<u8> = self
            .image
            .iter()
            .flat_map(|a| a.load(Ordering::Relaxed).to_ne_bytes())
            .collect();
        let value_data: Vec<f32> = self
            .values
            .iter()
            .map(|a| f32::from_bits(a.load(Ordering::Relaxed)))
            .collect();
        let max_range = if actual_max_range > 0.0 {
            actual_max_range
        } else {
            types::MAX_RANGE_KM
        };
        (image, max_range, value_data)
    }
}

// ── Sweep / azimuth helpers ──────────────────────────────────────────────────

/// Find the closest available elevation angle in a scan for the given product.
///
/// Iterates all sweeps, rounds each elevation to 1 decimal place, keeps those
/// that carry the requested product's moment data, and returns the one closest
/// to `target_elevation`. Used by the loop renderer to snap the user's
/// selected elevation to what's actually available in each historical scan.
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
        .min_by(|a, b| {
            ((*a - target_elevation).abs())
                .total_cmp(&((*b - target_elevation).abs()))
        })
}

/// Find the sweep whose first radial matches `elevation_angle` and carries the
/// requested product's moment data.
fn find_sweep(
    scan: &Scan,
    product: types::RadarProduct,
    elevation_angle: f32,
) -> Option<&[Radial]> {
    scan.sweeps().iter().find_map(|sweep| {
        let matches = sweep
            .radials()
            .first()
            .map(|r| {
                let rounded = (r.elevation_angle_degrees() * 10.0).round() / 10.0;
                (rounded - elevation_angle).abs() < 0.05 && product.get_moment(r).is_some()
            })
            .unwrap_or(false);
        matches.then(|| sweep.radials())
    })
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

/// Set up rendering infrastructure (projection + buffers), call the rendering
/// closure, then convert buffers to output and log completion.
fn render_with_projection(
    radar_lat: f64,
    radar_lon: f64,
    actual_max_range: f64,
    label: &str,
    fill: impl FnOnce(&MercatorProjection, &RenderBuffers),
) -> (Vec<u8>, f64, Vec<f32>) {
    let bounds = types::ImageBounds::from_radar_site(radar_lat, radar_lon);
    let proj = MercatorProjection::from_bounds(radar_lat, &bounds);
    let bufs = RenderBuffers::new();

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

/// Render radar data to an image projected for geographic display.
/// Returns (image_data, max_range_km, value_data) where:
/// - image_data: RGBA pixel data in geographic coordinate system
/// - max_range_km: actual radar range
/// - value_data: actual radar values at each pixel (f32::NAN for no data)
pub fn render_radar_to_image(
    data: &Scan,
    elevation_angle: f32,
    product: types::RadarProduct,
    radar_lat: f64,
    radar_lon: f64,
) -> Option<(Vec<u8>, f64, Vec<f32>)> {
    let radials = find_sweep(data, product, elevation_angle)?;

    // NROT is a derived product computed from velocity data
    if product == types::RadarProduct::NormalizedRotation {
        return render_nrot_to_image(radials, radar_lat, radar_lon);
    }

    let avg_azimuth_spacing = compute_azimuth_spacing(radials);
    let actual_max_range = compute_max_range(radials, product);

    let output = render_with_projection(
        radar_lat, radar_lon, actual_max_range, "Radar",
        |proj, bufs| {
            radials.par_iter().for_each(|radial| {
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

                        let color = get_color_for_value(product, scaled_value);
                        proj.render_gate(bufs, &ctx, range_km, gate_size, scaled_value, color);
                    }
                }
            });
        },
    );
    Some(output)
}

/// Render NROT (Normalized Rotation) to an image.
///
/// Computes azimuthal shear from Level II velocity data and normalizes by range.
/// The algorithm:
/// 1. Extracts velocity values into a 2D grid (azimuth × range)
/// 2. For each gate, computes the azimuthal velocity derivative using adjacent azimuths
/// 3. Normalizes by range to remove beam-broadening effects
/// 4. Scales to produce unitless values where >1.0 is significant, >2.5 is extreme
fn render_nrot_to_image(
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
    let azimuths_rad: Vec<f64> = vg.azimuths_deg.iter().map(|d| d.to_radians()).collect();
    let avg_spacing_deg = 360.0 / num_radials as f64;

    let nrot_grid = compute_nrot_grid(&vg.vel_grid, vg.gate_count, vg.first_gate_range_km, vg.gate_interval_km, &azimuths_rad);

    let nrot_grid = filter_nrot_grid(&nrot_grid, vg.gate_count);

    let output = render_with_projection(
        radar_lat, radar_lon, actual_max_range, "NROT",
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

                    let scaled_value = nrot_val as f32;
                    let color = get_color_for_value(
                        types::RadarProduct::NormalizedRotation,
                        scaled_value,
                    );
                    if color.3 == 0 {
                        continue;
                    }

                    proj.render_gate(bufs, &ctx, range_km, vg.gate_interval_km, scaled_value, color);
                }
            });
        },
    );
    Some(output)
}

/// Extracted velocity data organized as a 2D grid (azimuth × range).
struct VelocityGrid {
    vel_grid: Vec<Vec<f64>>,
    azimuths_deg: Vec<f64>,
    gate_count: usize,
    first_gate_range_km: f64,
    gate_interval_km: f64,
}

/// Extract velocity data from Level II radials into a 2D grid (azimuth × range).
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
                    && !v.is_nan() && *v < 999.0 {
                        gates[j] = *v as f64;
                    }
            }
        }
        vel_grid.push(gates);
    }

    Some(VelocityGrid { vel_grid, azimuths_deg, gate_count, first_gate_range_km, gate_interval_km })
}

/// Filter the NROT grid to remove isolated noise pixels.
///
/// For each gate, requires at least `MIN_COHERENT` of the 25 neighbors
/// (±2 azimuth × ±2 range) to be non-NaN and share the same sign as the
/// center. Random noise has alternating signs and gets suppressed; real
/// rotation couplets are spatially coherent and survive.
fn filter_nrot_grid(nrot_grid: &[Vec<f64>], gate_count: usize) -> Vec<Vec<f64>> {
    const HALF: i32 = 2;
    const MIN_COHERENT: usize = 8;

    let num_radials = nrot_grid.len() as i32;

    (0..num_radials as usize)
        .map(|i| {
            (0..gate_count)
                .map(|j| {
                    let center = nrot_grid[i][j];
                    if center.is_nan() {
                        return f64::NAN;
                    }
                    let center_positive = center > 0.0;
                    let mut count = 0usize;

                    for da in -HALF..=HALF {
                        let ai = ((i as i32 + da).rem_euclid(num_radials)) as usize;
                        for dr in -HALF..=HALF {
                            if da == 0 && dr == 0 {
                                continue;
                            }
                            let rj = j as i32 + dr;
                            if rj < 0 || rj >= gate_count as i32 {
                                continue;
                            }
                            let v = nrot_grid[ai][rj as usize];
                            if !v.is_nan() && (v > 0.0) == center_positive {
                                count += 1;
                            }
                        }
                    }

                    if count >= MIN_COHERENT {
                        center
                    } else {
                        f64::NAN
                    }
                })
                .collect()
        })
        .collect()
}

/// Compute NROT via LLSD (Linear Least Squares Derivative).
///
/// For each gate, collects all velocity values within a circular neighborhood
/// of `NEIGHBORHOOD_KM` radius and fits a linear regression V = a + b*θ
/// where θ is the azimuthal offset in radians. The slope b is dV/dθ;
/// dividing by range gives azimuthal shear (1/s), then scaled by `NROT_SCALE`.
///
/// This approach averages over ~50-100 gate pairs per point (at typical ranges),
/// providing inherent noise suppression far superior to a 2-point central
/// difference. No separate pre/post smoothing is needed.
fn compute_nrot_grid(
    vel_grid: &[Vec<f64>],
    gate_count: usize,
    first_gate_range: f64,
    gate_interval: f64,
    azimuths_rad: &[f64],
) -> Vec<Vec<f64>> {
    const NROT_SCALE: f64 = 250.0;
    const MIN_RANGE_KM: f64 = 10.0;
    const NEIGHBORHOOD_KM: f64 = 2.0;
    const MIN_POINTS: usize = 10;

    let num_radials = vel_grid.len();
    let rng_reach = (NEIGHBORHOOD_KM / gate_interval).ceil() as i32;

    (0..num_radials)
        .into_par_iter()
        .map(|i| {
            let center_az = azimuths_rad[i];

            (0..gate_count)
                .map(|j| {
                    let range_km = first_gate_range + j as f64 * gate_interval;
                    if range_km < MIN_RANGE_KM {
                        return f64::NAN;
                    }

                    // Number of azimuths that fit within the neighborhood at this range
                    let az_spacing_rad = 2.0 * PI / num_radials as f64;
                    let arc_per_az_km = range_km * az_spacing_rad;
                    let az_reach = (NEIGHBORHOOD_KM / arc_per_az_km).ceil() as i32;

                    // LLSD: fit V = a + b*θ via least squares
                    let mut sum_t = 0.0_f64;
                    let mut sum_v = 0.0_f64;
                    let mut sum_t2 = 0.0_f64;
                    let mut sum_tv = 0.0_f64;
                    let mut n = 0usize;

                    for da in -az_reach..=az_reach {
                        let ai =
                            ((i as i32 + da).rem_euclid(num_radials as i32)) as usize;
                        let mut dtheta = azimuths_rad[ai] - center_az;
                        if dtheta > PI {
                            dtheta -= 2.0 * PI;
                        }
                        if dtheta < -PI {
                            dtheta += 2.0 * PI;
                        }

                        let az_dist_km = (range_km * dtheta).abs();

                        for dr in -rng_reach..=rng_reach {
                            let rj = j as i32 + dr;
                            if rj < 0 || rj >= gate_count as i32 {
                                continue;
                            }

                            let rng_dist_km = (dr as f64 * gate_interval).abs();
                            let dist_sq =
                                az_dist_km * az_dist_km + rng_dist_km * rng_dist_km;
                            if dist_sq > NEIGHBORHOOD_KM * NEIGHBORHOOD_KM {
                                continue;
                            }

                            let v = vel_grid[ai][rj as usize];
                            if v.is_nan() {
                                continue;
                            }

                            sum_t += dtheta;
                            sum_v += v;
                            sum_t2 += dtheta * dtheta;
                            sum_tv += dtheta * v;
                            n += 1;
                        }
                    }

                    if n < MIN_POINTS {
                        return f64::NAN;
                    }

                    let nf = n as f64;
                    let denom = nf * sum_t2 - sum_t * sum_t;
                    // 1e-10 threshold: well above f64 epsilon (~2.2e-16) to reject
                    // near-singular least-squares systems before catastrophic cancellation
                    if denom.abs() < 1e-10 {
                        return f64::NAN;
                    }

                    // slope = dV/dθ (m/s per radian)
                    let slope = (nf * sum_tv - sum_t * sum_v) / denom;

                    // azimuthal shear = slope / range (1/s)
                    let range_m = range_km * 1000.0;
                    let az_shear = slope / range_m;

                    az_shear * NROT_SCALE
                })
                .collect()
        })
        .collect()
}

/// Render a Level III radial product to an image projected for geographic display.
///
/// Uses the same Web Mercator projection and atomic-buffer approach as
/// [`render_radar_to_image`], but reads from a Level III [`RadialPacket`]
/// instead of a Level II `Scan`.
///
/// For digital products, `scale` and `offset` convert raw gate bytes to physical
/// values: `physical = (gate_byte - offset) / scale`.
///
/// When `lut` is provided it overrides scale/offset: the gate value is used as
/// an index directly.  This covers both legacy 4-bit products (16-entry LUT)
/// and special digital products like VIL (256-entry LUT).
pub fn render_level3_radial_to_image(
    radial_packet: &nexrad_level3::model::RadialPacket,
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

    let gate_interval = radial_packet.gate_interval_km();
    let first_gate_range = radial_packet.first_gate_range_km();
    let num_bins = radial_packet.num_range_bins as usize;
    let actual_max_range = first_gate_range + num_bins as f64 * gate_interval;

    let radials = &radial_packet.radials;

    let output = render_with_projection(
        radar_lat, radar_lon, actual_max_range, "Level III",
        |proj, bufs| {
            radials.par_iter().for_each(|radial_run| {
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

                    let color = get_color_for_value(product, physical_value);
                    proj.render_gate(bufs, &ctx, range_km, gate_interval, physical_value, color);
                }
            });
        },
    );
    Some(output)
}

/// Render a Level III message to an image, extracting render parameters from the
/// message's symbology block and product description block.
///
/// This is the high-level entry point for Level III rendering that encapsulates
/// all nexrad-level3 internal knowledge (radial packet extraction, scale/offset,
/// LUT building). Callers only need to provide the decoded message, product type,
/// and radar location.
pub fn render_level3_message_to_image(
    l3_msg: &nexrad_level3::model::Level3Message,
    product: types::RadarProduct,
    radar_lat: f64,
    radar_lon: f64,
) -> Option<(Vec<u8>, f64, Vec<f32>)> {
    use nexrad_level3::model::DataPacket;

    // Extract radial packet from symbology layers
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
                product, rp.radials.len(), rp.num_range_bins, rp.is_legacy, rp.scale_factor
            );
            rp
        }
        None => {
            log::warn!("L3 {:?}: no radial packet found in symbology!", product);
            return None;
        }
    };

    // Build scale, offset, and optional LUT from the product description block.
    // Prefer XDR-derived scale/offset from packet 28 attributes when available,
    // since PDB thresholds don't encode IEEE-float values for some products
    // (e.g. 134 DVL, 135 EET).
    let scale = rp.xdr_data_scale.unwrap_or_else(|| l3_msg.pdb.data_scale());
    let offset = rp.xdr_data_offset.unwrap_or_else(|| l3_msg.pdb.data_offset());
    let vil_lut = build_vil_lut(&l3_msg.pdb);
    let legacy_lut;
    let lut: Option<&[f32]> = if vil_lut.is_some() {
        vil_lut.as_deref()
    } else if rp.is_legacy {
        legacy_lut = decode_legacy_thresholds(&l3_msg.pdb);
        Some(legacy_lut.as_slice())
    } else {
        None
    };

    log::debug!(
        "L3 {:?}: rendering with scale={}, offset={}, legacy={}, lut_len={:?}, xdr_scale={:?}, xdr_offset={:?}",
        product, scale, offset, rp.is_legacy, lut.map(|l| l.len()), rp.xdr_data_scale, rp.xdr_data_offset
    );

    render_level3_radial_to_image(rp, product, radar_lat, radar_lon, scale, offset, lut)
}

/// Build a 256-entry look-up table for Digital VIL (product 134).
///
/// VIL uses a hybrid linear + logarithmic mapping encoded with
/// NEXRAD-specific 16-bit floats (not IEEE-754).  The first five
/// thresholds carry: lin_scale, lin_offset, log_start, log_scale,
/// log_offset.  Gate values 2..log_start use a linear formula;
/// gate values log_start..254 use an exponential formula.
///
/// Returns `None` when the product code is not 134.
fn build_vil_lut(pdb: &nexrad_level3::model::ProductDescriptionBlock) -> Option<Vec<f32>> {
    if pdb.product_code != 134 {
        return None;
    }
    let lin_scale = nexrad_float16(pdb.thresholds[0]);
    let lin_offset = nexrad_float16(pdb.thresholds[1]);
    let log_start = pdb.thresholds[2] as usize;
    let log_scale = nexrad_float16(pdb.thresholds[3]);
    let log_offset = nexrad_float16(pdb.thresholds[4]);

    let mut lut = vec![f32::NAN; 256];
    // Gate 0 = below threshold, gate 1 = range folded → NaN
    for (i, slot) in lut.iter_mut().enumerate().take(log_start.min(255)).skip(2) {
        *slot = (i as f32 - lin_offset) / lin_scale;
    }
    for (i, slot) in lut.iter_mut().enumerate().take(255).skip(log_start.min(255)) {
        *slot = ((i as f32 - log_offset) / log_scale).exp();
    }
    // Gate 255 is reserved
    Some(lut)
}

/// Decode the 16 legacy data level thresholds into physical values.
///
/// For legacy products (e.g., code 56 SRM), each threshold `u16` encodes
/// a physical value with flag bits in the high byte and the numeric value
/// in the low byte. Returns a 16-element array where `NaN` means the
/// level is not displayable (blank, threshold, no data, or range-folded).
fn decode_legacy_thresholds(pdb: &nexrad_level3::model::ProductDescriptionBlock) -> [f32; 16] {
    let mut lut = [f32::NAN; 16];
    for (i, &t) in pdb.thresholds.iter().enumerate() {
        let codes = (t >> 8) as u8;
        let mut val = (t & 0xFF) as f32;

        if codes & 0x80 != 0 {
            // Special category: Blank, TH (below threshold),
            // ND (no data), RF (range folded) → not displayable
            continue;
        } else if codes & 0x40 != 0 {
            val *= 0.01;
        } else if codes & 0x20 != 0 {
            val *= 0.05;
        } else if codes & 0x10 != 0 {
            val *= 0.1;
        }

        if codes & 0x01 != 0 {
            val = -val;
        }

        lut[i] = val;
    }
    lut
}

/// Decode a NEXRAD-specific 16-bit floating-point value.
///
/// Format: sign (bit 15), exponent (bits 14–10), fraction (bits 9–0).
/// value = (-1)^sign × 2^(exp − 16) × (1 + frac/1024)  when exp ≠ 0
/// value = (-1)^sign × frac / 512                        when exp = 0
fn nexrad_float16(raw: u16) -> f32 {
    let frac = (raw & 0x03FF) as f32;
    let exp = ((raw >> 10) & 0x1F) as i32;
    let sign = raw >> 15;
    let value = if exp != 0 {
        2f32.powi(exp - 16) * (1.0 + frac / 1024.0)
    } else {
        frac / 512.0
    };
    if sign != 0 { -value } else { value }
}

/// Convert a Level III gate byte to a physical value, applying LUT or scale/offset
/// and the knots→m/s conversion for SRV products.
fn l3_physical_value(
    gate_value: u16,
    product: types::RadarProduct,
    scale: f32,
    offset: f32,
    lut: Option<&[f32]>,
) -> f32 {
    let v = if let Some(table) = lut {
        let idx = gate_value as usize;
        if idx < table.len() {
            table[idx]
        } else {
            f32::NAN
        }
    } else {
        (gate_value as f32 - offset) / scale
    };
    if matches!(product, types::RadarProduct::StormRelativeVelocity) {
        v * 0.514444
    } else {
        v
    }
}
