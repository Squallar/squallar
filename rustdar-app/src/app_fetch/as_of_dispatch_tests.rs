//! **WO-E7c: which instant the rasterizer is told the picture depicts.**
//!
//! The dispatch half of the same rule the cache token spells: an
//! `EventLifetime` layer on a scrubbed pane rasterizes *then*, everything else
//! keeps the wall clock — including every layer on a live pane, which is what
//! makes WO-M11's dark parity permanent instead of provisional.

use super::as_of_for_layer;
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
/// thirteen and nothing rasterizes differently because the field exists.
#[test]
fn a_live_pane_tells_every_layer_the_picture_depicts_now() {
    let mut gui = Gui::new();
    gui.pane_mut(0)
        .expect("pane 0")
        .set_time_mode(TimeMode::Live);
    let clock = ts(30);

    let layers = declared(&gui);
    assert_eq!(
        layers.len(),
        13 + cfg!(feature = "fake-source") as usize,
        "the walk below must cover every layer",
    );
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
