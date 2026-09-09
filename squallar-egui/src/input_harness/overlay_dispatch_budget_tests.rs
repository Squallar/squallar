//! **The batch is bounded in BYTES, and it still arrives whole.**
//!
//! Every shown layer of every pane re-rasterizes on the same map move, and
//! until this door existed nothing bounded that aggregate: `RendersInFlight`
//! admits one raster per pane and layer, and its own type note says the
//! product `panes x texture layers x budget x plan bytes` is "which the budget
//! alone does not bound". Each term of that product is a whole
//! `egui::ColorImage` resident on the host until its last upload band crosses
//! — 41.7 MB apiece at a 2878x1651 pane, and `squallar_gpu`'s band queue
//! drains one 4 MiB band a frame on every browser.
//!
//! **The door counted those terms until 2026-09-09, which bounded the wrong
//! thing.** A picture is `1.5 x 1.5` viewports at four bytes a texel, so a
//! count of four is 71.2 MiB of pipe at 1920x1080 and 170.1 MiB at 3440x1440:
//! a ceiling on the display, not on the pipe. It is now
//! [`squallar_device_profile::constants::MAX_OVERLAY_PICTURE_BYTES_OUTSTANDING`],
//! and [`the_afforded_count_falls_as_the_picture_grows`] is the property that
//! separates the two — the one a revert to a count reads green on everywhere
//! else.
//!
//! Three properties, and the second and third are what make the first safe: a
//! frame dispatches at most the ceiling's worth of new picture, every pane
//! that was refused asks again and is served, and what the panes already hold
//! is charged at its own size.
//!
//! Driven through the real `render_pane_map_content` on a six-pane layout, so
//! what is measured is the door as the frame asks it, not a predicate called
//! by a test.
//!
//! # The density, and why the fixture sets one
//!
//! [`MAX_PANES_DESKTOP`] is six, so a door denominated in bytes can only be
//! made to refuse by making the *pictures* large — at the harness's default
//! density a pane's picture is about a megabyte and sixty of them would fit.
//! **That is the property an input must have for any of this to reach the
//! defect**, and [`DENSITY`] is what gives the fixture it: every test below
//! asserts its own non-vacuity against the plan the frame actually emitted, so
//! a density that stopped producing large enough pictures fails loudly instead
//! of passing on a door that never closed.

use super::InputHarness;
use super::loop_overlay_draw_tests::raster;
use super::tests::{alert_over, ingest_alerts, land_requested_rasters, rasterizes_requested};
use crate::overlay_cache::OverlayTexturePlan;
use squallar_device_profile::budget::MAX_PANES_DESKTOP;
use squallar_device_profile::constants::MAX_OVERLAY_PICTURE_BYTES_OUTSTANDING;
use squallar_source::id::{LayerId, known};

/// The layer that asks. A `RenderMode::Texture` layer whose data a test can
/// hand over whole.
const ASKING: LayerId = known::NWS_ALERTS;

/// A second texture layer, used only to put pictures in the pipe that the
/// asking layer must then find the allowance already spent on.
const OCCUPANT: LayerId = super::loop_overlay_draw_tests::LAYER;

const FRAME_DT: f64 = 1.0 / 60.0;

/// Every pane the desktop layout can draw, which is the widest the fixture can
/// be: the door is per pane and layer, and one layer is asking.
const PANES: usize = MAX_PANES_DESKTOP;

/// The display density the fixture draws at, so that one pane's picture is a
/// desktop-sized fraction of the ceiling rather than the ~1 MB a 1024x768
/// harness pane plans at 1.0. See this module's note.
const DENSITY: f32 = 4.0;

/// The adapter limit, set rather than defaulted: the planner gives up overdraw
/// to stay under it, so leaving it at egui's default would make every figure
/// below a function of that default instead of of the density.
const MAX_TEXTURE_SIDE: usize = 8192;

/// Six panes drawing one texture layer over data every one of them covers, at
/// a density that makes one picture a real fraction of the ceiling.
fn scene() -> InputHarness {
    let mut h = InputHarness::new();
    h.set_pixels_per_point(DENSITY);
    h.set_max_texture_side(MAX_TEXTURE_SIDE);
    h.set_pane_count(PANES);
    h.gui_mut().enable_overlay_for_test(&ASKING);
    h.warm_up();
    let ground = h.ground_at(0, h.pane_rects()[0].center());
    ingest_alerts(
        &mut h,
        vec![alert_over("a", "Tornado Warning", ground.y(), ground.x())],
    );
    h
}

/// The texture plans the last frame asked `kind` for — the door's own figures,
/// read off the actions rather than re-planned, so nothing here can disagree
/// with what the door charged.
fn requested_plans(h: &InputHarness, kind: &LayerId) -> Vec<OverlayTexturePlan> {
    h.last_actions()
        .iter()
        .filter_map(|a| match a {
            crate::actions::GuiAction::RenderOverlay {
                overlay_kind,
                texture,
                ..
            } if overlay_kind == kind => Some(*texture),
            _ => None,
        })
        .collect()
}

/// What one pane of this fixture plans a whole picture at.
fn picture_plan() -> OverlayTexturePlan {
    let mut h = scene();
    h.frame_after(FRAME_DT);
    let plans = requested_plans(&h, &ASKING);
    assert!(
        !plans.is_empty(),
        "fixture: the scene's first frame asked for no picture at all, so \
         nothing below is reading the door",
    );
    plans[0]
}

/// **How many pictures of `bytes` the door lets out**, by its own rule: it
/// admits while the pipe is *below* the line, so picture `k` goes out when
/// `(k - 1) * bytes` is still under the ceiling.
fn afforded(bytes: u64) -> usize {
    assert!(
        bytes > 0,
        "a picture priced at nothing affords infinitely many"
    );
    MAX_OVERLAY_PICTURE_BYTES_OUTSTANDING.div_ceil(bytes) as usize
}

/// **One frame asks for at most the ceiling's worth of picture, and the panes
/// it refused are served by the frames after it.**
///
/// The counts are exact on both halves, and the second is the non-vacuity of
/// the first: `PANES` panes are stale on the same frame — that is what the two
/// totals add up to — so a frame that asked for what it could afford refused
/// the rest, and those are asked for again once the first have landed. A door
/// that dropped the refusal instead of latching it reads as `0` on the second
/// half; a door that is not there at all reads as `PANES` on the first.
#[test]
fn a_frame_dispatches_at_most_the_allowance_and_the_refused_panes_are_served_next() {
    let mut h = scene();
    h.frame_after(FRAME_DT);

    let plans = requested_plans(&h, &ASKING);
    let picture = plans[0].bytes();
    let allowance = afforded(picture);
    assert!(
        allowance < PANES,
        "fixture: a {}x{} picture is {picture} B and {allowance} of them fit \
         under {MAX_OVERLAY_PICTURE_BYTES_OUTSTANDING} B, which is every pane \
         this layout has — the door cannot refuse anything and every count \
         below is vacuous",
        plans[0].width,
        plans[0].height,
    );

    let first = plans.len();
    assert_eq!(
        first, allowance,
        "the frame asked for {first} whole pictures of {picture} B against a \
         ceiling of {MAX_OVERLAY_PICTURE_BYTES_OUTSTANDING} B",
    );
    // The overshoot the door's rule allows is one picture and no more: the
    // `k`th is admitted on the occupancy before it, so the pipe ends at most
    // one picture past the line. A door that admitted on "does it fit" instead
    // reads one lower here and refuses everything at a canvas whose picture is
    // larger than the whole ceiling.
    assert!(
        ((first - 1) as u64).saturating_mul(picture) < MAX_OVERLAY_PICTURE_BYTES_OUTSTANDING,
        "the pipe overshot the ceiling by more than the one picture the door's \
         rule allows",
    );

    land_requested_rasters(&mut h, &ASKING);
    h.frame_after(FRAME_DT);
    let second = rasterizes_requested(&h, &ASKING);
    assert_eq!(
        second,
        PANES - allowance,
        "the panes the first frame refused asked for {second} pictures once \
         the allowance came back, not the {} still owed",
        PANES - allowance,
    );
    assert_eq!(
        first + second,
        PANES,
        "non-vacuity: all {PANES} panes wanted a picture, so the {first} the \
         first frame asked for is a door and not the whole demand",
    );
}

/// **The count the door affords falls as the picture grows, and the bytes it
/// affords do not.** This is the cut itself.
///
/// The same six-pane scene at [`DENSITY`] and half again: the pictures differ
/// by the square of the ratio, so a door denominated in pictures lets the same
/// number through at both and a door denominated in bytes does not. Both legs
/// are above [`DENSITY`] rather than either side of it because the *lower* leg
/// has to close the door too — a leg the door never refused on says nothing
/// about what the door is denominated in. The byte totals
/// are what is bounded, and they are within one picture of the ceiling at
/// both.
///
/// **The property an input must have to reach a reverted door** is that the
/// two densities plan *different* picture sizes and that both are large enough
/// for the door to close — asserted, because a fixture whose two legs planned
/// alike would read green against a plain count.
#[test]
fn the_afforded_count_falls_as_the_picture_grows() {
    let mut seen: Vec<(u64, usize)> = Vec::new();
    for density in [DENSITY, DENSITY * 1.5] {
        let mut h = InputHarness::new();
        h.set_pixels_per_point(density);
        h.set_max_texture_side(MAX_TEXTURE_SIDE);
        h.set_pane_count(PANES);
        h.gui_mut().enable_overlay_for_test(&ASKING);
        h.warm_up();
        let ground = h.ground_at(0, h.pane_rects()[0].center());
        ingest_alerts(
            &mut h,
            vec![alert_over("a", "Tornado Warning", ground.y(), ground.x())],
        );
        h.frame_after(FRAME_DT);

        let plans = requested_plans(&h, &ASKING);
        let picture = plans[0].bytes();
        assert!(
            afforded(picture) < PANES,
            "fixture: at {density} the door affords every pane and cannot \
             close, so this leg proves nothing",
        );
        assert_eq!(plans.len(), afforded(picture), "at density {density}");
        seen.push((picture, plans.len()));
    }
    let [(small, few_bytes_count), (large, many_bytes_count)] = seen[..] else {
        unreachable!("two densities, two readings")
    };
    assert!(
        small < large,
        "fixture: the two densities planned {small} B and {large} B, so \
         nothing here separates a byte door from a count",
    );
    assert!(
        few_bytes_count > many_bytes_count,
        "the door let {few_bytes_count} pictures of {small} B and \
         {many_bytes_count} of {large} B through. A count would have let the \
         same number of each through, which is the bound this replaced.",
    );
    for (picture, count) in [(small, few_bytes_count), (large, many_bytes_count)] {
        let outstanding = (count as u64).saturating_mul(picture);
        assert!(
            outstanding < MAX_OVERLAY_PICTURE_BYTES_OUTSTANDING.saturating_add(picture),
            "the pipe reached {outstanding} B, more than the ceiling plus the \
             one picture of overshoot the door's rule allows",
        );
    }
}

/// **What the panes already hold spends the allowance, at its own size**, so
/// the door bounds the pipe rather than one frame's burst.
///
/// The occupant layer's pictures are *held* — arrived, uploading, not yet
/// delivered — which is `upload pending`'s own state, and there are exactly as
/// many of them as the allowance. The asking layer then gets nothing, though
/// every pane wants one and no ask of its own has gone out.
///
/// The control is the same scene with the holds one short of the allowance:
/// one picture is affordable and exactly one is asked for. So this reads the
/// *bytes* of what is outstanding, not merely "something is outstanding".
///
/// **The held pictures declare the plan's size over a 1x1 texture**, which is
/// the one place this suite parts from what the app builds. The quantity the
/// door charges is the picture's declared `width x height` — the same fields
/// `App::apply_render_to_pane` fills from the rasterized image, and the same
/// ones `squallar_gpu` bands — and allocating six real 18 MB images here would
/// put the suite's own peak above the ceiling it is gating.
#[test]
fn pictures_already_in_the_pipe_spend_the_allowance() {
    let plan = picture_plan();
    let allowance = afforded(plan.bytes());
    assert!(
        allowance >= 2,
        "fixture: the allowance is {allowance}, so the `allowance - 1` control \
         below is the empty pipe and not a partly spent one",
    );
    for held in [allowance - 1, allowance] {
        let mut h = scene();
        h.gui_mut().enable_overlay_for_test(&OCCUPANT);
        for idx in 0..held {
            let mut picture = raster(&h, &format!("occupant-{idx}"));
            picture.width = plan.width;
            picture.height = plan.height;
            h.gui_mut().panes_mut()[idx]
                .overlay_cache_mut(&OCCUPANT)
                .hold(picture, None);
        }
        let outstanding: u64 = h
            .gui()
            .panes()
            .iter()
            .map(crate::pane::PaneState::overlay_picture_bytes_outstanding)
            .sum();
        assert_eq!(
            outstanding,
            (held as u64) * plan.bytes(),
            "fixture: {held} pictures of {} B were put in the pipe and the \
             walk reads {outstanding} B",
            plan.bytes(),
        );

        h.frame_after(FRAME_DT);
        let asked = rasterizes_requested(&h, &ASKING);
        assert_eq!(
            asked,
            allowance - held,
            "with {held} pictures already in the pipe the frame asked for \
             {asked} more",
        );
    }
}

/// **A supersede is charged nothing**, because it is net zero on the pipe:
/// the `hold` drops the handle it was keeping, egui retires that texture and
/// `squallar_gpu`'s `TextureUploads::free` takes its bands out of the queue
/// in the same breath.
///
/// Every pane is holding a picture for the asking layer, so the allowance is
/// spent several times over — and every pane still asks, because none of them
/// is adding a picture to the queue. Without the exemption the count is the
/// allowance instead, and a pane mid-upload would be unable to follow the map.
#[test]
fn a_pane_replacing_the_picture_it_is_already_uploading_is_charged_nothing() {
    let plan = picture_plan();
    assert!(
        (PANES as u64) * plan.bytes() > MAX_OVERLAY_PICTURE_BYTES_OUTSTANDING,
        "fixture: {PANES} held pictures do not even reach the ceiling, so an \
         unexempted supersede would have been afforded anyway",
    );
    let mut h = scene();
    for idx in 0..PANES {
        let mut picture = raster(&h, &format!("held-{idx}"));
        picture.width = plan.width;
        picture.height = plan.height;
        h.gui_mut().panes_mut()[idx]
            .overlay_cache_mut(&ASKING)
            .hold(picture, None);
    }
    h.frame_after(FRAME_DT);
    let asked = rasterizes_requested(&h, &ASKING);
    assert_eq!(
        asked, PANES,
        "every one of the {PANES} panes is replacing a picture it is already \
         uploading, and {asked} of them were let through",
    );
}

/// **Radar is not counted.** Its rasters come from the app's own
/// `dispatch_pane_renders`, which this door cannot throttle, so charging them
/// would let a playing radar loop close the door on every other layer for as
/// long as it plays.
#[test]
fn a_radar_raster_in_the_pipe_does_not_spend_the_overlay_allowance() {
    let plan = picture_plan();
    let allowance = afforded(plan.bytes());
    let mut h = scene();
    for idx in 0..PANES {
        let mut picture = raster(&h, &format!("radar-{idx}"));
        picture.width = plan.width;
        picture.height = plan.height;
        h.gui_mut().panes_mut()[idx]
            .overlay_cache_mut(&known::RADAR)
            .hold(picture, None);
    }
    let outstanding: u64 = h
        .gui()
        .panes()
        .iter()
        .map(crate::pane::PaneState::overlay_picture_bytes_outstanding)
        .sum();
    assert_eq!(
        outstanding,
        0,
        "{PANES} panes hold a radar picture of {} B and the overlay walk \
         charged {outstanding} B for them",
        plan.bytes(),
    );

    h.frame_after(FRAME_DT);
    let asked = rasterizes_requested(&h, &ASKING);
    assert_eq!(
        asked, allowance,
        "the whole overlay allowance should have been free, and {asked} \
         pictures were asked for against an allowance of {allowance}",
    );
}
