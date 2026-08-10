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
    let prepared = crate::derive::prepare(volume.scan(), req.product, storm_motion_override)?;
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
            VolumeSampler::for_derived(crate::nyquist::Volume::new(scan, declared), req.product, slot)
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
mod tests;
