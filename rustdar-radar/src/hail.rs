//! POSH and MEHS — the WSR-88D Hail Detection Algorithm's severe-hail pair,
//! computed locally from the Level II reflectivity volume as **gridded**
//! fields.
//!
//! # What is implemented, and from which documents
//!
//! **Primary citation** — Witt, Eilts, Stumpf, Johnson, Mitchell and Thomas
//! (1998), *An Enhanced Hail Detection Algorithm for the WSR-88D*, Weather
//! and Forecasting **13**, 286–303. Every formula below is the paper's:
//!
//! * **Hail kinetic energy flux** (their Eq. 4, J m⁻² s⁻¹):
//!
//!   ```text
//!   Ė = 5×10⁻⁶ · 10^(0.084·Z) · W(Z)
//!   ```
//!
//!   with `W(Z)` the reflectivity weighting ramp (Eq. 2): 0 at or below
//!   `Z_L = 40` dBZ, `(Z − Z_L)/(Z_U − Z_L)` between, 1 at or above
//!   `Z_U = 50` dBZ — hail-sized scatterers dominate the returned power only
//!   above the ramp.
//!
//! * **Temperature-based height weighting** (Eq. 6): 0 at or below the 0 °C
//!   height `H_0`, `(H − H_0)/(H_m20 − H_0)` between, 1 at or above the
//!   −20 °C height `H_m20`. Both heights are **km above radar level (ARL)**
//!   — the RPG converts its MSL adaptation heights to ARL before use
//!   (`a31599.ftn`), and the NWS WDTD training pages state ARL explicitly.
//!
//! * **Severe hail index** (Eq. 7, J m⁻¹ s⁻¹):
//!
//!   ```text
//!   SHI = 0.1 · ∫ W_T(H) · Ė dH        (H₀ up to the storm top, H in metres)
//!   ```
//!
//! * **Warning threshold** (Eq. 8): `WT = 57.5·H₀ − 121` with `H₀` in km
//!   ARL, floored at 20 J m⁻¹ s⁻¹.
//!
//! * **POSH** (Eq. 9): `29·ln(SHI/WT) + 50`, clamped to [0, 100] % —
//!   `SHI = WT` reads exactly 50 %.
//!
//! * **MEHS** (Eq. 10): `2.54·SHI^0.5` in **mm** (the grid carries mm;
//!   display converts to inches).
//!
//! **Source cross-check** — the released ORPG hail task (`cpc015/tsk009`,
//! `a31509`–`a31599.ftn`, with `hail_algorithm.h` and the `hail.alg`
//! adaptation defaults, from the public CODE mirror `likev/CodeOrpgPub`)
//! confirms every constant, in its own units:
//!
//! * `hke_coef1 = 0.0005`, `hke_coef2 = 0.084`, `hke_coef3 = 10.0`
//!   (`a31539.ftn`: `HKE = HKE_COF1·REF_WF·(10^0.084)^Z`, summed as
//!   `HKE·ΔH_km·W_T`). `0.0005` per **km** of depth is exactly the paper's
//!   `0.1 × 5×10⁻⁶` per **metre**, so the two agree identically.
//! * `warn_thr_sel_mod_coef = 57.5`, default offset `−121.0`, and the
//!   hard floor `IF (WT .LT. 20.) WT = 20.` (`a31599.ftn`).
//! * `posh_coef = 29.0`, `posh_offset = 50` (`a31559.ftn`, applied ×0.1 and
//!   re-multiplied by 10 to round the *output* to the nearest 10 %).
//! * `shi_hail_size_coef = 0.10`, exponent `0.5` — hail size in **inches**;
//!   `0.10 in ≡ 2.54 mm`, so it is the paper's `2.54·SHI^0.5` mm exactly.
//! * MSL→ARL: `HT0_ARL = (HT0_MSL·1000 ft − radar height ft)·FT_TO_KM`,
//!   clamped at 0 below (`a31599.ftn`) — reproduced by [`env_arl_km`].
//!
//! **Where the released source differs from the paper** (noted per the
//! campaign convention; none changes the arithmetic here):
//!
//! * The fleet's `hail.alg` carries a **per-site WT offset table**
//!   (KDDC −74.2, KFSD −94.8, … full range −119.5 to +55.2) in place of the
//!   paper's single −121. The *default* is still −121.0
//!   (`hail_algorithm.h`), and that is what this module ships; the live
//!   harness parses each site's actual offset out of the NHI product's own
//!   adaptation page and scores against the site-tuned value too.
//! * The operational POSH is **rounded to the nearest 10 %** and MEHS to the
//!   nearest **¼ inch**, with sizes above 4 in flagged and displayed as
//!   `> 4.00` (`a31559.ftn`, `a31644.ftn`). Those are cell-product display
//!   encodings, not physics; the gridded fields here stay continuous and
//!   uncapped, and the harness's tolerances absorb the quantisation.
//! * The cell code only integrates a component whose centre sits below `H₀`
//!   when the freezing level is in the component's top half **and** the next
//!   component up also exceeds 40 dBZ (`a31529.ftn`) — a component-stack
//!   continuity gate with no analogue in the paper's integral. Not
//!   reproduced: the grid integrates every layer part above `H₀`, as the
//!   paper writes it.
//! * POH (probability of any hail) and the SCIT cell bookkeeping are out of
//!   scope: the two products here are POSH and MEHS.
//!
//! # The grid adaptation — and why it is one
//!
//! The RPG's HDA is **cell-based**: SCIT builds storm cells from 2-D
//! components (one per elevation), and the algorithm integrates each cell's
//! per-elevation *maximum* reflectivity up its (possibly tilted) axis. A
//! display product wants a field, not a table — GR2Analyst ships the same
//! quantities as gridded derived products — so this module evaluates the
//! paper's column integral **per 1° × 1 km polar column** of the
//! [`VolumeCube`]:
//!
//! * **Input** is each cell's recombined reflectivity
//!   ([`CellStat::LinearZMean`], the RPG's documented 1° × 1 km
//!   recombination average, as in [`crate::vil`]), on the volume's first
//!   pass ([`DedupPolicy::FirstOfVolume`] — the RPG's volume products never
//!   see SAILS revisits). The cell code integrates a *component maximum*
//!   instead; a column has no component to take a maximum over.
//! * **Column geometry** — all in the crate's 4/3-earth beam model
//!   ([`crate::volumetric`]), at the cell centre `r + 0.5` km: tilt `i`'s
//!   layer runs between the **midpoints of adjacent beam-centre heights**,
//!   the lowest layer starts at the ground, and the highest is capped at the
//!   beam's **half-power upper flank** (+0.475°) — the storm is never
//!   extrapolated past the volume ceiling. The layer straddling `H₀` is
//!   clipped to its part above `H₀`, and `W_T` is evaluated at the clipped
//!   layer's midpoint — exactly the cell code's `DH_POSH`/`MED_HT` handling
//!   of the freezing level (`a31539.ftn`).
//!
//!   This diverges from [`crate::vil`]'s `A313T1` depth table deliberately,
//!   and the divergences are: (1) boundaries are midpoints of 4/3-model
//!   *heights* rather than the tangent-plane `½·RH·(tan φ₊ − tan φ₋)` (equal
//!   to first order; hail needs the boundary *heights* themselves for the
//!   `H₀` clip, and they must live in the same vertical coordinate as `W_T`
//!   or a layer could disagree with its own depth); (2) the top cap uses the
//!   crate's 0.95° half-power beamwidth, not `a313t1.ftn`'s hardcoded
//!   0.017 rad (≈ 0.974°); (3) depths are evaluated at the cell centre
//!   `r + 0.5`, the height-table datum, not the legacy depth table's outer
//!   edge.
//! * **Elevation angles** are each sweep's measured median
//!   ([`crate::volumetric::sweep_elevation_deg`]), as in `eet`/`vil`.
//! * **Definedness**: a column is defined wherever *any* tilt carries valid
//!   reflectivity — a defined 0 for an echo column with no hail signal (the
//!   product-134 convention `vil` documents), `NaN` where nothing was
//!   sampled. With no [`EnvHeights`] there is **no field at all**
//!   ([`compute_hail`] returns `None`): a hail product without an
//!   environment is undefined, not zero.
//!
//! # Validation — read this before trusting any harness
//!
//! **The harnesses and their `validation_policy` (bounds, fixture pins)
//! now live on branch `campaign-harness`.** The figures below are the
//! last measured before the move; re-measuring means that branch.
//!
//! **There is no gridded RPG twin for POSH or MEHS.** The RPG publishes hail
//! only cell-based (product 59, NHI: per-cell values at SCIT centroids), so
//! the campaign's usual per-bin twin bar is *unavailable by construction* —
//! the offline, paper-pinned suite below is the **primary** validation, and
//! the live NHI comparison is a coarse sanity gate, never a bar. The
//! asymmetry is recorded in `validation_policy`, alongside the gate's
//! tolerances and what cell-vs-grid construction differences they absorb.

use crate::sounding::EnvHeights;
use crate::types::RadarProduct;
use crate::volumetric::{
    CellStat, DedupPolicy, HALF_POWER_BEAMWIDTH_DEG, RANGE_BINS, VolumeCube, VolumetricGrid,
    beam_height_km, sweep_elevation_deg,
};
use nexrad_model::data::Scan;

// ── The paper's constants, pinned (see `the_paper_constants_are_pinned`) ────

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

/// Warning threshold offset, J m⁻¹ s⁻¹ (Eq. 8). The paper's −121 and the
/// released source's default; the fleet's per-site `hail.alg` overrides are
/// documented in the module doc and applied only by the live harness.
pub const WT_OFFSET: f64 = -121.0;

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
    HKE_FLUX_COEF * 10f64.powf(HKE_FLUX_EXP_PER_DBZ * dbz) * refl_weight(dbz)
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

/// The warning threshold of Eq. 8: `57.5·H₀ − 121`, floored at 20 J m⁻¹ s⁻¹,
/// `H₀` in km ARL.
pub fn warning_threshold(h0_km_arl: f64) -> f64 {
    (WT_COEF_PER_KM * h0_km_arl + WT_OFFSET).max(WT_FLOOR)
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
/// the tower. `a31599.ftn`'s conversion,
/// including its clamp of negative ARL heights to 0 (a freezing level below
/// the radar reads as *at* the radar, not underground).
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
fn layer_bounds_km(elevs_deg: &[f64]) -> Vec<Vec<(f64, f64)>> {
    let n = elevs_deg.len();
    let centre = |e: f64| -> Vec<f64> {
        (0..RANGE_BINS)
            .map(|r| beam_height_km(r as f64 + 0.5, e))
            .collect()
    };
    let centres: Vec<Vec<f64>> = elevs_deg.iter().map(|&e| centre(e)).collect();
    let flank: Vec<f64> = match elevs_deg.last() {
        Some(&top) => centre(top + HALF_POWER_BEAMWIDTH_DEG / 2.0),
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
///
/// `env` is the per-site environmental sounding; **`None` means there is no
/// field** — the products are undefined without an environment, and the
/// render seam treats that as "nothing to draw", never as a zero-filled
/// grid. `radar_height_ft` is the **feedhorn** height above MSL in feet
/// ([`crate::eet::radar_height_ft_near`] on
/// [`crate::sites::Datum::Feedhorn`] on the render path), the datum that
/// converts the MSL sounding heights to the beam's ARL coordinate — ARL is
/// above the antenna, and the ground under the tower is 30–115 ft lower.
pub fn compute_hail(
    scan: &Scan,
    env: Option<&EnvHeights>,
    radar_height_ft: f64,
) -> Option<HailGrids> {
    let env = env?;
    let (h0_km, hm20_km) = env_arl_km(env, radar_height_ft);
    let wt = warning_threshold(h0_km);

    let cube = VolumeCube::build_with_stats(
        scan,
        &[(RadarProduct::Reflectivity, CellStat::LinearZMean)],
        DedupPolicy::FirstOfVolume,
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
    let bounds = layer_bounds_km(&elevs);

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
