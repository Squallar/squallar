//! **What a wait asks of the frame loop, and what it raises** — the four sites
//! that drew egui's spinner and the map's loading plate, driven over frames
//! with the wait pending.
//!
//! Every test here reads two things off a frame: the register
//! `crate::frame_need` kept for it, and the soonest repaint the pass asked for
//! ([`InputHarness::repaint_delay`]). A spinner is a `ZERO` on the second — it
//! asks for the next frame now — and a bare tick is a zero on the first: a
//! frame bought and nothing named. The property held throughout is the status
//! bar chip's: a frame is called necessary when the words moved, and asked for
//! only where a number is about to.
//!
//! The wall clock is real in these fixtures (`listing_wait` reads
//! `Instant::now`), so where a counter is involved the assertion is the
//! **relation** frame by frame — raised exactly when the words changed — and
//! the count is forced by moving the wait's start a whole second.
//!
//! Two things about the readings. Frames are driven with [`InputHarness::frame_after`]
//! rather than `frame`, because a frame that does not advance egui's clock
//! never finishes egui's own container fades, and a loop driven by `frame`
//! alone reads its own warm-up as the app holding the frame loop awake — the
//! containers ask for `ZERO` from `area.rs` and the reading is about them.
//! [`settle`] spends that warm-up first and says so if it cannot. And
//! [`askers`] reports the pass **before** last (`Context::repaint_causes`
//! returns `prev_causes`), which is why it is only read inside a steady loop,
//! where every pass is the pass before it.

use super::InputHarness;
use super::loop_overlay_draw_tests::{LAYER, model_loop, scrub_to, ts};
use crate::frame_need::{NeedCause, take};
use crate::pane::{LoopFrame, LoopPhase};
use squallar_source::id::known;
use std::time::Duration;

/// The step the driving loops advance egui's clock by: one frame at 60 Hz.
const FRAME: f64 = 1.0 / 60.0;

/// One frame, with the register read for that frame alone, as `cause`'s bit.
fn judged(h: &mut InputHarness, cause: NeedCause) -> bool {
    let _ = take();
    h.frame_after(FRAME);
    take() & cause.bit() != 0
}

/// Who asked for a frame, as `file:line` — the pass before last, per the
/// module note.
fn askers(h: &InputHarness) -> Vec<String> {
    h.ctx()
        .repaint_causes()
        .iter()
        .map(|cause| format!("{}:{}", cause.file, cause.line))
        .collect()
}

/// Every asker is a source of this crate's own — nothing in `egui` asked. The
/// widget this lane took out asked from `egui`'s own tree, so this is the
/// reading that says a wait is not being kept alive by something the ledger
/// cannot name.
fn only_this_crate_asked(h: &InputHarness) -> bool {
    askers(h).iter().all(|a| a.starts_with("squallar-egui/"))
}

/// Spend egui's start-up and its containers' fades, so what the loop after
/// this reads is the wait rather than the warm-up.
///
/// A fresh context is built with a repaint outstanding and egui's areas fade
/// in over the first frames of clock they are given; both read as `ZERO` and
/// neither is the wait. Driven until a pass asks for something other than the
/// next frame *now* — which is `MAX` where the wait asks for nothing and the
/// rest of the second where a counter is printing. A wait that never gets
/// there is the spin these tests exist to catch, so this says so rather than
/// leaving the loop to read it.
fn settle(h: &mut InputHarness) {
    for _ in 0..60 {
        h.frame_after(FRAME);
        if h.repaint_delay() != Duration::ZERO {
            return;
        }
    }
    panic!(
        "sixty frames and the pass still asks for the next one now; asked by {:?}",
        askers(h),
    );
}

/// The loading plate's caption on pane 0's glass this frame, if one is up.
fn plate_text(h: &InputHarness) -> Option<String> {
    h.painted_text_strings_in(h.pane_rects()[0])
        .into_iter()
        .find(|t| t.starts_with("loading frames") || (t.contains("of") && t.ends_with("loading")))
}

/// The transport's listing line on the glass this frame, if one is up.
fn listing_line(h: &InputHarness) -> Option<String> {
    h.painted_text_strings_in(h.screen_rect())
        .into_iter()
        .find(|t| t.starts_with("Loading scan list..."))
}

/// A wake that is the rest of a second: what a counter printing whole seconds
/// may ask for, and nothing else — zero is the spin, more than a second skips
/// a number.
fn is_a_seconds_tick(delay: Duration) -> bool {
    !delay.is_zero() && delay <= Duration::from_secs(1)
}

/// Put pane 0's `layer` loop into the listing wait, `since` ago, with no
/// frames held — a refill after a deep scrub.
fn listing_out_for(h: &mut InputHarness, layer: &squallar_source::id::LayerId, since: Duration) {
    let pane = &mut h.gui_mut().panes_mut()[0];
    let ls = pane.time_state_mut(layer);
    ls.phase = LoopPhase::FetchingScanList;
    ls.listing_since = Some(
        web_time::Instant::now()
            .checked_sub(since)
            .expect("the process has been up longer than the fixture's wait"),
    );
    ls.frames.clear();
    pane.settle_playheads();
}

/// Move the listing's start one whole second further back, so the next frame's
/// counter prints a number nobody has seen whatever the wall clock does.
fn one_more_second_out(h: &mut InputHarness, layer: &squallar_source::id::LayerId) {
    let ls = h.gui_mut().panes_mut()[0].time_state_mut(layer);
    let since = ls.listing_since.expect("precondition: the listing is out");
    ls.listing_since = Some(
        since
            .checked_sub(Duration::from_secs(1))
            .expect("the process has been up longer than the fixture's wait"),
    );
}

/// **The loading plate's listing counter raises the clock cause exactly on the
/// frames its number moves, and asks for the frame that lands where it moves
/// next.**
///
/// The plate re-armed a flat one-second repaint on every frame it was drawn
/// and raised nothing: every frame that tick bought — the ones that showed a
/// new number included — read as waste. Now the ask is the rest of the current
/// second and the cause follows the words. Compact, so no status bar chip is
/// restating the clock beside the plate.
#[test]
fn the_loading_plates_listing_counter_raises_the_clock_cause_exactly_when_its_number_moves() {
    let mut h = InputHarness::with_screen(egui::vec2(400.0, 800.0));
    let (_live, _frames) = model_loop(&mut h);
    listing_out_for(&mut h, &LAYER, Duration::from_secs(5));
    h.warm_up();
    settle(&mut h);
    let mut last = plate_text(&h).expect("precondition: the listing wait is on the glass");
    assert!(
        last.starts_with("loading frames - "),
        "precondition: the plate is not in its listing arm: {last:?}",
    );

    for i in 0..30 {
        let raised = judged(&mut h, NeedCause::Clock);
        let text = plate_text(&h).expect("the plate went away mid-wait");
        assert_eq!(
            raised,
            text != last,
            "frame {i}: the plate drew {text:?} after {last:?} and {} the clock cause",
            if raised { "raised" } else { "did not raise" },
        );
        let asked = h.repaint_delay();
        assert!(
            is_a_seconds_tick(asked),
            "frame {i}: the counter asked for a {asked:?} wake; a second is how \
             fast it moves, zero is the spin, and more skips a number",
        );
        assert!(
            only_this_crate_asked(&h),
            "frame {i}: the plate's frame was asked for from outside this crate \
             by {:?} — a widget buying its own frames raises nothing the ledger \
             can name",
            askers(&h),
        );
        last = text;
    }

    // Exactly one change, forced.
    one_more_second_out(&mut h, &LAYER);
    assert!(
        judged(&mut h, NeedCause::Clock),
        "the number moved and no clock cause was raised",
    );
    let moved = plate_text(&h).expect("the plate went away on the forced change");
    assert_ne!(moved, last, "the fixture did not move the number");
    // …and redrawing the moved number is waste again, until it moves again.
    let mut last = moved;
    for i in 0..10 {
        let raised = judged(&mut h, NeedCause::Clock);
        let text = plate_text(&h).expect("the plate went away after the change");
        assert_eq!(
            raised,
            text != last,
            "frame {i} after the change: drew {text:?} after {last:?} and {} the cause",
            if raised { "raised" } else { "did not raise" },
        );
        last = text;
    }
}

/// **A pane parked on an owed frame asks for no frame and raises nothing.**
///
/// The frame arm's caption moves when the picture lands — an arrival, which
/// raises its own cause and wakes its own frame — or when the playhead moves,
/// which is input or playback and likewise. The plate re-armed its one-second
/// tick in this arm too, which bought a frame a second for as long as a frame
/// was owed, and on a render that never answers, for ever.
#[test]
fn a_pane_parked_on_an_owed_frame_asks_for_no_frame_and_raises_nothing() {
    let mut h = InputHarness::with_screen(egui::vec2(400.0, 800.0));
    let (_live, _frames) = model_loop(&mut h);
    {
        let ls = h.gui_mut().panes_mut()[0].time_state_mut(&LAYER);
        ls.frames[1].image = None;
        ls.phase = LoopPhase::Rendering;
    }
    scrub_to(&mut h, ts(15));
    settle(&mut h);
    assert_eq!(
        plate_text(&h).as_deref(),
        Some("frame 2 of 3 loading"),
        "precondition: the owed frame's caption is up",
    );

    for i in 0..30 {
        assert!(
            !judged(&mut h, NeedCause::Clock),
            "frame {i}: an owed frame's caption restates no clock and raised the clock cause",
        );
        assert_eq!(
            h.repaint_delay(),
            Duration::MAX,
            "frame {i}: a pane parked on an owed frame is holding the frame loop \
             awake; asked by {:?}",
            askers(&h),
        );
    }
    assert_eq!(
        plate_text(&h).as_deref(),
        Some("frame 2 of 3 loading"),
        "the plate went away, so the thirty zeros above are about an empty glass",
    );
}

/// **The status bar's fetch asks for no frame while the download is out.**
///
/// The bar drew egui's spinner beside "Downloading" for the whole archive
/// fetch — a frame at the display's rate, none of them named, for as long as
/// the network took (the archive client's timeout is five minutes). The fetch's
/// answer comes back through a channel the app drains, from a worker that
/// posts its own wake; the bar has nothing to ask for. The control is the
/// chip coming back when the fetch ends: its countdown asks for its second,
/// which is what says the harness can read an ask at all.
#[test]
fn the_status_bars_fetch_asks_for_no_frame_while_the_download_is_out() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.load_scan("KTLX");
    h.gui_mut()
        .apply(crate::shell_api::GuiEvent::Fetching(true));
    h.warm_up();
    settle(&mut h);
    assert!(
        h.text_painted_in(h.screen_rect(), "Downloading"),
        "precondition: the bar is showing the fetch",
    );
    assert!(
        h.status_bar().poll_chip.is_none(),
        "precondition: the chip yields to the fetch, so nothing on the bar \
         restates the clock while the download is out",
    );

    for i in 0..30 {
        h.frame_after(FRAME);
        assert_eq!(
            h.repaint_delay(),
            Duration::MAX,
            "frame {i}: the bar asked for a frame with the download out and \
             nothing new to show; asked by {:?}",
            askers(&h),
        );
    }

    h.gui_mut()
        .apply(crate::shell_api::GuiEvent::Fetching(false));
    settle(&mut h);
    assert!(
        h.status_bar().poll_chip.is_some(),
        "control: the fetch ended and the chip did not come back",
    );

    // The control is on the instrument rather than on the bar: this fixture's
    // chip has no countdown to run — no poll round is owed — so what has to be
    // ruled out is a harness that reports `MAX` whatever was asked for. An ask
    // put on this very context reaches the reading. It is the immediate ask,
    // not a delayed one, because egui drops a delay asked for between passes
    // and carries an immediate one as an outstanding repaint; either way what
    // is being shown is that a `MAX` here is the bar asking for nothing.
    h.ctx().request_repaint();
    h.frame_after(FRAME);
    assert_eq!(
        h.repaint_delay(),
        Duration::ZERO,
        "control: a repaint asked for on this context did not reach the \
         reading, so the MAX readings above may be a harness that cannot read \
         an ask",
    );
}

/// The wide transport with row 2 open and a radar loop in `phase`, three
/// frames deep, none rendered — `radars_rendering_line_is_unchanged_beside_the_new_notice`'s
/// fixture, with the status bar collapsed first so the one legitimate tick on
/// this screen (the chip's countdown) is not what the readings below see.
fn radar_transport_in(phase: LoopPhase, in_flight: bool) -> InputHarness {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.mouse_click(h.status_bar().collapse.center());
    h.warm_up();
    assert!(
        h.status_bar().collapsed,
        "fixture: the status bar did not collapse, so its countdown is still ticking",
    );
    h.mouse_click(h.timeline().expander.center());
    h.warm_up();
    {
        let pane = &mut h.gui_mut().panes_mut()[0];
        let ls = pane.time_state_mut(&known::RADAR);
        ls.phase = phase;
        ls.frames = (0..3)
            .map(|i| LoopFrame {
                timestamp: ts(-30 + i * 10),
                image: None,
                render_in_flight: in_flight,
                render_failed: false,
            })
            .collect();
        pane.settle_playheads();
    }
    h.warm_up();
    h
}

/// **The transport's render count asks for no frame.** Its number and the bar
/// under it move when a render lands, and a landing buys its own frame; the
/// spinner beside it bought one every frame in between.
#[test]
fn the_transports_render_count_asks_for_no_frame() {
    let mut h = radar_transport_in(LoopPhase::Rendering, true);
    settle(&mut h);
    let row2 = h.timeline().row2.expect("the expander must open row 2");
    assert_eq!(
        row2.rendered_text, "Rendering 0/3...",
        "precondition: the render count is up",
    );

    for i in 0..30 {
        h.frame_after(FRAME);
        assert_eq!(
            h.repaint_delay(),
            Duration::MAX,
            "frame {i}: the transport asked for a frame with the renders out and \
             nothing new to show; asked by {:?}",
            askers(&h),
        );
    }
    assert!(
        h.text_painted_in(h.screen_rect(), "Rendering 0/3..."),
        "the render count left the glass, so the zeros above are about a blank row",
    );
}

/// **The transport's listing counter raises the clock cause exactly on the
/// frames its number moves, and asks for the frame that lands where it moves
/// next** — the plate's property, on the other counter that prints the same
/// wait.
///
/// Both are on this screen, both read the clock on their own reads, and the
/// cause is one bit, so the frame-by-frame relation below is held against the
/// **union** of their words: either number moving is what a raise means there.
/// That relation alone would pass on the plate's raise with this counter
/// naming nothing, so the tail separates them — this counter's memory of the
/// words it drew is the `Gui`'s own, and forgetting it leaves this counter,
/// and only this counter, with words it has not drawn before on a frame where
/// the plate's number has not moved.
#[test]
fn the_transports_listing_counter_raises_the_clock_cause_exactly_when_its_number_moves() {
    let mut h = radar_transport_in(LoopPhase::FetchingScanList, false);
    listing_out_for(&mut h, &known::RADAR, Duration::from_secs(5));
    h.warm_up();
    settle(&mut h);
    let words = |h: &InputHarness| (listing_line(h), plate_text(h));
    let mut last = words(&h);
    assert!(
        last.0.as_deref().is_some_and(|t| t.ends_with('s')) && last.1.is_some(),
        "precondition: both counters are up: {last:?}",
    );

    for i in 0..30 {
        let raised = judged(&mut h, NeedCause::Clock);
        let now = words(&h);
        assert_eq!(
            raised,
            now != last,
            "frame {i}: the glass went from {last:?} to {now:?} and {} the clock cause",
            if raised { "raised" } else { "did not raise" },
        );
        let asked = h.repaint_delay();
        assert!(
            is_a_seconds_tick(asked),
            "frame {i}: the counters asked for a {asked:?} wake",
        );
        assert!(
            only_this_crate_asked(&h),
            "frame {i}: the wait's frame was asked for from outside this crate \
             by {:?}",
            askers(&h),
        );
        last = now;
    }

    one_more_second_out(&mut h, &known::RADAR);
    assert!(
        judged(&mut h, NeedCause::Clock),
        "both numbers moved and no clock cause was raised",
    );
    let moved = words(&h);
    assert_ne!(moved, last, "the fixture did not move the numbers");
    let mut last = moved;
    for i in 0..10 {
        let raised = judged(&mut h, NeedCause::Clock);
        let now = words(&h);
        assert_eq!(
            raised,
            now != last,
            "frame {i} after the change: {last:?} to {now:?} and {} the cause",
            if raised { "raised" } else { "did not raise" },
        );
        last = now;
    }

    // And the raise is this counter's, not the plate's beside it. Retried
    // because the wall clock may turn the second on the very frame the memory
    // is forgotten, which moves the plate's number too and puts the tie back;
    // a run that never gets a frame to itself says so rather than reporting
    // the tie as the property.
    let mut told_apart = false;
    for _ in 0..8 {
        let plate_before = plate_text(&h);
        h.gui_mut().forget_timeline_wait_text_for_test();
        let raised = judged(&mut h, NeedCause::Clock);
        if plate_text(&h) != plate_before {
            continue;
        }
        assert!(
            raised,
            "the counter drew words it had no memory of drawing, with the \
             plate's number standing still beside it, and raised no clock \
             cause: the raises above are the plate's",
        );
        told_apart = true;
        break;
    }
    assert!(
        told_apart,
        "eight tries and the plate's number moved on every one, so nothing \
         here separates the two counters",
    );
}

/// **The offline download's block credits the arrival its figures show, once
/// per change, and asks for no frame.**
///
/// The engine wakes a frame on its own events through `Context::request_repaint`
/// rather than a channel the app drains, so nothing on that path raised a
/// cause: every frame a landed plan or a landed tile bought read as waste
/// while showing new figures, and the spinner beside the planning line bought
/// one every frame for the minutes a large area's plan takes to cut. Driven on
/// a bare context — the block is one function both surfaces call, and the
/// progress value is plain data.
#[test]
fn the_download_block_credits_the_arrival_once_per_change_and_asks_for_no_frame() {
    use crate::basemap_download::DownloadProgress;
    use squallar_units::DataSize;

    /// Megabytes, because [`DataSize::label`] prints them: the line is what
    /// the block draws, and bytes that move under the label's resolution move
    /// no words. That is the same rule the status bar chip holds against
    /// `describe_age`'s "just now", and it is asserted below rather than
    /// avoided.
    fn progress(segments_total: u32, megabytes_done: u64) -> DownloadProgress {
        DownloadProgress {
            tiles_done: 0,
            tiles_total: 10,
            bytes_done: DataSize::from_bytes(megabytes_done * 1_000_000),
            bytes_total: DataSize::from_bytes(8_000_000),
            segments_done: 0,
            segments_total,
            peak_held: DataSize::from_bytes(0),
        }
    }
    /// One pass drawing the block: whether the arrival cause was raised, what
    /// the pass asked for, and the words it drew.
    ///
    /// One context across the passes, as the app has: a fresh one is built
    /// with a repaint outstanding and reports `ZERO` for its first pass
    /// whatever is drawn on it, so a block measured on a context per pass
    /// cannot be told from one that asks every frame. `warm` below spends it.
    fn pass(
        ctx: &egui::Context,
        last: &mut Option<String>,
        p: DownloadProgress,
    ) -> (bool, Duration, Vec<String>) {
        let _ = take();
        let out = ctx.run_ui(egui::RawInput::default(), |ui| {
            crate::ui_download_area::render_download_progress(ui, p, last);
        });
        let asked = out
            .viewport_output
            .values()
            .map(|v| v.repaint_delay)
            .min()
            .unwrap_or(Duration::MAX);
        let drawn = out
            .shapes
            .iter()
            .filter_map(|clipped| match &clipped.shape {
                egui::Shape::Text(text) => Some(text.galley.text().to_owned()),
                _ => None,
            })
            .collect();
        (take() & NeedCause::Arrival.bit() != 0, asked, drawn)
    }

    let ctx = egui::Context::default();
    let _ = ctx.run_ui(egui::RawInput::default(), |_| {});
    assert_eq!(
        ctx.run_ui(egui::RawInput::default(), |_| {})
            .viewport_output
            .values()
            .map(|v| v.repaint_delay)
            .min(),
        Some(Duration::MAX),
        "fixture: an empty pass on this context still asks for a frame, so the \
         readings below are not about the block",
    );

    let mut last = None;
    // The planning phase appears: one arrival, no ask; a redraw of it is neither.
    let (raised, asked, drawn) = pass(&ctx, &mut last, progress(0, 0));
    assert!(raised, "the block appeared and credited nothing");
    assert_eq!(asked, Duration::MAX, "the planning line asked for a frame");
    assert!(
        drawn
            .iter()
            .any(|t| t == crate::ui_download_area::PREPARING_LABEL),
        "precondition: the planning line is not what was drawn: {drawn:?}",
    );
    for i in 0..10 {
        let (raised, asked, _) = pass(&ctx, &mut last, progress(0, 0));
        assert!(
            !raised,
            "redraw {i} of the planning line was credited as an arrival"
        );
        assert_eq!(
            asked,
            Duration::MAX,
            "redraw {i} of the planning line asked for a frame"
        );
    }
    // The plan lands: one arrival. The same figures: none. Bytes move: one.
    let (raised, asked, drawn) = pass(&ctx, &mut last, progress(4, 0));
    assert!(raised, "the plan landed and the block credited nothing");
    assert_eq!(asked, Duration::MAX);
    assert!(
        drawn.iter().any(|t| t.contains("fetched this run")),
        "the plan landed and the byte line is not up: {drawn:?}",
    );
    assert!(
        !pass(&ctx, &mut last, progress(4, 0)).0,
        "unchanged figures were credited",
    );
    assert!(
        pass(&ctx, &mut last, progress(4, 2)).0,
        "the byte line moved and nothing was credited",
    );
    assert!(
        !pass(&ctx, &mut last, progress(4, 2)).0,
        "unchanged figures were credited",
    );
    // And bytes that land under what the line prints are not an arrival the
    // glass can show: the words are the picture, so the frame stays waste.
    let under_the_label = DownloadProgress {
        bytes_done: DataSize::from_bytes(2_000_001),
        ..progress(4, 2)
    };
    assert!(
        !pass(&ctx, &mut last, under_the_label).0,
        "a byte that moved no words was credited as an arrival",
    );
}
