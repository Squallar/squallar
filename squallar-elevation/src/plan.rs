//! [`HeightPlan`]: which posts, over which footprint, off which tiles.
//!
//! A whole-box field is not enough. The 3D camera dollies to
//! `MIN_EYE_DISTANCE = 0.05` against a default standoff of about 1.94
//! (`squallar_egui::pane_content`, `volume_view::eye_distance_for_plan_scale`),
//! so the ground the pane shows shrinks by nearly forty times while the field
//! under it keeps the same post count: a 512-post grid over a 920 km box gives
//! about thirteen posts across the view at the zoom stop. The answer is to
//! re-fit the field over the camera's own footprint — **same topology, same
//! post count, finer spacing** — which is exactly the non-identity
//! `VolumeUniform::ground_box` the renderer already draws.
//!
//! # Where the ladder is
//!
//! [`HeightPlan::fit`] is written in the mould of
//! `squallar_device_profile::quality::VolumeQuality::fit`: a rung enum with a
//! [`PostRung::next_coarser`] and a loop, **total by construction** — the
//! ladder always answers, because the coarsest rung is returned rather than
//! refused. Four ceilings are walked down against, and the plan says which one
//! bound it ([`PlanLimit`]).
//!
//! `max_texture_dimension_2d` is a **runtime** ceiling and never a `cfg`.
//! `downlevel_webgl2_defaults()` guarantees only 2048, but `squallar_gpu`'s
//! `device_limits` lifts the 1D/2D/3D trio to the adapter's real figure through
//! `using_resolution` — Firefox reports 32768 on a real driver — and it is
//! readable before device creation. It arrives here as a plain `u32` for the
//! reason everything else does: this crate links inside the offload worker and
//! declares neither `wgpu` nor `squallar-device-profile`.
//!
//! # Reduce posts, never clamp the zoom
//!
//! Post spacing implies a tile zoom, and the archive has a deepest one. Where
//! the implied zoom would pass it the posts come down instead:
//! [`PlanLimit::Archive`], and 171 posts of real z11 rather than 512 posts of a
//! three-times oversample — fewer vertices, the same information, fewer tiles.
//! What that is enough for is [`HeightPlan::post_spacing_km`], which carries
//! the amended acceptance clause and the arithmetic behind the amendment.
//!
//! # Aspect
//!
//! Nothing here assumes a square. `squallar_radar::voxel::HalfExtentKm::clamped`
//! floors each axis at 10 km and then bounds the corner, so **1329 km x 20 km,
//! 66:1, is reachable**. The rung names the posts on the field's **longer**
//! axis and the shorter one is derived from the footprint's own aspect, so post
//! *spacing* comes out very nearly isotropic and neither axis collapses: the
//! short one is floored at [`MIN_POSTS_PER_AXIS`], which is the same 2 that
//! `squallar_volumetric`'s `upload_heights` refuses below.
//!
//! # The frame thread
//!
//! [`HeightPlan::fit`] is not cheap: [`crate::cover_for`] walks the whole
//! boundary of the post grid, so one rung costs `2 * (posts.x + posts.y)`
//! forward geodetic solutions and the ladder may try several. That is fine in
//! the worker and a defect on the frame thread, so the two halves are
//! different types. [`HeightPlanner::observe`] is what a frame calls: it is
//! `O(1)`, it touches no projection, and all it can do is hand back a
//! [`FitRequest`], which is plain `Send` data for somebody else to resolve.
//! [`ledger`] counts fits so that "`observe` did not fit" is a measurement
//! rather than a sentence.
//!
//! The debounce is counted in **observations, never in wall-clock time**: this
//! repository has a documented recurring defect where a timing test red-gates
//! unrelated branches under load. See [`QUIET_OBSERVATIONS`].

use crate::resample::{ElevationError, TileCover, cover_for, post_geo};

/// The most posts one axis of a field may carry.
///
/// A 920 km box at 1 km posts is 921; 8192 is a post every 112 m over the same
/// box. [`crate::cover_for`]'s boundary walk is linear in this, so it also
/// bounds the work a later refusal can cost. `squallar_volumetric`'s
/// `MAX_POSTS_PER_AXIS` is the same number derived from the other end — the
/// width of the `u32` vertex count — and the two are deliberately not shared,
/// because this crate cannot declare that one.
pub const MAX_POSTS_PER_AXIS: u32 = 8192;

/// The fewest posts an axis may carry.
///
/// Two, and not one: `squallar_volumetric::raymarch::upload_heights` refuses a
/// field with an axis under two outright, so a plan that produced one would be
/// a plan that cannot be drawn. It is the floor the short axis of an extreme
/// aspect lands on.
pub const MIN_POSTS_PER_AXIS: u32 = 2;

/// The most tiles one plan may name.
///
/// **A page-budget ceiling, not a measurement.** The plan's resource table
/// allows a height job 8 MB of tile bodies for the job's duration, and a
/// 256 px Terrain-RGB tile runs about 120 KB at the archive's encoding, so 64
/// is about 7.7 MB — inside it, with the tail of a steep tile set in mind.
/// Callers pass their own; this is what a caller with no better figure should
/// use.
pub const DEFAULT_MAX_TILES: usize = 64;

/// Observations of the same footprint before a re-fit is handed off.
///
/// **Counted, never timed.** A debounce is a timing behaviour and the obvious
/// spelling is a `Duration`, but a wall-clock threshold makes every test that
/// depends on it a load-sensitive one, and this repository has two instances in
/// one night of exactly that. The property wanted is "the camera stopped
/// moving", and the countable form of it is "the footprint the camera implies
/// has not moved materially for this many frames".
///
/// Eight is about a seventh of a second at 60 Hz and about a quarter at 30 —
/// long enough that a flick does not fetch, short enough that it is not felt.
/// The number is a choice; that it is a **count** is not.
pub const QUIET_OBSERVATIONS: u32 = 8;

/// How much the wanted footprint may scale before it is a different footprint.
///
/// Hysteresis, so a camera resting a hair off the settled field does not
/// re-fetch. 1.25 linear is a 56% change in area.
const RESCALE_TOLERANCE: f64 = 1.25;

/// How far the wanted footprint's centre may move before it is a different
/// footprint, as a fraction of the settled footprint's own half-extent.
const SHIFT_TOLERANCE: f64 = 0.25;

/// A rectangle in the volume box's own kilometres, east and north of the site.
///
/// The same two ranges `HeightField` and [`crate::TerrainHeightJob`] carry, so
/// a footprint and the field fitted over it are registered by construction.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Footprint {
    /// East extent as `(low, high)` kilometres about the site.
    pub x_km: (f64, f64),
    /// North extent as `(low, high)` kilometres about the site.
    pub y_km: (f64, f64),
}

impl Footprint {
    /// East then north, in kilometres. Negative for an inverted rectangle.
    pub fn extent_km(&self) -> (f64, f64) {
        (self.x_km.1 - self.x_km.0, self.y_km.1 - self.y_km.0)
    }

    /// East then north, in kilometres.
    pub fn centre_km(&self) -> (f64, f64) {
        (
            0.5 * (self.x_km.0 + self.x_km.1),
            0.5 * (self.y_km.0 + self.y_km.1),
        )
    }

    /// Whether both extents are finite and above zero.
    pub fn is_drawable(&self) -> bool {
        let (ex, ey) = self.extent_km();
        ex.is_finite() && ey.is_finite() && ex > 0.0 && ey > 0.0
    }

    /// Where this footprint sits inside `box_footprint`, as the
    /// `VolumeUniform::ground_box` affine `(scale_x, scale_y, offset_x,
    /// offset_y)` over the drawn box's unit square.
    ///
    /// `None` for a box with no extent to divide. The identity `[1, 1, 0, 0]`
    /// comes out exactly when the two rectangles are the same, which is what
    /// every settled whole-box frame is.
    pub fn ground_box_in(&self, box_footprint: &Footprint) -> Option<[f32; 4]> {
        let (bx, by) = box_footprint.extent_km();
        if !(bx.is_finite() && by.is_finite()) || bx == 0.0 || by == 0.0 {
            return None;
        }
        let (ex, ey) = self.extent_km();
        Some([
            (ex / bx) as f32,
            (ey / by) as f32,
            ((self.x_km.0 - box_footprint.x_km.0) / bx) as f32,
            ((self.y_km.0 - box_footprint.y_km.0) / by) as f32,
        ])
    }

    /// This rectangle held inside `outer`, or `outer` itself when the two do
    /// not overlap or either is unusable.
    ///
    /// Total on purpose: a footprint is what a *fit* is run over, and the
    /// answer for "the camera is looking at something this box does not
    /// contain" is the whole box rather than a refusal.
    pub fn clipped_to(&self, outer: &Footprint) -> Footprint {
        if !self.is_drawable() || !outer.is_drawable() {
            return *outer;
        }
        let clipped = Footprint {
            x_km: (self.x_km.0.max(outer.x_km.0), self.x_km.1.min(outer.x_km.1)),
            y_km: (self.y_km.0.max(outer.y_km.0), self.y_km.1.min(outer.y_km.1)),
        };
        if clipped.is_drawable() {
            clipped
        } else {
            *outer
        }
    }

    /// Whether `other` is the same footprint to within the planner's
    /// hysteresis: neither axis rescaled past [`RESCALE_TOLERANCE`], and the
    /// centre moved less than [`SHIFT_TOLERANCE`] of this one's half-extent on
    /// either axis.
    fn is_materially(&self, other: &Footprint) -> bool {
        if !self.is_drawable() || !other.is_drawable() {
            return false;
        }
        let (ax, ay) = self.extent_km();
        let (bx, by) = other.extent_km();
        let within = |a: f64, b: f64| {
            let ratio = a / b;
            (1.0 / RESCALE_TOLERANCE..=RESCALE_TOLERANCE).contains(&ratio)
        };
        if !within(ax, bx) || !within(ay, by) {
            return false;
        }
        let (cx, cy) = self.centre_km();
        let (ox, oy) = other.centre_km();
        (cx - ox).abs() <= SHIFT_TOLERANCE * 0.5 * ax
            && (cy - oy).abs() <= SHIFT_TOLERANCE * 0.5 * ay
    }
}

/// Where the camera is looking, in terms a crate that links neither egui nor
/// wgpu can hold.
///
/// Every field is a plain number the 3D pane already has. `fov_y_deg` is passed
/// rather than restated here: `squallar_egui::volume_view` owns the one
/// definition of the field of view, and a copy of it in this crate would be a
/// second number that has to agree with the first.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CameraFootprint {
    /// The **drawn** box, which is what a field is placed inside.
    pub drawn: Footprint,
    /// Eye to pivot, kilometres — `OrbitCamera::eye_distance` multiplied by
    /// `volume_view::framing_radius_km`.
    pub distance_km: f64,
    /// The vertical field of view, degrees.
    pub fov_y_deg: f64,
    /// Viewport width over height.
    pub aspect: f64,
    /// Elevation above the horizontal, degrees.
    pub pitch_deg: f64,
    /// Azimuth about the vertical axis, degrees, clockwise from north — the
    /// convention `OrbitCamera::yaw_deg` and this crate's own bearings share.
    pub yaw_deg: f64,
    /// The look-at point, as a fraction of the drawn box's half-extent on the
    /// east and north axes.
    pub pivot: [f64; 2],
}

impl CameraFootprint {
    /// The ground this camera is looking at, clipped into the drawn box.
    ///
    /// **A conservative rectangle, not the frustum's exact ground
    /// intersection**, and the difference is worth stating because it is a
    /// choice rather than an approximation nobody noticed:
    ///
    /// * The exact intersection is **unbounded** whenever the horizon is in
    ///   frame, which at these pitches is most of the travel. A rule that has
    ///   to special-case infinity is a rule with an arm no fixture reaches.
    /// * A height field's posts are on a **box-aligned** grid, so whatever the
    ///   frustum cuts has to become an axis-aligned rectangle before it is any
    ///   use. Cutting the shape first and then bounding it recovers the same
    ///   rectangle for much more work.
    /// * It is **centred on the pivot**, so under an oblique camera the near
    ///   ground is over-covered and the far ground under-covered. That is the
    ///   deliberate half: the pivot is the point the user aimed at, and the
    ///   apron ring the ground mesh carries (`volume.wgsl`'s `box_axis`) draws
    ///   the rest of the box at the field's own rim height rather than leaving
    ///   a hole.
    ///
    /// The arithmetic: the viewport subtends `distance_km * tan(fov_y / 2)`
    /// of ground half-height at the pivot's depth and `aspect` times that
    /// across. Looking down at `pitch` the along-view extent opens out by
    /// `1 / sin(pitch)`, which the clip into the drawn box is what bounds at
    /// the horizon. Yaw then turns that rectangle on the ground and the
    /// axis-aligned bound of the turned rectangle is what a field can cover.
    ///
    /// Total: anything non-finite, and any camera whose rectangle does not meet
    /// the box, answers the whole drawn box.
    pub fn visible(&self) -> Footprint {
        let finite = self.distance_km.is_finite()
            && self.fov_y_deg.is_finite()
            && self.aspect.is_finite()
            && self.pitch_deg.is_finite()
            && self.yaw_deg.is_finite()
            && self.pivot.iter().all(|v| v.is_finite());
        if !finite || self.distance_km <= 0.0 || self.aspect <= 0.0 || !self.drawn.is_drawable() {
            return self.drawn;
        }
        let half_fov = 0.5 * self.fov_y_deg.to_radians();
        if !(half_fov > 0.0 && half_fov < std::f64::consts::FRAC_PI_2) {
            return self.drawn;
        }
        // Half the ground the viewport subtends at the pivot's own depth,
        // across the view and up it.
        let across = self.distance_km * half_fov.tan() * self.aspect;
        let up = self.distance_km * half_fov.tan();
        // Opened out by the ground's inclination to the view. At the horizon
        // this is infinite, and the clip below is what turns that into "the
        // whole box" rather than a special case.
        let along = up / self.pitch_deg.to_radians().sin().abs();
        let (sin_yaw, cos_yaw) = self.yaw_deg.to_radians().sin_cos();
        // The along axis lies on the bearing `yaw`, whose ground direction is
        // `(sin yaw, cos yaw)` east-north; the across axis is a right angle
        // from it. The axis-aligned bound of a rectangle turned by an angle is
        // the sum of each side's projection.
        let half_x = along * sin_yaw.abs() + across * cos_yaw.abs();
        let half_y = along * cos_yaw.abs() + across * sin_yaw.abs();
        if !half_x.is_finite() || !half_y.is_finite() {
            return self.drawn;
        }
        let (ex, ey) = self.drawn.extent_km();
        let (cx, cy) = self.drawn.centre_km();
        let centre = (cx + self.pivot[0] * 0.5 * ex, cy + self.pivot[1] * 0.5 * ey);
        Footprint {
            x_km: (centre.0 - half_x, centre.0 + half_x),
            y_km: (centre.1 - half_y, centre.1 + half_y),
        }
        .clipped_to(&self.drawn)
    }
}

/// How many **tile zoom levels** below the starting one the fit settled at.
///
/// Reported so a caller can see how far the tile ceiling pushed the field, and
/// carrying the linear divisor that works out to: one zoom shallower is twice
/// the ground per tile pixel, so half the posts and a quarter of the texels.
///
/// **A zoom step and not a blind halving of posts, and that is the whole of the
/// F1 fix.** Two earlier spellings were not monotone in the archive's depth —
/// a *deeper* archive answered a *coarser* field:
///
/// * absolute post counts per rung (2048, 1024, 512 …): stepping down for the
///   byte budget jumped past what the budget could afford, because the coarser
///   rung's implied zoom fell back under the archive ceiling and the clamp that
///   would have spent the rest stopped running. Measured at the shipped
///   ceilings: **25% coarser with 38% of the texture budget unspent**.
/// * halving the posts of a properly capped base: a base one post finer can tip
///   the *zoom* its spacing needs over a level boundary, which **quadruples**
///   the tile count, which halves the posts. Measured: z12 answered
///   `[1517, 172]` where z11 answered `[2958, 337]`.
///
/// A zoom step has neither failure. The tile count only changes at a zoom
/// boundary, so stepping zooms is stepping the tile count's own quantum; and
/// every rung below the first derives its posts from that zoom's pixel alone,
/// which makes them independent of the base and the whole answer monotone in
/// every ceiling. [`the_ladder_steps_the_tile_counts_own_quantum`] is the pin.
///
/// **Not an enum, and that was measured rather than preferred.** It was one —
/// six variants, `Full` through `ThirtySecond` — and a fixed arity is the wrong
/// shape for this search: the ladder starts at whatever zoom the base needs, so
/// six rungs from z11 stop at z6, and a fixture with a four-tile ceiling
/// wanted z5. The ladder ran out and returned a plan that **violated the very
/// ceiling it was searching for**, silently. The search space is the zoom
/// range, so the loop is bounded by the zoom and terminates at z0, where the
/// whole world is one tile.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PostRung {
    zooms_below: u8,
}

impl PostRung {
    /// The finest rung: no step at all, and where a fit starts. What it answers
    /// is the whole of whatever the closed-form ceilings allow.
    pub const FINEST: Self = Self { zooms_below: 0 };

    /// A rung `zooms_below` levels under the starting zoom.
    pub fn from_zooms_below(zooms_below: u8) -> Self {
        Self { zooms_below }
    }

    /// How many zoom levels below the starting one this rung is.
    pub fn zooms_below(self) -> u8 {
        self.zooms_below
    }

    /// How many times fewer posts on each axis, because one zoom level is twice
    /// the ground per tile pixel.
    ///
    /// Saturating: past 31 levels the divisor is past a `u32`, and no archive
    /// is that deep — but a saturating shift is a total answer where a plain
    /// one is a panic in debug and a wrap in release.
    pub fn linear_divisor(self) -> u32 {
        1u32.checked_shl(u32::from(self.zooms_below))
            .unwrap_or(u32::MAX)
    }

    /// The next rung down. `None` only past the width of the counter, which no
    /// archive reaches; the ladder's real floor is z0, and the loop in
    /// [`HeightPlan::fit`] is bounded by the zoom rather than by this.
    pub fn next_coarser(self) -> Option<Self> {
        self.zooms_below.checked_add(1).map(Self::from_zooms_below)
    }
}

/// What stopped the fit from being finer.
///
/// The plan says which ceiling bound it rather than only what it chose, because
/// "the field is coarse here" and "the archive has nothing finer here" are
/// different facts and only one of them is worth spending a budget on.
///
/// **There is no "nothing bound it" arm.** The fit starts from the adapter's
/// own post ceiling and comes down, so one of these is always the answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PlanLimit {
    /// The archive's deepest zoom. Posts came **down** to what that zoom
    /// actually resolves rather than the zoom being clamped under an
    /// oversampled grid.
    Archive,
    /// The height texture's byte budget.
    TextureBytes,
    /// The tile ceiling one fetch round may name.
    TileCount,
    /// `max_texture_dimension_2d`, or [`MAX_POSTS_PER_AXIS`] — whichever the
    /// caller's adapter made the smaller.
    ///
    /// **Where the fit starts.** The base is the aspect-preserving grid whose
    /// long axis is exactly [`HeightCeilings::post_ceiling`], so this is the
    /// answer whenever neither the archive nor the budgets cut into it — which
    /// is what an extreme aspect on a generous budget produces, and is why a
    /// 2048-post WebGL2 adapter and a 32768-post desktop one give different
    /// fields over a 66:1 box.
    TextureDimension,
}

/// The ceilings a plan is fitted inside.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct HeightCeilings {
    /// Bytes the height texture may occupy. Two per post — the field's own
    /// `u16` transport encoding is also its texture format
    /// (`squallar_volumetric::raymarch::HEIGHT_FORMAT` is `R16Uint`), so
    /// nothing is re-quantised between the two and one figure prices both.
    pub texture_bytes: usize,
    /// The most tiles the plan may name. See [`DEFAULT_MAX_TILES`].
    pub max_tiles: usize,
    /// The adapter's `max_texture_dimension_2d`. **A runtime figure**, read off
    /// the adapter before device creation; never a `cfg`.
    pub max_texture_dimension: u32,
    /// The archive's deepest zoom.
    pub max_zoom: u8,
    /// The archive's tile side, pixels.
    pub tile_px: u32,
}

impl HeightCeilings {
    /// The WebGL2 downlevel guarantee, which is the figure
    /// `downlevel_webgl2_defaults()` promises and the floor every browser
    /// clears. Spelled here so a test can show two ceilings differing in
    /// exactly one field without restating it.
    pub const WEBGL2_DOWNLEVEL_DIMENSION: u32 = 2048;

    /// Posts an axis may not pass: the adapter's own ceiling held under
    /// [`MAX_POSTS_PER_AXIS`], and never under [`MIN_POSTS_PER_AXIS`].
    pub fn post_ceiling(&self) -> u32 {
        self.max_texture_dimension
            .clamp(MIN_POSTS_PER_AXIS, MAX_POSTS_PER_AXIS)
    }
}

/// A fitted height field: what to ask the archive for, and over what.
#[derive(Clone, Debug, PartialEq)]
pub struct HeightPlan {
    /// `(latitude, longitude)` of the box's origin, degrees — the anchor the
    /// footprint's kilometres are measured from.
    pub site: (f64, f64),
    /// The field's own footprint, which is the camera's clipped into the box.
    pub footprint: Footprint,
    /// Posts along east and north, in that order.
    pub posts: [u32; 2],
    /// The tiles that cover it.
    pub cover: TileCover,
    /// The rung the ladder stopped on.
    ///
    /// **The ladder's position, not the post count.** Where the adapter's
    /// ceiling or the archive's zoom cut the posts, [`Self::posts`] is smaller
    /// than `rung.posts()` and `rung` still says which rung was being tried.
    /// The posts are the answer; this is how it was arrived at.
    pub rung: PostRung,
    /// What stopped it.
    pub limit: PlanLimit,
}

impl HeightPlan {
    /// Post spacing east then north, kilometres.
    ///
    /// **The plan's original "post spacing finer than a pixel at full zoom"
    /// clause is withdrawn, and this is what replaced it: per-axis isotropy,
    /// and no coarser than the source DEM.**
    ///
    /// The arithmetic that retired it, every figure reproduced twice. At the
    /// zoom stop a 920 km box shows 23.68 km down the pane; over a 1080-point
    /// pane that is **21.92 m to the point**. A tile pixel at 39°N is 59.34 m
    /// at z11, 29.67 m at z12 and 14.83 m at z13. So the clause reads as
    /// "fund z13".
    ///
    /// **It is not, and z13 would buy nothing.** The archive's source is
    /// Copernicus GLO-30, whose postings are about 30 m — which is z12's 29.67 m
    /// almost exactly. z13's 14.83 m is not more terrain, it is the same
    /// terrain interpolated twice as finely at roughly four times the storage:
    /// precisely the oversample [`zoom_for_spacing`]'s `ceil` exists to refuse
    /// on the fetch side. Three further reasons it would not have delivered
    /// what it promised:
    ///
    /// * it clears the clause only with **at least 5.25 MiB of height texture**
    ///   — 2.6x the plan's own budget row — and **at least 77 tiles**, past
    ///   [`DEFAULT_MAX_TILES`];
    /// * the 21.92 m figure is taken at 89 degrees of pitch, which is
    ///   `MAX_PITCH_DEG` and the single most favourable value. At an ordinary
    ///   25 degree orbit the footprint opens to 66.40 km and wants 3028 posts,
    ///   which the shipped ceilings do not reach at z13 either. (A review held
    ///   that this made the clause unreachable at *any* budget. That was true
    ///   of the absolute-post-count ladder F1 replaced, whose finest rung was
    ///   2048; it is not true now, and the argument against z13 rests on the
    ///   oversample above rather than on reachability.);
    /// * the clause says *pixel* while the arithmetic is in *points*. At two
    ///   device pixels to the point the requirement is 10.96 m, which is z14.
    ///
    /// **The amended clause, which this module does hold**: post spacing is
    /// equal on both axes to within the rounding to whole posts
    /// (`a_sixty_six_to_one_box_gets_isotropic_posts_and_no_degenerate_axis`),
    /// and never finer than the source DEM — which is what
    /// [`PlanLimit::Archive`] says out loud when it is the archive rather than
    /// a budget that bound the field.
    pub fn post_spacing_km(&self) -> (f64, f64) {
        let (ex, ey) = self.footprint.extent_km();
        (ex / f64::from(self.posts[0]), ey / f64::from(self.posts[1]))
    }

    /// Bytes the height texture this plan describes will occupy.
    pub fn texture_bytes(&self) -> usize {
        self.posts[0] as usize * self.posts[1] as usize * 2
    }

    /// The request this plan is, less the tile bodies the caller must fetch.
    ///
    /// Spelled here so a caller cannot re-derive the box from the camera and
    /// name a different one than the cover was computed for.
    pub fn request(&self, tiles: Vec<crate::HeightTile>) -> crate::TerrainHeightJob {
        crate::TerrainHeightJob {
            site: self.site,
            x_km: self.footprint.x_km,
            y_km: self.footprint.y_km,
            posts: self.posts,
            cover: self.cover,
            tiles,
        }
    }

    /// Fit a field over `footprint`: the finest posts every closed-form ceiling
    /// allows, stepped down the rung ladder until the tile count fits too.
    ///
    /// **Two mechanisms, and which ceiling gets which is the design.** Three of
    /// the four ceilings have a closed form — the adapter's post ceiling, the
    /// archive's deepest zoom, and the texture byte budget — so they are
    /// *solved* into a base rather than searched for, and the base is what
    /// makes the answer monotone in each of them. The tile count has no closed
    /// form: it depends on where the footprint falls against the tile grid, so
    /// it is the one thing the ladder searches.
    ///
    /// **Reduce posts, never clamp the zoom.** The zoom is derived from the
    /// *final* posts, after every cap, so the fetch is never an oversample —
    /// and the archive's own ceiling comes down on the posts
    /// ([`posts_for_spacing`]) rather than on the zoom.
    ///
    /// **Total over the ladder**: the coarsest rung is returned rather than
    /// refused, so there is no arm where a camera gets no plan. It is *not*
    /// total over geography — a footprint that crosses the antimeridian or runs
    /// off Web Mercator has no tiles at any rung, and that comes back as the
    /// [`ElevationError`] [`crate::cover_for`] raised, because no rung would
    /// have helped.
    ///
    /// **This is the expensive half.** One rung is a boundary walk of
    /// `2 * (posts.x + posts.y)` forward geodetic solutions, and the ladder may
    /// try several. Run it where the decode runs, never on the frame thread;
    /// [`HeightPlanner`] is the half a frame is allowed to call.
    pub fn fit(
        site: (f64, f64),
        footprint: Footprint,
        ceilings: HeightCeilings,
    ) -> Result<Self, ElevationError> {
        if !footprint.is_drawable() {
            return Err(ElevationError::Empty);
        }
        if ceilings.tile_px == 0 {
            return Err(ElevationError::Empty);
        }
        ledger::note_fit();
        // The footprint's own centre, through this crate's one forward
        // projection. Its latitude is what a tile pixel's ground size is
        // measured at.
        let (lat, _) = post_geo(site, footprint.x_km, footprint.y_km, [1, 1], 0, 0);
        let (base, base_limit) = affordable_at(footprint, ceilings, lat, ceilings.max_zoom);
        // The zoom the base's own spacing needs, which is where the ladder
        // starts. Never deeper than the archive has, and never deeper than the
        // posts can use — deriving it from the FINAL posts is what keeps the
        // fetch from being an oversample.
        let start_zoom = zoom_of(footprint, base, lat, ceilings).min(ceilings.max_zoom);
        // **Bounded by the zoom, not by a fixed number of rungs.** `zoom`
        // decreases by one every pass and stops at 0, where the whole world is
        // one tile, so the loop is total and always reaches a zoom that meets
        // the tile ceiling if any zoom does.
        let mut zoom = start_zoom;
        loop {
            let rung = PostRung::from_zooms_below(start_zoom - zoom);
            // **Every rung below the first derives its posts from that zoom's
            // own pixel and not from the base.** That is what makes the answer
            // monotone: a base one post finer cannot push the search onto a
            // different ladder, only onto one more rung at the top of the same
            // one.
            let posts = if rung == PostRung::FINEST {
                base
            } else {
                affordable_at(footprint, ceilings, lat, zoom).0
            };
            let cover = cover_for(
                site,
                footprint.x_km,
                footprint.y_km,
                posts,
                zoom,
                ceilings.tile_px,
            )?;
            let limit = if rung == PostRung::FINEST {
                base_limit
            } else {
                PlanLimit::TileCount
            };
            let plan = HeightPlan {
                site,
                footprint,
                posts,
                cover,
                rung,
                limit,
            };
            if plan.cover.len() <= ceilings.max_tiles {
                return Ok(plan);
            }
            // z0 is the bottom, and it is the answer whether or not it cleared
            // the ceiling: the whole world is one tile there, so what is left
            // unmet is a `max_tiles` of zero. A plan is still better than a
            // pane with no ground, and `limit` says the tile count was the
            // thing still unsatisfied.
            if zoom == 0 {
                return Ok(HeightPlan {
                    limit: PlanLimit::TileCount,
                    ..plan
                });
            }
            zoom -= 1;
        }
    }
}

/// The zoom `posts` over `footprint` need: the shallower of the two axes'
/// spacings, which is the finer one.
fn zoom_of(footprint: Footprint, posts: [u32; 2], lat_deg: f64, ceilings: HeightCeilings) -> u8 {
    let (ex, ey) = footprint.extent_km();
    let spacing = (ex / f64::from(posts[0])).min(ey / f64::from(posts[1]));
    zoom_for_spacing(spacing, lat_deg, ceilings.tile_px)
}

/// The finest posts every **closed-form** ceiling allows if the deepest tile
/// zoom available is `zoom`, and which ceiling was tightest.
///
/// Solved rather than searched, and in this order: the adapter's post ceiling
/// is where the ask starts, `zoom`'s own pixel cuts it to what the archive
/// actually resolves, and the byte budget scales whatever is left. Each step
/// only ever reduces, so the answer is **monotone in every ceiling** — raise
/// one and the field cannot get coarser.
///
/// `zoom` is a parameter rather than `ceilings.max_zoom` because the tile
/// ladder walks zoom levels: every rung below the first asks this same question
/// one level shallower, which is what makes those rungs independent of the base
/// and the whole fit monotone.
fn affordable_at(
    footprint: Footprint,
    ceilings: HeightCeilings,
    lat_deg: f64,
    zoom: u8,
) -> ([u32; 2], PlanLimit) {
    let ceiling = ceilings.post_ceiling();

    // **Three candidate grids, each solved on its own, and the answer is the
    // elementwise smallest.** Not "apply one then scale by the next": that
    // makes each cap's rounding depend on what the previous one produced, and
    // a `floor` over two slightly different inputs can land a post apart —
    // which is exactly how the last residue of F1's non-monotonicity survived
    // two rewrites (z13 answering `[1714, 611]` against z12's `[1715, 611]`).
    // Three independent closed forms and a `min` is monotone in each ceiling
    // by construction, with no rounding to reason about.
    //
    // The adapter's own post ceiling is where the ask starts.
    let dimension = posts_at_ceiling(footprint, ceiling);
    // **The archive: no post finer than the deepest zoom's own pixel.** Asking
    // for more is asking the resampler to invent detail — it costs vertices and
    // tiles and carries no more information. 171 posts of real z11 beats 512 of
    // a threefold oversample.
    // Held at the CRATE's ceiling, not the adapter's: these two are compared
    // against `dimension` to decide which one to report, and clamping them at
    // the same number would make all three equal whenever the adapter is the
    // tightest — reporting `Archive` for a field the adapter bound.
    let archive = posts_for_spacing(
        footprint,
        tile_pixel_km(zoom, lat_deg, ceilings.tile_px),
        MAX_POSTS_PER_AXIS,
    );
    // The finest grid of the footprint's own aspect that fits the budget.
    let budget = posts_for_bytes(footprint, ceilings.texture_bytes, MAX_POSTS_PER_AXIS);

    let posts = [
        dimension[0].min(archive[0]).min(budget[0]),
        dimension[1].min(archive[1]).min(budget[1]),
    ];
    // Which one was tightest, on the axis that binds. Ties go to the cheaper
    // story to tell: the archive having nothing finer is a different fact from
    // a budget being spent, and only one of them is worth buying more of.
    let limit = if archive[0] <= budget[0] && archive[0] <= dimension[0] {
        PlanLimit::Archive
    } else if budget[0] <= dimension[0] {
        PlanLimit::TextureBytes
    } else {
        PlanLimit::TextureDimension
    };
    (posts, limit)
}

/// The aspect-preserving grid whose **long** axis is exactly `ceiling`.
///
/// The long axis rather than a nominated one, so an extreme aspect comes out
/// with isotropic spacing instead of a degenerate texture: a 66:1 footprint at
/// a 8192-post ceiling is 8192 by 123, not 8192 by 8192 refused nor 123 by 8192
/// wasted.
///
/// Both axes floored at [`MIN_POSTS_PER_AXIS`] and held under `ceiling`.
/// **The floor is what does the work; the derived axis's ceiling is
/// unreachable.** `short` is called with the shorter extent over the longer, so
/// its ratio is at most one and its answer is at most `long`, which is already
/// held under `ceiling` on the line above. A mutation run that replaced the
/// derived axis's `clamp` with a bare `max` killed no test, and that is written
/// down rather than left to be found. The floor is live: a 66:1 footprint under
/// a small ceiling asks for a fraction of a post on its short axis, and zero
/// posts is a texture `upload_heights` refuses.
fn posts_at_ceiling(footprint: Footprint, ceiling: u32) -> [u32; 2] {
    let (ex, ey) = footprint.extent_km();
    let long = ceiling.max(MIN_POSTS_PER_AXIS);
    let short = |ratio: f64| {
        let scaled = (f64::from(long) * ratio).round();
        if !scaled.is_finite() {
            return MIN_POSTS_PER_AXIS;
        }
        (scaled as i64).clamp(i64::from(MIN_POSTS_PER_AXIS), i64::from(ceiling)) as u32
    };
    if ex >= ey {
        [long, short(ey / ex)]
    } else {
        [short(ex / ey), long]
    }
}

/// The finest grid of `footprint`'s own aspect whose height texture fits
/// `budget_bytes`.
///
/// Two bytes a post — the field's `u16` transport encoding is also its texture
/// format, so one figure prices both. With `posts_y / posts_x = ey / ex`,
/// `2 · posts_x · posts_y ≤ budget` solves to
/// `posts_x = sqrt(budget · ex / (2 · ey))`. The same square root
/// `squallar_device_profile::quality::shrink_into_budget` takes, and for the
/// same reason: the area shrinks by the square of the linear factor.
///
/// **Solved from the aspect alone, never from a post count handed in.** That
/// independence is the point — see [`affordable_at`] for the rounding this
/// removes. `floor`, so the answer never exceeds the budget by a rounding, and
/// floored at [`MIN_POSTS_PER_AXIS`], which is the one way it still can: a
/// budget under eight bytes buys nothing, and a 2 x 2 field is better than a
/// refusal.
fn posts_for_bytes(footprint: Footprint, budget_bytes: usize, ceiling: u32) -> [u32; 2] {
    let (ex, ey) = footprint.extent_km();
    let hold = |v: f64| {
        if !v.is_finite() {
            return ceiling;
        }
        (v.floor() as i64).clamp(i64::from(MIN_POSTS_PER_AXIS), i64::from(ceiling)) as u32
    };
    let posts_x = (budget_bytes as f64 * ex / (2.0 * ey)).sqrt();
    if !posts_x.is_finite() || posts_x <= 0.0 {
        return [MIN_POSTS_PER_AXIS; 2];
    }
    [hold(posts_x), hold(posts_x * ey / ex)]
}

/// Posts on each axis at a given ground spacing, held inside the same floor and
/// ceiling [`posts_at_ceiling`] uses.
///
/// The arm the archive's ceiling takes: the spacing is the tile pixel the
/// deepest zoom actually resolves, so the answer is however many posts that
/// spacing puts across the footprint.
fn posts_for_spacing(footprint: Footprint, spacing_km: f64, ceiling: u32) -> [u32; 2] {
    let axis = |extent: f64| {
        if !(spacing_km.is_finite() && spacing_km > 0.0) {
            return ceiling;
        }
        let want = (extent / spacing_km).round();
        if !want.is_finite() {
            return MIN_POSTS_PER_AXIS;
        }
        (want as i64).clamp(i64::from(MIN_POSTS_PER_AXIS), i64::from(ceiling)) as u32
    };
    let (ex, ey) = footprint.extent_km();
    [axis(ex), axis(ey)]
}

/// Kilometres on the ground one tile pixel spans at `zoom` and `lat_deg`.
///
/// On [`squallar_geo::EARTH_RADIUS_KM`], because that is the sphere
/// [`crate::post_geo`] walks and [`crate::cover_for`]'s `global_px` projects
/// onto; a second radius here would be a second planet.
fn tile_pixel_km(zoom: u8, lat_deg: f64, tile_px: u32) -> f64 {
    let world_px = f64::from(tile_px) * 2f64.powi(i32::from(zoom));
    let circumference_km = 2.0 * std::f64::consts::PI * squallar_geo::EARTH_RADIUS_KM;
    circumference_km * lat_deg.to_radians().cos().abs() / world_px
}

/// The shallowest zoom whose tile pixel is no coarser than `spacing_km`.
///
/// `ceil`, never `round`: a post interpolated up from a coarser pixel is a post
/// carrying detail the archive does not have, which is the oversample this
/// module exists to refuse in the other direction. Saturates at `u8::MAX`, so a
/// spacing of nothing answers a zoom the archive will refuse rather than
/// wrapping to zero.
fn zoom_for_spacing(spacing_km: f64, lat_deg: f64, tile_px: u32) -> u8 {
    if !(spacing_km.is_finite() && spacing_km > 0.0) || tile_px == 0 {
        return u8::MAX;
    }
    let circumference_km = 2.0 * std::f64::consts::PI * squallar_geo::EARTH_RADIUS_KM;
    let ratio =
        circumference_km * lat_deg.to_radians().cos().abs() / (f64::from(tile_px) * spacing_km);
    if !(ratio.is_finite() && ratio > 1.0) {
        return 0;
    }
    let zoom = ratio.log2().ceil();
    if zoom >= f64::from(u8::MAX) {
        u8::MAX
    } else {
        zoom as u8
    }
}

/// What [`HeightPlanner::observe`] hands off: everything a fit needs, and
/// nothing that has to stay on the frame thread.
///
/// Plain data, `Send` and `'static`, so it crosses to the offload pool or a
/// browser Worker unchanged. Resolving it is [`HeightPlan::fit`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FitRequest {
    pub site: (f64, f64),
    pub footprint: Footprint,
    pub ceilings: HeightCeilings,
}

impl FitRequest {
    /// Run the fit this request names. **Not on the frame thread** — see
    /// [`HeightPlan::fit`].
    pub fn resolve(&self) -> Result<HeightPlan, ElevationError> {
        HeightPlan::fit(self.site, self.footprint, self.ceilings)
    }
}

/// The debounce: what a frame is allowed to call.
///
/// `O(1)` per observation and no projection anywhere in it — [`ledger`] is how
/// that is a measurement rather than a claim. It holds three facts: the
/// footprint the field on screen was fitted for, the footprint the camera has
/// been asking for, and how many observations in a row it has asked for it.
///
/// **In flight is a latch, not a queue.** One fit is out at a time, because a
/// second request for a footprint the camera has already left is work whose
/// answer will be discarded. [`HeightPlanner::landed`] is what clears it.
#[derive(Clone, Debug, Default)]
pub struct HeightPlanner {
    settled: Option<Footprint>,
    pending: Option<Footprint>,
    quiet: u32,
    in_flight: bool,
}

impl HeightPlanner {
    pub fn new() -> Self {
        Self::default()
    }

    /// The camera wants a field over `want`. Answers a request exactly once,
    /// on the [`QUIET_OBSERVATIONS`]th consecutive observation of a footprint
    /// materially different from the settled one.
    ///
    /// **Frame-thread safe by construction**: a few comparisons of eight
    /// `f64`s, no allocation, no geodesy, no fit.
    pub fn observe(
        &mut self,
        site: (f64, f64),
        want: Footprint,
        ceilings: HeightCeilings,
    ) -> Option<FitRequest> {
        if !want.is_drawable() {
            return None;
        }
        // Already showing this. Not "close enough to ignore" — close enough
        // that re-fitting would replace the field with an indistinguishable
        // one, at the cost of a fetch round and a visible swap.
        if self.settled.is_some_and(|s| s.is_materially(&want)) {
            self.pending = None;
            self.quiet = 0;
            return None;
        }
        match self.pending {
            Some(pending) if pending.is_materially(&want) => self.quiet += 1,
            _ => {
                self.pending = Some(want);
                self.quiet = 0;
                return None;
            }
        }
        if self.quiet < QUIET_OBSERVATIONS || self.in_flight {
            return None;
        }
        self.in_flight = true;
        Some(FitRequest {
            site,
            footprint: want,
            ceilings,
        })
    }

    /// A field over `footprint` is now the one on screen.
    pub fn landed(&mut self, footprint: Footprint) {
        self.settled = Some(footprint);
        self.pending = None;
        self.quiet = 0;
        self.in_flight = false;
    }

    /// The fit that was out came back with nothing. Clears the latch without
    /// claiming a field.
    ///
    /// The `quiet` counter is **not** reset, so a camera that has not moved
    /// re-asks on the next observation rather than waiting out another eight —
    /// a failed round should not be slower to retry than a fresh one.
    pub fn abandoned(&mut self) {
        self.in_flight = false;
    }

    /// The footprint the field on screen was fitted for.
    pub fn settled_footprint(&self) -> Option<Footprint> {
        self.settled
    }

    /// Whether a fit is out and unanswered.
    pub fn is_fitting(&self) -> bool {
        self.in_flight
    }

    /// Consecutive observations of the pending footprint.
    pub fn quiet_observations(&self) -> u32 {
        self.quiet
    }
}

/// Always-on counters for the one property that cannot be read off the types:
/// that the frame thread does not fit.
///
/// The same idiom as `squallar_egui::overlay_cache::ledger` — counters that
/// cost nothing and are reported whether or not anything gates on them.
/// `HeightPlanner::observe` is `O(1)` *by inspection*, which is exactly the
/// kind of claim that turns out to be false three refactors later.
///
/// **Two counters, and the per-thread one is the one the property is about.**
/// "No fit lands on the frame thread" is a statement about a *thread*, and a
/// process-global total cannot express it: a test asserting the global is
/// unchanged fails the moment any other thread fits, which under
/// `cargo test`'s own parallelism is immediately — measured, as the first
/// spelling of this. [`fits_here`] is what a frame-thread assertion reads;
/// [`fits`] is the reportable total.
pub mod ledger {
    use std::cell::Cell;
    use std::sync::atomic::{AtomicU64, Ordering};

    static FITS: AtomicU64 = AtomicU64::new(0);

    thread_local! {
        static FITS_HERE: Cell<u64> = const { Cell::new(0) };
    }

    pub(super) fn note_fit() {
        FITS.fetch_add(1, Ordering::Relaxed);
        FITS_HERE.with(|n| n.set(n.get() + 1));
    }

    /// Fits run anywhere in this process since it started.
    pub fn fits() -> u64 {
        FITS.load(Ordering::Relaxed)
    }

    /// Fits run **on the calling thread**. Zero on a frame thread is the
    /// property; anything else is a fit that got onto it.
    pub fn fits_here() -> u64 {
        FITS_HERE.with(Cell::get)
    }
}

#[path = "plan/tests.rs"]
#[cfg(test)]
mod tests;
