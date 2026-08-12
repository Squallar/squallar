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
//! 3. At each bin, the azimuthal derivative is the per-radial super-res
//!    operator ([`SPLIT_TAPS`]) inside 80 km and the composite 11-radial
//!    stencil [`COMPOSITE_TAPS`] beyond, applied to
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
    /// Where this sweep's cut **declared** its velocity folds, m/s, or `None`
    /// when the volume declared nothing for it.
    ///
    /// Read by [`dealias_with_knobs`] and by nothing else in this module: it is
    /// the interval every fold decision is a multiple of, and
    /// [`estimate_nyquist`] is what stands in when it is absent. A
    /// [`WindProfileBuilder`] sweep leaves it `None` on purpose — the VAD fit
    /// trims folded samples statistically and has no use for a limit.
    ///
    /// The number comes off Message 31's Radial Data Block by way of
    /// [`crate::nyquist::DeclaredNyquist`], which is also what
    /// [`crate::sampler::VolumeSampler`] guards its velocity interpolation on.
    /// The two reading one table is the point: a section and a plan view that
    /// disagree about where a sweep folds disagree about which of its gates are
    /// one datum.
    pub declared_nyquist_ms: Option<f64>,
}

/// How this sweep's rows sit in azimuth: the step every stencil's
/// `arc_per_radial` is built from — and so the scale of every NROT value this
/// module reports — together with whether row `n−1` borders row 0.
///
/// [`crate::azimuth::Rows`] holds both and says why they are one question.
/// What the answers cost *here*, when a sector takes the complete cut's pair
/// (`360 / n`, and "the last row borders the first"), is two separate wrong
/// numbers:
///
/// * **Scale.** A 36° sector of 72 radials is 0.5° apart and reads 5°, so every
///   arc is ten times too long and every rotation over it ten times too small —
///   a tornadic couplet at 1.8 comes back 0.18, under the 0.25 [`SIGNIFICANT`]
///   floor, and the product paints nothing where the strongest rotation in the
///   sector is.
/// * **Seam.** Both stencils need every cell of a ±5 span, so the outermost
///   five rows at each end of that sector are read partly from the other end of
///   it — 324° away, across ground the antenna never pointed at. A field of 6
///   (m/s)/km of honest shear stands 187 m/s apart across those two ends at 50
///   km, and dividing that by the 0.44 km of arc half a degree spans saturates:
///   rows 0, 1, 70 and 71 come back at the ±5 clamp and rows 3 and 68 at 2.49,
///   against the 0.43 the field carries there and with nothing rotating
///   anywhere in it.
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
    let med = preprocess_velocity_with(sweep, elevation_deg, profile);
    let mut grid = llsd_nrot(sweep, &med);
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
        sweep_rows(sweep, sweep.vel_grid.len()),
    )
}

/// Wind-profile layer thickness, km.
const PROFILE_LAYER_KM: f64 = 0.3;
/// Layers span 0..12 km AGL.
const PROFILE_LAYERS: usize = 40;
/// Sample cap per layer keeps memory bounded on wasm. A volume offers far
/// more than this: KCRP 2017-08-26 04:41:14 has 326 657 gates to give the
/// twenty layers under 6 km, and its lowest layer alone is offered more than
/// the cap within the first two of its fifteen cuts.
///
/// So *which* samples the cap keeps is a question about the fit, not only
/// about memory, and [`WindProfileBuilder::offer`] answers it by thinning
/// rather than by stopping. See there for what stopping cost.
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
///
/// # The fit is in the ground frame
///
/// Each sample carries the azimuth **the antenna was pointing at**, off the
/// radial. Nothing in [`VelocitySweep`] bins rows into a north-referenced
/// grid: `azimuths_deg` is filled from `Radial::azimuth_angle_degrees` in
/// sweep order, and a WSR-88D starts each cut wherever the previous one
/// ended — the fifteen velocity cuts of KCRP volume 2017-08-26 04:41:14
/// begin at 11.3°, 47.2°, 85.2°, 107.6°, … 104.5°, marching most of the way
/// round twice.
///
/// Row *index* would be a different angle in every cut. `u sin(θ) + v cos(θ)`
/// fitted against `θ = 2πi/n` returns the true wind turned by that cut's own
/// start azimuth, so pooling the volume's cuts into one layer — which is the
/// whole point of pooling, the reason a layer at 1 km holds samples from four
/// tilts at four ranges — averages winds that disagree by tens to hundreds of
/// degrees. Measured on that KCRP volume: the 0–6 km sample-weighted trimmed
/// RMS residual of the pooled fit is 4.85 m/s against azimuth and 5.48 m/s
/// against index, and on KDMX 2022-03-05 23:23:24 the 1.05 km layer alone
/// reads 3.55 m/s against 5.81. The model that explains the gates is the one
/// whose angle is the one the gates were measured at.
///
/// Within a single cut the error was invisible, and that is worth stating
/// because it is why nothing looked wrong: [`WindProfile::predict`] is
/// queried by the dealiaser at the same azimuth the fit was given, so a fit
/// turned by `az0` and a query turned by `az0` cancel and the predicted
/// radial velocity is right. What does not cancel is any reader of the
/// profile's `(u, v)` as a wind — the Bunkers storm motion under SRV
/// ([`crate::srv::bunkers_right_mover`]) is one, and it is a vector the user
/// reads off the pane.
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
    ///
    /// # Why not simply stop at the cap
    ///
    /// A layer fills in the order the volume is walked — cut by cut, row by
    /// row — so stopping keeps a *prefix*, and a prefix of a sweep is an arc.
    /// KCRP 2017-08-26 04:41:14 filled its 0.15 km layer partway through the
    /// second of fifteen cuts: the samples that layer was fitted from spanned
    /// 237.5° of azimuth with a 122.5° hole in them, 0.45 km spanned 282.5°
    /// and 0.75 km 315.5°. Those three are the layers the Bunkers 0–0.5 km
    /// head band and a sixth of its 0–6 km mean wind are read from.
    ///
    /// An arc still determines a VAD fit, so this was a conditioning cost
    /// rather than a wrong answer, and it is worth the size it is and no more.
    /// Against the same fit with the cap lifted altogether, the shipped
    /// right-mover moved 5.4 kt and 6.4° on KMSX 2022-06-04 20:05:58, 3.9 kt
    /// and 7.0° on that KCRP volume, and 2.1 kt and 0.9° on KDMX
    /// 2022-03-05 23:23:24 — a tenth of the rotation error the same three
    /// layers carried while the fit read row index, and still a vector a user
    /// reads.
    ///
    /// Thinning costs one halving per doubling of the offer count — four or
    /// five per layer for a super-res volume — and leaves the layer holding a
    /// uniform one-in-`stride` sample of the *whole* volume, so its azimuth
    /// coverage is the volume's. `offered` deliberately keeps counting across
    /// a halving: the kept samples sit at offers 0, 2·stride, 4·stride…, which
    /// is exactly the progression the doubled stride continues.
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
        // `zip`, not an index: the azimuth slice is the caller's and this
        // module already declines to assume it is `vel_grid.len()` long
        // (`sweep_rows` takes the row count separately, for the same reason).
        // A row the sweep named no azimuth for contributes no sample, which is
        // the same answer a row of all-NaN gates gives.
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

/// The fold limit read **off the data**: the largest speed the sweep observed.
///
/// The fallback, not the answer. `nexrad_model::data::Radial` drops the RDA's
/// declared Nyquist velocity, so for a long time this was the only number
/// available; [`crate::nyquist::DeclaredNyquist`] now carries the declaration
/// past the model boundary and [`fold_limit_ms`] prefers it wherever a volume
/// made one. This still stands in for a volume that declared nothing — every
/// Message 1 volume (the legacy message has no such field), every fixture, and
/// any caller holding only model types.
///
/// It is exact when the sweep folded at all, because folded data reaches the
/// limit by construction, and an **under**estimate when it did not. The
/// underestimate is what makes it a fallback rather than a peer: on a calm
/// sector whose fastest gate is 6 m/s it returns 6, and 2·6 m/s then becomes
/// the interval every fold decision below is a multiple of — so ordinary shear
/// across a 12 m/s step reads as a fold and comes back unfolded by a step that
/// was never there. [`crate::sampler::FOLD_LIMIT_FLOOR_MS`] is the floor that
/// stops the worst of it; the declaration removes the failure mode outright.
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
                        // A row past the end of a sector is skipped rather
                        // than counted empty, which is what the range axis
                        // does three lines down at the ends of the grid: the
                        // occupancy cliff below asks what fraction of the
                        // cells that exist carry raw data.
                        let Some(ai) = rows.neighbour(i, da) else {
                            continue;
                        };
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

/// The per-radial operator on a 0.5°-spaced sweep: taps [ĉ₂, ĉ₁−ĉ₂, ĉ₂, ĉ₃]
/// at row offsets 1/2/3/4, applied **antisymmetrically** — the same list on
/// both sides of the bin, positive toward increasing azimuth. Zero-sum by
/// construction; normalization is two rows of the grid, which is the legacy
/// 1.0° arc on the super-res grid it was fitted on (see
/// [`split_stencil_rot`], where the difference between those two readings is
/// what makes the operator a derivative rather than a reading of one
/// particular spacing).
///
/// # The reference has no pairing asymmetry to assign here
///
/// This used to be *two* tap lists — `SPLIT_CLEAN` = ĉ at offsets 2/3/4 on
/// the side facing a radial's whole-degree pair partner, these taps on the
/// side away from it — with [`pair_phase`] deciding which side each radial
/// faced. A step landing between pairs then read a flat four-radial core
/// (0.78 ×4 at 21.0 nm on a ±8 m/s step) and one landing inside a pair read a
/// two-radial core with 0.50 shoulders, on the same weather, alternating with
/// absolute azimuth.
///
/// The reference does neither: it reads the shouldered profile at **every**
/// step, whatever the parity. Measured by patching six ±8 m/s step edges into
/// the 30–47 km band of a real volume's 0.5° cut — three at whole-degree
/// azimuths (40.0, 160.0, 280.0), three at half-degree ones (100.5, 220.5,
/// 340.5), which are opposite radial-index parities because super-res radial
/// centres sit at x.21/x.71 — and hovering GR2Analyst's status bar along the
/// 21.0 nm arc at 0.25° steps. All 36 profiles (KLOT VCP 212, KATX 215, KMSX,
/// KHNX, KLWX, and a KTLX holdout; declared Nyquist 8.3–24.0 m/s) read
///
/// ```text
/// −0.18  +0.10  +0.49  +0.77  +0.77  +0.49  +0.10  −0.18
/// ```
///
/// symmetric about the edge, which is these taps on both sides (predicted
/// −0.176/+0.102/+0.501/+0.779) and is *not* any assignment of the old
/// asymmetry: applying the clean side uniformly reads three radials at 0.78
/// and one at 0.50, an unsymmetric profile the reference never shows. A step
/// response determines a zero-sum operator uniquely — its successive
/// differences *are* the taps — so this is a measurement of the operator, not
/// a fit to it. Eighteen ±10 m/s six-radial couplet profiles over the same
/// azimuth classes agree: identical at both parities, flanks +0.30 against
/// this operator's +0.31.
///
/// Nor is the anchor merely off by one: a companion set of volumes with every
/// super-res azimuth shifted +0.5° — which moves the sweep's first radial
/// from the low member of its degree to the high one, and so flips
/// index-parity against floor(az) — reads the same profile again at both
/// KLOT and KTLX. The reference's response is invariant to the pairing, so
/// there is no phase to anchor.
const SPLIT_TAPS: [(i32, f64); 4] = [(1, 0.238), (2, 0.342), (3, 0.238), (4, -0.151)];

/// The operator for a sweep that is *already* legacy resolution — a TDWR cut,
/// or a WSR-88D tilt above the super-res ones. Antisymmetric, at row offsets
/// ±1 and ±2 only, normalized by **one** row: ROT = Σ tₖ(v(i+k) − v(i−k)) /
/// arc_per_radial.
///
/// [`SPLIT_TAPS`] cannot serve here: it spans ±4 rows of a 0.5° grid, which
/// is ±4.0° of sky on a 1.0° one, and it carries a different gain
/// (`one_shear_reads_the_gain_its_own_operator_carries`). These taps are what
/// the reference does instead, hovered per radial off a synthetic 1.0° cut
/// carrying a ±8 m/s step and a ±10 m/s six-radial couplet (measured
/// provenance: branch `campaign-harness`):
///
/// * Its response is the **same at both index parities** — 8 step boundaries
///   and 8 couplets of alternating parity, over KLOT (VCP 212), KATX (215),
///   KMSX (35), KHNX (31), KLWX (32) and a KTLX holdout, Nyquist 11.3 to 24.2
///   m/s, every one reading alike. So there is no asymmetry to assign, and
///   this operator has none.
/// * Its support is **exactly ±2 rows**. A step reads full value on the two
///   radials flanking the discontinuity, tail/core = −0.14 on the next, and
///   nothing beyond; a couplet paints two radials past its poles and nothing
///   past that. Both edges are where a ±2 operator's response is identically
///   zero, so the ND boundary measures the span rather than any gate.
/// * It is **linear** there: the couplet's pole-edge/core ratio is −0.5 with
///   no free parameter under this support, and the reference reads −0.45/0.89
///   = −0.506. The matched-filter kernel bank, which compresses couplet edges
///   on the super-res grid, therefore does not run on such a sweep.
///
/// Twelve hovered readings — a three-range step ladder at 32.2/39.1/45.9 km
/// and the couplet's four distinct classes — fit these two taps with a worst
/// residual of 0.026, under the 0.04 the reference quantizes its own output
/// in. Their ramp gain, Σ2k·tₖ = 1.027, is the one number that is *not* the
/// split operator's (1.151): one shear reads 11% lower on a sweep collected at
/// 1.0° than on one collected at 0.5°, because the reference's coarse-grid
/// operator is a narrower one and not the same taps in row units.
const LEGACY_TAPS: [(i32, f64); 2] = [(1, 0.6812), (2, -0.0838)];

/// Matched-filter kernel bank: one per-radial tap operator per couplet pole
/// width (2/3/4 radials), as a clean/away tap pair a radial chooses between by
/// which side its whole-degree pair partner sits on. Each kernel is
/// empirically fitted so that its response to the ideal median-filtered
/// width-w couplet matches the reference's measured width-w couplet response,
/// anchored to the primary operator's own core response on the same pattern
/// (measured provenance: branch `campaign-harness`). The kernels never see
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

/// The super-res operator ([`SPLIT_TAPS`]) at one bin. Requires every tap
/// cell; profiles that do not correlate with the stencil read ND like the
/// composite estimator's.
fn split_stencil_rot(
    vel_grid: &[Vec<f64>],
    i: usize,
    j: usize,
    arc_per_radial: f64,
    gate_count: usize,
    rows: crate::azimuth::Rows,
) -> Option<f64> {
    // Range-averaged velocity at offsets −(4+margin)..=4+margin; prof[6 + o].
    let mut prof = [f64::NAN; 15];
    for (idx, slot) in prof.iter_mut().enumerate() {
        let da = idx as i32 - 7;
        if da.abs() > 7 {
            continue;
        }
        // Off the end of a sector's arc the cell stays NaN, and the
        // completeness rules below read that as the data edge it is.
        let Some(ai) = rows.neighbour(i, da) else {
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
    // Signed weight per profile cell: one tap list, mirrored — positive
    // toward increasing azimuth, negative away from it.
    let mut w = [0.0f64; 15];
    for &(o, t) in &SPLIT_TAPS {
        w[(7 + o) as usize] += t;
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
    // at row offsets, so on a finer grid the numerator spans proportionally
    // less sky and the divisor shrinks by the same factor, and the quotient
    // is the shear either way — measured across 0.5° and 0.25° samplings of
    // one field in `one_shear_reads_the_gain_its_own_operator_carries`.
    // Pinning the divisor to a physical degree instead would double the
    // reading on every grid whose rows are not half degrees.
    //
    // Which grids reach here is the other half of the answer, and it is not
    // "any": a sweep whose rows are already whole degrees has no pairing for
    // this operator's clean/away asymmetry to sit on, and takes the symmetric
    // [`legacy_stencil_rot`] instead ([`pair_phase`]). So the coarse sampling
    // of one field does *not* read what the fine one reads inside 80 km —
    // that is a difference of operators, measured against the reference, and
    // the same test pins it.
    Some(acc / (2.0 * arc_per_radial))
}

/// [`LEGACY_TAPS`] at one bin, for a sweep whose rows are already whole
/// degrees. Same profile, same completeness rule and same coherence floor as
/// [`split_stencil_rot`] — so which bins get a value is unchanged and only the
/// value changes — but symmetric, and normalized by one row rather than two.
///
/// It reads the same [`crate::azimuth::Rows`] the split operator does, for the
/// same reason: a 1.0° sweep is exactly where sectors live — every TDWR cut is
/// one — so past the end of an arc the profile cell stays NaN and the
/// completeness rule below reads the data edge it is.
fn legacy_stencil_rot(
    vel_grid: &[Vec<f64>],
    i: usize,
    j: usize,
    arc_per_radial: f64,
    gate_count: usize,
    rows: crate::azimuth::Rows,
) -> Option<f64> {
    let mut buf = EMPTY_PROFILE;
    // ±7, as the split operator reads, so the data-margin rule below tests the
    // same cells and neither operator paints an echo edge the other would not.
    let prof = az_profile(&mut buf, vel_grid, i, j, gate_count, 7, rows);
    for m in 0..GK_DATA_MARGIN {
        let o = (5 + m) as usize;
        if prof[7 + o].is_nan() || prof[7 - o].is_nan() {
            return None;
        }
    }
    let mut w = [0.0f64; 15];
    for &(o, t) in &LEGACY_TAPS {
        w[(7 + o) as usize] += t;
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
    // One row, not two: these taps sit at whole-degree offsets of a sweep whose
    // rows are whole degrees, and the reference's step response is 0.69 at
    // 39 km where two rows would make it 0.35.
    Some(acc / arc_per_radial)
}

/// Whether this sweep's rows pair into whole-degree legacy bins, and at which
/// index phase: radials (2k+phase, 2k+1+phase) share a degree sector. The
/// pairing is anchored to ABSOLUTE azimuth, not to collection order — a
/// super-res cut's radial centres sit at x.21/x.71 and the two sharing a
/// floor are the pair.
///
/// `Some` versus `None` is the load-bearing half of the answer: it says
/// whether the rows are a 0.5° grid (take [`SPLIT_TAPS`]) or already whole
/// degrees (take [`LEGACY_TAPS`]). The phase *value* is used by
/// [`apply_kernel_bank`] alone, to pick each kernel's clean/away form.
///
/// It no longer picks a form for the primary operator. It used to, and the
/// reference does not: hovered across 36 synthetic step edges at both
/// parities over six sites, its super-res step response is the same
/// symmetric profile every time ([`SPLIT_TAPS`] carries the readings). The
/// kernel bank's own clean/away split rests on the same assumption and has
/// not been re-measured — its couplet cores read 0.75 and 0.36 at the two
/// parities where the reference reads 0.97 on both, so the phase it takes
/// from here is a suspect, not a validated, input.
///
/// # `None` where there is no pairing to find
///
/// The question only has an answer on a 0.5° grid. Two radials 1.0° apart can
/// never share a whole degree — their floors differ by one by construction —
/// so on a 1.0°-spaced sweep both counts come back zero, for whole-degree
/// azimuths, for a sweep offset by 0.37° or 0.5°, and for one jittered ±0.06°
/// (all four measured, in `a_one_degree_sweep_has_no_pair_phase_to_measure`).
/// That is not a bad reading of a real pairing; there is no pairing. Each
/// radial of such a sweep *is* a legacy bin, and the caller reaches for
/// [`LEGACY_TAPS`] rather than a half of an operator it cannot choose between.
///
/// Answering `Some(0)` there — which this did until the reference was hovered
/// on a 1.0° cut — handed the primary operator's then clean/away asymmetry to
/// `i % 2`, off collection index rather than off azimuth. Two sites make the
/// cost concrete: the same synthetic step at az 100.1° and the same couplet at
/// az 140.1° landed on even indices at KLOT and odd ones at KATX, purely
/// because the antennas began their cuts at different azimuths, and the
/// pipeline read 0.388 against 0.249 across the step and 0.388 against 0.180
/// across the couplet — a factor of 2.2 on the same sky. The reference read
/// 0.69 and 0.89 at both.
///
/// A ragged sweep is deliberately not `None`. Only *no* cohabiting pair at
/// either alignment says the rows are whole degrees; a sector or a jittered
/// super-res cut still finds most of its pairs and keeps [`SPLIT_TAPS`],
/// which is the path validated against the reference.
fn pair_phase(azimuths_deg: &[f64]) -> Option<usize> {
    let n = azimuths_deg.len();
    if n < 4 {
        return None;
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
    let (c0, c1) = (cohabit(0), cohabit(1));
    // A real pairing accounts for *most* of the sweep: on a 0.5° grid one
    // alignment puts every pair inside a degree and the other puts none. A
    // 1.0° grid can still show a few, because an antenna that wanders a few
    // hundredths backwards leaves two consecutive radials on the same side of
    // a degree boundary — 8 such pairs in 180 on a ±0.06° jitter. Requiring a
    // majority separates the two without asking the caller for the spacing.
    if 2 * c0.max(c1) <= n / 2 {
        return None;
    }
    Some(usize::from(c1 > c0))
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
    rows: crate::azimuth::Rows,
) -> Option<(&'p [f64], f64)> {
    let prof = az_profile(out, vel_grid, i, j, gate_count, w + 3, rows);
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
/// divisor, the same [`pair_phase`] and the same [`crate::azimuth::Rows`] as
/// the primary chain, since its output is only ever a cap on the primary's
/// magnitude: two chains scaled over different arcs would cap by a ratio of
/// arcs rather than by kernel shape, and one reading past a sector's edge
/// where the other stopped would cap a real value against a manufactured one.
/// Requires every tap cell — a missing cell means the footprint bin keeps the
/// primary chain's value.
#[allow(clippy::too_many_arguments)]
fn bank_kernel_rot(
    vel_grid: &[Vec<f64>],
    i: usize,
    j: usize,
    arc_per_radial: f64,
    gate_count: usize,
    pair_first: bool,
    taps: TapPair,
    rows: crate::azimuth::Rows,
) -> Option<f64> {
    let (clean, away) = taps;
    let span = clean.len() as i32;
    let mut buf = EMPTY_PROFILE;
    let prof = az_profile(&mut buf, vel_grid, i, j, gate_count, span, rows);
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
///
/// None of it runs on a sweep whose rows are already whole degrees. Every
/// kernel here is a clean/away pair, chosen by [`pair_phase`], and so
/// has no form to choose on such a grid — and there is nothing for it to
/// compress: the reference's couplet response there is exactly what
/// [`LEGACY_TAPS`] predicts from its own step response, pole-edge over core
/// −0.45/0.89 = −0.506 against the −0.5 that support forces with no free
/// parameter. The compression this bank exists for is a super-res behaviour.
fn apply_kernel_bank(
    sweep: &VelocitySweep,
    vel_grid: &[Vec<f64>],
    grid: &mut [Vec<f64>],
    phase: Option<usize>,
) {
    let Some(phase) = phase else {
        return;
    };
    let num_radials = grid.len();
    if num_radials == 0 {
        return;
    }
    // The same rows the primary chain read: the bank's output is a cap on the
    // primary's magnitude, and two chains measured over different arcs would
    // cap by a ratio of arcs rather than by kernel shape — or, reading past a
    // sector's edge where the primary would not, cap a real value against a
    // manufactured one.
    let rows = sweep_rows(sweep, num_radials);
    let spacing_rad = rows.step_deg.to_radians();
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
                let prof = az_profile(&mut buf, vel_grid, i, j, sweep.gate_count, 7, rows);
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
                        rows,
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
                // A row at the end of a sector's arc has no neighbour on that
                // side, which is the same "no larger neighbour there" the two
                // tests below already read out of a NaN.
                let prev = rows.neighbour(i, -1).map_or(f64::NAN, |k| col[k]);
                let next = rows.neighbour(i, 1).map_or(f64::NAN, |k| col[k]);
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
                        gated_prof(&mut buf, vel_grid, i, j, sweep.gate_count, w, rows)
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
                    gated_prof(&mut asym_buf, vel_grid, i, j, sweep.gate_count, 3, rows)
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
                    rows,
                )
                .is_none()
                {
                    continue;
                }
                for d in -(w + 2)..=(w + 2) {
                    let Some(ii) = rows.neighbour(i, d) else {
                        continue;
                    };
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
                        rows,
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
    rows: crate::azimuth::Rows,
) -> Option<f64> {
    // Range-averaged velocity per azimuthal offset; prof[5 ± o] holds ±o.
    let mut prof = [f64::NAN; 11];
    for (idx, slot) in prof.iter_mut().enumerate() {
        let da = idx as i32 - 5;
        // As in the split stencil: past a sector's edge the cell stays NaN and
        // the all-five-pairs rule below reads the data edge.
        let Some(ai) = rows.neighbour(i, da) else {
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
    // on the legacy pairs of a super-res one. Neither number is a degree.
    //
    // This is the one operator every sweep reaches, whatever its rows are, so
    // it is where one field sampled at 0.5° and at 1.0° really does read one
    // number — the identity `one_shear_reads_the_gain_its_own_operator_carries`
    // pins past 80 km. Inside 80 km the two samplings take two different
    // operators and do not, which is a measurement rather than a divisor.
    Some(acc / arc_per_radial)
}

fn llsd_nrot(sweep: &VelocitySweep, vel_grid: &[Vec<f64>]) -> Vec<Vec<f64>> {
    let num_radials = vel_grid.len();
    let rows = sweep_rows(sweep, num_radials);
    let spacing_rad = rows.step_deg.to_radians();
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
                        match phase {
                            Some(_) => split_stencil_rot(
                                vel_grid,
                                i,
                                j,
                                arc_per_radial,
                                sweep.gate_count,
                                rows,
                            ),
                            // Rows that are already whole degrees: no partner
                            // to face, so no asymmetry to assign. Same rows
                            // either way — a sector's arc ends where the
                            // antenna stopped whichever operator reads it.
                            None => legacy_stencil_rot(
                                vel_grid,
                                i,
                                j,
                                arc_per_radial,
                                sweep.gate_count,
                                rows,
                            ),
                        }
                    } else {
                        composite_stencil_rot(
                            vel_grid,
                            i,
                            j,
                            arc_per_radial,
                            sweep.gate_count,
                            rows,
                        )
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

/// Half a circle, counted in rows of this grid — the offset from a radial to
/// the one facing it, which the zero-isodop seed pairs a near-zero gate
/// against.
///
/// On a grid that closes the circle it is half the grid, exactly and without
/// measuring anything: n rows spanning 360° put n/2 of them in 180°. An odd n
/// has no row at 180° and takes the one just inside, off by 360/2n° — a
/// quarter of a degree on the 719 rows a rotation that dropped a radial
/// leaves, and the ±3-row window below is three times that wide.
///
/// On an arc it is 180° at the arc's own spacing, and **not** half the arc's
/// rows: the 36 rows that are half of a 72-row, 36° sector sit 18° around, and
/// a radial 18° away is on the same side of the isodop as the one it would be
/// confirming — the near-zero band is tens of degrees wide, so such a pair
/// agrees with itself and seeds the whole band.
fn half_turn_rows(rows: crate::azimuth::Rows) -> i32 {
    if rows.closed {
        (rows.count / 2) as i32
    } else {
        (180.0 / rows.step_deg).round() as i32
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

/// The fold limit this sweep is dealiased against, m/s: what the RDA declared,
/// or what the data shows when it declared nothing. `None` abandons the pass.
///
/// **Declared wins, above the floor.** The declaration is a property of the
/// waveform — the PRF the cut was flown at — and it is right whether or not the
/// sweep happened to fold, which is exactly where [`estimate_nyquist`] is
/// wrong. A calm sector estimates a limit far below the real one and then
/// unfolds honest gradients into shear that was never in the air; a
/// declaration cannot do that.
///
/// # Which radar this reaches, measured
///
/// **The WSR-88D, and only it.** Its Doppler cuts declare 23.84–62.94 m/s
/// across ten volumes, and the number moves inside one volume as much as
/// between sites — KFFC's low Doppler cuts declare 25.65 and its cut 12
/// declares 62.94 — so a per-sweep declaration is worth having and this arm
/// takes it.
///
/// The TDWR does **not** reach it, and that is the correction to what stood
/// here before. Its short PRT does buy 150 m gates at the cost of unambiguous
/// velocity, so its Doppler cuts really do fold on ordinary storm motion — but
/// it never says where. Across 22 volumes from 10 TDWR sites over three days,
/// every cut declares `nyquist_velocity = 0`, which
/// [`crate::nyquist::DeclaredNyquist::declare`] refuses as the absence it is.
/// So a TDWR arrives here with `declared_nyquist_ms == None` and takes the
/// estimator arm — the radar this argument was originally made about is the one
/// still being estimated for.
///
/// [`crate::sampler::FOLD_LIMIT_FLOOR_MS`] bounds both, and it is the sampler's
/// own constant rather than a second copy of the number: the guard that refuses
/// to interpolate across a fold and the pass that removes one are answering the
/// same question about the same sweep, and two floors that could drift apart
/// would let a section and a plan view take different views of the same gate. A
/// declaration under the floor is refused rather than trusted — no operational
/// waveform folds that low, so such a value is a mis-decode, and the estimate
/// is the better of two poor answers.
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

pub(crate) fn dealias_with_knobs(
    vel_grid: &mut [Vec<f64>],
    sweep: &VelocitySweep,
    elevation_deg: f64,
    profile: Option<&WindProfile>,
    knobs: DealiasKnobs,
) {
    let Some(nyquist) = fold_limit_ms(sweep, vel_grid) else {
        return;
    };
    let interval = 2.0 * nyquist;
    let n = vel_grid.len();
    let gc = sweep.gate_count;
    if n < 8 {
        return;
    }
    // Every pass below propagates a fold decision from one gate to a
    // neighbouring one, so every one of them needs to know where the sweep's
    // rows end. On a rotation they do not end, and this is the wrap it always
    // was; on a sector the two ends are the two edges of the arc, and a
    // decision carried across them is carried across ground the antenna never
    // pointed at.
    let rows = sweep_rows(sweep, n);
    // Where each row points, in radians, for the two wind seeds below. Both
    // ask [`WindProfile::predict`] what the environment does along one line of
    // sight, and a line of sight is an angle in the sky, not a position in the
    // sweep — the same azimuth [`WindProfileBuilder::add_sweep`] fitted the
    // profile at, which is what makes prediction and fit the same wind.
    //
    // Hoisted out of the tile loop as well as the row loop: seed 1 visits
    // every row once per 10-gate tile column, so a 1832-gate super-res cut
    // re-derived each row's angle 184 times.
    //
    // `Option` per row, because the azimuth slice's length is the caller's to
    // decide (see `sweep_rows`, which takes the row count separately). A row
    // with no azimuth gets no prediction, and both seeds already treat "the
    // profile predicts nothing here" as "do not seed".
    let az_rad: Vec<Option<f64>> = (0..n)
        .map(|i| sweep.azimuths_deg.get(i).map(|a| a.to_radians()))
        .collect();
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
    // Seed 1b: gate-level wind seeds — a gate whose raw value matches the
    // wind component, with at least DA_SEEDGATE_NEIGHBORS of its 4 neighbors
    // also matching, is valid at raw.
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
                // A row past the end of an arc does not agree, in the same
                // way the gate before the first does not: `nj < gc` is that
                // same absence on the range axis. A gate on either edge of a
                // sector therefore needs all three neighbours it has, which
                // is what a gate at the first range gate has always needed.
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
    //
    // On an arc the counterpart is often not there to look at, and that is a
    // different answer from a radial 324° round the other way: a sector
    // narrower than 180° has no opposite radial for any of its rows, so this
    // seed finds nothing there rather than confirming a near-zero gate
    // against one of its own. Where the arc is wide enough to hold both ends
    // of a diameter, the counterpart lies forward of some rows and behind
    // others — the pairing is symmetric, so both are tried, and on a rotation
    // the forward lookup always answers and the second never runs.
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

        // (b) azimuthal bridge, tighter threshold; azimuth wraps where the
        // sweep closes the circle.
        let t_b = 0.35 * nyquist * DA_THRESH_SCALE;
        for j in 0..gc {
            for start in 0..n {
                if !valid[idx(start, j)] {
                    continue;
                }
                // The rows the walk crosses on its way to `end`, in order.
                // Recorded as it goes rather than recomputed from `start + m`,
                // because past the end of an arc there is no such row: the
                // walk stops there with no `end` to bridge to, which is what
                // it already does at a gate the radar saw nothing in.
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
                // A row on the edge of a sector is flooded from the one side
                // it has a neighbour on. The two directions run in order and
                // the second sees the first's writes, so which side a row is
                // missing is the side it is not filled from.
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
            break;
        }
    }

    // Convert unresolved to ND; write dealiased values back. Never-reached
    // data gates keep their raw values in bulk — measured: the reference
    // dealiaser resolves ALL present data,
    // including isolated gates no propagation pass can reach —
    // unresolved-to-ND conversion evidently applies to contradictory
    // bridging, not unreached data. Size-gate the kept-raw regions: connected
    // components (4-adjacency, azimuth wrapping where the sweep closes the
    // circle) of unreached data gates below the measured minimum are dropped
    // to ND.
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
            // A row at the edge of an arc has no neighbour on that side, in
            // exactly the sense the first and last gate of a radial have
            // none: there is no jump to measure, so nothing to censor for.
            let up = rows.neighbour(i, 1).map_or(f64::NAN, |k| snapshot[k][j]);
            let down = rows.neighbour(i, -1).map_or(f64::NAN, |k| snapshot[k][j]);
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
    // Only the stencil fixtures below sweep a full turn of synthetic
    // azimuths; nothing outside the tests reaches for π any more, now that
    // the wind fit reads the angle each radial declares.
    use std::f64::consts::PI;

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
            declared_nyquist_ms: None,
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
        }
    }

    fn ring_azimuths(n: usize) -> Vec<f64> {
        (0..n).map(|i| i as f64 * 360.0 / n as f64).collect()
    }

    // ---- the wind fit is in the ground frame ------------------------------
    //
    // A VAD cut carrying one known horizontal wind, laid down from whatever
    // azimuth the antenna happened to be at when the cut began. Real cuts
    // begin all over the circle — KCRP volume 2017-08-26 04:41:14 starts its
    // fifteen velocity cuts at 11.3°, 47.2°, 85.2°, … 104.5° — and the wind
    // over the radar is the same wind for every one of them.

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
    ///
    /// Both cuts carry the same 12 m/s westerly with a 5 m/s northerly
    /// component; one starts at north and one at 137.5°. Read against row
    /// index instead of azimuth the second returns that wind turned by 137.5°
    /// — (12, −5) arrives as (−5.5, 11.7), a 20° speed at 65° instead of a
    /// 13 m/s wind from 293°.
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
            // Not bit equality: the two cuts solve normal equations built from
            // the same angles in a different order, so they land within an ulp
            // or two of each other and of the wind. Everything this test is
            // about is orders of magnitude coarser than that.
            assert!(
                (au - bu).abs() < 1e-9 && (av - bv).abs() < 1e-9,
                "the two cuts disagree at {h} km: {a:?} against {b:?}",
            );
        }
    }

    /// The whole point of pooling a volume: four cuts, four start azimuths,
    /// four elevations, one atmosphere. Every sample in a layer has to be
    /// referred to the same north before the layer is solved, or the layer
    /// averages winds that disagree by whatever the cuts' starts disagree by
    /// — here 97.3°, 211.8° and 318.4°, which spans the compass.
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
    ///
    /// Two cuts of identical geometry, one carrying a 10 m/s westerly and one
    /// a 10 m/s southerly. Least squares over two stacked copies of one design
    /// is the mean of what each copy asks for, so the layer's answer is
    /// (5, 5) — a 14 m/s wind from 225° — and each cut on its own would say
    /// (10, 0) or (0, 10). Each cut offers this layer 69 120 samples against a
    /// [`PROFILE_MAX_SAMPLES`] of 16 384 — the pair thin to 8640 at a stride
    /// of 16 — so a layer that stopped at the cap would hold nothing but the
    /// first cut's opening rows and answer (10, 0).
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
        for wind in [(10.0, 0.0), (0.0, 10.0)] {
            let (grid, azimuths) = cut(wind);
            builder.add_sweep(
                &VelocitySweep {
                    vel_grid: &grid,
                    azimuths_deg: &azimuths,
                    gate_count: 700,
                    first_gate_range_km: 0.05,
                    gate_interval_km: 0.05,
                    declared_nyquist_ms: None,
                },
                0.5,
            );
        }
        let (u, v) = builder
            .finish()
            .expect("two oversubscribed cuts fit")
            .wind_at_km(0.15)
            .expect("the 0–0.3 km layer is the one they filled");
        // The thinned set is an arithmetic progression over both cuts' offers,
        // so the two are represented to within one sample of each other; the
        // slack here is that one sample and the round-off of nine thousand
        // normal-equation terms.
        assert!(
            (u - 5.0).abs() < 0.01 && (v - 5.0).abs() < 0.01,
            "the layer fitted ({u:.4}, {v:.4}), not the (5, 5) both cuts average to",
        );
    }

    /// A sector holds an arc, and an arc of a sinusoid still determines it.
    /// 90° of 0.5° radials — the narrowest the chunk feed hands over as a
    /// usable cut — recovers (7, 11) m/s to 7e-12 m/s. Read
    /// against row index the same 181 rows are stretched around a full
    /// circle, and the three-parameter fit then has nothing to do with the
    /// field it was given.
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
    ///
    /// The fixture above unfolds either way, because all forty of its gates
    /// sit inside 12 km and the zero-isodop seed reaches anything within 40:
    /// the near-zero rows anchor the field and the passes carry it round. This
    /// one puts its gates at 50–89 km, where that seed has nothing to work
    /// with (`near_gates` is zero), so the environmental wind is the only
    /// thing that can start the unfolding.
    ///
    /// A 30 m/s southerly is what `from_levels` states here and what
    /// [`crate::srv`] hands the SRV render: a wind over the radar, in the
    /// ground frame. Asked at row index instead, this cut's row 0 is "row 0 of
    /// 72" — due north, where the profile predicts +30 m/s — while the gate
    /// there holds the 21.2 m/s its true 45° azimuth carries. Nothing in the
    /// cut lands within the 4 m/s seed tolerance of its own row index: the two
    /// rows whose index prediction comes closest are the ones the fold has
    /// already moved 50 m/s. So no tile seeds, no gate seeds, no pass has
    /// anything to propagate, and the two folded arcs stay folded.
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
        // One bin pinned at the fold limit so the Nyquist estimate is exactly
        // 25, as in the fixture above: the 5° rows straddle the ±25 crossing
        // rather than landing on it, so unaided the estimate would be the
        // 24.57 of the nearest row and every unfold would land 0.85 m/s short.
        // Row 9 faces 90°, where a southerly reads zero — an isolated spike
        // the passes drop.
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

    /// A sector's two edges are two edges, not a join. The same demand as
    /// [`dealias_leaves_continuous_data_alone`] — a continuous field passes
    /// through untouched — laid over 36° of 0.5° rows instead of around a
    /// rotation: 40 m/s of azimuthal shear from one end of the arc to the
    /// other, 0.56 m/s between adjacent rows, and nothing folded anywhere in
    /// it, since the Nyquist estimate is the 20 m/s the field itself reaches.
    ///
    /// What the two ends read *across* each other is the point. Rows 0 and 71
    /// stand 40 m/s apart because they are 35.5° apart in the sky, and the
    /// post-pass censor blanks any bin more than [`CENSOR_VNY_FRAC`]·Vny —
    /// 24.8 m/s here — from a 4-neighbour. Counted as neighbours, the two rows
    /// are a fold wall the passes could not place, and both go ND over all 40
    /// of their gates: 80 of the sector's 2880 bins erased out of a field with
    /// no fold in it.
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
    ///
    /// The fixture is the shape a gust front makes: half the rotation inbound
    /// at 11 m/s, half outbound at 11 m/s, nothing else. Nothing in it is
    /// folded — the narrowest Doppler declaration measured across ten WSR-88D
    /// volumes is KTLX's 23.84 m/s, and the fastest gate here is under half of
    /// that.
    ///
    /// [`estimate_nyquist`] reads the fastest gate, so it answers **11**, and
    /// the post-pass censor then blanks any bin more than
    /// [`CENSOR_VNY_FRAC`]·Vny — 13.6 m/s — from a 4-neighbour. The two rows
    /// facing each other across the line stand 22 m/s apart, which under that
    /// limit is a fold wall no pass could have placed, so the censor erases
    /// them: 160 bins of the strongest convergence in the sweep, in a field
    /// with no fold anywhere in it. Told the 23.84 m/s the cut was flown at,
    /// the same wall sits inside a 29.6 m/s censor and stands.
    ///
    /// The `None` arm is not scaffolding — it is what every reader of this
    /// module did before the declaration crossed the model boundary, and it is
    /// what a Message 1 volume still gets.
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

    /// A declaration under [`crate::sampler::FOLD_LIMIT_FLOOR_MS`] is a
    /// mis-decoded field, not a very slow radar: no operational waveform folds
    /// at 3 m/s. It is refused and the estimate stands, which on
    /// [`a_declared_limit_keeps_a_shear_line_the_estimate_censors_as_a_fold`]'s
    /// fixture is the arm that erases the line.
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

    /// The radial a near-zero gate is confirmed against is the one facing it,
    /// and the seed asks for it at 180° rather than at half the rows.
    ///
    /// 241 rows of 1.0° covering az 60°..300°, carrying a 30 m/s wind folded
    /// at a 25 m/s Nyquist. The arc holds the whole isodop — the zeros at az
    /// 90° and 270° are both in it, and 180 rows apart — and one folded lobe,
    /// az 146°..214°, where |30·cos(az)| passes 25. The seeds fire along the
    /// isodop, the flood fills carry them over the arc, and every one of the
    /// 9640 bins comes back at the velocity the wind carries there.
    ///
    /// Half the *arc* is not half the circle. Counted in rows, 241/2 lands
    /// 120° around, where this wind reads 24.8 m/s and confirms nothing about
    /// a zero: paired that way no seed fires anywhere in the sector, no pass
    /// has anything to propagate from, and the lobe stays folded — 2601 bins a
    /// full 2·Vny from the velocity their own gate holds, with the fold walls
    /// at either edge of it censored to ND (160 bins more).
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
        // One bin pinned at the fold limit so the Nyquist estimate is exactly
        // the 25 the field was folded at (az 90°, where the wind's radial
        // component is zero — an isolated spike the passes drop).
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
    ///
    /// A rotation always has an answer, and it is the one it always gave:
    /// half the rows around, wrapping. An arc narrower than a half circle
    /// never has one — a 36° sector's rows face 36° of sky it never looked at
    /// — and a wider arc has one for the rows near its two ends and not for
    /// the 119 in its middle, whose counterparts lie in the 119° the antenna
    /// skipped. Forward and backward are both tried because facing is
    /// symmetric: on the 241-row arc below, az 60° is answered by az 240°
    /// ahead of it and az 300° by az 120° behind.
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

    /// Every azimuth lookup the dealiaser makes on a complete cut is the wrap
    /// it always was, at every row and every offset any of its passes reaches:
    /// ±1 for the four-neighbour seed tests, the flood fills' neighbouring
    /// radial and the fold censor, out to 39 for the azimuthal bridge's walk,
    /// and the half turn plus the isodop's three-row window. Every one of them
    /// goes through this one lookup, so this identity is what says the tuned
    /// constants below still measure what they were measured against.
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
        let filtered = median_filter(&grid, &grid, gates, 0.25, 0.25, rows_for(&azs, n));
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

        // Gate 200 → 50.25 km: inside the super-res operator's domain. Its
        // ramp gain is Σ t·o over the legacy arc, the same on both sides:
        // ĉ₂ + 2(ĉ₁−ĉ₂) + 3ĉ₂ + 4ĉ₃ = 1.032. The reference reads this gain
        // directly — a 6 m/s-per-degree synthetic ramp at 21.0 nm reads 0.45
        // there, against this operator's 0.450 and the old split operator's
        // 0.502 (measured, KLOT/KMSX/KLWX; see [`SPLIT_TAPS`]).
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
    /// the answer. The step stays `360 / n` bit for bit — n radials around a
    /// circle leave n gaps summing to 360°, so that is their exact mean and a
    /// measured median is only a noisier reading of the same number — and the
    /// last row still borders the first, at every offset any stencil here
    /// reaches for. Every constant in this module was calibrated against full
    /// rotations, so this is the invariance that leaves them measuring what
    /// they were measured against.
    #[test]
    fn a_closed_sweep_is_read_exactly_as_it_always_was() {
        for n in [360usize, 720] {
            assert_eq!(rows_for(&ring_azimuths(n), n).step_deg, 360.0 / n as f64);
        }

        // Collection order starts wherever the antenna was.
        let rolled: Vec<f64> = (0..720).map(|i| f64::from((i + 431) % 720) * 0.5).collect();
        assert_eq!(rows_for(&rolled, 720).step_deg, 0.5);

        // Real azimuths jitter by a few hundredths of a step; ±0.02° is that,
        // and the median of 720 such gaps still reads within a thousandth of a
        // degree of the mean — ten times inside what the closed-sweep test
        // leaves, so the sweep is still read as closed.
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

        // The seam. `PROFILE_MAX_HALF` is the widest any reader here reaches,
        // and every one of them goes through this lookup, so a rotation that
        // wraps at ±11 wraps everywhere this module indexes a neighbour.
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
        // inside the super-res operator's domain, where the ramp gain is its
        // Σ t·o over the legacy arc.
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
    /// rotates. The same 6 (m/s)/km of azimuthal shear as the full-rotation
    /// ramp test, laid over 36° of 0.5° radials: inside the arc every row reads
    /// the value that ramp analytically carries, and the five rows at each end
    /// — the ones whose ±5 stencil span reaches past a radial the antenna never
    /// collected — read ND, which is what this module reports at every other
    /// data edge.
    ///
    /// The number the two ends do *not* report is the point. Rows 0 and 71 sit
    /// 324° apart across ground the sweep never looked at, and this field
    /// stands 187 m/s apart across them at 50 km; over the 0.44 km of arc half
    /// a degree spans, that saturates. Nothing anywhere in this sector is
    /// allowed above 1.0, against a field whose own rotation runs from 0.29 at
    /// 25 km to 0.86 at the far gate.
    ///
    /// Five rows an end and not four: the two stencils read ±4 but demand ±5,
    /// because a bin whose support only just fits sits on a data edge where
    /// half the profile is echo boundary ([`GK_DATA_MARGIN`]). That rule costs
    /// one computable row at each end of a sector, and it is the same rule
    /// spending the same row at every echo edge in every full rotation.
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
        let nrot = llsd_nrot(&sweep(&grid, &azimuths, gates), &grid);

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
    /// there is. Its rows are whole degrees, so [`pair_phase`] reports nothing
    /// and [`legacy_stencil_rot`] reads it; its arc stops, so the rows past
    /// either end are not there to read.
    ///
    /// Both halves are asserted against the *rotation* rather than against a
    /// formula, which is what makes this an integration test rather than two
    /// restatements: rows 5..67 of the sector read, bit for bit, what the same
    /// field's complete 1.0° rotation reads at the same rows, because their
    /// whole support lies inside the arc — the kernel bank, the one reader
    /// that reaches ±11, does not run on this grid at all. The five rows at
    /// each end read ND, on the same [`GK_DATA_MARGIN`] the split operator
    /// spends there, and nothing in the sector is allowed near a rotation:
    /// rows 0 and 71 stand 71° and 372 m/s apart at 50 km, and stitched
    /// together over the 0.87 km of arc a whole degree spans they would
    /// saturate the clamp.
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
        assert_eq!(
            pair_phase(&sector_az),
            None,
            "a 1.0° sector found a pairing"
        );

        let full_nrot = llsd_nrot(&sweep(&full, &full_az, gates), &full);
        let sector_nrot = llsd_nrot(&sweep(&sector, &sector_az, gates), &sector);

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
    /// indexing off its own end. Three radials cannot fill either stencil's ±5
    /// span from inside a 1° arc, and there is nowhere else for the span to
    /// come from.
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
    ///
    /// Past 80 km the two samplings read *the same number*: one operator
    /// serves both grids there, [`COMPOSITE_TAPS`] at row offsets over a
    /// one-row divisor, so a 1.0° sweep's numerator spans twice the arc and
    /// its divisor grows by the same factor. The identity is exact in the
    /// reals and holds to 3.4e-14 over the 343 bins compared below.
    ///
    /// Inside 80 km they do not, and that is not the divisor: it is that the
    /// reference uses a *different operator* on a grid that is already legacy
    /// resolution. [`LEGACY_TAPS`] carries a ramp gain of 1.027 against the
    /// split operator's 1.151, so the same 6 (m/s)/km field reads 0.892 of
    /// itself on the coarser sweep — measured, not chosen: the taps are the
    /// ones the reference's own hovered step and couplet profiles solve to.
    ///
    /// What the test still has to rule out is the reading a reader meets
    /// first — that `2.0 * arc_per_radial` in [`split_stencil_rot`] means
    /// "1.0° of arc" rather than "two rows". Every gain asserted below is
    /// checked against its own taps rather than against the other sampling,
    /// which pins each operator's divisor in row counts. That alone no longer
    /// separates the two readings for the *split* operator, though, and this
    /// change is what took the separation away: the 1.0° sweep used to supply
    /// it, and it now takes a different operator, while on the 0.5° grid two
    /// rows and one degree are the same number by construction. So a third
    /// sampling at 0.25° carries it — still paired, so still the split
    /// operator, and its rows are quarter degrees, where the two readings
    /// differ by a factor of two.
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
        let fine_nrot = llsd_nrot(&sweep(&fine, &fine_az, gates), &fine);
        let coarse_nrot = llsd_nrot(&sweep(&coarse, &coarse_az, gates), &coarse);

        // Gate 380 is 95.25 km, past SPLIT_MAX_RANGE_KM, so both samplings run
        // the composite stencil over its one-row divisor. Azimuth 180 is the
        // field's own wrap, where both read the ±5 clamp for reasons that have
        // nothing to do with spacing, and is left out of it.
        let mut compared = 0;
        let mut worst = 0.0f64;
        for az in 0..360 {
            if (az as f64 - 180.0).abs() <= 8.0 {
                continue;
            }
            let (c, f) = (coarse_nrot[az][380], fine_nrot[2 * az][380]);
            assert!(
                c.is_finite() && f.is_finite(),
                "az {az}° read ND past 80 km"
            );
            worst = worst.max((c - f).abs());
            compared += 1;
        }
        assert_eq!(compared, 343);
        // Not bit-for-bit, and the gap is arithmetic rather than physical: the
        // stencil is zero-sum, so the background the profile sits on cancels
        // exactly in the reals and only to rounding in f64.
        assert!(
            worst < 1e-12,
            "0.5° and 1.0° read one field {worst} apart past 80 km — a real \
             disagreement, not rounding",
        );

        // Each grid reads the gain its own operator carries: the super-res
        // operator's is its Σ t·o over two rows, the composite's and the
        // legacy grid's are Σ 2·o·t over one.
        let split_gain: f64 = SPLIT_TAPS.iter().map(|&(o, t)| o as f64 * t).sum::<f64>();
        let composite_gain: f64 = COMPOSITE_TAPS
            .iter()
            .enumerate()
            .map(|(idx, &t)| 2.0 * (idx as f64 + 1.0) * t)
            .sum();
        let legacy_gain: f64 = LEGACY_TAPS.iter().map(|&(o, t)| 2.0 * o as f64 * t).sum();
        // Gates 100/200/300 are 25.25/50.25/75.25 km, all inside the split band.
        for j in [100usize, 200, 300] {
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
        let range_km = 0.25 + 380.0 * 0.25;
        let expect = k * composite_gain / rot_divisor(range_km / KM_PER_NM);
        assert!((coarse_nrot[90][380] - expect).abs() < 1e-9);

        // The whole of the difference inside 80 km is the ratio of those two
        // gains — 0.8925 — and none of it is the divisor.
        for j in [100usize, 200, 300] {
            let ratio = coarse_nrot[90][j] / fine_nrot[180][j];
            assert!(
                (ratio - legacy_gain / split_gain).abs() < 1e-9,
                "gate {j}: coarse/fine {ratio}, not the gain ratio {}",
                legacy_gain / split_gain,
            );
        }

        // A third sampling, and the one that keeps the divisor honest. On the
        // 0.5° grid "two rows" and "1.0° of arc" are the same number, and the
        // 1.0° grid no longer reaches this operator at all, so neither of the
        // two above can tell them apart. A 0.25° sweep can: its radials still
        // pair — a quarter degree apart, they share a whole degree — so it
        // runs the same split operator, over half the arc per row. Counting
        // rows it reads the shear that is there; read as a physical degree the
        // divisor would be twice the grid's own two rows and every bin would
        // come back at half.
        assert_eq!(
            pair_phase(&ring_azimuths(1440)),
            Some(0),
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
        let quarter_nrot = llsd_nrot(&sweep(&quarter, &quarter_az, probe_gates), &quarter);
        for j in [100usize, 200] {
            let range_km = 0.25 + j as f64 * 0.25;
            let expect = k * split_gain / rot_divisor(range_km / KM_PER_NM);
            let got = quarter_nrot[360][j]; // az 90°, clear of the field's wrap
            assert!(
                (got - expect).abs() < 1e-9,
                "0.25° gate {j}: read {got}, not the {expect} the same taps carry \
                 over this grid's own two rows — the divisor read a degree",
            );
            // And it is the same number the 0.5° sampling reads, which is the
            // spacing identity itself, inside the split band, between the two
            // grids that share the operator.
            assert!(
                (got - fine_nrot[180][j]).abs() < 1e-12,
                "0.25° gate {j} read {got} where 0.5° read {}",
                fine_nrot[180][j],
            );
        }
    }

    /// A 1.0°-spaced sweep has no pair phase, and the measurement says so by
    /// answering `None` rather than a phase.
    ///
    /// [`pair_phase`] asks which of two index alignments puts radials in the
    /// same whole degree. Two radials 1.0° apart never are — their floors
    /// differ by one by construction — so both counts are zero whatever the
    /// sweep's offset or jitter, and each of its radials *is* a whole-degree
    /// bin. Answering a phase there is what handed the split operator's
    /// asymmetry to the collection index; `None` is what sends such a sweep to
    /// [`legacy_stencil_rot`] instead.
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
            assert_eq!(pair_phase(azs), None, "a 1.0° sweep reported a pairing");
        }

        // The super-res control: there the pairing is real, is found, and
        // follows absolute azimuth rather than collection index — a sweep
        // whose collection started half a degree along reports the other
        // phase, which is what keeps the answer the same.
        assert_eq!(pair_phase(&ring_azimuths(720)), Some(0));
        let rolled: Vec<f64> = (0..720).map(|i| f64::from(i) * 0.5 + 0.5).collect();
        assert_eq!(pair_phase(&rolled), Some(1));

        // And a super-res sweep that only covers a sector still finds its
        // pairs: `None` means "these rows are whole degrees", not "this sweep
        // is ragged", so a 60° sector keeps the validated split operator.
        let sector: Vec<f64> = (0..120).map(|i| 30.0 + f64::from(i) * 0.5).collect();
        assert_eq!(pair_phase(&sector), Some(0));
    }

    /// Where the antenna happened to start a cut is not a property of the
    /// weather, so it must not move the rotation. On the 0.5° grid — the one
    /// validated against the reference — it does not: [`pair_phase`] anchors
    /// the split operator's asymmetry to absolute azimuth, and a sweep rolled
    /// by one radial reads bit for bit what it read before.
    ///
    /// The 1.0° half used to assert only that *something* moved, because the
    /// asymmetry fell to `i % 2` and nothing had been measured to say what it
    /// should be instead: a roll moved 7 of 353 bins past 0.04, flipped 3
    /// between a value and ND, and read −0.198 where the unrolled sweep read
    /// −0.086. The reference has since been hovered on 1.0° cuts and reads the
    /// same at both parities ([`LEGACY_TAPS`]), so such a sweep now takes a
    /// symmetric operator and this half asserts the same invariance as the
    /// other one.
    #[test]
    fn a_sweep_reads_the_same_wherever_collection_began() {
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
        assert_eq!(compared, 713, "the compared row read mostly ND");

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
    ///
    /// Procedure: a real volume's velocity moment was overwritten with this
    /// pattern — a ±8 m/s step and a ±10 m/s six-radial couplet in a 30–47 km
    /// band, twice each at opposite radial-index parity — and the reference
    /// read per radial by hovering its cursor along an arc at 0.25° steps and
    /// OCRing the azimuth/range/NROT triplet it reports. Six volumes: KLOT
    /// (VCP 212), KATX (215), KMSX (35), KHNX (31), KLWX (32), and KTLX held
    /// out of the fit; elevations 1.79–3.53°, Nyquist 11.3–24.2 m/s. All six
    /// read alike, and alike at both parities, at ~21.0 nm:
    ///
    /// ```text
    /// step    ND  ∓0.10  ±0.69  ±0.69  ∓0.10  ND
    /// couplet ND  +0.06  −0.45  −0.45  −0.06  +0.89  +0.89  −0.06  −0.45  −0.45  +0.06  ND
    /// ```
    ///
    /// Both ND boundaries are where a ±2-row operator's response is
    /// identically zero, which is what fixes the support; the couplet's
    /// −0.45/0.89 = −0.506 is the −0.5 that support forces with no free
    /// parameter, which is what says the reference does not compress couplets
    /// on this grid the way the kernel bank does on the super-res one.
    /// A three-range step ladder (0.77/0.69/0.65 at 32.2/39.1/45.9 km) fixes
    /// the divisor curve as the one already shipped.
    ///
    /// The tolerance is the 0.04 the reference quantizes its own output in.
    #[test]
    fn a_legacy_resolution_sweep_reads_the_reference_profiles() {
        let gates = 400;
        let n = 360;
        let azimuths = ring_azimuths(n); // whole degrees: no pairing to find
        // −8 below each boundary, +8 above; first +8 radial at 101 (odd index)
        // and at 122 (even), so the two steps sit at opposite parity.
        // Couplets: −10 on three radials then +10 on three, first +10 radial at
        // 140 (even) and at 161 (odd).
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
                        // A far uniform-wind band, so that max|v| — this
                        // module's Nyquist estimate — sits above the couplet's
                        // 20 m/s pole-to-pole jump and the fold censor leaves
                        // the couplet alone. Real cuts always carry one.
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
        let nrot = llsd_nrot(&sweep(&grid, &azimuths, gates), &grid);
        let j = 154; // 38.75 km, mid-band, where the reference was hovered
        let at = |i: usize| nrot[i][j];

        // A class the reference reports under [`SIGNIFICANT`] may read ND
        // here: this module's coherence floor drops profiles that correlate
        // weakly with the stencil, and those bins are below the palette's
        // first colour either way. The same allowance is made of the super-res
        // step tails in `split_stencil_matches_the_measured_step_profile`.
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
        // reference: they are the same field. This is the property the whole
        // change exists for.
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
    /// [`SPLIT_TAPS`] reproduces the reference's measured per-radial step
    /// profile, **and reads a step at a whole degree exactly as it reads one
    /// at a half degree**.
    ///
    /// The classes are the operator's cumulative sums from the outside in —
    /// which is what a step response *is* for a zero-sum operator — and the
    /// reference's own readings at 21.0 nm on a ±8 m/s step are printed
    /// beside them:
    ///
    /// ```text
    ///   radials flanking the edge   ĉ₁+ĉ₂+ĉ₃ = 0.667   0.780   GR 0.77
    ///   one further out             ĉ₂+(ĉ₁−ĉ₂)+ĉ₃      0.501   GR 0.49
    ///   two further out             ĉ₂+ĉ₃              0.102   GR 0.10
    ///   three further out           ĉ₃                −0.176   GR −0.18
    /// ```
    ///
    /// Both boundaries carry the same profile because the reference does:
    /// 36 hovered profiles over six sites, three at whole-degree azimuths and
    /// three at half-degree ones — opposite radial-index parities, since
    /// super-res radial centres sit at x.21/x.71 — every one reading the
    /// shouldered shape above. This test asserted a flat *four*-radial core
    /// at the whole-degree boundary until those readings were taken; that
    /// shape is what the old clean/away asymmetry produced there and the
    /// reference never shows it. See [`SPLIT_TAPS`].
    ///
    /// The sub-threshold tails may read ND (the coherence gate drops them,
    /// and the display palette would not paint them either) but when present
    /// must carry the measured class values.
    #[test]
    fn split_stencil_matches_the_measured_step_profile() {
        let n = 720;
        let gates = 400;
        let azimuths = ring_azimuths(n); // i·0.5°, pairs at whole degrees
        let j = 153; // 38.5 km
        let range_km = 0.25 + j as f64 * 0.25;
        let arc_legacy = range_km * 1.0_f64.to_radians();
        let scale = 16.0 / arc_legacy / rot_divisor_km(range_km);
        let (c1, c2, c3) = (0.580, 0.238, -0.151);
        let class = [
            (c1 + c2 + c3) * scale,
            (c2 + (c1 - c2) + c3) * scale,
            (c2 + c3) * scale,
            c3 * scale,
        ];

        // Radial 90 is the first +8 radial of a step at az 45.0, a boundary
        // *between* whole-degree pairs; radial 91 is the first of a step at
        // az 45.5, a boundary *inside* one.
        for (boundary_az, first_plus) in [(45.0, 90usize), (45.5, 91usize)] {
            let grid: Vec<Vec<f64>> = (0..n)
                .map(|i| vec![if azimuths[i] < boundary_az { -8.0 } else { 8.0 }; gates])
                .collect();
            let s = sweep(&grid, &azimuths, gates);
            let nrot = llsd_nrot(&s, &grid);
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
                    "az {boundary_az}, radial {radial}: got {got:.3}, expected \
                     {expect:.3}{}",
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
                    "az {boundary_az}, radial {radial}: got {got:.3}, expected ~0"
                );
            }
        }
    }
}
