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
        RadarProduct::StormRelativeVelocity => velocity_color(value),
        RadarProduct::SpecificDifferentialPhase => kdp_color(value),
        RadarProduct::EchoTops => echo_tops_color(value),
        RadarProduct::VerticallyIntegratedLiquid => vil_color(value),
        RadarProduct::HydrometeorClassification => hhc_color(value),
        RadarProduct::PrecipitationRate => precip_rate_color(value),
        RadarProduct::NormalizedRotation => nrot_color(value),
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

/// Specific differential phase (KDP) color scale (°/km)
/// Range: −2 to 10+ °/km
fn kdp_color(kdp: f32) -> (u8, u8, u8, u8) {
    let (r, g, b) = if kdp < -1.0 {
        (100, 0, 150) // Purple (negative — unusual)
    } else if kdp < 0.0 {
        (0, 80, 255) // Blue
    } else if kdp < 0.5 {
        (100, 200, 100) // Light green
    } else if kdp < 1.0 {
        (0, 255, 0) // Green
    } else if kdp < 2.0 {
        (255, 255, 0) // Yellow
    } else if kdp < 3.5 {
        (255, 165, 0) // Orange
    } else if kdp < 5.0 {
        (255, 0, 0) // Red
    } else if kdp < 7.0 {
        (180, 0, 0) // Dark red
    } else {
        (200, 0, 200) // Magenta (extreme)
    };

    (r, g, b, TRANSPARENCY)
}

/// Enhanced Echo Tops color scale (thousands of feet)
/// Range: 0–70+ kft
fn echo_tops_color(kft: f32) -> (u8, u8, u8, u8) {
    if kft < 5.0 {
        return (0, 0, 0, 0); // Transparent (very low)
    }

    let (r, g, b) = if kft < 10.0 {
        (100, 100, 100) // Grey
    } else if kft < 15.0 {
        (0, 100, 255) // Blue
    } else if kft < 20.0 {
        (0, 200, 255) // Cyan
    } else if kft < 25.0 {
        (0, 255, 0) // Green
    } else if kft < 30.0 {
        (0, 200, 0) // Dark green
    } else if kft < 35.0 {
        (255, 255, 0) // Yellow
    } else if kft < 40.0 {
        (255, 200, 0) // Gold
    } else if kft < 45.0 {
        (255, 150, 0) // Orange
    } else if kft < 50.0 {
        (255, 0, 0) // Red
    } else if kft < 55.0 {
        (200, 0, 0) // Dark red
    } else if kft < 60.0 {
        (255, 0, 255) // Magenta
    } else {
        (200, 0, 200) // Purple (extreme)
    };

    (r, g, b, TRANSPARENCY)
}

/// VIL (Vertically Integrated Liquid) color scale (kg/m²)
/// Range: 0–80+ kg/m²
fn vil_color(vil: f32) -> (u8, u8, u8, u8) {
    if vil < 1.0 {
        return (0, 0, 0, 0); // Transparent
    }

    let (r, g, b) = if vil < 5.0 {
        (100, 100, 100) // Grey
    } else if vil < 10.0 {
        (0, 150, 255) // Light blue
    } else if vil < 15.0 {
        (0, 255, 0) // Green
    } else if vil < 20.0 {
        (0, 200, 0) // Dark green
    } else if vil < 25.0 {
        (255, 255, 0) // Yellow
    } else if vil < 30.0 {
        (255, 200, 0) // Gold
    } else if vil < 35.0 {
        (255, 150, 0) // Orange
    } else if vil < 40.0 {
        (255, 0, 0) // Red
    } else if vil < 50.0 {
        (200, 0, 0) // Dark red
    } else if vil < 60.0 {
        (255, 0, 255) // Magenta
    } else {
        (200, 0, 200) // Purple (extreme)
    };

    (r, g, b, TRANSPARENCY)
}

/// Hydrometeor Classification color scale (categorical)
/// Values map to hydrometeor types per ICD table.
/// 0=ND, 10=BI, 20=AP, 30=IC, 40=DS, 50=WS, 60=RA, 70=HR,
/// 80=BD, 90=GR, 100=HA, 110=LH, 120=GH, 140=UK, 150=RF
fn hhc_color(val: f32) -> (u8, u8, u8, u8) {
    let (r, g, b) = if val < 5.0 {
        return (0, 0, 0, 0); // ND (No Data) — transparent
    } else if val < 15.0 {
        (128, 128, 128) // BI (Biological) — grey
    } else if val < 25.0 {
        (128, 0, 128) // AP (Ground clutter) — purple
    } else if val < 35.0 {
        (173, 216, 230) // IC (Ice crystals) — light blue
    } else if val < 45.0 {
        (0, 100, 255) // DS (Dry snow) — blue
    } else if val < 55.0 {
        (0, 200, 255) // WS (Wet snow) — cyan
    } else if val < 65.0 {
        (0, 200, 0) // RA (Rain) — green
    } else if val < 75.0 {
        (0, 100, 0) // HR (Heavy rain) — dark green
    } else if val < 85.0 {
        (255, 255, 0) // BD (Big drops) — yellow
    } else if val < 95.0 {
        (255, 150, 0) // GR (Graupel) — orange
    } else if val < 105.0 {
        (255, 0, 0) // HA (Hail w/ rain) — red
    } else if val < 115.0 {
        (200, 0, 0) // LH (Large hail) — dark red
    } else if val < 125.0 {
        (255, 200, 200) // GH (Giant hail) — pink
    } else if val < 145.0 {
        (200, 200, 200) // UK (Unknown) — light grey
    } else {
        (100, 0, 150) // RF (Range folded) — dark purple
    };

    (r, g, b, TRANSPARENCY)
}

/// Instantaneous Precipitation Rate color scale (in/hr)
/// Range: 0–20+ in/hr
fn precip_rate_color(rate: f32) -> (u8, u8, u8, u8) {
    if rate < 0.01 {
        return (0, 0, 0, 0); // Transparent (trace)
    }

    let (r, g, b) = if rate < 0.1 {
        (100, 100, 100) // Grey (very light)
    } else if rate < 0.25 {
        (0, 150, 255) // Light blue
    } else if rate < 0.5 {
        (0, 100, 255) // Blue
    } else if rate < 1.0 {
        (0, 255, 0) // Green
    } else if rate < 2.0 {
        (255, 255, 0) // Yellow
    } else if rate < 3.0 {
        (255, 200, 0) // Gold
    } else if rate < 4.0 {
        (255, 150, 0) // Orange
    } else if rate < 6.0 {
        (255, 0, 0) // Red
    } else if rate < 8.0 {
        (200, 0, 0) // Dark red
    } else if rate < 12.0 {
        (255, 0, 255) // Magenta
    } else {
        (200, 0, 200) // Purple (extreme)
    };

    (r, g, b, TRANSPARENCY)
}

/// Normalized Rotation (NROT) color scale (unitless)
/// Diverging palette: blue = anticyclonic (negative), red = cyclonic (positive)
/// Values near zero are transparent. >1.0 significant, >2.5 extreme.
fn nrot_color(nrot: f32) -> (u8, u8, u8, u8) {
    if nrot.is_nan() || nrot.is_infinite() {
        return (0, 0, 0, 0);
    }

    // Values near zero — transparent (no significant rotation)
    if nrot.abs() < 0.3 {
        return (0, 0, 0, 0);
    }

    let (r, g, b) = if nrot > 4.0 {
        (200, 0, 200) // Purple (extreme cyclonic)
    } else if nrot > 2.5 {
        (255, 0, 0) // Red (strong cyclonic)
    } else if nrot > 1.5 {
        (255, 150, 0) // Orange
    } else if nrot > 1.0 {
        (255, 255, 0) // Yellow (significant)
    } else if nrot > 0.3 {
        (200, 200, 200) // Light grey (weak positive)
    } else if nrot < -4.0 {
        (128, 0, 255) // Violet (extreme anticyclonic)
    } else if nrot < -2.5 {
        (0, 0, 255) // Blue (strong anticyclonic)
    } else if nrot < -1.5 {
        (0, 150, 255) // Light blue
    } else if nrot < -1.0 {
        (0, 255, 255) // Cyan (significant)
    } else {
        (160, 160, 160) // Grey (weak negative)
    };

    (r, g, b, TRANSPARENCY)
}
