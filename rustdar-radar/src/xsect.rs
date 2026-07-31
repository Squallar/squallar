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
//! pixels and there is no pool. `section_timing` is the measurement.
//!
//! # The raster
//!
//! [`SECTION_WIDTH`] is [`crate::types::IMAGE_SIZE`] and [`SECTION_HEIGHT`] is
//! half of it. That inherits the wasm/native split — 1024 on the web, 2048
//! native — and with it the WebGL2 2048-texture rationale
//! [`crate::types::IMAGE_SIZE`] already carries, for free and with no second
//! `cfg` cascade. Both dimensions stay powers of two and both stay inside the
//! floor a phone browser may report. A 2 : 1 raster also matches the shape of
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
//! The site elevation comes from [`crate::eet::radar_height_ft_near`], the same
//! source `render::render_hhc_to_image` uses for the same datum, so a section
//! and the environmental heights drawn beside it share one ground.
//!
//! Note the two are not the same coordinate: [`crate::beam`] measures heights
//! **above the antenna**, so every row height crosses that boundary exactly
//! once, at [`SectionAxes::row_height_km_msl`]'s caller.
//!
//! # The ground track is 6371, and the range ring is not
//!
//! Columns are great-circle points ([`beam::great_circle_point`]) and their
//! radar-relative coordinates come from [`beam::site_bearing_range_km`], both
//! on [`crate::types::EARTH_RADIUS_KM`] = 6371 km. That is deliberately the
//! same sphere `render::render_gate` projects gates onto, so a section samples
//! the ground the plan view put under the cursor.
//!
//! **It is not the sphere the plan view's range ring is drawn on.**
//! [`crate::types::ImageBounds`] works in `1.0 / 111.32` degrees per km, which
//! implies a 6378 km sphere, and the 230 km ring is drawn at
//! `MAX_RANGE_KM / 111.32` degrees of latitude. Converted back on 6371 that
//! latitude offset is **229.742 km**, so a point the ring puts at 230 km reads
//! as **258.4 m nearer the site** here — 1.15 px on a 2048-wide plan view,
//! 0.58 px on the 1024-wide wasm one. This is a deliberate choice, not an
//! oversight: the alternative is to reproduce a known 0.11 % inconsistency in
//! the image bounds so that a section agrees with a *ring* rather than with the
//! *gates* it is made of. `the_ground_track_sphere_is_the_one_render_gate_uses`
//! measures both numbers so the seam cannot drift unnoticed.
//!
//! # Clipped to the data, not to `MAX_RANGE_KM`
//!
//! A plan view stops at [`crate::types::MAX_RANGE_KM`] because its frame spans
//! ±230 km and a gate outside it has nowhere to go. A section has no such
//! frame — it has the line the user drew — so clipping there would silently
//! discard real super-resolution returns, which reach 300 km on the Doppler
//! half of a split cut and 460 km on the surveillance half. So this module
//! draws the whole line and reports [`SectionAxes::coverage_ground_range_km`]:
//! the farthest ground range at which this section actually found a gate.
//! Compared against [`SectionAxes::far_ground_range_km`] it says whether the
//! drawing ran out of data before it ran out of line, which is exactly the
//! "declared extent matches the artifact" property a plan view's `max_range`
//! does not have.
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
//! * **A section that does not register with the plan view above ~2°.** The
//!   sampler applies the `cos e` slant→ground correction that `render_gate`
//!   omits — 0.2 km at 2.4° and 4.0 km at 19.5°. The section is the correct
//!   one.

use nexrad_model::data::Scan;

use crate::beam;
use crate::par::*;
use crate::sampler::{Column, Sample, SampleStatus, VolumeSampler};
use crate::types::{self, RadarProduct};

/// Width of a rendered section, in pixels: [`crate::types::IMAGE_SIZE`].
pub const SECTION_WIDTH: usize = types::IMAGE_SIZE;

/// Height of a rendered section, in pixels: half [`SECTION_WIDTH`]. See the
/// module doc for why half and not square.
pub const SECTION_HEIGHT: usize = types::IMAGE_SIZE / 2;

/// How far above the site the default height axis reaches, km.
///
/// Above every beam in the volume at every range: the 19.5° cut — the highest
/// any operational VCP flies — passes 20 km above the antenna at 55.9 km of
/// ground range and only climbs from there, and no lower cut gets there at all.
/// So the default axis clips no data anywhere, which is what lets it be a
/// default rather than a guess.
pub const DEFAULT_AXIS_HEIGHT_KM: f64 = 20.0;

/// Feet to kilometres, for the site elevation
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
    /// The moment to section. Anything [`crate::sampler::samplable`] refuses —
    /// the hybrid classification, the column integrals, the velocity
    /// derivations — makes [`render_section`] return `None`.
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
/// * a product [`crate::sampler::samplable`] refuses, or a volume
///   [`VolumeSampler::new`] refuses — most importantly one whose coverage
///   pattern is the empty placeholder a worker's reconstructed scan carries,
///   which would otherwise build a *different tilt ladder* from the main
///   thread's with no error anywhere.
///
/// Every refusal is logged, so a `None` swallowed by a `?` still leaves its
/// reason somewhere.
pub fn render_section(
    scan: &Scan,
    req: &SectionRequest,
    lat: f64,
    lon: f64,
) -> Option<CrossSection> {
    let sampler = VolumeSampler::new(scan, req.product).ok()?;
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

    let base_km_msl = crate::eet::radar_height_ft_near(lat, lon) * FT_TO_KM;
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

    let pixels = SECTION_WIDTH * SECTION_HEIGHT;
    let mut image = vec![0u8; pixels * 4];
    let mut values = vec![f32::NAN; pixels];
    let mut status = vec![SampleStatus::NoCoverage.wire_code(); pixels];

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
    })
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
/// looked at, which is the reporting `MomentValue::RangeFolded` has never had
/// from this crate.
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
const FORMAT_VERSION: u16 = 2;

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
        Self::from_parts(image, values, status, axes, tilt_elevations_deg)
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
mod tests {
    use super::*;
    use nexrad_model::data::{
        ChannelConfiguration, ElevationCut, MomentData, PulseWidth, Radial, RadialStatus, Sweep,
        VolumeCoveragePattern, WaveformType,
    };

    // ── Fixtures ────────────────────────────────────────────────────────────
    //
    // Built to break this module, not to pass it. Deliberately different from
    // `sampler`'s fixtures in every axis they share, so the two suites are
    // independent evidence rather than one suite twice:
    //
    // * **Sweeps arrive out of cut order**, with a SAILS repeat, so a ladder
    //   that trusted collection order would come out scrambled.
    // * **Azimuths start off north and wrap through 0°**, so an index that
    //   assumed `radial[i].azimuth == i·spacing` is wrong from radial one.
    // * **Fields vary along range and azimuth**, so a rasterizer that read one
    //   gate and smeared it, or that dropped the range axis, still paints
    //   something — and it is the wrong something.
    // * **Upper cuts are range-truncated**, which is what real volumes do and
    //   what makes `BeyondRange` an ordinary status rather than a fault.
    // * **The first gate's centre is nonzero** (2.125 km, the operational
    //   super-resolution value), so forgetting it is ~8 gates of silent error.

    const REFL_SCALE: f32 = 2.0;
    const REFL_OFFSET: f32 = 66.0;
    const FIRST_GATE_M: u16 = 2125;
    const GATE_M: u16 = 250;

    /// KTLX, whose elevation `eet::radar_height_ft_near` reports as 1213 ft.
    const SITE: (f64, f64) = (35.3333, -97.2778);
    /// The conversion is written out rather than taken from [`FT_TO_KM`], so
    /// that an edit to the module's factor fails here instead of moving the
    /// expected value along with the measured one.
    const SITE_ELEV_KM: f64 = 1213.0 * 0.000_304_8;

    /// Raw code for a range-folded gate. Not a magic 1: the sampler's decoder
    /// reads it as [`SampleStatus::RangeFolded`] and this is the only way to
    /// plant one.
    const RAW_RANGE_FOLDED: u8 = 1;
    /// Raw code for a below-threshold gate.
    const RAW_BELOW_THRESHOLD: u8 = 0;

    fn encode_refl(dbz: f64) -> u8 {
        ((dbz * f64::from(REFL_SCALE) + f64::from(REFL_OFFSET)).round() as i64).clamp(2, 255) as u8
    }

    /// What `encode_refl` round-trips to, so a 0.5 dB quantisation step is not
    /// mistaken for a rasterizer error.
    fn round_trip_refl(dbz: f64) -> f32 {
        (f32::from(encode_refl(dbz)) - REFL_OFFSET) / REFL_SCALE
    }

    fn gate_slant_km(gate: usize) -> f64 {
        f64::from(FIRST_GATE_M) / 1000.0 + gate as f64 * f64::from(GATE_M) / 1000.0
    }

    /// What a gate holds: a dBZ, or one of the two raw status codes.
    #[derive(Clone, Copy)]
    enum Gate {
        Dbz(f64),
        BelowThreshold,
        RangeFolded,
    }

    /// A sweep whose gates are `field(azimuth_deg, slant_km)`, with azimuths
    /// laid out **starting at `first_azimuth_deg` and wrapping**.
    fn sweep(
        elevation_number: u8,
        elevation_deg: f32,
        n_radials: usize,
        n_gates: usize,
        first_azimuth_deg: f64,
        field: &dyn Fn(f64, f64) -> Gate,
        first_gate_m: u16,
    ) -> Sweep {
        let spacing = 360.0 / n_radials as f64;
        let radials = (0..n_radials)
            .map(|i| {
                let az = (first_azimuth_deg + i as f64 * spacing).rem_euclid(360.0);
                let bytes: Vec<u8> = (0..n_gates)
                    .map(|gate| {
                        let slant = f64::from(first_gate_m) / 1000.0
                            + gate as f64 * f64::from(GATE_M) / 1000.0;
                        match field(az, slant) {
                            Gate::Dbz(dbz) => encode_refl(dbz),
                            Gate::BelowThreshold => RAW_BELOW_THRESHOLD,
                            Gate::RangeFolded => RAW_RANGE_FOLDED,
                        }
                    })
                    .collect();
                Radial::new(
                    0,
                    i as u16,
                    az as f32,
                    spacing as f32,
                    RadialStatus::IntermediateRadialData,
                    elevation_number,
                    elevation_deg,
                    Some(MomentData::from_fixed_point(
                        bytes.len() as u16,
                        first_gate_m,
                        GATE_M,
                        8,
                        REFL_SCALE,
                        REFL_OFFSET,
                        bytes,
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

    fn cut(angle_deg: f64) -> ElevationCut {
        ElevationCut::new(
            angle_deg,
            ChannelConfiguration::ConstantPhase,
            WaveformType::CS,
            20.0,
            true,
            true,
            false,
            false,
            1,
            20,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            false,
            0,
            false,
            0,
            false,
            false,
        )
    }

    fn vcp(cut_angles: &[f64]) -> VolumeCoveragePattern {
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
            cut_angles.iter().copied().map(cut).collect(),
        )
    }

    /// The tilt ladder every geometry test below runs on: five cuts spanning
    /// 0.5° to 19.5°, **delivered out of order** and with the 0.5° cut repeated
    /// the way a SAILS volume repeats its base tilt.
    ///
    /// Gate counts fall with elevation, so the upper cuts are range-truncated
    /// exactly as real ones are: 0.5° reaches 2.125 + 1199·0.25 = 302 km of
    /// slant, 19.5° only 77 km.
    const LADDER: [(u8, f32, usize); 5] = [
        (1, 0.53, 1200),
        (2, 1.31, 1200),
        (3, 4.02, 800),
        (4, 9.94, 500),
        (5, 19.47, 300),
    ];

    /// `LADDER` under a VCP that declares its nominal cuts, with the sweeps in
    /// a hostile order and one SAILS repeat of the base tilt.
    fn scan_with(field: &dyn Fn(f64, f64) -> Gate) -> Scan {
        scan_with_first_gate(field, FIRST_GATE_M)
    }

    fn scan_with_first_gate(field: &dyn Fn(f64, f64) -> Gate, first_gate_m: u16) -> Scan {
        // Collection order: 4.0°, the base tilt, 19.5°, 1.3°, 10.0°, then the
        // SAILS repeat of the base tilt. Nothing about this is ascending.
        let order = [2usize, 0, 4, 1, 3];
        let mut sweeps: Vec<Sweep> = order
            .iter()
            .map(|&i| {
                let (number, elevation, gates) = LADDER[i];
                // Each rung starts at a different azimuth, so a per-rung index
                // that leaked between rungs is off by a different amount on
                // each of them.
                let first_az = 13.7 * (i as f64 + 1.0);
                sweep(number, elevation, 720, gates, first_az, field, first_gate_m)
            })
            .collect();
        sweeps.push(sweep(
            LADDER[0].0,
            LADDER[0].1 + 0.01,
            720,
            LADDER[0].2,
            201.3,
            field,
            first_gate_m,
        ));
        Scan::new(vcp(&[0.5, 1.3, 4.0, 10.0, 19.5]), sweeps)
    }

    /// A point `range_km` from `SITE` on `bearing_deg`, found by inverting
    /// [`beam::site_bearing_range_km`] numerically rather than by a second
    /// forward formula — so a fixture and the code under test cannot share a
    /// mistake.
    ///
    /// Bisects on latitude for a due-north/south leg and then solves the
    /// longitude, which is exact enough (1e-9 km) for every assertion here.
    fn point_at(bearing_deg: f64, range_km: f64) -> (f64, f64) {
        // Start from the spherical direct solution and refine: the direct
        // formula is on the same sphere, so one Newton step on the residual
        // range is already at machine precision.
        let ang = range_km / types::EARTH_RADIUS_KM;
        let (lat1, lon1) = (SITE.0.to_radians(), SITE.1.to_radians());
        let brg = bearing_deg.to_radians();
        let lat = (lat1.sin() * ang.cos() + lat1.cos() * ang.sin() * brg.cos()).asin();
        let lon =
            lon1 + (brg.sin() * ang.sin() * lat1.cos()).atan2(ang.cos() - lat1.sin() * lat.sin());
        (lat.to_degrees(), lon.to_degrees())
    }

    fn request(start: (f64, f64), end: (f64, f64)) -> SectionRequest {
        SectionRequest {
            start,
            end,
            top_km_msl: None,
            product: RadarProduct::Reflectivity,
        }
    }

    /// The section along a radial out of the site, which is the geometry every
    /// column test wants: column `c` sits at ground range
    /// `column_distance_km(c)` exactly.
    fn radial_section(scan: &Scan, bearing_deg: f64, length_km: f64) -> CrossSection {
        let req = request(SITE, point_at(bearing_deg, length_km));
        render_section(scan, &req, SITE.0, SITE.1).expect("a radial section renders")
    }

    fn status_at(section: &CrossSection, col: usize, row: usize) -> SampleStatus {
        SampleStatus::from_wire_code(section.status()[row * SECTION_WIDTH + col])
            .expect("every status byte decodes")
    }

    fn value_at(section: &CrossSection, col: usize, row: usize) -> f32 {
        section.values()[row * SECTION_WIDTH + col]
    }

    fn pixel_at(section: &CrossSection, col: usize, row: usize) -> (u8, u8, u8, u8) {
        let i = (row * SECTION_WIDTH + col) * 4;
        let px = &section.image()[i..i + 4];
        (px[0], px[1], px[2], px[3])
    }

    /// The column whose centre is nearest `distance_km` along the line.
    ///
    /// Nearest rather than a hard-coded index, because the two supported
    /// rasters are 1024 and 2048 columns wide and an index that named a place
    /// on one names a different place on the other.
    fn nearest_column(axes: &SectionAxes, distance_km: f64) -> usize {
        (0..SECTION_WIDTH)
            .min_by(|&a, &b| {
                (axes.column_distance_km(a) - distance_km)
                    .abs()
                    .total_cmp(&(axes.column_distance_km(b) - distance_km).abs())
            })
            .expect("the raster has columns")
    }

    /// The row whose centre is nearest `height_km_msl`.
    fn nearest_row(axes: &SectionAxes, height_km_msl: f64) -> usize {
        (0..SECTION_HEIGHT)
            .min_by(|&a, &b| {
                (axes.row_height_km_msl(a) - height_km_msl)
                    .abs()
                    .total_cmp(&(axes.row_height_km_msl(b) - height_km_msl).abs())
            })
            .expect("the raster has rows")
    }

    /// The two comparisons whose boundary no rendered fixture can reach, at
    /// the boundary.
    ///
    /// Mutation testing found both: `<` and `<=` are indistinguishable to every
    /// other test here, because the equality case needs a great-circle solution
    /// or a beam height to land on an exact `f64`. They are named functions and
    /// tested directly for that reason — one of them is a taste question and
    /// the other is not, and only this test says which.
    #[test]
    fn the_two_boundary_predicates_round_the_way_the_docs_say() {
        // Taste: a column at exactly the guard's range is sampled.
        assert!(!is_blind(BLIND_GROUND_RANGE_KM));
        assert!(is_blind(BLIND_GROUND_RANGE_KM * (1.0 - f64::EPSILON)));
        assert!(!is_blind(BLIND_GROUND_RANGE_KM * (1.0 + f64::EPSILON)));

        // Not taste: the cone test has to agree with what the sampler answers
        // at that exact height, so the rule is read off the sampler rather than
        // off this module's doc.
        let scan = scan_with(&|_az, _slant| Gate::Dbz(30.0));
        let sampler = VolumeSampler::new(&scan, RadarProduct::Reflectivity).unwrap();
        let column = sampler.column(45.0, 40.0);
        let (_, ceiling) = column.height_span_km().expect("the ladder has rungs");

        assert_ne!(
            column.at_height_km(ceiling).status(),
            SampleStatus::AboveVolume,
            "the sampler now reports a height exactly on the top rung as above \
             the volume, so `ceiling_is_under` should become non-strict",
        );
        assert!(
            !ceiling_is_under(ceiling, ceiling),
            "a ceiling exactly on the top row counts as inside the cone while \
             its top pixel carries a value",
        );

        let just_over = ceiling * (1.0 + 8.0 * f64::EPSILON);
        assert_eq!(
            column.at_height_km(just_over).status(),
            SampleStatus::AboveVolume,
            "precondition: a height a few ulps over the top rung is not above \
             the volume, so this pair does not straddle the boundary",
        );
        assert!(ceiling_is_under(ceiling, just_over));
    }

    // ── The raster's shape and its two axis mappings ────────────────────────

    /// The raster is `IMAGE_SIZE` by half of it, and that is what buys the
    /// WebGL2 argument for free.
    ///
    /// Pinned because the two constants are the load-bearing half of "the UI
    /// does not clamp against `max_texture_side`": if either stopped being a
    /// power of two at or under 2048, a section would start failing to upload
    /// on a device that reports the GLES 3.0 floor, and nothing in this crate
    /// would notice.
    #[test]
    fn the_raster_is_a_half_height_image_size_and_fits_the_webgl2_floor() {
        assert_eq!(SECTION_WIDTH, types::IMAGE_SIZE);
        assert_eq!(SECTION_HEIGHT * 2, SECTION_WIDTH);
        for (name, n) in [("width", SECTION_WIDTH), ("height", SECTION_HEIGHT)] {
            assert!(
                n.is_power_of_two(),
                "the section {name} {n} is not a power of two"
            );
            assert!(n <= 2048, "the section {name} {n} exceeds the WebGL2 floor");
            assert!(n > 0);
        }
        // And the target split is the one `IMAGE_SIZE` already made, not a
        // second one: 2048 x 1024 native, 1024 x 512 on wasm.
        #[cfg(target_arch = "wasm32")]
        assert_eq!((SECTION_WIDTH, SECTION_HEIGHT), (1024, 512));
        #[cfg(not(target_arch = "wasm32"))]
        assert_eq!((SECTION_WIDTH, SECTION_HEIGHT), (2048, 1024));
    }

    // ── The raster's two axis mappings ──────────────────────────────────────

    /// Row 0 is the top, the last row is the bottom, both are half a cell
    /// inside the axis, and the spacing is uniform.
    ///
    /// Every geometric assertion below is read through these two functions, so
    /// this is the one place they are checked against arithmetic written out by
    /// hand.
    #[test]
    fn the_axes_map_rows_downward_and_columns_from_the_start_point() {
        let axes = SectionAxes {
            length_km: 200.0,
            base_km_msl: 0.4,
            top_km_msl: 20.4,
            near_ground_range_km: 0.0,
            far_ground_range_km: 200.0,
            coverage_ground_range_km: 200.0,
            cone_of_silence_km: 0.0,
            tilt_count: 5,
            widest_tilt_gap_deg: 9.53,
            top_tilt_deg: 19.5,
            top_declared_cut_deg: 19.5,
        };

        let cell = 20.0 / SECTION_HEIGHT as f64;
        assert_eq!(axes.row_height_km_msl(0), 20.4 - 0.5 * cell);
        assert_eq!(
            axes.row_height_km_msl(SECTION_HEIGHT - 1),
            20.4 - (SECTION_HEIGHT as f64 - 0.5) * cell,
        );
        // Row 0 is the top: the first row is *higher* than the last, and the
        // last sits half a cell above the base rather than on it.
        assert!(axes.row_height_km_msl(0) > axes.row_height_km_msl(SECTION_HEIGHT - 1));
        assert!(
            (axes.row_height_km_msl(SECTION_HEIGHT - 1) - (0.4 + 0.5 * cell)).abs() < 1e-12,
            "the bottom row centre is {} km MSL, not half a cell over the base",
            axes.row_height_km_msl(SECTION_HEIGHT - 1),
        );
        // Uniform: every step is one cell, to the last one.
        for row in 1..SECTION_HEIGHT {
            let step = axes.row_height_km_msl(row - 1) - axes.row_height_km_msl(row);
            assert!(
                (step - cell).abs() < 1e-12,
                "row {row} steps {step} km, not {cell}",
            );
        }

        let width = 200.0 / SECTION_WIDTH as f64;
        assert_eq!(axes.column_distance_km(0), 0.5 * width);
        assert_eq!(
            axes.column_distance_km(SECTION_WIDTH - 1),
            (SECTION_WIDTH as f64 - 0.5) * width,
        );
        // The mapping is a fraction of the line, so the last column is inside
        // its far end by half a cell rather than on it.
        assert!(
            axes.column_distance_km(SECTION_WIDTH - 1) < 200.0,
            "the last column sits at or past the end of the line",
        );
    }

    /// Every pixel is the volume sampled at that pixel's own place — checked
    /// against geometry written out here rather than called.
    ///
    /// This is the rasterizer's whole contract in one test, and it is
    /// deliberately not routed through [`SectionAxes`]'s two accessors: the
    /// fraction, the great-circle point, the radar-relative coordinates, the
    /// MSL→above-antenna conversion and the row centre are all spelled out
    /// below, so an edit that moved the row order, dropped a half-pixel
    /// centring, mixed up the two height datums or measured the track on a
    /// different sphere fails here even if it edited both the renderer and the
    /// accessors in step.
    ///
    /// A grid rather than the whole raster, because the whole raster is the
    /// section rendered twice and takes a second; the grid is chosen to include
    /// both corners, both edges and a prime stride through the interior.
    #[test]
    fn every_pixel_is_the_volume_sampled_at_that_pixels_own_place() {
        let scan = scan_with(&|az, slant| {
            if slant > 120.0 && slant < 140.0 {
                Gate::RangeFolded
            } else if az > 300.0 {
                Gate::BelowThreshold
            } else {
                Gate::Dbz(-8.0 + slant / 5.0 + az / 90.0)
            }
        });
        // A line that does not start at the site and does not run along a
        // radial, so the azimuth changes down the raster and a track measured
        // as a straight lat/lon lerp would drift off it.
        let req = request(point_at(310.0, 40.0), point_at(65.0, 190.0));
        let section = render_section(&scan, &req, SITE.0, SITE.1).unwrap();
        let axes = *section.axes();
        let sampler = VolumeSampler::new(&scan, RadarProduct::Reflectivity).unwrap();

        let mut checked = 0usize;
        let mut statuses = std::collections::BTreeSet::new();
        let mut cols: Vec<usize> = (0..SECTION_WIDTH).step_by(37).collect();
        cols.push(SECTION_WIDTH - 1);
        let mut rows: Vec<usize> = (0..SECTION_HEIGHT).step_by(23).collect();
        rows.push(SECTION_HEIGHT - 1);

        for &col in &cols {
            // Spelled out, not called: `column_distance_km` divided by
            // `length_km` is the same fraction, and this is the place that must
            // not share it.
            let t = (col as f64 + 0.5) / SECTION_WIDTH as f64;
            let point = beam::great_circle_point(req.start, req.end, t);
            let (azimuth, ground) = beam::site_bearing_range_km(SITE.0, SITE.1, point.0, point.1);
            for &row in &rows {
                // Row 0 at the top, centres half a row inside the axis, and
                // the query height is above the *antenna* while the axis is MSL.
                let height_msl = axes.top_km_msl
                    - (row as f64 + 0.5) * (axes.top_km_msl - axes.base_km_msl)
                        / SECTION_HEIGHT as f64;
                let expected = sampler.sample(azimuth, ground, height_msl - axes.base_km_msl);

                let got = section.sample(col, row).expect("inside the raster");
                assert_eq!(
                    got.status(),
                    expected.status(),
                    "pixel ({col}, {row}) at {ground:.4} km / {azimuth:.4}° / \
                     {height_msl:.4} km MSL: raster says {:?}, the sampler says \
                     {:?}",
                    got.status(),
                    expected.status(),
                );
                assert_eq!(got.value(), expected.value(), "pixel ({col}, {row})");
                assert_eq!(
                    pixel_at(&section, col, row),
                    section_color(RadarProduct::Reflectivity, expected),
                );
                statuses.insert(expected.status().wire_code());
                checked += 1;
            }
        }
        assert_eq!(checked, cols.len() * rows.len());
        // precondition: the grid really covered more than one kind of answer.
        // Four of the seven statuses is what this geometry produces; a fixture
        // that produced one would make the comparison above vacuous.
        assert!(
            statuses.len() >= 4,
            "the grid hit only {} statuses ({statuses:?})",
            statuses.len(),
        );
    }

    // ── Geometry, against analytic fixtures ─────────────────────────────────

    /// A slab planted between two heights paints between those two rows, and
    /// nowhere else.
    ///
    /// The slab is planted in **beam coordinates** — a gate is in it when
    /// `beam::height_km(slant, elevation)` lands in the band — so what is being
    /// tested is that the rasterizer maps rows to those same heights, not that
    /// two copies of the same formula agree.
    ///
    /// The band is 4–6 km above the antenna, which on this ladder is crossed by
    /// three rungs at the ranges tested, so the vertical lerp has real brackets
    /// above and below rather than falling off the end of the ladder.
    #[test]
    fn a_planted_slab_paints_between_the_rows_of_its_planted_heights() {
        const SLAB: (f64, f64) = (4.0, 6.0);
        // A slab is a fact about *height*, and a gate's height depends on the
        // rung that flew it — so the field is planted per rung, in that rung's
        // own beam coordinates, and every rung's gates end up in the slab at a
        // different slant range. A field planted in slant range alone would be
        // a cone, not a slab, and would pass a rasterizer that ignored
        // elevation entirely.
        let slab_scan = {
            let order = [2usize, 0, 4, 1, 3];
            let mut sweeps: Vec<Sweep> = order
                .iter()
                .map(|&i| {
                    let (number, elevation, gates) = LADDER[i];
                    let elev = f64::from(elevation);
                    let field = move |_az: f64, slant: f64| {
                        let h = beam::height_km(slant, elev);
                        if h >= SLAB.0 && h <= SLAB.1 {
                            Gate::Dbz(45.0)
                        } else {
                            Gate::BelowThreshold
                        }
                    };
                    sweep(
                        number,
                        elevation,
                        720,
                        gates,
                        13.7 * (i as f64 + 1.0),
                        &field,
                        FIRST_GATE_M,
                    )
                })
                .collect();
            let (number, elevation, gates) = LADDER[0];
            let elev = f64::from(elevation) + 0.01;
            let field = move |_az: f64, slant: f64| {
                let h = beam::height_km(slant, elev);
                if h >= SLAB.0 && h <= SLAB.1 {
                    Gate::Dbz(45.0)
                } else {
                    Gate::BelowThreshold
                }
            };
            sweeps.push(sweep(
                number,
                elevation + 0.01,
                720,
                gates,
                201.3,
                &field,
                FIRST_GATE_M,
            ));
            Scan::new(vcp(&[0.5, 1.3, 4.0, 10.0, 19.5]), sweeps)
        };

        let section = radial_section(&slab_scan, 47.0, 200.0);
        let axes = *section.axes();

        // Probe at 65 km, where exactly one rung's beam passes through the
        // slab. That is not a convenience — it is what a 2 km slab *is* to a
        // five-rung ladder, and it fixes the answer analytically.
        let col = (0..SECTION_WIDTH)
            .min_by(|&a, &b| {
                (axes.column_distance_km(a) - 65.0)
                    .abs()
                    .total_cmp(&(axes.column_distance_km(b) - 65.0).abs())
            })
            .unwrap();
        let ground = axes.column_distance_km(col);

        // The ladder over that column, in the elevations the sampler actually
        // chose — the base rung is the SAILS repeat, one hundredth of a degree
        // up from the first pass.
        let rung_heights: Vec<f64> = [
            f64::from(LADDER[0].1 + 0.01),
            f64::from(LADDER[1].1),
            f64::from(LADDER[2].1),
            f64::from(LADDER[3].1),
            f64::from(LADDER[4].1),
        ]
        .iter()
        .map(|&e| beam::height_at_ground_km(ground, e))
        .collect();
        let inside: Vec<usize> = (0..rung_heights.len())
            .filter(|&i| (SLAB.0..=SLAB.1).contains(&rung_heights[i]))
            .collect();
        assert_eq!(
            inside.len(),
            1,
            "precondition: {} rungs pass through the slab at {ground:.2} km \
             ({rung_heights:?}); this test's prediction assumes exactly one",
            inside.len(),
        );
        let carrier = inside[0];
        assert!(
            carrier > 0 && carrier + 1 < rung_heights.len(),
            "precondition: the slab's rung is on the end of the ladder, so it \
             has no bracket on one side and the prediction below is wrong",
        );

        // What the section must paint. The sampler's vertical blend hands a
        // height to whichever bracketing rung carries the most weight when one
        // of them has no value, so the boundary of a one-rung layer sits at the
        // **midpoint** between that rung and its neighbours — not at the
        // planted 4–6 km. That is the sampler's documented edge treatment, and
        // predicting it is what makes this a test of the raster's row mapping
        // rather than of the blend.
        let predicted_bottom = (rung_heights[carrier - 1] + rung_heights[carrier]) / 2.0;
        let predicted_top = (rung_heights[carrier] + rung_heights[carrier + 1]) / 2.0;

        let painted: Vec<usize> = (0..SECTION_HEIGHT)
            .filter(|&row| status_at(&section, col, row) == SampleStatus::Value)
            .collect();
        assert!(
            !painted.is_empty(),
            "the slab painted nothing at {ground} km"
        );
        let top = axes.row_height_km_msl(painted[0]) - axes.base_km_msl;
        let bottom = axes.row_height_km_msl(*painted.last().unwrap()) - axes.base_km_msl;

        // Two rows of slack: the boundary is exact but falls between row
        // centres, and one row is 19.5 m native / 39 m on wasm.
        let row_km = (axes.top_km_msl - axes.base_km_msl) / SECTION_HEIGHT as f64;
        assert!(
            (top - predicted_top).abs() < 2.0 * row_km,
            "the layer's top painted at {top:.4} km ARL; the {:.3}°/{:.3}° \
             half-weight boundary is at {predicted_top:.4} km",
            LADDER[carrier].1,
            LADDER[carrier + 1].1,
        );
        assert!(
            (bottom - predicted_bottom).abs() < 2.0 * row_km,
            "the layer's bottom painted at {bottom:.4} km ARL; the \
             {:.3}°/{:.3}° half-weight boundary is at {predicted_bottom:.4} km",
            LADDER[carrier - 1].1,
            LADDER[carrier].1,
        );
        // And the planted slab really is inside what was painted, which is the
        // sanity the prediction could otherwise talk itself out of.
        assert!(
            bottom < SLAB.0 && top > SLAB.1,
            "the painted band {bottom:.3}..{top:.3} km does not contain the \
             planted {:?} km slab",
            SLAB,
        );
        // precondition: the band is a band, not the axis. A rasterizer that
        // ignored the height axis would paint every row.
        assert!(
            painted.len() < SECTION_HEIGHT / 3,
            "the layer painted {} of {SECTION_HEIGHT} rows",
            painted.len(),
        );
        // Contiguous: a layer is one band, not a comb.
        assert_eq!(
            painted.len(),
            painted[painted.len() - 1] - painted[0] + 1,
            "the painted layer has holes in it",
        );
        // And the rows either side of it are not values.
        assert_ne!(
            status_at(&section, col, painted[0] - 1),
            SampleStatus::Value,
            "the row above the layer painted a value",
        );
        assert_ne!(
            status_at(&section, col, painted[painted.len() - 1] + 1),
            SampleStatus::Value,
            "the row below the layer painted a value",
        );
    }

    /// A wall planted at one ground range paints in the columns at that ground
    /// range — on every rung, which is what proves the `cos e` correction runs.
    ///
    /// The wall is planted in **ground** range: a gate is in it when
    /// `beam::ground_range_km(slant, elevation)` falls in the band. A
    /// rasterizer that forgot `cos e` would paint the 19.5° rung's share of the
    /// wall 5.7 % further out and every other rung's in the right place, so the
    /// wall would lean. On this fixture's geometry — a 60 km wall drawn on a
    /// 150 km line — that is **3.65 km, about 50 native columns**, which
    /// `dropping_the_cos_e_correction_would_move_the_wall_further_than_the_tolerance`
    /// measures rather than assumes.
    #[test]
    fn a_planted_wall_paints_at_its_ground_range_on_every_rung() {
        const WALL: (f64, f64) = (60.0, 62.0);
        let wall_scan = {
            let order = [2usize, 0, 4, 1, 3];
            let sweeps: Vec<Sweep> = order
                .iter()
                .map(|&i| {
                    let (number, elevation, gates) = LADDER[i];
                    let elev = f64::from(elevation);
                    let field = move |_az: f64, slant: f64| {
                        let g = beam::ground_range_km(slant, elev);
                        if g >= WALL.0 && g <= WALL.1 {
                            Gate::Dbz(50.0)
                        } else {
                            Gate::BelowThreshold
                        }
                    };
                    sweep(
                        number,
                        elevation,
                        720,
                        gates,
                        13.7 * (i as f64 + 1.0),
                        &field,
                        FIRST_GATE_M,
                    )
                })
                .collect();
            Scan::new(vcp(&[0.5, 1.3, 4.0, 10.0, 19.5]), sweeps)
        };

        let section = radial_section(&wall_scan, 312.0, 150.0);
        let axes = *section.axes();

        // Every painted pixel, anywhere in the raster, is inside the wall's
        // ground-range band. A leaning wall fails this at its top.
        let mut painted_columns: Vec<usize> = Vec::new();
        let mut worst = (0.0f64, 0usize, 0usize);
        for row in 0..SECTION_HEIGHT {
            for col in 0..SECTION_WIDTH {
                if status_at(&section, col, row) != SampleStatus::Value {
                    continue;
                }
                let g = axes.column_distance_km(col);
                let miss = if g < WALL.0 {
                    WALL.0 - g
                } else if g > WALL.1 {
                    g - WALL.1
                } else {
                    0.0
                };
                if miss > worst.0 {
                    worst = (miss, col, row);
                }
                painted_columns.push(col);
            }
        }
        assert!(!painted_columns.is_empty(), "the wall painted nothing");
        // One column is 150/SECTION_WIDTH km (73 m native); the bilinear in
        // slant range spreads the 2 km wall by up to one gate either side, so
        // 0.6 km is generous for the smear and 6.1x under the 3.6509 km a
        // missing `cos e` would put the 19.5° rung out at a 60 km wall — the
        // figure the companion test below measures.
        assert!(
            worst.0 < 0.6,
            "a value painted {:.3} km outside the wall at column {} (ground \
             {:.3} km), row {}",
            worst.0,
            worst.1,
            axes.column_distance_km(worst.1),
            worst.2,
        );

        // precondition: the wall really was sampled on the steep rungs too, or
        // the `cos e` claim rests on nothing. The 19.5° beam is at 20.5 km ARL
        // over a 60 km ground range, so the wall must paint that high.
        let highest = (0..SECTION_HEIGHT)
            .find(|&row| {
                (0..SECTION_WIDTH).any(|col| status_at(&section, col, row) == SampleStatus::Value)
            })
            .expect("the wall painted at least one row");
        let top_km_arl = axes.row_height_km_msl(highest) - SITE_ELEV_KM;
        assert!(
            top_km_arl > 9.0,
            "precondition: the wall only reached {top_km_arl:.2} km ARL, so \
             the steep rungs never contributed and the `cos e` claim is untested",
        );
    }

    /// The wall test's negative: a rasterizer that dropped `cos e` really would
    /// fail it.
    ///
    /// Measured rather than asserted by construction — the divergence has to be
    /// bigger than the tolerance the test above uses, or that test proves
    /// nothing about `cos e` at all.
    #[test]
    fn dropping_the_cos_e_correction_would_move_the_wall_further_than_the_tolerance() {
        for &(_, elevation, _) in &LADDER {
            let elev = f64::from(elevation);
            // A gate on the 60 km wall, on this rung.
            let slant = beam::slant_range_for_ground_km(60.0, elev);
            let without = slant; // what a `cos e`-less rasterizer would call it
            let with = beam::ground_range_km(slant, elev);
            let divergence = without - with;
            if elev > 5.0 {
                assert!(
                    divergence > 0.6,
                    "at {elev}° the `cos e` divergence is only {divergence:.3} km, \
                     under the 0.6 km tolerance the wall test allows — that test \
                     would pass without the correction",
                );
            }
        }
        // The figure the beam module quotes as a percentage, restated as the
        // kilometres this test depends on: 5.7 % of 60 km at 19.5°.
        let at_195 = beam::slant_range_for_ground_km(60.0, 19.5) - 60.0;
        assert!(
            (at_195 - 3.6509).abs() < 0.001,
            "the 19.5° divergence at a 60 km wall moved: {at_195:.4} km, \
             documented as 3.6509 km",
        );
    }

    // ── The two spheres ─────────────────────────────────────────────────────

    /// The ground track is on 6371 and the plan view's range ring is not, and
    /// the gap is measured rather than discovered later.
    ///
    /// A point the 230 km ring puts on screen reads 259 m nearer the site here.
    /// The pixel figure is target-dependent — [`crate::types::IMAGE_SIZE`] is
    /// 2048 native and 1024 on wasm — so it is computed from the constant
    /// rather than quoted.
    #[test]
    fn the_ground_track_sphere_is_the_one_render_gate_uses() {
        // `ImageBounds`' degrees-per-km, which implies a 6378 km sphere.
        const IMAGE_BOUNDS_KM_PER_DEG: f64 = 111.32;
        let ring_deg = types::MAX_RANGE_KM / IMAGE_BOUNDS_KM_PER_DEG;

        // The same latitude offset, measured the way this module measures it.
        let ring_point = (SITE.0 + ring_deg, SITE.1);
        let (_, ours_km) = beam::site_bearing_range_km(SITE.0, SITE.1, ring_point.0, ring_point.1);

        let gap_m = (types::MAX_RANGE_KM - ours_km) * 1000.0;
        assert!(
            (gap_m - 258.42).abs() < 0.02,
            "the 6371/6378 seam moved: a point the {} km ring draws reads \
             {ours_km:.6} km here, a {gap_m:.2} m gap, documented as 258.42 m",
            types::MAX_RANGE_KM,
        );
        // Which way: the section samples *nearer* the site than the ring's
        // label claims, never further.
        assert!(
            ours_km < types::MAX_RANGE_KM,
            "the section now reads the ring as further out than 230 km, which \
             inverts the module doc's statement of the seam",
        );

        // In pixels of the plan view the ring is drawn on, which is
        // target-dependent: `IMAGE_SIZE` is 2048 native and 1024 on wasm, so
        // the same 258 m is twice as many pixels on a desktop.
        let px = gap_m / 1000.0 * types::PIXELS_PER_KM;
        #[cfg(target_arch = "wasm32")]
        let (target, expected) = ("wasm (1024 px)", 0.5753);
        #[cfg(not(target_arch = "wasm32"))]
        let (target, expected) = ("native (2048 px)", 1.1505);
        assert!(
            (px - expected).abs() < 0.001,
            "the seam is {px:.4} px on {target}, documented as {expected}",
        );

        // precondition: the two spheres really do differ, so this is a seam and
        // not a rounding artefact.
        assert!(
            (types::EARTH_RADIUS_KM * std::f64::consts::PI / 180.0 - IMAGE_BOUNDS_KM_PER_DEG).abs()
                > 0.1,
            "precondition: `ImageBounds`' 111.32 km/° and the 6371 sphere have \
             converged, so there is no seam left to record",
        );
    }

    // ── Coverage, clipping, and what runs out where ─────────────────────────

    /// The section draws the whole line and says where the data stopped, rather
    /// than stopping at `MAX_RANGE_KM`.
    ///
    /// The fixture's base tilt reaches 302 km of slant range, well past the
    /// 230 km a plan view draws, and the line is 420 km long — so a rasterizer
    /// that clipped at `MAX_RANGE_KM` would leave the last 190 km empty and
    /// report 230.
    #[test]
    fn the_section_runs_past_the_plan_views_clip_and_reports_where_data_ends() {
        let scan = scan_with(&|_az, slant| Gate::Dbz(10.0 + slant / 20.0));
        let section = radial_section(&scan, 91.0, 420.0);
        let axes = *section.axes();

        // The last gate centre of the base tilt, in ground range on its own
        // elevation — the farthest this volume can answer. The base rung is the
        // **SAILS repeat**, one hundredth of a degree up from the first pass,
        // because the ladder takes the newest sweep of a repeated cut.
        let last_slant = gate_slant_km(LADDER[0].2 - 1);
        let reach = beam::ground_range_km(last_slant, f64::from(LADDER[0].1 + 0.01));
        assert!(
            reach > types::MAX_RANGE_KM,
            "precondition: the fixture only reaches {reach:.1} km, so it never \
             tests the clip it is here to test",
        );

        // Coverage is measured on the column grid and a column half a gate past
        // the last gate centre still resolves to it, so the window is one
        // column wide either way — which is 0.21 km native and 0.41 km on wasm.
        let tolerance = axes.length_km / SECTION_WIDTH as f64 + 0.2;
        assert!(
            (axes.coverage_ground_range_km - reach).abs() < tolerance,
            "coverage reported {:.3} km against a {reach:.3} km reach",
            axes.coverage_ground_range_km,
        );
        assert!(
            axes.coverage_ground_range_km > types::MAX_RANGE_KM,
            "the section clipped at MAX_RANGE_KM: coverage {:.1} km",
            axes.coverage_ground_range_km,
        );
        // And it ran out before the line did, which is the comparison the field
        // exists to support.
        assert!(
            axes.coverage_ground_range_km < axes.far_ground_range_km,
            "precondition: the {:.1} km line did not outrun the {:.1} km of \
             data, so this section never ran out",
            axes.far_ground_range_km,
            axes.coverage_ground_range_km,
        );

        // Past the coverage the pixels say `BeyondRange`, not `NoCoverage` and
        // not a value: the ladder's rungs are all there, they simply stop.
        let past = (0..SECTION_WIDTH)
            .find(|&col| axes.column_distance_km(col) > axes.coverage_ground_range_km + 2.0)
            .expect("some column lies past the data");
        // A row inside the ladder at that range, which at 320 km is high: earth
        // curvature alone puts the base tilt's beam at 9 km ARL there, so
        // anything lower would read `BelowLowestBeam` and prove nothing about
        // the range clip. Take the lowest drawn row that clears it.
        let floor_msl = axes.base_km_msl
            + beam::height_at_ground_km(
                axes.column_distance_km(past),
                f64::from(LADDER[0].1 + 0.01),
            );
        let row = (0..SECTION_HEIGHT)
            .rev()
            .find(|&row| axes.row_height_km_msl(row) > floor_msl + 0.5)
            .expect("some drawn row clears the lowest beam at this range");
        assert_eq!(
            status_at(&section, past, row),
            SampleStatus::BeyondRange,
            "at {:.1} km, {:.2} km MSL the section says {:?}",
            axes.column_distance_km(past),
            axes.row_height_km_msl(row),
            status_at(&section, past, row),
        );
        assert!(value_at(&section, past, row).is_nan());
        assert_eq!(pixel_at(&section, past, row), (0, 0, 0, 0));
    }

    /// A below-threshold return counts as coverage: the radar looked and saw
    /// nothing, which is different from not having looked.
    ///
    /// Without this the coverage number would collapse to "the farthest echo",
    /// and a clear-air section would report that its data ran out at the site.
    #[test]
    fn clear_air_still_counts_as_coverage() {
        let scan = scan_with(&|_az, _slant| Gate::BelowThreshold);
        let section = radial_section(&scan, 175.0, 200.0);
        let axes = *section.axes();

        assert!(
            axes.coverage_ground_range_km > 199.0,
            "a clear-air volume reported coverage out to only {:.1} km",
            axes.coverage_ground_range_km,
        );
        // Nothing is painted, and every low pixel says why.
        let row = (0..SECTION_HEIGHT)
            .find(|&row| axes.row_height_km_msl(row) < 3.0)
            .unwrap();
        assert_eq!(
            status_at(&section, SECTION_WIDTH / 2, row),
            SampleStatus::BelowThreshold,
        );
        assert_eq!(pixel_at(&section, SECTION_WIDTH / 2, row), (0, 0, 0, 0));
    }

    // ── The cone of silence ─────────────────────────────────────────────────

    /// The cone is reported as an extent, and the extent is exactly the columns
    /// whose top row falls above the volume.
    ///
    /// The equivalence is the point: the number and the pixels are two readings
    /// of one fact, so a consumer that draws a hatch over `cone_of_silence_km`
    /// covers precisely the empty columns and no others.
    #[test]
    fn the_cone_of_silence_is_reported_as_the_extent_of_the_empty_columns() {
        let scan = scan_with(&|_az, _slant| Gate::Dbz(35.0));
        let section = radial_section(&scan, 5.0, 120.0);
        let axes = *section.axes();

        let empty_top: Vec<usize> = (0..SECTION_WIDTH)
            .filter(|&col| status_at(&section, col, 0) == SampleStatus::AboveVolume)
            .collect();
        assert!(
            !empty_top.is_empty(),
            "a section starting at the site has no column above the volume",
        );
        // The blind column at the site reads `NoCoverage`, not `AboveVolume`,
        // and is inside the cone by definition — so the count is the empty ones
        // plus the blind ones.
        let blind = (0..SECTION_WIDTH)
            .filter(|&col| status_at(&section, col, 0) == SampleStatus::NoCoverage)
            .count();
        let width = axes.length_km / SECTION_WIDTH as f64;
        assert!(
            (axes.cone_of_silence_km - (empty_top.len() + blind) as f64 * width).abs() < 1e-9,
            "the cone is reported as {:.4} km but {} columns are empty at the \
             top ({} of them blind), which is {:.4} km",
            axes.cone_of_silence_km,
            empty_top.len() + blind,
            blind,
            (empty_top.len() + blind) as f64 * width,
        );

        // The cone is the *near* end of the line, contiguous, and its far edge
        // is where the top rung's beam reaches the top row.
        assert_eq!(
            *empty_top.last().unwrap() + 1,
            empty_top.len() + blind,
            "the empty columns are not the contiguous near end of the line",
        );
        let top_row_arl = axes.row_height_km_msl(0) - axes.base_km_msl;
        let expected_edge = {
            // Ground range at which the highest rung's beam reaches the top row.
            let top_elev = f64::from(LADDER[4].1);
            beam::ground_range_km(
                beam::slant_range_for_height_km(top_row_arl, top_elev),
                top_elev,
            )
        };
        assert!(
            (axes.cone_of_silence_km - expected_edge).abs() < 2.0 * width,
            "the cone reaches {:.3} km; the {}° beam reaches the top row at \
             {expected_edge:.3} km",
            axes.cone_of_silence_km,
            LADDER[4].1,
        );

        // A section far from the site is not in the cone at all, which is what
        // makes the number a measurement rather than a constant.
        let far = render_section(
            &scan,
            &request(point_at(5.0, 120.0), point_at(35.0, 150.0)),
            SITE.0,
            SITE.1,
        )
        .unwrap();
        assert_eq!(far.axes().cone_of_silence_km, 0.0);
    }

    /// A lower axis top puts less of the line under the cone — which is why the
    /// number is reported instead of a threshold being invented.
    #[test]
    fn the_cone_extent_follows_the_axis_the_caller_asked_for() {
        let scan = scan_with(&|_az, _slant| Gate::Dbz(35.0));
        let end = point_at(5.0, 120.0);
        let tall = render_section(&scan, &request(SITE, end), SITE.0, SITE.1).unwrap();

        let low = render_section(
            &scan,
            &SectionRequest {
                top_km_msl: Some(SITE_ELEV_KM + 5.0),
                ..request(SITE, end)
            },
            SITE.0,
            SITE.1,
        )
        .unwrap();

        assert!(
            low.axes().cone_of_silence_km < tall.axes().cone_of_silence_km,
            "a 5 km axis reports the same {:.2} km cone as a 20 km one",
            tall.axes().cone_of_silence_km,
        );
        // Roughly proportional: the cone's edge is `h / tan(e_top)` plus the
        // beam's own curvature, so a quarter of the height is about a quarter
        // of the reach.
        let ratio = low.axes().cone_of_silence_km / tall.axes().cone_of_silence_km;
        assert!(
            (0.15..0.35).contains(&ratio),
            "the 5 km cone is {ratio:.3} of the 20 km one",
        );
    }

    // ── The site crossing ───────────────────────────────────────────────────

    /// A line drawn across the site produces a blind column, a ground range
    /// that goes to zero and comes back, and a 180° flip in bearing.
    #[test]
    fn a_line_across_the_site_blinds_one_column_and_flips_the_bearing() {
        let scan = scan_with(&|az, slant| Gate::Dbz(20.0 + az / 36.0 + slant / 50.0));
        // Straight through the site: 100 km out on 20°, 100 km out on 200°.
        let req = request(point_at(200.0, 100.0), point_at(20.0, 100.0));
        let section = render_section(&scan, &req, SITE.0, SITE.1).unwrap();
        let axes = *section.axes();

        // Ground ranges along the line, recomputed the way the renderer does.
        let ranges: Vec<f64> = (0..SECTION_WIDTH)
            .map(|col| {
                let t = axes.column_distance_km(col) / axes.length_km;
                let p = beam::great_circle_point(req.start, req.end, t);
                beam::site_bearing_range_km(SITE.0, SITE.1, p.0, p.1).1
            })
            .collect();
        let min_col = ranges
            .iter()
            .enumerate()
            .min_by(|a, b| a.1.total_cmp(b.1))
            .map(|(i, _)| i)
            .unwrap();
        assert!(
            ranges[min_col] < BLIND_GROUND_RANGE_KM,
            "the line's closest approach is {:.4} km, so it never crosses the \
             site and this test proves nothing",
            ranges[min_col],
        );
        assert!(
            (axes.near_ground_range_km - ranges[min_col]).abs() < 1e-12,
            "the reported nearest approach {:.6} km is not the measured \
             {:.6} km",
            axes.near_ground_range_km,
            ranges[min_col],
        );

        // The bearings either side of the crossing are 180° apart.
        let bearing = |col: usize| {
            let t = axes.column_distance_km(col) / axes.length_km;
            let p = beam::great_circle_point(req.start, req.end, t);
            beam::site_bearing_range_km(SITE.0, SITE.1, p.0, p.1).0
        };
        let before = bearing(min_col.saturating_sub(40));
        let after = bearing(min_col + 40);
        let flip = (after - before).rem_euclid(360.0);
        assert!(
            (flip - 180.0).abs() < 1.0,
            "the bearing turned {flip:.3}° across the site, not 180°",
        );

        // The blind columns are exactly the ones inside the guard, they are
        // contiguous, and each is blind at every row.
        //
        // *How many* is target-dependent and deliberately not asserted as one:
        // the guard is a 0.25 km window and a column is `length/WIDTH` wide —
        // 0.098 km native, 0.195 km on wasm — so a native raster sees two or
        // three and a wasm one sees one or two. What is target-independent is
        // that the blind region is the guard's width plus at most a column,
        // which is what the bound below says.
        let blind: Vec<usize> = (0..SECTION_WIDTH)
            .filter(|&col| ranges[col] < BLIND_GROUND_RANGE_KM)
            .collect();
        let column_width = axes.length_km / SECTION_WIDTH as f64;
        assert!(
            !blind.is_empty(),
            "the line crossed the site blind to nothing"
        );
        assert_eq!(
            blind.last().unwrap() - blind[0] + 1,
            blind.len(),
            "the blind columns are not contiguous: {blind:?}",
        );
        assert!(
            blind.len() as f64 * column_width <= 2.0 * BLIND_GROUND_RANGE_KM + column_width,
            "the blind region is {} columns ({:.3} km) wide, more than the \
             guard's {:.3} km window plus one column",
            blind.len(),
            blind.len() as f64 * column_width,
            2.0 * BLIND_GROUND_RANGE_KM,
        );
        for &col in &blind {
            for row in 0..SECTION_HEIGHT {
                assert_eq!(
                    status_at(&section, col, row),
                    SampleStatus::NoCoverage,
                    "blind column {col} has a status at row {row}",
                );
                assert_eq!(pixel_at(&section, col, row), (0, 0, 0, 0));
            }
        }
        // And the guard is a slit, not a wedge. The comparison has to be made
        // past the first gate's 2.125 km centre, which is its own reason for
        // emptiness and would otherwise be mistaken for the guard's: at 5 km
        // out the section paints again.
        let out = (0..SECTION_WIDTH)
            .min_by(|&a, &b| (ranges[a] - 5.0).abs().total_cmp(&(ranges[b] - 5.0).abs()))
            .unwrap();
        let row = nearest_row(&axes, axes.base_km_msl + 1.0);
        assert_ne!(
            status_at(&section, out, row),
            SampleStatus::NoCoverage,
            "the column at {:.2} km from the site is blind too, so the guard \
             is a wedge rather than a slit",
            ranges[out],
        );
    }

    /// The blind guard is observable, and not merely redundant with the first
    /// gate's range.
    ///
    /// With a nonzero `first_gate_range_km` the sampler refuses a sub-gate
    /// query anyway, so a fixture built the usual way cannot tell whether the
    /// guard exists. This one starts its gates at the antenna, so without the
    /// guard the column over the site would sample a bearing that is `atan2` of
    /// two rounding errors and paint whatever it found.
    #[test]
    fn the_blind_guard_holds_where_the_first_gate_starts_at_the_antenna() {
        let scan = scan_with_first_gate(&|az, _slant| Gate::Dbz(20.0 + az / 12.0), 0);
        let req = request(point_at(200.0, 40.0), point_at(20.0, 40.0));
        let section = render_section(&scan, &req, SITE.0, SITE.1).unwrap();
        let axes = *section.axes();

        let col = (0..SECTION_WIDTH)
            .min_by(|&a, &b| {
                let f = |col: usize| {
                    let t = axes.column_distance_km(col) / axes.length_km;
                    let p = beam::great_circle_point(req.start, req.end, t);
                    beam::site_bearing_range_km(SITE.0, SITE.1, p.0, p.1).1
                };
                f(a).total_cmp(&f(b))
            })
            .unwrap();

        // precondition: with gates from zero, the neighbouring columns *do*
        // paint — so the blind column's emptiness is the guard and not the
        // absence of data.
        let row = (0..SECTION_HEIGHT)
            .find(|&row| axes.row_height_km_msl(row) < SITE_ELEV_KM + 0.05)
            .unwrap();
        assert_eq!(
            status_at(&section, col + 30, row),
            SampleStatus::Value,
            "precondition: a column 30 past the site paints nothing even with \
             gates from the antenna, so the guard is untested",
        );
        for row in 0..SECTION_HEIGHT {
            assert_eq!(
                status_at(&section, col, row),
                SampleStatus::NoCoverage,
                "the column over the site painted {:?} at row {row}",
                status_at(&section, col, row),
            );
        }
    }

    // ── Colour ──────────────────────────────────────────────────────────────

    /// The colours come from `get_color_for_value`, floors and all — including
    /// the reflectivity floor that lives *only* there and not in the legend.
    #[test]
    fn the_transparency_floors_come_from_the_shared_colour_function() {
        // −5 dBZ is under reflectivity's 0 dBZ floor, which
        // `get_color_for_value` paints transparent and `LegendScale` does not
        // mention at all.
        let scan = scan_with(&|_az, _slant| Gate::Dbz(-5.0));
        let section = radial_section(&scan, 260.0, 100.0);
        let axes = *section.axes();
        let row = (0..SECTION_HEIGHT)
            .find(|&row| axes.row_height_km_msl(row) < SITE_ELEV_KM + 2.0)
            .unwrap();
        let col = SECTION_WIDTH / 2;

        assert_eq!(status_at(&section, col, row), SampleStatus::Value);
        assert!(
            value_at(&section, col, row) < 0.0,
            "the fixture's −5 dBZ arrived as {}",
            value_at(&section, col, row),
        );
        assert_eq!(
            pixel_at(&section, col, row),
            (0, 0, 0, 0),
            "a sub-floor reflectivity was painted",
        );

        // And a value above the floor is exactly what the shared function says,
        // rather than a second scale that happens to look similar.
        let strong = scan_with(&|_az, _slant| Gate::Dbz(47.5));
        let section = radial_section(&strong, 260.0, 100.0);
        let v = value_at(&section, col, row);
        assert_eq!(v, round_trip_refl(47.5));
        assert_eq!(
            pixel_at(&section, col, row),
            crate::get_color_for_value(RadarProduct::Reflectivity, v),
        );
    }

    /// A range-folded gate paints the fold's own colour, keeps its status, and
    /// carries no number — where the shared colour function would have made it
    /// invisible.
    #[test]
    fn a_range_folded_gate_is_painted_rather_than_left_transparent() {
        let scan = scan_with(&|_az, slant| {
            if slant > 40.0 && slant < 60.0 {
                Gate::RangeFolded
            } else {
                Gate::Dbz(30.0)
            }
        });
        let section = radial_section(&scan, 118.0, 120.0);
        let axes = *section.axes();
        // 50 km out and 3 km up: inside the fold on every rung that reaches
        // there, and well clear of the lowest beam (0.75 km ARL at 50 km).
        let row = nearest_row(&axes, axes.base_km_msl + 3.0);
        let col = nearest_column(&axes, 50.0);

        assert_eq!(status_at(&section, col, row), SampleStatus::RangeFolded);
        assert!(value_at(&section, col, row).is_nan());
        assert_eq!(pixel_at(&section, col, row), crate::palette::RANGE_FOLDED);
        // The whole point: the shared function would have erased it.
        assert_eq!(
            crate::get_color_for_value(RadarProduct::Reflectivity, f32::NAN),
            (0, 0, 0, 0),
            "precondition: `get_color_for_value` no longer erases a folded \
             gate, so the extra arm has nothing to fix",
        );
        // A folded gate is also the radar *looking*, so a volume that folds
        // everywhere still reports its full coverage. Without this arm the
        // coverage number would collapse to "the farthest unambiguous echo",
        // and a section through a wholly folded second trip would claim its
        // data ran out at the site.
        let all_folded = scan_with(&|_az, _slant| Gate::RangeFolded);
        let folded_section = radial_section(&all_folded, 118.0, 120.0);
        assert!(
            folded_section.axes().coverage_ground_range_km > 119.0,
            "a wholly range-folded volume reported coverage out to only \
             {:.1} km",
            folded_section.axes().coverage_ground_range_km,
        );

        // And the fold is distinguishable from the echo beside it.
        let echo_col = nearest_column(&axes, 80.0);
        assert_eq!(status_at(&section, echo_col, row), SampleStatus::Value);
        assert_ne!(
            pixel_at(&section, echo_col, row),
            crate::palette::RANGE_FOLDED,
        );
    }

    // ── The ladder's two warnings ───────────────────────────────────────────

    /// `tilt_count` and `widest_tilt_gap_deg` reach the axes, and they report
    /// the ladder rather than the sweep list.
    ///
    /// The fixture's six sweeps are five cuts plus a SAILS repeat, so a count
    /// that counted sweeps would say six. Its widest gap is 19.47 − 9.94 =
    /// 9.53°, which no pair of *adjacent sweeps in collection order* produces.
    #[test]
    fn the_ladders_shape_travels_with_the_raster() {
        let scan = scan_with(&|_az, _slant| Gate::Dbz(25.0));
        let section = radial_section(&scan, 77.0, 100.0);
        let axes = *section.axes();

        assert_eq!(axes.tilt_count, 5, "six sweeps, five cuts");
        // The rung elevations come back through `f32` radial angles, so the
        // expected gap is written the same way rather than in decimal — the
        // two differ by 2.7e-7°, which is float width and not a discrepancy.
        let expected = f64::from(LADDER[4].1) - f64::from(LADDER[3].1);
        assert!(
            (axes.widest_tilt_gap_deg - expected).abs() < 1e-12,
            "the widest gap reported {:.6}°, not the {expected:.6}° between the \
             {}° and {}° cuts",
            axes.widest_tilt_gap_deg,
            LADDER[3].1,
            LADDER[4].1,
        );
        // precondition: the widest gap really is that pair, so the assertion
        // is about the ladder's shape and not about any two adjacent numbers.
        for pair in LADDER.windows(2) {
            assert!(
                f64::from(pair[1].1) - f64::from(pair[0].1) <= expected,
                "precondition: {}°..{}° is a wider gap than the pair this test \
                 names",
                pair[0].1,
                pair[1].1,
            );
        }
    }

    /// **The section carries the ladder it was cut from**, and it carries where
    /// that ladder stops against where the pattern says it should.
    ///
    /// # Why the rungs travel with the raster
    ///
    /// Drawing the rungs is the section's first honesty device, and a rung
    /// drawn at the wrong angle over a correct picture is worse than no rung at
    /// all. Before this the consumer had to *rediscover* the ladder — from
    /// `ScanInfo::product_elevations`, which rounds each sweep's median to 0.1°
    /// and dedups, against a sampler that groups by the cut table's nominal
    /// angle. Those count different things and disagree whenever two sweeps of
    /// one cut have medians straddling an `x.x5` boundary, which is half of all
    /// precipitation-mode volumes, complete ones included. The guard that
    /// noticed the disagreement could only refuse, so the device was simply
    /// absent there. One ladder, arriving with the picture, has nothing to
    /// disagree with.
    ///
    /// # Why the two tops
    ///
    /// `widest_tilt_gap_deg` is the *wrong* number mid-volume and it is wrong
    /// in the flattering direction: a volume four rungs in is all low, closely
    /// spaced cuts, so its gap reads better than a complete volume's. Where the
    /// ladder **stops** is the number that degrades as the volume truncates,
    /// and it only means anything beside what the pattern declares.
    #[test]
    fn the_section_carries_its_own_rungs_and_says_where_they_stop() {
        let field = |_az: f64, _slant: f64| Gate::Dbz(25.0);
        let build = |rungs: &[(u8, f32, usize)]| {
            let sweeps: Vec<Sweep> = rungs
                .iter()
                .map(|&(number, elevation, gates)| {
                    sweep(
                        number,
                        elevation,
                        720,
                        gates,
                        41.9 * f64::from(number),
                        &field,
                        FIRST_GATE_M,
                    )
                })
                .collect();
            Scan::new(vcp(&[0.5, 1.3, 4.0, 10.0, 19.5]), sweeps)
        };

        let complete = build(&LADDER);
        let section = radial_section(&complete, 77.0, 100.0);

        // One rung per rung, always — `from_parts` refuses any other length, so
        // a consumer never has a count to check against.
        assert_eq!(
            section.tilt_elevations_deg().len(),
            section.axes().tilt_count,
            "the ladder and the count that describes it disagree",
        );
        // And they are the *sampler's* angles, the ones every height in the
        // raster was computed from — not the cut table's nominal keys, which
        // sit up to 0.044° off and would draw each curve slightly clear of the
        // data it is meant to mark.
        let sampler = crate::sampler::VolumeSampler::new(&complete, RadarProduct::Reflectivity)
            .expect("the fixture volume samples");
        let expected: Vec<f64> = sampler.elevations_deg().collect();
        assert_eq!(section.tilt_elevations_deg(), expected.as_slice());
        // Which is emphatically not the nominal ladder: if it were, the two
        // would be interchangeable and this test would be pinning nothing.
        let nominal: Vec<f64> = sampler.nominal_elevations_deg().collect();
        assert_ne!(
            expected, nominal,
            "the fixture's medians land exactly on its cut angles, so this \
             cannot tell a section carrying geometry from one carrying keys",
        );

        // A volume that flew its whole pattern has reached its own ceiling, so
        // the blank above the top rung really is the cone of silence.
        assert_eq!(
            section.axes().top_tilt_deg,
            section.axes().top_declared_cut_deg,
            "a complete volume reported itself truncated",
        );
        assert_eq!(section.axes().top_declared_cut_deg, 19.5);

        // Four rungs into the same pattern — the live chunk feed's ordinary
        // state for most of every six minutes — the ceiling is the *volume's*,
        // and the picture has to be able to say so.
        let partial = build(&LADDER[..2]);
        let mid_flight = radial_section(&partial, 77.0, 100.0);
        assert_eq!(mid_flight.axes().tilt_count, 2);
        assert!(
            mid_flight.axes().top_tilt_deg < mid_flight.axes().top_declared_cut_deg,
            "a volume two cuts into a five-cut pattern reported a complete \
             ladder ({} against a declared {})",
            mid_flight.axes().top_tilt_deg,
            mid_flight.axes().top_declared_cut_deg,
        );
        // The declared ceiling is a property of the *pattern*, so it does not
        // move as the volume fills. That is what makes the comparison mean
        // anything: a number that shrank with the ladder would always agree
        // with it.
        assert_eq!(
            mid_flight.axes().top_declared_cut_deg,
            section.axes().top_declared_cut_deg,
            "the declared ceiling followed the ladder down, so a truncated \
             volume can never be told from a complete one",
        );

        // And it survives the wire, which is where a section that was cut in a
        // worker reaches the pane that draws it.
        let decoded = CrossSection::from_bytes(&mid_flight.to_bytes())
            .expect("a mid-flight section round-trips");
        assert_eq!(
            decoded.tilt_elevations_deg(),
            mid_flight.tilt_elevations_deg()
        );
        assert_eq!(decoded.axes().top_tilt_deg, mid_flight.axes().top_tilt_deg);
        assert_eq!(
            decoded.axes().top_declared_cut_deg,
            mid_flight.axes().top_declared_cut_deg,
        );
    }

    /// A short ladder is the hazard these numbers exist for: it fills the gap
    /// with a smooth layer that is not there, and only the numbers say so.
    ///
    /// The field is planted so that **only the base tilt and the top tilt see
    /// anything** — the three cuts between them looked and found nothing. That
    /// is a real shape: an elevated layer over a surface return, with clear air
    /// between.
    ///
    /// * The five-rung volume measures the gap and draws it: two thin bands
    ///   with empty sky between.
    /// * The two-rung volume has nothing to measure it with, so both of its
    ///   rungs carry a value, the vertical lerp runs the whole 18.94° between
    ///   them, and it paints **one continuous column of echo** across a gap it
    ///   never sampled — smooth, finite, `Value`-statused, with no `NaN`, no
    ///   seam and nothing in any of the three planes to give it away.
    ///
    /// Which is why the two numbers travel with the raster.
    #[test]
    fn a_short_ladder_fills_the_gap_it_cannot_measure_and_only_the_numbers_object() {
        // Echo on the lowest and highest cuts only, and varying along range so
        // a rasterizer that smeared one gate still gets the shape wrong.
        let plant = |elev: f64| {
            move |_az: f64, slant: f64| {
                if !(1.0..=15.0).contains(&elev) {
                    Gate::Dbz(30.0 + slant / 40.0)
                } else {
                    Gate::BelowThreshold
                }
            }
        };
        let build = |rungs: &[(u8, f32, usize)], cuts: &[f64]| {
            let sweeps: Vec<Sweep> = rungs
                .iter()
                .map(|&(number, elevation, gates)| {
                    let f = plant(f64::from(elevation));
                    sweep(
                        number,
                        elevation,
                        720,
                        gates,
                        41.9 * f64::from(number),
                        &f,
                        FIRST_GATE_M,
                    )
                })
                .collect();
            Scan::new(vcp(cuts), sweeps)
        };

        let full = build(&LADDER, &[0.5, 1.3, 4.0, 10.0, 19.5]);
        // The same volume with the middle of the ladder abandoned: only the
        // base tilt and the top one arrived.
        let short = build(&[LADDER[0], LADDER[4]], &[0.5, 1.3, 4.0, 10.0, 19.5]);

        let full_section = radial_section(&full, 200.0, 120.0);
        let short_section = radial_section(&short, 200.0, 120.0);

        assert_eq!(full_section.axes().tilt_count, 5);
        assert_eq!(short_section.axes().tilt_count, 2);
        let expected = f64::from(LADDER[4].1) - f64::from(LADDER[0].1);
        assert!(
            (short_section.axes().widest_tilt_gap_deg - expected).abs() < 1e-12,
            "the short ladder's gap reported {:.6}°, not {expected:.6}°",
            short_section.axes().widest_tilt_gap_deg,
        );
        // The abandoned ladder's widest gap *is* its whole span — there is
        // nothing between its two rungs — while the full ladder's worst gap is
        // half of it. That contrast is the number's whole job, so it is
        // asserted as a relation rather than as two decimals.
        assert!(
            full_section.axes().widest_tilt_gap_deg < 0.6 * expected,
            "the full ladder's widest gap is {:.3}° of a {expected:.3}° span, \
             so it is no longer distinguishable from an abandoned one",
            full_section.axes().widest_tilt_gap_deg,
        );

        // Now the pixels. Take one column, well out from the site, and read the
        // whole height axis in both volumes.
        let axes = *full_section.axes();
        let col = nearest_column(&axes, 50.0);
        let painted_rows = |s: &CrossSection| -> Vec<usize> {
            (0..SECTION_HEIGHT)
                .filter(|&row| status_at(s, col, row) == SampleStatus::Value)
                .collect()
        };
        let full_rows = painted_rows(&full_section);
        let short_rows = painted_rows(&short_section);
        assert!(!full_rows.is_empty() && !short_rows.is_empty());

        // The five-rung volume draws the gap: two bands with sky between.
        let bands = |rows: &[usize]| rows.windows(2).filter(|w| w[1] != w[0] + 1).count() + 1;
        assert_eq!(
            bands(&full_rows),
            2,
            "the measured volume drew {} bands, not the surface return and the \
             elevated layer this field plants",
            bands(&full_rows),
        );
        // The two-rung volume draws one, because it lerped straight across.
        assert_eq!(
            bands(&short_rows),
            1,
            "the two-rung volume drew {} bands; the fabrication this test \
             describes is not happening, so the warning numbers may no longer \
             be warning about anything",
            bands(&short_rows),
        );
        assert!(
            short_rows.len() > 3 * full_rows.len(),
            "the two-rung volume painted {} rows against the five-rung \
             volume's {}, which is not the wholesale fill this test describes",
            short_rows.len(),
            full_rows.len(),
        );

        // And the fabricated rows are indistinguishable from measured ones:
        // status `Value`, a finite number, and a colour off the same scale.
        // There is nothing in any of the three planes to warn on.
        let fabricated: Vec<usize> = short_rows
            .iter()
            .copied()
            .filter(|row| !full_rows.contains(row))
            .collect();
        assert!(
            fabricated.len() > SECTION_HEIGHT / 4,
            "only {} rows were fabricated, out of {SECTION_HEIGHT}",
            fabricated.len(),
        );
        for &row in &fabricated {
            assert_eq!(status_at(&short_section, col, row), SampleStatus::Value);
            assert!(
                value_at(&short_section, col, row).is_finite(),
                "a fabricated pixel carries {}",
                value_at(&short_section, col, row),
            );
            assert_ne!(
                pixel_at(&short_section, col, row).3,
                0,
                "a fabricated pixel is transparent, which would have given it \
                 away",
            );
        }
    }

    // ── Refusals, invariants and the wire ───────────────────────────────────

    /// The four request-shape refusals, each for its own reason.
    #[test]
    fn a_request_that_names_no_section_is_refused() {
        let scan = scan_with(&|_az, _slant| Gate::Dbz(30.0));
        let end = point_at(45.0, 100.0);

        assert!(
            render_section(&scan, &request(SITE, SITE), SITE.0, SITE.1).is_none(),
            "a zero-length line rendered",
        );
        assert!(
            render_section(&scan, &request((f64::NAN, -97.0), end), SITE.0, SITE.1).is_none(),
            "a non-finite endpoint rendered",
        );
        assert!(
            render_section(&scan, &request(SITE, end), f64::INFINITY, SITE.1).is_none(),
            "a non-finite site rendered",
        );
        for top in [
            Some(SITE_ELEV_KM),
            Some(SITE_ELEV_KM - 1.0),
            Some(f64::NAN),
            Some(f64::INFINITY),
        ] {
            assert!(
                render_section(
                    &scan,
                    &SectionRequest {
                        top_km_msl: top,
                        ..request(SITE, end)
                    },
                    SITE.0,
                    SITE.1,
                )
                .is_none(),
                "a top of {top:?} km MSL rendered",
            );
        }
        // A top above the site does render, so the refusals above are a
        // boundary and not a blanket.
        assert!(
            render_section(
                &scan,
                &SectionRequest {
                    top_km_msl: Some(SITE_ELEV_KM + 0.001),
                    ..request(SITE, end)
                },
                SITE.0,
                SITE.1,
            )
            .is_some(),
        );

        // A product with no Level II moment behind it, and a volume whose
        // coverage pattern is the empty placeholder a worker's reconstructed
        // scan carries.
        assert!(
            render_section(
                &scan,
                &SectionRequest {
                    product: RadarProduct::VerticallyIntegratedLiquid,
                    ..request(SITE, end)
                },
                SITE.0,
                SITE.1,
            )
            .is_none(),
            "a column integral was sectioned",
        );
        let placeholder = Scan::new(vcp(&[]), scan.sweeps().to_vec());
        assert!(
            render_section(&placeholder, &request(SITE, end), SITE.0, SITE.1).is_none(),
            "a scan with an empty cut table rendered, so a worker would build \
             a different ladder than the main thread with no error",
        );
    }

    /// Every plane is the size the constants promise, and every axis number is
    /// finite — which is what lets `SectionAxes` derive `PartialEq`.
    #[test]
    fn every_axis_number_of_a_rendered_section_is_finite() {
        let scan = scan_with(&|az, slant| Gate::Dbz(15.0 + az / 60.0 + slant / 30.0));
        for (start, end) in [
            (SITE, point_at(0.0, 230.0)),
            (point_at(200.0, 100.0), point_at(20.0, 100.0)),
            (point_at(90.0, 400.0), point_at(270.0, 400.0)),
            (point_at(10.0, 0.2), point_at(190.0, 0.2)),
        ] {
            let section = render_section(&scan, &request(start, end), SITE.0, SITE.1).unwrap();
            let a = section.axes();
            for (name, v) in [
                ("length_km", a.length_km),
                ("base_km_msl", a.base_km_msl),
                ("top_km_msl", a.top_km_msl),
                ("near_ground_range_km", a.near_ground_range_km),
                ("far_ground_range_km", a.far_ground_range_km),
                ("coverage_ground_range_km", a.coverage_ground_range_km),
                ("cone_of_silence_km", a.cone_of_silence_km),
                ("widest_tilt_gap_deg", a.widest_tilt_gap_deg),
            ] {
                assert!(v.is_finite(), "{name} is {v} for {start:?}..{end:?}");
            }
            assert_eq!(a.base_km_msl, SITE_ELEV_KM);
            assert_eq!(a.top_km_msl, SITE_ELEV_KM + DEFAULT_AXIS_HEIGHT_KM);
            assert!(a.near_ground_range_km <= a.far_ground_range_km);
            assert!(a.coverage_ground_range_km <= a.far_ground_range_km);
            assert!(a.cone_of_silence_km <= a.length_km);

            let pixels = SECTION_WIDTH * SECTION_HEIGHT;
            assert_eq!(section.image().len(), pixels * 4);
            assert_eq!(section.values().len(), pixels);
            assert_eq!(section.status().len(), pixels);
        }
    }

    /// A section of nothing equals a copy of itself.
    ///
    /// This is the property a derived `PartialEq` destroys: every pixel of a
    /// blank section carries `f32::NAN`, and `NaN != NaN`, so WP-D's
    /// `assert_eq!(execute(&…), None)` over a `JobOutput` containing one would
    /// fail on a byte-identical value with nothing in the message saying why.
    #[test]
    fn a_section_with_no_values_still_equals_itself() {
        // Well outside the volume: every rung is `BeyondRange`, so every pixel
        // is missing and every value is `NaN`.
        let scan = scan_with(&|_az, _slant| Gate::Dbz(30.0));
        let far = render_section(
            &scan,
            &request(point_at(0.0, 800.0), point_at(90.0, 800.0)),
            SITE.0,
            SITE.1,
        )
        .unwrap();
        assert!(
            far.values().iter().all(|v| v.is_nan()),
            "precondition: the far section has values in it, so it is not the \
             blank raster this test needs",
        );
        // precondition: `all` is vacuously true on an empty slice, so the
        // assertion above says nothing unless the plane is the full raster.
        assert_eq!(far.values().len(), SECTION_WIDTH * SECTION_HEIGHT);
        let copy = far.clone();
        assert_eq!(far, copy, "a blank section is unequal to a copy of itself");

        // An ordinary near-site section — a real echo, drawn a few tens of km
        // out — is the case that makes this more than a corner. It is mostly
        // values, and it *still* fails under derived semantics, because the
        // upper cuts stop short, the base tilt has a floor and the cone has a
        // ceiling, so some pixel somewhere is a `NaN`.
        let near = radial_section(&scan, 45.0, 100.0);
        assert!(
            near.values().iter().any(|v| v.is_nan()) && near.values().iter().any(|v| v.is_finite()),
            "precondition: the near section is all one thing, so it is not the \
             ordinary mixed raster this half of the test is about",
        );
        assert_eq!(
            near,
            near.clone(),
            "an ordinary section is unequal to a copy of itself",
        );

        // And a section *with* values still compares on them: a changed number
        // is a changed section.
        let mut tweaked = near.clone();
        let i = tweaked
            .status
            .iter()
            .position(|&s| s == SampleStatus::Value.wire_code())
            .expect("the near section has values");
        tweaked.values[i] += 1.0;
        assert_ne!(near, tweaked, "a changed value did not change the section");

        // A changed *status* is a changed section even where both values are
        // NaN — the status plane is compared unconditionally.
        let mut restatused = far.clone();
        assert_ne!(far.status[0], SampleStatus::NoCoverage.wire_code());
        restatused.status[0] = SampleStatus::NoCoverage.wire_code();
        assert_ne!(far, restatused);

        // Every part is compared, and each is checked on its own: otherwise a
        // conjunct could be dropped one at a time with every other assertion
        // here still passing, and two sections of different places or
        // different pictures would compare equal on the wire.
        let mut reaxed = far.clone();
        reaxed.axes.top_km_msl += 1.0;
        assert_ne!(far, reaxed, "a changed axis did not change the section");

        let mut repainted = far.clone();
        repainted.image[0] = repainted.image[0].wrapping_add(1);
        assert_ne!(far, repainted, "a changed pixel did not change the section");

        // A length mismatch is an inequality and not a panic: without the
        // length test the `zip` would compare the shorter prefix and call a
        // truncated payload equal to a whole one.
        let mut truncated = far.clone();
        truncated.values.pop();
        assert_ne!(
            far, truncated,
            "a truncated value plane compared equal to the whole one",
        );
    }

    /// The wire constructor refuses anything that would panic a consumer, and
    /// round-trips anything that would not.
    #[test]
    fn the_wire_constructor_refuses_a_misshaped_section() {
        let scan = scan_with(&|_az, _slant| Gate::Dbz(30.0));
        let section = radial_section(&scan, 45.0, 100.0);
        let axes = *section.axes();
        let ladder = || section.tilt_elevations_deg().to_vec();

        let rebuilt = CrossSection::from_parts(
            section.image().to_vec(),
            section.values().to_vec(),
            section.status().to_vec(),
            axes,
            ladder(),
        )
        .expect("a section round-trips through its own planes");
        assert_eq!(rebuilt, section);

        let short = |n: usize| {
            let mut v = section.image().to_vec();
            v.truncate(n);
            v
        };
        assert!(
            CrossSection::from_parts(
                short(section.image().len() - 4),
                section.values().to_vec(),
                section.status().to_vec(),
                axes,
                ladder(),
            )
            .is_none(),
            "a short image plane was accepted — `apply_render_to_pane` asserts \
             this length on the main thread, live in release",
        );
        assert!(
            CrossSection::from_parts(
                section.image().to_vec(),
                section.values()[1..].to_vec(),
                section.status().to_vec(),
                axes,
                ladder(),
            )
            .is_none(),
            "a short value plane was accepted",
        );
        assert!(
            CrossSection::from_parts(
                section.image().to_vec(),
                section.values().to_vec(),
                section.status()[1..].to_vec(),
                axes,
                ladder(),
            )
            .is_none(),
            "a short status plane was accepted",
        );

        // A status byte this build cannot name — what a payload from a newer
        // sender looks like. Accepting it would make `sample` have to invent an
        // answer.
        let mut future = section.status().to_vec();
        future[7] = 200;
        assert!(
            CrossSection::from_parts(
                section.image().to_vec(),
                section.values().to_vec(),
                future,
                axes,
                ladder(),
            )
            .is_none(),
            "an unknown status code was accepted",
        );

        // A non-finite axis. Every field is checked on its own, because a
        // single `all_finite` walk that dropped one of them would still pass
        // an assertion made about any other.
        for name in [
            "length_km",
            "base_km_msl",
            "top_km_msl",
            "near_ground_range_km",
            "far_ground_range_km",
            "coverage_ground_range_km",
            "cone_of_silence_km",
            "widest_tilt_gap_deg",
        ] {
            // Both bars, because `is_finite` and `!is_nan` differ exactly on
            // the infinities and a mapping is affine in these fields: an
            // infinite `top_km_msl` gives every row height an infinity, and
            // `inf - inf` at the bottom of the axis a `NaN`.
            for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
                let mut broken = axes;
                match name {
                    "length_km" => broken.length_km = bad,
                    "base_km_msl" => broken.base_km_msl = bad,
                    "top_km_msl" => broken.top_km_msl = bad,
                    "near_ground_range_km" => broken.near_ground_range_km = bad,
                    "far_ground_range_km" => broken.far_ground_range_km = bad,
                    "coverage_ground_range_km" => broken.coverage_ground_range_km = bad,
                    "cone_of_silence_km" => broken.cone_of_silence_km = bad,
                    "widest_tilt_gap_deg" => broken.widest_tilt_gap_deg = bad,
                    other => unreachable!("{other} is not a field of SectionAxes"),
                }
                assert!(
                    CrossSection::from_parts(
                        section.image().to_vec(),
                        section.values().to_vec(),
                        section.status().to_vec(),
                        broken,
                        ladder(),
                    )
                    .is_none(),
                    "a {name} of {bad} was accepted",
                );
            }
        }
        // The two tilt-top angles are checked by the same walk, and they are
        // the pair a caption reads to decide whether a ceiling in the picture
        // is the radar's or the volume's. A `NaN` in either makes that
        // comparison answer "truncated" for every complete volume there is.
        for name in ["top_tilt_deg", "top_declared_cut_deg"] {
            for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
                let mut broken = axes;
                match name {
                    "top_tilt_deg" => broken.top_tilt_deg = bad,
                    "top_declared_cut_deg" => broken.top_declared_cut_deg = bad,
                    other => unreachable!("{other} is not a field of SectionAxes"),
                }
                assert!(
                    CrossSection::from_parts(
                        section.image().to_vec(),
                        section.values().to_vec(),
                        section.status().to_vec(),
                        broken,
                        ladder(),
                    )
                    .is_none(),
                    "a {name} of {bad} was accepted",
                );
            }
        }

        // `tilt_count` is a `usize` and has no non-finite value to have, so a
        // whole-axes rejection would be wrong: an ordinary count still builds —
        // as long as the ladder that comes with it is that long.
        assert!(
            CrossSection::from_parts(
                section.image().to_vec(),
                section.values().to_vec(),
                section.status().to_vec(),
                SectionAxes {
                    tilt_count: 0,
                    ..axes
                },
                Vec::new(),
            )
            .is_some(),
            "the finiteness check refused something that has no finiteness",
        );

        // **The ladder and the count that describes it are one fact.** This is
        // the refusal that lets a consumer draw the rungs without first
        // checking them against a separately-discovered elevation list — the
        // check that counted something else and went silent on half of all
        // precipitation-mode volumes. A section whose two halves disagree is
        // not representable.
        assert!(
            !ladder().is_empty(),
            "precondition: the fixture has a ladder to shorten"
        );
        for wrong in [ladder()[1..].to_vec(), Vec::new()] {
            assert!(
                CrossSection::from_parts(
                    section.image().to_vec(),
                    section.values().to_vec(),
                    section.status().to_vec(),
                    axes,
                    wrong.clone(),
                )
                .is_none(),
                "a {}-rung ladder was accepted for a {}-rung section",
                wrong.len(),
                axes.tilt_count,
            );
        }
        // And a rung that is not an angle. A `NaN` elevation draws no curve and
        // reports nothing, so the honesty device goes quiet in the one way
        // nobody notices.
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let mut broken = ladder();
            broken[0] = bad;
            assert!(
                CrossSection::from_parts(
                    section.image().to_vec(),
                    section.values().to_vec(),
                    section.status().to_vec(),
                    axes,
                    broken,
                )
                .is_none(),
                "a rung at {bad} degrees was accepted",
            );
        }

        // A `Value` status over a number that is not one. Both bars are
        // exercised: `NaN` paints nothing, but an **infinity** compares larger
        // than every threshold in the scale and paints the top of it, so a
        // section carrying one looks like the strongest echo in the volume.
        let at = section
            .status()
            .iter()
            .position(|&s| s == SampleStatus::Value.wire_code())
            .expect("the near section has values");
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut broken = section.values().to_vec();
            broken[at] = bad;
            assert!(
                CrossSection::from_parts(
                    section.image().to_vec(),
                    broken,
                    section.status().to_vec(),
                    axes,
                    ladder(),
                )
                .is_none(),
                "a Value pixel carrying {bad} was accepted",
            );
        }
        // And the same number under a status that has no number is ordinary —
        // it is what every missing pixel already holds — so the check is a
        // pairing rather than a blanket ban on `NaN` in the plane.
        let missing = section
            .status()
            .iter()
            .position(|&s| s != SampleStatus::Value.wire_code())
            .expect("the near section has missing pixels");
        let mut nan_where_nothing_is = section.values().to_vec();
        nan_where_nothing_is[missing] = f32::NAN;
        assert!(
            CrossSection::from_parts(
                section.image().to_vec(),
                nan_where_nothing_is,
                section.status().to_vec(),
                axes,
                ladder(),
            )
            .is_some(),
            "a NaN under a non-Value status was refused, which is every \
             missing pixel of every section",
        );
    }

    /// The per-pixel reader pairs the two planes back into the sample that
    /// produced them, which is what a hover readout needs.
    #[test]
    fn the_pixel_reader_recovers_the_sample_behind_a_pixel() {
        let scan = scan_with(&|_az, slant| {
            if slant > 40.0 && slant < 60.0 {
                Gate::RangeFolded
            } else {
                Gate::Dbz(33.0)
            }
        });
        let section = radial_section(&scan, 210.0, 120.0);
        let axes = *section.axes();
        let row = (0..SECTION_HEIGHT)
            .find(|&row| axes.row_height_km_msl(row) < SITE_ELEV_KM + 0.6)
            .unwrap();

        for col in [0, 1, SECTION_WIDTH / 3, SECTION_WIDTH - 1] {
            let sample = section.sample(col, row).expect("inside the raster");
            assert_eq!(sample.status(), status_at(&section, col, row));
            match sample.value() {
                Some(v) => assert_eq!(v, value_at(&section, col, row)),
                None => assert!(value_at(&section, col, row).is_nan()),
            }
        }
        // A folded pixel comes back as folded rather than as a number.
        let folded = (0..SECTION_WIDTH)
            .find(|&col| status_at(&section, col, row) == SampleStatus::RangeFolded)
            .expect("the fixture plants a fold");
        assert_eq!(
            section.sample(folded, row).unwrap().status(),
            SampleStatus::RangeFolded,
        );
        assert!(section.sample(folded, row).unwrap().value().is_none());

        assert!(section.sample(SECTION_WIDTH, row).is_none());
        assert!(section.sample(0, SECTION_HEIGHT).is_none());
    }

    /// Nothing is extrapolated: off the top of the ladder the section says
    /// `AboveVolume`, off the bottom it says `BelowLowestBeam`, and neither
    /// carries a number.
    ///
    /// The two happen at opposite ends of the *line*, not of one column, and
    /// that is the geometry rather than a convenience: near the site the ladder
    /// is squashed under the axis top and near its far end the lowest beam is
    /// above the axis bottom.
    #[test]
    fn the_section_says_which_side_of_the_ladder_it_fell_off() {
        let scan = scan_with(&|_az, _slant| Gate::Dbz(40.0));
        let section = radial_section(&scan, 133.0, 200.0);
        let axes = *section.axes();

        // Off the top: 20 km up over a column 10 km from the site, where even
        // the 19.5° beam is only 3.6 km ARL.
        let near = nearest_column(&axes, 10.0);
        assert_eq!(
            status_at(&section, near, 0),
            SampleStatus::AboveVolume,
            "at {:.1} km the top row says {:?}",
            axes.column_distance_km(near),
            status_at(&section, near, 0),
        );

        // Off the bottom: the bottom row at 150 km, where earth curvature alone
        // has lifted the base tilt's beam to 2.7 km ARL.
        let far = nearest_column(&axes, 150.0);
        assert_eq!(
            status_at(&section, far, SECTION_HEIGHT - 1),
            SampleStatus::BelowLowestBeam,
            "at {:.1} km the bottom row says {:?}",
            axes.column_distance_km(far),
            status_at(&section, far, SECTION_HEIGHT - 1),
        );

        for (col, row) in [(near, 0), (far, SECTION_HEIGHT - 1)] {
            assert!(value_at(&section, col, row).is_nan());
            assert_eq!(pixel_at(&section, col, row), (0, 0, 0, 0));
        }
    }

    /// A bracketing rung whose gates stop short reads `BeyondRange`, and that
    /// is ordinary rather than exceptional.
    ///
    /// Every real volume has one at 230 km and 300 km because the upper cuts
    /// are range-truncated, and 8 of 19 measured volumes have one at 150 km.
    /// A rasterizer that treated it as an error, or that dropped the empty rung
    /// and widened the bracket, would interpolate straight across a tilt that
    /// measured nothing — which is the fabrication the whole status plane
    /// exists to make visible.
    #[test]
    fn a_bracketing_rung_that_stops_short_reads_beyond_range() {
        let scan = scan_with(&|_az, _slant| Gate::Dbz(40.0));
        let section = radial_section(&scan, 133.0, 200.0);
        let axes = *section.axes();

        // At 150 km the 9.94° rung has run out (500 gates, 125 km of ground)
        // while the 4.02° rung below it (800 gates, 201 km) has not. A height
        // above their midpoint therefore brackets a live rung and a dead one,
        // and the dead one is the heavier.
        let col = nearest_column(&axes, 150.0);
        let ground = axes.column_distance_km(col);
        let low = beam::height_at_ground_km(ground, f64::from(LADDER[2].1));
        let high = beam::height_at_ground_km(ground, f64::from(LADDER[3].1));
        assert!(
            low < 20.0 && high > 20.0,
            "precondition: the 4.02°/9.94° bracket at {ground:.1} km is \
             {low:.2}..{high:.2} km ARL, which no longer straddles the axis top",
        );
        let row = nearest_row(&axes, axes.base_km_msl + (low + high) / 2.0 + 1.0);
        assert_eq!(
            status_at(&section, col, row),
            SampleStatus::BeyondRange,
            "at {ground:.1} km, {:.2} km ARL the section says {:?} instead of \
             reporting the truncated rung",
            axes.row_height_km_msl(row) - axes.base_km_msl,
            status_at(&section, col, row),
        );
        assert!(value_at(&section, col, row).is_nan());

        // And just under the midpoint the live rung wins, so the truncation is
        // a boundary in the picture rather than a blanket refusal.
        let live = nearest_row(&axes, axes.base_km_msl + low + 0.2);
        assert_eq!(
            status_at(&section, col, live),
            SampleStatus::Value,
            "the live 4.02° rung's own height reads {:?}",
            status_at(&section, col, live),
        );
    }

    /// The three planes agree pixel for pixel: a status of `Value` has a
    /// number, anything else has `NaN`, and the colour is what the colour
    /// function says about that pair.
    ///
    /// Swept over the whole raster rather than sampled, because a disagreement
    /// between the planes is exactly what a hover readout would surface as
    /// "42 dBZ (below the lowest beam)".
    #[test]
    fn the_three_planes_agree_everywhere() {
        let scan = scan_with(&|az, slant| {
            if slant > 90.0 && slant < 110.0 {
                Gate::RangeFolded
            } else if az > 200.0 {
                Gate::BelowThreshold
            } else {
                Gate::Dbz(-10.0 + slant / 4.0)
            }
        });
        let section = radial_section(&scan, 137.0, 240.0);

        let mut seen = std::collections::BTreeSet::new();
        for i in 0..SECTION_WIDTH * SECTION_HEIGHT {
            let status = SampleStatus::from_wire_code(section.status()[i]).unwrap();
            seen.insert(status.wire_code());
            let value = section.values()[i];
            if status == SampleStatus::Value {
                assert!(value.is_finite(), "a Value pixel carries {value}");
            } else {
                assert!(value.is_nan(), "a {status:?} pixel carries {value}");
            }
            let expected = if status == SampleStatus::RangeFolded {
                crate::palette::RANGE_FOLDED
            } else {
                crate::get_color_for_value(RadarProduct::Reflectivity, value)
            };
            let px = &section.image()[i * 4..i * 4 + 4];
            assert_eq!(
                (px[0], px[1], px[2], px[3]),
                expected,
                "pixel {i} ({status:?}, {value}) is painted wrong",
            );
        }
        // precondition: the fixture really exercised more than one status, or
        // the sweep above is a sweep over one arm.
        assert!(
            seen.len() >= 4,
            "the fixture produced only {} statuses ({seen:?}); this sweep is \
             not discriminating",
            seen.len(),
        );
    }

    // ── The wire codec ──────────────────────────────────────────────────────

    /// Where each of the three planes' length prefixes sits in an encoded
    /// section: after the magic, the version and the nine axis numbers, then
    /// after each preceding plane.
    ///
    /// Written out here rather than taken from the encoder, so a layout change
    /// that moved a field has to be made in both places — the mutations these
    /// offsets support are the whole point of the tests below, and an offset
    /// derived from the code under test would follow it wherever it went.
    /// `the_length_prefixes_are_where_the_tests_think_they_are` checks them,
    /// [`WIRE_FIXTURE_RUNGS`] included.
    ///
    /// How many rungs every fixture below encodes: `scan_with` flies a
    /// five-cut pattern, and the ladder's `f64` per rung sits between the axes
    /// and the first plane.
    const WIRE_FIXTURE_RUNGS: usize = 5;
    const IMAGE_LEN_AT: usize = 4 + 2 + 7 * 8 + 4 + 3 * 8 + 4 + WIRE_FIXTURE_RUNGS * 8;
    const VALUE_LEN_AT: usize = IMAGE_LEN_AT + 4 + SECTION_WIDTH * SECTION_HEIGHT * 4;
    const STATUS_LEN_AT: usize = VALUE_LEN_AT + 4 + SECTION_WIDTH * SECTION_HEIGHT * 4;

    fn prefix_at(bytes: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(bytes[at..at + 4].try_into().expect("four bytes"))
    }

    /// A mixed section — real echo, `BeyondRange` above, `BelowLowestBeam`
    /// below — which is the ordinary shape and the one every codec test below
    /// starts from.
    fn wire_fixture() -> CrossSection {
        let scan = scan_with(&|az, slant| {
            if slant > 60.0 && slant < 80.0 {
                Gate::RangeFolded
            } else if az > 240.0 {
                Gate::BelowThreshold
            } else {
                Gate::Dbz(-6.0 + slant / 6.0)
            }
        });
        radial_section(&scan, 137.0, 150.0)
    }

    /// The three offsets the mutation tests below index by are the three the
    /// encoder actually wrote.
    ///
    /// Every refusal test plants a value at one of them, so an offset that had
    /// drifted would leave those tests corrupting a byte of some other field
    /// and passing for the wrong reason — the classic way a suite of negative
    /// assertions goes green while testing nothing.
    #[test]
    fn the_length_prefixes_are_where_the_tests_think_they_are() {
        let fixture = wire_fixture();
        assert_eq!(
            fixture.tilt_elevations_deg().len(),
            WIRE_FIXTURE_RUNGS,
            "the fixture's ladder changed length, so every offset below now \
             points into the middle of a plane and the negative tests are \
             corrupting the wrong bytes",
        );
        let bytes = fixture.to_bytes();
        let pixels = SECTION_WIDTH * SECTION_HEIGHT;
        assert_eq!(prefix_at(&bytes, IMAGE_LEN_AT), (pixels * 4) as u32);
        assert_eq!(prefix_at(&bytes, VALUE_LEN_AT), pixels as u32);
        assert_eq!(prefix_at(&bytes, STATUS_LEN_AT), pixels as u32);
        assert_eq!(bytes.len(), STATUS_LEN_AT + 4 + pixels);
    }

    /// The version this layout ships is **2**, and it is written where a
    /// decoder from another build reads it.
    ///
    /// `a_malformed_section_payload_is_refused_rather_than_misread` plants
    /// `0xFF 0xFF` and watches the decode refuse, which pins that *a* version
    /// check exists — not *which* version ships. Setting `FORMAT_VERSION` back
    /// to 1 left that test, and every other test in the workspace, green: both
    /// ends of the codec move together, so a build is always self-consistent
    /// and the constant is only load-bearing *between* builds.
    ///
    /// Between builds is where it is the only defence. `rustdar-web`'s
    /// page/worker handshake is `build_token = version/PROTOCOL_VERSION/
    /// GITHUB_SHA`, and `GITHUB_SHA` is absent outside CI, so it degrades to
    /// `…/dev` and a stale worker shares a token with a fresh page. If a layout
    /// change forgets the bump — reordering two same-width `f64` axis fields is
    /// the easy one, since it round-trips perfectly through its own build's
    /// codec — the stale worker's payload decodes into the new field order
    /// silently, and a section is drawn with its axes swapped.
    ///
    /// So the literal is written twice on purpose: once as the constant, once
    /// as the bytes on the wire. Mirrors `render_input`'s
    /// `the_format_version_is_the_one_this_layout_ships`, which the same
    /// argument applies to.
    #[test]
    fn the_format_version_is_the_one_this_layout_ships() {
        assert_eq!(FORMAT_VERSION, 2);
        let bytes = wire_fixture().to_bytes();
        assert_eq!(&bytes[..4], &MAGIC, "the magic moved");
        assert_eq!(
            u16::from_le_bytes([bytes[4], bytes[5]]),
            2,
            "the version is not where a decoder from another build looks for it",
        );
    }

    /// A real section survives the wire, including one that is `NaN` in every
    /// pixel.
    ///
    /// This is what [`CrossSection`]'s hand-written `PartialEq` was written
    /// for: a blank section carries `f32::NAN` in every value, and under
    /// derived semantics `assert_eq!` here would fail on a byte-identical
    /// payload with nothing in the message saying why.
    #[test]
    fn a_section_round_trips_through_its_wire_form() {
        let scan = scan_with(&|az, slant| {
            if slant > 60.0 && slant < 80.0 {
                Gate::RangeFolded
            } else if az > 240.0 {
                Gate::BelowThreshold
            } else {
                Gate::Dbz(-6.0 + slant / 6.0)
            }
        });
        // A mixed section, a blank one and one whose axis the caller chose —
        // three different shapes of payload, not three copies of one.
        let mixed = radial_section(&scan, 137.0, 150.0);
        let blank = render_section(
            &scan,
            &request(point_at(0.0, 800.0), point_at(90.0, 800.0)),
            SITE.0,
            SITE.1,
        )
        .expect("a section well outside the volume still renders");
        let shallow = render_section(
            &scan,
            &SectionRequest {
                top_km_msl: Some(SITE_ELEV_KM + 4.0),
                ..request(SITE, point_at(45.0, 100.0))
            },
            SITE.0,
            SITE.1,
        )
        .expect("a low axis renders");

        assert!(
            blank.values().iter().all(|v| v.is_nan()),
            "precondition: the blank section has numbers in it, so the NaN \
             half of this claim is untested",
        );
        assert!(
            mixed.values().iter().any(|v| v.is_nan())
                && mixed.values().iter().any(|v| v.is_finite()),
            "precondition: the mixed section is all one thing",
        );
        let mut statuses = std::collections::BTreeSet::new();
        statuses.extend(mixed.status().iter().copied());
        assert!(
            statuses.len() >= 3,
            "precondition: the mixed section carries only {} statuses, so a \
             codec that lost the status plane could still pass",
            statuses.len(),
        );

        for (name, section) in [
            ("mixed", &mixed),
            ("blank", &blank),
            ("shallow axis", &shallow),
        ] {
            let decoded = CrossSection::from_bytes(&section.to_bytes())
                .unwrap_or_else(|| panic!("the {name} section did not decode"));
            assert_eq!(*section, decoded, "the {name} section changed in transit");
            // `PartialEq` ignores a value under a non-`Value` status, so an
            // encoder that dropped the value plane's NaN payloads would still
            // satisfy the assertion above. The bytes say more.
            assert_eq!(
                section.to_bytes(),
                decoded.to_bytes(),
                "the {name} section re-encodes differently",
            );
            assert_eq!(section.axes(), decoded.axes(), "{name}");
        }

        // And the comparison is not vacuous: three sections of three
        // different places do not decode to one another.
        assert_ne!(mixed, blank);
        assert_ne!(mixed, shallow);
        assert_ne!(
            CrossSection::from_bytes(&mixed.to_bytes()).unwrap(),
            CrossSection::from_bytes(&blank.to_bytes()).unwrap(),
        );
    }

    /// `to_bytes` reserves exactly what it writes. A section is 12 MB
    /// natively, so a wrong estimate is a copy of all of it.
    #[test]
    fn the_encoded_length_of_a_section_is_exact() {
        let section = wire_fixture();
        assert_eq!(section.encoded_len(), section.to_bytes().len());
        // A second shape, so the estimate is pinned against something other
        // than one raster's constant total.
        let blank = render_section(
            &scan_with(&|_az, _slant| Gate::Dbz(30.0)),
            &request(point_at(0.0, 800.0), point_at(90.0, 800.0)),
            SITE.0,
            SITE.1,
        )
        .unwrap();
        assert_eq!(blank.encoded_len(), blank.to_bytes().len());
    }

    /// The bytes arrive off a message port. Every malformed shape has to be a
    /// clean `None` — the two ends of that port can be different builds.
    #[test]
    fn a_malformed_section_payload_is_refused_rather_than_misread() {
        let section = wire_fixture();
        let good = section.to_bytes();
        let pixels = SECTION_WIDTH * SECTION_HEIGHT;

        assert!(CrossSection::from_bytes(&[]).is_none(), "empty");
        assert!(CrossSection::from_bytes(b"nope").is_none(), "wrong magic");

        // A **whole** payload relabelled, including with the two magics that
        // share this port. Mutation testing is why: a four-byte buffer cannot
        // pin the magic test, because it fails on the version read instead —
        // deleting the magic comparison outright left every short-buffer
        // assertion here green, and a `RenderInput` frame would then have been
        // decoded as a section.
        for wrong in [*b"nope", *b"RDRI", *b"RDVX"] {
            let mut relabelled = good.clone();
            relabelled[..4].copy_from_slice(&wrong);
            assert!(
                CrossSection::from_bytes(&relabelled).is_none(),
                "a whole payload labelled {} decoded as a section",
                String::from_utf8_lossy(&wrong),
            );
        }

        let mut wrong_version = good.clone();
        wrong_version[4] = 0xFF;
        wrong_version[5] = 0xFF;
        assert!(
            CrossSection::from_bytes(&wrong_version).is_none(),
            "an unknown version decoded",
        );

        for cut in [
            1,
            8,
            IMAGE_LEN_AT,
            IMAGE_LEN_AT + 2,
            VALUE_LEN_AT,
            VALUE_LEN_AT + 4,
            STATUS_LEN_AT,
            good.len() / 2,
            good.len() - 1,
        ] {
            assert!(
                CrossSection::from_bytes(&good[..cut]).is_none(),
                "truncated to {cut} bytes",
            );
        }

        let mut trailing = good.clone();
        trailing.push(0);
        assert!(
            CrossSection::from_bytes(&trailing).is_none(),
            "trailing bytes mean the layouts disagree",
        );

        // A length that cannot fit in what remains, on each of the three
        // planes. The value plane is the one that matters: four bytes an
        // element, so a believed `u32::MAX` reserves 16 GiB before the read
        // fails.
        for (name, at) in [
            ("image", IMAGE_LEN_AT),
            ("values", VALUE_LEN_AT),
            ("status", STATUS_LEN_AT),
        ] {
            let mut absurd = good.clone();
            absurd[at..at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
            assert!(
                CrossSection::from_bytes(&absurd).is_none(),
                "an absurd {name} length reached a read",
            );
        }

        // A plane sized for a different build's raster — the ordinary
        // cross-build case, since `SECTION_WIDTH` is 2048 native and 1024 on
        // wasm. Shrunk by one element each, with the prefix moved to match, so
        // the frame is well-formed right through to `at_end` and only
        // `from_parts` can object.
        for (name, at, element) in [
            ("image", IMAGE_LEN_AT, 1usize),
            ("values", VALUE_LEN_AT, 4),
            ("status", STATUS_LEN_AT, 1),
        ] {
            let mut short = good.clone();
            let count = prefix_at(&short, at) as usize;
            let plane_end = at + 4 + count * element;
            short[at..at + 4].copy_from_slice(&((count - 1) as u32).to_le_bytes());
            short.drain(plane_end - element..plane_end);
            assert!(
                CrossSection::from_bytes(&short).is_none(),
                "a {name} plane one element short of this build's raster decoded",
            );
        }

        // A status byte this build cannot name — a payload from a newer
        // sender.
        let mut future = good.clone();
        future[STATUS_LEN_AT + 4 + 7] = 200;
        assert!(
            CrossSection::from_bytes(&future).is_none(),
            "an unknown status code decoded",
        );

        // A non-finite axis. `top_km_msl` is the third `f64`, and it is the
        // one that makes every row height `NaN`.
        let mut nan_axis = good.clone();
        nan_axis[4 + 2 + 16..4 + 2 + 24].copy_from_slice(&f64::NAN.to_le_bytes());
        assert!(
            CrossSection::from_bytes(&nan_axis).is_none(),
            "a NaN top_km_msl decoded",
        );

        // A `Value` pixel with no finite number behind it.
        let at = section
            .status()
            .iter()
            .position(|&s| s == SampleStatus::Value.wire_code())
            .expect("the fixture has values");
        for bad in [f32::NAN, f32::INFINITY] {
            let mut broken = good.clone();
            let off = VALUE_LEN_AT + 4 + at * 4;
            broken[off..off + 4].copy_from_slice(&bad.to_le_bytes());
            assert!(
                CrossSection::from_bytes(&broken).is_none(),
                "a Value pixel carrying {bad} decoded",
            );
        }

        // precondition: the fixture the mutations were made against decodes,
        // so every refusal above is the mutation's doing and not the
        // fixture's.
        assert_eq!(
            CrossSection::from_bytes(&good).expect("the unmutated payload decodes"),
            section,
        );
        assert_eq!(good.len(), STATUS_LEN_AT + 4 + pixels);
    }

    /// The capacity guard, tested directly, because nothing end to end can
    /// see it.
    ///
    /// [`Reader::bounded`] does not change *what*
    /// [`CrossSection::from_bytes`] answers. `take` bounds every read, so a
    /// believed length fails on the read either way and the payload is refused
    /// with or without it. What it changes is whether four billion elements
    /// are reserved **first** — a 16 GiB allocation on the way to a `None`, on
    /// a worker thread, in a browser tab. Mutation testing confirms the gap
    /// rather than assuming it: deleting the call from `from_bytes` leaves the
    /// whole suite green, which is why the helper is named and pinned here
    /// instead, exactly as `is_blind` and `ceiling_is_under` are above.
    #[test]
    fn the_capacity_guard_refuses_a_length_the_buffer_cannot_hold() {
        let bytes = [0u8; 16];
        let r = Reader::new(&bytes);
        assert_eq!(r.bounded(4, 4), Some(4), "16 bytes hold four f32");
        assert_eq!(r.bounded(0, 4), Some(0));
        assert_eq!(r.bounded(5, 4), None, "20 bytes claimed from 16");
        assert_eq!(r.bounded(u32::MAX, 4), None, "16 GiB claimed from 16 bytes");

        // It measures against what is *left*, not against the whole buffer —
        // otherwise a length prefix late in a frame would be judged against
        // bytes already consumed.
        let mut part_way = Reader::new(&bytes);
        part_way.take(8).expect("half the buffer");
        assert_eq!(part_way.bounded(2, 4), Some(2));
        assert_eq!(part_way.bounded(3, 4), None);

        // And the multiply cannot overflow into a pass.
        assert_eq!(Reader::new(&bytes).bounded(u32::MAX, usize::MAX), None);
    }

    /// How long a section takes to draw, on the target it is built for.
    ///
    /// Ignored: it is a measurement, not a bound, and a shared CI runner cannot
    /// hold one. Run it with
    ///
    /// ```text
    /// cargo test -p rustdar-radar --release -- --ignored --nocapture section_timing
    /// ```
    #[test]
    #[ignore = "a measurement, not an assertion; see the doc comment"]
    fn section_timing() {
        let scan = scan_with(&|az, slant| Gate::Dbz(15.0 + az / 60.0 + slant / 30.0));
        let req = request(SITE, point_at(45.0, 230.0));
        // One warm run so the measurement is not the first-touch page faults.
        let _ = render_section(&scan, &req, SITE.0, SITE.1).unwrap();
        let started = std::time::Instant::now();
        let runs = 10;
        for _ in 0..runs {
            let _ = render_section(&scan, &req, SITE.0, SITE.1).unwrap();
        }
        let each = started.elapsed() / runs;
        println!(
            "render_section: {each:?} per {SECTION_WIDTH}x{SECTION_HEIGHT} \
             section, {} rungs",
            LADDER.len(),
        );
    }
}
