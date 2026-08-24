//! Hydrometeor Classification (the RPG's per-tilt product 165, AWIPS `N0H`)
//! computed locally from the Level II dual-pol moments of one tilt.
//!
//! Transcribed from the ORPG CODE Build 21.0r1.7 public source: task
//! `cpc023/tsk001` (`hca`), its feeders `cpc023/tsk002` (`qia`) and
//! `cpc004/tsk011` (`dpprep`, shared through [`crate::dpprep`]), and the
//! Melting Layer Detection Algorithm `cpc023/tsk003` (`mlda`); fleet-default
//! adaptation values from `cpc104/lib006/{hca,qia,mlda,dpprep,hail}.alg`.
//! Lineage: Park, Ryzhkov, Zrnić, Kim 2009, "The Hydrometeor Classification
//! Algorithm for the Polarimetric WSR-88D" (Weather and Forecasting 24,
//! 730–748) for HCA and Giangrande, Krause, Ryzhkov 2008 (JAMC 47, 1354–1364)
//! for the MLDA. Where the released source and the paper differ, the source
//! wins.
//!
//! Chain (`cpc104/lib003/task_attr_table`): super-res base data → `recomb` →
//! `dpprep` → `qia` → `hca` → `dualpol8bit` (product 165). Each dpprep field
//! crosses a task boundary as a quantized moment, so the 8-bit fields are
//! rounded to their transport resolution and a gate whose raw input was
//! missing stays missing downstream.
//!
//! Output uses the product's external codes (`dualpol8bit.c`'s
//! `Class_external`, class × 10): RA 60, HR 70, RH 100 (LH 110 / GH 120 for
//! the large/giant-hail subclasses), BD 80, BI 10, GC 20, DS 40, WS 50, IC 30,
//! GR 90, UK 140; NE encodes level 0 and decodes as undefined.
//!
//! [`resolve_melting_layer`] is the melting-layer chain: the RPG's own product
//! 166 for this volume, else this volume's MLDA ([`detect_melting_layer`]),
//! else a sounding freezing level, else the `hail.alg` 10.5 kft flat default.
//! The last rung is a guess and scores 16–20% exact class agreement against
//! `N0H` in winter/stratiform regimes where 84–99% is achievable from rung 1,
//! which is why [`MeltingLayer`] carries a [`MeltingLayerSource`].
//!
//! Documented gaps against the RPG: beam blockage (`read_Blockage`, the
//! FShield Z adjustment and the QIA blockage term) needs a per-site blockage
//! store the archive stream does not carry, so this runs unblocked and
//! terrain-blocked sectors at mountain sites diverge; on split-cut
//! surveillance tilts the archive carries no velocity, so the GC velocity kill
//! is inert there; the RPG computes in `float` and this in `f64`; the
//! RF → UK branch is unreachable from Archive II moments.

use crate::dpprep::{
    CORR_THRESH, DBZ_THRESH, DBZ_WINDOW, DpCombined, DpInput, LONG_GATE, MET_SIG_THRESHOLD,
    SHORT_GATE, UNFOLD_MIN_RHO, WINDOW, average_filter, clean_met_signal, combine_sweep_dp,
    find_met_signal, index_into, interpolate, is_high_attenuation_radial, isdp_from_queue,
    kdp_from_phi, median_filter, meteo_groups, radial_system_phi, resample_to_polar_grid,
    std_filter, unfold_phidp,
};
use crate::kdp::KdpParams;
use crate::par::*;
use nexrad_model::data::Radial;

pub use crate::dpprep::ReflCappi;

// ── Class indices (hca.h) and the product's external codes ──────────────────

pub(crate) const NUM_CLASSES: usize = 14;
const U0: usize = 0;
const U1: usize = 1;
pub(crate) const RA: usize = 2;
pub(crate) const HR: usize = 3;
pub(crate) const RH: usize = 4;
pub(crate) const BD: usize = 5;
pub(crate) const BI: usize = 6;
pub(crate) const GC: usize = 7;
pub(crate) const DS: usize = 8;
pub(crate) const WS: usize = 9;
pub(crate) const IC: usize = 10;
pub(crate) const GR: usize = 11;
pub(crate) const UK: usize = 12;
pub(crate) const NE: usize = 13;

/// `dualpol8bit.c`'s `Class_external`: internal class index → the product's data level
/// (class codes scaled by 10).
pub const CLASS_EXTERNAL: [f32; NUM_CLASSES] = [
    0.0, 0.0, 60.0, 70.0, 100.0, 80.0, 10.0, 20.0, 40.0, 50.0, 30.0, 90.0, 140.0, 0.0,
];

/// The C sentinel for a missing value (`HCA_NO_DATA`).
pub(crate) const NO_DATA: f64 = -1.0e5;

/// `MINI_LKTP`: LKdp for KDP below 0.001 °/km.
const MINI_LKTP: f64 = -40.0;

// ── hca.alg fleet defaults ───────────────────────────────────────────────────
//
// The five ZDR class kills below are `pub(crate)`: `voxel::volume_alpha_profile`
// takes the 3D transparency profile's quiet band for ZDR from them by
// reference, because the band where ZDR discriminates nothing is exactly the
// interval this algorithm leaves open for rain. `hca` is a `pub mod`, so
// `pub` would have published them as crate API for a consumer that lives one
// module over; `pub(crate)` is the reach they actually need. Everything else
// here stays private to the classifier.

const MIN_V_GC: f64 = 1.0;
const MAX_Z_RA: f64 = 50.0;
const MIN_RHO_RA: f64 = 0.94;
const MIN_PHIDP_RA: f64 = 100.0;
const MIN_Z_RH: f64 = 30.0;
const MIN_Z_HR: f64 = 30.0;
pub(crate) const MIN_ZDR_HR: f64 = 1.0;
const MAX_Z_IC: f64 = 40.0;
const MIN_Z_GR: f64 = 10.0;
const MAX_Z_GR: f64 = 60.0;
pub(crate) const MAX_ZDR_GR: f64 = 2.0;
const MIN_Z_BD: f64 = 15.0;
pub(crate) const MIN_ZDR_BD: f64 = 0.5;
// CCR NA15-00181: the Z leg of the WS kill is commented out of
// `hca_allowedHydroClass.c`; only ZDR remains.
pub(crate) const MIN_ZDR_WS: f64 = 0.0;
const MAX_RHOHV_BI: f64 = 0.97;
const MAX_Z_BI: f64 = 35.0;
pub(crate) const MAX_ZDR_DS: f64 = 2.0;
const MIN_AGG: f64 = 0.4;
const MIN_DIF_AGG: f64 = 0.001;
const MIN_SNR: f64 = 5.0;
/// `atten_control = Off`: the BI kills apply on every radial.
const ATTEN_CONTROL: bool = false;

/// The two-dimensional membership equations (`hca.alg` f/g coefficients):
/// `f = a·Z² + b·Z + c`, `g = b·Z + c`.
const F1_COEF: (f64, f64, f64) = (0.000_750, 0.0025, -0.5);
const F2_COEF: (f64, f64, f64) = (0.002_92, -0.0481, 0.68);
const F3_COEF: (f64, f64, f64) = (0.000_485, 0.0667, 1.42);
const G1_COEF: (f64, f64) = (0.8, -44.0);
const G2_COEF: (f64, f64) = (0.5, -22.0);

// ── Fuzzy-logic input indices (hca_local.h) ──────────────────────────────────

const SMZ: usize = 0;
const ZDR: usize = 1;
const LKDP: usize = 2;
const RHO: usize = 3;
const SDZ: usize = 4;
const SDP: usize = 5;
const NUM_FL_INPUTS: usize = 6;

/// Which equation adjusts a membership point (`memFlag*` in `hca.alg`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MemFlag {
    None,
    F1,
    F2,
    F3,
    G1,
    G2,
}

use MemFlag::{F1, F2, F3, G1, G2, None as MF};

/// One class's six membership rows: `[input][x1..x4]` base points, plus the
/// 2-D flags added to them (`Hca_setMembershipPoints`).
pub(crate) struct MemTable {
    pub(crate) points: [[f64; 4]; NUM_FL_INPUTS],
    pub(crate) flags: [[MemFlag; 4]; NUM_FL_INPUTS],
}

/// `hca.alg`'s `memRA`/`memFlagRA`.
pub(crate) const MEM_RA: MemTable = MemTable {
    points: [
        [5.00, 10.00, 45.00, 50.00],
        [-0.30, 0.00, 0.00, 0.50],
        [-1.00, 0.00, 0.00, 1.00],
        [0.95, 0.97, 1.00, 1.01],
        [0.00, 0.50, 3.00, 6.00],
        [0.00, 1.00, 15.00, 30.00],
    ],
    flags: [
        [MF, MF, MF, MF],
        [F1, F1, F2, F2],
        [G1, G1, G2, G2],
        [MF, MF, MF, MF],
        [MF, MF, MF, MF],
        [MF, MF, MF, MF],
    ],
};

/// `hca.alg`'s `memHR`/`memFlagHR`.
pub(crate) const MEM_HR: MemTable = MemTable {
    points: [
        [40.00, 45.00, 55.00, 60.00],
        [-0.30, 0.00, 0.00, 0.50],
        [-1.00, 0.00, 0.00, 1.00],
        [0.92, 0.95, 1.00, 1.01],
        [0.00, 0.50, 3.00, 6.00],
        [0.00, 1.00, 15.00, 30.00],
    ],
    flags: [
        [MF, MF, MF, MF],
        [F1, F1, F2, F2],
        [G1, G1, G2, G2],
        [MF, MF, MF, MF],
        [MF, MF, MF, MF],
        [MF, MF, MF, MF],
    ],
};

/// `hca.alg`'s `memRH`/`memFlagRH` (rain and hail).
pub(crate) const MEM_RH: MemTable = MemTable {
    points: [
        [45.00, 50.00, 75.00, 80.00],
        [-0.30, 0.00, 0.00, 0.50],
        [-10.00, -4.00, 0.00, 1.00],
        [0.85, 0.90, 1.00, 1.01],
        [0.00, 0.50, 3.00, 6.00],
        [0.00, 1.00, 15.00, 30.00],
    ],
    flags: [
        [MF, MF, MF, MF],
        [MF, MF, F1, F1],
        [MF, MF, G1, G1],
        [MF, MF, MF, MF],
        [MF, MF, MF, MF],
        [MF, MF, MF, MF],
    ],
};

/// `hca.alg`'s `memBD`/`memFlagBD` (big drops).
pub(crate) const MEM_BD: MemTable = MemTable {
    points: [
        [10.00, 15.00, 45.00, 50.00],
        [-0.30, 0.00, 0.00, 1.00],
        [-1.00, 0.00, 0.00, 1.00],
        [0.92, 0.95, 1.00, 1.01],
        [0.00, 0.50, 3.00, 6.00],
        [0.00, 1.00, 15.00, 30.00],
    ],
    flags: [
        [MF, MF, MF, MF],
        [F2, F2, F3, F3],
        [G1, G1, G2, G2],
        [MF, MF, MF, MF],
        [MF, MF, MF, MF],
        [MF, MF, MF, MF],
    ],
};

/// `hca.alg`'s `memBI`/`memFlagBI` (biological).
pub(crate) const MEM_BI: MemTable = MemTable {
    points: [
        [5.00, 10.00, 20.00, 30.00],
        [0.00, 0.00, 10.00, 12.00],
        [-30.00, -25.00, 10.00, 20.00],
        [0.30, 0.50, 0.85, 0.90],
        [1.00, 2.00, 4.00, 7.00],
        [8.00, 10.00, 40.00, 60.00],
    ],
    flags: [
        [MF, MF, MF, MF],
        [MF, F3, MF, MF],
        [MF, MF, MF, MF],
        [MF, MF, MF, MF],
        [MF, MF, MF, MF],
        [MF, MF, MF, MF],
    ],
};

/// `hca.alg`'s `memGC`/`memFlagGC` (ground clutter).
pub(crate) const MEM_GC: MemTable = MemTable {
    points: [
        [15.00, 20.00, 70.00, 80.00],
        [-4.00, -2.00, 1.00, 2.00],
        [-30.00, -25.00, 10.00, 20.00],
        [0.50, 0.60, 0.90, 0.95],
        [2.00, 4.00, 10.00, 15.00],
        [30.00, 40.00, 50.00, 60.00],
    ],
    flags: [[MF; 4]; 6],
};

/// `hca.alg`'s `memDS`/`memFlagDS` (dry snow).
pub(crate) const MEM_DS: MemTable = MemTable {
    points: [
        [5.00, 10.00, 35.00, 40.00],
        [-0.30, 0.00, 0.90, 1.10],
        [-30.00, -25.00, 10.00, 20.00],
        [0.98, 0.99, 1.00, 1.01],
        [0.00, 0.50, 3.00, 6.00],
        [0.00, 1.00, 15.00, 30.00],
    ],
    flags: [[MF; 4]; 6],
};

/// `hca.alg`'s `memWS`/`memFlagWS` (wet snow).
pub(crate) const MEM_WS: MemTable = MemTable {
    points: [
        [15.00, 25.00, 40.00, 50.00],
        [0.50, 1.00, 0.00, 0.30],
        [-30.00, -25.00, 10.00, 20.00],
        [0.84, 0.88, 0.97, 0.985],
        [0.00, 0.50, 3.00, 6.00],
        [0.00, 1.00, 15.00, 30.00],
    ],
    flags: [
        [MF, MF, MF, MF],
        [MF, MF, F2, F2],
        [MF, MF, MF, MF],
        [MF, MF, MF, MF],
        [MF, MF, MF, MF],
        [MF, MF, MF, MF],
    ],
};

/// `hca.alg`'s `memIC`/`memFlagIC` (ice crystals).
pub(crate) const MEM_IC: MemTable = MemTable {
    points: [
        [0.00, 5.00, 20.00, 25.00],
        [0.10, 0.40, 3.00, 3.30],
        [-5.00, 0.00, 10.00, 15.00],
        [0.95, 0.98, 1.00, 1.01],
        [0.00, 0.50, 3.00, 6.00],
        [0.00, 1.00, 15.00, 30.00],
    ],
    flags: [[MF; 4]; 6],
};

/// `hca.alg`'s `memGR`/`memFlagGR` (graupel).
pub(crate) const MEM_GR: MemTable = MemTable {
    points: [
        [25.00, 35.00, 50.00, 55.00],
        [-0.30, 0.00, 0.00, 0.30],
        [-30.00, -25.00, 10.00, 20.00],
        [0.90, 0.97, 1.00, 1.01],
        [0.00, 0.50, 3.00, 6.00],
        [0.00, 1.00, 15.00, 30.00],
    ],
    flags: [
        [MF, MF, MF, MF],
        [MF, MF, F1, F1],
        [MF, MF, MF, MF],
        [MF, MF, MF, MF],
        [MF, MF, MF, MF],
        [MF, MF, MF, MF],
    ],
};

/// The fuzzy-logic classes' membership tables, indexed `class − RA`.
pub(crate) const MEM: [&MemTable; 10] = [
    &MEM_RA, &MEM_HR, &MEM_RH, &MEM_BD, &MEM_BI, &MEM_GC, &MEM_DS, &MEM_WS, &MEM_IC, &MEM_GR,
];

/// `hca.alg`'s weight arrays, transposed to `[class − RA][input]`.
pub(crate) const WEIGHT: [[f64; NUM_FL_INPUTS]; 10] = [
    // SMZ  ZDR  LKDP RHO  SDZ  SDP
    [1.0, 0.8, 0.0, 0.6, 0.2, 0.2], // RA
    [1.0, 0.8, 1.0, 0.6, 0.2, 0.2], // HR
    [1.0, 0.8, 1.0, 0.6, 0.2, 0.2], // RH
    [0.8, 1.0, 0.0, 0.6, 0.2, 0.2], // BD
    [0.4, 0.6, 0.0, 1.0, 0.8, 0.8], // BI
    [0.2, 0.4, 0.0, 1.0, 0.6, 0.8], // GC
    [1.0, 0.8, 0.0, 1.0, 0.2, 0.2], // DS
    [0.6, 0.8, 0.0, 1.0, 0.2, 0.2], // WS
    [1.0, 0.6, 0.5, 0.4, 0.2, 0.2], // IC
    [0.8, 1.0, 0.0, 0.4, 0.2, 0.2], // GR
];

// ── qia.alg / qia_process.c constants ────────────────────────────────────────

const QIA_C: f64 = -0.69;
const PHI_DP_Z_THRESH: f64 = 600.0;
const PHI_DP_ZDR_THRESH: f64 = 300.0;
const PHI_DP_PHI_THRESH: f64 = 100.0;
const PHI_DP_KDP_THRESH: f64 = 100.0;
/// `pow(10, 0.1·5.0)` as the source spells it.
const LINEAR_SNR_ZDR_THRESH: f64 = 3.16228;
const DELTA_RHO_1_THRESHOLD: f64 = 0.5;
const RHO_MIN_THRESH: f64 = 0.8;
/// `qia.alg`'s `z_atten_thresh`.
const Z_ATTEN_THRESH: f64 = 25.0;
/// The quality indices' 8-bit transport (`Q_scale`/`Q_offset`).
const Q_SCALE: f64 = 100.0;
const Q_OFFSET: f64 = 2.0;

// ── mlda.alg fleet defaults / melting_layer.c constants ─────────────────────

const ML_DEPTH_KM: f64 = 0.5;
const ML_MAX_TOP_KM: f64 = 8.0;
const ML_HEIGHT_INTERVAL_KM: f64 = 0.1;
const ML_MAX_HEIGHTS: usize = 80;
const ML_UPPER_RHO: f64 = 0.97;
const ML_LOWER_RHO: f64 = 0.90;
const ML_LOW_RHO_PROFILE: f64 = 0.85;
const ML_UPPER_ZMAX: f64 = 47.0;
const ML_LOWER_ZMAX: f64 = 30.0;
const ML_UPPER_Z: f64 = 47.0;
const ML_LOWER_Z: f64 = 15.0;
const ML_UPPER_ZDRMAX: f64 = 2.2;
const ML_LOWER_ZDRMAX: f64 = 0.8;
const ML_HALF_WINDOW: usize = 10;
const ML_UPPER_ELEV: f64 = 10.0;
const ML_LOWER_ELEV: f64 = 4.0;
const ML_HIGH_PERCENTILE: f64 = 0.80;
const ML_LOW_PERCENTILE: f64 = 0.20;
const ML_MIN_WET_SNOW_SUM: f64 = 1500.0;
const ML_MIN_SNR: f64 = 5.0;
/// `melting_layer.c`'s beam-height model: 4/3-equivalent `IR·RE`.
const ML_IR: f64 = 1.21;
const ML_RE_KM: f64 = 6371.0;
/// `hca_beamMLIntersection.c`'s effective Earth radius ("per RPG
/// requirements" — not the 8498.67 km the 4/3 model would give).
const BEAM_ML_AE_KM: f64 = 7708.91;
const BEAM_WIDTH_DEG: f64 = 1.0;

/// The `height_0` fallback the source hardcodes when the adaptation store
/// is unreadable: 10.5 kft, in km MSL.
pub const DEFAULT_HEIGHT_0_KM_MSL: f64 = 10.5 * 0.3048;

// ── HSDA (Hail Size Discrimination, CCR NA14-00275; HailSize.cpp v3) ────────

/// `hca.alg`'s `enable_size` fleet default (Yes): product 165 subclasses RH
/// into small/large/giant hail, large and giant carrying their own codes.
const ENABLE_SIZE: bool = true;
/// `hca.alg`'s `min_data_size`: hail-size runs shorter than this despeckle
/// down one size.
const MIN_DATA_SIZE: usize = 2;
/// `dualpol8bit.c`'s `EXT_LH`/`EXT_GH`: the product codes of the RH
/// subclasses (small hail stays at RH's 100).
const EXT_LH: f32 = 110.0;
const EXT_GH: f32 = 120.0;
/// `hail.alg`'s operator-maintained wet-bulb heights, kft MSL → km: the
/// fleet defaults stand in when no environmental value is available.
pub const DEFAULT_HEIGHT_TW0_KM_MSL: f64 = 10.0 * 0.3048;
pub const DEFAULT_HEIGHT_TW_M25_KM_MSL: f64 = 22.0 * 0.3048;
/// `HailSize.cpp`'s hard bounds.
pub(crate) const HSDA_MAX_ZDR: f64 = 2.0;
const HSDA_MIN_ZDR: f64 = -7.75;
const HSDA_MIN_RHO: f64 = 0.0;
const HSDA_MAX_Z: f64 = 100.0;
const HSDA_DELTA_ZDR: f64 = -0.50;
const HSDA_MIN_PV: f64 = 0.2;
const HSDA_MIN_AGG: f64 = 0.6;

/// The wet-bulb heights the HSDA regimes and the RH ZDR-membership
/// modification read, km above radar level — `Hca_process_radial`'s
/// `Hca_0_Tw_height`/`Hca_minus_25_Tw_height` after its MSL → ARL conversion.
#[derive(Debug, Clone, Copy)]
pub struct HsdaHeights {
    pub tw0_km_arl: f64,
    pub twm25_km_arl: f64,
}

impl HsdaHeights {
    /// From MSL heights, as `Hca_process_radial` converts them.
    pub fn from_msl(tw0_km_msl: f64, twm25_km_msl: f64, radar_km_msl: f64) -> Self {
        Self {
            tw0_km_arl: tw0_km_msl - radar_km_msl,
            twm25_km_arl: twm25_km_msl - radar_km_msl,
        }
    }

    /// The `hail.alg` fleet defaults (10.0 / 22.0 kft MSL).
    pub fn operational_defaults(radar_km_msl: f64) -> Self {
        Self::from_msl(
            DEFAULT_HEIGHT_TW0_KM_MSL,
            DEFAULT_HEIGHT_TW_M25_KM_MSL,
            radar_km_msl,
        )
    }

    /// From the sounding's dry-bulb 0 °C / −20 °C heights (km MSL):
    /// −25 °C extrapolated by a quarter of the 0 → −20 °C depth.
    pub fn from_env_heights(h0c_km_msl: f64, hm20c_km_msl: f64, radar_km_msl: f64) -> Self {
        let hm25 = hm20c_km_msl + 0.25 * (hm20c_km_msl - h0c_km_msl);
        Self::from_msl(h0c_km_msl, hm25, radar_km_msl)
    }
}

// ── dpprep transport scales (dpp_format.c / qia_process.c Add_moment) ───────

const SMZ_SCALE: (f64, f64) = (2.0, 66.0);
const SNR_SCALE: (f64, f64) = (2.0, 26.0);
const SDZ_SCALE: (f64, f64) = (8.33, 2.0);
const SDP_SCALE: (f64, f64) = (2.5, 2.0);
const ZDR_SCALE: (f64, f64) = (16.0, 128.0);
const SMV_SCALE: (f64, f64) = (2.0, 129.0);

/// `dpprep.alg`'s texture exclusion thresholds.
const MAX_DIFF_DBZ: f64 = 50.0;
const MAX_DIFF_PHIDP: f64 = 100.0;

// ── Melting layer ────────────────────────────────────────────────────────────

/// Where a [`MeltingLayer`] came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MeltingLayerSource {
    /// The RPG's own melting layer for this volume, inverted from product
    /// 166 (AWIPS `N0M`). Per-azimuth.
    Rpg,
    /// This volume's own 4°–10° tilts through the MLDA.
    RadarDetected,
    /// Flat, at the environmental 0 °C height a sounding gave.
    Sounding,
    /// Flat, at the `hail.alg` fleet adaptation default (10.5 kft MSL) —
    /// a guess, measured to be up to 3.1 km wrong.
    FleetDefault,
}

impl MeltingLayerSource {
    /// Whether this layer was measured for this volume rather than assumed.
    pub fn is_measured(self) -> bool {
        matches!(self, Self::Rpg | Self::RadarDetected)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Rpg => "RPG melting layer",
            Self::RadarDetected => "radar-detected melting layer",
            Self::Sounding => "sounding freezing level",
            Self::FleetDefault => "assumed freezing level",
        }
    }

    pub fn caption(self) -> &'static str {
        match self {
            Self::Rpg => "melting layer from the RPG's own product for this volume",
            Self::RadarDetected => "melting layer detected from this volume's own tilts",
            Self::Sounding => "melting layer assumed flat at the sounding's freezing level",
            Self::FleetDefault => {
                "no melting layer available - assuming 10.5 kft, which in winter \
                 has disagreed with the RPG four times in five"
            }
        }
    }
}

/// Per-azimuth melting-layer top and bottom, km above radar level — the form
/// `Hca_buffer_control` holds (`ML_top`/`ML_bottom`).
#[derive(Debug, Clone)]
pub struct MeltingLayer {
    pub top_km_arl: [f64; 360],
    pub bottom_km_arl: [f64; 360],
    /// Where these heights came from.
    pub source: MeltingLayerSource,
}

impl MeltingLayer {
    /// A flat layer: top at `top_km_arl`, bottom 0.5 km below, both floored
    /// at ground — the source's default construction (`HALF_KM`).
    pub fn flat_from(top_km_arl: f64, source: MeltingLayerSource) -> Self {
        let top = top_km_arl.max(0.0);
        let bottom = (top - ML_DEPTH_KM).max(0.0);
        Self {
            top_km_arl: [top; 360],
            bottom_km_arl: [bottom; 360],
            source,
        }
    }

    /// [`flat_from`](Self::flat_from) at the fleet adaptation default.
    pub fn flat(top_km_arl: f64) -> Self {
        Self::flat_from(top_km_arl, MeltingLayerSource::FleetDefault)
    }

    /// The environmental 0 °C height (km MSL) converted to above-radar-level,
    /// bottom 0.5 km below.
    pub fn from_zero_c_height(h0c_km_msl: f64, radar_km_msl: f64) -> Self {
        Self::flat_from(h0c_km_msl - radar_km_msl, MeltingLayerSource::Sounding)
    }

    /// The RPG's own melting layer for one volume, recovered from its
    /// published **Melting Layer product** (Level III 166, AWIPS `N0M`).
    pub fn from_melting_layer_product(
        message: &nexrad_level3::model::Level3Message,
    ) -> Option<MeltingLayerRecovery> {
        use nexrad_level3::model::DataPacket;

        let elev_deg = f64::from(message.pdb.elevation_angle());
        if !elev_deg.is_finite() || elev_deg <= 0.0 || elev_deg >= 90.0 {
            log::warn!("Melting layer product declares an unusable elevation {elev_deg}");
            return None;
        }

        let rings: Vec<[f64; 360]> = message
            .symbology
            .iter()
            .flat_map(|block| block.layers.iter())
            .flat_map(|layer| layer.packets.iter())
            .filter_map(|packet| match packet {
                DataPacket::LinkedContour(contour) => Some(ring_radii_km(contour)),
                _ => None,
            })
            .collect();
        if rings.len() < 4 {
            log::warn!(
                "Melting layer product carries {} contours, not the four the layer needs",
                rings.len()
            );
            return None;
        }

        // Widest first.
        let mut ranked: Vec<(f64, usize)> = rings
            .iter()
            .enumerate()
            .map(|(i, r)| (median(r), i))
            .collect();
        ranked.sort_by(|a, b| b.0.total_cmp(&a.0));

        let at = |slot: usize, ray_deg: f64| -> [f64; 360] {
            let ring = &rings[ranked[slot].1];
            std::array::from_fn(|az| {
                if ring[az] > 0.0 {
                    ml_height_from_range(ray_deg, ring[az])
                } else {
                    0.0
                }
            })
        };
        let half_bw = BEAM_WIDTH_DEG / 2.0;
        let top_km_arl = at(1, elev_deg);
        let bottom_km_arl = at(2, elev_deg);
        let top_from_edge = at(0, elev_deg - half_bw);
        let bottom_from_edge = at(3, elev_deg + half_bw);

        let consistency_km = (0..360)
            .map(|az| {
                (top_km_arl[az] - top_from_edge[az])
                    .abs()
                    .max((bottom_km_arl[az] - bottom_from_edge[az]).abs())
            })
            .fold(0.0f64, f64::max);
        let depth_km = mean(&top_km_arl) - mean(&bottom_km_arl);

        Some(MeltingLayerRecovery {
            layer: MeltingLayer {
                top_km_arl,
                bottom_km_arl,
                source: MeltingLayerSource::Rpg,
            },
            depth_km,
            consistency_km,
        })
    }
}

/// A melting layer recovered from product 166, with two self-check numbers.
#[derive(Debug, Clone)]
pub struct MeltingLayerRecovery {
    pub layer: MeltingLayer,
    /// Mean top − mean bottom, km.
    pub depth_km: f64,
    /// Largest disagreement over all azimuths between a height read off the
    /// beam-centre ring and off the beam-edge ring at `elev ∓ bw/2`.
    pub consistency_km: f64,
}

impl MeltingLayerRecovery {
    /// Whether the two self-checks agree with what the algorithm draws.
    pub fn looks_sound(&self) -> bool {
        // A bottom above its top would hand `Hca_beamMLintersection` zone
        // bounds in the wrong order; reachable only if two rings rank equal
        // by median radius and sort the wrong way.
        if (0..360).any(|az| self.layer.bottom_km_arl[az] > self.layer.top_km_arl[az]) {
            return false;
        }
        if self.consistency_km > MAX_ML_INCONSISTENCY_KM {
            return false;
        }
        let ground_truncated = self
            .layer
            .bottom_km_arl
            .iter()
            .filter(|&&b| b <= 0.0)
            .count()
            > 180;
        ground_truncated || (0.25..=0.75).contains(&self.depth_km)
    }
}

/// How far apart the beam-centre and beam-edge routes to one height may land
/// before [`MeltingLayerRecovery::looks_sound`] gives up on the recovery.
const MAX_ML_INCONSISTENCY_KM: f64 = 0.5;

/// Per whole degree of azimuth, the radius of one contour, km.
fn ring_radii_km(contour: &nexrad_level3::model::LinkedContourPacket) -> [f64; 360] {
    let mut best = [(f64::INFINITY, 0.0f64); 360];
    for (east_km, north_km) in contour.points_km() {
        let radius = east_km.hypot(north_km);
        if radius <= 0.0 {
            continue;
        }
        let az = east_km.atan2(north_km).to_degrees().rem_euclid(360.0);
        for (i, slot) in best.iter_mut().enumerate() {
            let separation = (az - (i as f64 + 0.5) + 180.0).rem_euclid(360.0) - 180.0;
            if separation.abs() < slot.0 {
                *slot = (separation.abs(), radius);
            }
        }
    }
    std::array::from_fn(|i| best[i].1)
}

fn mean(values: &[f64; 360]) -> f64 {
    values.iter().sum::<f64>() / 360.0
}

fn median(values: &[f64; 360]) -> f64 {
    let mut sorted = *values;
    sorted.sort_by(f64::total_cmp);
    (sorted[179] + sorted[180]) / 2.0
}

/// The four beam/melting-layer intersection ranges of one radial, as DP bin
/// numbers (`Hca_beamMLintersection`).
#[derive(Debug, Clone, Copy)]
pub(crate) struct MlBins {
    /// `BEAM_EDGE_BOTTOM`: the beam's *upper* edge crossing the layer
    /// bottom — the nearest of the four, the absolute bottom of the layer.
    bb: i64,
    b: i64,
    t: i64,
    /// `BEAM_EDGE_TOP`: the beam's *lower* edge crossing the layer top —
    /// the farthest of the four, the absolute top of the layer.
    pub(crate) tt: i64,
}

/// `Hca_beamMLintersection`: where the 1° beam's bottom edge, centre and
/// top edge cross the layer, on the 7708.91-km effective Earth.
pub(crate) fn beam_ml_intersection(
    elev_deg: f64,
    az: usize,
    bin_size_km: f64,
    ml: &MeltingLayer,
) -> MlBins {
    let half_bw = (BEAM_WIDTH_DEG / 2.0).to_radians();
    let e = elev_deg.to_radians();
    let ae = BEAM_ML_AE_KM;
    let range = |h: f64, s: f64| (2.0 * h * ae + ae * ae * s * s).sqrt() - ae * s;
    let r_bb = range(ml.bottom_km_arl[az], (e + half_bw).sin());
    let r_b = range(ml.bottom_km_arl[az], e.sin());
    let r_t = range(ml.top_km_arl[az], e.sin());
    let r_tt = range(ml.top_km_arl[az], (e - half_bw).sin());
    MlBins {
        bb: (r_bb / bin_size_km).round() as i64,
        b: (r_b / bin_size_km).round() as i64,
        t: (r_t / bin_size_km).round() as i64,
        tt: (r_tt / bin_size_km).round() as i64,
    }
}

// ── Membership machinery ─────────────────────────────────────────────────────

/// `Hca_setMembershipPoints`: the class×input row's four points, the 2-D rows adjusted
/// by `f1/f2/f3/g1/g2` of the (FShield-adjusted) reflectivity.
fn set_membership_points(
    class: usize,
    fl_input: usize,
    z_fshield: f64,
    height_km: f64,
    tw0_km_arl: f64,
) -> [f64; 4] {
    let table = MEM[class - RA];
    let mut points = [0.0; 4];
    for (x, point) in points.iter_mut().enumerate() {
        let flag = table.flags[fl_input][x];
        let mut eqn = match flag {
            MemFlag::None => 0.0,
            MemFlag::F1 => F1_COEF.0 * z_fshield * z_fshield + F1_COEF.1 * z_fshield + F1_COEF.2,
            MemFlag::F2 => F2_COEF.0 * z_fshield * z_fshield + F2_COEF.1 * z_fshield + F2_COEF.2,
            MemFlag::F3 => F3_COEF.0 * z_fshield * z_fshield + F3_COEF.1 * z_fshield + F3_COEF.2,
            MemFlag::G1 => G1_COEF.0 * z_fshield + G1_COEF.1,
            MemFlag::G2 => G2_COEF.0 * z_fshield + G2_COEF.1,
        };
        if ENABLE_SIZE && class == RH && fl_input == ZDR && flag == MemFlag::F1 {
            if tw0_km_arl - 2.0 < height_km && height_km <= tw0_km_arl - 1.0 {
                eqn = 5e-4 * z_fshield * z_fshield + 1.5e-2 * z_fshield - 0.9;
            } else if tw0_km_arl - 1.0 < height_km && height_km < tw0_km_arl {
                eqn = 0.02 * z_fshield - 0.6;
            }
        }
        *point = eqn + table.points[fl_input][x];
    }
    points
}

/// `Hca_degreeMembership`: the trapezoid, 0 outside (x1, x4), 1 on
/// [x2, x3], linear on the shoulders — and 0 outright when the points are
/// not monotonic (which the Z-dependent rows produce at extreme Z).
fn degree_membership(d: f64, points: [f64; 4]) -> f64 {
    let [x1, x2, x3, x4] = points;
    if x1 > x2 || x2 > x3 || x3 > x4 {
        return 0.0;
    }
    if d >= x2 && d <= x3 {
        1.0
    } else if d <= x1 || d >= x4 {
        0.0
    } else if d > x1 && d < x2 {
        (d - x1) / (x2 - x1)
    } else {
        (x4 - d) / (x4 - x3)
    }
}

/// `Hca_weightedMembershipAggregation`: `Σ WQF / (Σ WQ + 0.01)`.
fn weighted_aggregation(weight: &[f64; 6], quality: &[f64; 6], fd_mem: &[f64; 6]) -> f64 {
    let mut s = 0.0;
    for i in 0..NUM_FL_INPUTS {
        s += weight[i] * quality[i];
    }
    let mut sfd = 0.0;
    for i in 0..NUM_FL_INPUTS {
        sfd += weight[i] * quality[i] * fd_mem[i] / (s + 0.01);
    }
    sfd
}

/// `Hca_allowedHydroClass`: the hard thresholds and the melting-layer
/// zones, setting disallowed classes to `INVALID_CLASS`.
#[allow(clippy::too_many_arguments)]
fn allowed_hydro_class(
    bin: i64,
    z: f64,
    zdr: f64,
    rho: f64,
    phi: f64,
    v: f64,
    atten_rad: bool,
    agg: &mut [f64; NUM_CLASSES],
    ml: MlBins,
) {
    const INVALID: f64 = -1.0;
    agg[U0] = INVALID;
    agg[U1] = INVALID;

    // The RF sentinel (−2e5) never occurs here (see the module doc), so the
    // velocity guard reduces to the NO_DATA check.
    if v != NO_DATA && v.abs() > MIN_V_GC {
        agg[GC] = INVALID;
    }
    if z > MAX_Z_RA {
        agg[RA] = INVALID;
    }
    if z < MIN_Z_RH {
        agg[RH] = INVALID;
    }
    if z < MIN_Z_HR || zdr < MIN_ZDR_HR {
        agg[HR] = INVALID;
    }
    if z > MAX_Z_IC {
        agg[IC] = INVALID;
    }
    if !(MIN_Z_GR..=MAX_Z_GR).contains(&z) || zdr > MAX_ZDR_GR {
        agg[GR] = INVALID;
    }
    if z < MIN_Z_BD || zdr < MIN_ZDR_BD {
        agg[BD] = INVALID;
    }
    // B21 (CCR NA15-00181): the WS kill lost its Z leg.
    if zdr < MIN_ZDR_WS {
        agg[WS] = INVALID;
    }
    if zdr > MAX_ZDR_DS {
        agg[DS] = INVALID;
    }
    if ATTEN_CONTROL && atten_rad {
        if rho > MAX_RHOHV_BI {
            agg[BI] = INVALID;
        }
    } else if rho > MAX_RHOHV_BI || z > MAX_Z_BI {
        agg[BI] = INVALID;
    }
    if rho < MIN_RHO_RA && phi < MIN_PHIDP_RA {
        agg[RA] = INVALID;
    }

    let allowed: &[usize] = if bin < ml.bb {
        &[GC, BI, BD, RA, HR, RH]
    } else if bin < ml.b {
        &[GC, BI, WS, GR, BD, RA, HR, RH]
    } else if bin < ml.t {
        &[GC, BI, DS, WS, GR, BD, RH]
    } else if bin < ml.tt {
        &[GC, BI, DS, WS, IC, GR, BD, RH]
    } else {
        &[GC, BI, DS, IC, GR, RH]
    };
    for (i, a) in agg.iter_mut().enumerate() {
        if !allowed.contains(&i) {
            *a = INVALID;
        }
    }
}

/// `Break_tie` (CCR NA14-00181): when the top two aggregations sit within
/// `min_Dif_Agg`, the class is chosen by the AEL Table 4 priority order of
/// the gate's melting-layer zone, with the source's "tuned" upper lists.
fn break_tie(bin: i64, ml: MlBins, h_class: usize, runner_up: usize) -> usize {
    let priority: &[usize] = if bin < ml.bb {
        &[GC, BI, BD, RA, HR, RH]
    } else if bin < ml.b {
        &[GC, BI, WS, GR, BD, RA, HR, RH]
    } else if bin < ml.t {
        &[GC, BI, DS, WS, GR, BD, RH]
    } else if bin < ml.tt {
        &[BI, GC, DS, WS, IC, GR, BD, RH] // "tuned"
    } else {
        &[GC, BI, DS, IC, GR, RH]
    };
    for &c in priority {
        if c == h_class {
            return h_class;
        }
        if c == runner_up {
            return runner_up;
        }
    }
    h_class
}

// ── The preprocessed per-radial fields HCA and the MLDA consume ─────────────

/// One recombined radial's HCA inputs, in the C sentinel domain
/// ([`NO_DATA`] for missing) after the documented moment transport.
pub(crate) struct Fields {
    pub(crate) az: f64,
    pub(crate) elev: f64,
    pub(crate) hatt: bool,
    pub(crate) n: usize,
    pub(crate) dg: f64,
    /// `DSMZ` (z_prcd), `DSNR`, `DSDZ` — the z-gate fields sampled at each
    /// DP gate.
    pub(crate) smz: Vec<f64>,
    pub(crate) snr: Vec<f64>,
    pub(crate) sdz: Vec<f64>,
    pub(crate) zdr: Vec<f64>,
    pub(crate) rho: Vec<f64>,
    pub(crate) kdp: Vec<f64>,
    pub(crate) phi: Vec<f64>,
    pub(crate) sdp: Vec<f64>,
    pub(crate) smv: Vec<f64>,
    /// The cleaned met signal per gate (`DMET`), NaN when the legacy flag
    /// ran instead — the hybrid-scan compositor's usability check reads it.
    pub(crate) met: Vec<f64>,
    /// The six quality indices per gate, in fuzzy-logic input order.
    pub(crate) q: Vec<[f64; 6]>,
}

/// One value through an 8-bit moment (`Add_moment` then
/// `RPGCS_radar_data_conversion`): round half away from zero at
/// `v·scale + offset`, clamp to [2, 255], decode back.
fn transport8(v: f64, (scale, offset): (f64, f64)) -> f64 {
    if !v.is_finite() {
        return f64::NAN;
    }
    let f = v * scale + offset;
    let t = if f >= 0.0 {
        (f + 0.5) as i64
    } else {
        -((-f + 0.5) as i64)
    };
    let t = t.clamp(2, 255);
    (t as f64 - offset) / scale
}

/// NaN → the C sentinel.
fn sentinel(v: f64) -> f64 {
    if v.is_finite() { v } else { NO_DATA }
}

/// The full dpprep + QIA chain for one recombined radial.
pub(crate) fn radial_fields(
    c: &DpCombined,
    init_fdp: f64,
    dbz0: Option<f64>,
    atmos: Option<f64>,
    quantize: bool,
    metsignal: bool,
    cappi: Option<&ReflCappi>,
) -> Fields {
    let r = &c.base;
    let n = r.phi.len();
    let nz = r.z.len();

    // SNR precedes the met signal (Compute_snr's first call).
    let ref_smd3 = average_filter(&r.z, DBZ_WINDOW);
    let snr_z: Vec<f64> = (0..nz)
        .map(|iz| match dbz0 {
            Some(dbz0) if !ref_smd3[iz].is_nan() => {
                let rr = (r.zr0 + iz as f64 * r.zg).max(1e-9);
                ref_smd3[iz] - 20.0 * rr.log10() + atmos.unwrap_or(0.0) * rr - dbz0
            }
            _ => f64::NAN,
        })
        .collect();

    // The met signal reads the raw fields — φ before unfolding.
    let met = if metsignal {
        let pick_z = |field: &[f64], i: usize| -> f64 {
            let d = r.dr0 + i as f64 * r.dg;
            index_into(d, r.zr0, r.zg, field.len())
                .map(|iz| field[iz])
                .unwrap_or(f64::NAN)
        };
        let z_dp: Vec<f64> = (0..n).map(|i| pick_z(&r.z, i)).collect();
        let snr_dp: Vec<f64> = (0..n).map(|i| pick_z(&snr_z, i)).collect();
        let mut met = find_met_signal(&z_dp, &r.vel, &c.zdr, &r.rho, &r.phi, &snr_dp);
        clean_met_signal(&mut met, MET_SIG_THRESHOLD);
        if let Some(cappi) = cappi {
            cappi.apply_radial(c.elev, r.az, r.dr0, r.dg, &mut met);
        }
        Some(met)
    } else {
        None
    };

    let mut phi = r.phi.clone();
    match &met {
        Some(met) => unfold_phidp(&mut phi, met, MET_SIG_THRESHOLD, init_fdp),
        None => unfold_phidp(&mut phi, &r.rho, UNFOLD_MIN_RHO, init_fdp),
    }

    // Textures about their own smoothing windows (dpp_process.c order).
    let ref_smd5 = average_filter(&r.z, WINDOW);
    let sd_zh = std_filter(&r.z, &ref_smd5, WINDOW, MAX_DIFF_DBZ);
    let phi_smd9 = average_filter(&phi, SHORT_GATE);
    let sd_phi = std_filter(&phi, &phi_smd9, SHORT_GATE, MAX_DIFF_PHIDP);

    let rho_smd = average_filter(&r.rho, WINDOW);
    let zdr_smd = average_filter(&c.zdr, WINDOW);
    let vel_smd = average_filter(&r.vel, WINDOW);

    let hatt = is_high_attenuation_radial(&r.z, &r.vel, &r.spw, &r.rho);

    // Meteorological flag: cleaned met signal above threshold (strictly —
    // dpp_process.c zeroes `<=`), or the legacy construction.
    let mut flag = vec![false; n];
    match &met {
        Some(met) => {
            for (i, f) in flag.iter_mut().enumerate() {
                *f = met[i] > MET_SIG_THRESHOLD;
            }
        }
        None if hatt && dbz0.is_some() => {
            let ngs = n.min(snr_z.len());
            for (i, f) in flag.iter_mut().enumerate().take(ngs) {
                *f = snr_z[i] >= crate::dpprep::MD_SNR_THRESH && !phi[i].is_nan();
            }
        }
        None => {
            for (i, f) in flag.iter_mut().enumerate() {
                *f = rho_smd[i] >= CORR_THRESH && !phi[i].is_nan();
            }
        }
    }
    let groups = meteo_groups(&flag);

    let mut phi_med = median_filter(&phi, WINDOW);
    for (i, f) in flag.iter().enumerate() {
        if !f {
            phi_med[i] = f64::NAN;
        }
    }
    let phi_short = interpolate(
        &average_filter(&phi_med, SHORT_GATE),
        SHORT_GATE,
        &groups,
        init_fdp,
    );
    let phi_long = interpolate(
        &average_filter(&phi_med, LONG_GATE),
        LONG_GATE,
        &groups,
        init_fdp,
    );

    let kdp9 = kdp_from_phi(&phi_short, SHORT_GATE, r.dg);
    let kdp25 = kdp_from_phi(&phi_long, LONG_GATE, r.dg);

    // z_prcd / zdr_prcd with the ΦDP-driven attenuation corrections
    // (Create_corrected_fields_and_adjust_kdp; the syscals are 0).
    let z_prcd: Vec<f64> = (0..nz)
        .map(|iz| {
            if ref_smd3[iz].is_nan() {
                return f64::NAN;
            }
            let zr = r.zr0 + iz as f64 * r.zg;
            let delta = match index_into(zr, r.dr0, r.dg, n) {
                Some(id) if phi_long[id].is_finite() && phi_long[id] >= init_fdp => {
                    0.04 * (phi_long[id] - init_fdp)
                }
                _ => 0.0,
            };
            ref_smd3[iz] + delta
        })
        .collect();
    let zdr_prcd: Vec<f64> = (0..n)
        .map(|i| {
            if zdr_smd[i].is_nan() {
                return f64::NAN;
            }
            let delta = if phi_long[i].is_finite() && phi_long[i] >= init_fdp {
                0.004 * (phi_long[i] - init_fdp)
            } else {
                0.0
            };
            zdr_smd[i] + delta
        })
        .collect();

    // The merged, censored KDP (the DKDP moment).
    let kdp_merged: Vec<f64> = (0..n)
        .map(|i| {
            if rho_smd[i].is_nan() || rho_smd[i] < CORR_THRESH {
                return f64::NAN;
            }
            let d = r.dr0 + i as f64 * r.dg;
            let zp = index_into(d, r.zr0, r.zg, nz)
                .map(|iz| z_prcd[iz])
                .unwrap_or(f64::NAN);
            if zp.is_finite() && zp > DBZ_THRESH {
                kdp9[i]
            } else {
                kdp25[i]
            }
        })
        .collect();

    // Moment transport: presence keys on the raw input (Add_moment's `inp`).
    let q8 = |v: f64, s: (f64, f64)| if quantize { transport8(v, s) } else { v };
    let mut fields = Fields {
        az: r.az,
        elev: c.elev,
        hatt,
        n,
        dg: r.dg,
        smz: Vec::with_capacity(n),
        snr: Vec::with_capacity(n),
        sdz: Vec::with_capacity(n),
        zdr: Vec::with_capacity(n),
        rho: Vec::with_capacity(n),
        kdp: Vec::with_capacity(n),
        phi: Vec::with_capacity(n),
        sdp: Vec::with_capacity(n),
        smv: Vec::with_capacity(n),
        // The DMET moment (8-bit, scale 2 / offset 50).
        met: match &met {
            Some(m) => m
                .iter()
                .map(|&v| {
                    if quantize {
                        transport8(v, (2.0, 50.0))
                    } else {
                        v
                    }
                })
                .collect(),
            None => vec![f64::NAN; n],
        },
        q: Vec::with_capacity(n),
    };
    for i in 0..n {
        let d = r.dr0 + i as f64 * r.dg;
        let zi = index_into(d, r.zr0, r.zg, nz);
        let z_present = zi.map(|iz| !r.z[iz].is_nan()).unwrap_or(false);
        // Quantize in the NaN domain (transport8 keeps NaN as NaN, i.e. an
        // undefined field value encodes level 0), sentinel afterwards.
        let pick_z = |field: &[f64]| -> f64 { zi.map(|iz| field[iz]).unwrap_or(f64::NAN) };
        fields.smz.push(if z_present {
            sentinel(q8(pick_z(&z_prcd), SMZ_SCALE))
        } else {
            NO_DATA
        });
        fields.snr.push(if z_present {
            sentinel(q8(pick_z(&snr_z), SNR_SCALE))
        } else {
            NO_DATA
        });
        fields.sdz.push(if z_present {
            sentinel(q8(pick_z(&sd_zh), SDZ_SCALE))
        } else {
            NO_DATA
        });

        let zdr_present = !c.zdr.get(i).copied().unwrap_or(f64::NAN).is_nan();
        fields.zdr.push(if zdr_present {
            sentinel(q8(zdr_prcd[i], ZDR_SCALE))
        } else {
            NO_DATA
        });

        let phi_present = !r.phi[i].is_nan();
        fields.rho.push(if !r.rho[i].is_nan() {
            sentinel(rho_smd[i])
        } else {
            NO_DATA
        });
        fields.kdp.push(if phi_present {
            sentinel(kdp_merged[i])
        } else {
            NO_DATA
        });
        fields.phi.push(if phi_present {
            sentinel(phi_long[i])
        } else {
            NO_DATA
        });
        fields.sdp.push(if phi_present {
            sentinel(q8(sd_phi[i], SDP_SCALE))
        } else {
            NO_DATA
        });

        let vel_raw = r.vel.get(i).copied().unwrap_or(f64::NAN);
        fields.smv.push(if !vel_raw.is_nan() {
            sentinel(q8(vel_smd.get(i).copied().unwrap_or(f64::NAN), SMV_SCALE))
        } else {
            NO_DATA
        });

        fields.q.push(quality_indices(
            fields.phi[i],
            fields.rho[i],
            fields.smz[i],
            fields.snr[i],
            quantize,
        ));
    }
    fields
}

/// `Qia_process_radial`'s six indices for one gate, in fuzzy-logic input order (SMZ,
/// ZDR, LKDP, RHO, SDZ, SDP).
fn quality_indices(phi: f64, rho: f64, smz: f64, snr: f64, quantize: bool) -> [f64; 6] {
    let linear_snr = 10f64.powf(0.1 * snr);
    let ac = phi / PHI_DP_Z_THRESH;
    let bc = 1.0 / linear_snr;
    let cc = phi / PHI_DP_ZDR_THRESH;
    let mut dc = (1.0 - rho) / DELTA_RHO_1_THRESHOLD;
    let ec = LINEAR_SNR_ZDR_THRESH / linear_snr;
    let fc = phi / PHI_DP_PHI_THRESH;
    let hc = 1.0 / linear_snr;
    let ic = phi / PHI_DP_KDP_THRESH;
    let lc = 1.0 / linear_snr;
    if rho < RHO_MIN_THRESH && smz < Z_ATTEN_THRESH {
        dc = 0.0;
    }
    let fix = |q: f64| if q.is_finite() { q } else { 0.0 };
    let mut q = [
        fix((QIA_C * (ac * ac + bc * bc)).exp()),
        fix((QIA_C * (cc * cc + dc * dc + ec * ec)).exp()),
        fix((QIA_C * (ic * ic + dc * dc + hc * hc)).exp()),
        fix((QIA_C * (fc * fc + dc * dc + hc * hc)).exp()),
        fix((QIA_C * (lc * lc)).exp()),
        fix((QIA_C * (hc * hc)).exp()),
    ];
    if quantize {
        for v in q.iter_mut() {
            *v = transport8(*v, (Q_SCALE, Q_OFFSET));
        }
    }
    q
}

/// One gate through `Hca_process_radial`'s classification: returns the internal class
/// index.
fn classify_gate(f: &Fields, bin: usize, ml: MlBins, tw0_km_arl: f64) -> usize {
    if f.snr[bin] < MIN_SNR {
        return NE;
    }
    // (The RF → UK branch is unreachable here; see the module doc.)

    let z_fshield = f.smz[bin]; // no blockage: FShield adjustment is 0
    // `RPGCS_height(bin·dg, elev)` — the bin height the HSDA membership
    // modification reads (the C measures range from bin 0, not `dr0`).
    let height_km = ml_height_from_range(f.elev, bin as f64 * f.dg);

    let mut agg = [0.0f64; NUM_CLASSES];
    allowed_hydro_class(
        bin as i64, f.smz[bin], f.zdr[bin], f.rho[bin], f.phi[bin], f.smv[bin], f.hatt, &mut agg,
        ml,
    );

    let lkdp = if f.kdp[bin] >= 0.001 {
        10.0 * f.kdp[bin].log10()
    } else {
        MINI_LKTP
    };
    let mut d = [0.0f64; NUM_FL_INPUTS];
    d[SMZ] = z_fshield;
    d[ZDR] = f.zdr[bin];
    d[LKDP] = lkdp;
    d[RHO] = f.rho[bin];
    d[SDZ] = f.sdz[bin];
    d[SDP] = f.sdp[bin];
    let quality = f.q[bin];

    for (h_class, a) in agg.iter_mut().enumerate() {
        if *a == -1.0 {
            *a = 0.0;
            continue;
        }
        // U0/U1/UK/NE carry all-zero weights in the adaptation data, so
        // their aggregations are identically 0 — skip the arithmetic.
        if !(RA..=GR).contains(&h_class) {
            continue;
        }
        let mut fd_mem = [0.0f64; 6];
        for (fl_input, fd) in fd_mem.iter_mut().enumerate() {
            let points = set_membership_points(h_class, fl_input, z_fshield, height_km, tw0_km_arl);
            *fd = degree_membership(d[fl_input], points);
        }
        *a = weighted_aggregation(&WEIGHT[h_class - RA], &quality, &fd_mem);
    }

    // The largest aggregation wins (first index on ties, as the C's strict
    // `<` keeps the earlier class), then the min_Agg gate; a margin under
    // min_Dif_Agg goes to the AEL Table 4 tie-break (B21; B16 read UK).
    let mut agg_max = -2.0;
    let mut max_cal = NE;
    for (h_class, &a) in agg.iter().enumerate() {
        if agg_max < a {
            agg_max = a;
            max_cal = h_class;
        }
    }
    let mut top_diff = 100.0;
    let mut runner_up = UK;
    for (h_class, &a) in agg.iter().enumerate() {
        if h_class != max_cal {
            let diff = agg_max - a;
            if diff < top_diff {
                top_diff = diff;
                runner_up = h_class;
            }
        }
    }
    if agg_max < MIN_AGG {
        return UK;
    }
    if top_diff < MIN_DIF_AGG {
        return break_tie(bin as i64, ml, max_cal, runner_up);
    }
    max_cal
}

pub(crate) fn classify_radial(f: &Fields, ml: &MeltingLayer, tw0_km_arl: f64) -> Vec<usize> {
    let az = (f.az.rem_euclid(360.0)) as usize % 360;
    let bins = beam_ml_intersection(f.elev, az, f.dg, ml);
    (0..f.n)
        .map(|bin| classify_gate(f, bin, bins, tw0_km_arl))
        .collect()
}

// ── Hail size discrimination (HailSize.cpp v3) ───────────────────────────────

/// The RH subclassification (`data.sub`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HailSize {
    NotHail,
    Current,
    Small,
    Large,
    Giant,
}

/// One height regime's three (Z, ZDR, ρ) trapezoids, small/large/giant.
type HsdaTraps = [[[f64; 4]; 3]; 3];

/// `HailSize_v3`'s inline trapezoids for one gate: the six height regimes against the
/// wet-bulb heights, the ZDR rows of the lower regimes built from the hail-size `f`/`g`
/// polynomials at the gate's Z (all carrying `DeltaZdr = −0.5`).
fn hsda_regime(height_km: f64, hs: &HsdaHeights, z: f64) -> ([f64; 3], HsdaTraps) {
    let dz = HSDA_DELTA_ZDR;
    let f1 = -0.5 + 2.5e-3 * z + 7.5e-4 * z * z + dz;
    let f2 = 0.1 * (z - 50.0) + dz;
    let f3 = 0.1 * (z - 60.0) + dz;
    let g1 = -0.9 + 1.5e-2 * z + 5.0e-4 * z * z + dz;
    let g2 = 0.075 * (z - 50.0) + dz;
    let g3 = 0.075 * (z - 60.0) + dz;
    let (zmin, rmin, zmax) = (HSDA_MIN_ZDR, HSDA_MIN_RHO, HSDA_MAX_Z);
    let (tw0, twm25) = (hs.tw0_km_arl, hs.twm25_km_arl);

    if height_km > twm25 {
        (
            [1.0, 0.3, 0.6],
            [
                [
                    [45.0, 50.0, 60.0, 65.0],
                    [-0.5, -0.3, 0.3, 0.5],
                    [0.92, 0.96, 0.99, 1.0],
                ],
                [
                    [48.0, 58.0, 63.0, 68.0],
                    [-0.5, -0.3, 0.3, 0.5],
                    [0.92, 0.96, 0.99, 1.0],
                ],
                [
                    [50.0, 60.0, zmax, zmax + 1.0],
                    [zmin - 1.0, zmin, 0.3, 0.5],
                    [rmin - 1.0, rmin, 0.99, 1.0],
                ],
            ],
        )
    } else if height_km > tw0 {
        (
            [1.0, 0.3, 0.6],
            [
                [
                    [45.0, 50.0, 60.0, 65.0],
                    [-0.5, -0.3, 0.3, 0.5],
                    [0.92, 0.96, 0.99, 1.0],
                ],
                [
                    [48.0, 58.0, 63.0, 68.0],
                    [-0.5, -0.3, 0.3, 0.5],
                    [0.86, 0.90, 0.96, 0.98],
                ],
                [
                    [50.0, 60.0, zmax, zmax + 1.0],
                    [zmin - 1.0, zmin, 0.2, 0.5],
                    [rmin - 1.0, rmin, 0.93, 0.98],
                ],
            ],
        )
    } else if height_km > tw0 - 1.0 {
        (
            [0.8, 0.5, 0.6],
            [
                [
                    [45.0, 50.0, 60.0, 65.0],
                    [-0.1, 0.3, 0.7, 1.2],
                    [0.93, 0.96, 0.99, 1.0],
                ],
                [
                    [48.0, 58.0, 63.0, 68.0],
                    [-0.3, 0.1, 0.5, 1.0],
                    [0.80, 0.91, 0.97, 0.98],
                ],
                [
                    [50.0, 60.0, zmax, zmax + 1.0],
                    [zmin - 1.0, zmin, 0.2, 0.7],
                    [rmin - 1.0, rmin, 0.94, 0.98],
                ],
            ],
        )
    } else if height_km > tw0 - 2.0 {
        (
            [0.7, 0.8, 0.6],
            [
                [
                    [45.0, 52.0, 62.0, 67.0],
                    [g2 - 0.3, g2, g1, g1 + 0.3],
                    [0.94, 0.96, 0.98, 1.0],
                ],
                [
                    [50.0, 60.0, 65.0, 70.0],
                    [g3 - 0.3, g3, g2, g2 + 0.3],
                    [0.80, 0.91, 0.97, 0.98],
                ],
                [
                    [52.0, 62.0, zmax, zmax + 1.0],
                    [zmin - 1.0, zmin, g3, g3 + 0.3],
                    [rmin - 1.0, rmin, 0.96, 0.98],
                ],
            ],
        )
    } else if height_km > tw0 - 3.0 {
        (
            [0.7, 1.0, 0.6],
            [
                [
                    [45.0, 49.0, 59.0, 64.0],
                    [f2 - 0.3, f2, f1, f1 + 0.3],
                    [0.91, 0.94, 0.96, 0.99],
                ],
                [
                    [50.0, 57.0, 62.0, 67.0],
                    [f3 - 0.3, f3, f2, f2 + 0.3],
                    [0.80, 0.93, 0.96, 0.99],
                ],
                [
                    [50.0, 59.0, zmax, zmax + 1.0],
                    [zmin - 1.0, zmin, f3, f3 + 0.3],
                    [rmin - 1.0, rmin, 0.93, 0.98],
                ],
            ],
        )
    } else {
        (
            [0.7, 1.0, 0.6],
            [
                [
                    [45.0, 47.0, 57.0, 62.0],
                    [f2 - 0.3, f2, f1, f1 + 0.3],
                    [0.91, 0.94, 0.96, 0.99],
                ],
                [
                    [50.0, 55.0, 60.0, 65.0],
                    [f3 - 0.3, f3, f2, f2 + 0.3],
                    [0.80, 0.93, 0.96, 0.99],
                ],
                [
                    [50.0, 57.0, zmax, zmax + 1.0],
                    [zmin - 1.0, zmin, f3, f3 + 0.3],
                    [rmin - 1.0, rmin, 0.93, 0.98],
                ],
            ],
        )
    }
}

/// `HailSize_v3` over one radial: subclassify the RH gates by hail size.
fn hail_size_radial(f: &Fields, classes: &[usize], hs: &HsdaHeights) -> Vec<HailSize> {
    use crate::dpprep::trap4;
    let mut sub: Vec<HailSize> = classes
        .iter()
        .map(|&c| {
            if c == RH {
                HailSize::Current
            } else {
                HailSize::NotHail
            }
        })
        .collect();

    for (i, cell) in sub.iter_mut().enumerate().take(f.n) {
        if *cell != HailSize::Current {
            continue;
        }
        let z = f.smz[i];
        let zdr = f.zdr[i];
        let rho = f.rho[i];
        let height_km = ml_height_from_range(f.elev, i as f64 * f.dg);
        let (w, traps) = hsda_regime(height_km, hs, z);
        let q = [f.q[i][SMZ], f.q[i][ZDR], f.q[i][RHO]];

        let mut agg = [0.0f64; 3];
        for (s, a) in agg.iter_mut().enumerate() {
            let t = &traps[s];
            let pv = [
                trap4(z, t[0][0], t[0][1], t[0][2], t[0][3]),
                trap4(zdr, t[1][0], t[1][1], t[1][2], t[1][3]),
                trap4(rho, t[2][0], t[2][1], t[2][2], t[2][3]),
            ];
            let sum_weights = w[0] * q[0] + w[1] * q[1] + w[2] * q[2];
            *a = (w[0] * pv[0] * q[0] + w[1] * pv[1] * q[1] + w[2] * pv[2] * q[2]) / sum_weights;
            // The "handcuffs": large and giant need every input to carry
            // at least some membership.
            if s != 0 && (pv[0] < HSDA_MIN_PV || pv[1] < HSDA_MIN_PV || pv[2] < HSDA_MIN_PV) {
                *a = 0.0;
            }
        }

        // Strict `>` keeps the earlier (smaller) size on ties; a NaN
        // aggregation (all-zero qualities) selects nothing, as in the C.
        let mut max_value = -1.0f64;
        let mut max_index = 0usize;
        for (s, &a) in agg.iter().enumerate() {
            if a > max_value {
                max_value = a;
                max_index = s;
            }
        }
        if max_value >= HSDA_MIN_AGG {
            // max_hail_cat is pinned at giant in the released source, so
            // the category caps never bind.
            *cell = match max_index {
                0 => HailSize::Small,
                1 => HailSize::Large,
                _ => HailSize::Giant,
            };
        }
        // Hard limit: high ZDR is never large/giant hail.
        if zdr >= HSDA_MAX_ZDR {
            *cell = HailSize::Small;
        }
    }

    despeckle_hail(&mut sub, HailSize::Giant, HailSize::Large);
    despeckle_hail(&mut sub, HailSize::Large, HailSize::Small);
    sub
}

/// One gate's product code: `dualpol8bit.c`'s `Class_external` with the RH subclass
/// split (`EXT_LH`/`EXT_GH`; small hail and unsized RH keep RH's 100).
fn external_code(class: usize, size: HailSize) -> f32 {
    let code = if class == RH {
        match size {
            HailSize::Large => EXT_LH,
            HailSize::Giant => EXT_GH,
            _ => CLASS_EXTERNAL[RH],
        }
    } else {
        CLASS_EXTERNAL[class]
    };
    if code == 0.0 { f32::NAN } else { code }
}

/// One despeckle pass: runs of `from` shorter than `min_data_size` become `to`.
fn despeckle_hail(sub: &mut [HailSize], from: HailSize, to: HailSize) {
    let mut short_runs: Vec<(usize, usize)> = Vec::new();
    let mut beg: Option<usize> = None;
    let mut count = 0usize;
    for (i, &cur) in sub.iter().enumerate() {
        if cur == from {
            if beg.is_none() {
                beg = Some(i);
            }
            count += 1;
        } else {
            if let Some(b) = beg
                && count < MIN_DATA_SIZE
            {
                short_runs.push((b, i));
            }
            beg = None;
            count = 0;
        }
    }
    for (b, e) in short_runs {
        for cell in sub[b..e].iter_mut() {
            *cell = to;
        }
    }
}

// ── Public entry points ──────────────────────────────────────────────────────

/// The conventions [`compute_hca`] pins; the harness varies them.
#[derive(Debug, Clone, Copy)]
pub(crate) struct HcaOptions {
    /// `isdp_apply = YES`: seed from the volume estimate before the RDA header value.
    pub(crate) isdp_estimated: bool,
    /// Reproduce the 8-bit moment transport between tasks (the primary).
    pub(crate) quantize_transport: bool,
    /// The met-signal meteorological flag (`metsignal_processing = ON`, the
    /// fleet default). Off is the legacy pre-B17 ρ/SNR flag.
    pub(crate) metsignal: bool,
}

impl HcaOptions {
    pub(crate) const fn primary() -> Self {
        Self {
            isdp_estimated: false,
            quantize_transport: true,
            metsignal: true,
        }
    }
}

/// The derived hydrometeor classification for one tilt, at the recombined
/// radials' native geometry.
pub struct DerivedHca {
    /// `[radial][gate]`, the product's external class codes (10–140);
    /// `NaN` where the gate is no-echo/undefined (external code 0).
    pub values: Vec<Vec<f32>>,
    pub azimuths_deg: Vec<f64>,
    pub first_gate_km: f64,
    pub gate_interval_km: f64,
    pub radial_width_deg: f64,
    pub init_fdp_deg: f64,
}

impl DerivedHca {
    /// Resample onto the 360° × 230 km comparison grid, cell for cell the
    /// way the twin comparator resamples the Level III product.
    pub fn to_polar_grid(&self) -> Vec<Vec<f32>> {
        resample_to_polar_grid(
            &self.values,
            &self.azimuths_deg,
            self.first_gate_km,
            self.gate_interval_km,
            self.radial_width_deg,
        )
    }
}

/// The `init_fdp` the pipeline seeds with — the same resolution the KDP
/// chain validated: the RDA header value, else the volume estimate; the
/// `isdp_apply = YES` variant prefers the estimate.
pub(crate) fn resolve_init_fdp(
    params: &KdpParams,
    combined: &[DpCombined],
    estimated: bool,
) -> f64 {
    let estimate = || {
        let mut queue: Vec<f64> = Vec::new();
        for c in combined {
            if queue.len() >= crate::dpprep::ISDP_MAX_QUEUE {
                break;
            }
            if let Some(p) = radial_system_phi(&c.base.phi, &c.base.rho, &c.base.z) {
                queue.push(p);
            }
        }
        isdp_from_queue(queue)
    };
    if estimated {
        params
            .isdp_est_deg
            .map(f64::from)
            .or_else(estimate)
            .or(params.init_fdp_deg.map(f64::from))
            .unwrap_or(0.0)
    } else {
        params
            .init_fdp_deg
            .map(f64::from)
            .or_else(estimate)
            .unwrap_or(0.0)
    }
}

/// Compute the tilt's hydrometeor classification: recombine the sweep to 1°, run the
/// dpprep and QIA chains, classify every gate against the melting layer, subclass RH by
/// hail size, emit external class codes.
pub fn compute_hca(
    radials: &[Radial],
    params: &KdpParams,
    ml: &MeltingLayer,
    hsda: &HsdaHeights,
    cappi: Option<&ReflCappi>,
) -> Option<DerivedHca> {
    compute_hca_impl(radials, params, ml, hsda, cappi, HcaOptions::primary())
}

/// Build the volume's reflectivity CAPPI from its ≥ 1° dual-pol sweeps.
pub fn build_refl_cappi(sweeps: &[&[Radial]]) -> ReflCappi {
    let mut cappi = ReflCappi::new();
    for &radials in sweeps {
        let inputs: Vec<DpInput> = radials.iter().filter_map(DpInput::from_radial).collect();
        if inputs.is_empty() {
            continue;
        }
        let combined = combine_sweep_dp(&inputs, true);
        for c in &combined {
            cappi.update_radial(c.elev, c.base.az, c.base.zr0, c.base.zg, &c.base.z);
        }
    }
    cappi
}

fn compute_hca_impl(
    radials: &[Radial],
    params: &KdpParams,
    ml: &MeltingLayer,
    hsda: &HsdaHeights,
    cappi: Option<&ReflCappi>,
    opts: HcaOptions,
) -> Option<DerivedHca> {
    let inputs: Vec<DpInput> = radials.iter().filter_map(DpInput::from_radial).collect();
    if inputs.is_empty() {
        return None;
    }
    let radial_width_deg = if inputs[0].half_degree {
        1.0
    } else {
        inputs[0].spacing
    };
    let combined = combine_sweep_dp(&inputs, true);
    let init_fdp = resolve_init_fdp(params, &combined, opts.isdp_estimated);

    let geometry = combined.iter().find(|c| !c.base.phi.is_empty())?;
    let first_gate_km = geometry.base.dr0;
    let gate_interval_km = geometry.base.dg;

    let dbz0 = params.dbz0.map(f64::from);
    let atmos = params.atmos_db_per_km.map(f64::from);

    // Per-radial and pure: nothing is summed across radials, so no float is
    // reassociated and the parallel product is the serial one gate for gate.
    let (values, azimuths): (Vec<Vec<f32>>, Vec<f64>) = combined
        .par_iter()
        .map(|c| {
            let fields = radial_fields(
                c,
                init_fdp,
                dbz0,
                atmos,
                opts.quantize_transport,
                opts.metsignal,
                cappi,
            );
            let classes = classify_radial(&fields, ml, hsda.tw0_km_arl);
            let sub = if ENABLE_SIZE {
                hail_size_radial(&fields, &classes, hsda)
            } else {
                vec![HailSize::NotHail; classes.len()]
            };
            let row: Vec<f32> = classes
                .iter()
                .zip(sub.iter())
                .map(|(&cl, &s)| external_code(cl, s))
                .collect();
            (row, c.base.az)
        })
        .collect::<Vec<_>>()
        .into_iter()
        .unzip();

    Some(DerivedHca {
        values,
        azimuths_deg: azimuths,
        first_gate_km,
        gate_interval_km,
        radial_width_deg,
        init_fdp_deg: init_fdp,
    })
}

/// Rebuild a split cut the way the RPG's combined base data stream feeds
/// dpprep/HCA: the surveillance cut's Z and dual-pol moments with the
/// Doppler cut's velocity and spectrum width grafted in by nearest azimuth.
pub fn merge_split_cut_doppler(surveillance: &[Radial], doppler: &[Radial]) -> Vec<Radial> {
    let dop: Vec<(f64, &Radial)> = doppler
        .iter()
        .filter(|r| r.velocity().is_some())
        .map(|r| (f64::from(r.azimuth_angle_degrees()), r))
        .collect();
    let circ = |a: f64, b: f64| -> f64 {
        let mut d = (a - b).rem_euclid(360.0);
        if d > 180.0 {
            d = 360.0 - d;
        }
        d
    };
    surveillance
        .iter()
        .map(|cs| {
            if cs.velocity().is_some() || dop.is_empty() {
                return cs.clone();
            }
            let az = f64::from(cs.azimuth_angle_degrees());
            let partner = dop
                .iter()
                .min_by(|(a, _), (b, _)| circ(*a, az).total_cmp(&circ(*b, az)))
                .filter(|(a, _)| circ(*a, az) <= 0.5 * f64::from(cs.azimuth_spacing_degrees()))
                .map(|(_, r)| *r);
            let Some(cd) = partner else {
                return cs.clone();
            };
            Radial::new(
                cs.collection_timestamp(),
                cs.azimuth_number(),
                cs.azimuth_angle_degrees(),
                cs.azimuth_spacing_degrees(),
                cs.radial_status(),
                cs.elevation_number(),
                cs.elevation_angle_degrees(),
                cs.reflectivity().cloned(),
                cd.velocity().cloned(),
                cd.spectrum_width().cloned(),
                cs.differential_reflectivity().cloned(),
                cs.differential_phase().cloned(),
                cs.correlation_coefficient().cloned(),
                None,
            )
        })
        .collect()
}

// ── Melting layer detection (cpc023/tsk003, melting_layer.c) ─────────────────

/// `Compute_height_from_range`: beam height above the radar, km, on the
/// `IR·RE` model.
fn ml_height_from_range(elev_deg: f64, range_km: f64) -> f64 {
    let s = elev_deg.to_radians().sin();
    range_km * s + range_km * range_km / (2.0 * ML_IR * ML_RE_KM)
}

/// `Compute_range_from_height`, its inverse.
fn ml_range_from_height(elev_deg: f64, height_km: f64) -> f64 {
    let s = elev_deg.to_radians().sin();
    ML_IR * ML_RE_KM * ((s * s + 2.0 * height_km / (ML_IR * ML_RE_KM)).sqrt() - s)
}

/// `Compute_elev_weight`: the gate-count × reliability weighting of a
/// detection at `elev`.
fn ml_elev_weight(elev_deg: f64) -> f64 {
    let gate_ratio = 0.36 * elev_deg - 0.56;
    let acc_ratio = 1.0 - (ML_UPPER_ELEV - elev_deg) / ML_UPPER_ELEV;
    gate_ratio * acc_ratio
}

/// Detect the melting layer from one volume's 4°–10° tilts per `melting_layer.c`
/// (Giangrande, Krause, Ryzhkov 2008).
pub fn detect_melting_layer(
    sweeps: &[&[Radial]],
    params: &KdpParams,
    default: &MeltingLayer,
    hsda: &HsdaHeights,
    cappi: Option<&ReflCappi>,
) -> MeltingLayer {
    detect_melting_layer_impl(sweeps, params, default, hsda, cappi, HcaOptions::primary())
}

fn detect_melting_layer_impl(
    sweeps: &[&[Radial]],
    params: &KdpParams,
    default: &MeltingLayer,
    hsda: &HsdaHeights,
    cappi: Option<&ReflCappi>,
    opts: HcaOptions,
) -> MeltingLayer {
    let default_top_km_arl = mean(&default.top_km_arl);
    let dbz0 = params.dbz0.map(f64::from);
    let atmos = params.atmos_db_per_km.map(f64::from);

    let mut weight = vec![[0.0f64; ML_MAX_HEIGHTS]; 360];
    for &radials in sweeps {
        let inputs: Vec<DpInput> = radials.iter().filter_map(DpInput::from_radial).collect();
        if inputs.is_empty() {
            continue;
        }
        let sweep_elev = inputs[0].elev;
        if !(ML_LOWER_ELEV..=ML_UPPER_ELEV).contains(&sweep_elev) {
            continue;
        }
        let combined = combine_sweep_dp(&inputs, true);
        let init_fdp = resolve_init_fdp(params, &combined, opts.isdp_estimated);
        let elev_weight = ml_elev_weight(sweep_elev);

        // Which heights each radial votes for is found in parallel; the votes
        // are cast in order, because `weight` is a float accumulator and
        // several radials round to the same whole degree.
        let votes: Vec<(usize, Vec<usize>)> = combined
            .par_iter()
            .map(|c| {
                let f = radial_fields(
                    c,
                    init_fdp,
                    dbz0,
                    atmos,
                    opts.quantize_transport,
                    opts.metsignal,
                    cappi,
                );
                let classes = classify_radial(&f, default, hsda.tw0_km_arl);
                let stop = (ml_range_from_height(c.elev, ML_MAX_TOP_KM) / f.dg + 0.5) as usize;
                let az_index = (f.az.rem_euclid(360.0)) as usize % 360;
                let mut heights = Vec::new();
                for (i, &class) in classes.iter().enumerate().take(f.n.min(stop)) {
                    if class == GC || class == BI || class == UK || class == NE {
                        continue;
                    }
                    if f.snr[i] <= ML_MIN_SNR {
                        continue;
                    }
                    if !(f.smz[i] > ML_LOWER_Z
                        && f.smz[i] < ML_UPPER_Z
                        && f.rho[i] > ML_LOWER_RHO
                        && f.rho[i] < ML_UPPER_RHO)
                    {
                        continue;
                    }
                    let height_index = (ml_height_from_range(c.elev, i as f64 * f.dg)
                        / ML_HEIGHT_INTERVAL_KM
                        + 0.5) as usize;
                    if height_index >= ML_MAX_HEIGHTS {
                        continue;
                    }
                    // Search up to 0.5 km above this gate for the Z and ZDR
                    // maxima that fingerprint wet snow.
                    let temp_height = ML_DEPTH_KM + ml_height_from_range(c.elev, i as f64 * f.dg);
                    let range_index = ((ml_range_from_height(c.elev, temp_height) / f.dg + 0.5)
                        as usize)
                        .min(f.n);
                    let (mut zmax, mut zdrmax) = (-1000.0f64, -1000.0f64);
                    let (mut zmax_i, mut zdrmax_i) = (i, i);
                    for j in i..range_index {
                        if f.snr[j] > ML_MIN_SNR {
                            if zmax < f.smz[j] {
                                zmax = f.smz[j];
                                zmax_i = j;
                            }
                            if zdrmax < f.zdr[j] {
                                zdrmax = f.zdr[j];
                                zdrmax_i = j;
                            }
                        }
                    }
                    if zmax > ML_LOWER_ZMAX
                        && zmax < ML_UPPER_ZMAX
                        && f.rho[zmax_i] > ML_LOW_RHO_PROFILE
                        && zdrmax > ML_LOWER_ZDRMAX
                        && zdrmax < ML_UPPER_ZDRMAX
                        && f.rho[zdrmax_i] > ML_LOW_RHO_PROFILE
                    {
                        heights.push(height_index);
                    }
                }
                (az_index, heights)
            })
            .collect();

        for (az_index, heights) in votes {
            for height_index in heights {
                weight[az_index][height_index] += 1.0 + elev_weight;
            }
        }
    }

    calculate_melting_layer(&weight, default_top_km_arl, default)
}

/// `Calculate_melting_layer`'s radar-only path over one accumulation of
/// wet-snow weights: the ±10° azimuth sums, the ±(2·depth) clip around the
/// previous top (the default top here — first-volume state), the 20th/80th
/// percentile bottom/top, gap interpolation around the circle.
fn calculate_melting_layer(
    weight: &[[f64; ML_MAX_HEIGHTS]],
    last_avg_top: f64,
    default: &MeltingLayer,
) -> MeltingLayer {
    let mut top = [f64::NAN; 360];
    let mut bottom = [f64::NAN; 360];

    let clip_high = ((last_avg_top + 2.0 * ML_DEPTH_KM) / ML_HEIGHT_INTERVAL_KM + 0.5) as i64;
    let clip_low = ((last_avg_top - 2.0 * ML_DEPTH_KM) / ML_HEIGHT_INTERVAL_KM + 0.5) as i64;

    for az in 0..360usize {
        let mut sum_heights = [0.0f64; ML_MAX_HEIGHTS];
        for d in -(ML_HALF_WINDOW as i64)..=(ML_HALF_WINDOW as i64) {
            let j = (az as i64 + d).rem_euclid(360) as usize;
            for (k, s) in sum_heights.iter_mut().enumerate() {
                *s += weight[j][k];
            }
        }
        // Zero out heights more than 2·depth from the previous top.
        for (k, s) in sum_heights.iter_mut().enumerate() {
            if (k as i64) < clip_low || (k as i64) > clip_high {
                *s = 0.0;
            }
        }
        let total: f64 = sum_heights.iter().sum();
        if total <= ML_MIN_WET_SNOW_SUM {
            continue;
        }
        let mut running = 0.0;
        let (mut low_index, mut high_index) = (-1i64, -1i64);
        for (k, &s) in sum_heights.iter().enumerate() {
            running += s;
            let statistic = running / total;
            if statistic > ML_LOW_PERCENTILE && low_index == -1 {
                low_index = k as i64;
            }
            if statistic > ML_HIGH_PERCENTILE && high_index == -1 {
                high_index = k as i64;
            }
            if low_index > 0 && high_index > 0 {
                break;
            }
        }
        top[az] = high_index as f64 * ML_HEIGHT_INTERVAL_KM + 0.05;
        bottom[az] = low_index as f64 * ML_HEIGHT_INTERVAL_KM + 0.05;
    }

    let valid: Vec<usize> = (0..360).filter(|&i| !top[i].is_nan()).collect();
    if valid.len() < 2 {
        // No radar detection (or a degenerate single azimuth): the default
        // flat layer, as the source's `ML_not_found` path sends.
        return default.clone();
    }

    // Fill the gaps by linear interpolation between the bracketing valid
    // azimuths, around the circle — the source's Valid_radar_index walk.
    let mut out_top = top;
    let mut out_bottom = bottom;
    for w in 0..valid.len() {
        let a = valid[w];
        let b = valid[(w + 1) % valid.len()];
        let span = ((b as i64 - a as i64).rem_euclid(360)) as usize;
        if span <= 1 {
            continue;
        }
        for step in 1..span {
            let az = (a + step) % 360;
            let t = step as f64 / span as f64;
            out_top[az] = top[a] * (1.0 - t) + top[b] * t;
            out_bottom[az] = bottom[a] * (1.0 - t) + bottom[b] * t;
        }
    }

    MeltingLayer {
        top_km_arl: out_top,
        bottom_km_arl: out_bottom,
        source: MeltingLayerSource::RadarDetected,
    }
}

/// Resolve one volume's melting layer, best source first, and say which one
/// answered.
pub fn resolve_melting_layer(
    rpg_melting_layer: Option<&nexrad_level3::model::Level3Message>,
    ml_sweeps: &[&[Radial]],
    params: &KdpParams,
    sounding_h0c_km_msl: Option<f64>,
    radar_km_msl: f64,
    hsda: &HsdaHeights,
    cappi: Option<&ReflCappi>,
) -> MeltingLayer {
    if let Some(message) = rpg_melting_layer {
        match MeltingLayer::from_melting_layer_product(message) {
            Some(recovery) if recovery.looks_sound() => {
                log::info!(
                    "Melting layer from the RPG's own product 166: mean top {:.3} km ARL, \
                     depth {:.3} km (algorithm draws {ML_DEPTH_KM}), self-consistency {:.3} km",
                    mean(&recovery.layer.top_km_arl),
                    recovery.depth_km,
                    recovery.consistency_km,
                );
                return recovery.layer;
            }
            Some(recovery) => log::warn!(
                "Melting layer product 166 inverted to a {:.3} km layer with {:.3} km of \
                 self-inconsistency; refusing it and falling back",
                recovery.depth_km,
                recovery.consistency_km,
            ),
            None => log::warn!("Melting layer product 166 carried no usable contours"),
        }
    }

    let seed = match sounding_h0c_km_msl {
        Some(h0c) => MeltingLayer::from_zero_c_height(h0c, radar_km_msl),
        None => MeltingLayer::flat(DEFAULT_HEIGHT_0_KM_MSL - radar_km_msl),
    };
    let detected = detect_melting_layer(ml_sweeps, params, &seed, hsda, cappi);
    if detected.source == MeltingLayerSource::RadarDetected {
        log::info!(
            "Melting layer detected from this volume's own 4-10 deg tilts: mean top {:.3} km ARL",
            mean(&detected.top_km_arl),
        );
        return detected;
    }
    if seed.source == MeltingLayerSource::FleetDefault {
        log::warn!(
            "No melting layer for this volume from the RPG, the radar or a sounding: \
             classifying on the {:.1} kft fleet adaptation default, which the twin campaign \
             measured 2.9-3.1 km wrong in winter regimes (16-20% exact against N0H)",
            DEFAULT_HEIGHT_0_KM_MSL / 0.3048,
        );
    }
    seed
}

#[cfg(test)]
mod tests;
