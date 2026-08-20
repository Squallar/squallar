//! VIL Density (`vild`, g/m³): the RPG's **own two published products**
//! divided — Digital VIL (product 134, AWIPS `DVL`, kg/m²) over Enhanced Echo
//! Tops (product 135, AWIPS `EET`, kft above MSL) — on the shared 1° × 1 km
//! polar grid.

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
/// volume, seconds.
pub const VOLUME_PAIRING_TOLERANCE_SECS: i64 = 60;

/// VIL density for one cell, g/m³: `1000 · VIL / ET` with VIL in kg/m² and
/// the echo top in **metres**, per Amburn & Wolf (1997).
pub fn vild_g_m3(vil_kg_m2: f32, top_kft_msl: f32) -> f32 {
    if !vil_kg_m2.is_finite() || !top_kft_msl.is_finite() || top_kft_msl <= 0.0 {
        return f32::NAN;
    }
    1000.0 * vil_kg_m2 / (top_kft_msl * KFT_TO_M)
}

/// VIL density for one cell from the RPG's own two published values:
/// [`vild_g_m3`] on the echo-top bin **centre**.
pub fn vild_from_published(dvl_kg_m2: f32, eet_published_kft: f32) -> f32 {
    if !eet_published_kft.is_finite() || eet_published_kft <= 0.0 {
        return f32::NAN;
    }
    vild_g_m3(dvl_kg_m2, eet_published_kft + EET_BIN_CENTRE_KFT)
}

/// This product's own precision at a cell, g/m³: the true top lies anywhere in
/// `[published, published + 1)` kft, so the bin-centre denominator carries a
/// ±0.5 kft top error — a relative VILD uncertainty of
/// `0.5/(published + 0.5)`.
pub fn quantization_halfwidth_g_m3(vild_g_m3: f32, eet_published_kft: f32) -> f32 {
    vild_g_m3 * EET_BIN_CENTRE_KFT / (eet_published_kft + EET_BIN_CENTRE_KFT)
}

/// A Level III radial packet resampled onto the 360° × 230 km polar grid and
/// decoded through the product's own codec.
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
