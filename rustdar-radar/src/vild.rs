//! VIL Density (`vild`, g/m³): the RPG's **own two published products**
//! divided — Digital VIL (product 134, AWIPS `DVL`, kg/m²) over Enhanced Echo
//! Tops (product 135, AWIPS `EET`, kft above MSL) — on the shared 1° × 1 km
//! polar grid.
//!
//! ```text
//! VILD = 1000 · DVL / ((EET_published + 0.5) · 304.8)     g/m³
//! ```
//!
//! Amburn & Wolf (1997, *Weather and Forecasting* 12, 473–478) define VIL
//! density as VIL in kg/m² over the echo top in metres, and they compute it
//! from **the WSR-88D's own two products** — not from a local integration of
//! the reflectivity volume. That is what this module does, and it is why VIL
//! density is a Level III product here rather than a Level II derivation.
//!
//! # Why this replaced the local derivation
//!
//! VIL density shipped for a while as `compute_vil / compute_eet`, both local
//! Level II derivations. The 2026-07-29 survey recorded in
//! [`crate::vil`]'s validation section measured that version against exactly
//! the quotient above over 41 precipitating site-hours and found it
//! **effectively mute at the thresholds the product is read for**: 13.6% of
//! the reference's 3.5 g/m³ cells and 1.9% of its 4.0 g/m³ cells, biased low
//! in storm cores, with the whole residual attributable to
//! [`crate::vil::compute_vil`] (our VIL over the RPG's published top scored
//! POD 8.4%; the RPG's DVL over our own echo top scored 88.3%). Both inputs
//! are already fetched and drawn by the app, so building the product from them
//! costs no new datasource and is as accurate as the RPG's own products allow.
//!
//! # Accuracy limit
//!
//! Product 135 encodes `level = ⌊kft⌋ + 2`, so a decoded height is a 1 kft
//! bin's **lower edge**: a published 32 kft means the real top lies anywhere
//! in [32, 33). Dividing by the lower edge biases VILD *high* by half a bin
//! (1.7% at a 30 kft top, 3.3% at 15), so the denominator here is the bin
//! **centre** ([`EET_BIN_CENTRE_KFT`]) — the unbiased estimator under a
//! uniform sub-bin top, and the same datum the survey's reference was built
//! on before any run.
//!
//! What remains is a genuine ±0.5 kft of top uncertainty per cell, i.e. a
//! relative VILD uncertainty of `0.5/(published + 0.5)`
//! ([`quantization_halfwidth_g_m3`]): ±0.113 g/m³ at a 15 kft top and VILD
//! 3.5, ±0.085 at 20 kft, **±0.057 at 30 kft**, ±0.043 at 40, ±0.035 at 50.
//! Product 134's hybrid encoding adds ~1.3% (half its log-region step) on top.
//! **This product therefore cannot resolve VIL density finer than roughly
//! ±0.1 g/m³ at Amburn & Wolf's decision threshold**, and that is its
//! accuracy limit, not a defect of the arithmetic.
//!
//! # Volume pairing
//!
//! The two objects **must be from the same volume scan**, and a mismatched
//! pair is refused rather than painted: DVL and EET are written seconds apart
//! but a poll can catch one volume's DVL beside the previous volume's EET, and
//! a ratio of two different volumes is a plausible-looking field of a storm
//! that never existed. [`compute_vild`] answers
//! [`Refusal::VolumeMismatch`] for that, and the render seam draws nothing —
//! the same answer the hail products give for a missing sounding.

use crate::twin::compare::{self, ValueCodec};
use crate::volumetric::{RANGE_BINS, VolumetricGrid};
use chrono::NaiveDateTime;
use nexrad_level3::model::{Level3Message, ProductDescriptionBlock, RadialPacket};

/// Digital VIL — the numerator's product code (AWIPS `DVL`, kg/m²).
pub const DVL_PRODUCT_CODE: i16 = 134;

/// Enhanced Echo Tops — the denominator's product code (AWIPS `EET`, kft MSL).
pub const EET_PRODUCT_CODE: i16 = 135;

/// One kilofoot in metres, exactly: 1000 ft · 0.3048 m/ft.
const KFT_TO_M: f32 = 304.8;

/// Product 135's height quantum, kft (`level = ⌊kft⌋ + 2`).
pub const EET_QUANTUM_KFT: f32 = 1.0;

/// Half of it — the bin-centre estimate of a published echo top. See the
/// module doc: dividing by the published *lower edge* would bias VILD high by
/// this much.
pub const EET_BIN_CENTRE_KFT: f32 = EET_QUANTUM_KFT / 2.0;

/// How far apart two PDB volume-scan starts may be and still name the same
/// volume, seconds. The same window the harness branch's `twin::live::l3_twin`
/// pairs a twin by — one convention for volume identity in the crate — and two orders
/// of magnitude tighter than the 4–10 minutes between consecutive volumes, so
/// it cannot admit a neighbour.
pub const VOLUME_PAIRING_TOLERANCE_SECS: i64 = 60;

/// VIL density for one cell, g/m³: `1000 · VIL / ET` with VIL in kg/m² and
/// the echo top in **metres**, per Amburn & Wolf (1997).
///
/// `NaN` when either input is undefined, or the top is non-positive. The
/// caller supplies the top the paper's arithmetic wants — for the RPG's
/// published product that is the bin centre, which [`vild_from_published`]
/// applies.
pub fn vild_g_m3(vil_kg_m2: f32, top_kft_msl: f32) -> f32 {
    if !vil_kg_m2.is_finite() || !top_kft_msl.is_finite() || top_kft_msl <= 0.0 {
        return f32::NAN;
    }
    1000.0 * vil_kg_m2 / (top_kft_msl * KFT_TO_M)
}

/// VIL density for one cell from the RPG's own two published values:
/// [`vild_g_m3`] on the echo-top bin **centre**.
///
/// Undefined when either input is, and — per the echo-top datum — when the
/// published top is **zero** (product 135's level 2, and the topped 130: a top
/// inside the lowest kilofoot, where the quotient's denominator is not
/// resolvable at all).
pub fn vild_from_published(dvl_kg_m2: f32, eet_published_kft: f32) -> f32 {
    if !eet_published_kft.is_finite() || eet_published_kft <= 0.0 {
        return f32::NAN;
    }
    vild_g_m3(dvl_kg_m2, eet_published_kft + EET_BIN_CENTRE_KFT)
}

/// This product's own precision at a cell, g/m³: the true top lies anywhere in
/// `[published, published + 1)` kft, so the bin-centre denominator carries a
/// ±0.5 kft top error — a relative VILD uncertainty of
/// `0.5/(published + 0.5)`. Nothing tighter than this is measurable; see the
/// module doc.
pub fn quantization_halfwidth_g_m3(vild_g_m3: f32, eet_published_kft: f32) -> f32 {
    vild_g_m3 * EET_BIN_CENTRE_KFT / (eet_published_kft + EET_BIN_CENTRE_KFT)
}

/// A Level III radial packet resampled onto the 360° × 230 km polar grid and
/// decoded through the product's own codec.
///
/// The same [`compare::resample_packet_levels`] machinery every twin harness
/// scores through, stopped one step later — at physical values rather than at
/// levels, because VIL density is a *quotient* of two products whose levels
/// are not commensurable.
///
/// The 230 km display cap and each product's own range extent are the
/// resampler's: cells its packet has no gate for come back `NaN`. This is what
/// puts two products of different gate spacing and different range extent onto
/// one grid.
pub fn resampled_field(packet: &RadialPacket, gate_km: f64, codec: &ValueCodec) -> Vec<Vec<f32>> {
    compare::resample_packet_levels(packet, gate_km)
        .iter()
        .map(|row| {
            row.iter()
                .map(|&g| g.map_or(f32::NAN, |level| codec.decode(level)))
                .collect()
        })
        .collect()
}

/// The RPG's published echo tops turned into this product's denominator: bin
/// centres, `NaN` where the published top is undefined or zero.
pub fn published_top_field(eet_published_kft: &[Vec<f32>]) -> Vec<Vec<f32>> {
    eet_published_kft
        .iter()
        .map(|row| {
            row.iter()
                .map(|&kft| {
                    if kft.is_finite() && kft > 0.0 {
                        kft + EET_BIN_CENTRE_KFT
                    } else {
                        f32::NAN
                    }
                })
                .collect()
        })
        .collect()
}

/// [`vild_g_m3`] cell for cell over the 360° × 230 km grid, `NaN` wherever
/// either input is undefined or a row is short.
///
/// The field constructor the shipped product and the survey's attribution rows
/// share, so that "the product", "the reference" and the two input mixes differ
/// only in *which* VIL and *which* top they are fed.
pub fn density_field(vil_kg_m2: &[Vec<f32>], top_kft: &[Vec<f32>]) -> Vec<Vec<f32>> {
    (0..360)
        .map(|az| {
            (0..RANGE_BINS)
                .map(|r| {
                    let v = vil_kg_m2.get(az).and_then(|row| row.get(r)).copied();
                    let t = top_kft.get(az).and_then(|row| row.get(r)).copied();
                    match (v, t) {
                        (Some(v), Some(t)) => vild_g_m3(v, t),
                        _ => f32::NAN,
                    }
                })
                .collect()
        })
        .collect()
}

/// Why a pair of Level III objects cannot make a VIL-density field.
///
/// Every arm is a refusal to draw, never a substitute field: this product has
/// two inputs and no sensible answer when one of them is wrong, exactly as the
/// hail products have none without an environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The arguments are not a (134, 135) pair — the two are not
    /// interchangeable, and swapping them would divide kilofeet by kilograms.
    WrongProduct { dvl: i16, eet: i16 },
    /// One of the objects carries no radial packet, or no codec its own PDB
    /// describes.
    NoRadialData,
    /// The two objects name different volume scans, or one of them names none
    /// at all. See the module doc.
    VolumeMismatch {
        dvl: Option<NaiveDateTime>,
        eet: Option<NaiveDateTime>,
    },
}

/// Whether two PDB volume-scan starts name the same volume: both readable and
/// within [`VOLUME_PAIRING_TOLERANCE_SECS`].
///
/// An unreadable start on either side is **not** a pair — a product whose
/// volume identity cannot be read cannot be proven to belong with the other.
pub fn volumes_pair(a: Option<NaiveDateTime>, b: Option<NaiveDateTime>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => (a - b).num_seconds().abs() <= VOLUME_PAIRING_TOLERANCE_SECS,
        _ => false,
    }
}

/// The volume start two Level III PDBs must agree on, as
/// [`compare::volume_scan_started`] reads it.
pub fn volume_scan_started(pdb: &ProductDescriptionBlock) -> Option<NaiveDateTime> {
    compare::volume_scan_started(pdb)
}

/// VIL density over a volume, from the RPG's own Digital VIL and Enhanced Echo
/// Tops for **that same volume**: a 1° × 1 km polar grid in g/m³, capped at the
/// 230 km display range.
///
/// `dvl` must decode as product 134 and `eet` as product 135; both are decoded
/// through their own codecs ([`ValueCodec::for_message`] — 134's hybrid LUT and
/// 135's mask/scale/offset with its topped flag), never off raw gate bytes, and
/// resampled onto the common grid by [`resampled_field`], so the two products'
/// differing gate spacing and range extents do not have to match.
///
/// A cell is defined only where **both** inputs are and the published echo top
/// is above zero. A weak-echo column carries a defined DVL of 0.0 kg/m² with no
/// echo top at all, and reads `NaN` here rather than 0.
pub fn compute_vild(dvl: &Level3Message, eet: &Level3Message) -> Result<VolumetricGrid, Refusal> {
    if dvl.pdb.product_code != DVL_PRODUCT_CODE || eet.pdb.product_code != EET_PRODUCT_CODE {
        return Err(Refusal::WrongProduct {
            dvl: dvl.pdb.product_code,
            eet: eet.pdb.product_code,
        });
    }
    let (dvl_started, eet_started) = (volume_scan_started(&dvl.pdb), volume_scan_started(&eet.pdb));
    if !volumes_pair(dvl_started, eet_started) {
        return Err(Refusal::VolumeMismatch {
            dvl: dvl_started,
            eet: eet_started,
        });
    }
    let (Some(dvl_packet), Some(eet_packet)) = (
        crate::srm::radial_packet(dvl),
        crate::srm::radial_packet(eet),
    ) else {
        return Err(Refusal::NoRadialData);
    };
    let (Some(dvl_codec), Some(eet_codec)) =
        (ValueCodec::for_message(dvl), ValueCodec::for_message(eet))
    else {
        return Err(Refusal::NoRadialData);
    };

    let dvl_field = resampled_field(
        dvl_packet,
        compare::gate_km(&dvl.pdb, dvl_packet),
        &dvl_codec,
    );
    let eet_field = resampled_field(
        eet_packet,
        compare::gate_km(&eet.pdb, eet_packet),
        &eet_codec,
    );
    Ok(VolumetricGrid {
        values: density_field(&dvl_field, &published_top_field(&eet_field)),
        range_bins: RANGE_BINS,
    })
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
