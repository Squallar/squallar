//! Normalized Rotation (NROT): the azimuthal derivative of radial velocity,
//! normalized by a range-dependent divisor so one number reads the same at
//! every distance from the radar. The pipeline is reverse-engineered against
//! a reference implementation: kernel taps, divisor curve, median geometry,
//! and gating are all empirical rather than derived. As last measured it
//! matches the reference's painted density and correlates 0.996 with its
//! cursor readouts; the measurement apparatus and the full calibration
//! record live on branch `campaign-harness`.
//!
//! 1. Dealias the base velocity with the validity-marking multi-pass
//!    ([`dealias`]): environmental-wind and zero-isodop seeds, then
//!    radial/azimuthal bridges, flood fills and head-and-shoulders until
//!    nothing changes; unreached data keeps raw in bulk, and residual fold
//!    walls are censored. Folded velocity reads as a ±2·Vny jump, which the
//!    derivative stage would misread as extreme shear.
//! 2. Median-filter the dealiased field (3–5 radials by physical width × 5
//!    gates); centres whose window is mostly missing raw data read ND.
//! 3. At each bin, the azimuthal derivative is the split-tap per-radial
//!    operator ([`SPLIT_CLEAN`]/[`SPLIT_AWAY`]) inside 80 km and the
//!    composite 11-radial stencil [`COMPOSITE_TAPS`] beyond, applied to
//!    3-gate range means and divided by the local arc per radial. The
//!    sign-reversed outer taps produce the small negative side lobes
//!    flanking every strong gradient. All five tap pairs must be intact and
//!    the profile must correlate with the stencil (r² ≥ 0.05); constant or
//!    incoherent profiles read ND.
//! 4. Divide ROT by the divisor curve — knot ranges in KILOMETRES, linearly
//!    interpolated (25 at ≤20 km → 20 → 12 → 8 at 80 km, flat beyond) — and
//!    clamp to ±5.
//!    Inside 80 km a matched-filter footprint pass ([`apply_kernel_bank`])
//!    then caps each detected rotation couplet with the kernel fitted to
//!    its measured pole width, reproducing the reference's width-dependent
//!    edge compression while monopolar notches keep the full value.
//! 5. Blank painted clusters under 4 bins and one-gate-deep slivers.
//!
//! Values above 1.0 are significant rotation; above 2.5, extreme. The
//! reference quantizes NROT in steps of 0.04, so differences below ~0.04
//! are not observable in its output at all.

use crate::beam::RE_EFF_KM;
// rayon on every target that has threads, the sequential stand-ins on wasm32.
use crate::par::*;
use std::f64::consts::PI;

const KM_PER_NM: f64 = 1.852;

/// NROT is defined on -5..+5; the divisor curve guarantees nothing, so clamp.
const NROT_LIMIT: f64 = 5.0;

/// Skip bins closer than this. Residual ground clutter close to the radar
/// produces clamp-level fake shear (adjacent ±30 m/s bins over tens of meters
/// of arc). Empirical floor — 12.5 km (6.75 nm), where the reference's own
/// painting starts. Measured provenance: branch `campaign-harness`.
const MIN_RANGE_NM: f64 = 6.75;

/// The magnitude at which this algorithm considers a bin **painted** — the
/// significance floor of the whole product, and the one number every consumer
/// of an NROT field has to agree with.
///
/// It is the threshold [`despeckle_nrot`] runs its 8-connected components
/// over: a bin under it is not part of any cluster, is never counted toward
/// [`DESPECKLE_MIN_BINS`], and survives only as a value nothing downstream is
/// meant to draw attention to. The 2D palette starts its first colour class
/// here too (`palette::NROT_CYCLONIC`'s "weak: slate…" stop), and the 3D
/// transparency profile takes its clear point from this constant by reference
/// (`voxel::volume_alpha_profile::NROT_CLEAR`) — so the value the algorithm
/// calls significant, the value the plan view first colours and the value the
/// volume first makes visible are one number and cannot drift apart.
pub const SIGNIFICANT: f64 = 0.25;

/// Blank painted clusters (8-connected runs of |NROT| ≥ [`SIGNIFICANT`])
/// smaller than this many bins. Empirical, chosen to match the reference's
/// painted density. Measured provenance: branch `campaign-harness`.
const DESPECKLE_MIN_BINS: usize = 4;

/// A velocity sweep as a dense azimuth × range grid. NaN marks missing data.
/// Rows are in sweep order, so row `i` borders rows `i±1` and the first and
/// last rows border each other.
pub struct VelocitySweep<'a> {
    pub vel_grid: &'a [Vec<f64>],
    pub azimuths_deg: &'a [f64],
    pub gate_count: usize,
    pub first_gate_range_km: f64,
    pub gate_interval_km: f64,
}

/// How much of the circle a sweep's radials must account for, at their own
/// measured spacing, before [`radial_step_deg`] calls the sweep closed and
/// takes `360 / n` for its step.
///
/// The two answers differ by exactly the fraction of the circle that is
/// missing, so this is what bounds the error the closed branch can carry: 2%,
/// which on a significant rotation of 1.0 is 0.02 — half the 0.04 step the
/// reference quantizes its own output in, and therefore invisible in the only
/// comparison this pipeline is calibrated against.
///
/// The margin on the other side is far wider than the noise it has to survive.
/// A complete cut's azimuths jitter by a few hundredths of a degree, which
/// moves the median of its 720 gaps by about a thousandth of one — 0.2% of a
/// step, against the 2% this leaves — so a cut that did close the circle
/// cannot fall through the test. Nor are the sweeps that do fall through near
/// cases: a 90° TDWR sector accounts for a quarter of the circle, a
/// half-received cut for half of it, and the abandoned 200° tail that
/// [`crate::azimuth::median_azimuth_step_deg`] exists for covers 55%.
const CLOSED_SWEEP_COVERAGE: f64 = 0.98;

/// How far apart this grid's adjacent rows are in azimuth, degrees — the step
/// every stencil's `arc_per_radial` is built from, and so the scale of every
/// NROT value this module reports.
///
/// On a sweep that closes the circle, `360 / n` is not an estimate of that
/// step: n radials laid around a circle leave n gaps summing to exactly 360°,
/// so their mean is exactly `360 / n` however much the antenna jittered on the
/// way round. A complete cut therefore takes it unmeasured, and every WSR-88D
/// VCP cut is a complete cut.
///
/// On a sweep that stops short, `360 / n` is the spacing of nothing. A 36°
/// sector of 72 radials is 0.5° apart and reads 5°, so every arc is ten times
/// too long and every rotation over it ten times too small — a tornadic
/// couplet at 1.8 comes back 0.18, under the 0.25 [`SIGNIFICANT`] floor, and
/// the product paints nothing where the strongest rotation in the sector is.
/// There the step is measured, and [`crate::azimuth::median_azimuth_step_deg`]
/// is where this crate measures it: **median**, so the one abandoned arc in a
/// half-received cut is not averaged into everyone's spacing, and **shared**,
/// so a sweep is differentiated at the same spacing the sampler serves it and
/// the plan view paints it at.
///
/// The declared `Radial::azimuth_spacing_degrees` is the other candidate and is
/// the wrong one here. What the stencils need is the angle between rows `i` and
/// `i+1` of the grid in front of them, which is a property of how the grid was
/// assembled — a sweep of 0.5° radials handed over every other radial has 1.0°
/// rows whatever each radial declares — and a declaration of zero, which the
/// derived grids' own synthetic radials are one refactor away from carrying,
/// would divide the arc to nothing and clamp the whole sweep to ±5. The
/// measurement cannot return zero: it drops zero gaps.
fn radial_step_deg(azimuths_deg: &[f64], num_radials: usize) -> f64 {
    let closed = 360.0 / num_radials.max(1) as f64;
    match crate::azimuth::median_azimuth_step_deg(azimuths_deg.iter().copied()) {
        Some(step) if step * (num_radials as f64) < CLOSED_SWEEP_COVERAGE * 360.0 => step,
        // Nothing to measure between — one radial, or a sweep that reported
        // one azimuth n times. Neither is a sector.
        _ => closed,
    }
}

/// Run the full pipeline without a wind profile (elevation assumed 0.5°).
/// Output is indexed like the input grid; NaN where NROT is undefined (no
/// velocity, or too few neighbours to fit).
pub fn compute_nrot_grid(sweep: &VelocitySweep) -> Vec<Vec<f64>> {
    compute_nrot_grid_with_profile(sweep, 0.5, None)
}

/// Run the full pipeline with a volume wind profile guiding fold-branch
/// decisions. The profile is fitted from every velocity tilt in the volume
/// via [`WindProfileBuilder`], so its predictions stay well-conditioned at
/// long range where the sweep's own echo fills only a narrow azimuth sector.
pub fn compute_nrot_grid_with_profile(
    sweep: &VelocitySweep,
    elevation_deg: f64,
    profile: Option<&WindProfile>,
) -> Vec<Vec<f64>> {
    let med = preprocess_velocity_with(sweep, elevation_deg, profile);
    let mut grid = llsd_nrot(sweep, &med);
    despeckle_nrot(&mut grid, DESPECKLE_MIN_BINS);
    grid
}

/// Blank painted clusters smaller than `min_bins`: 8-connected components of
/// |NROT| ≥ [`SIGNIFICANT`] (either sign — a tiny dipole is still speckle).
/// Azimuth wraps; range does not.
fn despeckle_nrot(grid: &mut [Vec<f64>], min_bins: usize) {
    let num_radials = grid.len();
    if num_radials == 0 {
        return;
    }
    let gate_count = grid[0].len();
    let painted = |g: &[Vec<f64>], i: usize, j: usize| {
        let v = g[i][j];
        !v.is_nan() && v.abs() >= SIGNIFICANT
    };
    let mut seen = vec![false; num_radials * gate_count];
    let mut stack = Vec::new();
    let mut comp = Vec::new();
    for i0 in 0..num_radials {
        for j0 in 0..gate_count {
            if seen[i0 * gate_count + j0] || !painted(grid, i0, j0) {
                continue;
            }
            comp.clear();
            stack.push((i0, j0));
            seen[i0 * gate_count + j0] = true;
            while let Some((i, j)) = stack.pop() {
                comp.push((i, j));
                for di in -1i32..=1 {
                    let ii = ((i as i32 + di).rem_euclid(num_radials as i32)) as usize;
                    for dj in -1i32..=1 {
                        let jj = j as i32 + dj;
                        if jj < 0 || jj >= gate_count as i32 {
                            continue;
                        }
                        let jj = jj as usize;
                        if !seen[ii * gate_count + jj] && painted(grid, ii, jj) {
                            seen[ii * gate_count + jj] = true;
                            stack.push((ii, jj));
                        }
                    }
                }
            }
            let (jmin, jmax) = comp
                .iter()
                .fold((usize::MAX, 0), |(lo, hi), &(_, j)| (lo.min(j), hi.max(j)));
            // A cluster one gate deep in range is a tangential sliver — an
            // artifact of hole-filling along a thin velocity arc, not a
            // rotation signature.
            if comp.len() < min_bins || jmax == jmin {
                for &(i, j) in &comp {
                    grid[i][j] = f64::NAN;
                }
            }
        }
    }
}

fn preprocess_velocity_with(
    sweep: &VelocitySweep,
    elevation_deg: f64,
    profile: Option<&WindProfile>,
) -> Vec<Vec<f64>> {
    let mut vel: Vec<Vec<f64>> = sweep.vel_grid.to_vec();
    dealias(
        &mut vel,
        sweep,
        elevation_deg,
        profile,
        DealiasProfile::NoFalseShear,
    );
    median_filter(
        &vel,
        sweep.vel_grid,
        sweep.gate_count,
        sweep.first_gate_range_km,
        sweep.gate_interval_km,
        radial_step_deg(sweep.azimuths_deg, sweep.vel_grid.len()),
    )
}

/// Wind-profile layer thickness, km.
const PROFILE_LAYER_KM: f64 = 0.3;
/// Layers span 0..12 km AGL.
const PROFILE_LAYERS: usize = 40;
/// Sample cap per layer keeps memory bounded on wasm.
const PROFILE_MAX_SAMPLES: usize = 16384;

/// Horizontal wind fitted per height layer from every velocity tilt of a
/// volume: vr ≈ u·sin(az)·cos(el) + v·cos(az)·cos(el) + c.
pub struct WindProfile {
    /// (u, v, c) per layer; NaN-filled layers had too little data.
    layers: Vec<Option<(f64, f64, f64)>>,
}

impl WindProfile {
    /// Build from explicit (height km, u, v) levels. The render path fits
    /// its profile from the volume ([`WindProfileBuilder`]); this constructor
    /// exists for callers that already hold levels — tests, mostly. Levels
    /// map to the internal layers; gaps between adjacent levels are filled
    /// by the nearer level.
    pub fn from_levels(levels: &[(f64, f64, f64)]) -> Option<Self> {
        if levels.is_empty() {
            return None;
        }
        let mut layers: Vec<Option<(f64, f64, f64)>> = vec![None; PROFILE_LAYERS];
        for &(h, u, v) in levels {
            let l = (h / PROFILE_LAYER_KM) as usize;
            if l < PROFILE_LAYERS {
                layers[l] = Some((u, v, 0.0));
            }
        }
        // Fill interior gaps from the nearest filled layer below/above.
        let filled: Vec<usize> = (0..PROFILE_LAYERS)
            .filter(|&l| layers[l].is_some())
            .collect();
        for l in 0..PROFILE_LAYERS {
            if layers[l].is_none() {
                let nearest = filled
                    .iter()
                    .min_by_key(|&&f| (f as i64 - l as i64).unsigned_abs());
                if let Some(&f) = nearest
                    && (f as i64 - l as i64).unsigned_abs() <= 3
                {
                    layers[l] = layers[f];
                }
            }
        }
        Some(WindProfile { layers })
    }

    /// Layer thickness the profile is discretised at, km. Public so a
    /// consumer integrating over height bands (Bunkers storm motion in
    /// [`crate::srv`]) can sample every layer exactly once via
    /// [`wind_at_km`](Self::wind_at_km) at the layer centres.
    pub const LAYER_KM: f64 = PROFILE_LAYER_KM;

    /// The fitted horizontal wind `(u, v)` in m/s at `height_km` AGL, or
    /// `None` below zero, above the profile, or in a layer nothing fit.
    /// Resolved at layer granularity ([`Self::LAYER_KM`]), no interpolation.
    pub fn wind_at_km(&self, height_km: f64) -> Option<(f64, f64)> {
        if !height_km.is_finite() || height_km < 0.0 {
            return None;
        }
        let l = (height_km / PROFILE_LAYER_KM) as usize;
        self.layers.get(l)?.map(|(u, v, _)| (u, v))
    }

    /// Predicted radial velocity at the given azimuth (radians), range (km)
    /// and elevation (degrees), or None where no layer was fit.
    fn predict(&self, az_rad: f64, range_km: f64, elevation_deg: f64) -> Option<f64> {
        let el = elevation_deg.to_radians();
        let h = crate::beam::height_km(range_km, elevation_deg);
        let l = (h / PROFILE_LAYER_KM) as usize;
        let layer = *self
            .layers
            .get(l)?
            .as_ref()
            .or_else(|| self.layers.get(l + 1)?.as_ref())
            .or_else(|| self.layers.get(l.wrapping_sub(1))?.as_ref())?;
        let (u, v, c) = layer;
        Some(u * az_rad.sin() * el.cos() + v * az_rad.cos() * el.cos() + c)
    }
}

/// [`crate::beam::height_km`] with `sin(elevation)` already computed.
///
/// The one place this crate still writes the beam-height expression out, and it
/// earns it: [`WindProfileBuilder::add_sweep`] hoists `sin` and `cos` once per
/// sweep and then runs this over every third gate of every radial — tens of
/// thousands of evaluations where the shared function would recompute
/// `to_radians().sin()` each time. (Its sibling in [`WindProfile::predict`]
/// hoisted nothing, so that one simply calls `beam::height_km`.)
///
/// Being a named function rather than an inline expression is the point: it is
/// what lets `the_hoisted_beam_height_is_bit_identical_to_the_shared_one` pin
/// the copy against `beam::height_km` **directly**, rather than against a
/// transcription of it in a test. That matters here more than elsewhere,
/// because NROT is the one calibrated path the echo-tops golden digests do not
/// cover, and `(h / PROFILE_LAYER_KM) as usize` **floors** — so a one-ulp drift
/// at a layer boundary silently moves a sample into the neighbouring wind layer.
#[inline]
fn height_km_with_sin_el(range_km: f64, sin_el: f64) -> f64 {
    range_km * sin_el + range_km * range_km / (2.0 * RE_EFF_KM)
}

/// Accumulates VAD samples per height layer across the volume's velocity
/// tilts, then fits each layer with one trimmed re-fit so folded bins in the
/// raw (not-yet-dealiased) sweeps cannot drag the wind estimate.
#[derive(Default)]
pub struct WindProfileBuilder {
    samples: Vec<Vec<(f64, f64, f64)>>, // (sin·cosθ, cos·cosθ, vr) per layer
}

impl WindProfileBuilder {
    pub fn new() -> Self {
        Self {
            samples: (0..PROFILE_LAYERS).map(|_| Vec::new()).collect(),
        }
    }

    pub fn add_sweep(&mut self, sweep: &VelocitySweep, elevation_deg: f64) {
        let el = elevation_deg.to_radians();
        let (sin_el, cos_el) = (el.sin(), el.cos());
        let n = sweep.vel_grid.len();
        for (i, row) in sweep.vel_grid.iter().enumerate() {
            let az = 2.0 * PI * i as f64 / n as f64;
            let (s, c) = (az.sin() * cos_el, az.cos() * cos_el);
            // Every 3rd gate is plenty for a 3-parameter fit per layer.
            for (j, v) in row.iter().enumerate().step_by(3) {
                if v.is_nan() {
                    continue;
                }
                let r = sweep.first_gate_range_km + j as f64 * sweep.gate_interval_km;
                let h = height_km_with_sin_el(r, sin_el);
                let l = (h / PROFILE_LAYER_KM) as usize;
                if l < PROFILE_LAYERS && self.samples[l].len() < PROFILE_MAX_SAMPLES {
                    self.samples[l].push((s, c, *v));
                }
            }
        }
    }

    pub fn finish(self) -> Option<WindProfile> {
        let mut any = false;
        let layers = self
            .samples
            .iter()
            .map(|pts| {
                let mut fit: Option<(f64, f64, f64)> = None;
                for _ in 0..2 {
                    let mut m = [[0.0f64; 3]; 3];
                    let mut b = [0.0f64; 3];
                    let mut n = 0u32;
                    for &(s, c, v) in pts {
                        if let Some((u, w, cc)) = fit
                            && (u * s + w * c + cc - v).abs() > 12.0
                        {
                            continue;
                        }
                        let x = [s, c, 1.0];
                        for r in 0..3 {
                            for q in 0..3 {
                                m[r][q] += x[r] * x[q];
                            }
                            b[r] += x[r] * v;
                        }
                        n += 1;
                    }
                    if n < 200 {
                        fit = None;
                        break;
                    }
                    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
                        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
                        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
                    if det.abs() < 1e-9 {
                        fit = None;
                        break;
                    }
                    let solve = |col: usize| {
                        let mut mm = m;
                        for (row, mm_row) in mm.iter_mut().enumerate() {
                            mm_row[col] = b[row];
                        }
                        (mm[0][0] * (mm[1][1] * mm[2][2] - mm[1][2] * mm[2][1])
                            - mm[0][1] * (mm[1][0] * mm[2][2] - mm[1][2] * mm[2][0])
                            + mm[0][2] * (mm[1][0] * mm[2][1] - mm[1][1] * mm[2][0]))
                            / det
                    };
                    fit = Some((solve(0), solve(1), solve(2)));
                }
                if fit.is_some() {
                    any = true;
                }
                fit
            })
            .collect();
        let mut layers: Vec<Option<(f64, f64, f64)>> = layers;
        // Clamp-extrapolate every unfitted layer from the nearest fitted one:
        // winds vary slowly with height, and a None prediction is worse than
        // the nearest fitted layer's — it vetoes every wind seed tile whose
        // beam reaches that height. Measured: without the extension most of
        // the reference's far band is lost (branch `campaign-harness`).
        let filled: Vec<usize> = (0..layers.len()).filter(|&l| layers[l].is_some()).collect();
        if !filled.is_empty() {
            for l in 0..layers.len() {
                if layers[l].is_none() {
                    let f = *filled
                        .iter()
                        .min_by_key(|&&f| (f as i64 - l as i64).unsigned_abs())
                        .unwrap();
                    layers[l] = layers[f];
                }
            }
        }
        any.then_some(WindProfile { layers })
    }
}

/// The Nyquist velocity is not carried through `nexrad_model::data::Radial`,
/// but it doesn't need to be: folded data always contains values at the fold
/// limit, so when aliasing occurred at all, max |v| *is* the Nyquist velocity.
/// When it didn't, the estimate is low but the field is continuous and no
/// region boundary exists to unfold.
fn estimate_nyquist(vel_grid: &[Vec<f64>]) -> f64 {
    vel_grid
        .iter()
        .flatten()
        .filter(|v| v.is_finite())
        .fold(0.0_f64, |a, &v| a.max(v.abs()))
}

// ————————————————————————————————————————————————————————————————————
// Step 2: range-dependent median filter
// ————————————————————————————————————————————————————————————————————

/// Half-width of the median kernel's azimuthal footprint, in km. The window
/// narrows from 5×5 toward 3×3 with range, so instead of switching at one
/// fixed range the radial count follows a constant physical footprint, capped
/// at 5 radials and floored at 3.
///
/// This is the one azimuthal scale in the module that is a **distance** and
/// not a count of rows, and so the one that divides the other way: the
/// stencils' taps sit at row offsets and their divisors count rows
/// ([`split_stencil_rot`]), while this counts rows *from* an arc, so a
/// coarser sweep gets fewer of them. Measured, at 20 km: five rows spanning
/// 2.5° on a 0.5° sweep, three spanning 3.0° on a 1.0° one — the same half
/// kilometre of sky either way, until [`MEDIAN_AZ_HALF_MAX`] and the floor of
/// one bound it.
const MEDIAN_HALF_WIDTH_KM: f64 = 0.4;

/// Cap on the median filter's azimuthal half-count. Empirical, set against
/// the reference median's couplet erasure and near-radar couplet amplitudes
/// (a 5×5 window counted in legacy 1° radials ≈ 9 super-res). Measured
/// provenance: branch `campaign-harness`.
const MEDIAN_AZ_HALF_MAX: i32 = 2;

/// Half-depth of the median kernel in range gates — deliberately deeper than it
/// is wide. Range is the axis this module does *not* differentiate, so smoothing
/// along it removes noise without touching the azimuthal shear being measured.
/// The depth is empirical: 2 gates agreed with reference readouts better than
/// 1 on amplitude, correlation and painted density. Measured provenance:
/// branch `campaign-harness`.
const MEDIAN_RNG_HALF: i32 = 2;

/// Minimum RAW-data fraction of the median window for a valid centre to
/// survive: the reference NDs under-populated windows, cleaning sparse fold
/// soup the raw-default dealias rule re-admits. The fraction is empirical.
/// Measured provenance: branch `campaign-harness`.
const MEDIAN_MIN_RAW_OCC: f64 = 0.6;

fn median_filter(
    vel_grid: &[Vec<f64>],
    raw_grid: &[Vec<f64>],
    gate_count: usize,
    first_gate_range_km: f64,
    gate_interval_km: f64,
    step_deg: f64,
) -> Vec<Vec<f64>> {
    let num_radials = vel_grid.len() as i32;
    let spacing_rad = step_deg.to_radians();

    (0..num_radials as usize)
        .into_par_iter()
        .map(|i| {
            let mut window: Vec<f64> = Vec::with_capacity(25);
            (0..gate_count)
                .map(|j| {
                    // No NaN fill: a missing centre stays missing.
                    if vel_grid[i][j].is_nan() {
                        return f64::NAN;
                    }
                    let range_km = first_gate_range_km + j as f64 * gate_interval_km;
                    let arc_per_radial = range_km * spacing_rad;
                    let az_half = ((MEDIAN_HALF_WIDTH_KM / arc_per_radial).round() as i32)
                        .clamp(1, MEDIAN_AZ_HALF_MAX);

                    window.clear();
                    let mut slots = 0u32;
                    let mut raw_occ = 0u32;
                    for da in -az_half..=az_half {
                        let ai = ((i as i32 + da).rem_euclid(num_radials)) as usize;
                        for dr in -MEDIAN_RNG_HALF..=MEDIAN_RNG_HALF {
                            let rj = j as i32 + dr;
                            if rj < 0 || rj >= gate_count as i32 {
                                continue;
                            }
                            slots += 1;
                            if !raw_grid[ai][rj as usize].is_nan() {
                                raw_occ += 1;
                            }
                            let v = vel_grid[ai][rj as usize];
                            if !v.is_nan() {
                                window.push(v);
                            }
                        }
                    }
                    // The sparsity cliff tests RAW data occupancy: censored
                    // fold walls carry raw data and must not deplete the
                    // window, only genuinely missing samples do.
                    if (raw_occ as f64) < MEDIAN_MIN_RAW_OCC * slots as f64 {
                        return f64::NAN;
                    }
                    window.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap());
                    let mid = window.len() / 2;
                    if window.len() % 2 == 1 {
                        window[mid]
                    } else {
                        (window[mid - 1] + window[mid]) / 2.0
                    }
                })
                .collect()
        })
        .collect()
}

// ————————————————————————————————————————————————————————————————————
// Steps 3 and 4: azimuthal derivative stencils, range-normalized
// ————————————————————————————————————————————————————————————————————

/// Divisor for the range normalization, range in nautical miles (the unit the
/// callers already carry); converts to the kilometre knot curve.
fn rot_divisor(range_nm: f64) -> f64 {
    rot_divisor_km(range_nm * KM_PER_NM)
}

/// The divisor curve, empirically fitted to the reference's range response:
/// the knot ranges are KILOMETERS and the curve is linearly interpolated
/// between knots (25 at ≤20 km ramping to 8 at 80 km, flat 8 beyond).
/// Measured provenance: branch `campaign-harness`.
fn rot_divisor_km(range_km: f64) -> f64 {
    const KNOTS: [(f64, f64); 4] = [(20.0, 25.0), (40.0, 20.0), (60.0, 12.0), (80.0, 8.0)];
    if range_km <= KNOTS[0].0 {
        return KNOTS[0].1;
    }
    for w in KNOTS.windows(2) {
        let ((r0, d0), (r1, d1)) = (w[0], w[1]);
        if range_km <= r1 {
            return d0 + (d1 - d0) * (range_km - r0) / (r1 - r0);
        }
    }
    KNOTS[KNOTS.len() - 1].1
}

/// Composite azimuthal derivative stencil, empirically measured from the
/// reference's step response (measured provenance: branch `campaign-harness`).
/// Antisymmetric, dimensionless; ROT = Σ cⱼ·v(i+j) / arc. The sign-reversed
/// outer taps are what produce the small negative side lobes flanking every
/// strong gradient — a plain least-squares slope cannot produce those.
const COMPOSITE_TAPS: [f64; 5] = [0.1039, 0.1595, 0.1187, -0.0037, -0.0630];

/// The per-radial split-tap operator: the side toward the whole-degree pair
/// partner applies `SPLIT_CLEAN` (the legacy-grid taps ĉ = [0.580, 0.238,
/// −0.151]) at 2/3/4 super-res offsets; the side away from the partner
/// applies `SPLIT_AWAY` = [ĉ₂, ĉ₁−ĉ₂, ĉ₂, ĉ₃] at 1/2/3/4. Both sides sum to
/// ĉ₁+ĉ₂+ĉ₃, so the operator is zero-sum; normalization is two rows of the
/// grid, which is the legacy 1.0° arc on the super-res grid these were fitted
/// on (see [`split_stencil_rot`], where the difference between those two
/// readings is what makes the operator a derivative rather than a reading of
/// one particular spacing). Empirical: it is the unique zero-sum anchored linear operator
/// solved exactly from the reference's measured per-radial step-response
/// profiles, which no pair-average-then-convolve chain reproduces.
/// Measured provenance: branch `campaign-harness`.
const SPLIT_CLEAN: [(i32, f64); 3] = [(2, 0.580), (3, 0.238), (4, -0.151)];
const SPLIT_AWAY: [(i32, f64); 4] = [(1, 0.238), (2, 0.342), (3, 0.238), (4, -0.151)];

/// Matched-filter kernel bank: one per-radial tap operator per couplet pole
/// width (2/3/4 radials), with the same (offset, tap) clean/away semantics
/// as [`SPLIT_CLEAN`]/[`SPLIT_AWAY`]. Each kernel is empirically fitted so
/// that its response to the ideal median-filtered width-w couplet matches
/// the reference's measured width-w couplet response, anchored to the
/// primary operator's own core response on the same pattern (measured
/// provenance: branch `campaign-harness`). The kernels never see
/// steps or notches: [`apply_kernel_bank`] only engages them — and only as
/// magnitude caps — where the local velocity profile carries a bipolar
/// couplet signature, so the primary chain keeps full ownership of sign,
/// ND, coherence and every non-couplet pattern.
const BANK_K2_CLEAN: [(i32, f64); 6] = [
    (1, 0.1916),
    (2, 0.3660),
    (3, 0.3660),
    (4, 0.1854),
    (5, 0.1854),
    (6, 0.0168),
];
const BANK_K2_AWAY: [(i32, f64); 6] = [
    (1, 0.1706),
    (2, 0.1706),
    (3, 0.2867),
    (4, 0.2867),
    (5, 0.0142),
    (6, 0.0142),
];
const BANK_K3_CLEAN: [(i32, f64); 7] = [
    (1, -0.0812),
    (2, 0.4297),
    (3, 0.0225),
    (4, 0.6003),
    (5, 0.1732),
    (6, 0.0613),
    (7, -0.2276),
];
const BANK_K3_AWAY: [(i32, f64); 7] = [
    (1, 0.2541),
    (2, -0.0761),
    (3, 0.8530),
    (4, 0.2776),
    (5, 0.4269),
    (6, -0.0377),
    (7, 0.1870),
];
const BANK_K4_CLEAN: [(i32, f64); 11] = [
    (1, 0.3727),
    (2, 0.1368),
    (3, 0.1368),
    (4, 0.5549),
    (5, 0.5549),
    (6, 0.4834),
    (7, 0.4834),
    (8, 0.1101),
    (9, 0.1101),
    (10, 0.5916),
    (11, 0.5916),
];
const BANK_K4_AWAY: [(i32, f64); 11] = [
    (1, 0.2494),
    (2, 0.2494),
    (3, 0.1336),
    (4, 0.1336),
    (5, 0.6856),
    (6, 0.6856),
    (7, 0.0728),
    (8, 0.0728),
    (9, 0.2768),
    (10, 0.2768),
    (11, 0.9262),
];

/// Asymmetric-couplet kernels: span-7 operators with the same clean/away
/// semantics, empirically fitted to the reference's measured
/// graded-asymmetry couplet responses — a +6/−4 pole pair (ratio 0.67) and
/// a +6/−2 pair (ratio 0.33). The reference compresses asymmetric-couplet
/// edges harder as the weak pole shrinks, and symmetric templates cannot
/// match these patterns, so they get their own kernels and templates.
/// Footprint-only: their tap energy is too high for the per-bin base cap,
/// and their template gate is the sole notch guard (measured monopolar
/// notches score far under the r² floor against them — the balance gate,
/// which such couplets themselves fail, is deliberately not applied).
/// Measured provenance: branch `campaign-harness`.
const BANK_A067_CLEAN: [(i32, f64); 7] = [
    (1, -0.1595),
    (2, 0.8119),
    (3, -0.29),
    (4, 0.1871),
    (5, 0.3338),
    (6, -0.7268),
    (7, 0.3928),
];
const BANK_A067_AWAY: [(i32, f64); 7] = [
    (1, 0.4798),
    (2, -0.5459),
    (3, 0.7539),
    (4, 0.3544),
    (5, -0.8263),
    (6, 0.3977),
    (7, -0.0731),
];
const BANK_A033_CLEAN: [(i32, f64); 7] = [
    (1, -0.0712),
    (2, 0.6955),
    (3, -0.4007),
    (4, 0.3542),
    (5, 0.2212),
    (6, -0.7511),
    (7, 0.5327),
];
const BANK_A033_AWAY: [(i32, f64); 7] = [
    (1, 0.5561),
    (2, -0.6041),
    (3, 0.6831),
    (4, 0.5249),
    (5, -1.0944),
    (6, 0.541),
    (7, -0.0264),
];

/// Candidate cores for the footprint pass: local azimuthal maxima of the
/// primary chain's |NROT| at or above the palette floor.
const BANK_DETECT_MIN: f64 = 0.25;

/// Template-match floor: squared Pearson correlation between the detrended
/// local velocity profile and the ideal width-w couplet template (best of
/// alignments −1/0/+1) must reach this before a width's kernel takes the
/// couplet's footprint; the best-scoring width wins. High on purpose: the
/// footprint layer repaints only clean template matches, while the per-bin
/// base cap handles everything merely couplet-like.
const BANK_R2_MIN: f64 = 0.8;

/// Compression floor for the footprint cap, as a fraction of the primary
/// chain's magnitude. On-template the kernels' fitted edge/core responses
/// stay above this fraction of the primary response, so any deeper
/// compression is off-template kernel texture, not measured couplet law —
/// bounding it keeps capped bins within the profile family the bank was
/// fitted to.
const BANK_CAP_FLOOR: f64 = 0.7;

/// Gain on the cap operators' output. The kernel fits are anchored to the
/// primary operator's core response on ideal patterns; on real weak couplet
/// shoulders the reference's compressed values run below that anchor, so
/// the cap output is recalibrated by this empirical factor. Measured
/// provenance: branch `campaign-harness`.
const BANK_CAP_GAIN: f64 = 0.90;

/// Deviation-balance floor for the per-bin base cap. Empirical separator:
/// measured monopolar notches balance well below it, rotation couplets at
/// or above it. Measured provenance: branch `campaign-harness`.
const BANK_BASE_BALANCE_MIN: f64 = 0.42;

/// Deviation-balance floor for footprint candidates: opposite deviations
/// about the window median must reach this ratio. Sits between the
/// measured notch and couplet balance points, nearer the notch to keep
/// weak lopsided couplets eligible. Measured provenance: branch
/// `campaign-harness`.
const BANK_BALANCE_MIN: f64 = 0.35;

/// Range limit in km for the split-tap operator; beyond it the composite
/// 11-tap stencil takes over. Each operator is used inside the range band
/// its calibration measurements cover: the split operator near the radar,
/// the composite at long range where pairing phase is invisible. Measured
/// provenance: branch `campaign-harness`.
const SPLIT_MAX_RANGE_KM: f64 = 80.0;

/// Range half-depth in gates for the stencils' 3-gate range means, per
/// Smith/Elmore's "3 range gates deep" — deeper smooths small features in
/// range and reads them low.
const STENCIL_RNG_HALF: i32 = 1;

/// Coherence floor for both stencils: squared correlation between the
/// velocity profile and the stencil's ramp response; constant or incoherent
/// profiles read ND, matching the reference's ND bins over good velocity.
const GK_MIN_R2: f64 = 0.05;

/// Extra valid radials required beyond the split stencil's ±4 span on each
/// side. The composite estimator's all-5-pairs completeness rule doubles as a
/// data-edge noise gate — bins whose support just barely fits the stencil sit
/// on echo boundaries where the profile is half real, half edge — and the
/// margin gives the split stencil the same protection.
const GK_DATA_MARGIN: i32 = 1;

/// The split-tap operator at one bin. `pair_first` says whether radial `i`
/// is the first member of its whole-degree pair (partner at i+1) or the
/// second (partner at i−1). Requires every tap cell; profiles that do not
/// correlate with the stencil read ND like the composite estimator's.
fn split_stencil_rot(
    vel_grid: &[Vec<f64>],
    i: usize,
    j: usize,
    arc_per_radial: f64,
    gate_count: usize,
    pair_first: bool,
) -> Option<f64> {
    let num_radials = vel_grid.len() as i32;
    // Range-averaged velocity at offsets −(4+margin)..=4+margin; prof[6 + o].
    let mut prof = [f64::NAN; 15];
    for (idx, slot) in prof.iter_mut().enumerate() {
        let da = idx as i32 - 7;
        if da.abs() > 7 {
            continue;
        }
        let ai = ((i as i32 + da).rem_euclid(num_radials)) as usize;
        let (mut sum, mut n) = (0.0, 0);
        for dr in -STENCIL_RNG_HALF..=STENCIL_RNG_HALF {
            let rj = j as i32 + dr;
            if rj < 0 || rj >= gate_count as i32 {
                continue;
            }
            let v = vel_grid[ai][rj as usize];
            if !v.is_nan() {
                sum += v;
                n += 1;
            }
        }
        if n > 0 {
            *slot = sum / n as f64;
        }
    }
    // Data-margin completeness: the composite stencil's ±5 span must be
    // populated too, so bins do not appear at echo edges it would reject.
    for m in 0..GK_DATA_MARGIN {
        let o = (5 + m) as usize;
        if prof[7 + o].is_nan() || prof[7 - o].is_nan() {
            return None;
        }
    }
    // Signed weight per profile cell: toward-partner side clean, away side
    // split. The partner sits at +1 for a pair-first radial.
    let mut w = [0.0f64; 15];
    let clean: &[(i32, f64)] = &SPLIT_CLEAN;
    let away: &[(i32, f64)] = &SPLIT_AWAY;
    let (plus, minus) = if pair_first {
        (clean, away)
    } else {
        (away, clean)
    };
    for &(o, t) in plus {
        w[(7 + o) as usize] += t;
    }
    for &(o, t) in minus {
        w[(7 - o) as usize] -= t;
    }
    let (mut acc, mut mean, mut nv) = (0.0, 0.0, 0);
    for k in 0..15 {
        if w[k] == 0.0 {
            continue;
        }
        let v = prof[k];
        if v.is_nan() {
            return None;
        }
        acc += w[k] * v;
        mean += v;
        nv += 1;
    }
    // Coherence gate, same form as the composite estimator's: squared
    // correlation between the profile and the stencil weights.
    mean /= nv as f64;
    let (mut svv, mut scc) = (0.0, 0.0);
    for k in 0..15 {
        if w[k] == 0.0 {
            continue;
        }
        svv += (prof[k] - mean).powi(2);
        scc += w[k] * w[k];
    }
    if svv <= 0.0 {
        return None;
    }
    if acc * acc / (scc * svv) < GK_MIN_R2 {
        return None;
    }
    // Normalize by two radials of *this* grid — the legacy 1.0° arc on the
    // 0.5° grid the taps were fitted on, and the arc of two rows on any
    // other. The 2 counts rows, not degrees, and that is what makes this a
    // derivative rather than a reading of one particular grid: the taps sit
    // at row offsets, so on a coarser grid the numerator spans proportionally
    // more sky and the divisor grows by the same factor. The quotient is the
    // shear either way — one field sampled at 0.5° and 1.0° reads the same
    // number to 3.1e-14, in both stencil bands, measured in
    // `one_shear_reads_the_same_at_either_radial_spacing`. Pinning the
    // divisor to a physical degree instead would make a 1.0°-spaced sweep
    // report exactly twice the shear its own velocities carry.
    Some(acc / (2.0 * arc_per_radial))
}

/// Which index phase pairs super-res radials into whole-degree legacy bins:
/// radials (2k+phase, 2k+1+phase) share a degree sector. The legacy pairing
/// is anchored to ABSOLUTE azimuth — a measured fact: steps at whole-degree
/// boundaries read clean, at half-degree boundaries pair-averaged. Measured
/// provenance: branch `campaign-harness`.
///
/// # A grid that is already legacy resolution has no phase to find
///
/// The question this asks only has an answer on a 0.5° grid. Two radials
/// 1.0° apart can never share a whole degree — their floors differ by one by
/// construction — so on a 1.0°-spaced sweep both counts come back zero and
/// the tie falls to phase 0, for whole-degree azimuths, for a sweep offset by
/// 0.37° or 0.5°, and for one jittered ±0.06° (all four measured, in
/// `a_one_degree_sweep_has_no_pair_phase_to_measure`). That is not a bad
/// reading of a real pairing; there is no pairing. Each radial of such a
/// sweep *is* a legacy bin.
///
/// The consequence is that [`split_stencil_rot`]'s clean/away asymmetry gets
/// assigned off `i % 2` — off collection index rather than off azimuth — so
/// the same sky, collected starting one radial later, is differentiated by
/// the other form of the operator. On the 0.5° grid the anchoring holds and
/// this cannot happen: rolling a sweep's collection order leaves all 714
/// compared bins bit-identical. On a 1.0° grid the same roll moved 7 bins of
/// 353 by more than the 0.04 the reference quantizes in, flipped 3 between a
/// value and ND, and read −0.198 where the unrolled sweep read −0.086 — both
/// measured in `a_super_res_sweep_reads_the_same_wherever_collection_began`.
///
/// This is **not** settled here, because settling it means choosing a value
/// nothing has measured. Averaging the two forms, falling back to
/// [`COMPOSITE_TAPS`] inside 80 km — whose ramp gain is 0.898 against the
/// split operator's 1.151, a 22% move on every bin of every such cut, in a
/// band it was never fitted in — and applying the legacy taps ĉ directly (a
/// 1.0° grid has no row at their 1.5-row offset) all give different fields.
/// What would settle it is a reference NROT field over a 1.0°-spaced sweep,
/// read per-radial across a couplet: whether the reference's response
/// alternates there at all says whether the operator must be symmetrized, and
/// its step and couplet profiles on such a grid set the gain the same way the
/// super-res profiles set [`SPLIT_CLEAN`]/[`SPLIT_AWAY`].
///
/// TDWR is where this became load-bearing — every one of its cuts is 1.0° —
/// but it is not where it started: a WSR-88D volume's tilts above the
/// super-res cuts are 1.0° too, and NROT has been derived for all of them.
fn pair_phase(azimuths_deg: &[f64]) -> usize {
    let n = azimuths_deg.len();
    if n < 4 {
        return 0;
    }
    let cohabit = |phase: usize| {
        (0..n / 2)
            .filter(|&k| {
                let (a, b) = (
                    azimuths_deg[(2 * k + phase) % n],
                    azimuths_deg[(2 * k + 1 + phase) % n],
                );
                a.floor() == b.floor()
            })
            .count()
    };
    if cohabit(1) > cohabit(0) { 1 } else { 0 }
}

/// The widest azimuthal half-span any profile reader asks for: the width-4
/// bank kernel's tap list ([`BANK_K4_CLEAN`]) reaches ±11 radials, which is
/// more than the base cap's ±7 and more than `gated_prof`'s ±(w+3) ≤ ±7.
const PROFILE_MAX_HALF: usize = 11;

/// Backing store for one [`az_profile`], sized for [`PROFILE_MAX_HALF`].
///
/// A `Vec` here was ~1.15 M heap allocations per sweep: [`apply_kernel_bank`]
/// takes a profile once per non-`NaN` bin and three more through
/// [`bank_kernel_rot`]'s kernel sweep, over ~230 k bins, for a fifteen-element
/// array with a fixed maximum length. [`composite_stencil_rot`] and
/// [`split_stencil_rot`] already had the right shape — a plain stack array —
/// and this gives the bank path the same one. Nothing about the values
/// changes; the profile is filled and read exactly as before.
type ProfileBuf = [f64; 2 * PROFILE_MAX_HALF + 1];

/// An empty [`ProfileBuf`], for a caller about to hand it to [`az_profile`].
const EMPTY_PROFILE: ProfileBuf = [f64::NAN; 2 * PROFILE_MAX_HALF + 1];

/// Range-averaged azimuthal velocity profile around (i, j): the 3-gate range
/// mean per radial offset −half..=half — the same per-radial samples the tap
/// stencils consume. NaN where a radial has no data in the range window.
///
/// Fills the leading `2·half + 1` entries of `out` and returns them; `half`
/// above [`PROFILE_MAX_HALF`] would be a caller with a wider tap list than any
/// kernel in the bank has, and panics rather than silently truncating.
fn az_profile<'p>(
    out: &'p mut ProfileBuf,
    vel_grid: &[Vec<f64>],
    i: usize,
    j: usize,
    gate_count: usize,
    half: i32,
) -> &'p [f64] {
    let num_radials = vel_grid.len() as i32;
    let len = 2 * half as usize + 1;
    let slot = &mut out[..len];
    for (idx, cell) in slot.iter_mut().enumerate() {
        let da = idx as i32 - half;
        let ai = ((i as i32 + da).rem_euclid(num_radials)) as usize;
        let (mut sum, mut n) = (0.0, 0);
        for dr in -STENCIL_RNG_HALF..=STENCIL_RNG_HALF {
            let rj = j as i32 + dr;
            if rj < 0 || rj >= gate_count as i32 {
                continue;
            }
            let v = vel_grid[ai][rj as usize];
            if !v.is_nan() {
                sum += v;
                n += 1;
            }
        }
        *cell = if n > 0 { sum / n as f64 } else { f64::NAN };
    }
    slot
}

/// Best squared Pearson correlation between a fully-valid profile (centred
/// on a candidate core radial) and the ideal width-`w` couplet template —
/// +1 across w radials meeting −`neg_amp` across w radials at the window
/// centre — over alignments −1/0/+1. r² is invariant under template
/// negation, so one template serves both rotation senses and both
/// orientations of an asymmetric pair (the sign-mirrored pattern scores
/// identically by construction, and the kernels are linear). Steps and ramps never pass: their profiles do not
/// return to the background on both sides of the window.
fn bank_template_r2(prof: &[f64], w: i32, neg_amp: f64) -> Option<f64> {
    let half = (prof.len() as i32 - 1) / 2;
    let n = prof.len() as f64;
    // Detrend: remove the profile's mean and linear component. The second
    // chain is flow-invariant (measured), so ambient azimuthal shear under a
    // couplet must not spoil the template match; a monopolar notch stays
    // monopolar after detrending and still matches nothing.
    let pm = prof.iter().sum::<f64>() / n;
    let sxx: f64 = (0..prof.len())
        .map(|k| (k as f64 - (n - 1.0) / 2.0).powi(2))
        .sum();
    let sxy: f64 = prof
        .iter()
        .enumerate()
        .map(|(k, p)| (k as f64 - (n - 1.0) / 2.0) * (p - pm))
        .sum();
    let slope = sxy / sxx;
    // Stack, not heap: this runs per candidate core per width, four `Vec`s a
    // call (the detrended profile and one template per alignment), and the
    // length is bounded by `PROFILE_MAX_HALF` like every other profile here.
    let mut detrended = EMPTY_PROFILE;
    let detrended = &mut detrended[..prof.len()];
    for (k, slot) in detrended.iter_mut().enumerate() {
        *slot = prof[k] - pm - slope * (k as f64 - (n - 1.0) / 2.0);
    }
    let pm = 0.0;
    let pv: f64 = detrended.iter().map(|p| (p - pm).powi(2)).sum();
    if pv <= 0.0 {
        return None;
    }
    let mut best: Option<f64> = None;
    let mut template = EMPTY_PROFILE;
    let t = &mut template[..prof.len()];
    for s in -1..=1 {
        for (k, slot) in t.iter_mut().enumerate() {
            let x = k as i32 - half - s;
            *slot = if (-w..0).contains(&x) {
                1.0
            } else if (0..w).contains(&x) {
                -neg_amp
            } else {
                0.0
            };
        }
        let tm = t.iter().sum::<f64>() / n;
        let tv: f64 = t.iter().map(|x| (x - tm).powi(2)).sum();
        let cov: f64 = detrended
            .iter()
            .zip(t.iter())
            .map(|(p, x)| (p - pm) * (x - tm))
            .sum();
        let r2 = cov * cov / (pv * tv);
        if best.is_none_or(|b| r2 > b) {
            best = Some(r2);
        }
    }
    best
}

/// A kernel's clean-side and away-side tap lists.
type TapPair<'a> = (&'a [(i32, f64)], &'a [(i32, f64)]);

/// A candidate window for the footprint pass that passes the ends-return gate
/// (a couplet's profile comes back to the background on both sides; a step's
/// does not), with the profile's bipolar balance about its median. `None` when
/// the window is incomplete or the gate rejects it.
///
/// Takes its own backing array rather than returning a `Vec`, and is a free
/// function rather than the closure it was so the borrow ends with the caller's
/// use of the slice.
fn gated_prof<'p>(
    out: &'p mut ProfileBuf,
    vel_grid: &[Vec<f64>],
    i: usize,
    j: usize,
    gate_count: usize,
    w: i32,
) -> Option<(&'p [f64], f64)> {
    let prof = az_profile(out, vel_grid, i, j, gate_count, w + 3);
    if prof.iter().any(|p| p.is_nan()) {
        return None;
    }
    let (lo, hi) = prof
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(l, h), &p| {
            (l.min(p), h.max(p))
        });
    if hi <= lo || (prof[prof.len() - 1] - prof[0]).abs() > 0.5 * (hi - lo) {
        return None;
    }
    let mut vals = EMPTY_PROFILE;
    let vals = &mut vals[..prof.len()];
    vals.copy_from_slice(prof);
    vals.sort_by(|x, y| x.partial_cmp(y).unwrap());
    let med = vals[vals.len() / 2];
    let dpos = vals[vals.len() - 1] - med;
    let dneg = med - vals[0];
    let balance = dpos.min(dneg) / dpos.max(dneg).max(1e-9);
    Some((prof, balance))
}

/// One bank kernel at one bin: the same clean/away weight assembly as
/// [`split_stencil_rot`], normalized by the same two rows of this grid — the
/// legacy 1.0° arc where the grid is super-res. It has to be the same
/// divisor and the same [`pair_phase`] as the primary chain, since its output
/// is only ever a cap on the primary's magnitude and two chains scaled over
/// different arcs would cap by a ratio of arcs rather than by kernel shape.
/// Requires every tap cell — a missing cell means the footprint bin keeps the
/// primary chain's value.
fn bank_kernel_rot(
    vel_grid: &[Vec<f64>],
    i: usize,
    j: usize,
    arc_per_radial: f64,
    gate_count: usize,
    pair_first: bool,
    taps: TapPair,
) -> Option<f64> {
    let (clean, away) = taps;
    let span = clean.len() as i32;
    let mut buf = EMPTY_PROFILE;
    let prof = az_profile(&mut buf, vel_grid, i, j, gate_count, span);
    let (plus, minus) = if pair_first {
        (clean, away)
    } else {
        (away, clean)
    };
    let mut acc = 0.0;
    for &(o, t) in plus {
        let v = prof[(span + o) as usize];
        if v.is_nan() {
            return None;
        }
        acc += t * v;
    }
    for &(o, t) in minus {
        let v = prof[(span - o) as usize];
        if v.is_nan() {
            return None;
        }
        acc -= t * v;
    }
    Some(acc / (2.0 * arc_per_radial))
}

/// Matched-filter bank pass over the primary NROT grid, inside the
/// split-tap domain. Two layers, both magnitude caps that keep the
/// primary's sign and leave everything their gates exclude untouched:
///
/// 1. Per-bin base cap: every bin whose velocity profile returns to its
///    background at both window ends (steps and pure azimuthal ramps never
///    do) and shows balanced opposite deviations about its median
///    (monopolar notches never do) is bounded by the smallest kernel
///    magnitude in the bank. This suppresses the broad sign-fragile
///    fringes the primary chain paints around weak structure.
/// 2. Footprint refinement: at each candidate core (a local azimuthal max
///    of |NROT| at or above [`BANK_DETECT_MIN`]) whose profile passes the
///    same gates and correlates with a couplet template at
///    [`BANK_R2_MIN`], the best-matching kernel — symmetric widths 2/3/4,
///    or an asymmetric width-3 pair (weak-pole ratio 0.67/0.33, exempt
///    from the balance gate their pattern class inherently fails) — caps
///    the couplet's whole footprint (core ± (w+2) radials), floored at
///    [`BANK_CAP_FLOOR`] of the primary value.
///
/// The reference's measured width-dependent edge compression with
/// full-value pass-through on monopolar notches follows from this
/// selection rule, and the cap form bounds the wide kernels' noise gain by
/// the primary response on real velocity texture. Measured provenance:
/// branch `campaign-harness`.
fn apply_kernel_bank(
    sweep: &VelocitySweep,
    vel_grid: &[Vec<f64>],
    grid: &mut [Vec<f64>],
    phase: usize,
) {
    let num_radials = grid.len();
    if num_radials == 0 {
        return;
    }
    // The same step the primary chain used: the bank's output is a cap on the
    // primary's magnitude, and two chains measured over different arcs would
    // cap by a ratio of arcs rather than by kernel shape.
    let spacing_rad = radial_step_deg(sweep.azimuths_deg, num_radials).to_radians();
    type Bank = [(i32, &'static [(i32, f64)], &'static [(i32, f64)]); 3];
    const BANK: Bank = [
        (2, &BANK_K2_CLEAN, &BANK_K2_AWAY),
        (3, &BANK_K3_CLEAN, &BANK_K3_AWAY),
        (4, &BANK_K4_CLEAN, &BANK_K4_AWAY),
    ];
    // Asymmetric entries: (weak-pole template amplitude, taps). Width-3
    // poles; footprint-only — see the tap constants' doc.
    type BankAsym = [(f64, &'static [(i32, f64)], &'static [(i32, f64)]); 2];
    const BANK_ASYM: BankAsym = [
        (2.0 / 3.0, &BANK_A067_CLEAN, &BANK_A067_AWAY),
        (1.0 / 3.0, &BANK_A033_CLEAN, &BANK_A033_AWAY),
    ];
    let primary: &[Vec<f64>] = grid;
    let overrides: Vec<Vec<(usize, f64)>> = (0..sweep.gate_count)
        .into_par_iter()
        .map(|j| {
            let mut ov: Vec<(usize, f64)> = Vec::new();
            let range_km = sweep.first_gate_range_km + j as f64 * sweep.gate_interval_km;
            if range_km >= SPLIT_MAX_RANGE_KM || range_km <= MIN_RANGE_NM * KM_PER_NM {
                return ov;
            }
            let arc_per_radial = range_km * spacing_rad;
            let divisor = rot_divisor(range_km / KM_PER_NM);
            let mut col: Vec<f64> = (0..num_radials).map(|i| primary[i][j]).collect();
            // Base layer: per-bin bank cap. Every bin
            // whose profile returns to its background at the window ends
            // (steps and pure azimuthal ramps do not — they keep the full
            // primary value) and shows balanced opposite deviations about
            // its median (monopolar notches do not) is bounded by the
            // smallest kernel magnitude in the bank. This is what
            // suppresses the broad sign-fragile fringes around weak
            // structure; the width-matched footprints below then refine
            // actual couplets.
            for (i, ci) in col.iter_mut().enumerate() {
                if ci.is_nan() {
                    continue;
                }
                let mut buf = EMPTY_PROFILE;
                let prof = az_profile(&mut buf, vel_grid, i, j, sweep.gate_count, 7);
                let mut vals = EMPTY_PROFILE;
                let mut nvals = 0;
                for &p in prof {
                    if !p.is_nan() {
                        vals[nvals] = p;
                        nvals += 1;
                    }
                }
                if nvals == 0 {
                    continue;
                }
                let vals = &mut vals[..nvals];
                let (first, last) = (vals[0], vals[vals.len() - 1]);
                vals.sort_by(|x, y| x.partial_cmp(y).unwrap());
                let (lo, hi) = (vals[0], vals[vals.len() - 1]);
                if hi <= lo || (last - first).abs() > 0.7 * (hi - lo) {
                    continue;
                }
                let med = vals[vals.len() / 2];
                let dpos = hi - med;
                let dneg = med - lo;
                let balance = dpos.min(dneg) / dpos.max(dneg).max(1e-9);
                if balance < BANK_BASE_BALANCE_MIN {
                    continue;
                }
                let mut cap: Option<f64> = None;
                for &(_, kc, ka) in BANK.iter() {
                    if let Some(rot) = bank_kernel_rot(
                        vel_grid,
                        i,
                        j,
                        arc_per_radial,
                        sweep.gate_count,
                        i % 2 == phase,
                        (kc, ka),
                    ) {
                        let kv = (BANK_CAP_GAIN * rot / divisor)
                            .clamp(-NROT_LIMIT, NROT_LIMIT)
                            .abs();
                        if cap.is_none_or(|c| kv < c) {
                            cap = Some(kv);
                        }
                    }
                }
                if let Some(kv) = cap
                    && kv < ci.abs()
                {
                    let capped = ci.signum() * kv;
                    *ci = capped;
                    ov.push((i, capped));
                }
            }
            for i in 0..num_radials {
                let v = col[i];
                if v.is_nan() || v.abs() < BANK_DETECT_MIN {
                    continue;
                }
                let prev = col[(i + num_radials - 1) % num_radials];
                let next = col[(i + 1) % num_radials];
                if (!prev.is_nan() && prev.abs() > v.abs())
                    || (!next.is_nan() && next.abs() > v.abs())
                {
                    continue;
                }
                let mut best: Option<(f64, i32, TapPair)> = None;
                let mut buf = EMPTY_PROFILE;
                for &(w, clean, away) in BANK.iter() {
                    // Bipolar balance about the window median: a monopolar
                    // notch (one deep pole, weak counter-deviation) never
                    // balances; a symmetric rotation couplet does.
                    let Some((prof, balance)) =
                        gated_prof(&mut buf, vel_grid, i, j, sweep.gate_count, w)
                    else {
                        continue;
                    };
                    if balance < BANK_BALANCE_MIN {
                        continue;
                    }
                    if let Some(r2) = bank_template_r2(prof, w, 1.0)
                        && r2 >= BANK_R2_MIN
                        && best.is_none_or(|(b, _, _)| r2 > b)
                    {
                        best = Some((r2, w, (clean, away)));
                    }
                }
                // Both asymmetric entries are width 3, so their window is one
                // window: gated as one, and the same three radial means the
                // symmetric width-3 pass already walked. It used to be
                // recomputed three times per candidate core.
                let mut asym_buf = EMPTY_PROFILE;
                if let Some((prof, _)) =
                    gated_prof(&mut asym_buf, vel_grid, i, j, sweep.gate_count, 3)
                {
                    for &(neg_amp, clean, away) in BANK_ASYM.iter() {
                        // Asymmetric couplets fail the balance test by nature;
                        // their template gate alone carries the notch guard
                        // (the measured notch scores r² ≈ 0.2–0.3 here).
                        if let Some(r2) = bank_template_r2(prof, 3, neg_amp)
                            && r2 >= BANK_R2_MIN
                            && best.is_none_or(|(b, _, _)| r2 > b)
                        {
                            best = Some((r2, 3, (clean, away)));
                        }
                    }
                }
                let Some((_, w, (clean, away))) = best else {
                    continue;
                };
                // The kernel must be computable at the core itself — a core
                // whose own span is incomplete sits on a data edge, where
                // the primary chain's completeness rules are the authority.
                if bank_kernel_rot(
                    vel_grid,
                    i,
                    j,
                    arc_per_radial,
                    sweep.gate_count,
                    i % 2 == phase,
                    (clean, away),
                )
                .is_none()
                {
                    continue;
                }
                for d in -(w + 2)..=(w + 2) {
                    let ii = ((i as i32 + d).rem_euclid(num_radials as i32)) as usize;
                    if col[ii].is_nan() {
                        continue;
                    }
                    if let Some(rot) = bank_kernel_rot(
                        vel_grid,
                        ii,
                        j,
                        arc_per_radial,
                        sweep.gate_count,
                        ii % 2 == phase,
                        (clean, away),
                    ) {
                        let kv = (rot / divisor).clamp(-NROT_LIMIT, NROT_LIMIT);
                        let mag = kv.abs().max(BANK_CAP_FLOOR * col[ii].abs());
                        if mag < col[ii].abs() {
                            ov.push((ii, col[ii].signum() * mag));
                        }
                    }
                }
            }
            ov
        })
        .collect();
    for (j, ov) in overrides.into_iter().enumerate() {
        for (i, val) in ov {
            grid[i][j] = val;
        }
    }
}

/// The measured composite estimator: tap correlation across 11 radials at
/// the centre gate, averaged over one gate each side in range. All five tap
/// pairs must be intact (fewer pairs → larger rescale → amplified noise at
/// data edges, so completeness is a noise gate as much as a validity one)
/// and the profile must correlate with the stencil; constant or incoherent
/// profiles read ND.
fn composite_stencil_rot(
    vel_grid: &[Vec<f64>],
    i: usize,
    j: usize,
    arc_per_radial: f64,
    gate_count: usize,
) -> Option<f64> {
    let num_radials = vel_grid.len() as i32;
    // Range-averaged velocity per azimuthal offset; prof[5 ± o] holds ±o.
    let mut prof = [f64::NAN; 11];
    for (idx, slot) in prof.iter_mut().enumerate() {
        let da = idx as i32 - 5;
        let ai = ((i as i32 + da).rem_euclid(num_radials)) as usize;
        let (mut sum, mut n) = (0.0, 0);
        for dr in -STENCIL_RNG_HALF..=STENCIL_RNG_HALF {
            let rj = j as i32 + dr;
            if rj < 0 || rj >= gate_count as i32 {
                continue;
            }
            let v = vel_grid[ai][rj as usize];
            if !v.is_nan() {
                sum += v;
                n += 1;
            }
        }
        if n > 0 {
            *slot = sum / n as f64;
        }
    }
    // All five pairs required: a tap pair with a missing member is a data
    // edge, and the reference reads ND there.
    let (mut acc, mut mean) = (0.0, 0.0);
    for (k, &tap) in COMPOSITE_TAPS.iter().enumerate() {
        let o = k + 1;
        let (p, m) = (prof[5 + o], prof[5 - o]);
        if p.is_nan() || m.is_nan() {
            return None;
        }
        acc += tap * (p - m);
        mean += p + m;
    }
    // Coherence gate: squared correlation between the velocity profile and
    // the stencil (both centred). Constant profiles have zero variance — ND.
    mean /= 10.0;
    let (mut svv, mut scc) = (0.0, 0.0);
    for (k, &tap) in COMPOSITE_TAPS.iter().enumerate() {
        let o = k + 1;
        let (p, m) = (prof[5 + o], prof[5 - o]);
        svv += (p - mean).powi(2) + (m - mean).powi(2);
        scc += 2.0 * tap * tap;
    }
    if svv <= 0.0 {
        return None;
    }
    if acc * acc / (scc * svv) < GK_MIN_R2 {
        return None;
    }
    // The normalization is Σc·v/arc with the full stencil (verified against
    // the reference's step response; branch `campaign-harness`). One arc here
    // against [`split_stencil_rot`]'s two, and both count rows of this grid:
    // these taps are anchored on the grid's own radials, the split operator's
    // on the legacy pairs of a super-res one. Neither number is a degree, so
    // both estimators read one shear at either spacing.
    Some(acc / arc_per_radial)
}

fn llsd_nrot(sweep: &VelocitySweep, vel_grid: &[Vec<f64>]) -> Vec<Vec<f64>> {
    let num_radials = vel_grid.len();
    let spacing_rad = radial_step_deg(sweep.azimuths_deg, num_radials).to_radians();
    let phase = pair_phase(sweep.azimuths_deg);

    let mut grid: Vec<Vec<f64>> = (0..num_radials)
        .into_par_iter()
        .map(|i| {
            (0..sweep.gate_count)
                .map(|j| {
                    if vel_grid[i][j].is_nan() {
                        return f64::NAN;
                    }
                    let range_km = sweep.first_gate_range_km + j as f64 * sweep.gate_interval_km;
                    if range_km <= MIN_RANGE_NM * KM_PER_NM {
                        return f64::NAN;
                    }

                    let arc_per_radial = range_km * spacing_rad;
                    let rot = if range_km < SPLIT_MAX_RANGE_KM {
                        split_stencil_rot(
                            vel_grid,
                            i,
                            j,
                            arc_per_radial,
                            sweep.gate_count,
                            i % 2 == phase,
                        )
                    } else {
                        composite_stencil_rot(vel_grid, i, j, arc_per_radial, sweep.gate_count)
                    };
                    match rot {
                        Some(rot) => {
                            let divisor = rot_divisor(range_km / KM_PER_NM);
                            (rot / divisor).clamp(-NROT_LIMIT, NROT_LIMIT)
                        }
                        None => f64::NAN,
                    }
                })
                .collect()
        })
        .collect();
    apply_kernel_bank(sweep, vel_grid, &mut grid, phase);
    grid
}

// ————————————————————————————————————————————————————————————————————
// Step 1: dealiaser — a validity-marking multi-pass. Gates start invalid;
// environmental-wind and zero-isodop seeds mark the first valid gates;
// bridge and flood-fill passes propagate validity; unreached data keeps raw
// in bulk (measured), the rest is converted to ND, and residual fold walls
// are censored.
// ————————————————————————————————————————————————————————————————————

/// Environmental-wind seed tolerance in m/s — deliberately tight; empirical,
/// tuned against the reference's kept fraction on folded volumes. Measured
/// provenance: branch `campaign-harness`.
const DA_SEED_TOL: f64 = 5.0;

/// Agreeing 4-neighbors required for a gate-level wind seed. A wind-matching
/// pocket inside storm-perturbed flow can never seed a 5×10 all-gates tile;
/// gate seeds anchor it at raw before any bridge can unfold it to the wrong
/// branch. Measured provenance: branch `campaign-harness`.
const DA_SEEDGATE_NEIGHBORS: i32 = 3;

/// Scale on every bridge/fill threshold — the pass ordering is fixed but the
/// base thresholds are nominal; the scale is empirical, set where dealias
/// coverage matches the reference. Measured provenance: branch
/// `campaign-harness`.
const DA_THRESH_SCALE: f64 = 1.4;

/// Iteration cap for the pass loop; propagation converges within ten on
/// every volume measured.
const DA_PASSES: i32 = 10;

/// Raw-continuity flood-fill threshold as a Vny fraction. The aliased flood
/// runs at a much lower threshold than the raw flood — raw acceptance does
/// no interval testing, so a high value cannot cause wrong-branch unfolds;
/// it only admits raw-continuous texture the bridges' agreement rules
/// refuse.
const DA_FLOOD_RAW_FRAC: f64 = 0.4;

/// Aliased flood-fill threshold as a Vny fraction.
const DA_FLOOD_ALIASED_FRAC: f64 = 0.25;

/// Gap length (gates) above which the skip-ND radial bridge waives the
/// continuity test on re-entry. This pass exists to connect distant regions
/// — comparing a gate's velocity to one 50 km back is meaningless; across
/// long gaps the two-direction identity requirement is the only sane check.
const DA_GAPJUMP_GATES: i32 = 10;

/// Zero-isodop seed tightness in m/s.
const DA_ZISO_TOL: f64 = 1.5;

/// Minimum connected-component size (bins, 4-adjacency) for a never-reached
/// data region to be kept at raw, and for a gate-seed cluster to count.
/// Empirical: the reference's raw-default keeps only regions above a
/// measured size gate this value sits inside. Measured provenance: branch
/// `campaign-harness`.
const DA_RAWMIN_BINS: usize = 16;

/// Censor threshold in units of Vny. Empirical fold-wall transfer point:
/// it sits between the largest adjacent jump the reference keeps as
/// rotation and the smallest fold soup it censors — lower values censored
/// real couplet cores. Measured provenance: branch `campaign-harness`.
const CENSOR_VNY_FRAC: f64 = 1.24;

/// The censoring posture [`dealias`] takes once the unfolding passes are done.
///
/// The passes themselves — seeds, bridges, flood fills, head-and-shoulders —
/// are identical under every profile; what differs is only what happens to
/// data the passes could not settle. NROT differentiates the field, so a
/// residual fold wall becomes clamp-level fake shear and is worth censoring
/// aggressively. A velocity *display* consumer (storm-relative velocity) shows
/// the field itself, where a censored gate is a hole in a couplet and the
/// harm runs the other way — so it keeps everything the passes did not prove
/// wrong.
///
/// The profile reaches ONLY the two post-pass censoring/ND knobs below.
/// [`DealiasProfile::NoFalseShear`] resolves to the tuned NROT constants
/// unchanged, so NROT's output is bit-identical to what it was before the
/// parameter existed — its calibration suite is what pins that.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DealiasProfile {
    /// NROT's tuned posture: unreached-data regions under [`DA_RAWMIN_BINS`]
    /// bins go ND, and any bin more than [`CENSOR_VNY_FRAC`]·Vny from a
    /// 4-neighbour is censored as a residual fold wall.
    NoFalseShear,
    /// Maximum retained coverage for velocity display consumers: every
    /// unreached data gate keeps its raw value regardless of region size
    /// ([`COVERAGE_RAWMIN_BINS`]), and the fold-wall censor runs at the same
    /// measured [`CENSOR_VNY_FRAC`] threshold — dropping the censor entirely
    /// was measured worse against the RPG's own dealiased velocity (a kept
    /// fold wall is a 2·Vny error on every gate it touches, which costs
    /// more level agreement than the censored hole costs coverage; the A/B
    /// record lives on branch `campaign-harness`).
    Coverage,
}

/// [`DealiasProfile::Coverage`]'s kept-raw region floor: keep every unreached
/// data gate, however small the region. The RPG's dealiaser resolves all
/// present data, so for a field that is *displayed* rather than
/// differentiated, matching its coverage matters more than suppressing
/// isolated pockets — the A/B against live N0G/N1G twins lives on branch
/// `campaign-harness`.
const COVERAGE_RAWMIN_BINS: usize = 1;

/// The two post-pass knobs a [`DealiasProfile`] resolves to. `pub(crate)` so
/// the srv harness can measure candidate postures without shipping a variant
/// per experiment.
#[derive(Debug, Clone, Copy)]
pub(crate) struct DealiasKnobs {
    /// Minimum connected-component size (bins, 4-adjacency) for a
    /// never-reached data region to keep raw rather than go ND.
    pub rawmin_bins: usize,
    /// Post-dealias fold-wall censor threshold, in units of Vny;
    /// `f64::INFINITY` disables the censor.
    pub censor_vny_frac: f64,
}

impl DealiasProfile {
    pub(crate) fn knobs(self) -> DealiasKnobs {
        match self {
            DealiasProfile::NoFalseShear => DealiasKnobs {
                rawmin_bins: DA_RAWMIN_BINS,
                censor_vny_frac: CENSOR_VNY_FRAC,
            },
            DealiasProfile::Coverage => DealiasKnobs {
                rawmin_bins: COVERAGE_RAWMIN_BINS,
                censor_vny_frac: CENSOR_VNY_FRAC,
            },
        }
    }
}

pub(crate) fn dealias(
    vel_grid: &mut [Vec<f64>],
    sweep: &VelocitySweep,
    elevation_deg: f64,
    profile: Option<&WindProfile>,
    dealias_profile: DealiasProfile,
) {
    dealias_with_knobs(
        vel_grid,
        sweep,
        elevation_deg,
        profile,
        dealias_profile.knobs(),
    )
}

pub(crate) fn dealias_with_knobs(
    vel_grid: &mut [Vec<f64>],
    sweep: &VelocitySweep,
    elevation_deg: f64,
    profile: Option<&WindProfile>,
    knobs: DealiasKnobs,
) {
    let nyquist = estimate_nyquist(vel_grid);
    if nyquist < 8.0 {
        return;
    }
    let interval = 2.0 * nyquist;
    let n = vel_grid.len();
    let gc = sweep.gate_count;
    if n < 8 {
        return;
    }
    let raw: Vec<Vec<f64>> = vel_grid.to_vec();
    // value[i][j] holds the dealiased velocity once valid[i][j].
    let mut valid = vec![false; n * gc];
    let mut value = vec![f64::NAN; n * gc];
    let idx = |i: usize, j: usize| i * gc + j;
    let has = |i: usize, j: usize| !raw[i][j].is_nan();

    // Seed 1: environmental winds. 5-radial × 10-gate tiles where every
    // data gate sits within the tight threshold of the wind component are
    // valid at their raw values (raw acceptance — no interval testing).
    if let Some(wp) = profile {
        for ti in (0..n).step_by(5) {
            for tj in (0..gc).step_by(10) {
                let mut ok = true;
                let mut any = false;
                'tile: for (i, row) in raw.iter().enumerate().take((ti + 5).min(n)).skip(ti) {
                    let az = 2.0 * PI * i as f64 / n as f64;
                    for (j, &v) in row.iter().enumerate().take((tj + 10).min(gc)).skip(tj) {
                        if v.is_nan() {
                            continue;
                        }
                        any = true;
                        let r = sweep.first_gate_range_km + j as f64 * sweep.gate_interval_km;
                        match wp.predict(az, r, elevation_deg) {
                            Some(pred) if (v - pred).abs() < DA_SEED_TOL => {}
                            _ => {
                                ok = false;
                                break 'tile;
                            }
                        }
                    }
                }
                if ok && any {
                    for i in ti..(ti + 5).min(n) {
                        for j in tj..(tj + 10).min(gc) {
                            if has(i, j) {
                                valid[idx(i, j)] = true;
                                value[idx(i, j)] = raw[i][j];
                            }
                        }
                    }
                }
            }
        }
    }
    // Seed 1b: gate-level wind seeds — a gate whose raw value matches the
    // wind component, with at least DA_SEEDGATE_NEIGHBORS of its 4 neighbors
    // also matching, is valid at raw.
    if let Some(wp) = profile {
        let close = |i: usize, j: usize| -> Option<bool> {
            if !has(i, j) {
                return None;
            }
            let az = 2.0 * PI * i as f64 / n as f64;
            let r = sweep.first_gate_range_km + j as f64 * sweep.gate_interval_km;
            wp.predict(az, r, elevation_deg)
                .map(|pred| (raw[i][j] - pred).abs() < DA_SEED_TOL)
        };
        let mut cand = vec![false; n * gc];
        for i in 0..n {
            for j in 0..gc {
                if valid[idx(i, j)] || close(i, j) != Some(true) {
                    continue;
                }
                let mut agree = 0;
                for (ni, nj) in [
                    ((i + n - 1) % n, j),
                    ((i + 1) % n, j),
                    (i, j.wrapping_sub(1)),
                    (i, j + 1),
                ] {
                    if nj < gc && close(ni, nj) == Some(true) {
                        agree += 1;
                    }
                }
                if agree >= DA_SEEDGATE_NEIGHBORS {
                    cand[idx(i, j)] = true;
                }
            }
        }
        // The reference has no gate-level seeding — this pass approximates
        // the tile seeds at finer granularity, so hold it to the same
        // measured region-size gate as kept-raw data: candidate components
        // smaller than DA_RAWMIN_BINS are not seeds.
        let mut seen = vec![false; n * gc];
        for si in 0..n {
            for sj in 0..gc {
                let s0 = idx(si, sj);
                if !cand[s0] || seen[s0] {
                    continue;
                }
                let mut comp = vec![(si, sj)];
                seen[s0] = true;
                let mut q = vec![(si, sj)];
                while let Some((ci, cj)) = q.pop() {
                    let neigh = [
                        ((ci + n - 1) % n, cj),
                        ((ci + 1) % n, cj),
                        (ci, cj.wrapping_sub(1)),
                        (ci, cj + 1),
                    ];
                    for (ni, nj) in neigh {
                        if nj >= gc {
                            continue;
                        }
                        let ix = idx(ni, nj);
                        if cand[ix] && !seen[ix] {
                            seen[ix] = true;
                            comp.push((ni, nj));
                            q.push((ni, nj));
                        }
                    }
                }
                if comp.len() >= DA_RAWMIN_BINS {
                    for (ci, cj) in comp {
                        valid[idx(ci, cj)] = true;
                        value[idx(ci, cj)] = raw[ci][cj];
                    }
                }
            }
        }
    }

    // Seed 2: zero isodop near the radar, with a counterpart ~180° away.
    let near_gates = ((40.0 - sweep.first_gate_range_km) / sweep.gate_interval_km) as usize;
    for i in 0..n {
        let opp = (i + n / 2) % n;
        for j in 0..near_gates.min(gc) {
            if has(i, j)
                && raw[i][j].abs() < DA_ZISO_TOL
                && (0..3).any(|d| {
                    let o = (opp + d) % n;
                    has(o, j) && raw[o][j].abs() < DA_ZISO_TOL
                })
            {
                valid[idx(i, j)] = true;
                value[idx(i, j)] = raw[i][j];
            }
        }
    }

    let unfold =
        |v: f64, reference: f64| -> f64 { v + ((reference - v) / interval).round() * interval };

    // Robust directional unfold chain over a gap (NaN = missing gate):
    // references the running mean of the last ≤3 accepted values and skips
    // isolated outliers (left uncommitted), aborting only when over a third
    // of the gap's data gates fail. Measured: a fragile strict chain reaches
    // reference coverage only with its thresholds widened far enough that
    // the radial bridge mis-unfolds pockets the reference keeps (branch
    // `campaign-harness`).
    let chain = |seed: f64, raws: &[f64], t: f64, gap_free: i32| -> Option<Vec<f64>> {
        let mut out = Vec::with_capacity(raws.len());
        let mut acc: Vec<f64> = vec![seed];
        let mut fails = 0usize;
        let mut datag = 0usize;
        let mut gap = 0i32;
        for &r in raws {
            if r.is_nan() {
                out.push(f64::NAN);
                gap += 1;
                continue;
            }
            let jumped = gap_free > 0 && gap >= gap_free;
            gap = 0;
            if jumped {
                // Re-entry after a long gap: unfold to the nearest branch of
                // the carried reference without a continuity test; the
                // two-direction identity check is the acceptance criterion.
                let refm = *acc.last().unwrap();
                let u = unfold(r, refm);
                out.push(u);
                acc.clear();
                acc.push(u);
                datag += 1;
                continue;
            }
            datag += 1;
            let take = acc.len().min(3);
            let refm: f64 = acc[acc.len() - take..].iter().sum::<f64>() / take as f64;
            let u = unfold(r, refm);
            if (u - refm).abs() > t {
                fails += 1;
                out.push(f64::NAN);
                if fails * 3 > datag {
                    return None;
                }
                continue;
            }
            out.push(u);
            acc.push(u);
        }
        Some(out)
    };

    // Two-direction agreement: one-sided gates are skipped; a two-sided
    // disagreement rejects the whole bridge.
    let bridge_reject = |fwd: &[f64], bwd: &[f64]| {
        fwd.iter()
            .zip(bwd)
            .any(|(a, b)| !a.is_nan() && !b.is_nan() && (a - b).abs() >= 0.01)
    };

    for _pass in 0..DA_PASSES {
        let mut changed = false;

        // (a) radial bridge (and (e): the skip variant tolerates ND inside).
        for skip_nd in [false, true] {
            let t = if skip_nd { 0.45 } else { 0.6 } * nyquist * DA_THRESH_SCALE;
            for i in 0..n {
                let mut j = 0;
                while j < gc {
                    if !valid[idx(i, j)] {
                        j += 1;
                        continue;
                    }
                    // find next valid gate beyond a run of invalid data gates
                    let mut k = j + 1;
                    let mut any_gap = false;
                    while k < gc && !valid[idx(i, k)] {
                        if has(i, k) {
                            any_gap = true;
                        } else if !skip_nd {
                            break;
                        }
                        k += 1;
                    }
                    if k >= gc || !valid[idx(i, k)] || !any_gap {
                        j = k.max(j + 1);
                        continue;
                    }
                    // outward from j, inward from k; commit where they agree
                    let raws_f: Vec<f64> = ((j + 1)..k)
                        .map(|m| if has(i, m) { raw[i][m] } else { f64::NAN })
                        .collect();
                    let raws_b: Vec<f64> = raws_f.iter().rev().copied().collect();
                    let gf = if skip_nd { DA_GAPJUMP_GATES } else { 0 };
                    let fwd = chain(value[idx(i, j)], &raws_f, t, gf);
                    let bwd = chain(value[idx(i, k)], &raws_b, t, gf).map(|mut v| {
                        v.reverse();
                        v
                    });
                    if let (Some(fwd), Some(bwd)) = (fwd, bwd)
                        && !bridge_reject(&fwd, &bwd)
                    {
                        for (off, (a, b)) in fwd.iter().zip(&bwd).enumerate() {
                            if !a.is_nan() && !b.is_nan() {
                                valid[idx(i, j + 1 + off)] = true;
                                value[idx(i, j + 1 + off)] = *a;
                                changed = true;
                            }
                        }
                    }
                    j = k;
                }
            }
        }

        // (b) azimuthal bridge, tighter threshold; azimuth wraps.
        let t_b = 0.35 * nyquist * DA_THRESH_SCALE;
        for j in 0..gc {
            for start in 0..n {
                if !valid[idx(start, j)] {
                    continue;
                }
                let mut k = 1;
                let mut any_gap = false;
                while k < 40 {
                    let ii = (start + k) % n;
                    if valid[idx(ii, j)] {
                        break;
                    }
                    if has(ii, j) {
                        any_gap = true;
                    } else {
                        k = 40;
                        break;
                    }
                    k += 1;
                }
                if k >= 40 || !any_gap {
                    continue;
                }
                let end = (start + k) % n;
                let raws_f: Vec<f64> = (1..k).map(|m| raw[(start + m) % n][j]).collect();
                let raws_b: Vec<f64> = raws_f.iter().rev().copied().collect();
                let fwd = chain(value[idx(start, j)], &raws_f, t_b, 0);
                let bwd = chain(value[idx(end, j)], &raws_b, t_b, 0).map(|mut v| {
                    v.reverse();
                    v
                });
                if let (Some(fwd), Some(bwd)) = (fwd, bwd)
                    && !bridge_reject(&fwd, &bwd)
                {
                    for (off, (a, b)) in fwd.iter().zip(&bwd).enumerate() {
                        if !a.is_nan() && !b.is_nan() {
                            let ii = (start + 1 + off) % n;
                            valid[idx(ii, j)] = true;
                            value[idx(ii, j)] = *a;
                            changed = true;
                        }
                    }
                }
            }
        }

        // (c)+(d) flood fills: runs of ≥10 unvalidated data gates alongside a
        // valid neighbour radial; raw acceptance at the (c) threshold,
        // unfolding acceptance at the tighter (d) threshold. Run-mean
        // decisions, not per-gate: the run's mean deviation from the neighbor
        // radial decides; individual gates only need to stay within 2t.
        for aliased in [false, true] {
            let t = if aliased {
                DA_FLOOD_ALIASED_FRAC
            } else {
                DA_FLOOD_RAW_FRAC
            } * nyquist
                * DA_THRESH_SCALE;
            for i in 0..n {
                for di in [n - 1, 1] {
                    let ni = (i + di) % n;
                    let mut run = 0usize;
                    for j in 0..gc {
                        let cand = has(i, j) && !valid[idx(i, j)] && valid[idx(ni, j)];
                        if cand {
                            run += 1;
                        } else {
                            run = 0;
                            continue;
                        }
                        if run >= 10 {
                            let lo = j + 1 - run;
                            let devs: Vec<(usize, f64, f64)> = (lo..=j)
                                .map(|m| {
                                    let neigh = value[idx(ni, m)];
                                    let u = if aliased {
                                        unfold(raw[i][m], neigh)
                                    } else {
                                        raw[i][m]
                                    };
                                    (m, u, u - neigh)
                                })
                                .collect();
                            let mean = devs.iter().map(|d| d.2).sum::<f64>() / run as f64;
                            if mean.abs() < t {
                                for &(m, u, d) in &devs {
                                    if d.abs() < 2.0 * t {
                                        valid[idx(i, m)] = true;
                                        value[idx(i, m)] = u;
                                        changed = true;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // (f) head and shoulders: single invalid gate matching the average of
        // three valid gates on each side along the radial.
        for i in 0..n {
            for j in 3..gc.saturating_sub(3) {
                if !has(i, j) || valid[idx(i, j)] {
                    continue;
                }
                let before: Vec<f64> = (j - 3..j)
                    .filter(|&m| valid[idx(i, m)])
                    .map(|m| value[idx(i, m)])
                    .collect();
                let after: Vec<f64> = (j + 1..j + 4)
                    .filter(|&m| valid[idx(i, m)])
                    .map(|m| value[idx(i, m)])
                    .collect();
                if before.len() == 3 && after.len() == 3 {
                    let avg = (before.iter().sum::<f64>() + after.iter().sum::<f64>()) / 6.0;
                    let u = unfold(raw[i][j], avg);
                    if (u - avg).abs() < 0.3 * nyquist * DA_THRESH_SCALE {
                        valid[idx(i, j)] = true;
                        value[idx(i, j)] = u;
                        changed = true;
                    }
                }
            }
        }

        if !changed {
            break;
        }
    }

    // Convert unresolved to ND; write dealiased values back. Never-reached
    // data gates keep their raw values in bulk — measured: the reference
    // dealiaser resolves ALL present data,
    // including isolated gates no propagation pass can reach —
    // unresolved-to-ND conversion evidently applies to contradictory
    // bridging, not unreached data. Size-gate the kept-raw regions: connected
    // components (4-adjacency, azimuth wraps) of unreached data gates below
    // the measured minimum are dropped to ND.
    let mut keep_raw = vec![false; n * gc];
    let mut seen = vec![false; n * gc];
    for si in 0..n {
        for sj in 0..gc {
            let s0 = idx(si, sj);
            if seen[s0] || valid[s0] || !has(si, sj) {
                continue;
            }
            let mut comp = vec![(si, sj)];
            seen[s0] = true;
            let mut q = vec![(si, sj)];
            while let Some((ci, cj)) = q.pop() {
                let neigh = [
                    ((ci + n - 1) % n, cj),
                    ((ci + 1) % n, cj),
                    (ci, cj.wrapping_sub(1)),
                    (ci, cj + 1),
                ];
                for (ni, nj) in neigh {
                    if nj >= gc {
                        continue;
                    }
                    let ix = idx(ni, nj);
                    if !seen[ix] && !valid[ix] && has(ni, nj) {
                        seen[ix] = true;
                        comp.push((ni, nj));
                        q.push((ni, nj));
                    }
                }
            }
            if comp.len() >= knobs.rawmin_bins {
                for (ci, cj) in comp {
                    keep_raw[idx(ci, cj)] = true;
                }
            }
        }
    }
    for i in 0..n {
        for j in 0..gc {
            vel_grid[i][j] = if valid[idx(i, j)] {
                value[idx(i, j)]
            } else if keep_raw[idx(i, j)] {
                raw[i][j]
            } else {
                f64::NAN
            };
        }
    }
    // Post-dealias fold censor: a bin more than CENSOR_VNY_FRAC·Vny from any
    // 4-neighbor marks a fold wall no pass could place — kept-raw folded
    // regions meet correctly unfolded ones exactly there.
    if knobs.censor_vny_frac.is_infinite() {
        return;
    }
    let snapshot: Vec<Vec<f64>> = vel_grid.to_vec();
    let censor_at = knobs.censor_vny_frac * nyquist;
    for i in 0..n {
        for j in 0..gc {
            let v = snapshot[i][j];
            if v.is_nan() {
                continue;
            }
            let up = snapshot[(i + 1) % n][j];
            let down = snapshot[(i + n - 1) % n][j];
            let right = if j + 1 < gc {
                snapshot[i][j + 1]
            } else {
                f64::NAN
            };
            let left = if j > 0 { snapshot[i][j - 1] } else { f64::NAN };
            for nb in [up, down, left, right] {
                if !nb.is_nan() && (nb - v).abs() > censor_at {
                    vel_grid[i][j] = f64::NAN;
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The hoisted beam height is the shared one, bit for bit.
    ///
    /// This module shares [`crate::beam::RE_EFF_KM`] but still writes the height
    /// arithmetic out once, in [`height_km_with_sin_el`], because
    /// [`WindProfileBuilder::add_sweep`] hoists `sin(elevation)` across tens of
    /// thousands of gates. Sharing a constant does not stop an expression from
    /// drifting, and NROT is the calibrated path the five pinned echo-tops
    /// digests never touch — so the copy is pinned here instead.
    ///
    /// Bit-exact rather than approximate on purpose: `add_sweep` bins samples
    /// with `(h / PROFILE_LAYER_KM) as usize`, a **floor**, so a single-ulp
    /// difference at a layer boundary moves a sample into the adjacent wind
    /// layer with no error, no NaN and no visible symptom.
    #[test]
    fn the_hoisted_beam_height_is_bit_identical_to_the_shared_one() {
        // The VCP 212 ladder, and gate centres out past the velocity extent.
        const ELEVS: [f64; 16] = [
            0.2, 0.5, 0.9, 1.3, 1.8, 2.4, 3.1, 4.0, 5.1, 6.4, 8.0, 10.0, 12.0, 14.0, 16.7, 19.5,
        ];
        let mut checked = 0usize;
        for &e in &ELEVS {
            let sin_el = e.to_radians().sin();
            // 0.125 km first-gate centre, 0.25 km gates, 1200 gates -> 300 km.
            for j in 0..1200 {
                let r = 0.125 + j as f64 * 0.25;
                let hoisted = height_km_with_sin_el(r, sin_el);
                let shared = crate::beam::height_km(r, e);
                assert_eq!(
                    hoisted.to_bits(),
                    shared.to_bits(),
                    "the hoisted height drifted from `beam::height_km` at \
                     {r} km / {e}°: {hoisted} vs {shared}",
                );
                // The consequence that makes bit-identity load-bearing.
                assert_eq!(
                    (hoisted / PROFILE_LAYER_KM) as usize,
                    (shared / PROFILE_LAYER_KM) as usize,
                    "the two heights bin to different wind layers at \
                     {r} km / {e}°",
                );
                checked += 1;
            }
        }
        assert_eq!(
            checked,
            ELEVS.len() * 1200,
            "precondition: the grid did not cover every tilt × gate",
        );
    }

    fn sweep_for<'a>(
        vel_grid: &'a [Vec<f64>],
        azimuths_deg: &'a [f64],
        gate_count: usize,
    ) -> VelocitySweep<'a> {
        VelocitySweep {
            vel_grid,
            azimuths_deg,
            gate_count,
            first_gate_range_km: 2.125,
            gate_interval_km: 0.25,
        }
    }

    fn sweep<'a>(grid: &'a [Vec<f64>], azimuths: &'a [f64], gates: usize) -> VelocitySweep<'a> {
        VelocitySweep {
            vel_grid: grid,
            azimuths_deg: azimuths,
            gate_count: gates,
            first_gate_range_km: 0.25,
            gate_interval_km: 0.25,
        }
    }

    fn ring_azimuths(n: usize) -> Vec<f64> {
        (0..n).map(|i| i as f64 * 360.0 / n as f64).collect()
    }

    /// A field folded across the Nyquist limit must come back continuous:
    /// with an environmental wind profile seeding the dealiaser, the folded
    /// arcs unfold by one full 2·Vny interval instead of standing as phantom
    /// shear walls.
    #[test]
    fn dealias_unfolds_a_folded_patch() {
        let n = 72;
        let gates = 40;
        let nyquist = 25.0;
        // True field: a uniform 30 m/s southerly flow, vr = 30·cos(az).
        // |vr| > 25 folds in the arcs around az 0 and az 180.
        let azs: Vec<f64> = (0..n).map(|i| i as f64 * 360.0 / n as f64).collect();
        let true_v: Vec<f64> = azs.iter().map(|a| 30.0 * a.to_radians().cos()).collect();
        let mut grid: Vec<Vec<f64>> = true_v
            .iter()
            .map(|&v| {
                let folded = if v > nyquist {
                    v - 2.0 * nyquist
                } else if v < -nyquist {
                    v + 2.0 * nyquist
                } else {
                    v
                };
                vec![folded; gates]
            })
            .collect();
        // One bin pinned at the fold limit so the Nyquist estimate is exactly
        // 25 (az 90°, true vr ≈ 0 — an isolated spike the passes drop).
        grid[18][0] = 25.0;
        let wp = WindProfile::from_levels(&[(0.0, 0.0, 30.0)]).unwrap();

        let vg = grid.clone();
        let sw = sweep_for(&vg, &azs, gates);
        dealias(&mut grid, &sw, 0.5, Some(&wp), DealiasProfile::NoFalseShear);

        assert_eq!(grid[0][10], 30.0, "folded arc should unfold to +30");
        assert_eq!(grid[12][10], true_v[12], "unfolded flow must not move");

        // The Coverage profile shares every unfolding pass, so a field the
        // passes settle comes back identical under both postures.
        let mut coverage = vg.clone();
        dealias(&mut coverage, &sw, 0.5, Some(&wp), DealiasProfile::Coverage);
        assert_eq!(coverage[0][10], 30.0);
        assert_eq!(coverage[12][10], true_v[12]);
    }

    /// The profile parameter reaches only the post-pass censoring: an
    /// unreached data region smaller than `DA_RAWMIN_BINS` goes ND under
    /// `NoFalseShear` — today's tuned NROT behaviour, unchanged — and keeps
    /// its raw values under `Coverage`.
    #[test]
    fn a_small_unreached_region_is_nd_for_nrot_and_raw_for_coverage() {
        let n = 72;
        let gates = 40;
        // Nothing seeds: no wind profile, and no data near zero inside 40 km
        // (the zero-isodop band). A lone 2×3 patch of 20 m/s at long range is
        // unreachable by every propagation pass.
        let mut grid: Vec<Vec<f64>> = vec![vec![f64::NAN; gates]; n];
        for row in grid.iter_mut().take(32).skip(30) {
            for g in row.iter_mut().take(39).skip(36) {
                *g = 20.0;
            }
        }
        // One far bin pins the Nyquist estimate above the 8 m/s floor.
        grid[0][39] = 26.0;
        let azs = ring_azimuths(n);
        let vg = grid.clone();
        let sw = sweep_for(&vg, &azs, gates);

        let mut strict = grid.clone();
        dealias(&mut strict, &sw, 0.5, None, DealiasProfile::NoFalseShear);
        assert!(
            strict[30][37].is_nan(),
            "a 6-bin unreached region is under DA_RAWMIN_BINS and goes ND"
        );

        let mut coverage = grid.clone();
        dealias(&mut coverage, &sw, 0.5, None, DealiasProfile::Coverage);
        assert_eq!(
            coverage[30][37], 20.0,
            "Coverage keeps every unreached data gate at raw"
        );
    }

    /// A continuous field, even a sheared one, must pass through untouched:
    /// the zero-isodop seeds anchor it and every propagation pass then keeps
    /// raw values on the zero-fold branch.
    #[test]
    fn dealias_leaves_continuous_data_alone() {
        let n = 72;
        let gates = 40;
        let grid_orig: Vec<Vec<f64>> = (0..n)
            .map(|i| {
                let v = 20.0 * (i as f64 / n as f64 * std::f64::consts::TAU).sin();
                vec![v; gates]
            })
            .collect();
        let mut grid = grid_orig.clone();
        let azs: Vec<f64> = (0..n).map(|i| i as f64 * 360.0 / n as f64).collect();
        let vg = grid.clone();
        let sw = sweep_for(&vg, &azs, gates);
        dealias(&mut grid, &sw, 0.5, None, DealiasProfile::NoFalseShear);
        assert_eq!(grid, grid_orig);
    }

    /// The median filter's job in this pipeline: a single-bin velocity spike
    /// disappears; the surrounding field survives.
    #[test]
    fn median_filter_removes_an_isolated_spike() {
        let n = 40;
        let gates = 40;
        let mut grid: Vec<Vec<f64>> = vec![vec![10.0; gates]; n];
        grid[20][20] = 90.0;
        let filtered = median_filter(&grid, &grid, gates, 0.25, 0.25, 360.0 / n as f64);
        assert_eq!(filtered[20][20], 10.0);
        assert_eq!(filtered[10][10], 10.0);
    }

    /// The divisor curve is the measured factory table: kilometre knots,
    /// linearly interpolated — from the reference step response on a
    /// synthetic volume. Check the knots, mid-band values, and both flat
    /// extensions.
    #[test]
    fn rot_divisor_matches_the_factory_curve() {
        assert_eq!(rot_divisor_km(10.0), 25.0); // flat below the first knot
        assert_eq!(rot_divisor_km(20.0), 25.0);
        assert_eq!(rot_divisor_km(30.0), 22.5); // halfway 25 → 20
        assert_eq!(rot_divisor_km(40.0), 20.0);
        assert_eq!(rot_divisor_km(50.0), 16.0); // halfway 20 → 12
        assert_eq!(rot_divisor_km(60.0), 12.0);
        assert_eq!(rot_divisor_km(70.0), 10.0); // halfway 12 → 8
        assert_eq!(rot_divisor_km(80.0), 8.0);
        assert_eq!(rot_divisor_km(250.0), 8.0); // flat beyond the last knot
        // The nm entry point converts and lands on the same curve.
        assert_eq!(rot_divisor(40.0 / KM_PER_NM), 20.0);
    }

    /// On v = k·(azimuthal arc), the recovered slope is k and NROT is k over
    /// the local divisor. Checked away from the grid edges at a known range.
    #[test]
    fn llsd_recovers_a_linear_azimuthal_gradient() {
        let n = 720;
        let gates = 400;
        let azimuths = ring_azimuths(n);
        // 6 (m/s)/km of azimuthal shear everywhere, small enough to not fold.
        let k = 6.0;
        let grid: Vec<Vec<f64>> = (0..n)
            .map(|i| {
                let theta = (azimuths[i]).to_radians();
                let dtheta = if theta > PI { theta - 2.0 * PI } else { theta };
                (0..gates)
                    .map(|j| {
                        let r = 0.25 + j as f64 * 0.25;
                        k * r * dtheta
                    })
                    .collect()
            })
            .collect();
        let s = sweep(&grid, &azimuths, gates);
        let nrot = llsd_nrot(&s, &grid);

        // Gate 200 → 50.25 km: inside the split-tap operator's domain. Its
        // ramp gain is the mean of the clean and split sides' Σ t·o over the
        // legacy arc: (2ĉ₁+3ĉ₂+4ĉ₃ + ĉ₂+2(ĉ₁−ĉ₂)+3ĉ₂+4ĉ₃) / 2.
        let range_nm = (0.25 + 200.0 * 0.25) / KM_PER_NM;
        let clean: f64 = SPLIT_CLEAN.iter().map(|&(o, t)| o as f64 * t).sum();
        let away: f64 = SPLIT_AWAY.iter().map(|&(o, t)| o as f64 * t).sum();
        let expected = k * (clean + away) / 2.0 / rot_divisor(range_nm);
        let got = nrot[10][200];
        assert!(
            (got - expected).abs() < 0.03,
            "NROT {got} != expected {expected}"
        );
    }

    /// Every complete cut keeps the step it always had, bit for bit: n radials
    /// around a circle leave n gaps summing to 360°, so `360 / n` is their
    /// exact mean and a measured median is only a noisier reading of the same
    /// number. Every constant in this module was calibrated against a full
    /// rotation, so this is the invariance that leaves them measuring what
    /// they were measured against.
    #[test]
    fn a_closed_sweep_keeps_the_step_it_always_had() {
        for n in [360usize, 720] {
            assert_eq!(radial_step_deg(&ring_azimuths(n), n), 360.0 / n as f64);
        }

        // Collection order starts wherever the antenna was.
        let rolled: Vec<f64> = (0..720).map(|i| f64::from((i + 431) % 720) * 0.5).collect();
        assert_eq!(radial_step_deg(&rolled, 720), 0.5);

        // Real azimuths jitter by a few hundredths of a step; ±0.02° is that,
        // and the median of 720 such gaps still reads within a thousandth of a
        // degree of the mean — ten times inside what CLOSED_SWEEP_COVERAGE
        // leaves, so the sweep is still read as closed.
        let jittered: Vec<f64> = (0..720)
            .map(|i| i as f64 * 0.5 + 0.02 * (i as f64 * 1.7).sin())
            .collect();
        assert_eq!(radial_step_deg(&jittered, 720), 0.5);

        // One radial dropped is a hole in a rotation, not a sector: 719 × 0.5°
        // still accounts for 359.5° of the 360.
        let dropped: Vec<f64> = ring_azimuths(720)
            .into_iter()
            .filter(|a| *a != 100.0)
            .collect();
        assert_eq!(radial_step_deg(&dropped, 719), 360.0 / 719.0);
    }

    /// The same physical shear presented twice — 0.5° radials all the way
    /// round, and the first 36° of them standing alone — reports the same
    /// rotation. The sector is differentiated over the arc its radials
    /// actually span, not over the 5° a row `360 / 72` would claim, which is
    /// ten times the arc and so a tenth of the shear.
    #[test]
    fn a_sector_reports_the_rotation_its_own_spacing_carries() {
        let gates = 400;
        // 6 (m/s)/km of azimuthal shear everywhere, as a function of azimuth
        // so both presentations see one field.
        let k = 6.0;
        let row = |az_deg: f64| -> Vec<f64> {
            let theta = az_deg.to_radians();
            let dtheta = if theta > PI { theta - 2.0 * PI } else { theta };
            (0..gates)
                .map(|j| k * (0.25 + j as f64 * 0.25) * dtheta)
                .collect()
        };
        let full_az = ring_azimuths(720);
        let sector_az = full_az[..72].to_vec();
        let full: Vec<Vec<f64>> = full_az.iter().map(|&a| row(a)).collect();
        let sector: Vec<Vec<f64>> = sector_az.iter().map(|&a| row(a)).collect();

        let full_nrot = llsd_nrot(&sweep(&full, &full_az, gates), &full);
        let sector_nrot = llsd_nrot(&sweep(&sector, &sector_az, gates), &sector);

        // Rows 20..52 of the sector read only rows 3..69 of it, so their whole
        // support lies inside the arc and is the full rotation's data bin for
        // bin — the kernel bank's footprint layer reaches furthest, capping a
        // row from a core ±6 away that is itself read over ±11 radials.
        let mut carried = 0;
        for i in 20..52 {
            for j in 100..300 {
                let (s, f) = (sector_nrot[i][j], full_nrot[i][j]);
                assert!(
                    s == f || (s.is_nan() && f.is_nan()),
                    "row {i} gate {j}: the sector read {s}, the rotation {f}",
                );
                carried += usize::from(s.is_finite());
            }
        }
        assert_eq!(carried, 32 * 200, "the compared window read mostly ND");

        // And the value is the shear that is there: gate 200 is 50.25 km,
        // inside the split-tap domain, where the ramp gain is the mean of the
        // clean and split sides' Σ t·o over the legacy arc.
        let range_nm = (0.25 + 200.0 * 0.25) / KM_PER_NM;
        let clean: f64 = SPLIT_CLEAN.iter().map(|&(o, t)| o as f64 * t).sum();
        let away: f64 = SPLIT_AWAY.iter().map(|&(o, t)| o as f64 * t).sum();
        let expected = k * (clean + away) / 2.0 / rot_divisor(range_nm);
        let got = sector_nrot[30][200];
        assert!(
            (got - expected).abs() < 0.03,
            "the sector read NROT {got}, not the {expected} its shear carries \
             (over a 5° row it would read {:.3})",
            expected / 10.0,
        );
    }

    /// Degenerate sweeps produce a step rather than an infinity or a zero.
    /// There is nothing to divide among no radials or one; a sweep that
    /// reported one azimuth n times has no gap to measure; and two radials —
    /// where the measurement reports the *larger* of the two circular gaps by
    /// construction — are not a sector, because 2 × 350° is more circle than
    /// there is.
    #[test]
    fn a_sweep_with_nothing_to_measure_keeps_the_closed_step() {
        assert_eq!(radial_step_deg(&[], 0), 360.0);
        assert_eq!(radial_step_deg(&[37.5], 1), 360.0);
        assert_eq!(radial_step_deg(&[12.0; 8], 8), 45.0);
        assert_eq!(radial_step_deg(&[0.0, 10.0], 2), 180.0);

        // Three radials of sector run the whole pipeline without dividing an
        // arc down to nothing: a constant field is incoherent, so every bin
        // reads ND rather than the ±5 clamp a zero arc would produce.
        let grid = vec![vec![10.0; 200]; 3];
        let azs = vec![0.0, 0.5, 1.0];
        let out = compute_nrot_grid(&sweep(&grid, &azs, 200));
        assert!(out.iter().flatten().all(|v| v.is_nan()));
    }

    /// One shear, sampled at 0.5° and at 1.0°, reads the same number — the
    /// property that says every stencil divisor in this module counts **rows
    /// of the grid** and not degrees of sky.
    ///
    /// Both estimators place their taps at row offsets, so a 1.0° sweep's
    /// numerator spans twice the arc a 0.5° sweep's does over the same field;
    /// `2.0 * arc_per_radial` and `arc_per_radial` grow by that same factor
    /// and the quotient is the shear either way. The identity is exact in the
    /// reals and holds to 3.1e-14 in f64 over the 1372 bins compared below —
    /// the stencils are zero-sum, so the background under the profile cancels
    /// exactly on paper and only to rounding in arithmetic.
    ///
    /// Pinning the divisor to a physical degree instead — reading
    /// `2.0 * arc_per_radial` as "1.0° of arc" rather than "two rows" — would
    /// halve it on a 1.0° sweep and report 0.869 where the field carries
    /// 0.434. That reading is what this test exists to rule out, since it is
    /// the one a reader meets first: the constant is *called* the legacy 1.0°
    /// arc, and on the super-res grid it was fitted on the two readings are
    /// the same number.
    #[test]
    fn one_shear_reads_the_same_at_either_radial_spacing() {
        let gates = 400;
        // 6 (m/s)/km of azimuthal shear, written as a function of azimuth so
        // both samplings see one field rather than two.
        let k = 6.0;
        let row = |az_deg: f64| -> Vec<f64> {
            let theta = az_deg.to_radians();
            let dtheta = if theta > PI { theta - 2.0 * PI } else { theta };
            (0..gates)
                .map(|j| k * (0.25 + j as f64 * 0.25) * dtheta)
                .collect()
        };
        let fine_az = ring_azimuths(720);
        let coarse_az = ring_azimuths(360);
        let fine: Vec<Vec<f64>> = fine_az.iter().map(|&a| row(a)).collect();
        let coarse: Vec<Vec<f64>> = coarse_az.iter().map(|&a| row(a)).collect();
        let fine_nrot = llsd_nrot(&sweep(&fine, &fine_az, gates), &fine);
        let coarse_nrot = llsd_nrot(&sweep(&coarse, &coarse_az, gates), &coarse);

        // Gates 100/200/300 are 25.25/50.25/75.25 km — the split-tap band;
        // 380 is 95.25 km, past SPLIT_MAX_RANGE_KM, so the composite stencil
        // and its one-row divisor are covered too. Azimuth 180 is the field's
        // own wrap, where both samplings read the ±5 clamp for reasons that
        // have nothing to do with spacing, and is left out of it.
        let mut compared = 0;
        let mut worst = 0.0f64;
        for az in 0..360 {
            if (az as f64 - 180.0).abs() <= 8.0 {
                continue;
            }
            for j in [100usize, 200, 300, 380] {
                let (c, f) = (coarse_nrot[az][j], fine_nrot[2 * az][j]);
                assert!(c.is_finite() && f.is_finite(), "az {az}° gate {j} read ND");
                worst = worst.max((c - f).abs());
                compared += 1;
            }
        }
        assert_eq!(compared, 343 * 4);
        // Not bit-for-bit, and the gap between the two is arithmetic rather
        // than physical: both stencils are zero-sum, so the background the
        // profile sits on cancels exactly in the reals and only to rounding in
        // f64, leaving a residue that depends on how large that background is.
        // The worst of the 1372 bins compared here is 3.1e-14, a trillion
        // times under the 0.04 the reference quantizes its own output in.
        assert!(
            worst < 1e-12,
            "0.5° and 1.0° read one field {worst} apart — a real disagreement, \
             not rounding",
        );

        // And the shared number is the shear that is there, not merely a
        // shared number: the split operator's ramp gain is the mean of its two
        // sides' Σ t·o over two rows, the composite's is Σ 2·o·c over one.
        let split_gain: f64 = (SPLIT_CLEAN.iter().map(|&(o, t)| o as f64 * t).sum::<f64>()
            + SPLIT_AWAY.iter().map(|&(o, t)| o as f64 * t).sum::<f64>())
            / 2.0;
        let composite_gain: f64 = COMPOSITE_TAPS
            .iter()
            .enumerate()
            .map(|(idx, &t)| 2.0 * (idx as f64 + 1.0) * t)
            .sum();
        for (j, gain) in [(200usize, split_gain), (380, composite_gain)] {
            let range_km = 0.25 + j as f64 * 0.25;
            let expect = k * gain / rot_divisor(range_km / KM_PER_NM);
            let got = coarse_nrot[90][j];
            assert!(
                (got - expect).abs() < 1e-9,
                "gate {j}: read {got}, not the {expect} a {k} (m/s)/km ramp carries",
            );
        }
    }

    /// A 1.0°-spaced sweep has no pair phase, and the measurement says so by
    /// returning nothing to choose between.
    ///
    /// [`pair_phase`] asks which of two index alignments puts radials in the
    /// same whole degree. Two radials 1.0° apart never are — their floors
    /// differ by one by construction — so both counts are zero and the answer
    /// is the tie-break, whatever the sweep's offset or jitter. That is the
    /// honest result for a grid that is already legacy resolution: each of its
    /// radials *is* a whole-degree bin. What it leaves behind is recorded at
    /// [`pair_phase`] and demonstrated in
    /// `a_super_res_sweep_reads_the_same_wherever_collection_began`.
    #[test]
    fn a_one_degree_sweep_has_no_pair_phase_to_measure() {
        let whole: Vec<f64> = (0..360).map(f64::from).collect();
        let offset: Vec<f64> = (0..360).map(|i| f64::from(i) + 0.37).collect();
        let half_offset: Vec<f64> = (0..360).map(|i| f64::from(i) + 0.5).collect();
        // A real antenna wanders a few hundredths of a degree off the grid.
        let jittered: Vec<f64> = (0..360)
            .map(|i| f64::from(i) + 0.06 * (f64::from(i) * 1.7).sin())
            .collect();
        for azs in [&whole, &offset, &half_offset, &jittered] {
            assert_eq!(pair_phase(azs), 0, "a 1.0° sweep reported a pairing");
        }

        // The super-res control: there the pairing is real, is found, and
        // follows absolute azimuth rather than collection index — a sweep
        // whose collection started half a degree along reports the other
        // phase, which is what keeps the answer the same.
        assert_eq!(pair_phase(&ring_azimuths(720)), 0);
        let rolled: Vec<f64> = (0..720).map(|i| f64::from(i) * 0.5 + 0.5).collect();
        assert_eq!(pair_phase(&rolled), 1);
    }

    /// Where the antenna happened to start a cut is not a property of the
    /// weather, so it must not move the rotation. On the 0.5° grid — the one
    /// validated against the reference — it does not: [`pair_phase`] anchors
    /// the split operator's asymmetry to absolute azimuth, and a sweep rolled
    /// by one radial reads bit for bit what it read before.
    ///
    /// The 1.0° half of this test records the open question rather than a
    /// property worth having. There the pairing cannot be measured
    /// (`a_one_degree_sweep_has_no_pair_phase_to_measure`), the asymmetry
    /// falls to `i % 2`, and the roll moves the field: bins cross the 0.04 the
    /// reference quantizes in, and some cross between a value and ND. The
    /// assertion is deliberately that *something* moves — when a measured
    /// answer for legacy-resolution grids arrives and this becomes invariant
    /// too, this is the line that should fail and be deleted.
    #[test]
    fn a_super_res_sweep_reads_the_same_wherever_collection_began() {
        let gates = 224; // to 56.0 km — the split-tap band, whole
        let j = 199; // 50.0 km, through the vortex core
        // A Rankine vortex at az 90°, 50 km, 3 km core, on a 15 m/s flow: the
        // tangential wind projects onto the beam as the azimuthal couplet the
        // product exists to find.
        let field = |az_deg: f64| -> Vec<f64> {
            (0..gates)
                .map(|jj| {
                    let r = 0.25 + jj as f64 * 0.25;
                    let across = r * (az_deg - 90.0).to_radians();
                    let along = r - 50.0;
                    let rad = (across * across + along * along).sqrt();
                    let vt = if rad < 3.0 {
                        20.0 * rad / 3.0
                    } else {
                        20.0 * 3.0 / rad
                    };
                    let couplet = if rad > 1e-9 { vt * across / rad } else { 0.0 };
                    15.0 * (az_deg - 40.0).to_radians().cos() + couplet
                })
                .collect()
        };
        // One row of NROT at 50 km, indexed by *physical* azimuth, from a
        // sweep whose collection order starts `roll` radials along.
        let read = |n: usize, step: f64, roll: usize| -> Vec<f64> {
            let azs: Vec<f64> = (0..n).map(|i| ((i + roll) % n) as f64 * step).collect();
            let grid: Vec<Vec<f64>> = azs.iter().map(|&a| field(a)).collect();
            let nrot = llsd_nrot(&sweep(&grid, &azs, gates), &grid);
            (0..n).map(|k| nrot[(k + n - roll) % n][j]).collect()
        };

        let fine = (read(720, 0.5, 0), read(720, 0.5, 1));
        let mut compared = 0;
        for (k, (&a, &b)) in fine.0.iter().zip(fine.1.iter()).enumerate() {
            assert!(
                a == b || (a.is_nan() && b.is_nan()),
                "0.5° az {}°: unrolled read {a}, rolled read {b}",
                k as f64 * 0.5,
            );
            compared += usize::from(a.is_finite());
        }
        assert_eq!(compared, 714, "the compared row read mostly ND");

        let coarse = (read(360, 1.0, 0), read(360, 1.0, 1));
        let moved = coarse
            .0
            .iter()
            .zip(coarse.1.iter())
            .filter(|(a, b)| {
                a.is_finite() != b.is_finite() || (a.is_finite() && (*a - *b).abs() > 0.04)
            })
            .count();
        assert!(
            moved > 0,
            "a 1.0° sweep became roll-invariant — if the pairing question was \
             settled, delete this half and pin the invariance instead",
        );
    }

    /// A patch whose velocity varies but carries no coherent azimuthal trend
    /// is noise, and the reference reports nothing at such bins even where it
    /// has good velocity. The coherence floor is what discards them, so a field
    /// that alternates sign radial-to-radial — zero net gradient, maximum
    /// variance — must produce no value rather than a large one.
    #[test]
    fn incoherent_patches_are_rejected_by_the_fit_quality_floor() {
        let n = 720;
        let gates = 200;
        let azimuths = ring_azimuths(n);
        // ±6 m/s alternating every radial: no linear trend at any scale.
        let grid: Vec<Vec<f64>> = (0..n)
            .map(|i| vec![if i % 2 == 0 { 6.0 } else { -6.0 }; gates])
            .collect();
        let s = sweep(&grid, &azimuths, gates);
        let nrot = llsd_nrot(&s, &grid);
        let strong = nrot
            .iter()
            .flatten()
            .filter(|v| !v.is_nan() && v.abs() >= 0.25)
            .count();
        assert_eq!(
            strong, 0,
            "{strong} incoherent bins survived as NROT >= 0.25"
        );
    }

    /// The full pipeline output is clamped to ±5 no matter how violent the
    /// input shear is.
    #[test]
    fn nrot_is_clamped_to_plus_minus_five() {
        let n = 720;
        let gates = 100;
        let azimuths = ring_azimuths(n);
        let grid: Vec<Vec<f64>> = (0..n)
            .map(|i| {
                // ±30 m/s alternating over 4-radial blocks: absurd shear.
                let v = if (i / 4) % 2 == 0 { 30.0 } else { -30.0 };
                vec![v; gates]
            })
            .collect();
        let s = sweep(&grid, &azimuths, gates);
        let nrot = compute_nrot_grid(&s);
        for row in &nrot {
            for v in row {
                assert!(v.is_nan() || v.abs() <= 5.0, "unclamped NROT {v}");
            }
        }
    }
    /// The split-tap operator reproduces the measured per-radial step
    /// profile: a ±8 m/s step at a whole-degree boundary reads the full
    /// value on four radials, then one radial each of the +0.10-class and
    /// −0.18-class tails, then nothing.
    #[test]
    fn split_stencil_matches_the_measured_step_profile() {
        let n = 720;
        let gates = 400;
        let azimuths = ring_azimuths(n); // i·0.5°, pairs at whole degrees
        let grid: Vec<Vec<f64>> = (0..n)
            .map(|i| vec![if azimuths[i] < 45.0 { -8.0 } else { 8.0 }; gates])
            .collect();
        let s = sweep(&grid, &azimuths, gates);
        let nrot = llsd_nrot(&s, &grid);

        let j = 153; // 38.5 km
        let range_km = 0.25 + j as f64 * 0.25;
        let arc_legacy = range_km * 1.0_f64.to_radians();
        let scale = 16.0 / arc_legacy / rot_divisor_km(range_km);
        let (c1, c2, c3) = (0.580, 0.238, -0.151);
        // Radial 90 is the first +8 radial; the boundary sits between pairs.
        // The full-value core must paint on exactly its four radials; the
        // sub-threshold tails may read ND (the coherence gate drops them,
        // and the display palette would not paint them either) but when
        // present must carry the measured class values.
        let full = (c1 + c2 + c3) * scale;
        for (radial, row) in nrot.iter().enumerate().take(92).skip(88) {
            let got = row[j];
            assert!(
                (got - full).abs() < 0.02,
                "radial {radial}: got {got:.3}, expected {full:.3}"
            );
        }
        for (radial, expect) in [
            (86, c3 * scale),
            (87, (c2 + c3) * scale),
            (92, (c2 + c3) * scale),
            (93, c3 * scale),
        ] {
            let got = nrot[radial][j];
            assert!(
                got.is_nan() || (got - expect).abs() < 0.02,
                "radial {radial}: got {got:.3}, expected ND or {expect:.3}"
            );
        }
        for radial in [84, 85, 94, 95] {
            let got = nrot[radial][j];
            assert!(
                got.is_nan() || got.abs() < 0.02,
                "radial {radial}: got {got:.3}, expected ~0"
            );
        }
    }
}
