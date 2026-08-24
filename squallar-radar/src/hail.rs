//! POSH and MEHS — the WSR-88D Hail Detection Algorithm's severe-hail pair,
//! computed locally from the Level II reflectivity volume as **gridded**
//! fields.

use crate::sounding::EnvHeights;
use crate::types::RadarProduct;
use crate::volumetric::{
    CellStat, DedupPolicy, RANGE_BINS, RangeBinning, VolumeCube, VolumetricGrid,
    sweep_elevation_deg,
};
use nexrad_model::data::Scan;

/// Ė's multiplicative coefficient, J m⁻² s⁻¹ (Witt et al. 1998 Eq. 4). The
/// ORPG's `hke_coef1 = 0.0005` is this times the SHI integral's 0.1 with the
/// depth in km instead of m — identical arithmetic, different factoring.
pub const HKE_FLUX_COEF: f64 = 5.0e-6;

/// Ė's exponential coefficient per dBZ (Eq. 4; ORPG `hke_coef2`).
pub const HKE_FLUX_EXP_PER_DBZ: f64 = 0.084;

/// `W(Z)`'s lower ramp limit `Z_L`, dBZ (Eq. 2; ORPG `hke_ref_wgt_low`).
pub const HKE_REF_WGT_LOW_DBZ: f64 = 40.0;

/// `W(Z)`'s upper ramp limit `Z_U`, dBZ (Eq. 2; ORPG `hke_ref_wgt_high`).
pub const HKE_REF_WGT_HIGH_DBZ: f64 = 50.0;

/// The SHI integral's leading coefficient (Eq. 7).
pub const SHI_COEF: f64 = 0.1;

/// Warning threshold slope, J m⁻¹ s⁻¹ per km of `H₀` ARL (Eq. 8; ORPG
/// `warn_thr_sel_mod_coef`).
pub const WT_COEF_PER_KM: f64 = 57.5;

/// Warning threshold offset for a site the RPG's own table does not list,
/// J m⁻¹ s⁻¹ — `hail.alg`'s `warn_thr_sel_mod_off` entry `Other_sites: -96.3`.
pub const WT_OFFSET_OTHER_SITES: f64 = -96.3;

/// The warning threshold's floor, J m⁻¹ s⁻¹ (`a31599.ftn`; WDTD states it
/// too).
pub const WT_FLOOR: f64 = 20.0;

/// POSH's log coefficient, % (Eq. 9; ORPG `posh_coef`).
pub const POSH_COEF: f64 = 29.0;

/// POSH's offset, % (Eq. 9; ORPG `posh_offset`) — the value at `SHI = WT`.
pub const POSH_OFFSET_PCT: f64 = 50.0;

/// MEHS's coefficient in **mm** (Eq. 10). The ORPG's `shi_hail_size_coef =
/// 0.10` is the same number in inches: 0.10 in × 25.4 mm/in = 2.54 mm.
pub const MEHS_COEF_MM: f64 = 2.54;

/// MEHS's exponent (Eq. 10; ORPG `shi_hail_size_exp`).
pub const MEHS_EXP: f64 = 0.5;

/// [`HKE_FLUX_EXP_PER_DBZ`] rebased on two: `10^(0.084·Z)` is `2^(0.084·log₂10·Z)`
/// exactly, and `exp2` is one libm fast path where a runtime-base `powf` is
/// libm's generic `exp(y·ln x)`.
const HKE_FLUX_EXP2_PER_DBZ: f64 = HKE_FLUX_EXP_PER_DBZ * std::f64::consts::LOG2_10;

/// One foot in kilometres, exactly — the `a31599.ftn` MSL→ARL conversion's
/// `FT_TO_KM`.
const FT_TO_KM: f64 = 0.0003048;

const M_PER_KM: f64 = 1000.0;

/// `W(Z)`: the reflectivity weighting ramp of Eq. 2, 0 at or below
/// [`HKE_REF_WGT_LOW_DBZ`], 1 at or above [`HKE_REF_WGT_HIGH_DBZ`].
pub fn refl_weight(dbz: f64) -> f64 {
    ((dbz - HKE_REF_WGT_LOW_DBZ) / (HKE_REF_WGT_HIGH_DBZ - HKE_REF_WGT_LOW_DBZ)).clamp(0.0, 1.0)
}

/// Ė: the hail kinetic energy flux of Eq. 4, J m⁻² s⁻¹, `W(Z)` included —
/// zero at or below 40 dBZ.
pub fn hail_kinetic_energy_flux(dbz: f64) -> f64 {
    if dbz <= HKE_REF_WGT_LOW_DBZ {
        return 0.0;
    }
    HKE_FLUX_COEF * (HKE_FLUX_EXP2_PER_DBZ * dbz).exp2() * refl_weight(dbz)
}

/// `W_T(H)`: the temperature-based height weighting of Eq. 6, on heights in
/// km **ARL**. A degenerate environment (`H_m20 ≤ H₀`, which the `a31599`
/// clamp can produce in winter when both floor at 0 ARL) steps from 0 to 1
/// at `H₀` rather than dividing by zero.
pub fn temp_weight(h_km_arl: f64, h0_km_arl: f64, hm20_km_arl: f64) -> f64 {
    let denom = hm20_km_arl - h0_km_arl;
    if denom > 0.0 {
        ((h_km_arl - h0_km_arl) / denom).clamp(0.0, 1.0)
    } else if h_km_arl >= h0_km_arl {
        1.0
    } else {
        0.0
    }
}

/// The warning threshold of Eq. 8 under an **explicit** site offset:
/// `57.5·H₀ + offset`, floored at 20 J m⁻¹ s⁻¹, `H₀` in km ARL.
pub fn warning_threshold_with_offset(h0_km_arl: f64, offset: f64) -> f64 {
    (WT_COEF_PER_KM * h0_km_arl + offset).max(WT_FLOOR)
}

/// The warning threshold of Eq. 8 for a site whose offset is unknown, using
/// the RPG's own unlisted-site fallback [`WT_OFFSET_OTHER_SITES`].
pub fn warning_threshold(h0_km_arl: f64) -> f64 {
    warning_threshold_with_offset(h0_km_arl, WT_OFFSET_OTHER_SITES)
}

/// POSH (Eq. 9), %: `29·ln(SHI/WT) + 50`, clamped to [0, 100]. A
/// non-positive SHI is 0 % — the source computes POSH only for `SHI > 0`
/// and leaves it at its zero initialisation otherwise (`a31559.ftn`).
pub fn posh_pct(shi: f64, warning_threshold: f64) -> f64 {
    if shi <= 0.0 || warning_threshold <= 0.0 {
        return 0.0;
    }
    (POSH_COEF * (shi / warning_threshold).ln() + POSH_OFFSET_PCT).clamp(0.0, 100.0)
}

/// MEHS (Eq. 10), **mm**: `2.54·SHI^0.5`, 0 for a non-positive SHI. No cap:
/// the cell product's `> 4.00 in` flag is a display encoding.
pub fn mehs_mm(shi: f64) -> f64 {
    if shi <= 0.0 {
        return 0.0;
    }
    MEHS_COEF_MM * shi.powf(MEHS_EXP)
}

/// [`EnvHeights`] (km **MSL**, from Open-Meteo) resolved to km **ARL**
/// against the radar's height in feet MSL — *above radar level* meaning above
/// the antenna, so `radar_height_ft` is the feedhorn and not the ground under
/// the tower.
pub fn env_arl_km(env: &EnvHeights, radar_height_ft: f64) -> (f64, f64) {
    let site_km = radar_height_ft * FT_TO_KM;
    (
        (env.h0c_km_msl - site_km).max(0.0),
        (env.hm20c_km_msl - site_km).max(0.0),
    )
}

/// The derived hail fields, each a 360° × 230 km polar grid. Defined
/// (finite) wherever the column carries any valid reflectivity; a defined
/// 0 where it carries no hail signal.
pub struct HailGrids {
    /// Severe hail index, J m⁻¹ s⁻¹ — the predictor both products derive
    /// from. Not a display product; the live harness scores it under
    /// site-tuned warning thresholds.
    pub shi: VolumetricGrid,
    /// Probability of severe hail, %.
    pub posh: VolumetricGrid,
    /// Maximum expected hail size, mm.
    pub mehs_mm: VolumetricGrid,
}

/// Per tilt of the ascending elevation ladder, the layer's (bottom, top)
/// heights in km ARL at every range cell centre: midpoints of adjacent
/// 4/3-model beam-centre heights, ground below the lowest, the half-power
/// upper flank above the highest. Empty for an empty ladder.
fn layer_bounds_km(elevs_deg: &[f64], half_power_beamwidth_deg: f64) -> Vec<Vec<(f64, f64)>> {
    let n = elevs_deg.len();
    let centre = |e: f64| -> Vec<f64> {
        (0..RANGE_BINS)
            .map(|r| crate::beam::height_at_ground_km(r as f64 + 0.5, e))
            .collect()
    };
    let centres: Vec<Vec<f64>> = elevs_deg.iter().map(|&e| centre(e)).collect();
    let flank: Vec<f64> = match elevs_deg.last() {
        Some(&top) => centre(top + half_power_beamwidth_deg / 2.0),
        None => return Vec::new(),
    };
    (0..n)
        .map(|i| {
            (0..RANGE_BINS)
                .map(|r| {
                    let b = if i == 0 {
                        0.0
                    } else {
                        (centres[i - 1][r] + centres[i][r]) / 2.0
                    };
                    let t = if i + 1 == n {
                        flank[r]
                    } else {
                        (centres[i][r] + centres[i + 1][r]) / 2.0
                    };
                    (b, t)
                })
                .collect()
        })
        .collect()
}

/// One layer's SHI contribution: the part of `[bottom, top]` above `H₀`,
/// weighted by `W_T` at the clipped layer's midpoint — `a31539.ftn`'s
/// `DH_POSH`/`MED_HT` freezing-level handling, in the paper's units.
fn layer_shi(dbz: f64, bottom_km: f64, top_km: f64, h0_km: f64, hm20_km: f64) -> f64 {
    let edot = hail_kinetic_energy_flux(dbz);
    if edot <= 0.0 {
        return 0.0;
    }
    let clip = bottom_km.max(h0_km);
    if top_km <= clip {
        return 0.0;
    }
    let median = (top_km + clip) / 2.0;
    SHI_COEF * temp_weight(median, h0_km, hm20_km) * edot * (top_km - clip) * M_PER_KM
}

/// Compute the gridded SHI/POSH/MEHS fields per the rules in the module doc.
pub fn compute_hail(
    scan: &Scan,
    env: Option<&EnvHeights>,
    radar_height_ft: f64,
    half_power_beamwidth_deg: f64,
) -> Option<HailGrids> {
    let env = env?;
    let (h0_km, hm20_km) = env_arl_km(env, radar_height_ft);
    let wt = warning_threshold(h0_km);

    let cube = VolumeCube::build_with_stats(
        scan,
        &[(RadarProduct::Reflectivity, CellStat::LinearZMean)],
        DedupPolicy::FirstOfVolume,
        RangeBinning::Ground,
    );

    // The tilts carrying reflectivity, ascending, each at its *actual*
    // elevation — the sweep's median radial angle, as in `eet` and `vil`.
    let tilts: Vec<(f64, &Vec<Vec<f32>>)> = cube
        .tilts
        .iter()
        .enumerate()
        .filter_map(|(ti, tilt)| {
            let grid = cube.grid(ti, RadarProduct::Reflectivity)?;
            let elev = scan
                .sweeps()
                .get(grid.sweep_index)
                .and_then(|s| sweep_elevation_deg(s.radials()))
                .unwrap_or(tilt.elevation_deg);
            Some((elev, &grid.values))
        })
        .collect();
    let elevs: Vec<f64> = tilts.iter().map(|&(e, _)| e).collect();
    let bounds = layer_bounds_km(&elevs, half_power_beamwidth_deg);

    let mut shi_grid = vec![vec![f32::NAN; RANGE_BINS]; 360];
    let mut posh_grid = vec![vec![f32::NAN; RANGE_BINS]; 360];
    let mut mehs_grid = vec![vec![f32::NAN; RANGE_BINS]; 360];
    for az in 0..360 {
        for r in 0..RANGE_BINS {
            let mut shi = 0.0f64;
            let mut any_valid = false;
            for (ti, &(_, dbz)) in tilts.iter().enumerate() {
                let z = dbz[az][r];
                if z.is_nan() {
                    continue;
                }
                any_valid = true;
                let (b, t) = bounds[ti][r];
                shi += layer_shi(f64::from(z), b, t, h0_km, hm20_km);
            }
            if any_valid {
                shi_grid[az][r] = shi as f32;
                posh_grid[az][r] = posh_pct(shi, wt) as f32;
                mehs_grid[az][r] = mehs_mm(shi) as f32;
            }
        }
    }

    let grid = |values| VolumetricGrid {
        values,
        range_bins: RANGE_BINS,
    };
    Some(HailGrids {
        shi: grid(shi_grid),
        posh: grid(posh_grid),
        mehs_mm: grid(mehs_grid),
    })
}

#[cfg(test)]
mod tests;
