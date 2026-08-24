//! **WO-E7c: which instant the rasterizer is told the picture depicts.**
//!
//! The dispatch half of the same rule the cache token spells: an
//! `EventLifetime` layer on a scrubbed pane rasterizes *then*, everything else
//! keeps the wall clock — including every layer on a live pane, which is what
//! makes WO-M11's dark parity permanent instead of provisional.

use super::{as_of_for_layer, fetch_config_for_layer};
use squallar_egui::Gui;
use squallar_egui::pane::TimeMode;
use squallar_source::id::LayerId;
use squallar_source::time::TimeAxis;

fn ts(minute: u32) -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2024, 6, 1)
        .unwrap()
        .and_hms_opt(12, minute, 0)
        .unwrap()
}

/// Every registered layer, with the arm it declares.
/// **Whether a layer's answer to "what do you show at `T`" depends on `T`.**
///
/// Both arms that have a past: an `EventLifetime` layer's picture is which of
/// its items are valid then, a `FrameSeries` layer's is the newest frame at or
/// before then. `Live` is the arm that declares no history, so its answer is
/// the same at every instant and it keeps the wall clock.
///
/// One predicate for both laws below, and it reads the DECLARED arm rather than
/// naming layers — so a layer that changes arm changes what these assert
/// instead of drifting from it.
fn has_a_past(axis: &TimeAxis) -> bool {
    matches!(axis, TimeAxis::EventLifetime | TimeAxis::FrameSeries { .. })
}

fn declared(gui: &Gui) -> Vec<(LayerId, TimeAxis)> {
    gui.overlays
        .handlers()
        .map(|h| (h.id(), h.time_axis()))
        .collect()
}

/// **A live pane hands every layer the wall clock**, so `as_of == now` for all
/// fifteen and nothing rasterizes differently because the field exists.
#[test]
fn a_live_pane_tells_every_layer_the_picture_depicts_now() {
    let mut gui = Gui::new();
    gui.pane_mut(0)
        .expect("pane 0")
        .set_time_mode(TimeMode::Live);
    let clock = ts(30);

    let layers = declared(&gui);
    assert_eq!(layers.len(), 15, "the walk below must cover every layer",);
    for (id, _) in &layers {
        assert_eq!(
            as_of_for_layer(&gui, 0, id, clock),
            clock,
            "{} was told a live pane depicts something other than now",
            id.as_str(),
        );
    }
}

/// **A scrubbed pane moves exactly the as-of-dependent layers.** Read off each
/// layer's declared arm rather than a second list, so a layer that changes arm
/// changes this answer instead of drifting from it.
#[test]
fn a_scrubbed_pane_moves_the_clock_for_the_as_of_dependent_layers_only() {
    let mut gui = Gui::new();
    let scrub = ts(10);
    let clock = ts(30);
    gui.pane_mut(0)
        .expect("pane 0")
        .set_time_mode(TimeMode::AsOf(scrub));

    let layers = declared(&gui);
    let moved: Vec<&LayerId> = layers
        .iter()
        .filter(|(_, axis)| has_a_past(axis))
        .map(|(id, _)| id)
        .collect();
    assert!(
        moved.len() >= 2,
        "non-triviality floor: alerts, lightning and the three frame layers \
         all have a past",
    );
    assert!(
        moved.len() < layers.len(),
        "non-triviality floor: and not every layer does, so \"only\" is a \
         distinguishable claim",
    );

    for (id, axis) in &layers {
        let want = if has_a_past(axis) { scrub } else { clock };
        assert_eq!(
            as_of_for_layer(&gui, 0, id, clock),
            want,
            "{} declares {axis:?}: a scrubbed pane must tell it {want}",
            id.as_str(),
        );
    }
}

/// A pane index that names no pane falls back to the clock rather than
/// inventing an instant — the dispatch reaches here after the pane could have
/// gone away.
#[test]
fn a_pane_that_is_not_there_falls_back_to_the_wall_clock() {
    let gui = Gui::new();
    let clock = ts(30);
    assert_eq!(
        as_of_for_layer(&gui, 999, &squallar_source::id::known::NWS_ALERTS, clock),
        clock,
    );
}

/// **The fetch context carries the same instant the paint context does.**
///
/// This is the seam the GLM archive rides on: `list_glm_files` is addressed by
/// `{year}/{doy}/{hour}`, so a poll built with the wall clock can only ever
/// reach the current hour no matter what the pane depicts. Read off each
/// layer's declared arm, so nothing here names a layer.
#[test]
fn a_scrubbed_panes_fetch_is_built_for_the_instant_it_depicts() {
    let mut gui = Gui::new();
    let scrub = ts(10);
    let clock = ts(30);
    gui.pane_mut(0)
        .expect("pane 0")
        .set_time_mode(TimeMode::AsOf(scrub));

    let layers = declared(&gui);
    let (moved, rest): (Vec<_>, Vec<_>) = layers.iter().partition(|(_, axis)| has_a_past(axis));
    assert!(
        !moved.is_empty() && !rest.is_empty(),
        "non-triviality floor: both sides of the distinction must be occupied",
    );

    for (id, axis) in &layers {
        let want = if has_a_past(axis) { scrub } else { clock };
        let config = fetch_config_for_layer(&gui, 0, id, base_config(clock));
        assert_eq!(
            config.as_of,
            want,
            "{} declares {axis:?}: its poll must be built for {want}",
            id.as_str(),
        );
    }
}

/// The other side: a live pane's poll is the one it always was, for every
/// layer — so "fetch the archive" cannot be reached by fetching the archive
/// always.
#[test]
fn a_live_panes_fetch_keeps_the_wall_clock_for_every_layer() {
    let mut gui = Gui::new();
    gui.pane_mut(0)
        .expect("pane 0")
        .set_time_mode(TimeMode::Live);
    let clock = ts(30);

    for (id, _) in declared(&gui) {
        assert_eq!(
            fetch_config_for_layer(&gui, 0, &id, base_config(clock)).as_of,
            clock,
            "{} would poll a live pane for some other instant",
            id.as_str(),
        );
    }
}

/// **The span half, under the same two predicates as the instant half.**
///
/// A pane on `AsOf` — parked or *playing a loop* — hands `as_of` one sampled
/// instant of a clock that sweeps the whole timeline span between polls. The
/// span is what lets an `EventLifetime` source retain the window the pane can
/// depict rather than the sample: GLM anchored on the sample alone lit a
/// two-hour loop on one frame.
///
/// **Floor — `span_for_every_layer`: drop the `EventLifetime` test from
/// `depicted_reach_for_layer`.** Every non-event layer then reads `Some(7200)`
/// where it must read `None`, and the count assertion is what refuses a
/// `depicted_reach_for_layer` that answers the span unconditionally.
///
/// This pane has **no loop armed**, so the reach is its own posture — the
/// Lookback slider — and the figure is unchanged by the loop-window read that
/// `a_poll_before_the_first_listing_asks_for_the_loops_window` pins.
#[test]
fn a_pane_on_a_loops_clock_hands_its_span_to_the_as_of_dependent_layers_only() {
    let mut gui = Gui::new();
    let span_secs = 7200;
    {
        let pane = gui.pane_mut(0).expect("pane 0");
        pane.time.span_secs = span_secs;
        // What a playing loop writes every tick: the frame it landed on.
        pane.set_time_mode(TimeMode::AsOf(ts(10)));
    }
    let clock = ts(30);

    let layers = declared(&gui);
    let event_count = layers
        .iter()
        .filter(|(_, axis)| matches!(axis, TimeAxis::EventLifetime))
        .count();
    assert!(
        event_count >= 2 && event_count < layers.len(),
        "non-triviality floor: both sides of the distinction must be \
         occupied, or \"only\" is not a claim. {event_count} of {} declare \
         EventLifetime",
        layers.len(),
    );

    let mut spanned = 0;
    for (id, axis) in &layers {
        let want = if matches!(axis, TimeAxis::EventLifetime) {
            Some(span_secs)
        } else {
            None
        };
        let config = fetch_config_for_layer(&gui, 0, id, base_config(clock));
        assert_eq!(
            config.depicted_span_secs,
            want,
            "{} declares {axis:?}: its poll must carry {want:?}",
            id.as_str(),
        );
        if config.depicted_span_secs.is_some() {
            spanned += 1;
        }
    }
    assert_eq!(
        spanned, event_count,
        "exactly the as-of-dependent layers may widen their poll",
    );
}

/// The parity clause for the span, and the reason "always widen" cannot pass:
/// a live pane's poll carries no span at all, so every quantity inside
/// `fetch_glm_flashes` is byte-for-byte what it was.
#[test]
fn a_live_panes_fetch_carries_no_span_for_any_layer() {
    let mut gui = Gui::new();
    gui.pane_mut(0).expect("pane 0").time.span_secs = 7200;
    gui.pane_mut(0)
        .expect("pane 0")
        .set_time_mode(TimeMode::Live);
    let clock = ts(30);

    let layers = declared(&gui);
    assert_eq!(layers.len(), 15, "the walk below must cover every layer");
    for (id, _) in &layers {
        assert_eq!(
            fetch_config_for_layer(&gui, 0, id, base_config(clock)).depicted_span_secs,
            None,
            "{} would widen a live pane's poll",
            id.as_str(),
        );
    }
}

/// **The instants half, under the same two predicates as the span half.**
///
/// The span says how *wide* the pane's timeline is; the frames say *where*
/// inside it the clock can stop, and the two are different numbers whenever a
/// layer raises the window its loop is listed over above the Lookback setting
/// (`SourceHandler::min_loop_span_secs`). A poll told only the setting reaches
/// one frame of a satellite loop's thirteen.
///
/// **Floor — `frames_for_everyone`: drop the `EventLifetime` test from
/// `depicted_frames_for_layer`.** Every non-event layer then carries the
/// transport's stamps where it must carry none, and the count assertion is
/// what refuses it.
#[test]
fn a_pane_on_a_loops_clock_names_its_frames_to_the_as_of_dependent_layers_only() {
    let mut gui = Gui::new();
    let stamps: Vec<chrono::NaiveDateTime> = (0..4).map(|k| ts(10 + k * 5)).collect();
    {
        let pane = gui.pane_mut(0).expect("pane 0");
        let mut timeline = squallar_egui::pane::LayerTimeState::new();
        timeline.phase = squallar_egui::pane::LoopPhase::Playing;
        timeline.frames = stamps
            .iter()
            .map(|at| squallar_egui::pane::LoopFrame {
                timestamp: *at,
                image: None,
                render_in_flight: false,
                render_failed: false,
            })
            .collect();
        *pane.transport_state_mut() = timeline;
        pane.set_time_mode(TimeMode::AsOf(stamps[0]));
    }
    let clock = ts(30);

    let layers = declared(&gui);
    let event_count = layers
        .iter()
        .filter(|(_, axis)| matches!(axis, TimeAxis::EventLifetime))
        .count();
    assert!(
        event_count >= 2 && event_count < layers.len(),
        "non-triviality floor: both sides of the distinction must be \
         occupied, or \"only\" is not a claim. {event_count} of {} declare \
         EventLifetime",
        layers.len(),
    );

    let mut named = 0;
    for (id, axis) in &layers {
        let want: Vec<chrono::NaiveDateTime> = if matches!(axis, TimeAxis::EventLifetime) {
            stamps.clone()
        } else {
            Vec::new()
        };
        let config = fetch_config_for_layer(&gui, 0, id, base_config(clock));
        assert_eq!(
            config.depicted_frames,
            want,
            "{} declares {axis:?}: its poll must name {} instants",
            id.as_str(),
            want.len(),
        );
        if !config.depicted_frames.is_empty() {
            named += 1;
        }
    }
    assert_eq!(
        named, event_count,
        "exactly the as-of-dependent layers may be told where the clock stops",
    );
}

/// The parity clause: a live pane's poll names no instant at all, so every
/// quantity inside `fetch_glm_flashes` stays the one it always was.
#[test]
fn a_live_panes_fetch_names_no_frames_for_any_layer() {
    let mut gui = Gui::new();
    {
        let pane = gui.pane_mut(0).expect("pane 0");
        let mut timeline = squallar_egui::pane::LayerTimeState::new();
        timeline.phase = squallar_egui::pane::LoopPhase::Playing;
        timeline.frames = vec![squallar_egui::pane::LoopFrame {
            timestamp: ts(10),
            image: None,
            render_in_flight: false,
            render_failed: false,
        }];
        *pane.transport_state_mut() = timeline;
        pane.set_time_mode(TimeMode::Live);
    }
    let clock = ts(30);

    for (id, _) in declared(&gui) {
        assert!(
            fetch_config_for_layer(&gui, 0, &id, base_config(clock))
                .depicted_frames
                .is_empty(),
            "{} would tell a live pane's poll where a clock stops",
            id.as_str(),
        );
    }
}

fn base_config(
    clock: chrono::NaiveDateTime,
) -> squallar_overlays::render::overlay_state::FetchConfig {
    squallar_overlays::render::overlay_state::FetchConfig {
        client: {
            squallar_source::tls::init();
            reqwest::Client::new()
        },
        zone_cache_dir: None,
        sources: squallar_radar::sources::DataSources::production(),
        viewport: None,
        as_of: clock,
        depicted_span_secs: None,
        depicted_frames: Vec::new(),
    }
}

/// **The acceptance for the loop's own window.** A poll landing between
/// `handle_enable_loop` and the transport's listing is told the window the
/// loop was **armed** over, not the Lookback slider.
///
/// The slider names one span for the whole application; a transport layer
/// raises the window its loop is listed over to its own `min_loop_span_secs`
/// floor through `Gui::loop_span_secs_for`, so a satellite loop is armed over
/// twelve hours while the slider still reads one. `depicted_frames` is empty
/// in this window — the listing has not landed — so `DepictedWindow` falls
/// through to the span, and the span was the slider's hour. A GLM poll was
/// told 1 h for a 12 h loop, and `cache.evict_before(cutoff)` was anchored an
/// eleven hours too late.
#[test]
fn a_poll_before_the_first_listing_asks_for_the_loops_window() {
    const SLIDER: u64 = 3600;
    const ARMED: u64 = 12 * 3600;

    let mut gui = Gui::new();
    let transport = LayerId::new("test/satellite-transport");
    {
        let pane = gui.pane_mut(0).expect("pane 0");
        pane.time.span_secs = SLIDER;
        pane.set_transport_layer(transport.clone());
        let ls = pane.transport_state_mut();
        // The state `arm_layer_loop` leaves behind: the window is recorded,
        // the listing is in the air, and no frame has been built yet.
        ls.phase = squallar_egui::pane::LoopPhase::FetchingScanList;
        ls.span_secs = ARMED;
        pane.set_time_mode(TimeMode::AsOf(ts(30)));
        assert!(
            pane.transport_state().frames.is_empty(),
            "premise: this is the window before the first listing lands, so \
             `depicted_frames` cannot answer and the span is what is read",
        );
    }
    let clock = ts(45);

    let events: Vec<LayerId> = declared(&gui)
        .into_iter()
        .filter(|(_, axis)| matches!(axis, TimeAxis::EventLifetime))
        .map(|(id, _)| id)
        .collect();
    assert!(
        events.len() >= 2,
        "non-triviality: the claim is about the as-of-dependent layers, and \
         there must be some",
    );
    for id in &events {
        assert_eq!(
            fetch_config_for_layer(&gui, 0, id, base_config(clock)).depicted_span_secs,
            Some(ARMED),
            "{} was told the slider's {SLIDER}s for a loop armed over {ARMED}s",
            id.as_str(),
        );
    }
}

/// **The floor.** A pane with **no** loop armed still reads exactly what it
/// read before — its own posture — so "always take the timeline's span" cannot
/// pass the acceptance above.
///
/// An inactive timeline's `span_secs` is whatever the last loop left in it, and
/// a pane that has never looped reads zero: taking it unconditionally would
/// silently narrow a parked scrub's poll to nothing.
#[test]
fn a_parked_pane_with_no_loop_still_asks_for_its_own_span() {
    const SLIDER: u64 = 7200;

    let mut gui = Gui::new();
    {
        let pane = gui.pane_mut(0).expect("pane 0");
        pane.time.span_secs = SLIDER;
        // A timeline left behind by a loop that has been switched off: the
        // width is still recorded and must NOT be read.
        let ls = pane.transport_state_mut();
        ls.phase = squallar_egui::pane::LoopPhase::Inactive;
        ls.span_secs = 12 * 3600;
        pane.set_time_mode(TimeMode::AsOf(ts(30)));
    }
    let clock = ts(45);

    for (id, axis) in declared(&gui) {
        let want = matches!(axis, TimeAxis::EventLifetime).then_some(SLIDER);
        assert_eq!(
            fetch_config_for_layer(&gui, 0, &id, base_config(clock)).depicted_span_secs,
            want,
            "{} on a pane with no loop must read its own posture",
            id.as_str(),
        );
    }
}
