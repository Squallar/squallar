//! The colour scale every product is drawn with, and where each one came from.
//!
//! # Provenance, as a class
//!
//! **A colour scale is an authored claim about presentation, not a
//! measurement.** There is no oracle for "the right colour", so no scale here
//! is verified the way a derived field is. The one checkable question is the
//! one each scale's own doc has to answer: does it *reproduce* a published
//! table, or *depart* from one on purpose? Keep that answered per scale.
//! Without it, a reader diffing this crate against AWIPS or a commercial
//! viewer cannot tell a defect from a decision — and that reader is usually a
//! future maintainer looking at a bug report about colour.
//!
//! Where the seventeen scales stand:
//!
//! * **Fidelity claims**, checkable against ORPG Build 21.0r1.7's own colour
//!   tables (`colors/*.plt`, held offline): the velocity pair — byte-exact at
//!   the extremes, interpolated where the RPG hand-tunes — plus spectrum
//!   width, ZDR, ρHV, ΦDP and KDP. Each names the table it was read against.
//! * **Deliberate departures**, where the doc says what it diverges from and
//!   why: reflectivity, VIL, precipitation rate, echo tops.
//! * **Authored around a published *breakpoint*, never a published colour**:
//!   VIL density (Amburn & Wolf 1997), POSH (Witt et al. 1998 Eq. 9), MEHS
//!   (NWS Instruction 10-511). Those citations are real and they are about the
//!   thresholds; the colours beside them are ours.
//! * **Categorical, keyed to codes rather than to a scheme**: HHC — the
//!   thresholds are the ICD class codes, the colours are ours.
//! * **No external counterpart of any kind**: the two NROT ramps, for a field
//!   no authority publishes, whose class boundaries come from the algorithm
//!   that produces it and whose colours were chosen here.
//!
//! Two tests in this module reach outside the tree and are the only ones that
//! can: `spectrum_width_is_the_rpgs_sw_8_table` pins six stops against the
//! RPG's `sw_8` table and has already caught a stray blue channel, and
//! `every_rpg_hydrometeor_class_has_its_own_colour` reads `hc.lgd`'s class
//! list. The rest — reachability, ordering, that no product's scale can reach
//! [`RANGE_FOLDED`] — are internal consistency, which is all the authored
//! scales admit.

use crate::types::{MS_TO_MPH, RadarProduct};
use std::sync::LazyLock;

const TRANSPARENCY: u8 = 180;

/// The colour of a **range-folded** gate: one whose true range is ambiguous
/// past the unambiguous range of its cut's PRF.
///
/// [`get_color_for_value`] cannot produce this, and that is the point. A folded
/// gate has no value — `MomentValue::RangeFolded` carries no number — so it
/// arrives at a renderer as `NaN`, which every product paints fully
/// transparent. A consumer that wants to *show* the fold has to carry the
/// status alongside the number and reach for this constant: the vertical views
/// through [`crate::sampler::SampleStatus::RangeFolded`], the plan view through
/// the NaN payload `crate::render`'s gate loop claims a folded pixel with. Two
/// carriers because the two rasters are shaped differently, one colour because
/// a fold looks the same from either view.
///
/// **Deliberately not the [`HHC`] table's class-150 entry**, which is the same
/// idea for a different product: that one is a hydrometeor *class* code in a
/// categorical scale, and sharing the constant would mean a future edit to the
/// classification palette silently repainting every folded velocity gate.
/// `the_range_folded_colour_is_unreachable_through_any_products_scale` pins that
/// no product's own scale can produce this colour at any value, so a folded
/// pixel is never mistaken for a measured one.
///
/// Exported because a colour with no key is a colour the reader has to guess
/// at: `rustdar-egui`'s velocity and spectrum-width legends draw an `RF` swatch
/// in this exact purple, so the constant that paints the gates is the one that
/// labels them.
pub const RANGE_FOLDED: (u8, u8, u8, u8) = (178, 102, 204, TRANSPARENCY);

/// Ascending-threshold color scale: for value `v`, the color of the last entry
/// whose threshold is <= `v`. The `bool` picks gradient (linear interpolation
/// between stops) over discrete steps.
type ColorThresholds = &'static [(f32, (u8, u8, u8))];
type ColorScale = &'static (ColorThresholds, bool);

/// The colour a scale gives `value`: the last stop at or below it, blended
/// toward the next stop where the scale is a gradient.
///
/// # The stops are searched, not walked
///
/// This runs **once per painted pixel**, and there are millions of them:
/// [`crate::render`]'s colour pass derives one colour per pixel and is the
/// pass whose own doc calls it dominant when it runs serially, and
/// [`crate::xsect`]'s fills are the same shape one pixel at a time. Placing a
/// value used to walk the table from the bottom — up to twenty-four stops,
/// [`PHI`] being that long, with [`REFLECTIVITY`] at twenty-two and the other
/// fifteen tables between four and fifteen — and is a
/// [`partition_point`](slice::partition_point) over the same table now.
///
/// **That is a bound on the work, not a measured speedup, and quite possibly
/// not a speedup at all.** Nobody has measured it, and the arithmetic argues
/// both ways: the walk exits early, and on most of these tables it exits soon.
/// Twelve of the seventeen are twelve stops or shorter — a table that fits in
/// one or two cache lines, where four data-dependent branches have no obvious
/// edge on a sequential scan a predictor sees coming. Even on [`REFLECTIVITY`]
/// the dBZ a radar image is mostly made of sit at stops three through nine,
/// against five steps of binary search. Whatever is there is concentrated in
/// [`PHI`] and the high-dBZ tail. What the rewrite buys unconditionally is
/// that the per-pixel cost stops scaling with how far up its own table a value
/// lands, and — through the tests below — the first check that the tables are
/// ordered the way this module has always said they are.
///
/// **The answer is the same one, not a near one**, which is the only thing
/// that makes the swap admissible in a crate whose products are pinned
/// byte-for-byte:
///
/// * The predicate `threshold <= value` *is* the walk's acceptance test
///   `value >= threshold`, so the index it returns is the index the walk broke
///   out on.
/// * [`ZDR`]'s `NEG_INFINITY` floor is accepted by every finite value, exactly
///   as `value >= -inf` was. This one is load-bearing: it is the only stop in
///   the module that is not a finite number, and it reaches here on live data.
/// * Equal adjacent stops all sit in the accepted prefix, so the *later* one
///   wins — as it did when each loop iteration overwrote the last.
/// * `NaN` is accepted by no stop, so it lands on index 0 — the flat first
///   colour the walk's first-iteration `break` yielded. No caller can observe
///   this: [`get_color_for_value`] rejects non-finite input before any scale is
///   consulted. It is pinned anyway because what is being replaced is this
///   function, not its callers.
///
/// `the_binary_search_paints_what_the_linear_scan_painted` carries the deleted
/// walk verbatim and diffs the two over every scale in this file. It is not a
/// quantised RGBA lookup table, deliberately: that is faster still and moves
/// gradient stops off the exact-digest pins.
///
/// The one thing a search needs that a walk did not is an **ascending** table.
/// The type alias above has asserted that in prose for as long as it has
/// existed, and nothing checked it; `every_scales_thresholds_ascend` does.
fn scale_color(scale: ColorScale, value: f32) -> (u8, u8, u8) {
    let &(thresholds, gradient) = scale;
    let i = thresholds.partition_point(|&(threshold, _)| threshold <= value);
    if i == 0 {
        // Below the first stop, or NaN: no stop took the value, which is where
        // the walk broke out on its first iteration.
        return thresholds[0].1;
    }
    let (last_threshold, color) = thresholds[i - 1];
    if i == thresholds.len() {
        // Every stop took it; the walk ran off the end holding this colour.
        return color;
    }
    let (threshold, c) = thresholds[i];
    // The walk's guard, carried over unchanged so this line still reads
    // against the loop it replaces. Two of its four conjuncts are now dead:
    // `i > 0` by the early return above, and `threshold > last_threshold`
    // because reaching here means `threshold > value >= last_threshold` — the
    // stop at `i` was the one the predicate rejected, and a rejected stop is
    // strictly above the value once `every_scales_thresholds_ascend` rules out
    // a NaN in the table. What is live is `last_threshold.is_finite()`: a
    // non-finite left stop (ZDR's NEG_INFINITY floor) would make `t` NaN and
    // cast every channel to 0, so those values take the flat color instead.
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
///
/// **Authored here, not the RPG's, and deliberately so.** ORPG Build
/// 21.0r1.7's own reflectivity tables — `colors/refl_16.plt` for the legacy
/// 16-level products and `colors/hires_refl.plt` for the super-res ones —
/// open cyan `#00ECEC` at the bottom of the scale and share only four
/// mid-range colours with this ramp, at different dBZ. This one opens grey,
/// carries no cyan at all, and spends the cool end on the 7.5–20 dBZ band.
/// It is the reflectivity leg of this crate's house ramp; anyone diffing it
/// against the RPG will find nothing in common, which is the intent and not
/// a drift.
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
///
/// The RPG's scheme at the RPG's bins — exact 10-knot steps, `#FF0000` at the
/// outbound extreme and `#00FF00` at the inbound one, both byte-exact against
/// `colors/vel_66.plt` and `colors/hires_vel1.plt` — but **interpolated where
/// the RPG hand-tunes**: these eight stops ramp 100→255 linearly across the
/// channel, and deliberately omit the RPG's grey zero-crossing band
/// (`#777777`, `#7F7777`, `#845A5A`) and its magenta high-outbound tail
/// (`#E80026`, `#C0004C`, `#A80072`). Two clean directional ramps beat a
/// third colour family at the point where the sign flips.
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
///
/// ORPG Build 21.0r1.7 configures every spectrum-width product (2, 8, 9, 10,
/// 185 in `src/code_util/tsk001/config/prod_config`) with
/// `config/colors/sw_8.plt`, an eight-entry table. Level 0 is
/// below-threshold black and level 7 is the fold, which this crate paints
/// with [`RANGE_FOLDED`] instead; levels 1–6 are the six visible bands and
/// are the six stops below. The thresholds are that product's exact 4-knot
/// bins — 2.0578 m/s is 4.0000 kt — so each stop sits on its own band's
/// lower edge.
///
/// `spectrum_width_is_the_rpgs_sw_8_table` pins all six against the file's
/// own numbers. The 4.1156 m/s stop was `#00BBBB` until that test was
/// written: a blue channel of `0xBB` where `sw_8` has `0x00`, which turned
/// the source's green band cyan.
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

/// Differential reflectivity ZDR (dB). Derived from the RPG's `zdr_16.plt`
/// (product 158/159, `prod_config`) — the same hue order, `#FFFFFF` at the
/// top exactly, and several stops that are a uniform per-channel offset from
/// it (`#7B67A3` is `#8C78B4` less 17 on every channel) — but re-toned, not
/// transcribed. Recorded as a derivation so nobody reads the residual as
/// drift from a table this was never a copy of.
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
///
/// The colours are `colors/cc_16.plt`'s. ORPG Build 21.0r1.7's `prod_config`
/// pairs that file with `legends/cc_raw_5.lgd` for products 161 and 167, the
/// operational digital-ρhv displays — i.e. the display of the very field
/// this palette paints. `cc_raw_5.lgd` assigns fourteen colours at explicit
/// data levels, converted by its own declared scale 300 / offset −60
/// (`(level + 60) / 300`, the same 300 `test_base_prods_8bit_main.c:115`
/// gives ρhv):
///
/// | ρ | 0.207 | 0.45 | 0.65 | 0.75 | 0.80 | 0.85 | 0.90 | 0.93 | 0.95 | 0.96 | 0.97 | 0.98 | 0.99 | 1.00 |
/// |---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
/// | | grey | `#14148C` | `#0000D9` | `#8787FF` | `#55FF55` | `#87CF00` | `#FFFF00` | `#FFC800` | `#FF8C00` | `#FF2D00` | `#E10000` | `#A00000` | `#990062` | `#FF8CAA` |
///
/// Seven gradient stops cannot resolve fourteen discrete steps, so this table
/// keeps the anchors a forecaster actually reads and drops the four reds
/// packed between 0.95 and 0.99. What survives lands where the RPG puts it:
/// blue at 0.45 (exact), periwinkle at 0.75 (exact), yellow at 0.90 (exact),
/// orange at 0.96 against the RPG's 0.95, magenta at 0.98 against its 0.99.
/// The two loose stops are 0.55's lightened `#0000D9` and 0.80's `#87CF00`,
/// 0.10 and 0.05 *below* where the source puts those colours. Every deviation
/// is low or zero; none is high.
///
/// **Not `cc_64.plt`/`cc_064.lgd`.** The same `prod_config` gives that pair
/// only to the *test* raw-data products 605 and 705, and `cc_064.lgd` carries
/// no level→colour assignment at all — only tick labels — so its 62 colours
/// spread linearly over data level rather than sitting at the operational
/// thresholds. Measured against that spread these stops appear 0.08–0.13
/// *high*; measured against the operational legend they are 0.00–0.10 low.
/// The operational legend is the one that decides.
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
///
/// **Authored, and deliberately not the RPG's**, which paints ΦDP as a
/// greyscale: `colors/phi_64.plt` is 52 grey steps under a red top, and the
/// 55-colour `generic_method_5_86.plt` that `legends/phi_raw_5.lgd` selects
/// for product 168 is the same idea at more levels. A cyclic rainbow is the
/// choice here because the field is an angle.
///
/// **The 360° wrap loses nothing, because 360° is the whole domain.** The
/// input is the Level II ΦDP moment, and ORPG Build 21.0r1.7 states its
/// encoding twice: `src/cpc102/tsk018/test_base_prods_8bit_main.c:103-105`
/// gives `data_offset = 2.0` with `data_scale = 2.8361 /* 10-bit */` and
/// `0.70277 /* 8-bit */`, and `src/cpc102/tsk085/superes8bit.c:20,21,776`
/// spells the direction out as `t = roundf((f * PHI_SCALE) + PHI_OFFSET)`.
/// The scale is levels **per degree**, so the 10-bit moment spans
/// `(1023 − 2) / 2.8361 = 360.00°` and the 8-bit product
/// `(255 − 2) / 0.70277 = 360.0°`. `rem_euclid(360.0)` is therefore an
/// identity on every value this product can carry.
///
/// The ~717° span one can read off `legends/phi.lgd` is that same 2.8361 read
/// backwards, as degrees per level; no `prod_config` row references that
/// file, and both files that products do reference agree on 360°.
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

/// Specific differential phase KDP (deg/km). Derived from the RPG's
/// `kdp_16.plt` (products 162/163, `prod_config`): `#767676`, `#4B4B4B`,
/// `#4B0000`, `#14B932` and `#0AFF0A` are that table's exactly, but this one
/// drops its two pinks and darkens the top two stops where the RPG lightens.
/// Recorded as a derivation, not a transcription with drift.
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
///
/// **The house ramp, authored — not the RPG's `hreet`/`et_16` tables**, which
/// put `#0000F5` at 25 kft where this is green and agree with none of these
/// twelve stops. `#646464 → #0064FF → #00C8FF → #00C800 → #FFFF00 → #FFC800
/// → #FF9600 → #FF0000 → #C80000 → #FF00FF → #C800C8 → #FFFFFF` is one ramp
/// reused across the volume products — this one, [`VIL`], [`VIL_DENSITY`],
/// [`POSH`], [`MEHS`], [`PRECIP_RATE`] and [`HHC`] — so they read as one
/// family and a forecaster learns the colours once. Only the *breakpoints*
/// are per-product, and where those cite an authority the doc says which.
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

/// Vertically Integrated Liquid (kg/m2). The house ramp described on
/// [`ECHO_TOPS`], at VIL's own linear breakpoints — **not** the RPG's
/// `dvil_255.plt`/`dvil_66.plt`, whose `dvil_2.lgd` breakpoints are
/// nonlinear and whose colours agree with none of these eleven stops.
/// Authored, deliberately.
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
///
/// **The codes are the RPG's and the colours are ours** — a deliberate split,
/// recorded here because a silence would read as a failed transcription.
/// ORPG Build 21.0r1.7 configures product 177 with `legends/hc.lgd` +
/// `colors/hc_256.plt` (`src/code_util/tsk001/config/prod_config`); of the
/// fourteen visible classes only HA/100's `#FF0000` coincides with this
/// table, because these colours are the same authored house ramp that
/// `ECHO_TOPS`, `VIL`, `VIL_DENSITY`, `POSH`, `MEHS` and `PRECIP_RATE`
/// share, not a transcription of the RPG's.
///
/// **Class 130 (MS, melting snow) is the one exception, and takes the RPG's
/// own `#9B7850`.** The house ramp has no melting-snow entry to be consistent
/// with, and omitting the code was worse than either choice: because
/// [`scale_color`] takes the last stop at or below the value, an MS gate did
/// not go unpainted — it read as class 120, giant hail. A class this table
/// does not know must never come out as a *different* class.
///
/// The table spans the whole class-code space, not the subset this crate can
/// currently produce: [`crate::hhc`] composites `crate::hca`'s
/// `CLASS_EXTERNAL`, which stops at 140, so 110/120/130/150 arrive only from
/// an RPG-generated object. That is the point — the codes are the product's,
/// and a code with no stop is a wrong answer waiting for its first gate.
/// `every_rpg_hydrometeor_class_has_its_own_colour` enumerates `hc.lgd`
/// against this table so the next hole is caught by construction.
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

/// Precipitation Rate (in/hr). The house ramp described on [`ECHO_TOPS`], at
/// rate breakpoints — **not** the RPG's `dpr_66v1.plt`, which product 176
/// pairs with `dpr_5v3.lgd` and which agrees with none of these eleven
/// stops. Authored, deliberately.
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
/// `as f32` of an `f64` 0.25 is exact — 0.25 is 2⁻², a power of two, and
/// every power of two in `f64`'s exponent range is representable in `f32` —
/// so no rounding sits between the despeckle's `>=` and this `<`.
const NROT_FIRST_CLASS: f32 = crate::nrot::SIGNIFICANT as f32;

/// NROT cyclonic / positive rotation (unitless)
///
/// **Original, and uniquely so: there is no published table to be faithful
/// to.** Every other scale in this module either names an ORPG `.plt` it
/// reproduces or names one it departs from; NROT has neither, because no
/// authority publishes the field. What the boundaries *are* pinned to is
/// internal — the class edges at 0.25/1.0/1.5/2.0/2.5/3.0 come from
/// [`crate::nrot`]'s own thresholds, [`NROT_FIRST_CLASS`] by reference so the
/// despeckle and the first visible colour cannot drift apart. The colours
/// themselves were chosen here and were calibrated against nothing: the
/// closed product this algorithm was reverse-engineered from clips its own
/// colour bar at −2.00…+3.00, so even eyeballing it would not settle the top
/// of this ramp. Read a disagreement with any other viewer's rotation colours
/// as two authored schemes, never as a defect in this one.
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
///
/// A legend is asked for per frame (colour-bar draw, tick layout, hover) and
/// its thresholds are an allocating `Vec`, so the per-call build was pure
/// rebuild-the-same-answer work. Built by **calling** [`build_legend_scale`]
/// over [`RadarProduct::all`], never by restating any table, and indexed by
/// `product as usize` — sound under the declaration-order law
/// `product_spec::tests::all_lists_every_variant_in_declaration_order` holds.
///
/// A `LazyLock` companion function rather than a `RadarProductSpec` field
/// because a `const fn` cannot read a `static` (E0013) and the thresholds
/// allocate.
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
///
/// The allocating signature every caller has always had; the borrowed
/// reshape is E5's. The clone is of [`legend_scale_static`]'s entry, so the
/// answer is the built-once table's, byte for byte.
pub fn get_legend_scale(product: RadarProduct) -> LegendScale {
    legend_scale_static(product).clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every colour scale in this file, so the sweeps below can walk all of
    /// them instead of whichever ones somebody remembered.
    ///
    /// A list is a snapshot and snapshots rot, so this one is not trusted on
    /// its own: `every_colour_scale_static_is_registered` reads this file's own
    /// [`ColorScale`] declarations back out of the source — at any visibility,
    /// `static` or `const`, indented or not, see [`declared_scale_name`] — and
    /// fails until every one of them appears here, and every row here names one
    /// of them. Adding an eighteenth scale therefore breaks a test rather than
    /// quietly escaping the sweeps.
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
    ///
    /// Every sweep below checks itself against this rather than against
    /// `ALL_SCALES.len()`, because a floor written in terms of the registry is
    /// satisfied by an empty registry: `0 >= 0` passes, and the sweep that
    /// covered nothing reads exactly like the sweep that covered everything.
    const SCALE_COUNT: usize = 17;

    /// The name a line declares a [`ColorScale`] under, if it declares one.
    ///
    /// **Deliberately loose about how it is declared**, because the strict
    /// version of this had a hole a whole scale fits through. A scan keyed to
    /// `static NAME:` at column zero misses `pub(crate) static`, `pub static`,
    /// `const`, and anything indented — and the miss is silent in the worst
    /// way: if the scale it missed is *also* missing from [`ALL_SCALES`], the
    /// counts still agree, every assertion passes, and a live scale is swept
    /// by nothing at all. That is not hypothetical. `nrot.rs`'s prose already
    /// points at `palette::NROT_CYCLONIC`, so `pub(crate) static
    /// NROT_CYCLONIC` is a plausible next edit to this file.
    fn declared_scale_name(line: &str) -> Option<&str> {
        let mut rest = line.trim_start();
        if let Some(after_pub) = rest.strip_prefix("pub") {
            // `pub`, or any restricted form: `pub(crate)`, `pub(super)`,
            // `pub(in crate::foo)`.
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

    /// [`ALL_SCALES`] is this file's list of scales, not a copy of it that was
    /// true once.
    #[test]
    fn every_colour_scale_static_is_registered() {
        // The scanner is the part of this guard that can fail silently, so it
        // is pinned first, on the spellings that used to slip past it and on
        // the near-misses in this very file that must not be counted.
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

        // precondition: a literal floor, not one derived from ALL_SCALES. The
        // `> 10` this used to carry was satisfied by exactly the failure it
        // was meant to catch — a scanner that missed the same scales the
        // registry missed leaves the two counts agreeing at 16, or 12, or 11.
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

        // A registry row is a (name, scale) pair and only the name is checked
        // above, so `("PHI", KDP)` would satisfy every assertion so far while
        // leaving PHI unswept and sweeping KDP twice. Identity, not name.
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

    /// Every scale's stops ascend — [`scale_color`]'s binary search needs it,
    /// and until that search existed nothing checked it.
    ///
    /// The walk this file used to do tolerated a table out of order: it simply
    /// stopped at the first stop above the value and painted whatever it had.
    /// A search does not, so the property that was an unstated authoring habit
    /// is now a pinned precondition. Non-decreasing is the real requirement —
    /// equal adjacent stops sit together in the accepted prefix and resolve to
    /// the later one, which is what the walk did too — so that, and not strict
    /// ascent, is what this asserts.
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

    /// The binary search paints exactly what the linear scan painted, on every
    /// scale in this file.
    ///
    /// **Non-circular by construction:** the expected colours are produced by
    /// the deleted scan itself, carried verbatim below, not by a transcription
    /// of what it used to return. This is the same shape as `nrot.rs`'s
    /// `the_hoisted_beam_height_is_bit_identical_to_the_shared_one` — one
    /// spelling checked against the other over a dense grid, with a
    /// precondition on the grid so a bound narrowed to nothing cannot leave it
    /// passing.
    ///
    /// The probes are the four places the two spellings could have parted:
    /// every stop *exactly* (a `<` where the scan had `>=` would show here),
    /// the two representable neighbours of every stop, the gradient interiors
    /// between stops, and the non-finite inputs — `NaN` both signs, `±inf` —
    /// plus values far below the first stop and far above the last.
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
                    // A non-finite left stop (e.g. ZDR's NEG_INFINITY floor)
                    // would make `t` NaN; fall through to the flat color.
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

        /// The next representable `f32` toward `+inf` (`up`) or `-inf`, so a
        /// stop is probed on its own two neighbours and not merely near them.
        /// Spelled with `to_bits` rather than `f32::next_up` so the test
        /// builds on the 1.85 floor `nexrad-level3` pins.
        fn neighbour(x: f32, up: bool) -> f32 {
            if x.is_nan() {
                return x;
            }
            if x == 0.0 {
                // Either zero's neighbours are the smallest subnormals.
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
        /// end, so the gradient interiors are sampled far more finely than the
        /// stops are spaced.
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
                // The stop itself, then its two representable neighbours.
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
            // Every table has finite stops to span, even ZDR, whose first one
            // is NEG_INFINITY.
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

            // precondition, per scale and as a literal: this table really got
            // the dense sweep and not a handful of stray stops.
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

        // preconditions, both as literals. A `swept == ALL_SCALES.len()` count
        // stood here and was true by construction — the loop has no `continue`
        // and no `break`, so no edit to this file could have falsified it —
        // and a `checked >= ALL_SCALES.len() * DENSE` floor passed vacuously
        // over an emptied registry (`0 >= 0`) and over `DENSE` lowered to 1.
        // Neither is worth more than the line it occupies unless the number it
        // is compared against comes from outside the thing being guarded.
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
    ///
    /// **Non-circular by construction:** the expected colours below are
    /// decoded from the RPG's file, not read back out of [`SPECTRUM_WIDTH`].
    /// ORPG Build 21.0r1.7,
    /// `src/code_util/tsk001/config/colors/sw_8.plt`, is four lines — a count
    /// `8`, then three planes of eight integers, all reds, then all greens,
    /// then all blues:
    ///
    /// ```text
    /// 8
    /// 0 118 156 0 255 208 255 119     (line 2, red)
    /// 0 118 156 187 0 112 255 0       (line 3, green)
    /// 0 118 156 0 0 0 0 125           (line 4, blue)
    /// ```
    ///
    /// so level *i* is `(red[i], green[i], blue[i])`. Every spectrum-width
    /// row of `src/code_util/tsk001/config/prod_config` — products 2, 8, 9,
    /// 10 and 185, unit `kt rms` — names that file. Level 0 is
    /// below-threshold black and level 7 is the fold, which this crate paints
    /// with [`RANGE_FOLDED`]; levels 1–6 are the six bands this palette
    /// carries, at the product's 4-knot bin edges.
    ///
    /// This is the test the plan asked for: the 4.1156 m/s stop read
    /// `(0, 187, 187)` against the source's `(0, 187, 0)` — a single stray
    /// blue channel that turned the RPG's green band cyan, and the sort of
    /// slip a test written against our own table cannot see.
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

            // And the wire from the public entry point, probed *inside* the
            // band rather than on its edge so no float comparison decides it.
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

    /// Every hydrometeor class the operational source enumerates has a stop
    /// of its own, and melting snow is not painted as giant hail.
    ///
    /// **Non-circular by construction:** the class list and the melting-snow
    /// colour below are the RPG's, not ours. ORPG Build 21.0r1.7's
    /// `src/code_util/tsk001/config/prod_config` configures product 177 —
    /// this product — as `legends/hc.lgd` + `colors/hc_256.plt`, and product
    /// 165 the same way. `hc.lgd` lines 4–19 are the class list reproduced
    /// below, and `hc_256.plt` paints each class across the three data levels
    /// centred on its code: levels 129–131 carry melting snow's
    /// `(155, 120, 80)`.
    ///
    /// Class 130 had no stop in [`HHC`] until this test was written, and
    /// because [`scale_color`] takes the last stop at or below the value that
    /// did not leave an MS gate unpainted — it painted it class 120, giant
    /// hail. Hence the second half of this test: a class whose colour equals
    /// the class below it is indistinguishable from a class this table has
    /// forgotten, so no two may share one.
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

        // ND is the below-threshold class; the dispatcher's `< 10` cut paints
        // it transparent, so it is the one code with no stop.
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

        // The fall-through signature: two classes reading the same colour.
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

        // And the class the campaign found, pinned to the source's colour.
        assert_eq!(
            get_color_for_value(RadarProduct::HydrometeorClassification, 130.0),
            HC_256_MELTING_SNOW,
            "melting snow is not hc_256.plt's melting snow",
        );
    }

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
