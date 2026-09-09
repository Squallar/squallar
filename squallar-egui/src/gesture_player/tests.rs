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

/// The native window the scene C legs run at; the pan-zoom press point is
/// its centre, (960, 540).
const NATIVE: egui::Rect = egui::Rect {
    min: egui::pos2(0.0, 0.0),
    max: egui::pos2(1920.0, 1080.0),
};

/// A 60 fps cadence in which every stroke's first frame lands 40 ms late —
/// the frame gap that put a press 25–40 pt down its stroke on the 2026-09-02
/// scene C legs. `period` is the stroke period the gaps straddle.
fn late_first_frames(period: f64, until: f64) -> Vec<f64> {
    let dt = 1.0 / 60.0;
    let mut out = Vec::new();
    let mut t = 0.0;
    while t < until {
        let next = t + dt;
        let boundary = (next / period).floor() * period;
        t = if boundary > t { boundary + 0.04 } else { next };
        out.push(t);
    }
    out
}

/// One press as the stream shows it: where it said, whether its batch moved
/// the pointer, where the pointer rested going into that frame, where the
/// batch left it (egui's hit-test point), and the displacement to its
/// release.
#[derive(Debug, PartialEq)]
struct Stroke {
    press: egui::Pos2,
    moved_in_press_batch: bool,
    rest_before: Option<egui::Pos2>,
    batch_final: egui::Pos2,
    net: egui::Vec2,
}

fn strokes(frames: &[(f64, Vec<egui::Event>)]) -> Vec<Stroke> {
    let mut out = Vec::new();
    let mut pos: Option<egui::Pos2> = None;
    let mut open: Option<usize> = None;
    for (_, events) in frames {
        let rest_before = pos;
        let mut moved = false;
        let mut pressed: Option<egui::Pos2> = None;
        for event in events {
            match event {
                egui::Event::PointerMoved(p) => {
                    moved = true;
                    pos = Some(*p);
                }
                egui::Event::PointerButton {
                    pos: p,
                    pressed: true,
                    ..
                } => {
                    pressed = Some(*p);
                    pos = Some(*p);
                }
                egui::Event::PointerButton {
                    pos: p,
                    pressed: false,
                    ..
                } => {
                    pos = Some(*p);
                    if let Some(i) = open.take() {
                        let s: &mut Stroke = &mut out[i];
                        s.net = *p - s.press;
                    }
                }
                _ => {}
            }
        }
        if let Some(press) = pressed {
            open = Some(out.len());
            out.push(Stroke {
                press,
                moved_in_press_batch: moved,
                rest_before,
                batch_final: pos.expect("a press sets the position"),
                net: egui::Vec2::ZERO,
            });
        }
    }
    out
}

fn replay(script: &str, times: &[f64], screen: egui::Rect) -> Vec<(f64, Vec<egui::Event>)> {
    let mut p = player(script);
    times
        .iter()
        .map(|t| (*t, p.events_with_targets(*t, screen, &BTreeMap::new())))
        .collect()
}

/// egui hit-tests a press at its batch's final pointer position and hands a
/// starting drag last frame's delta; so a press goes out alone, on a frame
/// after the one that parked the pointer at the press point, and the batch
/// leaves the pointer exactly where the press said — under a steady cadence
/// and under 40 ms first-frame gaps alike. On the scene C legs of
/// 2026-09-02 the batched press+move landed in the divider strip 17.67 pt
/// below (960, 540) and dragged it to its floor.
#[test]
fn a_press_goes_out_alone_where_the_pointer_already_rests() {
    for (script, period) in [
        ("pan-zoom-2d", pan_zoom_2d::STROKE_PERIOD),
        ("orbit-3d", LOOP_SECONDS),
    ] {
        for (cadence, times) in [
            ("jittered", jittered_times(1500)),
            (
                "late first frames",
                late_first_frames(period, 2.0 * LOOP_SECONDS),
            ),
        ] {
            let found = strokes(&replay(script, &times, NATIVE));
            assert!(!found.is_empty(), "{script}/{cadence}: no press at all");
            for (i, s) in found.iter().enumerate() {
                assert_eq!(s.press, NATIVE.center(), "{script}/{cadence}: press {i}");
                assert!(
                    !s.moved_in_press_batch,
                    "{script}/{cadence}: press {i}'s batch also moved the pointer"
                );
                assert_eq!(
                    s.rest_before,
                    Some(s.press),
                    "{script}/{cadence}: press {i} found the pointer somewhere else"
                );
                assert_eq!(
                    s.batch_final, s.press,
                    "{script}/{cadence}: press {i} would be hit-tested off its point"
                );
            }
        }
    }
}

/// The release pins each stroke's end, so the displacement from press to
/// release is the schedule's whatever the cadence: the same list of nets
/// from a steady 60 fps replay and from one whose first frames are 40 ms
/// late. For the orbit the one stroke per loop closes on itself.
#[test]
fn a_strokes_net_is_the_same_whatever_the_frame_gap() {
    for (script, period) in [
        ("pan-zoom-2d", pan_zoom_2d::STROKE_PERIOD),
        ("orbit-3d", LOOP_SECONDS),
    ] {
        // Two whole loops, cut in the second loop's quiet tail so neither
        // cadence opens a third loop's first stroke.
        let until = 2.0 * LOOP_SECONDS - 1.0;
        let steady: Vec<f64> = (1..=(until * 60.0) as usize)
            .map(|i| i as f64 / 60.0)
            .collect();
        let a: Vec<egui::Vec2> = strokes(&replay(script, &steady, NATIVE))
            .iter()
            .map(|s| s.net)
            .collect();
        let b: Vec<egui::Vec2> =
            strokes(&replay(script, &late_first_frames(period, until), NATIVE))
                .iter()
                .map(|s| s.net)
                .collect();
        assert_eq!(
            a.len(),
            b.len(),
            "{script}: a cadence changed the stroke count"
        );
        assert!(a.len() >= 2, "{script}: fewer than two strokes to compare");
        for (i, (x, y)) in a.iter().zip(&b).enumerate() {
            assert!(
                (*x - *y).length() < 1e-3,
                "{script}: stroke {i} nets {x:?} at 60 fps but {y:?} with late first frames"
            );
        }
        if script == "orbit-3d" {
            assert!(
                a.iter().all(|n| n.length() < 1e-3),
                "{script}: an orbit did not close: {a:?}"
            );
        }
    }
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

/// The rig driver, read at compile time so a moved file is a build failure.
const DRIVE_PY: &str = include_str!("../../../.github/browser-rig/drive.py");

/// The other end of the marker seam: the sentences above, substituted into
/// the very patterns `drive.py`'s `FRAME_LINE_PROBE` brackets bin-diffs
/// with — the same discipline `raster_telemetry_line_tests` holds for the
/// raster lines, because a pattern restated is a second place to be wrong.
#[test]
fn the_rig_brackets_with_the_markers_this_player_actually_writes() {
    let pattern = |name: &str| -> String {
        let head = format!("var {name} = /");
        let at = DRIVE_PY.find(&head).unwrap_or_else(|| {
            panic!("drive.py no longer declares `{head}…`; the rig's marker probe moved")
        });
        let rest = &DRIVE_PY[at + head.len()..];
        let end = rest
            .find("/;")
            .expect("the regex literal is not closed on its own line");
        rest[..end].to_string()
    };
    let substituted = |pat: &str, values: &[&str]| -> String {
        let mut out = pat.to_owned();
        for v in values {
            let group = [r"([a-z0-9-]+)", r"(\d+)"]
                .into_iter()
                .filter_map(|g| out.find(g).map(|at| (at, g)))
                .min()
                .expect("fewer capture groups than values offered");
            out.replace_range(group.0..group.0 + group.1.len(), v);
        }
        assert!(
            !out.contains(['\\', '(', ')', '[', ']', '*', '+', '?', '|', '^', '$']),
            "the pattern has a metacharacter outside its known groups, so \
             substitution no longer produces the sentence it matches: {out:?}",
        );
        out
    };

    let begin = pattern("gesture_begin_re");
    assert_eq!(
        begin_line(GestureScript::PanZoom2D),
        substituted(&begin, &["pan-zoom-2d"]),
        "the begin marker and the rig's bracket pattern have drifted",
    );
    // The floor: the substitution really can disagree.
    assert_ne!(
        begin_line(GestureScript::Orbit3D),
        substituted(&begin, &["pan-zoom-2d"]),
    );

    assert_eq!(
        loop_complete_line(GestureScript::UiSweep, 1203),
        substituted(&pattern("gesture_loop_re"), &["ui-sweep", "1203"]),
        "the loop marker and the rig's bracket pattern have drifted",
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

/// The camera state the pan-zoom script has handed the map at the end of a
/// replay, in the script's own vocabulary: the pan is the pointer
/// displacement delivered while the button was down, the zoom is the signed
/// wheel delta, and the two counts say the strokes and notches were really
/// emitted rather than skipped. `dragged` is the path length the pressed
/// pointer travelled — a "did the work happen" figure a stroke that never
/// pressed cannot fake, where the net pan can.
///
/// **This is the event stream, and it is not the camera.** Everything here is
/// a sum over the events the player wrote, so it reads a balanced stream as a
/// returning state — and the two are different things. Nothing between an
/// event and the map is modelled: not the drag threshold that hands
/// `Response::drag_delta()` nothing until the pointer has left a 6 pt circle,
/// not the release frame on which `dragged()` is already false and its step
/// goes to nobody, not `walkers`' inertia coast off `egui`'s smoothed pointer
/// velocity, not the zoom-to-cursor anchor, not the viewport zoom floor, not
/// the centre's latitude band. A run whose camera ended 5.5° of longitude
/// from where it started scores zero net pan here.
///
/// It is kept for what it *can* say — that the gesture was emitted at all —
/// and [`MapCamera`] is what says where the map went.
#[derive(Debug, Clone, PartialEq)]
struct StreamEnd {
    /// Summed in f64 so the harness contributes no error of its own: an f32
    /// running total over a few thousand deltas loses ~0.1 pt by itself.
    pan: (f64, f64),
    zoom_notches: f32,
    dragged: f64,
    presses: usize,
    wheel_events: usize,
}

fn stream_end(frames: &[(f64, Vec<egui::Event>)]) -> StreamEnd {
    let mut down = false;
    let mut pos: Option<egui::Pos2> = None;
    let mut end = StreamEnd {
        pan: (0.0, 0.0),
        zoom_notches: 0.0,
        dragged: 0.0,
        presses: 0,
        wheel_events: 0,
    };
    let apply = |end: &mut StreamEnd, to: egui::Pos2, down: bool, pos: &mut Option<egui::Pos2>| {
        if down && let Some(prev) = *pos {
            let (dx, dy) = (f64::from(to.x - prev.x), f64::from(to.y - prev.y));
            end.pan.0 += dx;
            end.pan.1 += dy;
            end.dragged += dx.hypot(dy);
        }
        *pos = Some(to);
    };
    for (_, events) in frames {
        for event in events {
            match event {
                egui::Event::PointerMoved(p) => apply(&mut end, *p, down, &mut pos),
                egui::Event::PointerButton {
                    pos: p, pressed, ..
                } => {
                    apply(&mut end, *p, down && !*pressed, &mut pos);
                    if *pressed {
                        end.presses += 1;
                    }
                    down = *pressed;
                }
                egui::Event::MouseWheel { delta, .. } => {
                    end.zoom_notches += delta.y;
                    end.wheel_events += 1;
                }
                _ => {}
            }
        }
    }
    end
}

/// Frames every `dt` seconds up to `until`.
fn steady_times(dt: f64, until: f64) -> Vec<f64> {
    let mut out = Vec::new();
    let mut t = 0.0;
    while t < until {
        t += dt;
        out.push(t);
    }
    out
}

/// A cadence that lands a frame just before and just after **every** boundary
/// the pan-zoom schedule has — each stroke's start and hold time, the drag's
/// end, and both zoom legs' ends — on top of a 30 fps fill. The half-line
/// branches are what a boundary frame probes: one that fired on a window
/// instead strands whatever the two frames straddled.
fn boundary_straddling_times(until: f64) -> Vec<f64> {
    use pan_zoom_2d::*;
    let nudge = 1e-4;
    let mut out = steady_times(1.0 / 30.0, until);
    let mut loop_start = 0.0;
    while loop_start < until {
        let mut marks = vec![
            DRAG_END,
            QUIET_1_END,
            ZOOM_IN_END,
            QUIET_2_END,
            ZOOM_OUT_END,
            LOOP_SECONDS,
        ];
        for stroke in 0..STROKES {
            marks.push(f64::from(stroke) * STROKE_PERIOD);
            marks.push(f64::from(stroke) * STROKE_PERIOD + STROKE_HOLD);
        }
        for mark in marks {
            out.push(loop_start + mark - nudge);
            out.push(loop_start + mark + nudge);
        }
        loop_start += LOOP_SECONDS;
    }
    out.sort_by(f64::total_cmp);
    out.retain(|t| *t > 0.0 && *t < until);
    out.dedup();
    out
}

/// A jittered cadence around `mean`, seeded — the shape of a real leg, and at
/// a 41 ms mean the one that put 1433-1457 frames on the Mac legs whose
/// six-segment totals spread 38.4 %.
fn jittered_around(seed: u64, mean: f64, until: f64) -> Vec<f64> {
    let mut state = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    let mut out = Vec::new();
    let mut t = 0.0;
    while t < until {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let u = ((state >> 33) as f64) / ((1u64 << 31) as f64);
        t += mean * (0.5 + u);
        out.push(t);
    }
    out
}

/// The script's end camera state is a function of elapsed time and nothing
/// else — the same map centre, the same zoom, and the same per-stroke map
/// displacements under every frame cadence.
///
/// **The camera here is a real `walkers::Map`** ([`MapCamera`]), built with
/// the same options `ui_map` builds the app's with. The version of this gate
/// that shipped with the release fix asserted a *reduction of the event
/// stream* instead — a sum of pointer deltas and wheel notches — and it
/// passed on a tree whose map, driven by the same events, ended anywhere
/// between Tennessee and Brazil across repeats of one build. A balanced
/// stream of events is not a returning camera, and only the map can tell the
/// two apart, because every mechanism that separates them lives between the
/// event and the map:
///
/// - `egui` hands a widget nothing (`Response::drag_delta() == ZERO`) until
///   the pointer has left a `max_click_dist` circle around the press point,
///   6 pt, and then only *this* frame's delta — so a ramp that starts at zero
///   loses `speed x t` of its first frames, a different amount per cadence;
/// - on the frame the release arrives `dragged()` is already false, so the
///   step from the last in-stroke frame to the release position reaches
///   nobody. That step is `reach(HOLD) - reach(t_of_last_frame)`: pure frame
///   luck, and it was worth up to 38 pt a stroke;
/// - `walkers` then coasts on `egui`'s smoothed `pointer.velocity()`
///   (`center.rs`, `velocity * INERTIA_TAU`), which is an estimate off the
///   last 100 ms of sampled positions and so is cadence-shaped too;
/// - the notches land over the *window's* centre, which is not the pane's, so
///   each is a zoom-to-cursor about an off-centre anchor;
/// - and the zoom has a floor and the centre a latitude band.
///
/// [`stream_end`] models none of that and cannot be made to without becoming
/// the widget. It is kept for the half it *can* speak to — that the gesture
/// was emitted at all — and asserted first, because a player that emitted
/// nothing would satisfy every "it landed in the same place" line below.
///
/// **What the player had to change to pass**: a stroke now comes to rest at
/// its reach for [`STROKE_HOLD - STROKE_TRAVEL`](pan_zoom_2d::STROKE_TRAVEL)
/// before the release, and starts one [`STROKE_LEAD`](pan_zoom_2d::STROKE_LEAD)
/// step clear of the press point. Both are saturations of the same closed
/// form in `t`, and between them the frame deltas telescope to the reach: the
/// first move crosses the drag threshold whole, and the release lands where
/// the pointer already is, with a velocity history 100 ms flat behind it.
///
/// **The floor, measured**: the map's displacement is exact from 10 fps up
/// and at every jittered cadence with a mean at or under 41 ms. Below that a
/// stroke does not get the four frames its sequence needs inside one 500 ms
/// period — release, park, press, move — and one is skipped outright, which
/// was true before this too.
///
/// The earlier mechanism this still pins: a stroke's release used to be
/// emitted only by a frame that landed inside its coast window,
/// `STROKE_PERIOD - STROKE_HOLD` = 50 ms wide. Any frame gap wider than that
/// stepped over the window, left the button down into the next stroke, and
/// that stroke's first move dragged the pointer back to the centre — so the
/// stranded stroke netted nothing, its mirrored pair stopped cancelling, and
/// the loop kept the pair's whole displacement as a net pan.
#[test]
fn the_pan_zoom_camera_lands_in_the_same_place_at_every_frame_cadence() {
    use pan_zoom_2d::*;
    // Three loops, cut in the third's quiet tail: past ZOOM_OUT_END, so all
    // three drag phases and all three zoom cycles are complete.
    const LOOPS: usize = 3;
    let until = LOOPS as f64 * LOOP_SECONDS - 0.5;

    // What the schedule alone says the run must deliver.
    let reach_cap = REACH_FRACTION * NATIVE.width().min(NATIVE.height());
    let stroke_reach = |stroke: u32| {
        let speed = STROKE_SPEED_BASE + STROKE_SPEED_STEP * (stroke / 2) as f32;
        f64::from((STROKE_LEAD + speed * STROKE_TRAVEL as f32).min(reach_cap))
    };
    // Each stroke presses at the centre and travels outward only, so its
    // pressed path length is exactly the reach its hold time buys.
    let expected_dragged = LOOPS as f64 * (0..STROKES).map(stroke_reach).sum::<f64>();
    let expected_presses = LOOPS * STROKES as usize;
    let expected_wheel_events = LOOPS * 2 * NOTCHES_PER_LEG as usize;

    // A tolerance of the f32 direction vector the player scales by: its
    // length is 1 only to f32 precision, so a stroke's delivered reach is its
    // scheduled reach times 1 +/- ~1e-7, and the run's pressed travel carries
    // a few thousandths of a point of that. Nothing else is approximate — the
    // sums above are f64. It is three orders of magnitude under the smallest
    // thing a stranded stroke could be worth, pair 0's 57 pt reach, so it
    // cannot swallow one.
    let tolerance = 1e-2;

    let cadences: Vec<(String, Vec<f64>)> = vec![
        ("60 fps".to_owned(), steady_times(1.0 / 60.0, until)),
        // Wider than the 50 ms coast window, so a release that needs a frame
        // inside it is stranded.
        ("16.7 fps".to_owned(), steady_times(0.06, until)),
        ("10 fps".to_owned(), steady_times(0.1, until)),
        (
            "60 fps, first frames 40 ms late".to_owned(),
            late_first_frames(STROKE_PERIOD, until),
        ),
        (
            "30 fps straddling every boundary".to_owned(),
            boundary_straddling_times(until),
        ),
        (
            "jittered ~41 ms".to_owned(),
            jittered_around(7, 0.041, until),
        ),
        (
            "jittered ~25 ms".to_owned(),
            jittered_around(11, 0.025, until),
        ),
        // Above 137 fps, and the reason the list reaches up here rather than
        // only down: the first frame after a press is what has to clear
        // `egui`'s 6 pt drag threshold in one step, and the faster the frames
        // come the less of the ramp there is in it. Pair 0 runs at 200 pt/s,
        // so a 5.7 ms frame carries 1.1 pt of it — a fifth of the bar. A list
        // that stopped at 60 fps would let `STROKE_LEAD` go to zero and stay
        // green.
        ("175 fps".to_owned(), steady_times(1.0 / 175.0, until)),
    ];

    // Every scripted stroke's own displacement, from the schedule alone.
    let scripted_stroke =
        |stroke: u32| GesturePlayer::stroke_pos(NATIVE, stroke, STROKE_HOLD) - NATIVE.center();

    // What the map's zoom may differ by between two cadences, in zoom levels.
    //
    // Not zero, and not a fudge: `egui` hands a discrete wheel notch out over
    // several frames (`wheel_state.rs` drains `unprocessed_wheel_delta` by an
    // `exponential_smooth_factor` of the frame time), and `walkers` ignores
    // any frame whose zoom delta is inside its own 0.001 deadband
    // (`map.rs::handle_gestures`). The drained tail below that deadband is
    // dropped, and how much lands there is a function of where the frames
    // fell. The bound is one deadband's worth of a notch, `0.001 * zoom_speed`
    // — `zoom_speed` is 2 — which is the most a single frame can silently
    // lose. The pan below is asserted a hundred times tighter, because
    // nothing in the drag path has an equivalent.
    let zoom_tolerance = 0.002;

    let mut reference: Option<(&str, CameraEnd)> = None;
    for (name, times) in &cadences {
        let frames = replay("pan-zoom-2d", times, NATIVE);
        let stream = stream_end(&frames);
        let camera = camera_end(&frames);

        // The gesture really did its work: every stroke pressed, every notch
        // emitted, and the pressed pointer travelled the whole scripted path.
        // A player that emitted nothing would satisfy every "it landed in the
        // same place" assertion below, so these come first.
        assert_eq!(
            stream.presses, expected_presses,
            "{name}: {} strokes pressed, the schedule has {expected_presses}",
            stream.presses
        );
        assert_eq!(
            stream.wheel_events, expected_wheel_events,
            "{name}: {} wheel notches, the schedule has {expected_wheel_events}",
            stream.wheel_events
        );
        assert!(
            (stream.dragged - expected_dragged).abs() < tolerance,
            "{name}: the pressed pointer travelled {} pt, the schedule's strokes total \
             {expected_dragged} pt",
            stream.dragged
        );
        assert_eq!(
            stream.zoom_notches, 0.0,
            "{name}: the stream left a net zoom"
        );

        // And the map took every stroke whole. This is the assertion the
        // stream cannot make: what reaches the camera is the drag `egui`
        // decided on, minus the release frame's own step, plus whatever
        // inertia `walkers` coasts on afterwards.
        assert_eq!(
            camera.strokes.len(),
            expected_presses,
            "{name}: the camera saw {} drag strokes, the schedule has {expected_presses}",
            camera.strokes.len()
        );
        for (i, applied) in camera.strokes.iter().enumerate() {
            let want = scripted_stroke(i as u32 % STROKES);
            assert!(
                f64::from((*applied - want).length()) < tolerance,
                "{name}: stroke {i} moved the map {applied:?} pt, the schedule says {want:?}"
            );
        }

        // And it landed back where it started: mirrored pairs, equal legs.
        assert!(
            camera.pan.0.hypot(camera.pan.1) < tolerance,
            "{name}: the run left the map centre {:?} pt from where it started",
            camera.pan
        );
        assert!(
            (camera.zoom - START_ZOOM).abs() < zoom_tolerance,
            "{name}: the run left the map at zoom {}, not {START_ZOOM}",
            camera.zoom
        );

        match &reference {
            None => reference = Some((name, camera)),
            Some((ref_name, ref_camera)) => {
                assert!(
                    (camera.pan.0 - ref_camera.pan.0).hypot(camera.pan.1 - ref_camera.pan.1)
                        < tolerance
                        && (camera.zoom - ref_camera.zoom).abs() < zoom_tolerance,
                    "the camera ends at pan {:?} zoom {} under {name} but at pan {:?} zoom {} \
                     under {ref_name}",
                    camera.pan,
                    camera.zoom,
                    ref_camera.pan,
                    ref_camera.zoom
                );
                for (i, (a, b)) in camera.strokes.iter().zip(&ref_camera.strokes).enumerate() {
                    assert!(
                        f64::from((*a - *b).length()) < tolerance,
                        "{name}: stroke {i} moved the map {a:?}, {ref_name} moved it {b:?}"
                    );
                }
            }
        }
    }
}

/// The map pane inside [`NATIVE`], inset by the shipped top bar.
///
/// Inset on purpose rather than filling the window: the script's wheel notches
/// go out over the **window's** centre, which is not the pane's, so every notch
/// is a zoom-to-cursor about an off-centre anchor — the same asymmetry the app
/// has and a whole-screen fixture would not.
const PANE: egui::Rect = egui::Rect {
    min: egui::pos2(0.0, 40.0),
    max: egui::pos2(1920.0, 1080.0),
};

/// The zoom the fixture starts at, and the one displacements are reported in.
const START_ZOOM: f64 = 4.0;

/// A real `walkers` map camera driven by the player's events — the app's own
/// widget, with the app's own options, over a headless [`egui::Context`].
///
/// The point of driving the widget rather than reducing the stream is that
/// everything the divergence lived in is *between* the two: `egui`'s drag
/// decision and per-frame `delta()`, the release frame's dropped step,
/// `walkers`' inertia coast, the zoom-to-cursor anchor and the viewport
/// clamps. A fixture that models the map itself has to be right about all of
/// them; this one is the map.
struct MapCamera {
    ctx: egui::Context,
    memory: walkers::MapMemory,
}

impl MapCamera {
    fn new() -> Self {
        let mut memory = walkers::MapMemory::default();
        memory
            .set_zoom(START_ZOOM)
            .expect("the start zoom is in walkers' range");
        memory.center_at(walkers::lon_lat(-97.28, 35.33));
        Self {
            ctx: egui::Context::default(),
            memory,
        }
    }

    /// One frame: the events the player wrote for it, through the same
    /// wheel-unit rewrite `EguiRenderer::begin_frame` applies, into the same
    /// `Map` builder `ui_map` uses.
    fn frame(&mut self, time: f64, events: Vec<egui::Event>) {
        let mut raw_input = egui::RawInput {
            screen_rect: Some(NATIVE),
            time: Some(time),
            events,
            ..Default::default()
        };
        crate::ui_input::normalize_wheel_units(&mut raw_input, 1.0);
        self.ctx.begin_pass(raw_input);
        let mut ui = egui::Ui::new(
            self.ctx.clone(),
            egui::Id::new("gesture player camera"),
            egui::UiBuilder::new()
                .layer_id(egui::LayerId::background())
                .max_rect(PANE),
        );
        ui.set_clip_rect(PANE);
        walkers::Map::new(None, &mut self.memory, walkers::lon_lat(-97.28, 35.33))
            .zoom_with_ctrl(false)
            .panning(false)
            .wheel_zoom_scales_with_frame_time(false)
            .drag_pan_buttons(egui::DragPanButtons::PRIMARY)
            .show(&mut ui, |_, _, _, _| ());
        let _ = self.ctx.end_pass();
    }

    fn zoom(&self) -> f64 {
        self.memory.zoom()
    }

    /// Where the map centre is, in Web Mercator points at `zoom`.
    fn centre_at(&self, zoom: f64) -> (f64, f64) {
        let p = self
            .memory
            .detached()
            .expect("the fixture centres the map itself, so it is always detached");
        let world = 256.0 * 2f64.powf(zoom);
        let lat = p.y().to_radians();
        (
            (p.x() + 180.0) / 360.0 * world,
            (1.0 - ((lat.tan() + 1.0 / lat.cos()).ln()) / std::f64::consts::PI) / 2.0 * world,
        )
    }

    /// Where the map centre is, in points at the map's **own** zoom.
    ///
    /// The live zoom is the right unit for a *drag*: the pointer's points are
    /// the map's points at the zoom the drag happened at, and a drag phase
    /// runs at one zoom throughout. It is the wrong unit for comparing two
    /// readings taken at different zooms, which is why the run's own start and
    /// end are read through [`Self::centre_at`] at one fixed zoom instead: a
    /// world coordinate is ~1618 pt from the origin here, and reading it in a
    /// world 1.8e-4 of a level wider moves it 0.29 pt without the map having
    /// gone anywhere.
    fn centre_points(&self) -> (f64, f64) {
        self.centre_at(self.memory.zoom())
    }
}

/// What one cadence did to a real map: where the centre finished relative to
/// where it started, what the zoom finished at, and the displacement the map
/// took from each drag stroke.
#[derive(Debug, Clone)]
struct CameraEnd {
    /// Centre displacement over the whole run, in points at [`START_ZOOM`].
    pan: (f64, f64),
    zoom: f64,
    /// Per stroke, the pointer travel the map actually applied — the centre's
    /// own move, negated, since the map goes the other way.
    strokes: Vec<egui::Vec2>,
}

/// Drive `frames` — the player's own output — through a real map camera.
///
/// The per-stroke list is read at the schedule's own stroke boundaries: the
/// centre's move over `(k·STROKE_PERIOD, (k+1)·STROKE_PERIOD]`, kept only for
/// the `k` that are drag strokes. The boundary reading is taken on every
/// boundary either way, so the quiet and zoom phases are absorbed into a
/// reading nobody keeps rather than into the next stroke's.
fn camera_end(frames: &[(f64, Vec<egui::Event>)]) -> CameraEnd {
    // Stroke slots in one loop — the drag strokes and then the quiet and
    // zoom phases, all of it cut on the same period.
    let slots = (LOOP_SECONDS / pan_zoom_2d::STROKE_PERIOD) as u32;
    let mut cam = MapCamera::new();
    let start = cam.centre_at(START_ZOOM);
    let mut at_boundary = cam.centre_points();
    let mut slot = 0u32;
    let mut strokes = Vec::new();
    for (time, events) in frames {
        cam.frame(*time, events.clone());
        let seen = (time / pan_zoom_2d::STROKE_PERIOD) as u32;
        if seen != slot {
            let now = cam.centre_points();
            // The slot that just closed. Only the drag strokes are kept; the
            // map goes the other way from the pointer, hence the negation.
            if slot % slots < pan_zoom_2d::STROKES {
                strokes.push(egui::vec2(
                    -(now.0 - at_boundary.0) as f32,
                    -(now.1 - at_boundary.1) as f32,
                ));
            }
            at_boundary = now;
            slot = seen;
        }
    }
    let end = cam.centre_at(START_ZOOM);
    CameraEnd {
        pan: (end.0 - start.0, end.1 - start.1),
        zoom: cam.zoom(),
        strokes,
    }
}
