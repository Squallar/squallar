use crate::types::{MS_TO_MPH, RadarProduct};

const TRANSPARENCY: u8 = 180;

/// The colour of a **range-folded** gate: one whose true range is ambiguous
/// past the unambiguous range of its cut's PRF.
///
/// [`get_color_for_value`] cannot produce this, and that is the point. A folded
/// gate has no value — `MomentValue::RangeFolded` carries no number — so it
/// arrives at a renderer as `NaN`, which every product paints fully
/// transparent. A consumer that wants to *show* the fold (which
/// [`crate::sampler::SampleStatus::RangeFolded`] finally makes possible) has to
/// branch on the status and reach for this constant.
///
/// **Deliberately not the [`HHC`] table's class-150 entry**, which is the same
/// idea for a different product: that one is a hydrometeor *class* code in a
/// categorical scale, and sharing the constant would mean a future edit to the
/// classification palette silently repainting every folded velocity gate.
/// `the_range_folded_colour_is_unreachable_through_any_products_scale` pins that
/// no product's own scale can produce this colour at any value, so a folded
/// pixel is never mistaken for a measured one.
pub(crate) const RANGE_FOLDED: (u8, u8, u8, u8) = (178, 102, 204, TRANSPARENCY);

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
            // A non-finite left stop (e.g. ZDR's NEG_INFINITY floor) would make
            // `t` NaN; fall through to the flat color instead.
            if gradient && i > 0 && threshold > last_threshold && last_threshold.is_finite() {
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
/// Non-finite values render transparent for every product.
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
///
/// Nothing under [`nrot::SIGNIFICANT`](crate::nrot::SIGNIFICANT) is painted at
/// all — the same floor the algorithm's own despeckle counts clusters over —
/// so the first visible class of this palette *is* that constant, and the
/// tables below start their first stop on it.
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

/// Spectrum width (m/s).
static SPECTRUM_WIDTH: ColorScale = &(
    &[
        (0.0, (118, 118, 118)),
        (2.0578, (156, 156, 156)),
        (4.1156, (0, 187, 187)),
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

/// Correlation coefficient (0-1).
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

/// Differential phase (degrees, pre-wrapped to 0-360). Cyclic: the tail returns
/// toward the first color so 0° and 360° are visually continuous.
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

/// VIL Density (g/m³). Authored NWS-style around the operational hail
/// scale: Amburn & Wolf (1997, *Wea. Forecasting* 12, 473–478) found severe
/// hail rare below 3.5 g/m³ and near-universal at 4.0 and above, and the
/// NWS WDTD training scale runs interest from ~0.5 to 4.5+. Cool colors
/// below the significance break, the warm ramp igniting at 3.0 and hitting
/// red exactly at Amburn & Wolf's 4.0.
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

/// Probability of Severe Hail (%). Authored NWS/AWIPS-style: **discrete
/// 10 % steps**, the operational display resolution — the RPG itself rounds
/// POSH to the nearest 10 % (`a31559.ftn`) and the NWS WDTD "Probability of
/// Severe Hail" training page describes the product in those steps. 50 % is
/// where the warm ramp ignites: it is the curve's fixed point (POSH = 50 %
/// exactly at SHI = WT, Witt et al. 1998 Eq. 9), the paper's nominal
/// warning-decision level, and the RCM "positive hail" threshold
/// (`hail_algorithm.h`, `rcm_positive_hail` default 50). Cool colors below,
/// yellow-through-red above, magenta at the certain-severe top.
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

/// Maximum Expected Hail Size (**inches** — the render seam converts the
/// derived field's mm, `crate::hail`). Authored as the standard hail-size
/// ramp in the NWS quarter-inch reporting steps (the RPG rounds MEHS to the
/// nearest ¼ in, `a31559.ftn`), with the two operational breaks: **1.00 in**
/// goes green→yellow — the NWS severe-thunderstorm hail criterion (raised
/// from ¾ in fleet-wide in 2010, NWS Instruction 10-511) — and **2.00 in**
/// goes red — SPC's "significant severe" hail threshold (Hales 1988's
/// sig-severe convention). The cell product's own display caps at
/// "> 4.00 in" (`a31644.ftn`), so the table tops out white at 4.
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

/// Hydrometeor Classification. Categorical: thresholds are the ICD class
/// codes, 0 = ND.
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

/// The NROT tables' first stop, and the cut in [`nrot_lookup`] above: one
/// name for the palette's own first visible class, taken by reference from
/// the algorithm that produces the field.
///
/// `as f32` of an `f64` 0.25 is exact — it is a power of two over four — so
/// no rounding sits between the despeckle's `>=` and this `<`.
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
    LegendScale {
        thresholds: entries,
        is_gradient: gradient,
        min_value: min,
        max_value: max,
    }
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
    LegendScale {
        thresholds: entries,
        is_gradient: gradient,
        min_value: min,
        max_value: max,
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// ZDR's first stop is a NEG_INFINITY floor; interpolating from it would
    /// make `t = inf/inf = NaN` and every channel would cast to 0. Values
    /// below the second stop must get the flat first-stop dark gray, not black.
    #[test]
    fn zdr_below_the_finite_stops_is_dark_gray_not_black() {
        let color = get_color_for_value(RadarProduct::DifferentialReflectivity, -5.0);
        assert_eq!(color, (66, 66, 66, TRANSPARENCY));
    }

    /// Every product renders non-finite input transparent, including velocity,
    /// whose inbound branch used to catch NaN and paint it dark green.
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

    /// A folded gate must be unmistakable: no product's own scale may produce
    /// [`RANGE_FOLDED`] at any value it can be asked about.
    ///
    /// This is what makes the constant worth having. If a reflectivity gradient
    /// happened to pass through it, a cross-section's folded pixels would be
    /// indistinguishable from a measured return at whatever dBZ landed there —
    /// which is precisely the "looks plausible, is wrong" failure the status
    /// plumbing exists to end. Swept densely over every product, including the
    /// gradient interiors where an interpolated colour lives that no table entry
    /// spells out.
    #[test]
    fn the_range_folded_colour_is_unreachable_through_any_products_scale() {
        // The class-150 entry the plan corrected: the same *idea* for the
        // hydrometeor classification, and deliberately a different constant.
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
            // −200..+400 in hundredths covers every scale's domain: dBZ and
            // classification codes at the top, m/s and ZDR in the middle,
            // ρHV and inch/hr rates near zero, and negative velocity below.
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
        // precondition: the sweep really ran, so a loop bound quietly narrowed
        // to nothing cannot leave this passing.
        assert_eq!(checked, products.len() * 60_001);
    }
}
