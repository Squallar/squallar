use crate::types::{RadarProduct, MS_TO_MPH};

const TRANSPARENCY: u8 = 180;

/// Ascending-threshold color scale: for value `v`, the color of the last entry
/// whose threshold is <= `v`. The `bool` picks gradient (linear interpolation
/// between stops) over discrete steps.
type ColorThresholds = &'static [(f32, (u8, u8, u8))];
type ColorScale = &'static (ColorThresholds, bool);

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

/// RGBA color for a radar value, in the product's own units.
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

/// Input is m/s; the velocity tables are in mph.
fn velocity_lookup(velocity_ms: f32) -> (u8, u8, u8, u8) {
    let mph = velocity_ms * MS_TO_MPH;
    let (r, g, b) = if mph > 0.0 {
        scale_color(VELOCITY_OUTBOUND, mph)
    } else {
        scale_color(VELOCITY_INBOUND, mph.abs())
    };
    (r, g, b, TRANSPARENCY)
}

/// Positive NROT is cyclonic, negative anticyclonic.
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
    (0.0,  (0, 0, 0)),
    (2.5,  (64, 64, 64)),
    (5.0,  (128, 128, 128)),
    (7.5,  (64, 114, 164)),
    (10.0, (100, 150, 255)),
    (15.0, (0, 100, 255)),
    (20.0, (0, 50, 200)),
    (25.0, (0, 255, 0)),
    (30.0, (0, 200, 0)),
    (35.0, (255, 255, 0)),
    (40.0, (255, 165, 0)),
    (45.0, (255, 0, 0)),
    (50.0, (200, 0, 0)),
    (55.0, (255, 192, 203)),
    (60.0, (255, 105, 180)),
    (65.0, (128, 0, 128)),
    (70.0, (75, 0, 130)),
    (75.0, (135, 206, 235)),   // hail
    (80.0, (173, 216, 230)),
    (85.0, (255, 140, 0)),
    (90.0, (255, 69, 0)),
    (95.0, (255, 255, 255)),
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
    (0.0,  (118, 118, 118)),
    (2.0578,  (156, 156, 156)),
    (4.1156,  (0, 187, 187)),
    (6.1733,  (255, 0, 0)),
    (8.2311,  (208, 112, 0)),
    (10.2889, (255, 255, 0)),
], false);

/// Differential reflectivity ZDR (dB).
static ZDR: ColorScale = &(&[
    (f32::NEG_INFINITY, (66, 66, 66)),
    (-2.0, (132, 132, 132)),
    (-1.0, (166, 166, 166)),
    (0.0,  (123, 103, 163)),
    (0.2, (2, 8, 155)),
    (1.0,  (32, 202, 164)),
    (1.5, (32, 217, 57)),
    (2.0,  (255, 242, 93)),
    (2.5,  (255, 170, 76)),
    (3.0,  (216, 0, 0)),
    (4.0,  (150, 0, 0)),
    (5.0,  (247, 138, 194)),
    (5.5,  (255, 255, 255)),
], true);

/// Correlation coefficient (0-1).
static RHO: ColorScale = &(&[
    (0.45,  (21, 19, 143)),
    (0.55, (51, 45, 216)),
    (0.75,  (124, 121, 214)),
    (0.8,  (127, 220, 25)),
    (0.9,  (255, 224, 0)),
    (0.96,  (255, 152, 0)),
    (0.98, (151, 5, 86)),
], true);

/// Differential phase (degrees, pre-wrapped to 0-360). Cyclic: the tail returns
/// toward the first color so 0° and 360° are visually continuous.
static PHI: ColorScale = &(&[
    (0.0,   (151, 151, 242)),
    (15.0,  (113, 113, 205)),
    (30.0,  (62, 125, 249)),
    (45.0,  (33, 67, 134)),
    (60.0,  (0, 249, 0)),
    (75.0,  (0, 134, 0)),
    (90.0,  (255, 249, 0)),
    (105.0, (255, 137, 0)),
    (120.0, (255, 0, 0)),
    (135.0, (173, 0, 0)),
    (150.0, (252, 0, 252)),
    (165.0, (144, 0, 144)),
    (180.0, (100, 0, 100)),
    (195.0, (60, 0, 130)),
    (210.0, (30, 30, 180)),
    (225.0, (0, 60, 200)),
    (240.0, (0, 120, 180)),
    (255.0, (0, 160, 130)),
    (270.0, (0, 180, 80)),
    (285.0, (80, 200, 0)),
    (300.0, (180, 220, 0)),
    (315.0, (220, 200, 80)),
    (330.0, (200, 180, 160)),
    (345.0, (175, 165, 210)),
], true);

/// Specific differential phase KDP (deg/km).
static KDP: ColorScale = &(&[
    (-2.0, (118, 118, 118)),
    (-1.0, (75, 75, 75)),
    (-0.5,  (75, 0, 0)),
    (0.0,  (121, 5, 29)),
    (1.0,  (196, 100, 154)),
    (1.5, (125, 107, 152)),
    (2.0,  (91, 237, 232)),
    (2.5, (20, 185, 50)),
    (3.0, (10, 255, 10)),
    (4.0,  (246, 246, 0)),
    (5.0,  (250, 117, 19)),
    (6.0, (202, 92, 14)),
    (6.5, (175, 78, 11)),
], true);

/// Enhanced Echo Tops (thousands of feet).
static ECHO_TOPS: ColorScale = &(&[
    (5.0,  (100, 100, 100)), // dispatcher renders < 5 transparent
    (10.0, (0, 100, 255)),
    (15.0, (0, 200, 255)),
    (20.0, (0, 255, 0)),
    (25.0, (0, 200, 0)),
    (30.0, (255, 255, 0)),
    (35.0, (255, 200, 0)),
    (40.0, (255, 150, 0)),
    (45.0, (255, 0, 0)),
    (50.0, (200, 0, 0)),
    (55.0, (255, 0, 255)),
    (60.0, (200, 0, 200)),
], true);

/// Vertically Integrated Liquid (kg/m2).
static VIL: ColorScale = &(&[
    (1.0,  (100, 100, 100)), // dispatcher renders < 1 transparent
    (5.0,  (0, 150, 255)),
    (10.0, (0, 255, 0)),
    (15.0, (0, 200, 0)),
    (20.0, (255, 255, 0)),
    (25.0, (255, 200, 0)),
    (30.0, (255, 150, 0)),
    (35.0, (255, 0, 0)),
    (40.0, (200, 0, 0)),
    (50.0, (255, 0, 255)),
    (60.0, (200, 0, 200)),
], true);

/// Hydrometeor Classification. Categorical: thresholds are the ICD class
/// codes, 0 = ND.
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
    (0.01, (100, 100, 100)), // dispatcher renders < 0.01 transparent
    (0.1,  (0, 150, 255)),
    (0.25, (0, 100, 255)),
    (0.5,  (0, 255, 0)),
    (1.0,  (255, 255, 0)),
    (2.0,  (255, 200, 0)),
    (3.0,  (255, 150, 0)),
    (4.0,  (255, 0, 0)),
    (6.0,  (200, 0, 0)),
    (8.0,  (255, 0, 255)),
    (12.0, (200, 0, 200)),
], true);

/// NROT cyclonic / positive rotation (unitless).
static NROT_CYCLONIC: ColorScale = &(&[
    (0.25, (0, 0, 255)),     // weak
    (1.0, (0, 255, 0)),      // significant
    (1.5, (255, 150, 0)),    // strong
    (2.0, (255, 0, 0)),      // very strong
    (2.5, (255, 141, 161)),  // extreme
    (2.75, (255, 255, 255)), // oh fuck
], true);

/// NROT anticyclonic / negative rotation (thresholds = abs values).
static NROT_ANTICYCLONIC: ColorScale = &(&[
    (0.25, (0, 255, 128)), // weak
    (1.0, (0, 255, 0)),    // significant
], true);

// ————————————————————————————————————————————————————————————————————
// Legend scale extraction
// ————————————————————————————————————————————————————————————————————

/// Color bar description for a product. Values are in the units fed to
/// `get_color_for_value()`.
pub struct LegendScale {
    /// Color stops, sorted ascending by value.
    pub thresholds: Vec<(f32, [u8; 3])>,
    pub is_gradient: bool,
    pub min_value: f32,
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

/// Merge inbound (negative) and outbound (positive) tables into one scale.
/// `inbound` thresholds are positive, so they are negated and reversed.
/// `unit_factor` converts the table's units to the input domain of
/// `get_color_for_value()` (mph / `MS_TO_MPH` for velocity).
fn merge_bidirectional(inbound: ColorScale, outbound: ColorScale, unit_factor: f32) -> LegendScale {
    let &(in_t, _) = inbound;
    let &(out_t, gradient) = outbound;
    let mut entries: Vec<(f32, [u8; 3])> = Vec::new();
    // Highest inbound magnitude first, i.e. most negative.
    for &(v, (r, g, b)) in in_t.iter().rev() {
        entries.push((-v / unit_factor, [r, g, b]));
    }
    for &(v, (r, g, b)) in out_t.iter() {
        entries.push((v / unit_factor, [r, g, b]));
    }
    let min = entries.first().map_or(0.0, |e| e.0);
    let max = entries.last().map_or(1.0, |e| e.0);
    LegendScale { thresholds: entries, is_gradient: gradient, min_value: min, max_value: max }
}

/// Thresholds are in the same unit domain as `get_color_for_value()`.
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
            // NROT is unitless, so no conversion.
            merge_bidirectional(NROT_ANTICYCLONIC, NROT_CYCLONIC, 1.0)
        }
    }
}
