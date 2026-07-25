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
}
use std::f64::consts::PI;
use std::sync::atomic::{AtomicU32, Ordering};
use crate::palette::get_color_for_value;
use crate::types;

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
/// Load-bearing on native: `render_gate` runs under a `par_iter` over radials
/// and two radials routinely land on the same pixel. The overlap is *defined*
/// but not *deterministic* — the last relaxed store wins, and which radial
/// stores last is scheduling, so a native render differs between runs (five
/// runs over one KTLX sweep, five hashes; `RAYON_NUM_THREADS=1`, one hash).
/// Anything byte-comparing a native render must pin the thread count. wasm32 is
/// single-threaded and so already reproducible.
///
/// The atomics are *not* load-bearing on wasm32, so cfg-splitting that arm to a
/// plain buffer looks like a free win. It was measured per component, not
/// assumed, against a real KTLX 0.5° reflectivity sweep (720 radials × 1832
/// gates) at `IMAGE_SIZE` 1024, release, rasterizer isolated from WebGL/winit:
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
    let radials = find_sweep(data, product, elevation_angle)?;

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

/// Render NROT (Normalized Rotation): azimuthal shear derived from Level II
/// velocity, normalized by range to remove beam broadening and scaled to a
/// unitless field where >1.0 is significant and >2.5 extreme.
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

/// Velocity as a 2D grid (azimuth × range).
struct VelocityGrid {
    vel_grid: Vec<Vec<f64>>,
    azimuths_deg: Vec<f64>,
    gate_count: usize,
    first_gate_range_km: f64,
    gate_interval_km: f64,
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
                    && !v.is_nan() && *v < 999.0 {
                        gates[j] = *v as f64;
                    }
            }
        }
        vel_grid.push(gates);
    }

    Some(VelocityGrid { vel_grid, azimuths_deg, gate_count, first_gate_range_km, gate_interval_km })
}

/// Drop isolated noise: a gate survives only if at least `MIN_COHERENT` of its
/// 24 neighbours (±2 azimuth × ±2 range, centre excluded) are non-NaN and share
/// its sign. Noise has alternating signs; real rotation couplets are spatially
/// coherent.
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

/// Compute NROT via LLSD (Linear Least Squares Derivative): per gate, fit
/// `V = a + b*θ` over the velocities within `NEIGHBORHOOD_KM`, where θ is the
/// azimuthal offset in radians; `b / range` is azimuthal shear (1/s), scaled by
/// `NROT_SCALE`.
///
/// Averaging over the ~50-100 gate pairs this reaches at typical ranges is why
/// no separate pre- or post-smoothing is needed.
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

                    // Azimuths that fit in the neighborhood at this range.
                    let az_spacing_rad = 2.0 * PI / num_radials as f64;
                    let arc_per_az_km = range_km * az_spacing_rad;
                    let az_reach = (NEIGHBORHOOD_KM / arc_per_az_km).ceil() as i32;

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
                    // Well above f64 epsilon (~2.2e-16): rejects near-singular
                    // systems before catastrophic cancellation.
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
                product, rp.radials.len(), rp.num_range_bins, rp.is_legacy, rp.scale_factor
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

/// Build a 256-entry look-up table for Digital VIL (product 134), `None` for
/// anything else.
///
/// VIL is a hybrid linear + logarithmic mapping encoded in NEXRAD 16-bit floats
/// (not IEEE-754). Thresholds 0..5 carry lin_scale, lin_offset, log_start,
/// log_scale, log_offset; gates 2..log_start are linear, log_start..254
/// exponential.
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
/// For legacy products (e.g. code 56 SRM) each threshold `u16` carries flag
/// bits in the high byte and the value in the low byte. `NaN` marks a level
/// that is not displayable.
fn decode_legacy_thresholds(pdb: &nexrad_level3::model::ProductDescriptionBlock) -> [f32; 16] {
    let mut lut = [f32::NAN; 16];
    for (i, &t) in pdb.thresholds.iter().enumerate() {
        let codes = (t >> 8) as u8;
        let mut val = (t & 0xFF) as f32;

        if codes & 0x80 != 0 {
            // Blank, TH (below threshold), ND (no data) or RF (range folded).
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

/// Decode a NEXRAD-specific 16-bit float: sign (bit 15), exponent (14–10),
/// fraction (9–0).
/// `value = (-1)^sign × 2^(exp − 16) × (1 + frac/1024)` when exp ≠ 0,
/// `value = (-1)^sign × frac / 512` when exp = 0.
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

/// Level III gate byte to physical value, via LUT or scale/offset. SRV is
/// converted knots → m/s.
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
