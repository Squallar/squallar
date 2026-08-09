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
mod tests {
    use super::*;
    use nexrad_level3::model::{
        DataLayer, DataPacket, MessageHeader, ProductDescriptionBlock, RadialRun, SymbologyBlock,
    };

    /// NEXRAD float16 for 1.0 and 2.0 (sign 0, exponent 16/17, fraction 0) —
    /// the encoding [`crate::l3_values::nexrad_float16`] decodes.
    const F16_ONE: u16 = 16 << 10;
    const F16_TWO: u16 = 17 << 10;

    /// Product 134's thresholds for a **deliberately linear** synthetic LUT:
    /// `lin_scale` 1.0, `lin_offset` 2.0, `log_start` 255 — so every level
    /// 2..=254 decodes as `level − 2` kg/m² and nothing lands in the log
    /// region.
    ///
    /// The hybrid decode itself is pinned in [`crate::l3_values`]; what the
    /// tests here pin is the quotient, its datum, the presence rules and the
    /// resampling, and an exactly hand-computable numerator is what those want.
    /// `[5]` upward are unused by [`crate::l3_values::build_vil_lut`].
    fn dvl_thresholds() -> [u16; 16] {
        let mut t = [0u16; 16];
        t[0] = F16_ONE; // lin_scale
        t[1] = F16_TWO; // lin_offset
        t[2] = 255; // log_start — past the table, so the whole LUT is linear
        t[3] = F16_ONE; // log_scale  (unreached)
        t[4] = F16_ONE; // log_offset (unreached)
        t
    }

    /// Product 135's thresholds as a live `TLX_EET` carries them:
    /// DATA_MASK 127, SCALE 1, OFFSET 2, TOPPED_MASK 128 — so
    /// `kft = (level & 127) − 2`, level 2 is 0 kft and bit 7 only flags a top
    /// above the volume's highest cut.
    fn eet_thresholds() -> [u16; 16] {
        let mut t = [0u16; 16];
        t[0] = 127;
        t[1] = 1;
        t[2] = 2;
        t[3] = 128;
        t
    }

    /// The level product 134 encodes `kg_m2` as, under [`dvl_thresholds`].
    fn dvl_level(kg_m2: u16) -> u16 {
        kg_m2 + 2
    }

    /// The level product 135 encodes a published `kft` as, under
    /// [`eet_thresholds`]: `⌊kft⌋ + 2`, with bit 7 set when `topped`.
    fn eet_level(kft: u16, topped: bool) -> u16 {
        kft + 2 + if topped { 128 } else { 0 }
    }

    /// One Level III message: `product_code`, its thresholds, a volume start
    /// (`volume_scan_time` seconds into MJD day `volume_scan_date`), the
    /// packet's gate spacing through `scale_factor`, and per-azimuth gate
    /// levels from `gates_at` (360 radials, one degree each).
    fn message(
        product_code: i16,
        thresholds: [u16; 16],
        volume_scan_date: u16,
        volume_scan_time: u32,
        scale_factor: f32,
        gates_at: impl Fn(usize) -> Vec<u16>,
    ) -> Level3Message {
        let pdb = ProductDescriptionBlock {
            block_divider: -1,
            latitude: 35.3333,
            longitude: -97.2778,
            height: 1200,
            product_code,
            operational_mode: 2,
            vcp: 212,
            sequence_number: 0,
            volume_scan_number: 39,
            volume_scan_date,
            volume_scan_time,
            generation_date: volume_scan_date,
            generation_time: volume_scan_time,
            product_specific_1: 0,
            product_specific_2: 0,
            elevation_number: 0,
            product_specific_3: 0,
            thresholds,
            product_specific_47_53: [0; 7],
            version: 0,
            spot_blank: 0,
            symbology_offset: 60,
            graphic_offset: 0,
            tabular_offset: 0,
        };
        let radials: Vec<RadialRun> = (0..360)
            .map(|az| RadialRun {
                start_angle: az as f32,
                angle_delta: 1.0,
                gate_values: gates_at(az),
            })
            .collect();
        let num_range_bins = radials
            .iter()
            .map(|r| r.gate_values.len())
            .max()
            .unwrap_or(0) as u16;
        Level3Message {
            header: MessageHeader {
                message_code: product_code,
                date_of_message: volume_scan_date,
                time_of_message: volume_scan_time,
                message_length: 0,
                source_id: 0,
                destination_id: 0,
                number_of_blocks: 3,
            },
            pdb,
            symbology: Some(SymbologyBlock {
                block_id: 1,
                block_length: 0,
                num_layers: 1,
                layers: vec![DataLayer {
                    layer_length: 0,
                    packets: vec![DataPacket::DigitalRadial(RadialPacket {
                        first_range_bin: 0,
                        num_range_bins,
                        i_center: 0,
                        j_center: 0,
                        scale_factor,
                        is_legacy: false,
                        xdr_data_scale: None,
                        xdr_data_offset: None,
                        radials,
                    })],
                }],
            }),
        }
    }

    /// A volume start both fixtures share, MJD day 20661 at 01:58:28Z.
    const VOL_DATE: u16 = 20661;
    const VOL_TIME: u32 = 7108;

    /// Digital VIL, 1 km gates, `kg_m2_at` kg/m² per azimuth (`None` =
    /// below-threshold level 0), over `bins` gates.
    fn dvl(
        volume_scan_time: u32,
        bins: usize,
        kg_m2_at: impl Fn(usize) -> Option<u16>,
    ) -> Level3Message {
        message(
            DVL_PRODUCT_CODE,
            dvl_thresholds(),
            VOL_DATE,
            volume_scan_time,
            1.0,
            move |az| vec![kg_m2_at(az).map_or(0, dvl_level); bins],
        )
    }

    /// Enhanced Echo Tops, 1 km gates, published `kft_at` kft per azimuth
    /// (`None` = below-threshold level 0), over `bins` gates.
    fn eet(
        volume_scan_time: u32,
        bins: usize,
        kft_at: impl Fn(usize) -> Option<u16>,
    ) -> Level3Message {
        message(
            EET_PRODUCT_CODE,
            eet_thresholds(),
            VOL_DATE,
            volume_scan_time,
            1.0,
            move |az| vec![kft_at(az).map_or(0, |kft| eet_level(kft, false)); bins],
        )
    }

    /// The pair the value tests read: DVL 35 kg/m² and a published 32 kft top
    /// at azimuth 10, 34 kg/m² over 32 kft at 11, a defined 0.0 kg/m² column
    /// at 12, DVL with no echo top at 13, an echo top with no DVL at 14, a
    /// published **zero** top at 15, and nothing at all at 16.
    fn golden_pair() -> (Level3Message, Level3Message) {
        let dvl_at = |az: usize| match az {
            10 => Some(35),
            11 => Some(34),
            12 => Some(0),
            13 => Some(20),
            15 => Some(35),
            _ => None,
        };
        let eet_at = |az: usize| match az {
            10 | 11 => Some(32),
            12 => Some(32),
            14 => Some(40),
            15 => Some(0),
            _ => None,
        };
        (dvl(VOL_TIME, 60, dvl_at), eet(VOL_TIME, 60, eet_at))
    }

    /// The two published products divided on the bin centre, hand-computed:
    ///
    /// * az 10 — 35 kg/m² over a published 32 kft top is 32.5 kft = 9906.0 m,
    ///   so `35000/9906` = **3.533212 g/m³**, above Amburn & Wolf's 3.5 break;
    /// * az 11 — 34 kg/m² over the same top is `34000/9906` = **3.432263**,
    ///   below it;
    /// * az 12 — a defined 0.0 kg/m² column over a real top is a defined
    ///   **0.0 g/m³**, not undefined.
    #[test]
    fn the_two_published_products_divide_to_hand_computed_vil_density() {
        let (num, den) = golden_pair();
        let grid = compute_vild(&num, &den).expect("a paired 134/135 pair renders");
        assert_eq!(grid.range_bins, RANGE_BINS);
        assert_eq!(grid.values.len(), 360);
        assert_eq!(grid.values[0].len(), RANGE_BINS);

        let r = 30;
        assert!(
            (grid.values[10][r] - 3.533_212).abs() < 1e-5,
            "got {}",
            grid.values[10][r],
        );
        assert!(grid.values[10][r] >= 3.5, "the 3.5 break must be crossed");
        assert!(
            (grid.values[11][r] - 3.432_263).abs() < 1e-5,
            "got {}",
            grid.values[11][r],
        );
        assert!(grid.values[11][r] < 3.5);
        assert_eq!(grid.values[12][r], 0.0, "a defined zero, not undefined");
    }

    /// Amburn & Wolf's own formula against hand-computed pairs: 20 kg/m² over
    /// a 10 km top (32.8084 kft = 10,000 m) is exactly 2.0 g/m³, and 35 kg/m²
    /// over the same top is their 3.5 g/m³ severe-hail break. One kilofoot of
    /// top is `1000·VIL/304.8`.
    #[test]
    fn the_arithmetic_reproduces_the_amburn_wolf_pairs() {
        assert!((vild_g_m3(20.0, 32.8084) - 2.0).abs() < 1e-5);
        assert!((vild_g_m3(35.0, 32.8084) - 3.5).abs() < 1e-5);
        assert!((vild_g_m3(1.0, 1.0) - 3.280_84).abs() < 1e-4);
        // And through the published datum, which puts the same 2.0 g/m³ half a
        // kilofoot lower down.
        assert!((vild_from_published(20.0, 32.3084) - 2.0).abs() < 1e-5);
        // A defined 0.0 kg/m² column is a defined 0.0 g/m³, not undefined.
        assert_eq!(vild_from_published(0.0, 32.0), 0.0);
    }

    /// The `+ 0.5` bin-centre datum is load-bearing, and a wrong datum fails
    /// here: 34 kg/m² over a published 32 kft top reads **3.432263** g/m³ on
    /// the bin centre but `34000/9753.6` = **3.485924** on the published floor
    /// — and 35 kg/m² reads 3.533212 against 3.588419, high by exactly
    /// 32.5/32 − 1 = 1.5625%.
    ///
    /// The straddle case is the one that decides a warning: 34.4 kg/m² over
    /// the same top is 3.472643 on the centre and 3.526903 on the floor —
    /// opposite sides of the 3.5 g/m³ severe-hail break, from the same two
    /// published numbers.
    #[test]
    fn the_bin_centre_datum_is_what_the_quotient_divides_by() {
        let centre = vild_from_published(35.0, 32.0);
        assert!((centre - 3.533_212).abs() < 1e-5, "got {centre}");
        let floor = vild_g_m3(35.0, 32.0);
        assert!((floor - 3.588_419).abs() < 1e-5, "got {floor}");
        assert!(
            (f64::from(floor / centre) - 32.5 / 32.0).abs() < 1e-6,
            "the floor datum's bias is exactly half a bin",
        );

        let straddle_centre = vild_from_published(34.4, 32.0);
        let straddle_floor = vild_g_m3(34.4, 32.0);
        assert!((straddle_centre - 3.472_643).abs() < 1e-5);
        assert!((straddle_floor - 3.526_903).abs() < 1e-5);
        assert!(
            straddle_centre < 3.5 && straddle_floor >= 3.5,
            "the datum decides the break: {straddle_centre} vs {straddle_floor}",
        );

        // And the datum the *grid* applies is the centre, not the floor: the
        // whole field would be 1.5625% high at a 32 kft top otherwise.
        let (num, den) = golden_pair();
        let grid = compute_vild(&num, &den).expect("renders");
        assert!(
            (grid.values[10][30] - vild_from_published(35.0, 32.0)).abs() < 1e-6,
            "the grid divides by something other than the bin centre",
        );
        assert!(
            (grid.values[10][30] - vild_g_m3(35.0, 32.0)).abs() > 0.05,
            "a floor-datum grid would be indistinguishable — the pin is vacuous",
        );

        // The published tops themselves become centres, with zero and
        // undefined dropped.
        let tops = published_top_field(&[vec![0.0, 32.0, f32::NAN, -1.0, 69.0]]);
        assert!(tops[0][0].is_nan(), "a 0 kft top has no usable quotient");
        assert_eq!(tops[0][1], 32.5);
        assert!(tops[0][2].is_nan());
        assert!(tops[0][3].is_nan());
        assert_eq!(tops[0][4], 69.5);
    }

    /// A cell is defined only where **both** inputs are: a DVL column with no
    /// echo top, an echo top with no DVL, and a cell with neither are all
    /// `NaN` — never 0, which the palette would paint.
    #[test]
    fn a_cell_is_undefined_wherever_either_input_is() {
        let (num, den) = golden_pair();
        let grid = compute_vild(&num, &den).expect("renders");
        let r = 30;
        assert!(
            grid.values[13][r].is_nan(),
            "DVL defined but no echo top: got {}",
            grid.values[13][r],
        );
        assert!(
            grid.values[14][r].is_nan(),
            "an echo top with no DVL: got {}",
            grid.values[14][r],
        );
        assert!(grid.values[16][r].is_nan(), "neither input");

        // The scalar agrees, in every combination.
        assert!(vild_from_published(f32::NAN, 32.0).is_nan());
        assert!(vild_from_published(35.0, f32::NAN).is_nan());
        assert!(vild_from_published(f32::NAN, f32::NAN).is_nan());
        assert!(vild_from_published(f32::INFINITY, 32.0).is_nan());
        assert!(vild_g_m3(20.0, f32::NAN).is_nan());
    }

    /// A published echo top of **zero** kilofeet has no resolvable quotient —
    /// product 135's level 2, and the topped level 130 that decodes the same —
    /// so the cell is undefined rather than dividing by a zero denominator or
    /// by half a kilofoot of quantization noise.
    #[test]
    fn a_zero_published_echo_top_leaves_the_cell_undefined() {
        let (num, den) = golden_pair();
        let grid = compute_vild(&num, &den).expect("renders");
        assert!(
            grid.values[15][30].is_nan(),
            "35 kg/m² over a 0 kft top: got {}",
            grid.values[15][30],
        );

        // The topped flag decodes to the same 0 kft and is refused the same
        // way, while a topped 32 kft is an ordinary defined cell.
        let topped = message(
            EET_PRODUCT_CODE,
            eet_thresholds(),
            VOL_DATE,
            VOL_TIME,
            1.0,
            |az| {
                let level = match az {
                    10 => eet_level(0, true),
                    11 => eet_level(32, true),
                    _ => 0,
                };
                vec![level; 60]
            },
        );
        let grid = compute_vild(&num, &topped).expect("renders");
        assert!(grid.values[10][30].is_nan(), "a topped 0 kft top");
        assert!(
            (grid.values[11][30] - vild_from_published(34.0, 32.0)).abs() < 1e-6,
            "a topped 32 kft top is an ordinary cell: got {}",
            grid.values[11][30],
        );

        assert!(vild_from_published(35.0, 0.0).is_nan());
        assert!(vild_from_published(35.0, -1.0).is_nan());
        assert!(vild_g_m3(20.0, 0.0).is_nan(), "a zero top divides");
    }

    /// Volume pairing is mandatory: a DVL from one volume beside an EET from
    /// the next is **refused**, not painted. A ratio of two volumes is a
    /// plausible field of a storm that never existed.
    #[test]
    fn a_volume_mismatch_refuses_to_render() {
        let (num, den) = golden_pair();
        assert!(compute_vild(&num, &den).is_ok(), "the paired case renders");

        // The next volume, four minutes later.
        let later = eet(VOL_TIME + 240, 60, |az| (az == 10).then_some(32));
        match compute_vild(&num, &later) {
            Err(Refusal::VolumeMismatch { dvl: a, eet: b }) => {
                assert_ne!(a, b, "the two starts must differ");
            }
            Err(other) => panic!("wrong refusal: {other:?}"),
            Ok(_) => panic!("a mismatched pair must be refused, not painted"),
        }

        // Inside the tolerance is the same volume — the RPG writes the two
        // objects seconds apart.
        let jittered = eet(VOL_TIME + 30, 60, |az| (az == 10).then_some(32));
        assert!(
            compute_vild(&num, &jittered).is_ok(),
            "{VOLUME_PAIRING_TOLERANCE_SECS} s of jitter is one volume",
        );
        let past = eet(VOL_TIME + 61, 60, |az| (az == 10).then_some(32));
        assert!(compute_vild(&num, &past).is_err(), "one second past it");

        // An unreadable volume start is not a pair either: it cannot be shown
        // to belong with the other object.
        let unreadable = message(EET_PRODUCT_CODE, eet_thresholds(), 0, VOL_TIME, 1.0, |_| {
            vec![eet_level(32, false); 60]
        });
        assert_eq!(volume_scan_started(&unreadable.pdb), None);
        assert!(matches!(
            compute_vild(&num, &unreadable),
            Err(Refusal::VolumeMismatch { eet: None, .. }),
        ));

        assert!(volumes_pair(
            volume_scan_started(&num.pdb),
            volume_scan_started(&den.pdb),
        ));
        assert!(!volumes_pair(None, None), "two unknowns are not a pair");
        assert!(!volumes_pair(volume_scan_started(&num.pdb), None));
    }

    /// The two products are not interchangeable: swapping them would divide
    /// kilofeet by kilograms and the palette would paint the result.
    #[test]
    fn only_a_134_over_135_pair_renders() {
        let (num, den) = golden_pair();
        assert_eq!(
            compute_vild(&den, &num).err(),
            Some(Refusal::WrongProduct {
                dvl: EET_PRODUCT_CODE,
                eet: DVL_PRODUCT_CODE,
            }),
            "the two products are not interchangeable",
        );
        assert!(matches!(
            compute_vild(&num, &num).err(),
            Some(Refusal::WrongProduct { .. }),
        ));

        // A message with no symbology carries no radial packet.
        let mut empty = den.clone();
        empty.symbology = None;
        assert_eq!(
            compute_vild(&num, &empty).err(),
            Some(Refusal::NoRadialData),
        );
    }

    /// An EET on 0.25 km gates (`scale_factor` 4.0), so the two products'
    /// spacings differ: the shared resampler represents each 1 km cell by the
    /// packet gate whose centre sits nearest the cell centre, first gate
    /// winning ties. Cell 0's centre is 0.5 km and gate 1's is 0.375 —
    /// 0.125 away, tying with gate 2's 0.625 and beating gate 0's 0.125 km —
    /// so cell `r` reads gate `4r + 1`.
    fn a_finer_gated_denominator_resamples_onto_the_1_km_cells() -> Level3Message {
        message(
            EET_PRODUCT_CODE,
            eet_thresholds(),
            VOL_DATE,
            VOL_TIME,
            4.0,
            |az| {
                let mut gates = vec![0u16; 12];
                if az == 10 {
                    gates[1] = eet_level(32, false); // cell 0
                    gates[5] = eet_level(16, false); // cell 1
                }
                gates
            },
        )
    }

    /// The grid is defined only where both packets reach, and never past the
    /// 230 km display cap — and the two products' gate spacings and range
    /// extents do not have to match.
    #[test]
    fn both_products_resample_onto_the_common_capped_grid() {
        // DVL out to 60 km, EET out to 40 km at 1 km gates: cells beyond each
        // packet's own extent are undefined.
        let short_eet = eet(VOL_TIME, 40, |az| (az == 10).then_some(32));
        let grid = compute_vild(
            &dvl(VOL_TIME, 60, |az| (az == 10).then_some(35)),
            &short_eet,
        )
        .expect("renders");
        assert!(grid.values[10][39].is_finite(), "inside both extents");
        assert!(
            grid.values[10][40].is_nan(),
            "past the EET's extent: got {}",
            grid.values[10][40],
        );
        assert!(grid.values[10][RANGE_BINS - 1].is_nan(), "past both");

        // Differing spacings: a 0.25 km-gated EET against a 1 km DVL.
        let grid = compute_vild(
            &dvl(VOL_TIME, 60, |az| (az == 10).then_some(35)),
            &a_finer_gated_denominator_resamples_onto_the_1_km_cells(),
        )
        .expect("renders");
        assert!(
            (grid.values[10][0] - vild_from_published(35.0, 32.0)).abs() < 1e-6,
            "cell 0 must read gate 1's 32 kft: got {}",
            grid.values[10][0],
        );
        assert!(
            (grid.values[10][1] - vild_from_published(35.0, 16.0)).abs() < 1e-6,
            "cell 1 must read gate 5's 16 kft: got {}",
            grid.values[10][1],
        );
        assert!(
            grid.values[10][2].is_nan(),
            "cell 2 reads gate 9, which is below threshold: got {}",
            grid.values[10][2],
        );
        assert!(grid.values[10][3].is_nan(), "past the finer packet's 3 km");
    }

    /// The product's own precision, hand-computed at VILD 3.5 g/m³ — the
    /// table in the module doc, and the reason no tighter agreement than
    /// ~±0.1 g/m³ is claimed anywhere.
    #[test]
    fn the_quantization_halfwidth_is_half_a_kilofoot_of_echo_top() {
        for (published, halfwidth) in [
            (15.0f32, 0.112_903f32),
            (20.0, 0.085_366),
            (30.0, 0.057_377),
            (40.0, 0.043_210),
            (50.0, 0.034_653),
        ] {
            let got = quantization_halfwidth_g_m3(3.5, published);
            assert!(
                (got - halfwidth).abs() < 1e-5,
                "at {published} kft: got {got}, hand-computed {halfwidth}",
            );
            // Relative, so it scales with the value itself.
            assert!((quantization_halfwidth_g_m3(7.0, published) - 2.0 * halfwidth).abs() < 1e-5);
        }
        assert_eq!(EET_QUANTUM_KFT, 1.0);
        assert_eq!(EET_BIN_CENTRE_KFT, 0.5);
    }

    /// **The anti-drift pin.** The shipped entry point must compose the
    /// reference the [`crate::vil`] survey scores against, step for step:
    /// resample each packet through its own codec at its own gate spacing,
    /// turn the published tops into bin centres, divide.
    ///
    /// Asserted through the constructors the survey's policy re-exports (the
    /// survey itself lives on branch `campaign-harness`) rather than by
    /// recomputing the arithmetic here, so the shipped product and the
    /// harness's reference cannot come apart: if [`compute_vild`] ever
    /// reorders, re-datums or re-resamples, this fails and the survey's
    /// verdict stops applying to what the app draws.
    #[test]
    fn the_shipped_path_is_the_surveys_reference_construction() {
        let policy = |dvl: &Level3Message, eet: &Level3Message| {
            let dvl_packet = crate::srm::radial_packet(dvl).expect("packet");
            let eet_packet = crate::srm::radial_packet(eet).expect("packet");
            let dvl_codec = ValueCodec::for_message(dvl).expect("codec");
            let eet_codec = ValueCodec::for_message(eet).expect("codec");
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
            density_field(&dvl_field, &published_top_field(&eet_field))
        };

        // Fields with something in every category: values either side of both
        // breaks, defined zeros, one-sided cells, a zero top, topped tops, and
        // a denominator on a different gate spacing from the numerator.
        let (golden_dvl, golden_eet) = golden_pair();
        let mut total_finite = 0usize;
        for (label, dvl_msg, eet_msg) in [
            ("golden", golden_dvl, golden_eet),
            (
                "whole domain",
                dvl(VOL_TIME, 230, |az| Some((az % 60) as u16)),
                eet(VOL_TIME, 230, |az| Some((az % 50) as u16)),
            ),
            (
                "topped tops",
                dvl(VOL_TIME, 100, |az| (az % 3 == 0).then_some(45)),
                message(
                    EET_PRODUCT_CODE,
                    eet_thresholds(),
                    VOL_DATE,
                    VOL_TIME,
                    1.0,
                    |az| vec![eet_level((az % 40) as u16, az % 7 == 0); 100],
                ),
            ),
            (
                "finer denominator",
                dvl(VOL_TIME, 60, |az| Some((az % 45) as u16)),
                a_finer_gated_denominator_resamples_onto_the_1_km_cells(),
            ),
        ] {
            let shipped = compute_vild(&dvl_msg, &eet_msg)
                .unwrap_or_else(|e| panic!("{label}: shipped path refused: {e:?}"));
            let reference = policy(&dvl_msg, &eet_msg);
            assert_eq!(shipped.values.len(), reference.len(), "{label}");
            let mut finite = 0usize;
            for (az, (ours, theirs)) in shipped.values.iter().zip(&reference).enumerate() {
                assert_eq!(ours.len(), theirs.len(), "{label} az {az}");
                for (r, (&a, &b)) in ours.iter().zip(theirs).enumerate() {
                    assert!(
                        (a.is_nan() && b.is_nan()) || a.to_bits() == b.to_bits(),
                        "{label} az {az} r {r}: shipped {a}, reference {b}",
                    );
                    finite += usize::from(a.is_finite());
                }
            }
            assert!(
                finite > 0,
                "{label}: no defined cells at all — the row compares nothing",
            );
            total_finite += finite;
        }
        assert!(
            total_finite > 50_000,
            "only {total_finite} defined cells pooled — the pin is too thin to catch \
             a re-datumed or re-resampled shipped path",
        );
    }
}
