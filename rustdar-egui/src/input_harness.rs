//! Headless input harness for [`Gui::ui`].
//!
//! Drives the real UI through a real [`egui::Context`] with hand-constructed
//! [`egui::RawInput`] — no window, no winit, no wgpu. Each [`InputHarness::frame`]
//! runs one full egui pass (`Gui::ui`, all panels, dialogs and map panes), and
//! `render_map` records the pointer state it resolved for each pane on the way
//! through. [`FrameOutcome::resolved`], [`FrameOutcome::resolved_inactive`],
//! [`FrameOutcome::modality`] and [`FrameOutcome::resolved_zoom`] are reads of
//! *that* — the shipped decision, not a second one taken here.
//!
//! # Do not resolve anything a second time
//!
//! This harness used to drive its own `ModalityLatch` and `InteractionState`
//! beside `Gui::ui` and assert on those. Nothing compared the two, so the
//! pointer suite validated a replica and every pointer decision in `ui_map.rs`
//! could be broken with it green. Anything claiming to be what the app does
//! must be read back out of [`Gui`].
//!
//! [`FrameOutcome::mouse`] and [`FrameOutcome::touch`] are the exceptions: they
//! drive each pipeline directly to say what it *would* have done. They are
//! ungated and no test may read them as the app's behaviour.
//!
//! # Event fidelity
//!
//! The pointer helpers emit exactly the event sequences the real integrations
//! produce, which is what makes the cancellation tests meaningful. They do not
//! agree with each other, and the disagreements are the whole reason the
//! tracker is shaped the way it is.
//!
//! `egui-winit` 0.35.0 (`src/lib.rs`) — `on_touch`'s body is byte-identical to
//! 0.34.1's, so every row below survived the bump unchanged:
//!
//! | winit event                | emitted here                                          |
//! |----------------------------|-------------------------------------------------------|
//! | `TouchPhase::Started`      | `Touch{Start}`, `PointerMoved`, `PointerButton{down}` |
//! | `TouchPhase::Moved`        | `Touch{Move}`, `PointerMoved`                         |
//! | `TouchPhase::Ended`        | `Touch{End}`, `PointerButton{up}`, `PointerGone`      |
//! | `TouchPhase::Cancelled`    | `Touch{Cancel}`, `PointerGone` — **no release**       |
//! | `WindowEvent::CursorLeft`  | `PointerGone` alone — and the position is forgotten,  |
//! |                            | so a release out there is dropped (`lib.rs:784`)      |
//!
//! eframe 0.35.0's web canvas (`src/web/events.rs`) — the four touch handlers
//! are likewise byte-identical to 0.34.1's:
//!
//! | DOM event     | emitted here                                                |
//! |---------------|-------------------------------------------------------------|
//! | `touchstart`  | `PointerButton{down}` **then** `Touch{Start}` — order flipped |
//! | `touchmove`   | `PointerMoved`, `Touch{Move}`                               |
//! | `touchend`    | `PointerButton{up}`, `PointerGone`, `Touch{End}`            |
//! | `touchcancel` | `Touch{Cancel}` **alone** — no release, no `PointerGone`    |
//! | `mousemove`   | `PointerMoved`                                              |
//!
//! Two rows carry the weight. A cancelled touch never reports a release and
//! egui does not clear `pointer.down` on `PointerGone`, so any gesture that
//! only exits on "pointer up" stays stuck forever — and on the web there is no
//! `PointerGone` either, so a tracker keying on that alone never notices the
//! cancellation at all.

use crate::Gui;
use crate::ui::DrawnMenuLeaf;
use crate::ui_input::{MapPointerFrame, TouchGestures};
use crate::ui_layout::PointerModality;
use rustdar_overlays::render::overlay_state::OverlayKind;

/// Viewport size used by the harness — a landscape desktop-ish window.
const SCREEN_SIZE: egui::Vec2 = egui::vec2(1024.0, 768.0);

/// Nominal seconds between harness frames (only used by [`InputHarness::frame`]).
const FRAME_DT: f64 = 1.0 / 60.0;

/// The pane pointer state produced by one harness frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct FrameOutcome {
    /// Pointer resolution from the mouse path, driven unconditionally.
    ///
    /// This and `touch` bypass the modality gate on purpose — they exercise
    /// each pipeline directly, whatever is actually pointing at the screen.
    /// For what the app really does with this frame's input, use `resolved`.
    pub mouse: MapPointerFrame,
    /// Pointer resolution from the touch pipeline, driven unconditionally.
    pub touch: MapPointerFrame,
    /// What the shipped `render_map` resolved for the active pane, read back
    /// out of `Gui`. See the module note.
    pub resolved: MapPointerFrame,
    /// The same for a non-active pane. `None` in a one-pane layout, where
    /// there is no inactive pane to observe.
    pub resolved_inactive: Option<MapPointerFrame>,
    /// The modality `render_map` ran this frame under.
    pub modality: PointerModality,
    /// Map zoom after the frame, on the ungated `touch` path.
    pub zoom: f64,
    /// The active pane's real map zoom, so a test can tell whether a gesture
    /// the gate should have blocked moved the actual map.
    pub resolved_zoom: f64,
}

/// Drives [`Gui::ui`] frame by frame with synthetic input.
pub(crate) struct InputHarness {
    ctx: egui::Context,
    gui: Gui,
    /// Touch gesture detectors driving the **ungated** `touch` probe, so one
    /// frame can be observed through that pipeline whatever the real UI chose.
    /// The gated answer is read out of `Gui`, never resolved here.
    gestures: TouchGestures,
    /// Map viewport the ungated zoom gesture acts on.
    map_memory: walkers::MapMemory,
    /// Screen rect handed to the **ungated** touch probe, and the position
    /// [`InputHarness::map_center`] reports. Roughly where the one-pane map
    /// lands; the gated path uses the layout's real pane rect, so a test that
    /// splits the panes must take its positions from
    /// [`InputHarness::pane_rects`] instead.
    pane_rect: egui::Rect,
    /// Wall-clock time reported to egui, in seconds.
    time: f64,
    /// Events queued for the next frame.
    events: Vec<egui::Event>,
    screen_rect: egui::Rect,
    /// Every rect painted during the last frame, in paint order. Lets a test
    /// assert on what was actually *drawn* rather than on an intermediate value
    /// — the only way to pin that a resolved decision reached the renderer.
    last_rects: Vec<egui::Rect>,
    /// `RawInput::max_texture_side` — what `egui_winit` is handed from
    /// `device.limits().max_texture_dimension_2d`, and what
    /// `plan_overlay_texture` reads back through `ui.ctx().input(..)`.
    /// `None` leaves egui on its own default of 2048.
    max_texture_side: Option<usize>,
    /// The [`GuiAction`]s `Gui::ui` returned from the last frame.
    last_actions: Vec<crate::actions::GuiAction>,
}

impl InputHarness {
    /// Build a harness with a fresh [`Gui`] and run enough frames for egui to
    /// settle (areas need a frame to register their rects).
    pub(crate) fn new() -> Self {
        Self::with_screen(SCREEN_SIZE)
    }

    /// A harness on a screen of the given size — e.g. a portrait phone, where
    /// the pane grid and the panel disagree about which way up they are.
    pub(crate) fn with_screen(size: egui::Vec2) -> Self {
        let screen_rect = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
        let mut harness = Self {
            ctx: egui::Context::default(),
            gui: Gui::new(),
            gestures: TouchGestures::default(),
            map_memory: walkers::MapMemory::default(),
            // The map occupies the middle of the window: inset generously so
            // the harness never depends on exact panel widths.
            pane_rect: egui::Rect::from_min_max(
                egui::pos2(220.0, 80.0),
                egui::pos2(1004.0, 690.0),
            ),
            time: 100.0,
            events: Vec::new(),
            screen_rect,
            last_rects: Vec::new(),
            max_texture_side: None,
            last_actions: Vec::new(),
        };
        harness.warm_up();
        // The first frame's `check_auto_polls` starts the initial fetch and
        // nothing here ever completes it, so without this every harness runs
        // with `fetching` latched true forever: the refresh button is
        // permanently `add_enabled(false)`, the status bar shows a spinner
        // instead of the auto-poll checkbox, and `FetchRadarScan`'s click path
        // is unreachable. Settling it puts the harness in the steady state the
        // app spends its life in rather than a transient no test intended.
        harness.gui.set_fetching(false);
        harness.warm_up();
        harness
    }

    /// Resize the viewport, as dragging a window edge or rotating a device
    /// does, and settle. This is how a test crosses a layout breakpoint.
    pub(crate) fn set_screen(&mut self, size: egui::Vec2) {
        self.screen_rect = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
        self.warm_up();
    }

    /// The egui `Id`s the last frame's layers panel resolved.
    pub(crate) fn widget_id_probes(&self) -> Vec<(&'static str, egui::Id)> {
        self.gui.widget_id_probes().to_vec()
    }

    /// Open or close the layers drawer, as tapping the hamburger does.
    pub(crate) fn set_drawer_open(&mut self, open: bool) {
        self.gui.set_drawer_open(open);
        self.warm_up();
    }

    /// Report host safe-area insets, as the Android side channel does.
    ///
    /// `egui-winit` fills `RawInput::safe_area_insets` only under
    /// `cfg(target_os = "ios")`, so Android pushes its `WindowInsets` through
    /// `Gui::set_safe_area_insets` instead. This is that route.
    pub(crate) fn set_safe_area_insets(&mut self, top: f32, bottom: f32, left: f32, right: f32) {
        self.gui.set_safe_area_insets(top, bottom, left, right);
        self.warm_up();
    }

    /// Whether egui has a real widget registered under `id` from the last
    /// frame.
    ///
    /// This is what stops an id probe from shadowing: a probe that reported a
    /// constant, or an id rebuilt from a format string the widget no longer
    /// uses, compares equal to itself across a resize and pins nothing. If
    /// egui knows the id, the widget really is keyed on it.
    pub(crate) fn widget_exists(&self, id: egui::Id) -> bool {
        self.ctx.read_response(id).is_some()
    }

    /// The scroll offset egui has stored under `id`, if any.
    ///
    /// Reading it back through the *probed* id is what makes the breakpoint
    /// test real: if the panel stopped salting its `ScrollArea`, the state
    /// would live under some other id and this returns `None`.
    pub(crate) fn scroll_offset(&self, id: egui::Id) -> Option<egui::Vec2> {
        egui::scroll_area::State::load(&self.ctx, id).map(|s| s.offset)
    }

    /// Scroll the widget under `pos`, as a wheel or a two-finger drag does.
    pub(crate) fn scroll_at(&mut self, pos: egui::Pos2, delta: egui::Vec2) {
        self.events.push(egui::Event::PointerMoved(pos));
        self.events.push(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta,
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::default(),
        });
    }

    /// The floating-chrome rects the last frame excluded from map clicks.
    pub(crate) fn excluded_rects(&self) -> Vec<egui::Rect> {
        self.gui.excluded_rects_for_test().to_vec()
    }

    /// Every menu leaf the last frame's chrome actually drew, whichever
    /// presentation was on screen.
    pub(crate) fn menu_leaves(&self) -> Vec<DrawnMenuLeaf> {
        self.gui.menu_leaves_for_test().to_vec()
    }

    /// The leaf drawn under `label`, if the last frame drew one.
    pub(crate) fn menu_leaf(&self, label: &str) -> Option<DrawnMenuLeaf> {
        self.menu_leaves().into_iter().find(|l| l.label == label)
    }

    /// Whether the **live** active pane has `kind` on — the state the menu
    /// checkbox claims to be showing.
    pub(crate) fn overlay_enabled(&self, kind: OverlayKind) -> bool {
        self.gui.active_pane().is_overlay_enabled(kind)
    }

    /// Whether pane `idx` has `kind` on, whichever pane is active.
    pub(crate) fn overlay_enabled_on(&self, idx: usize, kind: OverlayKind) -> bool {
        self.gui
            .pane(idx)
            .unwrap_or_else(|| panic!("no pane {idx}"))
            .is_overlay_enabled(kind)
    }

    /// Which pane is currently active.
    pub(crate) fn active_pane_index(&self) -> usize {
        self.gui.active_pane_index_for_test()
    }

    /// Turn layer sync between panes on or off.
    pub(crate) fn set_sync_layers(&mut self, on: bool) {
        self.gui.set_sync_layers_for_test(on);
        self.warm_up();
    }

    /// Whether layer sync between panes is on.
    pub(crate) fn sync_layers(&self) -> bool {
        self.gui.is_sync_layers()
    }

    /// Set one pane's overlay state directly, writing both the enabled map and
    /// the config the layers panel reloads from each frame — otherwise the
    /// next frame undoes it.
    pub(crate) fn set_overlay_on_pane(&mut self, idx: usize, kind: OverlayKind, on: bool) {
        self.gui.set_overlay_on_pane_for_test(idx, kind, on);
        self.warm_up();
    }

    /// The pane-count buttons the picker drew on the last frame.
    pub(crate) fn pane_options(&self) -> Vec<crate::ui::PaneOptionProbe> {
        self.gui.pane_options_for_test().to_vec()
    }

    /// Just the counts, in draw order.
    pub(crate) fn pane_option_counts(&self) -> Vec<usize> {
        self.pane_options().iter().map(|o| o.count).collect()
    }

    /// The number of panes the layout is currently split into.
    pub(crate) fn pane_count(&self) -> usize {
        self.gui.pane_count()
    }

    /// The excluded rects `render_map` was actually handed on the last frame.
    pub(crate) fn map_excluded_rects(&self) -> Vec<egui::Rect> {
        self.gui.map_excluded_rects_for_test().to_vec()
    }

    /// What the last frame's status bar drew.
    pub(crate) fn status_bar(&self) -> crate::ui::StatusBarProbe {
        self.gui.status_bar_for_test().clone()
    }

    /// Deliver a scan for `site`, through the host's own delivery path.
    ///
    /// `Gui::set_scan_info_for_site` is what the app calls when a fetch
    /// completes: it fills the matching panes, clears `fetching` *and* calls
    /// `auto_poll.on_success()`. Hand-rolling those would leave the harness in
    /// a state the app never reaches.
    pub(crate) fn load_scan(&mut self, site: &str) {
        let radar_site = rustdar_radar::sites::get_radar_site(site).expect("unknown radar site");
        let info = rustdar_radar::types::ScanInfo {
            site: radar_site.clone(),
            timestamp: chrono::NaiveDate::from_ymd_opt(2026, 7, 24)
                .unwrap()
                .and_hms_opt(18, 30, 0)
                .unwrap(),
            vcp_number: 212,
            available_products: vec![
                rustdar_radar::types::RadarProduct::Reflectivity,
                rustdar_radar::types::RadarProduct::Velocity,
            ],
            product_elevations: Default::default(),
            status: String::new(),
        };
        // The host matches panes by site, so point them at it first.
        for pane in self.gui.panes_mut() {
            pane.site = site.to_owned();
        }
        self.gui.set_scan_info_for_site(site, info);
        self.warm_up();
    }

    /// Every rect painted during the last frame, in paint order.
    pub(crate) fn painted_rects(&self) -> &[egui::Rect] {
        &self.last_rects
    }

    /// The width class the UI resolved for the last frame.
    pub(crate) fn width_class(&self) -> crate::ui_layout::WidthClass {
        self.gui.layout_for_test().width
    }

    /// Report `side` as the adapter's `max_texture_dimension_2d`, the way
    /// `EguiRenderer::new` reports the real device's limit to `egui_winit`.
    ///
    /// This is how a WebGL2-class limit is exercised without a wasm target: the
    /// number reaches `plan_overlay_texture` through exactly the path it does in
    /// the real app, `RawInput` -> `InputState` -> `ui.ctx().input(..)`.
    pub(crate) fn set_max_texture_side(&mut self, side: usize) {
        self.max_texture_side = Some(side);
        self.warm_up();
    }

    /// The actions the last frame's `Gui::ui` emitted.
    pub(crate) fn last_actions(&self) -> &[crate::actions::GuiAction] {
        &self.last_actions
    }

    /// Split the map into `count` panes, as the settings UI does.
    pub(crate) fn set_pane_count(&mut self, count: usize) {
        self.gui.set_pane_count_for_test(count);
        self.warm_up();
    }

    /// The pane rects the real layout produces inside the map panel.
    pub(crate) fn pane_rects(&self) -> Vec<egui::Rect> {
        self.gui.pane_rects_for_test()
    }

    /// The rect the pane grid is laid out in, as `render_map` sees it.
    pub(crate) fn map_panel_rect(&self) -> egui::Rect {
        self.gui.map_panel_rect_for_test()
    }

    /// The color-scale legend strips painted inside `pane`, classified by the
    /// axis they were drawn along.
    ///
    /// `render_color_scale` paints the bar as a run of 2px strips: `(2, 20)`
    /// for a bottom-edge bar, `(20, 2)` for a right-edge one
    /// (`ui_map_pane.rs:632` — `SCALE_BAR_WIDTH` is 20). That signature is what
    /// makes it possible to assert on the drawn result rather than on the value
    /// that was supposed to produce it.
    pub(crate) fn color_scale_strips(&self, pane: egui::Rect) -> (usize, usize) {
        let mut horizontal = 0;
        let mut vertical = 0;
        for rect in &self.last_rects {
            if !pane.contains(rect.center()) {
                continue;
            }
            let (w, h) = (rect.width(), rect.height());
            if (h - 20.0).abs() < 0.5 && w <= 4.0 {
                horizontal += 1;
            } else if (w - 20.0).abs() < 0.5 && h <= 4.0 {
                vertical += 1;
            }
        }
        (horizontal, vertical)
    }

    /// Run a few input-free frames so panels, areas and windows have registered
    /// their layer rects before any assertion depends on them.
    pub(crate) fn warm_up(&mut self) {
        for _ in 0..3 {
            self.frame();
        }
    }

    /// The centre of the map pane — a safe "on the map" position.
    pub(crate) fn map_center(&self) -> egui::Pos2 {
        self.pane_rect.center()
    }

    /// The centre of the viewport, where modal dialogs are placed.
    pub(crate) fn screen_center(&self) -> egui::Pos2 {
        self.screen_rect.center()
    }

    /// The viewport the harness is reporting to egui.
    pub(crate) fn screen_rect(&self) -> egui::Rect {
        self.screen_rect
    }

    /// Mutable access to the UI under test (e.g. to open a dialog).
    pub(crate) fn gui_mut(&mut self) -> &mut Gui {
        &mut self.gui
    }

    /// Whether a floating layer (dialog / popup) currently covers `pos`.
    /// Used by tests to assert their own preconditions.
    pub(crate) fn is_floating_layer_at(&self, pos: egui::Pos2) -> bool {
        self.ctx
            .layer_id_at(pos)
            .is_some_and(|l| l.order > egui::Order::Background)
    }

    /// Current map zoom.
    pub(crate) fn zoom(&self) -> f64 {
        self.map_memory.zoom()
    }

    /// Advance the harness clock without running a frame.
    pub(crate) fn advance(&mut self, seconds: f64) {
        self.time += seconds;
    }

    /// Advance the clock by `seconds`, then run one frame.
    pub(crate) fn frame_after(&mut self, seconds: f64) -> FrameOutcome {
        self.advance(seconds);
        self.frame()
    }

    /// Run `count` frames spaced `seconds` apart and return the last outcome.
    pub(crate) fn frames_for(&mut self, count: usize, seconds: f64) -> FrameOutcome {
        let mut outcome = FrameOutcome::default();
        for _ in 0..count {
            outcome = self.frame_after(seconds);
        }
        outcome
    }

    /// Run input-free frames for `seconds` of wall clock, asserting `check` on
    /// **every** frame.
    ///
    /// Watching only the last frame is how a re-arming gesture slips through: a
    /// stuck long press needs [`LONG_PRESS_DURATION_S`] to come back, so any
    /// "it stayed released" assertion has to cover well past that, frame by
    /// frame.
    pub(crate) fn assert_every_frame_for(
        &mut self,
        seconds: f64,
        step: f64,
        mut check: impl FnMut(usize, &FrameOutcome),
    ) -> FrameOutcome {
        let count = (seconds / step).ceil() as usize;
        let mut outcome = FrameOutcome::default();
        for frame in 0..count {
            outcome = self.frame_after(step);
            check(frame, &outcome);
        }
        outcome
    }

    // --- mouse input (mirrors egui-winit's cursor + button handling) --------

    pub(crate) fn mouse_move(&mut self, pos: egui::Pos2) {
        self.events.push(egui::Event::PointerMoved(pos));
    }

    pub(crate) fn mouse_press(&mut self, pos: egui::Pos2) {
        self.mouse_move(pos);
        self.events.push(pointer_button(pos, true));
    }

    pub(crate) fn mouse_release(&mut self, pos: egui::Pos2) {
        self.mouse_move(pos);
        self.events.push(pointer_button(pos, false));
    }

    /// The cursor left the window: `egui-winit` maps `WindowEvent::CursorLeft`
    /// to a bare [`egui::Event::PointerGone`] and forgets the pointer position
    /// (`egui-winit-0.34.1/src/lib.rs:340`). **No release is reported** — and
    /// while the position is forgotten, a real mouse release happening outside
    /// the window is dropped on the floor too (`lib.rs:796`), which is why
    /// egui's `primary_down()` can stay latched across the excursion.
    pub(crate) fn cursor_left(&mut self) {
        self.events.push(egui::Event::PointerGone);
    }

    /// Raw device motion (`DeviceEvent::MouseMotion` → [`egui::Event::MouseMoved`]).
    /// It carries a delta and **no position**, so egui has nothing to put in
    /// `interact_pos()` on such a frame.
    ///
    /// No integration in this workspace actually produces this:
    /// `egui-winit`'s `on_mouse_motion` (`lib.rs:759`) is reachable only from
    /// `DeviceEvent`, and `rustdar-platform/src/egui_renderer.rs:59` forwards
    /// `on_window_event` only. It is here to exercise the tracker's defensive
    /// position fallback, and to prove a delta with no coordinates cannot
    /// resurrect a cancelled touch.
    pub(crate) fn mouse_moved_raw(&mut self, delta: egui::Vec2) {
        self.events.push(egui::Event::MouseMoved(delta));
    }

    // --- web input (mirrors eframe 0.34.1's canvas listeners) ---------------

    /// `touchstart`, as eframe's web canvas emits it: the primary
    /// `PointerButton{pressed}` **first**, then `push_touches(Start)`
    /// (`eframe/src/web/events.rs:676`) — the opposite order to `egui-winit`,
    /// which is why the tracker correlates the pair over the whole frame.
    pub(crate) fn web_touch_start(&mut self, pos: egui::Pos2) {
        self.events.push(pointer_button(pos, true));
        self.events.push(touch(egui::TouchPhase::Start, pos));
    }

    /// `touchmove` (`events.rs:709`): a bare `PointerMoved`, with the raw touch
    /// pushed alongside it.
    pub(crate) fn web_touch_move(&mut self, pos: egui::Pos2) {
        self.events.push(egui::Event::PointerMoved(pos));
        self.events.push(touch(egui::TouchPhase::Move, pos));
    }

    /// `touchcancel` (`events.rs:788`): `push_touches(Cancel)` and **nothing
    /// else** — no release, no `PointerGone`. egui's `primary_down()` therefore
    /// stays latched `true` with no event ever clearing it, so a tracker that
    /// keys cancellation on `PointerGone` alone never fires at all here.
    pub(crate) fn web_touch_cancel(&mut self, pos: egui::Pos2) {
        self.events.push(touch(egui::TouchPhase::Cancel, pos));
    }

    /// `mousemove` (`events.rs:627`): a bare `PointerMoved`. Note this reaches
    /// the canvas whether or not any touch is involved, which is what makes a
    /// motion-based un-latch dangerous after a cancellation on the web.
    pub(crate) fn web_mouse_move(&mut self, pos: egui::Pos2) {
        self.events.push(egui::Event::PointerMoved(pos));
    }

    // --- touch input (mirrors egui-winit's `on_touch`) ----------------------

    pub(crate) fn touch_start(&mut self, pos: egui::Pos2) {
        self.events.push(touch(egui::TouchPhase::Start, pos));
        self.events.push(egui::Event::PointerMoved(pos));
        self.events.push(pointer_button(pos, true));
    }

    pub(crate) fn touch_move(&mut self, pos: egui::Pos2) {
        self.events.push(touch(egui::TouchPhase::Move, pos));
        self.events.push(egui::Event::PointerMoved(pos));
    }

    pub(crate) fn touch_end(&mut self, pos: egui::Pos2) {
        self.events.push(touch(egui::TouchPhase::End, pos));
        self.events.push(pointer_button(pos, false));
        self.events.push(egui::Event::PointerGone);
    }

    /// The OS/browser took the gesture away: **no release is reported**, only
    /// `PointerGone`, exactly as `egui-winit` does for `TouchPhase::Cancelled`.
    pub(crate) fn touch_cancel(&mut self, pos: egui::Pos2) {
        self.events.push(touch(egui::TouchPhase::Cancel, pos));
        self.events.push(egui::Event::PointerGone);
    }

    /// A *secondary* finger's touch being cancelled: a raw `Touch{Cancel}` for
    /// another `TouchId`, with no `PointerGone`, since the emulated pointer is
    /// still owned by the primary finger.
    pub(crate) fn secondary_touch_cancel(&mut self, pos: egui::Pos2) {
        self.events.push(egui::Event::Touch {
            device_id: egui::TouchDeviceId(0),
            id: egui::TouchId(1),
            phase: egui::TouchPhase::Cancel,
            pos,
            force: None,
        });
    }

    // --- composite gestures -------------------------------------------------

    /// A quick touch tap (press + release within the tap thresholds), spread
    /// over two frames like a real one.
    pub(crate) fn touch_tap(&mut self, pos: egui::Pos2) -> FrameOutcome {
        self.touch_start(pos);
        self.frame_after(FRAME_DT);
        self.touch_end(pos);
        self.frame_after(0.05)
    }

    /// A quick mouse click (press + release), spread over two frames.
    pub(crate) fn mouse_click(&mut self, pos: egui::Pos2) -> FrameOutcome {
        self.mouse_press(pos);
        self.frame_after(FRAME_DT);
        self.mouse_release(pos);
        self.frame_after(0.05)
    }

    /// Run one egui pass: `Gui::ui` followed by the pane pointer resolution.
    pub(crate) fn frame(&mut self) -> FrameOutcome {
        let raw_input = egui::RawInput {
            screen_rect: Some(self.screen_rect),
            time: Some(self.time),
            events: std::mem::take(&mut self.events),
            max_texture_side: self.max_texture_side,
            ..Default::default()
        };

        // `begin_pass`/`end_pass` rather than `run_ui`, so the body runs exactly
        // once per frame: a repeated pass would feed the same events to the
        // gesture detectors twice.
        let ctx = self.ctx.clone();
        ctx.begin_pass(raw_input);

        // The real UI, panels, dialogs and map panes included. `render_map`
        // resolves each pane's pointer state on the way through and records it.
        self.last_actions = self.gui.ui(&ctx);

        // `mouse` and `touch` drive each pipeline directly, bypassing the gate,
        // so a test can say what a given pipeline *would* have done. They are
        // the only two parallel probes left, and neither claims to be the app.
        let mouse = MapPointerFrame::from_mouse(&ctx);
        let touch = self
            .gestures
            .update(&ctx, &mut self.map_memory, self.pane_rect);

        // Everything gated is read back out of the `Gui` that just ran.
        let probes = self.gui.pane_pointers_for_test();
        let active = probes
            .iter()
            .find(|p| p.is_active)
            .copied()
            .unwrap_or_else(|| {
                panic!(
                    "render_map recorded no active pane this frame ({} pane probe(s)) \
                     — the pointer pipeline never ran, so nothing below means anything",
                    probes.len()
                )
            });
        let inactive = probes.iter().find(|p| !p.is_active).map(|p| p.frame);

        let outcome = FrameOutcome {
            mouse,
            touch,
            resolved: active.frame,
            resolved_inactive: inactive,
            modality: active.modality,
            zoom: self.map_memory.zoom(),
            resolved_zoom: self.gui.active_pane().map_memory.zoom(),
        };

        let full_output = ctx.end_pass();
        self.last_rects = full_output
            .shapes
            .iter()
            .filter_map(|clipped| match &clipped.shape {
                egui::Shape::Rect(rect_shape) => Some(rect_shape.rect),
                _ => None,
            })
            .collect();
        outcome
    }
}

fn pointer_button(pos: egui::Pos2, pressed: bool) -> egui::Event {
    egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::default(),
    }
}

fn touch(phase: egui::TouchPhase, pos: egui::Pos2) -> egui::Event {
    egui::Event::Touch {
        device_id: egui::TouchDeviceId(0),
        id: egui::TouchId(0),
        phase,
        pos,
        force: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two durations that bracket the idle backstop, deliberately **not**
    /// derived from `POINTER_IDLE_TIMEOUT_S`.
    ///
    /// A probe that sizes its own loop off the constant under test cannot
    /// notice that constant changing — it just moves with it, and both a 32s
    /// and a 600s backstop pass. These are absolute claims about the behaviour
    /// instead: a still hold must survive the first, and a pointer that has
    /// gone silent must not survive the second.
    const HOLD_MUST_SURVIVE_S: f64 = 45.0;
    const SILENCE_MUST_EXPIRE_S: f64 = 90.0;

    /// Long enough for a deferred single tap to be confirmed
    /// (`DOUBLE_TAP_TIMEOUT_S` is 0.4s).
    const AFTER_DOUBLE_TAP_TIMEOUT: f64 = 0.5;

    /// How long a "the gesture really ended" assertion must keep watching.
    ///
    /// It has to clear `LONG_PRESS_DURATION_S` (0.8s) by a wide margin — that
    /// is how long a detector that re-arms itself off a stale pointer takes to
    /// come back — and it also has to be long enough that a pointer which is
    /// *supposed* to stay dead is watched over a realistic span rather than a
    /// couple of seconds. Half a minute of frame-by-frame checking is cheap
    /// here (the whole suite runs headless in well under a second).
    const WATCH_PAST_LONG_PRESS: f64 = 30.0;

    /// 1. A single mouse click reports a click position at the clicked point
    ///    and never suppresses panning.
    #[test]
    fn mouse_single_click_reports_click_pos() {
        let mut h = InputHarness::new();
        let pos = h.map_center();

        let outcome = h.mouse_click(pos);

        assert_eq!(outcome.mouse.overlay_click_pos, Some(pos));
        assert!(!outcome.mouse.suppress_pan);
        assert_eq!(outcome.mouse.long_press_pos, None);

        // The click is a single-frame event: the next frame is clean again.
        let next = h.frame_after(FRAME_DT);
        assert_eq!(next.mouse.overlay_click_pos, None);
    }

    /// 2. A mouse double click reports a click on each release, and the touch
    ///    pipeline defers instead of firing two overlay taps.
    #[test]
    fn mouse_double_click_reports_each_click() {
        let mut h = InputHarness::new();
        let pos = h.map_center();

        let first = h.mouse_click(pos);
        assert_eq!(first.mouse.overlay_click_pos, Some(pos));

        // Second click inside egui's double-click window.
        let second = h.mouse_click(pos);
        assert_eq!(second.mouse.overlay_click_pos, Some(pos));
        assert!(!second.mouse.suppress_pan);

        // The touch pipeline treats the same input as a double-tap: no overlay
        // tap is emitted while the second press is pending.
        assert_eq!(first.touch.overlay_click_pos, None);
        assert_eq!(second.touch.overlay_click_pos, None);
    }

    /// 3. Pressing and holding for ~1s without moving is a long press: it
    ///    reports the held position and suppresses map panning, and it is not
    ///    a click.
    #[test]
    fn press_and_hold_becomes_long_press() {
        let mut h = InputHarness::new();
        let pos = h.map_center();

        h.mouse_press(pos);
        let pressed = h.frame_after(FRAME_DT);
        assert_eq!(pressed.touch.long_press_pos, None, "not held long enough yet");
        assert!(!pressed.touch.suppress_pan);

        // Hold for ~1s (LONG_PRESS_DURATION_S is 0.8s) without moving.
        let held = h.frames_for(10, 0.1);
        assert_eq!(held.touch.long_press_pos, Some(pos));
        assert!(held.touch.suppress_pan, "long press owns the pointer");
        assert_eq!(
            held.mouse.overlay_click_pos, None,
            "a press with no release is not a click"
        );

        // Releasing ends the long press; the slow release is not a tap either.
        h.mouse_release(pos);
        let released = h.frame_after(FRAME_DT);
        assert_eq!(released.touch.long_press_pos, None);
        assert!(!released.touch.suppress_pan);

        let settled = h.frames_for(3, 0.3);
        assert_eq!(
            settled.touch.overlay_click_pos, None,
            "a 1s hold is not a tap"
        );
    }

    /// 4. A touch tap is deferred until the double-tap window closes, then
    ///    reported once at the tapped position.
    #[test]
    fn touch_tap_is_deferred_then_confirmed() {
        let mut h = InputHarness::new();
        let pos = h.map_center();

        let on_release = h.touch_tap(pos);
        assert_eq!(
            on_release.touch.overlay_click_pos, None,
            "tap must wait out the double-tap window"
        );
        assert!(!on_release.touch.suppress_pan);

        let confirmed = h.frame_after(AFTER_DOUBLE_TAP_TIMEOUT);
        assert_eq!(confirmed.touch.overlay_click_pos, Some(pos));
        assert!(!confirmed.touch.suppress_pan);

        // Consumed exactly once.
        let next = h.frame_after(FRAME_DT);
        assert_eq!(next.touch.overlay_click_pos, None);
    }

    /// 5. Tap, then press again and drag down: the map zooms, panning is
    ///    suppressed for the whole drag, and no overlay tap is emitted.
    #[test]
    fn touch_double_tap_drag_zooms_and_suppresses_pan() {
        let mut h = InputHarness::new();
        let start = h.map_center();
        let zoom_before = h.zoom();

        // First tap.
        h.touch_tap(start);

        // Second press within the double-tap window enters the zoom drag.
        h.touch_start(start);
        let dragging = h.frame_after(0.05);
        assert!(dragging.touch.suppress_pan, "zoom drag must block map panning");
        assert_eq!(dragging.touch.overlay_click_pos, None);
        assert_eq!(dragging.touch.long_press_pos, None);

        // Drag downward: ZOOM_DRAG_SENSITIVITY is 150px per zoom level.
        for step in 1..=3 {
            h.touch_move(start + egui::vec2(0.0, 50.0 * step as f32));
            let frame = h.frame_after(FRAME_DT);
            assert!(frame.touch.suppress_pan);
        }
        let dragged = h.frame_after(FRAME_DT);
        assert!(
            dragged.zoom > zoom_before,
            "dragging down should zoom in: {} -> {}",
            zoom_before,
            dragged.zoom
        );

        // Lifting ends the gesture and does not emit an overlay tap.
        h.touch_end(start + egui::vec2(0.0, 150.0));
        let lifted = h.frame_after(FRAME_DT);
        assert!(!lifted.touch.suppress_pan, "pan must be restored on lift");

        let settled = h.frames_for(3, 0.3);
        assert_eq!(
            settled.touch.overlay_click_pos, None,
            "double-tap-drag must never open an overlay popup"
        );
    }

    /// 6. **PROBE B — regression test for the stranded zoom drag.** The OS cancels the
    ///    touch mid-drag: only `PointerGone` arrives, no release, and egui keeps
    ///    reporting `pointer.down == true` forever. The gesture must still end,
    ///    or the map stays un-pannable until the app restarts.
    #[test]
    fn touch_cancelled_mid_drag_releases_the_map() {
        let mut h = InputHarness::new();
        let start = h.map_center();

        h.touch_tap(start);
        h.touch_start(start);
        let dragging = h.frame_after(0.05);
        assert!(dragging.touch.suppress_pan, "precondition: zoom drag active");

        h.touch_move(start + egui::vec2(0.0, 60.0));
        assert!(h.frame_after(FRAME_DT).touch.suppress_pan);

        // System edge gesture / incoming call / browser `touchcancel`.
        h.touch_cancel(start + egui::vec2(0.0, 60.0));
        let cancelled = h.frame_after(FRAME_DT);
        assert!(
            !cancelled.touch.suppress_pan,
            "cancelled touch must not leave the map in zoom-drag"
        );
        assert_eq!(cancelled.touch.long_press_pos, None);

        // …and it must stay released, frame after frame, even though egui still
        // reports the primary button as down. This has to run well past
        // LONG_PRESS_DURATION_S (0.8s): the phantom finger is still "down", so a
        // detector that re-arms on `down` takes exactly that long to claim it
        // back — as a long press pinned at Pos2::ZERO, because `PointerGone`
        // cleared egui's pointer position.
        h.assert_every_frame_for(WATCH_PAST_LONG_PRESS, 0.1, |frame, outcome| {
            assert!(
                !outcome.touch.suppress_pan,
                "frame {frame}: map must remain pannable after a cancelled touch"
            );
            assert_eq!(
                outcome.touch.long_press_pos, None,
                "frame {frame}: a cancelled touch must not become a long press"
            );
            assert_eq!(outcome.touch.overlay_click_pos, None, "frame {frame}");
        });
    }

    /// 6b. **PROBE A** — the same cancellation, but during a long press: the
    ///     tooltip position must not stick, and must not come back either.
    #[test]
    fn touch_cancelled_during_long_press_clears_it() {
        let mut h = InputHarness::new();
        let pos = h.map_center();

        h.touch_start(pos);
        let held = h.frames_for(10, 0.1);
        assert_eq!(held.touch.long_press_pos, Some(pos), "precondition: long press");
        assert!(held.touch.suppress_pan);

        h.touch_cancel(pos);
        let cancelled = h.frame_after(FRAME_DT);
        assert_eq!(cancelled.touch.long_press_pos, None);
        assert!(!cancelled.touch.suppress_pan);

        // Watch past LONG_PRESS_DURATION_S: clearing the state once is not
        // enough if the detector is allowed to re-arm off egui's latched `down`.
        h.assert_every_frame_for(WATCH_PAST_LONG_PRESS, 0.1, |frame, outcome| {
            assert_eq!(
                outcome.touch.long_press_pos, None,
                "frame {frame}: the long press must not re-arm itself"
            );
            assert!(!outcome.touch.suppress_pan, "frame {frame}");
        });
    }

    /// 6c. A *secondary* finger being cancelled must not kill the primary
    ///     finger's live gesture. `Event::Touch { phase: Cancel }` carries a
    ///     `TouchId` that cannot be matched against the emulated pointer, so the
    ///     tracker keys on `PointerGone` alone.
    #[test]
    fn secondary_touch_cancel_does_not_end_the_drag() {
        let mut h = InputHarness::new();
        let start = h.map_center();

        h.touch_tap(start);
        h.touch_start(start);
        assert!(
            h.frame_after(0.05).touch.suppress_pan,
            "precondition: zoom drag active"
        );

        h.secondary_touch_cancel(start + egui::vec2(80.0, 0.0));
        let after = h.frame_after(FRAME_DT);
        assert!(
            after.touch.suppress_pan,
            "another finger's cancellation must not end the primary gesture"
        );

        // The drag still zooms.
        let zoom_before = after.zoom;
        h.touch_move(start + egui::vec2(0.0, 120.0));
        let dragged = h.frame_after(FRAME_DT);
        assert!(dragged.touch.suppress_pan);
        assert!(dragged.zoom > zoom_before, "the drag must still be live");
    }

    /// 6d. **PROBE C** — a zoom drag that keeps moving must never be cut off,
    ///     however long it runs — a user framing a view can easily hold one for
    ///     many seconds. (The pointer backstop is keyed on inactivity, not on
    ///     gesture age, so this runs well past `POINTER_IDLE_TIMEOUT_S`.)
    #[test]
    fn long_active_zoom_drag_is_never_cut_off() {
        let mut h = InputHarness::new();
        let start = h.map_center();

        h.touch_tap(start);
        h.touch_start(start);
        assert!(h.frame_after(0.05).touch.suppress_pan);

        // 40 seconds of continuous dragging, well past any plausible backstop.
        let mut offset = 0.0_f32;
        for step in 0..80 {
            offset = if step % 2 == 0 { 40.0 } else { -40.0 };
            h.touch_move(start + egui::vec2(0.0, offset));
            let frame = h.frame_after(0.5);
            assert!(
                frame.touch.suppress_pan,
                "step {step}: an actively moving drag must stay in control"
            );
            assert_eq!(
                frame.touch.long_press_pos, None,
                "step {step}: the drag must not hand the finger to the long press"
            );
        }

        // Still responding to movement at the end.
        let zoom_before = h.zoom();
        h.touch_move(start + egui::vec2(0.0, offset + 100.0));
        let dragged = h.frame_after(FRAME_DT);
        assert_ne!(dragged.zoom, zoom_before, "the drag must still zoom");
    }

    /// 6e. If pointer input simply stops arriving mid-gesture (the integration
    ///     went away without ever sending a release or a cancel), the stale
    ///     "finger is down" belief expires — and does not get handed to the long
    ///     press on the way out.
    #[test]
    fn silent_pointer_expires_and_stays_expired() {
        let mut h = InputHarness::new();
        let start = h.map_center();

        h.touch_tap(start);
        h.touch_start(start);
        assert!(h.frame_after(0.05).touch.suppress_pan);
        h.touch_move(start + egui::vec2(0.0, 40.0));
        assert!(h.frame_after(FRAME_DT).touch.suppress_pan);

        // No events at all from here on, for longer than the backstop allows.
        let expired = h.frames_for((SILENCE_MUST_EXPIRE_S / 0.5) as usize, 0.5);
        assert!(
            !expired.touch.suppress_pan,
            "a pointer that stopped reporting must not hold the map hostage"
        );

        h.assert_every_frame_for(WATCH_PAST_LONG_PRESS, 0.1, |frame, outcome| {
            assert!(!outcome.touch.suppress_pan, "frame {frame}");
            assert_eq!(
                outcome.touch.long_press_pos, None,
                "frame {frame}: an expired pointer must not become a long press"
            );
        });
    }

    /// 6f. **PROBE D** — the desktop excursion. The button is held, the cursor
    ///     leaves the window and comes back still held, and everything that
    ///     arrives in between is a `PointerMoved`: no press, ever.
    ///
    ///     `egui-winit` maps `CursorLeft` to a bare `PointerGone` and forgets
    ///     the pointer position, which also makes it drop a mouse release that
    ///     happens outside the window — so egui's `primary_down()` stays
    ///     latched right through the excursion. A latch that only a *press*
    ///     could clear therefore stranded the pointer here exactly as a
    ///     cancelled touch used to strand it: dead until the user clicked.
    #[test]
    fn pointer_returning_to_the_window_recovers_without_a_click() {
        let mut h = InputHarness::new();
        let inside = h.map_center();

        h.mouse_press(inside);
        let held = h.frames_for(10, 0.1);
        assert_eq!(
            held.touch.long_press_pos,
            Some(inside),
            "precondition: the pointer is live and held"
        );

        // The cursor leaves. Nothing says whether the button survived the trip,
        // so the pointer must be distrusted for as long as it stays silent.
        h.cursor_left();
        let gone = h.frame_after(FRAME_DT);
        assert_eq!(gone.touch.long_press_pos, None, "the held position must not stick");
        assert!(!gone.touch.suppress_pan);

        // It comes back, still dragging: five move events, no press among them.
        let mut back = inside;
        for step in 1..=5 {
            back = inside + egui::vec2(12.0 * step as f32, 7.0 * step as f32);
            h.mouse_move(back);
            h.frame_after(FRAME_DT);
        }

        // Coming back with nothing but motion is not enough to reopen: the
        // release that may have happened out of sight was discarded by the
        // integration (`lib.rs:796`), so this stream is indistinguishable from
        // a bare hover. A tooltip here would suppress panning until the user
        // clicked — see `ui_input::tests::an_excursion_is_terminal_until_a_press`.
        let hovering = h.frames_for(20, 0.1);
        assert_eq!(
            hovering.touch.long_press_pos, None,
            "a returning pointer must not open a hold nobody pressed for"
        );
        assert!(!hovering.touch.suppress_pan);

        // And it is not wedged either: one real press restores everything.
        h.mouse_press(back);
        let pressed = h.frames_for(10, 0.1);
        assert_eq!(pressed.touch.long_press_pos, Some(back));
        assert!(pressed.touch.suppress_pan);
    }

    /// 6f-R1. **PROBE R1** — a cancelled touch must not be resurrected by a
    ///        bare `PointerMoved`.
    ///
    ///        After a cancel, egui's `primary_down()` is latched `true` with
    ///        nothing left that will ever clear it, so the tracker's distrust
    ///        is the only thing standing between that and a phantom gesture.
    ///        Motion keeps arriving regardless — `egui-winit` clears
    ///        `pointer_touch_id` on cancel (`lib.rs:922`) and then admits the
    ///        *next* finger's moves as `PointerMoved` with no press
    ///        (`lib.rs:894`, `lib.rs:906`) — so "a cancel is followed by
    ///        silence" is true of that finger, not of the pointer stream.
    #[test]
    fn motion_after_a_cancel_does_not_resurrect_the_pointer() {
        let mut h = InputHarness::new();
        let pos = h.map_center();

        h.touch_start(pos);
        assert_eq!(
            h.frames_for(10, 0.1).touch.long_press_pos,
            Some(pos),
            "precondition: long press active"
        );

        h.touch_cancel(pos);
        assert_eq!(h.frame_after(FRAME_DT).touch.long_press_pos, None);

        // A second finger, still on the glass, moves.
        h.mouse_move(pos + egui::vec2(90.0, 60.0));
        h.frame_after(FRAME_DT);

        h.assert_every_frame_for(WATCH_PAST_LONG_PRESS, 0.1, |frame, outcome| {
            assert_eq!(
                outcome.touch.long_press_pos, None,
                "frame {frame}: motion is not the cancelled finger coming back"
            );
            assert!(!outcome.touch.suppress_pan, "frame {frame}");
        });
    }

    /// 6f-R2. **PROBE R2** — the same, for `MouseMoved`: a delta with no
    ///        coordinates at all. This is the worst resurrection vector,
    ///        because the phantom would land at `last_pos` — exactly where the
    ///        OS took the touch away.
    #[test]
    fn positionless_motion_after_a_cancel_does_not_resurrect_the_pointer() {
        let mut h = InputHarness::new();
        let pos = h.map_center();

        h.touch_start(pos);
        assert_eq!(h.frames_for(10, 0.1).touch.long_press_pos, Some(pos));

        h.touch_cancel(pos);
        assert_eq!(h.frame_after(FRAME_DT).touch.long_press_pos, None);

        h.mouse_moved_raw(egui::vec2(2.0, 1.0));
        h.frame_after(FRAME_DT);

        h.assert_every_frame_for(WATCH_PAST_LONG_PRESS, 0.1, |frame, outcome| {
            assert_eq!(
                outcome.touch.long_press_pos, None,
                "frame {frame}: a cancelled touch must not come back at its own last position"
            );
            assert!(!outcome.touch.suppress_pan, "frame {frame}");
        });
    }

    /// 6f-R3. **PROBE R3** — and for a cancelled *zoom drag*: motion must not
    ///        hand the map back to a gesture the OS took away.
    #[test]
    fn motion_after_a_cancelled_zoom_drag_does_not_restore_it() {
        let mut h = InputHarness::new();
        let start = h.map_center();

        h.touch_tap(start);
        h.touch_start(start);
        assert!(h.frame_after(0.05).touch.suppress_pan, "precondition: zoom drag");

        h.touch_cancel(start);
        assert!(!h.frame_after(FRAME_DT).touch.suppress_pan);

        h.mouse_move(start + egui::vec2(0.0, 80.0));
        h.frame_after(FRAME_DT);

        h.assert_every_frame_for(WATCH_PAST_LONG_PRESS, 0.1, |frame, outcome| {
            assert!(
                !outcome.touch.suppress_pan,
                "frame {frame}: the map must stay pannable after a cancelled drag"
            );
            assert_eq!(outcome.touch.long_press_pos, None, "frame {frame}");
        });
    }

    /// 6f-R4. **PROBE R4** — a cancellation on the web, which arrives as a bare
    ///        `Touch{Cancel}`.
    ///
    ///        eframe 0.34.1's `install_touchcancel` pushes `push_touches(Cancel)`
    ///        and nothing else (`eframe/src/web/events.rs:788`): no release, no
    ///        `PointerGone`. Keying cancellation on `PointerGone` alone never
    ///        fired here at all — the map stayed un-pannable behind a stuck
    ///        tooltip until the idle backstop, a minute later. Browsers fire
    ///        `touchcancel` routinely (scroll takeover, page hide, too many
    ///        contact points).
    #[test]
    fn web_touch_cancel_releases_the_map() {
        let mut h = InputHarness::new();
        let pos = h.map_center();

        h.web_touch_start(pos);
        h.frame_after(FRAME_DT);
        // A little jitter, as a real finger produces — and as a browser
        // delivers it, so the cancellation below is not reached through an
        // artificially silent stream.
        h.web_touch_move(pos + egui::vec2(2.0, 1.0));
        assert_eq!(
            h.frames_for(10, 0.1).touch.long_press_pos,
            Some(pos + egui::vec2(2.0, 1.0)),
            "precondition: long press active"
        );

        h.web_touch_cancel(pos + egui::vec2(2.0, 1.0));
        let cancelled = h.frame_after(FRAME_DT);
        assert_eq!(
            cancelled.touch.long_press_pos, None,
            "a bare Touch{{Cancel}} is the whole cancellation signal on the web"
        );
        assert!(!cancelled.touch.suppress_pan);

        // `mousemove` on the canvas is a bare `PointerMoved` and does not care
        // that a touch was involved — it must not undo the cancellation.
        h.web_mouse_move(pos + egui::vec2(70.0, 40.0));
        h.frame_after(FRAME_DT);

        h.assert_every_frame_for(WATCH_PAST_LONG_PRESS, 0.1, |frame, outcome| {
            assert_eq!(outcome.touch.long_press_pos, None, "frame {frame}");
            assert!(!outcome.touch.suppress_pan, "frame {frame}");
        });
    }

    /// 6f-R5. **PROBE R5** — the button was released *outside* the window.
    ///
    ///        `egui-winit` drops a mouse release while the cursor is out of the
    ///        window (`lib.rs:796` needs a position it no longer has), so egui
    ///        reports the button as down forever afterwards. Coming back is
    ///        then indistinguishable from coming back still holding it, which
    ///        is why no hold may arm on the strength of motion alone.
    #[test]
    fn a_release_outside_the_window_does_not_return_as_a_hold() {
        let mut h = InputHarness::new();
        let inside = h.map_center();

        h.mouse_press(inside);
        h.frame_after(FRAME_DT);

        // Out of the window; the release that happens out there never arrives.
        h.cursor_left();
        h.frame_after(FRAME_DT);

        // Back in, hovering, nothing held.
        h.mouse_move(inside + egui::vec2(30.0, 20.0));
        h.frame_after(FRAME_DT);

        h.assert_every_frame_for(WATCH_PAST_LONG_PRESS, 0.1, |frame, outcome| {
            assert_eq!(
                outcome.touch.long_press_pos, None,
                "frame {frame}: hovering must not become a hold"
            );
            assert!(
                !outcome.touch.suppress_pan,
                "frame {frame}: a phantom hold would kill panning until the next click"
            );
        });
    }

    /// 6g. **PROBE G** — recovery after the idle backstop fires. The finger was
    ///     resting, the backstop stopped believing in it, and then it moves.
    ///     Expiry latches (so the long press cannot pick a phantom finger back
    ///     up), but the latch has to be undoable by the finger itself —
    ///     otherwise a resumed drag needs a lift and a fresh press.
    #[test]
    fn pointer_recovers_from_idle_expiry_without_a_lift() {
        let mut h = InputHarness::new();
        let start = h.map_center();

        h.touch_start(start);
        assert!(h.frame_after(FRAME_DT).touch.long_press_pos.is_none());

        // Total silence, past the backstop.
        let expired = h.frames_for((SILENCE_MUST_EXPIRE_S / 0.5) as usize, 0.5);
        assert_eq!(
            expired.touch.long_press_pos, None,
            "precondition: the backstop gave up on the pointer"
        );
        assert!(!expired.touch.suppress_pan);

        // The finger was there all along, and starts moving again.
        let resumed = start + egui::vec2(0.0, 60.0);
        h.touch_move(resumed);
        h.frame_after(FRAME_DT);

        let recovered = h.frames_for(10, 0.1);
        assert_eq!(
            recovered.touch.long_press_pos,
            Some(resumed),
            "a resumed gesture must recover on its own, with no lift and no re-press"
        );
        assert!(recovered.touch.suppress_pan);
    }

    /// 6h. **PROBE H** — a deliberately still hold keeps its tooltip.
    ///
    ///     This is the case the idle backstop has to be sized against: reading
    ///     a radar value means holding a finger in one place and emitting
    ///     nothing at all, and half a minute of that is an ordinary thing to do.
    #[test]
    fn a_deliberately_still_hold_keeps_its_tooltip() {
        let mut h = InputHarness::new();
        let pos = h.map_center();

        h.touch_start(pos);
        let held = h.frames_for(10, 0.1);
        assert_eq!(held.touch.long_press_pos, Some(pos), "precondition: long press");

        // Not one event for thirty seconds; the finger has not moved a pixel.
        h.assert_every_frame_for(HOLD_MUST_SURVIVE_S, 0.25, |frame, outcome| {
            assert_eq!(
                outcome.touch.long_press_pos,
                Some(pos),
                "frame {frame}: the tooltip must survive a still finger"
            );
            assert!(outcome.touch.suppress_pan, "frame {frame}");
        });
    }

    /// 8. **The panel decides which edge, and that decision reaches the paint.**
    ///
    ///    A portrait phone split into three panes (`[2, 1]`) is the case no
    ///    per-pane threshold can get right: the two top panes come out clearly
    ///    portrait and the bottom one clearly landscape, so keying on each
    ///    pane's own rect paints two bottom bars and one right-hand bar on the
    ///    same screen.
    ///
    ///    This asserts on the *painted strips*, not on the resolved value,
    ///    because the resolved value was never the part at risk: what needed
    ///    pinning was that `render_map` resolves from the panel, that the
    ///    answer is threaded through `PaneRenderCtx`, and that neither renderer
    ///    quietly recomputes it from the pane it happens to be drawing.
    #[test]
    fn every_pane_draws_its_color_scale_on_the_same_edge() {
        let mut h = InputHarness::with_screen(egui::vec2(1080.0, 1273.0));
        h.set_pane_count(3);
        h.frame();

        let panes = h.pane_rects();
        assert_eq!(panes.len(), 3, "precondition: a [2, 1] grid");

        // Preconditions, so this fails loudly rather than silently stopping
        // being a test if the layout maths ever changes.
        let ratio = |r: egui::Rect| r.height() / r.width();
        assert!(
            ratio(panes[0]) > 1.35,
            "top panes must be clearly portrait, got {}",
            ratio(panes[0])
        );
        assert!(
            ratio(panes[2]) < 1.05,
            "the bottom pane must be clearly landscape, got {} — otherwise the \
             panes do not disagree and this test proves nothing",
            ratio(panes[2])
        );

        for (idx, pane) in panes.iter().enumerate() {
            let (horizontal, vertical) = h.color_scale_strips(*pane);
            assert!(
                horizontal > 0,
                "pane {idx}: expected a bottom-edge colour bar, painted none"
            );
            assert_eq!(
                vertical, 0,
                "pane {idx}: painted a right-edge bar — the panes disagree, \
                 which is the whole artefact the panel-keyed decision removes"
            );
        }
    }

    /// 8b. **The panel is the key, not the active pane.**
    ///
    ///     The `[2, 1]` test above cannot see the difference: in that grid
    ///     pane 0 is `panel_w/2 × panel_h/2`, which has the *same* aspect ratio
    ///     as the panel, so keying on the panel and keying on the active pane
    ///     agree by construction and its precondition is simultaneously a
    ///     statement about both.
    ///
    ///     A 2-pane grid separates them: each pane is `panel_w/2 × panel_h`, so
    ///     its ratio is exactly twice the panel's. At 1180×1000 the panel comes
    ///     out landscape while both panes are emphatically portrait, and the
    ///     two candidate keys give opposite answers.
    #[test]
    fn the_color_scale_axis_comes_from_the_panel_not_a_pane() {
        let mut h = InputHarness::with_screen(egui::vec2(1180.0, 1000.0));
        h.set_pane_count(2);
        h.frame();

        let panel = h.map_panel_rect();
        let panes = h.pane_rects();
        assert_eq!(panes.len(), 2);

        let ratio = |r: egui::Rect| r.height() / r.width();
        assert!(
            ratio(panel) < 1.05,
            "precondition: the panel must be clearly not portrait, got {}",
            ratio(panel)
        );
        assert!(
            ratio(panes[0]) > 1.35,
            "precondition: each pane must be clearly portrait, got {} — \
             otherwise panel and pane agree and this test proves nothing",
            ratio(panes[0])
        );

        for (idx, pane) in panes.iter().enumerate() {
            let (horizontal, vertical) = h.color_scale_strips(*pane);
            assert!(
                vertical > 0,
                "pane {idx}: the landscape *panel* decides, so the bar belongs \
                 on the right edge — painted none there"
            );
            assert_eq!(
                horizontal, 0,
                "pane {idx}: painted a bottom bar, i.e. the axis was taken from \
                 the pane's own shape"
            );
        }
    }

    /// 7. A tap that lands on a floating dialog is filtered out by the
    ///    dialog-blocking gate — for both the mouse and the touch path.
    #[test]
    fn tap_on_floating_dialog_is_filtered_out() {
        let mut h = InputHarness::new();
        h.gui_mut().show_settings = true;
        h.warm_up();

        let pos = h.screen_center();
        assert!(
            h.is_floating_layer_at(pos),
            "precondition: the settings dialog must cover the viewport centre"
        );
        assert!(
            h.map_center().distance(pos) < 200.0,
            "precondition: the dialog sits over the map pane, so only the \
             dialog gate can filter this click"
        );

        // Mouse path: egui reports the click, the gate drops it.
        let clicked = h.mouse_click(pos);
        assert_eq!(clicked.mouse.overlay_click_pos, None);
        assert!(!clicked.mouse.suppress_pan);

        // Touch path: the deferred tap is dropped as well, and nothing is
        // emitted once the double-tap window closes. (Note this half is caught
        // earlier, by the on-floating-UI check inside DoubleTapDragDetector —
        // `tap_confirmed_under_a_dialog_is_filtered_out` covers the gate
        // itself.)
        let tapped = h.touch_tap(pos);
        assert_eq!(tapped.touch.overlay_click_pos, None);
        let settled = h.frames_for(3, 0.3);
        assert_eq!(settled.touch.overlay_click_pos, None);

        // Sanity: with the dialog closed, the same position is clickable again.
        h.gui_mut().show_settings = false;
        h.warm_up();
        assert!(!h.is_floating_layer_at(pos));
        let clicked = h.mouse_click(pos);
        assert_eq!(clicked.mouse.overlay_click_pos, Some(pos));
    }

    /// 7b. A touch tap is deferred by 0.4s, so a dialog can open *during* the
    ///     deferral. The tap was legitimately on the map when it happened, so
    ///     the detector's own on-release check passes it through, and only
    ///     `filter_dialog_blocked` can stop it from punching through the dialog
    ///     that is now covering it.
    #[test]
    fn tap_confirmed_under_a_dialog_is_filtered_out() {
        let mut h = InputHarness::new();
        let pos = h.map_center();

        // Tap on the bare map: nothing is floating there yet.
        assert!(!h.is_floating_layer_at(pos));
        let tapped = h.touch_tap(pos);
        assert_eq!(tapped.touch.overlay_click_pos, None, "still deferred");

        // A dialog opens over the tap position before the window closes.
        h.gui_mut().show_settings = true;
        h.frame_after(FRAME_DT);
        assert!(
            h.is_floating_layer_at(pos),
            "precondition: the dialog now covers the tapped point"
        );

        let confirmed = h.frame_after(AFTER_DOUBLE_TAP_TIMEOUT);
        assert_eq!(
            confirmed.touch.overlay_click_pos, None,
            "a tap confirmed under a dialog must not reach the map"
        );
        let settled = h.frames_for(3, 0.3);
        assert_eq!(settled.touch.overlay_click_pos, None);

        // Sanity: the identical sequence without the dialog does deliver the
        // tap, so the assertion above is about the gate and not about the tap
        // being swallowed somewhere else.
        h.gui_mut().show_settings = false;
        h.warm_up();
        h.touch_tap(pos);
        let confirmed = h.frame_after(AFTER_DOUBLE_TAP_TIMEOUT);
        assert_eq!(confirmed.touch.overlay_click_pos, Some(pos));
    }

    // ── The modality gate ────────────────────────────────────────────────
    //
    // These are the only tests that read `FrameOutcome::resolved`. The `mouse`
    // and `touch` fields deliberately bypass the gate, so asserting on them
    // here would prove nothing about it.

    /// 9. **A slow mouse press is not a long press.**
    ///
    ///    `LongPressDetector` keys purely on "primary down for 0.8s", so under
    ///    a mouse it fires on an ordinary slow click — and because a long press
    ///    raises `suppress_pan`, it takes the drag away from the map. Every map
    ///    pan starts with the button going down and staying down, so an ungated
    ///    detector breaks mouse panning outright.
    ///
    ///    The `touch` assertion is the contrast that stops this being vacuous:
    ///    the identical input *does* drive the detector when it is not gated,
    ///    so what the test observes is the gate and not a dead detector.
    #[test]
    fn a_slow_mouse_press_never_becomes_a_long_press() {
        let mut h = InputHarness::new();
        let pos = h.map_center();

        h.mouse_press(pos);
        let held = {
            h.frame_after(FRAME_DT);
            h.frames_for(10, 0.1)
        };

        assert_eq!(
            held.modality,
            PointerModality::Mouse,
            "precondition: mouse events must have latched the mouse modality"
        );
        assert_eq!(
            held.touch.long_press_pos,
            Some(pos),
            "precondition: ungated, this input really does trip the detector — \
             otherwise the assertion below is satisfied by nothing happening"
        );

        assert_eq!(
            held.resolved.long_press_pos, None,
            "the gate must keep the long-press detector off a mouse"
        );
        assert!(
            !held.resolved.suppress_pan,
            "a held mouse button must still pan the map"
        );
    }

    /// 10. **A mouse click is not deferred.**
    ///
    ///     The touch path withholds every tap for `DOUBLE_TAP_TIMEOUT_S` so a
    ///     double-tap can claim it. Under a mouse that is 400ms of latency on
    ///     every overlay click, for a gesture a mouse cannot even perform.
    #[test]
    fn a_mouse_click_reports_immediately_rather_than_after_the_tap_window() {
        let mut h = InputHarness::new();
        let pos = h.map_center();

        let clicked = h.mouse_click(pos);
        assert_eq!(clicked.modality, PointerModality::Mouse);
        assert_eq!(
            clicked.resolved.overlay_click_pos,
            Some(pos),
            "the click must land on the frame it happened"
        );
        assert_eq!(
            clicked.touch.overlay_click_pos, None,
            "precondition: the touch pipeline would still be deferring it, so \
             the assertion above is about the gate"
        );
    }

    /// 10b. The touch path keeps its deferral, so the test above is a statement
    ///      about the modality and not about the deferral having been deleted.
    #[test]
    fn a_real_touch_tap_is_still_deferred_through_the_gate() {
        let mut h = InputHarness::new();
        let pos = h.map_center();

        let tapped = h.touch_tap(pos);
        assert_eq!(
            tapped.modality,
            PointerModality::Touch,
            "precondition: touch events latch the touch modality"
        );
        assert_eq!(tapped.resolved.overlay_click_pos, None, "still deferred");

        let confirmed = h.frame_after(AFTER_DOUBLE_TAP_TIMEOUT);
        assert_eq!(confirmed.resolved.overlay_click_pos, Some(pos));
    }

    /// 11. **A mouse double-click does not enter a zoom drag.**
    ///
    ///     Double-clicking is an ordinary thing to do with a mouse.
    ///     `DoubleTapDragDetector` would read it as the opening of a
    ///     double-tap-drag and start scrubbing the zoom with vertical motion.
    #[test]
    fn a_mouse_double_click_does_not_start_a_zoom_drag() {
        let mut h = InputHarness::new();
        let pos = h.map_center();

        let before = h.frame_after(FRAME_DT).resolved_zoom;

        // Two clicks well inside the double-tap window, then drag downwards
        // while still held — the exact shape of the touch zoom gesture.
        h.mouse_click(pos);
        h.mouse_press(pos);
        h.frame_after(0.05);
        h.mouse_move(pos + egui::vec2(0.0, 150.0));
        let dragged = h.frame_after(FRAME_DT);

        assert_eq!(dragged.modality, PointerModality::Mouse);
        assert_eq!(
            dragged.resolved_zoom, before,
            "a mouse double-click-drag must not scrub the map zoom"
        );
        assert!(
            !dragged.resolved.suppress_pan,
            "and it must leave panning to the map"
        );
    }

    /// 11b. The same gesture on the ungated touch path *does* zoom, so the test
    ///      above is not simply asserting that the gesture never works.
    #[test]
    fn the_same_drag_does_zoom_when_it_really_is_a_touch() {
        let mut h = InputHarness::new();
        let pos = h.map_center();

        let before = h.frame_after(FRAME_DT).resolved_zoom;

        h.touch_tap(pos);
        h.touch_start(pos);
        h.frame_after(0.05);
        h.touch_move(pos + egui::vec2(0.0, 150.0));
        let dragged = h.frame_after(FRAME_DT);

        assert_eq!(dragged.modality, PointerModality::Touch);
        assert_ne!(
            dragged.resolved_zoom, before,
            "the touch gesture must reach the map through the gate"
        );
        assert!(dragged.resolved.suppress_pan, "the zoom drag owns the pointer");
    }

    /// 12. **A gesture interrupted by a modality change is abandoned, and stays
    ///     abandoned when the modality comes back.**
    ///
    ///     A tap waiting for its double-tap partner is state held *inside* the
    ///     detector. Merely switching to a mouse hides it, because the mouse
    ///     branch never polls the detector at all — so the interesting case is
    ///     the round trip. Without an explicit reset the pending tap is still
    ///     sitting there when touch resumes, its 0.4s window long since
    ///     elapsed, and the very next touch frame promotes it: an overlay click
    ///     fires at a stale position the user last touched minutes ago.
    ///
    ///     Asserting only on the mouse leg would be satisfied by the branch
    ///     structure alone and would prove nothing about the reset.
    #[test]
    fn a_touch_gesture_interrupted_by_a_mouse_does_not_resume_when_touch_returns() {
        let mut h = InputHarness::new();
        let stale = h.map_center();

        let tapped = h.touch_tap(stale);
        assert_eq!(tapped.modality, PointerModality::Touch);
        assert_eq!(
            tapped.resolved.overlay_click_pos, None,
            "precondition: the tap is pending, not yet confirmed"
        );

        // The user picks up a mouse, somewhere else entirely.
        let elsewhere = stale + egui::vec2(200.0, 0.0);
        h.mouse_move(elsewhere);
        let switched = h.frame_after(FRAME_DT);
        assert_eq!(switched.modality, PointerModality::Mouse);
        assert_eq!(
            switched.resolved.overlay_click_pos, None,
            "nothing should fire while the mouse is in charge"
        );

        // Well past the double-tap window, so a surviving pending tap is now
        // eligible for promotion the moment the detector is polled again.
        h.frames_for(5, 0.2);

        // The finger comes back. This is the frame that would resurrect it.
        h.touch_start(elsewhere);
        let resumed = h.frame_after(FRAME_DT);
        assert_eq!(
            resumed.modality,
            PointerModality::Touch,
            "precondition: touch is driving again, so the detector is polled"
        );
        assert_eq!(
            resumed.resolved.overlay_click_pos, None,
            "the stale tap must not be promoted when touch resumes"
        );

        let settled = h.frames_for(4, 0.2);
        assert_eq!(
            settled.resolved.overlay_click_pos, None,
            "and it must not surface on any later frame either"
        );
    }

    /// 13. **Only the active pane sees a touch; every pane sees the mouse.**
    ///
    ///     The touch pipeline is single-pointer and stateful, so running it for
    ///     more than one pane would mean several detectors racing over one
    ///     finger. The mouse carries no such state, and resolving it for every
    ///     pane is what lets a click land on an overlay in a pane that is not
    ///     yet the active one — behaviour the desktop build always had.
    ///
    ///     Split into two real panes, because an inactive pane is not a thing
    ///     that exists in a one-pane layout: `render_map` would resolve exactly
    ///     one pane and there would be nothing to compare it against.
    #[test]
    fn a_touch_reaches_only_the_active_pane_but_a_click_reaches_them_all() {
        let mut h = InputHarness::new();
        h.set_pane_count(2);
        let pos = h.pane_rects()[0].center();
        assert!(
            h.pane_rects().len() == 2 && !h.pane_rects()[1].contains(pos),
            "precondition: two distinct panes, and the click lands in pane 0"
        );

        let clicked = h.mouse_click(pos);
        assert_eq!(clicked.modality, PointerModality::Mouse);
        assert_eq!(
            clicked.resolved.overlay_click_pos,
            Some(pos),
            "precondition: the active pane got the click"
        );
        assert_eq!(
            clicked.resolved_inactive.map(|f| f.overlay_click_pos),
            Some(Some(pos)),
            "a mouse click is resolved for every pane, not just the active one"
        );

        let mut h = InputHarness::new();
        h.set_pane_count(2);

        // The release frame is the one that separates the two branches: a
        // touch release carries the synthetic `PointerButton{up}` that makes
        // egui report a click, so the mouse path *would* return a position
        // here. A later, event-free frame would let both branches agree on
        // `None` and prove nothing.
        let tapped = h.touch_tap(pos);
        assert_eq!(tapped.modality, PointerModality::Touch);
        assert_eq!(
            tapped.mouse.overlay_click_pos,
            Some(pos),
            "precondition: on this frame the mouse path does resolve a click, \
             so `None` below is the touch branch and not an empty frame"
        );
        assert_eq!(
            tapped.resolved_inactive.map(|f| f.overlay_click_pos),
            Some(None),
            "an inactive pane takes no part in a touch gesture"
        );

        // ...and the active pane still gets it, once the deferral elapses.
        let confirmed = h.frame_after(AFTER_DOUBLE_TAP_TIMEOUT);
        assert_eq!(
            confirmed.resolved.overlay_click_pos,
            Some(pos),
            "the tap was deferred, not swallowed"
        );
    }

    // ── Responsive layout ────────────────────────────────────────────────

    /// 14. **Crossing a breakpoint must not move any widget's egui `Id`.**
    ///
    ///     egui keys widget memory — combo open state, scroll offsets, panel
    ///     sizes — on `Id`. An `Id` derived from anything layout-dependent
    ///     therefore looks like a *different widget* on the other side of a
    ///     resize, and every one of those becomes a silent reset: the user
    ///     drags a window edge and their scroll position jumps to the top.
    ///
    ///     This compares the `Id`s the panel actually resolved on two runs
    ///     rather than restating the constants that produced them, so it fails
    ///     for a layout-keyed `Id` however that keying was introduced.
    #[test]
    fn crossing_a_breakpoint_does_not_move_any_widget_id() {
        let mut h = InputHarness::with_screen(egui::vec2(1200.0, 800.0));
        // The drawer is what shows the panel below the sidebar breakpoint;
        // opening it up front means the panel is on screen for both runs.
        h.set_drawer_open(true);

        assert_eq!(
            h.width_class(),
            crate::ui_layout::WidthClass::Expanded,
            "precondition: start above the sidebar breakpoint"
        );
        let expanded = h.widget_id_probes();
        assert!(
            !expanded.is_empty(),
            "precondition: the panel must have reported some ids, or this test \
             compares two empty lists and passes for free"
        );

        // Every probed id must be one egui actually knows. Without this a
        // probe reporting a constant — or an id rebuilt from a format string
        // the widget itself no longer uses — would compare equal to itself on
        // both sides of the resize and prove nothing at all.
        let combo_id = expanded
            .iter()
            .find(|(name, _)| *name == "time_step_sel")
            .expect("precondition: the time step combo must report an id")
            .1;
        assert!(
            h.widget_exists(combo_id),
            "the time_step_sel probe reported an id egui has no widget for, so \
             it is a reconstruction rather than the combo box's own"
        );

        // Put real egui state behind one of those ids, so the comparison below
        // is backed by something that would visibly be lost. Reading it through
        // the probed id also pins that the panel really does key its scroll
        // area on that id rather than on a positional auto-id.
        let scroll_id = expanded
            .iter()
            .find(|(name, _)| *name == "layers_scroll")
            .expect("precondition: the scroll area must report an id")
            .1;
        h.scroll_at(egui::pos2(80.0, 400.0), egui::vec2(0.0, -120.0));
        h.frames_for(3, FRAME_DT);
        let scrolled = h.scroll_offset(scroll_id);
        assert!(
            scrolled.is_some_and(|o| o.y > 0.0),
            "precondition: the layers panel must have actually scrolled under \
             the probed id, got {scrolled:?}"
        );

        h.set_screen(egui::vec2(800.0, 800.0));
        h.set_drawer_open(true);
        assert_eq!(
            h.width_class(),
            crate::ui_layout::WidthClass::Medium,
            "precondition: the resize really did cross the 1000pt breakpoint"
        );
        let medium = h.widget_id_probes();

        assert_eq!(
            expanded, medium,
            "a widget id moved with the layout: everything egui remembers under \
             it — scroll offset, combo state — is silently discarded on resize"
        );
        assert_eq!(
            h.scroll_offset(scroll_id),
            scrolled,
            "the scroll position must survive the resize"
        );

        // ...and across the 600pt breakpoint too, where the menu bar goes away
        // and the panel header is the only chrome left above the controls.
        h.set_screen(egui::vec2(500.0, 800.0));
        h.set_drawer_open(true);
        assert_eq!(
            h.width_class(),
            crate::ui_layout::WidthClass::Compact,
            "precondition: the resize crossed the 600pt breakpoint"
        );
        assert_eq!(
            expanded,
            h.widget_id_probes(),
            "the compact layout must reuse the same ids as well"
        );
    }

    /// 15. **The hamburger's rect is reported by the code that draws it.**
    ///
    ///     A tap on the button must not also be a tap on the map underneath.
    ///     `ui_map.rs` used to rebuild this rect from its own copy of the
    ///     position constants, which could disagree with the button silently.
    #[test]
    fn the_hamburger_excludes_its_own_rect_from_map_clicks() {
        let mut h = InputHarness::with_screen(egui::vec2(500.0, 800.0));
        assert_eq!(
            h.width_class(),
            crate::ui_layout::WidthClass::Compact,
            "precondition: a compact screen has a hamburger and no sidebar"
        );

        let rects = h.excluded_rects();
        assert_eq!(
            rects.len(),
            1,
            "precondition: exactly the hamburger should be excluded, got {rects:?}"
        );
        let button = rects[0];

        // The rect must be where the button was actually painted, not merely
        // non-empty: a stale copy of the constants would still produce a rect.
        assert!(
            h.painted_rects().iter().any(|r| {
                (r.center() - button.center()).length() < 1.0 && r.width() == button.width()
            }),
            "the excluded rect does not match any painted rect — it was \
             reconstructed rather than reported"
        );

        // The chrome reporting a rect is only half of it: the map has to be
        // handed it. `render_map(&mut root_ui, &[])` leaves every assertion
        // above true and the button transparent to clicks.
        assert_eq!(
            h.map_excluded_rects(),
            rects,
            "the chrome's rects never reached render_map, so nothing downstream \
             can exclude the button from a map click"
        );

        // ...and the button is on a layer above the map, which is the *other*
        // half of `is_pos_blocked`. Each half masks the other: with only one
        // mutated the tap is still caught, and with both it falls through to
        // the map. So each is claimed separately.
        assert!(
            h.is_floating_layer_at(button.center()),
            "the hamburger dropped to a background layer: `is_pos_blocked`'s \
             layer check no longer sees it"
        );

        // With the drawer open the button is gone, and so is its exclusion.
        h.set_drawer_open(true);
        assert!(
            h.excluded_rects().is_empty(),
            "an open drawer replaces the button, so nothing should be excluded"
        );
        assert!(
            h.map_excluded_rects().is_empty(),
            "and the map must be told so too"
        );
    }

    /// 16b. **The map keeps usable space at every breakpoint.**
    ///
    ///      Panels claim space in call order and the map gets the remainder, so
    ///      chrome that is too greedy — or ordered wrongly — squeezes the map
    ///      toward zero. That rect feeds pane hit-testing, `excluded_rects` and
    ///      overlay texture sizing, so a degenerate one is silent everywhere
    ///      rather than obviously broken in one place.
    #[test]
    fn the_map_keeps_usable_space_at_every_breakpoint() {
        for (size, expected) in [
            (egui::vec2(420.0, 800.0), crate::ui_layout::WidthClass::Compact),
            (egui::vec2(800.0, 800.0), crate::ui_layout::WidthClass::Medium),
            (egui::vec2(1400.0, 900.0), crate::ui_layout::WidthClass::Expanded),
        ] {
            let mut h = InputHarness::with_screen(size);
            assert_eq!(
                h.width_class(),
                expected,
                "precondition: {size:?} should be {expected:?}"
            );

            for drawer in [false, true] {
                h.set_drawer_open(drawer);
                let panel = h.map_panel_rect();
                assert!(
                    panel.width() > 100.0 && panel.height() > 100.0,
                    "{expected:?} (drawer_open={drawer}): the map was squeezed to \
                     {:?} x {:?} — the chrome claimed nearly everything",
                    panel.width(),
                    panel.height()
                );
                // The status bar is present at every width, so the map never
                // gets the full height. This is what stops the bounds above
                // passing on a frame where no chrome rendered at all.
                assert!(
                    panel.height() < size.y,
                    "{expected:?} (drawer_open={drawer}): the map got the full \
                     height, so the status bar claimed nothing"
                );

                // A left panel is showing exactly when the sidebar is
                // persistent or the drawer is open; only then is the map
                // narrower than the screen.
                let has_left_panel = expected == crate::ui_layout::WidthClass::Expanded || drawer;
                assert_eq!(
                    panel.width() < size.x,
                    has_left_panel,
                    "{expected:?} (drawer_open={drawer}): the map's width does \
                     not agree with whether a left panel should be showing"
                );
            }
        }
    }

    // ── The menu, in whichever presentation is on screen ─────────────────

    /// A compact harness with the drawer open — the phone layout, and the only
    /// width that renders the menu as a drawer list.
    ///
    /// Tall rather than phone-shaped on purpose: the drawer's menu sits below
    /// the layer controls inside a `ScrollArea`, so on an 800pt screen it lays
    /// out past the bottom edge and a synthetic click at its rect would land on
    /// nothing at all — passing every "the overlay did not change" assertion
    /// for the wrong reason. Only the *width* decides the presentation.
    fn compact_with_drawer() -> InputHarness {
        let mut h = InputHarness::with_screen(egui::vec2(420.0, 1200.0));
        assert_eq!(
            h.width_class(),
            crate::ui_layout::WidthClass::Compact,
            "precondition: the drawer presentation only exists below 600pt"
        );
        h.set_drawer_open(true);
        h
    }

    /// The rect of a drawn menu leaf, checked to be somewhere a click can
    /// actually reach it.
    fn clickable_leaf(h: &InputHarness, label: &str) -> egui::Rect {
        let leaf = h
            .menu_leaf(label)
            .unwrap_or_else(|| panic!("the menu did not draw {label:?}"));
        assert!(
            h.screen_rect().contains(leaf.rect.center()),
            "{label:?} was laid out at {:?}, outside the {:?} viewport — a \
             click there hits nothing and would pass for the wrong reason",
            leaf.rect,
            h.screen_rect()
        );
        leaf.rect
    }

    /// 17. **The drawer's checkboxes show the live pane's state.**
    ///
    ///     Building the model inside the panel closure handed every overlay
    ///     toggle the `mem::take`n pane's empty `enabled_overlays`, so the box
    ///     rendered unchecked and each click emitted `Toggled(kind, true)`.
    ///     Auto-poll escaped it by living on `self`, which is why the model's
    ///     own unit tests stayed green.
    #[test]
    fn the_drawer_checkboxes_show_the_live_pane_not_a_default_one() {
        let mut h = compact_with_drawer();
        h.gui_mut().enable_overlay_for_test(OverlayKind::RadarSites);
        h.warm_up();

        assert!(
            h.overlay_enabled(OverlayKind::RadarSites),
            "precondition: the live pane must really have the overlay on"
        );

        let drawn = h.menu_leaf("Show radar sites").expect(
            "precondition: the compact drawer must draw the overlay toggles, \
             or there is no checkbox to be wrong about",
        );
        assert_eq!(
            drawn.value,
            Some(true),
            "the drawer drew the checkbox from a default pane, not the live \
             one: it renders unchecked and every click turns the overlay *on*",
        );

        // Auto-poll is the control that never broke — asserting it alone would
        // have passed throughout, so it is the contrast, not the claim.
        assert_eq!(
            h.menu_leaf("Auto-poll").map(|l| l.value),
            Some(Some(true)),
            "precondition: auto-poll defaults on and reads off `self`, so it \
             was never affected by the pane being taken"
        );
    }

    /// 18. **A checkbox in the drawer turns the overlay off, and it stays off.**
    ///
    ///     The watched frames are the point: `apply_menu_event` used to write
    ///     `enabled_overlays` only, and the layers panel reloaded the config
    ///     over it on the next frame. Asserting straight after the click
    ///     passes; the user, who sees the frame after, never got the change.
    ///     Also pins that `render_menu_drawer`'s events reach the dispatcher.
    #[test]
    fn clicking_a_drawer_checkbox_toggles_the_overlay_both_ways() {
        let mut h = compact_with_drawer();
        h.gui_mut().enable_overlay_for_test(OverlayKind::RadarSites);
        h.warm_up();
        assert!(h.overlay_enabled(OverlayKind::RadarSites), "precondition");

        h.mouse_click(clickable_leaf(&h, "Show radar sites").center());
        assert!(
            !h.overlay_enabled(OverlayKind::RadarSites),
            "clicking a checked box left the overlay on — the drawer can turn \
             an overlay on but never off"
        );

        // On the click frame itself the probe must report the state the
        // checkbox was *handed*, not the one the click produced. Recording
        // egui's post-click `current` instead would make a checkbox that
        // renders stale look correct on exactly the frame that matters.
        assert_eq!(
            h.menu_leaf("Show radar sites").map(|l| l.value),
            Some(Some(true)),
            "the probe recorded the post-click value, so it can no longer show \
             a checkbox being drawn from the wrong state"
        );
        for frame in 0..5 {
            h.frame_after(FRAME_DT);
            assert!(
                !h.overlay_enabled(OverlayKind::RadarSites),
                "the overlay came back on {} frame(s) after the click: the \
                 toggle reached `enabled_overlays` but not `overlay_configs`, \
                 so the layers panel reloaded it from the config and undid it",
                frame + 1
            );
        }

        // ...and back on, so this cannot pass by the click being read as an
        // unconditional "off".
        h.mouse_click(clickable_leaf(&h, "Show radar sites").center());
        h.frames_for(5, FRAME_DT);
        assert!(
            h.overlay_enabled(OverlayKind::RadarSites),
            "the toggle did not come back on"
        );

        // The checkbox on screen now agrees with the pane again — the two
        // halves of the round trip, not just the state behind it.
        assert_eq!(
            h.menu_leaf("Show radar sites").map(|l| l.value),
            Some(Some(true)),
            "the pane is on but the drawer still draws the box unchecked"
        );
    }

    /// A compact drawer harness split into two panes, with pane 1 made active
    /// the way a user does it — by tapping that pane on the map.
    fn compact_drawer_with_pane_1_active() -> InputHarness {
        let mut h = InputHarness::with_screen(egui::vec2(420.0, 1200.0));
        h.set_pane_count(2);

        // Tap pane 1 with the drawer shut, so the map has the full width and
        // the two pane rects are unambiguous.
        let target = h.pane_rects()[1].center();
        h.mouse_click(target);
        h.warm_up();
        assert_eq!(
            h.active_pane_index(),
            1,
            "precondition: tapping pane 1 must make it active, or this fixture \
             is testing pane 0 twice"
        );

        h.set_drawer_open(true);
        h
    }

    /// 27. **"The live active pane" means the active one, not pane 0.**
    ///
    ///     With `active_pane` stuck at 0 in every fixture, both `menu_model`
    ///     reading `&self.panes[0]` and `set_active_pane_overlay` writing it
    ///     survived. In the app: pane 1 active, tap a toggle in the drawer, the
    ///     overlay lands on pane 0. Sync is off so the panes can disagree —
    ///     with it on it copies the write back and hides the bug.
    #[test]
    fn the_menu_reads_and_writes_the_active_pane_not_pane_zero() {
        let mut h = compact_drawer_with_pane_1_active();
        h.set_sync_layers(false);

        // The panes must disagree about **two** kinds, not one.
        //
        // `RadarSites` is the kind being toggled, and `set_enabled` overwrites
        // it whichever config was loaded — so on its own it cannot show the
        // *read* going to the wrong pane. `CityLabels` is the witness:
        // `serialize_state` carries `enabled`, so loading pane 0's configs
        // imports pane 0's on/off state for every kind except the one being
        // set, and pane 1's city labels would silently go out.
        h.set_overlay_on_pane(0, OverlayKind::RadarSites, false);
        h.set_overlay_on_pane(0, OverlayKind::CityLabels, false);
        h.set_overlay_on_pane(1, OverlayKind::RadarSites, true);
        h.set_overlay_on_pane(1, OverlayKind::CityLabels, true);
        h.warm_up();
        assert!(
            h.overlay_enabled_on(1, OverlayKind::RadarSites)
                && !h.overlay_enabled_on(0, OverlayKind::RadarSites)
                && h.overlay_enabled_on(1, OverlayKind::CityLabels)
                && !h.overlay_enabled_on(0, OverlayKind::CityLabels),
            "precondition: the panes must disagree about both kinds"
        );

        // The checkbox must show pane 1's state, not pane 0's.
        assert_eq!(
            h.menu_leaf("Show radar sites").map(|l| l.value),
            Some(Some(true)),
            "the drawer drew pane 0's state while pane 1 is active"
        );

        // ...and clicking it must write to pane 1, leaving pane 0 alone.
        h.mouse_click(clickable_leaf(&h, "Show radar sites").center());
        h.frames_for(5, FRAME_DT);
        assert!(
            !h.overlay_enabled_on(1, OverlayKind::RadarSites),
            "the toggle did not reach the active pane"
        );
        assert!(
            !h.overlay_enabled_on(0, OverlayKind::RadarSites),
            "the toggle wrote to pane 0, which is not the active pane"
        );

        // The untouched kind must be untouched — on the active pane, and on
        // the one that was not being edited.
        assert!(
            h.overlay_enabled_on(1, OverlayKind::CityLabels),
            "toggling radar sites on pane 1 also turned its city labels off: \
             the config was read from pane 0, which had them off"
        );
        assert!(
            !h.overlay_enabled_on(0, OverlayKind::CityLabels),
            "pane 0's city labels changed, though it is not the active pane"
        );
    }

    /// 29. **A menu toggle saves the active pane's *own* overlay config.**
    ///
    ///     `render_pane_map_content` loads each pane's config as it draws it,
    ///     so mid-frame the handlers hold the last-drawn pane's settings.
    ///     `set_active_pane_overlay` then snapshots the handlers onto the
    ///     active pane — and `serialize_state` carries `enabled`, so a
    ///     snapshot taken against the wrong pane's config silently rewrites
    ///     every *other* overlay kind's on/off flag on the active pane.
    ///
    ///     Two separate things keep the handlers correct at that moment: the
    ///     reload at the end of `Gui::ui`, and the load at the top of
    ///     `set_active_pane_overlay`. Either alone is sufficient, so **neither
    ///     is individually killable** — removing just one is an equivalent
    ///     mutant. Removing both fails here, and only here.
    ///
    ///     Medium with the drawer shut: anywhere the layers panel is on screen
    ///     it reloads the active pane's config every frame and hides this.
    #[test]
    fn a_menu_toggle_loads_the_active_panes_config_before_saving_it() {
        let mut h = InputHarness::with_screen(egui::vec2(800.0, 900.0));
        assert_eq!(h.width_class(), crate::ui_layout::WidthClass::Medium);
        h.set_pane_count(2);
        h.set_sync_layers(false);
        assert_eq!(
            h.active_pane_index(),
            0,
            "precondition: pane 0 active, so the *last drawn* pane 1 is the one \
             whose config is left in the handlers"
        );

        h.set_overlay_on_pane(0, OverlayKind::CityLabels, true);
        h.set_overlay_on_pane(1, OverlayKind::CityLabels, false);
        h.set_overlay_on_pane(0, OverlayKind::RadarSites, false);
        h.warm_up();
        assert!(
            h.pane_options().is_empty(),
            "precondition: no layers panel, or its reload masks this"
        );

        h.mouse_click(clickable_leaf(&h, "View").center());
        h.frames_for(2, FRAME_DT);
        h.mouse_click(clickable_leaf(&h, "Show radar sites").center());
        h.frames_for(5, FRAME_DT);

        assert!(
            h.overlay_enabled_on(0, OverlayKind::RadarSites),
            "precondition: the toggle must have taken effect"
        );
        assert!(
            h.overlay_enabled_on(0, OverlayKind::CityLabels),
            "the active pane's city labels were overwritten by pane 1's config: \
             the handlers were saved without loading the active pane first"
        );
    }

    /// 28. **A menu toggle propagates to the other panes when sync is on.**
    ///
    ///     Driven on Medium with the drawer shut — the only layout where the
    ///     menu is on screen and the layers panel is not. Anywhere else
    ///     `render_layers_panel` calls `propagate_layer_sync` itself every
    ///     frame and masks the arm: a compact-drawer version of this test
    ///     passes with the call deleted.
    #[test]
    fn a_menu_toggle_propagates_to_the_other_panes_when_sync_is_on() {
        let mut h = InputHarness::with_screen(egui::vec2(800.0, 900.0));
        assert_eq!(
            h.width_class(),
            crate::ui_layout::WidthClass::Medium,
            "precondition: Medium is menubar-yes, sidebar-no"
        );
        h.set_pane_count(2);
        h.mouse_click(h.pane_rects()[1].center());
        h.warm_up();
        assert_eq!(h.active_pane_index(), 1, "precondition: pane 1 is active");

        assert!(h.sync_layers(), "precondition: layer sync is on by default");
        assert!(
            h.pane_options().is_empty(),
            "precondition: the layers panel must NOT be on screen, or its own \
             `propagate_layer_sync` masks the arm under test"
        );

        h.set_overlay_on_pane(0, OverlayKind::RadarSites, false);
        h.set_overlay_on_pane(1, OverlayKind::RadarSites, false);
        h.warm_up();

        // Through the menu bar: open "View", then tick the box.
        h.mouse_click(clickable_leaf(&h, "View").center());
        h.frames_for(2, FRAME_DT);
        h.mouse_click(clickable_leaf(&h, "Show radar sites").center());
        h.frames_for(5, FRAME_DT);

        assert!(
            h.overlay_enabled_on(1, OverlayKind::RadarSites),
            "precondition: the active pane must have taken the toggle"
        );
        assert!(
            h.overlay_enabled_on(0, OverlayKind::RadarSites),
            "the toggle did not propagate to the other pane, though layer sync \
             is on"
        );
    }

    /// 19. **The compact drawer carries the whole menu.**
    ///
    ///     Below 600pt there is no menu bar, so the drawer is the only route to
    ///     Settings, Time, Exit, Refresh and every toggle. Disconnecting it —
    ///     `show_menu_in_panel = false`, or a renderer that draws nothing —
    ///     strands all of them behind nothing at all.
    #[test]
    fn the_compact_drawer_is_the_only_route_to_the_whole_menu() {
        let h = compact_with_drawer();
        let labels: Vec<&str> = h.menu_leaves().iter().map(|l| l.label).collect();

        for wanted in [
            "Refresh Radar",
            "Exit",
            "Show radar sites",
            "Show city labels",
            "Auto-poll",
            "Time...",
            "Settings...",
        ] {
            assert!(
                labels.contains(&wanted),
                "compact has no menu bar, so {wanted:?} is unreachable — drew {labels:?}"
            );
        }
    }

    /// 20. **Invoking a command from the drawer really dispatches it.**
    ///
    ///     A click on "Exit" has to become a `GuiAction::Exit`. `Exit` and
    ///     `RefreshRadar` dispatch to a one-line arm, so a test that only walks
    ///     the model and calls `apply_menu_event` proves nothing about them:
    ///     an exhaustive `match` already guarantees the arm exists.
    #[test]
    fn a_command_invoked_from_the_drawer_reaches_the_dispatcher() {
        let mut h = compact_with_drawer();
        let exit = clickable_leaf(&h, "Exit");

        h.mouse_click(exit.center());
        assert!(
            h.last_actions()
                .iter()
                .any(|a| matches!(a, crate::actions::GuiAction::Exit)),
            "clicking Exit in the drawer emitted no Exit action ({} actions in all)",
            h.last_actions().len()
        );
    }

    /// 21. **The menu bar's events reach the dispatcher too.**
    ///
    ///     The other presentation, driven the way a user drives it: click the
    ///     "View" header to open the drop-down, then click the checkbox inside
    ///     it. Nothing here reaches into egui's menu memory.
    #[test]
    fn a_toggle_flipped_in_the_menu_bar_reaches_the_dispatcher() {
        let mut h = InputHarness::with_screen(egui::vec2(1200.0, 800.0));
        assert_eq!(
            h.width_class(),
            crate::ui_layout::WidthClass::Expanded,
            "precondition: a menu bar needs 600pt or more"
        );
        h.gui_mut().enable_overlay_for_test(OverlayKind::RadarSites);
        h.warm_up();
        assert!(h.overlay_enabled(OverlayKind::RadarSites), "precondition");

        let view = clickable_leaf(&h, "View");
        h.mouse_click(view.center());
        h.frames_for(2, FRAME_DT);

        assert_eq!(
            h.menu_leaf("Show radar sites").map(|l| l.value),
            Some(Some(true)),
            "the open drop-down must draw the toggle, from the live pane"
        );

        h.mouse_click(clickable_leaf(&h, "Show radar sites").center());
        h.frames_for(5, FRAME_DT);
        assert!(
            !h.overlay_enabled(OverlayKind::RadarSites),
            "the menu bar's toggle never reached apply_menu_event, or was \
             reverted by the layers panel on a later frame"
        );
    }

    /// 22. **The pane picker narrows on a phone; the config clamp does not.**
    ///
    ///     The two limits differ deliberately and each has to be read by the
    ///     right code. The values were pinned as constants, but nothing checked
    ///     the picker consulted the width class at all. The other half — a wide
    ///     layout surviving a load on a phone — is pinned in `ui_config.rs`.
    #[test]
    fn the_pane_picker_offers_fewer_panes_on_a_phone_than_on_a_desktop() {
        use crate::pane::{MAX_PANES_DESKTOP, MAX_PANES_MOBILE};

        let mut compact = InputHarness::with_screen(egui::vec2(420.0, 1200.0));
        compact.set_drawer_open(true);
        assert_eq!(
            compact.width_class(),
            crate::ui_layout::WidthClass::Compact,
            "precondition"
        );
        assert_eq!(
            compact.pane_option_counts(),
            (1..=MAX_PANES_MOBILE).collect::<Vec<_>>(),
            "the picker offered the desktop range on a phone"
        );

        let mut expanded = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
        assert_eq!(
            expanded.width_class(),
            crate::ui_layout::WidthClass::Expanded,
            "precondition"
        );
        assert_eq!(
            expanded.pane_option_counts(),
            (1..=MAX_PANES_DESKTOP).collect::<Vec<_>>(),
            "the picker narrowed a desktop to the phone range"
        );

        // Compared as *rendered* ranges rather than as the two constants, which
        // clippy would fold to `true` — and a precondition that is true by
        // construction is not one.
        assert!(
            compact.pane_option_counts().len() < expanded.pane_option_counts().len(),
            "precondition: the two ranges must differ, or both assertions above \
             are satisfied by one constant"
        );

        // The buttons must be real: exactly one reads as selected, and it is
        // the count actually in force. A probe rebuilt from `max_panes` would
        // agree with the range above while the loop drew nothing.
        let selected: Vec<usize> = expanded
            .pane_options()
            .iter()
            .filter(|o| o.selected)
            .map(|o| o.count)
            .collect();
        assert_eq!(
            selected,
            vec![expanded.pane_count()],
            "the picker's selected button must be the live pane count"
        );

        // ...and clicking one takes effect, which is the half no probe of the
        // *offered range* can reach.
        let three = expanded
            .pane_options()
            .iter()
            .find(|o| o.count == 3)
            .expect("the desktop range must include 3")
            .rect;
        assert_ne!(expanded.pane_count(), 3, "precondition");
        expanded.mouse_click(three.center());
        expanded.warm_up();
        assert_eq!(
            expanded.pane_count(),
            3,
            "clicking a pane-count button did not change the layout"
        );
        assert_eq!(
            expanded.pane_rects().len(),
            3,
            "the map still laid out the old number of panes"
        );
    }

    /// 23. **Host safe-area insets reach the chrome.**
    ///
    ///     `set_safe_area_insets` -> `LayoutCtx::resolve` -> the root `Ui`'s
    ///     rect, which is what insets every nested `Panel`. That last hop was
    ///     untested: dropping `.max_rect(..)` leaves the chrome under the
    ///     status bar, and nothing in the suite ever set an inset.
    #[test]
    fn host_safe_area_insets_inset_the_chrome() {
        const TOP: f32 = 60.0;
        const BOTTOM: f32 = 40.0;
        const LEFT: f32 = 30.0;
        const RIGHT: f32 = 20.0;

        let mut h = InputHarness::with_screen(egui::vec2(1200.0, 900.0));
        let bare = h.map_panel_rect();

        h.set_safe_area_insets(TOP, BOTTOM, LEFT, RIGHT);
        let inset = h.map_panel_rect();

        // The map is what is left after the panels claim their space, so it
        // moves by exactly what the insets took off each edge.
        assert_eq!(inset.left() - bare.left(), LEFT, "left inset ignored");
        assert_eq!(bare.right() - inset.right(), RIGHT, "right inset ignored");
        assert_eq!(inset.top() - bare.top(), TOP, "top inset ignored");
        assert_eq!(bare.bottom() - inset.bottom(), BOTTOM, "bottom inset ignored");

        // The hamburger is positioned from `content_rect` too, so it must have
        // moved clear of the notch rather than staying in the screen corner.
        let mut h = InputHarness::with_screen(egui::vec2(420.0, 1000.0));
        assert_eq!(
            h.width_class(),
            crate::ui_layout::WidthClass::Compact,
            "precondition: only a compact layout draws a hamburger"
        );
        let bare = h.excluded_rects()[0];
        h.set_safe_area_insets(TOP, 0.0, LEFT, 0.0);
        let inset = h.excluded_rects()[0];
        assert_eq!(
            (inset.left() - bare.left(), inset.top() - bare.top()),
            (LEFT, TOP),
            "the hamburger ignored the insets and stayed under the system bars"
        );
    }

    /// 24. **Insets move the breakpoint, not just the padding.**
    ///
    ///     Through `Gui::ui` rather than only on `shrink_to_content`, which is
    ///     what proves `Gui::safe_area_insets` is threaded into the resolve.
    #[test]
    fn host_insets_move_the_breakpoint_through_the_real_ui() {
        let mut h = InputHarness::with_screen(egui::vec2(610.0, 900.0));
        assert_eq!(
            h.width_class(),
            crate::ui_layout::WidthClass::Medium,
            "precondition: 610pt of raw viewport is Medium"
        );

        h.set_safe_area_insets(0.0, 0.0, 20.0, 20.0);
        assert_eq!(
            h.width_class(),
            crate::ui_layout::WidthClass::Compact,
            "570pt of content is Compact: the insets never reached the breakpoint"
        );
    }

    /// 25. **The hover readout follows the pointer, not the window width.**
    ///
    ///     Keying it on `WidthClass` gets both ends wrong: a 500pt desktop
    ///     window loses a readout it can use, a 1400pt tablet gets an empty one.
    #[test]
    fn the_hover_readout_follows_the_modality_not_the_width() {
        // A narrow *desktop* window: compact, but there is a mouse.
        let mut narrow = InputHarness::with_screen(egui::vec2(500.0, 800.0));
        narrow.mouse_click(narrow.map_center());
        assert_eq!(narrow.width_class(), crate::ui_layout::WidthClass::Compact);
        assert!(
            narrow.status_bar().hover,
            "a compact window with a mouse lost its hover readout"
        );

        // A wide *touch* device: roomy, but nothing can hover.
        let mut tablet = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
        tablet.touch_tap(tablet.map_center());
        assert_eq!(tablet.width_class(), crate::ui_layout::WidthClass::Expanded);
        assert!(
            !tablet.status_bar().hover,
            "a touch device was given a hover readout that can never fill in"
        );
    }

    /// 26. **A compact bar drops the long summary and the auto-poll box.**
    ///
    ///     The half left unpinned when one flag became two: inverting `roomy`
    ///     crammed both into a 420pt phone bar and stripped both from a 1400pt
    ///     desktop, suite green. Asserted on the text drawn, not the flag.
    #[test]
    fn a_compact_status_bar_drops_the_long_summary_and_the_auto_poll_box() {
        let mut phone = InputHarness::with_screen(egui::vec2(420.0, 900.0));
        phone.load_scan("KABR");
        assert_eq!(phone.width_class(), crate::ui_layout::WidthClass::Compact);
        let compact_bar = phone.status_bar();

        let mut desk = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
        desk.load_scan("KABR");
        assert_eq!(desk.width_class(), crate::ui_layout::WidthClass::Expanded);
        let roomy_bar = desk.status_bar();

        assert!(
            compact_bar.auto_poll.is_none(),
            "the auto-poll checkbox was crammed into a compact status bar"
        );
        assert!(
            roomy_bar.auto_poll.is_some(),
            "a desktop status bar lost its auto-poll checkbox"
        );

        // Both forms name the site, so the difference is the *detail*: only the
        // long form carries the date and the product count.
        assert!(
            compact_bar.scan_text.contains("KABR") && roomy_bar.scan_text.contains("KABR"),
            "precondition: both forms should name the site, got {:?} and {:?}",
            compact_bar.scan_text,
            roomy_bar.scan_text
        );
        assert!(
            roomy_bar.scan_text.contains("2 products") && roomy_bar.scan_text.contains("2026-07-24"),
            "the roomy bar dropped the long scan summary: {:?}",
            roomy_bar.scan_text
        );
        assert!(
            !compact_bar.scan_text.contains("products")
                && !compact_bar.scan_text.contains("2026-07-24"),
            "the compact bar drew the long scan summary: {:?}",
            compact_bar.scan_text
        );
    }

    /// 16. A wide screen has a persistent sidebar and therefore no hamburger,
    ///     so nothing is excluded — the complement of the test above.
    #[test]
    fn a_wide_screen_has_no_floating_chrome_to_exclude() {
        let h = InputHarness::with_screen(egui::vec2(1200.0, 800.0));
        assert_eq!(h.width_class(), crate::ui_layout::WidthClass::Expanded);
        assert!(
            h.excluded_rects().is_empty(),
            "a persistent sidebar claims panel space rather than floating over \
             the map, so it excludes nothing"
        );
    }

    // ── Overlay texture budget ───────────────────────────────────────────

    use crate::actions::GuiAction;
    use rustdar_overlays::render::overlay_state::OverlayKind;

    /// The texture plans the last frame asked for.
    fn requested_plans(h: &InputHarness) -> Vec<crate::overlay_cache::OverlayTexturePlan> {
        h.last_actions()
            .iter()
            .filter_map(|a| match a {
                GuiAction::RenderOverlay { texture, .. } => Some(*texture),
                _ => None,
            })
            .collect()
    }

    /// A harness with a texture overlay switched on, so the map pane emits
    /// `RenderOverlay`. `RadarSites` is the one overlay whose `has_data` is
    /// unconditionally true, so it needs no fetch to reach the render path.
    fn harness_requesting_overlays() -> InputHarness {
        let mut h = InputHarness::new();
        h.gui_mut().enable_overlay_for_test(OverlayKind::RadarSites);
        h.warm_up();
        h
    }

    /// The whole point of the change, exercised through the real UI: the number the
    /// adapter reports reaches `plan_overlay_texture` via `RawInput` and bounds what
    /// the pane asks for. Forcing the limit is how a WebGL2-class device is tested
    /// without a wasm target.
    #[test]
    fn a_small_adapter_limit_bounds_what_the_pane_requests() {
        // The smallest limit egui itself tolerates: `TextureAtlas::new` asserts
        // `size[0] >= 1024`, so a WebGL2 2048 cannot be halved further here. Still
        // well under what this pane asks for unclamped.
        const LIMIT: u32 = 1024;

        let mut h = harness_requesting_overlays();
        let unclamped = requested_plans(&h);
        assert!(
            !unclamped.is_empty(),
            "fixture must actually reach the render path — no RenderOverlay was emitted"
        );
        assert!(
            unclamped.iter().any(|p| p.width > LIMIT || p.height > LIMIT),
            "fixture must cross the limit before it is imposed, else the clamp is never \
             exercised; got {unclamped:?}"
        );

        h.set_max_texture_side(LIMIT as usize);
        let clamped = requested_plans(&h);
        assert!(!clamped.is_empty(), "still expected a render request after clamping");
        for plan in &clamped {
            assert!(
                plan.width <= LIMIT && plan.height <= LIMIT,
                "requested {}x{} against a {LIMIT} limit",
                plan.width,
                plan.height
            );
            assert!(
                plan.overdraw < crate::overlay_cache::OVERDRAW_FRACTION,
                "overdraw must have been given up to fit"
            );
        }
    }

    /// Desktop is untouched: a limit no window can reach leaves the full overdraw in
    /// place, so the plan is what the pre-clamp arithmetic produced.
    #[test]
    fn a_desktop_class_limit_leaves_the_request_alone() {
        let mut h = harness_requesting_overlays();
        let default_limit = requested_plans(&h);

        h.set_max_texture_side(16384);
        let desktop = requested_plans(&h);
        assert!(!desktop.is_empty());
        for plan in &desktop {
            assert_eq!(
                plan.overdraw,
                crate::overlay_cache::OVERDRAW_FRACTION,
                "a desktop adapter must not cost any overdraw"
            );
        }
        // egui's own default is 2048, which this pane already exceeds — so the two
        // sets differ, which is what makes the assertion above about the limit
        // rather than about the pane being small.
        assert_ne!(
            default_limit, desktop,
            "precondition: egui's 2048 default must clamp this pane, or this test \
             proves nothing about the limit being read at all"
        );
    }
}
