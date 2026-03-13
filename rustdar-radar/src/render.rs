use nexrad_model::data::{DataMoment, Radial, Scan};
use rayon::prelude::*;
use std::f64::consts::PI;
use std::sync::atomic::{AtomicU32, Ordering};
use crate::palette::get_color_for_value;
use crate::types;

/// Render radar data to an image projected for geographic display
/// Returns (image_data, max_range_km, value_data) where:
/// - image_data: RGBA pixel data in geographic coordinate system
/// - max_range_km: actual radar range
/// - value_data: actual radar values at each pixel (f32::NAN for no data)
pub fn render_radar_to_image(
    data: &Scan,
    elevation_angle: f32,
    product: types::RadarProduct,
    radar_lat: f64,
    _radar_lon: f64,
) -> Option<(Vec<u8>, f64, Vec<f32>)> {
    // Find the sweep that matches the requested elevation angle.
    // Round the same way as load_scan_data (1 decimal place) and also verify
    // the sweep contains data for the requested product – split-cut sweeps at
    // the same nominal angle may carry different moment types.
    let target_sweep = data.sweeps().iter().find(|sweep| {
        sweep.radials().first()
            .map(|r| {
                let rounded = (r.elevation_angle_degrees() * 10.0).round() / 10.0;
                (rounded - elevation_angle).abs() < 0.05 && product.get_moment(r).is_some()
            })
            .unwrap_or(false)
    })?;

    // NROT is a derived product computed from velocity data
    if product == types::RadarProduct::NormalizedRotation {
        return render_nrot_to_image(target_sweep.radials(), radar_lat);
    }

    // Create atomic RGBA image buffer (initialized to transparent = 0)
    let image_buf: Vec<AtomicU32> = (0..types::IMAGE_SIZE * types::IMAGE_SIZE)
        .map(|_| AtomicU32::new(0))
        .collect();

    // Create atomic value data buffer (initialized to NaN)
    let value_buf: Vec<AtomicU32> = (0..types::IMAGE_SIZE * types::IMAGE_SIZE)
        .map(|_| AtomicU32::new(f32::NAN.to_bits()))
        .collect();

    // Pre-compute Web Mercator projection constants
    let radar_lat_rad = radar_lat.to_radians();
    let cos_radar_lat = radar_lat_rad.cos();
    let center_px = types::IMAGE_SIZE as f64 / 2.0;

    // Mercator Y mapping: precompute bounds and scale so pixel Y is linear in Mercator Y
    let merc_y_top = types::lat_rad_to_mercator_y(radar_lat_rad + types::MAX_RANGE_KM / 6371.0);
    let merc_y_bottom = types::lat_rad_to_mercator_y(radar_lat_rad - types::MAX_RANGE_KM / 6371.0);
    let merc_y_span = merc_y_top - merc_y_bottom;
    let merc_y_scale = types::IMAGE_SIZE as f64 / merc_y_span;

    // Get the radials from the target sweep
    let radials = target_sweep.radials();

    // Single-pass average azimuth spacing — no Vec allocation needed
    let mut prev_azimuth: Option<f64> = None;
    let mut spacing_sum = 0.0f64;
    let mut spacing_count = 0u32;
    for radial in radials.iter() {
        let az = radial.azimuth_angle_degrees() as f64;
        if let Some(prev) = prev_azimuth {
            let mut diff = az - prev;
            if diff < -180.0 { diff += 360.0; }
            else if diff > 180.0 { diff -= 360.0; }
            spacing_sum += diff;
            spacing_count += 1;
        }
        prev_azimuth = Some(az);
    }
    let avg_azimuth_spacing = if spacing_count > 0 {
        spacing_sum / spacing_count as f64
    } else {
        1.0
    };

    // Compute max range from gate parameters (same for all radials in a sweep)
    let actual_max_range = radials.iter()
        .find_map(|radial| {
            let moment = product.get_moment(radial)?;
            let gate_count = moment.gate_count() as usize;
            Some(moment.first_gate_range_km() + gate_count as f64 * moment.gate_interval_km())
        })
        .unwrap_or(0.0);

    // Process radials in parallel — each writes to atomic buffers
    radials.par_iter().for_each(|radial| {
        let azimuth = radial.azimuth_angle_degrees() as f64;

        let data_moment = product.get_moment(radial);

        if let Some(moment) = data_moment {
            let moment_values = moment.values();
            let first_gate_range = moment.first_gate_range_km();
            let gate_size = moment.gate_interval_km();

            let az_half_spacing = avg_azimuth_spacing / 2.0;
            let az_start_rad = (azimuth - az_half_spacing) * PI / 180.0;
            let az_end_rad = (azimuth + az_half_spacing) * PI / 180.0;
            let cos_az_center = (azimuth * PI / 180.0).cos();

            // Pre-compute azimuth edge sin/cos once per radial.
            // Linear interpolation over the ~0.5° span introduces < 0.00001 error.
            let (sin_az_start, cos_az_start) = az_start_rad.sin_cos();
            let (sin_az_end, cos_az_end) = az_end_rad.sin_cos();
            let sin_az_delta = sin_az_end - sin_az_start;
            let cos_az_delta = cos_az_end - cos_az_start;

            for (gate_idx, moment_value) in moment_values.iter().enumerate() {
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

                let range_start = range_km - (gate_size / 2.0);
                let range_end = range_km + (gate_size / 2.0);

                let num_range_samples =
                    ((range_end - range_start) * types::PIXELS_PER_KM).ceil() as i32 + 2;
                let num_az_samples = ((az_half_spacing * 2.0 * range_km * PI / 180.0)
                    * types::PIXELS_PER_KM)
                    .ceil() as i32
                    + 2;
                let inv_num_range = 1.0 / num_range_samples.max(1) as f64;
                let inv_num_az = 1.0 / num_az_samples.max(1) as f64;

                for r_step in 0..num_range_samples {
                    let r = range_start + (range_end - range_start) * (r_step as f64 * inv_num_range);

                    let dy_center = r * cos_az_center;
                    let dest_lat_rad = radar_lat_rad + dy_center / 6371.0;
                    let cos_correction = cos_radar_lat / dest_lat_rad.cos();

                    for az_step in 0..num_az_samples {
                        let t = az_step as f64 * inv_num_az;
                        let sin_az = sin_az_start + sin_az_delta * t;
                        let cos_az = cos_az_start + cos_az_delta * t;

                        let dx_km = r * sin_az;
                        let dy_km = r * cos_az;
                        let px_i = (center_px + dx_km * cos_correction * types::PIXELS_PER_KM) as i32;
                        // Web Mercator Y: compute destination latitude, convert to
                        // Mercator Y, then map linearly to pixel row.
                        let dest_lat_rad = radar_lat_rad + dy_km / 6371.0;
                        let dest_merc_y = types::lat_rad_to_mercator_y(dest_lat_rad);
                        let py_i = ((merc_y_top - dest_merc_y) * merc_y_scale) as i32;

                        if px_i >= 0 && px_i < types::IMAGE_SIZE as i32
                            && py_i >= 0 && py_i < types::IMAGE_SIZE as i32
                        {
                            let pixel_idx = py_i as usize * types::IMAGE_SIZE + px_i as usize;
                            let packed = u32::from_ne_bytes([color.0, color.1, color.2, color.3]);
                            image_buf[pixel_idx].store(packed, Ordering::Relaxed);
                            value_buf[pixel_idx].store(scaled_value.to_bits(), Ordering::Relaxed);
                        }
                    }
                }
            }
        }
    });

    // Convert atomic buffers to final output
    let image: Vec<u8> = image_buf.iter()
        .flat_map(|a| a.load(Ordering::Relaxed).to_ne_bytes())
        .collect();
    let value_data: Vec<f32> = value_buf.iter()
        .map(|a| f32::from_bits(a.load(Ordering::Relaxed)))
        .collect();

    let max_range = if actual_max_range > 0.0 {
        actual_max_range
    } else {
        types::MAX_RANGE_KM
    };

    log::info!(
        "Radar rendering complete: actual_max_range={:.1}km, using max_range={:.1}km",
        actual_max_range,
        max_range
    );

    Some((image, max_range, value_data))
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

    // Get velocity parameters from first radial with velocity data
    let first_vel = radials.iter().find_map(|r| r.velocity())?;
    let gate_count = first_vel.gate_count() as usize;
    let first_gate_range = first_vel.first_gate_range_km();
    let gate_interval = first_vel.gate_interval_km();
    let actual_max_range = first_gate_range + gate_count as f64 * gate_interval;

    // Build velocity grid: vel_grid[radial_idx][gate_idx] = velocity in m/s or NAN
    let mut vel_grid: Vec<Vec<f64>> = Vec::with_capacity(num_radials);
    let mut azimuths_deg: Vec<f64> = Vec::with_capacity(num_radials);

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

    // Average azimuth spacing
    let avg_spacing_deg = 360.0 / num_radials as f64;
    let avg_spacing_rad = avg_spacing_deg.to_radians();

    // Scaling factor: AzShear (1/s) * NROT_SCALE → unitless NROT
    // Calibrated so a moderate mesocyclone (~20 m/s ΔV at 80 km) ≈ 1.0
    const NROT_SCALE: f64 = 250.0;
    const MIN_RANGE_KM: f64 = 5.0;

    // Pre-compute NROT grid
    let nrot_grid: Vec<Vec<f64>> = (0..num_radials)
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
                    // AzShear = ΔV / (R × 2Δθ) in 1/s
                    let az_shear = delta_v / (range_m * 2.0 * avg_spacing_rad);

                    az_shear * NROT_SCALE
                })
                .collect()
        })
        .collect();

    // Create atomic image and value buffers
    let image_buf: Vec<AtomicU32> = (0..types::IMAGE_SIZE * types::IMAGE_SIZE)
        .map(|_| AtomicU32::new(0))
        .collect();
    let value_buf: Vec<AtomicU32> = (0..types::IMAGE_SIZE * types::IMAGE_SIZE)
        .map(|_| AtomicU32::new(f32::NAN.to_bits()))
        .collect();

    // Web Mercator projection constants
    let radar_lat_rad = radar_lat.to_radians();
    let cos_radar_lat = radar_lat_rad.cos();
    let center_px = types::IMAGE_SIZE as f64 / 2.0;
    let merc_y_top = types::lat_rad_to_mercator_y(radar_lat_rad + types::MAX_RANGE_KM / 6371.0);
    let merc_y_bottom = types::lat_rad_to_mercator_y(radar_lat_rad - types::MAX_RANGE_KM / 6371.0);
    let merc_y_span = merc_y_top - merc_y_bottom;
    let merc_y_scale = types::IMAGE_SIZE as f64 / merc_y_span;

    // Render NROT grid to image in parallel
    nrot_grid.par_iter().enumerate().for_each(|(i, nrot_row)| {
        let azimuth = azimuths_deg[i];
        let az_half_spacing = avg_spacing_deg / 2.0;
        let az_start_rad = (azimuth - az_half_spacing) * PI / 180.0;
        let az_end_rad = (azimuth + az_half_spacing) * PI / 180.0;
        let cos_az_center = (azimuth * PI / 180.0).cos();

        let (sin_az_start, cos_az_start) = az_start_rad.sin_cos();
        let (sin_az_end, cos_az_end) = az_end_rad.sin_cos();
        let sin_az_delta = sin_az_end - sin_az_start;
        let cos_az_delta = cos_az_end - cos_az_start;

        for (j, &nrot_val) in nrot_row.iter().enumerate() {
            if nrot_val.is_nan() {
                continue;
            }

            let range_km = first_gate_range + j as f64 * gate_interval;
            if range_km > types::MAX_RANGE_KM {
                break;
            }

            let scaled_value = nrot_val as f32;
            let color = get_color_for_value(types::RadarProduct::NormalizedRotation, scaled_value);
            if color.3 == 0 {
                continue;
            }

            let range_start = range_km - gate_interval / 2.0;
            let range_end = range_km + gate_interval / 2.0;

            let num_range_samples =
                ((range_end - range_start) * types::PIXELS_PER_KM).ceil() as i32 + 2;
            let num_az_samples = ((az_half_spacing * 2.0 * range_km * PI / 180.0)
                * types::PIXELS_PER_KM)
                .ceil() as i32
                + 2;
            let inv_num_range = 1.0 / num_range_samples.max(1) as f64;
            let inv_num_az = 1.0 / num_az_samples.max(1) as f64;

            for r_step in 0..num_range_samples {
                let r =
                    range_start + (range_end - range_start) * (r_step as f64 * inv_num_range);
                let dy_center = r * cos_az_center;
                let dest_lat_rad = radar_lat_rad + dy_center / 6371.0;
                let cos_correction = cos_radar_lat / dest_lat_rad.cos();

                for az_step in 0..num_az_samples {
                    let t = az_step as f64 * inv_num_az;
                    let sin_az = sin_az_start + sin_az_delta * t;
                    let cos_az = cos_az_start + cos_az_delta * t;

                    let dx_km = r * sin_az;
                    let dy_km = r * cos_az;
                    let px_i = (center_px + dx_km * cos_correction * types::PIXELS_PER_KM) as i32;
                    let dest_lat_rad = radar_lat_rad + dy_km / 6371.0;
                    let dest_merc_y = types::lat_rad_to_mercator_y(dest_lat_rad);
                    let py_i = ((merc_y_top - dest_merc_y) * merc_y_scale) as i32;

                    if px_i >= 0
                        && px_i < types::IMAGE_SIZE as i32
                        && py_i >= 0
                        && py_i < types::IMAGE_SIZE as i32
                    {
                        let pixel_idx = py_i as usize * types::IMAGE_SIZE + px_i as usize;
                        let packed =
                            u32::from_ne_bytes([color.0, color.1, color.2, color.3]);
                        image_buf[pixel_idx].store(packed, Ordering::Relaxed);
                        value_buf[pixel_idx]
                            .store(scaled_value.to_bits(), Ordering::Relaxed);
                    }
                }
            }
        }
    });

    let image: Vec<u8> = image_buf
        .iter()
        .flat_map(|a| a.load(Ordering::Relaxed).to_ne_bytes())
        .collect();
    let value_data: Vec<f32> = value_buf
        .iter()
        .map(|a| f32::from_bits(a.load(Ordering::Relaxed)))
        .collect();

    let max_range = if actual_max_range > 0.0 {
        actual_max_range
    } else {
        types::MAX_RANGE_KM
    };

    log::info!(
        "NROT rendering complete: actual_max_range={:.1}km, using max_range={:.1}km",
        actual_max_range,
        max_range
    );

    Some((image, max_range, value_data))
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
    _radar_lon: f64,
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

    // Create atomic RGBA image buffer (initialized to transparent = 0)
    let image_buf: Vec<AtomicU32> = (0..types::IMAGE_SIZE * types::IMAGE_SIZE)
        .map(|_| AtomicU32::new(0))
        .collect();

    // Create atomic value data buffer (initialized to NaN)
    let value_buf: Vec<AtomicU32> = (0..types::IMAGE_SIZE * types::IMAGE_SIZE)
        .map(|_| AtomicU32::new(f32::NAN.to_bits()))
        .collect();

    // Pre-compute Web Mercator projection constants
    let radar_lat_rad = radar_lat.to_radians();
    let cos_radar_lat = radar_lat_rad.cos();
    let center_px = types::IMAGE_SIZE as f64 / 2.0;

    let merc_y_top = types::lat_rad_to_mercator_y(radar_lat_rad + types::MAX_RANGE_KM / 6371.0);
    let merc_y_bottom = types::lat_rad_to_mercator_y(radar_lat_rad - types::MAX_RANGE_KM / 6371.0);
    let merc_y_span = merc_y_top - merc_y_bottom;
    let merc_y_scale = types::IMAGE_SIZE as f64 / merc_y_span;

    let radials = &radial_packet.radials;

    // Debug: count total renderable gates across all radials
    {
        let mut total_gates = 0usize;
        let mut below_thresh = 0usize;
        let mut nan_or_999 = 0usize;
        let mut out_of_range = 0usize;
        let mut transparent_color = 0usize;
        let mut rendered = 0usize;
        for radial_run in radials.iter() {
            let bins_to_render = radial_run.gate_values.len().min(num_bins);
            for (gate_idx, &gate_value) in radial_run.gate_values[..bins_to_render].iter().enumerate() {
                total_gates += 1;
                if gate_value <= 1 {
                    below_thresh += 1;
                    continue;
                }
                let physical_value = if let Some(table) = lut {
                    let idx = gate_value as usize;
                    if idx < table.len() { table[idx] } else { f32::NAN }
                } else {
                    (gate_value as f32 - offset) / scale
                };
                // SRV products (both legacy N0S and digital N*U) output knots;
                // velocity_color() expects m/s.
                let physical_value = if matches!(product, types::RadarProduct::StormRelativeVelocity) {
                    physical_value * 0.514444
                } else {
                    physical_value
                };
                if physical_value.is_nan() || physical_value >= 999.0 {
                    nan_or_999 += 1;
                    continue;
                }
                let range_km = first_gate_range + gate_idx as f64 * gate_interval;
                if range_km > types::MAX_RANGE_KM {
                    out_of_range += 1;
                    continue;
                }
                let color = get_color_for_value(product, physical_value);
                if color.3 == 0 {
                    transparent_color += 1;
                    continue;
                }
                rendered += 1;
            }
        }
        log::debug!(
            "L3 {:?} gate stats: total={}, below_thresh(<=1)={}, nan_or_999={}, out_of_range(>{:.0}km)={}, transparent_color={}, renderable={}",
            product, total_gates, below_thresh, nan_or_999, types::MAX_RANGE_KM, out_of_range, transparent_color, rendered
        );
        // Log a few sample physical values from the first radial with data
        if let Some(r0) = radials.first() {
            let mut samples = Vec::new();
            let bins = r0.gate_values.len().min(num_bins);
            for (i, &gv) in r0.gate_values[..bins].iter().enumerate() {
                if gv > 1 && samples.len() < 5 {
                    let pv = if let Some(table) = lut {
                        let idx = gv as usize;
                        if idx < table.len() { table[idx] } else { f32::NAN }
                    } else {
                        (gv as f32 - offset) / scale
                    };
                    samples.push(format!("gate[{}]: raw={} -> phys={:.3}", i, gv, pv));
                }
            }
            log::debug!("L3 {:?} sample values: {:?}", product, samples);
        }
    }

    // Process radials in parallel
    radials.par_iter().for_each(|radial_run| {
        let azimuth = radial_run.start_angle as f64 + radial_run.angle_delta as f64 / 2.0;
        let az_half_spacing = radial_run.angle_delta as f64 / 2.0;
        let az_start_rad = (azimuth - az_half_spacing) * PI / 180.0;
        let az_end_rad = (azimuth + az_half_spacing) * PI / 180.0;
        let cos_az_center = (azimuth * PI / 180.0).cos();

        let (sin_az_start, cos_az_start) = az_start_rad.sin_cos();
        let (sin_az_end, cos_az_end) = az_end_rad.sin_cos();
        let sin_az_delta = sin_az_end - sin_az_start;
        let cos_az_delta = cos_az_end - cos_az_start;

        let bins_to_render = radial_run.gate_values.len().min(num_bins);
        for (gate_idx, &gate_value) in radial_run.gate_values[..bins_to_render].iter().enumerate() {
            // Gate value 0 and 1 are typically "below threshold" and "range folded"
            if gate_value <= 1 {
                continue;
            }

            let physical_value = if let Some(table) = lut {
                let idx = gate_value as usize;
                if idx < table.len() { table[idx] } else { f32::NAN }
            } else {
                (gate_value as f32 - offset) / scale
            };
            // SRV products (both legacy N0S and digital N*U) output knots;
            // velocity_color() expects m/s.
            let physical_value = if matches!(product, types::RadarProduct::StormRelativeVelocity) {
                physical_value * 0.514444
            } else {
                physical_value
            };
            if physical_value.is_nan() || physical_value >= 999.0 {
                continue;
            }

            let range_km = first_gate_range + gate_idx as f64 * gate_interval;
            if range_km > types::MAX_RANGE_KM {
                break;
            }

            let color = get_color_for_value(product, physical_value);

            let range_start = range_km - gate_interval / 2.0;
            let range_end = range_km + gate_interval / 2.0;

            let num_range_samples =
                ((range_end - range_start) * types::PIXELS_PER_KM).ceil() as i32 + 2;
            let num_az_samples = ((az_half_spacing * 2.0 * range_km * PI / 180.0)
                * types::PIXELS_PER_KM)
                .ceil() as i32
                + 2;
            let inv_num_range = 1.0 / num_range_samples.max(1) as f64;
            let inv_num_az = 1.0 / num_az_samples.max(1) as f64;

            for r_step in 0..num_range_samples {
                let r = range_start + (range_end - range_start) * (r_step as f64 * inv_num_range);
                let dy_center = r * cos_az_center;
                let dest_lat_rad = radar_lat_rad + dy_center / 6371.0;
                let cos_correction = cos_radar_lat / dest_lat_rad.cos();

                for az_step in 0..num_az_samples {
                    let t = az_step as f64 * inv_num_az;
                    let sin_az = sin_az_start + sin_az_delta * t;
                    let cos_az = cos_az_start + cos_az_delta * t;

                    let dx_km = r * sin_az;
                    let dy_km = r * cos_az;
                    let px_i = (center_px + dx_km * cos_correction * types::PIXELS_PER_KM) as i32;
                    let dest_lat_rad = radar_lat_rad + dy_km / 6371.0;
                    let dest_merc_y = types::lat_rad_to_mercator_y(dest_lat_rad);
                    let py_i = ((merc_y_top - dest_merc_y) * merc_y_scale) as i32;

                    if px_i >= 0
                        && px_i < types::IMAGE_SIZE as i32
                        && py_i >= 0
                        && py_i < types::IMAGE_SIZE as i32
                    {
                        let pixel_idx = py_i as usize * types::IMAGE_SIZE + px_i as usize;
                        let packed = u32::from_ne_bytes([color.0, color.1, color.2, color.3]);
                        image_buf[pixel_idx].store(packed, Ordering::Relaxed);
                        value_buf[pixel_idx]
                            .store(physical_value.to_bits(), Ordering::Relaxed);
                    }
                }
            }
        }
    });

    // Convert atomic buffers to final output
    let image: Vec<u8> = image_buf
        .iter()
        .flat_map(|a| a.load(Ordering::Relaxed).to_ne_bytes())
        .collect();
    let value_data: Vec<f32> = value_buf
        .iter()
        .map(|a| f32::from_bits(a.load(Ordering::Relaxed)))
        .collect();

    let max_range = if actual_max_range > 0.0 {
        actual_max_range
    } else {
        types::MAX_RANGE_KM
    };

    log::info!(
        "Level III rendering complete: actual_max_range={:.1}km, using max_range={:.1}km",
        actual_max_range,
        max_range
    );

    Some((image, max_range, value_data))
}
