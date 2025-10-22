use chrono::NaiveDateTime;
use log;
use nexrad::model::DataFile;
use std::f32::consts::PI;
use std::collections::HashMap;
use crate::sites::RadarSite;
use crate::palette::get_color_for_value;

const IMAGE_SIZE: usize = 1800; // 1800x1800 pixels for radar image
const MAX_RANGE_KM: f32 = 230.0; // NEXRAD max range ~230km

/// Information about a loaded radar scan
#[derive(Debug, Clone)]
pub struct ScanInfo {
    /// The radar site code
    pub site: RadarSite,
    /// The actual timestamp of the scan data
    pub timestamp: NaiveDateTime,
    /// Available products in this scan
    pub available_products: Vec<RadarProduct>,
    /// Map of product to available elevation angles (sorted)
    pub product_elevations: HashMap<RadarProduct, Vec<f32>>,
    /// Status message
    pub status: String,
}

/// Radar product types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RadarProduct {
    Reflectivity,
    Velocity,
    SpectrumWidth,
    DifferentialPhase,
    CorrelationCoefficient,
    DifferentialReflectivity,
    ClutterFilterPower,
}

impl RadarProduct {
    pub fn code(&self) -> &'static str {
        match self {
            RadarProduct::Reflectivity => "ref",
            RadarProduct::Velocity => "vel",
            RadarProduct::SpectrumWidth => "sw",
            RadarProduct::DifferentialPhase => "phi",
            RadarProduct::CorrelationCoefficient => "rho",
            RadarProduct::DifferentialReflectivity => "zdr",
            RadarProduct::ClutterFilterPower => "cfp",
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            RadarProduct::Reflectivity => "Reflectivity",
            RadarProduct::Velocity => "Velocity",
            RadarProduct::SpectrumWidth => "Spectrum Width",
            RadarProduct::DifferentialPhase => "Differential Phase",
            RadarProduct::CorrelationCoefficient => "Correlation Coefficient",
            RadarProduct::DifferentialReflectivity => "Differential Reflectivity",
            RadarProduct::ClutterFilterPower => "Clutter Filter Power",
        }
    }

    pub fn all() -> &'static [RadarProduct] {
        &[
            RadarProduct::Reflectivity,
            RadarProduct::Velocity,
            RadarProduct::SpectrumWidth,
            RadarProduct::DifferentialPhase,
            RadarProduct::CorrelationCoefficient,
            RadarProduct::DifferentialReflectivity,
            RadarProduct::ClutterFilterPower,
        ]
    }
}

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
