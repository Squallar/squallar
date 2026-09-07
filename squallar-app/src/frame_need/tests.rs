//! What the unnecessary-frame verdict has to get right, and the two ways it
//! can be wrong.
//!
//! **Under-reporting** is a frame that needed nothing being called necessary —
//! the failure a circular denominator produces, and the one the positive
//! control below exists to catch. **Over-reporting** is a frame that genuinely
//! needed drawing being called unnecessary, which is the worse of the two: it
//! is what makes an instrument unbelievable, and an unbelievable instrument is
//! ignored. Both directions are held here.

use super::{NeedLedger, WakeClaim};
use crate::app::{RepaintAction, repaint_action};
use squallar_egui::frame_need::{NeedCause, note, take};

/// A ledger with the process register cleared under it, so a fixture's frames
/// are judged on causes the fixture itself raised.
///
/// The register is per-thread in a test build (see
/// `squallar_egui::frame_need::sink`), and libtest gives each test its own
/// thread, so this clears only this test's own.
fn fresh() -> NeedLedger {
    let _ = take();
    NeedLedger::default()
}

/// **The instance-2 defect, built.**
///
/// The viewport clamp of `0e2161f4` stored the centre geographically and
/// re-read it through a projection, which costs about `1e-10` points. A map
/// resting against a bound was therefore out of bounds on **every** frame,
/// moved a ten-billionth of a point, and asked for a repaint forever. Nothing
/// in the tree could see it: no raster was dispatched, no command was
/// recorded, and every frame the ledger timed was a frame that ran.
///
/// This is that shape with the arithmetic real — a round trip that does not
/// round trip, compared with no deadband.
struct SelfNudgingMap {
    centre: f64,
    bound: f64,
    /// How many frames this map decided it had moved. The control's own
    /// non-vacuity: a fixture that stopped nudging would make every assertion
    /// below pass for the wrong reason.
    nudges: u32,
}

impl SelfNudgingMap {
    fn new() -> Self {
        Self {
            centre: 49.999_999_999_9,
            bound: 50.0,
            nudges: 0,
        }
    }

    /// One frame of the defect: re-read the centre through a projection, find
    /// it out of bounds by a ten-billionth, clamp, and ask for a repaint.
    /// Returns what the app's tail would have claimed.
    fn frame(&mut self) -> RepaintAction {
        // The round trip that does not round trip. Deliberately arithmetic
        // rather than a stubbed constant: the defect is that the error is
        // real and smaller than anything a reader would think to guard.
        let reprojected = ((self.centre * 1e7).round() / 1e7) + 1e-10;
        assert_ne!(
            reprojected, self.centre,
            "the fixture's projection round-trips exactly, so it cannot \
             reproduce the defect it is named for",
        );
        if reprojected > self.bound - 1e-9 {
            // No deadband: any excursion at all is a clamp, and a clamp is a
            // repaint. `Duration::ZERO` is what egui reports after a
            // `request_repaint()` inside the pass.
            self.centre = self.bound - 1e-10;
            self.nudges += 1;
            return repaint_action(std::time::Duration::ZERO);
        }
        repaint_action(std::time::Duration::MAX)
    }
}

/// **The positive control, and the instrument does not land without it.**
///
/// A map that re-triggers itself every frame with nothing changing must read
/// at or near 100 % unnecessary. It is also the non-circularity gate: this
/// fixture *requests* every repaint it wastes, so a denominator built from the
/// repaint ask — "egui wanted a frame, therefore the frame was needed" —
/// scores it 100 % **necessary** and this test goes red. That is the whole
/// design decision, held by a test rather than by a paragraph.
#[test]
fn a_map_that_nudges_itself_reads_wholly_unnecessary() {
    const FRAMES: u32 = 240;
    let mut ledger = fresh();
    let mut map = SelfNudgingMap::new();

    for _ in 0..FRAMES {
        // The frame's tail: the map nudged itself and egui was asked for an
        // immediate repaint, so nothing above it claimed and the charge falls
        // to `EguiNow`.
        let action = map.frame();
        assert_eq!(
            action,
            RepaintAction::Now,
            "the fixture stopped asking for repaints, so it is no longer the \
             defect this control is about",
        );
        ledger.record_wake_claim(WakeClaim::EguiNow);
        // No cause is raised: a clamp is not an input, not an arrival, not an
        // animation and not a surface change. The register is read through
        // the production spelling.
        ledger.record(take());
    }

    let r = ledger.reading();
    assert_eq!(
        map.nudges, FRAMES,
        "the fixture stopped nudging itself part way, so the reading below is \
         about a quiet map and not about the defect",
    );
    assert!(r.ran(), "no frame was judged at all");
    assert_eq!(r.drawn, u64::from(FRAMES));
    assert_eq!(
        r.needed, 0,
        "a frame with nothing changing was called necessary — the denominator \
         is agreeing with the defect it exists to find",
    );
    // Asserted against the extent of the run rather than against a threshold:
    // every frame this fixture drew was one it should not have drawn, so the
    // figure is the frame count and nothing else.
    assert_eq!(r.unnecessary(), u64::from(FRAMES));
    // Every frame but the first is charged to the immediate repaint that
    // bought it. The first is charged to `External` and correctly so: no tail
    // had yet run when it was judged, so nothing in this application had asked
    // for it — which is what a boot frame is. Stated as `FRAMES - 1` rather
    // than smoothed into a tolerance, because the one exception is a fact
    // about the claim carry and not noise.
    assert_eq!(
        (r.charge(WakeClaim::EguiNow), r.charge(WakeClaim::External),),
        (u64::from(FRAMES) - 1, 1),
        "the waste was not charged to the immediate repaint that bought it, \
         so a reader is pointed at the wrong code",
    );
    assert!(r.charges_balance());
    assert!(r.causes_cover_the_needed());
}

/// **The over-firing gate, one cause at a time.**
///
/// A frame that genuinely needed drawing must never be counted unnecessary.
/// Over-reporting is the worse direction here — it is what makes an instrument
/// ignored — so each cause is checked on its own rather than in a soup where
/// one working cause would carry the other three.
#[test]
fn a_frame_that_raised_any_cause_is_never_counted_unnecessary() {
    for cause in NeedCause::ALL {
        let mut ledger = fresh();
        // A claim stands, exactly as it would in a live app with a loop
        // playing: if the verdict ever consulted it, this frame would be
        // charged.
        ledger.record_wake_claim(WakeClaim::Loop);
        ledger.record(take());
        // ^ the first frame, before any tail — charged to `External`.
        note(cause);
        ledger.record(take());

        let r = ledger.reading();
        assert_eq!(r.drawn, 2);
        assert_eq!(
            r.needed,
            1,
            "a frame that raised `{}` was not counted as needing to be drawn",
            cause.name(),
        );
        assert_eq!(
            r.cause(cause),
            1,
            "the `{}` cause was raised and the reading does not show it",
            cause.name(),
        );
        assert_eq!(
            r.unnecessary(),
            1,
            "the frame that raised `{}` was counted unnecessary; only the \
             boot frame before it should be",
            cause.name(),
        );
        assert_eq!(r.charge(WakeClaim::External), 1);
        assert_eq!(r.charge(WakeClaim::Loop), 0);
    }
}

/// **A frame that raised several causes is one necessary frame, not several.**
///
/// The cause buckets overlap by design, so the only thing keeping `needed` a
/// frame count rather than a cause count is that the verdict is taken once.
#[test]
fn several_causes_on_one_frame_are_one_necessary_frame() {
    let mut ledger = fresh();
    note(NeedCause::Input);
    note(NeedCause::Arrival);
    note(NeedCause::Animation);
    ledger.record(take());

    let r = ledger.reading();
    assert_eq!(r.drawn, 1);
    assert_eq!(r.needed, 1, "one frame was counted as three");
    assert_eq!(r.unnecessary(), 0);
    assert_eq!(
        r.causes.iter().sum::<u64>(),
        3,
        "the three causes did not each record the frame they were raised on",
    );
    assert!(
        r.causes_cover_the_needed(),
        "the cause buckets no longer cover the frames called necessary",
    );
}

/// **A frame is charged to the claim that bought it, not the one it leaves.**
///
/// `handle_redraw`'s tail runs *before* the verdict, so the naive wiring
/// charges every frame to the claim it just made for the next one. That is
/// wrong in exactly the case that matters: the frame that finally receives an
/// answer clears the claim, and the poll frames before it would then be
/// charged to whatever came after them.
#[test]
fn a_frame_is_charged_to_the_claim_that_bought_it() {
    let mut ledger = fresh();

    // Frame 1: nobody had claimed anything yet.
    ledger.record_wake_claim(WakeClaim::Loop);
    ledger.record(take());
    // Frame 2: bought by frame 1's loop-active claim; its own tail now
    // claims something else entirely.
    ledger.record_wake_claim(WakeClaim::Gesture);
    ledger.record(take());
    // Frame 3: bought by frame 2's gesture claim.
    ledger.record_wake_claim(WakeClaim::EguiNow);
    ledger.record(take());

    let r = ledger.reading();
    assert_eq!(r.drawn, 3);
    assert_eq!(r.unnecessary(), 3);
    assert_eq!(
        (
            r.charge(WakeClaim::External),
            r.charge(WakeClaim::Loop),
            r.charge(WakeClaim::Gesture),
            r.charge(WakeClaim::EguiNow),
        ),
        (1, 1, 1, 0),
        "the charges are off by a frame: each one is being filed against the \
         claim its own tail made rather than against the claim that woke it",
    );
    assert!(r.charges_balance());
}

/// **A cause raised on a frame that never presented reaches the frame that
/// does.**
///
/// `handle_redraw` early-returns on a minimized window, a zero-area window and
/// a lost surface, and an arrival that landed on one of those has still not
/// been drawn. Clearing the register per redraw entry rather than per verdict
/// would throw it away and report the frame that finally draws it as waste.
#[test]
fn a_cause_survives_a_frame_that_drew_nothing() {
    let mut ledger = fresh();
    note(NeedCause::Arrival);
    // A `handle_redraw` that returned before presenting takes no verdict, so
    // it neither counts a frame nor clears the register.
    note(NeedCause::Input);
    ledger.record(take());

    let r = ledger.reading();
    assert_eq!(r.drawn, 1, "an unpresented frame was counted as drawn");
    assert_eq!(r.needed, 1);
    assert_eq!(
        (r.cause(NeedCause::Arrival), r.cause(NeedCause::Input)),
        (1, 1)
    );
}

/// **And the register is cleared past the last early return, not before it.**
///
/// The test above holds the ledger's half; this holds the caller's, and the
/// two are different mistakes. `FrameLedger::finalize` opens with three
/// guards — a frame that never reached the pass, a skipped or lost surface,
/// and a missing acquire — and every one of them returns without drawing. A
/// `take` above any of them counts an unpresented frame as drawn **and**
/// throws away causes nothing has shown, so the frame that finally shows them
/// reads as waste. That is the `publish at the seam` shape: a level sampled on
/// the wrong tick is a false zero for everything living inside it.
///
/// Held on the source text because the property is a position, and there is no
/// way to observe a position from inside a call that has already returned.
#[test]
fn the_cause_register_is_read_past_every_early_return_in_finalize() {
    const LEDGER: &str = include_str!("../frame_ledger.rs");
    let body = LEDGER
        .split_once("fn finalize(")
        .map(|(_, rest)| rest)
        .expect("finalize is no longer a method on FrameLedger");
    let at = |needle: &str| {
        body.find(needle)
            .unwrap_or_else(|| panic!("`{needle}` is gone from finalize"))
    };
    let take_at = at(concat!("frame_need::", "take()"));
    for guard in [
        // The frame never reached the pass.
        "// The frame early-returned before the pass; not a sample.",
        // The pass ended without a real present.
        "if m.skipped {",
        // No acquire and no present: nothing was shown.
        "(m.acquire, m.present_return)",
    ] {
        assert!(
            at(guard) < take_at,
            "the cause register is read before `{guard}`, so a frame that drew \
             nothing clears causes the next frame has still not shown — and \
             that frame is then reported as waste",
        );
    }
}

/// **The three groups on the line are three denominators**, and the two
/// identities that make them readable hold over a mixed run.
#[test]
fn the_line_s_three_groups_keep_their_own_identities() {
    let mut ledger = fresh();
    let script: [Option<NeedCause>; 8] = [
        None,
        Some(NeedCause::Input),
        None,
        Some(NeedCause::Arrival),
        Some(NeedCause::Arrival),
        None,
        Some(NeedCause::Animation),
        None,
    ];
    for (frame, cause) in script.iter().enumerate() {
        ledger.record_wake_claim(if frame % 2 == 0 {
            WakeClaim::Loop
        } else {
            WakeClaim::EguiNow
        });
        if let Some(cause) = cause {
            note(*cause);
        }
        ledger.record(take());
    }

    let r = ledger.reading();
    assert_eq!((r.drawn, r.needed, r.unnecessary()), (8, 4, 4));
    assert_eq!(
        r.needed + r.unnecessary(),
        r.drawn,
        "`needed` and `unnecessary` no longer partition `drawn`",
    );
    assert!(
        r.charges_balance(),
        "the charges do not sum to the unnecessary frames, so a frame reached \
         the verdict with no claim chosen",
    );
    assert!(r.causes_cover_the_needed());
    assert_eq!(
        (
            r.cause(NeedCause::Input),
            r.cause(NeedCause::Arrival),
            r.cause(NeedCause::Animation),
            r.cause(NeedCause::Surface),
        ),
        (1, 2, 1, 0),
    );
}

/// **Every claim has its own slot and its own name.** A duplicated index would
/// make one claim silently absorb another's count, and a duplicated name would
/// do it on the rig's side of the line.
#[test]
fn every_wake_claim_has_its_own_slot_and_name() {
    let mut indices: Vec<usize> = WakeClaim::ALL.iter().map(|c| c.index()).collect();
    indices.sort_unstable();
    assert_eq!(
        indices,
        (0..WakeClaim::COUNT).collect::<Vec<_>>(),
        "the claims do not occupy the slots the charge array has",
    );
    let mut names: Vec<&str> = WakeClaim::ALL.iter().map(|c| c.name()).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), WakeClaim::COUNT, "two claims share a name");
}

/// **The arrival cause has no bypass.**
///
/// Every channel drain on the frame thread must take its messages through
/// `ArrivalRecv::try_recv_arrival`, or a whole class of arrivals is invisible
/// and every frame that draws one is scored unnecessary. This is the
/// non-vacuity floor under the arrival figure: without it the cause could be
/// wired to one receiver of eighteen and every test above would still pass.
#[test]
fn every_frame_thread_drain_takes_its_messages_through_the_counted_spelling() {
    // Split so this file never contains the uncounted spelling contiguously,
    // on `arch_ratchets`' needle-hygiene terms.
    let plain = concat!("_receiver.try_", "recv()");
    let counted = concat!("_receiver.try_", "recv_arrival()");
    let mut offences = Vec::new();
    let mut counted_sites = 0usize;
    for path in scanned_app_sources() {
        let src = std::fs::read_to_string(&path).expect("source must be readable");
        counted_sites += src.matches(counted).count();
        if src.contains(plain) {
            offences.push(path.display().to_string());
        }
    }
    assert!(
        offences.is_empty(),
        "these frame-thread drains take messages without raising the arrival \
         cause, so a frame that draws what they delivered reads as waste: \
         {offences:?}",
    );
    // The floor: the pump's own inventory names the receivers a frame drains,
    // and the counted spelling has to reach at least that many of them.
    let drained = pump_drain_inventory();
    assert!(
        drained >= 15,
        "the pump inventory reads {drained} drained receivers — the walk that \
         counts them is broken, not the tree",
    );
    assert!(
        counted_sites >= drained,
        "{counted_sites} counted drains against {drained} receivers the frame \
         pump declares it drains: the arrival cause is wired to a subset",
    );
}

/// How many `ChannelHub` receiver fields the frame pump's own inventory says a
/// frame drains — the `drains:` lists in `frame_pump.rs`, which an
/// exhaustiveness test there already holds against the hub.
fn pump_drain_inventory() -> usize {
    let src = include_str!("../frame_pump.rs");
    let mut count = 0;
    let mut from = 0;
    while let Some(rel) = src[from..].find("_receiver\"") {
        count += 1;
        from += rel + 1;
    }
    count
}

/// This crate's sources, test files skipped by name — `ui_glyphs`' walk, and
/// the same reason: a test's own prose is not production code.
fn scanned_app_sources() -> Vec<std::path::PathBuf> {
    let mut roots = vec![std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")];
    let mut files = Vec::new();
    while let Some(dir) = roots.pop() {
        for entry in std::fs::read_dir(&dir).expect("source dir must be readable") {
            let path = entry.expect("dir entry").path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if path.is_dir() {
                roots.push(path);
            } else if name.ends_with(".rs") && !name.contains("test") {
                files.push(path);
            }
        }
    }
    assert!(
        files.len() > 20,
        "the scan found only {} sources — the walk is broken, not the tree",
        files.len(),
    );
    files
}

/// **The animation cause has no bypass either.**
///
/// egui's own widget animations are the one picture-mover neither an input nor
/// an arrival can see, and `squallar_egui::frame_need::animate_bool` is the
/// only spelling that raises the cause. A call written straight against
/// `Context` would animate a drawer while this instrument called every frame
/// of it waste — the over-report that makes an instrument ignored.
#[test]
fn the_animation_cause_has_no_bypass_in_the_ui_layer() {
    let needle = concat!("animate_bool_with", "_time(");
    let ui_src = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../squallar-egui/src");
    let mut roots = vec![ui_src];
    let mut sites = Vec::new();
    while let Some(dir) = roots.pop() {
        for entry in std::fs::read_dir(&dir).expect("source dir must be readable") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                roots.push(path);
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                let src = std::fs::read_to_string(&path).expect("source must be readable");
                let calls = src.matches(needle).count();
                if calls > 0 {
                    sites.push((path.display().to_string(), calls));
                }
            }
        }
    }
    let calls: usize = sites.iter().map(|(_, n)| n).sum();
    assert_eq!(
        calls, 1,
        "egui's animation is called from {calls} places; exactly one — the \
         wrapper in `squallar_egui::frame_need` — may call it, or an \
         animating picture reads as waste: {sites:?}",
    );
    assert!(
        sites[0].0.ends_with("frame_need.rs"),
        "the one call to egui's animation is no longer the wrapper that \
         raises the cause: {}",
        sites[0].0,
    );
}

/// The body of a free function or method named `head` in `source`, brace
/// balanced from its first `{`.
fn balanced_body(source: &str, head: &str) -> String {
    let start = source
        .find(head)
        .unwrap_or_else(|| panic!("`{head}` is gone"));
    let rest = &source[start..];
    let open = rest.find('{').expect("a body");
    let mut depth = 0usize;
    for (i, c) in rest[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return rest[open..open + i].to_string();
                }
            }
            _ => {}
        }
    }
    panic!("unbalanced braces in {head}");
}

/// **The texture upload queue has a claim of its own, and it is asked before
/// every other one.**
///
/// A banded upload buys its own frames: `end_pass_and_upload` zeroes the
/// frame's repaint delay while bands remain, so those frames happen whatever
/// the re-arm says. Asked last, [`WakeClaim::Upload`] would take only the
/// frames nothing else was standing for, and every claim above it would be
/// credited with frames the upload queue had already paid for — which is
/// precisely how deleting one of those claims reads as a saving while being a
/// relabelling. **This is the pin that keeps a later before/after honest**,
/// and it is a position, which is why it is held on the source text.
///
/// It also pins the other half: the upload claim posts nothing. It is folded
/// into the charge and left out of the `if … { notify_redraw }` guard, because
/// the renderer's own zero delay is what asks for that frame and a second ask
/// would be a wake nobody needed.
#[test]
fn the_upload_queue_names_its_own_claim_and_is_asked_before_the_rest() {
    const APP: &str = include_str!("../app.rs");
    // Presence control: the renderer still publishes the fact this reads. A
    // pin over a predicate that no longer exists would pass over nothing.
    const RENDERER: &str = include_str!("../../../squallar-gpu/src/egui_renderer.rs");
    assert!(
        RENDERER.contains("pub fn uploads_pending(&self) -> bool"),
        "the renderer no longer publishes whether upload bands are pending, \
         so the upload claim below is reading something else",
    );

    let body = balanced_body(APP, "fn handle_redraw(");
    let at = |needle: &str| {
        body.find(needle)
            .unwrap_or_else(|| panic!("`{needle}` is gone from handle_redraw"))
    };
    let upload = at("let upload_claim = ");
    let rearm = at("let wake_claim = ");
    assert!(
        upload < rearm,
        "the upload claim is asked after the re-arm chain, so every claim in \
         that chain is credited with frames the upload queue had already \
         bought and a removal there cannot be told from a rename",
    );
    assert!(
        body[upload..rearm].contains("uploads_pending()"),
        "the upload claim is no longer read from the renderer's pending \
         bands, so it is standing for something else",
    );
    // The fold itself, read between its own parentheses rather than by first
    // occurrence: `let upload_claim = …` is earlier in the body whatever the
    // fold does, so a search over the whole body would pass on the binding
    // alone and say nothing about the order the charge is decided in.
    let fold_at = at("self.frame_ledger.record_wake_claim(");
    let fold = &body[fold_at..];
    let fold = &fold[..fold
        .find(");")
        .expect("the record_wake_claim call has no end")];
    let first = fold
        .find("upload_claim")
        .expect("the charge fold no longer names the upload claim");
    let then = fold
        .find(".or(wake_claim)")
        .expect("the charge fold no longer names the re-arm's claim");
    assert!(
        first < then,
        "the charge fold asks the re-arm before the upload queue, so a frame          the upload queue had already bought is charged to whatever the          re-arm was standing for: {fold}",
    );
    // The guard on the redraw post names `wake_claim` alone: the upload claim
    // is an attribution, not a second ask for a frame that is already coming.
    assert!(
        body.contains("if wake_claim.is_some() {"),
        "the re-arm's redraw guard changed shape; if the upload claim has \
         been folded into it, every banded frame now posts a redraw twice",
    );
}

/// **A chunk round in flight is not a reason to draw a frame.**
///
/// The round is answered by the worker running it, which posts a redraw of its
/// own the moment it sends — so re-arming for it as well spent one frame per
/// frame of the round's latency with nothing to show. Measured on an idle
/// KTLX live feed over two 129 s windows with no input at all: 901 and 714
/// unnecessary frames of 930 and 743 drawn, of which 765 and 685 were charged
/// to that re-arm.
///
/// **The two halves have to be pinned together.** Dropping the poll is only
/// safe while the wake that replaced it exists, so this asserts the absence of
/// the poll *and* that both dispatch sites still post a redraw on a path that
/// cannot skip it. If a future edit makes one of those wakes conditional, this
/// fails here rather than as a feed that stops updating on an idle desktop.
#[test]
fn a_chunk_round_is_woken_by_the_worker_that_answers_it_not_by_a_poll() {
    const APP: &str = include_str!("../app.rs");
    const CHUNKS: &str = include_str!("../app_chunks.rs");

    let redraw = balanced_body(APP, "fn handle_redraw(");
    let arm_start = redraw
        .find("let wake_claim = ")
        .expect("the end-of-frame re-arm is gone from handle_redraw");
    let arm = &redraw[arm_start
        ..arm_start
            + redraw[arm_start..]
                .find("notify_redraw(")
                .expect("the re-arm no longer ends in a redraw request")];
    assert!(
        arm.contains("self.chunk_notify.handshake_pending()"),
        "the re-arm dropped the handshake, which has no producer of its own: \
         a socket that goes down is then retried only if something unrelated \
         draws a frame",
    );
    assert!(
        !arm.contains("chunk_feeds.any_in_flight"),
        "the feed's in-flight poll is back in the re-arm. It asks for a frame \
         for every frame a round is out, and the worker running that round \
         already posts one when it answers: {arm}",
    );

    for site in ["fn drive_chunk_feeds(", "fn fetch_notified_chunk("] {
        let body = balanced_body(CHUNKS, site);
        let send = body
            .find("sender.send(ChunkResponse {")
            .unwrap_or_else(|| panic!("{site} no longer replies with a ChunkResponse"));
        let wake = body
            .find("crate::app::notify_redraw(&window);")
            .unwrap_or_else(|| {
                panic!(
                    "{site} no longer asks for a frame when its round \
                     answers, and nothing else does: the round's result now \
                     waits for an unrelated wake"
                )
            });
        assert_eq!(
            body.matches("crate::app::notify_redraw(&window);").count(),
            1,
            "{site} asks for a frame in more than one place; this pin cannot \
             tell which of them is on the answering path",
        );
        assert!(
            send < wake,
            "{site} asks for its frame before it sends the result, so the \
             frame it buys can run before the message is on the channel",
        );
        let between = &body[send..wake];
        for branch in ["if ", "return", "match "] {
            assert!(
                !between.contains(branch),
                "{site} put a `{branch}` between the send and the wake, so \
                 there is now a path that sends a round's result and never \
                 asks for the frame that would draw it",
            );
        }
    }
}

/// **The clock reclassification, with `drawn` as the control.**
///
/// A lane that improves a waste ratio by re-attributing frames has improved
/// the ratio and nothing else, and the only thing separating an honest
/// re-attribution from a flattering one is that the population is untouched.
/// So the two arms here are the same sixty frames on the same schedule under
/// the same claim, differing in one fact: whether the words on the glass
/// moved. `drawn` is identical across both by assertion, not by inspection.
///
/// Arm B is also the shape the residual actually had. Those frames were
/// charged to [`WakeClaim::Timed`] — a frame arriving on a clock with nothing
/// to show — which is exactly what a frozen string on a live tick is, and
/// exactly what a moving one is not.
#[test]
fn the_clock_cause_moves_frames_between_the_verdicts_and_never_the_denominator() {
    const FRAMES: u32 = 60;

    let mut moving = fresh();
    for _ in 0..FRAMES {
        moving.record_wake_claim(WakeClaim::Timed);
        note(NeedCause::Clock);
        moving.record(take());
    }
    // The tamper arm: identical frame count, identical claim, identical tick.
    // The one difference is that nothing new was printed.
    let mut frozen = fresh();
    for _ in 0..FRAMES {
        frozen.record_wake_claim(WakeClaim::Timed);
        frozen.record(take());
    }

    let (moving, frozen) = (moving.reading(), frozen.reading());
    assert_eq!(
        (moving.drawn, frozen.drawn),
        (u64::from(FRAMES), u64::from(FRAMES)),
        "the two arms did not draw the same number of frames, so anything \
         below is a comparison of two different runs",
    );
    assert_eq!(
        (moving.needed, moving.unnecessary()),
        (u64::from(FRAMES), 0),
        "a frame that printed words nobody had seen was scored waste",
    );
    assert_eq!(
        moving.cause(NeedCause::Clock),
        u64::from(FRAMES),
        "the clock cause was raised on every frame and the reading does not \
         show it",
    );
    assert_eq!(
        (frozen.needed, frozen.unnecessary()),
        (0, u64::from(FRAMES)),
        "with the words held still the frames must go back to waste; a \
         reclassification that survives its own input being frozen is a \
         threshold wearing a cause's name",
    );
    // And the waste lands where the real residual landed: on the timer.
    assert_eq!(
        (
            frozen.charge(WakeClaim::Timed),
            frozen.charge(WakeClaim::External),
        ),
        (u64::from(FRAMES) - 1, 1),
        "the frozen arm's waste is not charged to the tick that bought it",
    );
    assert!(moving.charges_balance() && frozen.charges_balance());
    assert!(moving.causes_cover_the_needed() && frozen.causes_cover_the_needed());
}

/// **A render in flight is not a reason to draw a frame.**
///
/// `chunk_feeds.any_in_flight()` left the re-arm for this argument and
/// `render.any_render_in_flight()` is the same shape one claim over: a render
/// out on a worker is answered by that worker, which posts a redraw of its own
/// the moment it sends, so re-arming for it as well spent one frame per frame
/// of the render's latency with nothing to show. It was the largest charge
/// left after the chunk round went.
///
/// **The two halves have to be pinned together.** Dropping the poll is only
/// safe while the wake that replaced it exists, so this asserts the absence of
/// the poll *and* that every dispatch site still posts a redraw on a path that
/// cannot skip it. All four are held, not only the two the removed poll
/// tracked: the voxel build and the speculative render never set a pane's
/// in-flight flag, so they have relied on that wake alone all along, and the
/// argument for dropping the poll is that they are the rule rather than the
/// exception.
///
/// The **abandon** paths are the other precondition, and they need no wake at
/// all: they run on the frame thread and clear the flag there, beside the
/// `abandon_results` that stops the reply. That pairing is what keeps the
/// predicate falling for a render nobody will hear from again — with the poll
/// gone it no longer costs frames, but it still gates speculation
/// (`app_render::maybe_spawn_speculative_render`), so a leaked flag would
/// silently stop pre-rendering rather than spin.
#[test]
fn a_render_in_flight_is_woken_by_the_worker_that_answers_it_not_by_a_poll() {
    const APP: &str = include_str!("../app.rs");
    const DISPATCH: &str = include_str!("../render_dispatch.rs");
    const APP_RENDER: &str = include_str!("../app_render.rs");

    let redraw = balanced_body(APP, "fn handle_redraw(");
    let arm_start = redraw
        .find("let wake_claim = ")
        .expect("the end-of-frame re-arm is gone from handle_redraw");
    let arm = &redraw[arm_start
        ..arm_start
            + redraw[arm_start..]
                .find("notify_redraw(")
                .expect("the re-arm no longer ends in a redraw request")];
    assert!(
        !arm.contains("any_render_in_flight"),
        "the render in-flight poll is back in the re-arm. It asks for a frame \
         for every frame a render is out, and the worker running that render \
         already posts one when it answers: {arm}",
    );

    // Presence control, in two parts. The predicate has to still exist and
    // still be read somewhere, or this test would pass over a function that
    // had simply been deleted — and the reading that survives is the one that
    // never cost a frame: the gate that refuses to speculate while an
    // interactive render is out.
    assert!(
        DISPATCH.contains("pub fn any_render_in_flight(&self) -> bool"),
        "the dispatcher no longer publishes whether a render is in flight, so \
         the absence asserted above is a deletion and not a removal from the \
         re-arm",
    );
    assert!(
        balanced_body(APP_RENDER, "fn maybe_spawn_speculative_render(")
            .contains("self.render.any_render_in_flight()"),
        "the speculative gate no longer reads the in-flight flag, so nothing \
         but the re-arm ever did and the flag itself is now dead",
    );

    // Every dispatch site, and the wake each one's answer brings with it.
    for (site, response) in [
        ("fn spawn_render(", "sender.send(RenderResponse {"),
        (
            "fn spawn_section_render(",
            "sender.send(crate::channels::SectionResponse {",
        ),
        (
            "fn spawn_voxel_build(",
            "sender.send(crate::channels::VoxelResponse {",
        ),
        (
            "fn spawn_speculative_render(",
            "sender.send(RenderResponse {",
        ),
    ] {
        let body = balanced_body(DISPATCH, site);
        let send = body
            .find(response)
            .unwrap_or_else(|| panic!("{site} no longer replies with `{response}`"));
        const WAKE: &str = "crate::app::notify_redraw(&window);";
        let wake = body.find(WAKE).unwrap_or_else(|| {
            panic!(
                "{site} no longer asks for a frame when its render \
                     answers, and nothing else does: the raster it produced \
                     now waits for an unrelated wake"
            )
        });
        assert_eq!(
            body.matches(WAKE).count(),
            1,
            "{site} asks for a frame in more than one place; this pin cannot \
             tell which of them is on the answering path",
        );
        assert!(
            send < wake,
            "{site} asks for its frame before it sends the result, so the \
             frame it buys can run before the message is on the channel",
        );
        let between = &body[send..wake];
        for branch in ["if ", "return", "match "] {
            assert!(
                !between.contains(branch),
                "{site} put a `{branch}` between the send and the wake, so \
                 there is now a path that sends a render's result and never \
                 asks for the frame that would draw it",
            );
        }
        // **And the wake is the closure's last statement.** Both plan-view
        // sites send only while the result is still wanted, and a
        // `notify_redraw` moved inside that guard is still after the send
        // with no branch between the two — every check above passes while an
        // abandoned render quietly stops asking for its frame. Nothing may
        // stand between the wake and the closing of the closure, which is
        // what says no block encloses it and nothing runs after it.
        let tail = &body[wake + WAKE.len()..];
        assert!(
            tail.trim_start().starts_with("});"),
            "{site}'s wake is not the last statement of its reply closure, so \
             it sits inside some block that can decline to run it: {}",
            &tail[..tail.len().min(120)],
        );
    }

    // The abandon paths: the flag falls on the frame thread, beside the call
    // that stops the reply. Checked per site rather than against a count, so
    // a fifth abandon site added later owes the same pairing without anyone
    // re-pointing a number — the only thing a count is needed for here is
    // that the loop below is not vacuous. Four when this was written.
    let abandons: Vec<usize> = DISPATCH
        .match_indices(".abandon_results();")
        .map(|(at, _)| at)
        .collect();
    assert!(
        !abandons.is_empty(),
        "`render_dispatch` abandons no render results anywhere, so the pairing \
         checked below is checked over nothing",
    );
    for at in abandons {
        let before = &DISPATCH[..at];
        let cleared = before.rfind("render_finished();").unwrap_or_else(|| {
            panic!(
                "an `abandon_results()` at byte {at} has no `render_finished()` \
                 before it at all: that pane is marked in flight for a render \
                 whose reply has just been made unwanted, and nothing will \
                 ever clear it"
            )
        });
        let between = &before[cleared + "render_finished();".len()..];
        assert!(
            !between.contains('{') && !between.contains('}'),
            "an `abandon_results()` at byte {at} is separated from the \
             `render_finished()` before it by a block boundary, so they are \
             no longer the same straight-line path and one can run without \
             the other: {between}",
        );
    }
}
