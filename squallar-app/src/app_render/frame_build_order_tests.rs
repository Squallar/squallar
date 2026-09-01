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
#[test]
fn every_poller_runs_before_the_frame_is_laid_out() {
    let body = setup_body();
    let at = |needle: &str| {
        body.find(needle)
            .unwrap_or_else(|| panic!("{needle} is no longer called from setup_egui_frame"))
    };
    let apply = at("self.run_frame_pump(PumpPhase::Apply");
    let advance = at("self.run_frame_pump(PumpPhase::Advance");
    let dispatch = at("self.run_frame_pump(PumpPhase::Dispatch");
    let inputs = at("self.push_frame_inputs(");
    let laid_out = at("self.gui.ui_phased(");

    assert!(
        apply < advance,
        "the playback advance runs before results-apply, so a frame's last \
         result is not in the frame that advances onto it",
    );
    assert!(
        advance < dispatch,
        "dispatch runs before the playback advance, so the dispatchers \
         measure a budget being spent on stale panes",
    );
    assert!(
        dispatch < inputs,
        "the frame's facts are composed before the pump has finished, so \
         the UI reads half a frame's worth of arrivals",
    );
    assert!(
        inputs < laid_out,
        "the frame is laid out before its facts are applied, so every \
         poller's results are one frame late",
    );

    // The `Ingest` half: `poll_data_channels` runs the pump's ingest rows at
    // `handle_redraw`'s earlier moment, before the renderer-state early returns and before
    // layout begins at all.
    let (_, redraw) = include_str!("../app.rs")
        .split_once("fn handle_redraw(")
        .expect("handle_redraw is gone from app.rs");
    let poll = redraw
        .find("self.poll_data_channels(")
        .expect("handle_redraw no longer drains the data channels");
    let setup = redraw
        .find("self.setup_egui_frame(")
        .expect("handle_redraw no longer lays out a frame");
    assert!(
        poll < setup,
        "the frame is laid out before the ingest drains have applied \
         anything, so an arrival waits a frame to be drawn",
    );
}
