use super::{SurfaceStatus, finish_then_acquire};

/// The two facts about refused surfaces this file still has to test: the
/// status `acquire` returns comes back to the caller, and egui stays fully
/// usable — pending scale changes included — after any number of refused
/// frames.
///
/// This file used to assert finish-before-acquire ordering with pass
/// counts. It no longer does, because the ordering is enforced by the type
/// signature and cannot be broken while the code compiles:
/// `finish_then_acquire` takes `finish_pass: impl FnOnce() -> P` and
/// `acquire: impl FnOnce(&P) -> SurfaceStatus`, so the only `&P` that
/// `acquire` can be handed is the one `finish_pass` produced — `P` flows
/// from finish into acquire, and data flow, not statement order, carries
/// the requirement. The production wiring at `app_render.rs:1929` is
/// type-enforced the same way: `get_surface_texture` demands the
/// `&PreparedFrame` that only `end_pass_and_upload` returns, so acquiring
/// there without a finished pass fails to compile too.
#[test]
fn a_refused_surface_reaches_the_caller_and_scale_changes_still_apply() {
    let ctx = egui::Context::default();

    // The status the closure returns is the status the caller sees — this
    // is the value production matches on to decide to skip or reconfigure.
    ctx.begin_pass(egui::RawInput::default());
    let (_finished, status) = finish_then_acquire(|| ctx.end_pass(), |_| SurfaceStatus::Lost);
    assert!(matches!(status, SurfaceStatus::Lost));

    // And after frames the surface refused, a pending zoom/scale change
    // must still apply. egui only consumes one when it believes it is on
    // the outermost viewport — `begin_pass` pushes onto the viewport stack
    // and only `end_pass` pops it — so a single pass left open by a skipped
    // frame would mean a window moved to a different-DPI monitor never
    // rescales again. This asserts on a value the production path actually
    // reads back: `end_pass_and_upload` tessellates at
    // `ctx.pixels_per_point()`.
    for _ in 0..3 {
        ctx.begin_pass(egui::RawInput::default());
        let _ = finish_then_acquire(|| ctx.end_pass(), |_| SurfaceStatus::Skip);
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
