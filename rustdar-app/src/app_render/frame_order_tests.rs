use super::{SurfaceStatus, finish_then_acquire};

#[test]
fn a_refused_surface_reaches_the_caller_and_scale_changes_still_apply() {
    let ctx = egui::Context::default();

    ctx.begin_pass(egui::RawInput::default());
    let (_finished, status) = finish_then_acquire(|| ctx.end_pass(), |_| SurfaceStatus::Lost);
    assert!(matches!(status, SurfaceStatus::Lost));

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
