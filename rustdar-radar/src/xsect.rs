//! The cross-section rasterizer: a vertical slice through a volume, taken
//! along a great-circle line drawn on the ground.
//!
//! A plan view answers "what is at this place, on this tilt". A section answers
//! "what is above this line, at every height" — which is the question a
//! forecaster asks about a storm's core, its overhang and its echo top, and the
//! one rustdar could not answer at all before this module.
//!
//! [`render_section`] turns a two-point line and a product into an RGBA raster
//! plus the value and status planes behind it. It draws through
//! [`crate::sampler::VolumeSampler`] and adds no geometry of its own beyond the
//! two axis mappings in [`SectionAxes`].
//!
//! # Why the column primitive, and not one sample per pixel
//!
//! Every pixel of a section column shares one ground point, so it shares one
//! tilt ladder. [`crate::sampler::VolumeSampler::column`] resolves that ladder
//! once — `4·N` gate reads, ~64 on a 16-rung VCP 212 volume — and every row
//! after the first is a two-point lerp between rungs already sampled, reading
//! no gates at all. A per-pixel section pays the whole ladder
//! [`SECTION_HEIGHT`] times over instead: `W·H·4·N` against `W·4·N`, which on
//! the 2048 × 1024 native raster is **134 M gate reads against 131 k** — the
//! `H`-fold saving `VolumeSampler::column`'s own doc states, 1024× here.
//!
//! (The plan that commissioned this module quoted "~33.5 M against ~4.19 M".
//! Those are `W·H·N` and `W·H·2` — a per-pixel walk of every rung against a
//! per-pixel *bracketing pair*, both of which still resolve a ladder per pixel.
//! Neither is what the sampler's `column` primitive does, and the figures are
//! left here corrected rather than repeated.)
//!
//! So this module builds one [`Column`] per output column up front and then
//! fills rows across them. Measured at 12.5 ms per 2048 × 1024 section on a
//! five-rung ladder with rayon, 73 ms single-threaded — the single-threaded
//! figure being the one that bounds wasm, where the raster is a quarter the
//! pixels and there is no pool. `section_timing` (on branch
//! `campaign-harness`) is the measurement.
//!
//! # The raster
//!
//! [`SECTION_WIDTH`] is 1024 on the web and 2048 native, and [`SECTION_HEIGHT`]
//! is half of it. Both dimensions stay powers of two and both stay inside the
//! 2048 floor a phone browser may report. A 2 : 1 raster also matches the shape of
//! the thing drawn: a 230 km line under a 20 km axis is 11 : 1 in the world, so
//! the picture is already stretched vertically by an order of magnitude and a
//! square raster would spend three quarters of its rows on the stretch.
//!
//! **Row 0 is the top**, matching `egui::ColorImage`'s own row order, so the
//! buffer uploads without a flip. Row `r`'s centre sits at
//! `top − (r + 0.5)·(top − base)/height`; column `c`'s at
//! `(c + 0.5)·length/width`. Both mappings are public on [`SectionAxes`], and
//! the renderer calls them rather than restating them, so a hover readout and
//! the pixels it reads can never disagree.
//!
//! # The height axis is MSL
//!
//! The default axis is `[site_elev, site_elev + 20 km]` **km above mean sea
//! level**. 20 km above the antenna is over anything in the volume at any
//! range — the 19.5° cut reaches it at 55.9 km ground range and the 0.5° cut
//! never does — so the default never clips real data. MSL rather than
//! above-radar because that is the datum a sounding, a flight level and a
//! melting-layer height are all quoted in; a section is read *against* those.
//! The site height comes from [`crate::eet::radar_height_ft_near`] on
//! [`crate::sites::Datum::Feedhorn`] — the antenna, not the ground the tower
//! stands on — which is the same source and the same datum
//! `render::render_hhc_to_image` uses, so a section and the environmental
//! heights drawn beside it share one origin. It was the ground until the
//! datum became a word in the call, which put this axis a tower low.
//!
//! Note the two are not the same coordinate: [`crate::beam`] measures heights
//! **above the antenna**, so every row height crosses that boundary exactly
//! once, at [`SectionAxes::row_height_km_msl`]'s caller.
//!
//! # The ground track and the range ring are the same sphere
//!
//! Columns are great-circle points ([`beam::great_circle_point`]) and their
//! radar-relative coordinates come from [`beam::site_bearing_range_km`], both
//! on [`crate::types::EARTH_RADIUS_KM`] = 6371 km. That is deliberately the
//! same sphere `render::render_gate` projects gates onto, so a section samples
//! the ground the plan view put under the cursor.
//!
//! **It is now also the sphere the plan view's range ring is drawn on.** It
//! was not: [`crate::types::ImageBounds`] worked in `1.0 / 111.32` degrees per
//! km — a 6378 km sphere — and the 230 km ring was drawn at
//! `230 / 111.32` degrees of latitude, which converted back on 6371 is
//! 229.742 km. A point the ring put at 230 km therefore read **258.4 m
//! nearer the site** here, 1.15 px on a 2048-wide plan view. `ImageBounds`
//! reads [`crate::types::KM_PER_DEGREE_LAT`] now, which *is*
//! `EARTH_RADIUS_KM · π/180`, so the two agree exactly and
//! `the_ground_track_and_the_range_ring_are_the_same_sphere` pins that they
//! keep agreeing.
//!
//! The ring's radius is now the render's own `extent_km` rather than a
//! constant, and that changes nothing here: the disagreement was a *ratio* of
//! two spheres, so closing it closes it at every extent a sweep can ask for.
//! It would have been scale-free even unclosed — the raster's side follows its
//! extent ([`crate::types::raster_side_px`]), so 4.4522 px/km at the 230 km
//! floor against 4.4512 at a surveillance cut's ±460.11 km on 4096, and a fixed
//! fraction of a kilometre costs the same pixel on either frame: the seam was
//! 1.15 px on both. (That reach is 2.125 + 1832 × 0.25 = 460.125 km of slant
//! range; the 458 km this paragraph used to quote omitted the first gate and is
//! a figure no volume produces.)
//!
//! # Drawn to the line, where a plan view is drawn to the data
//!
//! A plan view's frame is now sized from its own sweep
//! ([`crate::types::plan_view_extent_km`]), so it no longer throws returns
//! away: a surveillance cut's 460 km and a TDWR's 417 km are drawn, where a
//! fixed ±230 km frame had nowhere to put them.
//!
//! A section has no such option, because its extent is not the data's — it is
//! the line the user drew, and that line routinely outruns the volume. So this
//! module draws the whole line and reports
//! [`SectionAxes::coverage_ground_range_km`]: the farthest ground range at
//! which this section actually found a gate. Compared against
//! [`SectionAxes::far_ground_range_km`] it says whether the drawing ran out of
//! data before it ran out of line — a question a plan view cannot be asked,
//! since its frame is cut to fit.
//!
//! # Two numbers that exist because a section can lie
//!
//! [`SectionAxes::tilt_count`] and [`SectionAxes::widest_tilt_gap_deg`] are not
//! diagnostics. A section drawn on a short ladder does not merely read low: it
//! **interpolates across the gap and draws a smooth layer that is not there**,
//! with no error, no `NaN` and no visible seam — and the result looks *better*
//! than the truth, because a real section is banded by the tilts and a
//! fabricated one is not. Nothing in the pixels can distinguish the two. These
//! two numbers are the only place a consumer can learn that a volume delivered
//! four cuts where its VCP declares fifteen, so they travel with the raster
//! rather than being available on request.
//!
//! [`SectionAxes::cone_of_silence_km`] is the same kind of number for the other
//! direction: over the site every rung's beam is at zero height, so the volume
//! has no ceiling to speak of and the top of the drawing is empty. Its extent
//! is *reported*, in kilometres along the line, rather than turned into a
//! threshold that refuses to draw — because how much of it matters depends on
//! the axis the caller asked for, and only the caller knows that.
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
//! * **A section that used to sit about a pixel off the plan view's range
//!   ring.** Neither cause survives. It was never the `cos e` correction —
//!   both renderers apply that now, so an echo is at the same ground range in
//!   both — and it is no longer the sphere either: this ground track and
//!   [`crate::types::ImageBounds`] are both on `EARTH_RADIUS_KM`, where the
//!   bounds used to imply 6378 and left a 258.4 m seam at 230 km. The
//!   paragraph above on the ground-track sphere has the measurement. It is
//!   listed here because the offset is what a reader of the old pictures
//!   remembers, not because it is still there.

use crate::beam;
use crate::par::*;
use crate::sampler::{Column, Sample, SampleStatus, VolumeSampler};
use crate::types::RadarProduct;

/// The wasm32 section width, named outside the cascade so a host build can
/// check it. See [`SECTION_WIDTH`].
pub const WASM_SECTION_WIDTH: usize = 1024;

/// The native section width. See [`SECTION_WIDTH`].
pub const NATIVE_SECTION_WIDTH: usize = 2048;

/// Width of a rendered section, in pixels.
///
/// These are the sizes a section has always been rendered at, now written down
/// instead of inherited: they used to read [`crate::types::IMAGE_SIZE`], which
/// stopped meaning "the picture a browser gets" when the web plan view went to
/// 2048. Following it there would have quadrupled a web section loop's
/// textures — eight resident frames at 8 MiB against a 48 MiB per-pane budget —
/// for a view whose cost the plan view's texture ceiling has nothing to say
/// about.
///
/// A section raster is not bounded by ground the way a plan view is, so
/// nothing here follows an extent. Both dimensions stay powers of two and both
/// stay inside the 2048 floor a phone browser may report. A 2 : 1 raster also
/// matches the shape of the thing drawn — see the module doc.
#[cfg(target_arch = "wasm32")]
pub const SECTION_WIDTH: usize = WASM_SECTION_WIDTH;
/// See the wasm32 arm above.
#[cfg(not(target_arch = "wasm32"))]
pub const SECTION_WIDTH: usize = NATIVE_SECTION_WIDTH;

/// Height of a rendered section, in pixels: half [`SECTION_WIDTH`]. See the
/// module doc for why half and not square.
pub const SECTION_HEIGHT: usize = SECTION_WIDTH / 2;

/// How far above the site the default height axis reaches, km.
///
/// Above every beam in the volume at every range: the 19.5° cut — the highest
/// any operational VCP flies — passes 20 km above the antenna at 55.9 km of
/// ground range and only climbs from there, and no lower cut gets there at all.
/// So the default axis clips no data anywhere, which is what lets it be a
/// default rather than a guess.
pub const DEFAULT_AXIS_HEIGHT_KM: f64 = 20.0;

/// Feet to kilometres, for the feedhorn height
/// [`crate::eet::radar_height_ft_near`] reports. The same factor
/// `render::render_hhc_to_image` and `hail::FT_TO_KM` use.
const FT_TO_KM: f64 = 0.0003048;

/// Ground range under which a column is not sampled at all, km.
///
/// Half of one 250 m super-resolution gate. Two things go wrong inside it and
/// neither announces itself. The bearing from
/// [`beam::site_bearing_range_km`] is `atan2` of two differences that have gone
/// to zero, so it is dominated by rounding and reaches `atan2(0, 0)` exactly
/// over the site; and every azimuth's gates converge there anyway, so whatever
/// bearing comes back names ground indistinguishable from every other bearing's.
/// Refusing is not a loss of data — the point is inside half a gate of the
/// antenna — but it makes the answer depend on the geometry rather than on the
/// last bits of a great-circle solution.
///
/// This is the "blind column" a line drawn across the site produces. How many
/// columns it covers is target-dependent — the guard is a 0.25 km window and a
/// column is `length/`[`SECTION_WIDTH`] wide, so a 200 km line sees two or three
/// natively and one or two on wasm — and it is honest either way: the radar
/// cannot see over its own head.
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
///
/// Every field is finite for any section [`render_section`] returns; the
/// request-shape refusals up front are what guarantees it
/// (`every_axis_number_of_a_rendered_section_is_finite`).
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
    ///
    /// "Found a gate" means the radar looked: a value, a below-threshold return
    /// and a range-folded one all count, because all three are the radar
    /// reporting on ground it illuminated. Only
    /// [`SampleStatus::BeyondRange`] and [`SampleStatus::NoCoverage`] do not.
    ///
    /// Read it against [`far_ground_range_km`](Self::far_ground_range_km):
    /// equal means the section drew its whole line, smaller means it ran out of
    /// data at this range and everything past it is empty. Zero means no column
    /// found anything at all.
    pub coverage_ground_range_km: f64,
    /// How much of the line, in km, lies under the cone of silence at this
    /// axis top — i.e. how many columns have the volume's ceiling below the
    /// topmost drawn row.
    ///
    /// Not a threshold and not a refusal: it is the along-line width of the
    /// region whose upper rows are [`SampleStatus::AboveVolume`], summed over
    /// the columns that are in it, so a line that enters and leaves the cone
    /// twice reports both crossings. A blind column (see the module doc) counts
    /// as inside it, having no ceiling at all.
    ///
    /// **The name is only true of a volume that flew its whole pattern.** It
    /// measures the region above the top *rung*, and mid-volume that rung is
    /// wherever the antenna has got to — a KMPX section four cuts into VCP 212
    /// tops out at 1.8°, so this reports most of the line as "cone of silence"
    /// when what it has measured is unscanned air. No consumer reads it yet;
    /// one that does should check
    /// [`top_tilt_deg`](Self::top_tilt_deg) against
    /// [`top_declared_cut_deg`](Self::top_declared_cut_deg) first and call it
    /// something else when they disagree, as
    /// `rustdar-egui`'s `describe_missing` does for the per-pixel version of
    /// exactly this conflation.
    pub cone_of_silence_km: f64,
    /// How many rungs the tilt ladder had for this moment.
    ///
    /// See the module doc: this and
    /// [`widest_tilt_gap_deg`](Self::widest_tilt_gap_deg) are the only evidence
    /// a consumer has that a smooth layer in the picture was interpolated
    /// across a gap rather than measured.
    pub tilt_count: usize,
    /// The largest angular step between adjacent rungs of the ladder, degrees.
    /// `0.0` for a single-rung ladder.
    pub widest_tilt_gap_deg: f64,
    /// The highest cut angle this ladder **has**, degrees — the top rung's VCP
    /// key, `0.0` for an empty ladder.
    ///
    /// [`widest_tilt_gap_deg`](Self::widest_tilt_gap_deg) says how coarse the
    /// ladder is *between* its rungs; this says where it stops, and mid-volume
    /// that is the only one of the two that means anything. A volume four rungs
    /// into its flight is all low, closely-spaced cuts, so its gap number is
    /// *better* than a complete volume's — the caption's own figures improve as
    /// the picture gets more truncated. This is the number that does not.
    pub top_tilt_deg: f64,
    /// The highest cut angle the coverage pattern **declares**, degrees.
    ///
    /// Read against [`top_tilt_deg`](Self::top_tilt_deg): equal means the
    /// volume flew its whole pattern and the ceiling in the picture is the
    /// radar's, lower means the ladder stopped short of what the pattern says
    /// and the ceiling is the *volume's* — either still filling, live, or
    /// abandoned. The two are the same pixels and completely different facts,
    /// and it is the second that turns
    /// [`SampleStatus::AboveVolume`](crate::sampler::SampleStatus::AboveVolume)
    /// from "the cone of silence" into "not scanned".
    ///
    /// See [`VolumeSampler::top_declared_cut_deg`](crate::sampler::VolumeSampler::top_declared_cut_deg)
    /// for why this is a comparison of the tops rather than of the counts.
    pub top_declared_cut_deg: f64,
}

impl SectionAxes {
    /// Whether every number here is finite.
    ///
    /// [`render_section`] guarantees it — the request-shape refusals up front
    /// are what buy it, and `every_axis_number_of_a_rendered_section_is_finite`
    /// pins it — but a set of axes that arrived over a wire has had no such
    /// pass made over it, so [`CrossSection::from_parts`] makes one. What a
    /// non-finite axis costs is not a panic but silence: the two mapping
    /// functions above are affine in these fields, so a `NaN` `top_km_msl`
    /// makes **every** row height and every column distance `NaN`, and a
    /// consumer that converts a pointer position into a height and a distance
    /// then formats `NaN km MSL` into a readout, or draws an axis tick at a
    /// coordinate that is not a number and gets nothing. An infinite
    /// `length_km` is the same shape of failure in the other mapping.
    ///
    /// [`tilt_count`](Self::tilt_count) is a `usize` and has no non-finite
    /// value to have; every other field is an `f64` and is checked. The two
    /// tilt angles are in here too and for the same reason as the rest: a `NaN`
    /// [`top_tilt_deg`](Self::top_tilt_deg) compares false against
    /// [`top_declared_cut_deg`](Self::top_declared_cut_deg) whatever it holds,
    /// so a consumer asking "did this volume reach the top of its pattern?"
    /// gets a confident *no* and captions an ordinary complete volume as
    /// truncated.
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
    /// than clamping, so a caller converting a pointer position that sits a
    /// pixel off the pane gets a height a pixel off the axis instead of a
    /// silently pinned one.
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
///
/// The fields are private and the lengths are checked in
/// [`from_parts`](Self::from_parts), because a mis-shaped section is not a
/// recoverable error anywhere downstream. `rustdar-frontend`'s
/// `app_render::apply_render_to_pane` builds a `ColorImage` from a buffer and a
/// size (`app_render.rs:331`); the length check is `epaint`'s own, an
/// `assert_eq!` inside `ColorImage::from_rgba_unmultiplied`
/// (`epaint-0.35.0/src/image.rs:114`). It runs on the **main thread**, live in
/// release, and under wasm a main-thread panic takes the whole app down. A
/// decoder handed a short payload has to find out here instead.
#[derive(Debug, Clone)]
pub struct CrossSection {
    image: Vec<u8>,
    values: Vec<f32>,
    status: Vec<u8>,
    axes: SectionAxes,
    /// Where the ladder's rungs actually are, in degrees of beam elevation, in
    /// the cut order the sampler resolved them in.
    ///
    /// See [`tilt_elevations_deg`](CrossSection::tilt_elevations_deg) for why
    /// this travels with the raster instead of being looked up.
    tilt_elevations_deg: Vec<f64>,
    /// When each of those rungs was flown, milliseconds since the Unix epoch,
    /// in the same order. See
    /// [`tilt_collected_ms`](CrossSection::tilt_collected_ms).
    tilt_collected_ms: Vec<i64>,
}

/// Equality that ignores a value where there is no value to compare.
///
/// **A derived `PartialEq` makes almost every section unequal to itself.**
/// Every non-`Value` pixel stores `f32::NAN` in `values`, and `NaN != NaN`, so
/// *one* such pixel anywhere in the raster is enough. That is not a corner
/// case — it is the common case, and the failure is total rather than rare:
///
/// * A section drawn entirely below the lowest beam — clear air well away from
///   a site — is `NaN` in every pixel.
/// * So is an ordinary convective section a few tens of km from the site. It
///   has `BeyondRange` where the upper cuts stop short, `BelowLowestBeam` under
///   the base tilt and `AboveVolume` in the cone, and any one of those is a
///   `NaN`. `a_section_with_no_values_still_equals_itself` exercises both, and
///   substituting derived semantics fails on the near-site one too.
///
/// WP-D's worker reply asserts `assert_eq!(execute(&…), None)` over a
/// `JobOutput` that contains one of these. Under a derive it would have broken
/// on almost any input, with nothing in the failure message saying why — so
/// this is load-bearing rather than tidy.
///
/// The same reasoning already produced a hand-written `PartialEq` on
/// [`crate::sampler::Sample`]; this is that decision applied to the plane form.
/// A pixel whose status *is* `Value` still compares as `f32`, so a `NaN` that
/// someone put in a `Value` remains unequal to itself — which is what a caller
/// who did that asked for.
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
    ///
    /// Four refusals, and every one of them is about a section that arrived
    /// from somewhere this build does not control:
    ///
    /// * **A plane that is not exactly this build's [`SECTION_WIDTH`] ×
    ///   [`SECTION_HEIGHT`].** Not a recoverable error anywhere downstream:
    ///   `rustdar-frontend`'s `app_render::apply_render_to_pane` builds a
    ///   `ColorImage` from a buffer and a size, and the length check is
    ///   `epaint`'s own `assert_eq!` inside `ColorImage::from_rgba_unmultiplied`
    ///   (`epaint-0.35.0/src/image.rs:114`), on the **main thread**, live in
    ///   release, where under wasm it takes the whole app down. It is also the
    ///   ordinary shape of a cross-build payload: this constant is 2048 native
    ///   and 1024 on wasm.
    /// * **A status byte this build cannot name.** That is what a payload from
    ///   a newer sender looks like, and [`sample`](Self::sample) would
    ///   otherwise have to invent an answer for one. Refusing keeps every
    ///   accessor total.
    /// * **A non-finite axis** — see [`SectionAxes::all_finite`] for what one
    ///   costs, which is a readout full of `NaN` rather than a crash.
    /// * **A pixel whose status is [`SampleStatus::Value`] but whose value is
    ///   not finite.** The bar is `is_finite`, not `!is_nan`, and deliberately:
    ///   an infinity passes every `is_nan` test, reaches
    ///   [`crate::get_color_for_value`] as a number, compares as larger than
    ///   every threshold and paints the top of the scale — so a section
    ///   carrying one looks like the strongest echo in the volume rather than
    ///   like corruption. `NaN` at least paints nothing. Both are refused.
    ///
    /// The last of these is the pairing the whole status plane exists to keep
    /// straight, and [`render_section`] never breaks it — every writer of the
    /// two planes goes through one [`Sample`], and
    /// `the_three_planes_agree_everywhere` sweeps a whole raster to say so.
    /// It is checkable only here because only here can the two planes have
    /// come from different senders.
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
        // **The two ladders are the same ladder, by construction.** A consumer
        // drawing the rungs over the picture has to know that the angles it is
        // drawing are the angles the picture was sampled at, and before this
        // the only thing standing between it and a fabrication was a UI-side
        // count comparison against a *separately discovered* elevation list —
        // which counted something else (medians rounded to 0.1°, deduped) and
        // so disagreed on half of all precipitation-mode volumes, complete ones
        // included. Refusing here is what lets that comparison go away
        // entirely: there is one ladder, it arrives with the raster, and it
        // cannot be a different length from the count that describes it.
        //
        // Non-finite refused for the same reason every axis number is: a `NaN`
        // rung draws no curve and reports no error, so the honesty device goes
        // quiet in exactly the way nothing notices.
        if tilt_elevations_deg.len() != axes.tilt_count
            || !tilt_elevations_deg.iter().all(|deg| deg.is_finite())
        {
            return None;
        }
        // The clocks are one per rung too, and checked for the same reason the
        // angles are: a consumer zips the two lists to say "this rung, this
        // old", and a short list would either silently truncate the ladder or
        // pair a rung with its neighbour's age — an age is a number a reader
        // has no way to sanity-check by looking at the picture.
        if tilt_collected_ms.len() != axes.tilt_count {
            return None;
        }
        // One pass over the two planes that have to agree with each other, so
        // the unknown-code test and the value-pairing test cannot drift apart
        // into two walks with two different ideas of which pixel is which.
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
    ///
    /// # Why this travels with the raster
    ///
    /// Drawing the rungs across the picture is the section's **first** honesty
    /// device (see the `rustdar-egui` section pane's module doc): data exists
    /// along those curves and everything between them is two-point
    /// interpolation, and the curves fan apart with range at exactly the rate
    /// the error grows. A curve drawn at the wrong angle is worse than no
    /// curve, so a consumer needs the ladder the section was *cut* from, not a
    /// ladder it discovered some other way.
    ///
    /// There is no other way that works. `ScanInfo::discover_product_elevations`
    /// rounds each sweep's median to 0.1° and dedups; [`crate::sampler`] groups
    /// by the cut table's nominal angle. Those count different things, and they
    /// disagree whenever two sweeps of one cut have medians straddling an
    /// `x.x5` boundary — measured at KLNX on a **complete** volume, where one
    /// 0.4834° cut flown at medians 0.4394 and 0.4779 became two entries for
    /// one rung, 16 against 14. Across 19 sites, five of ten complete VCP
    /// 212/215 reflectivity volumes disagreed. A consumer comparing counts to
    /// decide whether to draw was therefore silent on half of all
    /// precipitation-mode volumes — exactly where the ladder is coarse enough
    /// for the interpolation to matter.
    ///
    /// These are the chosen sweeps' **median** elevations, which is the angle
    /// every height in the section was computed from, and so the angle a curve
    /// has to be drawn at for it to lie where the data is. The ladder's
    /// *identity* — the VCP keys it was grouped by — is
    /// [`SectionAxes::top_tilt_deg`]'s business, not this list's.
    pub fn tilt_elevations_deg(&self) -> &[f64] {
        &self.tilt_elevations_deg
    }

    /// When each rung of [`tilt_elevations_deg`](Self::tilt_elevations_deg)
    /// was flown, milliseconds since the Unix epoch, in the same order and the
    /// same length. `0` for a rung whose chosen sweep carried no clock.
    ///
    /// # Why one time for the whole section is a lie
    ///
    /// A section *looks* like an instant. It is not: the radar flies one tilt
    /// at a time, so the bottom rung and the top rung of the same picture are
    /// minutes apart — four to five on a VCP 212, ten on a clear-air pattern —
    /// and a SAILS repeat can leave one rung in the middle newer than both of
    /// its neighbours. The pane used to caption the whole thing with a single
    /// volume time, which reads as a photograph taken at that moment.
    ///
    /// The list is the ladder's own, from
    /// [`VolumeSampler::collection_times_ms`](crate::sampler::VolumeSampler::collection_times_ms),
    /// which reads the chosen sweep's radials — the same sweeps
    /// [`crate::sampler::ladder_fingerprint`] keys the re-cut on. There is no
    /// second notion of which sweep a rung came from anywhere, and there must
    /// not be: an age attached to the wrong rung is unfalsifiable by looking at
    /// the picture.
    ///
    /// It travels with the raster for the reason
    /// [`tilt_elevations_deg`](Self::tilt_elevations_deg) does, and one more:
    /// the pane keeps a section across a suspend/resume and re-uploads it
    /// rather than re-cutting, precisely because the volume behind it may have
    /// been evicted by then. Anything looked up beside the section would be
    /// gone exactly when the section is still on screen.
    pub fn tilt_collected_ms(&self) -> &[i64] {
        &self.tilt_collected_ms
    }

    /// How long this ladder took to fly — see [`assembly_span_secs`].
    pub fn assembly_span_secs(&self) -> Option<i64> {
        assembly_span_secs(&self.tilt_collected_ms)
    }

    /// The sample behind one pixel, re-paired from the value and status planes.
    ///
    /// This is what a hover readout wants: it can say "below the lowest beam"
    /// or "range folded" instead of nothing, which is the whole reason the
    /// status plane travels beside the values. `None` outside the raster.
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
///
/// **`None` means "nothing here knows when anything was flown"**, and `Some(0)`
/// means "as far as the clocks go, all at once". Those are different claims and
/// collapsing them is how a caption comes to assert the second when it only has
/// grounds for the first — a Level III-sourced or hand-built section carries no
/// radial clocks at all, and it must stay silent rather than call itself
/// instantaneous.
///
/// A free function rather than only a method because a **loop frame** drops the
/// `CrossSection` behind its raster (the value and status planes are ~18 MB a
/// frame) and keeps only the labels, this list among them.
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
///   [`VolumeSampler::new`] refuses — most importantly one whose coverage
///   pattern is the empty placeholder a worker's reconstructed scan carries,
///   which would otherwise build a *different tilt ladder* from the main
///   thread's with no error anywhere.
///
/// `storm_motion_override` is the user's `(speed_kt, direction_from_deg)`
/// vector, read only when `req.product` is storm-relative velocity — the
/// same pair the plan-view SRV render receives, threaded from the
/// `RenderInput` by the worker's job handler.
///
/// Every refusal is logged, so a `None` swallowed by a `?` still leaves its
/// reason somewhere.
pub fn render_section<'a>(
    volume: impl Into<crate::nyquist::Volume<'a>>,
    req: &SectionRequest,
    lat: f64,
    lon: f64,
    storm_motion_override: Option<(f32, f32)>,
) -> Option<CrossSection> {
    let volume = volume.into();
    // The derivation seam: native moments pass through as a borrow; derived
    // products are computed here, per sweep, before anything samples — so a
    // raw volume can never be sampled under a derived label (the sampler's
    // own gate still refuses that combination).
    let prepared = crate::derive::prepare(volume, req.product, storm_motion_override)?;
    // The declared Nyquist table follows the scan through the derivation: it
    // is keyed by elevation number, which `prepare` preserves, and a derived
    // scan's rungs are the same cuts flown at the same PRFs. `prepare` reads
    // the same table on the way in — SRV and NROT unfold around the limit each
    // cut declared — so the field that arrives here was built against the
    // limits this sampler is about to guard on.
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
///
/// Private on purpose. Sharing one sampler across several sections of the same
/// moment is a real saving — the ladder and the per-rung azimuth index are
/// resolved once — but exposing it would let a caller pass a sampler built for
/// a *different* product than `req.product` names, and the colours would then
/// come from one scale while the numbers came from another. Nothing about the
/// two would look wrong. If a consumer ever needs the saving, the entry point
/// it should get is one that takes the product once.
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

    // Finite by construction, given the finite endpoints above: the haversine
    // is clamped to `0..=1` before the square root, so the range cannot come
    // back `NaN` and there is no non-finite case here to guard — only the
    // coincident one.
    let (_, length_km) =
        beam::site_bearing_range_km(req.start.0, req.start.1, req.end.0, req.end.1);
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
    //
    // Coordinates the table cannot place have no MSL datum to add, so the axis
    // runs from the antenna. See `render::render_site_height_ft`.
    let base_km_msl = crate::eet::radar_height_ft_near(lat, lon, crate::sites::Datum::Feedhorn)
        .unwrap_or(0.0)
        * FT_TO_KM;
    let top_km_msl = req
        .top_km_msl
        .unwrap_or(base_km_msl + DEFAULT_AXIS_HEIGHT_KM);
    // Finiteness is tested separately from the ordering, because `inf` passes
    // the ordering: an infinite top is "above" the site and would give every
    // row an infinite height, a `NaN` step and a raster of `NoCoverage` that
    // looks exactly like a volume with no data in it.
    if !top_km_msl.is_finite() || top_km_msl <= base_km_msl {
        log::warn!(
            "cross-section refused: a top of {top_km_msl} km MSL is not a \
             finite height above the {base_km_msl} km MSL site",
        );
        return None;
    }

    // The axes, less the four measurements the columns produce. They are filled
    // in below rather than defaulted, so a field added here and forgotten there
    // does not ship as a plausible zero.
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

    // Seeded exactly as the three `vec!`s this replaced were, and carried from
    // the cut before this one rather than allocated afresh. See [`POOLED_PLANES`].
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

    // Not `from_parts`: this is the writer the constructor's refusals exist to
    // check *other* senders against, and going through it would mean handing it
    // three planes it just built and then unwrapping an `Option` that cannot be
    // `None`. The ladder is the sampler's own, so it is `tilt_count` long by
    // the same construction that produced the count.
    Some(CrossSection {
        image,
        values,
        status,
        axes,
        tilt_elevations_deg: sampler.elevations_deg().collect(),
        // The same ladder, in the same cut order, so the two lists zip into
        // "this rung, this old" without either being re-derived.
        tilt_collected_ms: sampler.collection_times_ms().collect(),
    })
}

/// The three planes of one section, held between cuts instead of allocated
/// inside each one.
///
/// # What it is worth
///
/// A native section is `2048 × 1024` pixels, so the three planes are 8 MiB of
/// RGBA, 8 MiB of `f32` values and 2 MiB of status bytes: **18,874,368 bytes,
/// 4,608 pages**. All three are well under glibc's
/// `DEFAULT_MMAP_THRESHOLD_MAX` of 33,554,432, so this is *not* the `mmap`
/// cliff that `crate::render`'s `POOLED_CELLS` and
/// `rustdar_frontend::volume_raymarch::coverage_premultiplied_into` were fixed
/// for — these blocks come out of an arena, which is exactly the problem.
///
/// A cut runs on a thread of its own (`rustdar_frontend::offload` spawns one
/// per job natively), so it takes whichever arena glibc hands that thread; the
/// planes are freed when the section is dropped, the arena trims, and the next
/// cut faults all 4,608 pages back in. Which arena it gets is decided by the
/// parallel LDM decode that ran first: `scan::decoded` fans 50–130 records
/// across the whole rayon pool, and a 32-thread pool leaves the process with
/// enough arenas that no one of them keeps a warm 8 MiB chunk to hand back.
///
/// Measured on this box — 32 cores, `--release` (`lto = true`, `opt-level = 3`),
/// loadavg 3.5–4.5 — over two volumes cut six ways each on a fresh thread per
/// cut, nine cuts per geometry, interleaved arms with a fresh process for each,
/// in a process that had already run the parallel decode. The ranges are across
/// the six geometries, worst of them to best:
///
/// | | before | after |
/// |---|---|---|
/// | KFTG (21 sweeps), best ms | 9.01–12.61 | **4.08–7.41** |
/// | KFTG, median ms | 9.57–12.94 | **4.33–7.63** |
/// | KTLX (8 sweeps), best ms | 2.55–6.55 | **2.54–3.31** |
/// | minor faults per cut | 3,314–4,916 | **0–9** |
/// | allocations ≥ 1 MiB per cut | 3 | **0** |
/// | allocations per cut, all sizes | 6,246 | 6,243 |
/// | control, best ms | 2.38 | 2.36 |
///
/// The control is a fixed, allocation-free compute kernel over a slab already
/// faulted in, run on a fresh thread beside every cut. It measures the box
/// rather than the change and it does not move — 2.35–2.38 ms in every arm of
/// every round, on both volumes — which is what says the section's numbers are
/// the section's.
///
/// The fault count is the whole of the finding: 4,608 of those pages are these
/// three planes, and the count is the same on every cut, on a raster whose size
/// is a build constant. It is also the load-insensitive half of the evidence —
/// the timings move with the box, the faults do not, and the same run under a
/// loadavg of 10–16 gave 8.47–11.63 ms against 4.27–7.68 with these same two
/// fault columns. The last row but one is the same fact from the other side:
/// **three** allocations left, and they are the three planes. Nothing else the
/// cut does was ever the problem, which is why the 6,243 that remain — the
/// per-column tilt ladders, above all — are left alone. They cost 0 faults once
/// these three stop churning the arena, so pooling them too would buy nothing
/// that can be measured.
///
/// The light volume is where the shape of the defect is clearest. Its "before"
/// column is bimodal, not merely slower: the first cuts in a process pay
/// 3,200–3,700 faults and later ones pay none, because a small volume
/// eventually leaves the arena a chunk it can reuse and a large one never does.
/// After, every cut of both volumes is flat and faults nothing.
///
/// The decode itself does not move: 67.1 and 68.6 ms before against 68.3 and
/// 64.3 after on KFTG, 13.3/15.2 against 14.0/12.8 on KTLX, and over eight
/// further fresh processes per arm a best of 62.0 ms before against 56.3 after.
/// Noise in both directions and no systematic change, which is what a pool that
/// is not in the decode should look like.
///
/// `MALLOC_ARENA_MAX=1` removes the same faults and is **not** the fix. It is
/// process-global, it would apply to every allocation in the app, and it is a
/// trade rather than a win: the decode is precisely the thing that wants many
/// arenas, and capping them to one costs it **+41% to +53% on KFTG and +28% to
/// +37% on KTLX**. What it bought the section is what this pool buys without
/// charging the decode anything.
///
/// Those percentages were re-measured, and the ones this paragraph used to
/// carry were badly stale. It read 53.3 → 99.8 ms on KFTG and 13.8 → 29.4 ms on
/// KTLX — +87% and +113% — which was taken before the decode became the
/// single-pass `par_iter` it is now, so neither the absolute times nor the
/// ratio survive into the present tree. Re-taken on the current decode over two
/// volumes, two independent interleaved passes of 20 and 25 rounds, a fresh
/// process for every run, best-of-N: the cost is about half what this claimed.
/// The conclusion is unchanged, which is why only the numbers moved.
///
/// # Why the section, and what else is standing in the same place
///
/// The section is what this change fixes. It is **not** the only consumer with
/// this exposure, and the two neighbours are worth naming precisely because the
/// first survey of them was wrong in both directions.
///
/// **The plan view is worse, and is not fixed here.**
/// `crate::render`'s `RenderBuffers::into_output` allocates a fresh
/// `value_data: Vec<f32>` *and* a fresh `image: Vec<u8>` — 16 MiB each at side
/// 2048 — writes both in full, and hands both to the caller. Only `cells` is
/// pooled on that path. Measured on the same per-job thread after the same
/// decode: **4,895–14,481 minor faults per render**, more than this path was
/// paying. It is the larger case and it has work of its own; this paragraph
/// exists so the next reader does not inherit a survey that missed it.
///
/// **The voxel grid is smaller than it looks, and improves for free.**
/// [`crate::voxel::build_voxels`] is the obvious neighbour — same position
/// after the decode, an index plane of exactly 8 MiB at
/// [`crate::voxel::DESKTOP_SHAPE`]'s budget — and on a fresh thread per build
/// it cost 762–778 faults against this path's 4,608, with **9.77–11.33 ms
/// against 9.82–10.54 ms** under `MALLOC_ARENA_MAX=1`: no difference worth a
/// change. (Timed while that budget was built as 256 × 256 × 128;
/// [`crate::voxel::shape_for_budget`] now spends the same cells on
/// 512 × 512 × 32, which roughly doubles the millisecond figure — measured in
/// that module's own table. What this paragraph turns on is the **fault**
/// count, which is a property of the allocation, and the allocation has not
/// moved.) In
/// the production arm (`values_wanted: false`) it went **2,110–2,148 faults
/// before this pool to 1–424 after it**, without being touched — the 18 MiB
/// held here is what stops the arena trimming under it. So a grid that leaves
/// the builder the way a section does would cost the same `Drop` and the same
/// resident spare to buy something it is already getting. It is left alone.
///
/// # Why one process-wide slot, and not the caller's buffer
///
/// This is `crate::render`'s `POOLED_CELLS` argument, and it applies here
/// unchanged because the callers are the same three:
/// `rustdar_frontend::offload::execute` is documented *pure* and is the one
/// implementation shared by a fresh `std::thread` per job on native, a browser
/// worker reached over a message port that cannot be handed a pointer, and the
/// inline fallback. None of the three has a buffer to lend.
/// `rustdar_frontend::volume_bridge::VolumeResources::staging` is the other
/// shape and is right where it is used, because *its* caller is the frame
/// thread, which spans every upload. There is no such caller here.
///
/// A `thread_local` is worse than useless for the same reason: the native job
/// thread is created for one cut and joined after it, so the reuse rate would
/// be exactly zero.
///
/// One slot rather than a free list, again as the cells pool has it. Two
/// sections alive at once — a pane's and a loop frame's — means the second
/// allocates, exactly as it would with no pool, plus one uncontended `Mutex`
/// acquire around an `Option::take`. What is bought is the sequential case,
/// which is every case measured above: a pane re-cutting, a product switching,
/// a loop stepping.
///
/// # Residency
///
/// **One spare set of planes, from the first cut onwards**: 18 MiB native,
/// 4.5 MiB on wasm32, where the raster is a quarter the pixels. It is a spare
/// rather than an addition to the peak — a section that is alive is holding
/// the buffers, and the slot is empty while it does — so the ceiling is "the
/// sections alive at once, plus one".
///
/// wasm32 carries it too, and has less to win: dlmalloc in a linear memory
/// that never shrinks recycles an 8 MiB block as readily as any other and
/// there are no fresh zero pages to fault. It is carried regardless, because a
/// `cfg`-gated behavioural split here would be a second section renderer that
/// no row of this workspace's gate runs the tests of. Nothing in this module
/// is target-conditional; [`crate::par`] is where the one real target split
/// lives.
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
    ///
    /// # Why the seed is re-established rather than assumed
    ///
    /// [`render_with_sampler`]'s raster loop writes **every** pixel of all
    /// three planes — `columns` is `0..SECTION_WIDTH` long unconditionally and
    /// the row chunks cover the buffer exactly — so a pooled buffer's contents
    /// are, today, unobservable. That is a property of one loop, not of this
    /// type, and it is the property that has already been broken twice in this
    /// campaign. Re-seeding costs what the three `vec!`s cost anyway: two of
    /// them were an allocation plus a fill, and the third an `alloc_zeroed`
    /// whose zero pages are exactly the 2,048 faults this pool exists to
    /// remove. So the safe version is also the cheaper one, and what a caller
    /// gets back is indistinguishable from a fresh allocation rather than
    /// merely equal to one on the paths that exist right now.
    ///
    /// `clear` then `resize` rather than `resize` then `fill`: it is total in
    /// both directions in one step. The raster is a build constant, so a
    /// buffer coming out of the slot is always already the right length and
    /// the resize is a fill over retained capacity — but "always" here is a
    /// fact about two constants, and a length that is *made* correct cannot
    /// disagree with the one the section is about to claim, where a
    /// `debug_assert` would leave a release build indexing off the end.
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
    // fallback allocation below — 18 MiB and its page faults, under a
    // process-wide mutex, exactly where concurrent cuts that all miss the slot
    // would queue on each other instead of allocating in parallel as they do
    // with no pool at all.
    //
    // **Nothing catches this if it is edited back.** It is not a nursery lint
    // that this gate happens not to run: `clippy::significant_drop_in_scrutinee`
    // and `significant_drop_tightening` were both enabled explicitly against
    // the method-chain version and **neither fires**, because they look at
    // `match` and `if let` scrutinees rather than at a temporary in a chain. Nor
    // does any test — holding the lock across the allocation changes no output,
    // only contention, so a mutation that does it passes the whole suite. The
    // guarantee is the `let` and this comment, and it was verified by
    // measurement rather than by reading: with the two statements separated a
    // `try_lock` taken inside the fallback **succeeds**, and with them fused it
    // **fails**.
    let taken = pool().take();
    let mut planes = taken.unwrap_or_else(SectionPlanes::empty);
    planes.fit();
    planes
}

/// Offer a set of planes back, keeping them only if the slot is free.
///
/// Called from [`CrossSection`]'s [`Drop`], which is the only place a section's
/// planes stop being reachable. See [`POOLED_PLANES`] for why the slot is one
/// and not many.
fn recycle(planes: SectionPlanes) {
    let mut pool = pool();
    if pool.is_none() {
        *pool = Some(planes);
    }
}

/// The pool, with a poisoned lock read as a live one.
///
/// **What the lock covers is one `Option::take` in [`checkout`] and one
/// `is_none` plus a move-assign in [`recycle`], and nothing else.** The 18 MiB
/// fallback allocation, the re-seed, the whole raster loop and the drop of a
/// set [`recycle`] declines to keep are all outside it — the guard goes out of
/// scope before the argument does, which is a fact about Rust's drop order for
/// a local against a by-value parameter and was checked rather than assumed.
/// That is what makes it a lock cuts never contend on for any measurable time,
/// and it is also why nothing under it can panic, which makes poisoning
/// unreachable. Recovering rather than unwrapping keeps a panic that cannot
/// happen out of the renderer anyway.
fn pool() -> std::sync::MutexGuard<'static, Option<SectionPlanes>> {
    POOLED_PLANES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A section's planes go back to the pool when the section does.
///
/// This is the counterpart of `crate::render`'s `RenderBuffers::into_output`,
/// and it is a `Drop` rather than a step in the renderer because the section's
/// planes **are** its output: they leave [`render_with_sampler`] inside the
/// `CrossSection` and the last moment at which they are nobody's is this one.
///
/// Every `CrossSection` that exists has planes of exactly the raster's size —
/// [`render_with_sampler`] builds them that way and [`from_parts`](CrossSection::from_parts)
/// refuses anything else — so what goes back is always what the next cut wants,
/// whichever route the section came by. A section decoded off a worker's reply
/// therefore feeds the pool as readily as one this thread cut, which is the
/// case that matters on the web.
///
/// `mem::take` leaves three empty `Vec`s behind for the ordinary field drops to
/// run over, which is why this does not double-free and does not need
/// `ManuallyDrop`.
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
    /// Ground range from the site, km. Kept beside the [`Column`] rather than
    /// read back off it because a blind column carries no coordinates at all
    /// and this is the number the coverage and cone measurements are made in.
    ground_range_km: f64,
    /// The ladder, or an empty one for a blind column — which answers
    /// [`SampleStatus::NoCoverage`] at every height, so the raster loop needs
    /// no second branch.
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
            // A fraction of the line's *angle*, which `great_circle_point`
            // makes exactly a fraction of its ground range — so this is the
            // point `column_distance_km(col)` names, and not merely near it.
            let t = axes.column_distance_km(col) / axes.length_km;
            let point = beam::great_circle_point(req.start, req.end, t);
            let (azimuth_deg, ground_range_km) =
                beam::site_bearing_range_km(lat, lon, point.0, point.1);
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
///
/// **Strict**: a column at exactly the guard's range is sampled, not blinded.
/// No great-circle solution lands on that float, so this is a statement about
/// which way the boundary rounds rather than about anything a user will see —
/// and it is a named function precisely because of that. Left inline as a `<`
/// it is a comparison no test can distinguish from `<=`, and a later edit could
/// widen the blind slit by one boundary case with the whole suite still green.
/// `the_two_boundary_predicates_round_the_way_the_docs_say` pins it.
fn is_blind(ground_range_km: f64) -> bool {
    ground_range_km < BLIND_GROUND_RANGE_KM
}

/// Whether a column's ladder ceiling leaves the topmost drawn row above the
/// volume — the cone-of-silence test.
///
/// **Strict, and here the strictness is load-bearing rather than arbitrary.**
/// [`Column::at_height_km`] answers [`SampleStatus::AboveVolume`] only for a
/// height *strictly* over the highest rung; at exactly the rung's height it
/// returns that rung's own sample. So a `<=` here would count a column whose
/// ceiling lands on the top row as inside the cone while its top pixel carried
/// a value — breaking the equivalence
/// [`SectionAxes::cone_of_silence_km`] is documented by, in the one case no
/// rendered fixture can reach.
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

        // "The radar looked here": a value, a below-threshold return and a
        // folded gate all say so. Only `BeyondRange` and `NoCoverage` mean
        // there was no gate at all.
        let illuminated = at.column.rungs().iter().any(|rung| {
            matches!(
                rung.sample.status(),
                SampleStatus::Value | SampleStatus::BelowThreshold | SampleStatus::RangeFolded
            )
        });
        if illuminated {
            coverage = coverage.max(at.ground_range_km);
        }

        // In the cone when the ladder's ceiling is below the topmost drawn row,
        // which is exactly the condition under which that row comes back
        // `AboveVolume`. A blind column has no ceiling and is the middle of it.
        let in_cone = at
            .column
            .height_span_km()
            .is_none_or(|(_, ceiling_km)| ceiling_is_under(ceiling_km, top_row_arl_km));
        if in_cone {
            cone_columns += 1;
        }
    }

    // `min(far)` rather than a finiteness test on the seed. For a non-empty
    // raster — and [`SECTION_WIDTH`] is a nonzero constant, so it always is —
    // the nearest column is under the farthest and this is the identity. What
    // it buys is that the `INFINITY` seed cannot escape into the axes if that
    // ever stops being true, without an unreachable branch nothing can pin.
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
/// legend instead would paint a floor the plan view leaves empty. Non-`Value`
/// samples carry `f32::NAN`, which the same function already answers
/// `(0, 0, 0, 0)` for, so there is no missing-data branch to keep in step.
///
/// The one arm is the fold. A range-folded gate has no number to colour and
/// would otherwise vanish into the same transparency as ground the radar never
/// looked at. The plan view paints the same purple from its own gate loop, so
/// the two views agree about the colour; what they do not share is the
/// **readout** — a section can say `RangeFolded` because it carries a status
/// per sample, and the plan view's value grid has only a number to hand back,
/// so a hover there reads "no data" over a pixel this colour.
fn section_color(product: RadarProduct, sample: Sample) -> (u8, u8, u8, u8) {
    if sample.status() == SampleStatus::RangeFolded {
        return crate::palette::RANGE_FOLDED;
    }
    crate::get_color_for_value(product, sample.value_or_nan())
}

// ── Codec ────────────────────────────────────────────────────────────────────
//
// The payload type owns its codec; the job framing that carries it lives in
// `rustdar-frontend`'s `offload`. That split is `render_input`'s, kept for the
// reason it was made there: a section that can encode itself can be put on a
// message port, in an IndexedDB blob or in a test fixture without any of the
// three learning its layout, and there is one place where the layout is
// written down.
//
// So the frame is self-delimiting and self-describing — its own magic, its own
// version, its own lengths — rather than relying on the envelope to say how
// long it is or what it is. An envelope that had to know would be a second
// description of this layout.

/// Identifies a section payload, so a message that is not one fails on its
/// first four bytes instead of being read as a wildly-sized allocation.
///
/// Distinct from `render_input`'s `RDRI` on purpose: the two travel over the
/// same port, and a job that carried the wrong one has to fail here rather
/// than deep inside a decode that happens to line up.
const MAGIC: [u8; 4] = *b"RDXS";

/// Bumped whenever the layout below changes. The two ends of a worker boundary
/// can be different builds — see `rustdar-web`'s build-token handshake — so a
/// mismatch has to be a clean `None`, not a misparse.
///
/// * **1 → 2**: the axes gained `top_tilt_deg` and `top_declared_cut_deg`, and
///   the section gained the ladder's own rung elevations. A version 1 payload
///   is not a version 2 payload missing three fields — it is a payload whose
///   consumer would have to invent a ladder to draw, which is the fabrication
///   the whole change exists to remove. So it is refused rather than defaulted.
/// * **2 -> 3**: the section gained each rung's **collection timestamp**, beside
///   the rung elevations it already carried. A version 2 payload is a ladder
///   with no clocks, and a consumer would have to date its rungs from the one
///   volume time the pane already had — which is exactly the single-instant
///   claim [`CrossSection::tilt_collected_ms`] exists to withdraw. Refused
///   rather than zero-filled, because a zero here is indistinguishable from a
///   real sweep that carried no clock.
const FORMAT_VERSION: u16 = 3;

impl CrossSection {
    /// Encode for transport. Little-endian throughout; the image and status
    /// planes are copied verbatim, which is where nearly all the bytes are.
    ///
    /// The value plane is written as raw `f32` bit patterns, so a `NaN` keeps
    /// the payload it arrived with. That matters for what the round trip can
    /// claim: [`PartialEq`] ignores a value under a non-`Value` status, so
    /// equality would survive a lossier encoding, but a **byte** comparison of
    /// two encodings of the same section would not.
    ///
    /// A raster is a fixed size on any one build, so the three length prefixes
    /// are not needed to find the end of a plane — they are here to name the
    /// size the *sender* used. A payload encoded by the 1024-wide wasm build
    /// and decoded by the 2048-wide native one is the ordinary case, and it
    /// has to be refused by [`from_bytes`](Self::from_bytes) rather than read
    /// as a truncation of something else.
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
        // A `u32` for a `usize` field. The ladder has one rung per elevation
        // the volume flew — a couple of dozen on the longest operational VCP,
        // and the model numbers its cuts in a `u8` — so there is no reachable
        // count this narrows. `the_encoded_length_estimate_is_exact` would not
        // catch a truncation here, but nothing can produce one.
        out.extend_from_slice(&(axes.tilt_count as u32).to_le_bytes());
        for number in [
            axes.widest_tilt_gap_deg,
            axes.top_tilt_deg,
            axes.top_declared_cut_deg,
        ] {
            out.extend_from_slice(&number.to_le_bytes());
        }

        // The ladder itself. Its length is written even though `tilt_count`
        // already implies it, because `from_parts` is where the two are made to
        // agree and a decoder that derived one from the other could not hand it
        // a disagreement to refuse.
        out.extend_from_slice(&(self.tilt_elevations_deg.len() as u32).to_le_bytes());
        for elevation in &self.tilt_elevations_deg {
            out.extend_from_slice(&elevation.to_le_bytes());
        }
        // The clocks, length-prefixed for the same reason the angles are: the
        // two lists and `tilt_count` are made to agree in `from_parts`, and a
        // decoder that derived any of the three from another could not hand it
        // a disagreement to refuse.
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
    ///
    /// The plane checks are **read** rather than restated: everything past the
    /// framing goes through [`from_parts`](Self::from_parts), which is where a
    /// section arriving from anywhere is validated. A second copy of those
    /// rules here is how the wire and the constructor would come to disagree
    /// about which sections are acceptable.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let mut r = Reader::new(bytes);
        if r.take(4)? != MAGIC {
            return None;
        }
        if r.u16()? != FORMAT_VERSION {
            return None;
        }

        // Written in `SectionAxes`' declaration order, which is the order
        // `to_bytes` wrote them in: a struct literal evaluates its fields in
        // the order they appear here, so the two lists have to stay aligned
        // and are kept adjacent for that reason.
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

        // Eight bytes per element, so the claimed count is measured against
        // what remains before it becomes a capacity, exactly as the value plane
        // below is.
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

        // One byte per element, so `take` is the bound: it can only hand back
        // a slice that is really there, and nothing is reserved on the claimed
        // length before that.
        let image_len = r.u32()?;
        let image = r.take(image_len as usize)?.to_vec();

        // Four bytes per element, so the claimed count has to be measured
        // against what remains before it becomes a capacity — `u32::MAX` here
        // would otherwise reserve 16 GiB and then fail the read.
        let value_len = r.u32()?;
        let mut values = Vec::with_capacity(r.bounded(value_len, 4)?);
        for _ in 0..value_len {
            values.push(r.f32()?);
        }

        let status_len = r.u32()?;
        let status = r.take(status_len as usize)?.to_vec();

        // Trailing bytes mean the two ends disagree about the layout even
        // though the version matched. Better to refuse than to hand a pane
        // half a section from it.
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
    ///
    /// Exactly, not approximately: a section is 12 MB natively and a
    /// reallocation partway through copies all of it, so this is the
    /// difference between one allocation and several. Wrong by a little is
    /// only that copy; wrong by a lot means the layout and the estimate have
    /// drifted, which `the_encoded_length_of_a_section_is_exact` is what
    /// catches.
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
///
/// A private copy of `render_input`'s, deliberately rather than a shared one.
/// It is thirty lines with no state beyond an offset, and the alternative —
/// a public type, or a fourth crate for it — would make the byte layout of
/// three payloads depend on one shared decoder's idea of what a `u32` is.
/// Each module owning its own reader is what lets each own its own format.
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
