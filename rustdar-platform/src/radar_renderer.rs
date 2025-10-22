use nexrad::model::DataFile;
use rustdar_egui::actions::RadarProduct;
use std::f32::consts::PI;

const IMAGE_SIZE: usize = 1800; // 1800x1800 pixels for radar image
const MAX_RANGE_KM: f32 = 230.0; // NEXRAD max range ~230km

/// Render radar data to an RGBA image
/// Returns (image_data, max_range_km, value_data) where:
/// - image_data: RGBA pixel data
/// - max_range_km: actual radar range
/// - value_data: actual radar values at each pixel (f32::NAN for no data)
pub fn render_radar_to_image(
    data: &DataFile,
    elevation_angle: f32,
    product: RadarProduct,
) -> Option<(Vec<u8>, f32, Vec<f32>)> {
    // Find the elevation scan that matches the requested angle
    let elevation_scan = data.elevation_scans().values().find(|radials| {
        radials
            .first()
            .map(|r| (r.header().elev() - elevation_angle).abs() < 0.01)
            .unwrap_or(false)
    })?;

    // Create RGBA image buffer (initialized to transparent)
    let mut image = vec![0u8; IMAGE_SIZE * IMAGE_SIZE * 4];

    // Create value data buffer (initialized to NaN for no data)
    let mut value_data = vec![f32::NAN; IMAGE_SIZE * IMAGE_SIZE];

    let mut actual_max_range = 0.0f32;

    // elevation_scan is already a Vec<&Message31>, no need to collect
    let radials = elevation_scan;

    // Calculate azimuth angles for all radials
    let azimuths: Vec<f32> = radials.iter().map(|r| r.header().azm()).collect();

    // Calculate azimuth spacing (average angular difference between radials)
    let mut azimuth_diffs = Vec::new();
    for i in 1..azimuths.len() {
        let mut diff = azimuths[i] - azimuths[i - 1];
        // Handle wrap-around at 360/0 degrees
        if diff < -180.0 {
            diff += 360.0;
        } else if diff > 180.0 {
            diff -= 360.0;
        }
        azimuth_diffs.push(diff);
    }
    let avg_azimuth_spacing = if !azimuth_diffs.is_empty() {
        azimuth_diffs.iter().sum::<f32>() / azimuth_diffs.len() as f32
    } else {
        1.0 // Default to 1 degree if can't calculate
    };

    // Process each radial (sweep angle)
    for radial in radials.iter() {
        let azimuth = radial.header().azm(); // degrees

        // Get the data moment for the selected product
        let data_moment = match product {
            RadarProduct::Reflectivity => radial.reflectivity_data(),
            RadarProduct::Velocity => radial.velocity_data(),
            RadarProduct::SpectrumWidth => radial.sw_data(),
            RadarProduct::DifferentialReflectivity => radial.zdr_data(),
            RadarProduct::CorrelationCoefficient => radial.rho_data(),
            RadarProduct::DifferentialPhase => radial.phi_data(),
            RadarProduct::ClutterFilterPower => radial.cfp_data(),
        };

        if let Some(moment) = data_moment {
            let generic_data = moment.data();
            let gate_count = generic_data.number_data_moment_gates() as usize;
            let first_gate_range = generic_data.data_moment_range() as f32 / 1000.0; // meters to km
            let gate_size = generic_data.data_moment_range_sample_interval() as f32 / 1000.0; // meters to km
            let scale = generic_data.scale();
            let offset = generic_data.offset();

            // Calculate actual max range for this moment
            let moment_max_range = first_gate_range + (gate_count as f32 * gate_size);
            actual_max_range = actual_max_range.max(moment_max_range);

            // Get raw moment data
            let moment_data = moment.moment_data();

            // Process each gate (range bin)
            for gate_idx in 0..gate_count {
                let range_km = first_gate_range + (gate_idx as f32 * gate_size);

                if range_km > MAX_RANGE_KM {
                    break;
                }

                // Get raw value and convert to scaled value
                if gate_idx < moment_data.len() {
                    let raw_value = moment_data[gate_idx];

                    // Check for special values (0 or 1 typically mean no data or range folded)
                    // Also check for other common no-data values
                    if raw_value == 0 || raw_value == 1 || raw_value == 255 {
                        continue;
                    }

                    // Convert raw value to scaled value
                    // Different products use different scaling formulas
                    let scaled_value = if product == RadarProduct::Velocity {
                        // For velocity: (raw - offset) * scale (centers around zero)
                        ((raw_value as f32) - offset) * scale
                    } else if product == RadarProduct::Reflectivity {
                        // For reflectivity: (raw - offset) / scale
                        // This is the correct NEXRAD Level II formula for reflectivity
                        ((raw_value as f32) - offset) / scale
                    } else {
                        // For other products, use the standard formula
                        (raw_value as f32 * scale) + offset
                    };

                    // Skip if no valid data
                    if scaled_value >= 999.0 {
                        continue;
                    }

                    // Get color for this value
                    let color = get_color_for_value(product, scaled_value);

                    // Calculate azimuth edges (halfway between radials)
                    let az_half_spacing = avg_azimuth_spacing / 2.0;
                    let az_start = azimuth - az_half_spacing;
                    let az_end = azimuth + az_half_spacing;

                    // Calculate range edges (halfway between gates)
                    let range_start = range_km - (gate_size / 2.0);
                    let range_end = range_km + (gate_size / 2.0);

                    // Draw filled quadrilateral for this data cell
                    // We need to fill all pixels within the cell defined by:
                    // - Radial edges: az_start to az_end
                    // - Range edges: range_start to range_end

                    let pixels_per_km = (IMAGE_SIZE as f32) / (2.0 * MAX_RANGE_KM);

                    // Calculate the four corners of the cell in polar coordinates
                    // Then convert each to cartesian and fill the area
                    let az_start_rad = az_start * PI / 180.0;
                    let az_end_rad = az_end * PI / 180.0;

                    // Sample multiple points along the radial to fill the cell properly
                    let num_range_samples =
                        ((range_end - range_start) * pixels_per_km).ceil() as i32 + 2;
                    let num_az_samples = ((az_end - az_start).abs() * range_km * PI / 180.0
                        * pixels_per_km)
                        .ceil() as i32
                        + 2;

                    for r_step in 0..num_range_samples {
                        let r_frac = r_step as f32 / num_range_samples.max(1) as f32;
                        let r = range_start + (range_end - range_start) * r_frac;

                        for az_step in 0..num_az_samples {
                            let az_frac = az_step as f32 / num_az_samples.max(1) as f32;
                            let az_rad = az_start_rad + (az_end_rad - az_start_rad) * az_frac;

                            // Convert to cartesian
                            let x = r * az_rad.sin();
                            let y = -r * az_rad.cos();

                            // Convert to pixel coordinates
                            let px = ((IMAGE_SIZE as f32 / 2.0) + x * pixels_per_km) as i32;
                            let py = ((IMAGE_SIZE as f32 / 2.0) + y * pixels_per_km) as i32;

                            // Check bounds and set pixel
                            if px >= 0
                                && px < IMAGE_SIZE as i32
                                && py >= 0
                                && py < IMAGE_SIZE as i32
                            {
                                let pixel_idx = (py as usize * IMAGE_SIZE) + px as usize;
                                let rgba_idx = pixel_idx * 4;
                                if rgba_idx + 3 < image.len() {
                                    image[rgba_idx] = color.0;
                                    image[rgba_idx + 1] = color.1;
                                    image[rgba_idx + 2] = color.2;
                                    image[rgba_idx + 3] = color.3;

                                    // Store the actual value at this pixel
                                    value_data[pixel_idx] = scaled_value;
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Use the calculated max range, or fall back to a default if no data was found
    let max_range = if actual_max_range > 0.0 {
        actual_max_range
    } else {
        MAX_RANGE_KM // fallback to constant
    };

    log::info!(
        "Radar rendering complete: actual_max_range={:.1}km, using max_range={:.1}km",
        actual_max_range,
        max_range
    );

    Some((image, max_range, value_data))
}

/// Get RGBA color for a radar value based on product type
fn get_color_for_value(product: RadarProduct, value: f32) -> (u8, u8, u8, u8) {
    match product {
        RadarProduct::Reflectivity => reflectivity_color(value),
        RadarProduct::Velocity => velocity_color(value),
        RadarProduct::SpectrumWidth => spectrum_width_color(value),
        RadarProduct::DifferentialReflectivity => zdr_color(value),
        RadarProduct::CorrelationCoefficient => rho_color(value),
        RadarProduct::DifferentialPhase => phi_color(value),
        RadarProduct::ClutterFilterPower => cfp_color(value),
    }
}

/// Reflectivity color scale (dBZ)
/// Range: 0-95 dBZ with proper meteorological color scale
fn reflectivity_color(dbz: f32) -> (u8, u8, u8, u8) {
    // Log some values to debug the color mapping
    if dbz > 100.0 || dbz < -10.0 {
        log::debug!("Unusual reflectivity value: {:.2} dBZ", dbz);
    }

    // Handle invalid values
    if dbz.is_nan() || dbz.is_infinite() {
        return (0, 0, 0, 0); // Transparent
    }

    // Values below 0 dBZ should be transparent (no precipitation)
    if dbz < 0.0 {
        return (0, 0, 0, 0); // Transparent
    }

    let (r, g, b) = if dbz < 5.0 {
        // Grey (very light precipitation)
        let intensity = (dbz / 5.0 * 128.0) as u8;
        (intensity, intensity, intensity)
    } else if dbz < 10.0 {
        // Transition from grey to light blue
        let t = (dbz - 5.0) / 5.0; // 0 to 1
        let grey = (128.0 * (1.0 - t)) as u8;
        let blue = (200.0 * t) as u8;
        (grey, grey + blue / 2, grey + blue)
    } else if dbz < 15.0 {
        // Light blue
        (100, 150, 255)
    } else if dbz < 20.0 {
        // Blue
        (0, 100, 255)
    } else if dbz < 25.0 {
        // Dark blue
        (0, 50, 200)
    } else if dbz < 30.0 {
        // Green
        (0, 255, 0)
    } else if dbz < 35.0 {
        // Dark green
        (0, 200, 0)
    } else if dbz < 40.0 {
        // Yellow
        (255, 255, 0)
    } else if dbz < 45.0 {
        // Orange
        (255, 165, 0)
    } else if dbz < 50.0 {
        // Red
        (255, 0, 0)
    } else if dbz < 55.0 {
        // Dark red
        (200, 0, 0)
    } else if dbz < 60.0 {
        // Pink
        (255, 192, 203)
    } else if dbz < 65.0 {
        // Hot pink
        (255, 105, 180)
    } else if dbz < 70.0 {
        // Purple
        (128, 0, 128)
    } else if dbz < 75.0 {
        // Dark purple
        (75, 0, 130)
    } else if dbz < 80.0 {
        // Sky blue (hail signature)
        (135, 206, 235)
    } else if dbz < 85.0 {
        // Light blue
        (173, 216, 230)
    } else if dbz < 90.0 {
        // Orange (extreme)
        (255, 140, 0)
    } else if dbz < 95.0 {
        // Dark orange
        (255, 69, 0)
    } else {
        // White (extreme values above 95 dBZ)
        (255, 255, 255)
    };

    (r, g, b, 200) // More opaque for better visibility
}

/// Velocity color scale (m/s)
/// Custom velocity color scheme with clear visual gradients
/// Negative = inbound (toward radar), Positive = outbound (away from radar)
/// Note: Input is in m/s, thresholds are converted from mph
fn velocity_color(velocity_ms: f32) -> (u8, u8, u8, u8) {
    // Convert m/s to mph for threshold comparisons
    let velocity_mph = velocity_ms * 2.23694;

    let (r, g, b) = if !(-142.0..=141.0).contains(&velocity_mph) {
        // Range folded / extreme values - stark purple
        (128, 0, 128)
    } else if (-5.0..=5.0).contains(&velocity_mph) {
        // Near zero / calm - grey
        (128, 128, 128)
    }
    // Positive (outbound - moving away from radar)
    else if velocity_mph > 125.0 {
        (139, 69, 19) // Brown
    } else if velocity_mph > 100.0 {
        (255, 140, 0) // Orange
    } else if velocity_mph > 80.0 {
        (255, 218, 185) // Peach
    } else if velocity_mph > 55.0 {
        (255, 192, 203) // Pink
    } else if velocity_mph > 35.0 {
        (255, 0, 0) // Bright red
    } else if velocity_mph > 20.0 {
        (139, 0, 0) // Dark red
    } else if velocity_mph > 5.0 {
        // Gradient from grey to dark red
        let t = (velocity_mph - 5.0) / 15.0; // 0 to 1
        let r = (128.0 + (139.0 - 128.0) * t) as u8;
        let g = (128.0 * (1.0 - t)) as u8;
        let b = (128.0 * (1.0 - t)) as u8;
        (r, g, b)
    }
    // Negative (inbound - moving toward radar)
    else if velocity_mph < -125.0 {
        (255, 0, 255) // Fuchsia
    } else if velocity_mph < -100.0 {
        (0, 0, 255) // Blue
    } else if velocity_mph < -80.0 {
        (135, 206, 235) // Sky blue
    } else if velocity_mph < -55.0 {
        (173, 216, 230) // Light blue
    } else if velocity_mph < -35.0 {
        (0, 255, 0) // Bright green
    } else if velocity_mph < -20.0 {
        (0, 100, 0) // Dark green
    } else {
        // Gradient from dark green to grey (-20 to -5 mph)
        let t = (velocity_mph + 20.0) / 15.0; // 0 to 1
        let r = (128.0 * t) as u8;
        let g = (100.0 + (128.0 - 100.0) * t) as u8;
        let b = (128.0 * t) as u8;
        (r, g, b)
    };

    (r, g, b, 200)
}

/// Spectrum width color scale (m/s)
fn spectrum_width_color(sw: f32) -> (u8, u8, u8, u8) {
    if sw < 0.0 {
        return (0, 0, 0, 0);
    }

    let (r, g, b) = if sw < 2.0 {
        (0, 100, 0) // Dark green (low turbulence)
    } else if sw < 4.0 {
        (0, 200, 0) // Green
    } else if sw < 6.0 {
        (255, 255, 0) // Yellow
    } else if sw < 8.0 {
        (255, 150, 0) // Orange
    } else if sw < 10.0 {
        (255, 0, 0) // Red
    } else {
        (150, 0, 0) // Dark red (high turbulence)
    };

    (r, g, b, 180)
}

/// Differential reflectivity color scale (dB)
fn zdr_color(zdr: f32) -> (u8, u8, u8, u8) {
    let (r, g, b) = if zdr < -1.0 {
        (100, 0, 100) // Purple
    } else if zdr < 0.0 {
        (0, 100, 255) // Blue
    } else if zdr < 1.0 {
        (0, 255, 0) // Green
    } else if zdr < 2.0 {
        (255, 255, 0) // Yellow
    } else if zdr < 3.0 {
        (255, 150, 0) // Orange
    } else {
        (255, 0, 0) // Red
    };

    (r, g, b, 180)
}

/// Correlation coefficient color scale (0-1)
fn rho_color(rho: f32) -> (u8, u8, u8, u8) {
    if rho < 0.0 {
        return (0, 0, 0, 0);
    }

    let (r, g, b) = if rho < 0.7 {
        (255, 0, 0) // Red (low correlation - non-meteorological)
    } else if rho < 0.8 {
        (255, 150, 0) // Orange
    } else if rho < 0.9 {
        (255, 255, 0) // Yellow
    } else if rho < 0.95 {
        (0, 255, 0) // Green
    } else {
        (0, 150, 0) // Dark green (high correlation - pure rain)
    };

    (r, g, b, 180)
}

/// Differential phase color scale (degrees)
fn phi_color(phi: f32) -> (u8, u8, u8, u8) {
    let normalized = ((phi % 360.0) + 360.0) % 360.0;

    let (r, g, b) = if normalized < 60.0 {
        (255, 0, 0) // Red
    } else if normalized < 120.0 {
        (255, 255, 0) // Yellow
    } else if normalized < 180.0 {
        (0, 255, 0) // Green
    } else if normalized < 240.0 {
        (0, 255, 255) // Cyan
    } else if normalized < 300.0 {
        (0, 0, 255) // Blue
    } else {
        (255, 0, 255) // Magenta
    };

    (r, g, b, 180)
}

/// Clutter filter power color scale (dB)
fn cfp_color(cfp: f32) -> (u8, u8, u8, u8) {
    let (r, g, b) = if cfp < -30.0 {
        (0, 0, 100) // Dark blue
    } else if cfp < -20.0 {
        (0, 100, 255) // Blue
    } else if cfp < -10.0 {
        (0, 255, 255) // Cyan
    } else if cfp < 0.0 {
        (0, 255, 0) // Green
    } else if cfp < 10.0 {
        (255, 255, 0) // Yellow
    } else {
        (255, 0, 0) // Red
    };

    (r, g, b, 180)
}
