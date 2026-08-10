use super::{SurfaceStatus, finish_then_acquire};

/// Drive one frame whose surface acquisition fails.
///
/// `status` ignores the finished pass it is handed; production uses that
/// argument to make acquiring without one a compile error.
fn skipped_frame(ctx: &egui::Context, status: fn(&egui::FullOutput) -> SurfaceStatus) {
    ctx.begin_pass(egui::RawInput::default());
    let (_finished, _status) = finish_then_acquire(|| ctx.end_pass(), status);
}

/// A frame that cannot acquire a surface must still end egui's pass.
///
/// `cumulative_pass_nr` is incremented by `Context::end_pass` and by nothing
/// else, so it counts passes that actually completed. That is what tells
/// "the pass ended and then the frame was abandoned" apart from "the frame
/// was abandoned with the pass still open" — and only the second one leaks.
#[test]
fn a_lost_surface_still_ends_the_egui_pass() {
    let ctx = egui::Context::default();

    ctx.begin_pass(egui::RawInput::default());
    assert_eq!(ctx.cumulative_pass_nr(), 0, "pass is open, not yet ended");

    let (_finished, status) = finish_then_acquire(|| ctx.end_pass(), |_| SurfaceStatus::Lost);

    assert!(matches!(status, SurfaceStatus::Lost));
    assert_eq!(
        ctx.cumulative_pass_nr(),
        1,
        "the pass must be ended even though the surface was lost"
    );
}

/// Repeated surface failures must not accumulate open passes.
#[test]
fn every_skipped_frame_completes_its_pass() {
    let ctx = egui::Context::default();
    const FRAMES: u64 = 5;

    for _ in 0..FRAMES {
        skipped_frame(&ctx, |_| SurfaceStatus::Skip);
    }

    assert_eq!(
        ctx.cumulative_pass_nr(),
        FRAMES,
        "each skipped frame should have completed exactly one pass"
    );
}

/// The user-visible half of the leak.
///
/// egui only consumes a pending zoom/scale change when it believes it is on
/// the outermost viewport, and it stops believing that the moment one pass
/// is left open — `begin_pass` pushes onto the viewport stack and only
/// `end_pass` pops it. So a window moved to a different-DPI monitor after
/// any skipped frame would never rescale again.
///
/// This asserts on a value the production path actually reads back:
/// `end_pass_and_upload` tessellates at `ctx.pixels_per_point()`.
#[test]
fn scale_changes_still_apply_after_frames_the_surface_refused() {
    let ctx = egui::Context::default();

    for _ in 0..3 {
        skipped_frame(&ctx, |_| SurfaceStatus::Skip);
    }

    ctx.set_pixels_per_point(2.0);
    ctx.begin_pass(egui::RawInput::default());
    let applied = ctx.pixels_per_point();
    let _ = ctx.end_pass();

    assert_eq!(
        applied, 2.0,
        "a scale set after skipped frames must still reach the next pass"
    );
}
