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
/// against the radar's height in feet MSL — `a31599.ftn`'s conversion,
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
/// grid. `radar_height_ft` is the site height above MSL in feet
/// ([`crate::eet::radar_height_ft_near`] on the render path), the datum that
/// converts the MSL sounding heights to the beam's ARL coordinate.
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

/// The parts of [`live_validation`] that decide **what counts as passing**,
/// plus the NHI-product parsing that decision is made on.
///
/// # The validation asymmetry, recorded
///
/// Every other product in this campaign is scored per-bin against the RPG's
/// own gridded rendition of the same volume. **Hail has no gridded RPG
/// twin**: the RPG's only hail product is NHI (product 59), a *cell-based*
/// table of POSH/POH/MEHS at SCIT storm-cell centroids. The offline,
/// paper-pinned suite in `tests` is therefore the **primary** validation of
/// this module, and the live NHI comparison below is a **sanity gate**, not
/// a bar: cell-based and grid-based values differ by construction — SCIT
/// integrates each cell's per-elevation *component maximum* up a possibly
/// tilted axis, associates components across elevations scanned minutes
/// apart, and quantises its output (POSH to 10 %, MEHS to ¼ in, capped at
/// 4 in) — so the gate's tolerances are wide and its verdict is "not
/// insane", never "matches".
///
/// Outside the ignored module for the reason `eet::validation_policy` is:
/// the live harness never runs under `cargo test --workspace`, so anything
/// defined inside it could be quietly weakened without a default-suite test
/// noticing. Out here `policy_tests` reaches all of it offline, and does.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod validation_policy {
    /// A compared cell passes on POSH when the grid value is within this
    /// many points of the cell value.
    pub const POSH_TOLERANCE_PTS: f64 = 20.0;

    /// …and on MEHS when within this many inches.
    pub const MEHS_TOLERANCE_IN: f64 = 0.5;

    /// The gate concludes nothing until this many cells were compared…
    pub const MIN_CELLS: usize = 10;

    /// …across at least this many sites (or site-days).
    pub const MIN_SITES: usize = 2;

    /// Share of compared cells that must pass, per metric, for the gate to
    /// pass. Not 100 %: a SCIT centroid is one point on a cell whose core
    /// tilts and moves between the elevation scans the association spans, so
    /// occasional large misses are construction, not regression. Lowering
    /// this is how a broken derivation ships; it is pinned offline.
    pub const PASS_SHARE_MIN_PCT: f64 = 80.0;

    /// The comparison window around a cell centroid: the grid maximum over
    /// azimuth ± this many degrees…
    pub const WINDOW_AZ_DEG: usize = 2;

    /// …and range ± this many km. A centroid is a cell-mass centre; the
    /// grid's hail core sits wherever the column integral peaks nearby.
    pub const WINDOW_RANGE_KM: usize = 2;

    pub fn gate_is_conclusive(cells: usize, sites: usize) -> bool {
        cells >= MIN_CELLS && sites >= MIN_SITES
    }

    pub fn share_passes(passed: usize, compared: usize) -> bool {
        100.0 * passed as f64 / compared.max(1) as f64 >= PASS_SHARE_MIN_PCT
    }

    // ── NHI product parsing (harness-only) ─────────────────────────────────
    //
    // The vendored `nexrad-level3` decodes the header, PDB and symbology of
    // product 59 (its hail packets, codes 19 and 15, are skipped as unknown),
    // but not the Tabular Alphanumeric Block. The TAB and the storm-ID
    // symbology packet are parsed here, from the raw object bytes, for the
    // harness alone.

    /// One cell row of the NHI TAB ("STORM ID | PROBABILITY OF SEVERE HAIL
    /// (%) | PROBABILITY OF HAIL (%) | MAX EXPECTED HAIL SIZE (IN)",
    /// `a31644.ftn`).
    #[derive(Debug, Clone, PartialEq)]
    pub struct NhiCell {
        pub id: String,
        pub posh_pct: i32,
        pub poh_pct: i32,
        pub size: NhiSize,
    }

    /// The TAB's hail size column: `a31644.ftn` writes `<0.50` below the
    /// reporting floor, `>4.00` for the flagged over-4-inch sizes, plain
    /// `F4.2` otherwise.
    #[derive(Debug, Clone, Copy, PartialEq)]
    pub enum NhiSize {
        LessThan(f64),
        Exact(f64),
        GreaterThan(f64),
    }

    /// The RPG's own hail environment, from the TAB's adaptation page
    /// (`a3164f.ftn`): the 0 °C / −20 °C heights in **kft MSL** and the
    /// site's warning-threshold model — which is where the fleet's per-site
    /// WT offsets become visible.
    #[derive(Debug, Clone, Copy, PartialEq)]
    pub struct NhiEnv {
        pub h0_kft_msl: f64,
        pub hm20_kft_msl: f64,
        pub wtsm_coef: f64,
        pub wtsm_offset: f64,
    }

    /// The `decode_product` pre-processing the byte offsets are counted
    /// from: strip a WMO/AWIPS envelope, inflate a whole-file zlib wrap if
    /// present, strip again. Mirrors `nexrad-level3`'s own steps.
    pub fn clean_l3(bytes: &[u8]) -> Vec<u8> {
        fn strip_wmo(data: &[u8]) -> &[u8] {
            // A WMO/AWIPS envelope is printable text; requiring that of the
            // prefix keeps a `\r\r\n` that happens to sit inside compressed
            // binary from being read as one (`decode_product` gets away
            // without the check only because real envelopes precede the
            // zlib magic).
            let limit = data.len().min(100);
            let mut last = None;
            for i in 0..limit.saturating_sub(2) {
                let b = data[i];
                if !(b == b'\r' || b == b'\n' || (0x20..0x7F).contains(&b)) {
                    break;
                }
                if &data[i..i + 3] == b"\r\r\n" {
                    last = Some(i + 3);
                }
            }
            last.map_or(data, |p| &data[p..])
        }
        use std::io::Read;
        let stripped = strip_wmo(bytes);
        let mut inflated = Vec::new();
        let mut decoder = flate2::read::ZlibDecoder::new(stripped);
        if decoder.read_to_end(&mut inflated).is_ok() && !inflated.is_empty() {
            return strip_wmo(&inflated).to_vec();
        }
        stripped.to_vec()
    }

    fn read_u16(d: &[u8], o: usize) -> Option<u16> {
        Some(u16::from_be_bytes(d.get(o..o + 2)?.try_into().ok()?))
    }

    fn read_i16(d: &[u8], o: usize) -> Option<i16> {
        read_u16(d, o).map(|v| v as i16)
    }

    fn read_u32(d: &[u8], o: usize) -> Option<u32> {
        Some(u32::from_be_bytes(d.get(o..o + 4)?.try_into().ok()?))
    }

    /// The TAB's text lines, all pages concatenated. `tabular_offset` is the
    /// PDB's, in halfwords from the message-header start of a *cleaned*
    /// buffer. Layout per the Class 1 ICD: divider, block ID 3, block
    /// length, an embedded message header + PDB (120 bytes), divider, page
    /// count, then per page `{line length, characters}*` ended by a −1
    /// divider.
    pub fn tab_pages(clean: &[u8], tabular_offset_hw: u32) -> Option<Vec<String>> {
        let mut o = (tabular_offset_hw as usize).checked_mul(2)?;
        if read_i16(clean, o)? != -1 || read_u16(clean, o + 2)? != 3 {
            return None;
        }
        let _block_len = read_u32(clean, o + 4)?;
        o += 8 + 120; // embedded message header (18) + PDB (102)
        if read_i16(clean, o)? != -1 {
            return None;
        }
        let n_pages = read_u16(clean, o + 2)?;
        o += 4;
        let mut lines = Vec::new();
        for _ in 0..n_pages {
            loop {
                let v = read_u16(clean, o)?;
                o += 2;
                if v == 0xFFFF {
                    break; // end-of-page divider
                }
                let n = v as usize;
                let raw = clean.get(o..o + n)?;
                lines.push(String::from_utf8_lossy(raw).into_owned());
                o += n;
            }
        }
        Some(lines)
    }

    /// Cell rows out of the TAB lines: a 2-character SCIT ID (letter +
    /// digit) followed by POSH %, POH % and the qualified size. Rows whose
    /// values read `UNKNOWN` (beyond processing range) are dropped.
    pub fn parse_nhi_cells(lines: &[String]) -> Vec<NhiCell> {
        let mut cells = Vec::new();
        for line in lines {
            let tokens: Vec<&str> = line.split_whitespace().collect();
            if tokens.len() != 4 || tokens[0].len() != 2 {
                continue;
            }
            let mut chars = tokens[0].chars();
            let (Some(a), Some(b)) = (chars.next(), chars.next()) else {
                continue;
            };
            if !a.is_ascii_uppercase() || !b.is_ascii_digit() {
                continue;
            }
            let (Ok(posh), Ok(poh)) = (tokens[1].parse::<i32>(), tokens[2].parse::<i32>()) else {
                continue;
            };
            let size = match tokens[3].split_at_checked(1) {
                Some(("<", rest)) => rest.parse().ok().map(NhiSize::LessThan),
                Some((">", rest)) => rest.parse().ok().map(NhiSize::GreaterThan),
                _ => tokens[3].parse().ok().map(NhiSize::Exact),
            };
            let Some(size) = size else { continue };
            cells.push(NhiCell {
                id: tokens[0].to_string(),
                posh_pct: posh,
                poh_pct: poh,
                size,
            });
        }
        cells
    }

    /// The adaptation page's environment: the `a3164f.ftn` lines
    /// `xx.x (KFT) HEIGHT (0 DEG CELSIUS)`, `… (-20 DEG CELSIUS)`, and
    /// `xxx.x (10E2 J/M**2/S) WTSM COEFFICIENT | xxx.x (10E5 J/M/S) WTSM
    /// OFFSET`.
    pub fn parse_nhi_env(lines: &[String]) -> Option<NhiEnv> {
        let first_num =
            |line: &str| -> Option<f64> { line.split_whitespace().next()?.parse().ok() };
        let mut h0 = None;
        let mut hm20 = None;
        let mut coef = None;
        let mut offset = None;
        for line in lines {
            if line.contains("HEIGHT (0 DEG") {
                h0 = first_num(line);
            } else if line.contains("HEIGHT (-20 DEG") {
                hm20 = first_num(line);
            } else if line.contains("WTSM COEFFICIENT") {
                coef = first_num(line);
                offset = line.split('|').nth(1).and_then(first_num);
            }
        }
        Some(NhiEnv {
            h0_kft_msl: h0?,
            hm20_kft_msl: hm20?,
            wtsm_coef: coef?,
            wtsm_offset: offset?,
        })
    }

    /// Cell positions from the symbology's storm-ID packets (code 15:
    /// quarter-km I/J plus the 2-character ID, written for every cell with a
    /// nonzero POH or POSH — `a3164c.ftn`). Returns `(id, azimuth °, range
    /// km)`. Unknown packet codes are skipped by their byte length, which
    /// every packet the NHI symbology carries declares.
    pub fn storm_positions(clean: &[u8], symbology_offset_hw: u32) -> Vec<(String, f64, f64)> {
        let mut out = Vec::new();
        let Some(base) = (symbology_offset_hw as usize).checked_mul(2) else {
            return out;
        };
        let Some(n_layers) = read_u16(clean, base + 8) else {
            return out;
        };
        let mut o = base + 10;
        for _ in 0..n_layers {
            let Some(layer_len) = read_u32(clean, o + 2) else {
                return out;
            };
            o += 6;
            let end = o + layer_len as usize;
            while o + 4 <= end && o + 4 <= clean.len() {
                let (Some(code), Some(len)) = (read_u16(clean, o), read_u16(clean, o + 2)) else {
                    return out;
                };
                if code == 15
                    && len as usize >= 6
                    && let (Some(i), Some(j), Some(id)) = (
                        read_i16(clean, o + 4),
                        read_i16(clean, o + 6),
                        clean.get(o + 8..o + 10),
                    )
                {
                    let x_km = f64::from(i) / 4.0;
                    let y_km = f64::from(j) / 4.0;
                    out.push((
                        String::from_utf8_lossy(id).into_owned(),
                        x_km.atan2(y_km).to_degrees().rem_euclid(360.0),
                        x_km.hypot(y_km),
                    ));
                }
                o += 4 + len as usize;
            }
            o = o.max(end);
        }
        out
    }

    /// The grid maximum over the comparison window around a centroid:
    /// azimuth wraps, range clamps to the domain. `NaN` when every cell in
    /// the window is undefined.
    pub fn window_max(grid: &super::VolumetricGrid, az_deg: f64, range_km: f64) -> f32 {
        let az0 = az_deg.rem_euclid(360.0) as usize % 360;
        let r0 = range_km as i64;
        let mut best = f32::NAN;
        for daz in -(WINDOW_AZ_DEG as i64)..=WINDOW_AZ_DEG as i64 {
            let az = (az0 as i64 + daz).rem_euclid(360) as usize;
            for dr in -(WINDOW_RANGE_KM as i64)..=WINDOW_RANGE_KM as i64 {
                let Ok(r) = usize::try_from(r0 + dr) else {
                    continue;
                };
                if r >= grid.range_bins {
                    continue;
                }
                let v = grid.values[az][r];
                if !v.is_nan() && (best.is_nan() || v > best) {
                    best = v;
                }
            }
        }
        best
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexrad_model::data::{
        MomentData, PulseWidth, Radial, RadialStatus, Sweep, VolumeCoveragePattern,
    };

    const SCALE: f32 = 2.0;
    const OFFSET: f32 = 66.0;
    const GATES: usize = 40;
    /// 1 km gates: cube cell `r` reads gate `r` exactly, so every expected
    /// value is hand-computable without the resampling entering into it.
    const GATE_INTERVAL_M: u16 = 1000;

    fn vcp() -> VolumeCoveragePattern {
        VolumeCoveragePattern::new(
            212,
            0,
            0.5,
            PulseWidth::Short,
            false,
            0,
            false,
            0,
            false,
            false,
            0,
            false,
            false,
            Vec::new(),
        )
    }

    /// One reflectivity sweep of 360 radials on cell centres, with dBZ per
    /// azimuth cell from `dbz_at` (`None` = censored, gate byte 0).
    fn refl_sweep(
        elevation_number: u8,
        elevation_deg: f32,
        dbz_at: impl Fn(usize) -> Option<f64>,
    ) -> Sweep {
        let radials = (0..360)
            .map(|i| {
                let byte = match dbz_at(i) {
                    None => 0u8,
                    Some(dbz) => ((dbz * f64::from(SCALE) + f64::from(OFFSET)).round() as i64)
                        .clamp(2, 255) as u8,
                };
                Radial::new(
                    0,
                    i as u16,
                    i as f32 + 0.5,
                    1.0,
                    RadialStatus::IntermediateRadialData,
                    elevation_number,
                    elevation_deg,
                    Some(MomentData::from_fixed_point(
                        GATES as u16,
                        0,
                        GATE_INTERVAL_M,
                        8,
                        SCALE,
                        OFFSET,
                        vec![byte; GATES],
                    )),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                )
            })
            .collect();
        Sweep::new(elevation_number, radials)
    }

    /// An environment at km-MSL heights, radar at 0 ft: MSL ≡ ARL, so the
    /// hand arithmetic needs no datum term.
    fn env(h0_km: f64, hm20_km: f64) -> EnvHeights {
        EnvHeights {
            h0c_km_msl: h0_km,
            hm20c_km_msl: hm20_km,
            fetched_at: chrono::Utc::now(),
        }
    }

    /// Four tilts at 0.5°/1.5°/2.5°/3.5° exercising every column rule under
    /// `H₀ = 1.0`, `H₋₂₀ = 2.0` km (radar at 0 ft). Layer bounds at r = 30
    /// (centre 30.5 km), 4/3 model:
    ///
    /// * tilt 0: 0 – 0.5870331 (entirely below `H₀`);
    /// * tilt 1: 0.5870331 – 1.1191491 (straddles `H₀`: clipped);
    /// * tilt 2: 1.1191491 – 1.6509408;
    /// * tilt 3: 1.6509408 – 2.1690515 (half-power flank cap).
    ///
    /// Columns:
    ///
    /// * az 10: 50 dBZ ×4 — the full ramp, every layer case;
    /// * az 20: 45 dBZ ×4 — `W(Z) = 0.5` throughout;
    /// * az 30: 55/50/45/35 — graded, the 35 dBZ ceiling contributes 0;
    /// * az 40: 39.9 dBZ ×4 — sub-ramp echo: **defined 0**, not `NaN`;
    /// * az 50: censored — `NaN`;
    /// * az 60: 50 dBZ lowest tilt only — everything below `H₀`: defined 0;
    /// * az 70: 50 dBZ top tilt only — one fully-elevated layer.
    fn golden_scan() -> Scan {
        let profile = |tilt: usize| {
            move |az: usize| -> Option<f64> {
                match az {
                    10 => Some(50.0),
                    20 => Some(45.0),
                    30 => Some([55.0, 50.0, 45.0, 35.0][tilt]),
                    40 => Some(39.9),
                    60 => (tilt == 0).then_some(50.0),
                    70 => (tilt == 3).then_some(50.0),
                    _ => None,
                }
            }
        };
        Scan::new(
            vcp(),
            vec![
                refl_sweep(1, 0.5, profile(0)),
                refl_sweep(2, 1.5, profile(1)),
                refl_sweep(3, 2.5, profile(2)),
                refl_sweep(4, 3.5, profile(3)),
            ],
        )
    }

    fn assert_close(actual: f64, expected: f64, tol: f64, what: &str) {
        assert!(
            (actual - expected).abs() < tol,
            "{what}: expected {expected}, got {actual}",
        );
    }

    /// Ė at hand-computed points of the closed form
    /// `5×10⁻⁶·10^(0.084·Z)·W(Z)`:
    ///
    /// * Z = 45: `5×10⁻⁶·10^3.78·0.5` = **0.015063990** J m⁻² s⁻¹;
    /// * Z = 50: `5×10⁻⁶·10^4.2` = **0.079244660**;
    /// * Z = 55: **0.208434692** (ramp saturated);
    /// * Z = 60: **0.548239098**;
    /// * Z = 40 and below: exactly 0 — the ramp, not the exponential,
    ///   decides.
    #[test]
    fn hail_kinetic_energy_flux_matches_the_closed_form() {
        assert_eq!(hail_kinetic_energy_flux(20.0), 0.0);
        assert_eq!(hail_kinetic_energy_flux(39.9), 0.0);
        assert_eq!(hail_kinetic_energy_flux(40.0), 0.0);
        assert_close(
            hail_kinetic_energy_flux(45.0),
            0.015_063_989_651_858_952,
            1e-15,
            "E(45)",
        );
        assert_close(
            hail_kinetic_energy_flux(50.0),
            0.079_244_659_623_055_71,
            1e-15,
            "E(50)",
        );
        assert_close(
            hail_kinetic_energy_flux(55.0),
            0.208_434_691_735_167_77,
            1e-15,
            "E(55)",
        );
        assert_close(
            hail_kinetic_energy_flux(60.0),
            0.548_239_098_071_592_5,
            1e-15,
            "E(60)",
        );
    }

    /// `W(Z)`'s endpoints and midpoint, and that 50 dBZ is where it stops
    /// growing.
    #[test]
    fn refl_weight_ramps_from_40_to_50_dbz() {
        assert_eq!(refl_weight(0.0), 0.0);
        assert_eq!(refl_weight(40.0), 0.0);
        assert_eq!(refl_weight(45.0), 0.5);
        assert_eq!(refl_weight(47.5), 0.75);
        assert_eq!(refl_weight(50.0), 1.0);
        assert_eq!(refl_weight(75.0), 1.0);
    }

    /// `W_T`'s endpoints and midpoint on `H₀ = 3`, `H₋₂₀ = 6` km, plus the
    /// degenerate winter column where both heights clamp to the same value.
    #[test]
    fn temp_weight_ramps_between_the_freezing_heights() {
        assert_eq!(temp_weight(2.0, 3.0, 6.0), 0.0);
        assert_eq!(temp_weight(3.0, 3.0, 6.0), 0.0);
        assert_eq!(temp_weight(4.5, 3.0, 6.0), 0.5);
        assert_eq!(temp_weight(6.0, 3.0, 6.0), 1.0);
        assert_eq!(temp_weight(9.0, 3.0, 6.0), 1.0);
        // Degenerate: Hm20 at (or below) H0 steps instead of dividing by 0.
        assert_eq!(temp_weight(0.5, 1.0, 1.0), 0.0);
        assert_eq!(temp_weight(1.0, 1.0, 1.0), 1.0);
        assert_eq!(temp_weight(2.0, 1.0, 0.5), 1.0);
    }

    /// WT at the paper's example heights, and the 20 J m⁻¹ s⁻¹ floor: below
    /// `H₀ = 141/57.5 ≈ 2.452` km the model line sits under the floor.
    #[test]
    fn warning_threshold_is_the_papers_line_with_the_20_floor() {
        assert_close(warning_threshold(3.0), 51.5, 1e-12, "WT(3)");
        assert_close(warning_threshold(5.0), 166.5, 1e-12, "WT(5)");
        assert_eq!(warning_threshold(1.0), 20.0, "under the floor");
        assert_eq!(warning_threshold(0.0), 20.0);
        assert_eq!(warning_threshold(2.4), 20.0, "just under the crossover");
        assert!(warning_threshold(2.5) > 20.0, "just over the crossover");
    }

    /// The POSH curve: 50 % exactly at `SHI = WT`, the clamps at both ends,
    /// and 0 for the no-signal column.
    #[test]
    fn posh_reads_50_at_the_warning_threshold_and_clamps() {
        assert_eq!(posh_pct(20.0, 20.0), 50.0);
        assert_eq!(posh_pct(51.5, 51.5), 50.0);
        // 29·ln(e) = 29 points per e-fold.
        assert_close(
            posh_pct(20.0 * std::f64::consts::E, 20.0),
            79.0,
            1e-9,
            "one e-fold",
        );
        assert_eq!(posh_pct(20.0 * (50.0f64 / 29.0).exp() * 1.01, 20.0), 100.0);
        assert_eq!(posh_pct(1e-9, 20.0), 0.0, "clamped at 0");
        assert_eq!(posh_pct(0.0, 20.0), 0.0, "no signal");
        assert_eq!(posh_pct(-1.0, 20.0), 0.0);
    }

    /// MEHS at reference SHI values, in mm, and the inch equivalence the
    /// display relies on: SHI 100 → 25.4 mm = 1.00 in, SHI 25 → 12.7 mm =
    /// 0.50 in.
    #[test]
    fn mehs_reads_the_papers_millimetres() {
        assert_close(mehs_mm(100.0), 25.4, 1e-12, "SHI 100");
        assert_close(mehs_mm(25.0), 12.7, 1e-12, "SHI 25");
        assert_close(mehs_mm(51.5), 18.227_929_119_897_3, 1e-9, "SHI 51.5");
        assert_eq!(mehs_mm(0.0), 0.0);
        assert_eq!(mehs_mm(-3.0), 0.0);
        // The ORPG computes 0.10·√SHI in inches — the identical size.
        assert_eq!(0.10 * 25.4, MEHS_COEF_MM);
    }

    /// The MSL→ARL datum: `a31599.ftn`'s conversion at KTLX's 1213 ft, and
    /// its clamp at a high site whose freezing level sits below the radar.
    #[test]
    fn env_heights_resolve_msl_to_arl_against_the_site_elevation() {
        let (h0, hm20) = env_arl_km(&env(4.0, 7.0), 1213.0);
        assert_close(h0, 4.0 - 1213.0 * 0.0003048, 1e-12, "H0 ARL");
        assert_close(hm20, 7.0 - 1213.0 * 0.0003048, 1e-12, "Hm20 ARL");
        assert_close(h0, 3.630_277_6, 1e-6, "H0 ARL literal");

        // KABX sits at 5870 ft = 1.789 km: a 1.0 km MSL freezing level is
        // below the radar and clamps to 0 ARL, not −0.789 — and so does a
        // −20 °C surface at 1.5 km, the degenerate winter column
        // `temp_weight` steps through.
        let (h0, hm20) = env_arl_km(&env(1.0, 1.5), 5870.0);
        assert_eq!(h0, 0.0);
        assert_eq!(hm20, 0.0, "Hm20 below the radar clamps too");
        let (_, hm20) = env_arl_km(&env(1.0, 2.5), 5870.0);
        assert_close(hm20, 2.5 - 5870.0 * 0.0003048, 1e-12, "Hm20 above");

        // At 0 ft, MSL is ARL.
        assert_eq!(env_arl_km(&env(1.0, 2.0), 0.0), (1.0, 2.0));
    }

    /// The documented grid rules against hand-computed columns, all at
    /// r = 30 with `H₀ = 1.0`, `H₋₂₀ = 2.0` km ARL (WT floored at 20):
    ///
    /// * az 10 (50 dBZ ×4): tilt 0's layer tops at 0.587 km — below `H₀`,
    ///   nothing; tilt 1 clips to [1.0, 1.1191], `W_T` 0.0596 at its
    ///   midpoint; tilts 2–3 carry full depths at `W_T` 0.385/0.910 —
    ///   SHI **5.415110**, POSH `29·ln(5.4151/20) + 50` = **12.110366** %,
    ///   MEHS `2.54·√5.4151` = **5.910679** mm;
    /// * az 20 (45 dBZ ×4): the same column at `Ė(45)` — SHI **1.029384**,
    ///   POSH 0 (clamped), MEHS **2.577047**;
    /// * az 30 (graded): only the 50 dBZ tilt-1 and 45 dBZ tilt-2 layers
    ///   contribute (tilt 0 is below `H₀`, 35 dBZ is under the ramp) — SHI
    ///   **0.364706**, MEHS **1.533928**;
    /// * az 40 (39.9 dBZ): a **defined 0** across all three grids;
    /// * az 50 (censored): `NaN` across all three;
    /// * az 60 (50 dBZ lowest only): the whole echo sits below `H₀` —
    ///   defined 0;
    /// * az 70 (50 dBZ top only): one layer, [1.6509, 2.1691] — SHI
    ///   **3.736217**, POSH **1.347897**, MEHS **4.909641**.
    #[test]
    fn the_documented_rules_produce_hand_computed_hail() {
        let grids = compute_hail(&golden_scan(), Some(&env(1.0, 2.0)), 0.0).unwrap();
        assert_eq!(grids.posh.range_bins, RANGE_BINS);
        assert_eq!(grids.posh.values.len(), 360);
        let r = 30;

        let at = |g: &VolumetricGrid, az: usize| f64::from(g.values[az][r]);
        assert_close(at(&grids.shi, 10), 5.415_109_913, 1e-5, "az10 SHI");
        assert_close(at(&grids.posh, 10), 12.110_366, 1e-4, "az10 POSH");
        assert_close(at(&grids.mehs_mm, 10), 5.910_679, 1e-5, "az10 MEHS");

        assert_close(at(&grids.shi, 20), 1.029_383_7, 1e-6, "az20 SHI");
        assert_eq!(grids.posh.values[20][r], 0.0, "az20 POSH clamps to 0");
        assert_close(at(&grids.mehs_mm, 20), 2.577_047, 1e-5, "az20 MEHS");

        assert_close(at(&grids.shi, 30), 0.364_705_7, 1e-6, "az30 SHI");
        assert_close(at(&grids.mehs_mm, 30), 1.533_928, 1e-5, "az30 MEHS");

        for (az, why) in [(40, "sub-40 dBZ echo"), (60, "echo entirely below H0")] {
            assert_eq!(grids.shi.values[az][r], 0.0, "{why}: SHI");
            assert_eq!(grids.posh.values[az][r], 0.0, "{why}: POSH");
            assert_eq!(grids.mehs_mm.values[az][r], 0.0, "{why}: MEHS");
        }

        assert!(grids.shi.values[50][r].is_nan(), "censored column");
        assert!(grids.posh.values[50][r].is_nan());
        assert!(grids.mehs_mm.values[50][r].is_nan());
        assert!(grids.posh.values[10][GATES].is_nan(), "beyond the data");

        assert_close(at(&grids.shi, 70), 3.736_216_85, 1e-5, "az70 SHI");
        assert_close(at(&grids.posh, 70), 1.347_897, 1e-4, "az70 POSH");
        assert_close(at(&grids.mehs_mm, 70), 4.909_641, 1e-5, "az70 MEHS");
    }

    /// The column split by `H₋₂₀`: lowering it from 2.0 to 1.5 km saturates
    /// `W_T` over the upper layers and SHI rises to the hand value
    /// **7.463536** (POSH 21.414615, MEHS 6.939146). The degenerate
    /// `H₋₂₀ = H₀` environment steps `W_T` to 1 everywhere above `H₀`:
    /// **9.264109**.
    #[test]
    fn the_minus_20_height_splits_the_column() {
        let scan = golden_scan();
        let r = 30;

        let split = compute_hail(&scan, Some(&env(1.0, 1.5)), 0.0).unwrap();
        assert_close(
            f64::from(split.shi.values[10][r]),
            7.463_536_3,
            1e-5,
            "Hm20 = 1.5 SHI",
        );
        assert_close(
            f64::from(split.posh.values[10][r]),
            21.414_615,
            1e-4,
            "Hm20 = 1.5 POSH",
        );

        let step = compute_hail(&scan, Some(&env(1.0, 1.0)), 0.0).unwrap();
        assert_close(
            f64::from(step.shi.values[10][r]),
            9.264_108_6,
            1e-5,
            "degenerate SHI",
        );
    }

    /// A single-tilt volume: the one layer runs from the ground to the
    /// half-power upper flank — `[0, 0.5737472]` km at r = 30 for a 0.5°
    /// tilt — clipped at `H₀ = 0.2`: SHI **0.691840**.
    #[test]
    fn a_single_tilt_column_is_capped_at_the_beam_flank() {
        let scan = Scan::new(
            vcp(),
            vec![refl_sweep(1, 0.5, |az| (az == 10).then_some(50.0))],
        );
        let grids = compute_hail(&scan, Some(&env(0.2, 1.0)), 0.0).unwrap();
        assert_close(
            f64::from(grids.shi.values[10][30]),
            0.691_840_33,
            1e-6,
            "single-tilt SHI",
        );
        assert!(grids.shi.values[11][30].is_nan(), "uninvolved column");
    }

    /// No [`EnvHeights`] is **no field** — `None`, not a zero-filled grid
    /// pretending to be data. The render seam turns this into "nothing to
    /// draw".
    #[test]
    fn no_environment_means_no_field() {
        assert!(compute_hail(&golden_scan(), None, 0.0).is_none());
    }

    /// A SAILS repeat late in the volume must not displace the first look:
    /// the repeat carries 55 dBZ where the first 0.5°-family look has 45 —
    /// `Ė` differs by ×13.8 — so a newest-wins dedup would move SHI by an
    /// order of magnitude. Tilts at 1.5°/2.5° so the echo sits above
    /// `H₀ = 0.2` km.
    #[test]
    fn a_sails_repeat_does_not_displace_the_first_look() {
        let first = |az: usize| (az == 61).then_some(45.0);
        let upper = |az: usize| (az == 61).then_some(45.0);
        let repeat = |az: usize| match az {
            60 | 61 => Some(55.0),
            _ => None,
        };
        let scan = Scan::new(
            vcp(),
            vec![
                refl_sweep(1, 1.5, first),
                refl_sweep(2, 2.5, upper),
                refl_sweep(3, 1.5, repeat), // SAILS revisit, late
            ],
        );
        let grids = compute_hail(&scan, Some(&env(0.2, 1.0)), 0.0).unwrap();

        // az 60 exists only on the repeat: first-of-volume leaves it empty.
        assert!(
            grids.shi.values[60][30].is_nan(),
            "the SAILS repeat displaced the first look",
        );

        // az 61 must read the FIRST look's 45 dBZ on both tilts. With the
        // repeat's 55 dBZ in the lower slot the value would ~double.
        let with_45 = f64::from(grids.shi.values[61][30]);
        assert!(with_45 > 0.0);
        let alone = Scan::new(
            vcp(),
            vec![refl_sweep(1, 1.5, first), refl_sweep(2, 2.5, upper)],
        );
        let expected = compute_hail(&alone, Some(&env(0.2, 1.0)), 0.0).unwrap();
        assert_eq!(
            grids.shi.values[61][30], expected.shi.values[61][30],
            "the repeat's reflectivity leaked into the sum",
        );
    }

    /// Every coefficient the module ships, pinned literally: a tuning edit
    /// must be a visible diff here, not a silent drift.
    #[test]
    fn the_paper_constants_are_pinned() {
        assert_eq!(HKE_FLUX_COEF, 5.0e-6);
        assert_eq!(HKE_FLUX_EXP_PER_DBZ, 0.084);
        assert_eq!(HKE_REF_WGT_LOW_DBZ, 40.0);
        assert_eq!(HKE_REF_WGT_HIGH_DBZ, 50.0);
        assert_eq!(SHI_COEF, 0.1);
        assert_eq!(WT_COEF_PER_KM, 57.5);
        assert_eq!(WT_OFFSET, -121.0);
        assert_eq!(WT_FLOOR, 20.0);
        assert_eq!(POSH_COEF, 29.0);
        assert_eq!(POSH_OFFSET_PCT, 50.0);
        assert_eq!(MEHS_COEF_MM, 2.54);
        assert_eq!(MEHS_EXP, 0.5);
        // The ORPG factoring is the same arithmetic: 0.0005 per km of depth
        // is 0.1 × 5e-6 per metre (`hke_coef1` in `hail.alg`).
        assert!((SHI_COEF * HKE_FLUX_COEF * 1000.0 - 0.0005).abs() < 1e-18);
    }
}

/// Offline pins on the validation policy and the NHI parsing — everything
/// the ignored live gate decides with.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod policy_tests {
    use super::validation_policy as policy;

    /// The gate's tolerances and conclusiveness legs, pinned so the ignored
    /// harness cannot drift them.
    #[test]
    fn the_sanity_gate_is_what_the_campaign_set() {
        assert_eq!(policy::POSH_TOLERANCE_PTS, 20.0);
        assert_eq!(policy::MEHS_TOLERANCE_IN, 0.5);
        assert_eq!(policy::MIN_CELLS, 10);
        assert_eq!(policy::MIN_SITES, 2);
        assert_eq!(policy::PASS_SHARE_MIN_PCT, 80.0);
        assert_eq!(policy::WINDOW_AZ_DEG, 2);
        assert_eq!(policy::WINDOW_RANGE_KM, 2);

        assert!(policy::gate_is_conclusive(10, 2));
        assert!(!policy::gate_is_conclusive(9, 5), "cells leg");
        assert!(!policy::gate_is_conclusive(50, 1), "sites leg");

        assert!(policy::share_passes(8, 10), "80% is inclusive");
        assert!(!policy::share_passes(7, 10));
        assert!(!policy::share_passes(0, 0), "an empty sample never passes");
    }

    /// A synthetic TAB block: divider, ID 3, length, 120 bytes of embedded
    /// header/PDB, divider, one page of three lines ended by the page
    /// divider. `tab_pages` must return exactly the lines.
    fn tab_bytes(offset_hw: u32, lines: &[&str]) -> Vec<u8> {
        let mut d = vec![0u8; offset_hw as usize * 2];
        d.extend_from_slice(&(-1i16).to_be_bytes());
        d.extend_from_slice(&3u16.to_be_bytes());
        d.extend_from_slice(&0u32.to_be_bytes()); // block length, unread
        d.extend_from_slice(&[0u8; 120]); // embedded header + PDB
        d.extend_from_slice(&(-1i16).to_be_bytes());
        d.extend_from_slice(&1u16.to_be_bytes()); // one page
        for line in lines {
            d.extend_from_slice(&(line.len() as u16).to_be_bytes());
            d.extend_from_slice(line.as_bytes());
        }
        d.extend_from_slice(&(-1i16).to_be_bytes()); // end of page
        d
    }

    #[test]
    fn tab_pages_walks_the_icd_layout() {
        let lines = ["                    HAIL ", "  A1 stays", "trailer"];
        let bytes = tab_bytes(75, &lines);
        let got = policy::tab_pages(&bytes, 75).expect("well-formed TAB");
        assert_eq!(got, lines.map(String::from));

        // A wrong divider or block ID is a refusal, not a misparse.
        assert!(policy::tab_pages(&bytes, 74).is_none());
        let mut wrong_id = bytes.clone();
        wrong_id[75 * 2 + 3] = 9;
        assert!(policy::tab_pages(&wrong_id, 75).is_none());
        // Truncation inside a line is a clean None.
        assert!(policy::tab_pages(&bytes[..bytes.len() - 12], 75).is_none());
    }

    /// The cell-row parser against `a31644.ftn`'s own column layout: plain
    /// sizes, the `<0.50` floor, the `>4.00` cap, an UNKNOWN row (dropped),
    /// and header lines that must not parse as cells.
    #[test]
    fn nhi_cell_rows_parse_with_their_qualifiers() {
        let lines: Vec<String> = [
            "                                      HAIL ",
            "     RADAR ID 306   DATE/TIME 07:28:26  23:59:34   NUMBER OF STORM CELLS  4",
            "      STORM       PROBABILITY OF       PROBABILITY OF       MAX EXPECTED",
            "        ID        SEVERE HAIL (%)         HAIL (%)          HAIL SIZE (IN)",
            "",
            "        V0             60                  100                  1.25",
            "        B7              0                   60                 <0.50",
            "        Q3            100                  100                 >4.00",
            "        Z9            UNKNOWN              UNKNOWN             UNKNOWN",
        ]
        .map(String::from)
        .to_vec();
        let cells = policy::parse_nhi_cells(&lines);
        assert_eq!(
            cells,
            vec![
                policy::NhiCell {
                    id: "V0".into(),
                    posh_pct: 60,
                    poh_pct: 100,
                    size: policy::NhiSize::Exact(1.25),
                },
                policy::NhiCell {
                    id: "B7".into(),
                    posh_pct: 0,
                    poh_pct: 60,
                    size: policy::NhiSize::LessThan(0.5),
                },
                policy::NhiCell {
                    id: "Q3".into(),
                    posh_pct: 100,
                    poh_pct: 100,
                    size: policy::NhiSize::GreaterThan(4.0),
                },
            ],
        );
    }

    /// The adaptation-page parser against `a3164f.ftn`'s own formats,
    /// including the fleet's per-site WT offset — the paper-vs-source delta
    /// the module doc records.
    #[test]
    fn the_adaptation_page_yields_the_rpgs_own_environment() {
        let lines: Vec<String> = [
            "                        HAIL DETECTION ADAPTATION DATA",
            "   10.5 (KFT) HEIGHT (0 DEG CELSIUS)    | 1.600  (KM) POH HEIGHT DIFFERENCE #1",
            "   20.0 (KFT) HEIGHT (-20 DEG CELSIUS)  | 1.900  (KM) POH HEIGHT DIFFERENCE #2",
            "  57.5 (10E2 J/M**2/S) WTSM COEFFICIENT |  -74.2 (10E5 J/M/S) WTSM OFFSET",
            "    230 (KM)  MAX HAIL PROCESSING RANGE",
        ]
        .map(String::from)
        .to_vec();
        let env = policy::parse_nhi_env(&lines).expect("all four fields present");
        assert_eq!(env.h0_kft_msl, 10.5);
        assert_eq!(env.hm20_kft_msl, 20.0);
        assert_eq!(env.wtsm_coef, 57.5);
        assert_eq!(env.wtsm_offset, -74.2);

        assert!(policy::parse_nhi_env(&lines[..3]).is_none());
    }

    /// Storm positions out of a synthetic symbology block: a code-19 hail
    /// packet is skipped by its declared length, the code-15 storm-ID packet
    /// parses, and quarter-km I/J becomes azimuth/range — (100, 100) quarter
    /// km is 25 km east-northeast at 45°.
    #[test]
    fn storm_positions_read_the_code_15_packets() {
        let mut d = vec![0u8; 60 * 2];
        d.extend_from_slice(&(-1i16).to_be_bytes()); // divider
        d.extend_from_slice(&1u16.to_be_bytes()); // block id
        d.extend_from_slice(&0u32.to_be_bytes()); // block length
        d.extend_from_slice(&1u16.to_be_bytes()); // one layer
        d.extend_from_slice(&(-1i16).to_be_bytes()); // layer divider
        d.extend_from_slice(&30u32.to_be_bytes()); // layer length
        // code 19 (hail symbol), 10 bytes: skipped by length.
        d.extend_from_slice(&19u16.to_be_bytes());
        d.extend_from_slice(&10u16.to_be_bytes());
        d.extend_from_slice(&[0u8; 10]);
        // code 15 (storm ID): I = 100, J = 100 quarter-km, "V0".
        d.extend_from_slice(&15u16.to_be_bytes());
        d.extend_from_slice(&6u16.to_be_bytes());
        d.extend_from_slice(&100i16.to_be_bytes());
        d.extend_from_slice(&100i16.to_be_bytes());
        d.extend_from_slice(b"V0");

        let got = policy::storm_positions(&d, 60);
        assert_eq!(got.len(), 1);
        let (id, az, range) = &got[0];
        assert_eq!(id, "V0");
        assert!((az - 45.0).abs() < 1e-9, "azimuth {az}");
        assert!((range - 25.0 * std::f64::consts::SQRT_2 / 1.0).abs() < 1e-9);
    }

    /// The comparison window: the maximum within ±2° (wrapping) and ±2 km
    /// (clamping), `NaN` when nothing in the window is defined.
    #[test]
    fn window_max_wraps_azimuth_and_clamps_range() {
        let mut values = vec![vec![f32::NAN; 230]; 360];
        values[359][10] = 30.0;
        values[1][12] = 60.0;
        values[5][10] = 99.0; // outside ±2° of az 0
        let grid = super::VolumetricGrid {
            values,
            range_bins: 230,
        };
        assert_eq!(policy::window_max(&grid, 0.4, 10.2), 60.0);
        assert_eq!(policy::window_max(&grid, 359.9, 10.0), 60.0, "wraps up");
        assert!(policy::window_max(&grid, 180.0, 100.0).is_nan());
        // Range clamps at the domain edge without panicking.
        assert!(policy::window_max(&grid, 0.0, 229.9).is_nan());
        assert!(policy::window_max(&grid, 0.0, 0.4).is_nan());
    }

    /// The WMO-envelope strip and the zlib pass-through: plain bytes come
    /// back unchanged past the envelope, and a zlib wrap inflates.
    #[test]
    fn clean_l3_strips_envelopes_and_inflates() {
        let body = b"\x00\x01payload".to_vec();
        let mut enveloped = b"SDUS54 KOUN 282300\r\r\nNHITLX\r\r\n".to_vec();
        enveloped.extend_from_slice(&body);
        assert_eq!(policy::clean_l3(&enveloped), body);
        assert_eq!(policy::clean_l3(&body), body);

        use std::io::Write;
        let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(&enveloped).unwrap();
        let z = enc.finish().unwrap();
        assert_eq!(policy::clean_l3(&z), body, "zlib wrap inflates");
    }
}

/// The live sanity gate: our gridded POSH/MEHS against the RPG's own
/// cell-based NHI (product 59) for the **same volume**, at the SCIT cell
/// centroids, with the RPG's *own* environmental heights and site-tuned
/// warning threshold parsed out of the product's adaptation page.
///
/// A **sanity gate, not a bar** — see `validation_policy`'s module doc for
/// the asymmetry. Run against live convection, or against an archived severe
/// hour with an explicit timestamp:
///
/// ```text
/// HAIL_GATE_AT=2026-07-27T22:00:00 HAIL_GATE_SITES=KTLX,KDDC \
/// cargo test -p rustdar-radar --release --lib -- --ignored --nocapture live_nhi
/// ```
#[cfg(all(test, not(target_arch = "wasm32")))]
mod live_validation {
    use super::validation_policy as policy;
    use crate::sounding::EnvHeights;
    use crate::sources::DataSources;
    use crate::twin::live;

    const KFT_TO_KM: f64 = 0.3048;

    #[ignore = "hits the live S3 bucket"]
    #[tokio::test]
    async fn live_nhi_cells_sanity_gate() {
        crate::tls::init();
        let sources = DataSources::production();
        let when = std::env::var("HAIL_GATE_AT")
            .ok()
            .and_then(|s| chrono::NaiveDateTime::parse_from_str(&s, "%Y-%m-%dT%H:%M:%S").ok())
            .unwrap_or_else(|| chrono::Utc::now().naive_utc());
        let sites_env = std::env::var("HAIL_GATE_SITES").ok();
        let sites: Vec<&str> = match &sites_env {
            Some(s) => s.split(',').collect(),
            None => live::SITES.to_vec(),
        };

        let mut compared = 0usize;
        let mut posh_passed = 0usize;
        let mut mehs_compared = 0usize;
        let mut mehs_passed = 0usize;
        let mut sites_with_cells = std::collections::BTreeSet::new();

        for site in sites {
            let Some((scan, l2_start)) = live::l2_volume_near(site, when).await else {
                println!("{site}: SKIP — no archived Level II volume found");
                continue;
            };
            let Some(twin) = live::l3_twin(&sources, site, "NHI", l2_start, None).await else {
                println!("{site}: SKIP — no NHI twin names volume {l2_start}");
                continue;
            };
            if twin.message.pdb.product_code != 59 {
                println!(
                    "{site}: SKIP — twin {} decodes as product {}",
                    twin.stamp.key, twin.message.pdb.product_code,
                );
                continue;
            }

            let clean = policy::clean_l3(&twin.bytes);
            let Some(lines) = policy::tab_pages(&clean, twin.message.pdb.tabular_offset) else {
                println!("{site}: SKIP — no parseable TAB in {}", twin.stamp.key);
                continue;
            };
            let cells = policy::parse_nhi_cells(&lines);
            let Some(rpg_env) = policy::parse_nhi_env(&lines) else {
                println!("{site}: SKIP — no adaptation page in the TAB");
                continue;
            };
            let positions = policy::storm_positions(&clean, twin.message.pdb.symbology_offset);
            if cells.is_empty() {
                println!("{site}: no cells in the NHI TAB (quiet volume)");
                continue;
            }

            // The RPG's own environment, so the comparison isolates the
            // grid adaptation from any Open-Meteo disagreement.
            let env = EnvHeights {
                h0c_km_msl: rpg_env.h0_kft_msl * KFT_TO_KM,
                hm20c_km_msl: rpg_env.hm20_kft_msl * KFT_TO_KM,
                fetched_at: chrono::Utc::now(),
            };
            let radar_height_ft = f64::from(twin.message.pdb.height);
            let Some(grids) = super::compute_hail(&scan, Some(&env), radar_height_ft) else {
                println!("{site}: SKIP — compute_hail returned no field");
                continue;
            };
            // The site-tuned warning threshold from the adaptation page —
            // the fleet's per-site WT offset (module doc) — beside the
            // shipped paper WT.
            let (h0_arl, _) = super::env_arl_km(&env, radar_height_ft);
            let wt_site = (rpg_env.wtsm_coef * h0_arl + rpg_env.wtsm_offset).max(super::WT_FLOOR);
            let wt_paper = super::warning_threshold(h0_arl);
            println!(
                "{site}: vol {l2_start} twin {} | H0 {:.1} kft, H-20 {:.1} kft MSL, \
                 WT site {wt_site:.1} / paper {wt_paper:.1} | {} cells, {} positions",
                twin.stamp.key,
                rpg_env.h0_kft_msl,
                rpg_env.hm20_kft_msl,
                cells.len(),
                positions.len(),
            );

            let mut site_had_cell = false;
            for cell in &cells {
                let Some((_, az, range_km)) = positions.iter().find(|(id, ..)| *id == cell.id)
                else {
                    // Cells with POH = POSH = 0 carry no ID packet.
                    continue;
                };
                if *range_km >= 230.0 - policy::WINDOW_RANGE_KM as f64 {
                    continue;
                }
                let shi = policy::window_max(&grids.shi, *az, *range_km);
                let our_shi = if shi.is_nan() { 0.0 } else { f64::from(shi) };
                let our_posh_site = super::posh_pct(our_shi, wt_site);
                let our_posh_paper = super::posh_pct(our_shi, wt_paper);
                let our_mehs_in = super::mehs_mm(our_shi) / 25.4;

                let posh_diff = our_posh_site - f64::from(cell.posh_pct);
                let posh_ok = posh_diff.abs() <= policy::POSH_TOLERANCE_PTS;
                compared += 1;
                posh_passed += usize::from(posh_ok);
                site_had_cell = true;

                let (mehs_note, mehs_ok) = match cell.size {
                    policy::NhiSize::Exact(cell_in) => {
                        mehs_compared += 1;
                        let ok = (our_mehs_in - cell_in).abs() <= policy::MEHS_TOLERANCE_IN;
                        mehs_passed += usize::from(ok);
                        (format!("cell {cell_in:.2} in"), ok)
                    }
                    policy::NhiSize::LessThan(floor_in) => {
                        mehs_compared += 1;
                        let ok = our_mehs_in <= floor_in + policy::MEHS_TOLERANCE_IN;
                        mehs_passed += usize::from(ok);
                        (format!("cell < {floor_in:.2} in"), ok)
                    }
                    policy::NhiSize::GreaterThan(cap_in) => {
                        mehs_compared += 1;
                        let ok = our_mehs_in >= cap_in - policy::MEHS_TOLERANCE_IN;
                        mehs_passed += usize::from(ok);
                        (format!("cell > {cap_in:.2} in"), ok)
                    }
                };
                println!(
                    "  {} @ {az:.0}°/{range_km:.0} km: POSH cell {} / grid {:.0} \
                     (site WT; paper {:.0}) [{}] | MEHS grid {:.2} in vs {} [{}]",
                    cell.id,
                    cell.posh_pct,
                    our_posh_site,
                    our_posh_paper,
                    if posh_ok { "ok" } else { "MISS" },
                    our_mehs_in,
                    mehs_note,
                    if mehs_ok { "ok" } else { "MISS" },
                );
            }
            if site_had_cell {
                sites_with_cells.insert(site.to_string());
            }
        }

        println!(
            "gate: {compared} cells across {} sites | POSH within ±{}: {posh_passed}/{compared} \
             | MEHS within ±{} in: {mehs_passed}/{mehs_compared}",
            sites_with_cells.len(),
            policy::POSH_TOLERANCE_PTS,
            policy::MEHS_TOLERANCE_IN,
        );
        if !policy::gate_is_conclusive(compared, sites_with_cells.len()) {
            println!(
                "INCONCLUSIVE: need ≥{} cells across ≥{} sites — rerun against live \
                 convection or an archived severe hour (HAIL_GATE_AT)",
                policy::MIN_CELLS,
                policy::MIN_SITES,
            );
            return;
        }
        assert!(
            policy::share_passes(posh_passed, compared),
            "POSH sanity gate: {posh_passed}/{compared} within ±{} points",
            policy::POSH_TOLERANCE_PTS,
        );
        assert!(
            policy::share_passes(mehs_passed, mehs_compared),
            "MEHS sanity gate: {mehs_passed}/{mehs_compared} within ±{} in",
            policy::MEHS_TOLERANCE_IN,
        );
    }
}
