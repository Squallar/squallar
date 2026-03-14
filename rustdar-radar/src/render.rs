use nexrad_model::data::{DataMoment, Radial, Scan};
use rayon::prelude::*;
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
            let dest_lat_rad = self.radar_lat_rad + dy_center / 6371.0;
            let cos_correction = self.cos_radar_lat / dest_lat_rad.cos();

            for az_step in 0..num_az_samples {
                let t = az_step as f64 * inv_num_az;
                let sin_az = ctx.sin_az_start + ctx.sin_az_delta * t;
                let cos_az = ctx.cos_az_start + ctx.cos_az_delta * t;

                let dx_km = r * sin_az;
                let dy_km = r * cos_az;
                let px_i =
                    (self.center_px + dx_km * cos_correction * types::PIXELS_PER_KM) as i32;
                let dest_lat_rad = self.radar_lat_rad + dy_km / 6371.0;
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
                .partial_cmp(&((*b - target_elevation).abs()))
                .unwrap()
        })
}

/// Find the sweep whose first radial matches `elevation_angle` and carries the
/// requested product's moment data.
fn find_sweep<'a>(
    scan: &'a Scan,
    product: types::RadarProduct,
    elevation_angle: f32,
) -> Option<&'a [Radial]> {
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
        return render_nrot_to_image(radials, radar_lat);
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
) -> Option<(Vec<u8>, f64, Vec<f32>)> {
    let num_radials = radials.len();
    if num_radials < 3 {
        return None;
    }

    let (vel_grid, azimuths_deg, gate_count, first_gate_range, gate_interval) =
        build_velocity_grid(radials)?;

    let actual_max_range = first_gate_range + gate_count as f64 * gate_interval;
    let avg_spacing_deg = 360.0 / num_radials as f64;

    let nrot_grid = compute_nrot_grid(&vel_grid, gate_count, first_gate_range, gate_interval, avg_spacing_deg);

    let output = render_with_projection(
        radar_lat, 0.0, actual_max_range, "NROT",
        |proj, bufs| {
            nrot_grid.par_iter().enumerate().for_each(|(i, nrot_row)| {
                let ctx = RadialContext::new(azimuths_deg[i], avg_spacing_deg / 2.0);

                for (j, &nrot_val) in nrot_row.iter().enumerate() {
                    if nrot_val.is_nan() {
                        continue;
                    }

                    let range_km = first_gate_range + j as f64 * gate_interval;
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

                    proj.render_gate(bufs, &ctx, range_km, gate_interval, scaled_value, color);
                }
            });
        },
    );
    Some(output)
}

/// Extract velocity data from Level II radials into a 2D grid (azimuth × range).
///
/// Returns `(vel_grid, azimuths_deg, gate_count, first_gate_range_km, gate_interval_km)`.
fn build_velocity_grid(
    radials: &[Radial],
) -> Option<(Vec<Vec<f64>>, Vec<f64>, usize, f64, f64)> {
    let first_vel = radials.iter().find_map(|r| r.velocity())?;
    let gate_count = first_vel.gate_count() as usize;
    let first_gate_range = first_vel.first_gate_range_km();
    let gate_interval = first_vel.gate_interval_km();

    let mut vel_grid: Vec<Vec<f64>> = Vec::with_capacity(radials.len());
    let mut azimuths_deg: Vec<f64> = Vec::with_capacity(radials.len());

    for radial in radials.iter() {
        azimuths_deg.push(radial.azimuth_angle_degrees() as f64);
        let mut gates = vec![f64::NAN; gate_count];
        if let Some(moment) = radial.velocity() {
            for (j, val) in moment.values().iter().enumerate().take(gate_count) {
                if let nexrad_model::data::MomentValue::Value(v) = val {
                    if !v.is_nan() && *v < 999.0 {
                        gates[j] = *v as f64;
                    }
                }
            }
        }
        vel_grid.push(gates);
    }

    Some((vel_grid, azimuths_deg, gate_count, first_gate_range, gate_interval))
}

/// Compute NROT (azimuthal shear × scale) from a velocity grid.
///
/// For each gate, computes the azimuthal velocity derivative using adjacent
/// azimuths, normalizes by range to remove beam-broadening effects, and
/// scales by `NROT_SCALE`.
fn compute_nrot_grid(
    vel_grid: &[Vec<f64>],
    gate_count: usize,
    first_gate_range: f64,
    gate_interval: f64,
    avg_spacing_deg: f64,
) -> Vec<Vec<f64>> {
    const NROT_SCALE: f64 = 250.0;
    const MIN_RANGE_KM: f64 = 5.0;

    let num_radials = vel_grid.len();
    let avg_spacing_rad = avg_spacing_deg.to_radians();

    (0..num_radials)
        .map(|i| {
            let i_prev = if i == 0 { num_radials - 1 } else { i - 1 };
            let i_next = if i == num_radials - 1 { 0 } else { i + 1 };

            (0..gate_count)
                .map(|j| {
                    let range_km = first_gate_range + j as f64 * gate_interval;
                    if range_km < MIN_RANGE_KM {
                        return f64::NAN;
                    }

                    let v_prev = vel_grid[i_prev][j];
                    let v_next = vel_grid[i_next][j];

                    if v_prev.is_nan() || v_next.is_nan() {
                        return f64::NAN;
                    }

                    let delta_v = v_next - v_prev;
                    let range_m = range_km * 1000.0;
                    let az_shear = delta_v / (range_m * 2.0 * avg_spacing_rad);
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

    // Build scale, offset, and optional LUT from the product description block
    let scale = l3_msg.pdb.data_scale();
    let offset = l3_msg.pdb.data_offset();
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
        "L3 {:?}: rendering with scale={}, offset={}, legacy={}, lut_len={:?}",
        product, scale, offset, rp.is_legacy, lut.map(|l| l.len())
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
    for i in 2..log_start.min(255) {
        lut[i] = (i as f32 - lin_offset) / lin_scale;
    }
    for i in log_start.min(255)..255 {
        lut[i] = ((i as f32 - log_offset) / log_scale).exp();
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
