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
//! truncated `18`). Site-adaptable in principle; the twin harness (branch
//! `campaign-harness`) measures whether 18.3 holds in practice.
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
//!   RPG does not (measured: topped bins are vanishingly rare on live
//!   volumes). Here **only the volume's highest tilt makes a top topped**; a censored
//!   cell above clamps the top to the crossing tilt's own altitude,
//!   non-topped. Topped-flag agreement is printed by the harness so the gap
//!   stays measured.
//! * **SAILS/MRLE revisits**: HREET consumes each elevation's DQA buffer once
//!   as the volume completes, so the cube is deduplicated
//!   [`DedupPolicy::FirstOfVolume`] — the coherent first pass the RPG's
//!   volume products are computed from, not the freshest look. (Measured:
//!   newest-wins is indistinguishable, so the choice is by the doc,
//!   uncontradicted.)
//! * **Cell statistic** — twin-arbitrated, not documented: each 1° × 1 km
//!   cell takes the **maximum** dBZ of its sub-gates ([`CellStat::Max`]).
//!   The documented recombination average (linear-Z mean) measured lower
//!   against a live EET twin and left bins undefined that the twin defines:
//!   the RPG keeps peaks. Measured provenance: branch `campaign-harness`.
//!
//! # Validation status — read before trusting the twin harness to pass
//!
//! **The live twin harness, its `validation_policy`, and the full survey
//! record live on branch `campaign-harness`**; re-measuring means that
//! branch.
//!
//! As last measured, this derivation does **not** meet the campaign bar
//! (99% within one level, per site) against the RPG's own EET on
//! convective volumes: clear-air/weak sites pass, sites with real storms
//! fall well short with a storm-depth-dependent low bias, and the twin
//! defines bins no per-column recomputation of the same Level II data can
//! (its field is also visibly smoother than a raw column scan). Every
//! reproducible candidate for the residual was measured and ruled out,
//! each change isolated — the A/B record lives on the branch. The
//! remaining candidate is HREET's own pre/post-processing (its input is
//! the DQA buffer and its source, `cpc014/tsk012`, is not in any public
//! CODE distribution), so the residual is recorded here rather than
//! papered over: do not lower the bar, and do not calibrate further
//! heuristics against a single twin volume.

use crate::sites::Datum;
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
/// value the twin's PDB carries, or [`radar_height_ft_near`] on
/// [`Datum::Feedhorn`] for a render. The feedhorn, because every height this
/// adds it to is measured above the antenna.
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

/// Height above MSL, in feet, of the site nearest a lat/lon, on `datum` — for
/// the render path, which knows only the coordinates.
///
/// # Why the caller names a datum
///
/// A site has two heights 30–115 ft apart, the ground and the feedhorn, and
/// this used to return one number without saying which. Everything here is
/// added to a beam height, and [`crate::beam`] measures heights above the
/// **antenna**, so the answer every current caller wants is
/// [`Datum::Feedhorn`]. Naming it is the point: the old signature let a
/// caller inherit the ground silently and be a whole tower low.
///
/// # Why the nearest site rather than the named one
///
/// The render path is handed coordinates, not an ICAO. In practice those
/// coordinates always *came* from a [`crate::sites::RadarSite`] row —
/// `ScanInfo::from_scan` resolves the site by identifier through
/// [`crate::sites::get_radar_site`] and only falls back to (0, 0) with a
/// warning for an identifier the table does not carry — and every row is its
/// own nearest neighbour (`every_site_is_its_own_nearest_neighbour`). So this
/// resolves to the site the caller meant, exactly, and the nearest-neighbour
/// search is an indirection rather than a guess.
///
/// # Sites the table cannot answer for
///
/// Rows that do not record `datum` are **skipped**, not selected and then
/// unwrapped to zero. That distinction is the whole point: picking the
/// nearest row and *then* asking it for a height meant a row that could not
/// answer short-circuited the entire lookup to 0 ft — sea level, which is a
/// perfectly plausible reading for a coastal site and was a 292 ft error at
/// KLWX. Skipping degrades instead to a genuine neighbour's height, wrong by
/// the terrain between them rather than by the whole height of the site.
///
/// No shipped row fails to answer [`Datum::Feedhorn`]
/// (`every_site_answers_the_feedhorn_datum` pins that). Forty-six rows
/// genuinely cannot answer [`Datum::SiteBase`] — the TDWRs and `LPLA`, whose
/// volumes report a single height — and for those this returns a neighbour's
/// ground, which is why no render path asks for that datum. An empty table
/// still yields 0 ft, having nothing else to say.
pub fn radar_height_ft_near(lat: f64, lon: f64, datum: Datum) -> f64 {
    crate::sites::RADARS
        .iter()
        .filter_map(|s| s.height_ft(datum).map(|ft| (s, ft)))
        .min_by(|(a, _), (b, _)| {
            let da = (a.lat - lat).powi(2) + (a.lon - lon).powi(2);
            let db = (b.lat - lat).powi(2) + (b.lon - lon).powi(2);
            da.total_cmp(&db)
        })
        .map_or(0.0, |(_, ft)| f64::from(ft))
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

    /// The render path's site lookup: the nearest site, on the datum asked
    /// for.
    ///
    /// Both datums are pinned at the same coordinate, because a lookup that
    /// answered the same number for either would mean the parameter is
    /// decorative. KTLX's ground is 1213 ft and its feedhorn 1275.
    #[test]
    fn radar_height_lookup_finds_the_nearest_site() {
        // KTLX's own coordinates give KTLX's heights.
        assert_eq!(
            radar_height_ft_near(35.33306, -97.2775, Datum::SiteBase),
            1213.0
        );
        assert_eq!(
            radar_height_ft_near(35.33306, -97.2775, Datum::Feedhorn),
            1275.0
        );
        // A point nudged off-site still lands on it.
        assert_eq!(radar_height_ft_near(35.4, -97.2, Datum::Feedhorn), 1275.0);
    }

    /// A row that cannot answer the datum must not drag the lookup down to
    /// sea level.
    ///
    /// This is the shape of the KLWX defect. `radar_height_ft_near` used to
    /// pick the nearest row and *then* reach for its elevation, so standing
    /// on a row that had none returned 0 ft rather than anything about the
    /// terrain — and 0 ft is indistinguishable from a real answer, several
    /// rows of the table being under 20 ft.
    ///
    /// The hole survived the move to two datums: a row can now record a
    /// height and still be unable to answer the datum a caller names. So this
    /// asserts the property on **both** datums — no coordinate anywhere in the
    /// table's footprint returns exactly zero — which is the version that
    /// fails if a future row records only a base and a feedhorn caller stands
    /// on it.
    #[test]
    fn a_row_that_cannot_answer_never_reports_sea_level() {
        // The precondition that makes "exactly 0 ft" a usable sentinel: no row
        // is genuinely at sea level, the lowest being KBYX at 87 ft. A future
        // row at exactly 0 would make this test wrong rather than the code, so
        // it fails here first and says so.
        let lowest = crate::sites::RADARS
            .iter()
            .filter_map(|s| s.height_ft(Datum::Feedhorn))
            .min()
            .expect("the table is not empty");
        assert!(
            lowest > 0,
            "a site now records {lowest} ft, so 0 ft no longer means \
             'no height' and this test needs a different sentinel",
        );

        for datum in [Datum::SiteBase, Datum::Feedhorn] {
            for site in crate::sites::RADARS.iter() {
                let ft = radar_height_ft_near(site.lat, site.lon, datum);
                assert_ne!(
                    ft, 0.0,
                    "{} at ({}, {}) resolved to sea level on {datum:?}",
                    site.name, site.lat, site.lon,
                );
            }
        }
    }

    /// The six sites the table once shipped with no elevation, pinned by
    /// value on both datums.
    ///
    /// Their bases are `site_height` in metres — KDGX 151, KFSX 2261,
    /// KRTX 492, KSRX 200, KVWX 156, KLWX 89 — and all six have now been read
    /// back out of a real volume by `site_elev_probe`, which closes the gap
    /// this test used to record: only KLWX had been measured, and a
    /// transcription error in the other five would have gone unseen.
    ///
    /// KLWX is the one the cross-section campaign caught: it anchored a
    /// section 89 m low, and 89 m is four and a half rows of a 1024-row raster
    /// over a 20 km axis.
    #[test]
    fn the_six_formerly_unrecorded_sites_carry_their_measured_elevation() {
        for (name, base_ft, feedhorn_ft) in [
            ("KDGX", 495, 607),
            ("KFSX", 7418, 7513),
            ("KRTX", 1614, 1726),
            ("KSRX", 656, 735),
            ("KVWX", 512, 624),
            ("KLWX", 292, 404),
        ] {
            let site = crate::sites::get_radar_site(name).expect("in the table");
            assert_eq!(site.height_ft(Datum::SiteBase), Some(base_ft), "{name}");
            assert_eq!(site.height_ft(Datum::Feedhorn), Some(feedhorn_ft), "{name}");
            assert_eq!(
                radar_height_ft_near(site.lat, site.lon, Datum::Feedhorn),
                f64::from(feedhorn_ft),
            );
        }
    }
}
