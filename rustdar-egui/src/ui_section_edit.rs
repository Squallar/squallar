//! Grabbing a committed cross-section line — by an end to re-aim it, by its
//! body to slide or sweep it — and re-cutting only on the drop.
//!
//! # The shape of the interaction, and why each part of it is the way it is
//!
//! A committed section's ground track already draws its two endpoints as
//! labelled caps. This module makes the whole track **grabbable**: press a cap
//! and drag to move that end; press the line between them and drag to slide
//! the whole line rigidly (GR2Analyst's Position motion, as a direct gesture);
//! hold shift at the press to sweep it about its midpoint instead. In every
//! case the track previews live while the drag is in flight and the section is
//! re-cut **only when the drag is dropped**. Before this, adjusting a line
//! meant re-arming the draw mode from the menu and drawing the whole line
//! again, which turns "nudge B a few kilometres east" into a four-step trip
//! through a drawer — and made walking a cut through a storm impractical
//! outright.
//!
//! The body's motions are **rigid on the sphere**: translation carries the
//! midpoint and rebuilds the ends about it, rotation pivots on the midpoint;
//! length is preserved by construction in both ([`rebuilt`] is the one
//! constructor). The fine-step spelling of the same two motions — and the only
//! sweep a touch screen with no modifier keys gets — lives on the section
//! pane itself ([`pan_step_km`], [`SWEEP_STEP_DEG`]), beside a readout of the
//! line's bearing and length so a sweep is aimed rather than blind.
//!
//! Every decision here is one of the map's existing rules, applied:
//!
//! * **No arming.** The armed modal drags exist because a drag on a *bare map*
//!   already means pan, so a rare gesture needs a mode to disambiguate it. A
//!   handle is different: it is a visible target a few points across, pressed
//!   deliberately, so proximity is the disambiguation — the same way a pane
//!   divider takes precedence over the map under it. The radius
//!   ([`ENDPOINT_GRAB_RADIUS_PT`]) is the whole contract with panning: inside
//!   it a press is an edit, outside it the map pans exactly as before. While
//!   either armed mode **is** on, the handles go inert — one drag on one map
//!   pane cannot be two gestures, and the armed mode was asked for last.
//! * **The preview is geographic, and it moves only when the pointer does.**
//!   The drag suppresses panning but not zooming (walkers reads the wheel
//!   itself), so a mid-drag zoom is ordinary. The grabbed end is re-anchored
//!   from the pointer **only on frames the pointer moved**
//!   ([`SectionEditDrag::pointer_moved`]): on a zoom-only frame the pointer
//!   sits still over new ground, and re-unprojecting it would silently slide
//!   the endpoint to wherever that pixel names now — the exact failure
//!   [`SectionLine`](crate::pane::SectionLine) stores geography to prevent.
//! * **The re-cut happens on the drop, and through the existing dispatch.** A
//!   cut is a multi-MB extraction walking the merged volume's gate bytes; per
//!   frame it would be the most expensive thing in the app by an order of
//!   magnitude. So the drag never touches the pane's stored line. The drop
//!   records a [`pending edit`](crate::ui::Gui) applied after the pane loop,
//!   which writes the line and nothing else — the section staleness key
//!   carries the line, so the ordinary poll notices and re-cuts, and there is
//!   no second render path to drift from the first.
//! * **A drop that says nothing new commits nothing.** A press-and-release on
//!   a handle is how a user checks it is grabbable; committing the unchanged
//!   line would burn a re-cut on it. And a drag that shrinks the line under
//!   [`MIN_SECTION_EDIT_KM`] is discarded whole rather than committed — the
//!   same refusal the modal draw makes of a too-short gesture, made in ground
//!   kilometres here because an edit has no gesture length to measure: the
//!   pointer may travel a long way bringing B almost onto A.

use crate::pane::{GeoPoint, PaneId, SectionLine};

/// How close to an endpoint a press must land to grab it, in points.
///
/// The balance the radius strikes is stated in the module doc: generous enough
/// to grab — the visible cap is ~6 points and a finger is ~25, so 14 forgives
/// half a finger of aim — and small enough not to steal pans, since two
/// 14-point discs are a vanishing fraction of a pane. In **points**, not
/// pixels, so it is the same physical target on a hidpi desktop and a phone.
pub(crate) const ENDPOINT_GRAB_RADIUS_PT: f32 = 14.0;

/// The shortest line an edit may commit, in kilometres of ground.
///
/// The arithmetic bar is `SectionLine::new`'s refusal of coincident endpoints;
/// this is the usability bar above it. A section a couple of kilometres long is
/// two or three raster columns stretched across the pane — a picture of
/// nothing — and the likeliest way to produce one is overshooting while
/// dragging B toward A. Measured in ground km rather than gesture points
/// because the gesture can be long while the *line* ends up short.
///
/// Refused **whole**, not clamped: the drop keeps the line the drag started
/// from, exactly as a too-small modal drag keeps the mode armed. A clamped
/// commit would cut a line the user did not put there.
pub(crate) const MIN_SECTION_EDIT_KM: f64 = 2.0;

/// How close to the line's **body** a press must land to grab it, in points.
///
/// Deliberately tighter than [`ENDPOINT_GRAB_RADIUS_PT`]: the body is a track
/// that can run the whole width of a pane, so its capture zone is a long thin
/// band rather than two small discs — at 14 points either side it would turn
/// most pans across a sectioned storm into line drags. 8 points is four times
/// the stroke's width, enough to press the line a user can see.
pub(crate) const BODY_GRAB_RADIUS_PT: f32 = 8.0;

/// Degrees one sweep step turns the line about its midpoint.
///
/// Small enough that stepping around a storm feature reads as motion rather
/// than as jumps — a 5° turn moves a 100 km line's ends ~4.4 km — and large
/// enough that a full quarter-turn is 18 clicks, not 90.
pub(crate) const SWEEP_STEP_DEG: f64 = 5.0;

/// Kilometres one pan step slides the line, as a function of its length.
///
/// A fraction of the line rather than a constant, clamped to a sane band: a
/// 20 km line stepped 10 km at a time skips half its own width of storm, and a
/// 300 km line stepped 1 km at a time takes three hundred clicks to cross what
/// it shows. 5% reads as "walk the section through the storm" at every length
/// the app can cut; the clamps keep the ends of the range honest.
pub(crate) fn pan_step_km(length_km: f64) -> f64 {
    (length_km * 0.05).clamp(1.0, 10.0)
}

/// Which part of a committed line a press landed on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SectionGrab {
    /// The `A` end — the section raster's left-hand column.
    A,
    /// The `B` end — the right-hand column.
    B,
    /// The line between the ends. A plain drag translates (length and bearing
    /// kept); a shift-drag sweeps about the midpoint (midpoint and length
    /// kept).
    Body,
}

/// Where one line's grabbable geometry was drawn last frame, in screen points.
///
/// Recorded from inside `Map::show` — the only place a projector exists — and
/// read **before** it, by `render_panes`' pan-suppression decision: a press
/// that is going to become a grab is indistinguishable from one that is going
/// to become a pan until the pointer moves, and by then the map has already
/// slid. One frame stale by construction, which for a press is harmless — a
/// pointer about to press is not also flinging the viewport.
///
/// Carries the whole track polyline rather than reduced hit shapes, so the
/// suppression decision and the authoritative in-show hit test are **the same
/// call to [`grab_at`]** on the same geometry — two spellings of one hit test
/// is how a press comes to pan the map and start a drag at once.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SectionGrabZone {
    /// The map pane the line is drawn on.
    pub map_pane: PaneId,
    /// The section pane whose line this is.
    pub section_pane: PaneId,
    /// The `A` end, on screen.
    pub a_px: egui::Pos2,
    /// The `B` end, on screen.
    pub b_px: egui::Pos2,
    /// The drawn track's polyline — the body's hit geometry.
    pub track: Vec<egui::Pos2>,
}

impl SectionGrabZone {
    /// What a press at `pos` grabs in this zone.
    pub(crate) fn grab_at(&self, pos: egui::Pos2) -> Option<SectionGrab> {
        grab_at(pos, self.a_px, self.b_px, &self.track)
    }
}

/// An endpoint drag in flight.
///
/// Held on the `Gui`, like [`RegionDrag`](crate::ui_region::RegionDrag) and for
/// the same reason: it is a property of the *gesture*, advanced only from
/// inside the owning map pane's `Map::show`, and it must not survive the modes
/// that conflict with it (both armed-drag setters clear it).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SectionEditDrag {
    /// The map pane the drag is happening on. A drag belongs to one pane for
    /// its whole life; the pointer leaving that pane's rect does not end it,
    /// because dragging an endpoint across a pane edge is ordinary.
    pub map_pane: PaneId,
    /// The section pane whose line is being edited — where the drop lands.
    pub section_pane: PaneId,
    pub grab: SectionGrab,
    /// The line as it stood when the handle was grabbed. What the pane keeps
    /// showing until the drop, and what a discarded drag leaves untouched.
    pub original: SectionLine,
    /// The line as the drag currently previews it. Geographic, so it means the
    /// same ground through any mid-drag zoom.
    preview: SectionLine,
    /// Where the pointer was last folded in, in screen points. The moved-gate:
    /// see [`Self::pointer_moved`].
    last_pointer: egui::Pos2,
    /// The ground under the press — the body drag's anchor. Geographic for the
    /// reason every anchor here is: a pixel anchor would re-aim the whole line
    /// on a mid-drag zoom.
    press_ground: GeoPoint,
    /// Whether a body drag sweeps (rotates about the midpoint) rather than
    /// translating. Decided **at the press** from the shift modifier and held
    /// for the drag's life: a gesture that changed verbs mid-flight whenever
    /// a modifier key slipped would leave the line somewhere no single motion
    /// explains.
    sweep: bool,
}

impl SectionEditDrag {
    /// Start a drag on `grab` of `original`.
    ///
    /// `sweep` is only consulted for a [`SectionGrab::Body`] drag — an
    /// endpoint drag means the same thing with or without a modifier down.
    pub(crate) fn begin(
        map_pane: PaneId,
        section_pane: PaneId,
        grab: SectionGrab,
        original: SectionLine,
        pointer: egui::Pos2,
        press_ground: GeoPoint,
        sweep: bool,
    ) -> Self {
        Self {
            map_pane,
            section_pane,
            grab,
            original,
            preview: original,
            last_pointer: pointer,
            press_ground,
            sweep,
        }
    }

    /// The line the track should draw this frame.
    pub(crate) fn preview(&self) -> SectionLine {
        self.preview
    }

    /// Whether the pointer has moved since the last fold-in — the gate that
    /// keeps a zoom-only frame from re-anchoring the grabbed end to whatever
    /// ground its pixel names after the zoom.
    pub(crate) fn pointer_moved(&self, pointer: egui::Pos2) -> bool {
        pointer != self.last_pointer
    }

    /// Fold a moved pointer in.
    ///
    /// An endpoint grab moves that end to `ground` and leaves the other
    /// exactly where it is. A body grab recomputes from the **original** line
    /// and the whole press→pointer motion — translation, or rotation about
    /// the original midpoint under [`sweep`](Self::begin) — rather than
    /// accumulating frame deltas, so floating error cannot walk the line and
    /// a pointer that returns to the press returns the line to its start.
    ///
    /// A motion the constructors refuse — off Earth, coincident ends, a
    /// rotation press too close to the midpoint to have a bearing — leaves
    /// the **preview** as it was rather than the drag dead: the pointer will
    /// move again next frame, and a refusal that stuck would read as the line
    /// freezing mid-drag.
    pub(crate) fn drag_to(&mut self, pointer: egui::Pos2, ground: GeoPoint) {
        self.last_pointer = pointer;
        let next = match self.grab {
            SectionGrab::A | SectionGrab::B => with_endpoint(self.preview, self.grab, ground),
            SectionGrab::Body if self.sweep => {
                let mid = midpoint(self.original);
                let (from_bearing, from_km) = rustdar_geo::site_bearing_range_km(
                    mid.lat,
                    mid.lon,
                    self.press_ground.lat,
                    self.press_ground.lon,
                );
                let (to_bearing, to_km) =
                    rustdar_geo::site_bearing_range_km(mid.lat, mid.lon, ground.lat, ground.lon);
                // Within a fifth of a kilometre of the midpoint neither
                // bearing means anything — the press was effectively *on* the
                // pivot — and a rotation computed from noise would spin the
                // line under a still-ish pointer.
                if from_km < 0.2 || to_km < 0.2 {
                    None
                } else {
                    rotated(self.original, to_bearing - from_bearing)
                }
            }
            SectionGrab::Body => translated(self.original, self.press_ground, ground),
        };
        if let Some(line) = next {
            self.preview = line;
        }
    }

    /// The line this drop commits, or `None` for a drop that must leave the
    /// pane's line alone.
    ///
    /// `None` twice over: an unchanged preview (a press-and-release, the way a
    /// user checks a handle is grabbable) would burn a multi-MB re-cut to
    /// produce the same picture; and a line under [`MIN_SECTION_EDIT_KM`] is
    /// refused whole rather than clamped, keeping the line the drag started
    /// from.
    pub(crate) fn commit(self) -> Option<SectionLine> {
        (self.preview != self.original && length_km(self.preview) >= MIN_SECTION_EDIT_KM)
            .then_some(self.preview)
    }
}

/// `line` with the `grab` end moved to `ground`, or `None` for a line that
/// cannot be cut — and `None` for a body grab, which has no single end to
/// move ([`SectionEditDrag::drag_to`] routes it before asking).
///
/// Through [`SectionLine::new`], which is the one gate on finite, distinct
/// endpoints — this must not become a second spelling of that judgement.
pub(crate) fn with_endpoint(
    line: SectionLine,
    grab: SectionGrab,
    ground: GeoPoint,
) -> Option<SectionLine> {
    match grab {
        SectionGrab::A => SectionLine::new(ground, line.b()),
        SectionGrab::B => SectionLine::new(line.a(), ground),
        SectionGrab::Body => None,
    }
}

/// The line's ground length, in kilometres.
pub(crate) fn length_km(line: SectionLine) -> f64 {
    let (_, km) =
        rustdar_geo::site_bearing_range_km(line.a().lat, line.a().lon, line.b().lat, line.b().lon);
    km
}

/// The line's midpoint on the ground — halfway along the great circle the cut
/// follows, from the same walk (`rustdar_geo::great_circle_point`) the sampler and
/// the drawn track both use, so "the middle of the line" is one place in every
/// part of the app.
pub(crate) fn midpoint(line: SectionLine) -> GeoPoint {
    let (lat, lon) = rustdar_geo::great_circle_point(
        (line.a().lat, line.a().lon),
        (line.b().lat, line.b().lon),
        0.5,
    );
    GeoPoint { lat, lon }
}

/// The line's bearing, degrees clockwise from north: the direction A→B as it
/// passes through the midpoint.
///
/// **At the midpoint**, not at A: a great circle's bearing changes along its
/// run, and the midpoint is the point the pan and sweep operations are defined
/// around — measuring the bearing anywhere else would make "rotate by 0°"
/// move the line.
pub(crate) fn bearing_deg(line: SectionLine) -> f64 {
    let mid = midpoint(line);
    let (bearing, _) =
        rustdar_geo::site_bearing_range_km(mid.lat, mid.lon, line.b().lat, line.b().lon);
    bearing
}

/// The point `distance_km` from `from` along `bearing_deg`, on the sphere the
/// rest of the crate's geodesy walks ([`rustdar_geo::EARTH_RADIUS_KM`]).
///
/// The longitude is wrapped into `[-180, 180]` because
/// [`GeoPoint::is_on_earth`] — and therefore `SectionLine::new` — refuses
/// anything outside it, and a translation across the antimeridian is a place a
/// section can legitimately go.
fn destination(from: GeoPoint, bearing_deg: f64, distance_km: f64) -> GeoPoint {
    let (lat, lon_raw) =
        rustdar_geo::great_circle_destination(from.lat, from.lon, bearing_deg, distance_km);
    let mut lon = lon_raw;
    if lon > 180.0 {
        lon -= 360.0;
    } else if lon < -180.0 {
        lon += 360.0;
    }
    GeoPoint { lat, lon }
}

/// The line rebuilt about `mid` with the given bearing and half-length — the
/// one constructor pan and sweep both go through, so "length and bearing are
/// preserved" is true by construction rather than by two matching derivations.
///
/// Through [`SectionLine::new`], which is still the only gate on a line that
/// can be cut.
fn rebuilt(mid: GeoPoint, bearing: f64, half_km: f64) -> Option<SectionLine> {
    let a = destination(mid, bearing + 180.0, half_km);
    let b = destination(mid, bearing, half_km);
    SectionLine::new(a, b)
}

/// `line` translated by the ground motion `from` → `to`: the midpoint is
/// carried along that displacement, the bearing and length are kept.
///
/// Defined through the midpoint rather than by offsetting each endpoint's
/// coordinates, because a lat/lon offset is not a rigid motion on a sphere —
/// sliding a 200 km line 100 km south by subtracting degrees changes its
/// length by the ratio of the two latitudes' `cos`, and the section would
/// quietly grow as it walked toward the equator.
pub(crate) fn translated(line: SectionLine, from: GeoPoint, to: GeoPoint) -> Option<SectionLine> {
    let (motion_bearing, motion_km) =
        rustdar_geo::site_bearing_range_km(from.lat, from.lon, to.lat, to.lon);
    if !motion_km.is_finite() {
        return None;
    }
    if motion_km == 0.0 {
        return Some(line);
    }
    let mid = destination(midpoint(line), motion_bearing, motion_km);
    rebuilt(mid, bearing_deg(line), length_km(line) * 0.5)
}

/// `line` rotated `delta_deg` clockwise about its midpoint: the midpoint and
/// length are kept, the bearing changes by exactly the delta.
pub(crate) fn rotated(line: SectionLine, delta_deg: f64) -> Option<SectionLine> {
    if !delta_deg.is_finite() {
        return None;
    }
    rebuilt(
        midpoint(line),
        bearing_deg(line) + delta_deg,
        length_km(line) * 0.5,
    )
}

/// `line` slid `step_km` perpendicular to itself — positive to the **right**
/// of A→B — with bearing and length kept. GR2Analyst's Position slider, as a
/// step: the "walk the section through the storm" motion.
pub(crate) fn panned(line: SectionLine, step_km: f64) -> Option<SectionLine> {
    if !step_km.is_finite() {
        return None;
    }
    let mid = destination(midpoint(line), bearing_deg(line) + 90.0, step_km);
    rebuilt(mid, bearing_deg(line), length_km(line) * 0.5)
}

/// Which part of the line a press at `pos` grabs, given the endpoints and the
/// drawn track's polyline — or `None` for a press that should pan the map.
///
/// **Endpoints before body**, unconditionally: the track passes through its
/// own end caps, so every press on a handle is also within the body band, and
/// resolving by distance instead of by kind would make the grab near a cap
/// flicker between "move this end" and "move everything" with sub-point
/// pointer noise. The **nearer** endpoint wins when both are in radius (a
/// short line puts them within a finger of each other).
pub(crate) fn grab_at(
    pos: egui::Pos2,
    a_px: egui::Pos2,
    b_px: egui::Pos2,
    track: &[egui::Pos2],
) -> Option<SectionGrab> {
    let da = a_px.distance(pos);
    let db = b_px.distance(pos);
    if da <= ENDPOINT_GRAB_RADIUS_PT && da <= db {
        return Some(SectionGrab::A);
    }
    if db <= ENDPOINT_GRAB_RADIUS_PT {
        return Some(SectionGrab::B);
    }
    let on_body = track
        .windows(2)
        .any(|pair| distance_to_segment(pos, pair[0], pair[1]) <= BODY_GRAB_RADIUS_PT);
    on_body.then_some(SectionGrab::Body)
}

/// Distance from `pos` to the segment `a`–`b`, in points.
fn distance_to_segment(pos: egui::Pos2, a: egui::Pos2, b: egui::Pos2) -> f32 {
    let ab = b - a;
    let len_sq = ab.length_sq();
    if len_sq <= f32::EPSILON {
        return a.distance(pos);
    }
    let t = ((pos - a).dot(ab) / len_sq).clamp(0.0, 1.0);
    (a + ab * t).distance(pos)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(lat: f64, lon: f64) -> GeoPoint {
        GeoPoint { lat, lon }
    }

    fn line() -> SectionLine {
        SectionLine::new(point(35.0, -97.8), point(35.6, -96.9)).expect("a real line")
    }

    /// Moving one end leaves the other exactly where it was — the property
    /// that makes a handle a handle rather than a redraw.
    #[test]
    fn an_endpoint_moves_and_the_other_stays() {
        let target = point(35.3, -97.2);

        let moved_a = with_endpoint(line(), SectionGrab::A, target).expect("a valid line");
        assert_eq!(moved_a.a(), target);
        assert_eq!(moved_a.b(), line().b(), "grabbing A must not move B");

        let moved_b = with_endpoint(line(), SectionGrab::B, target).expect("a valid line");
        assert_eq!(moved_b.b(), target);
        assert_eq!(moved_b.a(), line().a(), "grabbing B must not move A");
    }

    /// The constructor's refusals hold through the edit path: an endpoint
    /// cannot be dragged off the Earth or onto its partner.
    #[test]
    fn an_impossible_endpoint_is_refused_not_laundered() {
        for bad in [point(f64::NAN, -97.0), point(1e9, -97.0), point(35.0, 1e9)] {
            assert!(
                with_endpoint(line(), SectionGrab::A, bad).is_none(),
                "an endpoint at {bad:?} must be refused"
            );
        }
        assert!(
            with_endpoint(line(), SectionGrab::A, line().b()).is_none(),
            "both ends on one point is not a line"
        );
    }

    /// A grab needs to be inside its radius, the nearer endpoint wins, and the
    /// body is grabbable between them — the rules that keep a press on the
    /// open map a pan.
    #[test]
    fn a_grab_prefers_the_nearer_endpoint_and_refuses_beyond_its_radius() {
        let a = egui::pos2(100.0, 100.0);
        let b = egui::pos2(300.0, 100.0);
        let track = [a, egui::pos2(200.0, 100.0), b];

        assert_eq!(
            grab_at(egui::pos2(104.0, 103.0), a, b, &track),
            Some(SectionGrab::A)
        );
        assert_eq!(
            grab_at(egui::pos2(295.0, 95.0), a, b, &track),
            Some(SectionGrab::B)
        );
        // Between the caps, on the drawn line: the body.
        assert_eq!(
            grab_at(egui::pos2(200.0, 105.0), a, b, &track),
            Some(SectionGrab::Body)
        );
        // Just past the body band: a pan. One point of margin, so a radius
        // mutated bigger or smaller moves this press across the boundary.
        assert_eq!(
            grab_at(
                egui::pos2(200.0, 100.0 + BODY_GRAB_RADIUS_PT + 1.0),
                a,
                b,
                &track
            ),
            None
        );
        // Just outside the endpoint radius, and off the body's run: a pan.
        let outside = egui::pos2(100.0 - ENDPOINT_GRAB_RADIUS_PT - 1.0, 100.0);
        assert_eq!(grab_at(outside, a, b, &track), None);
        // A press near a cap is inside the body band too — the track runs
        // through its own caps — and the cap must win, or the grab would
        // flicker between "move this end" and "move everything".
        assert_eq!(
            grab_at(egui::pos2(107.0, 100.0), a, b, &track),
            Some(SectionGrab::A),
            "the endpoint must win over the body it sits on"
        );
        // Dead centre of a short line: both caps in radius, nearer one wins.
        let close_b = egui::pos2(120.0, 100.0);
        assert_eq!(
            grab_at(egui::pos2(112.0, 100.0), a, close_b, &[a, close_b]),
            Some(SectionGrab::B),
            "the nearer endpoint must win when both are in radius"
        );
        // The pan contract in **absolute points**, with nothing derived from
        // the constants: 30 points from the A cap and 20 from the body must be
        // a pan. The margin probes above compute their presses *from* the
        // radii, so they follow a mutated constant wherever it goes; this one
        // sits where the shipped numbers (14 and 8) put the boundary, and a
        // radius grown past it fails here by turning the press into a grab.
        assert_eq!(
            grab_at(egui::pos2(122.36, 80.0), a, b, &track),
            None,
            "a press 30 points from a cap and 20 points off the body must pan"
        );
    }

    /// Pan and sweep are rigid motions: translation keeps length and bearing
    /// and carries the midpoint; rotation keeps midpoint and length and turns
    /// the bearing by exactly the delta.
    ///
    /// Tolerances are metres-and-microdegrees against motions of tens of
    /// kilometres and tens of degrees: what these close is not floating error
    /// but the shape of the arithmetic — a translation done by offsetting
    /// lat/lon (which stretches with latitude), or a rotation pivoted on an
    /// endpoint instead of the midpoint.
    #[test]
    fn pan_and_sweep_are_rigid_motions_of_the_line() {
        let line = line();
        let len = length_km(line);
        let bearing = bearing_deg(line);
        let mid = midpoint(line);

        // Translation: from → to is ~40 km north-east.
        let from = point(35.2, -97.5);
        let to = point(35.5, -97.2);
        let slid = translated(line, from, to).expect("a rigid motion stays on Earth");
        assert!(
            (length_km(slid) - len).abs() < 0.01,
            "translation changed the length: {len} -> {}",
            length_km(slid)
        );
        assert!(
            (bearing_deg(slid) - bearing).abs() < 0.01,
            "translation changed the bearing: {bearing} -> {}",
            bearing_deg(slid)
        );
        let (want_bearing, want_km) =
            rustdar_geo::site_bearing_range_km(from.lat, from.lon, to.lat, to.lon);
        let new_mid = midpoint(slid);
        let (got_bearing, got_km) =
            rustdar_geo::site_bearing_range_km(mid.lat, mid.lon, new_mid.lat, new_mid.lon);
        assert!(
            (got_km - want_km).abs() < 0.05,
            "the midpoint moved {got_km} km for a {want_km} km drag"
        );
        assert!(
            (got_bearing - want_bearing).abs() < 1.0,
            "the midpoint moved on bearing {got_bearing} for a drag on {want_bearing}"
        );
        // A motion of nothing is the identity, not a rebuild's worth of drift.
        assert_eq!(translated(line, from, from), Some(line));

        // Rotation: 30° about the midpoint.
        let turned = rotated(line, 30.0).expect("a rotation stays on Earth");
        let turned_mid = midpoint(turned);
        assert!(
            (turned_mid.lat - mid.lat).abs() < 1e-6 && (turned_mid.lon - mid.lon).abs() < 1e-6,
            "the rotation moved its own pivot: {mid:?} -> {turned_mid:?}"
        );
        assert!((length_km(turned) - len).abs() < 0.01);
        assert!(
            (bearing_deg(turned) - (bearing + 30.0)).abs() < 0.01,
            "a 30° sweep turned the bearing from {bearing} to {}",
            bearing_deg(turned)
        );

        // The pan step: perpendicular, to the right of A→B, and invertible.
        let stepped = panned(line, 15.0).expect("a step stays on Earth");
        assert!((length_km(stepped) - len).abs() < 0.01);
        assert!((bearing_deg(stepped) - bearing).abs() < 0.01);
        let stepped_mid = midpoint(stepped);
        let (step_bearing, step_km) =
            rustdar_geo::site_bearing_range_km(mid.lat, mid.lon, stepped_mid.lat, stepped_mid.lon);
        assert!(
            (step_km - 15.0).abs() < 0.01,
            "a 15 km step moved the line {step_km} km"
        );
        assert!(
            (step_bearing - (bearing + 90.0))
                .rem_euclid(360.0)
                .min((360.0 - (step_bearing - (bearing + 90.0)).rem_euclid(360.0)).abs())
                < 0.1,
            "the step was not perpendicular: line bearing {bearing}, step \
             bearing {step_bearing}"
        );
        // Out and back returns the line to within metres. Not exactly: "keep
        // the numeric bearing" is not parallel transport, so a 15 km step
        // carries ~10 m of spherical residual — invisible at any zoom, and
        // bounded per *user action* rather than per frame, so it cannot
        // accumulate on its own. The tolerance is 30 m; what it closes is a
        // sign error or a wrong pivot, both of which miss by kilometres.
        let back = panned(stepped, -15.0).expect("the inverse step");
        let back_mid = midpoint(back);
        assert!(
            (back_mid.lat - mid.lat).abs() < 3e-4 && (back_mid.lon - mid.lon).abs() < 3e-4,
            "stepping out and back did not return the line: {mid:?} -> {back_mid:?}"
        );
    }

    /// The pan step scales with the line and stays inside its band.
    #[test]
    fn the_pan_step_scales_with_the_line_and_stays_inside_its_band() {
        assert_eq!(pan_step_km(10.0), 1.0, "short lines step the 1 km floor");
        assert_eq!(pan_step_km(100.0), 5.0, "5% of the line in the band");
        assert_eq!(pan_step_km(400.0), 10.0, "long lines stop at 10 km");
    }

    /// A body drag translates; a shift body drag sweeps about the midpoint;
    /// and a sweep press on the pivot itself is refused rather than spun.
    #[test]
    fn a_body_drag_translates_and_a_shift_body_drag_sweeps() {
        let at = egui::pos2(50.0, 50.0);
        let press = point(35.2, -97.5);

        // Translate: the whole press→pointer motion, recomputed from the
        // original — so returning the pointer returns the line.
        let mut slide = SectionEditDrag::begin(0, 1, SectionGrab::Body, line(), at, press, false);
        slide.drag_to(egui::pos2(60.0, 55.0), point(35.5, -97.2));
        let slid = slide.preview();
        assert!((length_km(slid) - length_km(line())).abs() < 0.01);
        assert!((bearing_deg(slid) - bearing_deg(line())).abs() < 0.01);
        assert_ne!(midpoint(slid), midpoint(line()), "the drag moved nothing");
        slide.drag_to(egui::pos2(61.0, 55.0), press);
        assert_eq!(
            slide.preview(),
            line(),
            "a pointer returned to the press must return the line"
        );

        // Sweep: bearing follows the pointer's bearing about the midpoint.
        let mid = midpoint(line());
        let press_on_line = point(
            mid.lat + (line().b().lat - mid.lat) * 0.6,
            mid.lon + (line().b().lon - mid.lon) * 0.6,
        );
        let mut sweep =
            SectionEditDrag::begin(0, 1, SectionGrab::Body, line(), at, press_on_line, true);
        // Roughly north of the midpoint, well off the line's own bearing.
        sweep.drag_to(egui::pos2(60.0, 40.0), point(mid.lat + 0.4, mid.lon));
        let swept = sweep.preview();
        assert!((length_km(swept) - length_km(line())).abs() < 0.01);
        let swept_mid = midpoint(swept);
        assert!(
            (swept_mid.lat - mid.lat).abs() < 1e-6 && (swept_mid.lon - mid.lon).abs() < 1e-6,
            "the sweep moved its own pivot"
        );
        assert!(
            (bearing_deg(swept) - bearing_deg(line())).abs() > 5.0,
            "the sweep did not turn the line: {} -> {}",
            bearing_deg(line()),
            bearing_deg(swept)
        );
        // Signed, not absolute: the grabbed point must move *toward* the
        // pointer's bearing about the pivot, so the line's bearing lands at
        // its old value plus exactly the pivot-relative swing from the press
        // to the pointer. A negated delta sweeps the line the other way and
        // misses this by roughly twice the swing — ~100° here — while every
        // magnitude-only assertion above still passes.
        let to = point(mid.lat + 0.4, mid.lon);
        let (to_bearing, _) = rustdar_geo::site_bearing_range_km(mid.lat, mid.lon, to.lat, to.lon);
        let (from_bearing, _) = rustdar_geo::site_bearing_range_km(
            mid.lat,
            mid.lon,
            press_on_line.lat,
            press_on_line.lon,
        );
        let want = (bearing_deg(line()) + (to_bearing - from_bearing)).rem_euclid(360.0);
        let off = (bearing_deg(swept).rem_euclid(360.0) - want).rem_euclid(360.0);
        assert!(
            off.min(360.0 - off) < 0.05,
            "the sweep did not carry the grabbed point toward the pointer: \
             wanted bearing {want}, got {}",
            bearing_deg(swept).rem_euclid(360.0)
        );

        // A sweep press on the pivot has no bearing to rotate from: refused,
        // preview kept.
        let mut degenerate = SectionEditDrag::begin(0, 1, SectionGrab::Body, line(), at, mid, true);
        degenerate.drag_to(egui::pos2(70.0, 70.0), point(mid.lat + 0.4, mid.lon));
        assert_eq!(
            degenerate.preview(),
            line(),
            "a rotation press on the pivot spun the line from bearing noise"
        );
    }

    /// A drop with nothing new in it commits nothing: not an unchanged line,
    /// and not a line shrunk under the minimum.
    #[test]
    fn a_commit_needs_a_change_and_a_minimum_length() {
        let at = egui::pos2(50.0, 50.0);

        // Press-and-release: the preview never moved.
        let untouched = SectionEditDrag::begin(0, 1, SectionGrab::B, line(), at, line().a(), false);
        assert!(
            untouched.commit().is_none(),
            "an unchanged preview must not burn a re-cut"
        );

        // A real move commits the preview, and the preview is what commits.
        let mut moved = SectionEditDrag::begin(0, 1, SectionGrab::B, line(), at, line().a(), false);
        let target = point(35.9, -96.5);
        moved.drag_to(egui::pos2(60.0, 60.0), target);
        let committed = moved.commit().expect("a moved endpoint commits");
        assert_eq!(committed.b(), target);
        assert_eq!(committed.a(), line().a());

        // Overshooting B almost onto A: refused whole, not clamped.
        let mut shrunk =
            SectionEditDrag::begin(0, 1, SectionGrab::B, line(), at, line().a(), false);
        let a = line().a();
        shrunk.drag_to(egui::pos2(70.0, 70.0), point(a.lat + 0.005, a.lon));
        assert!(
            length_km(shrunk.preview()) < MIN_SECTION_EDIT_KM,
            "precondition: the preview really is under the minimum"
        );
        assert!(
            shrunk.commit().is_none(),
            "a line under {MIN_SECTION_EDIT_KM} km must be discarded whole"
        );
    }

    /// The moved-gate and the refusal that keeps a drag alive: a stationary
    /// pointer is not folded in, and a bad ground mid-drag keeps the last
    /// good preview rather than freezing or dying.
    #[test]
    fn a_stationary_pointer_is_not_folded_in_and_a_bad_ground_keeps_the_preview() {
        let at = egui::pos2(50.0, 50.0);
        let mut drag = SectionEditDrag::begin(0, 1, SectionGrab::B, line(), at, line().a(), false);

        assert!(
            !drag.pointer_moved(at),
            "a pointer that sat still reported movement — a zoom-only frame \
             would re-anchor the endpoint to new ground"
        );
        assert!(drag.pointer_moved(egui::pos2(51.0, 50.0)));

        let good = point(35.9, -96.5);
        drag.drag_to(egui::pos2(60.0, 60.0), good);
        drag.drag_to(egui::pos2(61.0, 60.0), point(f64::NAN, -97.0));
        assert_eq!(
            drag.preview().b(),
            good,
            "a refused ground must keep the last good preview"
        );
    }
}
