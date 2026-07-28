//! Storm-relative velocity derived locally from the Level II velocity volume.
//!
//! The Level III pipeline this replaces ([`crate::srm`]) fetched five bucket
//! objects per site — `N0S` for the storm motion vector in its PDB and
//! `N0G`/`N1G`/`N2U`/`N3U` as the four dealiased tilts — because Level II
//! velocity is aliased and, at the time, nothing local could unfold it. The
//! NROT campaign built that dealiaser ([`crate::nrot::dealias`], a
//! validity-marking multi-pass calibrated against a reference
//! implementation), so the whole product can now be computed from the volume
//! already in hand:
//!
//! ```text
//! SRV(az, r) = V_dealiased(az, r) + speed · cos(direction − az)      [m/s]
//! ```
//!
//! per gate, with the correction applied at the radial's **centre** azimuth —
//! the angle the renderer centres the strip on — and `direction` the
//! meteorological "from" direction, which is why the term adds rather than
//! subtracts. Both conventions are ported verbatim from [`crate::srm`], whose
//! sign was settled over a million live gates (92.7% exact with `+`, 3.9%
//! with `−`), and are pinned here offline against `srm::derive` itself.
//!
//! # The dealiaser profile
//!
//! NROT censors aggressively — it differentiates the field, so a residual
//! fold wall reads as clamp-level fake shear. A displayed velocity field
//! wants the opposite posture: a censored gate is a hole in the couplet the
//! product exists to show. [`crate::nrot::DealiasProfile::Coverage`] keeps
//! every unreached data gate at raw (region size gate dropped to 1) and
//! keeps the fold-wall censor at NROT's measured 1.24·Vny. SRV also skips
//! NROT's median filter entirely: the RPG's own dealiased products are not
//! median-filtered, and the filter's ND rules cost coverage.
//!
//! Both knob choices were A/B'd live (2026-07-28) against the RPG's own
//! dealiased velocity — products 154/99, the `N0G`/`N1G`/`N2U`/`N3U` twins
//! of the same Level II volume and cut — on five climatologically spread
//! decision sites (KMPX upper midwest, KTLX southern plains, KMOB gulf
//! coast, KMTX mountain west, KMRX Appalachian southeast; 4 tilts × 4
//! postures × 5 sites), the other seventeen roster sites the holdout:
//!
//! * kept-raw floor 16 → 1: coverage 90.3–99.4% → 99.2–100% (the floor-16
//!   posture would *fail* the 95% coverage bar at KMTX's upper tilts, 90.3%
//!   and 91.8%), for at most 0.05 points of within-±1 given up — every
//!   decision site, same verdict;
//! * censor off: coverage gains at most 0.8 points anywhere, and within-±1
//!   *drops* wherever real folding exists — KMRX 99.54 → 99.10 (`N0G`),
//!   99.82 → 99.64, 99.97 → 99.76, 99.88 → 99.55 — because a kept fold wall
//!   is a 2·Vny error on every gate it touches. The censor stays.
//!
//! The holdout confirmed the choice: the full 22-site Protocol A survey
//! below ran on the shipped knobs and passed everywhere.
//!
//! # Validation status (2026-07-28 survey)
//!
//! **Protocol A** — the dealiased grid (Coverage profile, no median filter,
//! pre-SRM) against the RPG's own dealiased velocity, per site per tilt,
//! all 22 roster sites, 1.37 M gates compared: within one 0.5 m/s level
//! 99.54–100.00% on all 88 site-tilts (bar 99%; worst KMRX `N0G` 99.54%),
//! coverage 99.2–100% of the twin's defined gates (bar 95%). Every site
//! conclusive, none quarantined. Unlike EET/DVL, whose oracle is the
//! DQA-edited reflectivity chain this campaign cannot reach, the velocity
//! twins are reproducible from raw Level II: unfolded gates carry identical
//! 0.5 m/s codes on both sides, so the residual is confined to fold regions
//! and edited gates.
//!
//! **Protocol B** — the same `N0G` and the same `N0S` vector through this
//! module's m/s arithmetic and through [`crate::srm::derive`]'s knots
//! arithmetic: 18 sites with nonzero vectors, 100.000% within ±1 derived
//! level (0.5 kt) at every one, 3.1 M gates. The port is exact to float
//! rounding.
//!
//! # Storm motion
//!
//! The RPG's SCIT-average vector lived in `N0S`'s PDB and is gone with the
//! fetch. The local default is the **Bunkers right-mover** (Bunkers et al.
//! 2000, *Wea. Forecasting*, 15, 61–79: "Predicting Supercell Motion Using
//! a New Hodograph Technique", the ID method), computed from the volume's
//! own VAD-fitted wind profile ([`crate::nrot::WindProfileBuilder`], the
//! same fit NROT's dealiaser seeds from):
//!
//! ```text
//! V_rm = V_mean + 7.5 m/s · (S × k̂) / |S|
//! V_mean = non-pressure-weighted mean wind, 0–6 km AGL
//! S      = (5.5–6 km mean wind) − (0–0.5 km mean wind)
//! S × k̂  = (S_v, −S_u)      — 90° clockwise of the shear vector
//! ```
//!
//! The bands are read at the profile's 0.3 km layer centres, so "0–0.5 km"
//! is layers centred 0.15/0.45 km and "5.5–6 km" layers centred 5.55/5.85 km
//! — a 0.1 km band skew the discretisation imposes, documented rather than
//! hidden. A user-entered vector overrides Bunkers everywhere; the override
//! plumbing and its dominance are the frontend's
//! (`render_dispatch::set_storm_motion_override`), unchanged.
//!
//! How far Bunkers sits from the RPG's SCIT average is **measured and
//! printed** by the live harness per site, never asserted: the two are
//! different estimators of different things (cell-track average against
//! hodograph prediction), and which to ship is a product decision the
//! numbers inform. The 2026-07-28 survey, a quiet-to-moderate day: where
//! the RPG carried a real vector the deltas ranged from +0.4 kt/+9°
//! (KUEX) and +0.5 kt/+141° (KPAH) to −37.6 kt/+91° (KEAX, where SCIT was
//! tracking a single 53 kt cell) — direction deltas are large exactly
//! where speeds are small and SCIT is fitting few cells. Four sites'
//! profiles refused Bunkers outright (quiet, shear under the floor at the
//! time; the mean-wind fallback now covers that case), and three RPG
//! vectors were 0.0 kt where Bunkers offered 11–14 kt. Read it as: on
//! organised-convection days the two broadly agree; on quiet days they
//! are both guessing, and the derived product at least guesses from the
//! whole volume's hodograph rather than from one cell track.
//!
//! # Units and the display seam
//!
//! Gate values are **m/s**, like every Level II product: the velocity
//! palette takes m/s, `format_value` converts m/s to the user's speed unit,
//! and the hover reads the value grid raw — so the whole display path works
//! unchanged. (The Level III pipeline stored quantized knots and converted
//! back with `l3_physical_value`'s SRV-only `× 0.514444`; that seam dies
//! with the fetch.) Nothing is quantized on the render path;
//! [`crate::srm::quantize_to_rpg_levels`] and the derived 0.5 kt levels
//! exist only so the validation below can compare like with like.

use crate::nrot::{DealiasProfile, VelocitySweep, WindProfile, WindProfileBuilder};
use nexrad_model::data::{DataMoment, Radial, Scan};

/// Metres per second per knot.
pub const KT_TO_MS: f64 = 0.514_444;

/// Bunkers et al. 2000's deviation from the 0–6 km mean wind, m/s,
/// perpendicular-right of the 0–0.5 km → 5.5–6 km shear vector.
pub const BUNKERS_DEVIATION_MS: f64 = 7.5;

/// Depth of the Bunkers mean-wind layer, km AGL.
pub const BUNKERS_MEAN_DEPTH_KM: f64 = 6.0;

/// Where a storm motion vector came from. Two sources, not three: the RPG's
/// SCIT average is not fetched any more, so it cannot be one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StormMotionSource {
    /// Bunkers right-mover from the volume's own VAD wind profile.
    BunkersRightMover,
    /// A vector the user typed in. Dominant over Bunkers wherever set.
    UserOverride,
}

/// A storm motion vector in the product's conventions: knots, and the
/// meteorological direction the storm comes **from**.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SrvMotion {
    pub speed_kt: f32,
    /// Direction the storm comes *from*, degrees. See the module docs for
    /// why the radial correction then adds rather than subtracts.
    pub direction_deg: f32,
    pub source: StormMotionSource,
}

impl SrvMotion {
    /// A vector the user typed in. `None` for a non-finite speed or
    /// direction — the same refusal [`crate::srm::StormMotionSample::user_override`]
    /// makes, for the same reason: a NaN defeats every equality test
    /// downstream change detectors rely on.
    pub fn user_override(speed_kt: f32, direction_deg: f32) -> Option<Self> {
        if !speed_kt.is_finite() || !direction_deg.is_finite() {
            return None;
        }
        Some(Self {
            speed_kt,
            direction_deg,
            source: StormMotionSource::UserOverride,
        })
    }
}

/// A velocity field as a dense azimuth × range grid in m/s, NaN where
/// undefined — the shape [`crate::nrot::VelocitySweep`] borrows, with the
/// geometry the renderer needs carried alongside.
#[derive(Debug, Clone)]
pub struct VelocityGrid {
    /// m/s per (radial, gate); NaN is no data.
    pub values: Vec<Vec<f64>>,
    /// Radial **centre** azimuths, degrees, in sweep order.
    pub azimuths_deg: Vec<f64>,
    pub gate_count: usize,
    /// Range to the **centre** of the first gate, km — the Level II moment
    /// header's convention, and what the renderer centres gate strips on.
    pub first_gate_range_km: f64,
    pub gate_interval_km: f64,
}

impl VelocityGrid {
    fn sweep(&self) -> VelocitySweep<'_> {
        VelocitySweep {
            vel_grid: &self.values,
            azimuths_deg: &self.azimuths_deg,
            gate_count: self.gate_count,
            first_gate_range_km: self.first_gate_range_km,
            gate_interval_km: self.gate_interval_km,
        }
    }
}

/// The raw velocity of one sweep as a grid, or `None` when no radial
/// carries the moment. The same extraction the NROT render makes.
pub fn velocity_grid(radials: &[Radial]) -> Option<VelocityGrid> {
    let first_vel = radials.iter().find_map(|r| r.velocity())?;
    let gate_count = first_vel.gate_count() as usize;
    let first_gate_range_km = first_vel.first_gate_range_km();
    let gate_interval_km = first_vel.gate_interval_km();

    let mut values: Vec<Vec<f64>> = Vec::with_capacity(radials.len());
    let mut azimuths_deg: Vec<f64> = Vec::with_capacity(radials.len());
    for radial in radials {
        azimuths_deg.push(radial.azimuth_angle_degrees() as f64);
        let mut gates = vec![f64::NAN; gate_count];
        if let Some(moment) = radial.velocity() {
            for (j, val) in moment.values().iter().enumerate().take(gate_count) {
                if let nexrad_model::data::MomentValue::Value(v) = val
                    && !v.is_nan()
                    && *v < 999.0
                {
                    gates[j] = *v as f64;
                }
            }
        }
        values.push(gates);
    }
    Some(VelocityGrid {
        values,
        azimuths_deg,
        gate_count,
        first_gate_range_km,
        gate_interval_km,
    })
}

/// One sweep's velocity, dealiased for display: the Coverage profile,
/// **no median filter** — see the module docs for both choices.
pub fn dealiased_grid(
    radials: &[Radial],
    elevation_deg: f64,
    profile: Option<&WindProfile>,
) -> Option<VelocityGrid> {
    let mut grid = velocity_grid(radials)?;
    let sweep_view = VelocitySweep {
        vel_grid: &grid.values.clone(),
        azimuths_deg: &grid.azimuths_deg,
        gate_count: grid.gate_count,
        first_gate_range_km: grid.first_gate_range_km,
        gate_interval_km: grid.gate_interval_km,
    };
    crate::nrot::dealias(
        &mut grid.values,
        &sweep_view,
        elevation_deg,
        profile,
        DealiasProfile::Coverage,
    );
    Some(grid)
}

/// Add the storm-motion term to every defined gate, in place:
/// `v += speed · cos(direction − azimuth)`, at the radial's centre azimuth.
///
/// The correction is constant along range, so it is computed once per
/// radial. NaN gates stay NaN — mapping them through the arithmetic would
/// paint the storm-motion field itself across every gate the radar saw
/// nothing in, the same failure [`crate::srm::derive`] guards against.
pub fn apply_storm_motion(grid: &mut VelocityGrid, motion: &SrvMotion) {
    let speed_ms = motion.speed_kt as f64 * KT_TO_MS;
    for (row, &az) in grid.values.iter_mut().zip(&grid.azimuths_deg) {
        let component = speed_ms * (motion.direction_deg as f64 - az).to_radians().cos();
        for v in row.iter_mut() {
            if !v.is_nan() {
                *v += component;
            }
        }
    }
}

/// The full per-tilt derivation: dealias (Coverage, no median filter), then
/// the storm-motion correction. `None` when the sweep carries no velocity.
pub fn compute_srv_grid(
    radials: &[Radial],
    elevation_deg: f64,
    profile: Option<&WindProfile>,
    motion: &SrvMotion,
) -> Option<VelocityGrid> {
    let mut grid = dealiased_grid(radials, elevation_deg, profile)?;
    apply_storm_motion(&mut grid, motion);
    Some(grid)
}

/// Fit the volume wind profile from every velocity tilt in the scan — the
/// same fit `render::build_wind_profile` makes for NROT, exposed here so the
/// SRV harness and the render path share one.
pub fn volume_wind_profile(scan: &Scan) -> Option<WindProfile> {
    let mut builder = WindProfileBuilder::new();
    for sweep in scan.sweeps() {
        let radials = sweep.radials();
        let Some(first) = radials.first() else {
            continue;
        };
        if first.velocity().is_none() || radials.len() < 3 {
            continue;
        }
        let Some(grid) = velocity_grid(radials) else {
            continue;
        };
        builder.add_sweep(&grid.sweep(), first.elevation_angle_degrees() as f64);
    }
    builder.finish()
}

/// The Bunkers et al. 2000 right-mover from a fitted wind profile, as a
/// motion vector `(u, v)` in m/s (the direction the storm moves *toward*),
/// or `None` when the profile cannot support it.
///
/// The paper's ID method, exactly (their Eq. 1, right member):
/// `V_rm = V_mean + D · (S × k̂)/|S|` with `V_mean` the non-pressure-weighted
/// 0–6 km AGL mean wind, `S` the shear vector from the 0–500 m mean wind to
/// the 5500–6000 m mean wind, `D` = 7.5 m/s, and `S × k̂ = (S_v, −S_u)` the
/// 90°-clockwise rotation that puts the deviation perpendicular-**right** of
/// the shear through the mean wind.
///
/// Bands are read at the profile's 0.3 km layer centres (see the module
/// docs). Refused — `None` — when fewer than [`BUNKERS_MIN_MEAN_LAYERS`] of
/// the twenty 0–6 km layers carry a fit, when either shear band is empty, or
/// when the shear magnitude is under [`BUNKERS_MIN_SHEAR_MS`]: a
/// near-zero shear vector has no meaningful "right of", and the deviation
/// direction would be noise.
pub fn bunkers_right_mover_uv(profile: &WindProfile) -> Option<(f64, f64)> {
    let layer_km = WindProfile::LAYER_KM;
    let layers = (BUNKERS_MEAN_DEPTH_KM / layer_km).round() as usize;

    let mut mean = (0.0f64, 0.0f64, 0usize);
    let mut head = (0.0f64, 0.0f64, 0usize); // 0–0.5 km
    let mut tail = (0.0f64, 0.0f64, 0usize); // 5.5–6 km
    for l in 0..layers {
        let centre = (l as f64 + 0.5) * layer_km;
        let Some((u, v)) = profile.wind_at_km(centre) else {
            continue;
        };
        mean = (mean.0 + u, mean.1 + v, mean.2 + 1);
        if centre < 0.5 {
            head = (head.0 + u, head.1 + v, head.2 + 1);
        }
        if (5.5..BUNKERS_MEAN_DEPTH_KM).contains(&centre) {
            tail = (tail.0 + u, tail.1 + v, tail.2 + 1);
        }
    }
    if mean.2 < BUNKERS_MIN_MEAN_LAYERS || head.2 == 0 || tail.2 == 0 {
        return None;
    }
    let mean = (mean.0 / mean.2 as f64, mean.1 / mean.2 as f64);
    let head = (head.0 / head.2 as f64, head.1 / head.2 as f64);
    let tail = (tail.0 / tail.2 as f64, tail.1 / tail.2 as f64);
    let shear = (tail.0 - head.0, tail.1 - head.1);
    let magnitude = (shear.0 * shear.0 + shear.1 * shear.1).sqrt();
    if magnitude < BUNKERS_MIN_SHEAR_MS {
        // No shear direction worth deviating from: the propagation term is
        // dropped and the estimate is pure advection — the storm moves with
        // the mean wind. This is the quiet-day case, where the RPG's own
        // SCIT average reads ~0 kt and the Level III product still painted;
        // refusing here would blank the pane instead.
        return Some(mean);
    }
    // (S × k̂)/|S| = (S_v, −S_u)/|S|: perpendicular, 90° clockwise of S.
    Some((
        mean.0 + BUNKERS_DEVIATION_MS * shear.1 / magnitude,
        mean.1 - BUNKERS_DEVIATION_MS * shear.0 / magnitude,
    ))
}

/// Fewest fitted 0–6 km layers (of twenty) for a mean wind worth calling
/// "the 0–6 km mean": under this the estimate is a few tilts' sidelobes.
pub const BUNKERS_MIN_MEAN_LAYERS: usize = 12;

/// Smallest 0–0.5 → 5.5–6 km shear magnitude, m/s, whose direction is worth
/// deviating from. Well under any supercell environment (Bunkers' own
/// dataset median is ~2.6× this) — under the floor the deviation is dropped
/// and the estimate is the mean wind alone, not refused.
pub const BUNKERS_MIN_SHEAR_MS: f64 = 2.0;

/// [`bunkers_right_mover_uv`] in the product's conventions: knots, and the
/// meteorological direction the motion comes **from**.
pub fn bunkers_right_mover(profile: &WindProfile) -> Option<SrvMotion> {
    let (u, v) = bunkers_right_mover_uv(profile)?;
    let speed_kt = (u * u + v * v).sqrt() / KT_TO_MS;
    // Toward-direction is atan2(u, v) in compass degrees; "from" is its
    // reciprocal. rem_euclid keeps it in [0, 360).
    let direction_deg = (u.atan2(v).to_degrees() + 180.0).rem_euclid(360.0);
    Some(SrvMotion {
        speed_kt: speed_kt as f32,
        direction_deg: direction_deg as f32,
        source: StormMotionSource::BunkersRightMover,
    })
}

/// The storm motion a render should apply: the user's override when one is
/// set, otherwise Bunkers from the volume's profile, otherwise `None` — and
/// a `None` means **no SRV render**, because painting base velocity under a
/// storm-relative label is the failure the Level III path refused too.
pub fn storm_motion(
    profile: Option<&WindProfile>,
    user_override: Option<SrvMotion>,
) -> Option<SrvMotion> {
    if let Some(motion) = user_override {
        // Only an override may claim to be one; a mislabelled sample would
        // let a Bunkers vector survive an override change detector.
        if motion.source == StormMotionSource::UserOverride {
            return Some(motion);
        }
    }
    profile.and_then(bunkers_right_mover)
}

// ── Validation policy ────────────────────────────────────────────────────────

/// What counts as passing, pinned outside the `#[ignore]`d live module so
/// the default suite can hold it — the same architecture as
/// [`crate::srm::validation_policy`] and for the same reason: a bar that
/// only a network test reads is a bar that can be lowered without any CI
/// gate noticing.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod validation_policy {
    use crate::twin::compare::Tally;

    /// Protocol A's acceptance bar: percent of compared gates within one
    /// twin data level. The twin is the RPG's own dealiased velocity at
    /// 0.5 m/s per level (asserted live off the PDB), so ±1 level is
    /// ±0.5 m/s.
    pub const WITHIN_ONE_PCT: f64 = 99.0;

    /// Protocol A's coverage floor: our defined-gate count as a percentage
    /// of the twin's, per site per tilt. The Coverage dealias profile exists
    /// to hold this without giving up the level bar.
    pub const COVERAGE_MIN_PCT: f64 = 95.0;

    /// Sites that must pass Protocol A for the conversion to ship.
    pub const MIN_SITES: usize = 3;

    /// Fewest compared gates for a site to count toward [`MIN_SITES`].
    pub const MIN_DEFINED_GATES: usize = 10_000;

    /// Fewest compared gates for one tilt's percentage to be asserted on at
    /// all; thinner tilts are printed, never asserted.
    pub const MIN_TILT_GATES: usize = 1_000;

    /// Protocol B's bar: the SRM arithmetic port must agree with
    /// [`crate::srm::derive`] on ≥ this percent of gates within ±1 derived
    /// level (0.5 kt each) when both are fed the same velocity product and
    /// the same vector. The two computations differ only in the order of
    /// unit conversion, so anything under this is a ported-convention bug,
    /// not noise.
    pub const PORT_CHECK_PCT: f64 = 99.9;

    /// Sites measured to miss a Protocol A bar, excluded from assertion but
    /// never from measurement. **Empty**: no site has been quarantined, and
    /// adding one is admitting a gap — record the numbers and what was ruled
    /// out, never widen a bar instead.
    pub const QUARANTINED: &[&str] = &[];

    pub fn is_quarantined(site: &str) -> bool {
        QUARANTINED.contains(&site)
    }

    pub fn meets_level_bar(within_one_pct: f64) -> bool {
        within_one_pct >= WITHIN_ONE_PCT
    }

    pub fn meets_coverage_bar(coverage_pct: f64) -> bool {
        coverage_pct >= COVERAGE_MIN_PCT
    }

    pub fn meets_port_bar(within_one_pct: f64) -> bool {
        within_one_pct >= PORT_CHECK_PCT
    }

    /// Our defined-gate count as a percentage of the twin's, from a tally.
    /// Over `max(l3, 1)` so an empty twin reads 0 rather than NaN; every
    /// assertion checks the gate floors separately.
    pub fn coverage_pct(t: &Tally) -> f64 {
        100.0 * t.derived_defined as f64 / t.l3_defined.max(1) as f64
    }

    /// Whether one tilt's tally is thick enough to assert on.
    pub fn tilt_is_asserted(t: &Tally) -> bool {
        t.compared >= MIN_TILT_GATES
    }

    /// Whether a site's pooled sample is thick enough to count toward
    /// [`MIN_SITES`].
    pub fn site_is_conclusive(compared: usize) -> bool {
        compared >= MIN_DEFINED_GATES
    }

    /// Sample a native-resolution grid onto the 360° × 230 km comparison
    /// lattice [`crate::twin::compare::tally_packet`] reads the Level III
    /// side on: per cell, the radial covering the cell-centre azimuth
    /// `az + 0.5°` and the gate whose centre falls nearest the cell-centre
    /// range `r + 0.5` km within that cell.
    ///
    /// **Sampling, not averaging** — deliberately. The tally reads the twin
    /// packet the same way, and super-res Level II shares the twin's native
    /// geometry gate for gate (0.25 km × 0.5°, centres in phase), so each
    /// cell compares one physical gate against itself. The SRM campaign's
    /// azimuth-mean/range-peak resampler reconstructed the RPG's 1 km
    /// **recombination**; nothing is recombined here, so none is modelled.
    pub fn sample_to_comparison_grid(grid: &super::VelocityGrid) -> Vec<Vec<f32>> {
        use crate::volumetric::RANGE_BINS;
        let n = grid.values.len();
        let mut slots: Vec<Option<usize>> = vec![None; 3600];
        let half_spacing = 360.0 / n as f64 / 2.0;
        for (i, &az) in grid.azimuths_deg.iter().enumerate() {
            let start = ((az - half_spacing) * 10.0).round() as i32;
            let width = ((half_spacing * 2.0) * 10.0).round().max(1.0) as i32;
            for k in 0..width {
                slots[(start + k).rem_euclid(3600) as usize] = Some(i);
            }
        }

        // Which gate represents each 1 km cell: centre nearest the cell
        // centre, first gate winning ties — the tally's own rule.
        let mut gate_for_bin: Vec<Option<usize>> = vec![None; RANGE_BINS];
        let mut best = vec![f64::INFINITY; RANGE_BINS];
        for j in 0..grid.gate_count {
            let centre = grid.first_gate_range_km + j as f64 * grid.gate_interval_km;
            let bin = centre.floor() as i64;
            if !(0..RANGE_BINS as i64).contains(&bin) {
                continue;
            }
            let d = (centre - (bin as f64 + 0.5)).abs();
            if d < best[bin as usize] {
                best[bin as usize] = d;
                gate_for_bin[bin as usize] = Some(j);
            }
        }

        (0..360)
            .map(|az| {
                let radial = slots[az * 10 + 5];
                (0..RANGE_BINS)
                    .map(|r| {
                        let (Some(i), Some(j)) = (radial, gate_for_bin[r]) else {
                            return f32::NAN;
                        };
                        grid.values[i].get(j).copied().unwrap_or(f64::NAN) as f32
                    })
                    .collect()
            })
            .collect()
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::validation_policy::*;
    use super::*;
    use nexrad_model::data::{MomentData, Radial, RadialStatus};

    /// Levels at the profile's 0.3 km layer centres, `count` of them, so
    /// `WindProfile::from_levels` maps level `l` onto layer `l` exactly.
    fn profile_at_centres(uv: impl Fn(usize) -> (f64, f64), count: usize) -> WindProfile {
        let levels: Vec<(f64, f64, f64)> = (0..count)
            .map(|l| {
                let (u, v) = uv(l);
                ((l as f64 + 0.5) * WindProfile::LAYER_KM, u, v)
            })
            .collect();
        WindProfile::from_levels(&levels).expect("levels are non-empty")
    }

    /// Unidirectional westerly shear, hand-computed: u = 10 + 2·l over the
    /// twenty 0–6 km layers, v = 0.
    ///
    /// mean u = 10 + 2·(19/2) = 29; head (l = 0, 1) = 11; tail (l = 18, 19)
    /// = 47; S = (36, 0); (S_v, −S_u)/|S| = (0, −1); V_rm = (29, −7.5).
    #[test]
    fn bunkers_on_unidirectional_shear_deviates_straight_right() {
        let p = profile_at_centres(|l| (10.0 + 2.0 * l as f64, 0.0), 20);
        let (u, v) = bunkers_right_mover_uv(&p).expect("a full profile supports Bunkers");
        assert!((u - 29.0).abs() < 1e-9, "u = {u}");
        assert!((v + 7.5).abs() < 1e-9, "v = {v}");

        let m = bunkers_right_mover(&p).expect("same profile");
        assert_eq!(m.source, StormMotionSource::BunkersRightMover);
        let want_kt = (29.0f64.powi(2) + 7.5f64.powi(2)).sqrt() / KT_TO_MS;
        assert!((m.speed_kt as f64 - want_kt).abs() < 1e-3, "{}", m.speed_kt);
        // Motion toward (29, −7.5): compass atan2(29, −7.5) = 104.5° toward,
        // so 284.5° from.
        let want_dir = (29.0f64.atan2(-7.5).to_degrees() + 180.0).rem_euclid(360.0);
        assert!(
            (m.direction_deg as f64 - want_dir).abs() < 1e-3,
            "{} vs {want_dir}",
            m.direction_deg,
        );
    }

    /// A curved (two-segment) hodograph, hand-computed: the bottom ten
    /// layers at (0, 10), the top ten at (10, 0).
    ///
    /// mean = (5, 5); head = (0, 10); tail = (10, 0); S = (10, −10),
    /// |S| = 14.142…; deviation = 7.5·(−10, −10)/|S| = (−5.303, −5.303);
    /// V_rm = (−0.303, −0.303).
    #[test]
    fn bunkers_on_a_curved_hodograph_deviates_right_of_the_shear() {
        let p = profile_at_centres(|l| if l < 10 { (0.0, 10.0) } else { (10.0, 0.0) }, 20);
        let (u, v) = bunkers_right_mover_uv(&p).expect("a full profile supports Bunkers");
        let want = 5.0 - 7.5 * 10.0 / (200.0f64).sqrt();
        assert!((u - want).abs() < 1e-9, "u = {u}, want {want}");
        assert!((v - want).abs() < 1e-9, "v = {v}, want {want}");
        assert!(u < 0.0, "the deviation outweighs the mean here");
    }

    /// Without a shear direction the deviation is dropped, not invented:
    /// the estimate falls back to pure advection by the mean wind. A
    /// profile that is mostly holes is refused outright.
    #[test]
    fn bunkers_falls_back_to_the_mean_wind_without_shear_direction() {
        // Uniform wind: shear is exactly zero, motion is the mean itself —
        // the quiet-day case, which must keep painting.
        let uniform = profile_at_centres(|_| (15.0, 5.0), 20);
        assert_eq!(bunkers_right_mover_uv(&uniform), Some((15.0, 5.0)));

        // Only three levels fit near the surface: `from_levels` clamp-fills
        // three layers past the last one, leaving 6 of 20 — under the floor
        // — and the 5.5–6 km band empty twice over.
        let hollow = profile_at_centres(|l| (2.0 * l as f64, 0.0), 3);
        assert_eq!(bunkers_right_mover_uv(&hollow), None);
    }

    /// The correction is `+speed·cos(direction − azimuth)` in m/s at the
    /// radial centre — [`crate::srm`]'s pinned conventions, on this module's
    /// own grid type.
    #[test]
    fn the_storm_motion_term_is_added_along_the_radial() {
        let mut grid = VelocityGrid {
            values: vec![vec![10.0; 4]; 4],
            azimuths_deg: vec![90.0, 180.0, 270.0, 0.0],
            gate_count: 4,
            first_gate_range_km: 2.125,
            gate_interval_km: 0.25,
        };
        let motion = SrvMotion {
            speed_kt: 30.0,
            direction_deg: 90.0,
            source: StormMotionSource::UserOverride,
        };
        apply_storm_motion(&mut grid, &motion);
        let full = 30.0 * KT_TO_MS;
        // Azimuth 90 points at the direction the storm comes from: full +.
        assert!((grid.values[0][0] - (10.0 + full)).abs() < 1e-9, "az 090");
        // The reciprocal takes the full −; orthogonals keep base velocity.
        assert!((grid.values[2][0] - (10.0 - full)).abs() < 1e-9, "az 270");
        assert!((grid.values[1][0] - 10.0).abs() < 1e-9, "az 180");
        assert!((grid.values[3][0] - 10.0).abs() < 1e-9, "az 000");
    }

    /// A zero vector reproduces base velocity exactly, and NaN gates stay
    /// NaN under any vector.
    #[test]
    fn a_zero_vector_is_identity_and_no_data_stays_empty() {
        let mut grid = VelocityGrid {
            values: vec![vec![-12.5, f64::NAN, 33.0]],
            azimuths_deg: vec![137.0],
            gate_count: 3,
            first_gate_range_km: 2.125,
            gate_interval_km: 0.25,
        };
        let zero = SrvMotion {
            speed_kt: 0.0,
            direction_deg: 285.7,
            source: StormMotionSource::UserOverride,
        };
        apply_storm_motion(&mut grid, &zero);
        assert_eq!(grid.values[0][0], -12.5);
        assert!(grid.values[0][1].is_nan());
        assert_eq!(grid.values[0][2], 33.0);

        let moving = SrvMotion {
            speed_kt: 45.0,
            direction_deg: 137.0,
            source: StormMotionSource::UserOverride,
        };
        apply_storm_motion(&mut grid, &moving);
        assert!(
            grid.values[0][1].is_nan(),
            "a gate with no data must not paint the storm-motion field"
        );
        assert!((grid.values[0][0] - (-12.5 + 45.0 * KT_TO_MS)).abs() < 1e-9);
    }

    /// **The port check, offline**: the same velocity product and the same
    /// vector through this module's m/s arithmetic and through
    /// [`crate::srm::derive`]'s knots arithmetic land on the same derived
    /// levels. This is Protocol B's exact comparison on a synthetic message,
    /// so the live run can only fail for a live-data reason.
    #[test]
    fn the_msus_arithmetic_matches_the_level3_derivation_gate_for_gate() {
        use nexrad_level3::model::{
            DataLayer, DataPacket, Level3Message, MessageHeader, ProductDescriptionBlock,
            RadialPacket, RadialRun, SymbologyBlock,
        };

        // A 154-shaped message: 0.5°-wide radials, thresholds -63.5/0.5/254.
        let mut thresholds = [0u16; 16];
        thresholds[0] = -635i16 as u16;
        thresholds[1] = 5;
        thresholds[2] = 254;
        let radials: Vec<RadialRun> = (0..720)
            .map(|i| RadialRun {
                start_angle: i as f32 * 0.5,
                angle_delta: 0.5,
                // Every level 2..=255 appears repeatedly across the sweep.
                gate_values: (0..40)
                    .map(|j| (2 + (i * 7 + j * 11) % 254) as u16)
                    .collect(),
            })
            .collect();
        let msg = Level3Message {
            header: MessageHeader {
                message_code: 154,
                date_of_message: 20661,
                time_of_message: 7108,
                message_length: 0,
                source_id: 0,
                destination_id: 0,
                number_of_blocks: 3,
            },
            pdb: ProductDescriptionBlock {
                block_divider: -1,
                latitude: 44.849,
                longitude: -93.565,
                height: 1000,
                product_code: 154,
                operational_mode: 2,
                vcp: 212,
                sequence_number: 0,
                volume_scan_number: 39,
                volume_scan_date: 20661,
                volume_scan_time: 7108,
                generation_date: 20661,
                generation_time: 7108,
                product_specific_1: 0,
                product_specific_2: 0,
                elevation_number: 1,
                product_specific_3: 5,
                thresholds,
                product_specific_47_53: [-93, 74, 0, 8097, 1, 13, 16382],
                version: 0,
                spot_blank: 0,
                symbology_offset: 60,
                graphic_offset: 0,
                tabular_offset: 0,
            },
            symbology: Some(SymbologyBlock {
                block_id: 1,
                block_length: 0,
                num_layers: 1,
                layers: vec![DataLayer {
                    layer_length: 0,
                    packets: vec![DataPacket::DigitalRadial(RadialPacket {
                        first_range_bin: 0,
                        num_range_bins: 40,
                        i_center: 0,
                        j_center: 0,
                        scale_factor: 0.999,
                        is_legacy: false,
                        xdr_data_scale: None,
                        xdr_data_offset: None,
                        radials,
                    })],
                }],
            }),
        };

        let sample = crate::srm::StormMotionSample {
            motion: nexrad_level3::model::StormMotion {
                speed_kt: 33.7,
                direction_deg: 213.4,
                is_scit_average: true,
            },
            volume: Some((20661, 7108)),
        };
        let theirs = crate::srm::derive(&msg, &sample).expect("154 derives");

        let motion = SrvMotion {
            speed_kt: 33.7,
            direction_deg: 213.4,
            source: StormMotionSource::UserOverride,
        };
        let packet = crate::srm::radial_packet(&msg).expect("the fixture carries radials");
        let mut ours = grid_from_packet(packet, &msg.pdb);
        apply_storm_motion(&mut ours, &motion);

        let (mut n, mut within_one, mut exact) = (0usize, 0usize, 0usize);
        for (our_row, their_run) in ours.values.iter().zip(&theirs.packet.radials) {
            for (our_ms, &their_gate) in our_row.iter().zip(&their_run.gate_values) {
                if their_gate < 2 {
                    continue;
                }
                let our_level =
                    (our_ms / KT_TO_MS * theirs.scale as f64).round() + theirs.offset as f64;
                let diff = (our_level - their_gate as f64).abs();
                n += 1;
                exact += usize::from(diff == 0.0);
                within_one += usize::from(diff <= 1.0);
            }
        }
        assert_eq!(n, 720 * 40, "every gate compared");
        assert_eq!(
            within_one, n,
            "the two arithmetics may differ by float rounding alone"
        );
        assert!(
            exact as f64 / n as f64 > 0.999,
            "more than rounding separates the ports: {exact}/{n} exact"
        );
    }

    /// A Level III velocity packet as this module's grid: m/s through the
    /// PDB's scale/offset, radial-centre azimuths, gate centres at
    /// `(first_range_bin + j + 0.5) · 0.25 km`. Protocol B's "ours" side.
    fn grid_from_packet(
        packet: &nexrad_level3::model::RadialPacket,
        pdb: &nexrad_level3::model::ProductDescriptionBlock,
    ) -> VelocityGrid {
        let scale = pdb.data_scale() as f64;
        let offset = pdb.data_offset() as f64;
        let gate_km = pdb.range_gate_km().unwrap_or(0.25);
        VelocityGrid {
            values: packet
                .radials
                .iter()
                .map(|run| {
                    run.gate_values
                        .iter()
                        .map(|&g| {
                            if g < 2 {
                                f64::NAN
                            } else {
                                (g as f64 - offset) / scale
                            }
                        })
                        .collect()
                })
                .collect(),
            azimuths_deg: packet
                .radials
                .iter()
                .map(|run| run.start_angle as f64 + run.angle_delta as f64 / 2.0)
                .collect(),
            gate_count: packet
                .radials
                .iter()
                .map(|r| r.gate_values.len())
                .max()
                .unwrap_or(0),
            first_gate_range_km: (packet.first_range_bin as f64 + 0.5) * gate_km,
            gate_interval_km: gate_km,
        }
    }

    fn radial(azimuth: f32, elevation: f32, gates: Vec<u8>) -> Radial {
        Radial::new(
            0,
            0,
            azimuth,
            0.5,
            RadialStatus::IntermediateRadialData,
            1,
            elevation,
            None,
            Some(MomentData::from_fixed_point(
                gates.len() as u16,
                2125,
                250,
                8,
                2.0,
                129.0,
                gates,
            )),
            None,
            None,
            None,
            None,
            None,
        )
    }

    /// `dealiased_grid` runs the Coverage profile: an isolated velocity
    /// pocket the propagation passes never reach survives, where NROT's
    /// posture censors it. This is the behavioural difference the profile
    /// parameter exists for, observed through this module's public entry.
    #[test]
    fn the_display_dealias_keeps_isolated_pockets_the_nrot_posture_drops() {
        let n = 72;
        let gates = 40;
        // Mostly empty sweep: a 2×3 pocket of 20 m/s at long range, one far
        // bin pinning the Nyquist estimate at 26 m/s.
        let radials: Vec<Radial> = (0..n)
            .map(|i| {
                let mut bytes = vec![0u8; gates]; // 0 = below threshold
                if (30..32).contains(&i) {
                    for b in bytes.iter_mut().take(39).skip(36) {
                        *b = 129 + 40; // 20 m/s
                    }
                }
                if i == 0 {
                    bytes[39] = 129 + 52; // 26 m/s
                }
                radial(i as f32 * 5.0, 0.5, bytes)
            })
            .collect();

        let grid = dealiased_grid(&radials, 0.5, None).expect("velocity present");
        assert!(
            (grid.values[30][37] - 20.0).abs() < 1e-6,
            "Coverage keeps the unreached pocket: {}",
            grid.values[30][37],
        );

        // The same sweep through NROT's preprocessing censors it — the two
        // profiles really are different postures over one dealiaser.
        let raw = velocity_grid(&radials).expect("velocity present");
        let mut strict = raw.values.clone();
        crate::nrot::dealias(
            &mut strict,
            &raw.sweep(),
            0.5,
            None,
            DealiasProfile::NoFalseShear,
        );
        assert!(
            strict[30][37].is_nan(),
            "NoFalseShear censors the same pocket"
        );
    }

    /// The full per-tilt derivation end to end on a clean sweep: dealias is
    /// a no-op on continuous data, so the output is base velocity plus the
    /// correction — and the geometry comes through for the renderer.
    #[test]
    fn compute_srv_derives_a_full_sweep() {
        let n = 72;
        let radials: Vec<Radial> = (0..n)
            .map(|i| {
                let az = i as f64 * 360.0 / n as f64;
                let v_ms = 15.0 * az.to_radians().sin(); // zero-isodop north
                let byte = (129.0 + v_ms * 2.0).round() as u8;
                radial(az as f32, 0.5, vec![byte; 40])
            })
            .collect();
        let motion = SrvMotion {
            speed_kt: 20.0,
            direction_deg: 0.0,
            source: StormMotionSource::UserOverride,
        };
        let grid = compute_srv_grid(&radials, 0.5, None, &motion).expect("velocity present");
        assert_eq!(grid.gate_count, 40);
        assert!((grid.first_gate_range_km - 2.125).abs() < 1e-9);
        assert!((grid.gate_interval_km - 0.25).abs() < 1e-9);
        // At azimuth 0 the storm comes straight down the radial: +20 kt.
        assert!(
            (grid.values[0][0] - 20.0 * KT_TO_MS).abs() < 0.3,
            "az 0: {}",
            grid.values[0][0],
        );
        // At azimuth 90: base 15 m/s, correction zero.
        assert!(
            (grid.values[18][0] - 15.0).abs() < 0.3,
            "az 90: {}",
            grid.values[18][0],
        );
    }

    /// The override is dominant and Bunkers is only a default — and a
    /// non-override sample can never pose as one.
    #[test]
    fn the_user_override_dominates_bunkers() {
        let p = profile_at_centres(|l| (10.0 + 2.0 * l as f64, 0.0), 20);
        let over = SrvMotion::user_override(45.0, 210.0).expect("finite");
        let picked = storm_motion(Some(&p), Some(over)).expect("an override is a vector");
        assert_eq!(picked.speed_kt, 45.0);
        assert_eq!(picked.direction_deg, 210.0);
        assert_eq!(picked.source, StormMotionSource::UserOverride);

        let default = storm_motion(Some(&p), None).expect("Bunkers from the profile");
        assert_eq!(default.source, StormMotionSource::BunkersRightMover);

        // Mislabelled sample: claims Bunkers, arrives in the override slot.
        let poser = SrvMotion {
            speed_kt: 1.0,
            direction_deg: 2.0,
            source: StormMotionSource::BunkersRightMover,
        };
        let picked = storm_motion(Some(&p), Some(poser)).expect("falls through to Bunkers");
        assert_eq!(picked.source, StormMotionSource::BunkersRightMover);
        assert_ne!(picked.speed_kt, 1.0);

        assert_eq!(storm_motion(None, None), None, "no vector, no render");
        assert!(SrvMotion::user_override(f32::NAN, 90.0).is_none());
        assert!(SrvMotion::user_override(30.0, f32::INFINITY).is_none());
    }

    // ---- The validation policy, held offline. ----

    /// The bars are what the campaign says they are, stated absolutely so a
    /// lowered constant fails here rather than shipping.
    #[test]
    fn the_bars_are_pinned() {
        assert_eq!(WITHIN_ONE_PCT, 99.0);
        assert_eq!(COVERAGE_MIN_PCT, 95.0);
        assert_eq!(MIN_SITES, 3);
        assert_eq!(MIN_DEFINED_GATES, 10_000);
        assert_eq!(PORT_CHECK_PCT, 99.9);
        assert!(meets_level_bar(99.0), "the bar is inclusive");
        assert!(!meets_level_bar(98.99));
        assert!(meets_coverage_bar(95.0));
        assert!(!meets_coverage_bar(94.99));
        assert!(meets_port_bar(99.9));
        assert!(!meets_port_bar(99.89));
        assert!(site_is_conclusive(10_000));
        assert!(!site_is_conclusive(9_999));
    }

    /// The quarantine table ships empty. Populating it is a recorded
    /// decision, not a tweak — this test is what makes it one.
    #[test]
    fn no_site_is_quarantined() {
        assert!(QUARANTINED.is_empty(), "record numbers in the module docs");
        assert!(!is_quarantined("KMPX"));
    }

    /// The comparison lattice reads the derived grid exactly as the tally
    /// reads the Level III packet: cell-centre azimuth, nearest gate centre.
    #[test]
    fn the_comparison_grid_samples_cell_centres() {
        // 720 half-degree radials; value encodes the radial index so the
        // azimuth resolution is observable. Gates every 0.25 km from
        // 2.125 km, value offset encodes the gate.
        let n = 720;
        let grid = VelocityGrid {
            values: (0..n)
                .map(|i| (0..40).map(|j| (i * 100 + j) as f64).collect())
                .collect(),
            azimuths_deg: (0..n).map(|i| i as f64 * 0.5 + 0.25).collect(),
            gate_count: 40,
            first_gate_range_km: 2.125,
            gate_interval_km: 0.25,
        };
        let sampled = sample_to_comparison_grid(&grid);
        assert_eq!(sampled.len(), 360);
        assert_eq!(sampled[0].len(), crate::volumetric::RANGE_BINS);

        // Cell az 10 reads its centre 10.5°, which the radial centred at
        // 10.25° covers ([10.0, 10.5)) — no: 10.5 is the *start* of the
        // radial centred 10.75. The slot table must resolve exactly that.
        let radial_for_cell_10 = (sampled[10][2] as usize) / 100;
        assert_eq!(
            grid.azimuths_deg[radial_for_cell_10], 10.75,
            "the radial whose [start, end) covers 10.5°",
        );
        // Cell r=2 spans [2, 3) km; candidates centre at 2.125 (j=0) …
        // 2.875 (j=3); nearest to 2.5 is 2.375 (j=1), the tie at 2.625
        // going to the earlier gate.
        assert_eq!((sampled[10][2] as usize) % 100, 1, "gate j=1 is nearest");
        // Cells before the first gate stay undefined.
        assert!(sampled[10][0].is_nan(), "no gate centre falls under 2 km");
        assert!(sampled[10][1].is_nan(), "cell [1,2) has no gate either");
    }

    /// Coverage percentage reads derived-over-twin and survives an empty
    /// twin without NaN.
    #[test]
    fn coverage_pct_is_derived_over_twin() {
        use crate::twin::compare::Tally;
        let t = Tally {
            derived_defined: 950,
            l3_defined: 1000,
            ..Default::default()
        };
        assert!((coverage_pct(&t) - 95.0).abs() < 1e-12);
        assert!(meets_coverage_bar(coverage_pct(&t)));
        let empty = Tally::default();
        assert_eq!(coverage_pct(&empty), 0.0);
    }
}

// ── Live validation ──────────────────────────────────────────────────────────

/// Protocol A — our dealiased Level II velocity against the RPG's own
/// dealiased velocity products, the real oracle — and Protocol B — the SRM
/// arithmetic port against [`crate::srm::derive`] on live data.
///
/// ```text
/// cargo test -p rustdar-radar --release --lib -- --ignored --nocapture live_srv
/// ```
///
/// Native-only, `#[ignore]`d: hits the live S3 buckets. Everything that
/// decides pass/fail lives in [`validation_policy`], which the default
/// suite pins offline.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod live_validation {
    use super::validation_policy::*;
    use super::*;
    use crate::sources::DataSources;
    use crate::twin::compare::{ProductKind, Tally, tally_against_l3, volume_scan_started};
    use crate::twin::live::{SITES, candidate_keys, l2_volume_near, l3_twin};
    use nexrad_level3::model::Level3Message;

    /// The four dealiased-velocity twins, lowest first — the same bucket
    /// keys the Level III SRM pipeline fetched as tilts.
    const VELOCITY_TWINS: [&str; 4] = ["N0G", "N1G", "N2U", "N3U"];

    /// The velocity sweep of `scan` matching a twin: the twin's PDB angle,
    /// with SAILS/MRLE repeats at one angle told apart by proximity to the
    /// product's generation time (the cut completes, the product is
    /// published within seconds).
    fn matching_sweep<'a>(
        scan: &'a nexrad_model::data::Scan,
        twin: &Level3Message,
    ) -> Option<&'a [nexrad_model::data::Radial]> {
        let angle = twin.pdb.elevation_angle();
        let generated_ms = generated_at(twin)?.and_utc().timestamp_millis();
        scan.sweeps()
            .iter()
            .filter(|sweep| {
                let radials = sweep.radials();
                radials.first().is_some_and(|r| r.velocity().is_some())
                    && radials.len() >= 3
                    && crate::volumetric::sweep_elevation_deg(radials)
                        .is_some_and(|e| (e - angle as f64).abs() < 0.3)
            })
            .min_by_key(|sweep| {
                sweep
                    .radials()
                    .iter()
                    .map(nexrad_model::data::Radial::collection_timestamp)
                    .max()
                    .map(|end| (end - generated_ms).abs())
                    .unwrap_or(i64::MAX)
            })
            .map(|sweep| sweep.radials())
    }

    /// Halfword 24's day 1 is 1970-01-01 — the SRM harness's convention.
    fn generated_at(msg: &Level3Message) -> Option<chrono::NaiveDateTime> {
        let days = u64::from(msg.pdb.generation_date).checked_sub(1)?;
        chrono::NaiveDate::from_ymd_opt(1970, 1, 1)?
            .checked_add_days(chrono::Days::new(days))?
            .and_hms_opt(0, 0, 0)?
            .checked_add_signed(chrono::Duration::seconds(i64::from(
                msg.pdb.generation_time,
            )))
    }

    /// One tilt of Protocol A under a given knob posture. Returns the tally
    /// plus the twin's cut labels for the print.
    fn tilt_tally(
        scan: &nexrad_model::data::Scan,
        twin: &Level3Message,
        profile: Option<&WindProfile>,
        knobs: crate::nrot::DealiasKnobs,
    ) -> Option<Tally> {
        let radials = matching_sweep(scan, twin)?;
        let mut grid = velocity_grid(radials)?;
        let elevation = twin.pdb.elevation_angle() as f64;
        let sweep_view = crate::nrot::VelocitySweep {
            vel_grid: &grid.values.clone(),
            azimuths_deg: &grid.azimuths_deg,
            gate_count: grid.gate_count,
            first_gate_range_km: grid.first_gate_range_km,
            gate_interval_km: grid.gate_interval_km,
        };
        crate::nrot::dealias_with_knobs(&mut grid.values, &sweep_view, elevation, profile, knobs);
        let sampled = sample_to_comparison_grid(&grid);
        tally_against_l3(&sampled, twin, ProductKind::Numeric)
    }

    /// The twin's own data levels really are 0.5 m/s — what "±1 level"
    /// means everywhere here. Products 154/99 encode min/increment in
    /// tenths of m/s in threshold halfwords 31–32.
    fn assert_half_ms_levels(site: &str, code: &str, twin: &Level3Message) {
        let increment = twin.pdb.thresholds[1] as i16;
        assert_eq!(
            increment, 5,
            "{site} {code}: increment halfword is {increment} tenths of m/s, not 5 \
             — the ±1-level bar would not be ±0.5 m/s",
        );
        assert_eq!(twin.pdb.data_scale(), 2.0, "{site} {code}: scale");
    }

    /// **Protocol A.** Per site, per tilt: dealiased Level II velocity
    /// (Coverage profile, no median filter, pre-SRM) against the RPG's own
    /// `N0G`/`N1G`/`N2U`/`N3U` of the same volume and cut. Asserted at
    /// [`WITHIN_ONE_PCT`] within one 0.5 m/s level and [`COVERAGE_MIN_PCT`]
    /// of the twin's defined gates, on every tilt thick enough to read;
    /// the run is conclusive at [`MIN_SITES`] sites.
    ///
    /// Also prints, per site, the Bunkers right-mover against the volume's
    /// own `N0S` vector — measured, never asserted; see the module docs.
    #[ignore = "hits the live S3 buckets"]
    #[tokio::test]
    async fn live_srv_dealias_agrees_with_the_rpgs_own_velocity() {
        crate::tls::init();
        let sources = DataSources::production();
        let now = chrono::Utc::now().naive_utc();
        let mut conclusive_sites = 0usize;

        for &site in SITES {
            let Some((scan, l2_start)) = l2_volume_near(site, now).await else {
                println!("{site}: no archived Level II volume");
                continue;
            };
            let profile = volume_wind_profile(&scan);
            if profile.is_none() {
                println!("{site}: no wind profile fit (dealias runs unseeded)");
            }

            let mut site_compared = 0usize;
            let mut asserted_any = false;
            let mut site_failed: Vec<String> = Vec::new();
            for code in VELOCITY_TWINS {
                let Some(twin) = l3_twin(&sources, site, code, l2_start, None).await else {
                    println!("  {site} {code}: no twin for volume {l2_start}");
                    continue;
                };
                assert_half_ms_levels(site, code, &twin.message);
                let Some(t) = tilt_tally(
                    &scan,
                    &twin.message,
                    profile.as_ref(),
                    DealiasProfile::Coverage.knobs(),
                ) else {
                    println!("  {site} {code}: no matching velocity sweep");
                    continue;
                };
                let cov = coverage_pct(&t);
                println!(
                    "  {site} {code} ({:.1}°, cut {}): n={} exact={:.2}% within1={:.2}% \
                     within2={:.2}% coverage={:.1}% (ours {} vs twin {}){}",
                    twin.message.pdb.elevation_angle(),
                    twin.message.pdb.elevation_number,
                    t.compared,
                    t.exact_pct(),
                    t.within_one_pct(),
                    t.within_two_pct(),
                    cov,
                    t.derived_defined,
                    t.l3_defined,
                    if tilt_is_asserted(&t) {
                        ""
                    } else {
                        "  [thin, not asserted]"
                    },
                );
                site_compared += t.compared;
                if tilt_is_asserted(&t) && !is_quarantined(site) {
                    asserted_any = true;
                    if !meets_level_bar(t.within_one_pct()) {
                        site_failed.push(format!(
                            "{code} within1 {:.2}% < {WITHIN_ONE_PCT}%",
                            t.within_one_pct()
                        ));
                    }
                    if !meets_coverage_bar(cov) {
                        site_failed
                            .push(format!("{code} coverage {cov:.1}% < {COVERAGE_MIN_PCT}%"));
                    }
                }
            }

            // Bunkers against the RPG's own vector, printed never asserted.
            if let Some(p) = profile.as_ref() {
                print_bunkers_divergence(&sources, site, l2_start, p).await;
            }

            assert!(
                site_failed.is_empty(),
                "{site}: {:?}. If this is genuinely beyond the derivation, quarantine the \
                 site with its numbers and eliminations in validation_policy::QUARANTINED — \
                 do not widen a bar.",
                site_failed,
            );
            if asserted_any && site_is_conclusive(site_compared) {
                conclusive_sites += 1;
                println!("{site}: PASS over {site_compared} gates");
            } else if site_compared > 0 {
                println!("{site}: measured {site_compared} gates, not conclusive");
            }
        }

        assert!(
            conclusive_sites >= MIN_SITES,
            "only {conclusive_sites} sites were conclusive; {MIN_SITES} are required — re-run \
             when more of the roster has recent volumes and twins",
        );
        println!("PROTOCOL A: {conclusive_sites} sites conclusive, all asserted tilts passed");
    }

    /// The Bunkers right-mover against the same volume's `N0S` vector.
    async fn print_bunkers_divergence(
        sources: &DataSources,
        site: &str,
        l2_start: chrono::NaiveDateTime,
        profile: &WindProfile,
    ) {
        let Some(n0s) = l3_twin(sources, site, "N0S", l2_start, None).await else {
            println!("  {site} Bunkers: no N0S for this volume to compare against");
            return;
        };
        let Some(rpg) = n0s.message.pdb.storm_motion() else {
            return;
        };
        match bunkers_right_mover(profile) {
            Some(b) => {
                let ddir = (b.direction_deg - rpg.direction_deg + 540.0).rem_euclid(360.0) - 180.0;
                println!(
                    "  {site} Bunkers {:.1} kt/{:.0}° vs RPG SCIT {:.1} kt/{:.0}° \
                     (Δspeed {:+.1} kt, Δdir {:+.0}°){}",
                    b.speed_kt,
                    b.direction_deg,
                    rpg.speed_kt,
                    rpg.direction_deg,
                    b.speed_kt - rpg.speed_kt,
                    ddir,
                    if rpg.speed_kt == 0.0 {
                        "  [RPG vector zero]"
                    } else {
                        ""
                    },
                );
            }
            None => println!(
                "  {site} Bunkers: profile refused (RPG {:.1} kt/{:.0}°)",
                rpg.speed_kt, rpg.direction_deg,
            ),
        }
    }

    /// **Protocol B.** Vector-isolated port check: the newest `N0S` vector
    /// applied by this module's m/s arithmetic and by [`crate::srm::derive`]
    /// to the *same* `N0G` of the same volume; both quantized to the derived
    /// 0.5 kt levels and to [`crate::srm::RPG_LEVEL_EDGES`]. The velocity
    /// input is identical on both sides, so anything past float rounding is
    /// a ported-convention bug — asserted at [`PORT_CHECK_PCT`] within ±1
    /// derived level per site.
    #[ignore = "hits the live S3 bucket"]
    #[tokio::test]
    async fn live_srv_port_check_matches_the_level3_derivation() {
        crate::tls::init();
        let sources = DataSources::production();
        let now = chrono::Utc::now().naive_utc();
        let mut sites_checked = 0usize;

        for &site in SITES {
            let Ok(n0s) = crate::level3::fetch_latest_product(&sources, site, "N0S", now).await
            else {
                println!("{site}: no N0S");
                continue;
            };
            let Some(sample) = crate::srm::StormMotionSample::from_message(&n0s.message) else {
                continue;
            };
            if sample.motion.speed_kt == 0.0 {
                println!("{site}: zero vector — the correction would be untested");
                continue;
            }
            let Some(started) = volume_scan_started(&n0s.message.pdb) else {
                continue;
            };
            let Some(n0g) = bucket_same_volume(&sources, site, "N0G", &n0s.message, started).await
            else {
                println!("{site}: no N0G from the N0S volume");
                continue;
            };

            let theirs = crate::srm::derive(&n0g, &sample).expect("N0G derives");
            let packet = crate::srm::radial_packet(&n0g).expect("N0G carries radials");
            let motion = SrvMotion {
                speed_kt: sample.motion.speed_kt,
                direction_deg: sample.motion.direction_deg,
                source: StormMotionSource::UserOverride,
            };
            let scale = n0g.pdb.data_scale() as f64;
            let offset = n0g.pdb.data_offset() as f64;

            let (mut n, mut within_one, mut exact, mut rpg_within_one) =
                (0usize, 0usize, 0usize, 0usize);
            for (run, their_run) in packet.radials.iter().zip(&theirs.packet.radials) {
                let az = run.start_angle as f64 + run.angle_delta as f64 / 2.0;
                let component = motion.speed_kt as f64
                    * KT_TO_MS
                    * (motion.direction_deg as f64 - az).to_radians().cos();
                for (&gate, &their_gate) in run.gate_values.iter().zip(&their_run.gate_values) {
                    if gate < 2 || their_gate < 2 {
                        continue;
                    }
                    let ours_ms = (gate as f64 - offset) / scale + component;
                    let our_level =
                        (ours_ms / KT_TO_MS * theirs.scale as f64).round() + theirs.offset as f64;
                    let diff = (our_level - their_gate as f64).abs();
                    n += 1;
                    exact += usize::from(diff == 0.0);
                    within_one += usize::from(diff <= 1.0);
                    let ours_rpg = crate::srm::quantize_to_rpg_levels((ours_ms / KT_TO_MS) as f32);
                    let theirs_rpg = crate::srm::quantize_to_rpg_levels(
                        (their_gate as f32 - theirs.offset) / theirs.scale,
                    );
                    rpg_within_one += usize::from((ours_rpg as i32 - theirs_rpg as i32).abs() <= 1);
                }
            }
            if n == 0 {
                println!("{site}: no overlapping gates");
                continue;
            }
            let pct = 100.0 * within_one as f64 / n as f64;
            println!(
                "{site}: vector {:.1} kt/{:.1}°, n={n} exact={:.2}% within1(0.5 kt)={:.3}% \
                 within1(RPG levels)={:.3}%",
                sample.motion.speed_kt,
                sample.motion.direction_deg,
                100.0 * exact as f64 / n as f64,
                pct,
                100.0 * rpg_within_one as f64 / n as f64,
            );
            assert!(
                meets_port_bar(pct),
                "{site}: the m/s arithmetic disagrees with srm::derive on {:.3}% of {n} gates \
                 — a ported-convention bug (sign, centre azimuth, or kt/m-s), not live noise",
                100.0 - pct,
            );
            sites_checked += 1;
        }
        assert!(
            sites_checked >= MIN_SITES,
            "only {sites_checked} sites carried a nonzero vector with a matching N0G",
        );
        println!("PROTOCOL B: {sites_checked} sites, all at or above {PORT_CHECK_PCT}%");
    }

    /// The bucket object of `code` from the same volume as `like`, nearest
    /// its volume start — the SRM harness's pairing rule, locally.
    async fn bucket_same_volume(
        sources: &DataSources,
        site: &str,
        code: &str,
        like: &Level3Message,
        near: chrono::NaiveDateTime,
    ) -> Option<Level3Message> {
        for key in candidate_keys(sources, site, code, near).await {
            let url = sources.level3_object_url(&key);
            let Ok(bytes) = crate::archive::get_bytes(crate::archive::shared_client(), url).await
            else {
                continue;
            };
            let Ok(message) = nexrad_level3::decode::decode_product(&bytes) else {
                continue;
            };
            if message.pdb.volume_key() == like.pdb.volume_key() {
                return Some(message);
            }
        }
        None
    }

    /// The Coverage knob A/B, for tuning only — prints, asserts nothing.
    ///
    /// Four postures per tilt per site: the NoFalseShear knobs unchanged,
    /// kept-raw floor dropped to 1, censor disabled, and both. Run it on
    /// the **decision sites** (below), choose, then confirm the choice on
    /// the rest of the roster via the main Protocol A run — the
    /// site-diversity rule, mechanised.
    #[ignore = "hits the live S3 buckets; tuning aid, not a gate"]
    #[tokio::test]
    async fn live_srv_coverage_knob_ab() {
        crate::tls::init();
        // Climatologically spread, deliberately: upper midwest, southern
        // plains, gulf coast, mountain west, Appalachian southeast. The
        // remaining seventeen SITES are the holdout.
        const DECISION_SITES: [&str; 5] = ["KMPX", "KTLX", "KMOB", "KMTX", "KMRX"];
        let variants: [(&str, crate::nrot::DealiasKnobs); 4] = [
            ("rawmin16/censor1.24", DealiasProfile::NoFalseShear.knobs()),
            ("rawmin1/censor1.24", DealiasProfile::Coverage.knobs()),
            (
                "rawmin16/censor-off",
                crate::nrot::DealiasKnobs {
                    rawmin_bins: 16,
                    censor_vny_frac: f64::INFINITY,
                },
            ),
            (
                "rawmin1/censor-off",
                crate::nrot::DealiasKnobs {
                    rawmin_bins: 1,
                    censor_vny_frac: f64::INFINITY,
                },
            ),
        ];

        let sources = DataSources::production();
        let now = chrono::Utc::now().naive_utc();
        for site in DECISION_SITES {
            let Some((scan, l2_start)) = l2_volume_near(site, now).await else {
                println!("{site}: no archived Level II volume");
                continue;
            };
            let profile = volume_wind_profile(&scan);
            for code in VELOCITY_TWINS {
                let Some(twin) = l3_twin(&sources, site, code, l2_start, None).await else {
                    continue;
                };
                for (label, knobs) in variants {
                    let Some(t) = tilt_tally(&scan, &twin.message, profile.as_ref(), knobs) else {
                        continue;
                    };
                    println!(
                        "  {site} {code} {label}: n={} within1={:.2}% coverage={:.1}%",
                        t.compared,
                        t.within_one_pct(),
                        coverage_pct(&t),
                    );
                }
            }
        }
    }
}
