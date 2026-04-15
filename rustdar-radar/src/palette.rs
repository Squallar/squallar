use crate::types::{RadarProduct, MS_TO_MPH};

const TRANSPARENCY: u8 = 180;

/// Ascending-threshold color scale. For value `v`, the color of the last
/// entry whose threshold is <= `v` is returned. The final boolean is if
/// the scale should be a gradient (interpolated) or discrete steps.
type ColorThresholds = &'static [(f32, (u8, u8, u8))];
type ColorScale = &'static (ColorThresholds, bool);

/// Look up the color for `value` in an ascending-threshold scale.
/// When an entry's `gradient` flag is true, interpolates linearly from the
/// previous entry's color to this entry's color across the threshold range.
fn scale_color(scale: ColorScale, value: f32) -> (u8, u8, u8) {
    let &(thresholds, gradient) = scale;
    let mut color = thresholds[0].1;
    let mut last_threshold = thresholds[0].0;
    for (i, &(threshold, c)) in thresholds.iter().enumerate() {
        if value >= threshold {
            color = c;
            last_threshold = threshold;
        } else {
            if gradient && i > 0 && threshold > last_threshold {
                let t = (value - last_threshold) / (threshold - last_threshold);
                return (
                    (color.0 as f32 + (c.0 as f32 - color.0 as f32) * t) as u8,
                    (color.1 as f32 + (c.1 as f32 - color.1 as f32) * t) as u8,
                    (color.2 as f32 + (c.2 as f32 - color.2 as f32) * t) as u8,
                );
            }
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
            if value < 10.0 {
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

/// Velocity color with m/s->mph conversion and bidirectional handling.
fn velocity_lookup(velocity_ms: f32) -> (u8, u8, u8, u8) {
    let mph = velocity_ms * MS_TO_MPH;
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
static REFLECTIVITY: ColorScale = &(&[
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
], true);

/// Velocity outbound / positive (mph thresholds).
static VELOCITY_OUTBOUND: ColorScale = &(&[
    (0.0,      (100, 0, 0)),
    (11.5078,  (110, 0, 0)),
    (23.0156,  (140, 0, 0)),
    (34.5234,  (165, 0, 0)),
    (46.0312,  (190, 0, 0)),
    (57.539,   (210, 0, 0)),
    (69.0468,  (230, 0, 0)),
    (80.5546,  (255, 0, 0)),
], true);

/// Velocity inbound / negative (thresholds are positive, applied to abs(mph)).
static VELOCITY_INBOUND: ColorScale = &(&[
    (0.0,      (0, 100, 0)),
    (11.5078,  (0, 110, 0)),
    (23.0156,  (0, 140, 0)),
    (34.5234,  (0, 165, 0)),
    (46.0312,  (0, 190, 0)),
    (57.539,   (0, 210, 0)),
    (69.0468,  (0, 230, 0)),
    (80.5546,  (0, 255, 0)),
], true);

/// Spectrum width (m/s).
static SPECTRUM_WIDTH: ColorScale = &(&[
    (0.0,  (118, 118, 118)),   // Dark grey
    (2.0578,  (156, 156, 156)),   // Light grey
    (4.1156,  (0, 187, 187)), // Cyan
    (6.1733,  (255, 0, 0)), // Red
    (8.2311,  (208, 112, 0)),   // Orange
    (10.2889, (255, 255, 0)),   // Yellow
], false);

/// Differential reflectivity ZDR (dB).
static ZDR: ColorScale = &(&[
    (f32::NEG_INFINITY, (66, 66, 66)), // Dark grey
    (-2.0, (132, 132, 132)),              // Light grey
    (-1.0, (166, 166, 166)),              // Lighter greay
    (0.0,  (123, 103, 163)),                // Purple
    (0.2, (2, 8, 155)),                // Blue
    (1.0,  (32, 202, 164)),              // Cyan
    (1.5, (32, 217, 57)), // Green
    (2.0,  (255, 242, 93)),              // Yellow
    (2.5,  (255, 170, 76)),              // Orange
    (3.0,  (216, 0, 0)),                // Red
    (4.0,  (150, 0, 0)),                // Dark red
    (5.0,  (247, 138, 194)),                // Pink
    (5.5,  (255, 255, 255)),                // White
], true);

/// Correlation coefficient (0-1).
static RHO: ColorScale = &(&[
    (0.45,  (21, 19, 143)),   // Blue
    (0.55, (51, 45, 216)), // Blue
    (0.75,  (124, 121, 214)),   // Light blue
    (0.8,  (127, 220, 25)), // Green
    (0.9,  (255, 224, 0)), // Yellow
    (0.96,  (255, 152, 0)),   // Orange
    (0.98, (151, 5, 86)),   // Purple
], true);

/// Differential phase (degrees, pre-wrapped to 0-360). Cyclic scale covering
/// the full 360° range at 15° increments, wrapping back toward the starting
/// color so that 0° and 360° are visually continuous.
static PHI: ColorScale = &(&[
    (0.0,   (151, 151, 242)), // Light purple
    (15.0,  (113, 113, 205)), // Light blue-purple
    (30.0,  (62, 125, 249)),  // Blue
    (45.0,  (33, 67, 134)),   // Dark blue
    (60.0,  (0, 249, 0)),     // Green
    (75.0,  (0, 134, 0)),     // Dark green
    (90.0,  (255, 249, 0)),   // Yellow
    (105.0, (255, 137, 0)),   // Orange
    (120.0, (255, 0, 0)),     // Red
    (135.0, (173, 0, 0)),     // Dark red
    (150.0, (252, 0, 252)),   // Magenta
    (165.0, (144, 0, 144)),   // Dark magenta
    (180.0, (100, 0, 100)),   // Deep purple
    (195.0, (60, 0, 130)),    // Indigo
    (210.0, (30, 30, 180)),   // Blue-purple
    (225.0, (0, 60, 200)),    // Medium blue
    (240.0, (0, 120, 180)),   // Teal blue
    (255.0, (0, 160, 130)),   // Teal
    (270.0, (0, 180, 80)),    // Sea green
    (285.0, (80, 200, 0)),    // Lime green
    (300.0, (180, 220, 0)),   // Yellow-green
    (315.0, (220, 200, 80)),  // Light gold
    (330.0, (200, 180, 160)), // Warm grey
    (345.0, (175, 165, 210)), // Lavender
], true);

/// Specific differential phase KDP (deg/km).
static KDP: ColorScale = &(&[
    (-2.0, (118, 118, 118)), // Grey
    (-1.0, (75, 75, 75)),               // Dark grey
    (-0.5,  (75, 0, 0)),            // Dark red
    (0.0,  (121, 5, 29)),            // Red
    (1.0,  (196, 100, 154)),              // Pink
    (1.5, (125, 107, 152)),              // Purple
    (2.0,  (91, 237, 232)),              // Cyan
    (2.5, (20, 185, 50)),              // Green
    (3.0, (10, 255, 10)),              // Bright green
    (4.0,  (246, 246, 0)),                // Yellow
    (5.0,  (250, 117, 19)),                // Orange
    (6.0, (202, 92, 14)),              // Dark orange
    (6.5, (175, 78, 11)),              // Darker orange
], true);

/// Enhanced Echo Tops (thousands of feet).
static ECHO_TOPS: ColorScale = &(&[
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
], true);

/// Vertically Integrated Liquid (kg/m2).
static VIL: ColorScale = &(&[
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
], true);

/// Hydrometeor Classification (categorical per ICD table).
/// 0=ND, 10=BI, 20=AP, 30=IC, 40=DS, 50=WS, 60=RA, 70=HR,
/// 80=BD, 90=GR, 100=HA, 110=LH, 120=GH, 140=UK, 150=RF
static HHC: ColorScale = &(&[
    (10.0,  (128, 128, 128)), // BI (Biological)
    (20.0,  (128, 0, 128)),   // AP (Ground clutter)
    (30.0,  (173, 216, 230)), // IC (Ice crystals)
    (40.0,  (0, 100, 255)),   // DS (Dry snow)
    (50.0,  (0, 200, 255)),   // WS (Wet snow)
    (60.0,  (0, 200, 0)),     // RA (Rain)
    (70.0,  (0, 100, 0)),     // HR (Heavy rain)
    (80.0,  (255, 255, 0)),   // BD (Big drops)
    (90.0,  (255, 150, 0)),   // GR (Graupel)
    (100.0, (255, 0, 0)),     // HA (Hail w/ rain)
    (110.0, (200, 0, 0)),     // LH (Large hail)
    (120.0, (255, 200, 200)), // GH (Giant hail)
    (140.0, (200, 200, 200)), // UK (Unknown)
    (150.0, (100, 0, 150)),   // RF (Range folded)
], false);

/// Precipitation Rate (in/hr).
static PRECIP_RATE: ColorScale = &(&[
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
], true);

/// NROT cyclonic / positive rotation (unitless).
static NROT_CYCLONIC: ColorScale = &(&[
    (0.25, (0, 0, 255)),     // Blue (weak)
    (1.0, (0, 255, 0)),      // Green (significant)
    (1.5, (255, 150, 0)),    // Yellow (strong)
    (2.0, (255, 0, 0)),      // Red (very strong)
    (2.5, (255, 141, 161)),  // Pink (extreme)
    (2.75, (255, 255, 255)), // White (oh fuck)
], true);

/// NROT anticyclonic / negative rotation (thresholds = abs values).
static NROT_ANTICYCLONIC: ColorScale = &(&[
    (0.25, (0, 255, 128)), // Aqua (weak)
    (1.0, (0, 255, 0)),    // Green (significant)
], true);

// ————————————————————————————————————————————————————————————————————
// Legend scale extraction
// ————————————————————————————————————————————————————————————————————

/// A color scale legend describing the threshold→color mapping for a radar product.
/// Values are in the same units fed to `get_color_for_value()`.
pub struct LegendScale {
    /// Sorted ascending (value, RGB) pairs defining the scale's color stops.
    pub thresholds: Vec<(f32, [u8; 3])>,
    /// Whether the rendered color bar should interpolate between stops.
    pub is_gradient: bool,
    /// The minimum value the color bar should span.
    pub min_value: f32,
    /// The maximum value the color bar should span.
    pub max_value: f32,
}

fn extract_scale(scale: ColorScale) -> LegendScale {
    let &(thresholds, gradient) = scale;
    let entries: Vec<(f32, [u8; 3])> = thresholds
        .iter()
        .filter(|(v, _)| v.is_finite())
        .map(|&(v, (r, g, b))| (v, [r, g, b]))
        .collect();
    let min = entries.first().map_or(0.0, |e| e.0);
    let max = entries.last().map_or(1.0, |e| e.0);
    LegendScale { thresholds: entries, is_gradient: gradient, min_value: min, max_value: max }
}

/// Build a merged bidirectional scale from inbound (negative) and outbound (positive) tables.
/// `inbound` thresholds are positive (applied to abs value); they are negated and reversed.
/// The `unit_factor` converts from the table's unit domain to the input domain of
/// `get_color_for_value()` (e.g. divide mph by `MS_TO_MPH` to get m/s for velocity).
fn merge_bidirectional(inbound: ColorScale, outbound: ColorScale, unit_factor: f32) -> LegendScale {
    let &(in_t, _) = inbound;
    let &(out_t, gradient) = outbound;
    let mut entries: Vec<(f32, [u8; 3])> = Vec::new();
    // Inbound: negate and reverse (highest magnitude first → most negative)
    for &(v, (r, g, b)) in in_t.iter().rev() {
        entries.push((-v / unit_factor, [r, g, b]));
    }
    // Outbound
    for &(v, (r, g, b)) in out_t.iter() {
        entries.push((v / unit_factor, [r, g, b]));
    }
    let min = entries.first().map_or(0.0, |e| e.0);
    let max = entries.last().map_or(1.0, |e| e.0);
    LegendScale { thresholds: entries, is_gradient: gradient, min_value: min, max_value: max }
}

/// Get the legend scale description for a radar product.
/// Threshold values are in the same unit domain as `get_color_for_value()`.
pub fn get_legend_scale(product: RadarProduct) -> LegendScale {
    match product {
        RadarProduct::Reflectivity => extract_scale(REFLECTIVITY),
        RadarProduct::Velocity | RadarProduct::StormRelativeVelocity => {
            merge_bidirectional(VELOCITY_INBOUND, VELOCITY_OUTBOUND, MS_TO_MPH)
        }
        RadarProduct::SpectrumWidth => extract_scale(SPECTRUM_WIDTH),
        RadarProduct::DifferentialReflectivity => extract_scale(ZDR),
        RadarProduct::CorrelationCoefficient => extract_scale(RHO),
        RadarProduct::DifferentialPhase => extract_scale(PHI),
        RadarProduct::SpecificDifferentialPhase => extract_scale(KDP),
        RadarProduct::EchoTops => extract_scale(ECHO_TOPS),
        RadarProduct::VerticallyIntegratedLiquid => extract_scale(VIL),
        RadarProduct::HydrometeorClassification => extract_scale(HHC),
        RadarProduct::PrecipitationRate => extract_scale(PRECIP_RATE),
        RadarProduct::NormalizedRotation => {
            // Merge anticyclonic (negative) and cyclonic (positive), unit factor = 1.0 (unitless)
            merge_bidirectional(NROT_ANTICYCLONIC, NROT_CYCLONIC, 1.0)
        }
    }
}
