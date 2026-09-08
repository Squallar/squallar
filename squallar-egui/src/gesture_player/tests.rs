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
#[derive(Debug, Clone, PartialEq)]
struct CameraEnd {
    /// Summed in f64 so the harness contributes no error of its own: an f32
    /// running total over a few thousand deltas loses ~0.1 pt by itself.
    pan: (f64, f64),
    zoom_notches: f32,
    dragged: f64,
    presses: usize,
    wheel_events: usize,
}

fn camera_end(frames: &[(f64, Vec<egui::Event>)]) -> CameraEnd {
    let mut down = false;
    let mut pos: Option<egui::Pos2> = None;
    let mut end = CameraEnd {
        pan: (0.0, 0.0),
        zoom_notches: 0.0,
        dragged: 0.0,
        presses: 0,
        wheel_events: 0,
    };
    let apply = |end: &mut CameraEnd, to: egui::Pos2, down: bool, pos: &mut Option<egui::Pos2>| {
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
/// else — the same pan, the same zoom, and the same per-stroke displacements
/// under every frame cadence.
///
/// The mechanism this pins: a stroke's release used to be emitted only by a
/// frame that landed inside its coast window, `STROKE_PERIOD - STROKE_HOLD`
/// = 50 ms wide. Any frame gap wider than that stepped over the window, left
/// the button down into the next stroke, and that stroke's first move dragged
/// the pointer back to the centre — so the stranded stroke netted nothing,
/// its mirrored pair stopped cancelling, and the loop kept the pair's whole
/// displacement (up to `REACH_FRACTION` of the shorter edge, 432 pt at
/// 1920x1080) as a net pan. Which strokes were stranded was pure frame-gap
/// luck: five Mac Firefox legs of the same build, script and seed ended
/// between 36.41 N / 430 km and 79.89 N / 4,973 km.
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
        f64::from((speed * STROKE_HOLD as f32).min(reach_cap))
    };
    // Each stroke presses at the centre and travels outward only, so its
    // pressed path length is exactly the reach its hold time buys.
    let expected_dragged = LOOPS as f64 * (0..STROKES).map(stroke_reach).sum::<f64>();
    let expected_presses = LOOPS * STROKES as usize;
    let expected_wheel_events = LOOPS * 2 * NOTCHES_PER_LEG as usize;

    // A tolerance of the f32 direction vector the player scales by: its
    // length is 1 only to f32 precision, so a stroke's delivered reach is its
    // scheduled reach times 1 +/- ~1e-7, and the run's 16,173 pt of pressed
    // travel carries a few thousandths of a point of that. Nothing else is
    // approximate — the sums above are f64. It is four orders of magnitude
    // under the smallest thing a stranded stroke could be worth, pair 0's
    // 90 pt reach, so it cannot swallow one.
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
    ];

    let mut reference: Option<(&str, CameraEnd, Vec<egui::Vec2>)> = None;
    for (name, times) in &cadences {
        let frames = replay("pan-zoom-2d", times, NATIVE);
        let end = camera_end(&frames);
        let nets: Vec<egui::Vec2> = strokes(&frames).iter().map(|s| s.net).collect();

        // The gesture really did its work: every stroke pressed, every notch
        // emitted, and the pressed pointer travelled the whole scripted path.
        assert_eq!(
            end.presses, expected_presses,
            "{name}: {} strokes pressed, the schedule has {expected_presses}",
            end.presses
        );
        assert_eq!(
            end.wheel_events, expected_wheel_events,
            "{name}: {} wheel notches, the schedule has {expected_wheel_events}",
            end.wheel_events
        );
        assert!(
            (end.dragged - expected_dragged).abs() < tolerance,
            "{name}: the pressed pointer travelled {} pt, the schedule's strokes total \
             {expected_dragged} pt",
            end.dragged
        );
        // And it landed back where it started: mirrored pairs, equal legs.
        assert!(
            end.pan.0.hypot(end.pan.1) < tolerance,
            "{name}: the run left a net pan of {:?} pt",
            end.pan
        );
        assert_eq!(end.zoom_notches, 0.0, "{name}: the run left a net zoom");

        match &reference {
            None => reference = Some((name, end, nets)),
            Some((ref_name, ref_end, ref_nets)) => {
                assert!(
                    (end.pan.0 - ref_end.pan.0).hypot(end.pan.1 - ref_end.pan.1) < tolerance
                        && end.zoom_notches == ref_end.zoom_notches
                        && (end.dragged - ref_end.dragged).abs() < tolerance
                        && end.presses == ref_end.presses
                        && end.wheel_events == ref_end.wheel_events,
                    "the camera ends at {end:?} under {name} but at {ref_end:?} under {ref_name}"
                );
                assert_eq!(
                    nets.len(),
                    ref_nets.len(),
                    "{name}: {} stroke displacements against {ref_name}'s {}",
                    nets.len(),
                    ref_nets.len()
                );
                for (i, (a, b)) in nets.iter().zip(ref_nets).enumerate() {
                    assert!(
                        f64::from((*a - *b).length()) < tolerance,
                        "{name}: stroke {i} displaced {a:?}, {ref_name} displaced {b:?}"
                    );
                }
            }
        }
    }
}
