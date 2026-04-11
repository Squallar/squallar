use crate::types::RadarProduct;

const TRANSPARENCY: u8 = 180;

/// Ascending-threshold color scale. For value `v`, the color of the last
/// entry whose threshold is <= `v` is returned.
type ColorScale = &'static [(f32, (u8, u8, u8))];

/// Look up the color for `value` in an ascending-threshold scale.
fn scale_color(scale: ColorScale, value: f32) -> (u8, u8, u8) {
    let mut color = scale[0].1;
    for &(threshold, c) in scale {
        if value >= threshold {
            color = c;
        } else {
            break;
        }
    }
    color
}

/// Get RGBA color for a radar value based on product type.
pub fn get_color_for_value(product: RadarProduct, value: f32) -> (u8, u8, u8, u8) {
    match product {
        RadarProduct::Reflectivity => {
            if value.is_nan() || value.is_infinite() || value < 0.0 {
                return (0, 0, 0, 0);
            }
            let (r, g, b) = scale_color(REFLECTIVITY, value);
            (r, g, b, TRANSPARENCY)
        }
        RadarProduct::Velocity | RadarProduct::StormRelativeVelocity => velocity_lookup(value),
        RadarProduct::SpectrumWidth => {
            if value < 0.0 {
                return (0, 0, 0, 0);
            }
            let (r, g, b) = scale_color(SPECTRUM_WIDTH, value);
            (r, g, b, TRANSPARENCY)
        }
        RadarProduct::DifferentialReflectivity => {
            let (r, g, b) = scale_color(ZDR, value);
            (r, g, b, TRANSPARENCY)
        }
        RadarProduct::CorrelationCoefficient => {
            if value < 0.0 {
                return (0, 0, 0, 0);
            }
            let (r, g, b) = scale_color(RHO, value);
            (r, g, b, TRANSPARENCY)
        }
        RadarProduct::DifferentialPhase => {
            let (r, g, b) = scale_color(PHI, value.rem_euclid(360.0));
            (r, g, b, TRANSPARENCY)
        }
        RadarProduct::SpecificDifferentialPhase => {
            let (r, g, b) = scale_color(KDP, value);
            (r, g, b, TRANSPARENCY)
        }
        RadarProduct::EchoTops => {
            if value < 5.0 {
                return (0, 0, 0, 0);
            }
            let (r, g, b) = scale_color(ECHO_TOPS, value);
            (r, g, b, TRANSPARENCY)
        }
        RadarProduct::VerticallyIntegratedLiquid => {
            if value < 1.0 {
                return (0, 0, 0, 0);
            }
            let (r, g, b) = scale_color(VIL, value);
            (r, g, b, TRANSPARENCY)
        }
        RadarProduct::HydrometeorClassification => {
            if value < 5.0 {
                return (0, 0, 0, 0);
            }
            let (r, g, b) = scale_color(HHC, value);
            (r, g, b, TRANSPARENCY)
        }
        RadarProduct::PrecipitationRate => {
            if value < 0.01 {
                return (0, 0, 0, 0);
            }
            let (r, g, b) = scale_color(PRECIP_RATE, value);
            (r, g, b, TRANSPARENCY)
        }
        RadarProduct::NormalizedRotation => nrot_lookup(value),
    }
}

use crate::types::MS_TO_MPH;

/// Velocity color with m/s->mph conversion and bidirectional handling.
fn velocity_lookup(velocity_ms: f32) -> (u8, u8, u8, u8) {
    let mph = velocity_ms * MS_TO_MPH;
    if !(-142.0..=141.0).contains(&mph) {
        return (128, 0, 128, TRANSPARENCY); // Range folded
    }
    if (-5.0..=5.0).contains(&mph) {
        return (128, 128, 128, TRANSPARENCY); // Near zero
    }
    let (r, g, b) = if mph > 0.0 {
        scale_color(VELOCITY_OUTBOUND, mph)
    } else {
        scale_color(VELOCITY_INBOUND, mph.abs())
    };
    (r, g, b, TRANSPARENCY)
}

/// NROT color with bidirectional cyclonic/anticyclonic handling.
fn nrot_lookup(nrot: f32) -> (u8, u8, u8, u8) {
    if nrot.is_nan() || nrot.is_infinite() || nrot.abs() < 0.5 {
        return (0, 0, 0, 0);
    }
    let (r, g, b) = if nrot > 0.0 {
        scale_color(NROT_CYCLONIC, nrot)
    } else {
        scale_color(NROT_ANTICYCLONIC, nrot.abs())
    };
    (r, g, b, TRANSPARENCY)
}

// ————————————————————————————————————————————————————————————————————
// Color scale tables
// ————————————————————————————————————————————————————————————————————

/// Reflectivity (dBZ). Gradient regions 0-10 dBZ approximated with discrete steps.
static REFLECTIVITY: ColorScale = &[
    (0.0,  (0, 0, 0)),         // Grey ramp start
    (2.5,  (64, 64, 64)),      // Grey ramp midpoint
    (5.0,  (128, 128, 128)),   // Grey ramp end / transition start
    (7.5,  (64, 114, 164)),    // Grey -> blue midpoint
    (10.0, (100, 150, 255)),   // Light blue
    (15.0, (0, 100, 255)),     // Blue
    (20.0, (0, 50, 200)),      // Dark blue
    (25.0, (0, 255, 0)),       // Green
    (30.0, (0, 200, 0)),       // Dark green
    (35.0, (255, 255, 0)),     // Yellow
    (40.0, (255, 165, 0)),     // Orange
    (45.0, (255, 0, 0)),       // Red
    (50.0, (200, 0, 0)),       // Dark red
    (55.0, (255, 192, 203)),   // Pink
    (60.0, (255, 105, 180)),   // Hot pink
    (65.0, (128, 0, 128)),     // Purple
    (70.0, (75, 0, 130)),      // Dark purple
    (75.0, (135, 206, 235)),   // Sky blue (hail)
    (80.0, (173, 216, 230)),   // Light blue
    (85.0, (255, 140, 0)),     // Orange (extreme)
    (90.0, (255, 69, 0)),      // Dark orange
    (95.0, (255, 255, 255)),   // White (extreme > 95 dBZ)
];

/// Velocity outbound / positive (mph thresholds).
/// Gradient 5-20 mph approximated with midpoint entry.
static VELOCITY_OUTBOUND: ColorScale = &[
    (5.0,   (128, 128, 128)), // Grey (just above near-zero band)
    (12.5,  (133, 64, 64)),   // Grey -> dark red midpoint
    (20.0,  (139, 0, 0)),     // Dark red
    (35.0,  (255, 0, 0)),     // Bright red
    (55.0,  (255, 192, 203)), // Pink
    (80.0,  (255, 218, 185)), // Peach
    (100.0, (255, 140, 0)),   // Orange
    (125.0, (139, 69, 19)),   // Brown
];

/// Velocity inbound / negative (thresholds are positive, applied to abs(mph)).
/// Gradient 5-20 mph approximated with midpoint entry.
static VELOCITY_INBOUND: ColorScale = &[
    (5.0,   (128, 128, 128)), // Grey (just above near-zero band)
    (12.5,  (64, 114, 64)),   // Grey -> dark green midpoint
    (20.0,  (0, 100, 0)),     // Dark green
    (35.0,  (0, 255, 0)),     // Bright green
    (55.0,  (173, 216, 230)), // Light blue
    (80.0,  (135, 206, 235)), // Sky blue
    (100.0, (0, 0, 255)),     // Blue
    (125.0, (255, 0, 255)),   // Fuchsia
];

/// Spectrum width (m/s).
static SPECTRUM_WIDTH: ColorScale = &[
    (0.0,  (118, 118, 118)),   // Dark grey
    (2.0578,  (156, 156, 156)),   // Light grey
    (4.1156,  (0, 187, 187)), // Cyan
    (6.1733,  (255, 0, 0)), // Red
    (8.2311,  (208, 112, 0)),   // Orange
    (10.2889, (255, 255, 0)),   // Yellow
];

/// Differential reflectivity ZDR (dB).
static ZDR: ColorScale = &[
    (f32::NEG_INFINITY, (100, 0, 100)), // Purple (< -1)
    (-1.0, (0, 100, 255)),              // Blue
    (0.0,  (0, 255, 0)),                // Green
    (1.0,  (255, 255, 0)),              // Yellow
    (2.0,  (255, 150, 0)),              // Orange
    (3.0,  (255, 0, 0)),                // Red
];

/// Correlation coefficient (0-1).
static RHO: ColorScale = &[
    (0.45,  (21, 19, 143)),   // Blue
    (0.55, (51, 45, 216)), // Blue
    (0.75,  (124, 121, 214)),   // Light blue
    (0.8,  (127, 220, 25)), // Green
    (0.9,  (255, 224, 0)), // Yellow
    (0.96,  (255, 152, 0)),   // Orange
    (0.98, (151, 5, 86)),   // Purple
];

/// Differential phase (degrees, pre-wrapped to 0-360).
static PHI: ColorScale = &[
    (0.0,   (255, 0, 0)),   // Red
    (60.0,  (255, 255, 0)), // Yellow
    (120.0, (0, 255, 0)),   // Green
    (180.0, (0, 255, 255)), // Cyan
    (240.0, (0, 0, 255)),   // Blue
    (300.0, (255, 0, 255)), // Magenta
];

/// Specific differential phase KDP (deg/km).
static KDP: ColorScale = &[
    (f32::NEG_INFINITY, (100, 0, 150)), // Purple (< -1, unusual)
    (-1.0, (0, 80, 255)),               // Blue
    (0.0,  (100, 200, 100)),            // Light green
    (0.5,  (0, 255, 0)),                // Green
    (1.0,  (255, 255, 0)),              // Yellow
    (2.0,  (255, 165, 0)),              // Orange
    (3.5,  (255, 0, 0)),                // Red
    (5.0,  (180, 0, 0)),                // Dark red
    (7.0,  (200, 0, 200)),              // Magenta (extreme)
];

/// Enhanced Echo Tops (thousands of feet).
static ECHO_TOPS: ColorScale = &[
    (5.0,  (100, 100, 100)), // Grey (dispatcher handles < 5 as transparent)
    (10.0, (0, 100, 255)),   // Blue
    (15.0, (0, 200, 255)),   // Cyan
    (20.0, (0, 255, 0)),     // Green
    (25.0, (0, 200, 0)),     // Dark green
    (30.0, (255, 255, 0)),   // Yellow
    (35.0, (255, 200, 0)),   // Gold
    (40.0, (255, 150, 0)),   // Orange
    (45.0, (255, 0, 0)),     // Red
    (50.0, (200, 0, 0)),     // Dark red
    (55.0, (255, 0, 255)),   // Magenta
    (60.0, (200, 0, 200)),   // Purple (extreme)
];

/// Vertically Integrated Liquid (kg/m2).
static VIL: ColorScale = &[
    (1.0,  (100, 100, 100)), // Grey (dispatcher handles < 1 as transparent)
    (5.0,  (0, 150, 255)),   // Light blue
    (10.0, (0, 255, 0)),     // Green
    (15.0, (0, 200, 0)),     // Dark green
    (20.0, (255, 255, 0)),   // Yellow
    (25.0, (255, 200, 0)),   // Gold
    (30.0, (255, 150, 0)),   // Orange
    (35.0, (255, 0, 0)),     // Red
    (40.0, (200, 0, 0)),     // Dark red
    (50.0, (255, 0, 255)),   // Magenta
    (60.0, (200, 0, 200)),   // Purple (extreme)
];

/// Hydrometeor Classification (categorical per ICD table).
/// 0=ND, 10=BI, 20=AP, 30=IC, 40=DS, 50=WS, 60=RA, 70=HR,
/// 80=BD, 90=GR, 100=HA, 110=LH, 120=GH, 140=UK, 150=RF
static HHC: ColorScale = &[
    (5.0,   (128, 128, 128)), // BI (Biological)
    (15.0,  (128, 0, 128)),   // AP (Ground clutter)
    (25.0,  (173, 216, 230)), // IC (Ice crystals)
    (35.0,  (0, 100, 255)),   // DS (Dry snow)
    (45.0,  (0, 200, 255)),   // WS (Wet snow)
    (55.0,  (0, 200, 0)),     // RA (Rain)
    (65.0,  (0, 100, 0)),     // HR (Heavy rain)
    (75.0,  (255, 255, 0)),   // BD (Big drops)
    (85.0,  (255, 150, 0)),   // GR (Graupel)
    (95.0,  (255, 0, 0)),     // HA (Hail w/ rain)
    (105.0, (200, 0, 0)),     // LH (Large hail)
    (115.0, (255, 200, 200)), // GH (Giant hail)
    (125.0, (200, 200, 200)), // UK (Unknown)
    (145.0, (100, 0, 150)),   // RF (Range folded)
];

/// Precipitation Rate (in/hr).
static PRECIP_RATE: ColorScale = &[
    (0.01, (100, 100, 100)), // Grey (very light; dispatcher handles < 0.01)
    (0.1,  (0, 150, 255)),   // Light blue
    (0.25, (0, 100, 255)),   // Blue
    (0.5,  (0, 255, 0)),     // Green
    (1.0,  (255, 255, 0)),   // Yellow
    (2.0,  (255, 200, 0)),   // Gold
    (3.0,  (255, 150, 0)),   // Orange
    (4.0,  (255, 0, 0)),     // Red
    (6.0,  (200, 0, 0)),     // Dark red
    (8.0,  (255, 0, 255)),   // Magenta
    (12.0, (200, 0, 200)),   // Purple (extreme)
];

/// NROT cyclonic / positive rotation (unitless).
static NROT_CYCLONIC: ColorScale = &[
    (0.25, (0, 0, 255)),     // Blue (weak)
    (1.0, (0, 255, 0)),      // Green (significant)
    (1.5, (255, 150, 0)),    // Yellow (strong)
    (2.0, (255, 0, 0)),      // Red (very strong)
    (2.5, (255, 141, 161)),  // Pink (extreme)
    (2.75, (255, 255, 255)), // White (oh fuck)
];

/// NROT anticyclonic / negative rotation (thresholds = abs values).
static NROT_ANTICYCLONIC: ColorScale = &[
    (0.25, (0, 255, 128)), // Aqua (weak)
    (1.0, (0, 255, 0)),    // Green (significant)
];
