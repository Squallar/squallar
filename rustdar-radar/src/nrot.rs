//! Normalized Rotation (NROT): the azimuthal derivative of radial velocity,
//! normalized by a range-dependent divisor so one number reads the same at
//! every distance from the radar. Every stage below was empirically
//! calibrated against a reference implementation by injecting synthetic
//! Level II volumes with known velocity patterns (azimuthal steps, couplets,
//! noise, sinusoids, range slabs) and measuring the response — kernel taps,
//! divisor curve, median geometry, and gating all measured rather than
//! guessed:
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
//!    operator ([`SPLIT_CLEAN`]/[`SPLIT_AWAY`]) inside 80 km — solved
//!    exactly from measured per-radial step profiles — and the
//!    composite 11-radial stencil [`COMPOSITE_TAPS`] beyond, measured from
//!    the step response, applied to 3-gate range means and divided by the
//!    local arc per radial. The sign-reversed outer taps produce the small
//!    negative side lobes flanking every strong gradient. All five tap pairs
//!    must be intact and the profile must correlate with the stencil
//!    (r² ≥ 0.05); constant or incoherent profiles read ND.
//! 4. Divide ROT by the divisor curve — knot ranges in KILOMETRES, linearly
//!    interpolated (25 at ≤20 km → 20 → 12 → 8 at 80 km, flat beyond) — and
//!    clamp to ±5. Measured from the step-response ladder.
//!    Inside 80 km a matched-filter footprint pass ([`apply_kernel_bank`])
//!    then caps each detected rotation couplet with the kernel fitted to
//!    its measured pole width, reproducing the reference's width-dependent
//!    edge compression while monopolar notches keep the full value.
//! 5. Blank painted clusters under 4 bins and one-gate-deep slivers; the
//!    result matches the reference painted density and correlates 0.996
//!    with reference cursor readouts over a real-volume ground-truth set.
//!
//! Values above 1.0 are significant rotation; above 2.5, extreme. The
//! reference quantizes NROT in steps of 0.04, so differences below ~0.04
//! are not observable in its output at all.

#[cfg(not(target_arch = "wasm32"))]
use rayon::prelude::*;
use std::f64::consts::PI;

#[cfg(target_arch = "wasm32")]
use seq_fallback::*;

/// Sequential stand-in for the one rayon entry point this module uses, for
/// the same reason as [`crate::render`]'s fallback: wasm32 is single-threaded.
#[cfg(target_arch = "wasm32")]
mod seq_fallback {
    pub trait IntoParIterFallback {
        type Item;
        fn into_par_iter(self) -> impl Iterator<Item = Self::Item>;
    }

    impl IntoParIterFallback for std::ops::Range<usize> {
        type Item = usize;
        fn into_par_iter(self) -> impl Iterator<Item = usize> {
            self
        }
    }
}

const KM_PER_NM: f64 = 1.852;

/// NROT is defined on -5..+5; the divisor curve guarantees nothing, so clamp.
const NROT_LIMIT: f64 = 5.0;

/// Skip bins closer than this. Residual ground clutter close to the radar
/// produces clamp-level fake shear (adjacent ±30 m/s bins over tens of meters
/// of arc). Measured exactly on a synthetic volume: the reference reads ND
/// for a ±15 m/s couplet at 12.48 km and a value at 12.59 km, so the floor is
/// 12.5 km (6.75 nm) — consistent with reference sweep extremes always
/// landing at 6.8-7.2 nm on real volumes.
const MIN_RANGE_NM: f64 = 6.75;

/// Blank painted clusters (8-connected runs of |NROT| ≥ 0.25) smaller than
/// this many bins. Matches the reference painted density over five volumes.
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

/// Run the full pipeline without a wind profile (elevation assumed 0.5°).
/// Output is indexed like the input grid; NaN where NROT is undefined (no
/// velocity, or too few neighbours to fit).
pub fn compute_nrot_grid(sweep: &VelocitySweep) -> Vec<Vec<f64>> {
    compute_nrot_grid_with_profile(sweep, 0.5, None)
}

/// Run the full pipeline with a volume wind profile guiding fold-branch
/// decisions. The profile comes from the RPG's NVW product or from every
/// velocity tilt in the volume via [`WindProfileBuilder`], so its
/// predictions stay well-conditioned at long range where the sweep's own
/// echo fills only a narrow azimuth sector.
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
/// |NROT| ≥ 0.25 (either sign — a tiny dipole is still speckle). Azimuth
/// wraps; range does not.
fn despeckle_nrot(grid: &mut [Vec<f64>], min_bins: usize) {
    let num_radials = grid.len();
    if num_radials == 0 {
        return;
    }
    let gate_count = grid[0].len();
    let painted = |g: &[Vec<f64>], i: usize, j: usize| {
        let v = g[i][j];
        !v.is_nan() && v.abs() >= 0.25
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
    )
}

/// Effective earth radius for beam-height, km (4/3 model).
const RE_EFF_KM: f64 = 4.0 / 3.0 * 6371.0;
/// Wind-profile layer thickness, km.
const VWP_LAYER_KM: f64 = 0.3;
/// Layers span 0..12 km AGL.
const VWP_LAYERS: usize = 40;
/// Sample cap per layer keeps memory bounded on wasm.
const VWP_MAX_SAMPLES: usize = 16384;

/// Horizontal wind fitted per height layer from every velocity tilt of a
/// volume: vr ≈ u·sin(az)·cos(el) + v·cos(az)·cos(el) + c.
pub struct WindProfile {
    /// (u, v, c) per layer; NaN-filled layers had too little data.
    layers: Vec<Option<(f64, f64, f64)>>,
}

/// Extract (height km, u m/s, v m/s) wind levels from a Level III VAD Wind
/// Profile (NVW) product payload. The product's tabular alphanumeric block
/// carries plain-text rows — `ALT(100s of ft)  U  V  W  DIR  SPD …` — so this
/// scans ASCII runs rather than decoding the binary product structure. Rows
/// repeat per tilt; the last row per altitude wins. Feed the result to
/// [`WindProfile::from_levels`] and the winds-aware render entry points.
pub fn parse_nvw_wind_levels(payload: &[u8]) -> Vec<(f64, f64, f64)> {
    let mut by_alt: Vec<(u32, f64, f64)> = Vec::new();
    for run in payload.split(|b| !(0x20..0x7f).contains(b)) {
        let Ok(text) = std::str::from_utf8(run) else {
            continue;
        };
        let mut it = text.split_whitespace().peekable();
        // Optional leading page marker.
        if it.peek() == Some(&"P") {
            it.next();
        }
        let (Some(alt), Some(u), Some(v)) = (it.next(), it.next(), it.next()) else {
            continue;
        };
        if alt.len() != 3 || !alt.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let (Ok(alt), Ok(u), Ok(v)) = (alt.parse::<u32>(), u.parse::<f64>(), v.parse::<f64>())
        else {
            continue;
        };
        if !(u.abs() <= 150.0 && v.abs() <= 150.0) {
            continue;
        }
        if let Some(e) = by_alt.iter_mut().find(|(a, _, _)| *a == alt) {
            (e.1, e.2) = (u, v);
        } else {
            by_alt.push((alt, u, v));
        }
    }
    by_alt.sort_unstable_by_key(|e| e.0);
    by_alt
        .into_iter()
        .map(|(a, u, v)| (a as f64 * 100.0 * 0.3048 / 1000.0, u, v))
        .collect()
}

impl WindProfile {
    /// Build from explicit (height km, u, v) levels — e.g. the RPG's own VAD
    /// Wind Profile (Level 3 NVW product), an externally quality-controlled
    /// wind source well suited to seeding the dealiaser. Levels map to the
    /// internal layers; gaps between adjacent levels are filled by the
    /// nearer level.
    pub fn from_levels(levels: &[(f64, f64, f64)]) -> Option<Self> {
        if levels.is_empty() {
            return None;
        }
        let mut layers: Vec<Option<(f64, f64, f64)>> = vec![None; VWP_LAYERS];
        for &(h, u, v) in levels {
            let l = (h / VWP_LAYER_KM) as usize;
            if l < VWP_LAYERS {
                layers[l] = Some((u, v, 0.0));
            }
        }
        // Fill interior gaps from the nearest filled layer below/above.
        let filled: Vec<usize> = (0..VWP_LAYERS).filter(|&l| layers[l].is_some()).collect();
        for l in 0..VWP_LAYERS {
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
    pub const LAYER_KM: f64 = VWP_LAYER_KM;

    /// The fitted horizontal wind `(u, v)` in m/s at `height_km` AGL, or
    /// `None` below zero, above the profile, or in a layer nothing fit.
    /// Resolved at layer granularity ([`Self::LAYER_KM`]), no interpolation.
    pub fn wind_at_km(&self, height_km: f64) -> Option<(f64, f64)> {
        if !height_km.is_finite() || height_km < 0.0 {
            return None;
        }
        let l = (height_km / VWP_LAYER_KM) as usize;
        self.layers.get(l)?.map(|(u, v, _)| (u, v))
    }

    /// Predicted radial velocity at the given azimuth (radians), range (km)
    /// and elevation (degrees), or None where no layer was fit.
    fn predict(&self, az_rad: f64, range_km: f64, elevation_deg: f64) -> Option<f64> {
        let el = elevation_deg.to_radians();
        let h = range_km * el.sin() + range_km * range_km / (2.0 * RE_EFF_KM);
        let l = (h / VWP_LAYER_KM) as usize;
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
            samples: (0..VWP_LAYERS).map(|_| Vec::new()).collect(),
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
                let h = r * sin_el + r * r / (2.0 * RE_EFF_KM);
                let l = (h / VWP_LAYER_KM) as usize;
                if l < VWP_LAYERS && self.samples[l].len() < VWP_MAX_SAMPLES {
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
        // beam reaches that height. Measured on a real volume: the reference
        // keeps 56% of its far band where we kept 11% without the extension.
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
const MEDIAN_HALF_WIDTH_KM: f64 = 0.4;

/// Cap on the median filter's azimuthal half-count. Measured: the reference
/// median erases 2-radial couplets at 31 km and reads compact near-radar
/// couplets ~40% below a 5-radial-median pipeline, implying its azimuthal
/// window keeps growing toward the radar (a 5×5 window counted in legacy 1°
/// radials ≈ 9 super-res).
const MEDIAN_AZ_HALF_MAX: i32 = 2;

/// Half-depth of the median kernel in range gates — deliberately deeper than it
/// is wide. Range is the axis this module does *not* differentiate, so smoothing
/// along it removes noise without touching the azimuthal shear being measured.
/// Deepening it from 1 to 2 gates took agreement with reference readouts from
/// 0.99 to 1.00 in amplitude and 0.968 to 0.972 in correlation, and pulled
/// painted density to 1.00 of the reference's averaged over five volumes.
const MEDIAN_RNG_HALF: i32 = 2;

/// Minimum RAW-data fraction of the median window for a valid centre to
/// survive. Measured on sparsity ladders: the reference NROT paints at 25%
/// ND and dies by 50% — its median footprint NDs under-populated windows,
/// cleaning sparse fold soup the raw-default dealias rule re-admits.
const MEDIAN_MIN_RAW_OCC: f64 = 0.6;

fn median_filter(
    vel_grid: &[Vec<f64>],
    raw_grid: &[Vec<f64>],
    gate_count: usize,
    first_gate_range_km: f64,
    gate_interval_km: f64,
) -> Vec<Vec<f64>> {
    let num_radials = vel_grid.len() as i32;
    let spacing_rad = (360.0 / num_radials as f64).to_radians();

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

/// The divisor curve as measured from the reference step response on a
/// synthetic volume: the knot ranges are KILOMETERS and the curve is
/// linearly interpolated between knots (25 at ≤20 km ramping to 8 at
/// 80 km, flat 8 beyond). Solving NROT·div·range·(taps) = const across an
/// 8–140 nm ladder of step-response peaks reproduces this to ±0.8.
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

/// Composite azimuthal derivative stencil, measured via synthetic-volume
/// injection: the per-bin NROT values of the reference response to a
/// ±8 m/s azimuthal velocity step are the cumulative sums of these taps.
/// Antisymmetric, dimensionless; ROT = Σ cⱼ·v(i+j) / arc. The sign-reversed
/// outer taps are what produce the small negative side lobes flanking every
/// strong gradient — a plain least-squares slope cannot produce those.
const COMPOSITE_TAPS: [f64; 5] = [0.1039, 0.1595, 0.1187, -0.0037, -0.0630];

/// The measured per-radial split-tap operator, solved exactly from
/// per-radial step-response profiles: the side toward the whole-degree pair
/// partner applies `SPLIT_CLEAN` (the legacy-grid taps ĉ = [0.580, 0.238,
/// −0.151]) at 2/3/4 super-res offsets; the side away from the partner
/// applies `SPLIT_AWAY` = [ĉ₂, ĉ₁−ĉ₂, ĉ₂, ĉ₃] at 1/2/3/4. Both sides sum to
/// ĉ₁+ĉ₂+ĉ₃, so the operator is zero-sum; normalization is the legacy 1.0°
/// arc. This is the unique zero-sum anchored linear operator reproducing
/// both measured step profiles — pair-aligned [−0.18, +0.10, +0.77×4, +0.10,
/// −0.18] and mid-pair [−0.18, +0.10, +0.49, +0.77×2, +0.49, +0.10, −0.18] —
/// per radial, and via superposition the reference's full −1.48 response to
/// an aligned synthetic 6-radial couplet where every
/// pair-average-then-convolve chain reads ~−1.1.
const SPLIT_CLEAN: [(i32, f64); 3] = [(2, 0.580), (3, 0.238), (4, -0.151)];
const SPLIT_AWAY: [(i32, f64); 4] = [(1, 0.238), (2, 0.342), (3, 0.238), (4, -0.151)];

/// Matched-filter kernel bank: one per-radial tap operator per couplet pole
/// width (2/3/4 radials), with the same (offset, tap) clean/away semantics
/// as [`SPLIT_CLEAN`]/[`SPLIT_AWAY`]. Each kernel was empirically fitted
/// (ridge least squares, profile-only — no step constraints) so that its
/// response to the ideal 3-median-filtered width-w couplet matches the
/// measured width-w couplet response profile, scaled by the primary
/// operator's own core response on the same pattern. The kernels never see
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
/// semantics, empirically fitted to the measured graded-asymmetry couplet
/// profiles — a +6/−4 pole pair (ratio 0.67) and a +6/−2 pair (ratio
/// 0.33). The reference compresses asymmetric-couplet edges harder as the
/// weak pole shrinks (edge/core 0.26 and 0.13 vs 0.34 balanced); symmetric
/// templates cannot match these patterns, so they get their own kernels
/// and templates. Footprint-only: their tap energy is too high for the
/// per-bin base cap, and their template gate is the sole notch guard (the
/// measured monopolar notch scores r² ≈ 0.2–0.3 against them, far under
/// the 0.8 floor — the balance gate, which such couplets themselves fail,
/// is deliberately not applied).
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
/// primary operator's core response on ideal patterns; hover readouts on
/// real weak couplet shoulders show the reference's compressed values run
/// below that anchor, so the cap output is recalibrated by this factor,
/// measured against a real-volume readout set.
const BANK_CAP_GAIN: f64 = 0.90;

/// Deviation-balance floor for the per-bin base cap, measured on real
/// volumes: the KMKX monopolar notch balances at 0.29, rotation couplets
/// at 0.42 and above.
const BANK_BASE_BALANCE_MIN: f64 = 0.42;

/// Deviation-balance floor for footprint candidates: opposite deviations
/// about the window median must reach this ratio. Measured on real
/// volumes: a monopolar notch the reference paints at full value balances
/// at 0.29, rotation couplets at 0.42 and above; the floor sits between,
/// nearer the notch to keep weak lopsided couplets eligible.
const BANK_BALANCE_MIN: f64 = 0.35;

/// Range limit in km for the split-tap operator; beyond it the composite
/// 11-tap stencil takes over. The split operator is measured ground truth
/// near the radar (synthetic steps at 36–41 km, a real couplet at 12.6 km);
/// the composite's 0.997 real-field agreement was earned at 70–240 km where
/// pairing phase is invisible — each kernel is used inside its measured
/// domain.
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
    // Normalize by the legacy 1.0° arc (two radials of this grid).
    Some(acc / (2.0 * arc_per_radial))
}

/// Which index phase pairs super-res radials into whole-degree legacy bins:
/// radials (2k+phase, 2k+1+phase) share a degree sector. The legacy pairing
/// is anchored to ABSOLUTE azimuth, proven with synthetic steps: a step at a
/// whole degree (az 45.0) reads clean while the same step at a half degree
/// (az 135.5) reads pair-averaged.
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

/// Range-averaged azimuthal velocity profile around (i, j): the 3-gate range
/// mean per radial offset −half..=half — the same per-radial samples the tap
/// stencils consume. NaN where a radial has no data in the range window.
fn az_profile(vel_grid: &[Vec<f64>], i: usize, j: usize, gate_count: usize, half: i32) -> Vec<f64> {
    let num_radials = vel_grid.len() as i32;
    (-half..=half)
        .map(|da| {
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
            if n > 0 { sum / n as f64 } else { f64::NAN }
        })
        .collect()
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
    let prof: Vec<f64> = prof
        .iter()
        .enumerate()
        .map(|(k, p)| p - pm - slope * (k as f64 - (n - 1.0) / 2.0))
        .collect();
    let pm = 0.0;
    let pv: f64 = prof.iter().map(|p| (p - pm).powi(2)).sum();
    if pv <= 0.0 {
        return None;
    }
    let mut best: Option<f64> = None;
    for s in -1..=1 {
        let t: Vec<f64> = (-half..=half)
            .map(|d| {
                let x = d - s;
                if (-w..0).contains(&x) {
                    1.0
                } else if (0..w).contains(&x) {
                    -neg_amp
                } else {
                    0.0
                }
            })
            .collect();
        let tm = t.iter().sum::<f64>() / n;
        let tv: f64 = t.iter().map(|x| (x - tm).powi(2)).sum();
        let cov: f64 = prof.iter().zip(&t).map(|(p, x)| (p - pm) * (x - tm)).sum();
        let r2 = cov * cov / (pv * tv);
        if best.is_none_or(|b| r2 > b) {
            best = Some(r2);
        }
    }
    best
}

/// A kernel's clean-side and away-side tap lists.
type TapPair<'a> = (&'a [(i32, f64)], &'a [(i32, f64)]);

/// One bank kernel at one bin: the same clean/away weight assembly as
/// [`split_stencil_rot`], normalized by the legacy 1.0° arc. Requires every
/// tap cell — a missing cell means the footprint bin keeps the primary
/// chain's value.
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
    let prof = az_profile(vel_grid, i, j, gate_count, span);
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
/// Measured on couplet-width ladders: the reference's width-dependent edge
/// compression (0.47/0.34/0.55 of core for pole widths 2/3/4) with
/// full-value pass-through on monopolar notches follows from this
/// selection rule, and the cap form bounds the wide kernels' noise gain by
/// the primary response on real velocity texture.
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
    let spacing_rad = (360.0 / num_radials as f64).to_radians();
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
            // Base layer: per-bin bank cap, the direct successor of the
            // second-chain magnitude cap this bank replaces. Every bin
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
                let prof = az_profile(vel_grid, i, j, sweep.gate_count, 7);
                let mut vals: Vec<f64> = prof.iter().copied().filter(|p| !p.is_nan()).collect();
                if vals.is_empty() {
                    continue;
                }
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
                // A candidate window that passes the ends-return gate (a
                // couplet's profile comes back to the background on both
                // sides; a step's does not), or None with the profile's
                // bipolar balance about its median otherwise.
                let gated_prof = |w: i32| -> Option<(Vec<f64>, f64)> {
                    let prof = az_profile(vel_grid, i, j, sweep.gate_count, w + 3);
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
                    let mut vals: Vec<f64> = prof.clone();
                    vals.sort_by(|x, y| x.partial_cmp(y).unwrap());
                    let med = vals[vals.len() / 2];
                    let dpos = vals[vals.len() - 1] - med;
                    let dneg = med - vals[0];
                    let balance = dpos.min(dneg) / dpos.max(dneg).max(1e-9);
                    Some((prof, balance))
                };
                let mut best: Option<(f64, i32, TapPair)> = None;
                for &(w, clean, away) in BANK.iter() {
                    // Bipolar balance about the window median: a monopolar
                    // notch (one deep pole, weak counter-deviation) never
                    // balances; a symmetric rotation couplet does.
                    let Some((prof, balance)) = gated_prof(w) else {
                        continue;
                    };
                    if balance < BANK_BALANCE_MIN {
                        continue;
                    }
                    if let Some(r2) = bank_template_r2(&prof, w, 1.0)
                        && r2 >= BANK_R2_MIN
                        && best.is_none_or(|(b, _, _)| r2 > b)
                    {
                        best = Some((r2, w, (clean, away)));
                    }
                }
                for &(neg_amp, clean, away) in BANK_ASYM.iter() {
                    // Asymmetric couplets fail the balance test by nature;
                    // their template gate alone carries the notch guard
                    // (the measured notch scores r² ≈ 0.2–0.3 here).
                    let w = 3;
                    let Some((prof, _)) = gated_prof(w) else {
                        continue;
                    };
                    if let Some(r2) = bank_template_r2(&prof, w, neg_amp)
                        && r2 >= BANK_R2_MIN
                        && best.is_none_or(|(b, _, _)| r2 > b)
                    {
                        best = Some((r2, w, (clean, away)));
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
    // the step-response ladder at 20–140 nm to ~5%).
    Some(acc / arc_per_radial)
}

fn llsd_nrot(sweep: &VelocitySweep, vel_grid: &[Vec<f64>]) -> Vec<Vec<f64>> {
    let num_radials = vel_grid.len();
    let avg_spacing_deg = 360.0 / num_radials as f64;
    let spacing_rad = avg_spacing_deg.to_radians();
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

/// Environmental-wind seed tolerance in m/s — deliberately tight, tuned on a
/// real folded volume against the reference kept-fraction per region.
const DA_SEED_TOL: f64 = 5.0;

/// Agreeing 4-neighbors required for a gate-level wind seed. A wind-matching
/// pocket inside storm-perturbed flow (the 50.7°/61 nm case the reference
/// paints −1.13) can never seed a 5×10 all-gates tile; gate seeds anchor it
/// at raw before any bridge can unfold it to the wrong branch.
const DA_SEEDGATE_NEIGHBORS: i32 = 3;

/// Scale on every bridge/fill threshold — the pass ordering is fixed but the
/// base thresholds are nominal; 1.4 measured as the point where dealias
/// coverage matches the reference on real volumes.
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
/// Empirically, the reference paints nothing for isolated 2×4-bin
/// distinct-velocity patches but paints 4×8 ones — its raw-default keeps
/// only regions above a size gate between 8 and 32 bins.
const DA_RAWMIN_BINS: usize = 16;

/// Censor threshold in units of Vny — measured fold-wall transfer:
/// the reference keeps a 1.25·Vny adjacent jump (a clean ±15 m/s synthetic
/// couplet at Vny 23.9 reads as rotation) and censors a 1.9·Vny fold soup,
/// so the threshold sits between; 1.2 censored real couplet cores.
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
    /// was measured worse against the RPG's own dealiased velocity (see
    /// `crate::srv`'s A/B notes; a kept fold wall is a 2·Vny error on every
    /// gate it touches, which costs more level agreement than the censored
    /// hole costs coverage).
    Coverage,
}

/// [`DealiasProfile::Coverage`]'s kept-raw region floor: keep every unreached
/// data gate, however small the region. The RPG's dealiaser resolves all
/// present data, so for a field that is *displayed* rather than
/// differentiated, matching its coverage matters more than suppressing
/// isolated pockets — the A/B against live N0G/N1G twins is recorded in
/// `crate::srv`.
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
    // of the gap's data gates fail. At the nominal per-gate thresholds a
    // fragile strict chain needs a 2.125× threshold widening to reach
    // reference coverage — and the widened radial bridge is what mis-unfolds
    // the 50.7°/61 nm pocket the reference keeps.
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
    // data gates keep their raw values in bulk: measured over sparsity and
    // amplitude ladders, the reference dealiaser resolves ALL present data,
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
    // regions meet correctly unfolded ones exactly there. The measured
    // transfer censors 1.9·Vny soup and keeps 1.25·Vny.
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
        let filtered = median_filter(&grid, &grid, gates, 0.25, 0.25);
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

    /// The NVW parser on a real KLOT VAD Wind Profile product captured from
    /// tgftp: plausible level count, heights, and winds.
    #[test]
    fn nvw_parser_reads_a_real_product() {
        let payload = include_bytes!("../testdata/klot_nvw.bin");
        let levels = parse_nvw_wind_levels(payload);
        assert!(levels.len() >= 10, "few levels: {}", levels.len());
        assert!(levels.first().unwrap().0 < 1.0, "first level should be low");
        assert!(
            levels.last().unwrap().0 > 3.0,
            "levels should reach altitude"
        );
        for (h, u, v) in &levels {
            assert!((0.0..20.0).contains(h) && u.abs() < 150.0 && v.abs() < 150.0);
        }
        assert!(WindProfile::from_levels(&levels).is_some());
    }
}
