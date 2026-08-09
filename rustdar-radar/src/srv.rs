//! Storm-relative velocity derived locally from the Level II velocity volume.
//!
//! The Level III pipeline this replaces ([`crate::srm`]) fetched five bucket
//! objects per site — `N0S` for the storm motion vector in its PDB and
//! `N0G`/`N1G`/`N2U`/`N3U` as the four dealiased tilts — because Level II
//! velocity is aliased and, at the time, nothing local could unfold it.
//! The local dealiaser ([`crate::nrot::dealias`], a validity-marking
//! multi-pass calibrated against a reference implementation) removed that
//! constraint, so the whole product can now be computed from the volume
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
//! sign was settled by measurement over a million live gates (branch
//! `campaign-harness`), and are pinned here offline against `srm::derive`
//! itself.
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
//! Both knob choices were A/B'd live against the RPG's own dealiased
//! velocity — products 154/99, the `N0G`/`N1G`/`N2U`/`N3U` twins of the
//! same Level II volume and cut — on five climatologically spread decision
//! sites with the rest of the roster as holdout (the record lives on
//! branch `campaign-harness`):
//!
//! * kept-raw floor 16 → 1: large coverage gains everywhere (the floor-16
//!   posture would *fail* the coverage bar at some sites' upper tilts) for
//!   a negligible within-±1 cost — every decision site, same verdict;
//! * censor off: coverage gains almost nothing anywhere, and within-±1
//!   *drops* wherever real folding exists — a kept fold wall is a 2·Vny
//!   error on every gate it touches. The censor stays.
//!
//! The holdout confirmed the choice: the full-roster Protocol A survey ran
//! on the shipped knobs and passed everywhere.
//!
//! # Validation status
//!
//! **The live harness, its `validation_policy` (bars, quarantine table),
//! and the full survey record live on branch `campaign-harness`**;
//! re-measuring means that branch.
//!
//! As last measured, all roster sites: **Protocol A** — the dealiased grid
//! (Coverage profile, no median filter, pre-SRM) against the RPG's own
//! dealiased velocity, per site per tilt — passed the within-±1 and
//! coverage bars on every site-tilt, none quarantined. Unlike EET/DVL,
//! whose oracle is the DQA-edited reflectivity chain no local derivation
//! can reach, the velocity twins are reproducible from raw Level II:
//! unfolded gates carry identical 0.5 m/s codes on both sides, so the
//! residual is confined to fold regions and edited gates.
//!
//! **Protocol B** — the same `N0G` and the same `N0S` vector through this
//! module's m/s arithmetic and through [`crate::srm::derive`]'s knots
//! arithmetic — read 100% within ±1 derived level (0.5 kt) at every site
//! with a nonzero vector. The port is exact to float rounding.
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
//! numbers inform. As last surveyed (the per-site deltas live on branch
//! `campaign-harness`): on organised-convection days the two broadly
//! agree; direction deltas are large exactly where speeds are small and
//! SCIT is fitting few cells — on quiet days they are both guessing, and
//! the derived product at least guesses from the whole volume's hodograph
//! rather than from one cell track. (Profiles whose shear sat under the
//! floor once refused Bunkers outright; the mean-wind fallback now covers
//! that case.)
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

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
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
}
