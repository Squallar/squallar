use std::collections::BTreeMap;

use super::*;

const SCREEN: egui::Rect = egui::Rect {
    min: egui::pos2(0.0, 0.0),
    max: egui::pos2(1024.0, 768.0),
};

/// A believable widget map for the UiSweep unit tests: three eyes, the two
/// top-bar toggles, the inspector's close button and one slider.
fn seeded_targets() -> BTreeMap<String, egui::Rect> {
    let at = |x: f32, y: f32| egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(24.0, 16.0));
    BTreeMap::from([
        (format!("{}alpha", ui_sweep::EYE_PREFIX), at(40.0, 100.0)),
        (format!("{}bravo", ui_sweep::EYE_PREFIX), at(40.0, 130.0)),
        (format!("{}charlie", ui_sweep::EYE_PREFIX), at(40.0, 160.0)),
        (ui_sweep::LAYERS_TOGGLE.to_owned(), at(200.0, 10.0)),
        (ui_sweep::INSPECTOR_TOGGLE.to_owned(), at(900.0, 10.0)),
        (ui_sweep::INSPECTOR_CLOSE.to_owned(), at(1000.0, 40.0)),
        (
            format!("{}GLM_opacity", ui_sweep::SLIDER_PREFIX),
            at(850.0, 200.0),
        ),
    ])
}

fn player(script: &str) -> GesturePlayer {
    GesturePlayer::from_name(script).expect("a known script name")
}

/// A deliberately uneven frame cadence, so nothing accidentally depends on a
/// fixed dt.
fn jittered_times(frames: usize) -> Vec<f64> {
    let pattern = [1.0 / 60.0, 1.0 / 45.0, 1.0 / 144.0, 1.0 / 30.0, 1.0 / 90.0];
    let mut t = 0.0;
    (0..frames)
        .map(|i| {
            t += pattern[i % pattern.len()];
            t
        })
        .collect()
}

/// One loop's worth of frames at 60 fps, with the events each frame emitted.
fn one_loop(script: &str, targets: &BTreeMap<String, egui::Rect>) -> Vec<(f64, Vec<egui::Event>)> {
    let mut p = player(script);
    let dt = 1.0 / 60.0;
    let mut out = Vec::new();
    let mut t = 0.0;
    while t < LOOP_SECONDS - 1e-9 {
        t += dt;
        let t_clamped = t.min(LOOP_SECONDS - 1e-6);
        out.push((t_clamped, p.events_with_targets(t_clamped, SCREEN, targets)));
    }
    out
}

#[test]
fn the_same_elapsed_times_replay_the_same_stream() {
    for script in ["pan-zoom-2d", "orbit-3d", "pinch-2d", "ui-sweep"] {
        let targets = seeded_targets();
        let times = jittered_times(2400); // ~2 loops of uneven cadence
        let mut a = player(script);
        let mut b = player(script);
        for (i, t) in times.iter().enumerate() {
            let ea = a.events_with_targets(*t, SCREEN, &targets);
            let eb = b.events_with_targets(*t, SCREEN, &targets);
            assert_eq!(
                ea, eb,
                "{script}: frame {i} at t={t} diverged between two replays \
                 of the same elapsed-time sequence"
            );
        }
    }
}

/// Replays the stream against an emulated pointer and returns the net
/// dragged displacement (pointer deltas while the button was down) and the
/// net wheel delta.
fn net_drag_and_wheel(frames: &[(f64, Vec<egui::Event>)]) -> (egui::Vec2, f32) {
    let mut down = false;
    let mut pos: Option<egui::Pos2> = None;
    let mut net = egui::Vec2::ZERO;
    let mut wheel = 0.0;
    for (_, events) in frames {
        for event in events {
            match event {
                egui::Event::PointerMoved(p) => {
                    if down && let Some(prev) = pos {
                        net += *p - prev;
                    }
                    pos = Some(*p);
                }
                egui::Event::PointerButton {
                    pos: p, pressed, ..
                } => {
                    if *pressed {
                        down = true;
                    } else {
                        if down && let Some(prev) = pos {
                            net += *p - prev;
                        }
                        down = false;
                    }
                    pos = Some(*p);
                }
                egui::Event::MouseWheel { delta, .. } => wheel += delta.y,
                _ => {}
            }
        }
    }
    (net, wheel)
}

/// The spike measured 2744 km of drift from un-mirrored strokes; the mirrored
/// pairs and the equal in/out notch legs are what re-centre each loop.
#[test]
fn a_pan_zoom_loop_nets_zero_drag_and_zero_wheel() {
    let frames = one_loop("pan-zoom-2d", &BTreeMap::new());
    let (net, wheel) = net_drag_and_wheel(&frames);
    assert!(
        net.length() < 0.01,
        "the drag strokes left a net pan of {net:?} points per loop"
    );
    assert_eq!(wheel, 0.0, "the zoom legs left a net wheel delta");
}

#[test]
fn an_orbit_loop_nets_zero_drag_and_zero_wheel() {
    let frames = one_loop("orbit-3d", &BTreeMap::new());
    let (net, wheel) = net_drag_and_wheel(&frames);
    assert!(
        net.length() < 0.01,
        "the closed Lissajous path left a net orbit of {net:?} points per loop"
    );
    assert_eq!(wheel, 0.0, "the dolly legs left a net wheel delta");
}

/// A pinch session's zoom is its last finger gap over its first; the loop is
/// net-zero only if the product over all four sessions is exactly one, which
/// the boundary-gap release guarantees whatever the cadence sampled.
#[test]
fn a_pinch_loop_returns_the_gap_ratio_to_one() {
    let frames = one_loop("pinch-2d", &BTreeMap::new());
    let mut a: Option<egui::Pos2> = None;
    let mut b: Option<egui::Pos2> = None;
    let mut first_gap: Option<f32> = None;
    let mut last_gap = 0.0f32;
    let mut ratio = 1.0f64;
    for (_, events) in &frames {
        for event in events {
            if let egui::Event::Touch { id, phase, pos, .. } = event {
                match (id.0, phase) {
                    (crate::input_fidelity::WEB_FINGER_A, egui::TouchPhase::End) => {
                        ratio *= f64::from(last_gap) / f64::from(first_gap.take().expect("gap"));
                        a = None;
                        b = None;
                    }
                    (crate::input_fidelity::WEB_FINGER_A, _) => a = Some(*pos),
                    (crate::input_fidelity::WEB_FINGER_B, egui::TouchPhase::End) => {}
                    (crate::input_fidelity::WEB_FINGER_B, _) => b = Some(*pos),
                    _ => {}
                }
                if let (Some(a), Some(b)) = (a, b) {
                    last_gap = (b - a).length();
                    if first_gap.is_none() {
                        first_gap = Some(last_gap);
                    }
                }
            }
        }
    }
    assert!(
        (ratio - 1.0).abs() < 1e-6,
        "one pinch loop multiplied the zoom by {ratio}"
    );
}

/// K is a published constant per script; this pins it to the schedule it
/// claims to describe. A quiet phase here is a maximal event-free window of
/// at least [`QUIET_MIN_SECONDS`] (with a little slack for frame
/// quantisation), which is what lets a settle fire inside every one.
#[test]
fn each_loop_contains_the_scripted_quiet_phases() {
    for (script, expected) in [
        ("pan-zoom-2d", pan_zoom_2d::QUIET_PHASES),
        ("orbit-3d", orbit_3d::QUIET_PHASES),
        ("pinch-2d", pinch_2d::QUIET_PHASES),
        ("ui-sweep", ui_sweep::QUIET_PHASES),
    ] {
        let targets = seeded_targets();
        let frames = one_loop(script, &targets);
        let mut emitting: Vec<f64> = frames
            .iter()
            .filter(|(_, events)| !events.is_empty())
            .map(|(t, _)| *t)
            .collect();
        emitting.push(LOOP_SECONDS);
        let quiet = emitting
            .windows(2)
            .filter(|w| w[1] - w[0] >= QUIET_MIN_SECONDS - 0.1)
            .count() as u32;
        assert_eq!(
            quiet, expected,
            "{script}: the schedule shows {quiet} quiet phases per loop, \
             the published constant says {expected}"
        );
    }
}

/// The gate the click registry exists for: over one sweep loop, every
/// scheduled target receives exactly its scheduled press/release pairs —
/// each eye twice (off, then on), the layers toggle twice (close, open), the
/// inspector toggle once, the close button once, the slider once.
#[test]
fn a_sweep_loop_delivers_the_scheduled_pairs() {
    let targets = seeded_targets();
    let mut p = player("ui-sweep");
    let dt = 1.0 / 60.0;
    let mut t = 0.0;
    while t < LOOP_SECONDS - 1e-9 {
        t += dt;
        p.events_with_targets(t.min(LOOP_SECONDS - 1e-6), SCREEN, &targets);
    }
    let expected: BTreeMap<String, u32> = BTreeMap::from([
        (format!("{}alpha", ui_sweep::EYE_PREFIX), 2),
        (format!("{}bravo", ui_sweep::EYE_PREFIX), 2),
        (format!("{}charlie", ui_sweep::EYE_PREFIX), 2),
        (ui_sweep::LAYERS_TOGGLE.to_owned(), 2),
        (ui_sweep::INSPECTOR_TOGGLE.to_owned(), 1),
        (ui_sweep::INSPECTOR_CLOSE.to_owned(), 1),
        (format!("{}GLM_opacity", ui_sweep::SLIDER_PREFIX), 1),
    ]);
    assert_eq!(p.pairs_delivered(), &expected);
}

/// Presses and releases stay balanced through the wrap, and the wrap is
/// where the loop marker's frame count comes from.
#[test]
fn a_loop_wrap_releases_everything_and_counts_loops() {
    for script in ["pan-zoom-2d", "orbit-3d", "pinch-2d", "ui-sweep"] {
        let targets = seeded_targets();
        let mut p = player(script);
        let dt = 1.0 / 60.0;
        let mut t = 0.0;
        let mut presses = 0i64;
        let mut releases = 0i64;
        // Ends inside the third loop's quiet tail, where every script has
        // let go of everything — a mid-gesture cut would show one open press
        // that is no imbalance at all.
        while t < 2.0 * LOOP_SECONDS + 19.0 {
            t += dt;
            for event in p.events_with_targets(t, SCREEN, &targets) {
                if let egui::Event::PointerButton { pressed, .. } = event {
                    if pressed {
                        presses += 1;
                    } else {
                        releases += 1;
                    }
                }
            }
        }
        assert_eq!(p.loops_completed(), 2, "{script}");
        assert_eq!(
            presses, releases,
            "{script}: a press crossed the wrap without its release"
        );
    }
}

/// The non-vacuity half the player owns: an armed player speaks on its first
/// frame. (UiSweep necessarily waits for the registry's first snapshot, so
/// its floor is "within the first second".)
#[test]
fn an_armed_player_emits_events_immediately() {
    for script in ["pan-zoom-2d", "orbit-3d", "pinch-2d"] {
        let mut p = player(script);
        let events = p.events_with_targets(1.0 / 60.0, SCREEN, &BTreeMap::new());
        assert!(
            !events.is_empty(),
            "{script}: the first armed frame was silent"
        );
    }
    let targets = seeded_targets();
    let mut p = player("ui-sweep");
    let mut t = 0.0;
    let mut total = 0;
    while t < 1.0 {
        t += 1.0 / 60.0;
        total += p.events_with_targets(t, SCREEN, &targets).len();
    }
    assert!(total > 0, "ui-sweep: a whole armed second was silent");
}

#[test]
fn an_unknown_name_arms_no_player() {
    assert!(GesturePlayer::from_name("no-such-script").is_none());
}

#[test]
fn every_script_name_round_trips() {
    for script in [
        GestureScript::PanZoom2D,
        GestureScript::Orbit3D,
        GestureScript::Pinch2D,
        GestureScript::UiSweep,
    ] {
        assert_eq!(GestureScript::from_name(script.name()), Some(script));
    }
}

/// The rig brackets its bin-diffs with these exact sentences; a reworded
/// marker silently un-brackets every measurement leg.
#[test]
fn the_marker_lines_are_pinned() {
    assert_eq!(
        begin_line(GestureScript::PanZoom2D),
        "gesture script pan-zoom-2d begin"
    );
    assert_eq!(
        loop_complete_line(GestureScript::UiSweep, 1203),
        "gesture script ui-sweep loop complete: 1203 frames"
    );
}

/// The registry side of dormancy: nothing registers unless a sweep is
/// collecting, and taking the frame empties it.
#[test]
fn the_registry_collects_only_while_armed() {
    let rect = egui::Rect::from_min_size(egui::pos2(1.0, 2.0), egui::vec2(3.0, 4.0));
    click_registry::register("dormant", rect);
    assert!(click_registry::take_frame().is_empty());

    click_registry::set_collecting(true);
    click_registry::register("armed", rect);
    let taken = click_registry::take_frame();
    assert_eq!(taken.len(), 1);
    assert!(taken.contains_key("armed"));
    assert!(
        click_registry::take_frame().is_empty(),
        "taking the frame must expire the entries"
    );
    click_registry::set_collecting(false);
}
