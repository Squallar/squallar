//! The cross-section rasterizer: a vertical slice through a volume, taken
//! along a great-circle line drawn on the ground.
//!
//! [`render_section`] turns a two-point line and a product into an RGBA raster
//! plus the value and status planes behind it. It draws through
//! [`crate::sampler::VolumeSampler`] and adds no geometry of its own beyond the
//! two axis mappings in [`SectionAxes`].
//!
//! **Row 0 is the top**, matching `egui::ColorImage`'s own row order, so the
//! buffer uploads without a flip. Row `r`'s centre sits at
//! `top − (r + 0.5)·(top − base)/height`; column `c`'s at
//! `(c + 0.5)·length/width`. Both mappings are public on [`SectionAxes`], and
//! the renderer calls them rather than restating them, so a hover readout and
//! the pixels it reads can never disagree.
//!
//! The default axis is `[site_elev, site_elev + 20 km]` **km above mean sea
//! level**.
//!
//! Note the two are not the same coordinate: [`crate::beam`] measures heights
//! **above the antenna**, so every row height crosses that boundary exactly
//! once, at [`SectionAxes::row_height_km_msl`]'s caller.
//!
//! # What is ordinary here and looks like a bug
//!
//! * **A bracketing rung with no data.** Every volume has one at 230 km and at
//!   300 km, and 8 of 19 measured volumes have one at 150 km, because the upper
//!   cuts are range-truncated. It surfaces as
//!   [`SampleStatus::BeyondRange`] on that rung and is beam geometry, not a
//!   defect.
//! * **A blind column where the line crosses the site**, and a 180° flip in
//!   bearing on either side of it. Both are real: the ground range goes to zero
//!   and comes back, and the azimuth is the *opposite* one afterwards.

use crate::par::*;
use crate::sampler::{Column, Sample, SampleStatus, VolumeSampler};
use crate::types::RadarProduct;

/// The wasm32 section width, named outside the cascade so a host build can
/// check it. See [`SECTION_WIDTH`].
pub const WASM_SECTION_WIDTH: usize = 1024;

/// The native section width. See [`SECTION_WIDTH`].
pub const NATIVE_SECTION_WIDTH: usize = 2048;

/// Width of a rendered section, in pixels.
#[cfg(target_arch = "wasm32")]
pub const SECTION_WIDTH: usize = WASM_SECTION_WIDTH;
/// See the wasm32 arm above.
#[cfg(not(target_arch = "wasm32"))]
pub const SECTION_WIDTH: usize = NATIVE_SECTION_WIDTH;

/// Height of a rendered section, in pixels: half [`SECTION_WIDTH`]. See the
/// module doc for why half and not square.
pub const SECTION_HEIGHT: usize = SECTION_WIDTH / 2;

/// How far above the site the default height axis reaches, km.
/// Measured over the 158-volume corpus, on the highest cut each volume actually
/// flew: **115 of 158 volumes carry gates above this axis**. The highest beam
/// centre any of them reaches is 21.28 km, and the shallowest cut that gets
/// there is 4.48°. So the clipping is real, it is ordinary rather than a corner
/// case, and it is **at most 1.3 km deep**.
pub const DEFAULT_AXIS_HEIGHT_KM: f64 = 20.0;

/// Feet to kilometres, for the feedhorn height
/// [`crate::eet::radar_height_ft_near`] reports. The same factor
/// `render::render_hhc_to_image` and `hail::FT_TO_KM` use.
const FT_TO_KM: f64 = 0.0003048;

/// Ground range under which a column is not sampled at all, km.
const BLIND_GROUND_RANGE_KM: f64 = 0.125;

/// Where to cut, how high to draw, and what to draw.
///
/// `start` and `end` are `(latitude, longitude)` in degrees. The line between
/// them is a great circle, not a lat/lon lerp, and the order matters only in
/// that column 0 is at `start`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SectionRequest {
    /// Where the line begins — column 0's end of the raster.
    pub start: (f64, f64),
    /// Where the line ends — the last column's end.
    pub end: (f64, f64),
    /// Top of the height axis, km MSL. `None` takes the site's elevation plus
    /// [`DEFAULT_AXIS_HEIGHT_KM`], which clears the whole volume.
    pub top_km_msl: Option<f64>,
    /// The moment to section. Anything [`crate::derive::volume_slot`] refuses
    /// — the hybrid classification, the column integrals, the precipitation
    /// rate — makes [`render_section`] return `None`; the velocity and phase
    /// derivations (SRV, NROT, KDP) are computed per sweep by
    /// [`crate::derive::prepare`] before sampling.
    pub product: RadarProduct,
}

/// What the two axes mean, and four measurements of how much of the drawing is
/// real.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SectionAxes {
    /// Ground length of the drawn line, km. The horizontal axis spans
    /// `0..length_km`, left to right, from `start` to `end`.
    pub length_km: f64,
    /// Bottom of the height axis, km MSL — always the site's own elevation,
    /// because that is the datum the beam heights are measured from.
    pub base_km_msl: f64,
    /// Top of the height axis, km MSL.
    pub top_km_msl: f64,
    /// Ground range from the site to the nearest column of the section, km.
    ///
    /// Near zero, not zero, when the line crosses the site: columns are sampled
    /// at their centres, so the closest one lands within half a column of the
    /// antenna rather than on it.
    pub near_ground_range_km: f64,
    /// Ground range from the site to the farthest column, km.
    pub far_ground_range_km: f64,
    /// The farthest ground range at which this section found a gate, km — as
    /// measured from the columns actually sampled, not from the volume's
    /// declared extent.
    pub coverage_ground_range_km: f64,
    /// How much of the line, in km, lies under the cone of silence at this
    /// axis top — i.e. how many columns have the volume's ceiling below the
    /// topmost drawn row.
    pub cone_of_silence_km: f64,
    /// How many rungs the tilt ladder had for this moment.
    pub tilt_count: usize,
    /// The largest angular step between adjacent rungs of the ladder, degrees.
    /// `0.0` for a single-rung ladder.
    pub widest_tilt_gap_deg: f64,
    /// The highest cut angle this ladder **has**, degrees — the top rung's VCP
    /// key, `0.0` for an empty ladder.
    pub top_tilt_deg: f64,
    /// The highest cut angle the coverage pattern **declares**, degrees.
    pub top_declared_cut_deg: f64,
}

impl SectionAxes {
    /// Whether every number here is finite.
    fn all_finite(self) -> bool {
        [
            self.length_km,
            self.base_km_msl,
            self.top_km_msl,
            self.near_ground_range_km,
            self.far_ground_range_km,
            self.coverage_ground_range_km,
            self.cone_of_silence_km,
            self.widest_tilt_gap_deg,
            self.top_tilt_deg,
            self.top_declared_cut_deg,
        ]
        .iter()
        .all(|v| v.is_finite())
    }

    /// The height, km MSL, of the centre of row `row`.
    ///
    /// **Row 0 is the top.** Extrapolates outside `0..SECTION_HEIGHT` rather
/// than clamping.
    pub fn row_height_km_msl(&self, row: usize) -> f64 {
        self.top_km_msl
            - (row as f64 + 0.5) * (self.top_km_msl - self.base_km_msl) / SECTION_HEIGHT as f64
    }

    /// The distance along the line, km from `start`, of the centre of column
    /// `col`. Extrapolates outside `0..SECTION_WIDTH`, as
    /// [`row_height_km_msl`](Self::row_height_km_msl) does.
    pub fn column_distance_km(&self, col: usize) -> f64 {
        (col as f64 + 0.5) * self.length_km / SECTION_WIDTH as f64
    }
}

/// A rendered section: the picture, the numbers behind it, and why a number is
/// missing where it is.
///
/// The three planes are one raster in three parallel forms, all
/// [`SECTION_WIDTH`] × [`SECTION_HEIGHT`] and all row-major with row 0 at the
/// top: `image` is RGBA8, `values` is the product's own unit with `f32::NAN`
/// wherever there is no value, and `status` is one
/// [`SampleStatus::wire_code`] per pixel saying which of the seven reasons
/// applies.
#[derive(Debug, Clone)]
pub struct CrossSection {
    image: Vec<u8>,
    values: Vec<f32>,
    status: Vec<u8>,
    axes: SectionAxes,
    /// Where the ladder's rungs actually are, in degrees of beam elevation, in
    /// the cut order the sampler resolved them in.
    tilt_elevations_deg: Vec<f64>,
    /// When each of those rungs was flown, milliseconds since the Unix epoch,
/// in the same order.
    tilt_collected_ms: Vec<i64>,
}

/// Equality that ignores a value where there is no value to compare.
///
/// **A derived `PartialEq` makes almost every section unequal to itself.**
/// Every non-`Value` pixel stores `f32::NAN` in `values`, and `NaN != NaN`, so
/// *one* such pixel anywhere in the raster is enough.
impl PartialEq for CrossSection {
    fn eq(&self, other: &Self) -> bool {
        self.axes == other.axes
            && self.tilt_elevations_deg == other.tilt_elevations_deg
            && self.tilt_collected_ms == other.tilt_collected_ms
            && self.image == other.image
            && self.status == other.status
            && self.values.len() == other.values.len()
            && self
                .values
                .iter()
                .zip(&other.values)
                .zip(&self.status)
                .all(|((a, b), &st)| st != VALUE_CODE || a == b)
    }
}

/// [`SampleStatus::Value`]'s wire code, hoisted so the `PartialEq` above reads
/// as a comparison rather than as a magic byte.
const VALUE_CODE: u8 = 0;

impl CrossSection {
    /// Reassemble a section from planes that crossed a boundary — the worker
    /// wire, a cache, a test.
    pub fn from_parts(
        image: Vec<u8>,
        values: Vec<f32>,
        status: Vec<u8>,
        axes: SectionAxes,
        tilt_elevations_deg: Vec<f64>,
        tilt_collected_ms: Vec<i64>,
    ) -> Option<Self> {
        let pixels = SECTION_WIDTH * SECTION_HEIGHT;
        if image.len() != pixels * 4 || values.len() != pixels || status.len() != pixels {
            return None;
        }
        if !axes.all_finite() {
            return None;
        }
        if tilt_elevations_deg.len() != axes.tilt_count
            || !tilt_elevations_deg.iter().all(|deg| deg.is_finite())
        {
            return None;
        }
        if tilt_collected_ms.len() != axes.tilt_count {
            return None;
        }
        let planes_agree = status.iter().zip(&values).all(|(&code, &value)| {
            SampleStatus::from_wire_code(code)
                .is_some_and(|status| status != SampleStatus::Value || value.is_finite())
        });
        if !planes_agree {
            return None;
        }
        Some(Self {
            image,
            values,
            status,
            axes,
            tilt_elevations_deg,
            tilt_collected_ms,
        })
    }

    /// RGBA8, row-major, row 0 at the top, `SECTION_WIDTH * SECTION_HEIGHT * 4`
    /// bytes.
    pub fn image(&self) -> &[u8] {
        &self.image
    }

    /// The same pixels, to rewrite in place.
    ///
    /// A `&mut [u8]` and not a `&mut Vec<u8>`, which is the whole of the
    /// safety: the length is what [`from_parts`](Self::from_parts) validated
    /// and what every consumer's `ColorImage` assertion stands on, and a slice
/// cannot change it.
    pub fn image_mut(&mut self) -> &mut [u8] {
        &mut self.image
    }

    /// The product's own units, `f32::NAN` wherever there is no value.
    pub fn values(&self) -> &[f32] {
        &self.values
    }

    /// One [`SampleStatus::wire_code`] per pixel.
    pub fn status(&self) -> &[u8] {
        &self.status
    }

    /// What the two axes mean and how much of the drawing is real.
    pub fn axes(&self) -> &SectionAxes {
        &self.axes
    }

    /// The beam elevation of each rung of the ladder this section was sampled
    /// from, degrees, in cut order. Exactly
    /// [`SectionAxes::tilt_count`] of them —
    /// [`from_parts`](Self::from_parts) refuses any other length.
    pub fn tilt_elevations_deg(&self) -> &[f64] {
        &self.tilt_elevations_deg
    }

    /// When each rung of [`tilt_elevations_deg`](Self::tilt_elevations_deg)
    /// was flown, milliseconds since the Unix epoch, in the same order and the
    /// same length. `0` for a rung whose chosen sweep carried no clock.
    pub fn tilt_collected_ms(&self) -> &[i64] {
        &self.tilt_collected_ms
    }

    /// How long this ladder took to fly — see [`assembly_span_secs`].
    pub fn assembly_span_secs(&self) -> Option<i64> {
        assembly_span_secs(&self.tilt_collected_ms)
    }

    /// The sample behind one pixel, re-paired from the value and status planes.
    pub fn sample(&self, col: usize, row: usize) -> Option<Sample> {
        if col >= SECTION_WIDTH || row >= SECTION_HEIGHT {
            return None;
        }
        let i = row * SECTION_WIDTH + col;
        // Total by construction: every writer of `status` goes through
        // `wire_code`, and `from_parts` refuses a byte that does not decode.
        let status = SampleStatus::from_wire_code(self.status[i])?;
        Some(if status == SampleStatus::Value {
            Sample::found(self.values[i])
        } else {
            Sample::missing(status)
        })
    }
}

/// How long a tilt ladder took to fly: the newest rung's clock less the
/// oldest's, in seconds, over a list shaped like
/// [`CrossSection::tilt_collected_ms`].
pub fn assembly_span_secs(tilt_collected_ms: &[i64]) -> Option<i64> {
    // `0` is the decoder's "no clock" value, not a date, so it is filtered
    // rather than minimised over — one unstamped rung would otherwise report a
    // section assembled over half a century.
    let mut clocked = tilt_collected_ms.iter().copied().filter(|&ms| ms > 0);
    let first = clocked.next()?;
    let (min, max) = clocked.fold((first, first), |(lo, hi), ms| (lo.min(ms), hi.max(ms)));
    Some((max - min) / 1000)
}

/// Draw a vertical section of `scan` along `req`'s line, for a radar at
/// `(lat, lon)`.
///
/// `None` for a request that names no section rather than for one that finds no
/// data — an empty volume still renders, as a raster of
/// [`SampleStatus::NoCoverage`] with its axes filled in. The refusals are:
///
/// * a non-finite endpoint or site coordinate;
/// * a line of zero length (the two endpoints are the same place);
/// * a `top_km_msl` that is not above the site's elevation, which names no
///   axis at all;
/// * a product [`crate::derive::volume_slot`] refuses (no native moment and
///   no derivation), a derivation that cannot run ([`crate::derive::prepare`]
///   — above all SRV with no storm motion vector), or a volume
///   [`VolumeSampler::new`] refuses.
///
/// `storm_motion_override` is the user's `(speed_kt, direction_from_deg)`
/// vector and `rpg_storm_motion` is the RPG's own for this volume, read only
/// when `req.product` is storm-relative velocity.
pub fn render_section<'a>(
    volume: impl Into<crate::nyquist::Volume<'a>>,
    req: &SectionRequest,
    lat: f64,
    lon: f64,
    motion: crate::srv::MotionInputs,
) -> Option<CrossSection> {
    let volume = volume.into();
    // The derivation seam: native moments pass through as a borrow; derived
    // products are computed here, per sweep, before anything samples — so a
// raw volume can never be sampled under a derived label.
    let prepared = crate::derive::prepare(volume, req.product, motion, lat, lon)?;
    // The declared Nyquist table follows the scan through the derivation: it
    // is keyed by elevation number, which `prepare` preserves, and a derived
// scan's rungs are the same cuts flown at the same PRFs.
    let declared = volume.declared_nyquist();
    let sampler = match &prepared {
        crate::derive::Prepared::Native(scan) => {
            VolumeSampler::new(crate::nyquist::Volume::new(scan, declared), req.product).ok()?
        }
        crate::derive::Prepared::Derived(scan) => {
            let slot = crate::derive::derived_slot(req.product)?;
            VolumeSampler::for_derived(
                crate::nyquist::Volume::new(scan, declared),
                req.product,
                slot,
            )
            .ok()?
        }
    };
    render_with_sampler(&sampler, req, lat, lon)
}

/// [`render_section`] against a sampler the caller already built.
fn render_with_sampler(
    sampler: &VolumeSampler<'_>,
    req: &SectionRequest,
    lat: f64,
    lon: f64,
) -> Option<CrossSection> {
    debug_assert_eq!(
        sampler.product(),
        req.product,
        "the sampler's moment and the request's product must be the same, or \
         the values and the colours come from different scales",
    );

    if ![req.start.0, req.start.1, req.end.0, req.end.1, lat, lon]
        .iter()
        .all(|v| v.is_finite())
    {
        log::warn!(
            "cross-section refused: a non-finite coordinate in {:?} or site ({lat}, {lon})",
            (req.start, req.end),
        );
        return None;
    }

    let (_, length_km) =
        rustdar_geo::site_bearing_range_km(req.start.0, req.start.1, req.end.0, req.end.1);
    if length_km <= 0.0 {
        log::warn!(
            "cross-section refused: {:?} to {:?} is a line of {length_km} km",
            req.start,
            req.end,
        );
        return None;
    }

    // The feedhorn: every height on this axis is a beam height, and `beam`
    // measures those above the antenna, not above the ground the tower
    // stands on.
    let base_km_msl = crate::eet::radar_height_ft_near(lat, lon, crate::sites::Datum::Feedhorn)
        .unwrap_or(0.0)
        * FT_TO_KM;
    let top_km_msl = req
        .top_km_msl
        .unwrap_or(base_km_msl + DEFAULT_AXIS_HEIGHT_KM);
    // Finiteness is tested separately from the ordering, because `inf` passes
    // the ordering: an infinite top is "above" the site and would give every
// row an infinite height, a `NaN` step and a raster of `NoCoverage`.
    if !top_km_msl.is_finite() || top_km_msl <= base_km_msl {
        log::warn!(
            "cross-section refused: a top of {top_km_msl} km MSL is not a \
             finite height above the {base_km_msl} km MSL site",
        );
        return None;
    }

    let mut axes = SectionAxes {
        length_km,
        base_km_msl,
        top_km_msl,
        near_ground_range_km: 0.0,
        far_ground_range_km: 0.0,
        coverage_ground_range_km: 0.0,
        cone_of_silence_km: 0.0,
        tilt_count: sampler.tilt_count(),
        widest_tilt_gap_deg: sampler.widest_tilt_gap_deg(),
        top_tilt_deg: sampler.top_tilt_deg(),
        top_declared_cut_deg: sampler.top_declared_cut_deg(),
    };

    let columns = sample_columns(sampler, req, &axes, lat, lon);
    // Heights inside a `Column` are above the antenna; the axis is MSL.
    let top_row_arl_km = axes.row_height_km_msl(0) - base_km_msl;
    summarize(&columns, &mut axes, top_row_arl_km);

    let SectionPlanes {
        mut image,
        mut values,
        mut status,
    } = checkout();

    image
        .par_chunks_mut(SECTION_WIDTH * 4)
        .zip(values.par_chunks_mut(SECTION_WIDTH))
        .zip(status.par_chunks_mut(SECTION_WIDTH))
        .enumerate()
        .for_each(|(row, ((pixel_row, value_row), status_row))| {
            let height_arl_km = axes.row_height_km_msl(row) - base_km_msl;
            for (col, at) in columns.iter().enumerate() {
                let sample = at.column.at_height_km(height_arl_km);
                value_row[col] = sample.value_or_nan();
                status_row[col] = sample.status().wire_code();
                let (r, g, b, a) = section_color(req.product, sample);
                pixel_row[col * 4..col * 4 + 4].copy_from_slice(&[r, g, b, a]);
            }
        });

    Some(CrossSection {
        image,
        values,
        status,
        axes,
        tilt_elevations_deg: sampler.elevations_deg().collect(),
        tilt_collected_ms: sampler.collection_times_ms().collect(),
    })
}

/// The three planes of one section, held between cuts instead of allocated
/// inside each one.
static POOLED_PLANES: std::sync::Mutex<Option<SectionPlanes>> = std::sync::Mutex::new(None);

/// The three parallel planes of one section — see [`CrossSection`], whose
/// fields these become.
struct SectionPlanes {
    image: Vec<u8>,
    values: Vec<f32>,
    status: Vec<u8>,
}

impl SectionPlanes {
    /// Nothing at all, which [`fit`](Self::fit) turns into a section's worth of
    /// planes. What a pool miss starts from.
    fn empty() -> Self {
        Self {
            image: Vec::new(),
            values: Vec::new(),
            status: Vec::new(),
        }
    }

    /// Make these planes exactly what `vec![0u8; pixels * 4]`,
    /// `vec![f32::NAN; pixels]` and `vec![NoCoverage; pixels]` would be.
    fn fit(&mut self) {
        let pixels = SECTION_WIDTH * SECTION_HEIGHT;
        self.image.clear();
        self.image.resize(pixels * 4, 0u8);
        self.values.clear();
        self.values.resize(pixels, f32::NAN);
        self.status.clear();
        self.status
            .resize(pixels, SampleStatus::NoCoverage.wire_code());
    }
}

/// A seeded, correctly-sized set of planes — the pool's if it has one.
fn checkout() -> SectionPlanes {
    // Bound to a `let`, and deliberately not written as
    // `pool().take().unwrap_or_else(..)`: in that shape the temporary guard
    // lives to the end of the statement and holds the pool lock across the
// fallback allocation below.
    let taken = pool().take();
    let mut planes = taken.unwrap_or_else(SectionPlanes::empty);
    planes.fit();
    planes
}

/// Offer a set of planes back, keeping them only if the slot is free.
fn recycle(planes: SectionPlanes) {
    let mut pool = pool();
    if pool.is_none() {
        *pool = Some(planes);
    }
}

/// The pool, with a poisoned lock read as a live one.
fn pool() -> std::sync::MutexGuard<'static, Option<SectionPlanes>> {
    POOLED_PLANES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A section's planes go back to the pool when the section does.
impl Drop for CrossSection {
    fn drop(&mut self) {
        recycle(SectionPlanes {
            image: std::mem::take(&mut self.image),
            values: std::mem::take(&mut self.values),
            status: std::mem::take(&mut self.status),
        });
    }
}

/// One output column's ground range and the tilt ladder over it.
struct ColumnAt {
/// Ground range from the site, km.
    ground_range_km: f64,
/// The ladder, or an empty one for a blind column.
    column: Column,
}

/// Walk the line and resolve one tilt ladder per output column.
fn sample_columns(
    sampler: &VolumeSampler<'_>,
    req: &SectionRequest,
    axes: &SectionAxes,
    lat: f64,
    lon: f64,
) -> Vec<ColumnAt> {
    (0..SECTION_WIDTH)
        .map(|col| {
            let t = axes.column_distance_km(col) / axes.length_km;
            let point = rustdar_geo::great_circle_point(req.start, req.end, t);
            let (azimuth_deg, ground_range_km) =
                rustdar_geo::site_bearing_range_km(lat, lon, point.0, point.1);
            let column = if is_blind(ground_range_km) {
                Column::new()
            } else {
                sampler.column(azimuth_deg, ground_range_km)
            };
            ColumnAt {
                ground_range_km,
                column,
            }
        })
        .collect()
}

/// Whether a column sits inside the guard over the site — see
/// [`BLIND_GROUND_RANGE_KM`].
fn is_blind(ground_range_km: f64) -> bool {
    ground_range_km < BLIND_GROUND_RANGE_KM
}

/// Whether a column's ladder ceiling leaves the topmost drawn row above the
/// volume — the cone-of-silence test.
fn ceiling_is_under(ceiling_km: f64, top_row_arl_km: f64) -> bool {
    ceiling_km < top_row_arl_km
}

/// Fill in the four measurements that can only be made once the columns exist.
fn summarize(columns: &[ColumnAt], axes: &mut SectionAxes, top_row_arl_km: f64) {
    let column_width_km = axes.length_km / SECTION_WIDTH as f64;
    let mut near = f64::INFINITY;
    let mut far: f64 = 0.0;
    let mut coverage: f64 = 0.0;
    let mut cone_columns = 0usize;

    for at in columns {
        near = near.min(at.ground_range_km);
        far = far.max(at.ground_range_km);

        let illuminated = at.column.rungs().iter().any(|rung| {
            matches!(
                rung.sample.status(),
                SampleStatus::Value | SampleStatus::BelowThreshold | SampleStatus::RangeFolded
            )
        });
        if illuminated {
            coverage = coverage.max(at.ground_range_km);
        }

        let in_cone = at
            .column
            .height_span_km()
            .is_none_or(|(_, ceiling_km)| ceiling_is_under(ceiling_km, top_row_arl_km));
        if in_cone {
            cone_columns += 1;
        }
    }

    axes.near_ground_range_km = near.min(far);
    axes.far_ground_range_km = far;
    axes.coverage_ground_range_km = coverage;
    axes.cone_of_silence_km = cone_columns as f64 * column_width_km;
}

/// The colour of one section pixel.
///
/// Everything except a folded gate goes through
/// [`crate::get_color_for_value`], and that is load-bearing rather than
/// convenient: the per-product transparency floors — reflectivity below 0 dBZ,
/// echo tops below 5 kft, VIL below 1 — live **only** inside that function and
/// are not in `LegendScale::thresholds`, so a renderer that consulted the
/// legend instead would paint a floor the plan view leaves empty.
fn section_color(product: RadarProduct, sample: Sample) -> (u8, u8, u8, u8) {
    if sample.status() == SampleStatus::RangeFolded {
        return crate::palette::RANGE_FOLDED;
    }
    crate::get_color_for_value(product, sample.value_or_nan())
}

/// Identifies a section payload, so a message that is not one fails on its
/// first four bytes instead of being read as a wildly-sized allocation.
const MAGIC: [u8; 4] = *b"RDXS";

/// Bumped whenever the layout below changes. The two ends of a worker boundary
/// can be different builds — see `rustdar-web`'s build-token handshake — so a
/// mismatch has to be a clean `None`, not a misparse.
const FORMAT_VERSION: u16 = 3;

impl CrossSection {
    /// Encode for transport. Little-endian throughout; the image and status
    /// planes are copied verbatim, which is where nearly all the bytes are.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.encoded_len());
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());

        let axes = &self.axes;
        for number in [
            axes.length_km,
            axes.base_km_msl,
            axes.top_km_msl,
            axes.near_ground_range_km,
            axes.far_ground_range_km,
            axes.coverage_ground_range_km,
            axes.cone_of_silence_km,
        ] {
            out.extend_from_slice(&number.to_le_bytes());
        }
        out.extend_from_slice(&(axes.tilt_count as u32).to_le_bytes());
        for number in [
            axes.widest_tilt_gap_deg,
            axes.top_tilt_deg,
            axes.top_declared_cut_deg,
        ] {
            out.extend_from_slice(&number.to_le_bytes());
        }

        out.extend_from_slice(&(self.tilt_elevations_deg.len() as u32).to_le_bytes());
        for elevation in &self.tilt_elevations_deg {
            out.extend_from_slice(&elevation.to_le_bytes());
        }
        out.extend_from_slice(&(self.tilt_collected_ms.len() as u32).to_le_bytes());
        for collected in &self.tilt_collected_ms {
            out.extend_from_slice(&collected.to_le_bytes());
        }

        out.extend_from_slice(&(self.image.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.image);
        out.extend_from_slice(&(self.values.len() as u32).to_le_bytes());
        for value in &self.values {
            out.extend_from_slice(&value.to_le_bytes());
        }
        out.extend_from_slice(&(self.status.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.status);
        out
    }

    /// Decode a payload [`to_bytes`](Self::to_bytes) produced.
    ///
    /// `None` on anything malformed — wrong magic, unknown version, truncation,
    /// trailing bytes, a plane sized for a different build's raster, a status
    /// code this build does not have, a non-finite axis, a `Value` pixel with
    /// no finite number. Every length is checked against what remains before
    /// it is used, so a corrupt frame cannot ask for a large allocation.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let mut r = Reader::new(bytes);
        if r.take(4)? != MAGIC {
            return None;
        }
        if r.u16()? != FORMAT_VERSION {
            return None;
        }

        let axes = SectionAxes {
            length_km: r.f64()?,
            base_km_msl: r.f64()?,
            top_km_msl: r.f64()?,
            near_ground_range_km: r.f64()?,
            far_ground_range_km: r.f64()?,
            coverage_ground_range_km: r.f64()?,
            cone_of_silence_km: r.f64()?,
            tilt_count: r.u32()? as usize,
            widest_tilt_gap_deg: r.f64()?,
            top_tilt_deg: r.f64()?,
            top_declared_cut_deg: r.f64()?,
        };

        let tilt_len = r.u32()?;
        let mut tilt_elevations_deg = Vec::with_capacity(r.bounded(tilt_len, 8)?);
        for _ in 0..tilt_len {
            tilt_elevations_deg.push(r.f64()?);
        }

        let clock_len = r.u32()?;
        let mut tilt_collected_ms = Vec::with_capacity(r.bounded(clock_len, 8)?);
        for _ in 0..clock_len {
            tilt_collected_ms.push(r.i64()?);
        }

        let image_len = r.u32()?;
        let image = r.take(image_len as usize)?.to_vec();

        let value_len = r.u32()?;
        let mut values = Vec::with_capacity(r.bounded(value_len, 4)?);
        for _ in 0..value_len {
            values.push(r.f32()?);
        }

        let status_len = r.u32()?;
        let status = r.take(status_len as usize)?.to_vec();

        if !r.at_end() {
            return None;
        }
        Self::from_parts(
            image,
            values,
            status,
            axes,
            tilt_elevations_deg,
            tilt_collected_ms,
        )
    }

    /// What [`to_bytes`](Self::to_bytes) will write, exactly.
    fn encoded_len(&self) -> usize {
        let header = 4 + 2;
        // Seven `f64`, the tilt count as a `u32`, then the widest gap and the
        // two ladder-top angles.
        let axes = 7 * 8 + 4 + 3 * 8;
        header
            + axes
            + (4 + self.tilt_elevations_deg.len() * 8)
            + (4 + self.tilt_collected_ms.len() * 8)
            + (4 + self.image.len())
            + (4 + self.values.len() * 4)
            + (4 + self.status.len())
    }
}

/// A bounds-checked cursor. Every accessor returns `None` rather than
/// panicking, because the bytes come off a message port and are not trusted.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(n)?;
        let slice = self.bytes.get(self.at..end)?;
        self.at = end;
        Some(slice)
    }

    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
    }

    fn i64(&mut self) -> Option<i64> {
        Some(i64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn f32(&mut self) -> Option<f32> {
        Some(f32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn f64(&mut self) -> Option<f64> {
        Some(f64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }

    /// `count` as a capacity, refused if the buffer cannot possibly hold that
    /// many items of `min_size` bytes each. Keeps a corrupt length from
    /// reserving gigabytes before the read fails.
    fn bounded(&self, count: u32, min_size: usize) -> Option<usize> {
        let count = count as usize;
        (count.checked_mul(min_size)? <= self.bytes.len() - self.at).then_some(count)
    }

    fn at_end(&self) -> bool {
        self.at == self.bytes.len()
    }
}

#[cfg(test)]
mod tests;
