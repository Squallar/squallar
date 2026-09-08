//! **The batch is bounded, and it still arrives whole.**
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
//! Two properties, and the second is what makes the first safe: a frame
//! dispatches at most
//! [`squallar_device_profile::constants::MAX_OVERLAY_PICTURES_OUTSTANDING`]
//! new pictures, and every pane that was refused asks again and is served, so
//! the batch is delivered in slices rather than cut short.
//!
//! Driven through the real `render_pane_map_content` on a six-pane layout, so
//! what is measured is the door as the frame asks it, not a predicate called
//! by a test.

use super::InputHarness;
use super::loop_overlay_draw_tests::raster;
use super::tests::{alert_over, ingest_alerts, land_requested_rasters, rasterizes_requested};
use squallar_device_profile::constants::MAX_OVERLAY_PICTURES_OUTSTANDING;
use squallar_source::id::{LayerId, known};

/// The layer that asks. A `RenderMode::Texture` layer whose data a test can
/// hand over whole.
const ASKING: LayerId = known::NWS_ALERTS;

/// A second texture layer, used only to put pictures in the pipe that the
/// asking layer must then find the allowance already spent on.
const OCCUPANT: LayerId = super::loop_overlay_draw_tests::LAYER;

const FRAME_DT: f64 = 1.0 / 60.0;

/// More panes than the allowance, so a frame that dispatched everything it
/// was asked for would be visibly over it.
const PANES: usize = MAX_OVERLAY_PICTURES_OUTSTANDING + 2;

/// Six panes drawing one texture layer over data every one of them covers.
fn scene() -> InputHarness {
    let mut h = InputHarness::new();
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

/// **One frame asks for at most the allowance, and the panes it refused are
/// served by the frames after it.**
///
/// The counts are exact on both halves, and the second is the non-vacuity of
/// the first: `PANES` panes are stale on the same frame — that is what the
/// two totals add up to — so a frame that asked for four of them refused two,
/// and those two are asked for again once the four have landed. A door that
/// dropped the refusal instead of latching it reads as `0` on the second
/// half; a door that is not there at all reads as `PANES` on the first.
#[test]
fn a_frame_dispatches_at_most_the_allowance_and_the_refused_panes_are_served_next() {
    let mut h = scene();
    h.frame_after(FRAME_DT);

    let first = rasterizes_requested(&h, &ASKING);
    assert_eq!(
        first, MAX_OVERLAY_PICTURES_OUTSTANDING,
        "the frame asked for {first} whole pictures against an allowance of \
         {MAX_OVERLAY_PICTURES_OUTSTANDING}",
    );

    land_requested_rasters(&mut h, &ASKING);
    h.frame_after(FRAME_DT);
    let second = rasterizes_requested(&h, &ASKING);
    assert_eq!(
        second,
        PANES - MAX_OVERLAY_PICTURES_OUTSTANDING,
        "the panes the first frame refused asked for {second} pictures once \
         the allowance came back, not the {} still owed",
        PANES - MAX_OVERLAY_PICTURES_OUTSTANDING,
    );
    assert_eq!(
        first + second,
        PANES,
        "non-vacuity: all {PANES} panes wanted a picture, so the {first} the \
         first frame asked for is a door and not the whole demand",
    );
}

/// **What the panes already hold spends the allowance**, so the door bounds
/// the pipe rather than one frame's burst.
///
/// The occupant layer's pictures are *held* — arrived, uploading, not yet
/// delivered — which is `upload pending`'s own state, and there are exactly
/// as many of them as the allowance. The asking layer then gets nothing,
/// though every pane wants one and no ask of its own has gone out.
///
/// The control is the same scene with the holds one short of the allowance:
/// one picture is affordable and exactly one is asked for. So this reads the
/// *count* of what is outstanding, not merely "something is outstanding".
#[test]
fn pictures_already_in_the_pipe_spend_the_allowance() {
    for held in [
        MAX_OVERLAY_PICTURES_OUTSTANDING - 1,
        MAX_OVERLAY_PICTURES_OUTSTANDING,
    ] {
        let mut h = scene();
        h.gui_mut().enable_overlay_for_test(&OCCUPANT);
        for idx in 0..held {
            let picture = raster(&h, &format!("occupant-{idx}"));
            h.gui_mut().panes_mut()[idx]
                .overlay_cache_mut(&OCCUPANT)
                .hold(picture, None);
        }
        let outstanding: usize = h
            .gui()
            .panes()
            .iter()
            .map(crate::pane::PaneState::overlay_pictures_outstanding)
            .sum();
        assert_eq!(
            outstanding, held,
            "fixture: {held} panes were put in the pipe and the walk counts \
             {outstanding}",
        );

        h.frame_after(FRAME_DT);
        let asked = rasterizes_requested(&h, &ASKING);
        assert_eq!(
            asked,
            MAX_OVERLAY_PICTURES_OUTSTANDING - held,
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
    let mut h = scene();
    for idx in 0..PANES {
        let picture = raster(&h, &format!("held-{idx}"));
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
    let mut h = scene();
    for idx in 0..PANES {
        let picture = raster(&h, &format!("radar-{idx}"));
        h.gui_mut().panes_mut()[idx]
            .overlay_cache_mut(&known::RADAR)
            .hold(picture, None);
    }
    let outstanding: usize = h
        .gui()
        .panes()
        .iter()
        .map(crate::pane::PaneState::overlay_pictures_outstanding)
        .sum();
    assert_eq!(
        outstanding, 0,
        "{PANES} panes hold a radar picture and the overlay walk counted \
         {outstanding} of them",
    );

    h.frame_after(FRAME_DT);
    let asked = rasterizes_requested(&h, &ASKING);
    assert_eq!(
        asked, MAX_OVERLAY_PICTURES_OUTSTANDING,
        "the whole overlay allowance should have been free, and {asked} \
         pictures were asked for",
    );
}
