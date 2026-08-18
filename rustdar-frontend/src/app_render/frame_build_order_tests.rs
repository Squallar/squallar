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
///
/// Since WO-E3 the pollers are `FRAME_PUMP` rows, so the literal list this
/// test used to walk lives in the table and its order is pinned by
/// `frame_pump::tests::the_pump_rows_are_in_the_pinned_order`. What is left
/// to hold here is the run moments: the three phase runners, in phase
/// order, before the frame's facts are composed and before `Gui::ui` lays
/// the frame out — and the `Ingest` phase a whole call earlier, in
/// `handle_redraw`, before layout is even reachable.
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
    let laid_out = at("self.gui.ui(");

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

    // The `Ingest` half: `poll_data_channels` runs the pump's ingest rows
    // at `handle_redraw`'s earlier moment, before the renderer-state early
    // returns and before layout begins at all.
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
