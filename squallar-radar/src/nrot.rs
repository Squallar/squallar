//! Normalized Rotation (NROT): the azimuthal derivative of radial velocity,
//! normalized by a range-dependent divisor so one number reads the same at
//! every distance from the radar. The pipeline is reverse-engineered against
//! a reference implementation: kernel taps, divisor curve, median geometry,
//! and gating are all empirical rather than derived.

use crate::beam::RE_EFF_KM;
// rayon on every target that has threads, the sequential stand-ins on wasm32.
use crate::par::*;

const KM_PER_NM: f64 = 1.852;

/// NROT is defined on -5..+5; the divisor curve guarantees nothing, so clamp.
const NROT_LIMIT: f64 = 5.0;

/// Skip bins closer than this. Residual ground clutter close to the radar
/// produces clamp-level fake shear (adjacent ±30 m/s bins over tens of meters
/// of arc).
const MIN_RANGE_NM: f64 = 7.05;

/// The magnitude at which this algorithm considers a bin **painted** — the
/// significance floor of the whole product, and the one number every consumer
/// of an NROT field has to agree with.
pub const SIGNIFICANT: f64 = 0.25;

/// Blank painted clusters (8-connected runs of |NROT| ≥ [`SIGNIFICANT`])
/// smaller than this many bins. Empirical, chosen to match the reference's
/// painted density.
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
    /// Where this sweep's cut **declared** its velocity folds, m/s, or `None`
    /// when the volume declared nothing for it.
    pub declared_nyquist_ms: Option<f64>,
    /// Why each cell of [`vel_grid`](Self::vel_grid) is `NaN`, at the same
    /// `(radial, gate)` indices — [`crate::velocity::VelocityGrid::status`],
    /// borrowed.
    pub status: Option<&'a [Vec<crate::types::GateReport>]>,
}

/// How this sweep's rows sit in azimuth: the step every stencil's
/// `arc_per_radial` is built from — and so the scale of every NROT value this
/// module reports — together with whether row `n−1` borders row 0.
fn sweep_rows(sweep: &VelocitySweep, num_radials: usize) -> crate::azimuth::Rows {
    crate::azimuth::Rows::of(sweep.azimuths_deg, num_radials)
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
    let pre = preprocess_velocity_with(sweep, elevation_deg, profile);
    let mut grid = llsd_nrot(sweep, &pre.dealiased, &pre.median, pre.refused.as_deref());
    despeckle_nrot(
        &mut grid,
        DESPECKLE_MIN_BINS,
        sweep_rows(sweep, sweep.vel_grid.len()),
    );
    grid
}

/// Blank painted clusters smaller than `min_bins`: 8-connected components of
/// |NROT| ≥ [`SIGNIFICANT`] (either sign — a tiny dipole is still speckle).
/// Azimuth wraps where the sweep closes the circle; range never does.
fn despeckle_nrot(grid: &mut [Vec<f64>], min_bins: usize, rows: crate::azimuth::Rows) {
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
                    // Two clusters at the two ends of a sector are two
                    // clusters, each counted against `min_bins` on its own.
                    let Some(ii) = rows.neighbour(i, di) else {
                        continue;
                    };
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
            if comp.len() < min_bins || jmax == jmin {
                for &(i, j) in &comp {
                    grid[i][j] = f64::NAN;
                }
            }
        }
    }
}

/// Everything step 1 and step 2 produce that [`llsd_nrot`] then reads.
struct Preprocessed {
    /// The dealiased field, which the continuity ceiling is measured on.
    dealiased: Vec<Vec<f64>>,
    /// The median-filtered field, which the stencils differentiate.
    median: Vec<Vec<f64>>,
    /// The incoherence mask the dealiasing set aside, or `None` from a
    /// dealiasing that produced none — see [`dealias_with_knobs`] for which is
    /// which, and why the difference matters.
    refused: Option<Vec<bool>>,
}

fn preprocess_velocity_with(
    sweep: &VelocitySweep,
    elevation_deg: f64,
    profile: Option<&WindProfile>,
) -> Preprocessed {
    let mut vel: Vec<Vec<f64>> = sweep.vel_grid.to_vec();
    let refused = dealias(
        &mut vel,
        sweep,
        elevation_deg,
        profile,
        DealiasProfile::NoFalseShear,
    );
    let med = median_filter(
        &vel,
        sweep.vel_grid,
        sweep.status,
        sweep.gate_count,
        sweep.first_gate_range_km,
        sweep.gate_interval_km,
        sweep_rows(sweep, sweep.vel_grid.len()),
    );
    Preprocessed {
        dealiased: vel,
        median: med,
        refused,
    }
}

/// Wind-profile layer thickness, km.
const PROFILE_LAYER_KM: f64 = 0.3;
/// Layers span 0..12 km AGL.
const PROFILE_LAYERS: usize = 40;

/// How far an unfitted layer may be filled from the nearest fitted one, in
/// layers — 3, so **0.9 km** at [`PROFILE_LAYER_KM`].
const PROFILE_FILL_MAX_LAYERS: i64 = 3;
/// Sample cap per layer keeps memory bounded on wasm. A volume offers far
/// more than this: KCRP 2017-08-26 04:41:14 has 326 657 gates to give the
/// twenty layers under 6 km, and its lowest layer alone is offered more than
/// the cap within the first two of its fifteen cuts.
const PROFILE_MAX_SAMPLES: usize = 16384;

/// Largest RMS fit residual, m/s, a layer may carry and still be published as
/// a wind — the RPG's own goodness-of-fit ceiling, converted.
const PROFILE_MAX_RMS_MS: f64 = 9.7 * 0.514_444;

/// Residual, m/s, past which a sample is dropped from the second of
/// [`WindProfileBuilder::finish`]'s two fit passes — the robust-regression trim
/// that keeps folded bins in a raw sweep from dragging the layer's wind.
const PROFILE_TRIM_MS: f64 = 12.0;

/// Horizontal wind fitted per height layer from every velocity tilt of a
/// volume: vr ≈ u·sin(az)·cos(el) + v·cos(az)·cos(el) + c.
pub struct WindProfile {
    /// (u, v, c) per layer; NaN-filled layers had too little data.
    layers: Vec<Option<(f64, f64, f64)>>,
}

impl WindProfile {
    /// Build from explicit (height km, u, v) levels. The render path fits
    /// its profile from the volume ([`WindProfileBuilder`]); this constructor
    /// exists for callers that already hold levels — tests, mostly.
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
                    && (f as i64 - l as i64).abs() <= PROFILE_FILL_MAX_LAYERS
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
#[inline]
fn height_km_with_sin_el(range_km: f64, sin_el: f64) -> f64 {
    range_km * sin_el + range_km * range_km / (2.0 * RE_EFF_KM)
}

/// Accumulates VAD samples per height layer across the volume's velocity
/// tilts, then fits each layer with one trimmed re-fit so folded bins in the
/// raw (not-yet-dealiased) sweeps cannot drag the wind estimate.
#[derive(Default)]
pub struct WindProfileBuilder {
    samples: Vec<Layer>,
}

/// One height layer's accumulated VAD samples, thinned to
/// [`PROFILE_MAX_SAMPLES`] as they arrive.
struct Layer {
    /// (sin·cosθ, cos·cosθ, vr).
    pts: Vec<(f64, f64, f64)>,
    /// Samples offered so far, counted so `stride` can be applied to them.
    offered: usize,
    /// One offer in this many is kept. Doubles each time the layer fills.
    stride: usize,
}

impl Default for Layer {
    fn default() -> Self {
        Self {
            pts: Vec::new(),
            offered: 0,
            stride: 1,
        }
    }
}

impl WindProfileBuilder {
    pub fn new() -> Self {
        Self {
            samples: (0..PROFILE_LAYERS).map(|_| Layer::default()).collect(),
        }
    }

    /// Hand one sample to a layer, keeping one offer in `stride` and halving
    /// the layer whenever it fills.
    fn offer(&mut self, l: usize, sample: (f64, f64, f64)) {
        let layer = &mut self.samples[l];
        let keep = layer.offered.is_multiple_of(layer.stride);
        layer.offered += 1;
        if !keep {
            return;
        }
        layer.pts.push(sample);
        if layer.pts.len() == PROFILE_MAX_SAMPLES {
            layer.pts = layer.pts.iter().step_by(2).copied().collect();
            layer.stride *= 2;
        }
    }

    pub fn add_sweep(&mut self, sweep: &VelocitySweep, elevation_deg: f64) {
        let el = elevation_deg.to_radians();
        let (sin_el, cos_el) = (el.sin(), el.cos());
        for (row, &az_deg) in sweep.vel_grid.iter().zip(sweep.azimuths_deg) {
            let az = az_deg.to_radians();
            let (s, c) = (az.sin() * cos_el, az.cos() * cos_el);
            // Every 3rd gate is plenty for a 3-parameter fit per layer.
            for (j, v) in row.iter().enumerate().step_by(3) {
                if v.is_nan() {
                    continue;
                }
                let r = sweep.first_gate_range_km + j as f64 * sweep.gate_interval_km;
                let h = height_km_with_sin_el(r, sin_el);
                let l = (h / PROFILE_LAYER_KM) as usize;
                if l < PROFILE_LAYERS {
                    self.offer(l, (s, c, *v));
                }
            }
        }
    }

    pub fn finish(self) -> Option<WindProfile> {
        let mut any = false;
        let layers = self
            .samples
            .iter()
            .map(|layer| {
                let pts = &layer.pts;
                let mut fit: Option<(f64, f64, f64)> = None;
                for _ in 0..2 {
                    let mut m = [[0.0f64; 3]; 3];
                    let mut b = [0.0f64; 3];
                    let mut n = 0u32;
                    for &(s, c, v) in pts {
                        if let Some((u, w, cc)) = fit
                            && (u * s + w * c + cc - v).abs() > PROFILE_TRIM_MS
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
                // The gate the RPG publishes and this did not have.
                if let Some((u, w, cc)) = fit {
                    let (mut sq, mut n) = (0.0f64, 0u32);
                    for &(s, c, v) in pts {
                        let r = u * s + w * c + cc - v;
                        if r.abs() > PROFILE_TRIM_MS {
                            continue;
                        }
                        sq += r * r;
                        n += 1;
                    }
                    if n == 0 || (sq / f64::from(n)).sqrt() > PROFILE_MAX_RMS_MS {
                        fit = None;
                    }
                }
                if fit.is_some() {
                    any = true;
                }
                fit
            })
            .collect();
        let mut layers: Vec<Option<(f64, f64, f64)>> = layers;
        let filled: Vec<usize> = (0..layers.len()).filter(|&l| layers[l].is_some()).collect();
        for l in 0..layers.len() {
            if layers[l].is_none()
                && let Some(&f) = filled
                    .iter()
                    .min_by_key(|&&f| (f as i64 - l as i64).unsigned_abs())
                && (f as i64 - l as i64).abs() <= PROFILE_FILL_MAX_LAYERS
            {
                layers[l] = layers[f];
            }
        }
        any.then_some(WindProfile { layers })
    }
}

/// The fold limit read **off the data**: the largest speed the sweep observed.
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

/// Cap on the median filter's azimuthal half-count. It is what stops
/// [`MEDIAN_HALF_WIDTH_KM`] from scaling: four-tenths of a kilometre of arc
/// is three rows at 9 nm and four at 7 nm, and this holds both at two.
const MEDIAN_AZ_HALF_MAX: i32 = 2;

/// Half-depth of the median kernel in range gates — deliberately deeper than it
/// is wide. Range is the axis this module does *not* differentiate, so smoothing
/// along it removes noise without touching the azimuthal shear being measured.
const MEDIAN_RNG_HALF: i32 = 2;

/// Minimum fraction of the median window that must carry **echo** — a gate the
/// radar returned a number for — for a valid centre to survive: the reference
/// NDs under-populated windows, cleaning sparse fold soup the raw-default
/// dealias rule re-admits.
const MEDIAN_MIN_RAW_OCC: f64 = 0.6;

/// Minimum fraction of the median window that must still **carry a dealiased
/// value** for a median to be reported at all.
const MEDIAN_MIN_DEALIASED_OCC: f64 = 0.37;

/// The coverage rule this filter is often blamed for costs nothing where it is
/// blamed, and the rule it does **not** have is what costs.
fn median_filter(
    vel_grid: &[Vec<f64>],
    raw_grid: &[Vec<f64>],
    raw_status: Option<&[Vec<crate::types::GateReport>]>,
    gate_count: usize,
    first_gate_range_km: f64,
    gate_interval_km: f64,
    rows: crate::azimuth::Rows,
) -> Vec<Vec<f64>> {
    let num_radials = vel_grid.len();
    let spacing_rad = rows.step_deg.to_radians();

    (0..num_radials)
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
                        let Some(ai) = rows.neighbour(i, da) else {
                            continue;
                        };
                        for dr in -MEDIAN_RNG_HALF..=MEDIAN_RNG_HALF {
                            let rj = j as i32 + dr;
                            if rj < 0 || rj >= gate_count as i32 {
                                continue;
                            }
                            slots += 1;
                            // **Echo**, asked of the report plane by name.
                            let carries_echo = match raw_status {
                                Some(st) => st[ai][rj as usize] == crate::types::GateReport::Value,
                                None => !raw_grid[ai][rj as usize].is_nan(),
                            };
                            if carries_echo {
                                raw_occ += 1;
                            }
                            let v = vel_grid[ai][rj as usize];
                            if !v.is_nan() {
                                window.push(v);
                            }
                        }
                    }
                    if (raw_occ as f64) < MEDIAN_MIN_RAW_OCC * slots as f64 {
                        return f64::NAN;
                    }
                    if (window.len() as f64) < MEDIAN_MIN_DEALIASED_OCC * slots as f64 {
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

/// The divisor curve: knot ranges in KILOMETRES, linearly interpolated, flat
/// outside the knots. These are the reference's own values, read off it
/// directly rather than fitted to it.
fn rot_divisor_km(range_km: f64) -> f64 {
    const KNOTS: [(f64, f64); 18] = [
        (13.1, 22.43),
        (16.0, 22.97),
        (19.0, 23.60),
        (22.0, 23.97),
        (26.0, 23.40),
        (30.0, 22.69),
        (35.0, 21.69),
        (40.0, 20.57),
        (45.0, 19.06),
        (50.0, 17.16),
        (55.0, 15.03),
        (60.0, 12.93),
        (65.0, 11.64),
        (70.0, 10.67),
        (75.0, 9.65),
        (80.0, 8.62),
        (81.5, 8.31),
        (85.0, 8.23),
    ];
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

/// The per-radial operator on a 0.5°-spaced sweep: four taps t₁..t₄
/// at row offsets 1/2/3/4, applied **antisymmetrically** — the same list on
/// both sides of the bin, positive toward increasing azimuth.
const SPLIT_TAPS: [(i32, f64); 4] = [(1, 0.2241), (2, 0.3433), (3, 0.2526), (4, -0.1530)];

/// The operator for a sweep that is *already* legacy resolution — a TDWR cut,
/// or a WSR-88D tilt above the super-res ones. Antisymmetric, at row offsets
/// ±1 and ±2 only, normalized by **one** row: ROT = Σ tₖ(v(i+k) − v(i−k)) /
/// arc_per_radial.
const LEGACY_TAPS: [(i32, f64); 2] = [(1, 0.6969), (2, -0.0813)];

/// Range half-depth in gates for the stencils' 3-gate range means, per
/// Smith/Elmore's "3 range gates deep" — deeper smooths small features in
/// range and reads them low.
const STENCIL_RNG_HALF: i32 = 1;

/// Coherence floor for both stencils: squared correlation between the
/// velocity profile and the stencil's ramp response; constant or incoherent
/// profiles read ND, matching the reference's ND bins over good velocity.
const GK_MIN_R2: f64 = 0.01;

/// Extra valid radials required beyond the split stencil's ±4 span on each
/// side.
const GK_DATA_MARGIN: i32 = 1;

/// Range-continuity ceiling. Rotation is reported only over velocity the radar
/// measured continuously along the beam: [`range_texture`] must stay under
/// this multiple of the cut's own fold limit.
const GK_MAX_TEXTURE_VNY_FRAC: f64 = 0.44;

/// One antisymmetric tap list applied to an azimuthal profile: Σ tₖ·(v(i+k) −
/// v(i−k)), returned **unnormalized** — the caller divides by the arc its own
/// taps are anchored on, which is the only thing that separates the two
/// operators here.
fn tap_stencil(prof: &[f64], taps: &[(i32, f64)]) -> Option<f64> {
    const C: usize = PROFILE_MAX_HALF;
    // Data-margin completeness: the outermost cells must be populated too, so
    // bins do not appear at echo edges where the profile is half real.
    for m in 0..GK_DATA_MARGIN {
        let o = C - m as usize;
        if prof[C + o].is_nan() || prof[C - o].is_nan() {
            return None;
        }
    }
    // Signed weight per profile cell: one tap list, mirrored — positive
    // toward increasing azimuth, negative away from it.
    let mut w = [0.0f64; 2 * PROFILE_MAX_HALF + 1];
    for &(o, t) in taps {
        w[(C as i32 + o) as usize] += t;
        w[(C as i32 - o) as usize] -= t;
    }
    let (mut acc, mut mean, mut nv) = (0.0, 0.0, 0i32);
    for (k, &wk) in w.iter().enumerate() {
        if wk == 0.0 {
            continue;
        }
        let v = prof[k];
        if v.is_nan() {
            return None;
        }
        acc += wk * v;
        mean += v;
        nv += 1;
    }
    // Coherence gate: squared correlation between the velocity profile and the
    // stencil weights. Constant profiles have zero variance — ND.
    mean /= f64::from(nv);
    let (mut svv, mut scc) = (0.0, 0.0);
    for (k, &wk) in w.iter().enumerate() {
        if wk == 0.0 {
            continue;
        }
        svv += (prof[k] - mean).powi(2);
        scc += wk * wk;
    }
    if svv <= 0.0 || acc * acc / (scc * svv) < GK_MIN_R2 {
        return None;
    }
    Some(acc)
}

/// The super-res operator ([`SPLIT_TAPS`]) at one bin, at **every** range.
fn split_stencil_rot(
    vel_grid: &[Vec<f64>],
    i: usize,
    j: usize,
    arc_per_radial: f64,
    gate_count: usize,
    rows: crate::azimuth::Rows,
) -> Option<f64> {
    let mut buf = EMPTY_PROFILE;
    // Off the end of a sector's arc a cell comes back NaN, and the
    // completeness rules read that as the data edge it is.
    let prof = az_profile(
        &mut buf,
        vel_grid,
        i,
        j,
        gate_count,
        PROFILE_MAX_HALF as i32,
        rows,
    );
    let acc = tap_stencil(prof, &SPLIT_TAPS)?;
    Some(acc / (2.0 * arc_per_radial))
}

/// [`LEGACY_TAPS`] at one bin, for a sweep whose rows are already whole
/// degrees. Same profile, same completeness rule and same coherence floor as
/// [`split_stencil_rot`] — so which bins get a value is unchanged and only the
/// value changes — but symmetric, and normalized by one row rather than two.
fn legacy_stencil_rot(
    vel_grid: &[Vec<f64>],
    i: usize,
    j: usize,
    arc_per_radial: f64,
    gate_count: usize,
    rows: crate::azimuth::Rows,
) -> Option<f64> {
    let mut buf = EMPTY_PROFILE;
    // The same span the split operator reads, so the data-margin rule tests the
    // same cells and neither operator paints an echo edge the other would not.
    let prof = az_profile(
        &mut buf,
        vel_grid,
        i,
        j,
        gate_count,
        PROFILE_MAX_HALF as i32,
        rows,
    );
    let acc = tap_stencil(prof, &LEGACY_TAPS)?;
    Some(acc / arc_per_radial)
}

/// Whether this sweep's rows pair into whole-degree legacy bins: radials
/// (2k, 2k+1) — or (2k+1, 2k+2) — sharing a degree sector. The pairing is
/// anchored to ABSOLUTE azimuth, not to collection order: a super-res cut's
/// radial centres sit at x.21/x.71 and the two sharing a floor are the pair.
fn rows_are_half_degree_pairs(azimuths_deg: &[f64]) -> bool {
    let n = azimuths_deg.len();
    if n < 4 {
        return false;
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
    // A real pairing accounts for *most* of the sweep: on a 0.5° grid one
    // alignment puts every pair inside a degree and the other puts none.
    2 * cohabit(0).max(cohabit(1)) > n / 2
}

/// The azimuthal half-span every profile reader asks for: the widest stencil's
/// own ±4 plus [`GK_DATA_MARGIN`]. Both operators here demand exactly this and
/// [`az_profile`] is asked for exactly this, which is what makes which bins get
/// a value independent of which tap list reads them.
const PROFILE_MAX_HALF: usize = 4 + GK_DATA_MARGIN as usize;

/// Backing store for one [`az_profile`], sized for [`PROFILE_MAX_HALF`].
type ProfileBuf = [f64; 2 * PROFILE_MAX_HALF + 1];

/// An empty [`ProfileBuf`], for a caller about to hand it to [`az_profile`].
const EMPTY_PROFILE: ProfileBuf = [f64::NAN; 2 * PROFILE_MAX_HALF + 1];

/// Range-averaged azimuthal velocity profile around (i, j): the 3-gate range
/// mean per radial offset −half..=half — the same per-radial samples the tap
/// stencils consume. NaN where a radial has no data in the range window.
fn az_profile<'p>(
    out: &'p mut ProfileBuf,
    vel_grid: &[Vec<f64>],
    i: usize,
    j: usize,
    gate_count: usize,
    half: i32,
    rows: crate::azimuth::Rows,
) -> &'p [f64] {
    let len = 2 * half as usize + 1;
    let slot = &mut out[..len];
    for (idx, cell) in slot.iter_mut().enumerate() {
        let da = idx as i32 - half;
        let Some(ai) = rows.neighbour(i, da) else {
            *cell = f64::NAN;
            continue;
        };
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

/// Half-depth in km of the range window [`range_texture`] reads. Wide enough
/// that a bin's own gate does not decide the question and narrow enough that
/// the window is still one feature.
const TEXTURE_RANGE_HALF_KM: f64 = 1.0;

/// Separation in km at which [`range_texture`] differences velocity along the
/// beam. A physical distance rather than a gate count, because a super-res
/// velocity cut repeats one estimate over two 0.25 km gates and adjacent-gate
/// differences there are structurally zero half the time.
const TEXTURE_STEP_KM: f64 = 0.5;

/// Azimuthal half-span of the texture window, in rows: the widest span any
/// stencil here reads, so the question asked is about the whole neighbourhood
/// the estimator differentiates and not about one row of it.
const TEXTURE_AZ_HALF: i32 = 5;

/// Pairs required before the window has an answer.
const TEXTURE_MIN_PAIRS: usize = 8;

/// Rms velocity difference along the beam, over the neighbourhood each stencil
/// reads. Root-mean-square of `v(r + `[`TEXTURE_STEP_KM`]`) − v(r)` across
/// ±[`TEXTURE_RANGE_HALF_KM`] in range and ±[`TEXTURE_AZ_HALF`] rows, NaN
/// where the window holds fewer than [`TEXTURE_MIN_PAIRS`] pairs.
fn range_texture(
    grid: &[Vec<f64>],
    sweep: &VelocitySweep,
    rows: crate::azimuth::Rows,
) -> Vec<Vec<f64>> {
    let n = grid.len();
    let gc = sweep.gate_count;
    if gc == 0 {
        return vec![Vec::new(); n];
    }
    let dk = ((TEXTURE_STEP_KM / sweep.gate_interval_km).round() as usize).max(1);
    let gh = ((TEXTURE_RANGE_HALF_KM / sweep.gate_interval_km).round() as i32).max(1);
    // Per row, the squared difference at `dk` and its running window sum, so
    // the azimuthal pass below adds rows rather than rewalking gates.
    let mut sum = vec![0.0f32; n * gc];
    let mut cnt = vec![0u16; n * gc];
    sum.par_chunks_mut(gc)
        .zip(cnt.par_chunks_mut(gc))
        .enumerate()
        .for_each_init(
            || (vec![0.0f64; gc + 1], vec![0u32; gc + 1]),
            |(pre, pcn), (i, (sum_row, cnt_row))| {
                for j in 0..gc {
                    let (mut d2, mut ok) = (0.0f64, 0u32);
                    if j + dk < gc {
                        let (a, b) = (grid[i][j], grid[i][j + dk]);
                        if !a.is_nan() && !b.is_nan() {
                            d2 = (b - a).powi(2);
                            ok = 1;
                        }
                    }
                    pre[j + 1] = pre[j] + d2;
                    pcn[j + 1] = pcn[j] + ok;
                }
                for j in 0..gc {
                    let lo = (j as i32 - gh).max(0) as usize;
                    let hi = ((j as i32 + gh) as usize).min(gc - 1);
                    sum_row[j] = (pre[hi + 1] - pre[lo]) as f32;
                    cnt_row[j] = (pcn[hi + 1] - pcn[lo]) as u16;
                }
            },
        );
    (0..n)
        .into_par_iter()
        .map(|i| {
            (0..gc)
                .map(|j| {
                    let (mut s, mut c) = (0.0f64, 0u32);
                    for da in -TEXTURE_AZ_HALF..=TEXTURE_AZ_HALF {
                        if let Some(ai) = rows.neighbour(i, da) {
                            s += f64::from(sum[ai * gc + j]);
                            c += u32::from(cnt[ai * gc + j]);
                        }
                    }
                    if (c as usize) < TEXTURE_MIN_PAIRS {
                        f64::NAN
                    } else {
                        (s / c as f64).sqrt()
                    }
                })
                .collect()
        })
        .collect()
}

/// `refused` is the incoherence mask [`dealias_with_knobs`] already built for
/// this sweep, or `None` from a dealiasing that built none. See
/// [`incoherent_velocity`] for why the two are the same mask, and
/// [`preprocess_velocity_with`] for how it gets here.
fn llsd_nrot(
    sweep: &VelocitySweep,
    dealiased: &[Vec<f64>],
    vel_grid: &[Vec<f64>],
    refused: Option<&[bool]>,
) -> Vec<Vec<f64>> {
    let num_radials = vel_grid.len();
    let gc = sweep.gate_count;
    let rows = sweep_rows(sweep, num_radials);
    let spacing_rad = rows.step_deg.to_radians();
    let half_degree_rows = rows_are_half_degree_pairs(sweep.azimuths_deg);
    // The cut's own limit, read off the raw sweep: after the dealiaser has
    // run, a grid no longer folds at it.
    let limit = fold_limit_ms(sweep, sweep.vel_grid);
    // The ceiling and the field it is applied to are one value, so the stage
    // cannot run where nothing reads it.
    let texture = limit.map(|v| {
        (
            GK_MAX_TEXTURE_VNY_FRAC * v,
            range_texture(dealiased, sweep, rows),
        )
    });
    let fallback: Option<Vec<bool>> = match (refused, limit) {
        (None, Some(v)) => Some(incoherent_velocity(
            sweep.vel_grid,
            rows,
            gc,
            sweep.gate_interval_km,
            v,
        )),
        _ => None,
    };
    let incoherent: Option<&[bool]> = refused.or(fallback.as_deref());

    (0..num_radials)
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
                    if incoherent.is_some_and(|m| m[i * gc + j]) {
                        return f64::NAN;
                    }
                    // Rotation is only reported over velocity the radar
                    // measured continuously along the beam.
                    if let Some((max, tex)) = &texture
                        && tex[i][j] > *max
                    {
                        return f64::NAN;
                    }

                    let arc_per_radial = range_km * spacing_rad;
                    // The operator is chosen by the sweep's own row spacing and
                    // by nothing else — not by range.
                    let op = if half_degree_rows {
                        split_stencil_rot
                    } else {
                        legacy_stencil_rot
                    };
                    let rot = op(vel_grid, i, j, arc_per_radial, sweep.gate_count, rows);
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
        .collect()
}

// ————————————————————————————————————————————————————————————————————
// Step 1: dealiaser — a validity-marking multi-pass. Gates start invalid;
// environmental-wind and zero-isodop seeds mark the first valid gates;
// bridge and flood-fill passes propagate validity; unreached data keeps raw
// in bulk (measured), the rest is converted to ND, and residual fold walls
// are censored.
// ————————————————————————————————————————————————————————————————————

/// Environmental-wind seed tolerance in m/s — deliberately tight; empirical,
/// tuned against the reference's kept fraction on folded volumes.
const DA_SEED_TOL: f64 = 5.0;

/// Agreeing 4-neighbors required for a gate-level wind seed. A wind-matching
/// pocket inside storm-perturbed flow can never seed a 5×10 all-gates tile;
/// gate seeds anchor it at raw before any bridge can unfold it to the wrong
/// branch.
const DA_SEEDGATE_NEIGHBORS: i32 = 3;

/// Scale on every bridge/fill threshold — the pass ordering is fixed but the
/// base thresholds are nominal; the scale is empirical, set where dealias
/// coverage matches the reference.
const DA_THRESH_SCALE: f64 = 1.4;

/// Iteration cap for the pass loop. **The loop converges; ten does not reach
/// it**, on 26 of the 72 velocity tilts of the five-volume storm corpus.
const DA_PASSES: i32 = 10;

/// Raw-continuity flood-fill threshold as a Vny fraction. The aliased flood
/// runs at a much lower threshold than the raw flood — raw acceptance does
/// no interval testing, so a high value cannot cause wrong-branch unfolds.
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
/// measured size gate this value sits inside.
const DA_RAWMIN_BINS: usize = 16;

/// Censor threshold in units of Vny: the jump between 4-neighbours above
/// which the pair is a residual fold wall rather than shear, and both bins go.
const CENSOR_VNY_FRAC: f64 = 1.80;

/// The posture [`dealias`] takes towards data its passes could not settle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DealiasProfile {
    /// NROT's tuned posture: velocity with no coherent solution comes back
    /// exactly as reported, unreached-data regions under [`DA_RAWMIN_BINS`]
    /// bins go ND, and any bin more than [`CENSOR_VNY_FRAC`]·Vny from a
    /// 4-neighbour is censored as a residual fold wall.
    NoFalseShear,
    Coverage,
}

/// [`DealiasProfile::Coverage`]'s kept-raw region floor: keep every unreached
/// data gate, however small the region.
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
    /// Hand back velocity with no coherent solution
    /// ([`incoherent_velocity`]) exactly as the radar reported it, rather
    /// than unfolding, censoring or dropping any of it.
    pub refuse_incoherent: bool,
    /// Run [`region_assign`] between two rounds of the propagation passes.
    /// [`RegionKnobs::SHIPPED`] in both profiles since the composition was
    /// measured against the RPG's own field; that constant carries the table.
    pub region: Option<RegionKnobs>,
}

impl DealiasProfile {
    pub(crate) fn knobs(self) -> DealiasKnobs {
        match self {
            DealiasProfile::NoFalseShear => DealiasKnobs {
                rawmin_bins: DA_RAWMIN_BINS,
                censor_vny_frac: CENSOR_VNY_FRAC,
                refuse_incoherent: true,
                region: Some(RegionKnobs::SHIPPED),
            },
            DealiasProfile::Coverage => DealiasKnobs {
                rawmin_bins: COVERAGE_RAWMIN_BINS,
                censor_vny_frac: CENSOR_VNY_FRAC,
                refuse_incoherent: false,
                region: Some(RegionKnobs::SHIPPED),
            },
        }
    }
}

/// Half a circle, counted in rows of this grid — the offset from a radial to
/// the one facing it, which the zero-isodop seed pairs a near-zero gate
/// against.
fn half_turn_rows(rows: crate::azimuth::Rows) -> i32 {
    if rows.closed {
        (rows.count / 2) as i32
    } else {
        (180.0 / rows.step_deg).round() as i32
    }
}

/// Returns what [`dealias_with_knobs`] returns: the incoherence mask this
/// dealiasing set aside, for the one caller that refuses the same ground
/// again. A caller that only wants the grid drops it — [`crate::srv`] does,
/// and under [`DealiasProfile::Coverage`] there is nothing to drop.
pub(crate) fn dealias(
    vel_grid: &mut [Vec<f64>],
    sweep: &VelocitySweep,
    elevation_deg: f64,
    profile: Option<&WindProfile>,
    dealias_profile: DealiasProfile,
) -> Option<Vec<bool>> {
    dealias_with_knobs(
        vel_grid,
        sweep,
        elevation_deg,
        profile,
        dealias_profile.knobs(),
    )
}

/// The dealias the VAD's refit runs, with **region assignment deliberately
/// off**.
pub(crate) fn dealias_for_refit(
    vel_grid: &mut [Vec<f64>],
    sweep: &VelocitySweep,
    elevation_deg: f64,
    profile: Option<&WindProfile>,
) -> Option<Vec<bool>> {
    dealias_with_knobs(
        vel_grid,
        sweep,
        elevation_deg,
        profile,
        DealiasKnobs {
            region: None,
            ..DealiasProfile::Coverage.knobs()
        },
    )
}

/// The fold limit this sweep is dealiased against, m/s: what the RDA declared,
/// or what the data shows when it declared nothing. `None` abandons the pass.
fn fold_limit_ms(sweep: &VelocitySweep, vel_grid: &[Vec<f64>]) -> Option<f64> {
    let floor = crate::sampler::FOLD_LIMIT_FLOOR_MS;
    match sweep.declared_nyquist_ms {
        Some(declared) if declared >= floor => Some(declared),
        _ => {
            let estimated = estimate_nyquist(vel_grid);
            (estimated >= floor).then_some(estimated)
        }
    }
}

/// Along-beam difference above which two gates [`TEXTURE_STEP_KM`] apart
/// **straddle** — cannot be read as one continuous velocity — as a fraction of
/// the cut's own limit.
const COH_STRADDLE_VNY_FRAC: f64 = 0.90;

/// Along-beam difference **above** which the pair is a fold rather than a
/// straddle, as a fraction of the cut's own limit.
const COH_FOLD_VNY_FRAC: f64 = 1.30;

/// Fraction of a neighbourhood's along-beam pairs allowed to straddle before
/// the sweep is held to carry no coherent velocity there.
const COH_MAX_STRADDLE: f64 = 0.039;

/// The neighbourhood the straddling fraction is counted over: half-spans in
/// rows and in gates.
const COH_AZ_HALF: i32 = 16;
const COH_RANGE_HALF: i32 = 192;

/// Where the sweep carries velocity that has no coherent solution.
fn incoherent_velocity(
    raw: &[Vec<f64>],
    rows: crate::azimuth::Rows,
    gc: usize,
    gate_interval_km: f64,
    nyquist: f64,
) -> Vec<bool> {
    let n = raw.len();
    if gc == 0 {
        return Vec::new();
    }
    let tol = COH_STRADDLE_VNY_FRAC * nyquist;
    let fold = COH_FOLD_VNY_FRAC * nyquist;
    let dk = ((TEXTURE_STEP_KM / gate_interval_km).round() as usize).max(1);
    let mut straddling = vec![0u16; n * gc];
    let mut present = vec![0u16; n * gc];
    straddling
        .par_chunks_mut(gc)
        .zip(present.par_chunks_mut(gc))
        .enumerate()
        .for_each_init(
            || (vec![0u32; gc + 1], vec![0u32; gc + 1]),
            |(ps, pp), (i, (straddling_row, present_row))| {
                for j in 0..gc {
                    let (mut s, mut p) = (0u32, 0u32);
                    if j + dk < gc {
                        let (a, b) = (raw[i][j], raw[i][j + dk]);
                        if !a.is_nan() && !b.is_nan() {
                            p = 1;
                            let dv = (b - a).abs();
                            s = u32::from(dv > tol && dv < fold);
                        }
                    }
                    ps[j + 1] = ps[j] + s;
                    pp[j + 1] = pp[j] + p;
                }
                for j in 0..gc {
                    let lo = (j as i32 - COH_RANGE_HALF).max(0) as usize;
                    let hi = ((j as i32 + COH_RANGE_HALF) as usize).min(gc - 1);
                    straddling_row[j] = (ps[hi + 1] - ps[lo]) as u16;
                    present_row[j] = (pp[hi + 1] - pp[lo]) as u16;
                }
            },
        );
    let mut refused = vec![false; n * gc];
    refused
        .par_chunks_mut(gc)
        .enumerate()
        .for_each(|(i, refused_row)| {
            for (j, out) in refused_row.iter_mut().enumerate() {
                if raw[i][j].is_nan() {
                    continue;
                }
                let (mut s, mut p) = (0u32, 0u32);
                for da in -COH_AZ_HALF..=COH_AZ_HALF {
                    // Past the end of a sector's arc there is no row to count,
                    // exactly as there is no gate past the last.
                    if let Some(ai) = rows.neighbour(i, da) {
                        s += u32::from(straddling[ai * gc + j]);
                        p += u32::from(present[ai * gc + j]);
                    }
                }
                *out = p > 0 && (s as f64) > COH_MAX_STRADDLE * p as f64;
            }
        });
    refused
}

/// An experimental arm, off in both shipped [`DealiasProfile`]s: assign a fold
/// branch to a whole **region** rather than to a gate near a wall.
#[derive(Debug, Clone, Copy)]
pub struct RegionKnobs {
    /// Sub-intervals the Nyquist interval is split into for region finding.
    /// 3 is `dealias_region_based`'s default and its documented compromise.
    pub splits: usize,
    /// Gates below which a connected component is speckle: no branch, no edge.
    pub region_min: usize,
    /// Gate pairs an edge needs before its mean is evidence of anything.
    pub min_pairs: usize,
    /// Integrality: `|mean/interval − round(mean/interval)|` at most this. A
    /// boundary that lands halfway between two branches has named neither.
    pub int_tol: f64,
    /// Per-pair agreement radius about the edge's branch, in units of Vny.
    pub pair_tol: f64,
    /// Fraction of an edge's own gate pairs that must fall inside `pair_tol`.
    pub agree_frac: f64,
    /// Fraction of a component's cycle-closing edges that may contradict
    /// before the whole component is refused.
    pub contra_frac: f64,
    /// Anchor votes a component needs before its evidence counts.
    pub anchor_min: usize,
    /// Fraction of those votes the winning branch must hold.
    pub anchor_frac: f64,
    /// Fall back to "the component's largest region is branch 0" when it has
    /// no anchor evidence at all — `centered`'s prior, taken per component.
    /// Off is the strict posture: no evidence, no claim.
    pub anchor_largest: bool,
    /// Run between the seeds and the propagation passes rather than after
    /// them.
    pub before_passes: bool,
    /// Replace a branch a pass already placed when the region disagrees.
    /// Irrelevant when `before_passes`, where the only prior placements are
    /// the seeds'.
    pub overrule: bool,
}

impl RegionKnobs {
    /// The shipped thresholds — **exactly the values the shipping arm was
    /// measured at**, unrounded and untidied, so that what runs is what was
    /// scored.
    pub const SHIPPED: RegionKnobs = RegionKnobs {
        splits: 3,
        region_min: 1,
        min_pairs: 2,
        int_tol: 0.20,
        pair_tol: 0.35,
        agree_frac: 0.80,
        contra_frac: 0.25,
        anchor_min: 8,
        anchor_frac: 0.60,
        anchor_largest: false,
        before_passes: true,
        overrule: false,
    };
}

/// What one region-assignment pass did.
#[derive(Debug, Clone, Copy, Default)]
pub struct RegionStats {
    pub regions: usize,
    pub edges: usize,
    pub accepted: usize,
    pub components: usize,
    pub contested: usize,
    pub placed: usize,
    /// Components refused for want of an anchor: no evidence at all, or
    /// evidence that did not agree with itself to `anchor_frac`. Reported
    /// because it is the arm's binding constraint, not a footnote.
    pub no_anchor: usize,
    /// Gates in a component that got a branch of zero — reached by the graph,
    /// and left where they were.
    pub zeroed: usize,
}

/// Union-find over regions carrying a fold-branch offset:
/// `branch(x) = off[x] + branch(find(x))`.
struct BranchDsu {
    parent: Vec<u32>,
    off: Vec<i32>,
    size: Vec<u32>,
    contra: Vec<u32>,
    agree: Vec<u32>,
}

impl BranchDsu {
    fn new(n: usize) -> Self {
        Self {
            parent: (0..n as u32).collect(),
            off: vec![0; n],
            size: vec![1; n],
            contra: vec![0; n],
            agree: vec![0; n],
        }
    }

    /// Root of `x` and `x`'s branch offset relative to it. Iterative, with the
    /// offsets rewritten on the way back down so the walk stays flat.
    fn find(&mut self, x: u32) -> (u32, i32) {
        let (mut root, mut acc) = (x, 0i32);
        while self.parent[root as usize] != root {
            acc += self.off[root as usize];
            root = self.parent[root as usize];
        }
        let (mut cur, mut cacc) = (x, acc);
        while self.parent[cur as usize] != cur {
            let (next, noff) = (self.parent[cur as usize], self.off[cur as usize]);
            self.parent[cur as usize] = root;
            self.off[cur as usize] = cacc;
            cacc -= noff;
            cur = next;
        }
        (root, acc)
    }

    /// Assert `branch(x) − branch(y) == d`. A cycle that contradicts what is
    /// already implied is counted and the edge dropped: the edges arrive in
    /// descending weight order, so the contradicting one is the weaker
    /// evidence by construction.
    fn union(&mut self, x: u32, y: u32, d: i32) {
        let (rx, ox) = self.find(x);
        let (ry, oy) = self.find(y);
        if rx == ry {
            if ox - oy == d {
                self.agree[rx as usize] += 1;
            } else {
                self.contra[rx as usize] += 1;
            }
            return;
        }
        let (a, b, oa, ob, d) = if self.size[rx as usize] >= self.size[ry as usize] {
            (rx, ry, ox, oy, d)
        } else {
            (ry, rx, oy, ox, -d)
        };
        self.parent[b as usize] = a;
        self.off[b as usize] = oa - ob - d;
        self.size[a as usize] += self.size[b as usize];
        self.contra[a as usize] += self.contra[b as usize];
        self.agree[a as usize] += self.agree[b as usize];
    }
}

/// Label 4-connected components of gates whose wrapped velocity falls in one
/// sub-interval of the Nyquist interval. Returns `(labels, count)`, label 0
/// meaning "no region".
fn label_regions(raw: &[Vec<f64>], gc: usize, nyquist: f64, splits: usize) -> (Vec<u32>, usize) {
    let n = raw.len();
    let mut label = vec![0u32; n * gc];
    let mut nf = 0usize;
    let width = 2.0 * nyquist / splits as f64;
    let mut stack: Vec<(usize, usize)> = Vec::new();
    for b in 0..splits {
        let lo = -nyquist + b as f64 * width;
        let hi = lo + width;
        let last = b + 1 == splits;
        let inband = |v: f64| !v.is_nan() && v >= lo && (v < hi || (last && v <= hi));
        for si in 0..n {
            for sj in 0..gc {
                if label[si * gc + sj] != 0 || !inband(raw[si][sj]) {
                    continue;
                }
                nf += 1;
                let id = nf as u32;
                label[si * gc + sj] = id;
                stack.push((si, sj));
                while let Some((ci, cj)) = stack.pop() {
                    let mut visit = |i: usize, j: usize, label: &mut Vec<u32>| {
                        if j < gc && label[i * gc + j] == 0 && inband(raw[i][j]) {
                            label[i * gc + j] = id;
                            stack.push((i, j));
                        }
                    };
                    if ci > 0 {
                        visit(ci - 1, cj, &mut label);
                    }
                    if ci + 1 < n {
                        visit(ci + 1, cj, &mut label);
                    }
                    if cj > 0 {
                        visit(ci, cj - 1, &mut label);
                    }
                    visit(ci, cj + 1, &mut label);
                }
            }
        }
    }
    (label, nf)
}

/// The pass itself. Writes `valid`/`value` only where it places a **non-zero**
/// branch, so that a gate this refuses is left exactly where the shipped passes
/// left it — including in the never-reached population the kept-raw region floor
/// and the fold censor act on.
#[allow(clippy::too_many_arguments)]
fn region_assign(
    raw: &[Vec<f64>],
    rows: crate::azimuth::Rows,
    gc: usize,
    nyquist: f64,
    valid: &mut [bool],
    value: &mut [f64],
    predict: &dyn Fn(usize, usize) -> Option<f64>,
    k: RegionKnobs,
) -> RegionStats {
    let n = raw.len();
    let interval = 2.0 * nyquist;
    let (mut label, nf) = label_regions(raw, gc, nyquist, k.splits.max(1));
    let mut stats = RegionStats::default();
    if nf < 2 {
        return stats;
    }
    let mut size = vec![0u32; nf + 1];
    for &l in &label {
        size[l as usize] += 1;
    }
    // Speckle is neither a claimant nor evidence.
    for l in label.iter_mut() {
        if *l != 0 && (size[*l as usize] as usize) < k.region_min {
            *l = 0;
        }
    }
    stats.regions = (1..=nf)
        .filter(|&l| size[l] as usize >= k.region_min)
        .count();

    let mut index: std::collections::HashMap<u64, usize> = std::collections::HashMap::new();
    let mut ea: Vec<u32> = Vec::new();
    let mut eb: Vec<u32> = Vec::new();
    let mut cnt: Vec<u32> = Vec::new();
    let mut sum: Vec<f64> = Vec::new();
    let each_pair = |mut f: Box<dyn FnMut(u32, u32, f64) + '_>| {
        for i in 0..n {
            let up = rows.neighbour(i, 1);
            for j in 0..gc {
                let la = label[i * gc + j];
                if la == 0 {
                    continue;
                }
                let va = raw[i][j];
                if j + 1 < gc {
                    let lb = label[i * gc + j + 1];
                    if lb != 0 && lb != la {
                        f(la, lb, va - raw[i][j + 1]);
                    }
                }
                if let Some(ni) = up {
                    let lb = label[ni * gc + j];
                    if lb != 0 && lb != la {
                        f(la, lb, va - raw[ni][j]);
                    }
                }
            }
        }
    };
    each_pair(Box::new(|la, lb, d| {
        let (a, b, d) = if la < lb { (la, lb, d) } else { (lb, la, -d) };
        let key = ((a as u64) << 32) | b as u64;
        let e = *index.entry(key).or_insert_with(|| {
            ea.push(a);
            eb.push(b);
            cnt.push(0);
            sum.push(0.0);
            ea.len() - 1
        });
        cnt[e] += 1;
        sum[e] += d;
    }));
    stats.edges = ea.len();
    if ea.is_empty() {
        return stats;
    }
    let branch_of: Vec<i32> = (0..ea.len())
        .map(|e| (sum[e] / cnt[e] as f64 / interval).round() as i32)
        .collect();
    // Second walk: how much of each boundary's own evidence sits on that branch.
    let mut agree = vec![0u32; ea.len()];
    let tol = k.pair_tol * nyquist;
    each_pair(Box::new(|la, lb, d| {
        let (a, b, d) = if la < lb { (la, lb, d) } else { (lb, la, -d) };
        let e = index[&(((a as u64) << 32) | b as u64)];
        if (d - branch_of[e] as f64 * interval).abs() <= tol {
            agree[e] += 1;
        }
    }));

    let mut accepted: Vec<usize> = (0..ea.len())
        .filter(|&e| {
            let m = sum[e] / cnt[e] as f64 / interval;
            cnt[e] as usize >= k.min_pairs
                && (m - branch_of[e] as f64).abs() <= k.int_tol
                && agree[e] as f64 >= k.agree_frac * cnt[e] as f64
        })
        .collect();
    stats.accepted = accepted.len();
    accepted.sort_unstable_by(|&x, &y| cnt[y].cmp(&cnt[x]).then(x.cmp(&y)));
    let mut dsu = BranchDsu::new(nf + 1);
    for &e in &accepted {
        dsu.union(eb[e], ea[e], branch_of[e]);
    }

    let mut members: std::collections::HashMap<u32, Vec<(u32, i32)>> =
        std::collections::HashMap::new();
    for l in 1..=nf as u32 {
        if (size[l as usize] as usize) < k.region_min {
            continue;
        }
        let (root, off) = dsu.find(l);
        members.entry(root).or_default().push((l, off));
    }
    stats.components = members.len();

    let mut gates_of: Vec<Vec<u32>> = vec![Vec::new(); nf + 1];
    for (g, &l) in label.iter().enumerate() {
        if l != 0 {
            gates_of[l as usize].push(g as u32);
        }
    }
    let mut branch = vec![0i32; nf + 1];
    for (root, mem) in &members {
        let (c, a) = (
            dsu.contra[*root as usize] as f64,
            dsu.agree[*root as usize] as f64,
        );
        if c > k.contra_frac * (c + a).max(1.0) {
            stats.contested += 1;
            continue;
        }
        let mut votes: std::collections::HashMap<i32, usize> = std::collections::HashMap::new();
        for &(l, off) in mem {
            for &g in &gates_of[l as usize] {
                let (g, i, j) = (g as usize, g as usize / gc, g as usize % gc);
                // The shipped passes' own decision, where they made one.
                if valid[g] {
                    let b = ((value[g] - raw[i][j]) / interval).round() as i32;
                    *votes.entry(b - off).or_default() += 1;
                }
                // The environment, on Seed 1's own agreement test.
                if let Some(p) = predict(i, j) {
                    let b = ((p - raw[i][j]) / interval).round() as i32;
                    if (raw[i][j] + b as f64 * interval - p).abs() < DA_SEED_TOL {
                        *votes.entry(b - off).or_default() += 1;
                    }
                }
            }
        }
        let total: usize = votes.values().sum();
        let best = votes.iter().max_by_key(|(b, c)| (**c, -**b));
        let base = match best {
            Some((&b, &c)) if total >= k.anchor_min && c as f64 >= k.anchor_frac * total as f64 => {
                b
            }
            // No evidence, or evidence that disagrees with itself.
            _ if k.anchor_largest => {
                stats.no_anchor += 1;
                -mem.iter()
                    .max_by_key(|(l, _)| size[*l as usize])
                    .map_or(0, |(_, off)| *off)
            }
            _ => {
                stats.no_anchor += 1;
                continue;
            }
        };
        for &(l, off) in mem {
            branch[l as usize] = base + off;
        }
    }

    for (g, &l) in label.iter().enumerate() {
        let b = branch[l as usize];
        if l == 0 {
            continue;
        }
        if b == 0 {
            stats.zeroed += 1;
            continue;
        }
        if valid[g] && !k.overrule {
            continue;
        }
        valid[g] = true;
        value[g] = raw[g / gc][g % gc] + b as f64 * interval;
        stats.placed += 1;
    }
    stats
}

/// Returns the `n · gc` incoherence mask this dealiasing set aside, so that
/// [`llsd_nrot`] — which refuses exactly the same ground — can read it instead
/// of asking [`incoherent_velocity`] the same question about the same grid a
/// second time.
pub(crate) fn dealias_with_knobs(
    vel_grid: &mut [Vec<f64>],
    sweep: &VelocitySweep,
    elevation_deg: f64,
    profile: Option<&WindProfile>,
    knobs: DealiasKnobs,
) -> Option<Vec<bool>> {
    let nyquist = fold_limit_ms(sweep, vel_grid)?;
    let interval = 2.0 * nyquist;
    let n = vel_grid.len();
    let gc = sweep.gate_count;
    if n < 8 {
        return None;
    }
    let rows = sweep_rows(sweep, n);
    // Where each row points, in radians, for the two wind seeds below.
    let az_rad: Vec<Option<f64>> = (0..n)
        .map(|i| sweep.azimuths_deg.get(i).map(|a| a.to_radians()))
        .collect();
    let reported: Vec<Vec<f64>> = vel_grid.to_vec();
    let refused = if knobs.refuse_incoherent {
        incoherent_velocity(&reported, rows, gc, sweep.gate_interval_km, nyquist)
    } else {
        vec![false; n * gc]
    };
    let mut raw = reported.clone();
    for (i, row) in raw.iter_mut().enumerate() {
        for (j, v) in row.iter_mut().enumerate() {
            if refused[i * gc + j] {
                *v = f64::NAN;
            }
        }
    }
    let raw = raw;
    // value[i][j] holds the dealiased velocity once valid[i][j].
    let mut valid = vec![false; n * gc];
    let mut value = vec![f64::NAN; n * gc];
    let idx = |i: usize, j: usize| i * gc + j;
    let has = |i: usize, j: usize| !raw[i][j].is_nan();

    // Seed 1: environmental winds.
    if let Some(wp) = profile {
        for ti in (0..n).step_by(5) {
            for tj in (0..gc).step_by(10) {
                let mut ok = true;
                let mut any = false;
                'tile: for (i, row) in raw.iter().enumerate().take((ti + 5).min(n)).skip(ti) {
                    let az = az_rad[i];
                    for (j, &v) in row.iter().enumerate().take((tj + 10).min(gc)).skip(tj) {
                        if v.is_nan() {
                            continue;
                        }
                        any = true;
                        let r = sweep.first_gate_range_km + j as f64 * sweep.gate_interval_km;
                        match az.and_then(|az| wp.predict(az, r, elevation_deg)) {
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
    if let Some(wp) = profile {
        let close = |i: usize, j: usize| -> Option<bool> {
            if !has(i, j) {
                return None;
            }
            let az = az_rad[i]?;
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
                    (rows.neighbour(i, -1), j),
                    (rows.neighbour(i, 1), j),
                    (Some(i), j.wrapping_sub(1)),
                    (Some(i), j + 1),
                ] {
                    if let Some(ni) = ni
                        && nj < gc
                        && close(ni, nj) == Some(true)
                    {
                        agree += 1;
                    }
                }
                if agree >= DA_SEEDGATE_NEIGHBORS {
                    cand[idx(i, j)] = true;
                }
            }
        }
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
                    // Candidate pockets at the two ends of a sector are two
                    // pockets, each held to `DA_RAWMIN_BINS` on its own.
                    let neigh = [
                        (rows.neighbour(ci, -1), cj),
                        (rows.neighbour(ci, 1), cj),
                        (Some(ci), cj.wrapping_sub(1)),
                        (Some(ci), cj + 1),
                    ];
                    for (ni, nj) in neigh {
                        let Some(ni) = ni else {
                            continue;
                        };
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
    let half_turn = half_turn_rows(rows);
    let opposite = |i: usize| {
        rows.neighbour(i, half_turn)
            .or_else(|| rows.neighbour(i, -half_turn))
    };
    let near_gates = ((40.0 - sweep.first_gate_range_km) / sweep.gate_interval_km) as usize;
    for i in 0..n {
        let Some(opp) = opposite(i) else {
            continue;
        };
        for j in 0..near_gates.min(gc) {
            if has(i, j)
                && raw[i][j].abs() < DA_ZISO_TOL
                && (0..3).any(|d| {
                    rows.neighbour(opp, d)
                        .is_some_and(|o| has(o, j) && raw[o][j].abs() < DA_ZISO_TOL)
                })
            {
                valid[idx(i, j)] = true;
                value[idx(i, j)] = raw[i][j];
            }
        }
    }

    let unfold =
        |v: f64, reference: f64| -> f64 { v + ((reference - v) / interval).round() * interval };

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

    // The environment along one line of sight, for the region arm's anchor —
    // the same question Seed 1 asks of a 5 × 10 tile, asked of a component.
    let predict = |i: usize, j: usize| -> Option<f64> {
        let r = sweep.first_gate_range_km + j as f64 * sweep.gate_interval_km;
        profile?.predict(az_rad[i]?, r, elevation_deg)
    };
    let mut region_done = false;
    let mut region_stats = RegionStats::default();
    // The region arm's own place in the order.
    if let Some(k) = knobs.region.filter(|k| k.before_passes) {
        region_done = true;
        region_stats = region_assign(&raw, rows, gc, nyquist, &mut valid, &mut value, &predict, k);
    }
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

        // (b) azimuthal bridge, tighter threshold; azimuth wraps where the
        // sweep closes the circle.
        let t_b = 0.35 * nyquist * DA_THRESH_SCALE;
        for j in 0..gc {
            for start in 0..n {
                if !valid[idx(start, j)] {
                    continue;
                }
                // The rows the walk crosses on its way to `end`, in order.
                let mut gap = [0usize; 39];
                let mut k = 1;
                let mut end = None;
                while k < 40 {
                    let Some(ii) = rows.neighbour(start, k as i32) else {
                        break;
                    };
                    if valid[idx(ii, j)] {
                        end = Some(ii);
                        break;
                    }
                    if !has(ii, j) {
                        break;
                    }
                    gap[k - 1] = ii;
                    k += 1;
                }
                // `k == 1` is `end` sitting in the next row along, with no gap
                // between the two to bridge.
                let Some(end) = end.filter(|_| k > 1) else {
                    continue;
                };
                let raws_f: Vec<f64> = gap[..k - 1].iter().map(|&ii| raw[ii][j]).collect();
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
                            let ii = gap[off];
                            valid[idx(ii, j)] = true;
                            value[idx(ii, j)] = *a;
                            changed = true;
                        }
                    }
                }
            }
        }

        for aliased in [false, true] {
            let t = if aliased {
                DA_FLOOD_ALIASED_FRAC
            } else {
                DA_FLOOD_RAW_FRAC
            } * nyquist
                * DA_THRESH_SCALE;
            for i in 0..n {
                // A row on the edge of a sector is flooded from the one side
                // it has a neighbour on.
                for di in [-1i32, 1] {
                    let Some(ni) = rows.neighbour(i, di) else {
                        continue;
                    };
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
            // The passes have converged.
            match knobs.region.filter(|_| !region_done) {
                Some(k) => {
                    region_done = true;
                    region_stats =
                        region_assign(&raw, rows, gc, nyquist, &mut valid, &mut value, &predict, k);
                }
                None => break,
            }
        }
    }
    if region_done {
        // Campaign instrument: the arm's own account of what it refused.
        if std::env::var_os("SQUALLAR_REGION_STATS").is_some() {
            eprintln!("REGIONSTATS {region_stats:?}");
        }
        log::debug!(
            "region-assign: {} regions, {}/{} edges accepted, {} components \
             ({} contested, {} unanchored), {} gates placed, {} left at zero",
            region_stats.regions,
            region_stats.accepted,
            region_stats.edges,
            region_stats.components,
            region_stats.contested,
            region_stats.no_anchor,
            region_stats.placed,
            region_stats.zeroed
        );
    }

    // Convert unresolved to ND; write dealiased values back.
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
                    (rows.neighbour(ci, -1), cj),
                    (rows.neighbour(ci, 1), cj),
                    (Some(ci), cj.wrapping_sub(1)),
                    (Some(ci), cj + 1),
                ];
                for (ni, nj) in neigh {
                    let Some(ni) = ni else {
                        continue;
                    };
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
            vel_grid[i][j] = if refused[idx(i, j)] {
                reported[i][j]
            } else if valid[idx(i, j)] {
                value[idx(i, j)]
            } else if keep_raw[idx(i, j)] {
                raw[i][j]
            } else {
                f64::NAN
            };
        }
    }
    if knobs.censor_vny_frac.is_infinite() {
        return knobs.refuse_incoherent.then_some(refused);
    }
    let snapshot: Vec<Vec<f64>> = vel_grid.to_vec();
    let censor_at = knobs.censor_vny_frac * nyquist;
    for i in 0..n {
        for j in 0..gc {
            if refused[idx(i, j)] {
                continue;
            }
            let v = snapshot[i][j];
            if v.is_nan() {
                continue;
            }
            let nb_of = |i: usize, j: usize| {
                if refused[idx(i, j)] {
                    f64::NAN
                } else {
                    snapshot[i][j]
                }
            };
            let up = rows.neighbour(i, 1).map_or(f64::NAN, |k| nb_of(k, j));
            let down = rows.neighbour(i, -1).map_or(f64::NAN, |k| nb_of(k, j));
            let right = if j + 1 < gc {
                nb_of(i, j + 1)
            } else {
                f64::NAN
            };
            let left = if j > 0 { nb_of(i, j - 1) } else { f64::NAN };
            for nb in [up, down, left, right] {
                if !nb.is_nan() && (nb - v).abs() > censor_at {
                    vel_grid[i][j] = f64::NAN;
                    break;
                }
            }
        }
    }
    knobs.refuse_incoherent.then_some(refused)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    /// The hoisted beam height is the shared one, bit for bit.
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
            declared_nyquist_ms: None,
            status: None,
        }
    }

    /// The fixtures declare nothing, so every existing expectation below is
    /// measured against [`estimate_nyquist`] exactly as it always was;
    /// `declaring` is how the two tests that are *about* the declaration state
    /// one.
    fn sweep<'a>(grid: &'a [Vec<f64>], azimuths: &'a [f64], gates: usize) -> VelocitySweep<'a> {
        VelocitySweep {
            vel_grid: grid,
            azimuths_deg: azimuths,
            gate_count: gates,
            first_gate_range_km: 0.25,
            gate_interval_km: 0.25,
            declared_nyquist_ms: None,
            status: None,
        }
    }

    fn ring_azimuths(n: usize) -> Vec<f64> {
        (0..n).map(|i| i as f64 * 360.0 / n as f64).collect()
    }

    // ---- the region arm ---------------------------------------------------

    /// A sweep whose *true* radial velocity is known, and the wrapped field the
    /// radar would report for it. Nothing here is produced by the dealiaser, so
    /// what the tests below assert is agreement with the constructed field
    /// rather than with any output of the code under test.
    fn folded_ramp(n: usize, gc: usize, ny: f64) -> (Vec<Vec<f64>>, Vec<Vec<f64>>) {
        let truth: Vec<Vec<f64>> = (0..n)
            .map(|_| {
                (0..gc)
                    .map(|j| match j {
                        0..=100 => 15.0,
                        101..=160 => 15.0 + 30.0 * (j - 100) as f64 / 60.0,
                        _ => 45.0,
                    })
                    .collect()
            })
            .collect();
        let wrapped = truth
            .iter()
            .map(|row| {
                row.iter()
                    .map(|&v| v - (v / (2.0 * ny)).round() * 2.0 * ny)
                    .collect()
            })
            .collect();
        (truth, wrapped)
    }

    /// What [`vad_folded`] hands back: a folded field, the truth it was folded
    /// from, the geometry the two share, and the only wind evidence the
    /// dealiaser is allowed to see.
    struct VadFolded {
        /// The environmental profile the dealiaser is given. Fitted only below
        /// 1.0 km — see [`vad_folded`] for why that is the situation and not a
        /// convenience.
        seed: WindProfile,
        /// Ring azimuths in degrees, one per radial of `truth` and `wrapped`.
        azimuths_deg: Vec<f64>,
        /// The unfolded field the wind produces: what recovery is scored against.
        truth: Vec<Vec<f64>>,
        /// `truth` wrapped onto the Nyquist interval: the dealiaser's input.
        wrapped: Vec<Vec<f64>>,
    }

    /// A field an actual wind produces, and the wind that produced it.
    fn vad_folded(n: usize, gc: usize, ny: f64, elev: f64) -> VadFolded {
        let speed = |h: f64| match h {
            h if h < 1.0 => 15.0,
            h if h < 2.5 => 15.0 + 30.0 * (h - 1.0) / 1.5,
            _ => 45.0,
        };
        let level = |l: usize| (l as f64 + 0.5) * PROFILE_LAYER_KM;
        let air: Vec<(f64, f64, f64)> = (0..PROFILE_LAYERS)
            .map(|l| (level(l), 0.0, speed(level(l))))
            .collect();
        let full = WindProfile::from_levels(&air).expect("profile");
        let seed = WindProfile::from_levels(
            &air.iter()
                .copied()
                .filter(|&(h, _, _)| h < 1.0)
                .collect::<Vec<_>>(),
        )
        .expect("seed profile");
        let azimuths_deg = ring_azimuths(n);
        let truth: Vec<Vec<f64>> = (0..n)
            .map(|i| {
                (0..gc)
                    .map(|j| {
                        let r = 2.125 + j as f64;
                        full.predict(azimuths_deg[i].to_radians(), r, elev)
                            .expect("prediction")
                    })
                    .collect()
            })
            .collect();
        let wrapped = truth
            .iter()
            .map(|row| {
                row.iter()
                    .map(|&v| v - (v / (2.0 * ny)).round() * 2.0 * ny)
                    .collect()
            })
            .collect();
        VadFolded {
            seed,
            azimuths_deg,
            truth,
            wrapped,
        }
    }

    fn coverage_knobs(region: Option<RegionKnobs>) -> DealiasKnobs {
        DealiasKnobs {
            rawmin_bins: COVERAGE_RAWMIN_BINS,
            censor_vny_frac: CENSOR_VNY_FRAC,
            refuse_incoherent: false,
            region,
        }
    }

    /// The same fixture as
    /// [`the_interior_of_a_folded_region_is_recovered_from_a_seed`], with the
    /// seed taken away — and with it, every scrap of evidence about which
    /// absolute branch the region sits on.
    #[test]
    fn a_region_with_no_anchor_evidence_is_refused_rather_than_guessed() {
        let (ny, n, gc) = (20.0, 72usize, 200usize);
        let (truth, wrapped) = folded_ramp(n, gc, ny);
        let az = ring_azimuths(n);
        let sweep = VelocitySweep {
            vel_grid: &wrapped,
            azimuths_deg: &az,
            gate_count: gc,
            first_gate_range_km: 2.125,
            gate_interval_km: 0.5,
            declared_nyquist_ms: Some(ny),
            status: None,
        };
        // Same preconditions as ever: gate 180 is deep interior, folded, and
        // reported at a quarter of the limit.
        assert!(
            wrapped[0][180].abs() < 0.5 * ny
                && (truth[0][180] - 45.0).abs() < 1e-9
                && (wrapped[0][180] - 5.0).abs() < 1e-9,
            "precondition: the fixture's folded interior is not +45 reported as +5",
        );

        let mut arm = wrapped.clone();
        dealias_with_knobs(
            &mut arm,
            &sweep,
            0.5,
            None,
            coverage_knobs(Some(RegionKnobs::SHIPPED)),
        );
        let interior: Vec<(usize, usize)> = (0..n)
            .flat_map(|i| (165..gc).map(move |j| (i, j)))
            .collect();
        let moved = interior
            .iter()
            .filter(|&&(i, j)| (arm[i][j] - wrapped[i][j]).abs() > 1e-9)
            .count();
        assert_eq!(
            moved,
            0,
            "{moved} of {} interior gates were given a branch on no evidence",
            interior.len(),
        );

        let mut guessed = wrapped.clone();
        dealias_with_knobs(
            &mut guessed,
            &sweep,
            0.5,
            None,
            coverage_knobs(Some(RegionKnobs {
                anchor_largest: true,
                ..RegionKnobs::SHIPPED
            })),
        );
        let guessed_right = interior
            .iter()
            .filter(|&&(i, j)| (guessed[i][j] - truth[i][j]).abs() < 1e-9)
            .count();
        assert_eq!(
            guessed_right,
            interior.len(),
            "with `anchor_largest` on this fixture must resolve, or the refusal \
             above is not the posture's doing and this test is pinning nothing",
        );
    }

    /// The defect the arm exists for: a folded region's **interior** is not
    /// reachable by propagation from a wall, and is reachable by assigning a
    /// branch to the region — from evidence.
    #[test]
    fn the_interior_of_a_folded_region_is_recovered_from_a_seed() {
        let (ny, n, gc, elev) = (20.0, 72usize, 200usize, 0.5);
        let VadFolded {
            seed,
            azimuths_deg,
            truth,
            wrapped,
        } = vad_folded(n, gc, ny, elev);
        let sweep = VelocitySweep {
            vel_grid: &wrapped,
            azimuths_deg: &azimuths_deg,
            gate_count: gc,
            first_gate_range_km: 2.125,
            gate_interval_km: 1.0,
            declared_nyquist_ms: Some(ny),
            status: None,
        };
        // The gates this test is about: genuinely folded, and reported well
        // inside the interior column where a wall-local rule has nothing to see.
        let interior: Vec<(usize, usize)> = (0..n)
            .flat_map(|i| (0..gc).map(move |j| (i, j)))
            .filter(|&(i, j)| {
                (truth[i][j] - wrapped[i][j]).abs() > 1e-9 && wrapped[i][j].abs() < 0.5 * ny
            })
            .collect();
        assert!(
            interior.len() > 500,
            "precondition: the fixture has only {} folded interior gates to \
             measure, which is too few to conclude anything from",
            interior.len(),
        );

        let mut shipped = wrapped.clone();
        dealias_with_knobs(
            &mut shipped,
            &sweep,
            elev,
            Some(&seed),
            coverage_knobs(None),
        );
        let mut arm = wrapped.clone();
        dealias_with_knobs(
            &mut arm,
            &sweep,
            elev,
            Some(&seed),
            coverage_knobs(Some(RegionKnobs::SHIPPED)),
        );
        let right = |g: &Vec<Vec<f64>>| {
            interior
                .iter()
                .filter(|&&(i, j)| (g[i][j] - truth[i][j]).abs() < 1e-9)
                .count()
        };
        let (a, b) = (right(&shipped), right(&arm));
        assert!(
            b > a,
            "the region arm recovered {b} of {} folded interior gates and the \
             wall-local one {a}: this fixture is not isolating the assignment",
            interior.len(),
        );
        assert!(
            2 * b > interior.len(),
            "the region arm recovered only {b} of {} folded interior gates",
            interior.len(),
        );
    }

    /// The constraint that makes the arm shippable at all: on a field with no
    /// coherent structure it must claim nothing, and the refusal must be what
    /// makes it so.
    #[test]
    fn a_field_with_no_structure_gets_no_branch_and_the_refusal_is_why() {
        let (ny, n, gc) = (20.0, 72usize, 200usize);
        // Deterministic uniform noise on the Nyquist interval: no seed, no
        // fixture file, and identical on every platform.
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 11) as f64 / (1u64 << 53) as f64 * 2.0 * ny - ny
        };
        let noise: Vec<Vec<f64>> = (0..n).map(|_| (0..gc).map(|_| next()).collect()).collect();
        let az = ring_azimuths(n);
        let sweep = VelocitySweep {
            vel_grid: &noise,
            azimuths_deg: &az,
            gate_count: gc,
            first_gate_range_km: 2.125,
            gate_interval_km: 0.5,
            declared_nyquist_ms: Some(ny),
            status: None,
        };
        let moved = |knobs: DealiasKnobs| {
            let mut g = noise.clone();
            dealias_with_knobs(&mut g, &sweep, 0.5, None, knobs);
            (0..n)
                .flat_map(|i| (0..gc).map(move |j| (i, j)))
                .filter(|&(i, j)| g[i][j].is_finite() && (g[i][j] - noise[i][j]).abs() > 1e-9)
                .count()
        };
        let refused = moved(coverage_knobs(Some(RegionKnobs::SHIPPED)));
        let admitted = moved(coverage_knobs(Some(RegionKnobs {
            min_pairs: 1,
            int_tol: 1.0,
            agree_frac: 0.0,
            contra_frac: 1.01,
            anchor_largest: true,
            ..RegionKnobs::SHIPPED
        })));
        let total = n * gc;
        assert!(
            refused * 50 < total,
            "the arm moved {refused} of {total} gates on structureless noise — the \
             floor this whole design exists to hold is 2% and it did not hold it",
        );
        assert!(
            admitted > refused * 4,
            "the published posture moved {admitted} gates against this arm's \
             {refused}; if those are comparable then the refusal is not what \
             holds this field and the ablation proves nothing",
        );
        assert!(
            admitted * 4 > total,
            "the ablation moved only {admitted} of {total} gates, so it is not \
             ablating: with `refused` at {refused} the comparison above passes on \
             a single gate, and a refusal that has quietly stopped refusing would \
             read as a pass",
        );
    }

    /// Range geometry for the synthetic cuts below: 200 gates a kilometre
    /// apart, which at 0.5° reaches 4.1 km and gives every layer it crosses
    /// thousands of samples, well clear of both the 200-sample floor and the
    /// [`PROFILE_MAX_SAMPLES`] ceiling.
    fn vad_sweep<'a>(grid: &'a [Vec<f64>], azimuths: &'a [f64]) -> VelocitySweep<'a> {
        VelocitySweep {
            vel_grid: grid,
            azimuths_deg: azimuths,
            gate_count: grid.first().map_or(0, Vec::len),
            first_gate_range_km: 1.0,
            gate_interval_km: 1.0,
            declared_nyquist_ms: None,
            status: None,
        }
    }

    /// `rows` radials `step_deg` apart from `az0`, every gate holding the
    /// radial component of `(u, v)`: `vr = u·sin(az)·cos(el) + v·cos(az)·cos(el)`.
    /// One wind at every height, so every layer must fit the same one.
    fn vad_cut(
        rows: usize,
        az0: f64,
        step_deg: f64,
        (u, v): (f64, f64),
        el_deg: f64,
    ) -> (Vec<Vec<f64>>, Vec<f64>) {
        let azimuths: Vec<f64> = (0..rows)
            .map(|i| (az0 + i as f64 * step_deg).rem_euclid(360.0))
            .collect();
        let cos_el = el_deg.to_radians().cos();
        let grid = azimuths
            .iter()
            .map(|a| {
                let r = a.to_radians();
                vec![(u * r.sin() + v * r.cos()) * cos_el; 200]
            })
            .collect();
        (grid, azimuths)
    }

    /// Heights every synthetic cut below reaches with room to spare.
    const VAD_PROBES: [f64; 4] = [0.15, 0.75, 1.65, 2.85];

    fn assert_wind(profile: &WindProfile, (u, v): (f64, f64), tol: f64, what: &str) {
        for h in VAD_PROBES {
            let (fu, fv) = profile
                .wind_at_km(h)
                .unwrap_or_else(|| panic!("{what}: no fit at {h} km"));
            assert!(
                (fu - u).abs() < tol && (fv - v).abs() < tol,
                "{what}: {h} km fitted ({fu:.4}, {fv:.4}), wind is ({u}, {v})",
            );
        }
    }

    /// The wind a cut measures does not depend on where the cut began.
    #[test]
    fn a_cut_that_starts_off_north_fits_the_wind_a_cut_starting_at_north_does() {
        let wind = (12.0, -5.0);
        let mut fits = Vec::new();
        for az0 in [0.0, 137.5] {
            let (grid, azimuths) = vad_cut(360, az0, 1.0, wind, 0.5);
            let mut builder = WindProfileBuilder::new();
            builder.add_sweep(&vad_sweep(&grid, &azimuths), 0.5);
            fits.push(builder.finish().expect("a noiseless cut fits"));
        }
        assert_wind(&fits[0], wind, 1e-6, "cut starting at north");
        assert_wind(&fits[1], wind, 1e-6, "cut starting at 137.5°");
        for h in VAD_PROBES {
            let (a, b) = (fits[0].wind_at_km(h), fits[1].wind_at_km(h));
            let ((au, av), (bu, bv)) = (a.unwrap(), b.unwrap());
            assert!(
                (au - bu).abs() < 1e-9 && (av - bv).abs() < 1e-9,
                "the two cuts disagree at {h} km: {a:?} against {b:?}",
            );
        }
    }

    /// The whole point of pooling a volume: four cuts, four start azimuths,
    /// four elevations, one atmosphere.
    #[test]
    fn cuts_that_start_at_four_azimuths_pool_to_the_one_wind() {
        let wind = (-9.0, 14.0);
        let cuts: Vec<(Vec<Vec<f64>>, Vec<f64>, f64)> =
            [(0.0, 0.5), (97.3, 0.9), (211.8, 1.5), (318.4, 2.4)]
                .into_iter()
                .map(|(az0, el)| {
                    let (grid, azimuths) = vad_cut(360, az0, 1.0, wind, el);
                    (grid, azimuths, el)
                })
                .collect();
        let mut builder = WindProfileBuilder::new();
        for (grid, azimuths, el) in &cuts {
            builder.add_sweep(&vad_sweep(grid, azimuths), *el);
        }
        assert_wind(
            &builder.finish().expect("four noiseless cuts fit"),
            wind,
            1e-6,
            "four cuts pooled",
        );
    }

    /// A layer offered more than it can hold is fitted from all of it, thinned
    /// — not from the first of it.
    #[test]
    fn a_layer_offered_more_than_it_holds_is_fitted_from_the_whole_volume() {
        // 700 gates 50 m apart reach 35 km, which at 0.5° is still inside the
        // second layer: nearly every sample lands in the 0–0.3 km one.
        let cut = |(u, v): (f64, f64)| -> (Vec<Vec<f64>>, Vec<f64>) {
            let azimuths: Vec<f64> = (0..360).map(|i| i as f64).collect();
            let cos_el = 0.5f64.to_radians().cos();
            let grid = azimuths
                .iter()
                .map(|a| {
                    let r = a.to_radians();
                    vec![(u * r.sin() + v * r.cos()) * cos_el; 700]
                })
                .collect();
            (grid, azimuths)
        };
        let mut builder = WindProfileBuilder::new();
        for wind in [(6.0, 0.0), (0.0, 6.0)] {
            let (grid, azimuths) = cut(wind);
            builder.add_sweep(
                &VelocitySweep {
                    vel_grid: &grid,
                    azimuths_deg: &azimuths,
                    gate_count: 700,
                    first_gate_range_km: 0.05,
                    gate_interval_km: 0.05,
                    declared_nyquist_ms: None,
                    status: None,
                },
                0.5,
            );
        }
        let (u, v) = builder
            .finish()
            .expect("two oversubscribed cuts fit")
            .wind_at_km(0.15)
            .expect("the 0–0.3 km layer is the one they filled");
        assert!(
            (u - 3.0).abs() < 0.01 && (v - 3.0).abs() < 0.01,
            "the layer fitted ({u:.4}, {v:.4}), not the (3, 3) both cuts average to",
        );
    }

    /// A sector holds an arc, and an arc of a sinusoid still determines it.
    /// 90° of 0.5° radials — the narrowest the chunk feed hands over as a
    /// usable cut — recovers (7, 11) m/s to 7e-12 m/s.
    #[test]
    fn a_ninety_degree_sector_fits_the_wind_over_the_arc_it_has() {
        let wind = (7.0, 11.0);
        let (grid, azimuths) = vad_cut(181, 42.0, 0.5, wind, 0.5);
        let mut builder = WindProfileBuilder::new();
        builder.add_sweep(&vad_sweep(&grid, &azimuths), 0.5);
        assert_wind(
            &builder
                .finish()
                .expect("a 90° arc still determines a VAD fit"),
            wind,
            1e-9,
            "90° sector",
        );
    }

    /// [`vad_cut`] with a residual of a **chosen size** laid over the wind:
    /// `+amp` on even gate indices, `−amp` on odd ones.
    fn vad_cut_noisy(
        rows: usize,
        (u, v): (f64, f64),
        el_deg: f64,
        amp: f64,
    ) -> (Vec<Vec<f64>>, Vec<f64>) {
        let azimuths: Vec<f64> = (0..rows).map(|i| i as f64).collect();
        let cos_el = el_deg.to_radians().cos();
        let grid = azimuths
            .iter()
            .map(|a| {
                let r = a.to_radians();
                let base = (u * r.sin() + v * r.cos()) * cos_el;
                (0..200)
                    .map(|j| if j % 2 == 0 { base + amp } else { base - amp })
                    .collect()
            })
            .collect();
        (grid, azimuths)
    }

    /// A layer that solves but does not *fit* is not a wind.
    #[test]
    fn a_layer_whose_residual_clears_the_rpgs_ceiling_is_not_published() {
        let wind = (9.0, -6.0);
        // 4 m/s of residual — 7.8 kt, inside the RPG's 9.7 — is a fit.
        let (grid, azimuths) = vad_cut_noisy(360, wind, 0.5, 4.0);
        let mut builder = WindProfileBuilder::new();
        builder.add_sweep(&vad_sweep(&grid, &azimuths), 0.5);
        let kept = builder
            .finish()
            .expect("a 7.8 kt residual is under the RPG's 9.7 kt ceiling");
        // The wind itself is untouched by the perturbation, which is what
        // "orthogonal to the design" buys and what makes the arms comparable.
        assert_wind(&kept, wind, 0.05, "4 m/s residual");

        // 6 m/s — 11.7 kt — is not, and no layer of it is.
        let (grid, azimuths) = vad_cut_noisy(360, wind, 0.5, 6.0);
        let mut builder = WindProfileBuilder::new();
        builder.add_sweep(&vad_sweep(&grid, &azimuths), 0.5);
        assert!(
            builder.finish().is_none(),
            "an 11.7 kt residual is over the RPG's 9.7 kt ceiling and must not \
             be published as a wind",
        );
    }

    /// A volume that never sampled 6 km does not get to answer at 6 km.
    #[test]
    fn the_fill_reaches_three_layers_past_the_top_fitted_one_and_stops() {
        let wind = (13.0, 4.0);
        let (grid, azimuths) = vad_cut(360, 0.0, 1.0, wind, 0.5);
        let mut builder = WindProfileBuilder::new();
        builder.add_sweep(&vad_sweep(&grid, &azimuths), 0.5);
        let profile = builder.finish().expect("layers 0-13 fit");

        let centre = |l: usize| (l as f64 + 0.5) * PROFILE_LAYER_KM;
        // 13 is the top layer the beam reached, and it fit.
        assert!(
            profile.wind_at_km(centre(13)).is_some(),
            "layer 13 is the top layer this geometry samples and must fit",
        );
        // 14, 15, 16 are the fill, and carry layer 13's wind.
        for l in 14..=16 {
            let (u, v) = profile
                .wind_at_km(centre(l))
                .unwrap_or_else(|| panic!("layer {l} is within the fill's reach"));
            assert!(
                (u - wind.0).abs() < 1e-6 && (v - wind.1).abs() < 1e-6,
                "layer {l} is a clamp-copy and must carry the fitted wind",
            );
        }
        // 17 is one layer too far, and so is everything above it.
        for l in 17..PROFILE_LAYERS {
            assert!(
                profile.wind_at_km(centre(l)).is_none(),
                "layer {l} is {} layers past the top fitted one and must not \
                 answer; unbounded, this profile answered at every layer to 39",
                l - 13,
            );
        }
    }

    /// The two constructors fill by the same rule.
    #[test]
    fn both_constructors_fill_to_the_same_reach() {
        let centre = |l: usize| (l as f64 + 0.5) * PROFILE_LAYER_KM;
        // One level, at the bottom: everything else is fill or nothing.
        let from_levels =
            WindProfile::from_levels(&[(0.0, 7.0, -3.0)]).expect("one level is a profile");
        let reach = PROFILE_FILL_MAX_LAYERS as usize;
        assert!(
            from_levels.wind_at_km(centre(reach)).is_some(),
            "from_levels must fill {reach} layers out",
        );
        assert!(
            from_levels.wind_at_km(centre(reach + 1)).is_none(),
            "from_levels must stop at {reach} layers out",
        );

        // And `finish`, whose top fitted layer is 13 by the geometry above.
        let (grid, azimuths) = vad_cut(360, 0.0, 1.0, (7.0, -3.0), 0.5);
        let mut builder = WindProfileBuilder::new();
        builder.add_sweep(&vad_sweep(&grid, &azimuths), 0.5);
        let finished = builder.finish().expect("layers 0-13 fit");
        assert!(
            finished.wind_at_km(centre(13 + reach)).is_some(),
            "finish must fill {reach} layers out, the same as from_levels",
        );
        assert!(
            finished.wind_at_km(centre(13 + reach + 1)).is_none(),
            "finish must stop at {reach} layers out, the same as from_levels",
        );
    }

    const _: () = assert!(
        4.0 < PROFILE_MAX_RMS_MS && PROFILE_MAX_RMS_MS < 6.0,
        "the two planted residuals must straddle PROFILE_MAX_RMS_MS",
    );

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

    /// The wind seeds ask the profile about the sky, not about the sweep.
    #[test]
    fn the_wind_seeds_read_the_azimuth_the_antenna_pointed_at() {
        let n = 72;
        let gates = 40;
        let nyquist = 25.0;
        // 45° is a whole number of 5° rows, so this cut holds exactly the rows
        // a cut starting at north would, renumbered: its row 63 faces north.
        let azs: Vec<f64> = (0..n)
            .map(|i| (45.0 + i as f64 * 360.0 / n as f64).rem_euclid(360.0))
            .collect();
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
        grid[9][0] = 25.0;
        let wp = WindProfile::from_levels(&[(0.0, 0.0, 30.0)]).unwrap();

        let vg = grid.clone();
        let sw = VelocitySweep {
            vel_grid: &vg,
            azimuths_deg: &azs,
            gate_count: gates,
            first_gate_range_km: 50.0,
            gate_interval_km: 1.0,
            // No declaration: this fixture's expectations were measured against
            // the estimator, which is what an undeclared sweep still reaches.
            declared_nyquist_ms: None,
            status: None,
        };
        dealias(&mut grid, &sw, 0.5, Some(&wp), DealiasProfile::NoFalseShear);

        assert_eq!(grid[63][10], 30.0, "the folded arc should unfold to +30");
        assert_eq!(grid[3][10], true_v[3], "unfolded flow must not move");
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
        // (the zero-isodop band).
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

    /// A sector's two edges are two edges, not a join.
    #[test]
    fn dealias_leaves_a_sectors_continuous_data_alone() {
        let n = 72;
        let gates = 40;
        let azimuths: Vec<f64> = (0..n).map(|i| i as f64 * 0.5).collect();
        let orig: Vec<Vec<f64>> = (0..n)
            .map(|i| vec![-20.0 + 40.0 * i as f64 / (n - 1) as f64; gates])
            .collect();
        let mut grid = orig.clone();
        let vg = grid.clone();
        dealias(
            &mut grid,
            &sweep_for(&vg, &azimuths, gates),
            0.5,
            None,
            DealiasProfile::NoFalseShear,
        );
        assert_eq!(grid, orig);
    }

    /// A gust front is 11 m/s in and 11 m/s out across one line, and the
    /// declared limit is what tells that from a fold.
    #[test]
    fn a_declared_limit_keeps_a_shear_line_the_estimate_censors_as_a_fold() {
        /// KTLX's 0.5° Doppler cut, 2026-08-11 10:09 — the narrowest real
        /// declaration in the ten-volume WSR-88D control, so the fixture is
        /// tested against the tightest censor an archive has actually asked
        /// for.
        const DECLARED_MS: f64 = 23.84;
        let n = 72;
        let gates = 40;
        let azimuths = ring_azimuths(n);
        let orig: Vec<Vec<f64>> = (0..n)
            .map(|i| vec![if i < n / 2 { 11.0 } else { -11.0 }; gates])
            .collect();

        let run = |declared: Option<f64>| {
            let mut grid = orig.clone();
            let vg = grid.clone();
            let mut sweep = sweep_for(&vg, &azimuths, gates);
            sweep.declared_nyquist_ms = declared;
            dealias(&mut grid, &sweep, 0.5, None, DealiasProfile::NoFalseShear);
            grid
        };

        let declared = run(Some(DECLARED_MS));
        assert_eq!(
            declared, orig,
            "the declared limit leaves a 22 m/s shear line exactly where it was",
        );

        let estimated = run(None);
        for row in [0usize, n / 2 - 1, n / 2, n - 1] {
            assert!(
                estimated[row].iter().all(|v| v.is_nan()),
                "row {row} faces the line and the 11 m/s estimate censors it",
            );
        }
        assert_eq!(
            estimated.iter().flatten().filter(|v| v.is_nan()).count(),
            4 * gates,
            "only the four rows either side of the two lines are erased",
        );
    }

    /// Both halves of the censor's job, on the two jumps the reference draws
    /// its line between.
    #[test]
    fn the_censor_keeps_the_shear_the_reference_paints_and_drops_a_fold_displacement() {
        /// KHNX 2024-12-16 08:01:56, elevation 2, the cut the ladder was
        /// painted into.
        const VNY: f64 = 11.66;
        let n = 72;
        let gates = 40;
        let azimuths = ring_azimuths(n);
        let run = |amp: f64| {
            let orig: Vec<Vec<f64>> = (0..n)
                .map(|i| vec![if i < n / 2 { amp } else { -amp }; gates])
                .collect();
            let mut grid = orig.clone();
            let vg = grid.clone();
            let mut sweep = sweep_for(&vg, &azimuths, gates);
            sweep.declared_nyquist_ms = Some(VNY);
            dealias(&mut grid, &sweep, 0.5, None, DealiasProfile::NoFalseShear);
            (orig, grid)
        };

        let (orig, kept) = run(0.85 * VNY);
        assert_eq!(
            kept, orig,
            "a 1.70·Vny jump is shear, and the reference paints it",
        );

        let (_, walled) = run(VNY);
        for row in [0usize, n / 2 - 1, n / 2, n - 1] {
            assert!(
                walled[row].iter().all(|v| v.is_nan()),
                "row {row} faces a 2.00·Vny wall and must not survive it",
            );
        }
        assert_eq!(
            walled.iter().flatten().filter(|v| v.is_nan()).count(),
            4 * gates,
            "only the four rows either side of the two walls are erased",
        );
    }

    /// A declaration under [`crate::sampler::FOLD_LIMIT_FLOOR_MS`] is a
    /// mis-decoded field, not a very slow radar: no operational waveform folds
    /// at 3 m/s.
    #[test]
    fn a_declaration_below_the_floor_falls_back_to_the_estimate() {
        let n = 72;
        let gates = 40;
        let azimuths = ring_azimuths(n);
        let orig: Vec<Vec<f64>> = (0..n)
            .map(|i| vec![if i < n / 2 { 11.0 } else { -11.0 }; gates])
            .collect();
        let vg = orig.clone();
        let mut sweep = sweep_for(&vg, &azimuths, gates);

        sweep.declared_nyquist_ms = Some(3.0);
        assert_eq!(
            fold_limit_ms(&sweep, &orig),
            Some(11.0),
            "a sub-floor declaration is refused and the estimate answers",
        );
        sweep.declared_nyquist_ms = Some(crate::sampler::FOLD_LIMIT_FLOOR_MS);
        assert_eq!(
            fold_limit_ms(&sweep, &orig),
            Some(crate::sampler::FOLD_LIMIT_FLOOR_MS),
            "the floor itself is believed",
        );

        let mut grid = orig.clone();
        sweep.declared_nyquist_ms = Some(3.0);
        dealias(&mut grid, &sweep, 0.5, None, DealiasProfile::NoFalseShear);
        assert!(
            grid[0].iter().all(|v| v.is_nan()),
            "the sub-floor arm censors exactly as the estimate does",
        );
    }

    /// A sweep too slow for even the estimate abandons the pass outright,
    /// declaration or none: the field is returned untouched.
    #[test]
    fn a_sweep_under_the_floor_with_no_declaration_is_left_alone() {
        let n = 72;
        let gates = 40;
        let azimuths = ring_azimuths(n);
        let orig: Vec<Vec<f64>> = (0..n)
            .map(|i| vec![if i < n / 2 { 3.0 } else { -3.0 }; gates])
            .collect();
        let vg = orig.clone();
        let sweep = sweep_for(&vg, &azimuths, gates);
        assert_eq!(fold_limit_ms(&sweep, &orig), None);
        let mut grid = orig.clone();
        dealias(&mut grid, &sweep, 0.5, None, DealiasProfile::NoFalseShear);
        assert_eq!(grid, orig);
    }

    // ---- velocity with no coherent solution --------------------------------

    /// A sweep of incoherent velocity beside a sweep that only folds, both
    /// built the same way and differing in one thing: whether the velocity
    /// under the wrap is a field or a coin toss.
    fn coherence_fixture(noise: bool) -> (Vec<Vec<f64>>, Vec<f64>, usize) {
        const VNY: f64 = 12.0;
        let (n, gates) = (360usize, 400usize);
        let azimuths = ring_azimuths(n);
        // A fixed multiplicative-congruential stream, so the fixture is the
        // same on every run and on every platform.
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 11) as f64 / (1u64 << 53) as f64
        };
        let grid: Vec<Vec<f64>> = (0..n)
            .map(|i| {
                (0..gates)
                    .map(|j| {
                        let v = if noise {
                            2.0 * VNY * next() - VNY
                        } else {
                            let truth = 10.0 * VNY * j as f64 / gates as f64
                                + 3.0 * (i as f64).to_radians().cos();
                            (truth + VNY).rem_euclid(2.0 * VNY) - VNY
                        };
                        (v / 0.5).round() * 0.5
                    })
                    .collect()
            })
            .collect();
        (grid, azimuths, gates)
    }

    /// Aliasing is not incoherence, and the statistic that separates them says
    /// so on a fixture where nothing else differs.
    #[test]
    fn a_fold_wall_is_coherent_and_a_coin_toss_is_not() {
        for (noise, want) in [(false, false), (true, true)] {
            let (grid, azimuths, gates) = coherence_fixture(noise);
            let sweep = sweep_for(&grid, &azimuths, gates);
            let rows = sweep_rows(&sweep, grid.len());
            let nyq = fold_limit_ms(&sweep, &grid).expect("a limit");
            let mask = incoherent_velocity(&grid, rows, gates, sweep.gate_interval_km, nyq);
            let refused = mask.iter().filter(|m| **m).count();
            let all = grid.len() * gates;
            if want {
                assert_eq!(refused, all, "every bin of a coin toss is refused");
            } else {
                assert_eq!(refused, 0, "a wrapping ramp is refused nowhere");
            }
        }
    }

    /// One physical field twice: as the short-pulse cut whose gates really are
    /// 0.5 km apart, and as the long-pulse cut that declares 0.25 km gates and
    /// fills each pair of them with one estimate.
    fn long_pulse_fixture(n: usize, cells: usize) -> (Vec<Vec<f64>>, Vec<Vec<f64>>, Vec<f64>) {
        const SPARSE: usize = 34;
        let azimuths = ring_azimuths(n);
        let coarse: Vec<Vec<f64>> = (0..n)
            .map(|i| {
                let base = 5.0 + 8.0 * azimuths[i].to_radians().cos();
                (0..cells)
                    .map(|k| {
                        if (i * 7 + k * 13) % SPARSE == 0 {
                            base + STRADDLE_VNY
                        } else {
                            base
                        }
                    })
                    .collect()
            })
            .collect();
        let replicated = coarse
            .iter()
            .map(|row| row.iter().flat_map(|&v| [v, v]).collect())
            .collect();
        (coarse, replicated, azimuths)
    }

    /// [`incoherent_velocity`] separates its samples by a physical distance
    /// converted through the declared gate spacing, and **not** by gate
    /// adjacency — which is the only reason long-pulse volumes do not defeat
    /// it.
    #[test]
    fn the_gate_separation_is_a_distance_and_not_a_gate_count() {
        let (n, cells) = (360usize, 400usize);
        let (coarse, replicated, azimuths) = long_pulse_fixture(n, cells);
        assert!(
            (0..cells).all(|k| replicated[0][2 * k] == replicated[0][2 * k + 1]),
            "the fixture must carry 500 m content on a declared 250 m grid",
        );

        let coarse_sweep = VelocitySweep {
            vel_grid: &coarse,
            azimuths_deg: &azimuths,
            gate_count: cells,
            first_gate_range_km: 2.125,
            gate_interval_km: 0.5,
            declared_nyquist_ms: Some(STRADDLE_VNY),
            status: None,
        };
        let rows = sweep_rows(&coarse_sweep, n);
        let refused = |grid: &[Vec<f64>], gc: usize, gi: f64| {
            incoherent_velocity(grid, rows, gc, gi, STRADDLE_VNY)
                .iter()
                .filter(|m| **m)
                .count()
        };

        // The field is incoherent at 500 m, whichever grid reports it.
        assert_eq!(
            refused(&coarse, cells, 0.5),
            n * cells,
            "the 0.5 km-gate cut must refuse every bin of it",
        );
        assert_eq!(
            refused(&replicated, 2 * cells, 0.25),
            n * 2 * cells,
            "and so must the same field replicated onto a 0.25 km grid",
        );

        // The counterfactual: one gate apart on the replicated grid, half of
        // every comparison is a value against its own copy.
        assert_eq!(
            refused(&replicated, 2 * cells, 0.5),
            0,
            "differencing adjacent gates on replicated content refuses nothing",
        );
    }

    /// What the dealiaser does with it: nothing at all. Not a value unfolded,
    /// not a gate censored, not a region dropped — the field comes back bit
    /// for bit as the radar reported it, which is the only honest answer where
    /// no assignment of fold branches explains it.
    #[test]
    fn the_dealiaser_hands_incoherent_velocity_back_as_reported() {
        let (orig, azimuths, gates) = coherence_fixture(true);
        let vg = orig.clone();
        let mut grid = orig.clone();
        dealias(
            &mut grid,
            &sweep_for(&vg, &azimuths, gates),
            0.5,
            None,
            DealiasProfile::NoFalseShear,
        );
        assert_eq!(grid, orig);
    }

    /// And rotation is not reported over it. Ungated, the estimator finds
    /// plenty there — a coin toss differentiates to whatever it likes — which
    /// is the whole reason the refusal is worth having.
    #[test]
    fn no_rotation_is_reported_over_velocity_with_no_coherent_solution() {
        let (grid, azimuths, gates) = coherence_fixture(true);
        let sweep = sweep_for(&grid, &azimuths, gates);
        let nrot = compute_nrot_grid(&sweep);
        assert!(
            nrot.iter().flatten().all(|v| v.is_nan()),
            "a coin toss carries no rotation to report",
        );
    }

    /// Region assignment is on by default in **both** shipped postures, at the
    /// thresholds the shipping arm was scored at.
    #[test]
    fn both_shipped_postures_assign_regions_at_the_measured_thresholds() {
        let s = RegionKnobs::SHIPPED;
        for p in [DealiasProfile::Coverage, DealiasProfile::NoFalseShear] {
            let k = p.knobs().region.expect("region assignment ships on");
            assert_eq!(
                (k.splits, k.region_min, k.min_pairs, k.anchor_min),
                (s.splits, s.region_min, s.min_pairs, s.anchor_min),
            );
            assert_eq!(
                (
                    k.int_tol,
                    k.pair_tol,
                    k.agree_frac,
                    k.contra_frac,
                    k.anchor_frac
                ),
                (
                    s.int_tol,
                    s.pair_tol,
                    s.agree_frac,
                    s.contra_frac,
                    s.anchor_frac
                ),
            );
            assert!(!k.anchor_largest && k.before_passes && !k.overrule);
        }
        // The ordering is most of the arm, and it was measured: run after the
        // passes instead of before them and interior recall is 2.08%, not 12.49%.
        assert!(s.before_passes && !s.overrule);
        // `anchor_largest` off is the strict posture — no evidence, no claim.
        assert!(!s.anchor_largest);

        // The revert path still runs, still produces a field, and still differs
        // on ground built to fold.
        let (ny, n, gc) = (20.0, 72usize, 200usize);
        let (_truth, wrapped) = folded_ramp(n, gc, ny);
        let az = ring_azimuths(n);
        let sweep = VelocitySweep {
            vel_grid: &wrapped,
            azimuths_deg: &az,
            gate_count: gc,
            first_gate_range_km: 2.125,
            gate_interval_km: 0.5,
            declared_nyquist_ms: Some(ny),
            status: None,
        };
        let mut off = wrapped.clone();
        let mut on = wrapped.clone();
        dealias_with_knobs(&mut off, &sweep, 0.5, None, coverage_knobs(None));
        dealias_with_knobs(&mut on, &sweep, 0.5, None, coverage_knobs(Some(s)));
        assert!(
            off.iter().flatten().any(|v| v.is_finite()),
            "the `None` path must still produce a field, not an empty one",
        );
        assert!(
            off != on,
            "if the two paths agree on a folded field the knob has stopped doing anything",
        );
    }

    /// The display profile is untouched by any of it. Storm-relative velocity
    /// is measured against the RPG's own dealiased field, which resolves
    /// everything present, so the refusal is NROT's posture and not the
    /// module's.
    #[test]
    fn the_display_profile_still_resolves_incoherent_velocity() {
        assert!(DealiasProfile::NoFalseShear.knobs().refuse_incoherent);
        assert!(!DealiasProfile::Coverage.knobs().refuse_incoherent);
        let (orig, azimuths, gates) = coherence_fixture(true);
        let vg = orig.clone();
        let mut grid = orig.clone();
        dealias(
            &mut grid,
            &sweep_for(&vg, &azimuths, gates),
            0.5,
            None,
            DealiasProfile::Coverage,
        );
        assert_ne!(grid, orig, "the coverage posture still works the field");
    }

    // ---- the mask is built once and read twice -----------------------------

    /// Every f64 of a grid as the bits it is, so a comparison is exact over
    /// NaN — which is most of an NROT grid, and exactly the part these tests
    /// are about.
    fn bits(grid: &[Vec<f64>]) -> Vec<Vec<u64>> {
        grid.iter()
            .map(|row| row.iter().map(|v| v.to_bits()).collect())
            .collect()
    }

    /// The fold limit [`STRADDLE_FIXTURE`] declares. Declared rather than
    /// estimated so the two thresholds the fixture has to sit between are
    /// fixed numbers and not a property of the noise.
    const STRADDLE_VNY: f64 = 25.0;

    /// A smooth wind sprinkled with lone gates a whole interval away from it —
    /// the one fixture on which [`incoherent_velocity`] is the *deciding*
    /// rule.
    fn straddle_fixture(n: usize, gates: usize) -> (Vec<Vec<f64>>, Vec<f64>) {
        const SPARSE: usize = 24;
        let azimuths = ring_azimuths(n);
        let grid = (0..n)
            .map(|i| {
                let base = 5.0 + 8.0 * azimuths[i].to_radians().cos();
                (0..gates)
                    .map(|j| {
                        if (i * 7 + j * 13) % SPARSE == 0 {
                            base + STRADDLE_VNY
                        } else {
                            base
                        }
                    })
                    .collect()
            })
            .collect();
        (grid, azimuths)
    }

    fn straddle_sweep<'a>(
        grid: &'a [Vec<f64>],
        azimuths: &'a [f64],
        gates: usize,
    ) -> VelocitySweep<'a> {
        VelocitySweep {
            vel_grid: grid,
            azimuths_deg: azimuths,
            gate_count: gates,
            first_gate_range_km: 2.125,
            gate_interval_km: 0.25,
            declared_nyquist_ms: Some(STRADDLE_VNY),
            status: None,
        }
    }

    /// [`dealias_with_knobs`] reports the mask it built, and reports **none**
    /// wherever it built none.
    #[test]
    fn the_dealiaser_reports_the_mask_it_built_and_nothing_it_did_not() {
        let (orig, azimuths, gates) = coherence_fixture(true);
        let vg = orig.clone();
        let sweep = sweep_for(&vg, &azimuths, gates);
        let rows = sweep_rows(&sweep, orig.len());
        let nyq = fold_limit_ms(&sweep, &orig).expect("a limit");
        let want = incoherent_velocity(&orig, rows, gates, sweep.gate_interval_km, nyq);
        assert!(want.iter().any(|m| *m), "the fixture must refuse something");

        let mut grid = orig.clone();
        let got = dealias(&mut grid, &sweep, 0.5, None, DealiasProfile::NoFalseShear);
        assert_eq!(
            got.as_deref(),
            Some(want.as_slice()),
            "the mask handed out is the mask the passes ran against",
        );

        // The posture that refuses nothing produced no mask to hand out.
        let mut grid = orig.clone();
        assert_eq!(
            dealias(&mut grid, &sweep, 0.5, None, DealiasProfile::Coverage),
            None,
            "a dealiasing that never asks the question reports no answer",
        );
    }

    /// The two early returns report no mask either — and they are the two the
    /// fallback in [`llsd_nrot`] exists for.
    #[test]
    fn a_dealiasing_that_returns_early_reports_no_mask() {
        // No fold limit: every gate is under `FOLD_LIMIT_FLOOR_MS`, so
        // `fold_limit_ms` abandons the pass before a mask is reachable.
        let calm: Vec<Vec<f64>> = vec![vec![1.5; 40]; 360];
        let calm_az = ring_azimuths(360);
        let vg = calm.clone();
        let sweep = sweep_for(&vg, &calm_az, 40);
        assert_eq!(fold_limit_ms(&sweep, &calm), None);
        let mut grid = calm.clone();
        assert_eq!(
            dealias(&mut grid, &sweep, 0.5, None, DealiasProfile::NoFalseShear),
            None,
        );

        // Too few rows to propagate a fold decision across.
        let (small, small_az) = straddle_fixture(7, 40);
        let vg = small.clone();
        let sweep = straddle_sweep(&vg, &small_az, 40);
        assert!(
            fold_limit_ms(&sweep, &small).is_some(),
            "this arm must fail on the row count and not on the limit",
        );
        let mut grid = small.clone();
        assert_eq!(
            dealias(&mut grid, &sweep, 0.5, None, DealiasProfile::NoFalseShear),
            None,
        );
    }

    /// Handed the dealiaser's mask, [`llsd_nrot`] produces exactly what it
    /// produces when it computes one itself — and neither is what an empty
    /// mask produces.
    #[test]
    fn the_reused_mask_and_the_recomputed_one_are_the_same_grid() {
        let (orig, azimuths) = straddle_fixture(360, 400);
        let vg = orig.clone();
        let sweep = straddle_sweep(&vg, &azimuths, 400);
        let pre = preprocess_velocity_with(&sweep, 0.5, None);
        let (dealiased, med) = (&pre.dealiased, &pre.median);
        let mask = pre
            .refused
            .clone()
            .expect("a full sweep with a limit produces a mask");
        assert!(mask.iter().any(|m| *m), "the fixture must refuse something");

        let reused = llsd_nrot(&sweep, dealiased, med, Some(&mask));
        let recomputed = llsd_nrot(&sweep, dealiased, med, None);
        assert_eq!(bits(&reused), bits(&recomputed));

        let empty = vec![false; orig.len() * 400];
        let unrefused = llsd_nrot(&sweep, dealiased, med, Some(&empty));
        assert!(
            unrefused.iter().flatten().any(|v| !v.is_nan()),
            "the fixture must paint once the refusal is taken away",
        );
        assert_ne!(
            bits(&reused),
            bits(&unrefused),
            "an empty mask must not be what an absent one means",
        );
    }

    /// End to end on a sweep the dealiaser will not run on: the refusal
    /// survives.
    #[test]
    fn a_sweep_too_small_to_dealias_still_refuses_incoherent_velocity() {
        let (grid, azimuths) = straddle_fixture(7, 400);
        let vg = grid.clone();
        let sweep = straddle_sweep(&vg, &azimuths, 400);
        let rows = sweep_rows(&sweep, 7);
        assert!(rows.closed, "the fixture must give the stencils neighbours");

        let pre = preprocess_velocity_with(&sweep, 0.5, None);
        let (dealiased, med) = (&pre.dealiased, &pre.median);
        assert_eq!(pre.refused, None, "seven rows is too few to dealias");

        let nyq = fold_limit_ms(&sweep, &grid).expect("a limit");
        let mask = incoherent_velocity(&grid, rows, 400, sweep.gate_interval_km, nyq);
        assert!(mask.iter().any(|m| *m), "the fixture must refuse something");

        let finish = |mut g: Vec<Vec<f64>>| {
            despeckle_nrot(&mut g, DESPECKLE_MIN_BINS, rows);
            g
        };
        let want = finish(llsd_nrot(&sweep, dealiased, med, Some(&mask)));
        let unrefused = finish(llsd_nrot(
            &sweep,
            dealiased,
            med,
            Some(&vec![false; 7 * 400]),
        ));
        let got = compute_nrot_grid_with_profile(&sweep, 0.5, None);

        assert!(
            unrefused.iter().flatten().any(|v| !v.is_nan()),
            "the fixture must paint once the refusal is taken away",
        );
        assert_eq!(bits(&got), bits(&want), "the mask was recomputed, not lost");
        assert_ne!(
            bits(&got),
            bits(&unrefused),
            "and it refused ground an empty mask would have painted",
        );
    }

    /// A sweep with no fold limit has no continuity ceiling either, and that
    /// is why [`range_texture`] does not run on one.
    #[test]
    fn a_sweep_with_no_fold_limit_has_no_continuity_ceiling() {
        let (n, gates) = (360usize, 400usize);
        let azimuths = ring_azimuths(n);
        let grid: Vec<Vec<f64>> = (0..n)
            .map(|i| {
                let base = 3.0 * azimuths[i].to_radians().cos();
                (0..gates)
                    .map(|j| base + if (j % 4) < 2 { 2.5 } else { -2.5 })
                    .collect()
            })
            .collect();
        let vg = grid.clone();
        let sweep = sweep_for(&vg, &azimuths, gates);
        assert_eq!(
            fold_limit_ms(&sweep, &grid),
            None,
            "the fixture must reach the arm this test is about",
        );

        let rows = sweep_rows(&sweep, n);
        let texture = range_texture(&grid, &sweep, rows);
        let coarsest = texture
            .iter()
            .flatten()
            .filter(|v| !v.is_nan())
            .fold(0.0f64, |a, &b| a.max(b));
        assert!(coarsest > 4.0, "the fixture must be rough: {coarsest}");

        let nrot = compute_nrot_grid_with_profile(&sweep, 0.5, None);
        assert!(
            nrot.iter().flatten().any(|v| !v.is_nan()),
            "with no limit there is no ceiling, so roughness refuses nothing",
        );
    }

    // ---- the row scratch is pooled, and no row can tell ---------------------

    /// A sweep of no gates is answered before either windowing stage splits
    /// its work.
    #[test]
    fn a_sweep_with_no_gates_is_answered_before_the_work_is_split() {
        let n = 360usize;
        let grid: Vec<Vec<f64>> = vec![Vec::new(); n];
        let azimuths = ring_azimuths(n);
        let sweep = straddle_sweep(&grid, &azimuths, 0);
        let rows = sweep_rows(&sweep, n);
        assert_eq!(
            fold_limit_ms(&sweep, &grid),
            Some(STRADDLE_VNY),
            "the declaration must stand so both stages are reached",
        );

        let empty: Vec<Vec<f64>> = vec![Vec::new(); n];
        assert_eq!(range_texture(&grid, &sweep, rows), empty);
        assert!(
            incoherent_velocity(&grid, rows, 0, sweep.gate_interval_km, STRADDLE_VNY).is_empty(),
            "no gates is no mask, not a mask of nothing",
        );
        assert_eq!(compute_nrot_grid(&sweep), empty);
    }

    /// [`range_texture`]'s range pass with a fresh prefix-sum pair per row,
    /// and the squared difference laid down in a pass of its own — the shape
    /// the pooled version has to agree with, bit for bit.
    fn range_texture_fresh_scratch(
        grid: &[Vec<f64>],
        sweep: &VelocitySweep,
        rows: crate::azimuth::Rows,
    ) -> Vec<Vec<f64>> {
        let n = grid.len();
        let gc = sweep.gate_count;
        let dk = ((TEXTURE_STEP_KM / sweep.gate_interval_km).round() as usize).max(1);
        let gh = ((TEXTURE_RANGE_HALF_KM / sweep.gate_interval_km).round() as i32).max(1);
        let per_row: Vec<(Vec<f32>, Vec<u16>)> = (0..n)
            .map(|i| {
                let mut d2 = vec![0.0f64; gc];
                let mut ok = vec![0u32; gc];
                for j in 0..gc.saturating_sub(dk) {
                    let (a, b) = (grid[i][j], grid[i][j + dk]);
                    if !a.is_nan() && !b.is_nan() {
                        d2[j] = (b - a).powi(2);
                        ok[j] = 1;
                    }
                }
                let mut pre = vec![0.0f64; gc + 1];
                let mut pcn = vec![0u32; gc + 1];
                for j in 0..gc {
                    pre[j + 1] = pre[j] + d2[j];
                    pcn[j + 1] = pcn[j] + ok[j];
                }
                let (mut sum, mut cnt) = (vec![0.0f32; gc], vec![0u16; gc]);
                for j in 0..gc {
                    let lo = (j as i32 - gh).max(0) as usize;
                    let hi = ((j as i32 + gh) as usize).min(gc - 1);
                    sum[j] = (pre[hi + 1] - pre[lo]) as f32;
                    cnt[j] = (pcn[hi + 1] - pcn[lo]) as u16;
                }
                (sum, cnt)
            })
            .collect();
        (0..n)
            .map(|i| {
                (0..gc)
                    .map(|j| {
                        let (mut s, mut c) = (0.0f64, 0u32);
                        for da in -TEXTURE_AZ_HALF..=TEXTURE_AZ_HALF {
                            if let Some(ai) = rows.neighbour(i, da) {
                                s += f64::from(per_row[ai].0[j]);
                                c += u32::from(per_row[ai].1[j]);
                            }
                        }
                        if (c as usize) < TEXTURE_MIN_PAIRS {
                            f64::NAN
                        } else {
                            (s / c as f64).sqrt()
                        }
                    })
                    .collect()
            })
            .collect()
    }

    /// [`incoherent_velocity`]'s range pass with a fresh prefix-sum pair per
    /// row, and the mask gathered per row rather than written flat.
    fn incoherent_velocity_fresh_scratch(
        raw: &[Vec<f64>],
        rows: crate::azimuth::Rows,
        gc: usize,
        gate_interval_km: f64,
        nyquist: f64,
    ) -> Vec<bool> {
        let n = raw.len();
        let tol = COH_STRADDLE_VNY_FRAC * nyquist;
        let fold = COH_FOLD_VNY_FRAC * nyquist;
        let dk = ((TEXTURE_STEP_KM / gate_interval_km).round() as usize).max(1);
        let per_row: Vec<(Vec<u16>, Vec<u16>)> = (0..n)
            .map(|i| {
                let mut ps = vec![0u32; gc + 1];
                let mut pp = vec![0u32; gc + 1];
                for j in 0..gc {
                    let (mut s, mut p) = (0u32, 0u32);
                    if j + dk < gc {
                        let (a, b) = (raw[i][j], raw[i][j + dk]);
                        if !a.is_nan() && !b.is_nan() {
                            p = 1;
                            let dv = (b - a).abs();
                            s = u32::from(dv > tol && dv < fold);
                        }
                    }
                    ps[j + 1] = ps[j] + s;
                    pp[j + 1] = pp[j] + p;
                }
                let (mut s, mut p) = (vec![0u16; gc], vec![0u16; gc]);
                for j in 0..gc {
                    let lo = (j as i32 - COH_RANGE_HALF).max(0) as usize;
                    let hi = ((j as i32 + COH_RANGE_HALF) as usize).min(gc - 1);
                    s[j] = (ps[hi + 1] - ps[lo]) as u16;
                    p[j] = (pp[hi + 1] - pp[lo]) as u16;
                }
                (s, p)
            })
            .collect();
        (0..n)
            .flat_map(|i| {
                (0..gc)
                    .map(|j| {
                        if raw[i][j].is_nan() {
                            return false;
                        }
                        let (mut s, mut p) = (0u32, 0u32);
                        for da in -COH_AZ_HALF..=COH_AZ_HALF {
                            if let Some(ai) = rows.neighbour(i, da) {
                                s += u32::from(per_row[ai].0[j]);
                                p += u32::from(per_row[ai].1[j]);
                            }
                        }
                        p > 0 && (s as f64) > COH_MAX_STRADDLE * p as f64
                    })
                    .collect::<Vec<bool>>()
            })
            .collect()
    }

    /// [`straddle_fixture`] with deterministic holes punched through it, so the
    /// present-pair counts vary along every row and between rows rather than
    /// standing at the window width everywhere.
    fn holed_straddle_fixture(n: usize, gates: usize) -> (Vec<Vec<f64>>, Vec<f64>) {
        let (mut grid, azimuths) = straddle_fixture(n, gates);
        for (i, row) in grid.iter_mut().enumerate() {
            for (j, v) in row.iter_mut().enumerate() {
                if (i * 11 + j * 5) % 37 < 4 {
                    *v = f64::NAN;
                }
            }
        }
        (grid, azimuths)
    }

    /// Both windowing stages hold one prefix-sum pair **per job the pool splits
    /// off** rather than one per row, and a row cannot tell.
    #[test]
    fn a_pooled_prefix_sum_and_a_fresh_one_are_the_same_row() {
        let (n, gates) = (360usize, 400usize);
        let (grid, azimuths) = holed_straddle_fixture(n, gates);
        assert!(
            grid.iter().flatten().any(|v| v.is_nan()),
            "the fixture must have holes for the counts to carry",
        );
        let vg = grid.clone();
        let sweep = straddle_sweep(&vg, &azimuths, gates);
        let rows = sweep_rows(&sweep, n);

        assert_eq!(
            bits(&range_texture(&grid, &sweep, rows)),
            bits(&range_texture_fresh_scratch(&grid, &sweep, rows)),
            "the texture a pooled scratch produces is the texture a fresh one does",
        );

        let interval = sweep.gate_interval_km;
        let pooled = incoherent_velocity(&grid, rows, gates, interval, STRADDLE_VNY);
        let fresh = incoherent_velocity_fresh_scratch(&grid, rows, gates, interval, STRADDLE_VNY);
        assert!(
            pooled.iter().any(|m| *m),
            "the fixture must refuse something"
        );
        assert!(
            !pooled.iter().all(|m| *m),
            "and must not refuse everything, or the comparison is one value",
        );
        assert_eq!(pooled, fresh);
    }

    /// The radial a near-zero gate is confirmed against is the one facing it,
    /// and the seed asks for it at 180° rather than at half the rows.
    #[test]
    fn a_sector_pairs_a_zero_across_the_diameter_it_scanned() {
        let n = 241;
        let gates = 40;
        let azimuths: Vec<f64> = (0..n).map(|i| 60.0 + i as f64).collect();
        let truth: Vec<f64> = azimuths
            .iter()
            .map(|a| 30.0 * a.to_radians().cos())
            .collect();
        let fold = |v: f64| (v + 25.0).rem_euclid(50.0) - 25.0;
        let mut grid: Vec<Vec<f64>> = truth.iter().map(|&v| vec![fold(v); gates]).collect();
        grid[30][0] = 25.0;
        let vg = grid.clone();
        dealias(
            &mut grid,
            &sweep_for(&vg, &azimuths, gates),
            0.5,
            None,
            DealiasProfile::NoFalseShear,
        );

        for (i, row) in grid.iter().enumerate() {
            for (j, &v) in row.iter().enumerate() {
                if (i, j) == (30, 0) {
                    assert!(v.is_nan(), "the pinned spike survived as {v}");
                    continue;
                }
                assert!(
                    (v - truth[i]).abs() < 1e-9,
                    "az {} gate {j} read {v}, not the {} its wind carries",
                    azimuths[i],
                    truth[i],
                );
            }
        }
    }

    /// Which radial faces which, on a rotation and on an arc.
    #[test]
    fn a_radial_faces_the_one_the_antenna_pointed_at_or_none() {
        for n in [360usize, 720] {
            let rows = rows_for(&ring_azimuths(n), n);
            assert_eq!(half_turn_rows(rows), (n / 2) as i32);
            for i in 0..n {
                assert_eq!(
                    rows.neighbour(i, half_turn_rows(rows)),
                    Some((i + n / 2) % n)
                );
            }
        }

        // 36° of 0.5° rows: 360 rows of *this* grid would be a half circle,
        // and it has 72.
        let sector: Vec<f64> = (0..72).map(|i| f64::from(i) * 0.5).collect();
        let rows = rows_for(&sector, 72);
        assert_eq!(half_turn_rows(rows), 360);
        for i in 0..72 {
            assert_eq!(rows.neighbour(i, 360), None);
            assert_eq!(rows.neighbour(i, -360), None);
        }

        // 241 rows of 1.0° covering az 60°..300°.
        let arc: Vec<f64> = (0..241).map(|i| 60.0 + f64::from(i)).collect();
        let rows = rows_for(&arc, 241);
        assert_eq!(half_turn_rows(rows), 180);
        assert_eq!(rows.neighbour(0, 180), Some(180));
        assert_eq!(rows.neighbour(240, 180), None);
        assert_eq!(rows.neighbour(240, -180), Some(60));
        let facing = |i: usize| rows.neighbour(i, 180).or_else(|| rows.neighbour(i, -180));
        assert_eq!((0..241).filter(|&i| facing(i).is_none()).count(), 119);
    }

    #[test]
    fn a_closed_sweeps_dealias_neighbours_are_the_wrap_they_always_were() {
        for n in [360usize, 720] {
            let rows = rows_for(&ring_azimuths(n), n);
            assert!(rows.closed);
            let half = half_turn_rows(rows);
            for i in 0..n {
                for d in (-39..=39).chain(half..=half + 3).chain([-half]) {
                    assert_eq!(
                        rows.neighbour(i, d),
                        Some((i as i32 + d).rem_euclid(n as i32) as usize),
                        "row {i} offset {d}",
                    );
                }
            }
        }
    }

    /// The median filter's job in this pipeline: a single-bin velocity spike
    /// disappears; the surrounding field survives.
    #[test]
    fn median_filter_removes_an_isolated_spike() {
        let n = 40;
        let gates = 40;
        let mut grid: Vec<Vec<f64>> = vec![vec![10.0; gates]; n];
        grid[20][20] = 90.0;
        let azs = ring_azimuths(n);
        let filtered = median_filter(&grid, &grid, None, gates, 0.25, 0.25, rows_for(&azs, n));
        assert_eq!(filtered[20][20], 10.0);
        assert_eq!(filtered[10][10], 10.0);
    }

    /// A window the fold censor has emptied has no median, and the linear one
    /// it used to report stood on neither branch of what was left.
    #[test]
    fn a_median_window_the_censor_emptied_reports_nothing() {
        let n = 40;
        let gates = 40;
        // Raw is complete: the sky was sampled, so the raw cliff is not what
        // decides this.
        let raw: Vec<Vec<f64>> = vec![vec![30.0; gates]; n];
        let mut deal: Vec<Vec<f64>> = vec![vec![f64::NAN; gates]; n];
        let survivors = [
            (18, 19, 31.0),
            (18, 20, 30.0),
            (19, 18, -31.0),
            (19, 19, 30.0),
            (20, 20, 30.0),
            (21, 20, 0.0),
            (21, 21, -30.0),
            (22, 18, -30.0),
            (22, 20, 0.0),
        ];
        for (i, j, v) in survivors {
            deal[i][j] = v;
        }
        let azs = ring_azimuths(n);
        // 0.5 km first gate and 0.5 km gates put az_half at its cap of 2, so
        // the window is the 5 × 5 the reading above was taken over.
        let rows = rows_for(&azs, n);
        let filtered = median_filter(&deal, &raw, None, gates, 0.5, 0.5, rows);
        assert_eq!(deal[20][20], 30.0, "the centre carries a dealiased value");
        assert!(
            filtered[20][20].is_nan(),
            "9 of 25 cells is under MEDIAN_MIN_DEALIASED_OCC, so there is no \
             neighbourhood to take a median of; got {}",
            filtered[20][20]
        );
        // A window the censor left alone still reports, at the same occupancy
        // floor the raw cliff has always allowed.
        let full: Vec<Vec<f64>> = vec![vec![30.0; gates]; n];
        assert_eq!(
            median_filter(&full, &raw, None, gates, 0.5, 0.5, rows)[20][20],
            30.0
        );
    }

    /// [`MEDIAN_MIN_RAW_OCC`] counts **echo**, and a below-threshold gate is
    /// not echo however plainly the radar looked at it.
    #[test]
    fn the_raw_occupancy_cliff_counts_echo_and_not_illumination() {
        use crate::types::GateReport;
        use nexrad_model::data::{MomentData, Radial, RadialStatus};

        // Velocity's own codec: raw 0 is below threshold, 1 is range folded,
        // and 2 upward is a number.
        const SCALE: f32 = 2.0;
        const OFFSET: f32 = 129.0;
        // 50 km out, 250 m gates and half a degree of spacing put az_half at
        // its floor of 1, so the centre's window is 3 rows × 5 gates = 15.
        const FIRST_GATE_M: u16 = 50_000;
        const GATE_M: u16 = 250;
        let rows_bytes: [[u8; 5]; 3] = [
            [200, 200, 200, 0, 0],
            [200, 200, 200, 0, 0],
            [200, 200, 0, 0, 0],
        ];
        let radials: Vec<Radial> = rows_bytes
            .iter()
            .enumerate()
            .map(|(i, bytes)| {
                Radial::new(
                    0,
                    i as u16,
                    i as f32 * 0.5,
                    0.5,
                    RadialStatus::IntermediateRadialData,
                    1,
                    0.5,
                    None,
                    Some(MomentData::from_fixed_point(
                        bytes.len() as u16,
                        FIRST_GATE_M,
                        GATE_M,
                        8,
                        SCALE,
                        OFFSET,
                        bytes.to_vec(),
                    )),
                    None,
                    None,
                    None,
                    None,
                    None,
                )
            })
            .collect();
        let grid = crate::velocity::grid(&radials).expect("the radials carry velocity");
        let sweep = grid.sweep(None);
        assert!(
            sweep.status.is_some(),
            "a decoded sweep's view carries the report plane",
        );

        for row in &grid.status {
            for report in row {
                assert_ne!(
                    *report,
                    GateReport::NotReported,
                    "the radar reported every gate of this window",
                );
                assert!(report.is_measured());
            }
        }
        assert!(
            grid.status[0].contains(&GateReport::BelowThreshold),
            "and seven of them are the decoder's measurement of emptiness",
        );

        let rows = sweep_rows(&sweep, radials.len());
        let filtered = median_filter(
            &grid.values,
            &grid.values,
            sweep.status,
            grid.gate_count,
            grid.first_gate_range_km,
            grid.gate_interval_km,
            rows,
        );
        assert!(
            grid.values[1][2].is_finite(),
            "precondition: the centre itself carries echo",
        );
        assert!(
            filtered[1][2].is_nan(),
            "8 of 15 cells carry echo, under MEDIAN_MIN_RAW_OCC; got {}",
            filtered[1][2]
        );
        // And the `None` arm is the same predicate, not a weaker one: a sweep
        // with no plane reads identically cell for cell.
        let planeless = median_filter(
            &grid.values,
            &grid.values,
            None,
            grid.gate_count,
            grid.first_gate_range_km,
            grid.gate_interval_km,
            rows,
        );
        assert!(
            planeless
                .iter()
                .flatten()
                .zip(filtered.iter().flatten())
                .all(|(a, b)| (a.is_nan() && b.is_nan()) || a == b),
            "finiteness selects the same cells GateReport::Value does",
        );
    }

    /// The divisor curve is the reference's own, read off it at 60 ranges on
    /// six sites. Check the knots, a mid-segment interpolation, and both flat
    /// extensions.
    #[test]
    fn rot_divisor_matches_the_reference_curve() {
        // Flat below the first knot — unreachable in the pipeline, which skips
        // everything inside MIN_RANGE_NM (13.06 km), one gate under it.
        assert_eq!(rot_divisor_km(10.0), 22.43);
        assert_eq!(rot_divisor_km(13.1), 22.43);
        assert_eq!(rot_divisor_km(22.0), 23.97);
        assert!((rot_divisor_km(17.5) - 23.285).abs() < 0.001); // 16→19 segment
        assert_eq!(rot_divisor_km(40.0), 20.57);
        assert_eq!(rot_divisor_km(60.0), 12.93);
        assert!((rot_divisor_km(72.5) - 10.16).abs() < 0.005); // 70→75 segment
        assert_eq!(rot_divisor_km(80.0), 8.62);
        // The corner is at 81.5 km, where the fall of 2.6%/km before it gives
        // way to 0.27%/km after.
        assert_eq!(rot_divisor_km(81.5), 8.31);
        assert!((rot_divisor_km(83.25) - 8.27).abs() < 0.005); // 81.5→85 segment
        assert_eq!(rot_divisor_km(85.0), 8.23);
        assert_eq!(rot_divisor_km(250.0), 8.23); // flat beyond the last knot
        // The nm entry point converts and lands on the same curve.
        assert_eq!(rot_divisor(40.0 / KM_PER_NM), 20.57);
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
        let nrot = llsd_nrot(&s, &grid, &grid, None);

        // Gate 200 → 50.25 km: inside the super-res operator's domain.
        let range_nm = (0.25 + 200.0 * 0.25) / KM_PER_NM;
        let gain: f64 = SPLIT_TAPS.iter().map(|&(o, t)| o as f64 * t).sum();
        let expected = k * gain / rot_divisor(range_nm);
        let got = nrot[10][200];
        assert!(
            (got - expected).abs() < 0.03,
            "NROT {got} != expected {expected}"
        );
    }

    /// The rows `sweep_rows` reports for `azimuths`, without a grid to hang
    /// them on: only the azimuths and the count decide.
    fn rows_for(azimuths: &[f64], n: usize) -> crate::azimuth::Rows {
        let grid: Vec<Vec<f64>> = Vec::new();
        sweep_rows(&sweep(&grid, azimuths, 0), n)
    }

    /// Every complete cut is read exactly as it always was, in both halves of
    /// the answer.
    #[test]
    fn a_closed_sweep_is_read_exactly_as_it_always_was() {
        for n in [360usize, 720] {
            assert_eq!(rows_for(&ring_azimuths(n), n).step_deg, 360.0 / n as f64);
        }

        // Collection order starts wherever the antenna was.
        let rolled: Vec<f64> = (0..720).map(|i| f64::from((i + 431) % 720) * 0.5).collect();
        assert_eq!(rows_for(&rolled, 720).step_deg, 0.5);

        let jittered: Vec<f64> = (0..720)
            .map(|i| i as f64 * 0.5 + 0.02 * (i as f64 * 1.7).sin())
            .collect();
        assert_eq!(rows_for(&jittered, 720).step_deg, 0.5);

        // One radial dropped is a hole in a rotation, not a sector: 719 × 0.5°
        // still accounts for 359.5° of the 360.
        let dropped: Vec<f64> = ring_azimuths(720)
            .into_iter()
            .filter(|a| *a != 100.0)
            .collect();
        assert_eq!(rows_for(&dropped, 719).step_deg, 360.0 / 719.0);

        // The seam.
        let rows = rows_for(&jittered, 720);
        let half = PROFILE_MAX_HALF as i32;
        for i in 0..720 {
            for d in -half..=half {
                assert_eq!(
                    rows.neighbour(i, d),
                    Some((i as i32 + d).rem_euclid(720) as usize),
                    "row {i} offset {d}",
                );
            }
        }
    }

    /// The same physical shear presented twice — 0.5° radials all the way
    /// round, and the first 36° of them standing alone — reports the same
    /// rotation.
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

        let full_nrot = llsd_nrot(&sweep(&full, &full_az, gates), &full, &full, None);
        let sector_nrot = llsd_nrot(&sweep(&sector, &sector_az, gates), &sector, &sector, None);

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

        let range_nm = (0.25 + 200.0 * 0.25) / KM_PER_NM;
        let gain: f64 = SPLIT_TAPS.iter().map(|&(o, t)| o as f64 * t).sum();
        let expected = k * gain / rot_divisor(range_nm);
        let got = sector_nrot[30][200];
        assert!(
            (got - expected).abs() < 0.03,
            "the sector read NROT {got}, not the {expected} its shear carries \
             (over a 5° row it would read {:.3})",
            expected / 10.0,
        );
    }

    /// A sector has two edges, and neither of them is a place where anything
    /// rotates.
    #[test]
    fn a_sectors_edges_read_no_data_rather_than_its_far_end() {
        let gates = 400;
        let k = 6.0;
        let azimuths: Vec<f64> = (0..72).map(|i| f64::from(i) * 0.5).collect();
        let grid: Vec<Vec<f64>> = azimuths
            .iter()
            .map(|az| {
                let dtheta = az.to_radians();
                (0..gates)
                    .map(|j| k * (0.25 + j as f64 * 0.25) * dtheta)
                    .collect()
            })
            .collect();
        let nrot = llsd_nrot(&sweep(&grid, &azimuths, gates), &grid, &grid, None);

        let gain: f64 = SPLIT_TAPS.iter().map(|&(o, t)| o as f64 * t).sum();
        for j in 100..300 {
            let range_km = 0.25 + j as f64 * 0.25;
            let expected = k * gain / rot_divisor(range_km / KM_PER_NM);
            for i in (0..5).chain(67..72) {
                assert!(
                    nrot[i][j].is_nan(),
                    "row {i} gate {j} read {} past the arc's edge",
                    nrot[i][j],
                );
            }
            for (i, row) in nrot.iter().enumerate().take(67).skip(5) {
                assert!(
                    (row[j] - expected).abs() < 1e-9,
                    "row {i} gate {j} read {}, not the {expected} its shear carries",
                    row[j],
                );
            }
        }
        assert!(
            nrot.iter().flatten().all(|v| v.is_nan() || v.abs() < 1.0),
            "a sector of pure shear painted a rotation",
        );
    }

    /// The sector rule and the legacy-resolution operator meet on the same
    /// sweep, and this is that sweep: 72 rows of 1.0°, which is every TDWR cut
    /// there is.
    #[test]
    fn a_legacy_resolution_sector_reads_its_arc_and_stops() {
        let gates = 400;
        let k = 6.0;
        let row = |az_deg: f64| -> Vec<f64> {
            let theta = az_deg.to_radians();
            let dtheta = if theta > PI { theta - 2.0 * PI } else { theta };
            (0..gates)
                .map(|j| k * (0.25 + j as f64 * 0.25) * dtheta)
                .collect()
        };
        let full_az = ring_azimuths(360);
        let sector_az = full_az[..72].to_vec();
        let full: Vec<Vec<f64>> = full_az.iter().map(|&a| row(a)).collect();
        let sector: Vec<Vec<f64>> = sector_az.iter().map(|&a| row(a)).collect();
        assert!(
            !rows_are_half_degree_pairs(&sector_az),
            "a 1.0° sector found a pairing"
        );

        let full_nrot = llsd_nrot(&sweep(&full, &full_az, gates), &full, &full, None);
        let sector_nrot = llsd_nrot(&sweep(&sector, &sector_az, gates), &sector, &sector, None);

        let legacy_gain: f64 = LEGACY_TAPS.iter().map(|&(o, t)| 2.0 * o as f64 * t).sum();
        let mut carried = 0;
        for j in 100..300 {
            let range_km = 0.25 + j as f64 * 0.25;
            let expected = k * legacy_gain / rot_divisor(range_km / KM_PER_NM);
            for i in (0..5).chain(67..72) {
                assert!(
                    sector_nrot[i][j].is_nan(),
                    "row {i} gate {j} read {} past the arc's edge",
                    sector_nrot[i][j],
                );
            }
            for i in 5..67 {
                let (s, f) = (sector_nrot[i][j], full_nrot[i][j]);
                assert!(
                    s == f,
                    "row {i} gate {j}: the sector read {s}, the rotation {f}",
                );
                assert!(
                    (s - expected).abs() < 1e-9,
                    "row {i} gate {j} read {s}, not the {expected} its own taps carry",
                );
                carried += 1;
            }
        }
        assert_eq!(carried, 62 * 200, "the compared window read mostly ND");
        assert!(
            sector_nrot
                .iter()
                .flatten()
                .all(|v| v.is_nan() || v.abs() < 1.0),
            "a 1.0° sector of pure shear painted a rotation",
        );
    }

    /// A sweep far too small for anything here to read runs the whole pipeline
    /// and reports nothing, rather than dividing an arc down to nothing or
    /// indexing off its own end.
    #[test]
    fn a_sweep_too_small_for_a_stencil_reads_nothing() {
        let grid = vec![vec![10.0; 200]; 3];
        let azs = vec![0.0, 0.5, 1.0];
        let out = compute_nrot_grid(&sweep(&grid, &azs, 200));
        assert!(out.iter().flatten().all(|v| v.is_nan()));
    }

    /// Every stencil divisor in this module counts **rows of the grid** and
    /// not degrees of sky, and one shear sampled at 0.5° and at 1.0° is what
    /// shows it.
    #[test]
    fn one_shear_reads_the_gain_its_own_operator_carries() {
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
        let fine_nrot = llsd_nrot(&sweep(&fine, &fine_az, gates), &fine, &fine, None);
        let coarse_nrot = llsd_nrot(&sweep(&coarse, &coarse_az, gates), &coarse, &coarse, None);

        let split_gain: f64 = SPLIT_TAPS.iter().map(|&(o, t)| o as f64 * t).sum::<f64>();
        let legacy_gain: f64 = LEGACY_TAPS.iter().map(|&(o, t)| 2.0 * o as f64 * t).sum();
        // 25.25/50.25/75.25 km, and 95.25 km — the last one past where a second
        // stencil used to take both grids over.
        for j in [100usize, 200, 300, 380] {
            let range_km = 0.25 + j as f64 * 0.25;
            let divisor = rot_divisor(range_km / KM_PER_NM);
            for (label, got, gain) in [
                ("0.5°", fine_nrot[180][j], split_gain),
                ("1.0°", coarse_nrot[90][j], legacy_gain),
            ] {
                let expect = k * gain / divisor;
                assert!(
                    (got - expect).abs() < 1e-9,
                    "{label} gate {j}: read {got}, not the {expect} a {k} (m/s)/km \
                     ramp carries through its own taps",
                );
            }
        }

        // The whole of the difference is the ratio of those two gains —
        // 1.0115 — at every range, and none of it is the divisor.
        for j in [100usize, 200, 300, 380] {
            let ratio = coarse_nrot[90][j] / fine_nrot[180][j];
            assert!(
                (ratio - legacy_gain / split_gain).abs() < 1e-9,
                "gate {j}: coarse/fine {ratio}, not the gain ratio {}",
                legacy_gain / split_gain,
            );
        }

        // A third sampling, and the one that keeps the divisor honest.
        assert!(
            rows_are_half_degree_pairs(&ring_azimuths(1440)),
            "a 0.25° sweep stopped pairing, and this probe stopped probing",
        );
        let probe_gates = 250; // to 62.75 km — inside the split band, whole
        let quarter_az = ring_azimuths(1440);
        let quarter: Vec<Vec<f64>> = quarter_az
            .iter()
            .map(|&az_deg| {
                let theta = az_deg.to_radians();
                let dtheta = if theta > PI { theta - 2.0 * PI } else { theta };
                (0..probe_gates)
                    .map(|j| k * (0.25 + j as f64 * 0.25) * dtheta)
                    .collect()
            })
            .collect();
        let quarter_nrot = llsd_nrot(
            &sweep(&quarter, &quarter_az, probe_gates),
            &quarter,
            &quarter,
            None,
        );
        for j in [100usize, 200] {
            let range_km = 0.25 + j as f64 * 0.25;
            let expect = k * split_gain / rot_divisor(range_km / KM_PER_NM);
            let got = quarter_nrot[360][j]; // az 90°, clear of the field's wrap
            assert!(
                (got - expect).abs() < 1e-9,
                "0.25° gate {j}: read {got}, not the {expect} the same taps carry \
                 over this grid's own two rows — the divisor read a degree",
            );
            assert!(
                (got - fine_nrot[180][j]).abs() < 1e-12,
                "0.25° gate {j} read {got} where 0.5° read {}",
                fine_nrot[180][j],
            );
        }
    }

    /// A 1.0°-spaced sweep has no pairing, and the measurement says so.
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
            assert!(
                !rows_are_half_degree_pairs(azs),
                "a 1.0° sweep reported a pairing"
            );
        }

        assert!(rows_are_half_degree_pairs(&ring_azimuths(720)));
        let rolled: Vec<f64> = (0..720).map(|i| f64::from(i) * 0.5 + 0.5).collect();
        assert!(rows_are_half_degree_pairs(&rolled));

        let sector: Vec<f64> = (0..120).map(|i| 30.0 + f64::from(i) * 0.5).collect();
        assert!(rows_are_half_degree_pairs(&sector));
    }

    /// Where the antenna happened to start a cut is not a property of the
    /// weather, so it must not move the rotation. It does not: no reader in
    /// this module takes a radial's index parity any more, so a sweep rolled
    /// by one radial reads bit for bit what it read before.
    #[test]
    fn a_sweep_reads_the_same_wherever_collection_began() {
        let gates = 224; // to 56.0 km — the split-tap band, whole
        let j = 199; // 50.0 km, through the vortex core
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
            let nrot = llsd_nrot(&sweep(&grid, &azs, gates), &grid, &grid, None);
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
        // 713 until the operator's shape was corrected, then 714; 715 since
        // [`GK_MIN_R2`] came down to the floor the six-site step ladder admits.
        assert_eq!(compared, 715, "the compared row read mostly ND");

        let coarse = (read(360, 1.0, 0), read(360, 1.0, 1));
        let mut compared = 0;
        for (k, (&a, &b)) in coarse.0.iter().zip(coarse.1.iter()).enumerate() {
            assert!(
                a == b || (a.is_nan() && b.is_nan()),
                "1.0° az {k}°: unrolled read {a}, rolled read {b}",
            );
            compared += usize::from(a.is_finite());
        }
        assert_eq!(compared, 358, "the compared row read mostly ND");
    }

    /// The reference's own per-radial profiles over a 1.0°-spaced cut, which
    /// are what [`LEGACY_TAPS`] was solved from.
    #[test]
    fn a_legacy_resolution_sweep_reads_the_reference_profiles() {
        let gates = 400;
        let n = 360;
        let azimuths = ring_azimuths(n); // whole degrees: no pairing to find
        // −8 below each boundary, +8 above; first +8 radial at 101 (odd index)
        // and at 122 (even), so the two steps sit at opposite parity.
        let vel = |i: usize| -> f64 {
            match i {
                91..=100 | 122..=131 => -8.0,
                101..=121 => 8.0,
                137..=139 | 158..=160 => -10.0,
                140..=142 | 161..=163 => 10.0,
                _ => 0.0,
            }
        };
        let grid: Vec<Vec<f64>> = (0..n)
            .map(|i| {
                (0..gates)
                    .map(|j| {
                        let r = 0.25 + j as f64 * 0.25;
                        if r >= 60.0 {
                            return 23.0 * f64::from(i as u16).to_radians().cos();
                        }
                        if (30.0..47.0).contains(&r) {
                            vel(i)
                        } else {
                            0.0
                        }
                    })
                    .collect()
            })
            .collect();
        let nrot = llsd_nrot(&sweep(&grid, &azimuths, gates), &grid, &grid, None);
        let j = 154; // 38.75 km, mid-band, where the reference was hovered
        let at = |i: usize| nrot[i][j];

        let agrees = |got: f64, want: f64| {
            (got - want).abs() < 0.04 || (got.is_nan() && want.abs() < SIGNIFICANT)
        };

        // Both steps, both parities, against the hovered ∓0.10 / ±0.69.
        for (first, sign) in [(101usize, 1.0), (122usize, -1.0)] {
            for (offset, want) in [(-2i32, -0.10), (-1, 0.69), (0, 0.69), (1, -0.10)] {
                let i = (first as i32 + offset) as usize;
                let (got, want) = (at(i), sign * want);
                assert!(
                    agrees(got, want),
                    "step at {first}: radial {i} read {got:.4}, reference {want:.2}",
                );
            }
            for i in [first - 3, first + 2] {
                assert!(
                    at(i).is_nan(),
                    "radial {i} painted where the reference is ND"
                );
            }
        }

        // Both couplets, both parities, against the hovered ten-radial profile.
        const COUPLET: [f64; 10] = [
            0.06, -0.45, -0.45, -0.06, 0.89, 0.89, -0.06, -0.45, -0.45, 0.06,
        ];
        for first in [140usize, 161] {
            for (k, &want) in COUPLET.iter().enumerate() {
                let i = first - 5 + k;
                let got = at(i);
                assert!(
                    agrees(got, want),
                    "couplet at {first}: radial {i} read {got:.4}, reference {want:.2}",
                );
            }
            for i in [first - 6, first + 5] {
                assert!(
                    at(i).is_nan(),
                    "radial {i} painted where the reference is ND"
                );
            }
        }

        // The two parities are not merely each within a quantum of the
        // reference: they are the same field.
        for k in 0..10 {
            let (a, b) = (at(135 + k), at(156 + k));
            assert!(
                a == b || (a.is_nan() && b.is_nan()),
                "couplet radial {k}: even parity read {a}, odd parity read {b}",
            );
        }
    }

    /// A patch whose velocity varies but carries no coherent azimuthal trend
    /// is noise, and the reference reports nothing at such bins even where it
    /// has good velocity.
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
        let nrot = llsd_nrot(&s, &grid, &grid, None);
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

    /// Rotation is reported only over velocity that is continuous along the
    /// beam, and "continuous" is measured against the cut's own fold limit.
    #[test]
    fn rotation_is_reported_only_over_velocity_continuous_along_the_beam() {
        let n = 720;
        let gates = 400;
        let azimuths = ring_azimuths(n);
        let j = 155; // 38.75 km, where the ladder was hovered
        const VNY: f64 = 11.66; // KHNX's declared limit
        const AMP: f64 = 5.0;
        const CORE: usize = 100;

        let paint = |bump: f64| -> Vec<Vec<f64>> {
            (0..n)
                .map(|i| {
                    let d = i as i64 - CORE as i64;
                    let pole = if (0..3).contains(&d) {
                        AMP
                    } else if (-3..0).contains(&d) {
                        -AMP
                    } else {
                        0.0
                    };
                    (0..gates)
                        .map(|g| pole + if (g / 2) % 2 == 0 { bump } else { -bump })
                        .collect()
                })
                .collect()
        };
        let read = |grid: &[Vec<f64>], nyquist: f64| -> f64 {
            let mut s = sweep(grid, &azimuths, gates);
            s.declared_nyquist_ms = Some(nyquist);
            llsd_nrot(&s, grid, grid, None)[CORE][j]
        };

        let ceiling = GK_MAX_TEXTURE_VNY_FRAC * VNY;
        let (under, over) = (0.4 * ceiling, 0.7 * ceiling); // textures 0.8× and 1.4×
        let flat = paint(0.0);
        let quiet = paint(under);
        let broken = paint(over);

        let clean = read(&flat, VNY);
        assert!(
            clean.abs() >= SIGNIFICANT,
            "precondition: the couplet itself reads {clean:.3}",
        );
        let kept = read(&quiet, VNY);
        assert!(
            kept.abs() >= SIGNIFICANT,
            "a discontinuity at {:.2} of the limit blanked the couplet: read {kept:.3}",
            2.0 * under / VNY,
        );
        let dropped = read(&broken, VNY);
        assert!(
            dropped.is_nan(),
            "a discontinuity at {:.2} of the limit still read {dropped:.3}",
            2.0 * over / VNY,
        );

        // The same grid, twice the declared limit: the ceiling is a multiple of
        // what the cut says it can measure, not a velocity.
        let wider = read(&broken, 2.0 * VNY);
        assert!(
            !wider.is_nan() && wider.abs() >= SIGNIFICANT,
            "a cut declaring {:.2} refused what {:.2} of its own limit allows: read {wider:.3}",
            2.0 * VNY,
            2.0 * over / (2.0 * VNY),
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
    /// [`SPLIT_TAPS`] reproduces the reference's measured per-radial step
    /// profile, **and reads a step at a whole degree exactly as it reads one
    /// at a half degree**.
    #[test]
    fn split_stencil_matches_the_measured_step_profile() {
        let n = 720;
        let gates = 800; // to 200.25 km, so a gate past 80 km is a gate here
        let azimuths = ring_azimuths(n); // i·0.5°, pairs at whole degrees
        // The step response is the operator's tail sums, outermost first.
        let tail: Vec<f64> = (0..SPLIT_TAPS.len())
            .map(|m| SPLIT_TAPS[m..].iter().map(|&(_, t)| t).sum())
            .collect();

        for (boundary_az, first_plus) in [(45.0, 90usize), (45.5, 91usize)] {
            let grid: Vec<Vec<f64>> = (0..n)
                .map(|i| vec![if azimuths[i] < boundary_az { -8.0 } else { 8.0 }; gates])
                .collect();
            let s = sweep(&grid, &azimuths, gates);
            let nrot = llsd_nrot(&s, &grid, &grid, None);
            // 38.5 km, and 95.25 and 175.25 km — both past the range a second
            // operator used to take the bin over at.
            for j in [153usize, 380, 700] {
                let range_km = 0.25 + j as f64 * 0.25;
                let arc_legacy = range_km * 1.0_f64.to_radians();
                let scale = 16.0 / arc_legacy / rot_divisor_km(range_km);
                let class = [
                    tail[0] * scale,
                    tail[1] * scale,
                    tail[2] * scale,
                    tail[3] * scale,
                ];
                for (radial, expect) in [
                    (first_plus - 1, class[0]),
                    (first_plus, class[0]),
                    (first_plus - 2, class[1]),
                    (first_plus + 1, class[1]),
                    (first_plus - 3, class[2]),
                    (first_plus + 2, class[2]),
                    (first_plus - 4, class[3]),
                    (first_plus + 3, class[3]),
                ] {
                    let got = nrot[radial][j];
                    let core = expect == class[0];
                    assert!(
                        (got - expect).abs() < 0.02 || (!core && got.is_nan()),
                        "az {boundary_az}, gate {j}, radial {radial}: got \
                         {got:.3}, expected {expect:.3}{}",
                        if core { "" } else { " or ND" },
                    );
                }
                for radial in [
                    first_plus - 6,
                    first_plus - 5,
                    first_plus + 4,
                    first_plus + 5,
                ] {
                    let got = nrot[radial][j];
                    assert!(
                        got.is_nan() || got.abs() < 0.02,
                        "az {boundary_az}, gate {j}, radial {radial}: got \
                         {got:.3}, expected ~0"
                    );
                }
            }
        }
    }

    /// A couplet reads the operator its own step response fixes — at every
    /// pole width, at both boundary parities, and with no compression stage
    /// between.
    #[test]
    fn a_couplet_reads_the_operator_its_own_step_response_fixes() {
        let n = 720;
        let gates = 400;
        let azimuths = ring_azimuths(n);
        let j = 155; // 38.75 km, mid-band, where the reference was hovered
        const AMP: f64 = 5.0;

        // Per radial outward from the centre, as hovered.
        type Profile = [Option<f64>; 6];
        /// An asymmetric couplet's two sides are two profiles: the weak pole
        /// is a shorter step, and the reference reads each side accordingly.
        type Sides = (f64, [Option<f64>; 5], [Option<f64>; 5]);
        const SYMMETRIC: [(usize, Profile); 5] = [
            (
                2,
                [
                    Some(0.30),
                    Some(0.14),
                    Some(-0.18),
                    Some(-0.26),
                    None,
                    Some(0.06),
                ],
            ),
            (
                3,
                [
                    Some(0.49),
                    Some(0.14),
                    Some(-0.18),
                    Some(-0.34),
                    Some(-0.14),
                    None,
                ],
            ),
            (
                4,
                [
                    Some(0.53),
                    Some(0.30),
                    Some(-0.10),
                    Some(-0.34),
                    Some(-0.22),
                    Some(-0.14),
                ],
            ),
            (
                5,
                [
                    Some(0.45),
                    Some(0.38),
                    None,
                    Some(-0.26),
                    Some(-0.22),
                    Some(-0.22),
                ],
            ),
            (
                6,
                [
                    Some(0.45),
                    Some(0.30),
                    Some(0.14),
                    Some(-0.14),
                    Some(-0.14),
                    Some(-0.22),
                ],
            ),
        ];
        // (weak-pole ratio, strong side outward, weak side outward)
        const ASYMMETRIC: [Sides; 2] = [
            (
                2.0 / 3.0,
                [
                    Some(0.42),
                    Some(0.10),
                    Some(-0.18),
                    Some(-0.34),
                    Some(-0.14),
                ],
                [
                    Some(0.42),
                    Some(0.14),
                    Some(-0.10),
                    Some(-0.26),
                    Some(-0.10),
                ],
            ),
            (
                1.0 / 3.0,
                [Some(0.30), None, Some(-0.18), Some(-0.30), Some(-0.14)],
                [Some(0.34), Some(0.14), None, Some(-0.14), None],
            ),
        ];

        let paint = |w: usize, first_plus: usize, ratio: f64| -> Vec<Vec<f64>> {
            (0..n)
                .map(|i| {
                    let d = i as i64 - first_plus as i64;
                    let v = if (0..w as i64).contains(&d) {
                        AMP
                    } else if (-(w as i64)..0).contains(&d) {
                        -ratio * AMP
                    } else {
                        0.0
                    };
                    vec![v; gates]
                })
                .collect()
        };
        let agrees = |got: f64, want: Option<f64>| match want {
            Some(w) => (got - w).abs() < 0.04 || (got.is_nan() && w.abs() < SIGNIFICANT),
            None => got.is_nan() || got.abs() < SIGNIFICANT,
        };

        for (w, profile) in SYMMETRIC {
            // Even and odd `first_plus`: the couplet's centre falls between a
            // whole-degree pair and inside one.
            let mut both = Vec::new();
            for first_plus in [100usize, 141] {
                let grid = paint(w, first_plus, 1.0);
                let nrot = llsd_nrot(&sweep(&grid, &azimuths, gates), &grid, &grid, None);
                let mut read = Vec::new();
                for (m, want) in profile.iter().enumerate() {
                    for radial in [first_plus + m, first_plus - 1 - m] {
                        let got = nrot[radial][j];
                        assert!(
                            agrees(got, *want),
                            "width {w} at {first_plus}: radial {radial} read \
                             {got:.3}, the reference {want:?}",
                        );
                        read.push(got);
                    }
                }
                both.push(read);
            }
            for (k, (a, b)) in both[0].iter().zip(&both[1]).enumerate() {
                assert!(
                    a == b || (a.is_nan() && b.is_nan()),
                    "width {w} radial {k}: one parity read {a}, the other {b}",
                );
            }
        }

        for (ratio, strong, weak) in ASYMMETRIC {
            for first_plus in [100usize, 141] {
                let grid = paint(3, first_plus, ratio);
                let nrot = llsd_nrot(&sweep(&grid, &azimuths, gates), &grid, &grid, None);
                for (m, want) in strong.iter().enumerate() {
                    let got = nrot[first_plus + m][j];
                    assert!(
                        agrees(got, *want),
                        "ratio {ratio:.2} at {first_plus}: strong side +{m} read \
                         {got:.3}, the reference {want:?}",
                    );
                }
                for (m, want) in weak.iter().enumerate() {
                    let got = nrot[first_plus - 1 - m][j];
                    assert!(
                        agrees(got, *want),
                        "ratio {ratio:.2} at {first_plus}: weak side {m} out read \
                         {got:.3}, the reference {want:?}",
                    );
                }
            }
        }
    }
}
