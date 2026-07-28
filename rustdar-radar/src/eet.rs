//! Enhanced Echo Tops (the RPG's product 135, "HREET") computed locally from
//! the Level II reflectivity volume.
//!
//! # What is implemented, and from which documents
//!
//! **Algorithm rules** — ORPG man page `hireseet(1)` (task `cpc014/tsk012`,
//! High Resolution Enhanced Echo Tops), from the WSR-88D CODE distribution:
//! per column of a 1° × 1 km polar grid, scanning the volume's elevations,
//!
//! * a gate **equal to** the echo-top threshold with every higher tilt below
//!   it puts the top at *that gate's altitude*;
//! * a gate **above** the threshold whose adjacent tilt above is *below* it
//!   interpolates the top linearly between the two adjacent tilts;
//! * the top is **topped** when the highest available tilt is still above the
//!   threshold — the storm extends past the volume's ceiling.
//!
//! Both non-topped rules are one formula here: the interpolation fraction
//! `(z − t)/(z − z_up)` is zero when `z == t`, which lands exactly on the
//! lower gate's altitude.
//!
//! **Threshold** — 18.3 dBZ, the fleet default for `alg.vil_echo_tops
//! min_refl` (`vil_echo_tops.alg`; a live KTLX EET PDB annotates it as the
//! truncated `18`). Site-adaptable in principle; the twin harness below
//! measures whether 18.3 holds in practice.
//!
//! **Altitude and datum** — the RPG's own height computation, from the legacy
//! VIL/Echo Tops source (`a313e1.ftn`):
//!
//! ```text
//! PRESHGT = RS·(SINEL + RS·INREXINR) + RADHTKFT      INREXINR = 6.4860e-5 /km
//! ```
//!
//! i.e. beam-**centre** height at the bin **centre** (`r + 0.5` km slant
//! range), through an effective earth radius of `1/(2·6.4860e-5) km` =
//! **1.21 · 6371 km** — the RPG's refraction model for this product family,
//! *not* the 4/3 model the rest of this crate draws beams with — plus the
//! radar height above mean sea level. Output is therefore **kft above MSL**,
//! which is also what the Level III EET twin encodes.
//!
//! **Encoding** — ICD for the RPG to Class 1 User (2620001), product 135
//! amendment shipped as `doc/eet.doc` in the ORPG source ("Documentation for
//! the High Resolution Enhanced Echo Tops (HREET)"): levels 0/1 are below
//! threshold and bad data; levels 2–71 are echo tops `0 ≤ EET < 70` kft in
//! 1 kft bins (level = ⌊kft⌋ + 2); the topped set 130–199 repeats them with
//! bit 7 set; any top at or above 70 kft becomes level 1. The PDB declares
//! DATA_MASK 0x7F / DATA_SCALE 1 / DATA_OFFSET 2 / TOPPED_MASK 0x80 in
//! threshold halfwords 31–34, and the decode is
//! `value = ((data & 0x7F)/1) − 2`, `topped = data & 0x80` — verified against
//! a live `TLX_EET` object (packet 16, 360 × 1° radials, 1 km gates, 346
//! bins, thresholds `[127, 1, 2, 128]`).
//!
//! The crate's Level III render path and twin codec decode 135 through
//! `l3_values::build_eet_lut`, which reads exactly these four threshold
//! halfwords. (They once fell back to the PDB's scale 1 / offset 0 — 135's
//! thresholds are not IEEE floats — which painted every bin 2 kft high and
//! topped bins as absurd 130–199 kft heights.)
//!
//! # Documented gaps against the RPG
//!
//! * **Input** is raw Level II reflectivity, not the DQA-edited buffer the
//!   RPG feeds HREET, so AP and constant-power artifacts the DQA would remove
//!   can produce tops here. The harness's presence-disagreement gate is what
//!   bounds this.
//! * **"Bad data above" topped rule**: HREET also declares a top topped when
//!   the adjacent tilt above holds DQA *bad data*. Raw Level II cannot tell
//!   "artifact-edited" from "below SNR" — both are simply censored — and
//!   treating every censored-above column as topped would flag vast areas the
//!   RPG does not (a live TLX volume carried 12 topped bins in 124,560).
//!   Here **only the volume's highest tilt makes a top topped**; a censored
//!   cell above clamps the top to the crossing tilt's own altitude,
//!   non-topped. Topped-flag agreement is printed by the harness so the gap
//!   stays measured.
//! * **SAILS/MRLE revisits**: HREET consumes each elevation's DQA buffer once
//!   as the volume completes, so the cube is deduplicated
//!   [`DedupPolicy::FirstOfVolume`] — the coherent first pass the RPG's
//!   volume products are computed from, not the freshest look. (Measured
//!   against a live KMRX 212 twin, newest-wins changes the score by under a
//!   tenth of a point either way, so the choice is by the doc, uncontradicted.)
//! * **Cell statistic** — twin-arbitrated, not documented: each 1° × 1 km
//!   cell takes the **maximum** dBZ of its sub-gates ([`CellStat::Max`]).
//!   The documented recombination average (linear-Z mean) reads 1.5 data
//!   levels *lower* against a live KMRX EET twin (mean level bias −2.75
//!   against −1.23, within-±1 43% against 58%), and leaves thousands of bins
//!   undefined that the twin defines whose column maximum sits at 14–18 dBZ —
//!   just under threshold. The same finding as the SRM campaign's range
//!   recombination: the RPG keeps peaks.
//!
//! # Validation status — read before trusting the twin harness to pass
//!
//! The live harness below holds this derivation to the campaign bar (99%
//! within one level, per site) against the RPG's own EET for the same
//! volume. As of the 2026-07-28 survey it does **not** meet that bar on
//! convective volumes: clear-air/weak sites read 99–100% within-±1, but
//! sites with real storms plateau at 60–80% with a storm-depth-dependent
//! low bias, and the twin defines 15–30% more bins than any per-column
//! recomputation of the same Level II data can (its extra bins sit at
//! column maxima of 14–18 dBZ). The twin's field is also visibly smoother
//! than a raw column scan — flat 2–3-level plateaus across cores.
//!
//! Ruled out by measurement (single-volume A/B against the KMRX/KSGF/KMLB
//! twins, each change isolated): dedup policy (first against newest —
//! indistinguishable); datum (MSL confirmed: near-radar twin bins encode
//! site-elevation heights, low-top bins agree to a quarter level); beam top
//! against beam centre (+0.475–0.5° overshoots, range-proportionally);
//! linear-Z interpolation (centres the mean but widens the spread); azimuth
//! registration (cells cover [k, k+1)°, the centred alternative is worse);
//! range sub-column lanes, azimuth pooling across the half-degree radials,
//! ground-projected (flat-polar) sampling, interpolating toward a floor when
//! censored above, and 3×3 median/max input and output filters — each moves
//! the score by single points, none closes it. The remaining candidate is
//! HREET's own pre/post-processing (its input is the DQA buffer and its
//! source, `cpc014/tsk012`, is not in any public CODE distribution), so the
//! residual is recorded here rather than papered over: do not lower the bar,
//! and do not calibrate further heuristics against a single twin volume.

use crate::types::RadarProduct;
use crate::volumetric::{CellStat, DedupPolicy, RANGE_BINS, VolumeCube};
use nexrad_model::data::Scan;

/// Echo-top reflectivity threshold, dBZ: the `alg.vil_echo_tops min_refl`
/// fleet default.
pub const EET_THRESHOLD_DBZ: f32 = 18.3;

/// The RPG's quadratic beam-height coefficient, 1/km: `INREXINR` from
/// `a313e1.ftn`, equal to `1/(2 · 1.21 · 6371 km)`. Deliberately not the
/// crate-wide 4/3 model — the twin encodes *this* one, and the two differ by
/// a full data level at 230 km.
const RPG_HEIGHT_QUADRATIC_PER_KM: f64 = 0.000_064_860;

const KM_TO_KFT: f64 = 3.28084;
const FT_TO_KFT: f64 = 0.001;

/// Tops at or above this many kft encode as level 1 ("bad data") per the ICD.
pub const MAX_EET_KFT: f32 = 70.0;

const LEVEL_OFFSET: u16 = 2;
const TOPPED_FLAG: u16 = 0x80;
/// The ICD's DATA_MASK: the height bits of a packed EET byte.
pub const EET_DATA_MASK: u16 = 0x7F;

/// The derived Enhanced Echo Tops field: per 1° × 1 km cell, the echo-top
/// altitude in **kft above MSL** (`NaN` where no echo reaches the threshold)
/// and whether that top is *topped* (echo still above threshold at the
/// volume's highest tilt).
pub struct EetGrid {
    /// `[az_deg][range_km]`, kft above MSL, `NaN` undefined.
    pub values: Vec<Vec<f32>>,
    /// Paired with [`values`](Self::values); meaningful only where defined.
    pub topped: Vec<Vec<bool>>,
    pub range_bins: usize,
}

/// Beam-centre altitude in kft above MSL at a slant range, using the RPG's
/// own constants (see the module doc): `h = r·sin θ + r²·6.4860e-5` km above
/// the radar, plus the radar height.
fn beam_centre_kft_msl(range_km: f64, elev_deg: f64, radar_height_kft: f64) -> f64 {
    let h_km =
        range_km * elev_deg.to_radians().sin() + range_km * range_km * RPG_HEIGHT_QUADRATIC_PER_KM;
    h_km * KM_TO_KFT + radar_height_kft
}

/// Pack a derived top into the ICD's product-135 data level: 0 for no top,
/// 1 for ≥ 70 kft, otherwise `⌊kft⌋ + 2` (heights below 0 kft MSL clamp to
/// the 0-kft bin) with bit 7 for topped.
pub fn encode_level(value_kft: f32, topped: bool) -> u16 {
    if value_kft.is_nan() {
        return 0;
    }
    if value_kft >= MAX_EET_KFT {
        return 1;
    }
    let level = (value_kft.floor() as i32).clamp(0, 69) as u16 + LEVEL_OFFSET;
    if topped { level | TOPPED_FLAG } else { level }
}

/// One reflectivity tilt of the cube, with its altitude table.
struct TiltView<'a> {
    /// Beam-centre altitude, kft MSL, per range cell.
    heights_kft: Vec<f64>,
    /// `[az][range]` reflectivity, dBZ.
    dbz: &'a [Vec<f32>],
}

/// Compute Enhanced Echo Tops from a Level II volume, per the rules in the
/// module doc. `radar_height_ft` is the radar height above MSL in feet — the
/// value the twin's PDB carries, or [`radar_height_ft_near`] for a render.
pub fn compute_eet(scan: &Scan, radar_height_ft: f64) -> EetGrid {
    let cube = VolumeCube::build_with_stats(
        scan,
        &[(RadarProduct::Reflectivity, CellStat::Max)],
        DedupPolicy::FirstOfVolume,
    );
    let radar_height_kft = radar_height_ft * FT_TO_KFT;

    // The tilts carrying reflectivity, ascending, each with altitudes at its
    // *actual* elevation angle — the sweep's median radial elevation. The
    // cube's key is rounded to 0.1°, which is 0.2 km of beam height at
    // 230 km — enough to matter against the twin.
    let tilts: Vec<TiltView> = cube
        .tilts
        .iter()
        .enumerate()
        .filter_map(|(ti, tilt)| {
            let grid = cube.grid(ti, RadarProduct::Reflectivity)?;
            let elev = scan
                .sweeps()
                .get(grid.sweep_index)
                .and_then(|s| crate::volumetric::sweep_elevation_deg(s.radials()))
                .unwrap_or(tilt.elevation_deg);
            Some(TiltView {
                heights_kft: (0..RANGE_BINS)
                    .map(|r| beam_centre_kft_msl(r as f64 + 0.5, elev, radar_height_kft))
                    .collect(),
                dbz: &grid.values,
            })
        })
        .collect();

    let mut values = vec![vec![f32::NAN; RANGE_BINS]; 360];
    let mut topped = vec![vec![false; RANGE_BINS]; 360];
    for (az, (row_v, row_t)) in values.iter_mut().zip(topped.iter_mut()).enumerate() {
        for (r, (cell_v, cell_t)) in row_v.iter_mut().zip(row_t.iter_mut()).enumerate() {
            // The topmost tilt meeting the threshold governs the column.
            for ti in (0..tilts.len()).rev() {
                let z = tilts[ti].dbz[az][r];
                if z.is_nan() || z < EET_THRESHOLD_DBZ {
                    continue;
                }
                let h = tilts[ti].heights_kft[r];
                let (kft, is_topped) = if ti + 1 == tilts.len() {
                    // Above threshold at the volume's ceiling: topped.
                    (h, true)
                } else {
                    let z_up = tilts[ti + 1].dbz[az][r];
                    if z_up.is_nan() {
                        // Censored above (below SNR in raw Level II): clamp
                        // to this tilt's altitude. See the module doc's
                        // "bad data above" gap — this is deliberately not
                        // marked topped.
                        (h, false)
                    } else {
                        // z_up < threshold, else ti would not be topmost.
                        let frac = (z - EET_THRESHOLD_DBZ) / (z - z_up);
                        let h_up = tilts[ti + 1].heights_kft[r];
                        (h + (h_up - h) * f64::from(frac), false)
                    }
                };
                *cell_v = kft as f32;
                *cell_t = is_topped;
                break;
            }
        }
    }
    EetGrid {
        values,
        topped,
        range_bins: RANGE_BINS,
    }
}

/// Radar height above MSL, in feet, of the site nearest a lat/lon — for the
/// render path, which knows only the coordinates. Sites without a recorded
/// elevation (and an empty table) fall back to 0 ft, which costs at most one
/// data level at the handful of sites affected.
pub fn radar_height_ft_near(lat: f64, lon: f64) -> f64 {
    crate::sites::RADARS
        .iter()
        .min_by(|a, b| {
            let da = (a.lat - lat).powi(2) + (a.lon - lon).powi(2);
            let db = (b.lat - lat).powi(2) + (b.lon - lon).powi(2);
            da.total_cmp(&db)
        })
        .and_then(|s| s.elev)
        .map_or(0.0, f64::from)
}

/// The parts of [`live_validation`] that decide **what counts as passing**,
/// plus the packet pre-processing that decision is made on.
///
/// Outside the ignored module for the reason `srm::validation_policy` is:
/// the live harness never runs under `cargo test --workspace`, so anything
/// defined inside it could be quietly weakened — the bar lowered, the topped
/// mask dropped — without a default-suite test noticing. Out here
/// `policy_tests` reaches all of it offline, and does.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod validation_policy {
    use nexrad_level3::model::RadialPacket;

    /// The acceptance bar: percent of compared bins within one RPG data level
    /// (1 kft). Lowering this is how a derivation that got worse ships
    /// anyway; it is pinned by `the_acceptance_bar_is_what_the_campaign_set`.
    pub const ACCEPTANCE_BAR_WITHIN_ONE_PCT: f64 = 99.0;

    /// Ceiling on cells defined on exactly one side, as a share of the union
    /// of defined cells — the DQA gap's budget.
    pub const PRESENCE_DISAGREEMENT_MAX_PCT: f64 = 2.0;

    /// A run concludes nothing until this many sites were actually asserted…
    pub const MIN_SITES: usize = 4;

    /// …and this many bins were compared, pooled across the asserted sites.
    pub const MIN_DEFINED_BINS: usize = 10_000;

    /// Volumes whose twin defines fewer bins than this are skipped, not
    /// scored: a clear-air volume's handful of bins measures nothing and a
    /// single speck disagreeing would swing whole percentage points. The same
    /// rule as the SRM harness's zero-vector controls — a skip is printed,
    /// never silent.
    pub const MIN_TWIN_DEFINED_BINS: usize = 500;

    pub fn meets_acceptance_bar(within_one_pct: f64) -> bool {
        within_one_pct >= ACCEPTANCE_BAR_WITHIN_ONE_PCT
    }

    pub fn presence_is_acceptable(presence_disagreement_pct: f64) -> bool {
        presence_disagreement_pct <= PRESENCE_DISAGREEMENT_MAX_PCT
    }

    pub fn volume_is_scoreable(twin_defined_bins: usize) -> bool {
        twin_defined_bins >= MIN_TWIN_DEFINED_BINS
    }

    pub fn sample_is_conclusive(sites_asserted: usize, pooled_compared_bins: usize) -> bool {
        sites_asserted >= MIN_SITES && pooled_compared_bins >= MIN_DEFINED_BINS
    }

    /// How much of a quarantined site stops being asserted on. EET is a
    /// volume product with no per-tilt figure, so the only scope is the whole
    /// site.
    #[derive(Debug, PartialEq, Eq, Clone, Copy)]
    pub enum Scope {
        Whole,
    }

    pub struct Quarantine {
        pub site: &'static str,
        pub scope: Scope,
        pub why: &'static str,
    }

    /// Sites measured to miss the bar, with what has been ruled out.
    ///
    /// Empty until the survey earns an entry: quarantining requires recorded
    /// evidence from **at least two volumes across at least two runs** — one
    /// run's miss is a lead, not a verdict (`KFSD` read 99.57% and 98.70% on
    /// different single runs of the SRM harness). A quarantined site stays in
    /// [`crate::twin::live::SITES`] and stays measured and printed; only the
    /// assertion is withheld. Never widen the bar instead.
    pub const QUARANTINED: &[Quarantine] = &[];

    pub fn quarantine(site: &str) -> Option<&'static Quarantine> {
        QUARANTINED.iter().find(|q| q.site == site)
    }

    /// Whether a site's tally may enter the run's assertions and its pooled
    /// conclusiveness figure.
    pub fn site_is_asserted(site: &str) -> bool {
        quarantine(site).is_none()
    }

    /// The twin packet with the topped bit stripped from every data level, so
    /// the tally compares **heights** in the product's own 1-kft levels.
    /// Levels 0 and 1 (below threshold / bad data) pass through untouched —
    /// they are the undefined codes the codec already maps to `NaN`.
    pub fn mask_heights_packet(packet: &RadialPacket) -> RadialPacket {
        let mut out = packet.clone();
        for radial in &mut out.radials {
            for gate in &mut radial.gate_values {
                if *gate >= 2 {
                    *gate &= super::EET_DATA_MASK;
                }
            }
        }
        out
    }

    /// The topped-class level: defined bins collapse to 130 (topped) or 2
    /// (not), so a tally over this packet's twin measures pure flag
    /// agreement — its `exact` count is the number of bins whose topped flag
    /// matches.
    pub const TOPPED_CLASS_LEVEL: u16 = 130;
    pub const NOT_TOPPED_CLASS_LEVEL: u16 = 2;

    /// The twin packet collapsed to the topped classes above.
    pub fn topped_class_packet(packet: &RadialPacket) -> RadialPacket {
        let mut out = packet.clone();
        for radial in &mut out.radials {
            for gate in &mut radial.gate_values {
                if *gate >= 2 {
                    *gate = if *gate & super::TOPPED_FLAG != 0 {
                        TOPPED_CLASS_LEVEL
                    } else {
                        NOT_TOPPED_CLASS_LEVEL
                    };
                }
            }
        }
        out
    }

    /// The derived grid as (height levels, topped classes), in the same
    /// domains as the two packets above: height level 2–71 as `f32` (`NaN`
    /// where the derivation is undefined *or* encodes level 1, which the twin
    /// also treats as undefined), and topped class 130/2.
    pub fn derived_level_grids(grid: &super::EetGrid) -> (Vec<Vec<f32>>, Vec<Vec<f32>>) {
        let mut heights = vec![vec![f32::NAN; grid.range_bins]; grid.values.len()];
        let mut topped = vec![vec![f32::NAN; grid.range_bins]; grid.values.len()];
        for (az, (row_v, row_t)) in grid.values.iter().zip(grid.topped.iter()).enumerate() {
            for (r, (&v, &t)) in row_v.iter().zip(row_t.iter()).enumerate() {
                let level = super::encode_level(v, t);
                if level < 2 {
                    continue;
                }
                heights[az][r] = f32::from(level & super::EET_DATA_MASK);
                topped[az][r] = if t {
                    f32::from(TOPPED_CLASS_LEVEL)
                } else {
                    f32::from(NOT_TOPPED_CLASS_LEVEL)
                };
            }
        }
        (heights, topped)
    }

    /// The number of bins the twin defines (levels ≥ 2) — what
    /// [`volume_is_scoreable`] gates on.
    pub fn twin_defined_bins(packet: &RadialPacket) -> usize {
        packet
            .radials
            .iter()
            .flat_map(|r| r.gate_values.iter())
            .filter(|&&g| g >= 2)
            .count()
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
    /// 1 kft exactly, so the MSL offset is legible in every expectation.
    const RADAR_HEIGHT_FT: f64 = 1000.0;

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
    /// * az 10: 50 dBZ on all four tilts — **topped** at the 3.5° ceiling;
    /// * az 20: 40/30/20/10 dBZ — the 2.5° tilt is topmost above 18.3, the
    ///   3.5° sample is 10 dBZ, so the top **interpolates** between them;
    /// * az 30: 25/25/censored/10 dBZ — censored above the topmost crossing,
    ///   so the top **clamps** to the 1.5° altitude and is *not* topped (the
    ///   documented DQA-bad-data gap);
    /// * az 40: 15 dBZ everywhere — below threshold, **no top**;
    /// * az 50: censored everywhere — **no top**.
    fn golden_scan() -> Scan {
        let profile = |tilt: usize| {
            move |az: usize| -> Option<f64> {
                match az {
                    10 => Some(50.0),
                    20 => Some([40.0, 30.0, 20.0, 10.0][tilt]),
                    30 => [Some(25.0), Some(25.0), None, Some(10.0)][tilt],
                    40 => Some(15.0),
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

    /// The documented rules against hand-computed altitudes.
    ///
    /// All expectations use the RPG constants pinned below: heights at bin
    /// centre 30.5 km, `h = r·sinθ + r²·6.4860e-5` km, × 3.28084, + 1 kft:
    ///
    /// * 3.5° → 1.9223165 km → **7.306813 kft** (az 10, topped);
    /// * 2.5° → 1.3907273 km, interpolated 17% of the way to 3.5° (fraction
    ///   (20 − 18.3)/(20 − 10)) → 1.4811028 km → **5.859244 kft** (az 20);
    /// * 1.5° → 0.8587329 km → **3.817365 kft** (az 30, clamped, not topped).
    #[test]
    fn the_documented_rpg_rules_produce_hand_computed_tops() {
        let grid = compute_eet(&golden_scan(), RADAR_HEIGHT_FT);
        assert_eq!(grid.range_bins, RANGE_BINS);
        assert_eq!(grid.values.len(), 360);

        let r = 30;
        assert!((grid.values[10][r] - 7.306_813).abs() < 1e-3, "topped col");
        assert!(grid.topped[10][r], "echo at the ceiling must be topped");

        assert!((grid.values[20][r] - 5.859_244).abs() < 1e-3, "interp col");
        assert!(!grid.topped[20][r]);

        assert!((grid.values[30][r] - 3.817_365).abs() < 1e-3, "clamped col");
        assert!(
            !grid.topped[30][r],
            "censored-above clamps without topping (the documented DQA gap)",
        );

        assert!(grid.values[40][r].is_nan(), "15 dBZ background topped");
        assert!(grid.values[50][r].is_nan(), "a censored column topped");
        assert!(grid.values[10][GATES].is_nan(), "beyond the data extent");

        // And the very levels the twin would see.
        assert_eq!(encode_level(grid.values[10][r], grid.topped[10][r]), 137);
        assert_eq!(encode_level(grid.values[20][r], grid.topped[20][r]), 7);
        assert_eq!(encode_level(grid.values[30][r], grid.topped[30][r]), 5);
    }

    /// The altitude formula is the RPG's, not this crate's beam model: at
    /// 100.5 km on a 0.5° tilt over a 1.213 kft site the RPG constants give
    /// 6.2396374 kft, while the crate's 4/3-earth model would give ~0.199 kft
    /// less — a fifth of a data level here and a full level at 230 km.
    #[test]
    fn beam_altitudes_use_the_rpgs_own_refraction_constant() {
        let got = beam_centre_kft_msl(100.5, 0.5, 1.213);
        assert!((got - 6.239_637_4).abs() < 1e-6, "got {got}");

        let four_thirds = (100.5 * (0.5f64).to_radians().sin()
            + 100.5 * 100.5 / (2.0 * 6371.0 * 4.0 / 3.0))
            * KM_TO_KFT
            + 1.213;
        assert!(
            (got - four_thirds).abs() > 0.15,
            "the two refraction models became indistinguishable — the pin \
             above no longer guards the constant",
        );
    }

    /// A SAILS repeat late in the volume must not displace the first look:
    /// the RPG computes volume products from the volume's first pass.
    ///
    /// The repeat carries 50 dBZ where the first 0.5° look has 30 dBZ, and
    /// the interpolation fraction depends on that value — (30−18.3)/20 of the
    /// 0.5°→1.5° gap against (50−18.3)/40 — so a newest-wins dedup would move
    /// the answer, not just the provenance.
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
        let grid = compute_eet(&scan, RADAR_HEIGHT_FT);

        // az 60 exists only on the repeat: first-of-volume leaves it empty.
        assert!(
            grid.values[60][30].is_nan(),
            "the SAILS repeat displaced the first look",
        );

        // az 61 interpolates from the FIRST look's 30 dBZ: fraction
        // 11.7/20 = 0.585 of h(0.5°)→h(1.5°), (0.3264953 + 0.585·0.5322376)
        // km → 2.0927 kft + 1 = 3.0927; the repeat's 50 dBZ would give
        // fraction 0.7925 and ~3.45 kft.
        assert!(
            (grid.values[61][30] - 3.092_698).abs() < 1e-3,
            "got {} — the repeat's reflectivity leaked into the interpolation",
            grid.values[61][30],
        );
    }

    /// The ICD bit layout, floor bins and clamps of [`encode_level`].
    #[test]
    fn encode_level_follows_the_icd_bit_layout() {
        assert_eq!(encode_level(f32::NAN, false), 0, "no top is level 0");
        assert_eq!(encode_level(f32::NAN, true), 0);
        assert_eq!(encode_level(70.0, false), 1, "≥ 70 kft is bad data");
        assert_eq!(encode_level(70.0, true), 1, "topped does not rescue it");
        assert_eq!(encode_level(123.4, false), 1);

        assert_eq!(encode_level(0.0, false), 2, "the 0-kft bin");
        assert_eq!(encode_level(0.99, false), 2, "floor bins, not rounding");
        assert_eq!(encode_level(1.0, false), 3);
        assert_eq!(encode_level(12.999, false), 14);
        assert_eq!(encode_level(13.0, false), 15);
        assert_eq!(encode_level(69.99, false), 71, "the last height level");
        assert_eq!(
            encode_level(-0.5, false),
            2,
            "below MSL clamps into the 0-kft bin",
        );

        assert_eq!(encode_level(5.2, true), 135, "topped sets bit 7");
        assert_eq!(encode_level(69.99, true), 199, "the last topped level");
        assert_eq!(encode_level(0.0, true), 130);
    }

    /// The render path's site lookup: the nearest site's elevation, 0 when
    /// the site records none.
    #[test]
    fn radar_height_lookup_finds_the_nearest_site() {
        // KTLX's own coordinates give KTLX's elevation.
        assert_eq!(radar_height_ft_near(35.33306, -97.2775), 1213.0);
        // A point nudged off-site still lands on it.
        assert_eq!(radar_height_ft_near(35.4, -97.2), 1213.0);
    }
}

/// Offline pins on the validation policy and the harness's packet
/// pre-processing — everything the ignored live test decides with.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod policy_tests {
    use super::validation_policy as policy;
    use nexrad_level3::model::{RadialPacket, RadialRun};

    fn packet(gate_values: Vec<u16>) -> RadialPacket {
        RadialPacket {
            first_range_bin: 0,
            num_range_bins: gate_values.len() as u16,
            i_center: 0,
            j_center: 0,
            scale_factor: 0.001, // what a live EET packet carries
            is_legacy: false,
            xdr_data_scale: None,
            xdr_data_offset: None,
            radials: vec![RadialRun {
                start_angle: 0.0,
                angle_delta: 1.0,
                gate_values,
            }],
        }
    }

    /// The campaign's bars, pinned so the ignored harness cannot drift them.
    #[test]
    fn the_acceptance_bar_is_what_the_campaign_set() {
        assert_eq!(policy::ACCEPTANCE_BAR_WITHIN_ONE_PCT, 99.0);
        assert_eq!(policy::PRESENCE_DISAGREEMENT_MAX_PCT, 2.0);
        assert_eq!(policy::MIN_SITES, 4);
        assert_eq!(policy::MIN_DEFINED_BINS, 10_000);
        assert_eq!(policy::MIN_TWIN_DEFINED_BINS, 500);

        assert!(policy::meets_acceptance_bar(99.0), "the bar is inclusive");
        assert!(!policy::meets_acceptance_bar(98.999_999));
        assert!(policy::presence_is_acceptable(2.0));
        assert!(!policy::presence_is_acceptable(2.000_001));
    }

    /// The conclusiveness fold: both legs required, boundaries inclusive.
    #[test]
    fn a_run_is_conclusive_only_with_enough_sites_and_bins() {
        assert!(policy::sample_is_conclusive(4, 10_000));
        assert!(!policy::sample_is_conclusive(3, 1_000_000), "sites gate");
        assert!(!policy::sample_is_conclusive(20, 9_999), "bins gate");
        assert!(!policy::sample_is_conclusive(0, 0));
    }

    /// The near-empty-volume skip rule.
    #[test]
    fn near_empty_volumes_are_skipped_not_scored() {
        assert!(!policy::volume_is_scoreable(0));
        assert!(!policy::volume_is_scoreable(499));
        assert!(policy::volume_is_scoreable(500));
    }

    /// The quarantine table starts empty, and any entry ever added must name
    /// a site the harness still measures — a quarantined site silently
    /// dropped from `SITES` is a site nobody would notice had got worse.
    #[test]
    fn the_quarantine_table_is_empty_and_would_stay_measured() {
        assert!(
            policy::QUARANTINED.is_empty(),
            "an entry appeared: it needs evidence from ≥2 volumes across ≥2 \
             runs recorded in its `why`, per the table's doc",
        );
        for q in policy::QUARANTINED {
            assert!(
                crate::twin::live::SITES.contains(&q.site),
                "{} ({:?}) is quarantined but no longer measured: {}",
                q.site,
                q.scope,
                q.why,
            );
        }
        assert!(policy::quarantine("KMPX").is_none());
        assert!(policy::site_is_asserted("KMPX"));
        // The only scope an entry could carry: whole-site. Constructed here
        // so the empty table does not leave the enum dead.
        assert_eq!(format!("{:?}", policy::Scope::Whole), "Whole");
    }

    /// Masking strips bit 7 from data levels and leaves the undefined codes
    /// alone, so a topped twin bin compares by its height.
    #[test]
    fn the_heights_mask_strips_the_topped_bit_only_from_data_levels() {
        let masked = policy::mask_heights_packet(&packet(vec![0, 1, 2, 71, 130, 199]));
        assert_eq!(masked.radials[0].gate_values, vec![0, 1, 2, 71, 2, 71]);
    }

    /// The topped-class collapse: defined bins become 130/2 by their flag,
    /// undefined codes stay undefined.
    #[test]
    fn the_topped_class_packet_collapses_to_flag_classes() {
        let classes = policy::topped_class_packet(&packet(vec![0, 1, 2, 71, 130, 199]));
        assert_eq!(classes.radials[0].gate_values, vec![0, 1, 2, 2, 130, 130]);
    }

    /// The derived grids encode through [`super::encode_level`]: heights as
    /// masked levels, topped as classes, ≥ 70 kft and NaN both undefined.
    #[test]
    fn derived_level_grids_mirror_the_twin_side_encodings() {
        let mut grid = super::EetGrid {
            values: vec![vec![f32::NAN; 4]; 1],
            topped: vec![vec![false; 4]; 1],
            range_bins: 4,
        };
        grid.values[0][0] = 12.7; // level 14
        grid.values[0][1] = 3.2; // level 5, topped → masked 5, class 130
        grid.topped[0][1] = true;
        grid.values[0][2] = 71.0; // ≥ 70 kft: level 1, undefined both ways
        // [0][3] stays NaN.

        let (heights, topped) = policy::derived_level_grids(&grid);
        assert_eq!(heights[0][0], 14.0);
        assert_eq!(topped[0][0], 2.0);
        assert_eq!(heights[0][1], 5.0, "the mask strips the topped bit");
        assert_eq!(topped[0][1], 130.0);
        assert!(heights[0][2].is_nan(), "≥ 70 kft is bad data, not a height");
        assert!(topped[0][2].is_nan());
        assert!(heights[0][3].is_nan());
        assert!(topped[0][3].is_nan());
    }

    /// The scoreability count reads defined twin bins, not bytes.
    #[test]
    fn twin_defined_bins_counts_data_levels_only() {
        assert_eq!(
            policy::twin_defined_bins(&packet(vec![0, 1, 2, 130, 0, 71])),
            3,
        );
    }
}

/// The live twin harness: score the derivation against the RPG's own EET for
/// the **same volume**, across [`crate::twin::live::SITES`].
///
/// ```text
/// cargo test -p rustdar-radar --release --lib -- --ignored --nocapture live_derived_eet
/// ```
#[cfg(all(test, not(target_arch = "wasm32")))]
mod live_validation {
    use super::validation_policy as policy;
    use crate::sources::DataSources;
    use crate::twin::{compare, live};

    /// Per site: the archived Level II volume nearest now, the EET object
    /// generated **from that volume** (paired by PDB volume start, never key
    /// freshness), our derivation on the twin's own radar height, and two
    /// tallies in the product's own data levels — heights with bit 7 masked,
    /// and the topped flag as a two-class field. Per-site assertion, pooled
    /// only for the conclusiveness gate.
    #[ignore = "hits the live S3 bucket"]
    #[tokio::test]
    async fn live_derived_eet_matches_the_rpgs_own_product() {
        crate::tls::init();
        let sources = DataSources::production();
        let now = chrono::Utc::now().naive_utc();

        // Levels are compared as themselves: the derived side is already in
        // data levels, and the masked twin's gates decode to their own value.
        let identity = compare::ValueCodec::Scaled {
            scale: 1.0,
            offset: 0.0,
        };

        let mut asserted_sites = 0usize;
        let mut pooled_compared = 0usize;
        let mut failures: Vec<String> = Vec::new();

        for &site in live::SITES {
            let Some((scan, l2_start)) = live::l2_volume_near(site, now).await else {
                println!("{site}: SKIP — no archived Level II volume found");
                continue;
            };
            let Some(twin) = live::l3_twin(&sources, site, "EET", l2_start, None).await else {
                println!("{site}: SKIP — no EET twin names volume {l2_start}");
                continue;
            };
            let Some(packet) = crate::srm::radial_packet(&twin.message) else {
                println!(
                    "{site}: SKIP — twin {} has no radial packet",
                    twin.stamp.key
                );
                continue;
            };

            let defined = policy::twin_defined_bins(packet);
            if !policy::volume_is_scoreable(defined) {
                println!(
                    "{site}: SKIP — near-empty twin ({defined} defined bins < {})",
                    policy::MIN_TWIN_DEFINED_BINS,
                );
                continue;
            }

            let grid = super::compute_eet(&scan, f64::from(twin.message.pdb.height));
            let (derived_heights, derived_topped) = policy::derived_level_grids(&grid);

            let gate_km = compare::gate_km(&twin.message.pdb, packet);
            let heights = compare::tally_packet(
                &derived_heights,
                &policy::mask_heights_packet(packet),
                gate_km,
                &identity,
                compare::ProductKind::Numeric,
            );
            let topped = compare::tally_packet(
                &derived_topped,
                &policy::topped_class_packet(packet),
                gate_km,
                &identity,
                compare::ProductKind::Numeric,
            );

            println!(
                "{site}: vol {l2_start} twin {} VCP {} | compared {} exact {:.2}% ±1 {:.2}% \
                 ±2 {:.2}% presence {:.2}% (derived {} / twin {}) | topped agree {:.2}% \
                 over {} (twin topped bins {})",
                twin.stamp.key,
                twin.message.pdb.vcp,
                heights.compared,
                heights.exact_pct(),
                heights.within_one_pct(),
                heights.within_two_pct(),
                heights.presence_disagreement_pct(),
                heights.derived_defined,
                heights.l3_defined,
                topped.exact_pct(),
                topped.compared,
                packet
                    .radials
                    .iter()
                    .flat_map(|r| r.gate_values.iter())
                    .filter(|&&g| g >= 2 && g & 0x80 != 0)
                    .count(),
            );

            if !policy::site_is_asserted(site) {
                println!("{site}: measured but quarantined — not asserted");
                continue;
            }

            let mut misses = Vec::new();
            if !policy::meets_acceptance_bar(heights.within_one_pct()) {
                misses.push(format!(
                    "within-one {:.2}% < {}%",
                    heights.within_one_pct(),
                    policy::ACCEPTANCE_BAR_WITHIN_ONE_PCT,
                ));
            }
            if !policy::presence_is_acceptable(heights.presence_disagreement_pct()) {
                misses.push(format!(
                    "presence disagreement {:.2}% > {}%",
                    heights.presence_disagreement_pct(),
                    policy::PRESENCE_DISAGREEMENT_MAX_PCT,
                ));
            }
            if !misses.is_empty() {
                failures.push(format!("{site} ({l2_start}): {}", misses.join("; ")));
            }
            asserted_sites += 1;
            pooled_compared += heights.compared;
        }

        println!(
            "asserted {asserted_sites} sites, {pooled_compared} bins pooled; failures: {}",
            failures.len(),
        );
        assert!(
            failures.is_empty(),
            "sites under the bar:\n  {}",
            failures.join("\n  "),
        );
        assert!(
            policy::sample_is_conclusive(asserted_sites, pooled_compared),
            "inconclusive run: {asserted_sites} sites / {pooled_compared} bins asserted, \
             need ≥{} sites and ≥{} bins — re-run when more sites carry echo",
            policy::MIN_SITES,
            policy::MIN_DEFINED_BINS,
        );
    }
}
