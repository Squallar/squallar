//! A 3D pane's region: the one gesture that picks one, and the two things a
//! picked or defaulted region then implies — the viewport its floor is drawn
//! through, and the zoom gesture that must not touch either.
//!
//! [`RegionDrag`] is the **only** writer of a region in this program, and it
//! writes one exactly once per completed drag. Everything else here runs
//! **from** the region rather than towards it: [`viewport_for_region`] frames
//! the floor strip on the box; [`zoom_camera`] spends the wheel on the eye.
//! Neither of those two writes a region, and that separation is the subject of
//! the section below.
//!
//! # The region is stored, and only a deliberate gesture writes it
//!
//! A 3D pane resamples a stored patch of ground: the volume's own data reach,
//! circumscribed — the whole ring — or a smaller region the user picked. It
//! changes when the site, the product, the reach or the selection changes, and
//! at no other time. Not on a zoom, not on a pan, not on a divider drag, not on
//! a window resize. See [`crate::pane::VolumeRegion`], which is where it now
//! lives, and [`rustdar_radar::voxel::box_half_width_km`], which answers the
//! unselected case from the reach.
//!
//! "The selection changes" is [`RegionDrag`] and nothing else, which is what
//! keeps that list short: a writer reachable only from an armed mode, a
//! deliberate drag and a release cannot be reached by a gesture that meant to
//! do something else. The old derivation was the opposite — it was reached by
//! *every* frame — and that is why zoom, pan, divider drags and window resizes
//! were all on the list at once.
//!
//! It used to be **derived**, every frame, from the pane's own viewport, and
//! that is the defect this module was rebuilt to remove. Deriving it made the
//! gesture that frames the picture also re-cut the data under it, so the ground
//! the pane covered shrank as the user zoomed in — reported three times:
//!
//! > the goddamn 3d viewer still covers less and less 3d geometry
//!
//! > The 3d viewer's region should CAP at either the size of the data in the
//! > radar scan, or the region selected if the user did that. That region (the
//! > selector OR the radar's ring) must never change. Zooming should keep the
//! > rest of the region around and merely zoom into what's already there.
//!
//! On the reported session the pane's own caption stated the loss: `802 × 490
//! km box` as opened, `668 × 408 km box` after a zoom. That is
//! Chattanooga-to-Jacksonville reduced to Dalton-to-Wilmington. The box was not
//! *cropped* by the zoom — which would at least have been a visible edge — it
//! was rebuilt smaller, so the storms outside the new box stopped being
//! resampled at all and the picture lost them silently.
//!
//! # What the gesture does instead
//!
//! It divides [`OrbitCamera::eye_distance`](crate::pane::OrbitCamera::eye_distance).
//! That value is a multiple of the box's framing radius, so halving it halves
//! the ground the pane is *looking at* while the box, the grid inside it and
//! the floor under it all stand exactly still. The rest of the region stays
//! around, off the edges of the frame, which is the plain reading of the ask.
//!
//! Nothing else in the pane is touched. In particular the pane's own
//! `walkers::MapMemory` is left alone, where the old arm wrote it on every
//! gesture frame: it is shared with the pane's plan view, so writing it here
//! also meant that flipping a pane to 3D, scrolling, and flipping back moved
//! the map the user had aimed.
//!
//! # Why the bound is the camera's and not the box's
//!
//! The old arm clamped the gesture against
//! [`rustdar_radar::voxel::MIN_HALF_WIDTH_KM`] and
//! [`rustdar_radar::voxel::MAX_HALF_DIAGONAL_KM`], because the gesture was
//! resizing the thing those constants bound. It no longer is, so the only
//! bound left is the one the camera has always carried:
//! [`crate::pane::MIN_EYE_DISTANCE`]`..=`[`crate::pane::MAX_EYE_DISTANCE`],
//! 0.05 to 8.0, applied by
//! [`OrbitCamera::nudge`](crate::pane::OrbitCamera::nudge). That is a range of
//! 160×, or **7.32 zoom levels**, from inside the box to well outside it — and
//! it is the honest bound, because what the gesture now runs out of is somewhere
//! to put the eye rather than ground to resample.
//!
//! It also removes the re-measure probe the old arm needed. That probe existed
//! because the box's floor was reached by a logarithm that is exact in a flat
//! projection and out by a fraction of a percent in Mercator, and at the tight
//! end that fraction decided whether a box was legal at all. A clamp on a
//! camera ratio has no projection in it and nothing to verify.

/// The armed region pick's yellow: the box in flight, the resolution hint over
/// its top edge, and the active pane's armed hint chip all paint in this one
/// colour — so the chip advertises exactly the box the drag will draw.
///
/// Deliberately **not** `ui_map`'s `SECTION_TRACK_COLOR`, 255,214,10:
/// the two armed modes are mutually exclusive and a user who armed the wrong
/// one has the chip's colour as the fastest way to notice. Warm for the reason
/// that constant gives — every overlay in the registry is a hazard colour or a
/// muted grey and the radar image spans the spectrum, so an arm has to stay
/// findable over a 70 dBZ core — but a paler, sandier warm than the section
/// track's saturated amber.
///
/// A *committed* region is drawn back on its map in a different colour: it is a
/// record, not the gesture. See `ui_map`'s `REGION_COMMITTED_COLOR`.
pub(crate) const REGION_ARM_COLOR: egui::Color32 = egui::Color32::from_rgb(255, 220, 120);

/// A region drag in flight.
///
/// Geographic, and converted on the press frame: a pixel anchor denotes
/// different ground after a mid-drag wheel zoom, and zoom is *not* suppressed
/// while armed even though pan is. The same argument
/// [`crate::pane::SectionLine`] makes at length, and the same one
/// [`crate::ui_input::ArmedDragGesture::Anchored`] states for the gesture that
/// feeds this.
///
/// Held on the `Gui` rather than on the pane because it is a property of the
/// *gesture*, and a gesture that started on one pane must not be inherited by
/// another when the layout changes under it — which is what [`Self::pane_idx`]
/// is checked for on every frame of the drag.
///
/// # Why it is square
///
/// A single half-width, applied to both axes on commit. The box **can** be a
/// rectangle — [`crate::pane::VolumeRegion`] carries a
/// [`HalfExtentKm`](rustdar_radar::voxel::HalfExtentKm) with an axis each, and
/// says at length why — so this is a choice about the *gesture*, not a
/// limitation of what it can produce.
///
/// It is square because the alternative that was tried is worse. Keying the
/// picked box on the **pane's aspect** is the obvious way to make a 3D view
/// fill its pane, and it puts the divider back on the list of things that
/// change the region: drag the divider between two panes and the aspect
/// changes, so the box changes, so the ground the pane resamples changes — the
/// exact defect the stored region exists to remove, reintroduced through a
/// different door. A square asks nothing about the pane it is drawn on, so
/// there is no pane geometry left for a region to depend on.
///
/// Squaring a *free* rectangle drag would be worse still: silently squaring a
/// user's drag reads as a bug the first time they drag a wide box and get a
/// tall one. So the square is drawn from the first frame — pressing sets the
/// centre and dragging sets the half-width, which is the shape of the request
/// made visible.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct RegionDrag {
    /// Which map pane the press landed on. A drag belongs to one pane for its
    /// whole life; the pointer leaving that pane's rect does not end it, because
    /// dragging past the edge of a pane to make a big box is ordinary.
    pane_idx: crate::pane::PaneId,
    /// The box's centre, fixed on the press frame and never revised.
    centre: crate::pane::GeoPoint,
    /// Half-width in kilometres as the pointer currently stands. Capped at the
    /// resampler's maximum on the way in — see [`Self::extend_to`] — but *not*
    /// held up to its minimum: a too-small drag is refused whole at commit
    /// rather than resized. Zero until the pointer moves.
    half_width_km: f64,
}

impl RegionDrag {
    /// Start a drag centred on `centre`.
    ///
    /// `None` for a press the projector could not place on Earth — which happens
    /// for a pane collapsed to nothing by a divider drag. Refused rather than
    /// clamped, because there is no nearest sensible patch of ground and
    /// `f64::clamp` propagates NaN.
    pub(crate) fn begin(
        pane_idx: crate::pane::PaneId,
        centre: crate::pane::GeoPoint,
    ) -> Option<Self> {
        centre.is_on_earth().then_some(Self {
            pane_idx,
            centre,
            half_width_km: 0.0,
        })
    }

    /// Which pane this drag belongs to.
    pub(crate) fn pane_idx(self) -> crate::pane::PaneId {
        self.pane_idx
    }

    /// The centre the press fixed.
    pub(crate) fn centre(self) -> crate::pane::GeoPoint {
        self.centre
    }

    /// Half-width as it currently stands, kilometres.
    pub(crate) fn half_width_km(self) -> f64 {
        self.half_width_km
    }

    /// Re-measure the half-width against a pointer now over `corner`.
    ///
    /// **Chebyshev, not Euclidean**: the half-width is the larger of the two
    /// axis distances, so the square's *edge* follows the pointer rather than its
    /// corner. Dragging straight right therefore grows the box at the rate the
    /// pointer moves, which is what makes the square read as being pulled out
    /// rather than as tracking something behind the cursor.
    ///
    /// A `corner` that is not on Earth leaves the drag exactly as it was. That is
    /// the same refusal [`Self::begin`] makes, and it matters more here: this runs
    /// every frame, so a single laundered NaN would stick for the rest of the
    /// drag.
    ///
    /// **The result is capped at the resampler's maximum** —
    /// [`MAX_HALF_WIDTH_KM`](rustdar_radar::voxel::MAX_HALF_WIDTH_KM), which is
    /// the square case of the corner bound [`VolumeRegion::new`] clamps to on
    /// commit. The preview box and its hint read this value straight off the
    /// drag, so without the cap a long drag would paint an ever-bigger square
    /// past 470 km and release the same box every time — what is drawn has to be
    /// what is resampled. The *minimum* is deliberately not applied here: a
    /// too-small drag is refused whole by [`Self::commit`] rather than resized,
    /// so the preview honestly shows the too-small square that is about to be
    /// discarded.
    ///
    /// [`VolumeRegion::new`]: crate::pane::VolumeRegion::new
    pub(crate) fn extend_to(&mut self, corner: crate::pane::GeoPoint) {
        if !corner.is_on_earth() {
            return;
        }
        // The codebase's real geodesy, and the same function the resampler
        // places the box's own corners with — not a flat approximation, which
        // is what `corners_for` is allowed to use because it only ever draws.
        let (bearing_deg, range_km) = rustdar_radar::beam::site_bearing_range_km(
            self.centre.lat,
            self.centre.lon,
            corner.lat,
            corner.lon,
        );
        let bearing = bearing_deg.to_radians();
        let east = (range_km * bearing.sin()).abs();
        let north = (range_km * bearing.cos()).abs();
        let half = east.max(north);
        if half.is_finite() {
            self.half_width_km = half.min(rustdar_radar::voxel::MAX_HALF_WIDTH_KM);
        }
    }

    /// The region this drag would commit, or `None` if it is too small to be one.
    ///
    /// The bar is the resampler's own [`MIN_HALF_WIDTH_KM`] rather than a pixel
    /// count, and that is the useful choice: a drag below it would be *clamped*
    /// up by `build_voxels`, so committing it would resample a box the user did
    /// not draw and the pane's own resolution readout would describe the wrong
    /// picture. Refusing instead means every committed region is one that will
    /// be honoured exactly.
    ///
    /// A **kilometre** bar rather than the section draw's
    /// [`MIN_SECTION_DRAG_PT`](crate::ui_input::MIN_SECTION_DRAG_PT), and the
    /// difference is not an oversight: a line is refused for being an
    /// accidental *tap*, which is a fact about the gesture and therefore about
    /// points on a screen; a box is refused for naming ground the resampler
    /// will not honour, which is a fact about kilometres. A 24-point drag at a
    /// continental zoom is a legitimate 300 km box, and a 24-point drag at
    /// street level is 200 m and is not.
    ///
    /// The mode stays armed when this answers `None` — that decision belongs to
    /// the caller, and it is stated here because it is the reason this returns
    /// an `Option` rather than clamping.
    ///
    /// [`MIN_HALF_WIDTH_KM`]: rustdar_radar::voxel::MIN_HALF_WIDTH_KM
    pub(crate) fn commit(self) -> Option<crate::pane::VolumeRegion> {
        (self.half_width_km >= rustdar_radar::voxel::MIN_HALF_WIDTH_KM)
            .then(|| {
                crate::pane::VolumeRegion::new(
                    self.centre,
                    rustdar_radar::voxel::HalfExtentKm::square(self.half_width_km),
                )
            })
            .flatten()
    }
}

/// A box's corners as geographic points, `(north-west, south-east)`.
///
/// For drawing only. A free function over a centre and an extent rather than a
/// method on [`crate::pane::VolumeRegion`], so that a *committed* region and
/// the preview of the drag that produced it are drawn by the same arithmetic —
/// two versions disagreeing by a pixel would be read as the commit having moved
/// the box.
///
/// # Why it takes both axes when the drag only ever makes squares
///
/// Because the committed box it also draws need not be one. A config can carry
/// a rectangle, and
/// [`HalfExtentKm::clamped`](rustdar_radar::voxel::HalfExtentKm::clamped)
/// scales both axes of an oversized ask by one factor rather than reshaping it,
/// so a region is a rectangle in general even though [`RegionDrag`] only
/// produces the square case. Taking the extent whole is also what stops the two
/// axes being transposed — the reason
/// [`HalfExtentKm`](rustdar_radar::voxel::HalfExtentKm) is a named struct
/// rather than two `f64`s.
///
/// The latitude conversion is the flat approximation named on
/// [`KM_PER_DEGREE_LAT`](rustdar_radar::types::KM_PER_DEGREE_LAT); the longitude
/// one divides by `cos(lat)` so the box is sized in *kilometres* rather than in
/// degrees, which is the whole point — a degree-square box drawn at 35°N would
/// be 22% wider than it is tall and would not be the box that gets resampled.
///
/// The approximation is worth at most a pixel of drawn edge and never a
/// kilometre of grid: [`RegionDrag::extend_to`] measures the drag itself with
/// [`rustdar_radar::beam::site_bearing_range_km`], the codebase's real geodesy,
/// and this is only ever asked where the resulting box goes on screen.
///
/// `None` at the poles, where `cos(lat)` is zero and every longitude is the same
/// place. No NEXRAD site is within 20° of one; the check is here because the
/// alternative is an infinity in a painter.
pub(crate) fn corners_for(
    centre: crate::pane::GeoPoint,
    half: rustdar_radar::voxel::HalfExtentKm,
) -> Option<(crate::pane::GeoPoint, crate::pane::GeoPoint)> {
    let d_lat = half.north_km / rustdar_radar::types::KM_PER_DEGREE_LAT;
    let cos_lat = centre.lat.to_radians().cos();
    if !(cos_lat.is_finite() && cos_lat.abs() > 1e-6) {
        return None;
    }
    let d_lon = half.east_km / (rustdar_radar::types::KM_PER_DEGREE_LAT * cos_lat);
    let nw = crate::pane::GeoPoint {
        lat: centre.lat + d_lat,
        lon: centre.lon - d_lon,
    };
    let se = crate::pane::GeoPoint {
        lat: centre.lat - d_lat,
        lon: centre.lon + d_lon,
    };
    (d_lat.is_finite() && d_lon.is_finite()).then_some((nw, se))
}

/// `walkers::Map::zoom_speed`'s default, which the plan view leaves alone.
///
/// Named here because this module has to apply the *same* number: "zoom" means
/// one thing in both render modes, and a second constant is how the two come to
/// disagree by a factor nobody notices until they compare two panes side by
/// side. If the plan view's arm ever calls `zoom_speed`, this follows it.
const ZOOM_SPEED: f64 = 2.0;

/// Points of wheel travel worth one Web Mercator zoom level.
///
/// **The whole of one notch, not one frame's share of it.**
///
/// It is 120 and not something tuned because it *is* the shipped feel at 60 Hz:
/// the frame-time multiplier this replaced worked out to
/// `smooth_scroll_delta.y / 120` exactly when a frame took 1/60 s. See
/// [`zoom_step`] for why that was only ever true at 60 Hz.
///
/// # What a notch is worth, per backend — and the 3× that is not this bug
///
/// 120 points is a browser notch. Chromium spells it `deltaY: 120` outright;
/// Firefox spells the same detent `deltaY: 6` in line mode and
/// [`crate::ui_input::normalize_wheel_units`] rescales it to 120. Both arrive
/// here as 120, so a web notch is one zoom level.
///
/// **Native is not 120.** winit reports a discrete wheel as
/// `MouseScrollDelta::LineDelta(0, ±1)`, `egui-winit` maps that to
/// `MouseWheelUnit::Line`, and egui multiplies by `line_scroll_speed` — 40.0
/// off the web against 8.0 on it. So a native notch reaches this function as
/// **40 points, or 0.333 zoom levels**, a third of what the same wheel does in
/// a browser. `normalize_wheel_units` is `cfg(target_arch = "wasm32")` at its
/// call site, so nothing rescales the native spelling.
///
/// That gap is real, measured, and **older than the frame-rate bug this
/// constant fixes** — it is a question about which feel is right for both
/// platforms, not about correctness, so it is recorded rather than quietly
/// decided. What this commit changes is its *status*: the line above now states
/// "120 points is one zoom level" as a contract, and one platform delivers a
/// third of it.
///
/// **Un-gating `normalize_wheel_units` is not the fix, and would make it
/// worse.** `ui_input::PX_PER_WHEEL_LINE` is 20.0 because *Firefox*
/// reports six lines per notch and 6 × 20 lands on Chromium's 120. Native winit
/// reports **one** line per notch, so the same constant applied natively gives
/// 1 × 20 = 20 points — half of today's 40, not the 120 that would close the
/// gap. Whatever closes it needs a per-backend scale, not the browser's number
/// applied to a different spelling of a notch.
const POINTS_PER_ZOOM_LEVEL: f64 = 120.0;

/// This frame's geography zoom, in zoom **levels**, from whatever device
/// produced one.
///
/// A restatement of `walkers::Map::zoom_delta` — deliberately, and it is the
/// only honest way to get "the same gesture means the same thing": a 3D pane's
/// map is drawn off screen, so `Map::handle_gestures` skips its zoom outright
/// (`ui.ui_contains_pointer()` is false down there) and no amount of
/// configuration will make walkers do this for us. So the arithmetic is
/// restated, once, beside the gesture it feeds — and pinned by
/// `a_scroll_moves_a_3d_pane_the_same_distance_it_moves_a_plan_view`, which
/// drives the two arms through the real UI and compares the answers rather than
/// comparing this function against a copy of itself.
///
/// The two branches are walkers' own, for the `zoom_with_ctrl(false)` the plan
/// view selects: `zoom_delta` carries a pinch or a ctrl-scroll, and a frame
/// with neither — which reports exactly 1.0 — falls back to the raw scroll.
///
/// # Why the frame time is not in here, where walkers has it
///
/// **It was, and it was the whole of a reported bug**:
///
/// > scrolling behavior has gotten choppy. When I'm zoomed out wide, a zoom
/// > moves a lot more than a close zoom, which is a tiny jump.
///
/// walkers multiplies the scroll by
/// `stable_dt.clamp(predicted_dt * 0.5, predicted_dt * 2.0)`, and this function
/// restated that faithfully. It reads like the frame-rate normalisation every
/// animation needs, and it is the exact opposite of one, because
/// `smooth_scroll_delta` **is not a rate**. egui does not hand a frame the
/// scroll that arrived during it: `input_state/wheel_state.rs::after_events`
/// holds an accumulator and releases `1 - 0.1^(dt/0.1)` of it per frame, a
/// low-pass filter whose output sums to its input *whatever the frame rate is*.
/// It is a quantity of points, already frame-rate independent, and multiplying
/// a quantity by a time makes the total ∝ the frame time.
///
/// Measured through a real `egui::Context`, one notch at the shipped
/// `predicted_dt` — which `egui-winit` never writes, so it is
/// `RawInput::default()`'s 60 Hz for the life of the process, and the clamp is
/// therefore a fixed `1/120 ..= 1/30`:
///
/// | frame rate | zoom levels per notch |
/// |---|---|
/// | 240 Hz | 0.50 |
/// | 120 Hz | 0.50 |
/// | 60 Hz | 1.00 |
/// | 30 Hz | 2.00 |
/// | 10 Hz | 2.00 |
/// | 3.5 Hz (web, alerts on) | 2.00 |
///
/// A **4× spread**, saturating at both clamp bounds, which is both halves of
/// the report at once. Zoomed out wide is when the app is slowest — more
/// overlay area, more features — so a notch there was 2.0 levels, a
/// quadrupling of scale in one click; zoomed in the frames are quick and the
/// same notch was 0.5 levels. The render cost was feeding back into the input.
///
/// So the frame time is gone and [`POINTS_PER_ZOOM_LEVEL`] stands in its place:
/// the 60 Hz row above, held at every frame rate. Pinned by
/// `a_notch_is_the_same_zoom_at_every_frame_rate`.
fn zoom_step(input: &egui::InputState) -> f64 {
    let mut delta = f64::from(input.zoom_delta());
    if delta == 1.0 {
        delta = 1.0 + f64::from(input.smooth_scroll_delta.y) / (POINTS_PER_ZOOM_LEVEL * ZOOM_SPEED);
    }
    (delta - 1.0) * ZOOM_SPEED
}

/// What walkers' own frame-time multiplier has to be divided by for a
/// `walkers::Map` to zoom the way [`zoom_step`] does.
///
/// The plan view reaches walkers' arithmetic rather than this module's, so
/// fixing [`zoom_step`] alone would fix the 3D pane and leave the main map with
/// the bug — and leave the two disagreeing by up to 4× as well, which is the
/// thing `a_scroll_moves_a_3d_pane_the_same_distance_it_moves_a_plan_view`
/// exists to refuse. walkers has no lever for this: `zoom_speed` scales the
/// combined delta, so it moves pinch and double-click zoom with it and cannot
/// be a function of the frame rate anyway.
///
/// What it does have is the input it reads. `smooth_scroll_delta` is a public
/// field that egui expects consumers to write — `ScrollArea` zeroes it to say
/// it took the scroll — so the correction goes there: scale the scroll by the
/// reciprocal of the multiplier walkers is about to apply, and the product is
/// [`POINTS_PER_ZOOM_LEVEL`]'s constant again.
///
/// # Why the finiteness test is not dead code
///
/// `f32::clamp` **propagates a NaN `self`** — it is written as two `if`s that a
/// NaN fails both of, so `NAN.clamp(0.00833, 0.03333)` is `NAN`, verified with
/// standalone `rustc` rather than argued from the docs. `stable_dt` is a
/// wall-clock difference the platform supplies, so that is a reachable input,
/// and without the test a NaN would be multiplied into `smooth_scroll_delta`
/// and from there into `MapMemory::zoom` — which is stored, so one bad frame
/// would poison the map until the pane was rebuilt. `applied > 0.0` covers the
/// other end: a `predicted_dt` of zero clamps to zero and would divide by it.
/// Both arms are pinned by `a_frame_with_unusable_timing_leaves_walkers_alone`.
///
/// A NaN `predicted_dt` is the one case not handled here, and deliberately:
/// `clamp` asserts `min <= max`, which a NaN bound fails, so it panics. walkers
/// runs the identical expression on the identical input a few microseconds
/// later, so that frame was going to panic either way; mirroring walkers
/// exactly is worth more than being the one place that survives it, because
/// the mirroring is what makes the cancellation exact.
fn wheel_rate_correction(input: &egui::InputState) -> f32 {
    // walkers' `Map::zoom_delta`, restated — the same restatement `zoom_step`
    // is, and drifts from walkers in the same test.
    let applied = input
        .stable_dt
        .clamp(input.predicted_dt * 0.5, input.predicted_dt * 2.0);
    // The frame time walkers' `/ 4.0` was calibrated at, and the one
    // `POINTS_PER_ZOOM_LEVEL` restates: `120 * (1/60) / 4 * 2` is one level.
    const NOMINAL_FRAME_S: f32 = 1.0 / 60.0;
    if applied.is_finite() && applied > 0.0 {
        NOMINAL_FRAME_S / applied
    } else {
        1.0
    }
}

/// Show a `walkers::Map` with the wheel held steady against the frame rate,
/// then hand egui its own scroll back.
///
/// **The map goes in the closure.** An earlier version returned an RAII guard
/// and told the caller to keep it alive, which is a rule the type system does
/// not enforce: `#[must_use]` catches a bare `steady_wheel(ctx);` statement and
/// does *not* catch `let _ = steady_wheel(ctx);` — that binding is rustc's own
/// suggested way to silence the lint, so the documented footgun was the one
/// shape the documented protection missed. Taking the closure makes the scope
/// the call itself, which cannot be got wrong.
///
/// See [`wheel_rate_correction`] for why the correction is applied to the input
/// rather than to walkers.
///
/// # Why it is scoped rather than applied once per frame
///
/// `smooth_scroll_delta` is what every `ScrollArea` in the app reads too, and a
/// `ScrollArea` takes it as points with no frame-time multiplier on it —
/// already correct, and scaling it would break list scrolling in the opposite
/// direction. Only the map wants this, so only the map gets it.
///
/// Restoring rather than zeroing keeps the blast radius at nothing: walkers has
/// never consumed the scroll it zooms with, so a wheel over a map pane goes on
/// reaching whatever else reads it, exactly as before. The restore is a `Drop`
/// so that a panic inside the closure cannot leave the frame's input scaled.
///
/// # What the caller must not change without reading this
///
/// walkers reads `smooth_scroll_delta` **twice**: the zoom at `map.rs:281`,
/// which is what this exists to correct, and pan-by-scroll at `map.rs:246`.
/// The second read is live only when `Map::panning` is on, and the plan view
/// passes `panning(false)` — so today the correction reaches the zoom and
/// nothing else. **Turning panning on would make a scaled scroll pan the map
/// too**, by the reciprocal of the frame time, which is a far worse bug than
/// the one this fixes. If panning is ever wanted, the correction has to move
/// off the shared field and onto something only the zoom reads.
///
/// # Why the plan view is not simply driven off [`zoom_step`] instead
///
/// That was the first design and it is blocked, not merely inconvenient.
/// walkers anchors its zoom on the cursor by rewriting `MapMemory::center_mode`
/// either side of the zoom, and `Center` and `AdjustedPosition` are both
/// private to walkers 0.56 — `MapMemory` exposes `zoom()` and `set_zoom()` and
/// no `zoom_by`, so a caller can move the zoom but cannot hold the ground under
/// the pointer still while doing it. Reimplementing cursor-anchored zoom
/// outside the crate means reimplementing the projection it anchors through.
///
/// **The real fix is upstream**: walkers should not multiply
/// `smooth_scroll_delta` by a frame time at all, for the reason [`zoom_step`]
/// gives. When that lands this function deletes, rather than becoming
/// permanent — it is a compensation for a specific upstream defect and it says
/// so on purpose.
pub(crate) fn steady_wheel<R>(ctx: &egui::Context, map: impl FnOnce() -> R) -> R {
    /// Puts egui's own scroll back, panic or not.
    struct Restore {
        ctx: egui::Context,
        y: f32,
    }
    impl Drop for Restore {
        fn drop(&mut self) {
            let y = self.y;
            self.ctx.input_mut(|input| input.smooth_scroll_delta.y = y);
            HELD.with(|held| held.set(false));
        }
    }

    // Nesting would apply `wheel_rate_correction` twice and square it — a 289 ms
    // frame would come out 300× over rather than right. Unreachable today (the
    // one call site is a leaf), so this is a `debug_assert` guarding a future
    // caller rather than a condition anyone has hit. Single-threaded on every
    // target: wasm32 has no threads and egui's UI pass is one thread anyway.
    debug_assert!(
        !HELD.with(std::cell::Cell::get),
        "steady_wheel is already open on this thread; nesting squares the correction",
    );
    HELD.with(|held| held.set(true));

    let _restore = Restore {
        ctx: ctx.clone(),
        y: ctx.input_mut(|input| {
            let before = input.smooth_scroll_delta.y;
            input.smooth_scroll_delta.y = before * wheel_rate_correction(input);
            before
        }),
    };
    map()
}

thread_local! {
    /// Whether a [`steady_wheel`] scope is already open on this thread.
    static HELD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// This frame's scroll or pinch as a multiplicative dolly for
/// [`OrbitDelta::zoom_factor`](crate::pane::OrbitDelta::zoom_factor), or `1.0`
/// — "the eye did not move" — for a frame with no gesture for this pane.
///
/// # Why the factor is two to the power of the step
///
/// [`zoom_step`] answers in Web Mercator **zoom levels**, and one level is a
/// factor of two of ground per point by the projection's own definition. The
/// ground a perspective camera sees at its pivot plane is `2 · d · tan(fov/2)`
/// — linear in the standoff `d` at a fixed field of view — so one level is one
/// halving of `eye_distance`, exactly. That is what makes a wheel notch on a 3D
/// pane cover the same ground it covers on the plan view beside it, and it is
/// derived from the two definitions rather than tuned to feel similar.
///
/// The neutral value is 1.0 rather than 0.0 because this is a *ratio*: it is
/// what `nudge` divides the standoff by, and `2^0 = 1` is the identity a frame
/// with no gesture must produce.
///
/// # Why the gate is correctness and not politeness
///
/// `Input::zoom_delta` and `smooth_scroll_delta` are **global**: they report the
/// frame's gesture wherever on screen it happened. Without `hovered() ||
/// dragged()` a pinch over a map pane would dolly every 3D pane on screen at
/// once. The topmost-layer check is the same rule `filter_dialog_blocked`
/// applies to clicks — a wheel over the timeline must work the timeline, not
/// the map under it — and its own ground is the `dragged()` arm, which keeps
/// the response resolving after the pointer has wandered onto the chrome.
///
/// # Why a non-finite step is refused rather than clamped
///
/// [`OrbitCamera::nudge`](crate::pane::OrbitCamera::nudge) refuses a whole
/// delta whose factor is not finite and positive, which is the right answer but
/// discards the frame's orbit and pan with it. Answering 1.0 here keeps those
/// two verbs working on a frame whose scroll arrived as a NaN — and a `2^step`
/// that overflowed to infinity is the one arithmetic step between `zoom_step`'s
/// own finiteness and the camera's.
pub(crate) fn zoom_camera(ctx: &egui::Context, response: &egui::Response) -> f32 {
    if !(response.hovered() || response.dragged()) || !pointer_on_map_layer(ctx) {
        return 1.0;
    }
    dolly_for_step(ctx.input(zoom_step))
}

/// [`zoom_camera`]'s arithmetic, without the gate: zoom levels in, a standoff
/// divisor out.
///
/// Split from the gate because the two fail in different ways and only one of
/// them can be tested without a live `egui::Context` — this half is the one
/// that has to answer 1.0 for every input a gesture can produce that the camera
/// would refuse, and it is exhaustively checkable as a function.
fn dolly_for_step(step: f64) -> f32 {
    if !step.is_finite() || step == 0.0 {
        return 1.0;
    }
    let factor = step.exp2() as f32;
    if factor.is_finite() && factor > 0.0 {
        factor
    } else {
        1.0
    }
}

/// How many passes [`viewport_for_region`] gets to settle, and the number the
/// loop really can stop short of.
///
/// The east–west lane converges in **one**: walkers' points per degree of
/// longitude is exactly `tile_size · 2^zoom / 360`, so the ratio of what is
/// covered to what is wanted is exactly a power of two away and the logarithm
/// lands on it. The north–south lane does not, because Web Mercator's scale
/// varies with latitude and the latitudes of the rect's own top and bottom edges
/// move as the zoom does — so the second pass measures a projection the first
/// pass changed. Which lane *binds* is therefore what decides the pass count,
/// and it is the box's shape against the strip's: a square box in a tall narrow
/// strip binds east and settles in two, the same box in a wide strip binds north
/// and has to iterate.
///
/// # The measurement
///
/// A sweep of every strip from 8 to 1400 points on each axis, every latitude
/// from 0° to 84°, and six boxes — the whole ring square, 300 km square, the
/// 10 km floor, the 664 × 10 km rectangle a config can carry, its transpose, and
/// an ordinary 120 × 75 — settling against [`COVERAGE_MARGIN`], worst case per
/// band:
///
/// | centre latitude | passes to settle |
/// |---|---|
/// | 0–49° | 4 |
/// | 50–59° | 5 |
/// | 60–69° | 6 |
/// | 70–79° | 7 |
/// | 80°+ | does not settle |
///
/// Eight is the 70–79° figure with one pass in hand. It is not chosen to cover
/// the last row, because nothing covers the last row: past about 80° the
/// single-logarithm step stops converging — 40 passes leaves the worst shapes no
/// closer than 8 does — so a bigger budget there buys arithmetic and no
/// coverage. Those latitudes are 15° past the northernmost radar the US network
/// has and are reachable only by panning a map to the Arctic and picking a
/// region there; the loop's answer is documented on [`viewport_for_region`] and
/// is still a strip framed on the box.
///
/// # Why this was four, and dead
///
/// It was four with a doc claiming "pass two is already inside `COVERAGE_MARGIN`
/// and the loop stops there; four is that with two passes to spare". Both halves
/// were wrong. The loop could not stop at all — its early-out compared the
/// margin-inflated shortfall against 1.0, a value the solve approaches from
/// above and never reaches, for the reason [`COVERAGE_MARGIN`] exists — so every
/// framing in the application ran exactly four passes. And the shape the figure
/// was measured on, a 450 × 900 point strip, is the *easy* one: it binds east
/// and settles in two. Turn it on its side and 700 × 450 at 64.8°N takes four,
/// which is the budget entire, with nothing spare.
///
/// [`the_solve_stops_as_soon_as_the_strip_covers_the_box`] holds the early-out
/// to firing, and [`the_framing_budget_covers_every_latitude_a_region_can_sit_at`]
/// re-measures the table above.
///
/// [`the_solve_stops_as_soon_as_the_strip_covers_the_box`]:
///     tests::the_solve_stops_as_soon_as_the_strip_covers_the_box
/// [`the_framing_budget_covers_every_latitude_a_region_can_sit_at`]:
///     tests::the_framing_budget_covers_every_latitude_a_region_can_sit_at
const MAX_FRAMING_PASSES: usize = 8;

/// How much more than the box the framing deliberately covers, as a fraction of
/// it.
///
/// **Not slack, and not a fudge — it is the direction the solve is wrong in.**
/// The single-logarithm step is exact for the east–west lane and asymptotic for
/// the north–south one, and measurement says it approaches from *above*: at
/// 64.8°N in a 450 × 900 point strip the shortfall runs 1.00098, 1.0000019,
/// 1.0000000038 — never crossing 1.0, so a solve aimed at exact coverage lands
/// fractionally **short** on every pass it will ever take. Short is the one
/// failure this function exists to prevent: `floor_colour` answers transparent
/// off the mirror, so a hairline short is a hairline of the volume standing on
/// nothing, right where the box's edge is.
///
/// The sequence above is what the arithmetic *would* run to; the loop stops on
/// the first of those terms, because this constant is also its settle bar. The
/// solve is aimed at the box plus this and stops when it has cleared the box
/// itself — [`MAX_FRAMING_PASSES`] for how that check is written and for why it
/// used to be one nothing could satisfy.
///
/// So the target is the box plus this, and the loop converges onto a viewport
/// that covers it. 0.1% of a 920 km box is 920 m — under a third of one cell at
/// the shipped grid's 3.6 km — which is the price of never being short.
const COVERAGE_MARGIN: f64 = 0.001;

/// The widest zoom `walkers::MapMemory::set_zoom` will accept: the whole world
/// in one 256-point tile.
///
/// Restated here rather than imported because walkers does not export it.
/// `Zoom::try_from` is
///
/// ```text
/// if !(0. ..=26.).contains(&value) { Err(InvalidZoom) } else { Ok(Self(value)) }
/// ```
///
/// — `walkers-0.56.0/src/zoom.rs:14` — and `Zoom` is `pub(crate)` to walkers, so
/// `set_zoom`'s `Err(InvalidZoom)` is the only way a caller can learn where the
/// bounds are. [`a_zoom_walkers_refuses_is_one_this_module_clamps_away`] drives
/// `set_zoom` across both ends and fails if walkers ever moves them, which is
/// what keeps this pair honest across a version bump.
///
/// **`RangeInclusive::contains` is false for a `NaN`**, so a `NaN` zoom is
/// refused too — and a `NaN` survives `f64::clamp` rather than being bounded
/// away by it. That is why [`viewport_for_region`]'s finiteness test comes
/// *before* the clamp and not after.
///
/// [`a_zoom_walkers_refuses_is_one_this_module_clamps_away`]:
///     tests::a_zoom_walkers_refuses_is_one_this_module_clamps_away
const MIN_ZOOM_LEVEL: f64 = 0.0;

/// The tightest zoom `walkers::MapMemory::set_zoom` will accept. See
/// [`MIN_ZOOM_LEVEL`] for where both numbers come from; walkers' own comment
/// calls this end "artificial".
///
/// Unreachable from [`viewport_for_region`]'s solve, which starts at
/// `MapMemory::default()`'s 16 and only ever zooms *out* — the step is
/// `-log2(shortfall)` and the loop has already refused any `shortfall` at or
/// below 1.0. It is applied anyway, because "the caller only moves one way" is
/// a property of today's loop rather than of walkers' contract.
const MAX_ZOOM_LEVEL: f64 = 26.0;

/// A map viewport that frames `rect` on the box of `half` about `centre` —
/// **the inverse of the measurement this module used to make**.
///
/// The floor strip is a real `walkers::Map` drawn off screen, and the volume
/// shader samples it as the ground under the box. So the strip has to be showing
/// the box: `floor_hit` clips the floor to the box's own bottom face, and
/// `floor_colour` clips it again to the mirror's `0..1`, transparent outside
/// rather than clamped. Those two rectangles used to be the same one because the
/// box *was* the viewport. Now the box is stored, so the viewport is derived —
/// the same coupling, with the causality the user asked for:
///
/// > the "stage"/"floor" should never get smaller (in real-world geography
/// > terms) / nor the data above it ofc
///
/// # Why the whole box, rather than the part of it on screen
///
/// **Because "the part on screen" is not expressible here.** `floor_colour` maps
/// the box's own unit square through `floor_geo` and `box_size_km` and samples
/// the mirror with the result; nothing in the uniform says where the camera is.
/// Sizing the mirror to the visible part would mean putting the camera in it and
/// re-rendering the strip on every frame the eye moved — a mirror re-render per
/// gesture frame, to save ground that costs nothing to carry.
///
/// And at the standoffs a pane is usually at there would be nothing to save. The
/// far edge of the frustum meets the ground this many box **half-widths** past
/// the box's centre — a ratio that depends only on the standoff and the pitch,
/// not on how big the box is:
///
/// | `eye_distance` | pitch 25° (default) | 45° | 89° (`MAX_PITCH_DEG`) |
/// |---|---|---|---|
/// | 0.05 (`MIN_EYE_DISTANCE`) | 0.28 | 0.06 | 0.03 |
/// | 1.94276 (a pane opens here) | 10.78 | 2.22 | 1.01 |
/// | 8.0 (`MAX_EYE_DISTANCE`) | 44.40 | 9.16 | 4.14 |
///
/// Below 20° — half the 40° vertical field of view — the ray never meets the
/// ground at all and the entry is unbounded.
///
/// So the box's edge *is* off screen at the tight end, and that is new: the zoom
/// gesture now moves the eye, where it used to shrink the box. It changes
/// nothing here. The mirror is sampled in box space whatever the camera is
/// doing, so a strip that covers less than the box is a strip with transparent
/// ground in it, waiting for the first orbit or dolly that brings that part of
/// the box into frame.
///
/// # Why it is measured rather than computed from the zoom
///
/// `walkers::Projector` is the projection the strip is actually drawn in, and
/// going through it rather than restating `tile_size · 2^zoom / 360` is what
/// keeps the two the same: a tile size or zoom convention that moves in
/// `walkers` moves both together. The passes are what that costs, and
/// [`MAX_FRAMING_PASSES`] measures it.
///
/// The result is deliberately *tight* — it stops as soon as the strip covers the
/// box rather than leaving it wide — because the mirror is a fixed number of
/// pixels, so every kilometre of ground outside the box is floor resolution
/// spent on ground the box will clip away.
///
/// # A box the strip cannot show is framed as wide as it can be, not abandoned
///
/// The solve's step is a zoom, and walkers refuses one outside
/// [`MIN_ZOOM_LEVEL`]`..=`[`MAX_ZOOM_LEVEL`]. A strip whose *shorter* side is a
/// few points across needs more than the 16 levels between
/// `MapMemory::default()` and the world-in-one-tile end to reach a
/// continental box, so the step it asks for is a **negative** zoom and walkers
/// says no. That is reachable rather than theoretical: with a whole-ring box the
/// bar is about 7 points at the equator and about 14 at 65°N, which a pane
/// collapsed by a divider drag, a web canvas in a shrunk container, or a frame
/// mid-resize all clear.
///
/// The step is therefore **clamped** into walkers' range and the solve stops
/// when the clamp leaves it nowhere to go. The strip that comes back is short of
/// the box, and says so by being the widest walkers can draw — but it is
/// centred on the box, which the caller's fallback is not: `floor_frame_for`
/// answers the pane's own map memory, at whatever zoom the user left their plan
/// view and centred on the *site* rather than on a picked region. At the
/// geometry that trips this the difference is the middle 374 km of a 470 km box
/// against a fraction of one kilometre of it, so returning the clamped framing
/// is not a consolation prize; it is most of the floor against almost none of
/// it.
///
/// This used to be `set_zoom(..).ok()?` — a refusal indistinguishable from a
/// box that was never framable, which is what put the caller on the fallback.
/// [`a_box_wider_than_the_strip_can_show_is_framed_as_wide_as_walkers_allows`]
/// drives the geometry that trips it, and asserts on the trip as well as on the
/// answer so that it cannot quietly stop being a trigger.
///
/// `None` for a rect with no area or an extent that is not finite and positive
/// — the two asks that have no framing at all. The caller falls back to the
/// pane's own map memory, which is what the strip was always drawn through.
///
/// [`a_box_wider_than_the_strip_can_show_is_framed_as_wide_as_walkers_allows`]:
///     tests::a_box_wider_than_the_strip_can_show_is_framed_as_wide_as_walkers_allows
pub(crate) fn viewport_for_region(
    rect: egui::Rect,
    centre: walkers::Position,
    half: rustdar_radar::voxel::HalfExtentKm,
) -> Option<walkers::MapMemory> {
    Some(solve_viewport(rect, centre, half)?.0)
}

/// [`viewport_for_region`]'s whole body, plus **how many passes it took**.
///
/// The count exists because [`MAX_FRAMING_PASSES`] is a claim about what the
/// loop does, and the only honest way to hold a loop to a claim about its own
/// iterations is to count the ones that ran. It used to be checked by a test
/// that re-ran the solve's arithmetic beside it and counted *that* — which is
/// how the early-out came to be dead for as long as it was: the copy in the test
/// used the settle condition the prose describes, the loop here used a different
/// one, and no run of either could notice.
///
/// It is a bare `usize` on a private tuple rather than a field on a named
/// struct so that there is nothing for the application to branch on: the count
/// is evidence about the solve, not a fact about the framing, and
/// [`viewport_for_region`] drops it on the way out.
fn solve_viewport(
    rect: egui::Rect,
    centre: walkers::Position,
    half: rustdar_radar::voxel::HalfExtentKm,
) -> Option<(walkers::MapMemory, usize)> {
    if !(rect.width() > 0.0 && rect.height() > 0.0) {
        return None;
    }
    if !(half.is_finite() && half.east_km > 0.0 && half.north_km > 0.0) {
        return None;
    }

    // The box plus the margin: what the strip is actually solved onto. See
    // `COVERAGE_MARGIN` — the solve approaches from above, so aiming at the box
    // itself lands short of it every time.
    let want = rustdar_radar::voxel::HalfExtentKm {
        east_km: half.east_km * (1.0 + COVERAGE_MARGIN),
        north_km: half.north_km * (1.0 + COVERAGE_MARGIN),
    };

    let mut memory = walkers::MapMemory::default();
    for pass in 1..=MAX_FRAMING_PASSES {
        let covered = ground_half_extent(rect, &memory, centre)?;
        // How much wider the target is than what the strip currently shows, on
        // whichever axis is the binding one. Above 1.0 the strip is short.
        let shortfall = (want.east_km / covered.east_km).max(want.north_km / covered.north_km);
        if !shortfall.is_finite() || shortfall <= 0.0 {
            return None;
        }
        // Settled once the strip covers the **box** — the margin is what is
        // being converged *through*, not what has to be reached exactly, so the
        // bar is the margin and not 1.0. `shortfall` is measured against `want`,
        // which is the box scaled by `1 + COVERAGE_MARGIN`, so "covers the box"
        // is `shortfall / (1 + COVERAGE_MARGIN) <= 1` and this is that with the
        // division folded into the constant.
        if shortfall <= 1.0 + COVERAGE_MARGIN {
            return Some((memory, pass));
        }
        // Ground per point halves with every zoom level, so the zoom that
        // covers the target is one logarithm away rather than a search — held
        // inside the range walkers will accept, for the reason above the
        // signature. `shortfall` is finite and positive and `memory.zoom()` is
        // in range, so this is finite and the clamp therefore bounds it.
        let target = (memory.zoom() - shortfall.log2()).clamp(MIN_ZOOM_LEVEL, MAX_ZOOM_LEVEL);
        // The clamp bit, and there is nowhere left to go: the next pass would
        // measure the same projection and ask for the same refused zoom. So the
        // widest strip walkers can draw is the answer, and it is centred on the
        // box.
        if target == memory.zoom() {
            return Some((memory, pass));
        }
        // Unreachable: `target` is finite and inside the range walkers checks,
        // which is the whole of `Zoom::try_from`. Handled rather than unwrapped
        // because a panic in a paint is not a proportionate answer to walkers
        // moving its bounds — and keeping the framing already solved is the same
        // degradation the clamp above makes, for the same reason.
        if memory.set_zoom(target).is_err() {
            return Some((memory, pass));
        }
    }
    // Out of passes without the strip ever measuring as covering. Reachable
    // only past about 80° of latitude, where the single-logarithm step stops
    // converging at all — see `MAX_FRAMING_PASSES`. The answer is still framed
    // on the box and still biased outward: the loop's last act was a step, so
    // this memory is one whole measured shortfall wider than the last thing
    // measured short. Answered rather than refused, because the alternative is
    // the pane's own map memory, which is not framed on the box at all.
    Some((memory, MAX_FRAMING_PASSES))
}

/// The ground `rect` covers either side of `centre`, kilometres on each
/// horizontal axis, through the projection the strip is drawn in.
///
/// **Each axis takes the nearer of its own two edges.** Mercator makes the
/// poleward edge the near one, so the north–south lane is governed by the top
/// edge in the northern hemisphere and the bottom in the southern — and the
/// near edge is the binding one for *coverage*, because a box that reaches past
/// it is a box with a transparent strip along that side.
///
/// [`rustdar_radar::beam::site_bearing_range_km`] rather than a flat
/// approximation, because it is the codebase's real geodesy and the same
/// function the resampler places the box's own corners with.
///
/// `pub(crate)` for the tests in `ui_map` rather than for any caller: it is the
/// instrument the coverage property is measured with, and the end-to-end pin —
/// that a **picked** region's floor is framed on that region rather than on the
/// site — has to measure the same way this module's own unit sweep does, or the
/// two could pass while disagreeing about what "covers" means.
pub(crate) fn ground_half_extent(
    rect: egui::Rect,
    map_memory: &walkers::MapMemory,
    centre: walkers::Position,
) -> Option<rustdar_radar::voxel::HalfExtentKm> {
    let projector = walkers::Projector::new(rect, map_memory, centre);
    let ground_km = |pos: egui::Pos2| {
        let point = projector.unproject(pos.to_vec2());
        let (_, range_km) = rustdar_radar::beam::site_bearing_range_km(
            centre.y(),
            centre.x(),
            point.y(),
            point.x(),
        );
        (range_km.is_finite() && range_km > 0.0).then_some(range_km)
    };
    Some(rustdar_radar::voxel::HalfExtentKm {
        east_km: ground_km(egui::pos2(rect.left(), rect.center().y))?
            .min(ground_km(egui::pos2(rect.right(), rect.center().y))?),
        north_km: ground_km(egui::pos2(rect.center().x, rect.top()))?
            .min(ground_km(egui::pos2(rect.center().x, rect.bottom()))?),
    })
}

/// Whether the pointer is over the map rather than over floating chrome.
///
/// The pane-rect gate alone stopped being enough at the full-bleed flip: pane
/// rects now run under the timeline, the status bar and the layers panel, so
/// "the pointer is over this pane" no longer implies "the pointer is over the
/// *map*". A position covered by any layer above `Background` belongs to that
/// layer, and a wheel there must work the chrome, not the geography under it.
///
/// No pointer at all answers `false`, which is the conservative half of the
/// pair: a frame that cannot say where the gesture happened cannot say it
/// happened over the map.
pub(crate) fn pointer_on_map_layer(ctx: &egui::Context) -> bool {
    ctx.pointer_latest_pos().is_some_and(|pos| {
        !ctx.layer_id_at(pos)
            .is_some_and(|l| l.order > egui::Order::Background)
    })
}

#[cfg(test)]
mod tests;
