/// The body of `setup_egui_frame`.
fn setup_body() -> &'static str {
    let (_, rest) = include_str!("../app_render.rs")
        .split_once("fn setup_egui_frame(")
        .expect("setup_egui_frame is no longer a method here");
    rest.split_once("\n    }")
        .map(|(body, _)| body)
        .expect("setup_egui_frame has no recognisable body")
}

/// Nothing a poller applies may land after the frame has been laid out.
///
/// A result applied afterwards misses the frame it was applied to, and
/// nothing schedules the one that would show it: the re-arm at the end of
/// `handle_redraw` covers a render still in flight, auto-poll and an active
/// loop, and the last result of a batch is none of those. With auto-poll off
/// it sat there, applied and unpresented, until a mouse move repainted.
#[test]
fn every_poller_runs_before_the_frame_is_laid_out() {
    let body = setup_body();
    let laid_out = body
        .find("self.gui.ui(")
        .expect("setup_egui_frame no longer lays out a frame");

    for poller in [
        "self.poll_render_results(",
        "self.poll_level3_results(",
        "self.poll_overlay_render_results(",
        "self.poll_loop_scan_list_results(",
        "self.poll_loop_scan_download_results(",
        // The Level III loop's two stages, listed here for the same reason
        // as the Level II pair: a pairing that lands after layout is a frame
        // that stays blank until something unrelated repaints.
        "self.poll_loop_l3_list_results(",
        "self.poll_loop_l3_fetch_results(",
        "self.poll_loop_render_results(",
        // A section is the slowest thing this app produces, so it is the
        // one most likely to be the last result of a batch — the exact case
        // the re-arm at the end of `handle_redraw` does not cover.
        "self.poll_section_results(",
    ] {
        let at = body
            .find(poller)
            .unwrap_or_else(|| panic!("{poller} is no longer called from setup_egui_frame"));
        assert!(
            at < laid_out,
            "{poller} applies its results after the frame has been laid \
                 out, so the last of a batch is not on screen until something \
                 unrelated repaints",
        );
    }
}
