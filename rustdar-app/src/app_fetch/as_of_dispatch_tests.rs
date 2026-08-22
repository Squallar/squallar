//! **WO-E7c: which instant the rasterizer is told the picture depicts.**
//!
//! The dispatch half of the same rule the cache token spells: an
//! `EventLifetime` layer on a scrubbed pane rasterizes *then*, everything else
//! keeps the wall clock — including every layer on a live pane, which is what
//! makes WO-M11's dark parity permanent instead of provisional.

use super::{as_of_for_layer, fetch_config_for_layer};
use rustdar_egui::Gui;
use rustdar_egui::pane::TimeMode;
use rustdar_source::id::LayerId;
use rustdar_source::time::TimeAxis;

fn ts(minute: u32) -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2024, 6, 1)
        .unwrap()
        .and_hms_opt(12, minute, 0)
        .unwrap()
}

/// Every registered layer, with the arm it declares.
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
    let event: Vec<&LayerId> = layers
        .iter()
        .filter(|(_, axis)| matches!(axis, TimeAxis::EventLifetime))
        .map(|(id, _)| id)
        .collect();
    assert!(
        event.len() >= 2,
        "non-triviality floor: alerts and lightning both declare EventLifetime",
    );
    assert!(
        event.len() < layers.len(),
        "non-triviality floor: and not every layer does, so \"only\" is a \
         distinguishable claim",
    );

    for (id, axis) in &layers {
        let want = if matches!(axis, TimeAxis::EventLifetime) {
            scrub
        } else {
            clock
        };
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
        as_of_for_layer(&gui, 999, &rustdar_source::id::known::NWS_ALERTS, clock),
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
    let (event, rest): (Vec<_>, Vec<_>) = layers
        .iter()
        .partition(|(_, axis)| matches!(axis, TimeAxis::EventLifetime));
    assert!(
        !event.is_empty() && !rest.is_empty(),
        "non-triviality floor: both sides of the distinction must be occupied",
    );

    for (id, axis) in &layers {
        let want = if matches!(axis, TimeAxis::EventLifetime) {
            scrub
        } else {
            clock
        };
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
/// `depicted_span_for_layer`.** Every non-event layer then reads `Some(7200)`
/// where it must read `None`, and the count assertion is what refuses a
/// `depicted_span_for_layer` that answers the span unconditionally.
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

fn base_config(
    clock: chrono::NaiveDateTime,
) -> rustdar_overlays::render::overlay_state::FetchConfig {
    rustdar_overlays::render::overlay_state::FetchConfig {
        client: {
            rustdar_source::tls::init();
            reqwest::Client::new()
        },
        zone_cache_dir: None,
        sources: rustdar_radar::sources::DataSources::production(),
        viewport: None,
        as_of: clock,
        depicted_span_secs: None,
    }
}
