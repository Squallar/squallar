//! Headless input harness for [`Gui::ui`].
//!
//! Drives the real UI through a real [`egui::Context`] with hand-constructed
//! [`egui::RawInput`] — no window, no winit, no wgpu. Each [`InputHarness::frame`]
//! runs one full egui pass (`Gui::ui`, all panels, dialogs and map panes), and
//! `render_panes` records the pointer state it resolved for each pane on the way
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
use crate::pane::{GeoPoint, PaneKind, SectionLine};
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
    /// What the shipped `render_panes` resolved for the active pane, read back
    /// out of `Gui`. See the module note.
    pub resolved: MapPointerFrame,
    /// The same for a non-active pane. `None` in a one-pane layout, where
    /// there is no inactive pane to observe.
    pub resolved_inactive: Option<MapPointerFrame>,
    /// The modality `render_panes` ran this frame under.
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
    /// Keyboard modifiers reported with every frame's `RawInput`, as a held
    /// key really is — set by [`InputHarness::set_modifiers`].
    modifiers: egui::Modifiers,
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
    /// Every text run painted during the last frame, with its layout rect.
    last_texts: Vec<(egui::Rect, String)>,
    /// Every textured quad painted during the last frame — see [`PaintedImage`].
    last_images: Vec<PaintedImage>,
    /// Every line segment painted during the last frame, with its stroke.
    last_segments: Vec<(egui::Pos2, egui::Pos2, egui::Stroke)>,
    /// Rects that came back under a different widget id between passes,
    /// accumulated over every frame since the last [`InputHarness::clear_id_changes`].
    /// See [`InputHarness::id_changes`].
    id_changes: Vec<egui::Rect>,
    /// The previous pass's widget bookkeeping, diffed against each new pass by
    /// [`id_changes_between`] to feed [`InputHarness::id_changes`].
    prev_widgets: egui::WidgetRects,
}

/// A textured quad the last frame painted: where it went, and **which way up**
/// its texture was mapped onto it.
///
/// The second half is the whole reason this exists. `Painter::image` takes a uv
/// rect, and swapping its corners flips the picture vertically with no error, no
/// layout change and no visible fault — a flipped section of a mature storm
/// still looks like a storm, which the section module's own doc calls the single
/// most likely mistake in it and the least likely to be noticed. Reading the uv
/// back off the mesh is the only way a test can see it: the shape carries no
/// image identity beyond a texture id, and the pixels never reach a test at all.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PaintedImage {
    /// The screen rect the quad covers.
    pub rect: egui::Rect,
    /// The texture coordinate at [`rect`](Self::rect)'s top-left corner. `(0,0)`
    /// for an unflipped image, because egui's uv origin is the texture's top
    /// left.
    pub uv_at_top_left: egui::Pos2,
    /// The texture coordinate at [`rect`](Self::rect)'s bottom-right corner.
    /// `(1,1)` for an unflipped image.
    pub uv_at_bottom_right: egui::Pos2,
}

/// Read a textured quad's geometry back off the mesh `Painter::image` built.
///
/// `None` for any mesh that is not one — egui tessellates fonts, shadows and
/// rounded rectangles into meshes too, and none of them is a four-corner image.
fn painted_image(mesh: &egui::epaint::Mesh) -> Option<PaintedImage> {
    if mesh.vertices.len() != 4 {
        return None;
    }
    let mut rect = egui::Rect::NOTHING;
    for vertex in &mesh.vertices {
        rect.extend_with(vertex.pos);
    }
    // Matched by position rather than by index, because the corner order
    // `add_rect_with_uv` emits is epaint's business and not something a test
    // should encode.
    let uv_at = |corner: egui::Pos2| {
        mesh.vertices
            .iter()
            .min_by(|a, b| {
                (a.pos - corner)
                    .length_sq()
                    .total_cmp(&(b.pos - corner).length_sq())
            })
            .map(|v| v.uv)
    };
    Some(PaintedImage {
        rect,
        uv_at_top_left: uv_at(rect.min)?,
        uv_at_bottom_right: uv_at(rect.max)?,
    })
}

/// The finished pass's widget bookkeeping, read back out of the context.
///
/// [`egui::Context::end_pass`] swaps the pass it just closed into `prev_pass`,
/// so immediately after it returns this is the pass that just painted.
fn pass_widgets(ctx: &egui::Context) -> egui::WidgetRects {
    ctx.viewport(|viewport| viewport.prev_pass.widgets.clone())
}

/// Rects that came back under a different widget id while staying put: the
/// verdict of `egui::context::warn_if_rect_changes_id` — the check that logs
/// `Widget rect … changed id between passes` on device — mirrored condition
/// for condition (`egui-0.35.0/src/context.rs:4177`) over the same
/// [`egui::WidgetRects`] bookkeeping it runs on.
///
/// Mirrored rather than read, because egui's own check is `#[cfg(debug_assertions)]`
/// — the function *and* its call site — so in a release build it is compiled
/// out entirely, no runtime option can enable it, and the red marker rect it
/// paints in debug never exists to be matched. The bookkeeping is maintained
/// in every profile, so this reader answers identically under `cargo test`
/// and `cargo test --release`, and
/// `the_id_change_probe_reports_a_real_id_change` holds it against a real id
/// change in whichever profile is running.
fn id_changes_between(prev: &egui::WidgetRects, new: &egui::WidgetRects) -> Vec<egui::Rect> {
    use std::collections::BTreeMap;

    /// Bitwise key so exact float equality groups rects, as egui's
    /// `OrderedRect` does.
    fn rect_key(rect: &egui::Rect) -> [u32; 4] {
        [
            rect.min.x.to_bits(),
            rect.min.y.to_bits(),
            rect.max.x.to_bits(),
            rect.max.y.to_bits(),
        ]
    }

    fn by_rect<'a>(
        widgets: impl Iterator<Item = &'a egui::WidgetRect>,
    ) -> BTreeMap<[u32; 4], Vec<&'a egui::WidgetRect>> {
        let mut lookup: BTreeMap<[u32; 4], Vec<&egui::WidgetRect>> = BTreeMap::new();
        for widget in widgets {
            lookup
                .entry(rect_key(&widget.rect))
                .or_default()
                .push(widget);
        }
        lookup
    }

    let mut changed = Vec::new();
    for (layer_id, new_layer_widgets) in new.layers() {
        let prev_by_rect = by_rect(prev.get_layer(*layer_id));
        for (key, new_at_rect) in by_rect(new_layer_widgets.iter()) {
            let Some(prev_at_rect) = prev_by_rect.get(&key) else {
                continue; // this rect did not exist in the previous pass
            };
            if prev_at_rect
                .iter()
                .any(|pw| new_at_rect.iter().any(|nw| nw.id == pw.id))
            {
                continue; // at least one id stayed the same: not an id change
            }
            // If every previous id still exists somewhere this pass, widgets
            // merely shifted and the rect match is a coincidence.
            if prev_at_rect.iter().all(|pw| new.contains(pw.id)) {
                continue;
            }
            // If every parent id changed too, this is a cascading id shift,
            // not a widget bug.
            if !prev_at_rect
                .iter()
                .any(|pw| new_at_rect.iter().any(|nw| nw.parent_id == pw.parent_id))
            {
                continue;
            }
            changed.push(new_at_rect[0].rect);
        }
    }
    changed
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
            pane_rect: egui::Rect::from_min_max(egui::pos2(220.0, 80.0), egui::pos2(1004.0, 690.0)),
            time: 100.0,
            events: Vec::new(),
            modifiers: egui::Modifiers::default(),
            screen_rect,
            last_rects: Vec::new(),
            max_texture_side: None,
            last_actions: Vec::new(),
            last_texts: Vec::new(),
            last_images: Vec::new(),
            last_segments: Vec::new(),
            id_changes: Vec::new(),
            prev_widgets: egui::WidgetRects::default(),
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

    /// Open or close the layers drawer directly — the state the top bar's
    /// Layers toggle writes below the sidebar breakpoint. For the user's route
    /// see [`Self::open_layers`].
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

    /// One wheel notch over `pos`, in whichever unit the browser chose to
    /// report it in. `egui-winit` derives the unit straight from winit's
    /// `MouseScrollDelta`, so this is the only thing that differs between a
    /// browser that sends `DOM_DELTA_PIXEL` and one that sends `DOM_DELTA_LINE`.
    pub(crate) fn wheel_notch(
        &mut self,
        pos: egui::Pos2,
        unit: egui::MouseWheelUnit,
        delta_y: f32,
    ) {
        self.events.push(egui::Event::PointerMoved(pos));
        self.events.push(egui::Event::MouseWheel {
            unit,
            delta: egui::vec2(0.0, delta_y),
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::default(),
        });
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

    /// Every handler dropdown the last frame's layers panel drew.
    pub(crate) fn dropdowns(&self) -> Vec<crate::ui::DrawnDropdown> {
        self.gui.dropdowns_for_test().to_vec()
    }

    /// The `(options, selected)` the handler behind `label` is offering — the
    /// model a [`crate::ui::DrawnDropdown`] is supposed to be a rendering of.
    pub(crate) fn dropdown_model(&self, label: &str) -> Option<(Vec<(String, String)>, String)> {
        self.gui.dropdown_model_for_test(label)
    }

    /// Every control item the last frame's layers panel drew, whatever its
    /// shape. See [`crate::ui::DrawnControlItem`].
    pub(crate) fn control_items(&self) -> Vec<crate::ui::DrawnControlItem> {
        self.gui.control_items_for_test().to_vec()
    }

    /// The control tree `kind`'s handler currently offers — the model behind
    /// [`Self::control_items`], asked of the handler rather than the renderer.
    pub(crate) fn control_item_model(
        &self,
        kind: OverlayKind,
    ) -> Vec<rustdar_overlays::render::controls::ControlItem> {
        self.gui.control_item_model_for_test(kind)
    }

    /// Every settings row the last frame drew. See
    /// [`crate::ui::DrawnSettingsRow`].
    pub(crate) fn settings_rows(&self) -> Vec<crate::ui::DrawnSettingsRow> {
        self.gui.settings_rows_for_test().to_vec()
    }

    /// The settings row drawn under `id`, if the last frame drew one.
    pub(crate) fn settings_row(&self, id: &str) -> Option<crate::ui::DrawnSettingsRow> {
        self.settings_rows().into_iter().find(|row| row.id == id)
    }

    /// Every leaf label the menu model currently offers, flattened. The
    /// inventory half of the parity walk's menu audit; the drawn half is
    /// [`Self::menu_leaves`].
    pub(crate) fn menu_leaf_labels(&self) -> Vec<&'static str> {
        self.gui.menu_model_leaf_labels()
    }

    /// The menu model's top-level groups with their leaf labels, for walking
    /// the menu-bar presentation one drop-down at a time.
    pub(crate) fn menu_groups(&self) -> Vec<(&'static str, Vec<&'static str>)> {
        self.gui.menu_model_groups()
    }

    // --- scripted user routes ------------------------------------------------
    //
    // Shared by the parity walk and any test that wants to arrive somewhere
    // the way a user does, rather than by poking state. Each one drives the
    // real chrome: real clicks on really-drawn rects, never a setter.

    /// Scroll at `pos` in `step` increments until `pred` passes, or give up
    /// after `max_steps` frames. Returns whether the predicate ever passed.
    ///
    /// The bound is the point: a `ScrollArea` lays content out beyond the
    /// viewport (and may cull it), so "keep scrolling until it shows up" has
    /// to terminate when the thing is genuinely absent rather than spin.
    pub(crate) fn scroll_until(
        &mut self,
        pos: egui::Pos2,
        step: egui::Vec2,
        max_steps: usize,
        mut pred: impl FnMut(&Self) -> bool,
    ) -> bool {
        if pred(self) {
            return true;
        }
        for _ in 0..max_steps {
            self.scroll_at(pos, step);
            self.frame_after(FRAME_DT);
            if pred(self) {
                return true;
            }
        }
        false
    }

    /// What the last frame's top bar drew.
    pub(crate) fn top_bar(&self) -> crate::ui::TopBarProbe {
        self.gui.top_bar_for_test().clone()
    }

    /// What the last frame's bottom bar drew — the phone shell's page
    /// switcher; all-`NOTHING` on the wider widths, which draw no bar.
    pub(crate) fn bottom_bar(&self) -> crate::ui::BottomBarProbe {
        *self.gui.bottom_bar_for_test()
    }

    /// What the last frame's phone sheet drew.
    pub(crate) fn sheet(&self) -> crate::ui::SheetProbe {
        self.gui.sheet_for_test().clone()
    }

    /// The open sheet's rect, or `None` while no page is up.
    pub(crate) fn sheet_rect(&self) -> Option<egui::Rect> {
        let probe = self.sheet();
        probe.page.map(|_| probe.rect)
    }

    /// What the last frame's phone error toast drew — `None` while no error
    /// is up, or while a wider width hosts the error in the status bar.
    pub(crate) fn error_toast(&self) -> Option<crate::ui::ErrorToastProbe> {
        self.gui.error_toast_for_test()
    }

    /// The rect egui holds for the area under `id`, if it has ever been
    /// shown — how a test proves a Modal or Window was (or was not) on
    /// screen without reconstructing its geometry.
    pub(crate) fn area_rect(&self, id: egui::Id) -> Option<egui::Rect> {
        egui::AreaState::load(&self.ctx, id).map(|state| state.rect())
    }

    /// Whether this width presents its chrome through the phone sheet.
    fn is_phone(&self) -> bool {
        self.width_class() == crate::ui_layout::WidthClass::Compact
    }

    /// What the last frame's layer stack drew.
    pub(crate) fn stack(&self) -> crate::ui::StackProbe {
        self.gui.stack_for_test().clone()
    }

    /// The stack row drawn for `kind`, if the last frame drew one.
    pub(crate) fn stack_row(&self, kind: OverlayKind) -> Option<crate::ui::StackRowProbe> {
        self.stack().rows.into_iter().find(|row| row.kind == kind)
    }

    /// What the last frame's inspector drew.
    pub(crate) fn inspector(&self) -> crate::ui::InspectorProbe {
        self.gui.inspector_for_test().clone()
    }

    /// The floating inspector's on-screen rect, from the area state egui
    /// itself keeps — the same authority [`Self::layers_panel_rect`] answers
    /// from — or `None` while it is closed.
    pub(crate) fn inspector_rect(&self) -> Option<egui::Rect> {
        self.inspector()
            .open
            .then(|| egui::AreaState::load(&self.ctx, egui::Id::new("inspector_panel")))
            .flatten()
            .map(|state| state.rect())
    }

    /// Close the inspector the user's way — its own ⟩ collapse button on the
    /// hosts that draw one. The sheet host draws none (M7's sheet-header
    /// polish: the back-chain is the close there), so on Compact this walks
    /// the same back press a phone user would; the page beneath survives
    /// either way. A no-op when it is closed.
    pub(crate) fn close_inspector(&mut self) {
        let probe = self.inspector();
        if !probe.open {
            return;
        }
        if probe.collapse == egui::Rect::NOTHING {
            assert!(
                self.gui.dismiss_top_layer(),
                "the inspector page was open, so a back press must pop it"
            );
        } else {
            self.mouse_click(probe.collapse.center());
        }
        self.warm_up();
        assert!(
            !self.inspector().open,
            "closing the inspector did not close it"
        );
    }

    /// Select `kind`'s options in the inspector the user's way: open the
    /// stack, scroll its row on screen, click it. Asserts the inspector's
    /// body arm for exactly that layer drew.
    pub(crate) fn open_layer_in_inspector(&mut self, kind: OverlayKind) {
        // An inspector left open from a previous selection covers the rows —
        // as the right slide-over on Medium, and as the sheet's Inspector
        // page over its Layers page on Compact.
        self.close_inspector();
        self.open_layers();
        // Scroll inside the panel wherever its host put it — the sheet's
        // body sits in the lower half of a phone screen, so a fixed
        // left-edge position would spin the wheel over the scrim.
        let scroll_pos = self
            .layers_panel_rect()
            .expect("the stack was just opened")
            .center();
        let found = self.scroll_until(scroll_pos, egui::vec2(0.0, -120.0), 60, |h| {
            h.stack_row(kind)
                .is_some_and(|row| h.screen_rect().contains(row.rect.center()))
        });
        assert!(found, "the stack never drew a row for {kind:?} on screen");
        let row = self.stack_row(kind).expect("the row was just found");
        self.mouse_click(row.rect.center());
        self.warm_up();
        assert_eq!(
            self.inspector().mode,
            Some(crate::ui::InspectorSelection::Layer(kind)),
            "clicking {kind:?}'s row did not put its layer body on screen"
        );
    }

    /// Select the active pane's properties the user's way: the stack header.
    pub(crate) fn open_pane_props(&mut self) {
        self.open_layers();
        let header = self.stack().header;
        self.mouse_click(header.center());
        self.warm_up();
        assert_eq!(
            self.inspector().mode,
            Some(crate::ui::InspectorSelection::PaneProps),
            "clicking the stack header did not open Pane properties"
        );
    }

    /// What the last frame's timeline transport drew.
    pub(crate) fn timeline(&self) -> crate::ui::TimelineProbe {
        self.gui.timeline_for_test().clone()
    }

    /// What the last frame's pill rows drew, in pane order.
    pub(crate) fn pill_rows(&self) -> Vec<crate::ui::PillRowProbe> {
        self.gui.pill_rows_for_test().to_vec()
    }

    /// Pane `idx`'s pill row, if the last frame drew one.
    pub(crate) fn pill_row(&self, idx: usize) -> Option<crate::ui::PillRowProbe> {
        self.pill_rows().into_iter().find(|row| row.pane_idx == idx)
    }

    /// Pane `idx`'s `kind` pill — its drawn text and rect — if the last
    /// frame drew one.
    pub(crate) fn pill(&self, idx: usize, kind: crate::ui::PillKind) -> Option<(String, egui::Rect)> {
        self.pill_row(idx)?
            .pills
            .into_iter()
            .find(|(k, _, _)| *k == kind)
            .map(|(_, text, rect)| (text, rect))
    }

    /// The pill popover the last frame drew, if one was open.
    pub(crate) fn pill_popover(&self) -> Option<crate::ui::PillPopoverProbe> {
        self.gui.pill_popover_for_test().cloned()
    }

    /// Whether some feature consumed the last frame's map click — the
    /// consumption half of the fade trigger.
    pub(crate) fn click_consumed(&self) -> bool {
        self.gui.click_consumed_for_test()
    }

    /// Whether the UI is faded (plan §1.8) — the state the fade contracts
    /// assert beside the probes' drawn/not-drawn evidence.
    pub(crate) fn faded(&self) -> bool {
        self.gui.ui_faded_for_test()
    }

    /// What the last frame's Add-layer catalog drew.
    pub(crate) fn catalog(&self) -> crate::ui::CatalogProbe {
        self.gui.catalog_for_test().clone()
    }

    /// The catalog tile drawn under `label` in `group`, if the last frame
    /// drew one.
    pub(crate) fn catalog_tile(
        &self,
        group: crate::ui::CatalogGroup,
        label: &str,
    ) -> Option<crate::ui::CatalogTileProbe> {
        self.catalog()
            .tiles
            .into_iter()
            .find(|tile| tile.group == group && tile.label == label)
    }

    /// Open the Add-layer catalog the user's way: the stack's top
    /// `+ Add layer` button. Asserts it really opened.
    pub(crate) fn open_catalog(&mut self) {
        if self.catalog().open {
            return;
        }
        self.open_layers();
        let add = self.stack().add_top;
        assert!(
            add.is_positive(),
            "the stack drew no Add-layer button to open the catalog with"
        );
        self.mouse_click(add.center());
        self.warm_up();
        assert!(
            self.catalog().open,
            "clicking + Add layer did not open the catalog"
        );
    }

    /// Put the layers panel on screen the user's way: the top bar's Layers
    /// toggle on the wide widths, the bottom bar's Layers item on the phone
    /// — where the panel is the sheet's Layers page. Idempotent — each
    /// route's own probe says whether the panel is already showing, and a
    /// second click would close it again (the bottom bar's toggle
    /// semantics).
    pub(crate) fn open_layers(&mut self) {
        if self.is_phone() {
            if self.sheet().page == Some(crate::ui::SheetPage::Layers) {
                return;
            }
            let (item, _) = self.bottom_bar().layers;
            self.mouse_click(item.center());
            self.warm_up();
            assert_eq!(
                self.sheet().page,
                Some(crate::ui::SheetPage::Layers),
                "tapping the bottom bar's Layers item did not open the Layers page"
            );
            return;
        }
        let (toggle, open) = self.top_bar().layers_toggle;
        if open {
            return;
        }
        self.mouse_click(toggle.center());
        self.warm_up();
    }

    /// Take the layers panel off screen the user's way — the same toggle, or
    /// the same bottom-bar item on the phone.
    ///
    /// Since the full-bleed flip the panel floats *over* the map's left side,
    /// so a map-interaction test whose positions land under it must close it
    /// first: a click there belongs to the panel, exactly as it does for a
    /// user.
    pub(crate) fn close_layers(&mut self) {
        if self.is_phone() {
            if self.sheet().page != Some(crate::ui::SheetPage::Layers) {
                return;
            }
            let (item, _) = self.bottom_bar().layers;
            self.mouse_click(item.center());
            self.warm_up();
            return;
        }
        let (toggle, open) = self.top_bar().layers_toggle;
        if !open {
            return;
        }
        self.mouse_click(toggle.center());
        self.warm_up();
    }

    /// The floating layers panel's on-screen rect, from the area state egui
    /// itself keeps — the same authority `layer_id_at` answers from — or
    /// `None` while the panel is closed.
    pub(crate) fn layers_panel_rect(&self) -> Option<egui::Rect> {
        self.layers_panel_on_screen()
            .then(|| egui::AreaState::load(&self.ctx, egui::Id::new("layers_panel")))
            .flatten()
            .map(|state| state.rect())
    }

    /// Open the whole menu the user's way: a click on the top bar's ☰
    /// button on the wide widths, a tap on the bottom bar's Menu item on the
    /// phone — where the menu is the sheet's Menu page. Idempotent — with
    /// the menu already open its leaves are drawn, and a second click would
    /// close it.
    pub(crate) fn open_menu(&mut self) {
        if !self.menu_leaves().is_empty() {
            return;
        }
        let button = if self.is_phone() {
            self.bottom_bar().menu.0
        } else {
            self.top_bar().menu_button
        };
        self.mouse_click(button.center());
        self.warm_up();
        assert!(
            !self.menu_leaves().is_empty(),
            "clicking the menu button did not put the menu on screen"
        );
    }

    /// Close the menu by clicking its own button again — the toggle half of
    /// `Popup::menu`'s contract, and of the bottom bar's (contract 64). A
    /// no-op when it is not open.
    pub(crate) fn close_menu(&mut self) {
        if self.menu_leaves().is_empty() {
            return;
        }
        let button = if self.is_phone() {
            self.bottom_bar().menu.0
        } else {
            self.top_bar().menu_button
        };
        self.mouse_click(button.center());
        self.warm_up();
        assert!(
            self.menu_leaves().is_empty(),
            "clicking the menu button did not close the open menu"
        );
    }

    /// Whether the last frame drew the layers panel, in either form — read off
    /// the panel's own id probes rather than off the flags that decide it.
    pub(crate) fn layers_panel_on_screen(&self) -> bool {
        self.widget_id_probes()
            .iter()
            .any(|(name, _)| *name == "layers_scroll")
    }

    /// Open the settings the way a user does: through the ☰ dropdown's
    /// "Settings..." entry, which opens the inspector on App › Settings.
    pub(crate) fn open_settings(&mut self) {
        self.open_menu();
        let leaf = self
            .menu_leaf("Settings...")
            .expect("the menu did not draw the Settings... entry");
        assert!(
            self.screen_rect().contains(leaf.rect.center()),
            "Settings... was drawn at {:?}, outside the {:?} viewport",
            leaf.rect,
            self.screen_rect()
        );
        self.mouse_click(leaf.rect.center());
        self.warm_up();
        assert!(
            self.gui.settings_visible(),
            "clicking Settings... did not open the inspector's settings body"
        );
    }

    /// Every text run the last frame painted, without its rect.
    pub(crate) fn painted_text_strings(&self) -> Vec<String> {
        self.last_texts
            .iter()
            .map(|(_, text)| text.clone())
            .collect()
    }

    /// Every text run the last frame painted inside `rect`.
    ///
    /// The whole-screen list above cannot tell a colour-bar tick from a number
    /// in the chrome, which matters when the assertion is "this pane's bar is
    /// labelled in millimetres and in nothing else".
    pub(crate) fn painted_text_strings_in(&self, rect: egui::Rect) -> Vec<String> {
        self.last_texts
            .iter()
            .filter(|(r, _)| rect.contains(r.center()))
            .map(|(_, text)| text.clone())
            .collect()
    }

    /// Lay the frames out at a scale other than 1 physical pixel per point.
    ///
    /// The harness runs at 1 by default, which makes points and pixels the same
    /// number — and therefore makes any test that multiplies by
    /// [`Self::pixels_per_point`] pass whether the production code multiplies or
    /// not. A test about *pixels* has to run at a scale where the two differ.
    pub(crate) fn set_pixels_per_point(&mut self, ppp: f32) {
        self.ctx.set_pixels_per_point(ppp);
        self.warm_up();
    }

    /// Every text run the last frame painted, with the rect it occupies.
    ///
    /// The rect is the part [`Self::painted_text_strings_in`] throws away, and
    /// it is what a test needs to ask whether something *fits* rather than
    /// merely whether it was drawn. `Painter::text` will happily lay a sentence
    /// out on one line twice as wide as its pane.
    pub(crate) fn painted_text_rects(&self) -> Vec<(egui::Rect, String)> {
        self.last_texts.clone()
    }

    /// Every textured quad the last frame painted whose rect is inside `rect`.
    ///
    /// The other end of [`Self::painted_text_strings_in`]: a section pane's
    /// picture is not text and cannot be read back as any, so without this the
    /// only thing a harness test could say about a rendered section was what its
    /// caption said about it.
    pub(crate) fn painted_images_in(&self, rect: egui::Rect) -> Vec<PaintedImage> {
        self.last_images
            .iter()
            .copied()
            .filter(|image| rect.contains(image.rect.center()))
            .collect()
    }

    /// Every line segment the last frame painted inside `rect` with `color`.
    ///
    /// Filtered by colour because a section pane draws three different things
    /// with `line_segment` — the axis grid, the tilt ladder's halo and the
    /// ladder itself — and a count that mixed them could not tell a missing
    /// ladder from a grid with more ticks on it.
    pub(crate) fn painted_segments_in(
        &self,
        rect: egui::Rect,
        color: egui::Color32,
    ) -> Vec<(egui::Pos2, egui::Pos2)> {
        self.last_segments
            .iter()
            .filter(|(a, b, stroke)| {
                stroke.color == color && rect.contains(*a) && rect.contains(*b)
            })
            .map(|&(a, b, _)| (a, b))
            .collect()
    }

    /// Whether `needle` was painted anywhere inside `rect`.
    ///
    /// The other end of a probe: a `DrawnDropdown` says what the renderer was
    /// *handed*, this says what egui put on the glass, so a test can require
    /// the two to agree.
    pub(crate) fn text_painted_in(&self, rect: egui::Rect, needle: &str) -> bool {
        self.last_texts
            .iter()
            .any(|(r, text)| rect.contains(r.center()) && text.contains(needle))
    }

    /// The display name the registry gives `kind` — what the stack rows and
    /// the catalog's overlay tiles both print.
    pub(crate) fn overlay_display_name(&self, kind: OverlayKind) -> &str {
        self.gui.overlays.display_name(kind)
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

    /// What kind each visible pane *is* — the **input** to `render_panes`' kind
    /// branch, read off the live pane state.
    ///
    /// Deliberately not the same thing as [`Self::pane_content_probes`], which
    /// reports the arm that ran. A test that only asserted on this would agree
    /// with a branch that ignored it.
    pub(crate) fn pane_kinds(&self) -> Vec<PaneKind> {
        self.gui.panes().iter().map(|pane| pane.kind()).collect()
    }

    /// Which render arm ran for each pane on the last frame — the **output** of
    /// the kind branch, recorded inside the arms. See
    /// [`crate::ui::PaneContentProbe`].
    pub(crate) fn pane_content_probes(&self) -> Vec<crate::ui::PaneContentProbe> {
        self.gui.pane_content_for_test().to_vec()
    }

    /// The pointer state `render_panes` resolved for every pane last frame, not
    /// just the active one that [`FrameOutcome`] exposes.
    pub(crate) fn pane_pointers(&self) -> Vec<crate::ui_input::PanePointerProbe> {
        self.gui.pane_pointers_for_test().to_vec()
    }

    /// Convert pane `idx` to a cross-section pane cut along `a` → `b`, as the
    /// draw interaction will.
    ///
    /// Goes through `PaneState::set_kind` and `SectionLine::new` rather than
    /// assembling a `PaneContent` here, so the fixture cannot construct a state
    /// the shipped writers refuse — a line with a non-finite endpoint, in
    /// particular, which is the one that would make the pane re-render forever.
    pub(crate) fn make_pane_cross_section(&mut self, idx: usize, a: GeoPoint, b: GeoPoint) {
        let line = SectionLine::new(a, b)
            .expect("a fixture line must be finite and have two distinct ends");
        let pane = self
            .gui
            .pane_mut(idx)
            .unwrap_or_else(|| panic!("no pane {idx}"));
        pane.set_kind(PaneKind::CrossSection);
        pane.cross_section_mut()
            .expect("the pane was just converted to a section")
            .line = Some(line);
        self.warm_up();
    }

    /// Put a finished cut on pane `idx`, as `poll_section_results` does — a
    /// full-size raster and a texture for it — so the pane draws its picture
    /// rather than its "cutting…" state.
    ///
    /// `axes` is the caller's, because the caption is computed from it and the
    /// numbers in the caption are the whole reason the caption exists.
    ///
    /// `rungs` likewise, and it is not decoration: the drawn tilt ladder is the
    /// section's *first* honesty device and it is drawn from the elevations the
    /// cut carries, so a fixture that left them out would paint a picture with
    /// the device missing and no test could tell.
    /// `CrossSection::from_parts` refuses a ladder that is not `tilt_count`
    /// long, so the two cannot drift apart here either.
    pub(crate) fn place_section(
        &mut self,
        idx: usize,
        axes: rustdar_radar::xsect::SectionAxes,
        rungs: &[f64],
    ) {
        use rustdar_radar::sampler::SampleStatus;
        use rustdar_radar::xsect::{CrossSection, SECTION_HEIGHT, SECTION_WIDTH};
        let pixels = SECTION_WIDTH * SECTION_HEIGHT;
        let cut = CrossSection::from_parts(
            vec![0u8; pixels * 4],
            vec![f32::NAN; pixels],
            vec![SampleStatus::BelowLowestBeam.wire_code(); pixels],
            axes,
            rungs.to_vec(),
        )
        .expect(
            "a full-size, all-BelowLowestBeam section with a matching ladder is \
             well formed",
        );
        let texture = self.ctx.load_texture(
            format!("harness-section-{idx}"),
            egui::ColorImage::filled([1, 1], egui::Color32::WHITE),
            egui::TextureOptions::NEAREST,
        );
        let section = self
            .gui
            .pane_mut(idx)
            .unwrap_or_else(|| panic!("no pane {idx}"))
            .cross_section_mut()
            .expect("pane is not a section pane");
        section.section = Some(std::sync::Arc::new(cut));
        section.texture = Some(texture);
        section.unavailable = None;
        self.warm_up();
    }

    /// Convert pane `idx` to a cross-section pane that has **not been aimed**,
    /// as arming the draw and then converting a pane would leave it.
    ///
    /// The distinction from [`make_pane_cross_section`](Self::make_pane_cross_section)
    /// is behavioural rather than cosmetic: a section pane with no line paints
    /// the "draw a line" instruction, and one *with* a line and no render yet
    /// paints that it is cutting. Both are correct and they are different
    /// screens, so a test about either has to say which pane it means.
    pub(crate) fn make_pane_unaimed_cross_section(&mut self, idx: usize) {
        self.gui
            .pane_mut(idx)
            .unwrap_or_else(|| panic!("no pane {idx}"))
            .set_kind(PaneKind::CrossSection);
        self.warm_up();
    }

    /// Whether the cross-section draw is armed, as the menu checkbox sets it.
    pub(crate) fn section_draw_armed(&self) -> bool {
        self.gui.section_draw_armed()
    }

    /// Arm or disarm the cross-section draw.
    ///
    /// The menu entry's own end-to-end click has its own test; this is for the
    /// pointer tests, whose subject is the drag rather than the checkbox.
    pub(crate) fn set_section_draw_armed(&mut self, armed: bool) {
        self.gui.set_section_draw_armed(armed);
    }

    /// Whether the 3D region drag is armed — the *other* modal drag on a map
    /// pane, and the one the cross-section draw has to be mutually exclusive
    /// with.
    pub(crate) fn region_arm(&self) -> bool {
        self.gui.region_arm_for_test()
    }

    /// Arm or disarm the 3D region drag, through the same setter the menu uses.
    ///
    /// Deliberately the real setter and not a field write: arming this is what
    /// disarms the cross-section draw, and a test that reached past that rule
    /// would be testing a state the app cannot be in.
    pub(crate) fn set_region_arm(&mut self, on: bool) {
        self.gui.set_region_arm_for_test(on);
    }

    /// The line pane `idx` is aimed along, if it is a section pane with one.
    pub(crate) fn section_line(&self, idx: usize) -> Option<SectionLine> {
        self.gui.pane(idx)?.cross_section()?.line
    }

    /// Pane `idx`'s own map centre, as the shipped `render_panes` left it.
    ///
    /// Read off `Gui` rather than off the harness' parallel `map_memory`, so a
    /// test asking "did the map pan?" is asking about the map on screen.
    pub(crate) fn pane_center(&self, idx: usize) -> Option<walkers::Position> {
        self.gui.pane(idx)?.map_memory.detached()
    }

    /// Where pane `idx` is looking at `pos`, on the ground.
    ///
    /// Built from the pane's live `MapMemory` and the rect the layout gave it —
    /// the same two inputs `Map::show` builds its own projector from, which is
    /// what makes this the map's answer rather than a second Mercator.
    pub(crate) fn ground_at(&self, idx: usize, pos: egui::Pos2) -> walkers::Position {
        let rect = self.pane_rects()[idx];
        let memory = &self.gui.pane(idx).expect("no such pane").map_memory;
        let centre = memory
            .detached()
            .unwrap_or_else(|| walkers::lat_lon(35.3333, -97.2778));
        walkers::Projector::new(rect, memory, centre).unproject(egui::vec2(pos.x, pos.y))
    }

    /// Where pane `idx` draws `ground`, on screen — [`Self::ground_at`]'s
    /// inverse, from the same projector inputs, so a test can aim a pointer at
    /// a section handle the way `draw_section_tracks` placed it.
    pub(crate) fn screen_of(&self, idx: usize, ground: GeoPoint) -> egui::Pos2 {
        let rect = self.pane_rects()[idx];
        let memory = &self.gui.pane(idx).expect("no such pane").map_memory;
        let centre = memory
            .detached()
            .unwrap_or_else(|| walkers::lat_lon(35.3333, -97.2778));
        walkers::Projector::new(rect, memory, centre)
            .project(walkers::lat_lon(ground.lat, ground.lon))
            .to_pos2()
    }

    /// Convert pane `idx` to a 3D volume pane, as the menu toggle will.
    pub(crate) fn make_pane_volume(&mut self, idx: usize) {
        self.gui
            .pane_mut(idx)
            .unwrap_or_else(|| panic!("no pane {idx}"))
            .set_kind(PaneKind::Volume);
        self.warm_up();
    }

    /// What the 3D arm decided for each volume pane on the last frame.
    ///
    /// The only thing that can tell a pane that drew a volume from one that drew
    /// nothing: both paint the same number of egui shapes, and a callback whose
    /// payload the renderer cannot use looks exactly like an empty state.
    pub(crate) fn volume_arms(&self) -> Vec<crate::ui::VolumeArmProbe> {
        self.gui.volume_arms_for_test().to_vec()
    }

    /// The scale the frames are being laid out at. The 3D pane's offscreen is
    /// sized from this, so a test about pixels has to read it rather than assume
    /// it is 1.
    pub(crate) fn pixels_per_point(&self) -> f32 {
        self.ctx.pixels_per_point()
    }

    /// The excluded rects `render_panes` was actually handed on the last frame.
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
        let collected = info.timestamp;
        self.gui.set_scan_info_for_site(site, info);
        // And the substrate half, because that is what a volume arrival does:
        // `App` writes its base holder and `set_scan_info_for_site` from the
        // same arm and publishes the current-volume stamp each frame. A harness
        // that filled only the plan view's half would leave a 3D pane waiting
        // for a volume that, in production, had already landed.
        //
        // Use `set_current_volume` to take them apart — that is how a stamp
        // that trails or leads the plan view's own time is staged.
        self.set_current_volume(site, Some(collected));
        self.warm_up();
    }

    /// Say what `site`'s current-volume stamp is, or that the site has no
    /// volume at all yet.
    ///
    /// The 3D pane's only input, and deliberately separable from
    /// [`Self::load_scan`]: the pane names the volume it builds from by this
    /// stamp alone, never by the plan view's `scan_info`.
    pub(crate) fn set_current_volume(
        &mut self,
        site: &str,
        collected: Option<chrono::NaiveDateTime>,
    ) {
        let mut volumes = std::collections::HashMap::new();
        if let Some(collected) = collected {
            volumes.insert(
                site.to_owned(),
                crate::ui::CurrentVolumeStamp {
                    newest: collected,
                    // A pure base volume: what an archive arrival publishes.
                    // Tests staging a merged or still-filling state build the
                    // stamp themselves through `Gui::set_current_volumes`.
                    base_started: Some(collected),
                },
            );
        }
        self.gui.set_current_volumes(volumes);
        self.warm_up();
    }

    /// Say when the data behind pane `idx`'s radar image was collected, as
    /// `apply_render_to_pane` does when a render lands — whichever datasource the
    /// product came from.
    pub(crate) fn set_data_time(&mut self, idx: usize, collected: Option<chrono::NaiveDateTime>) {
        self.gui
            .pane_mut(idx)
            .unwrap_or_else(|| panic!("no pane {idx}"))
            .data_time = collected;
        self.warm_up();
    }

    /// Offer `product` at `elevation` on pane `idx`'s loaded scan, as a landed
    /// Level II volume or Level III object does.
    ///
    /// `ScanInfo::from_scan` lists the volume's own moments and
    /// `poll_level3_results` adds each bucket product with the angle off its PDB;
    /// this is the state either leaves behind, which is what makes the product
    /// selectable and gives `get_rendering_params` an angle to snap to.
    pub(crate) fn offer_product(
        &mut self,
        idx: usize,
        product: rustdar_radar::types::RadarProduct,
        elevation: f32,
    ) {
        let pane = self
            .gui
            .pane_mut(idx)
            .unwrap_or_else(|| panic!("no pane {idx}"));
        let info = pane
            .scan_info
            .as_mut()
            .expect("load_scan first: a product is offered on a scan");
        if !info.available_products.contains(&product) {
            info.available_products.push(product);
            info.available_products
                .sort_by_key(rustdar_radar::types::RadarProduct::sort_order);
        }
        let angles = info.product_elevations.entry(product).or_default();
        if !angles.iter().any(|a| (a - elevation).abs() < 0.05) {
            angles.push(elevation);
            angles.sort_by(|a, b| a.total_cmp(b));
        }
        self.warm_up();
    }

    /// Select `product` on pane `idx`, as the layers panel's product combo box
    /// does — including the elevation reset that combo performs on a change.
    pub(crate) fn select_product(
        &mut self,
        idx: usize,
        product: rustdar_radar::types::RadarProduct,
    ) {
        let pane = self
            .gui
            .pane_mut(idx)
            .unwrap_or_else(|| panic!("no pane {idx}"));
        if pane.selected_product != product {
            pane.selected_product = product;
            pane.selected_elevation = 0.0;
        }
        self.warm_up();
    }

    /// Place a finished radar image on pane `idx`, as `apply_render_to_pane` does
    /// when a render lands: a texture in the pane's Radar overlay cache, with the
    /// metadata that says what it depicts.
    ///
    /// The metadata is the point — `PaneState::stale_image_on_screen` reads
    /// `product` and `elevation` off it — and the fields are filled the way the
    /// host fills them, from the render's own product and *snapped* elevation
    /// rather than from the pane's selection. There is one such assignment in
    /// production, shared by both datasources, and
    /// `a_placed_render_describes_what_it_depicts` in `rustdar-frontend` holds
    /// this fixture to it.
    pub(crate) fn place_radar_image(
        &mut self,
        idx: usize,
        product: rustdar_radar::types::RadarProduct,
        elevation: f32,
    ) {
        use crate::overlay_cache::{OverlayTextureData, RadarTextureMeta};
        use rustdar_radar::types::ImageBounds;

        let (lat, lon) = {
            let pane = self
                .gui
                .pane(idx)
                .unwrap_or_else(|| panic!("no pane {idx}"));
            let info = pane
                .scan_info
                .as_ref()
                .expect("load_scan first: an image is projected from a site");
            (info.site.lat, info.site.lon)
        };
        let image = egui::ColorImage::from_rgba_unmultiplied([1, 1], &[255, 255, 255, 255]);
        let texture = self
            .ctx
            .load_texture("harness_radar", image, egui::TextureOptions::NEAREST);
        let bounds = ImageBounds::from_radar_site(lat, lon);
        let cache = self
            .gui
            .pane_mut(idx)
            .unwrap()
            .overlay_cache_mut(OverlayKind::Radar);
        cache.current = Some(OverlayTextureData {
            texture,
            geo_bounds: rustdar_overlays::types::GeoBounds {
                min_lat: bounds.min_lat,
                max_lat: bounds.max_lat,
                min_lon: bounds.min_lon,
                max_lon: bounds.max_lon,
            },
            data_generation: 0,
            render_zoom: 0,
            width: 1,
            height: 1,
            radar_meta: Some(RadarTextureMeta {
                value_data: std::sync::Arc::new(Vec::new()),
                lat,
                lon,
                max_range_km: 230.0,
                product,
                elevation,
            }),
            hit_map: None,
        });
        self.warm_up();
    }

    /// Every rect painted during the last frame, in paint order.
    pub(crate) fn painted_rects(&self) -> &[egui::Rect] {
        &self.last_rects
    }

    /// Rects that came back under a different widget id between passes, since
    /// the last [`InputHarness::clear_id_changes`].
    ///
    /// The same verdict egui logs as `Widget rect … changed id between passes`
    /// on device, computed by [`id_changes_between`] from the per-pass widget
    /// bookkeeping egui maintains in every build profile. egui's own check is
    /// compiled out of release builds, so reading its painted debug marker
    /// instead would leave `cargo test --release` asserting on a probe that
    /// cannot fire.
    pub(crate) fn id_changes(&self) -> &[egui::Rect] {
        &self.id_changes
    }

    /// Forget the id changes seen so far, so a test can attribute later ones to
    /// one specific transition.
    pub(crate) fn clear_id_changes(&mut self) {
        self.id_changes.clear();
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

    /// Make the layout claim `count` panes without giving it that many
    /// `PaneState`s — the skew described on `Gui::claim_pane_count_for_test`.
    pub(crate) fn claim_pane_count(&mut self, count: usize) {
        self.gui.claim_pane_count_for_test(count);
        self.warm_up();
    }

    /// The pane rects the real layout produces inside the map panel.
    pub(crate) fn pane_rects(&self) -> Vec<egui::Rect> {
        self.gui.pane_rects_for_test()
    }

    /// The rect the pane grid is laid out in, as `render_panes` sees it.
    pub(crate) fn map_panel_rect(&self) -> egui::Rect {
        self.gui.map_panel_rect_for_test()
    }

    /// Pan pane `idx` until `site`'s icon is drawn at `target`, as dragging the
    /// map there does.
    ///
    /// Solved with walkers' own [`walkers::Projector`], built from the pane's
    /// live `MapMemory` and the rect the layout gave it — the same two inputs
    /// `Map::show` builds the projector the icon is placed with from. A
    /// hand-rolled Mercator here would be a second implementation, and the one
    /// the test depends on would be the wrong one.
    ///
    /// `Projector::unproject` is anchored on the *current* centre, so the
    /// centring pass below is not redundant: it is what makes the reflection
    /// `2·centre − target` solve for the position that puts the site at
    /// `target`.
    pub(crate) fn place_site_at(&mut self, idx: usize, site: &str, target: egui::Pos2) {
        let radar = rustdar_radar::sites::get_radar_site(site).expect("unknown radar site");
        let geo = walkers::lat_lon(radar.lat, radar.lon);

        self.gui
            .pane_mut(idx)
            .unwrap_or_else(|| panic!("no pane {idx}"))
            .map_memory
            .center_at(geo);
        self.warm_up();

        let rect = self.pane_rects()[idx];
        let centre = rect.center();
        let shifted = {
            let memory = &self.gui.pane(idx).expect("pane vanished").map_memory;
            let projector = walkers::Projector::new(rect, memory, geo);
            projector.unproject(egui::vec2(
                2.0 * centre.x - target.x,
                2.0 * centre.y - target.y,
            ))
        };
        self.gui
            .pane_mut(idx)
            .unwrap()
            .map_memory
            .center_at(shifted);
        self.warm_up();
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

    /// The id of the topmost egui layer at `pos`, if any — the same authority
    /// [`Self::is_floating_layer_at`] consults, exposed whole so a test can
    /// name *which* surface owns a point where two floating things overlap.
    pub(crate) fn top_layer_id_at(&self, pos: egui::Pos2) -> Option<egui::Id> {
        self.ctx.layer_id_at(pos).map(|layer| layer.id)
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

    /// The right button down. The 3D pane's pan is on it, and egui reports
    /// per-button drags — so a test that pressed the primary button would be
    /// testing the orbit.
    pub(crate) fn mouse_press_secondary(&mut self, pos: egui::Pos2) {
        self.mouse_move(pos);
        self.events
            .push(pointer_button_of(pos, egui::PointerButton::Secondary, true));
    }

    pub(crate) fn mouse_release_secondary(&mut self, pos: egui::Pos2) {
        self.mouse_move(pos);
        self.events.push(pointer_button_of(
            pos,
            egui::PointerButton::Secondary,
            false,
        ));
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
    /// `DeviceEvent`, and `rustdar-frontend/src/egui_renderer.rs:79` forwards
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

    // --- multi-touch (mirrors winit's web backend) --------------------------

    /// A second finger lands while the first stays down.
    ///
    /// `egui-winit` emits pointer emulation only for the finger held in
    /// `pointer_touch_id` (`lib.rs:882`), so the second finger is a bare
    /// `Touch` — and winit's web backend gives it **its own device id**, which
    /// is the whole reason pinch needed fixing. Both fingers keep their web
    /// device ids here so the tests run against the real event shape.
    pub(crate) fn web_second_finger_down(&mut self, pos: egui::Pos2) {
        self.events
            .push(web_touch(WEB_FINGER_B, egui::TouchPhase::Start, pos));
    }

    /// Both fingers move. Only the first drives the emulated pointer.
    pub(crate) fn web_pinch_move(&mut self, a: egui::Pos2, b: egui::Pos2) {
        self.events
            .push(web_touch(WEB_FINGER_A, egui::TouchPhase::Move, a));
        self.events.push(egui::Event::PointerMoved(a));
        self.events
            .push(web_touch(WEB_FINGER_B, egui::TouchPhase::Move, b));
    }

    /// The first finger goes down, on the web backend's per-finger device.
    pub(crate) fn web_first_finger_down(&mut self, pos: egui::Pos2) {
        self.events
            .push(web_touch(WEB_FINGER_A, egui::TouchPhase::Start, pos));
        self.events.push(egui::Event::PointerMoved(pos));
        self.events.push(pointer_button(pos, true));
    }

    /// Lift the **second** finger, leaving the first down — pinch ending with
    /// one finger still on the glass.
    pub(crate) fn web_second_finger_up(&mut self, pos: egui::Pos2) {
        self.events
            .push(web_touch(WEB_FINGER_B, egui::TouchPhase::End, pos));
    }

    /// Lift the **first** finger — the one backing the emulated pointer —
    /// while the second stays down. `egui-winit` releases and drops the pointer
    /// here (`lib.rs:904`), so this is the ordering that can strand the map.
    pub(crate) fn web_first_finger_up(&mut self, pos: egui::Pos2) {
        self.events
            .push(web_touch(WEB_FINGER_A, egui::TouchPhase::End, pos));
        self.events.push(pointer_button(pos, false));
        self.events.push(egui::Event::PointerGone);
    }

    /// Spread two fingers apart from `center` over `steps` frames, from
    /// `from_gap` to `to_gap` pixels of separation. Returns the last frame.
    pub(crate) fn web_pinch(
        &mut self,
        center: egui::Pos2,
        from_gap: f32,
        to_gap: f32,
        steps: usize,
    ) -> FrameOutcome {
        let at = |gap: f32| {
            (
                center - egui::vec2(gap / 2.0, 0.0),
                center + egui::vec2(gap / 2.0, 0.0),
            )
        };
        let (a, b) = at(from_gap);
        self.web_first_finger_down(a);
        self.web_second_finger_down(b);
        let mut outcome = self.frame_after(FRAME_DT);
        for step in 1..=steps {
            let gap = from_gap + (to_gap - from_gap) * (step as f32 / steps as f32);
            let (a, b) = at(gap);
            self.web_pinch_move(a, b);
            outcome = self.frame_after(FRAME_DT);
        }
        outcome
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

    /// Hold or release keyboard modifiers for every following frame, as a key
    /// held across a gesture really is. Pass `Modifiers::default()` to let go.
    pub(crate) fn set_modifiers(&mut self, modifiers: egui::Modifiers) {
        self.modifiers = modifiers;
    }

    /// Type `text` into whatever widget holds keyboard focus, as the
    /// integrations deliver committed text — the way a test fills a focused
    /// `TextEdit` (clicking one focuses it; egui does that itself).
    pub(crate) fn type_text(&mut self, text: &str) {
        self.events.push(egui::Event::Text(text.to_owned()));
        self.frame_after(FRAME_DT);
    }

    /// Give keyboard focus to the widget behind `id`, as tabbing to it would.
    ///
    /// For the widgets a click does not focus — egui's `Slider` reads its
    /// arrow keys only `if response.has_focus()`, and only `TextEdit`
    /// requests focus from a click. The id must come from a probe the
    /// renderer reported, so the focus lands on the widget egui really keyed.
    pub(crate) fn focus_widget(&mut self, id: egui::Id) {
        self.ctx.memory_mut(|mem| mem.request_focus(id));
        self.frame_after(FRAME_DT);
    }

    /// One press-and-release of `key` in the next frame's `RawInput`, as the
    /// desktop integrations deliver a quick tap. Android's back never takes
    /// this route — it is a logical event with no egui key behind it — which
    /// is exactly the difference the dismissal tests need both sides of.
    pub(crate) fn key_press(&mut self, key: egui::Key) {
        self.events.push(egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::default(),
        });
        self.events.push(egui::Event::Key {
            key,
            physical_key: None,
            pressed: false,
            repeat: false,
            modifiers: egui::Modifiers::default(),
        });
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
        let mut raw_input = egui::RawInput {
            screen_rect: Some(self.screen_rect),
            time: Some(self.time),
            events: std::mem::take(&mut self.events),
            max_texture_side: self.max_texture_side,
            modifiers: self.modifiers,
            ..Default::default()
        };
        // The same call `EguiRenderer::begin_frame` makes, at the same point in
        // the pipeline, so the multi-touch tests exercise the shipped function.
        crate::ui_input::normalize_touch_devices(&mut raw_input);
        // Likewise, and at the same point: the web build's wheel-unit rewrite.
        // `zoom_factor` is 1.0 here, matching an unscaled UI.
        crate::ui_input::normalize_wheel_units(&mut raw_input, 1.0);

        // `begin_pass`/`end_pass` rather than `run_ui`, so the body runs exactly
        // once per frame: a repeated pass would feed the same events to the
        // gesture detectors twice.
        let ctx = self.ctx.clone();
        ctx.begin_pass(raw_input);

        // The real UI, panels, dialogs and map panes included. `render_panes`
        // resolves each pane's pointer state on the way through and records it.
        self.last_actions = self.gui.ui(&ctx);

        // The double-render guard, enforced on every frame any test runs:
        // each handler-control pass ends by saving the handlers' state over
        // the active pane's configs, so a second pass in one frame would save
        // over the first's writes. See `Gui::control_render_passes`.
        assert!(
            self.gui.control_render_passes_for_test() <= 1,
            "handler ControlItems rendered {} times in one frame; each pass \
             is a load→mutate→save round trip over the active pane's overlay \
             configs, and two of them fight",
            self.gui.control_render_passes_for_test()
        );

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
                    "render_panes recorded no active pane this frame ({} pane probe(s)) \
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
        let widgets = pass_widgets(&ctx);
        self.id_changes
            .extend(id_changes_between(&self.prev_widgets, &widgets));
        self.prev_widgets = widgets;
        self.last_rects = full_output
            .shapes
            .iter()
            .filter_map(|clipped| match &clipped.shape {
                egui::Shape::Rect(rect_shape) => Some(rect_shape.rect),
                _ => None,
            })
            .collect();
        self.last_texts = full_output
            .shapes
            .iter()
            .filter_map(|clipped| match &clipped.shape {
                egui::Shape::Text(text) => Some((
                    egui::Rect::from_min_size(text.pos, text.galley.size()),
                    text.galley.text().to_owned(),
                )),
                _ => None,
            })
            .collect();
        self.last_images = full_output
            .shapes
            .iter()
            .filter_map(|clipped| match &clipped.shape {
                egui::Shape::Mesh(mesh) => painted_image(mesh),
                _ => None,
            })
            .collect();
        self.last_segments = full_output
            .shapes
            .iter()
            .filter_map(|clipped| match &clipped.shape {
                egui::Shape::LineSegment { points, stroke } => {
                    Some((points[0], points[1], *stroke))
                }
                _ => None,
            })
            .collect();
        outcome
    }
}

fn pointer_button(pos: egui::Pos2, pressed: bool) -> egui::Event {
    pointer_button_of(pos, egui::PointerButton::Primary, pressed)
}

fn pointer_button_of(pos: egui::Pos2, button: egui::PointerButton, pressed: bool) -> egui::Event {
    egui::Event::PointerButton {
        pos,
        button,
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

/// The browser `pointerId`s the two fingers arrive under. winit's web backend
/// uses that one number for **both** the touch id and the device id
/// (`window_target.rs:410`), so these deliberately do the same.
const WEB_FINGER_A: u64 = 3;
const WEB_FINGER_B: u64 = 4;

/// A touch exactly as winit's web backend reports it: a device id fabricated
/// per finger from the pointer id.
fn web_touch(pointer_id: u64, phase: egui::TouchPhase, pos: egui::Pos2) -> egui::Event {
    egui::Event::Touch {
        device_id: egui::TouchDeviceId(pointer_id),
        id: egui::TouchId(pointer_id),
        phase,
        pos,
        force: None,
    }
}

#[cfg(test)]
mod tests;
