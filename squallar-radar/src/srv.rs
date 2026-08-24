//! Storm-relative velocity derived locally from the Level II velocity volume.
//!
//! ```text
//! SRV(az, r) = V_dealiased(az, r) + speed · cos(direction − az)      [m/s]
//! ```
//!
//! per gate, with the correction applied at the radial's centre azimuth — the
//! angle the renderer centres the strip on — and `direction` the
//! meteorological "from" direction, which is why the term adds rather than
//! subtracts.
//!
//! The dealiaser runs [`crate::nrot::DealiasProfile::Coverage`] — every
//! unreached data gate kept at raw, the fold-wall censor kept at NROT's
//! measured 1.24·Vny — and skips NROT's median filter: a censored gate is a
//! hole in the couplet the product exists to show, and the RPG's own dealiased
//! products are not median-filtered.
//!
//! Storm motion is three rungs deep, resolved by [`storm_motion`] and carried
//! on the value by [`StormMotionSource`]:
//!
//! ```text
//! UserOverride       a vector the user typed in — dominant wherever set
//! RpgScitAverage     the RPG's own applied vector, read from N0S's PDB
//! MeanWind           0–6 km mean wind — the derived default
//! BunkersRightMover  supercell motion prediction — reached only on request
//! ```
//!
//! The RPG's vector is read from the `N0S` Product Description Block
//! (halfwords 51–52), paired to the volume by [`crate::level3::names_volume`].
//! It is the whole of the accuracy story — 80.6% in-band against the RPG's own
//! product where Bunkers scores 16.6% — and because the fine→coarse operator
//! is max-magnitude, the motion term must be added at the native
//! 0.25 km × 0.5° resolution before any rasterisation.
//!
//! Of the two derived rungs the mean wind is the default: median 9.2° of
//! direction error against the RPG's, against Bunkers' 59.1°, and it answers
//! on profiles [`bunkers_right_mover`] refuses outright.
//!
//! Bunkers et al. 2000, *Wea. Forecasting*, 15, 61–79, "Predicting Supercell
//! Motion Using a New Hodograph Technique", the ID method, computed from the
//! volume's own VAD-fitted wind profile:
//!
//! ```text
//! V_rm = V_mean + 7.5 m/s · (S × k̂) / |S|
//! V_mean = non-pressure-weighted mean wind, 0–6 km AGL
//! S      = (5.5–6 km mean wind) − (0–0.5 km mean wind)
//! S × k̂  = (S_v, −S_u)      — 90° clockwise of the shear vector
//! ```
//!
//! The bands are read at the profile's 0.3 km layer centres, so "0–0.5 km" is
//! layers centred 0.15/0.45 km and "5.5–6 km" layers centred 5.55/5.85 km — a
//! 0.1 km band skew the discretisation imposes.
//!
//! An `N0S` vector of exactly 0.0 kt from 0.0° is a reading, not a gap: SCIT
//! tracked no cells, so the RPG paints an unshifted field, and
//! [`SrvMotion::rpg_scit_average`] accepts it. Gate values are m/s, like every
//! Level II product.

use crate::nrot::{DealiasProfile, VelocitySweep, WindProfile};
use crate::velocity::VelocityGrid;
use nexrad_model::data::Radial;

/// Metres per second per knot.
pub const KT_TO_MS: f64 = 0.514_444;

/// Bunkers et al. 2000's deviation from the 0–6 km mean wind, m/s,
/// perpendicular-right of the 0–0.5 km → 5.5–6 km shear vector.
pub const BUNKERS_DEVIATION_MS: f64 = 7.5;

/// Depth of the Bunkers mean-wind layer, km AGL.
pub const BUNKERS_MEAN_DEPTH_KM: f64 = 6.0;

/// Where a storm motion vector came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StormMotionSource {
    /// A vector the user typed in. Dominant over every derived rung.
    UserOverride,
    /// The RPG's SCIT cell-track average, read from the `N0S` Product
    /// Description Block. The vector the reference product was built with.
    RpgScitAverage,
    /// The 0–6 km mean wind from the volume's own VAD profile — the derived
    /// default, measured closest to what the RPG publishes.
    MeanWind,
    /// Bunkers right-mover from the volume's own VAD wind profile — a
    /// prediction, not an average of what was tracked, so it is reached only
    /// through [`SrvFallback::BunkersRightMover`].
    BunkersRightMover,
}

impl StormMotionSource {
    /// Whether this is the vector the RPG itself applied.
    pub fn is_rpg(self) -> bool {
        self == Self::RpgScitAverage
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::UserOverride => "storm motion you entered",
            Self::RpgScitAverage => "NWS storm motion for this volume",
            Self::MeanWind => "0-6 km mean wind",
            Self::BunkersRightMover => "Bunkers right-mover",
        }
    }

    pub fn tag(self) -> &'static str {
        match self {
            Self::UserOverride => "yours",
            Self::RpgScitAverage => "NWS",
            Self::MeanWind => "mean wind",
            Self::BunkersRightMover => "Bunkers",
        }
    }
}

/// Which derived rung [`storm_motion`] falls to when there is no override and
/// no `N0S` vector for the volume.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Default,
    serde::Serialize,
    serde::Deserialize,
)]
pub enum SrvFallback {
    /// The 0–6 km mean wind. The default.
    #[default]
    MeanWind,
    /// The Bunkers right-mover, which degrades to the mean wind under
    /// [`BUNKERS_MIN_SHEAR_MS`].
    BunkersRightMover,
}

impl SrvFallback {
    /// The rung this preference selects, for a settings label.
    pub fn source(self) -> StormMotionSource {
        match self {
            Self::MeanWind => StormMotionSource::MeanWind,
            Self::BunkersRightMover => StormMotionSource::BunkersRightMover,
        }
    }
}

/// Everything [`storm_motion`] needs that the volume's own winds cannot
/// supply.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct MotionInputs {
    /// The user's `(speed_kt, direction_from_deg)`, if they entered one.
    pub user_override: Option<(f32, f32)>,
    /// The RPG's own `(speed_kt, direction_from_deg)` for this volume, from its `N0S`.
    pub rpg: Option<(f32, f32)>,
    /// Which derived rung to use when neither of the above arrived.
    pub fallback: SrvFallback,
}

impl MotionInputs {
    /// The resolved vector, or `None` for "no storm-relative render".
    pub fn resolve(self, profile: Option<&WindProfile>) -> Option<SrvMotion> {
        let user = self
            .user_override
            .and_then(|(speed, direction)| SrvMotion::user_override(speed, direction));
        let rpg = self
            .rpg
            .and_then(|(speed, direction)| SrvMotion::rpg_scit_average(speed, direction));
        storm_motion(profile, user, rpg, self.fallback)
    }
}

/// A storm motion vector in the product's conventions: knots, and the
/// meteorological direction the storm comes **from**.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SrvMotion {
    pub speed_kt: f32,
    /// Direction the storm comes *from*, degrees.
    pub direction_deg: f32,
    pub source: StormMotionSource,
}

impl SrvMotion {
    /// A vector the user typed in.
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

    /// The RPG's own applied vector, as carried in an `N0S` Product
    /// Description Block.
    pub fn rpg_scit_average(speed_kt: f32, direction_deg: f32) -> Option<Self> {
        if !speed_kt.is_finite() || !direction_deg.is_finite() {
            return None;
        }
        Some(Self {
            speed_kt,
            direction_deg,
            source: StormMotionSource::RpgScitAverage,
        })
    }
}

/// One already-decoded grid, dealiased in place for display: the Coverage
/// profile, no median filter.
pub fn dealias_grid(
    grid: &mut VelocityGrid,
    elevation_deg: f64,
    profile: Option<&WindProfile>,
    declared_nyquist_ms: Option<f64>,
) {
    // The dealiaser writes the values it is also reading the sweep's geometry
    // from, so the borrow it is handed has to be a copy.
    let reported = grid.values.clone();
    let sweep_view = VelocitySweep {
        vel_grid: &reported,
        azimuths_deg: &grid.azimuths_deg,
        gate_count: grid.gate_count,
        first_gate_range_km: grid.first_gate_range_km,
        gate_interval_km: grid.gate_interval_km,
        declared_nyquist_ms,
        status: Some(&grid.status),
    };
    // This profile sets `refuse_incoherent` off, so the mask is `None`.
    let _ = crate::nrot::dealias(
        &mut grid.values,
        &sweep_view,
        elevation_deg,
        profile,
        DealiasProfile::Coverage,
    );
}

/// [`dealias_grid`] straight off a sweep's radials, decoding it first.
pub fn dealiased_grid(
    radials: &[Radial],
    elevation_deg: f64,
    profile: Option<&WindProfile>,
    declared_nyquist_ms: Option<f64>,
) -> Option<VelocityGrid> {
    let mut grid = crate::velocity::grid(radials)?;
    dealias_grid(&mut grid, elevation_deg, profile, declared_nyquist_ms);
    Some(grid)
}

/// Add the storm-motion term to every defined gate, in place:
/// `v += speed · cos(direction − azimuth)`, at the radial's centre azimuth.
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

/// The full per-tilt derivation on an already-decoded grid: dealias
/// (Coverage, no median filter), then the storm-motion correction.
pub fn storm_relative_grid(
    mut grid: VelocityGrid,
    elevation_deg: f64,
    profile: Option<&WindProfile>,
    motion: &SrvMotion,
    declared_nyquist_ms: Option<f64>,
) -> VelocityGrid {
    dealias_grid(&mut grid, elevation_deg, profile, declared_nyquist_ms);
    apply_storm_motion(&mut grid, motion);
    grid
}

/// [`storm_relative_grid`] straight off a sweep's radials, decoding it first.
pub fn compute_srv_grid(
    radials: &[Radial],
    elevation_deg: f64,
    profile: Option<&WindProfile>,
    motion: &SrvMotion,
    declared_nyquist_ms: Option<f64>,
) -> Option<VelocityGrid> {
    Some(storm_relative_grid(
        crate::velocity::grid(radials)?,
        elevation_deg,
        profile,
        motion,
        declared_nyquist_ms,
    ))
}

/// The Bunkers et al. 2000 right-mover from a fitted wind profile, as a
/// motion vector `(u, v)` in m/s (the direction the storm moves *toward*),
/// or `None` when the profile cannot support it.
pub fn bunkers_right_mover_uv(profile: &WindProfile) -> Option<(f64, f64)> {
    bunkers_estimate(profile).map(|(uv, _)| uv)
}

/// [`bunkers_right_mover_uv`]'s arithmetic, plus which quantity it produced.
fn bunkers_estimate(profile: &WindProfile) -> Option<((f64, f64), StormMotionSource)> {
    let bands = profile_bands(profile)?;
    let (mean, head, tail) = (bands.mean, bands.head?, bands.tail?);
    let shear = (tail.0 - head.0, tail.1 - head.1);
    let magnitude = (shear.0 * shear.0 + shear.1 * shear.1).sqrt();
    if magnitude < BUNKERS_MIN_SHEAR_MS {
        // No shear direction worth deviating from: the propagation term is
        // dropped and the estimate is pure advection, reported as the mean
        // wind because that is what it is.
        return Some((mean, StormMotionSource::MeanWind));
    }
    // (S × k̂)/|S| = (S_v, −S_u)/|S|: perpendicular, 90° clockwise of S.
    Some((
        (
            mean.0 + BUNKERS_DEVIATION_MS * shear.1 / magnitude,
            mean.1 - BUNKERS_DEVIATION_MS * shear.0 / magnitude,
        ),
        StormMotionSource::BunkersRightMover,
    ))
}

/// The three layer means both derived rungs are built out of, computed once.
struct ProfileBands {
    /// 0–6 km mean wind, m/s. Present or the whole struct is `None`.
    mean: (f64, f64),
    /// 0–0.5 km mean wind, m/s, or `None` when no layer centre fell in it.
    head: Option<(f64, f64)>,
    /// 5.5–6 km mean wind, m/s, or `None` when no layer centre fell in it.
    tail: Option<(f64, f64)>,
}

/// [`ProfileBands`] off a fitted profile, or `None` when fewer than
/// [`BUNKERS_MIN_MEAN_LAYERS`] of the twenty 0–6 km layers carry a fit.
fn profile_bands(profile: &WindProfile) -> Option<ProfileBands> {
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
    if mean.2 < BUNKERS_MIN_MEAN_LAYERS {
        return None;
    }
    let average = |(u, v, n): (f64, f64, usize)| (n > 0).then(|| (u / n as f64, v / n as f64));
    Some(ProfileBands {
        mean: average(mean)?,
        head: average(head),
        tail: average(tail),
    })
}

/// The non-pressure-weighted 0–6 km AGL mean wind as `(u, v)` in m/s.
pub fn mean_wind_uv(profile: &WindProfile) -> Option<(f64, f64)> {
    profile_bands(profile).map(|bands| bands.mean)
}

/// [`mean_wind_uv`] in the product's conventions: knots, and the meteorological
/// direction the motion comes **from**.
pub fn mean_wind(profile: &WindProfile) -> Option<SrvMotion> {
    mean_wind_uv(profile).map(|(u, v)| motion_from_uv(u, v, StormMotionSource::MeanWind))
}

/// A motion vector `(u, v)` in m/s, toward-direction, as the knots-and-from
/// pair the rest of the product speaks.
fn motion_from_uv(u: f64, v: f64, source: StormMotionSource) -> SrvMotion {
    let speed_kt = (u * u + v * v).sqrt() / KT_TO_MS;
    // Toward-direction is atan2(u, v) in compass degrees; "from" is its reciprocal.
    let direction_deg = (u.atan2(v).to_degrees() + 180.0).rem_euclid(360.0);
    SrvMotion {
        speed_kt: speed_kt as f32,
        direction_deg: direction_deg as f32,
        source,
    }
}

/// Fewest fitted 0–6 km layers (of twenty) for a usable mean wind.
pub const BUNKERS_MIN_MEAN_LAYERS: usize = 12;

/// Smallest 0–0.5 → 5.5–6 km shear magnitude, m/s, whose direction is worth deviating
/// from — Bunkers' own dataset median is ~2.6× this.
pub const BUNKERS_MIN_SHEAR_MS: f64 = 2.0;

/// [`bunkers_right_mover_uv`] in the product's conventions: knots, and the
/// meteorological direction the motion comes **from**.
pub fn bunkers_right_mover(profile: &WindProfile) -> Option<SrvMotion> {
    let ((u, v), source) = bunkers_estimate(profile)?;
    Some(motion_from_uv(u, v, source))
}

/// The storm motion a render should apply, resolved down the chain: the user's
/// override, else the RPG's own vector for this volume, else the derived rung
/// `fallback` names — the 0–6 km mean wind by default.
pub fn storm_motion(
    profile: Option<&WindProfile>,
    user_override: Option<SrvMotion>,
    rpg: Option<SrvMotion>,
    fallback: SrvFallback,
) -> Option<SrvMotion> {
    if let Some(motion) = user_override {
        // Only an override may claim to be one.
        if motion.source == StormMotionSource::UserOverride {
            return Some(motion);
        }
    }
    if let Some(motion) = rpg {
        // Same guard: only a vector read from an `N0S` may claim that label.
        if motion.source == StormMotionSource::RpgScitAverage {
            return Some(motion);
        }
    }
    let profile = profile?;
    match fallback {
        SrvFallback::MeanWind => mean_wind(profile),
        SrvFallback::BunkersRightMover => bunkers_right_mover(profile),
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
