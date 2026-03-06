use crate::render::RadarProduct;

const TRANSPARENCY: u8 = 180;

/// Get RGBA color for a radar value based on product type
pub fn get_color_for_value(product: RadarProduct, value: f32) -> (u8, u8, u8, u8) {
    match product {
        RadarProduct::Reflectivity => reflectivity_color(value),
        RadarProduct::Velocity => velocity_color(value),
        RadarProduct::SpectrumWidth => spectrum_width_color(value),
        RadarProduct::DifferentialReflectivity => zdr_color(value),
        RadarProduct::CorrelationCoefficient => rho_color(value),
        RadarProduct::DifferentialPhase => phi_color(value),
    }
}

/// Reflectivity color scale (dBZ)
/// Range: 0-95 dBZ with proper meteorological color scale
fn reflectivity_color(dbz: f32) -> (u8, u8, u8, u8) {
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

    (r, g, b, TRANSPARENCY)
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

    (r, g, b, TRANSPARENCY)
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

    (r, g, b, TRANSPARENCY)
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

    (r, g, b, TRANSPARENCY)
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

    (r, g, b, TRANSPARENCY)
}

/// Differential phase color scale (degrees)
fn phi_color(phi: f32) -> (u8, u8, u8, u8) {
    let normalized = phi.rem_euclid(360.0);

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

    (r, g, b, TRANSPARENCY)
}
