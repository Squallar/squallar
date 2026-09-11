//! **The frame at the playhead is decoded and resident once the pump has
//! settled.**
//!
//! Nothing else in this tree asserts it. The two halves of the loop's decode
//! cycle are gated apart — `LoopDownloadManager::frames_needing_decode` has
//! its own suite next door in `loop_scan_cache_tests`, and
//! `App::evict_unneeded_loop_scans`' residency policy has its own in
//! `loop_decoded_census_tests` — and both are green while the frame the user
//! is looking at is never decoded at all, because no suite runs the two
//! together and then looks at the playhead.
//!
//! # What is asserted, and what deliberately is not
//!
//! **The playhead, parked, and untextured.** That is the narrowest statement
//! of the property that is true on every arm and visible to the user:
//!
//! * *Untextured* — a frame with no picture yet has nothing on the glass, so
//!   "its moments are resident" is the difference between a drawn frame and a
//!   blank one. It is also the one arm of `decoded_wanted` that does not
//!   depend on a device constant: the residency pass keeps an untextured
//!   frame's moments whatever `LOOP_DECODED_LOOKAHEAD_FRAMES` says, so this
//!   assertion means the same thing on desktop, mobile and wasm.
//! * *Parked* — the playhead is not moving. A loop in playback draws the
//!   texture and reads no moments at all (`evict_decoded_except`'s own
//!   doc-comment: "Playback reads neither"), so residency at a *moving*
//!   playhead is not a property the product owes; asserting it would be
//!   asserting that decodes outrun playback, which is a rate, which is a
//!   clock. This gate has no clock in it.
//! * **The lookahead is NOT asserted.** `LOOP_DECODED_LOOKAHEAD_FRAMES` is
//!   `Some(1)` on desktop, `Some(0)` on mobile and `None` on wasm, and its own
//!   doc calls it retarget insurance — a head start on a re-render, priced in
//!   megabytes. Two of the three arms keep none of it, so a gate that asserted
//!   it would be pinning a desktop tuning number as a user-visible promise and
//!   would have to be re-pointed every time that number is tuned.
//!
//! # How "settled" is decided
//!
//! Not by a round count picked here. A round runs the real residency sweep and
//! the real pump, and **settled is the state the pump reports by dispatching
//! nothing**: no frame it wants to start work on is left. The cap exists only
//! so a pump that never settles ends the test instead of hanging, and it is
//! denominated — one round per frame in the plan, against a plan that needs
//! two decodes and is given four slots a round.
//!
//! # Non-vacuity
//!
//! The control arm runs the identical rig with the playhead on the first frame
//! of the plan and asserts the same property positively. It passes on this
//! tree. So a red on the subject arm is the playhead's POSITION and not a rig
//! that cannot deliver residency at all, and the assertion cannot be one that
//! passes for free because the machinery is absent — the machinery answered
//! twelve frames earlier in the same plan.

use super::loop_dispatch_tests::volume_with_sweeps;
use super::radar_timeline_addressing_tests::{active_loop, at, point_at_site, textured};
use super::*;
use crate::app::tests::headless;
use crate::platform_double::TestBridge;
use squallar_radar::loop_downloads::{FramePlan, PendingDownloads};
use squallar_source::id::known;

const SITE: &str = "KTLX";
const TILT: f32 = 0.5;
/// Twenty-four frames, the shape a one-hour loop at a precipitation VCP has.
const FRAMES: usize = 24;
/// Mid-loop, and far enough from frame zero that a walk which starts at the
/// oldest frame and stops when its slots run out cannot reach it.
const SUBJECT_PLAYHEAD: usize = 12;
/// The first frame of the plan, which that same walk reaches first.
const CONTROL_PLAYHEAD: usize = 0;

/// Enough bytes to be a real entry in the archive cache and few enough that
/// neither byte ceiling is in this test's way — the defect this gates is not a
/// ceiling, and a fixture that hit one would be testing the ceiling instead.
const ARCHIVE_BYTES: usize = 4096;

/// `at(0)` is what `point_at_site` parks the pane's `scan_info` on, and
/// `evict_unneeded_loop_scans` pins a parked volume unconditionally. The
/// frames start after it so that nothing in this fixture is held by that pin.
fn stamps() -> Vec<chrono::NaiveDateTime> {
    (1..=FRAMES as i64).map(at).collect()
}

/// **A loop that has played through once and is parked on a frame that never
/// got a picture.**
///
/// Every frame carries its compressed archive and none carries its moments,
/// which is where both eviction passes leave a loop that has been running:
/// `evict_decoded_except` trades a textured frame's volume for the archive it
/// can be rebuilt from, and `evict_decoded_to_ceiling` pins only what a pane
/// is *parked* at — never the loop's own playhead — so under pressure it can
/// and does take the frame on the glass too.
///
/// From here the pump owes the user exactly one decode: the playhead's.
fn parked_loop_with_an_undrawn_playhead(
    ctx: &egui::Context,
    playhead: usize,
) -> (crate::app::App, Vec<chrono::NaiveDateTime>) {
    let stamps = stamps();
    let mut app = headless(TestBridge::desktop());
    point_at_site(&mut app, 0);
    app.loop_mgr = squallar_radar::loop_downloads::LoopDownloadManager::new();
    for &stamp in &stamps {
        app.loop_mgr
            .cache_archive(SITE, stamp, Arc::new(vec![0u8; ARCHIVE_BYTES]));
    }

    let ls = app
        .gui
        .pane_mut(0)
        .expect("a headless app has a pane")
        .time_state_mut(&known::RADAR);
    *ls = active_loop(&stamps);
    for (idx, frame) in ls.frames.iter_mut().enumerate() {
        if idx != playhead {
            frame.image = Some(textured(ctx));
        }
    }
    ls.settle_playhead(squallar_egui::pane::TimeMode::AsOf(stamps[playhead]));
    assert_eq!(
        ls.current_frame(),
        playhead,
        "fixture: the playhead did not settle where this arm parked it",
    );
    assert!(
        ls.frames[playhead].image.is_none(),
        "fixture: the frame on the glass already has a picture, so residency \
         at it would be the lookahead's question and not the blank frame's",
    );

    // **The plan is the pane's own frame list, in the pane's own order.** The
    // app builds it that way (`app_render`'s `rederive` walk hands
    // `FramePlan::new` the timestamps of `ls.frames`), and the order is the
    // whole subject here: typing an order into the fixture would be choosing
    // whether the defect can be reached.
    let frames: Vec<chrono::NaiveDateTime> = ls.frames.iter().map(|f| f.timestamp).collect();
    assert_eq!(
        frames, stamps,
        "fixture: the plan is the pane's frame list, oldest-first",
    );
    app.loop_mgr
        .set_plan(0, FramePlan::new(SITE.to_string(), frames));
    // An empty download queue: every frame's bytes are already here, so the
    // only errand the pump has is the decode arm. `dispatch_pending_loop_
    // downloads` returns early without a pending entry, so the entry is what
    // lets the pump run at all.
    app.loop_mgr.insert_pending(
        0,
        PendingDownloads {
            site: SITE.to_string(),
            queue: VecDeque::new(),
        },
    );
    (app, stamps)
}

/// **One tick of the app, in the order the app runs it**: the residency sweep
/// (`handle_redraw` calls it before the dispatch below), then the pump, then
/// the decodes it started landing through the arrival path.
///
/// Returns how many decodes the pump dispatched, which is the state that says
/// whether it has settled. The decodes are landed here rather than polled off
/// the channel because the spawned job decodes the fixture's archive bytes,
/// which are not an Archive II file; what this gate is about is which frames
/// the pump chose, and every frame it chose is landed.
fn tick(app: &mut crate::app::App, stamps: &[chrono::NaiveDateTime]) -> usize {
    app.evict_unneeded_loop_scans();
    app.dispatch_pending_loop_downloads(0);
    let dispatched: Vec<chrono::NaiveDateTime> = stamps
        .iter()
        .copied()
        .filter(|ts| app.loop_mgr.is_in_flight(SITE, ts))
        .collect();
    for ts in &dispatched {
        apply_completed_download(
            &mut app.loop_mgr,
            crate::channels::LoopScanDownloadResponse {
                site: SITE.to_string(),
                timestamp: *ts,
                scan: Some(volume_with_sweeps(&[TILT])),
                archive: Some(Arc::new(vec![0u8; ARCHIVE_BYTES])),
            },
        );
    }
    dispatched.len()
}

/// Run ticks until the pump dispatches nothing, or until the cap. Returns
/// `(ticks_run, total_decodes_dispatched, settled)`.
fn run_until_settled(
    app: &mut crate::app::App,
    stamps: &[chrono::NaiveDateTime],
) -> (usize, usize, bool) {
    // One tick per frame in the plan. A pump that starts at least one decode a
    // tick and never repeats one covers the whole plan inside this, and the
    // plan here needs two decodes against four slots a tick — so reaching the
    // cap is itself a statement about the pump, never about the cap.
    let cap = stamps.len();
    let mut total = 0;
    for run in 1..=cap {
        let dispatched = tick(app, stamps);
        total += dispatched;
        if dispatched == 0 {
            return (run, total, true);
        }
    }
    (cap, total, false)
}

/// **The gate: the frame at the playhead is decoded and resident.**
///
/// # The property
///
/// A parked loop whose playhead frame has no picture yet. Its compressed bytes
/// are in the process; the decode that turns them into something drawable is
/// the pump's errand and nothing else's. Until it runs, the user is looking at
/// a hole in the loop — and `evict_unneeded_loop_scans` agrees, because an
/// untextured frame is in `decoded_wanted` on every arm.
///
/// # Why it can fail
///
/// `frames_needing_decode` walks the plan oldest-first and the pump `break`s
/// on the first frame that will not fit its slots, so every slot goes to the
/// frames furthest from the glass; the residency sweep then evicts those same
/// volumes on the next tick because they are textured and outside the
/// lookahead, and the next pump pass offers them again. The playhead is never
/// reached. This is not a slow path — it does not converge.
///
/// # Reading a failure
///
/// The control arm above the subject asserts the same property with the
/// playhead on the plan's first frame. If the control is green and the subject
/// is red, the pump served the frame it walked into first and starved the one
/// the user is looking at.
///
/// TAMPER: there is none to write. The unmodified tree fails this test — the
/// defect is live — which is a stronger non-vacuity proof than a mutation.
#[test]
fn the_frame_at_the_playhead_is_decoded_once_the_pump_has_settled() {
    let ctx = egui::Context::default();

    // --- the control: the same rig, playhead on the plan's first frame ---
    let (mut app, stamps) = parked_loop_with_an_undrawn_playhead(&ctx, CONTROL_PLAYHEAD);
    assert!(
        app.loop_mgr.needs_decode(SITE, &stamps[CONTROL_PLAYHEAD]),
        "premise: the playhead's bytes are here and its moments are not, so \
         the pump is the only thing that can draw this frame",
    );
    let (ticks, decodes, settled) = run_until_settled(&mut app, &stamps);
    assert!(
        decodes > 0,
        "the pump dispatched nothing at all in {ticks} ticks, so nothing \
         below is a statement about which frame it chose",
    );
    assert!(
        app.loop_mgr.is_cached(SITE, &stamps[CONTROL_PLAYHEAD]),
        "the rig cannot deliver residency at the playhead even when the \
         playhead is the first frame the pump walks into: {decodes} decodes \
         over {ticks} ticks, settled={settled}. The subject arm below would \
         prove nothing.",
    );

    // --- the subject: the identical rig, playhead mid-loop ---
    let (mut app, stamps) = parked_loop_with_an_undrawn_playhead(&ctx, SUBJECT_PLAYHEAD);
    assert!(
        app.loop_mgr.needs_decode(SITE, &stamps[SUBJECT_PLAYHEAD]),
        "premise: the playhead's bytes are here and its moments are not",
    );
    let (ticks, decodes, settled) = run_until_settled(&mut app, &stamps);
    let resident: Vec<usize> = (0..FRAMES)
        .filter(|idx| app.loop_mgr.is_cached(SITE, &stamps[*idx]))
        .collect();
    assert!(
        app.loop_mgr.is_cached(SITE, &stamps[SUBJECT_PLAYHEAD]),
        "the frame the user is parked on was never decoded. Playhead is frame \
         {SUBJECT_PLAYHEAD} of {FRAMES}; the pump dispatched {decodes} decodes \
         over {ticks} ticks (settled={settled}) and what it left resident is \
         {resident:?}. Its archive is in the process the whole time — this \
         frame needs no network to be drawn, only a decode it was never \
         offered.",
    );
}
