//! Grabbing a committed cross-section line by its ends, and re-cutting only on
//! the drop.
//!
//! # The shape of the interaction, and why each part of it is the way it is
//!
//! A committed section's ground track already draws its two endpoints as
//! labelled caps. This module makes them **handles**: press one, drag it, and
//! the line follows — the track previews live while the drag is in flight, and
//! the section itself is re-cut **only when the handle is dropped**. Before
//! this, adjusting a line meant re-arming the draw mode from the menu and
//! drawing the whole line again, which turns "nudge B a few kilometres east"
//! into a four-step trip through a drawer.
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

/// Which part of a committed line a press landed on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SectionGrab {
    /// The `A` end — the section raster's left-hand column.
    A,
    /// The `B` end — the right-hand column.
    B,
}

/// Where one grabbable handle was drawn last frame, in screen points.
///
/// Recorded from inside `Map::show` — the only place a projector exists — and
/// read **before** it, by `render_panes`' pan-suppression decision: a press
/// that is going to become a handle drag is indistinguishable from one that is
/// going to become a pan until the pointer moves, and by then the map has
/// already slid. One frame stale by construction, which for a press is
/// harmless — a pointer about to press is not also flinging the viewport.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SectionHandleSpot {
    /// The map pane the handle is drawn on.
    pub map_pane: PaneId,
    /// The section pane whose line the handle belongs to.
    pub section_pane: PaneId,
    pub grab: SectionGrab,
    pub pos: egui::Pos2,
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
}

impl SectionEditDrag {
    /// Start a drag on `grab` of `original`.
    pub(crate) fn begin(
        map_pane: PaneId,
        section_pane: PaneId,
        grab: SectionGrab,
        original: SectionLine,
        pointer: egui::Pos2,
    ) -> Self {
        Self {
            map_pane,
            section_pane,
            grab,
            original,
            preview: original,
            last_pointer: pointer,
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

    /// Fold a moved pointer in: the grabbed end goes to `ground`, the other
    /// end stays exactly where it is.
    ///
    /// A `ground` the line constructor refuses — off Earth, or coincident with
    /// the fixed end — leaves the **preview** as it was rather than the drag
    /// dead: the pointer will move again next frame, and a refusal that stuck
    /// would read as the handle freezing mid-drag.
    pub(crate) fn drag_to(&mut self, pointer: egui::Pos2, ground: GeoPoint) {
        self.last_pointer = pointer;
        if let Some(line) = with_endpoint(self.preview, self.grab, ground) {
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
/// cannot be cut.
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
    }
}

/// The line's ground length, in kilometres.
pub(crate) fn length_km(line: SectionLine) -> f64 {
    let (_, km) = rustdar_radar::beam::site_bearing_range_km(
        line.a().lat,
        line.a().lon,
        line.b().lat,
        line.b().lon,
    );
    km
}

/// Which handle a press at `pos` grabs, given where the two endpoints are on
/// screen — or `None` for a press that should pan the map.
///
/// The **nearer** endpoint wins when both are in radius (a short line puts
/// them within a finger of each other), and a tie goes to `A` only because a
/// tie is a zero-length line nothing can cut anyway.
pub(crate) fn grab_at(pos: egui::Pos2, a_px: egui::Pos2, b_px: egui::Pos2) -> Option<SectionGrab> {
    let da = a_px.distance(pos);
    let db = b_px.distance(pos);
    if da <= ENDPOINT_GRAB_RADIUS_PT && da <= db {
        Some(SectionGrab::A)
    } else if db <= ENDPOINT_GRAB_RADIUS_PT {
        Some(SectionGrab::B)
    } else {
        None
    }
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

    /// A grab needs to be inside the radius, and the nearer endpoint wins —
    /// the rule that keeps a press on the open map a pan.
    #[test]
    fn a_grab_prefers_the_nearer_endpoint_and_refuses_beyond_its_radius() {
        let a = egui::pos2(100.0, 100.0);
        let b = egui::pos2(300.0, 100.0);

        assert_eq!(grab_at(egui::pos2(104.0, 103.0), a, b), Some(SectionGrab::A));
        assert_eq!(grab_at(egui::pos2(295.0, 95.0), a, b), Some(SectionGrab::B));
        // Just outside the radius: a pan, not a grab. The margin is one point,
        // so a radius mutated bigger or smaller moves this press across the
        // boundary.
        let outside = egui::pos2(100.0, 100.0 + ENDPOINT_GRAB_RADIUS_PT + 1.0);
        assert_eq!(grab_at(outside, a, b), None);
        // Dead centre of a short line: both in radius, nearer one wins.
        let close_b = egui::pos2(120.0, 100.0);
        assert_eq!(
            grab_at(egui::pos2(112.0, 100.0), a, close_b),
            Some(SectionGrab::B),
            "the nearer endpoint must win when both are in radius"
        );
    }

    /// A drop with nothing new in it commits nothing: not an unchanged line,
    /// and not a line shrunk under the minimum.
    #[test]
    fn a_commit_needs_a_change_and_a_minimum_length() {
        let at = egui::pos2(50.0, 50.0);

        // Press-and-release: the preview never moved.
        let untouched = SectionEditDrag::begin(0, 1, SectionGrab::B, line(), at);
        assert!(
            untouched.commit().is_none(),
            "an unchanged preview must not burn a re-cut"
        );

        // A real move commits the preview, and the preview is what commits.
        let mut moved = SectionEditDrag::begin(0, 1, SectionGrab::B, line(), at);
        let target = point(35.9, -96.5);
        moved.drag_to(egui::pos2(60.0, 60.0), target);
        let committed = moved.commit().expect("a moved endpoint commits");
        assert_eq!(committed.b(), target);
        assert_eq!(committed.a(), line().a());

        // Overshooting B almost onto A: refused whole, not clamped.
        let mut shrunk = SectionEditDrag::begin(0, 1, SectionGrab::B, line(), at);
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
        let mut drag = SectionEditDrag::begin(0, 1, SectionGrab::B, line(), at);

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
