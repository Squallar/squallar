//! The colour scale every product is drawn with, and where each one came from.

use crate::types::{MS_TO_MPH, RadarProduct};
use std::sync::LazyLock;

const TRANSPARENCY: u8 = 180;

/// The colour of a **range-folded** gate: one whose true range is ambiguous
/// past the unambiguous range of its cut's PRF.
pub const RANGE_FOLDED: (u8, u8, u8, u8) = (178, 102, 204, TRANSPARENCY);

/// Ascending-threshold color scale: for value `v`, the color of the last entry whose
/// threshold is <= `v`.
type ColorThresholds = &'static [(f32, (u8, u8, u8))];
type ColorScale = &'static (ColorThresholds, bool);

/// The colour a scale gives `value`: the last stop at or below it, blended
/// toward the next stop where the scale is a gradient.
fn scale_color(scale: ColorScale, value: f32) -> (u8, u8, u8) {
    let &(thresholds, gradient) = scale;
    let i = thresholds.partition_point(|&(threshold, _)| threshold <= value);
    if i == 0 {
        // Below the first stop, or NaN.
        return thresholds[0].1;
    }
    let (last_threshold, color) = thresholds[i - 1];
    if i == thresholds.len() {
        return color;
    }
    let (threshold, c) = thresholds[i];
    if gradient && i > 0 && threshold > last_threshold && last_threshold.is_finite() {
        let t = (value - last_threshold) / (threshold - last_threshold);
        return (
            (color.0 as f32 + (c.0 as f32 - color.0 as f32) * t) as u8,
            (color.1 as f32 + (c.1 as f32 - color.1 as f32) * t) as u8,
            (color.2 as f32 + (c.2 as f32 - color.2 as f32) * t) as u8,
        );
    }
    color
}

/// RGBA color for a radar value, in the product's own units.
pub fn get_color_for_value(product: RadarProduct, value: f32) -> (u8, u8, u8, u8) {
    if !value.is_finite() {
        return (0, 0, 0, 0);
    }
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
        RadarProduct::EchoTops | RadarProduct::EchoTopsInterpolated => {
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
        RadarProduct::VilDensity => {
            if value < 0.5 {
                return (0, 0, 0, 0);
            }
            let (r, g, b) = scale_color(VIL_DENSITY, value);
            (r, g, b, TRANSPARENCY)
        }
        RadarProduct::ProbabilityOfSevereHail => {
            if value < 10.0 {
                return (0, 0, 0, 0);
            }
            let (r, g, b) = scale_color(POSH, value);
            (r, g, b, TRANSPARENCY)
        }
        RadarProduct::MaxExpectedHailSize => {
            if value < 0.25 {
                return (0, 0, 0, 0);
            }
            let (r, g, b) = scale_color(MEHS, value);
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
    if nrot.is_nan() || nrot.is_infinite() || nrot.abs() < NROT_FIRST_CLASS {
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
static REFLECTIVITY: ColorScale = &(
    &[
        (0.0, (0, 0, 0)),
        (2.5, (64, 64, 64)),
        (5.0, (128, 128, 128)),
        (7.5, (64, 114, 164)),
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
        (75.0, (135, 206, 235)), // hail
        (80.0, (173, 216, 230)),
        (85.0, (255, 140, 0)),
        (90.0, (255, 69, 0)),
        (95.0, (255, 255, 255)),
    ],
    true,
);

/// Velocity outbound / positive (mph thresholds).
static VELOCITY_OUTBOUND: ColorScale = &(
    &[
        (0.0, (100, 0, 0)),
        (11.5078, (110, 0, 0)),
        (23.0156, (140, 0, 0)),
        (34.5234, (165, 0, 0)),
        (46.0312, (190, 0, 0)),
        (57.539, (210, 0, 0)),
        (69.0468, (230, 0, 0)),
        (80.5546, (255, 0, 0)),
    ],
    true,
);

/// Velocity inbound / negative (thresholds are positive, applied to abs(mph)).
static VELOCITY_INBOUND: ColorScale = &(
    &[
        (0.0, (0, 100, 0)),
        (11.5078, (0, 110, 0)),
        (23.0156, (0, 140, 0)),
        (34.5234, (0, 165, 0)),
        (46.0312, (0, 190, 0)),
        (57.539, (0, 210, 0)),
        (69.0468, (0, 230, 0)),
        (80.5546, (0, 255, 0)),
    ],
    true,
);

/// Spectrum width (m/s) — the RPG's own `sw_8` table, transcribed.
static SPECTRUM_WIDTH: ColorScale = &(
    &[
        (0.0, (118, 118, 118)),
        (2.0578, (156, 156, 156)),
        (4.1156, (0, 187, 0)),
        (6.1733, (255, 0, 0)),
        (8.2311, (208, 112, 0)),
        (10.2889, (255, 255, 0)),
    ],
    false,
);

/// Differential reflectivity ZDR (dB).
static ZDR: ColorScale = &(
    &[
        (f32::NEG_INFINITY, (66, 66, 66)),
        (-2.0, (132, 132, 132)),
        (-1.0, (166, 166, 166)),
        (0.0, (123, 103, 163)),
        (0.2, (2, 8, 155)),
        (1.0, (32, 202, 164)),
        (1.5, (32, 217, 57)),
        (2.0, (255, 242, 93)),
        (2.5, (255, 170, 76)),
        (3.0, (216, 0, 0)),
        (4.0, (150, 0, 0)),
        (5.0, (247, 138, 194)),
        (5.5, (255, 255, 255)),
    ],
    true,
);

/// Correlation coefficient (0-1): a seven-stop gradient reduction of the
/// RPG's own ρhv ramp, at the RPG's own diagnostic breaks.
static RHO: ColorScale = &(
    &[
        (0.45, (21, 19, 143)),
        (0.55, (51, 45, 216)),
        (0.75, (124, 121, 214)),
        (0.8, (127, 220, 25)),
        (0.9, (255, 224, 0)),
        (0.96, (255, 152, 0)),
        (0.98, (151, 5, 86)),
    ],
    true,
);

/// Differential phase (degrees, pre-wrapped to 0-360).
static PHI: ColorScale = &(
    &[
        (0.0, (151, 151, 242)),
        (15.0, (113, 113, 205)),
        (30.0, (62, 125, 249)),
        (45.0, (33, 67, 134)),
        (60.0, (0, 249, 0)),
        (75.0, (0, 134, 0)),
        (90.0, (255, 249, 0)),
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
    ],
    true,
);

/// Specific differential phase KDP (deg/km).
static KDP: ColorScale = &(
    &[
        (-2.0, (118, 118, 118)),
        (-1.0, (75, 75, 75)),
        (-0.5, (75, 0, 0)),
        (0.0, (121, 5, 29)),
        (1.0, (196, 100, 154)),
        (1.5, (125, 107, 152)),
        (2.0, (91, 237, 232)),
        (2.5, (20, 185, 50)),
        (3.0, (10, 255, 10)),
        (4.0, (246, 246, 0)),
        (5.0, (250, 117, 19)),
        (6.0, (202, 92, 14)),
        (6.5, (175, 78, 11)),
    ],
    true,
);

/// Enhanced Echo Tops (thousands of feet).
static ECHO_TOPS: ColorScale = &(
    &[
        (5.0, (100, 100, 100)), // dispatcher renders < 5 transparent
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
    ],
    true,
);

/// Vertically Integrated Liquid (kg/m2).
static VIL: ColorScale = &(
    &[
        (1.0, (100, 100, 100)), // dispatcher renders < 1 transparent
        (5.0, (0, 150, 255)),
        (10.0, (0, 255, 0)),
        (15.0, (0, 200, 0)),
        (20.0, (255, 255, 0)),
        (25.0, (255, 200, 0)),
        (30.0, (255, 150, 0)),
        (35.0, (255, 0, 0)),
        (40.0, (200, 0, 0)),
        (50.0, (255, 0, 255)),
        (60.0, (200, 0, 200)),
    ],
    true,
);

/// VIL Density (g/m³).
static VIL_DENSITY: ColorScale = &(
    &[
        (0.5, (100, 100, 100)), // dispatcher renders < 0.5 transparent
        (1.0, (0, 100, 255)),
        (1.5, (0, 200, 255)),
        (2.0, (0, 200, 0)),
        (2.5, (0, 150, 0)),
        (3.0, (255, 255, 0)),
        (3.5, (255, 130, 0)), // below here severe hail is rare
        (4.0, (255, 0, 0)),   // at and above, nearly every storm severe
        (4.5, (200, 0, 0)),
        (5.0, (255, 0, 255)),
        (6.0, (255, 255, 255)),
    ],
    true,
);

/// Probability of Severe Hail (%).
static POSH: ColorScale = &(
    &[
        (10.0, (100, 100, 100)), // dispatcher renders < 10 transparent
        (20.0, (0, 100, 255)),
        (30.0, (0, 200, 255)),
        (40.0, (0, 200, 0)),
        (50.0, (255, 255, 0)), // POSH = 50 at SHI = WT: the decision level
        (60.0, (255, 200, 0)),
        (70.0, (255, 150, 0)),
        (80.0, (255, 0, 0)),
        (90.0, (200, 0, 0)),
        (100.0, (255, 0, 255)),
    ],
    false,
);

/// Maximum Expected Hail Size (**inches** — the render seam converts the derived
/// field's mm, `crate::hail`).
static MEHS: ColorScale = &(
    &[
        (0.25, (100, 100, 100)), // dispatcher renders < 0.25 transparent
        (0.5, (0, 100, 255)),
        (0.75, (0, 200, 255)),
        (1.0, (0, 200, 0)), // NWS severe criterion (1.00 in, 10-511 / 2010)
        (1.25, (255, 255, 0)),
        (1.5, (255, 200, 0)),
        (1.75, (255, 150, 0)),
        (2.0, (255, 0, 0)), // SPC significant-severe (2.00 in)
        (2.5, (200, 0, 0)),
        (3.0, (255, 0, 255)),
        (3.5, (200, 0, 200)),
        (4.0, (255, 255, 255)), // the cell product's "> 4.00" cap
    ],
    false,
);

/// Hydrometeor Classification.
static HHC: ColorScale = &(
    &[
        (10.0, (128, 128, 128)),  // BI (Biological)
        (20.0, (128, 0, 128)),    // AP (Ground clutter)
        (30.0, (173, 216, 230)),  // IC (Ice crystals)
        (40.0, (0, 100, 255)),    // DS (Dry snow)
        (50.0, (0, 200, 255)),    // WS (Wet snow)
        (60.0, (0, 200, 0)),      // RA (Rain)
        (70.0, (0, 100, 0)),      // HR (Heavy rain)
        (80.0, (255, 255, 0)),    // BD (Big drops)
        (90.0, (255, 150, 0)),    // GR (Graupel)
        (100.0, (255, 0, 0)),     // HA (Hail w/ rain)
        (110.0, (200, 0, 0)),     // LH (Large hail)
        (120.0, (255, 200, 200)), // GH (Giant hail)
        (130.0, (155, 120, 80)),  // MS (Melting snow) — hc_256.plt's own
        (140.0, (200, 200, 200)), // UK (Unknown)
        (150.0, (100, 0, 150)),   // RF (Range folded)
    ],
    false,
);

/// Precipitation Rate (in/hr).
static PRECIP_RATE: ColorScale = &(
    &[
        (0.01, (100, 100, 100)), // dispatcher renders < 0.01 transparent
        (0.1, (0, 150, 255)),
        (0.25, (0, 100, 255)),
        (0.5, (0, 255, 0)),
        (1.0, (255, 255, 0)),
        (2.0, (255, 200, 0)),
        (3.0, (255, 150, 0)),
        (4.0, (255, 0, 0)),
        (6.0, (200, 0, 0)),
        (8.0, (255, 0, 255)),
        (12.0, (200, 0, 200)),
    ],
    true,
);

/// The NROT tables' first stop, and the cut in [`nrot_lookup`] above.
const NROT_FIRST_CLASS: f32 = crate::nrot::SIGNIFICANT as f32;

/// NROT cyclonic / positive rotation (unitless)
static NROT_CYCLONIC: ColorScale = &(
    &[
        (NROT_FIRST_CLASS, (64, 64, 128)), // weak: slate...
        (0.999, (0, 0, 255)),              // ...to blue
        (1.0, (0, 192, 0)),                // significant: green...
        (1.499, (64, 255, 64)),
        (1.5, (192, 192, 64)), // strong: olive to yellow
        (1.999, (255, 255, 0)),
        (2.0, (192, 64, 64)), // very strong: brick to red
        (2.499, (255, 0, 0)),
        (2.5, (255, 0, 255)), // extreme: solid magenta
        (2.999, (255, 0, 255)),
        (3.0, (255, 255, 255)), // off the chart: white
    ],
    true,
);

/// NROT anticyclonic / negative rotation (thresholds = abs values)
static NROT_ANTICYCLONIC: ColorScale = &(
    &[
        (NROT_FIRST_CLASS, (48, 96, 64)), // weak: dim green...
        (0.999, (96, 192, 128)),
        (1.0, (0, 160, 0)), // significant: green,
        (2.0, (0, 255, 0)), // brightening and then solid
    ],
    true,
);

// ————————————————————————————————————————————————————————————————————
// Legend scale extraction
// ————————————————————————————————————————————————————————————————————

/// Color bar description for a product.
#[derive(Clone)]
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
    LegendScale {
        thresholds: entries,
        is_gradient: gradient,
        min_value: min,
        max_value: max,
    }
}

/// Merge inbound (negative) and outbound (positive) tables into one scale.
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
    LegendScale {
        thresholds: entries,
        is_gradient: gradient,
        min_value: min,
        max_value: max,
    }
}

/// Thresholds are in the same unit domain as `get_color_for_value()`.
fn build_legend_scale(product: RadarProduct) -> LegendScale {
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
        RadarProduct::EchoTops | RadarProduct::EchoTopsInterpolated => extract_scale(ECHO_TOPS),
        RadarProduct::VerticallyIntegratedLiquid => extract_scale(VIL),
        RadarProduct::VilDensity => extract_scale(VIL_DENSITY),
        RadarProduct::ProbabilityOfSevereHail => extract_scale(POSH),
        RadarProduct::MaxExpectedHailSize => extract_scale(MEHS),
        RadarProduct::HydrometeorClassification => extract_scale(HHC),
        RadarProduct::PrecipitationRate => extract_scale(PRECIP_RATE),
        RadarProduct::NormalizedRotation => {
            // NROT is unitless, so no conversion.
            merge_bidirectional(NROT_ANTICYCLONIC, NROT_CYCLONIC, 1.0)
        }
    }
}

/// [`build_legend_scale`]'s answer for every product, built once.
pub(crate) fn legend_scale_static(product: RadarProduct) -> &'static LegendScale {
    static ALL: LazyLock<Vec<LegendScale>> = LazyLock::new(|| {
        RadarProduct::all()
            .iter()
            .map(|&p| build_legend_scale(p))
            .collect()
    });
    &ALL[product as usize]
}

/// Thresholds are in the same unit domain as `get_color_for_value()`.
pub fn get_legend_scale(product: RadarProduct) -> LegendScale {
    legend_scale_static(product).clone()
}

/// [`LegendScale`] without the allocation: the built-once table's own entry,
/// borrowed.
#[derive(Clone, Copy, Debug)]
pub struct LegendScaleRef {
    /// Colour stops, sorted ascending by value.
    pub thresholds: &'static [(f32, [u8; 3])],
    pub is_gradient: bool,
    pub min_value: f32,
    pub max_value: f32,
}

/// The borrowed [`LegendScale`] for `product`. See [`LegendScaleRef`].
pub fn get_legend_scale_ref(product: RadarProduct) -> LegendScaleRef {
    let scale = legend_scale_static(product);
    LegendScaleRef {
        thresholds: &scale.thresholds,
        is_gradient: scale.is_gradient,
        min_value: scale.min_value,
        max_value: scale.max_value,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every colour scale in this file.
    const ALL_SCALES: &[(&str, ColorScale)] = &[
        ("REFLECTIVITY", REFLECTIVITY),
        ("VELOCITY_OUTBOUND", VELOCITY_OUTBOUND),
        ("VELOCITY_INBOUND", VELOCITY_INBOUND),
        ("SPECTRUM_WIDTH", SPECTRUM_WIDTH),
        ("ZDR", ZDR),
        ("RHO", RHO),
        ("PHI", PHI),
        ("KDP", KDP),
        ("ECHO_TOPS", ECHO_TOPS),
        ("VIL", VIL),
        ("VIL_DENSITY", VIL_DENSITY),
        ("POSH", POSH),
        ("MEHS", MEHS),
        ("HHC", HHC),
        ("PRECIP_RATE", PRECIP_RATE),
        ("NROT_CYCLONIC", NROT_CYCLONIC),
        ("NROT_ANTICYCLONIC", NROT_ANTICYCLONIC),
    ];

    /// The number of colour scales this module documents, as a **literal**.
    const SCALE_COUNT: usize = 17;

    /// The name a line declares a [`ColorScale`] under, if it declares one.
    fn declared_scale_name(line: &str) -> Option<&str> {
        let mut rest = line.trim_start();
        if let Some(after_pub) = rest.strip_prefix("pub") {
            rest = match after_pub.strip_prefix('(') {
                Some(restriction) => restriction.split_once(')')?.1,
                None => after_pub,
            }
            .trim_start();
        }
        let rest = rest
            .strip_prefix("static ")
            .or_else(|| rest.strip_prefix("const "))?;
        let (name, ty) = rest.split_once(':')?;
        if ty.trim_start().starts_with("ColorScale") {
            Some(name.trim())
        } else {
            None
        }
    }

    #[test]
    fn every_colour_scale_static_is_registered() {
        for (line, expected) in [
            ("static X: ColorScale = &(", Some("X")),
            ("pub static X: ColorScale = &(", Some("X")),
            ("pub(crate) static X: ColorScale = &(", Some("X")),
            ("pub(super) static X: ColorScale = &(", Some("X")),
            ("pub(in crate::render) static X: ColorScale = &(", Some("X")),
            ("    pub(crate) const X: ColorScale = &(", Some("X")),
            ("static X : ColorScale = &(", Some("X")),
            ("const TRANSPARENCY: u8 = 180;", None),
            (
                "pub const RANGE_FOLDED: (u8, u8, u8, u8) = (178, 102, 204, 180);",
                None,
            ),
            ("type ColorScale = &'static (ColorThresholds, bool);", None),
            ("const ALL_SCALES: &[(&str, ColorScale)] = &[", None),
            ("/// static X: ColorScale is prose, not a declaration", None),
        ] {
            assert_eq!(
                declared_scale_name(line),
                expected,
                "the declaration scanner misreads `{line}`",
            );
        }

        let declared: Vec<&str> = include_str!("palette.rs")
            .lines()
            .filter_map(declared_scale_name)
            .collect();

        // precondition: a literal floor, not one derived from ALL_SCALES.
        assert!(
            declared.len() >= SCALE_COUNT,
            "the source scan found only {} colour-scale declarations, and this \
             module documents {SCALE_COUNT}. Suspect `declared_scale_name` — \
             a visibility or keyword it does not know — and do NOT reconcile \
             this by deleting rows from ALL_SCALES, which would leave a live \
             scale swept by nothing.",
            declared.len(),
        );

        for name in &declared {
            assert!(
                ALL_SCALES.iter().any(|&(n, _)| n == *name),
                "colour scale `{name}` is declared in this file but is in no \
                 sweep; add it to ALL_SCALES",
            );
        }
        for &(name, _) in ALL_SCALES {
            assert!(
                declared.contains(&name),
                "ALL_SCALES lists `{name}`, which this file does not declare",
            );
        }

        for (i, &(name, scale)) in ALL_SCALES.iter().enumerate() {
            for &(other_name, other) in &ALL_SCALES[..i] {
                assert!(
                    !std::ptr::eq(scale, other),
                    "ALL_SCALES rows `{other_name}` and `{name}` are the same \
                     table, so one of the two named scales is never swept",
                );
            }
        }

        assert_eq!(
            declared.len(),
            ALL_SCALES.len(),
            "ALL_SCALES lists {} scales and the file declares {}",
            ALL_SCALES.len(),
            declared.len(),
        );
    }

    #[test]
    fn every_scales_thresholds_ascend() {
        assert!(
            ALL_SCALES.len() >= SCALE_COUNT,
            "this checked {} scales, not the {SCALE_COUNT} the module has",
            ALL_SCALES.len(),
        );
        for &(name, scale) in ALL_SCALES {
            let &(thresholds, _) = scale;
            assert!(
                !thresholds.is_empty(),
                "{name} has no stops, so `scale_color`'s `thresholds[0]` panics",
            );
            for (i, pair) in thresholds.windows(2).enumerate() {
                let (lower, upper) = (pair[0].0, pair[1].0);
                assert!(
                    lower <= upper,
                    "{name} stop {i} is {lower} and stop {} is {upper}: the \
                     table is out of order, so the binary search and the walk \
                     it replaced disagree about which stop owns a value",
                    i + 1,
                );
            }
        }
    }

    #[test]
    fn the_binary_search_paints_what_the_linear_scan_painted() {
        /// [`scale_color`]'s body before it became a binary search, verbatim.
        fn linear_scan(scale: ColorScale, value: f32) -> (u8, u8, u8) {
            let &(thresholds, gradient) = scale;
            let mut color = thresholds[0].1;
            let mut last_threshold = thresholds[0].0;
            for (i, &(threshold, c)) in thresholds.iter().enumerate() {
                if value >= threshold {
                    color = c;
                    last_threshold = threshold;
                } else {
                    if gradient && i > 0 && threshold > last_threshold && last_threshold.is_finite()
                    {
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

        /// The next representable `f32` toward `+inf` (`up`) or `-inf`.
        fn neighbour(x: f32, up: bool) -> f32 {
            if x.is_nan() {
                return x;
            }
            if x == 0.0 {
                return if up {
                    f32::from_bits(1)
                } else {
                    -f32::from_bits(1)
                };
            }
            let away_from_zero = up == (x > 0.0);
            if x.is_infinite() && away_from_zero {
                return x;
            }
            let bits = x.to_bits();
            f32::from_bits(if away_from_zero { bits + 1 } else { bits - 1 })
        }

        /// Steps across each scale's own domain plus a half-span skirt at each
        /// end.
        const DENSE: usize = 6_000;

        let mut checked = 0usize;
        for &(name, scale) in ALL_SCALES {
            let &(thresholds, _) = scale;

            let mut probes: Vec<f32> = vec![
                f32::NAN,
                -f32::NAN,
                f32::INFINITY,
                f32::NEG_INFINITY,
                0.0,
                -0.0,
                -1e9,
                1e9,
            ];
            for &(t, _) in thresholds {
                probes.push(t);
                probes.push(neighbour(t, true));
                probes.push(neighbour(t, false));
                for d in [
                    -1e-6, 1e-6, -1e-3, 1e-3, -0.25, 0.25, -1.0, 1.0, -37.0, 37.0,
                ] {
                    probes.push(t + d);
                }
            }
            for pair in thresholds.windows(2) {
                let (lower, upper) = (pair[0].0, pair[1].0);
                for f in [0.25, 0.5, 0.75] {
                    probes.push(lower + f * (upper - lower));
                }
            }
            let finite: Vec<f32> = thresholds
                .iter()
                .map(|&(t, _)| t)
                .filter(|t| t.is_finite())
                .collect();
            let lo = *finite.first().expect("every scale has a finite stop");
            let hi = *finite.last().expect("every scale has a finite stop");
            let span = (hi - lo).max(1.0);
            for k in 0..=DENSE {
                probes.push(lo - span / 2.0 + 2.0 * span * (k as f32 / DENSE as f32));
            }

            // precondition: this table really got the dense sweep.
            assert!(
                probes.len() >= 6_000,
                "{name} was probed {} times, which is not a dense sweep",
                probes.len(),
            );

            for &value in &probes {
                let scanned = linear_scan(scale, value);
                let searched = scale_color(scale, value);
                assert_eq!(
                    searched,
                    scanned,
                    "{name} paints {value} (bits {:#010x}) as {searched:?}, \
                     the linear scan it replaced paints it {scanned:?}",
                    value.to_bits(),
                );
                checked += 1;
            }
        }

        // preconditions, both as literals.
        assert!(
            ALL_SCALES.len() >= SCALE_COUNT,
            "the sweep ran over {} scales, not the {SCALE_COUNT} the module has",
            ALL_SCALES.len(),
        );
        assert!(
            checked >= 100_000,
            "{checked} probes is far short of a dense sweep of {SCALE_COUNT} \
             scales; a narrowed bound, not a pass",
        );
    }

    /// Every visible spectrum-width band is the operational source's own.
    /// The expected colours below are decoded from the RPG's own file, not read
    /// back out of [`SPECTRUM_WIDTH`]. ORPG Build 21.0r1.7,
    /// `src/code_util/tsk001/config/colors/sw_8.plt`, is a count `8` then three
    /// planes of eight integers — reds, greens, blues:
    ///
    /// ```text
    /// 8
    /// 0 118 156 0 255 208 255 119     (line 2, red)
    /// 0 118 156 187 0 112 255 0       (line 3, green)
    /// 0 118 156 0 0 0 0 125           (line 4, blue)
    /// ```
    ///
    /// so level *i* is `(red[i], green[i], blue[i])`. Level 0 is
    /// below-threshold black and level 7 is the fold ([`RANGE_FOLDED`]); levels
    /// 1–6 are the six bands this palette carries, at the product's 4-knot bin
    /// edges.
    #[test]
    fn spectrum_width_is_the_rpgs_sw_8_table() {
        const SW_8_RED: [u8; 8] = [0, 118, 156, 0, 255, 208, 255, 119];
        const SW_8_GREEN: [u8; 8] = [0, 118, 156, 187, 0, 112, 255, 0];
        const SW_8_BLUE: [u8; 8] = [0, 118, 156, 0, 0, 0, 0, 125];
        /// 1 kt = 1852 m / 3600 s, exactly (BIPM).
        const KT_TO_MS: f32 = 1852.0 / 3600.0;

        let &(stops, _) = SPECTRUM_WIDTH;
        assert_eq!(
            stops.len(),
            6,
            "sw_8 has six visible levels (1..=6); level 0 is below threshold \
             and level 7 is the fold",
        );

        for (i, &(threshold, colour)) in stops.iter().enumerate() {
            let level = i + 1;
            assert_eq!(
                colour,
                (SW_8_RED[level], SW_8_GREEN[level], SW_8_BLUE[level]),
                "stop {i} is not sw_8.plt level {level}",
            );

            // The bins are the product's own 4 kt steps: level 1 opens at
            // 0 kt, level 6 at 20 kt.
            let expected = (level as f32 - 1.0) * 4.0 * KT_TO_MS;
            assert!(
                (threshold - expected).abs() < 1e-4,
                "stop {i} sits at {threshold} m/s, not the {} kt bin edge \
                 {expected} m/s",
                (level - 1) * 4,
            );

            let probe = expected + 1.0;
            assert_eq!(
                get_color_for_value(RadarProduct::SpectrumWidth, probe),
                (
                    SW_8_RED[level],
                    SW_8_GREEN[level],
                    SW_8_BLUE[level],
                    TRANSPARENCY
                ),
                "{probe} m/s does not paint sw_8.plt level {level}",
            );
        }
    }

    #[test]
    fn every_rpg_hydrometeor_class_has_its_own_colour() {
        /// `hc.lgd` lines 4–19, code and displayed code, verbatim.
        const HC_LGD: [(f32, &str); 16] = [
            (0.0, "ND"),
            (10.0, "BI"),
            (20.0, "GC"),
            (30.0, "IC"),
            (40.0, "DS"),
            (50.0, "WS"),
            (60.0, "RA"),
            (70.0, "HR"),
            (80.0, "BD"),
            (90.0, "GR"),
            (100.0, "HA"),
            (110.0, "LH"),
            (120.0, "GH"),
            (130.0, "MS"),
            (140.0, "UK"),
            (150.0, "RF"),
        ];
        /// `hc_256.plt`, data levels 129–131.
        const HC_256_MELTING_SNOW: (u8, u8, u8, u8) = (155, 120, 80, TRANSPARENCY);

        let legend = get_legend_scale(RadarProduct::HydrometeorClassification);

        assert_eq!(
            get_color_for_value(RadarProduct::HydrometeorClassification, 0.0),
            (0, 0, 0, 0),
            "hc.lgd's class 0 (ND) is below threshold and must not paint",
        );

        for &(code, label) in &HC_LGD[1..] {
            assert!(
                legend.thresholds.iter().any(|&(t, _)| t == code),
                "hc.lgd class {code} ({label}) has no stop of its own, so \
                 scale_color paints it as the class below it",
            );
        }

        for (i, &(code, label)) in HC_LGD[1..].iter().enumerate() {
            for &(other, other_label) in &HC_LGD[1..][..i] {
                assert_ne!(
                    get_color_for_value(RadarProduct::HydrometeorClassification, code),
                    get_color_for_value(RadarProduct::HydrometeorClassification, other),
                    "class {code} ({label}) and class {other} ({other_label}) \
                     paint the same colour",
                );
            }
        }

        assert_eq!(
            get_color_for_value(RadarProduct::HydrometeorClassification, 130.0),
            HC_256_MELTING_SNOW,
            "melting snow is not hc_256.plt's melting snow",
        );
    }

    #[test]
    fn zdr_below_the_finite_stops_is_dark_gray_not_black() {
        let color = get_color_for_value(RadarProduct::DifferentialReflectivity, -5.0);
        assert_eq!(color, (66, 66, 66, TRANSPARENCY));
    }

    #[test]
    fn non_finite_input_is_transparent_for_every_product() {
        let products = [
            RadarProduct::Reflectivity,
            RadarProduct::Velocity,
            RadarProduct::StormRelativeVelocity,
            RadarProduct::SpectrumWidth,
            RadarProduct::DifferentialReflectivity,
            RadarProduct::CorrelationCoefficient,
            RadarProduct::DifferentialPhase,
            RadarProduct::SpecificDifferentialPhase,
            RadarProduct::NormalizedRotation,
            RadarProduct::VilDensity,
            RadarProduct::ProbabilityOfSevereHail,
            RadarProduct::MaxExpectedHailSize,
        ];
        for product in products {
            for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
                assert_eq!(
                    get_color_for_value(product, bad),
                    (0, 0, 0, 0),
                    "{product:?} should render {bad} transparent"
                );
            }
        }
    }

    #[test]
    fn the_range_folded_colour_is_unreachable_through_any_products_scale() {
        assert_ne!(
            (RANGE_FOLDED.0, RANGE_FOLDED.1, RANGE_FOLDED.2),
            (100, 0, 150),
            "the range-folded colour is the HHC class-150 entry again, so a \
             change to the classification palette would repaint every folded \
             velocity gate",
        );

        let products = [
            RadarProduct::Reflectivity,
            RadarProduct::Velocity,
            RadarProduct::StormRelativeVelocity,
            RadarProduct::SpectrumWidth,
            RadarProduct::DifferentialReflectivity,
            RadarProduct::CorrelationCoefficient,
            RadarProduct::DifferentialPhase,
            RadarProduct::SpecificDifferentialPhase,
            RadarProduct::EchoTops,
            RadarProduct::EchoTopsInterpolated,
            RadarProduct::VerticallyIntegratedLiquid,
            RadarProduct::VilDensity,
            RadarProduct::ProbabilityOfSevereHail,
            RadarProduct::MaxExpectedHailSize,
            RadarProduct::HydrometeorClassification,
            RadarProduct::PrecipitationRate,
            RadarProduct::NormalizedRotation,
        ];
        let mut checked = 0usize;
        for product in products {
            for step in -20_000..=40_000 {
                let value = step as f32 / 100.0;
                assert_ne!(
                    get_color_for_value(product, value),
                    RANGE_FOLDED,
                    "{product:?} paints {value} the range-folded colour",
                );
                checked += 1;
            }
        }
        // precondition: the sweep really ran.
        assert_eq!(checked, products.len() * 60_001);
    }
}
