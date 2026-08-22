//! **A layer's own loop window** (WB-6): the Lookback slider is one number for
//! the whole application, and one number cannot be a loop at two cadences.
//!
//! End-to-end and not hand-armed. Every window below is read off the
//! `GuiAction::EnableLoop` the ∞ button really emitted, through the same
//! `render_timeline` a user clicks — not off `Gui::loop_span_secs_for`, which
//! is the thing under test and would agree with itself. The distinction is the
//! whole reason this file exists: an earlier item in this campaign computed one
//! window and dispatched another, and only a test driving the real action
//! caught it.

use super::InputHarness;
use crate::Gui;
use crate::actions::GuiAction;
use rustdar_device_profile::constants::MIN_LOOP_FRAMES_PER_PANE;
use rustdar_source::id::{LayerId, known};

/// The hourly layer. A **real, registered** layer rather than a fixture: its
/// `time_axis` declares a 3600 s step in the shipping build, so what is
/// asserted below is what a user gets.
const HOURLY: LayerId = known::MODEL_DATA;

/// **The layer the requirement actually named** — satellite imagery, hourly,
/// looping. `TimeAxis::FrameSeries` with the whole frame contract behind it
/// since WB-11; before that it was `Live` and could not be looped over any
/// window at all.
const SATELLITE: LayerId = known::GMGSI;

/// The Lookback slider's own default, in seconds — `PaneTimePosture::default`
/// and the config file's default are the same number.
const SLIDER_DEFAULT: u64 = 3600;

/// Draw `id` on pane 0, or stop drawing it, through the same door a ticked
/// layer row goes through.
fn draw(h: &mut InputHarness, id: &LayerId, on: bool) {
    let gui = h.gui_mut();
    let mut pane = std::mem::take(gui.pane_mut(0).expect("pane 0"));
    Gui::write_pane_overlay(&mut gui.overlays, 0, &mut pane, id, on);
    *gui.pane_mut(0).expect("pane 0") = pane;
    h.warm_up();
}

/// Click the ∞ button and answer with the window the emitted action asked for.
///
/// `expect` rather than an `Option`: a click that emitted nothing is a broken
/// fixture, and returning `None` would let a caller's `assert_ne!` pass on two
/// absences.
fn enable_loop_window(h: &mut InputHarness) -> u64 {
    h.warm_up();
    h.mouse_click(h.timeline().loop_toggle.0.center());
    h.last_actions()
        .iter()
        .find_map(|a| match a {
            GuiAction::EnableLoop {
                pane_idx: 0,
                lookback_secs,
            } => Some(*lookback_secs),
            _ => None,
        })
        .expect("the ∞ button must emit EnableLoop for pane 0")
}

/// Frames a window of `window` seconds holds at a `step`-second cadence — the
/// arithmetic `frames_for_span` does, spelled out here because what is under
/// test is the *window*, and the count is what a window is worth.
fn frames(window: u64, step: u64) -> usize {
    (window / step + 1) as usize
}

/// **The pin of WB-6.** Two layers, one slider, two windows.
///
/// Radar's window is the slider's number to the second — the control, and the
/// reason "widen everything" cannot pass. The hourly layer's is its own, wide
/// enough to be a loop rather than a before-and-after. The slider still
/// governs above both.
///
/// Floors, each run and each observed red:
/// - point both `EnableLoop` sites back at `panes[i].time.span_secs` → the
///   hourly window collapses to 3600 s and its frame count to 2.
/// - `Model::min_loop_frames` → `0` → same collapse, from the layer's side.
/// - `loop_span_secs_for` → `span.max(43_200)` for every layer ("widen
///   everything") → the radar control fails.
#[test]
fn an_hourly_layer_loops_over_its_own_window_and_radar_over_the_sliders() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.load_scan("KTLX");

    // ── Radar, on the slider's own number ────────────────────────────────
    assert_eq!(
        h.gui_mut().pane(0).expect("pane 0").transport_layer(),
        &known::RADAR,
        "precondition: a radar pane's transport addresses radar"
    );
    let radar_window = enable_loop_window(&mut h);
    assert_eq!(
        radar_window, SLIDER_DEFAULT,
        "radar's loop window must be the slider's number to the second — a \
         floor that reached radar would be this item widening everything"
    );

    // ── The hourly layer, on its own ─────────────────────────────────────
    draw(&mut h, &HOURLY, true);
    draw(&mut h, &known::RADAR, false);
    assert_eq!(
        h.gui_mut().pane(0).expect("pane 0").transport_layer(),
        &HOURLY,
        "precondition: with radar off the transport addresses the hourly \
         layer, so the window below is that layer's"
    );
    let hourly_window = enable_loop_window(&mut h);

    // Non-triviality: one global number cannot be both of these.
    assert_ne!(
        hourly_window, radar_window,
        "the two layers were listed over the SAME window — a single global \
         span is still deciding, whatever the floor computes"
    );

    // The property, in the unit that matters: frames.
    let hourly_frames = frames(hourly_window, 3600);
    assert!(
        hourly_frames >= MIN_LOOP_FRAMES_PER_PANE * 4,
        "an hourly layer looped over {hourly_window} s is {hourly_frames} \
         frames; below {} it is a before-and-after, not a loop",
        MIN_LOOP_FRAMES_PER_PANE * 4
    );
    assert_eq!(
        frames(radar_window, 300),
        13,
        "and radar's own count over the unchanged slider window"
    );

    // ── Nothing new persists: the setting is still the slider's ──────────
    assert_eq!(
        h.gui_mut().loop_lookback_secs,
        SLIDER_DEFAULT,
        "the floor must not write itself back into the persisted setting — \
         reopen shows the slider where the user left it"
    );

    // ── The slider still governs above the floor ─────────────────────────
    h.gui_mut().set_loop_span_secs(86_400);
    let widened = enable_loop_window(&mut h);
    assert_eq!(
        widened, 86_400,
        "a floor must be a floor: dragging Lookback past it has to widen the \
         hourly layer too, not clamp it back to {hourly_window}"
    );
}

/// **The caption moved with the scope.** `TUNING_SCOPE_CAPTION` promised the
/// sliders were the whole answer; on a pane whose layer has its own floor they
/// are not, and the row says what the answer is instead.
///
/// The quantity, never an apology — the reader cannot act on "approximate",
/// and can act on "12 h, 13 frames".
///
/// Floor: return `base` unconditionally from `tuning_scope_caption` → the
/// hourly assertions fail while the radar one still passes.
#[test]
fn the_tuning_caption_names_the_window_a_layer_with_its_own_floor_gets() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.load_scan("KTLX");
    h.mouse_click(h.timeline().expander.center());
    h.warm_up();

    let radar_caption = h
        .timeline()
        .row2
        .expect("the expander must open row 2")
        .tuning_scope;
    assert!(
        radar_caption.contains("every pane"),
        "the sliders still name their reach; drew {radar_caption:?}"
    );

    draw(&mut h, &HOURLY, true);
    draw(&mut h, &known::RADAR, false);
    let row2 = h
        .timeline()
        .row2
        .expect("row 2 is still open on the hourly pane");
    assert_ne!(
        row2.tuning_scope, radar_caption,
        "the caption is the same on both panes — it still describes a scope \
         the sliders no longer have"
    );
    for figure in ["12 h", "13 frames", "1 h apart"] {
        assert!(
            row2.tuning_scope.contains(figure),
            "the caption must show the quantity {figure:?}; drew {:?}",
            row2.tuning_scope
        );
    }
    assert!(
        h.text_painted_in(h.screen_rect(), &row2.tuning_scope),
        "the caption is a probe string that never reached the glass"
    );
}

/// **The span floor is what makes the satellite layer a loop at all** (WB-11).
///
/// GMGSI publishes one blended mosaic an hour, so the Lookback slider's own
/// default of 3600 s buys **two** frames on it — a before-and-after. Its
/// `min_loop_frames` of 13 is what turns that into a twelve-hour, thirteen-
/// frame window, and this reads that window off the `EnableLoop` the infinity
/// button really emitted rather than off `loop_span_secs_for`, which is the
/// thing under test.
///
/// Radar in the same harness is the non-triviality control: its window is the
/// slider's number to the second, so what is asserted is a floor and not a
/// global widening.
///
/// **Floor — `no_satellite_floor`:** `Gmgsi::min_loop_frames` -> 0. Observed
/// red: the window came back 3600 s — the slider's number, two hourly frames —
/// instead of 43,200 s, so one global span was deciding both layers again.
#[test]
fn the_satellite_layer_loops_over_twelve_hours_and_radar_over_the_sliders() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.load_scan("KTLX");

    let radar_window = enable_loop_window(&mut h);
    assert_eq!(
        radar_window, SLIDER_DEFAULT,
        "control: radar is unaffected by the satellite layer's floor"
    );

    draw(&mut h, &SATELLITE, true);
    draw(&mut h, &known::RADAR, false);
    assert_eq!(
        h.gui_mut().pane(0).expect("pane 0").transport_layer(),
        &SATELLITE,
        "precondition: GMGSI is a FrameSeries layer, so with radar off it is \
         what the pane's transport addresses. Before WB-11 it was `Live` and \
         this pane had no transport at all"
    );

    let satellite_window = enable_loop_window(&mut h);
    assert_eq!(
        satellite_window, 43_200,
        "thirteen hourly mosaics is twelve hours end to end"
    );
    assert_eq!(
        frames(satellite_window, 3600),
        13,
        "the window in the unit that matters: {} s at an hourly cadence",
        satellite_window,
    );
    assert_eq!(
        frames(SLIDER_DEFAULT, 3600),
        2,
        "and what the slider alone would have bought, which is not a loop"
    );
    assert_ne!(
        satellite_window, radar_window,
        "both layers were listed over the SAME window, so one global span is \
         still deciding whatever the floor computes"
    );

    // The slider still governs above the floor, and nothing new persists.
    h.gui_mut().set_loop_span_secs(86_400);
    assert_eq!(
        enable_loop_window(&mut h),
        86_400,
        "dragging Lookback past the floor must widen satellite too"
    );
}

/// **The national mosaic needs no floor, and gets none** (WB-10).
///
/// The inverse of the satellite case, asserted rather than assumed: at MRMS's
/// 120 s cadence the Lookback slider's default hour is already 31 frames — a
/// real loop — so `Mrms::min_loop_frames` stays the trait's 0 and the window
/// the ∞ button emits is the slider's number **to the second**. A floor wide
/// enough to bind here (anything past 31 frames) would widen every MRMS
/// pane's rail for nothing; one narrower is dead code.
///
/// Radar's window in the same harness doubles as the control that nothing
/// global widened, and the tuning caption stays the base string — no floor
/// binds, so there is no quantity to show.
///
/// **Floor — `a_floor_that_binds`:** `Mrms::min_loop_frames` -> 361 (twelve
/// hours of 120 s frames). Red: the emitted window becomes 43,200 s, not the
/// slider's 3600.
#[test]
fn the_mosaic_loops_over_the_sliders_own_window_with_no_floor() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.load_scan("KTLX");

    let radar_window = enable_loop_window(&mut h);
    assert_eq!(radar_window, SLIDER_DEFAULT, "control: radar on the slider");

    draw(&mut h, &known::MRMS, true);
    draw(&mut h, &known::RADAR, false);
    assert_eq!(
        h.gui_mut().pane(0).expect("pane 0").transport_layer(),
        &known::MRMS,
        "precondition: MRMS is a FrameSeries layer since WB-10, so with \
         radar off it is what the pane's transport addresses"
    );

    let mosaic_window = enable_loop_window(&mut h);
    assert_eq!(
        mosaic_window, SLIDER_DEFAULT,
        "MRMS declares no min_loop_frames, so the slider's number reaches \
         the loop to the second"
    );
    assert_eq!(
        frames(mosaic_window, 120),
        31,
        "and the slider's default hour is already {} frames at the 2-minute \
         cadence — a real loop with no floor's help, which is why declaring \
         one would only ever widen the rail",
        frames(mosaic_window, 120),
    );
    assert!(frames(mosaic_window, 120) >= MIN_LOOP_FRAMES_PER_PANE * 4);
}

/// **The two hourly layers do not collide.** GMGSI's weight is 5 and the
/// model's is 10, so a pane drawing both keeps the model as its transport —
/// which is the whole of the WB-11 draw-order ruling, read at the pane rather
/// than at the registry.
///
/// **Floor — `satellite_on_top`:** `Gmgsi::draw_order_weight` -> 20. Observed
/// red: the transport moved to GMGSI on a pane that draws the model, which is
/// the "must be ruled on, not absorbed" case.
#[test]
fn a_pane_drawing_both_hourly_layers_keeps_the_model_as_its_transport() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.load_scan("KTLX");
    draw(&mut h, &SATELLITE, true);
    draw(&mut h, &HOURLY, true);
    draw(&mut h, &known::RADAR, false);

    assert_eq!(
        h.gui_mut().pane(0).expect("pane 0").transport_layer(),
        &HOURLY,
        "GMGSI is the lowest-weight layer registered, so it takes a pane's \
         clock from nothing"
    );
    // And radar still outranks both.
    draw(&mut h, &known::RADAR, true);
    assert_eq!(
        h.gui_mut().pane(0).expect("pane 0").transport_layer(),
        &known::RADAR,
    );
}
