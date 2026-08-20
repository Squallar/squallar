//! Digital VIL (the RPG's product 134, "HRVIL", AWIPS `DVL`) computed locally
//! from the Level II reflectivity volume.
//!
//! # What is implemented, and from which documents
//!
//! **The governing document is FMH-11 Part C § 3.2.6**, "Digital High
//! Resolution Vertically Integrated Liquid Water" (Federal Meteorological
//! Handbook No. 11, *WSR-88D Products and Algorithms*, FCM-H11C-2017, OFCM,
//! October 2017, pp. 3-13 – 3-14).
//!
//! **Flow** — ORPG man pages `hrvil(1)` and `hrvil(4)` (task `cpc014/tsk010`,
//! High Resolution VIL): per 1° × 1 km polar gate, a *partial VIL* is
//! computed for each elevation and summed as the volume completes; the total
//! is the product.
//!
//! **Cell statistic** — the largest sub-gate, [`CellStat::Max`]. FMH-11
//! § 3.2.6, verbatim:
//!
//! > Each column is populated by a set of range gate sample volumes from each
//! > intersected elevation tilt plane of the radar volume. The DVL determines
//! > the partial VIL contribution from each intersected elevation tilt plane
//! > of the column by **selecting the range gate sample volume with the
//! > largest reflectivity factor**, converting it to equivalent liquid water,
//! > and vertically integrating through the depth of the range gate sample
//! > volume.
//!
//! The legacy Fortran does the same thing in its own 4 km × 4 km box, and it
//! is worth reading because it is the one piece of this algorithm family whose
//! source is public. `a313g1.ftn`'s `A313G1__CART_MAP1` keeps a running
//! `MXLIQWAT` per box and **assigns** — never accumulates — the partial VIL:
//!
//! ```text
//! IF (LIQWAT(NBIN) .GT. MXLIQWAT(IH(W),JH(W))) THEN
//!   MXLIQWAT(IH(W),JH(W))=LIQWAT(NBIN)
//!   PTLVIL(IH(W),JH(W)) = FLOAT(LIQWAT(NBIN))*BEAM_DEPTH(NBIN)
//! END IF
//! ```
//!
//! So max-in-cell is the VIL family's convention in both the specification and
//! the only implementation of it anyone can read.
//!
//! **Liquid water** — Greene & Clark (1972), floating point, **uncapped**:
//!
//! ```text
//! LW = 3.44e-3 · Z^(4/7)   g/m³,   Z = 10^(dBZ/10)
//! ```
//!
//! FMH-11 § 3.2.6 again: DVL's partial VIL "is the same as that done for the
//! VIL Algorithm except that DVL uses **non-quantized** reflectivity factor
//! data and includes the conversion to VIL of reflectivity factor **below 18
//! dBZ threshold and above the greater dBZ (i.e., all reflectivity used)**."
//! That sentence settles three things at once, and all three were previously
//! open questions arbitrated from twins:
//!
//! * *non-quantized* → the unfloored analytic form, not the `A313B1` table;
//! * *below 18 dBZ included* → no participation gate;
//! * *above the greater dBZ* → **no 56 dBZ hail cap**.
//!
//! The legacy task reads the `A313B1` look-up table (`a313.inc`) instead —
//! verified here entry for entry to be this formula **floored at hundredths
//! of g/m³** and saturated at 5.40 from data level 178 (56.0 dBZ) up. That
//! floor and that saturation are the *legacy 16-level product's*.
//!
//! **Threshold** — none in the primary. The legacy task gates every sample
//! on `min_refl` 18.3 dBZ (`vil_echo_tops.alg`; `IREFMIN = NINT(2·18.3 +
//! 66) = 103` in `a313a1.ftn`, i.e. ≥ 18.5 dBZ on half-dB data).
//!
//! **Layer depths** — `A313T1__COMPUTE_DEPTH` (`a313t1.ftn`), reproduced
//! case for case with `RH = RS·cos φ` and `RS` the bin's **outer edge** in
//! km (the routine evaluates bin `J` at `RS = J` while the height table
//! `a313e1.ftn` uses centres — both kept):
//!
//! * **lowest tilt**: ground up to the angular midpoint of tilts 1–2,
//!   `RH·tan(φ_avg) + RH²/(2·(4/3)·6371·cos²φ_avg)` — the one place the
//!   depth table has a curvature term, and it is the **4/3 earth model**,
//!   *not* the 1.21·Re the same task uses for echo-top heights; each
//!   constant is copied from its own routine;
//! * **middle tilts**: flat-earth midpoint boundaries,
//!   `½·RH·(tan φ_above − tan φ_below)`;
//! * **highest tilt**: `½·RH·(tan(φ_top + BW/2) − tan φ_below)` with the
//!   routine's hardcoded `BW = 0.017` **radians** — the beam's upper flank
//!   caps the column, no extrapolation past the volume ceiling.
//!
//! A volume with a single reflectivity tilt follows the routine's actual
//! control flow: the top-tilt case overwrites the lowest-tilt case with an
//! unfound angle below (`tan 0`), giving `½·RH·tan(φ + BW/2)`.
//!
//! **Cut participation** — every reflectivity cut of the volume's first
//! pass: `viletalg.ftn` sets `ALLOW_SUPPL_SCANS = 0`, so the RPG's VIL never
//! sees SAILS/MRLE revisits and the cube is deduplicated
//! [`DedupPolicy::FirstOfVolume`], with layer depths built from the
//! SAILS-free elevation ladder — the same ladder the routine builds from the
//! "local" VCP definition.
//!
//! **Units** — kg/m² (g/m³ · km), summed in f64 per column. The legacy
//! task's per-elevation `NINT` to whole kg/m² (`a313d1.ftn`) is a
//! 16-level-product encoding artifact and is *not* applied.
//!
//! # Documented gaps against the RPG
//!
//! * **Input** is raw Level II reflectivity, not the DQA-edited buffer
//!   (`dqa(1)`, product 297) HRVIL consumes.
//! * **Elevation angles**: the RPG builds its depth table from the VCP's
//!   nominal angles in tenths of degrees; here each sweep's measured median
//!   elevation is used (the antenna's real ladder, within a few hundredths
//!   of a degree of nominal).

use crate::types::RadarProduct;
use crate::volumetric::{
    CellStat, DedupPolicy, RANGE_BINS, VolumeCube, VolumetricGrid, sweep_elevation_deg,
};
use nexrad_model::data::Scan;

/// The legacy VIL task's reflectivity gate, dBZ: the `alg.vil_echo_tops
/// min_refl` fleet default, applied by `a313f1.ftn` as `IREFMIN`. The
/// primary derivation does **not** apply it.
pub const VIL_MIN_REFL_DBZ: f32 = 18.3;

/// Greene & Clark's coefficient: `LW = 3.44e-3 · Z^(4/7)` g/m³.
const GREENE_CLARK_COEFF: f64 = 3.44e-3;

/// The `A313B1` table's saturation value, hundredths of g/m³: every data
/// level from 178 (56.0 dBZ) up maps to 540. This is the **legacy** 16-level
/// product's hail cap and belongs only to [`LwMapping::TableFloor`], which
/// models it; product 134 uses all reflectivity (FMH-11 Part C § 3.2.6).
const LW_CAP_HUNDREDTHS: f64 = 540.0;

/// `A313T1__COMPUTE_DEPTH`'s hardcoded beamwidth, **radians** (`BW = .017`):
/// the top layer extends half of this above the highest tilt's centre.
const A313T1_BEAMWIDTH_RAD: f64 = 0.017;

/// Earth radius (km) and the 4/3 refraction factor of the depth table's one
/// curvature term (`RE = 6371.0`, `FOURP/THREEP` in `a313t1.ftn`). The echo
/// top height table uses 1.21·Re instead; each is faithful to its source.
const RE_KM: f64 = 6371.0;
const FOUR_THIRDS: f64 = 4.0 / 3.0;

/// How a gate's dBZ becomes liquid water — an A/B knob of the harness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LwMapping {
    /// The `A313B1` table's exact semantics: floored at hundredths of g/m³
    /// (zero below 8.5 dBZ), saturated at 5.40. The legacy 16-level
/// product's quantization, kept as an A/B variant.
    #[cfg_attr(not(test), allow(dead_code))]
    TableFloor,
    /// Greene–Clark in floating point over the **whole** dBZ range — no
    /// floor and no 56 dBZ hail cap. The primary, and both halves of that
    /// are specified: FMH-11 Part C § 3.2.6 has DVL using "non-quantized
    /// reflectivity factor data" and converting reflectivity "below 18 dBZ
/// threshold and above the greater dBZ (i.e., all reflectivity used)".
    Analytic,
}

/// The conventions [`compute_vil`] pins; the harness varies them.
#[derive(Debug, Clone, Copy)]
struct VilOptions {
    /// How super-resolution gates collapse into a 1° × 1 km cell.
    stat: CellStat,
    lw: LwMapping,
    /// `false`: depth evaluated at the bin's outer edge (`RS = J`, the
    /// `a313t1.ftn` literal). `true`: at the bin centre, the `a313e1.ftn`
    /// height-table reading of the same bin.
    depth_at_centre: bool,
    /// A participation gate on each dBZ sample: `Some(18.3)` is the legacy
    /// task's `IREFMIN` (`a313f1.ftn`); `None` lets every valid gate through
/// to the LWC table, whose own floor zeroes everything below 8.5 dBZ.
    min_refl: Option<f32>,
    /// `true`: a column with no participating gate is undefined — the
    /// legacy 4×4 km product's background. `false`: any column carrying
    /// valid reflectivity is defined, at 0.0 if nothing contributes — the
/// convention product 134 encodes (its level 2 is a defined 0.0 kg/m²).
    echo_only: bool,
}

impl VilOptions {
    /// The primary: the largest sub-gate in the cell, floating-point
    /// Greene–Clark, depths at the outer bin edge, no participation gate,
/// every data-carrying column defined.
    const fn primary() -> Self {
        Self {
            stat: CellStat::Max,
            lw: LwMapping::Analytic,
            depth_at_centre: false,
            min_refl: None,
            echo_only: false,
        }
    }

    /// The legacy `cpc013` reading: the floored LWC table, `IREFMIN`-gated,
/// background undefined.
    #[cfg_attr(not(test), allow(dead_code))]
    const fn legacy_threshold() -> Self {
        Self {
            lw: LwMapping::TableFloor,
            min_refl: Some(VIL_MIN_REFL_DBZ),
            echo_only: true,
            ..Self::primary()
        }
    }
}

/// Liquid water for one gate, g/m³. The caller applies the reflectivity
/// threshold; this is only the mapping.
fn liquid_water_g_m3(dbz: f32, mapping: LwMapping) -> f64 {
    let lw = GREENE_CLARK_COEFF * 10f64.powf(f64::from(dbz) * 4.0 / 70.0);
    match mapping {
        LwMapping::TableFloor => (lw * 100.0).floor().min(LW_CAP_HUNDREDTHS) / 100.0,
        LwMapping::Analytic => lw,
    }
}

/// The `A313T1__COMPUTE_DEPTH` table: per tilt of the ascending elevation
/// ladder, the layer depth in km at every range cell. Empty for an empty
/// ladder.
fn layer_depths_km(elevs_deg: &[f64], depth_at_centre: bool) -> Vec<Vec<f64>> {
    let n = elevs_deg.len();
    let phi: Vec<f64> = elevs_deg.iter().map(|e| e.to_radians()).collect();
    (0..n)
        .map(|i| {
            (0..RANGE_BINS)
                .map(|r| {
                    let rs = r as f64 + if depth_at_centre { 0.5 } else { 1.0 };
                    if n == 1 {
                        // The routine's real control flow for one tilt: the
                        // top case overwrites the lowest case, with the
                        // unfound angle below reading tan 0.
                        let rh = rs * phi[0].cos();
                        return 0.5 * rh * (phi[0] + A313T1_BEAMWIDTH_RAD / 2.0).tan();
                    }
                    if i == 0 {
                        // Ground up to the midpoint of the two lowest tilts,
                        // through the 4/3 earth: the table's one curvature
                        // term.
                        let phi_avg = (phi[0] + phi[1]) / 2.0;
                        let rh = rs * phi[0].cos();
                        rh * phi_avg.tan()
                            + rh * rh / (2.0 * FOUR_THIRDS * RE_KM * phi_avg.cos().powi(2))
                    } else if i + 1 == n {
                        // The beam's upper flank caps the column.
                        let rh = rs * phi[i].cos();
                        0.5 * rh * ((phi[i] + A313T1_BEAMWIDTH_RAD / 2.0).tan() - phi[i - 1].tan())
                    } else {
                        // Flat-earth midpoint boundaries.
                        let rh = rs * phi[i].cos();
                        0.5 * rh * (phi[i + 1].tan() - phi[i - 1].tan())
                    }
                })
                .collect()
        })
        .collect()
}

/// Compute Digital VIL from a Level II volume, per the rules in the module
/// doc: per 1° × 1 km cell, `Σ LW(dBZ) · Δh` over the tilts whose gate meets
/// the threshold, in kg/m². `NaN` where no tilt in the column does.
pub fn compute_vil(scan: &Scan) -> VolumetricGrid {
    compute_vil_impl(scan, VilOptions::primary())
}

fn compute_vil_impl(scan: &Scan, opts: VilOptions) -> VolumetricGrid {
    // Slant, for the reason `crate::eet` gives at its own build: this is the
    // RPG's product 134 reproduced bin for bin, and the RPG bins slant.
    let cube = VolumeCube::build_with_stats(
        scan,
        &[(RadarProduct::Reflectivity, opts.stat)],
        DedupPolicy::FirstOfVolume,
        crate::volumetric::RangeBinning::Slant,
    );

    // The tilts carrying reflectivity, ascending, each at its *actual*
    // elevation — the sweep's median radial angle, as in `crate::eet` (the
    // cube key is rounded to 0.1°).
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
    let depths = layer_depths_km(&elevs, opts.depth_at_centre);

    let mut values = vec![vec![f32::NAN; RANGE_BINS]; 360];
    for (az, row) in values.iter_mut().enumerate() {
        for (r, cell) in row.iter_mut().enumerate() {
            let mut sum = 0.0f64;
            let mut any_valid = false;
            let mut any_participating = false;
            for (ti, &(_, dbz)) in tilts.iter().enumerate() {
                let z = dbz[az][r];
                if z.is_nan() {
                    continue;
                }
                any_valid = true;
                if opts.min_refl.is_some_and(|t| z < t) {
                    continue;
                }
                sum += liquid_water_g_m3(z, opts.lw) * depths[ti][r];
                any_participating = true;
            }
            let defined = if opts.echo_only {
                any_participating
            } else {
                any_valid
            };
            if defined {
                *cell = sum as f32;
            }
        }
    }
    VolumetricGrid {
        values,
        range_bins: RANGE_BINS,
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

    /// Four tilts at 0.5°/1.5°/2.5°/3.5° exercising every column rule:
    ///
    /// * az 10: 50 dBZ on all four tilts — a **multi-layer** column summing
    ///   every depth, topped off at the volume ceiling (the top layer stops
    ///   at the beam's upper flank, nothing is extrapolated);
    /// * az 20: 40/30/20/10 dBZ — a graded column down to weak echo;
    /// * az 30: 25/25/censored/10 — a censored gate mid-column, which simply
    ///   drops out of the sum;
    /// * az 40: 15 dBZ everywhere — weak echo, in the sum under the primary
    ///   (0.0248 g/m³) and background under the legacy `IREFMIN` gate;
    /// * az 45: 5 dBZ everywhere — a tiny liquid water under the primary's
    ///   unfloored mapping, a **defined 0.0** under the table's 8.5 dBZ zero
    ///   floor, `NaN` under echo-only;
    /// * az 50: censored everywhere — **no VIL** under every convention;
    /// * az 60/61/62: 60/56/55.5 dBZ on the lowest tilt only — the 56 dBZ
    ///   **cap**: 60 and 56 dBZ both map to 5.40 g/m³, 55.5 to its own 5.10.
    pub(super) fn golden_scan() -> Scan {
        let profile = |tilt: usize| {
            move |az: usize| -> Option<f64> {
                match az {
                    10 => Some(50.0),
                    20 => Some([40.0, 30.0, 20.0, 10.0][tilt]),
                    30 => [Some(25.0), Some(25.0), None, Some(10.0)][tilt],
                    40 => Some(15.0),
                    45 => Some(5.0),
                    60 => (tilt == 0).then_some(60.0),
                    61 => (tilt == 0).then_some(56.0),
                    62 => (tilt == 0).then_some(55.5),
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

    /// The documented rules against hand-computed VIL.
    ///
    /// All expectations use the constants pinned in the module: depths at
    /// bin 30's outer edge (RS = 31 km) for the 0.5/1.5/2.5/3.5° ladder are
    /// 0.597664238 / 0.541292613 / 0.541622832 / 0.402838053 km (Σd =
    /// 2.083418), and the unfloored LWs are 2.4757187 (50 dBZ), 0.66416
    /// (40), 0.1781739 (30), 0.0922847 (25), 0.0477986 (20), 0.0247572
    /// (15), 0.0128229 (10), 0.0066416 (5), 9.2284735 (60), 5.4520326 (56),
    /// 5.1048974 (55.5) g/m³ — the mapping is uncapped, so 60 dBZ is its own
    /// value and not 56's:
    ///
    /// * az 10: LW(50) · Σd = **5.157956** kg/m²;
    /// * az 20: Σ LWᵢ·dᵢ over 40/30/20/10 = **0.524443**;
    /// * az 30: LW(25)·(d₀ + d₁) + LW(10)·d₃ = **0.110274**;
    /// * az 40: LW(15) · Σd = **0.051580**;
    /// * az 45: LW(5) · Σd = **0.013837** — no zero floor in the primary
    ///   mapping;
    /// * az 60: 9.2284735·d₀ = **5.515529**, and az 61: 5.4520326·d₀ =
    ///   **3.258485** — with no hail truncation, 60 dBZ and 56 dBZ are
    ///   distinguishable, which under the old cap they were not;
    /// * az 62: 5.1048974·d₀ = **3.051015**.
    #[test]
    fn the_documented_rpg_rules_produce_hand_computed_vil() {
        let grid = compute_vil(&golden_scan());
        assert_eq!(grid.range_bins, RANGE_BINS);
        assert_eq!(grid.values.len(), 360);

        let r = 30;
        assert!(
            (grid.values[10][r] - 5.157_956).abs() < 1e-4,
            "multi-layer column: got {}",
            grid.values[10][r],
        );
        assert!(
            (grid.values[20][r] - 0.524_443).abs() < 1e-5,
            "graded column: got {}",
            grid.values[20][r],
        );
        assert!(
            (grid.values[30][r] - 0.110_274).abs() < 1e-5,
            "censored mid-column gate must drop out: got {}",
            grid.values[30][r],
        );
        assert!(
            (grid.values[40][r] - 0.051_580).abs() < 1e-6,
            "15 dBZ weak echo integrates under the primary: got {}",
            grid.values[40][r],
        );
        assert!(
            (grid.values[45][r] - 0.013_837).abs() < 1e-6,
            "5 dBZ carries its tiny unfloored liquid water: got {}",
            grid.values[45][r],
        );

        assert!(grid.values[50][r].is_nan(), "a censored column made VIL");
        assert!(grid.values[10][GATES].is_nan(), "beyond the data extent");

        // No cap: 60 dBZ must now out-run 56 dBZ rather than tie with it.
        // This is the pin that fails if the legacy hail truncation is ever
        // reintroduced into the primary mapping.
        assert!(
            grid.values[60][r] > grid.values[61][r],
            "the 56 dBZ cap is gone: 60 dBZ ({}) must exceed 56 dBZ ({})",
            grid.values[60][r],
            grid.values[61][r],
        );
        assert!((grid.values[60][r] - 5.515_529).abs() < 1e-4);
        assert!((grid.values[61][r] - 3.258_485).abs() < 1e-4);
        assert!(
            (grid.values[62][r] - 3.051_015).abs() < 1e-4,
            "55.5 dBZ sits under the cap at its own value: got {}",
            grid.values[62][r],
        );
    }

    /// The legacy `cpc013` reading, kept as an A/B variant: the floored LWC
    /// table (2.47 / 0.17 / 0.09 / 0.66 / 0.04 hundredth-floored g/m³), the
    /// `IREFMIN` gate dropping sub-18.3 dBZ gates from the sum, and
    /// echo-free columns undefined.
    #[test]
    fn the_legacy_threshold_variant_gates_at_18_3_dbz() {
        let grid = compute_vil_impl(&golden_scan(), VilOptions::legacy_threshold());
        let r = 30;
        // az 20's 20 dBZ layer stays, its 10 dBZ ceiling goes:
        // 0.66·d₀ + 0.17·d₁ + 0.04·d₂ = 0.508143.
        assert!(
            (grid.values[20][r] - 0.508_143).abs() < 1e-5,
            "got {}",
            grid.values[20][r],
        );
        // az 30 loses its 10 dBZ ceiling too: 0.09·(d₀ + d₁) = 0.102506.
        assert!((grid.values[30][r] - 0.102_506).abs() < 1e-5);
        assert!(
            grid.values[40][r].is_nan(),
            "15 dBZ background is undefined under the legacy gate",
        );
        assert!(grid.values[45][r].is_nan());
        assert!(grid.values[50][r].is_nan());
        // The table's 56 dBZ saturation: 5.40·d₀ = 3.227387, under the
        // primary's unfloored 3.258485.
        assert!((grid.values[60][r] - 3.227_387).abs() < 1e-4);
        // And 50 dBZ floors to 2.47: 2.47·Σd = 5.146042.
        assert!((grid.values[10][r] - 5.146_042).abs() < 1e-4);
    }

    /// The `A313B1` table literals, straight from `a313.inc` — not from the
    /// formula the implementation uses, so a drifted coefficient, a round
    /// where the table floors, or a mis-set cap each break a different pin.
    /// Data level b encodes (b − 66)/2 dBZ; the table value is hundredths of
    /// g/m³.
    #[test]
    fn liquid_water_reproduces_the_rpgs_lwc_table() {
        for (byte, hundredths) in [
            (82u16, 0.0), // 8.0 dBZ: first zero of the ramp
            (83, 1.0),    // 8.5 dBZ: first nonzero
            (106, 4.0),   // 20 dBZ
            (122, 13.0),  // 28 dBZ
            (130, 23.0),  // 32 dBZ
            (161, 178.0), // 47.5 dBZ — ⌊178.24⌋, a round would say 178 too,
            (160, 166.0), // 47 dBZ — ⌊166.88⌋, but a round would say 167
            (170, 322.0), // 52 dBZ
            (176, 477.0), // 55 dBZ
            (177, 510.0), // 55.5 dBZ: the last unfloored level
            (178, 540.0), // 56 dBZ: the cap (analytic would floor 545)
            (254, 540.0), // 94 dBZ: still the cap
        ] {
            let dbz = (f32::from(byte) - 66.0) / 2.0;
            let got = liquid_water_g_m3(dbz, LwMapping::TableFloor) * 100.0;
            assert!(
                (got - hundredths).abs() < 1e-9,
                "LWC({byte}) = {got}, table says {hundredths}",
            );
        }
        // The analytic variant is unfloored and **uncapped**: FMH-11 Part C
        // § 3.2.6 has DVL converting reflectivity "above the greater dBZ
        // (i.e., all reflectivity used)", so 94 dBZ keeps rising past the
        // legacy table's 5.40 saturation instead of stopping at 56 dBZ's
        // 5.452. Greene–Clark at 94 dBZ: 3.44e-3 · 10^(94·4/70).
        let analytic_94 = liquid_water_g_m3(94.0, LwMapping::Analytic);
        assert!(
            (analytic_94 - 809.071_706_464).abs() < 1e-6,
            "uncapped Greene-Clark at 94 dBZ: got {analytic_94}",
        );
        // 56 dBZ is no longer a ceiling, only a point on the curve.
        let analytic_56 = liquid_water_g_m3(56.0, LwMapping::Analytic);
        assert!((analytic_56 - 5.452_032_582).abs() < 1e-6);
        assert!(
            analytic_94 > analytic_56,
            "the cap is gone: 94 dBZ must exceed 56 dBZ",
        );
        assert!(analytic_94 > liquid_water_g_m3(94.0, LwMapping::TableFloor));
    }

    /// `A313T1__COMPUTE_DEPTH`'s three cases against hand-computed depths at
    /// bin 100 (outer edge RS = 101 km), ladder 0.5/1.5/2.5/3.5°:
    ///
    /// * lowest: RH·tan 1° + RH²/(2·(4/3)·6371·cos²1°) = **2.363467** km;
    /// * middle: ½·101·cos 1.5°·(tan 2.5° − tan 0.5°) = **1.763566**, and
    ///   ½·101·cos 2.5°·(tan 3.5° − tan 1.5°) = **1.764642**;
    /// * top: ½·101·cos 3.5°·(tan(3.5° + 0.0085 rad) − tan 2.5°) =
    ///   **1.312472** — the 0.017 **radian** beamwidth, not degrees.
    #[test]
    fn layer_depths_follow_a313t1s_three_cases() {
        let d = layer_depths_km(&[0.5, 1.5, 2.5, 3.5], false);
        assert_eq!(d.len(), 4);
        assert_eq!(d[0].len(), RANGE_BINS);
        assert!((d[0][100] - 2.363_467_199).abs() < 1e-8);
        assert!((d[1][100] - 1.763_566_256).abs() < 1e-8);
        assert!((d[2][100] - 1.764_642_130).abs() < 1e-8);
        assert!((d[3][100] - 1.312_472_366).abs() < 1e-8);

        // Two tilts: the lowest and top cases meet with no middle.
        let two = layer_depths_km(&[0.5, 1.5], false);
        assert!((two[0][100] - 2.363_467_199).abs() < 1e-8);
        assert!((two[1][100] - 1.310_883_172).abs() < 1e-8);

        // One tilt: the routine's top case overwrites the lowest, reading
        // tan 0 below — ½·101·cos 0.5°·tan(0.5° + 0.0085 rad).
        let one = layer_depths_km(&[0.5], false);
        assert!((one[0][100] - 0.869_998_572).abs() < 1e-8);

        // The A/B centre datum shifts the whole table half a kilometre of
        // range: strictly smaller depths at every cell.
        let centre = layer_depths_km(&[0.5, 1.5, 2.5, 3.5], true);
        for (edge_row, centre_row) in d.iter().zip(&centre) {
            for (e, c) in edge_row.iter().zip(centre_row) {
                assert!(c < e);
            }
        }

        assert!(layer_depths_km(&[], false).is_empty());
    }

    /// A SAILS repeat late in the volume must not displace the first look:
    /// `viletalg.ftn` sets `ALLOW_SUPPL_SCANS = 0`, so the RPG's VIL is
    /// computed from the volume's first pass only.
    ///
    /// The repeat carries 50 dBZ where the first 0.5° look has 30 dBZ —
    /// LW 2.47 against 0.17 g/m³ — so a newest-wins dedup would move the
    /// answer by an order of magnitude, not just the provenance.
    #[test]
    fn a_sails_repeat_does_not_displace_the_first_look() {
        let first = |az: usize| (az == 61).then_some(30.0);
        let upper = |az: usize| (az == 61).then_some(10.0);
        let repeat = |az: usize| match az {
            60 => Some(50.0),
            61 => Some(50.0),
            _ => None,
        };
        let scan = Scan::new(
            vcp(),
            vec![
                refl_sweep(1, 0.5, first),
                refl_sweep(2, 1.5, upper),
                refl_sweep(3, 0.5, repeat), // SAILS revisit, late
            ],
        );
        let grid = compute_vil(&scan);

        // az 60 exists only on the repeat: first-of-volume leaves it empty.
        assert!(
            grid.values[60][30].is_nan(),
            "the SAILS repeat displaced the first look",
        );

        // az 61 uses the FIRST look's 30 dBZ on the two-tilt ladder's lowest
        // depth at RS = 31, plus the 10 dBZ ceiling:
        // 0.1781739 · 0.597664238 + 0.0128229 · 0.402350280 = 0.111647; the
        // repeat's 50 dBZ would put the first term at 1.479676.
        assert!(
            (grid.values[61][30] - 0.111_647).abs() < 1e-5,
            "got {} — the repeat's reflectivity leaked into the sum",
            grid.values[61][30],
        );
    }

    /// The A/B knobs really vary the conventions they name: on a uniform
    /// 56 dBZ column the floored table reads its saturated 5.40 against the
    /// primary's unfloored cap, and the centre depth datum shrinks every
    /// layer.
    #[test]
    fn the_ab_variants_move_in_the_documented_directions() {
        let scan = Scan::new(
            vcp(),
            vec![
                refl_sweep(1, 0.5, |az| (az == 10).then_some(56.0)),
                refl_sweep(2, 1.5, |az| (az == 10).then_some(56.0)),
            ],
        );
        let primary = compute_vil_impl(&scan, VilOptions::primary());
        let table = compute_vil_impl(
            &scan,
            VilOptions {
                lw: LwMapping::TableFloor,
                ..VilOptions::primary()
            },
        );
        let centred = compute_vil_impl(
            &scan,
            VilOptions {
                depth_at_centre: true,
                ..VilOptions::primary()
            },
        );
        let r = 30;
        // 5.40/5.452 exactly, since LW factors out of the whole column.
        let ratio = f64::from(table.values[10][r]) / f64::from(primary.values[10][r]);
        assert!((ratio - 5.40 / 5.452_032_582).abs() < 1e-4, "ratio {ratio}");
        assert!(centred.values[10][r] < primary.values[10][r]);
        // Uninvolved cells agree bit for bit: NaN everywhere else.
        assert!(primary.values[11][r].is_nan());
        assert!(table.values[11][r].is_nan());
    }

    /// Every convention of the primary is the one FMH-11 Part C § 3.2.6
    /// specifies, and this is the pin that fails if one drifts back.
    #[test]
    fn the_primary_conventions_are_the_ones_fmh11_specifies() {
        let p = VilOptions::primary();
        assert_eq!(
            p.stat,
            CellStat::Max,
            "FMH-11 § 3.2.6 selects the largest sample volume in the cell; \
             a linear-Z mean is the base-data recombination, not the VIL \
             algorithm's statistic, and reading it as one cost a flat 0.751",
        );
        assert_eq!(
            p.lw,
            LwMapping::Analytic,
            "'non-quantized' rules out the floored A313B1 table",
        );
        assert!(
            p.min_refl.is_none(),
            "'below 18 dBZ threshold ... included' rules out IREFMIN",
        );
        assert!(
            !p.echo_only,
            "product 134 defines every data-carrying column",
        );
        // The mapping carries no ceiling: 'above the greater dBZ' means the
        // legacy 56 dBZ hail truncation is not product 134's.
        assert!(
            liquid_water_g_m3(70.0, LwMapping::Analytic)
                > liquid_water_g_m3(56.0, LwMapping::Analytic),
            "the primary mapping must not truncate at 56 dBZ",
        );
    }

    /// The mechanism, demonstrated: a 1° × 1 km cell holding four 250 m
    /// sub-gates reads its **peak**, not their linear-Z mean.
    ///
    /// This is what the 0.751 was. The sub-gates here are 20/20/20/50 dBZ,
    /// a single hot gate in an otherwise weak cell — the sharp-gradient case
/// that deep convection supplies and a smooth stratiform volume does not.
    #[test]
    fn a_textured_cell_reads_its_largest_sub_gate() {
        const SUB: usize = 4;
        let byte = |dbz: f64| ((dbz * f64::from(SCALE) + f64::from(OFFSET)).round() as i64) as u8;
        // 250 m gates: four per 1 km cell. Cell 0 is 20/20/20/50 dBZ.
        let gates: Vec<u8> = vec![byte(20.0), byte(20.0), byte(20.0), byte(50.0)];
        let radials = (0..360)
            .map(|i| {
                Radial::new(
                    0,
                    i as u16,
                    i as f32 + 0.5,
                    1.0,
                    RadialStatus::IntermediateRadialData,
                    1,
                    0.5,
                    Some(MomentData::from_fixed_point(
                        SUB as u16,
                        0,
                        250,
                        8,
                        SCALE,
                        OFFSET,
                        gates.clone(),
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
        let scan = Scan::new(vcp(), vec![Sweep::new(1, radials)]);

        let peak = compute_vil_impl(&scan, VilOptions::primary());
        let mean = compute_vil_impl(
            &scan,
            VilOptions {
                stat: CellStat::LinearZMean,
                ..VilOptions::primary()
            },
        );

        let (p, m) = (peak.values[0][0], mean.values[0][0]);
        assert!(p.is_finite() && m.is_finite(), "cell 0 must be defined");
        assert!(
            p > m,
            "the largest sub-gate must beat their linear-Z mean: {p} vs {m}",
        );
        // Depth is common to both arms, so the ratio is purely the
        // statistic: LW(50 dBZ) against LW of the mean of 10^2,10^2,10^2,10^5.
        let z_mean = (3.0 * 10f64.powf(2.0) + 10f64.powf(5.0)) / 4.0;
        let expected = liquid_water_g_m3(50.0, LwMapping::Analytic)
            / liquid_water_g_m3(10.0 * z_mean.log10() as f32, LwMapping::Analytic);
        let ratio = f64::from(p) / f64::from(m);
        assert!(
            (ratio - expected).abs() < 1e-3,
            "ratio {ratio} should be the pure LW ratio {expected}",
        );
    }
}
