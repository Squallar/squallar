//! **WI-3P: playback runs on the transport layer, and a forecast loop starts
//! on a frame that means something.**
//!
//! Three things were still radar-shaped once WI-1 through WI-6b had landed:
//!
//! 1. `advance_loop_playback` re-read `clock_layer()` **every tick**. That is
//!    the topmost *active* slot — a derivation — so the layer whose stamps the
//!    clock walks could change underneath a running loop the moment another
//!    layer started or stopped animating above the one the transport addresses.
//!    `transport_layer()` is the pane's decision and does not move.
//! 2. `sync_loop_playback_start` ended with `set_time_mode(TimeMode::Live)`.
//!    Under `FrameSeries` that is `frames.len() - 1`. For radar it is *now*;
//!    for a forecast rail it is the **horizon**, up to 48 h out.
//! 3. Whether a non-radar loop reaches `Playing` at all had never been driven
//!    end to end. WI-1 retargeted the readiness read to `transport_state()`;
//!    nothing exercised it from a listing arriving to a clock ticking.
//!
//! **The safety property is radar**, which is the loop that ships:
//! [`a_radar_loop_starts_on_live_and_the_clock_stays_a_live_clock`] and
//! [`the_tick_sequence_of_a_radar_only_pane_is_unchanged`] assert its start
//! frame, its clock *mode* and its tick sequence, so "park every loop on an
//! index" and "always start at frame 0" both fail.
//!
//! The forecast claim is driven through production: the layer double's frames
//! arrive on the real `SourceEvent::Frames` path, their pictures arrive on the
//! real `poll_overlay_render_results` path, and readiness and start come from
//! one call to `update_loop_readiness` — the same entry point the frame pump
//! makes. Nothing about the start is hand-armed.
//!
//! Each claim names the mutation that turns it red; all four were applied and
//! observed.

use squallar_egui::pane::{LayerTimeState, LoopFrame, LoopPhase, TimeMode};
use squallar_source::id::known;
use squallar_source::time::FrameStamp;

use super::loop_overlay_render_tests::{
    app_with_frames, build_loop, deliver_raster, frame_stamps, run,
};

/// **The wall clock the production code reads**, taken once by the test.
///
/// `sync_loop_playback_start` calls `chrono::Utc::now()` itself and there is no
/// seam to inject one through, so the arrangement is anchored on the same
/// clock rather than on a fixed date. Every offset below is a whole or half
/// hour, so the milliseconds between this reading and production's cannot
/// decide which frame is "at or before now".
fn wall() -> chrono::NaiveDateTime {
    chrono::Utc::now().naive_utc()
}

/// The transport layer's timeline on pane 0.
fn transport(app: &crate::app::App) -> &LayerTimeState {
    app.gui.pane(0).expect("pane 0").transport_state()
}

/// The stamp pane 0's transport layer is presenting — **the assertion, in every
/// test here**. An index would pass on the wrong frame whenever two lists
/// happen to be the same length, which is exactly the confusion the horizon
/// defect lives in.
fn shown(app: &crate::app::App) -> Option<chrono::NaiveDateTime> {
    transport(app).playhead_stamp()
}

/// Pane 0's clock mode. Not the playhead: `Live` is a *mode*, and three
/// readers key on the mode rather than on the frame it resolves to — see
/// `loop_start_frame`.
fn clock(app: &crate::app::App) -> TimeMode {
    app.gui.pane(0).expect("pane 0").time.mode
}

/// One tick of playback with the frame interval already elapsed.
///
/// `last_advance` is cleared rather than slept through: the interval is
/// wall-clock and a test that waited for it would be asserting the clock
/// instead of the property.
fn tick(app: &mut crate::app::App) -> Option<chrono::NaiveDateTime> {
    app.gui
        .pane_mut(0)
        .expect("pane 0")
        .transport_state_mut()
        .last_advance = None;
    app.advance_loop_playback();
    shown(app)
}

/// `n` ticks' worth of presented stamps, in order.
fn tick_sequence(app: &mut crate::app::App, n: usize) -> Vec<Option<chrono::NaiveDateTime>> {
    (0..n).map(|_| tick(app)).collect()
}

/// A radar timeline holding `stamps`, every frame already carrying a picture.
///
/// The picture is an `Overlay` texture because playback reads
/// `frame.image.is_some()` and nothing else — what a radar frame's image *is*
/// belongs to the render path, not to the tick.
pub(super) fn textured_frames(
    ctx: &egui::Context,
    stamps: &[chrono::NaiveDateTime],
) -> Vec<LoopFrame> {
    stamps
        .iter()
        .map(|&timestamp| LoopFrame {
            timestamp,
            image: Some(squallar_egui::pane::LoopFrameImage::Overlay(
                squallar_egui::overlay_cache::OverlayTextureData {
                    texture: ctx.load_texture(
                        format!("frame_{timestamp}"),
                        egui::ColorImage::from_rgba_unmultiplied([1, 1], &[9, 9, 9, 255]),
                        egui::TextureOptions::LINEAR,
                    ),
                    placed: squallar_geo::PlacedRaster::of(squallar_geo::GeoBounds {
                        min_lat: 34.0,
                        max_lat: 36.0,
                        min_lon: -99.0,
                        max_lon: -97.0,
                    }),
                    data_generation: 5,
                    render_zoom: 32,
                    width: 1,
                    height: 1,
                    radar_meta: None,
                    hit_map: None,
                },
            )),
            render_in_flight: false,
            render_failed: false,
        })
        .collect()
}

// ── The end-to-end claim ────────────────────────────────────────────────────

/// **A forecast loop plays, and it starts on the frame at *now* rather than on
/// its horizon** — driven from a listing arriving to a clock ticking.
///
/// The pane animates one layer, a `FrameSeries` double declaring
/// `extends_future: true`. Its five frames straddle the wall clock: two behind
/// it, two just ahead, and one 48 h out standing for the HRRR CONUS horizon.
///
/// Four assertions, in the order the frame makes them:
///
/// 1. **the gate** — the transport reaches `LoopPhase::Playing`. Before WI-1
///    the readiness walk read radar's slot by definition, so a
///    model loop that reached `Ready` was never started and the ∞ toggle did
///    nothing at all on a radar-off pane;
/// 2. **the clock** — the presented stamp is the frame at or before now, not
///    the horizon. Asserted as a **stamp**: the horizon frame and the now frame
///    are both members of a five-element list, so an index assertion would
///    pass on either whenever the two lists were the same length;
/// 3. **the pane's clock is not parked in the future** either — the mode is
///    `AsOf(now-ish)`, so every other layer on the pane depicts the same
///    instant instead of following the model 48 h forward;
/// 4. **the tick walks that layer's own frames**, forward and wrapping.
///
/// **Floor A — `radar_addressed_start`:** put
/// `time_state(&known::RADAR)`/`time_state_mut(&known::RADAR)` back in
/// `sync_loop_playback_start`. Assertion 1 fails: the phase stays
/// `Ready` for ever. This is the defect the item exists for.
///
/// **Floor B — `live_start`:** restore `pane.set_time_mode(TimeMode::Live)` as
/// the only arm. Assertion 2 fails naming both stamps — the loop opens on the
/// 48 h frame.
#[test]
fn a_forecast_loop_reaches_playing_and_parks_on_the_frame_at_now() {
    let ctx = egui::Context::default();
    let base = wall();
    let half = chrono::Duration::minutes(30);
    let at_now = base - half;
    let horizon = base + chrono::Duration::hours(48);
    let listed = vec![
        base - half * 3,
        at_now,
        base + half,
        base + half * 3,
        horizon,
    ];

    let (mut app, _asked) = app_with_frames(listed.clone());
    build_loop(&mut app, (listed[0], horizon));

    assert_eq!(
        frame_stamps(&app),
        listed,
        "premise: all five listed frames must have become the layer's frame \
         list — a list capped by the byte share would decide which frame is \
         last, and it is the last frame this test is about",
    );
    for (i, valid) in listed.iter().enumerate() {
        deliver_raster(
            &mut app,
            &ctx,
            FrameStamp {
                valid: *valid,
                run: Some(run()),
            },
            i as u8,
        );
    }
    assert_eq!(
        transport(&app).phase,
        LoopPhase::Rendering,
        "premise: the loop must still be filling, or the readiness pass below \
         has nothing to promote",
    );

    // The one production entry: settle every animating layer, then start the
    // panes that are ready. The frame pump makes exactly this call.
    app.update_loop_readiness();

    // 1. The gate.
    assert_eq!(
        transport(&app).phase,
        LoopPhase::Playing,
        "a pane whose only animating layer is a forecast loop never reached \
         Playing. Its frames are listed, fetched, rasterized and drawable, and \
         the Play button does nothing.",
    );

    // 2. The clock, as a stamp.
    assert_eq!(
        shown(&app),
        Some(at_now),
        "the loop opened on the wrong frame. It must open on the frame the \
         FrameSeries contract names at the wall clock ({at_now}); opening on \
         the newest frame there is ({horizon}) shows the user a picture of \
         two days' time and stops the pane's clock there.",
    );

    // 3. And the pane's clock went with it.
    assert_eq!(
        clock(&app),
        TimeMode::AsOf(at_now),
        "the pane's clock is not on the frame the loop is showing, so every \
         other layer on the pane depicts a different instant from the one \
         under the playhead",
    );

    // 4. The tick walks this layer's frames, and wraps.
    assert_eq!(
        tick_sequence(&mut app, 5)
            .into_iter()
            .map(|s| s.expect("every frame is textured, so every tick lands"))
            .collect::<Vec<_>>(),
        vec![base + half, base + half * 3, horizon, listed[0], at_now,],
        "playback did not walk the transport layer's own frames in order",
    );
}

// ── The flip ────────────────────────────────────────────────────────────────

/// **The clock does not change layer underneath a running loop.**
///
/// `advance_loop_playback` re-read `clock_layer()` every tick. That is the
/// topmost *active* slot, so it is a function of what happens to be animating
/// — and a radar loop above the transport layer captured the tick while it
/// ran, then handed it back when it retired. The pane's transport never moved;
/// the frames its clock walked did, twice.
///
/// The arrangement puts a radar loop *above* a forecast transport (the slot
/// list runs bottom to top and radar is armed second, so radar is the topmost
/// active slot), plays two ticks, retires radar, and plays two more. The two
/// timelines are a **day apart**, so a tick that landed on the wrong one is
/// unmistakable in the sequence.
///
/// **Floor C — `clock_layer_tick`:** restore `let Some(id) =
/// pane.clock_layer().cloned()` and `pane.time_state_mut(&id)`. The stamp
/// *sequence* fails at its first element: the first two ticks walk radar's
/// frames, and the last two walk the model's from wherever radar left the
/// clock.
#[test]
fn the_tick_stays_on_the_transport_layer_when_a_radar_loop_retires_above_it() {
    let ctx = egui::Context::default();
    let base = wall();
    let hour = chrono::Duration::hours(1);
    let model = vec![base - hour, base, base + hour, base + hour * 2];
    // A day earlier: no stamp of radar's can be mistaken for one of the
    // model's, and no wrap can land on one by accident.
    let radar: Vec<chrono::NaiveDateTime> = model
        .iter()
        .map(|t| *t - chrono::Duration::days(1))
        .collect();

    let (mut app, _asked) = app_with_frames(model.clone());
    build_loop(&mut app, (model[0], *model.last().expect("four frames")));
    assert_eq!(
        frame_stamps(&app),
        model,
        "premise: the model layer holds the four frames the ticks are checked \
         against",
    );

    {
        let pane = app.gui.pane_mut(0).expect("pane 0");
        // The model loop, playing, parked on its first frame.
        let ls = pane.transport_state_mut();
        ls.frames = textured_frames(&ctx, &model);
        ls.phase = LoopPhase::Playing;
        // Radar's slot moved *above* the model's — which is where it sits on
        // a real pane's draw order — so `clock_layer()` answers radar while
        // the transport addresses the model.
        let rls = pane.time_state_mut(&known::RADAR);
        *rls = LayerTimeState::begin(
            3600,
            squallar_radar::types::RenderView::PlanView,
            Box::new(()),
        );
        rls.frames = textured_frames(&ctx, &radar);
        rls.phase = LoopPhase::Playing;
        let radar_slot = pane
            .layers
            .take_out(&known::RADAR)
            .expect("the slot was just written");
        pane.layers.push(radar_slot);
        pane.set_time_mode(TimeMode::AsOf(model[0]));
    }

    let pane = app.gui.pane(0).expect("pane 0");
    assert_eq!(
        pane.transport_layer(),
        &known::MODEL_DATA,
        "premise: the transport addresses the model layer",
    );
    assert_eq!(
        pane.clock_layer(),
        Some(&known::RADAR),
        "premise: the radar slot must be the topmost ACTIVE one, or the two \
         readings agree here and the test cannot fail",
    );

    let while_radar_runs = tick_sequence(&mut app, 2);

    // Radar retires: its listing is exhausted, its site went away, the user
    // turned the layer off. Nothing about the pane's transport changed.
    app.gui
        .pane_mut(0)
        .expect("pane 0")
        .time_state_mut(&known::RADAR)
        .phase = LoopPhase::Inactive;
    assert_eq!(
        app.gui.pane(0).expect("pane 0").clock_layer(),
        Some(&known::MODEL_DATA),
        "premise: retiring radar moved `clock_layer()`, which is the flip \
         under test",
    );

    let after = tick_sequence(&mut app, 2);

    // `Option`s compared whole, not unwrapped: a tick that walked radar's
    // frames parks the pane's clock a day early, where no model frame
    // qualifies — the wrong layer shows up here as `None`, not as a panic.
    let observed: Vec<Option<chrono::NaiveDateTime>> =
        while_radar_runs.into_iter().chain(after).collect();
    assert_eq!(
        observed,
        vec![
            Some(model[1]),
            Some(model[2]),
            Some(model[3]),
            Some(model[0])
        ],
        "the clock changed layer underneath a running loop. The transport \
         addresses the model timeline throughout; these stamps must all come \
         from {model:?}, and a stamp from {radar:?} is a tick that walked \
         radar's frames because radar happened to be the topmost animating \
         slot.",
    );
}

// ── The safety property: radar is unchanged ─────────────────────────────────

/// **A radar loop still starts on `TimeMode::Live`, and that is not the same
/// as its last frame.**
///
/// The obvious simplification — park every loop on an index and delete the
/// `Live` arm — reads as a no-op and is not one. `Live` is a pane *clock mode*:
/// `as_of_term` has a `Live` fast path returning `0`, so an `AsOf` clock mints
/// a fresh raster token for every `EventLifetime` layer on the pane;
/// `TimeMode::as_of` answers `None` under `Live`; and `settle_playheads` under
/// `Live` puts **each** layer on its own newest frame rather than every layer
/// on the latest frame at or before one layer's stamp.
///
/// So this asserts the mode *and* the frame, and the frame is asserted as the
/// **last** stamp — which is what makes "always start at frame 0" fail here
/// rather than pass.
///
/// **Floor D — `park_every_loop`:** take the `extends_future` test out of
/// `loop_start_frame` and park unconditionally on
/// `qualifying_frame_at(AsOf(now))`. The mode assertion fails: radar's clock
/// becomes `AsOf`, and every `EventLifetime` layer on the pane starts
/// re-rasterizing per instant.
#[test]
fn a_radar_loop_starts_on_live_and_the_clock_stays_a_live_clock() {
    let ctx = egui::Context::default();
    let base = wall();
    let minute = chrono::Duration::minutes(5);
    let stamps = vec![base - minute * 3, base - minute * 2, base - minute];

    // The registry holds the forecast double, so the *only* thing separating
    // this pane from the one above is which layer its transport addresses.
    let (mut app, _asked) = app_with_frames(stamps.clone());
    {
        let pane = app.gui.pane_mut(0).expect("pane 0");
        let ls = pane.transport_state_mut();
        *ls = LayerTimeState::begin(
            3600,
            squallar_radar::types::RenderView::PlanView,
            Box::new(()),
        );
        ls.frames = textured_frames(&ctx, &stamps);
        ls.phase = LoopPhase::Ready;
    }
    assert_eq!(
        app.gui.pane(0).expect("pane 0").transport_layer(),
        &known::RADAR,
        "premise: an untouched pane's transport is radar, which is what makes \
         this the shipping path",
    );

    app.sync_loop_playback_start();

    assert_eq!(
        transport(&app).phase,
        LoopPhase::Playing,
        "radar's loop no longer starts",
    );
    assert_eq!(
        clock(&app),
        TimeMode::Live,
        "radar's loop started on an AsOf clock. `Live` is a mode, not frame \
         `len() - 1`: `as_of_term` returns 0 only under `Live`, so this change \
         alone makes every EventLifetime layer on the pane rasterize once per \
         depicted instant.",
    );
    assert_eq!(
        shown(&app),
        stamps.last().copied(),
        "radar's loop opened on a frame other than its newest, which is the \
         frame it has always opened on",
    );
}

/// **A radar-only pane's tick sequence is what it always was.**
///
/// The companion to the mode assertion above: the start is one frame, and this
/// is the walk. It fails on any change to what `advance_loop_playback` does on
/// the shipping path — a different starting frame, a different direction, a
/// missed wrap.
///
/// **Floor C again** (`clock_layer_tick`) leaves this **green**, and that is
/// the point: on a radar-only pane `clock_layer()` and `transport_layer()` are
/// the same layer, so the retarget is a no-op here. The non-triviality is
/// carried by the flip test, and this one carries the guarantee that the
/// no-op really is one.
#[test]
fn the_tick_sequence_of_a_radar_only_pane_is_unchanged() {
    let ctx = egui::Context::default();
    let base = wall();
    let minute = chrono::Duration::minutes(5);
    let stamps = vec![base - minute * 3, base - minute * 2, base - minute];

    let (mut app, _asked) = app_with_frames(Vec::new());
    {
        let pane = app.gui.pane_mut(0).expect("pane 0");
        let ls = pane.transport_state_mut();
        *ls = LayerTimeState::begin(
            3600,
            squallar_radar::types::RenderView::PlanView,
            Box::new(()),
        );
        ls.frames = textured_frames(&ctx, &stamps);
        ls.phase = LoopPhase::Ready;
    }
    app.sync_loop_playback_start();

    assert_eq!(
        tick_sequence(&mut app, 4)
            .into_iter()
            .map(|s| s.expect("every frame is textured"))
            .collect::<Vec<_>>(),
        vec![stamps[0], stamps[1], stamps[2], stamps[0]],
        "a radar loop's tick sequence moved. It starts on the newest frame, \
         wraps to the oldest and walks forward; anything else is a change to \
         the one loop this application ships.",
    );
}

/// **A parked pane keeps its instant when its loop arms.**
///
/// `loop_start_frame` returns `None` for every transport that does not extend
/// into the future — which is every radar pane — so the `Live` alignment arm is
/// the common path, not an edge. Applied to a pane the user scrubbed to a
/// moment, it threw that moment away the instant the loop armed: a pane pinned
/// to the 2013 Moore volume came back showing the current afternoon.
///
/// **Non-vacuity floor**: the live pane in the same table must still be aligned
/// to `Live`, so "never touch the clock" does not pass either.
///
/// **WHAT THIS DOES NOT PIN.** It replicates the match rather than driving
/// `start_ready_loops`, which needs an `App` with render-ready loop state. So it
/// pins the intended semantics, not the call site: edit the real arm and this
/// still passes. Reaching the real path needs the loop harness, and this is
/// named honestly rather than left to read as a gate it is not.
#[test]
fn arming_a_loop_leaves_a_parked_pane_on_its_instant() {
    let parked = chrono::NaiveDate::from_ymd_opt(2013, 5, 20)
        .unwrap()
        .and_hms_opt(19, 59, 0)
        .unwrap();

    for (start_mode, expected) in [
        (
            squallar_egui::pane::TimeMode::AsOf(parked),
            squallar_egui::pane::TimeMode::AsOf(parked),
        ),
        (
            squallar_egui::pane::TimeMode::Live,
            squallar_egui::pane::TimeMode::Live,
        ),
    ] {
        let mut gui = squallar_egui::Gui::new();
        gui.pane_mut(0).expect("pane 0").time.mode = start_mode;

        // The arm the fix guards: `park` is `None` for a radar transport.
        let pane = gui.pane_mut(0).expect("pane 0");
        match None::<usize> {
            Some(index) => {
                pane.park_on_transport_frame(index);
            }
            None if pane.time.mode.as_of().is_some() => pane.settle_playheads(),
            None => pane.set_time_mode(squallar_egui::pane::TimeMode::Live),
        }

        assert_eq!(
            pane.time.mode, expected,
            "starting from {start_mode:?} the loop-arm must leave the clock at {expected:?}"
        );
    }
}
