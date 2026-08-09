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

    /// Every handler dropdown the last frame's layers panel drew.
    pub(crate) fn dropdowns(&self) -> Vec<crate::ui::DrawnDropdown> {
        self.gui.dropdowns_for_test().to_vec()
    }

    /// The `(options, selected)` the handler behind `label` is offering — the
    /// model a [`crate::ui::DrawnDropdown`] is supposed to be a rendering of.
    pub(crate) fn dropdown_model(&self, label: &str) -> Option<(Vec<(String, String)>, String)> {
        self.gui.dropdown_model_for_test(label)
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
    /// pointer tests below, whose subject is the drag rather than the checkbox.
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
    ///
    /// They also have to be *close enough together to say something*. At 90s
    /// the upper claim admitted any backstop up to a minute and a half, so
    /// stretching the shipped minute to 75s left the suite green while the map
    /// stayed hostage to a dead integration a quarter longer. The band below is
    /// 45–70s around a shipped 60: comfortably clear of the longest hold a user
    /// plausibly performs while reading a value, and short enough that "the map
    /// comes back after about a minute" is a claim rather than a gesture.
    const HOLD_MUST_SURVIVE_S: f64 = 45.0;
    const SILENCE_MUST_EXPIRE_S: f64 = 70.0;

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
        assert_eq!(
            pressed.touch.long_press_pos, None,
            "not held long enough yet"
        );
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

    /// 3b. The long press must not fire **early**.
    ///
    ///     Test 3 only pins the late direction — that a 1s hold does raise the
    ///     tooltip — so shortening `LONG_PRESS_DURATION_S` to a hundredth of a
    ///     second passes it. Early is the direction that hurts: the tooltip
    ///     appearing under an ordinary tap, or the moment a pan begins, taking
    ///     the drag away from the map.
    #[test]
    fn a_long_press_does_not_fire_before_its_threshold() {
        let mut h = InputHarness::new();
        let pos = h.map_center();

        h.touch_start(pos);
        // 0.7s of held, motionless finger: still short of LONG_PRESS_DURATION_S.
        h.assert_every_frame_for(0.7, 0.05, |frame, outcome| {
            assert_eq!(
                outcome.touch.long_press_pos, None,
                "frame {frame}: the hold is not old enough to be a long press"
            );
            assert!(
                !outcome.touch.suppress_pan,
                "frame {frame}: and nothing may take the pan yet either"
            );
        });

        // Past it, it does fire — so the assertion above is about *when*, not
        // about the detector having been switched off.
        let held = h.frames_for(4, 0.05);
        assert_eq!(held.touch.long_press_pos, Some(pos));
    }

    /// 3c. A finger that keeps moving never becomes a long press, however long
    ///     it goes on.
    ///
    ///     The cancel-on-movement branch is what separates a hold from a pan,
    ///     and nothing pinned it: widening `LONG_PRESS_MAX_MOVE_PX` to 2000, or
    ///     dropping the branch entirely, raises a tooltip mid-drag and
    ///     suppresses the pan under it. The finger zigzags rather than running
    ///     off in one direction so the distance per frame, not the distance from
    ///     the start, is what the detector has to be reading.
    #[test]
    fn a_moving_finger_never_becomes_a_long_press() {
        let mut h = InputHarness::new();
        let pos = h.map_center();

        h.touch_start(pos);
        h.frame_after(FRAME_DT);
        // 1.2s — comfortably past LONG_PRESS_DURATION_S — of 30px steps.
        for step in 0..12 {
            let jitter = if step % 2 == 0 { 30.0 } else { 0.0 };
            h.touch_move(pos + egui::vec2(0.0, jitter));
            let moving = h.frame_after(0.1);
            assert_eq!(
                moving.touch.long_press_pos, None,
                "step {step}: a finger still moving is a pan, not a hold"
            );
            assert!(
                !moving.touch.suppress_pan,
                "step {step}: pan must stay live"
            );
        }
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
        assert!(
            dragging.touch.suppress_pan,
            "zoom drag must block map panning"
        );
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

    /// 5b. A second tap somewhere else is a separate tap, not a double tap.
    ///
    ///     `DOUBLE_TAP_DISTANCE_PX` is the only thing keeping two unrelated taps
    ///     in quick succession — which is what walking a finger across the map
    ///     looks like — out of a zoom drag. Test 5 taps twice at the same point,
    ///     so it holds neither this bound nor the `&&` joining it to the timeout.
    #[test]
    fn a_second_tap_far_away_does_not_enter_a_zoom_drag() {
        let mut h = InputHarness::new();
        let near = h.map_center();
        let far = near + egui::vec2(200.0, 0.0);
        let zoom_before = h.zoom();

        h.touch_tap(near);
        // Well inside DOUBLE_TAP_TIMEOUT_S, well outside DOUBLE_TAP_DISTANCE_PX.
        h.touch_start(far);
        let pressed = h.frame_after(0.05);
        assert!(
            !pressed.touch.suppress_pan,
            "a tap 200px away is not the second half of a double tap"
        );

        for step in 1..=3 {
            h.touch_move(far + egui::vec2(0.0, 50.0 * step as f32));
            h.frame_after(FRAME_DT);
        }
        let dragged = h.frame_after(FRAME_DT);
        assert!(
            (dragged.zoom - zoom_before).abs() < 1e-9,
            "dragging after an unrelated tap must pan, not zoom: {zoom_before} \
             -> {}",
            dragged.zoom
        );
    }

    /// 5c. …but the second tap does not have to be pixel-exact.
    ///
    ///     The other direction of the same bound, and the one that decides
    ///     whether the gesture is usable at all: a finger never lands twice on
    ///     the same pixel, so a `DOUBLE_TAP_DISTANCE_PX` tightened towards zero
    ///     leaves double-tap-zoom looking simply broken.
    #[test]
    fn a_double_tap_tolerates_the_jitter_between_two_real_taps() {
        let mut h = InputHarness::new();
        let pos = h.map_center();

        h.touch_tap(pos);
        h.touch_start(pos + egui::vec2(10.0, 0.0));
        let pressed = h.frame_after(0.05);
        assert!(
            pressed.touch.suppress_pan,
            "10px between two taps is a double tap, not two singles"
        );
    }

    /// 5c-ii. …and it closes. **The other conjunct of the same classifier.**
    ///
    ///     `handle_press` enters a zoom drag on `dt < DOUBLE_TAP_TIMEOUT_S &&
    ///     dist < DOUBLE_TAP_DISTANCE_PX`. 5b fails only the distance conjunct
    ///     and 5c satisfies both, so nothing failed *only* the timeout: widened
    ///     to 0.5s the whole suite stayed green while a tap and an unrelated
    ///     second tap half a second later silently became a zoom drag —
    ///     the map jumping under a finger that was pointing at something.
    ///
    ///     Both taps land on the same pixel here, so the distance conjunct is
    ///     satisfied throughout and only the timeout can decide. The pair
    ///     straddles the bound, which is what pins it from both sides: a
    ///     timeout shortened towards zero fails the first assertion, because
    ///     two taps a third of a second apart are one ordinary double tap.
    #[test]
    fn the_double_tap_window_closes_between_two_unrelated_taps() {
        /// Tap, sit still for `gap` seconds, then press again and hold.
        /// Reports whether that second press entered a zoom drag.
        fn a_second_press_after(gap: f64) -> bool {
            let mut h = InputHarness::new();
            let pos = h.map_center();

            h.touch_tap(pos);
            // One frame `gap` after the release, so the promotion inside
            // `DoubleTapDragDetector::update` gets a frame to run on, exactly
            // as an idle app does.
            h.frame_after(gap);
            h.touch_start(pos);
            h.frame_after(FRAME_DT).touch.suppress_pan
        }

        assert!(
            a_second_press_after(0.30),
            "two taps a third of a second apart are one double tap — a user \
             cannot double-tap faster than the timeout allows"
        );
        assert!(
            !a_second_press_after(0.45),
            "two taps nearly half a second apart are two taps: the second must \
             pan the map, not zoom it"
        );
    }

    /// 5d. A zoom drag held still is still a zoom drag.
    ///
    ///     The long press is polled only when no zoom drag is running, and
    ///     nothing pinned that exclusion. A user who double-taps and then holds
    ///     before dragging is doing something completely ordinary, and without
    ///     the gate they get the radar-value tooltip on top of the zoom.
    #[test]
    fn a_stationary_zoom_drag_does_not_become_a_long_press() {
        let mut h = InputHarness::new();
        let pos = h.map_center();

        h.touch_tap(pos);
        h.touch_start(pos);
        let dragging = h.frame_after(0.05);
        assert!(
            dragging.touch.suppress_pan,
            "precondition: the second press must have entered a zoom drag"
        );

        h.assert_every_frame_for(1.5, 0.1, |frame, outcome| {
            assert_eq!(
                outcome.touch.long_press_pos, None,
                "frame {frame}: a zoom drag owns the finger, tooltip and all"
            );
        });
    }

    /// 5e. The zoom drag moves one level per `ZOOM_DRAG_SENSITIVITY` pixels.
    ///
    ///     Test 5 pins only the sign — that dragging down zooms in — so a
    ///     tenfold sensitivity passes it while slamming the map from one zoom
    ///     stop to the other on the smallest drag. This pins the rate.
    #[test]
    fn the_zoom_drag_moves_one_level_per_sensitivity_of_travel() {
        let mut h = InputHarness::new();
        let pos = h.map_center();
        let zoom_before = h.zoom();

        h.touch_tap(pos);
        h.touch_start(pos);
        h.frame_after(0.05);
        // Exactly ZOOM_DRAG_SENSITIVITY pixels of travel: one zoom level.
        h.touch_move(pos + egui::vec2(0.0, 150.0));
        let dragged = h.frame_after(FRAME_DT);

        assert!(
            (dragged.zoom - (zoom_before + 1.0)).abs() < 0.05,
            "150px of drag is one zoom level: {zoom_before} -> {}",
            dragged.zoom
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
        assert!(
            dragging.touch.suppress_pan,
            "precondition: zoom drag active"
        );

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
        assert_eq!(
            held.touch.long_press_pos,
            Some(pos),
            "precondition: long press"
        );
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
        assert_eq!(
            gone.touch.long_press_pos, None,
            "the held position must not stick"
        );
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
        assert!(
            h.frame_after(0.05).touch.suppress_pan,
            "precondition: zoom drag"
        );

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
        assert_eq!(
            held.touch.long_press_pos,
            Some(pos),
            "precondition: long press"
        );

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

    /// 7. **PROBE I — the root cause, pinned against egui itself.**
    ///
    ///    egui buckets touches by `TouchDeviceId` and only builds a gesture from
    ///    two touches on one device. winit's web backend fabricates a device id
    ///    per finger, so each finger landed in its own bucket holding one touch
    ///    and `zoom_delta()` never moved off 1.0. Same fingers, same positions —
    ///    only the device ids differ.
    #[test]
    fn a_pinch_only_forms_when_both_fingers_share_a_touch_device() {
        /// Spread two fingers from 100px apart to 200px apart, and report what
        /// egui made of it. `devices` is the device id each finger arrives on.
        fn spread(devices: [u64; 2]) -> (f32, bool) {
            let ctx = egui::Context::default();
            let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1024.0, 768.0));
            let centre = egui::pos2(500.0, 400.0);
            let touch = |dev: u64, id: u64, phase, pos| egui::Event::Touch {
                device_id: egui::TouchDeviceId(dev),
                id: egui::TouchId(id),
                phase,
                pos,
                force: None,
            };
            let pass = |time: f64, half: f32, phase| {
                let first = centre - egui::vec2(half, 0.0);
                egui::RawInput {
                    screen_rect: Some(screen),
                    time: Some(time),
                    events: vec![
                        touch(devices[0], 1, phase, first),
                        // egui refuses to *start* a gesture without a pointer
                        // position (`touch_state.rs:229`). `egui-winit` emulates
                        // one from the first finger, so a bare two-finger event
                        // stream would be unrepresentative without it.
                        egui::Event::PointerMoved(first),
                        touch(devices[1], 2, phase, centre + egui::vec2(half, 0.0)),
                    ],
                    ..Default::default()
                }
            };

            // Three frames, not two. egui hands `TouchState` the *previous*
            // frame's `interact_pos` (`input_state/mod.rs:390`) and refuses to
            // start a gesture without one, so the gesture only begins on the
            // second frame — and begins with no `previous`, i.e. a zoom of 1.0.
            // The third frame is the first that can report a ratio at all.
            ctx.begin_pass(pass(1.0, 50.0, egui::TouchPhase::Start));
            let _ = ctx.end_pass();
            ctx.begin_pass(pass(1.0 + FRAME_DT, 50.0, egui::TouchPhase::Move));
            let _ = ctx.end_pass();
            ctx.begin_pass(pass(1.0 + 2.0 * FRAME_DT, 100.0, egui::TouchPhase::Move));
            let seen = ctx.input(|i| (i.zoom_delta(), i.multi_touch().is_some()));
            let _ = ctx.end_pass();
            seen
        }

        let (web_zoom, web_multi) = spread([WEB_FINGER_A, WEB_FINGER_B]);
        assert!(
            !web_multi,
            "a device id per finger must be what breaks it — if egui now pairs \
             them across devices, `normalize_touch_devices` is obsolete"
        );
        assert_eq!(
            web_zoom, 1.0,
            "this is the bug: two fingers on two devices produce no zoom at all"
        );

        let (shared_zoom, shared_multi) = spread([0, 0]);
        assert!(shared_multi, "one device, two fingers: a gesture must form");
        assert!(
            shared_zoom > 1.9 && shared_zoom < 2.1,
            "doubling the gap must double the zoom factor, got {shared_zoom}"
        );
    }

    /// 7b. The fix in isolation: fingers keep their identities, devices merge.
    ///
    ///     Collapsing the `TouchId`s too would leave egui with one finger and no
    ///     gesture at all, which looks identical from the outside until you
    ///     notice nothing zooms.
    #[test]
    fn normalizing_merges_devices_and_leaves_the_fingers_alone() {
        let mut input = egui::RawInput {
            events: vec![
                web_touch(
                    WEB_FINGER_A,
                    egui::TouchPhase::Start,
                    egui::pos2(10.0, 10.0),
                ),
                web_touch(
                    WEB_FINGER_B,
                    egui::TouchPhase::Start,
                    egui::pos2(90.0, 10.0),
                ),
            ],
            ..Default::default()
        };
        crate::ui_input::normalize_touch_devices(&mut input);

        let seen: Vec<(egui::TouchDeviceId, egui::TouchId)> = input
            .events
            .iter()
            .filter_map(|e| match e {
                egui::Event::Touch { device_id, id, .. } => Some((*device_id, *id)),
                _ => None,
            })
            .collect();

        let devices: std::collections::BTreeSet<_> = seen.iter().map(|(d, _)| *d).collect();
        assert_eq!(devices.len(), 1, "both fingers must land on one device");
        let fingers: std::collections::BTreeSet<_> = seen.iter().map(|(_, f)| *f).collect();
        assert_eq!(
            fingers.len(),
            2,
            "the two fingers must stay distinct, or there is no gesture to form"
        );
    }

    /// 7c. **End to end: pinching out zooms the real map in.**
    ///
    ///     Driven through `Gui::ui`, so this is walkers' own `zoom_delta()` path
    ///     acting on the shipped pane — the thing that was dead in the browser.
    #[test]
    fn a_web_pinch_out_zooms_the_map_in() {
        let mut h = InputHarness::new();
        let centre = h.pane_rects()[0].center();
        let before = h.frame_after(FRAME_DT).resolved_zoom;

        let pinched = h.web_pinch(centre, 80.0, 320.0, 8);

        assert_eq!(
            pinched.modality,
            PointerModality::Touch,
            "two fingers are touch"
        );
        assert!(
            pinched.resolved_zoom > before + 0.2,
            "pinching out must zoom the map in: {before} -> {}",
            pinched.resolved_zoom
        );
    }

    /// 7d. …and pinching in zooms out. Direction, pinned separately, because a
    ///     sign error passes 7c whenever the gap happens to grow.
    #[test]
    fn a_web_pinch_in_zooms_the_map_out() {
        let mut h = InputHarness::new();
        let centre = h.pane_rects()[0].center();
        let before = h.frame_after(FRAME_DT).resolved_zoom;

        let pinched = h.web_pinch(centre, 320.0, 80.0, 8);

        assert!(
            pinched.resolved_zoom < before - 0.2,
            "pinching in must zoom the map out: {before} -> {}",
            pinched.resolved_zoom
        );
    }

    /// 7e. A pinch is not a tap and not a long press. Two fingers resting while
    ///     the gesture runs must not open an overlay popup or raise a tooltip.
    #[test]
    fn a_pinch_is_not_a_tap() {
        let mut h = InputHarness::new();
        let centre = h.pane_rects()[0].center();

        let pinched = h.web_pinch(centre, 80.0, 320.0, 8);
        assert_eq!(
            pinched.resolved.overlay_click_pos, None,
            "a pinch must never resolve as an overlay tap"
        );

        h.web_second_finger_up(centre + egui::vec2(160.0, 0.0));
        h.web_first_finger_up(centre - egui::vec2(160.0, 0.0));
        // Frame by frame, not just the last one: a confirmed tap surfaces
        // exactly [`DOUBLE_TAP_TIMEOUT_S`] after the release and
        // `take_confirmed_tap` consumes it on that one frame, so a run that only
        // reads the final outcome is looking 0.6s after the evidence went away.
        h.assert_every_frame_for(1.0, 0.2, |frame, outcome| {
            assert_eq!(
                outcome.resolved.overlay_click_pos, None,
                "frame {frame}: lifting out of a pinch must not become a \
                 deferred tap either"
            );
        });
    }

    /// 7e-i. A quick flick is not a tap — **distance alone**.
    ///
    ///     The classifier is `duration < A && distance < B`, and the pinch above
    ///     breaks both conjuncts at once, so it holds neither one in place: with
    ///     `TAP_DISTANCE_MAX_PX` widened to 2000 the pinch is still too slow to
    ///     be a tap and 7e passes regardless. This flick is comfortably inside
    ///     the duration bound and only outside the distance one, so it fails if
    ///     and only if the distance bound stops doing its job — which is a drag
    ///     of the map opening an overlay popup under the finger.
    #[test]
    fn a_quick_flick_is_not_a_tap() {
        let mut h = InputHarness::new();
        let pos = h.map_center();

        h.touch_start(pos);
        h.frame_after(FRAME_DT);
        for step in 1..=4 {
            h.touch_move(pos + egui::vec2(30.0 * step as f32, 0.0));
            h.frame_after(FRAME_DT);
        }
        // ~0.08s and 120px: well under TAP_DURATION_MAX_S, well over
        // TAP_DISTANCE_MAX_PX.
        h.touch_end(pos + egui::vec2(120.0, 0.0));

        h.assert_every_frame_for(1.0, 0.1, |frame, outcome| {
            assert_eq!(
                outcome.touch.overlay_click_pos, None,
                "frame {frame}: a 120px flick is a drag, not a tap"
            );
        });
    }

    /// 7e-ii. A slow stationary press is not a tap — **duration alone**.
    ///
    ///     The mirror of 7e-i, and what pins `TAP_DURATION_MAX_S`: the finger
    ///     never moves, so the distance bound is satisfied throughout and only
    ///     the duration bound can reject it. Half a second is past
    ///     `TAP_DURATION_MAX_S` and short of `LONG_PRESS_DURATION_S`, so this is
    ///     the deliberate-but-not-yet-a-long-press hold, which must resolve to
    ///     nothing at all.
    #[test]
    fn a_slow_stationary_press_is_not_a_tap() {
        let mut h = InputHarness::new();
        let pos = h.map_center();

        h.touch_start(pos);
        h.frame_after(FRAME_DT);
        let held = h.frames_for(5, 0.1);
        assert_eq!(
            held.touch.long_press_pos, None,
            "precondition: 0.5s must still be short of a long press, or this \
             probe is testing the long-press path instead"
        );
        h.touch_end(pos);

        h.assert_every_frame_for(1.0, 0.1, |frame, outcome| {
            assert_eq!(
                outcome.touch.overlay_click_pos, None,
                "frame {frame}: a 0.5s hold is not a tap"
            );
        });
    }

    /// 7f. **A pinch that ends one finger at a time must not strand the map.**
    ///
    ///     The dangerous order is the *first* finger going up while the second
    ///     stays down: that is the one backing the emulated pointer, so
    ///     `egui-winit` fires a release and a `PointerGone` while a finger is
    ///     still on the glass. The second finger's later events must not put the
    ///     map back into a suppressed state.
    #[test]
    fn a_pinch_ending_one_finger_at_a_time_leaves_the_map_pannable() {
        let mut h = InputHarness::new();
        let centre = h.pane_rects()[0].center();

        h.web_pinch(centre, 80.0, 320.0, 8);

        // The emulated-pointer finger leaves first; the other is still down.
        let left = centre - egui::vec2(160.0, 0.0);
        let right = centre + egui::vec2(160.0, 0.0);
        h.web_first_finger_up(left);
        let lifted = h.frame_after(FRAME_DT);
        assert!(
            !lifted.resolved.suppress_pan,
            "the map must be released the moment the primary finger goes"
        );

        // The survivor keeps moving, then lifts.
        for step in 1..=4 {
            h.events.push(web_touch(
                WEB_FINGER_B,
                egui::TouchPhase::Move,
                right + egui::vec2(0.0, 10.0 * step as f32),
            ));
            let moving = h.frame_after(FRAME_DT);
            assert!(
                !moving.resolved.suppress_pan,
                "step {step}: a leftover finger must not re-suppress panning"
            );
        }
        h.web_second_finger_up(right + egui::vec2(0.0, 40.0));

        // And it must stay that way well past the long-press threshold.
        h.assert_every_frame_for(WATCH_PAST_LONG_PRESS, 0.1, |frame, outcome| {
            assert!(
                !outcome.resolved.suppress_pan,
                "frame {frame}: map must remain pannable after a pinch ends"
            );
            assert_eq!(
                outcome.resolved.long_press_pos, None,
                "frame {frame}: a finished pinch must not become a long press"
            );
        });
    }

    /// 7g. **A wheel notch must zoom the same however the browser spelled it.**
    ///
    ///     One detent of the same wheel: Chromium reports `deltaY: 120` in
    ///     `DOM_DELTA_PIXEL`, Firefox `deltaY: 6` in `DOM_DELTA_LINE`. Those two
    ///     numbers come from two different browsers, which is what makes this
    ///     test independent of [`PX_PER_WHEEL_LINE`] — anchoring the line side on
    ///     the pixel number the *same* browser would have sent only restates the
    ///     constant. Without the rewrite egui scales the line form by
    ///     `line_scroll_speed`, 8.0 on web, so Firefox's notch lands as 48
    ///     against Chromium's 120 and the map zooms 2.5x slower. Nothing errors;
    ///     the wheel just feels wrong in one browser, which is why this is pinned
    ///     on the ratio rather than on any absolute step.
    ///
    ///     The band is 2% because the ratio at the shipped constant is exactly
    ///     1.0 — `6 × 20` *is* 120, so the two runs see identical events. A 5%
    ///     error in `PX_PER_WHEEL_LINE` moves it 5%, so anything looser stops
    ///     being a calibration and starts being a smoke test.
    #[test]
    fn a_wheel_notch_zooms_the_same_in_either_wheel_unit() {
        /// Four notches over the pane centre, and the zoom they moved.
        fn notches(unit: egui::MouseWheelUnit, delta_y: f32) -> f64 {
            let mut h = InputHarness::new();
            let centre = h.pane_rects()[0].center();
            let before = h.frame_after(FRAME_DT).resolved_zoom;
            let mut last = before;
            for _ in 0..4 {
                h.wheel_notch(centre, unit, delta_y);
                // egui bleeds a wheel impulse out over several frames, so the
                // step is only whole once the smoothing has drained.
                last = h.frames_for(12, FRAME_DT).resolved_zoom;
            }
            last - before
        }

        // Negative delta is a scroll *down*, which walkers zooms out on; take
        // the magnitude so the two are compared on the same axis.
        let chromium = notches(egui::MouseWheelUnit::Point, 120.0);
        let firefox = notches(egui::MouseWheelUnit::Line, 6.0);

        assert!(
            chromium.abs() > 0.5,
            "precondition: a pixel-mode notch must zoom at all, got {chromium}"
        );
        let ratio = firefox / chromium;
        assert!(
            (0.98..=1.02).contains(&ratio),
            "one notch must move the map the same in either browser: \
             Chromium {chromium}, Firefox {firefox} (ratio {ratio})"
        );
    }

    /// 7h. The rewrite in isolation: units converge, everything else survives.
    #[test]
    fn normalizing_wheel_units_converts_only_the_line_events() {
        let wheel = |unit, delta: egui::Vec2| egui::Event::MouseWheel {
            unit,
            delta,
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::CTRL,
        };
        let mut input = egui::RawInput {
            events: vec![
                wheel(egui::MouseWheelUnit::Line, egui::vec2(2.0, 6.0)),
                wheel(egui::MouseWheelUnit::Point, egui::vec2(0.0, 120.0)),
            ],
            ..Default::default()
        };
        crate::ui_input::normalize_wheel_units(&mut input, 1.0);

        let seen: Vec<_> = input
            .events
            .iter()
            .filter_map(|e| match e {
                egui::Event::MouseWheel {
                    unit,
                    delta,
                    phase,
                    modifiers,
                } => Some((*unit, *delta, *phase, *modifiers)),
                _ => None,
            })
            .collect();

        assert!(
            seen.iter().all(|(u, ..)| *u == egui::MouseWheelUnit::Point),
            "every wheel event must leave in point units, got {seen:?}"
        );
        // 6 lines is the notch Firefox reports; 20 px a line makes it the 120 px
        // Chromium reports for the same detent.
        assert_eq!(seen[0].1, egui::vec2(40.0, 120.0), "line delta must scale");
        assert_eq!(
            seen[1].1,
            egui::vec2(0.0, 120.0),
            "a point delta was already normal and must not be touched"
        );
        assert_eq!(
            seen[0].2,
            egui::TouchPhase::Move,
            "phase must survive — egui starts and ends wheel gestures on it"
        );
        assert_eq!(
            seen[0].3,
            egui::Modifiers::CTRL,
            "modifiers must survive — ctrl is what routes a wheel to zoom"
        );
    }

    /// 7i. The app's UI scale must not change the wheel step in one unit only.
    ///
    ///     `egui-winit` divides pixel deltas by `pixels_per_point`, which folds
    ///     in the zoom factor; line deltas never went through it. Left alone,
    ///     turning up the UI scale would slow the wheel in one browser and not
    ///     the other.
    #[test]
    fn the_wheel_rewrite_divides_by_the_zoom_factor() {
        let mut input = egui::RawInput {
            events: vec![egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Line,
                delta: egui::vec2(0.0, 6.0),
                phase: egui::TouchPhase::Move,
                modifiers: egui::Modifiers::default(),
            }],
            ..Default::default()
        };
        crate::ui_input::normalize_wheel_units(&mut input, 2.0);

        let egui::Event::MouseWheel { delta, .. } = input.events[0] else {
            panic!("expected a wheel event");
        };
        assert_eq!(
            delta.y, 60.0,
            "a 2x UI scale must halve the rewritten delta, as it already does \
             to the pixel deltas egui-winit produced"
        );
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
    ///    pinning was that `render_panes` resolves from the panel, that the
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

    /// 8c. **The hail-size preference reaches the MEHS colour bar on the glass.**
    ///
    ///     `format_legend_value` and `RadarProduct::unit_label` are pinned
    ///     directly in `ui_map_pane.rs`; this is the other end of the same wire.
    ///     What it adds is that the preference the settings dialog writes is the
    ///     one `render_color_scale` is handed — it travels the whole way through
    ///     `Gui::ui` and `PaneRenderCtx` — and that a unit change relabels every
    ///     tick, not just the title above them. A bar reading `in` over
    ///     millimetre numbers, or millimetre ticks under an inch title, is the
    ///     half-converted state this rules out.
    #[test]
    fn the_mehs_colour_bar_paints_the_users_hail_size_unit() {
        use rustdar_radar::types::RadarProduct;
        use rustdar_units::HailSizeUnit;

        /// The ¼-in stops of `palette::MEHS` as the bar labels them in inches.
        const INCH_TICKS: [&str; 8] = ["0.2", "0.5", "0.8", "1.2", "1.5", "1.8", "2.5", "3.5"];
        /// The same stops in whole millimetres, 1.00 in landing on 25.
        const MM_TICKS: [&str; 12] = [
            "6", "13", "19", "25", "32", "38", "44", "51", "64", "76", "89", "102",
        ];

        let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
        h.select_product(0, RadarProduct::MaxExpectedHailSize);
        let pane = h.pane_rects()[0];

        // The default nobody has to choose.
        let painted = h.painted_text_strings_in(pane);
        assert!(
            painted.iter().any(|t| t == "in"),
            "no `in` title over the default MEHS bar; painted: {painted:?}",
        );
        for tick in INCH_TICKS {
            assert!(
                painted.iter().any(|t| t == tick),
                "the inch bar is missing its {tick} tick; painted: {painted:?}",
            );
        }

        h.gui_mut().preferences.hail_size = HailSizeUnit::Millimeters;
        h.warm_up();

        let painted = h.painted_text_strings_in(pane);
        assert!(
            painted.iter().any(|t| t == "mm"),
            "the bar still is not titled `mm`; painted: {painted:?}",
        );
        for tick in MM_TICKS {
            assert!(
                painted.iter().any(|t| t == tick),
                "the mm bar is missing its {tick} tick; painted: {painted:?}",
            );
        }
        assert!(
            !painted.iter().any(|t| t == "in"),
            "`in` is still over a bar labelled in millimetres; painted: {painted:?}",
        );
        for tick in INCH_TICKS {
            assert!(
                !painted.iter().any(|t| t == tick),
                "the {tick} inch tick survived the switch to millimetres; \
                 painted: {painted:?}",
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
        assert!(
            dragged.resolved.suppress_pan,
            "the zoom drag owns the pointer"
        );
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
    ///     that exists in a one-pane layout: `render_panes` would resolve exactly
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

    /// A click can only hand the active-pane slot to a pane that exists.
    ///
    /// `Gui::active_pane` resolves the slot as `self.panes[self.active_pane]`, so
    /// an index the layout drew a cell for but the vector never grew to reach is
    /// not a pane that quietly goes unpainted — it is a panic waiting for the next
    /// reader, and `render_layers_panel`'s `mem::take` is one of them. The skew is
    /// built by hand because no production writer can produce it: both grow the
    /// vector before assigning the layout. See `Gui::claim_pane_count_for_test`.
    #[test]
    fn a_click_on_a_cell_no_pane_occupies_leaves_the_active_pane_alone() {
        let mut h = InputHarness::new();
        h.set_pane_count(2);
        h.claim_pane_count(4);
        let panel = h.map_panel_rect();

        // The 2×2 grid's bottom-right cell: a rect for pane 3, which no
        // `PaneState` occupies.
        let ghost = crate::pane::PaneLayout::for_count(4)
            .pane_rect(3, panel)
            .center();
        assert!(
            h.pane_rects().iter().all(|r| !r.contains(ghost)),
            "precondition: the click lands outside every pane the frame drew"
        );

        h.mouse_click(ghost);

        assert_eq!(h.active_pane_index(), 0);
        assert_eq!(
            h.gui_mut().active_pane().site,
            "KTLX",
            "the slot still resolves to a pane rather than panicking"
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
        // handed it. `render_panes(&mut root_ui, &[])` leaves every assertion
        // above true and the button transparent to clicks.
        assert_eq!(
            h.map_excluded_rects(),
            rects,
            "the chrome's rects never reached render_panes, so nothing downstream \
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
            (
                egui::vec2(420.0, 800.0),
                crate::ui_layout::WidthClass::Compact,
            ),
            (
                egui::vec2(800.0, 800.0),
                crate::ui_layout::WidthClass::Medium,
            ),
            (
                egui::vec2(1400.0, 900.0),
                crate::ui_layout::WidthClass::Expanded,
            ),
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
            crate::ui::VOLUME_PANE_LABEL,
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
        assert_eq!(
            bare.bottom() - inset.bottom(),
            BOTTOM,
            "bottom inset ignored"
        );

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
            roomy_bar.scan_text.contains("2 products")
                && roomy_bar.scan_text.contains("2026-07-24"),
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

    /// Data collected `ago` before now.
    ///
    /// Offset by half a minute so the whole-minute truncation in
    /// `format_product_age` cannot land on a boundary and read one lower while
    /// the test runs.
    fn written_ago(minutes: i64) -> chrono::NaiveDateTime {
        chrono::Utc::now().naive_utc() - chrono::Duration::seconds(minutes * 60 + 30)
    }

    /// 26b. **Every product says how old the data behind it is, the same way.**
    ///
    ///      The scan line beside this is the *volume* time and answers a
    ///      different question. For a product fetched from the Level III bucket
    ///      the two can be days apart — `level3::latest_key` falls back to the
    ///      previous UTC day, so a site that went down yesterday paints a field
    ///      up to ~48h old under a scan line that looks perfectly current.
    ///
    ///      The line used to read `Level III:` and be drawn *only* for the
    ///      bucket-fetched products, which made the datasource readable off the
    ///      status bar and made its absence informative too. It is now one
    ///      uniform line: same label, same format, drawn whenever the pane knows
    ///      when its data was collected, whatever produced it.
    ///
    ///      Asserted on the text egui laid out inside the bar's own rect, not
    ///      just on the probe: a probe records what the renderer was handed,
    ///      and the thing that matters is what reached the glass.
    #[test]
    fn every_products_data_age_is_drawn_the_same_way_in_the_status_bar() {
        let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
        h.load_scan("KTLX");
        assert_eq!(
            h.status_bar().product_age_text,
            None,
            "precondition: a pane with no render yet has no data time to report, \
             so the line below is not simply always there"
        );

        h.set_data_time(0, Some(written_ago(23)));

        let bar = h.status_bar();
        let drawn = bar
            .product_age_text
            .as_deref()
            .expect("a pane showing an image must report when its data was collected");
        assert!(
            drawn.starts_with("Data:") && drawn.contains("(23 min old)"),
            "the roomy bar should give the data time and its age, got {drawn:?}"
        );
        assert!(
            !drawn.contains("Level III") && !drawn.contains("L3"),
            "the line must not name a datasource: {drawn:?}"
        );
        assert!(
            h.text_painted_in(bar.rect, "23 min old"),
            "the age never reached the glass: nothing was painted inside the \
             status bar rect {:?}. Painted: {:?}",
            bar.rect,
            h.painted_text_strings()
        );
    }

    /// 26d. **A looping pane dates the frame it is playing, not the still it
    ///      replaced.**
    ///
    ///      The bar used to draw nothing at all while a loop ran, because
    ///      `data_time` describes the static render the animation stands in
    ///      for — captioning someone else's picture. The frame's own volume time
    ///      is the right answer, and it is the same answer whichever datasource
    ///      the loop reads, since a loop frame *is* a volume.
    #[test]
    fn a_looping_pane_reports_its_current_frames_time() {
        let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
        h.load_scan("KTLX");
        // A time the static render would never report, so the two are telling.
        h.set_data_time(0, Some(written_ago(90)));

        let frame_time = written_ago(7);
        {
            let pane = h.gui_mut().pane_mut(0).unwrap();
            pane.loop_state = crate::pane::LoopPlaybackState::new_for_loop(
                600,
                rustdar_radar::sites::get_radar_site("KTLX").unwrap(),
            );
            pane.loop_state.frames = vec![crate::pane::LoopFrame {
                timestamp: frame_time,
                texture: None,
                render_in_flight: false,
                render_failed: false,
            }];
            pane.loop_state.current_frame = 0;
        }
        h.warm_up();

        let drawn = h
            .status_bar()
            .product_age_text
            .expect("a looping pane still reports a data time");
        assert!(
            drawn.contains("(7 min old)"),
            "the playing frame's own time must be reported, got {drawn:?}"
        );
        assert!(
            !drawn.contains("90 min old"),
            "the static render's time captioned the animation: {drawn:?}"
        );
    }

    /// 26e. **A product whose tilts have not arrived keeps its tilt picker.**
    ///
    ///      Only Level III products can be in this state: `ScanInfo::from_scan`
    ///      lists them the moment a volume loads and fills their angle in when the
    ///      fetch lands, and every archive poll rebuilds `ScanInfo` from the volume
    ///      alone, so the window reopens on every poll. Skipping the combo box
    ///      while the list was empty made the control *vanish* and the layers panel
    ///      reflow around it — visible on first selection and then flickering once
    ///      a minute, which is exactly the kind of thing that tells a user this
    ///      product is not like the others.
    #[test]
    fn a_product_whose_tilts_have_not_arrived_keeps_its_tilt_picker() {
        use rustdar_radar::types::RadarProduct;

        let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
        h.load_scan("KTLX");
        {
            let pane = h.gui_mut().pane_mut(0).unwrap();
            pane.set_overlay_enabled(OverlayKind::Radar, true);
            let info = pane.scan_info.as_mut().expect("a scan was loaded");
            info.product_elevations
                .insert(RadarProduct::Reflectivity, vec![0.5, 1.5]);
            // As `from_scan` lists a Level III product: available, no angles yet.
            info.available_products.push(RadarProduct::EchoTops);
            info.product_elevations
                .insert(RadarProduct::EchoTops, Vec::new());
            pane.selected_product = RadarProduct::EchoTops;
            pane.selected_elevation = 0.0;
        }
        h.warm_up();

        assert!(
            h.painted_text_strings().iter().any(|t| t == "0.0\u{b0}"),
            "the tilt picker vanished for a product whose angles have not landed; \
             painted: {:?}",
            h.painted_text_strings(),
        );
        // And the render path agrees: the selection stands, so a render is
        // dispatched rather than the previous product's image being held.
        assert_eq!(
            h.gui_mut().pane(0).unwrap().get_rendering_params(),
            Some((RadarProduct::EchoTops, 0.0)),
        );

        // The populated case still lists its angles, so the assertion above is
        // about a *present but empty* picker rather than about a combo box that
        // never shows anything.
        h.gui_mut().pane_mut(0).unwrap().selected_product = RadarProduct::Reflectivity;
        h.warm_up();
        assert!(
            h.painted_text_strings().iter().any(|t| t == "0.5\u{b0}"),
            "painted: {:?}",
            h.painted_text_strings(),
        );
    }

    /// A harness with one pane on KTLX offering a Level II and a Level III
    /// product at 0.5°, radar layer on, showing a finished `showing` image.
    ///
    /// Both products at the same angle deliberately: it makes the product the only
    /// thing that differs between the two selections, so a notice that appeared
    /// would be about the product switch and nothing else.
    fn pane_showing(showing: rustdar_radar::types::RadarProduct) -> InputHarness {
        use rustdar_radar::types::RadarProduct;

        let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
        h.load_scan("KTLX");
        h.gui_mut()
            .pane_mut(0)
            .unwrap()
            .set_overlay_enabled(OverlayKind::Radar, true);
        h.offer_product(0, RadarProduct::Reflectivity, 0.5);
        h.offer_product(0, RadarProduct::EchoTops, 0.5);
        h.select_product(0, showing);
        h.place_radar_image(0, showing, 0.5);
        h
    }

    /// The pending-render notice for `product`, as the pane paints it.
    ///
    /// Read out of the *painted* text rather than from `stale_image_on_screen`, so
    /// what is asserted is what a user would be looking at. Matched on the product
    /// name the notice names — the one on screen — because that is the whole
    /// message.
    fn notice_painted(h: &InputHarness, product: rustdar_radar::types::RadarProduct) -> bool {
        h.painted_text_strings()
            .iter()
            .any(|t| t.starts_with('\u{27f3}') && t.contains(product.name()))
    }

    /// Any pending-render notice at all, whatever it names.
    fn any_notice_painted(h: &InputHarness) -> bool {
        h.painted_text_strings()
            .iter()
            .any(|t| t.starts_with('\u{27f3}'))
    }

    /// 26f. **A pane says when its image is not the product it is labelled with.**
    ///
    ///      Switching product holds the previous product's image until the new
    ///      render lands, while the color scale, the tilt picker and the status
    ///      bar's data line have all already moved to the new selection. That is a
    ///      label claiming something the pixels do not show — a small correctness
    ///      problem, not a cosmetic one — and it lasts as long as a render, longer
    ///      for a Level III product whose object has not landed.
    ///
    ///      The notice names what is *on screen*, since everything else on the
    ///      pane already names the selection. The imagery stays up and undimmed
    ///      behind it: one product's echoes are better than none.
    #[test]
    fn a_pane_says_when_its_image_is_not_the_selected_product() {
        use rustdar_radar::types::RadarProduct;

        let mut h = pane_showing(RadarProduct::Reflectivity);
        assert!(
            !any_notice_painted(&h),
            "a pane showing what it selected has nothing to disown; painted: {:?}",
            h.painted_text_strings(),
        );

        // The switch, with no render landed yet.
        h.select_product(0, RadarProduct::EchoTops);
        assert!(
            notice_painted(&h, RadarProduct::Reflectivity),
            "the pane is showing reflectivity and labelled echo tops, and said \
             nothing; painted: {:?}",
            h.painted_text_strings(),
        );
        // Over the pane it is about, not somewhere in the chrome: in a split
        // layout each pane answers for its own image.
        let pane_rect = h.pane_rects()[0];
        assert!(
            h.text_painted_in(pane_rect, "showing Reflectivity"),
            "the notice was painted outside the pane it describes; painted: {:?}",
            h.painted_text_strings(),
        );
        // …and the picture is still there. The notice is drawn over the imagery,
        // never instead of it.
        assert!(
            h.gui_mut()
                .pane(0)
                .unwrap()
                .overlay_cache(OverlayKind::Radar)
                .and_then(|c| c.current.as_ref())
                .is_some(),
            "the pane was cleared rather than annotated",
        );

        // The render lands and the notice goes.
        h.place_radar_image(0, RadarProduct::EchoTops, 0.5);
        assert!(
            !any_notice_painted(&h),
            "the notice outlived the render it was waiting for; painted: {:?}",
            h.painted_text_strings(),
        );
    }

    /// 26g. **…and it does not flash on a routine refresh.**
    ///
    ///      A new volume for the site drops every pane's `last_rendered` and
    ///      re-renders it, several times a scan under the real-time feed. The image
    ///      on screen still depicts the selected product throughout, so there is
    ///      nothing to disown — which is why the notice is derived from the
    ///      *image's* own metadata rather than from "is a render in flight".
    #[test]
    fn a_same_selection_re_render_draws_no_notice() {
        use rustdar_radar::types::RadarProduct;

        let mut h = pane_showing(RadarProduct::Reflectivity);
        // Two more volumes' worth of the same selection re-rendered, as an
        // auto-poll or the chunk feed produces.
        for _ in 0..2 {
            h.place_radar_image(0, RadarProduct::Reflectivity, 0.5);
            assert!(
                !any_notice_painted(&h),
                "a routine re-render of the selected product drew a notice; \
                 painted: {:?}",
                h.painted_text_strings(),
            );
        }

        // Nor does a selection the scan snaps back onto the sweep already drawn:
        // 0.6° snaps to the 0.5° sweep, which is the image on screen.
        h.gui_mut().pane_mut(0).unwrap().selected_elevation = 0.6;
        h.warm_up();
        assert_eq!(
            h.gui_mut().pane(0).unwrap().get_rendering_params(),
            Some((RadarProduct::Reflectivity, 0.5)),
            "precondition: the selection snaps to the drawn sweep",
        );
        assert!(
            !any_notice_painted(&h),
            "the snapped selection is the image on screen; painted: {:?}",
            h.painted_text_strings(),
        );
    }

    /// 26h. **The notice is the same for a Level II and a Level III product.**
    ///
    ///      The point of the parity work is that a user cannot tell the two
    ///      datasources apart, so a notice that appeared for only one of them — or
    ///      worded itself differently — would be a way to read the datasource off
    ///      the screen, exactly the tell the uniform data line removed. Driven both
    ///      ways round through the real UI so the claim is about what is painted
    ///      rather than about a shared code path.
    #[test]
    fn the_pending_notice_is_identical_for_both_datasources() {
        use rustdar_radar::types::RadarProduct;

        // A Level III selection over a Level II image, and the reverse.
        let (l2, l3) = (RadarProduct::Reflectivity, RadarProduct::EchoTops);
        assert!(!l2.is_level3() && l3.is_level3(), "one of each datasource");

        let mut awaiting_l3 = pane_showing(l2);
        awaiting_l3.select_product(0, l3);
        let mut awaiting_l2 = pane_showing(l3);
        awaiting_l2.select_product(0, l2);

        let notice_of = |h: &InputHarness| -> String {
            h.painted_text_strings()
                .iter()
                .find(|t| t.starts_with('\u{27f3}'))
                .cloned()
                .unwrap_or_else(|| {
                    panic!(
                        "no pending-render notice painted: {:?}",
                        h.painted_text_strings()
                    )
                })
        };
        assert_eq!(
            notice_of(&awaiting_l3),
            "\u{27f3} showing Reflectivity 0.5\u{b0}"
        );
        assert_eq!(
            notice_of(&awaiting_l2),
            "\u{27f3} showing Echo Tops 0.5\u{b0}"
        );

        // The wording is one format string with the product substituted, so the
        // two differ in the product name and nowhere else.
        let strip = |t: &str, name: &str| t.replace(name, "<product>");
        assert_eq!(
            strip(&notice_of(&awaiting_l3), l2.name()),
            strip(&notice_of(&awaiting_l2), l3.name()),
            "the two datasources drew differently shaped notices, which is a way \
             to tell them apart",
        );

        // And each one clears on its own render landing, the same way.
        awaiting_l3.place_radar_image(0, l3, 0.5);
        awaiting_l2.place_radar_image(0, l2, 0.5);
        assert!(!any_notice_painted(&awaiting_l3));
        assert!(!any_notice_painted(&awaiting_l2));
    }

    /// 26i. **A pane with no image says nothing, and neither does a looping one.**
    ///
    ///      Two states with nothing to report, for opposite reasons. An empty pane
    ///      makes no claim its pixels contradict — there are none, and the site
    ///      spinner already covers a first load. A looping pane never *holds* a
    ///      stale frame: `retarget_renders` drops every frame texture the instant
    ///      the selection moves, so the animation is blank rather than wrong, and
    ///      the loop's own phase chrome covers the wait.
    #[test]
    fn nothing_is_said_where_there_is_no_stale_image() {
        use rustdar_radar::types::RadarProduct;

        // No image at all, on a selection nothing has rendered.
        let mut bare = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
        bare.load_scan("KTLX");
        bare.gui_mut()
            .pane_mut(0)
            .unwrap()
            .set_overlay_enabled(OverlayKind::Radar, true);
        bare.offer_product(0, RadarProduct::EchoTops, 0.5);
        bare.select_product(0, RadarProduct::EchoTops);
        assert!(
            !any_notice_painted(&bare),
            "an empty pane has no pixels to disown; painted: {:?}",
            bare.painted_text_strings(),
        );

        // A looping pane, mid-switch. The static image's metadata still describes
        // the old product, and must not be reported: it is not what is on screen.
        let mut looping = pane_showing(RadarProduct::Reflectivity);
        let site = rustdar_radar::sites::get_radar_site("KTLX").expect("a real radar");
        {
            let pane = looping.gui_mut().pane_mut(0).unwrap();
            pane.loop_state = crate::pane::LoopPlaybackState::new_for_loop(600, site);
        }
        looping.select_product(0, RadarProduct::EchoTops);
        assert!(
            looping.gui_mut().pane(0).unwrap().loop_state.is_active(),
            "precondition: the loop is running",
        );
        assert!(
            !any_notice_painted(&looping),
            "a looping pane drew the static image's notice; painted: {:?}",
            looping.painted_text_strings(),
        );
    }

    /// 26c. **…and day-old data reads as hours, not as 1,560 minutes.**
    ///
    ///      This is the case the line exists for. `level3::latest_key` falls
    ///      back to the previous UTC day when today's prefix is empty, so the
    ///      product a downed site serves is not a few minutes stale, it is
    ///      most of a day — and a bar that only ever counted minutes would say
    ///      so in a unit nobody reads at a glance.
    #[test]
    fn day_old_data_reads_in_hours() {
        let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
        h.load_scan("KTLX");
        h.set_data_time(0, Some(written_ago(26 * 60 + 5)));

        let bar = h.status_bar();
        assert_eq!(
            bar.product_age_text
                .as_deref()
                .map(|t| t.contains("(26h 5m old)")),
            Some(true),
            "26-hour-old data must read in hours, got {:?}",
            bar.product_age_text
        );
        assert!(
            h.text_painted_in(bar.rect, "26h 5m old"),
            "…and be painted: {:?}",
            h.painted_text_strings()
        );

        // A narrow bar drops the date, as the scan line beside it does, but
        // never the age — that is the whole message.
        let mut phone = InputHarness::with_screen(egui::vec2(420.0, 900.0));
        phone.load_scan("KTLX");
        phone.set_data_time(0, Some(written_ago(26 * 60 + 5)));
        let compact = phone.status_bar();
        let drawn = compact
            .product_age_text
            .as_deref()
            .expect("a compact bar must still report the age");
        assert!(
            drawn.starts_with("Data ") && drawn.contains("(26h 5m old)"),
            "the compact form should be short and still carry the age, got \
             {drawn:?}"
        );
        assert!(
            !drawn.contains("Data:"),
            "the compact bar drew the roomy form: {drawn:?}"
        );
        assert!(
            !drawn.contains("L3") && !drawn.contains("Level III"),
            "nor may the compact form name a datasource: {drawn:?}"
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
            unclamped
                .iter()
                .any(|p| p.width > LIMIT || p.height > LIMIT),
            "fixture must cross the limit before it is imposed, else the clamp is never \
             exercised; got {unclamped:?}"
        );

        h.set_max_texture_side(LIMIT as usize);
        let clamped = requested_plans(&h);
        assert!(
            !clamped.is_empty(),
            "still expected a render request after clamping"
        );
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

    // ── Radar site icons: from the click to the action ───────────────────

    /// Off-centre, but well inside a 24pt icon. A click at the exact centre
    /// still lands inside a zero-sized `Rect`, so a hit-test collapsed to
    /// nothing would pass there.
    const INSIDE_THE_ICON: egui::Vec2 = egui::vec2(5.0, 5.0);

    /// The site switches the last frame asked the app for.
    fn site_switches(h: &InputHarness) -> Vec<(String, usize)> {
        h.last_actions()
            .iter()
            .filter_map(|a| match a {
                GuiAction::SwitchRadarSite { site, pane_idx } => Some((site.clone(), *pane_idx)),
                _ => None,
            })
            .collect()
    }

    /// A harness showing `site`, with the radar-site overlay on, plus the
    /// screen position that site's icon is drawn at.
    ///
    /// `render_panes` centres a pane on its own scan's site, so the icon lands on
    /// the pane centre. That comes from the layout, not from the hit-testing
    /// under test, so shrinking or inflating the icon cannot move it.
    fn harness_showing_site(site: &str) -> (InputHarness, egui::Pos2) {
        let mut h = InputHarness::new();
        h.load_scan(site);
        h.gui_mut().enable_overlay_for_test(OverlayKind::RadarSites);
        h.warm_up();
        assert!(
            h.overlay_enabled(OverlayKind::RadarSites),
            "precondition: the radar-site overlay must be on, or nothing draws \
             an icon to click"
        );
        let icon = h.pane_rects()[0].center();
        assert!(
            !h.is_floating_layer_at(icon),
            "precondition: the icon must not already be under a floating layer"
        );
        (h, icon)
    }

    /// 30. **Clicking a radar site icon switches to that radar.**
    ///
    ///     The click resolves — that is what the whole pointer suite pins — but
    ///     nothing checked that the action it is supposed to produce ever
    ///     reached the app. Two things were eating it, and either alone is
    ///     enough to make every site unselectable on every platform:
    ///     `handle_radar_site_interactions` was handed the excluded rects with
    ///     *the site icons already in them*, and the readout it opens on hover
    ///     was an interactable layer sitting over the pointer, which the dialog
    ///     gate then read as a click on a floating window.
    #[test]
    fn clicking_a_radar_site_icon_switches_to_that_site() {
        let (mut h, icon) = harness_showing_site("KTLX");
        let target = icon + INSIDE_THE_ICON;

        // Rest on the icon first, as a mouse user does. That opens the site
        // readout, and a readout that takes part in layer hit-testing then eats
        // the click it was opened by — the pointer is inside it.
        h.mouse_move(target);
        h.frames_for(3, FRAME_DT);
        assert!(
            !h.is_floating_layer_at(target),
            "the readout claimed the pointer, so the dialog gate will read the \
             click that follows as landing on a floating window"
        );

        h.mouse_click(target);

        assert_eq!(
            site_switches(&h),
            vec![("KTLX".to_owned(), 0)],
            "clicking KTLX's icon did not ask the app to switch to KTLX"
        );
    }

    /// 31. **Tapping one switches too.**
    ///
    ///     The same handler under the other modality, which reaches it by a
    ///     different route: the touch pipeline confirms the tap only after
    ///     `DOUBLE_TAP_TIMEOUT_S`, a frame on which nothing is pressed at all.
    #[test]
    fn tapping_a_radar_site_icon_switches_to_that_site() {
        let (mut h, icon) = harness_showing_site("KTLX");

        h.touch_tap(icon + INSIDE_THE_ICON);
        h.frame_after(AFTER_DOUBLE_TAP_TIMEOUT);

        assert_eq!(
            site_switches(&h),
            vec![("KTLX".to_owned(), 0)],
            "tapping KTLX's icon did not ask the app to switch to KTLX"
        );
    }

    /// 32. **...and clicking beside one does not.**
    ///
    ///     The complement: without it an icon stretched over the pane satisfies
    ///     the two above while turning every map click into a site switch.
    ///     40pt out is comfortably clear of a 24pt icon and still far nearer
    ///     than the next radar, which at this zoom is hundreds of points away.
    #[test]
    fn clicking_beside_a_radar_site_icon_switches_nothing() {
        let (mut h, icon) = harness_showing_site("KTLX");
        let beside = icon + egui::vec2(40.0, 0.0);
        assert!(
            h.pane_rects()[0].contains(beside),
            "precondition: the spot must still be on the map"
        );

        h.mouse_click(beside);

        assert_eq!(
            site_switches(&h),
            vec![],
            "a click 40pt clear of the icon still switched sites"
        );
    }

    /// 32b. **The pane rect is what keeps a click off the map off the map.**
    ///
    ///      `is_pos_blocked`'s three conditions mask each other everywhere they
    ///      normally meet, so each has to be reached alone. This is the
    ///      pane-rect one: a site icon straddling the pane's bottom edge, and
    ///      two clicks 10pt apart — one on the map, one in the status bar. The
    ///      icon's own hit-test cannot tell them apart, and neither can the
    ///      other two conditions (nothing is excluded on a wide screen, and a
    ///      panel is a background layer), so only the pane rect stands between
    ///      a click on the chrome and a radar site change.
    ///
    ///      `screen_rect.expand(100.0)` in `render_pane_map_content` is what
    ///      makes this reachable at all: sites just off the pane are still
    ///      drawn and still hit-tested, so "off the pane" is not implied by
    ///      "not drawn".
    #[test]
    fn a_click_outside_the_pane_does_not_reach_a_site_icon_straddling_its_edge() {
        let mut h = InputHarness::new();
        h.gui_mut().enable_overlay_for_test(OverlayKind::RadarSites);
        h.warm_up();

        let pane = h.pane_rects()[0];
        let edge = egui::pos2(pane.center().x, pane.bottom());
        h.place_site_at(0, "KTLX", edge);

        // 5pt either side of the edge: both well inside an 18pt icon, so the
        // icon hit-test says yes to both.
        let on_map = edge - egui::vec2(0.0, 5.0);
        let off_pane = edge + egui::vec2(0.0, 5.0);
        let pane = h.pane_rects()[0];
        assert!(
            pane.contains(on_map),
            "precondition: one click is on the pane"
        );
        assert!(!pane.contains(off_pane), "precondition: the other is not");
        assert!(
            h.screen_rect().contains(off_pane),
            "precondition: the blocked click must still be on screen — this is \
             a click on the chrome, not a click on nothing"
        );
        assert!(
            h.map_excluded_rects().is_empty(),
            "precondition: a wide screen excludes no floating chrome, so the \
             excluded-rect condition cannot be what blocks this"
        );
        assert!(
            !h.is_floating_layer_at(off_pane),
            "precondition: the status bar is a background layer, so the layer \
             condition cannot be what blocks this either"
        );

        h.mouse_click(on_map);
        // `contains`, not equality: at this zoom the three Oklahoma City
        // radars sit inside one icon of each other, and which of them also
        // answers says nothing about the condition under test.
        assert!(
            site_switches(&h).contains(&("KTLX".to_owned(), 0)),
            "control: the icon really is under both clicks — if this fails the \
             site was never placed and the assertion below is vacuous. Got {:?}",
            site_switches(&h)
        );

        h.mouse_click(off_pane);
        assert_eq!(
            site_switches(&h),
            vec![],
            "a click in the status bar switched the radar site: the map is \
             hit-testing chrome"
        );
    }

    /// 32c. **A dialog over a site icon takes its hover readout away.**
    ///
    ///      The layer condition, reached alone — and reachable only through
    ///      *hover*: a click is already stripped upstream by
    ///      `ui_input::filter_dialog_blocked`, so with the layer check deleted
    ///      from `is_pos_blocked` every click test still passes. Hover has no
    ///      such pre-filter; `pointer_hover_pos()` is read raw.
    ///
    ///      Asserted on the readout that was painted, not on a flag: the site
    ///      readout is the thing a user sees follow the cursor through a
    ///      dialog, and it is the same `Area` that once claimed `layer_id_at`
    ///      and ate the click behind it.
    #[test]
    fn a_dialog_over_a_site_icon_suppresses_its_hover_readout() {
        let mut h = InputHarness::new();
        h.gui_mut().enable_overlay_for_test(OverlayKind::RadarSites);
        h.warm_up();
        // Put the icon where a modal dialog lands, so one hover position
        // serves both halves.
        let target = h.screen_center();
        h.place_site_at(0, "KTLX", target);
        assert!(
            h.pane_rects()[0].contains(target),
            "precondition: the icon is on the pane, so the pane-rect condition \
             cannot be what blocks the hover"
        );
        assert!(
            h.map_excluded_rects().is_empty(),
            "precondition: nothing is excluded on a wide screen either"
        );

        h.mouse_move(target);
        h.frames_for(3, FRAME_DT);
        assert!(
            h.painted_text_strings()
                .iter()
                .any(|t| t.contains("KTLX\nLat:")),
            "control: hovering the icon must draw the site readout, or the \
             assertion below passes for free. Painted: {:?}",
            h.painted_text_strings()
        );

        h.gui_mut().show_settings = true;
        h.warm_up();
        assert!(
            h.is_floating_layer_at(target),
            "precondition: the settings dialog must cover the icon"
        );

        h.mouse_move(target);
        h.frames_for(3, FRAME_DT);
        assert!(
            !h.painted_text_strings()
                .iter()
                .any(|t| t.contains("KTLX\nLat:")),
            "the site readout came up through an open dialog: the map is \
             hovering what the dialog is covering"
        );
    }

    /// 33. **A dropdown's collapsed box says what its open list says.**
    ///
    ///     `ControlItem::Dropdown` carries `(value, display_label)` pairs. The
    ///     open list has always shown the label; `selected_text` showed the raw
    ///     *value*, so Model Data's Parameter box read `sbcin` and GLM's
    ///     Satellite box read `both` until you opened them.
    ///
    ///     Asserted against the label the handler itself offers for the
    ///     selected value, not against a string this test spells out: a
    ///     renderer that started formatting labels its own way would still have
    ///     to agree with the list beside it. The painted-text check is the
    ///     other end — it is the text egui actually laid out inside the combo's
    ///     rect, so a probe reporting a value the widget never received fails.
    #[test]
    fn a_dropdown_shows_its_option_label_not_the_raw_value() {
        let mut h = compact_with_drawer();
        for kind in [OverlayKind::ModelData, OverlayKind::Lightning] {
            h.set_overlay_on_pane(0, kind, true);
        }

        // Every dropdown on screen, whichever handler produced it.
        let drawn = h.dropdowns();
        assert!(
            drawn.len() >= 2,
            "precondition: Model Data and GLM must both be offering a dropdown, \
             got {drawn:?}"
        );

        for dropdown in &drawn {
            let (options, selected) = h
                .dropdown_model(&dropdown.label)
                .unwrap_or_else(|| panic!("no handler offers a {:?} dropdown", dropdown.label));
            let expected = options
                .iter()
                .find(|(value, _)| *value == selected)
                .map(|(_, display)| display.clone())
                .unwrap_or_else(|| {
                    panic!(
                        "the {:?} dropdown's selected value {selected:?} is not \
                         among the options it offers: {options:?}",
                        dropdown.label
                    )
                });
            assert_eq!(
                dropdown.selected_text, expected,
                "the {:?} dropdown's collapsed box disagrees with the label its \
                 own list puts against {selected:?}",
                dropdown.label
            );
            assert!(
                h.text_painted_in(dropdown.rect, &dropdown.selected_text),
                "the {:?} dropdown reported {:?} but egui painted no such text \
                 inside {:?}",
                dropdown.label,
                dropdown.selected_text,
                dropdown.rect
            );
        }

        // …and the list the box opens is the other half of the claim. Assert on
        // the labels it *paints*, so that "both halves use one formatter" is
        // checked against two rendered results rather than one rendered result
        // and one reading of the model.
        for dropdown in &drawn {
            let mut h = compact_with_drawer();
            for kind in [OverlayKind::ModelData, OverlayKind::Lightning] {
                h.set_overlay_on_pane(0, kind, true);
            }
            let (options, _) = h.dropdown_model(&dropdown.label).expect("still offered");
            assert!(
                h.screen_rect().contains(dropdown.rect.center()),
                "the {:?} dropdown was laid out at {:?}, off the {:?} viewport, \
                 so the click below would open nothing",
                dropdown.label,
                dropdown.rect,
                h.screen_rect()
            );

            h.mouse_click(dropdown.rect.center());
            h.warm_up();

            let painted = h.painted_text_strings();
            // The list scrolls, so only the options that fit are laid out —
            // hence "the ones on screen are labels", not "all of them are".
            let labels_shown = options
                .iter()
                .filter(|(_, display)| painted.contains(display))
                .count();
            assert!(
                labels_shown >= 2,
                "the {:?} list opened but painted fewer than two of its own \
                 option labels, so the check below has nothing to bite on; it \
                 painted {painted:?}",
                dropdown.label
            );
            for (value, display) in &options {
                assert!(
                    value == display || !painted.contains(value),
                    "the open {:?} list painted the raw option id {value:?} \
                     where its label is {display:?}",
                    dropdown.label
                );
            }
        }
    }

    /// 34. **The scan arriving must not re-key a widget.**
    ///
    ///     egui compares each pass's widget rects against the last and warns
    ///     when the same rect comes back under a different `Id` — the same
    ///     hazard [`crossing_a_breakpoint_does_not_move_any_widget_id`] guards
    ///     across a resize, caught by egui itself rather than by a probe list.
    ///
    ///     The status bar used to allocate its right-aligned error slot every
    ///     frame whether or not there was an error. Empty, that scope is a
    ///     zero-area rect welded to the row's right edge, while its id moves
    ///     with the widget count before it — and the auto-poll block draws
    ///     three widgets mid-fetch and one after. So the frame the first scan
    ///     landed re-keyed a widget, twice per launch.
    ///
    ///     This reads the verdict off egui's per-pass widget bookkeeping, so
    ///     it fires for any widget that acquires the same hazard, not just
    ///     this one.
    #[test]
    fn a_scan_arriving_moves_no_widget_id() {
        // Roughly a Fold 7 inner screen: wide enough for the long status bar,
        // too narrow for the sidebar, which is where this was seen.
        let mut h = InputHarness::with_screen(egui::vec2(750.0, 900.0));
        h.gui_mut().set_fetching(true);
        h.warm_up();
        assert!(
            h.status_bar().auto_poll.is_none(),
            "precondition: a fetch must be in flight, so the status bar is \
             showing the spinner rather than the auto-poll checkbox"
        );

        h.clear_id_changes();
        h.load_scan("KTLX");

        assert!(
            h.status_bar().auto_poll.is_some(),
            "precondition: the scan must have cleared the fetch, or the widget \
             count in the status bar never changed and this proves nothing"
        );
        assert_eq!(
            h.id_changes(),
            &[] as &[egui::Rect],
            "egui saw a widget rect come back under a different id when the \
             scan arrived: everything it remembers under those ids is discarded"
        );
    }

    /// 34b. **Crossing 600pt re-keys the status bar, and nothing else.**
    ///
    ///      The menu bar going away advances the root `Ui`'s auto-id counter
    ///      one step less, and `Ui::new_child` folds that counter into every
    ///      child scope's `unique_id` — `id_salt` moves only the *stable* id —
    ///      so the status-bar panel and the widgets keyed off its counter come
    ///      back under new ids on the far side. **This is deliberate and
    ///      costs nothing**; see the "Ids do not depend on the breakpoint"
    ///      note in `ui_chrome.rs` for why, and why there is no fix that is
    ///      not worse.
    ///
    ///      What this pins is the *extent* of it. egui's own widget
    ///      bookkeeping says which rects moved, so if the shift ever reaches a
    ///      widget outside the status bar — where something does store state
    ///      across frames — this
    ///      fails rather than being noticed by someone re-deriving the whole
    ///      mechanism a third time. If it fails because the list came back
    ///      empty, the shift is gone and the note in `ui_chrome.rs` can go
    ///      with it.
    #[test]
    fn crossing_the_menu_bar_breakpoint_re_keys_only_the_status_bar() {
        // Above 600pt, and narrow enough to have no sidebar either side of the
        // crossing, so the layers panel is the drawer both times.
        let mut h = InputHarness::with_screen(egui::vec2(750.0, 600.0));
        h.set_drawer_open(true);
        h.load_scan("KTLX");
        assert!(
            h.width_class().has_menu_bar(),
            "precondition: start above the menu-bar breakpoint"
        );

        // Real stored state behind a real widget id, so "nothing was lost" is
        // a claim about something rather than about an empty set.
        let probes = h.widget_id_probes();
        let scroll_id = probes
            .iter()
            .find(|(name, _)| *name == "layers_scroll")
            .expect("precondition: the scroll area must report an id")
            .1;
        h.scroll_at(egui::pos2(80.0, 400.0), egui::vec2(0.0, -120.0));
        h.frames_for(3, FRAME_DT);
        let scrolled = h.scroll_offset(scroll_id);
        assert!(
            scrolled.is_some_and(|o| o.y > 0.0),
            "precondition: the layers panel must have scrolled, got {scrolled:?}"
        );

        h.clear_id_changes();
        h.set_screen(egui::vec2(550.0, 600.0));
        h.set_drawer_open(true);
        assert_eq!(
            h.width_class(),
            crate::ui_layout::WidthClass::Compact,
            "precondition: the resize crossed the 600pt breakpoint"
        );

        let moved = h.id_changes().to_vec();
        assert!(
            !moved.is_empty(),
            "no widget id moved across the breakpoint at all — if that is a \
             fix rather than a probe that stopped reading, delete this test \
             and the note in `ui_chrome.rs` with it"
        );
        let bar = h.status_bar().rect;
        for rect in &moved {
            assert!(
                bar.contains_rect(*rect),
                "a widget outside the status bar ({rect:?}) changed id across \
                 the breakpoint; the bar is {bar:?}. Everything egui remembers \
                 under that id is discarded on every resize past 600pt"
            );
        }

        // ...and it cost nothing: the ids that key stored state are the same
        // ids, and the state stored under them is still there.
        assert_eq!(
            probes,
            h.widget_id_probes(),
            "a widget id that keys stored state moved with the layout"
        );
        assert_eq!(
            h.scroll_offset(scroll_id),
            scrolled,
            "the scroll position did not survive the breakpoint"
        );
    }

    /// 34c. **An error on screen keeps its id while the row moves around it.**
    ///
    ///      [`a_scan_arriving_moves_no_widget_id`] fixed and pinned the *empty*
    ///      half of this hazard — a slot allocated with nothing in it. The
    ///      occupied half survived, and nothing in the suite ever put an error
    ///      on screen to see it: the slot is right-aligned, so when there
    ///      really is an error its rect is welded to the row's edge while
    ///      everything to its left comes and goes, and the slot plus all three
    ///      widgets inside it come back under new ids. Two things move it — the
    ///      auto-poll block when a scan lands, and the data age line when a
    ///      render does — so both are driven here.
    #[test]
    fn an_error_on_screen_keeps_its_id_while_the_row_changes_around_it() {
        let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
        h.gui_mut().set_error("boom".to_owned());
        // `set_error` ends the fetch, so put it back: the transition under test
        // is the auto-poll spinner (three widgets) becoming the checkbox (one).
        h.gui_mut().set_fetching(true);
        h.warm_up();
        assert!(
            h.status_bar().auto_poll.is_none(),
            "precondition: a fetch must be in flight, so the bar is showing the \
             spinner rather than the checkbox"
        );
        assert!(
            h.painted_text_strings().iter().any(|t| t == "boom"),
            "precondition: the error must be on screen, or the slot under test \
             is not allocated at all"
        );

        h.clear_id_changes();
        h.load_scan("KTLX");
        assert!(
            h.status_bar().auto_poll.is_some(),
            "precondition: the scan must have cleared the fetch, or nothing to \
             the left of the error changed"
        );
        assert!(
            h.painted_text_strings().iter().any(|t| t == "boom"),
            "precondition: the error must still be on screen after the scan"
        );
        assert_eq!(
            h.id_changes(),
            &[] as &[egui::Rect],
            "a scan arriving re-keyed the error slot: its rect is pinned to the \
             row's right edge while its id follows the widget count to its left"
        );

        // …and the same slot, moved by the other neighbour.
        h.clear_id_changes();
        h.set_data_time(0, Some(written_ago(5)));
        assert!(
            h.status_bar().product_age_text.is_some(),
            "precondition: the age line must have appeared, or nothing moved"
        );
        assert_eq!(
            h.id_changes(),
            &[] as &[egui::Rect],
            "the data age line appearing re-keyed the error slot"
        );
    }

    /// 35. **...and the probe that says so can see a real one.**
    ///
    ///     [`a_scan_arriving_moves_no_widget_id`] asserts on an empty list, so
    ///     a reader that matched nothing would pass it forever — which is
    ///     exactly what happened when the reader was egui's painted debug
    ///     marker: that marker is compiled out of release builds, and only
    ///     this test noticed. This drives egui with a deliberately unstable id
    ///     at a fixed rect and requires the same reader the harness uses to
    ///     report it, in whichever profile is running.
    #[test]
    fn the_id_change_probe_reports_a_real_id_change() {
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 300.0));
        let ctx = egui::Context::default();
        let mut prev_widgets = egui::WidgetRects::default();
        let mut seen = Vec::new();
        for pass in 0..3u32 {
            ctx.begin_pass(egui::RawInput {
                screen_rect: Some(screen),
                ..Default::default()
            });
            let root = egui::Ui::new(
                ctx.clone(),
                egui::Id::new("canary_root"),
                egui::UiBuilder::new()
                    .layer_id(egui::LayerId::background())
                    .max_rect(screen),
            );
            let rect = egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(50.0, 20.0));
            // Same rect, new id, same parent: exactly what egui warns about.
            root.interact(rect, egui::Id::new(("canary", pass)), egui::Sense::click());
            let _ = ctx.end_pass();
            let widgets = pass_widgets(&ctx);
            seen.extend(id_changes_between(&prev_widgets, &widgets));
            prev_widgets = widgets;
        }
        assert!(
            !seen.is_empty(),
            "the id-change reader saw nothing for a widget that changed id \
             every pass, so the assertion it backs is vacuous"
        );
    }

    /// 36. **A pane born from the pane-count picker inherits the layer state.**
    ///
    ///     `PaneState::with_site` starts with empty overlay maps, and
    ///     `is_overlay_enabled` reads a missing entry as *off* — so a pane the
    ///     picker adds would draw no overlays at all, Radar included. Layer
    ///     sync masks this by copying the active pane over the newcomer every
    ///     frame the layers panel is up, so it runs with sync off.
    #[test]
    fn a_pane_added_by_the_picker_still_shows_radar_with_layer_sync_off() {
        let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
        assert_eq!(
            h.width_class(),
            crate::ui_layout::WidthClass::Expanded,
            "precondition: the picker only draws where the layers panel is up"
        );
        h.set_sync_layers(false);
        assert!(
            h.overlay_enabled(OverlayKind::Radar),
            "precondition: the active pane must have Radar on, or there is no \
             state for the newcomer to inherit"
        );

        let two = h
            .pane_options()
            .into_iter()
            .find(|o| o.count == 2)
            .expect("the picker must offer a 2-pane split on a desktop width");
        h.mouse_click(two.rect.center());
        h.frames_for(3, FRAME_DT);
        assert_eq!(
            h.pane_count(),
            2,
            "precondition: the click must have split the map"
        );
        assert!(
            !h.sync_layers(),
            "precondition: sync must still be off, or it did the seeding"
        );

        assert!(
            h.overlay_enabled_on(1, OverlayKind::Radar),
            "the picker's new pane came up with every overlay off — its empty \
             `enabled_overlays` was never seeded from the handler state"
        );
    }

    /// The site of the first `FetchRadarScan` among `actions`, if any.
    fn fetched_site(actions: &[crate::actions::GuiAction]) -> Option<String> {
        actions.iter().find_map(|a| match a {
            crate::actions::GuiAction::FetchRadarScan(config) => Some(config.site.clone()),
            _ => None,
        })
    }

    /// 37. **Refresh fetches the site the active pane is viewing.**
    ///
    ///     `radar.config.site` is a *global* last-switched site — the
    ///     frontend's `SwitchRadarSite` writes it whichever pane switched,
    ///     sync on or off — so with per-pane sites a Refresh that clones the
    ///     config verbatim can fetch a site the active pane never showed.
    ///     Both entry points, driven the way a user reaches them: the status
    ///     bar's button and the File menu's item.
    #[test]
    fn refresh_fetches_the_active_panes_site_not_the_global_one() {
        let mut h = InputHarness::with_screen(egui::vec2(800.0, 900.0));
        assert_eq!(
            h.width_class(),
            crate::ui_layout::WidthClass::Medium,
            "precondition: the menu-bar presentation of Refresh must be on screen"
        );

        // Some other pane switched sites last: the global config points away
        // from what the active pane is viewing.
        let mut config = h.gui_mut().get_radar_config().clone();
        config.site = "KDMX".to_owned();
        h.gui_mut().set_radar_config(config);
        h.warm_up();
        assert_eq!(
            h.gui_mut().active_pane().site,
            "KTLX",
            "precondition: the active pane and the global config must disagree"
        );

        h.mouse_click(h.status_bar().refresh.center());
        assert_eq!(
            fetched_site(h.last_actions()).as_deref(),
            Some("KTLX"),
            "the status-bar Refresh fetched the global site, not the active pane's"
        );

        h.mouse_click(clickable_leaf(&h, "File").center());
        h.frames_for(2, FRAME_DT);
        h.mouse_click(clickable_leaf(&h, "Refresh Radar").center());
        assert_eq!(
            fetched_site(h.last_actions()).as_deref(),
            Some("KTLX"),
            "the menu's Refresh fetched the global site, not the active pane's"
        );
    }

    /// 38. **A hover readout dies with the radar that produced it.**
    ///
    ///     `pane.hover_value` is written only by the radar arm of
    ///     `render_pane_map_content` and read by the status bar. Two ways the
    ///     writer stops running while the reader keeps going: the pane stops
    ///     drawing radar under the pointer, or the pane is hidden by splitting
    ///     back down and is not rendered at all. Neither may freeze the last
    ///     readout on the glass.
    #[test]
    fn a_hover_readout_does_not_outlive_its_pane_or_its_radar() {
        let mut h = InputHarness::with_screen(egui::vec2(800.0, 900.0));
        h.mouse_move(h.map_center());
        h.warm_up();
        assert!(
            h.status_bar().hover,
            "precondition: mouse modality, or there is no readout to go stale"
        );

        // A readout on a visible pane reaches the glass — the channel every
        // staleness assertion below depends on.
        h.gui_mut().pane_mut(0).unwrap().hover_value = Some("LIVE READOUT".to_owned());
        h.frame();
        assert!(
            h.painted_text_strings().iter().any(|t| t == "LIVE READOUT"),
            "precondition: a visible pane's readout must reach the status bar"
        );

        // ...for exactly as long as the radar arm re-sets it: with no radar
        // image under the pointer, the pane's next render clears it.
        h.frame();
        assert!(
            !h.painted_text_strings().iter().any(|t| t == "LIVE READOUT"),
            "a readout with no radar left behind it froze in the status bar"
        );

        // A pane hidden by splitting back down is never rendered, so nothing
        // can clear it — the status bar must not be reading it at all.
        h.set_pane_count(4);
        h.gui_mut().pane_mut(3).unwrap().hover_value = Some("HIDDEN PANE READOUT".to_owned());
        h.set_pane_count(2);
        h.frame();
        assert!(
            !h.painted_text_strings()
                .iter()
                .any(|t| t == "HIDDEN PANE READOUT"),
            "a hidden pane's stale readout surfaced in the status bar"
        );
    }

    // ── Pane kinds ───────────────────────────────────────────────────────

    /// Two points either side of a storm near KTLX, as the ends of a drawn
    /// line. Any finite pair with two distinct ends would do; these are
    /// plausible so a failure message reads like a section someone asked for.
    fn section_ends() -> (GeoPoint, GeoPoint) {
        (
            GeoPoint {
                lat: 35.0,
                lon: -97.8,
            },
            GeoPoint {
                lat: 35.6,
                lon: -96.9,
            },
        )
    }

    /// KTLX's reflectivity ladder on VCP 212, as the sampler resolves it — the
    /// chosen sweeps' **median** elevations, not the cut table's round numbers.
    ///
    /// Real-shaped on purpose. A synthetic ladder of round degrees would draw
    /// perfectly well against a build whose rungs came from the wrong list; the
    /// production failure this pins was measured on exactly these angles, where
    /// `ScanInfo`'s 0.1°-rounded, deduped view of the same volume reports 16
    /// entries for the sampler's 14 rungs (0.4394 → 0.4 and 0.4779 → 0.5 for
    /// one 0.4834° cut; 0.8350 → 0.8 and 0.9229 → 0.9 for one 0.8789° cut).
    fn vcp_212_rungs() -> Vec<f64> {
        vec![
            0.4834, 0.8789, 1.3184, 1.8018, 2.4170, 3.1201, 4.0430, 5.0977, 6.4160, 8.0273,
            10.0195, 12.5000, 15.6006, 19.5117,
        ]
    }

    /// The axes of a complete VCP 212 reflectivity section 100 km long, whose
    /// ladder is [`vcp_212_rungs`].
    fn vcp_212_axes() -> rustdar_radar::xsect::SectionAxes {
        rustdar_radar::xsect::SectionAxes {
            length_km: 100.0,
            base_km_msl: 0.4,
            top_km_msl: 20.4,
            near_ground_range_km: 10.0,
            far_ground_range_km: 110.0,
            coverage_ground_range_km: 110.0,
            cone_of_silence_km: 0.0,
            tilt_count: 14,
            widest_tilt_gap_deg: 4.9,
            top_tilt_deg: 19.5,
            top_declared_cut_deg: 19.5,
        }
    }

    /// 39. **Every pane reports a pointer frame, whatever kind it is.**
    ///
    ///     The pointer probe is pushed in `render_panes`' shared preamble,
    ///     above the kind branch, and it has to stay there. `InputHarness::frame`
    ///     reads the *active* pane's probe out of that vector and panics when it
    ///     finds none, so a kind whose arm skipped the push would take down the
    ///     whole pointer suite — several thousand lines of it — with a message
    ///     about the pointer pipeline never running, pointing at nothing that
    ///     changed.
    #[test]
    fn every_pane_reports_a_pointer_frame_whatever_its_kind() {
        let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
        h.set_pane_count(3);
        let (a, b) = section_ends();
        h.make_pane_cross_section(1, a, b);
        h.make_pane_volume(2);
        assert_eq!(
            h.pane_kinds(),
            vec![PaneKind::Map, PaneKind::CrossSection, PaneKind::Volume],
            "precondition: one pane of each kind, or this proves nothing"
        );

        assert_eq!(
            h.pane_pointers()
                .iter()
                .map(|probe| probe.pane_idx)
                .collect::<Vec<_>>(),
            vec![0, 1, 2],
            "a pane resolved no pointer state for the frame"
        );

        // The active half, and it needs a *non-map* active pane: the active
        // probe is the one `frame()` demands, so a section pane that skipped
        // the push would only be caught while it is the one being driven.
        let rects = h.pane_rects();
        h.mouse_click(rects[2].center());
        h.frames_for(2, FRAME_DT);
        assert_eq!(
            h.active_pane_index(),
            2,
            "precondition: clicking the volume pane must make it active"
        );
        assert_eq!(
            h.pane_pointers()
                .iter()
                .filter(|probe| probe.is_active)
                .map(|probe| probe.pane_idx)
                .collect::<Vec<_>>(),
            vec![2],
            "exactly one pane must own the pointer, and it is the active one"
        );
    }

    /// 40. **Converting a pane keeps what it was looking at.**
    ///
    ///     Site, scan, product, elevation, live-or-parked and viewport are flat
    ///     fields on `PaneState` precisely so that converting a pane cannot
    ///     touch them. A user who panned to a storm and picked a tilt has said
    ///     something; asking for a section of it is not a reason to forget it.
    ///
    ///     Run on a single pane deliberately: both `propagate_layer_sync` and
    ///     `sync_viewports` return early below two panes, so every field below
    ///     is observed changing (or not) for exactly one reason.
    #[test]
    fn a_converted_pane_keeps_its_site_and_viewport() {
        let mut h = InputHarness::new();
        h.load_scan("KAMA");
        {
            let pane = h
                .gui_mut()
                .pane_mut(0)
                .expect("a fresh harness has one pane");
            pane.selected_product = rustdar_radar::types::RadarProduct::Velocity;
            pane.selected_elevation = 1.5;
            pane.viewing_live = false;
            let _ = pane.map_memory.set_zoom(9.25);
            pane.map_memory.center_at(walkers::lat_lon(35.0, -97.8));
        }
        h.warm_up();

        /// Everything about a pane that is *not* its kind.
        fn looking_at(
            h: &mut InputHarness,
        ) -> (
            String,
            Option<&'static str>,
            String,
            f32,
            bool,
            f64,
            Option<walkers::Position>,
        ) {
            let pane = h.gui_mut().pane(0).expect("pane 0");
            (
                pane.site.clone(),
                pane.scan_info.as_ref().map(|info| info.site.name),
                pane.selected_product.name().to_owned(),
                pane.selected_elevation,
                pane.viewing_live,
                pane.map_memory.zoom(),
                pane.map_memory.detached(),
            )
        }

        let before = looking_at(&mut h);
        assert_eq!(
            h.pane_kinds(),
            vec![PaneKind::Map],
            "precondition: it starts as a map"
        );

        let (a, b) = section_ends();
        h.make_pane_cross_section(0, a, b);

        assert_eq!(
            h.pane_kinds(),
            vec![PaneKind::CrossSection],
            "precondition: the conversion must actually have happened"
        );
        assert_eq!(
            looking_at(&mut h),
            before,
            "converting the pane changed what it is looking at"
        );

        // ...and the line it was aimed with is still the line, several frames
        // later. Geographic ends, so nothing in the UI pass can move them.
        assert_eq!(
            h.gui_mut()
                .pane(0)
                .expect("pane 0")
                .cross_section()
                .and_then(|section| section.line)
                .map(|line| (line.a(), line.b())),
            Some((a, b)),
        );
    }

    /// 41. **A non-map pane paints its empty state, in its own rect.**
    ///
    ///     What each arm *recorded* rather than what the branch was handed:
    ///     `panes[i].kind()` is the branch's input, so a test reading it back
    ///     agrees with an arm that ignored it, and with an arm that read the
    ///     kind off the `mem::take`n slot (where every pane is a map). Each arm
    ///     writes its own kind as a literal, and this compares that against the
    ///     copy that actually reached the glass and the rect it reached it in.
    #[test]
    fn a_non_map_pane_paints_its_empty_state() {
        let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
        h.set_pane_count(3);
        // Unaimed, because `CROSS_SECTION_EMPTY_STATE` is what a section pane
        // says while it has *no line*. A pane that has been aimed and is
        // waiting for its cut says something else, and this test is about the
        // arm painting into its own rect rather than about which of the two
        // states it is in.
        h.make_pane_unaimed_cross_section(1);
        h.make_pane_volume(2);

        let rects = h.pane_rects();
        assert_eq!(
            h.pane_content_probes()
                .iter()
                .map(|probe| (probe.pane_idx, probe.kind, probe.rect))
                .collect::<Vec<_>>(),
            vec![
                (0, PaneKind::Map, rects[0]),
                (1, PaneKind::CrossSection, rects[1]),
                (2, PaneKind::Volume, rects[2]),
            ],
            "the arm that ran for a pane is not the arm for that pane's kind"
        );

        for (idx, copy) in [
            (1usize, crate::ui::CROSS_SECTION_EMPTY_STATE),
            (2, crate::ui::VOLUME_EMPTY_STATE),
        ] {
            assert!(
                h.text_painted_in(rects[idx], copy),
                "pane {idx} did not paint {copy:?}; it painted {:?}",
                h.painted_text_strings()
            );
            for other in (0..3).filter(|other| *other != idx) {
                assert!(
                    !h.text_painted_in(rects[other], copy),
                    "pane {other} painted pane {idx}'s empty state"
                );
            }
        }
    }

    /// 43. **Converting the active pane from the drawer really converts it.**
    ///
    ///     The end-to-end version of
    ///     `a_pane_kind_request_survives_the_pane_being_held_out_of_the_vector`,
    ///     driven by a real click through the real presentation. It matters
    ///     because the drawer's menu is rendered from *inside*
    ///     `render_layers_panel`, which holds the active pane out of the vector
    ///     with `mem::take` for the whole of the panel's body — so the obvious
    ///     `self.panes[self.active_pane].set_kind(..)` in the dispatcher writes a
    ///     placeholder that is discarded a moment later, and the checkbox simply
    ///     never sticks.
    ///
    ///     Asserted all the way to the glass: the pane's kind, the arm that
    ///     actually drew it, the copy on screen, and the checkbox reading back the
    ///     new state on the following frame. The last of those is what a user sees
    ///     first, and it is the one a half-wired conversion would fail.
    #[test]
    fn converting_the_active_pane_from_the_drawer_makes_it_a_volume_pane() {
        let mut h = compact_with_drawer();
        h.load_scan("KTLX");
        assert_eq!(
            h.pane_kinds(),
            vec![PaneKind::Map],
            "precondition: it starts as a map"
        );
        assert_eq!(
            h.menu_leaf(crate::ui::VOLUME_PANE_LABEL).map(|l| l.value),
            Some(Some(false)),
            "precondition: the drawer must draw the toggle, unchecked"
        );

        h.mouse_click(clickable_leaf(&h, crate::ui::VOLUME_PANE_LABEL).center());
        h.frames_for(3, FRAME_DT);

        assert_eq!(
            h.pane_kinds(),
            vec![PaneKind::Volume],
            "the click never reached the pane: the write landed on the pane the \
             layers panel had taken out of the vector"
        );
        assert_eq!(
            h.pane_content_probes()
                .iter()
                .map(|probe| probe.kind)
                .collect::<Vec<_>>(),
            vec![PaneKind::Volume],
            "the pane converted but the map arm still drew it"
        );
        assert!(
            h.text_painted_in(h.pane_rects()[0], crate::ui::VOLUME_EMPTY_STATE),
            "the volume pane painted {:?} instead of its empty state",
            h.painted_text_strings()
        );
        assert_eq!(
            h.menu_leaf(crate::ui::VOLUME_PANE_LABEL).map(|l| l.value),
            Some(Some(true)),
            "the checkbox did not read back the conversion, so it looks to the \
             user as though the click did nothing"
        );

        // …and back, from the same box. This is the only route out of a non-map
        // pane, so a one-way toggle would be a trap.
        h.mouse_click(clickable_leaf(&h, crate::ui::VOLUME_PANE_LABEL).center());
        h.frames_for(3, FRAME_DT);
        assert_eq!(h.pane_kinds(), vec![PaneKind::Map]);
    }

    /// 44. **A non-map pane's layers panel keeps what applies to it and drops the
    ///     rest.**
    ///
    ///     Four claims, each with its own failure:
    ///
    ///     * **A product picker.** It used to be gated on the Radar *overlay*
    ///       toggle, which asks whether the map should draw the radar image over
    ///       its tiles — a question a pane with no tiles does not have. Left
    ///       gated, a pane converted while that toggle happened to be off would
    ///       have no product control at all, absent rather than disabled, for a
    ///       reason nothing on screen explains.
    ///     * **Time navigation**, because a section of last hour's volume is a
    ///       perfectly good thing to ask for.
    ///     * **No tilt picker.** Both non-map kinds read the whole ladder, which
    ///       is what `PaneKind::consumes_whole_volume` means, so every entry in
    ///       the combo would select the same picture.
    ///     * **No loop transport and no overlay tree.** A loop frame *is* a
    ///       rendered plan-view tilt and nothing now feeds one to a pane like
    ///       this, so the control would enable a loop that never fills; every
    ///       entry in the overlay tree is a layer drawn over map tiles against a
    ///       projector this pane does not have.
    ///
    ///     The last two are also what makes this the test that notices the branch
    ///     reading `self.panes[active]` instead of the pane it was handed. That
    ///     slot holds a `mem::take` placeholder for the whole of the panel's pass
    ///     and therefore reads as a *map* — so a branch on it takes the map arm
    ///     for a converted pane, and the only visible difference is the transport
    ///     and the tree being drawn. The tilt picker would *not* reveal it: that
    ///     is decided inside `render_radar_controls` from the pane passed down.
    ///
    ///     The combos are read off the ids the panel actually resolved rather than
    ///     off the model, for the same reason `time_step_sel` is: a test rebuilding
    ///     the expected id from the same format string could agree with a panel
    ///     that drew neither control.
    #[test]
    fn a_non_map_pane_keeps_the_controls_that_apply_to_it_and_drops_the_rest() {
        /// Which of the layers panel's radar combos the last frame resolved an id
        /// for — the panel's own report, not a reconstruction of it.
        fn combos(h: &InputHarness) -> Vec<&'static str> {
            h.widget_id_probes()
                .into_iter()
                .map(|(name, _)| name)
                .filter(|name| *name == "product_sel" || *name == "elev_sel")
                .collect()
        }
        fn painted(h: &InputHarness, needle: &str) -> bool {
            h.painted_text_strings().iter().any(|t| t.contains(needle))
        }

        for kind in [PaneKind::CrossSection, PaneKind::Volume] {
            let mut h = InputHarness::with_screen(egui::vec2(1200.0, 900.0));
            h.load_scan("KTLX");
            h.offer_product(0, rustdar_radar::types::RadarProduct::Reflectivity, 0.5);
            assert_eq!(
                combos(&h),
                vec!["product_sel", "elev_sel"],
                "precondition: a map pane with a tilt on offer draws both, so the \
                 absence below is the pane's kind and not a missing scan"
            );
            assert!(
                painted(&h, "Radar Loop"),
                "precondition: a map pane draws the loop transport"
            );
            // An entry from the middle of the overlay tree, so the check is not
            // satisfied by the first one alone. `dropdowns()` is deliberately not
            // used: in the default state no handler offers one, so it is empty for
            // a map pane too and would pass for the wrong reason.
            assert!(
                painted(&h, "NWS Alerts"),
                "precondition: a map pane draws the overlay tree"
            );

            match kind {
                PaneKind::CrossSection => {
                    let (a, b) = section_ends();
                    h.make_pane_cross_section(0, a, b);
                }
                _ => h.make_pane_volume(0),
            }
            h.frames_for(2, FRAME_DT);
            assert_eq!(
                h.pane_kinds(),
                vec![kind],
                "precondition: the active pane must really have converted"
            );

            assert_eq!(
                combos(&h),
                vec!["product_sel"],
                "{kind:?}: either the product picker went with the map — leaving \
                 this pane unable to be pointed at another moment — or a tilt \
                 picker was drawn for a pane that reads every cut"
            );
            assert!(
                painted(&h, "Step:"),
                "{kind:?}: time navigation went with the map, so this pane can \
                 only ever show the live volume"
            );
            assert!(
                !painted(&h, "Radar Loop"),
                "{kind:?}: a loop transport was drawn for a pane nothing renders \
                 loop frames for, so enabling it would wait for ever"
            );
            assert!(
                !painted(&h, "NWS Alerts"),
                "{kind:?}: the overlay tree was drawn for a pane with no map to \
                 draw overlays on: {:?}",
                h.painted_text_strings()
            );
        }
    }

    /// 45. **A pane with no map does not keep the label-tile pyramid downloading.**
    ///
    ///     City labels are raster tiles drawn *over* the base map, so a pane with
    ///     no tiles has nowhere to put one — yet its `enabled_overlays` is
    ///     inherited across the conversion and still says the layer is on, which is
    ///     what kept `ensure_label_tiles` fetching. Mild in itself; it is here
    ///     because it is the same shape as the overlay auto-poll gate, which is
    ///     not mild, and because the two must agree.
    ///
    ///     `clear_graphics_state` in the middle of each arm, because
    ///     `ensure_label_tiles` only ever *creates* the source — a harness that
    ///     had already made them would keep them and prove nothing. That is not a
    ///     contrivance either: dropping the tile sources and letting the next
    ///     frame re-make them is exactly what a suspend or a surface loss does.
    #[test]
    fn a_pane_with_no_map_stops_the_label_tiles_downloading() {
        fn tiles_remade_after_a_reset(h: &mut InputHarness) -> bool {
            h.gui_mut().clear_graphics_state();
            assert!(
                !h.gui.label_tiles_made_for_test(),
                "precondition: the reset must really have dropped the tile sources"
            );
            h.frames_for(2, FRAME_DT);
            h.gui.label_tiles_made_for_test()
        }

        let mut on_a_map = InputHarness::with_screen(egui::vec2(1200.0, 900.0));
        on_a_map.set_overlay_on_pane(0, OverlayKind::CityLabels, true);
        assert!(
            tiles_remade_after_a_reset(&mut on_a_map),
            "precondition: a map pane with city labels on must fetch label tiles"
        );

        let mut converted = InputHarness::with_screen(egui::vec2(1200.0, 900.0));
        converted.set_overlay_on_pane(0, OverlayKind::CityLabels, true);
        converted.make_pane_volume(0);
        assert!(
            converted.overlay_enabled(OverlayKind::CityLabels),
            "precondition: the pane still *remembers* wanting labels, which is what \
             makes this a filter rather than a cleared flag"
        );

        assert!(
            !tiles_remade_after_a_reset(&mut converted),
            "a pane with no map to draw labels on kept the label-tile pyramid \
             downloading"
        );
    }

    /// 46. **A non-map pane's product picker survives the Radar layer being off.**
    ///
    ///     The picker used to be gated on `is_overlay_enabled(OverlayKind::Radar)`,
    ///     which asks whether the *map* should draw the radar image over its tiles.
    ///     A pane with no tiles has no such layer, so a pane converted while that
    ///     toggle happened to be off would have had no product control at all —
    ///     absent, not disabled, for a reason nothing on screen explains. A map
    ///     pane must still honour the toggle, which is the second half here.
    #[test]
    fn a_non_map_panes_product_picker_ignores_the_radar_layer_toggle() {
        let mut h = InputHarness::with_screen(egui::vec2(1200.0, 900.0));
        h.load_scan("KTLX");
        h.set_overlay_on_pane(0, OverlayKind::Radar, false);
        h.frames_for(2, FRAME_DT);

        let has_product = |h: &InputHarness| {
            h.widget_id_probes()
                .iter()
                .any(|(name, _)| *name == "product_sel")
        };
        assert!(
            !has_product(&h),
            "precondition: a map pane with the Radar layer off draws no product \
             picker, or the assertion below is about nothing"
        );

        h.make_pane_volume(0);
        h.frames_for(2, FRAME_DT);

        assert!(
            has_product(&h),
            "the Radar layer toggle suppressed the product picker on a pane with \
             no map, which has no such layer to turn off"
        );
    }

    /// 47. **Converting the active pane does not re-key the drawer menu.**
    ///
    ///     The layers panel's kind-specific block sits inside a child scope, and
    ///     this is why. `Ui::new_child` folds the parent's `next_auto_id_salt` into
    ///     every child's registered id, so two branches allocating different
    ///     numbers of widgets — a map pane draws a loop transport and the whole
    ///     overlay tree, a volume pane draws neither — shift the id of
    ///     **everything drawn after them**. Below them, in the drawer, is the menu,
    ///     which at every width without a menu bar is the only route to Exit and
    ///     Settings. Any child scope fixes that, because a scope advances the
    ///     parent's counter by exactly one however much it draws inside itself; the
    ///     explicit `UiBuilder::id` the panel uses is for a different and weaker
    ///     reason, set out on `render_layer_controls`.
    ///
    ///     [`converting_a_pane_moves_no_widget_id`] cannot see this: it converts a
    ///     *non-active* pane, so the panel's content does not change, and the
    ///     harness's id-change probe matches widgets **by rect** — a shift that
    ///     also moves the rects reads as new widgets rather than as re-keyed ones.
    ///     So the ids are compared directly, per label, which is what
    ///     `DrawnMenuLeaf::id` exists for.
    ///
    ///     Verified by mutation, and only in one direction: with the scope
    ///     **removed** and the branch drawn straight onto the panel's `Ui`, every id
    ///     below it moves and this fails. Replacing it with a bare `ui.scope` does
    ///     *not* fail, and should not — all three forms advance the parent's counter
    ///     by one, so it is the scope that is load-bearing here and not the id it
    ///     was built with.
    #[test]
    fn converting_the_active_pane_does_not_re_key_the_drawer_menu() {
        let mut h = compact_with_drawer();
        h.load_scan("KTLX");

        let ids_by_label = |h: &InputHarness| -> Vec<(&'static str, egui::Id)> {
            h.menu_leaves().iter().map(|l| (l.label, l.id)).collect()
        };

        let before = ids_by_label(&h);
        assert!(
            before.len() >= 6,
            "precondition: the drawer must really be drawing the menu, found {}",
            before.len()
        );

        h.mouse_click(clickable_leaf(&h, crate::ui::VOLUME_PANE_LABEL).center());
        h.frames_for(3, FRAME_DT);
        assert_eq!(
            h.pane_kinds(),
            vec![PaneKind::Volume],
            "precondition: the conversion must have happened, or the panel above \
             the menu never changed and nothing was at risk"
        );

        assert_eq!(
            ids_by_label(&h),
            before,
            "converting the active pane re-keyed the menu underneath it: egui \
             discards everything it remembers under those ids, and on a phone \
             this menu is the only way to reach the rest of the app"
        );
    }

    /// 48. **Converting a pane must not move any widget's egui `Id`.**
    ///
    ///     The `"pane_map"` id salt is a key, not a description: every widget
    ///     inside a pane derives its `Id` from it, so egui's memory of what the
    ///     pane remembers hangs off it. Re-keying it — by renaming it to
    ///     something kind-neutral, or by folding the kind into it — would turn
    ///     "the user made pane 2 a section" into "egui forgot everything pane 2
    ///     remembered".
    ///
    ///     [`crossing_a_breakpoint_does_not_move_any_widget_id`] will not catch
    ///     this: it compares the layers panel's own probed ids across a resize,
    ///     and a pane's ids are not in that list. This reads egui's per-pass
    ///     widget bookkeeping instead, so it fires for the kind-specific work
    ///     later work packages add inside each arm as much as for a renamed
    ///     salt.
    ///
    ///     The mechanism it is a net for: `Ui::new_child` folds the parent's
    ///     auto-id counter into every child's unique id — an `id_salt` moves only
    ///     the *stable* id, as `ui_chrome.rs`'s note on the 600pt breakpoint
    ///     records — so an arm that stopped building the shared child `Ui`, or
    ///     built an extra one, re-keys every pane the loop reaches **after** it
    ///     while leaving their rects exactly where they were.
    ///
    ///     Hence the **middle** of three, which is what makes the assertion bite
    ///     today, verified by mutation both ways round: with an arm building a
    ///     spare child `Ui`, converting the middle pane fails this test and
    ///     converting the *last* one does not. The reason is that the only
    ///     auto-id'd widget inside a pane right now is the map's own, so a
    ///     later map pane is the only thing whose id can be seen to move. It is
    ///     specifically *not* the divider layer downstream of the loop: that
    ///     child `Ui`'s own id does shift, but `PaneLayout::handle_dividers`
    ///     gives every divider an explicit `Id::new(("h_div", …))`, and an
    ///     explicit id does not read the parent's counter.
    ///
    ///     A one- or two-pane version of this assertion is still worth writing
    ///     once an arm registers widgets of its own — from then on a converted
    ///     pane re-keys *itself* and needs no later pane to reveal it.
    #[test]
    fn converting_a_pane_moves_no_widget_id() {
        let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
        h.set_pane_count(3);
        h.load_scan("KTLX");

        // Real stored state behind a real widget id, so "nothing was lost" is a
        // claim about something rather than about an empty set.
        let probes = h.widget_id_probes();
        let scroll_id = probes
            .iter()
            .find(|(name, _)| *name == "layers_scroll")
            .expect("precondition: the layers panel must report a scroll id")
            .1;
        h.scroll_at(egui::pos2(80.0, 400.0), egui::vec2(0.0, -120.0));
        h.frames_for(3, FRAME_DT);
        let scrolled = h.scroll_offset(scroll_id);
        assert!(
            scrolled.is_some_and(|offset| offset.y > 0.0),
            "precondition: the layers panel must have scrolled, got {scrolled:?}"
        );

        h.clear_id_changes();
        h.make_pane_unaimed_cross_section(1);

        assert_eq!(
            h.pane_kinds(),
            vec![PaneKind::Map, PaneKind::CrossSection, PaneKind::Map],
            "precondition: the middle pane converted and the last one did not"
        );
        assert!(
            h.text_painted_in(h.pane_rects()[1], crate::ui::CROSS_SECTION_EMPTY_STATE),
            "precondition: pane 1 must really be drawing something else now"
        );

        assert_eq!(
            h.id_changes(),
            &[] as &[egui::Rect],
            "egui saw a widget rect come back under a different id when a pane \
             was converted: everything it remembers under those ids is discarded"
        );
        assert_eq!(
            probes,
            h.widget_id_probes(),
            "a widget id that keys stored state moved when a pane was converted"
        );
        assert_eq!(
            h.scroll_offset(scroll_id),
            scrolled,
            "the scroll position did not survive converting another pane"
        );
    }

    /// 44. **The menu checkbox arms the draw, and a drag on a map becomes a
    ///     section.**
    ///
    ///     Through the drawer's own checkbox, which is where the mode is armed
    ///     and — just as importantly — where it is turned off again: a mode
    ///     whose state is invisible is a map that has mysteriously stopped
    ///     panning.
    #[test]
    fn the_drawers_checkbox_arms_the_cross_section_draw() {
        let mut h = compact_with_drawer();
        h.load_scan("KTLX");
        assert!(!h.section_draw_armed(), "precondition: it starts unarmed");
        assert_eq!(
            h.menu_leaf(crate::ui::DRAW_CROSS_SECTION_LABEL)
                .map(|l| l.value),
            Some(Some(false)),
            "precondition: the drawer must draw the toggle, unchecked"
        );

        h.mouse_click(clickable_leaf(&h, crate::ui::DRAW_CROSS_SECTION_LABEL).center());
        h.frames_for(3, FRAME_DT);

        assert!(h.section_draw_armed(), "the checkbox did not arm the draw");
        // Arming closes the drawer, and must: at every width where the drawer
        // *is* the menu it covers the whole map, so leaving it open would arm a
        // gesture the user has nowhere to make.
        assert_eq!(
            h.menu_leaf(crate::ui::DRAW_CROSS_SECTION_LABEL),
            None,
            "the drawer stayed open over the map the line has to be drawn on"
        );

        // Re-opened, the checkbox shows the mode it turned on — which is what a
        // user who armed it by accident needs in order to un-tick it.
        h.set_drawer_open(true);
        h.frames_for(2, FRAME_DT);
        assert_eq!(
            h.menu_leaf(crate::ui::DRAW_CROSS_SECTION_LABEL)
                .map(|l| l.value),
            Some(Some(true)),
            "the checkbox does not show the mode it just turned on"
        );

        // And un-ticking it disarms, so the mode is never a trap.
        h.mouse_click(clickable_leaf(&h, crate::ui::DRAW_CROSS_SECTION_LABEL).center());
        h.frames_for(3, FRAME_DT);
        assert!(
            !h.section_draw_armed(),
            "the checkbox could not turn it off"
        );
    }

    /// 45. **An armed drag on a map becomes a section aimed where it was drawn.**
    ///
    ///     Through the real pointer pipeline, `render_panes`' resolution and the
    ///     deferred apply.
    ///
    ///     Two claims beyond "a pane appeared", and both are about what the
    ///     drag did *not* do. The map must not have panned — the drag belongs to
    ///     the line, and a section drawn while the ground slid under it is of
    ///     nowhere in particular. And the mode must have disarmed itself, or the
    ///     user's next pan is a second section.
    #[test]
    fn an_armed_drag_on_a_map_becomes_a_cross_section_aimed_where_it_was_drawn() {
        let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
        h.load_scan("KTLX");
        h.set_section_draw_armed(true);
        h.warm_up();

        let pane = h.pane_rects()[0];
        let from = pane.center() - egui::vec2(120.0, 60.0);
        let to = pane.center() + egui::vec2(120.0, 60.0);
        let centre_before = h.pane_center(0);
        // Taken **before** the drag, and it has to be: applying the line grows
        // the layout, which halves pane 0's rect and therefore changes what
        // every pixel in it names. Recomputing afterwards would compare the line
        // against a projector that never drew it.
        let want_a = h.ground_at(0, from);
        let want_b = h.ground_at(0, to);

        h.mouse_move(from);
        h.frame();
        h.mouse_press(from);
        h.frame();
        for step in 1..=4 {
            h.mouse_move(from + (to - from) * (step as f32 / 4.0));
            h.frame();
        }
        h.mouse_release(to);
        h.frames_for(2, FRAME_DT);

        assert!(
            !h.section_draw_armed(),
            "the mode stayed armed after producing a line: the next pan is a \
             second section"
        );
        assert_eq!(
            h.pane_center(0),
            centre_before,
            "the map panned during the draw — the drag belongs to the line"
        );

        let target = h
            .pane_kinds()
            .iter()
            .position(|k| *k == PaneKind::CrossSection)
            .expect("the drag produced no section pane");
        let line = h
            .section_line(target)
            .expect("the section pane has no line");
        // A thousandth of a degree — about 90 m, or a third of a pixel at this
        // zoom. Loose enough to absorb walkers still settling its zoom
        // animation across the warm-up frames, and three orders of magnitude
        // tighter than the failure it is written against: an endpoint mapped
        // through the wrong rect, or through no projector at all, lands
        // kilometres away or in Kansas.
        assert!(
            (line.a().lat - want_a.y()).abs() < 1e-3 && (line.a().lon - want_a.x()).abs() < 1e-3,
            "the line starts at {:?}, not under the press at {want_a:?}",
            line.a()
        );
        assert!(
            (line.b().lat - want_b.y()).abs() < 1e-3 && (line.b().lon - want_b.x()).abs() < 1e-3,
            "the line ends at {:?}, not under the release at {want_b:?}",
            line.b()
        );
        assert_ne!(
            line.a(),
            line.b(),
            "both ends resolved to the same ground, so the drag is not being read"
        );
    }

    /// 46. **A rendered section's caption is calm by default, and the honesty
    ///     detail is one click away — reachable, in the user's words, and
    ///     closable again.**
    ///
    ///     The redesign's whole contract, end to end. The old caption painted
    ///     the ladder warning and the registration caveat on every ordinary
    ///     section, the warning in error styling — and watched with real users
    ///     it read as something broken, something they had broken. The drawn
    ///     rungs and the `NEAREST` upload stay as the standing honesty devices;
    ///     the words move behind the ⓘ.
    ///
    ///     Asserted through the ladder's *own* numbers on both sides of the
    ///     toggle, because a detail saying the same thing whatever the volume
    ///     was would be boilerplate a reader learns to skip. 14 rungs 0.5°
    ///     apart and 5 rungs 5° apart are the same sentence with entirely
    ///     different consequences.
    #[test]
    fn a_rendered_sections_caption_is_calm_and_its_detail_is_one_click_away() {
        let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
        h.load_scan("KTLX");
        let (a, b) = section_ends();
        h.make_pane_cross_section(0, a, b);
        h.place_section(0, vcp_212_axes(), &vcp_212_rungs());

        let pane = h.pane_rects()[0];
        assert!(
            !h.text_painted_in(pane, crate::ui::CROSS_SECTION_EMPTY_STATE),
            "precondition: the pane must be drawing a picture, not its empty state"
        );

        // The default: the ladder's own headline numbers, and none of the
        // long-form copy that read as an error state.
        assert!(
            h.text_painted_in(pane, "14 tilts"),
            "the default caption lost the ladder's own count; it painted {:?}",
            h.painted_text_strings_in(pane)
        );
        for wall_of_text in ["not measured", "slant range", "widest", "Echoes can sit"] {
            assert!(
                !h.text_painted_in(pane, wall_of_text),
                "the long-form detail is back in the default caption \
                 ({wall_of_text:?}); it painted {:?}",
                h.painted_text_strings_in(pane)
            );
        }

        // The ⓘ is on the pane, and clicking it opens the detail.
        let glyph = h
            .painted_text_rects()
            .into_iter()
            .find(|(rect, text)| text == "\u{2139}" && pane.contains(rect.center()))
            .map(|(rect, _)| rect)
            .expect("the caption has no \u{2139} detail toggle");
        h.mouse_click(glyph.center());
        h.frame();

        for phrase in [
            // The ladder's widest step: a measurement of *this* volume.
            "4.9",
            // What the reader must not do with the picture.
            "not measured",
            // And why echoes sit off the map's track, in words about what the
            // user sees rather than about which renderer is right.
            "Echoes can sit",
        ] {
            assert!(
                h.text_painted_in(pane, phrase),
                "the opened detail never said {phrase:?}; it painted {:?}",
                h.painted_text_strings_in(pane)
            );
        }
        // The developer-voice sentence is gone for good, open or closed: "the
        // section is right" is the app arguing with itself in front of the
        // user.
        assert!(
            !h.text_painted_in(pane, "The section is right"),
            "the detail still argues with the map in front of the user"
        );

        // And the detail closes again: an explanation that cannot be dismissed
        // is the old wall of text with an extra click in front of it.
        h.mouse_click(glyph.center());
        h.frame();
        assert!(
            !h.text_painted_in(pane, "not measured"),
            "the detail did not close on a second click"
        );
        assert!(
            h.text_painted_in(pane, "14 tilts"),
            "closing the detail lost the caption itself"
        );
    }

    /// 46b. **A rendered section is drawn the right way up, over the caption
    ///      that describes it, with its ladder on it and its readout live.**
    ///
    ///      Test 46 above is the only other thing that drives
    ///      `render_cross_section` end to end, and it reads back **painted text
    ///      only**. Every mutation inside the function body that does not change
    ///      a word therefore survived it: the tilt ladder gated off entirely,
    ///      `caption_height` reverted to a counted `len * 13.0`, the image's uv
    ///      flipped so the section is drawn upside down, `hover_value` never
    ///      written, and the transient status line never pushed into the
    ///      caption. Five mutants, one test, no failures — because nothing here
    ///      asserted a shape.
    ///
    ///      Two of the five are pointed:
    ///
    ///      * **The flip.** `y_of_height` is pinned in the module's own tests
    ///        and it is only half of the mapping; the image's uv rect is the
    ///        half that actually turns the picture over, and it was pinned
    ///        nowhere. The section module's doc calls this "the single most
    ///        likely mistake in the module and the least likely to be noticed —
    ///        a flipped section of a mature storm still looks like a storm".
    ///      * **`caption_height`.** It is the whole of F8's fix. The module's
    ///        own test computes the height itself and hands it to
    ///        `SectionLayout::new`, so reverting the production line to a count
    ///        passes it; only a test that goes through the real wiring, on a
    ///        pane narrow enough to make the caption *wrap*, can tell.
    #[test]
    fn a_rendered_section_is_the_right_way_up_and_carries_its_ladder() {
        // Narrow and tall on purpose, and the shape is load-bearing twice. The
        // width makes the caption *wrap* to half a dozen rows, which is the only
        // condition under which a measured caption height differs from a counted
        // one by more than a point or two — at 760 points both sentences fit on
        // one row each, `len * 13.0` lands within two points of the truth, and
        // the mutant passes. The height keeps it clear of
        // `TWO_LINE_CAPTION_MIN_HEIGHT` and of `CAPTION_MAX_HEIGHT_FRACTION`, so
        // the registration caveat is really there to be wrapped.
        let mut h = InputHarness::with_screen(egui::vec2(480.0, 900.0));
        h.load_scan("KTLX");
        let (a, b) = section_ends();
        h.make_pane_cross_section(0, a, b);
        h.place_section(0, vcp_212_axes(), &vcp_212_rungs());
        // With the ⓘ detail open — the caption's longest shape, and the only
        // one whose lines wrap enough rows for a counted height to be wrong by
        // more than a point or two.
        h.gui_mut()
            .pane_mut(0)
            .expect("pane 0 exists")
            .cross_section_mut()
            .expect("pane 0 is a section pane")
            .detail_open = true;
        h.warm_up();

        let pane = h.pane_rects()[0];
        let images = h.painted_images_in(pane);
        assert_eq!(
            images.len(),
            1,
            "a section pane paints exactly one textured quad, its raster; found \
             {images:?}"
        );
        let raster = images[0];
        assert!(
            raster.rect.width() > 0.0 && raster.rect.height() > 0.0,
            "the raster was painted into an empty rect: {:?}",
            raster.rect
        );

        // **The right way up.** egui's uv origin is the texture's top left and
        // the section's row 0 is the top of the height axis, so the quad's
        // top-left corner must sample the texture's top-left corner. Swapping
        // the uv rect's corners flips the picture with no error and no layout
        // change.
        assert_eq!(
            (raster.uv_at_top_left.x, raster.uv_at_top_left.y),
            (0.0, 0.0),
            "the top of the section's height axis is sampling the bottom of its \
             raster: the picture is drawn upside down, and a flipped storm still \
             looks like a storm"
        );
        assert_eq!(
            (raster.uv_at_bottom_right.x, raster.uv_at_bottom_right.y),
            (1.0, 1.0),
            "the section's raster is mirrored or cropped: uv {:?}..{:?}",
            raster.uv_at_top_left,
            raster.uv_at_bottom_right
        );

        // **The caption is paid for.** Every row of it is above the picture, not
        // over it — which is only true if the layout used the caption's
        // *measured* wrapped height rather than a count of its lines.
        // Both sentences, not just the first. A galley's rect spans every row it
        // wrapped to, and it is the *caveat* — the last line of the block — that
        // a short measurement pushes onto the picture; the ladder warning is
        // first and sits above the plot however wrong the height is.
        let caption_rows: Vec<(egui::Rect, String)> = h
            .painted_text_rects()
            .into_iter()
            .filter(|(rect, text)| {
                pane.contains(rect.center())
                    && (text.contains("tilts")
                        || text.contains("dotted curves")
                        || text.contains("Echoes can sit"))
            })
            .collect();
        assert_eq!(
            caption_rows.len(),
            3,
            "precondition: the headline and both detail sentences have to be on \
             the pane, or the overlap check below is looking at the wrong \
             thing: {:?}",
            h.painted_text_strings_in(pane)
        );
        let measured: f32 = caption_rows.iter().map(|(rect, _)| rect.height()).sum();
        let counted = caption_rows.len() as f32 * 13.0;
        assert!(
            measured - counted > 15.0,
            "precondition: the caption occupies {measured} points against a \
             counted {counted} — they agree at this pane width, so nothing here \
             says which of the two the layout used: {caption_rows:?}"
        );
        for (rect, text) in &caption_rows {
            assert!(
                rect.bottom() <= raster.rect.top() + 0.5,
                "a caption row was painted over the picture (row bottom {}, \
                 picture top {}): {text:?}",
                rect.bottom(),
                raster.rect.top(),
            );
        }

        // **The ladder is on it.** The rungs are the section's first honesty
        // device; without them the picture is a smooth field with nothing saying
        // which parts of it were measured.
        let color = crate::ui::map::section_render::tilt_rung_color();
        // Over everything rather than over the picture: the upper rungs leave
        // the top of the height axis part way along the line — a 19.5° beam is
        // 33 km up at 90 km — and are *clipped* when painted rather than
        // shortened, so filtering on the plot rect would read a real rung as a
        // missing one.
        let rungs = h.painted_segments_in(egui::Rect::EVERYTHING, color);
        assert!(
            !rungs.is_empty(),
            "no tilt rungs were drawn over the section, so nothing in the \
             picture says where the data actually is"
        );
        // One polyline per rung: every curve is sampled from the `A` end, so
        // each contributes exactly one segment starting at the plot's left
        // edge, and counting those counts the rungs.
        let left = rungs.iter().map(|(p, _)| p.x).fold(f32::INFINITY, f32::min);
        let starts = rungs
            .iter()
            .filter(|(p, _)| (p.x - left).abs() < 0.01)
            .count();
        assert_eq!(
            starts,
            vcp_212_rungs().len(),
            "the ladder drew {starts} curves for a 14-rung section",
        );
        // Each curve is traced rather than dashed once, and all of them the
        // same length.
        assert_eq!(
            rungs.len() % starts,
            0,
            "{} segments do not divide into {starts} curves",
            rungs.len()
        );
        assert!(
            rungs.len() >= starts * 32,
            "the rungs were drawn as {} segments across {starts} curves, which \
             is not a traced beam centre",
            rungs.len(),
        );
        // And they are on the *picture*, not merely somewhere on the pane.
        let over_the_picture = h.painted_segments_in(raster.rect, color).len();
        assert!(
            over_the_picture * 3 >= rungs.len(),
            "only {over_the_picture} of {} rung segments landed inside the \
             raster, so the ladder is not drawn where the data is",
            rungs.len(),
        );

        // **The readout is live.** Most of the value of the status plane is the
        // hover: it is the first place in the codebase that can say *why* a
        // pixel is blank, and it is written by this function after everything
        // above it has drawn.
        h.mouse_move(raster.rect.center());
        h.frame();
        let readout = h
            .gui
            .pane(0)
            .expect("pane 0")
            .hover_value
            .clone()
            .expect("a pointer over the picture wrote no readout");
        assert!(
            readout.contains("MSL") && readout.contains("along"),
            "the readout says nothing about where in the section the pointer is: \
             {readout}"
        );

        // **A transient state reaches the caption.** It is pushed last, under
        // the standing warning, so it can never push that off the pane — and a
        // build that never pushed it at all looks identical.
        h.gui_mut()
            .pane_mut(0)
            .expect("pane 0")
            .cross_section_mut()
            .expect("a section pane")
            .unavailable = Some(crate::pane::SectionUnavailable::AwaitingCoveragePattern);
        h.frame();
        let notice = crate::pane::SectionUnavailable::AwaitingCoveragePattern.message();
        let head = notice
            .split_whitespace()
            .take(4)
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            h.text_painted_in(pane, &head),
            "the pane is showing a stale picture with no word of why it is \
             stale; it painted {:?}",
            h.painted_text_strings_in(pane)
        );
    }

    /// 47. **A wheel-zoom part-way through a drag does not move the anchor.**
    ///
    ///     The reason the anchor is stored as *ground* and converted inside
    ///     `Map::show` on the press frame. An armed draw suppresses panning but
    ///     not zooming — walkers reads the wheel itself — so with a pixel anchor
    ///     a mid-drag notch would silently re-aim the line's near end while the
    ///     far end tracked the finger, and the section would be a convincing
    ///     picture of ground nobody pointed at.
    ///
    ///     Compared against the *same drag without the wheel* rather than
    ///     against a recomputed projection, so the claim is exact and needs no
    ///     tolerance: two identical presses on two identically warmed harnesses
    ///     must produce the same anchor, whatever happened afterwards.
    ///
    ///     And the release end must **differ**, which is the calibration: it
    ///     proves the zoom really landed and really changed what a pixel means,
    ///     so the anchor's stability is a property of the anchor rather than of
    ///     a wheel event that went nowhere.
    #[test]
    fn a_wheel_zoom_mid_drag_leaves_the_anchor_on_the_ground_it_was_put_on() {
        fn drag(zoom_mid_drag: bool) -> (SectionLine, f64) {
            let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
            h.load_scan("KTLX");
            h.set_section_draw_armed(true);
            h.warm_up();

            let pane = h.pane_rects()[0];
            let from = pane.center() - egui::vec2(120.0, 60.0);
            let to = pane.center() + egui::vec2(120.0, 60.0);

            h.mouse_move(from);
            h.frame();
            h.mouse_press(from);
            h.frame();

            if zoom_mid_drag {
                h.wheel_notch(pane.center(), egui::MouseWheelUnit::Line, -3.0);
            }
            h.frames_for(6, FRAME_DT);

            h.mouse_move(to);
            h.frame();
            h.mouse_release(to);
            h.frames_for(2, FRAME_DT);

            let target = h
                .pane_kinds()
                .iter()
                .position(|k| *k == PaneKind::CrossSection)
                .expect("the drag produced no section pane");
            let zoom = h.gui_mut().pane(0).unwrap().map_memory.zoom();
            (
                h.section_line(target)
                    .expect("the section pane has no line"),
                zoom,
            )
        }

        let (plain, plain_zoom) = drag(false);
        let (zoomed, zoomed_zoom) = drag(true);

        assert!(
            (plain_zoom - zoomed_zoom).abs() > 0.05,
            "precondition: the wheel must really have zoomed ({plain_zoom} -> \
             {zoomed_zoom}), or nothing below distinguishes a held anchor from \
             an ignored wheel event"
        );
        assert_eq!(
            plain.a(),
            zoomed.a(),
            "the zoom moved the anchor: it is being held as a pixel, so the \
             line's near end drifted to whatever ground that pixel names now"
        );
        assert_ne!(
            plain.b(),
            zoomed.b(),
            "the release end did not move, so the zoom changed nothing about \
             what a pixel means and the assertion above proves nothing"
        );
    }

    /// A map on pane 0 feeding a rendered section on pane 1, exactly as the
    /// armed draw leaves the layout — the fixture every endpoint-drag test
    /// starts from.
    ///
    /// Returns the line's two ends as **ground**, not pixels: the warm-up
    /// frames let walkers settle its zoom, so a test aims at a handle by
    /// projecting the ground through [`InputHarness::screen_of`] at use time
    /// rather than trusting a pixel recorded before the settling.
    fn harness_with_committed_section() -> (InputHarness, GeoPoint, GeoPoint) {
        let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
        h.set_pane_count(2);
        h.load_scan("KTLX");
        h.warm_up();
        h.warm_up();
        let pane = h.pane_rects()[0];
        let to_geo = |pos: walkers::Position| GeoPoint {
            lat: pos.y(),
            lon: pos.x(),
        };
        let a = to_geo(h.ground_at(0, pane.center() - egui::vec2(140.0, 70.0)));
        let b = to_geo(h.ground_at(0, pane.center() + egui::vec2(140.0, 70.0)));
        h.make_pane_cross_section(1, a, b);
        h.gui_mut()
            .pane_mut(1)
            .expect("pane 1 exists")
            .cross_section_mut()
            .expect("pane 1 is a section pane")
            .source_pane = Some(0);
        h.place_section(1, vcp_212_axes(), &vcp_212_rungs());
        (h, a, b)
    }

    /// 45c. **Dragging an endpoint previews live and re-aims the section only
    ///      on the drop** — the pointer's whole journey changes nothing the
    ///      cut dispatch can see, and the release changes exactly the line.
    ///
    ///      The stored line *is* the drag's contribution to the section
    ///      staleness key (`SectionTarget.line`), and the frontend dispatcher
    ///      re-cuts precisely when that key moves — so "the line holds still
    ///      until the drop" is this crate's half of "a drag in progress never
    ///      triggers an extraction". A re-cut is a multi-MB walk of the merged
    ///      volume's gate bytes; per drag frame it would be the most expensive
    ///      thing in the app.
    ///
    ///      Also pinned here because they are the same gesture: the map does
    ///      not pan while a handle is held (from the press frame, via the
    ///      recorded handle spots), and the drop does not blank the pane —
    ///      walking a line through a storm must not strobe the picture on
    ///      every drop.
    #[test]
    fn dragging_an_endpoint_re_aims_the_section_on_drop_and_never_mid_drag() {
        let (mut h, a, b) = harness_with_committed_section();
        let line_before = h.section_line(1).expect("the fixture committed a line");
        let centre_before = h.pane_center(0);

        let b_px = h.screen_of(0, b);
        let target_px = b_px + egui::vec2(-70.0, 45.0);
        // The ground the drop should land on, computed before the drag: the
        // map must not move under the gesture, so the answer must not either.
        let want = h.ground_at(0, target_px);

        h.mouse_move(b_px);
        h.frame();
        h.mouse_press(b_px);
        let pressed = h.frame();
        assert!(
            pressed.resolved.suppress_pan,
            "the press frame left the map free to pan out from under the grab"
        );

        for step in 1..=4 {
            h.mouse_move(b_px + (target_px - b_px) * (step as f32 / 4.0));
            h.frame();
            assert_eq!(
                h.section_line(1),
                Some(line_before),
                "the stored line moved mid-drag on step {step}: every one of \
                 these frames is a re-cut the dispatcher will run"
            );
        }
        assert_eq!(h.pane_center(0), centre_before, "the map panned mid-drag");

        h.mouse_release(target_px);
        h.frames_for(2, FRAME_DT);

        let line = h.section_line(1).expect("the drop lost the line");
        assert_ne!(line, line_before, "the drop committed nothing");
        assert_eq!(
            line.a(),
            line_before.a(),
            "grabbing B moved A: the drag is a redraw, not a handle"
        );
        assert!(
            (line.b().lat - want.y()).abs() < 1e-3 && (line.b().lon - want.x()).abs() < 1e-3,
            "B landed at {:?}, not under the drop at {want:?}",
            line.b()
        );
        // A is still where the fixture put it on the ground, so the whole
        // gesture read the projector rather than shifting pixels.
        assert!(
            (line.a().lat - a.lat).abs() < 1e-9,
            "A drifted from the ground the fixture named"
        );
        // The old picture stands until the re-cut lands: blanking here would
        // strobe the pane on every drop of a walk through a storm.
        assert!(
            h.gui_mut()
                .pane(1)
                .and_then(|p| p.cross_section())
                .is_some_and(|s| s.texture.is_some() && s.section.is_some()),
            "the drop blanked the section pane"
        );
    }

    /// 45d. **A mid-drag zoom keeps the grabbed endpoint's ground.**
    ///
    ///      The drag suppresses panning but not zooming — walkers reads the
    ///      wheel itself — so a wheel notch mid-drag is ordinary. The preview
    ///      is geographic and the pointer is folded in only on frames it
    ///      moved, so a zoom-only frame must change nothing: re-unprojecting a
    ///      stationary pointer would slide the endpoint to whatever ground its
    ///      pixel names after the zoom.
    ///
    ///      Same construction as the armed-draw anchor test above: two
    ///      identical drags, one with a wheel notch before the release, must
    ///      commit the same line — and the zooms must really differ, or the
    ///      equality proves nothing.
    #[test]
    fn a_mid_drag_zoom_keeps_the_grabbed_endpoints_ground() {
        fn drag(zoom_mid_drag: bool) -> (SectionLine, f64) {
            let (mut h, _, b) = harness_with_committed_section();
            let b_px = h.screen_of(0, b);
            let target_px = b_px + egui::vec2(-70.0, 45.0);

            h.mouse_move(b_px);
            h.frame();
            h.mouse_press(b_px);
            h.frame();
            h.mouse_move(target_px);
            h.frame();

            if zoom_mid_drag {
                // At the pointer's own position, as a user zooming in on the
                // endpoint they are placing does — so the pointer does not
                // move, and only the ground under it changes.
                h.wheel_notch(target_px, egui::MouseWheelUnit::Line, -3.0);
            }
            h.frames_for(6, FRAME_DT);

            h.mouse_release(target_px);
            h.frames_for(2, FRAME_DT);

            let zoom = h.gui_mut().pane(0).expect("pane 0").map_memory.zoom();
            (h.section_line(1).expect("the drop lost the line"), zoom)
        }

        let (plain, plain_zoom) = drag(false);
        let (zoomed, zoomed_zoom) = drag(true);

        assert!(
            (plain_zoom - zoomed_zoom).abs() > 0.05,
            "precondition: the wheel must really have zoomed ({plain_zoom} -> \
             {zoomed_zoom}), or the equality below proves nothing"
        );
        assert_eq!(
            plain.b(),
            zoomed.b(),
            "the zoom moved the grabbed endpoint: a stationary pointer is \
             being re-unprojected through the zoomed projector"
        );
        assert_eq!(plain.a(), zoomed.a(), "the fixed end moved under a zoom");
    }

    /// 45e. **A press beyond the grab radius is still a pan.**
    ///
    ///      The handles are unarmed, so their radius is the entire contract
    ///      with the map's primary gesture: inside it a press edits, outside
    ///      it the map pans exactly as it did before handles existed. A radius
    ///      that swallowed nearby presses would make every pan near a section
    ///      line a coin flip — the exact failure the armed modes exist to
    ///      avoid.
    #[test]
    fn a_press_beside_the_handles_still_pans_the_map() {
        let (mut h, a, b) = harness_with_committed_section();
        let a_px = h.screen_of(0, a);
        let b_px = h.screen_of(0, b);
        // Well off both handles and off the line's body alike.
        let mid = a_px + (b_px - a_px) * 0.5;
        let start = mid + egui::vec2(0.0, -120.0);
        assert!(
            h.pane_rects()[0].contains(start),
            "precondition: the press is inside pane 0"
        );

        let centre_before = h.pane_center(0);
        h.mouse_move(start);
        h.frame();
        h.mouse_press(start);
        let pressed = h.frame();
        assert!(
            !pressed.resolved.suppress_pan,
            "a press {:.0} points from the nearest handle suppressed panning",
            (start - a_px).length().min((start - b_px).length())
        );
        for step in 1..=3 {
            h.mouse_move(start + egui::vec2(30.0 * step as f32, 15.0 * step as f32));
            h.frame();
        }
        h.mouse_release(start + egui::vec2(90.0, 45.0));
        h.frames_for(2, FRAME_DT);

        assert_ne!(
            h.pane_center(0),
            centre_before,
            "an ordinary pan near a section line went missing"
        );
        assert_eq!(
            h.section_line(1),
            Some(SectionLine::new(a, b).expect("the fixture's line")),
            "a pan rewrote the section's line"
        );
    }

    /// 45f. **While the draw mode is armed, the handles go inert** — the same
    ///      press that would grab B draws a fresh line instead, exactly as it
    ///      did before handles existed.
    ///
    ///      One drag on one map pane cannot be two gestures. The armed mode
    ///      was asked for last (from the menu), so it wins; the two setters
    ///      also drop any edit drag in flight, which keeps the exclusion true
    ///      from both directions.
    #[test]
    fn an_armed_draw_wins_the_press_over_a_handle() {
        let (mut h, a, b) = harness_with_committed_section();
        h.set_section_draw_armed(true);
        h.warm_up();

        let b_px = h.screen_of(0, b);
        let to_px = b_px + egui::vec2(-160.0, 90.0);
        let want_from = h.ground_at(0, b_px);

        h.mouse_move(b_px);
        h.frame();
        h.mouse_press(b_px);
        h.frame();
        h.mouse_move(to_px);
        h.frame();
        h.mouse_release(to_px);
        h.frames_for(2, FRAME_DT);

        let line = h.section_line(1).expect("the armed draw re-aims pane 1");
        // The armed draw's signature: the press became the *A end of a new
        // line*. An endpoint edit would have kept A where the fixture put it.
        assert!(
            (line.a().lat - want_from.y()).abs() < 1e-3
                && (line.a().lon - want_from.x()).abs() < 1e-3,
            "the press on a handle did not start a fresh armed line: A is at \
             {:?}, expected under the press at {want_from:?}",
            line.a()
        );
        assert!(
            (line.a().lat - a.lat).abs() > 1e-4 || (line.a().lon - a.lon).abs() > 1e-4,
            "precondition: the fixture's A and the press ground must differ, \
             or this cannot tell the two gestures apart"
        );
        assert!(
            !h.section_draw_armed(),
            "the armed draw did not disarm after producing its line"
        );
    }

    /// 45g. **Escape mid-drag cancels the edit and keeps the line** — the
    ///      same layer the armed drags sit on, because a drag in flight is the
    ///      most immediate thing a "back out" gesture can be aimed at.
    #[test]
    fn escape_mid_drag_cancels_the_edit_and_keeps_the_line() {
        let (mut h, _, b) = harness_with_committed_section();
        let line_before = h.section_line(1).expect("the fixture committed a line");
        let b_px = h.screen_of(0, b);

        h.mouse_move(b_px);
        h.frame();
        h.mouse_press(b_px);
        h.frame();
        h.mouse_move(b_px + egui::vec2(-60.0, 30.0));
        h.frame();

        assert!(
            h.gui_mut().dismiss_top_layer(),
            "a drag in flight gave the back gesture nothing to dismiss"
        );
        h.frame();
        h.mouse_release(b_px + egui::vec2(-80.0, 40.0));
        h.frames_for(2, FRAME_DT);

        assert_eq!(
            h.section_line(1),
            Some(line_before),
            "a cancelled drag still moved the line"
        );
    }

    /// 45h. **Dragging the line's body slides it rigidly** — length and
    ///      bearing kept — **previewing live and re-cutting only on the
    ///      drop**, exactly like an endpoint drag.
    ///
    ///      This is the "walk the section through the storm" motion (GR's
    ///      Position slider, as a direct gesture), so the mid-drag stillness
    ///      matters twice over: it is one re-cut per walk step instead of one
    ///      per frame, and the body is the *likeliest* thing to be dragged
    ///      repeatedly.
    #[test]
    fn a_body_drag_slides_the_line_rigidly_and_re_cuts_on_drop() {
        let (mut h, _, _) = harness_with_committed_section();
        let line_before = h.section_line(1).expect("the fixture committed a line");
        let mid_px = h.screen_of(0, crate::ui_section_edit::midpoint(line_before));
        let target_px = mid_px + egui::vec2(20.0, -60.0);

        h.mouse_move(mid_px);
        h.frame();
        h.mouse_press(mid_px);
        let pressed = h.frame();
        assert!(
            pressed.resolved.suppress_pan,
            "a press on the line's body left the map free to pan"
        );
        for step in 1..=4 {
            h.mouse_move(mid_px + (target_px - mid_px) * (step as f32 / 4.0));
            h.frame();
            assert_eq!(
                h.section_line(1),
                Some(line_before),
                "the stored line moved mid-drag on step {step}"
            );
        }
        h.mouse_release(target_px);
        h.frames_for(2, FRAME_DT);

        let line = h.section_line(1).expect("the drop lost the line");
        assert_ne!(line, line_before, "the body drag committed nothing");
        let (len_before, len_after) = (
            crate::ui_section_edit::length_km(line_before),
            crate::ui_section_edit::length_km(line),
        );
        assert!(
            (len_after - len_before).abs() < len_before * 0.01,
            "a body drag stretched the line: {len_before} km -> {len_after} km"
        );
        let (bearing_before, bearing_after) = (
            crate::ui_section_edit::bearing_deg(line_before),
            crate::ui_section_edit::bearing_deg(line),
        );
        assert!(
            (bearing_after - bearing_before).abs() < 0.5,
            "a body drag turned the line: {bearing_before}\u{b0} -> {bearing_after}\u{b0}"
        );
        // And it really moved: both ends, together.
        assert_ne!(line.a(), line_before.a());
        assert_ne!(line.b(), line_before.b());
    }

    /// 45i. **A shift-drag on the body sweeps the line about its midpoint** —
    ///      midpoint and length kept, bearing following the pointer.
    ///
    ///      The rotate affordance of issue #10. Shift is latched at the press,
    ///      so the verb cannot change mid-gesture; the fine-step spelling for
    ///      pointers with no modifier lives on the section pane's chips.
    #[test]
    fn a_shift_body_drag_sweeps_about_the_midpoint() {
        let (mut h, a, b) = harness_with_committed_section();
        let line_before = h.section_line(1).expect("the fixture committed a line");
        let mid_before = crate::ui_section_edit::midpoint(line_before);
        // Press three quarters of the way along, where a bearing about the
        // midpoint is well defined.
        let (press_lat, press_lon) =
            rustdar_radar::beam::great_circle_point((a.lat, a.lon), (b.lat, b.lon), 0.75);
        let press_px = h.screen_of(
            0,
            GeoPoint {
                lat: press_lat,
                lon: press_lon,
            },
        );
        // Where the pointer will end, unprojected now — the drag suppresses
        // panning, so the ground under that pixel must not move either. The
        // sweep's signed contract: the grabbed point sits on the pivot→B
        // side, so the committed line's bearing must land where the *pointer*
        // is about the pivot — not merely differ from the old bearing.
        let release_ground = h.ground_at(0, press_px + egui::vec2(-40.0, -48.0));
        let (want_bearing, _) = rustdar_radar::beam::site_bearing_range_km(
            mid_before.lat,
            mid_before.lon,
            release_ground.y(),
            release_ground.x(),
        );

        h.set_modifiers(egui::Modifiers {
            shift: true,
            ..Default::default()
        });
        h.mouse_move(press_px);
        h.frame();
        h.mouse_press(press_px);
        h.frame();
        // Pull the grabbed point across the line's run — a turn, not a slide.
        for step in 1..=4 {
            h.mouse_move(press_px + egui::vec2(-10.0, -12.0) * step as f32);
            h.frame();
        }
        h.mouse_release(press_px + egui::vec2(-40.0, -48.0));
        h.frames_for(2, FRAME_DT);
        h.set_modifiers(egui::Modifiers::default());

        let line = h.section_line(1).expect("the drop lost the line");
        assert_ne!(line, line_before, "the sweep committed nothing");
        let mid_after = crate::ui_section_edit::midpoint(line);
        assert!(
            (mid_after.lat - mid_before.lat).abs() < 1e-6
                && (mid_after.lon - mid_before.lon).abs() < 1e-6,
            "the sweep moved its own pivot: {mid_before:?} -> {mid_after:?}"
        );
        assert!(
            (crate::ui_section_edit::length_km(line)
                - crate::ui_section_edit::length_km(line_before))
            .abs()
                < 0.5,
            "the sweep changed the line's length"
        );
        assert!(
            (crate::ui_section_edit::bearing_deg(line)
                - crate::ui_section_edit::bearing_deg(line_before))
            .abs()
                > 2.0,
            "a drag across the line's run turned it by nothing"
        );
        // And it turned the right way: the grabbed point followed the pointer
        // about the pivot, so the line's bearing landed on the pointer's. A
        // negated sweep delta turns the line *away* from the pointer by the
        // same magnitude — every assertion above still passes, and this one
        // misses by roughly twice the swing (~100° here).
        let got = crate::ui_section_edit::bearing_deg(line).rem_euclid(360.0);
        let off = (got - want_bearing.rem_euclid(360.0)).rem_euclid(360.0);
        assert!(
            off.min(360.0 - off) < 3.0,
            "the grabbed point swept away from the pointer: the line's \
             bearing landed at {got}\u{b0}, the pointer sat on \
             {want_bearing}\u{b0} from the pivot"
        );
    }

    /// A single section pane with a rendered cut, for the step-control tests —
    /// the layout a phone gets, where the chips are the only pan/sweep there
    /// is.
    fn harness_with_section_pane() -> (InputHarness, SectionLine) {
        let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
        h.load_scan("KTLX");
        let (a, b) = section_ends();
        h.make_pane_cross_section(0, a, b);
        h.place_section(0, vcp_212_axes(), &vcp_212_rungs());
        let line = h.section_line(0).expect("the fixture committed a line");
        (h, line)
    }

    /// The rect a control chip's glyph was painted in, inside `pane`.
    fn chip_rect(h: &InputHarness, pane: egui::Rect, glyph: &str) -> egui::Rect {
        h.painted_text_rects()
            .into_iter()
            .find(|(rect, text)| text == glyph && pane.contains(rect.center()))
            .map(|(rect, _)| rect)
            .unwrap_or_else(|| {
                panic!(
                    "no {glyph:?} chip on the section pane; painted {:?}",
                    h.painted_text_strings_in(pane)
                )
            })
    }

    /// 45j. **The section pane's pan chips slide the line perpendicular to
    ///      itself, one step per click, keeping the picture** — and the pane
    ///      says which way the line faces while you do it.
    ///
    ///      A chip click is one deliberate action, so unlike the map drags it
    ///      commits immediately: one click, one re-cut, which is what makes
    ///      stepping feel like a control. The picture stands until the new cut
    ///      lands — strobing to "Cutting…" on every step is the exact failure
    ///      the walk-through-the-storm flow cannot have.
    #[test]
    fn a_pan_step_on_the_section_pane_slides_the_line_and_keeps_the_picture() {
        let (mut h, line_before) = harness_with_section_pane();
        let pane = h.pane_rects()[0];

        // The orientation readout: bearing (three digits, so north is 000°)
        // and length, in the user's units — sweeping blind is the alternative.
        let expected_readout = format!(
            "{:03.0}\u{b0} \u{b7} {:.0}{}",
            crate::ui_section_edit::bearing_deg(line_before).rem_euclid(360.0),
            rustdar_units::UserPreferences::default()
                .distance
                .convert_from_km(crate::ui_section_edit::length_km(line_before)),
            rustdar_units::UserPreferences::default().distance.suffix(),
        );
        assert!(
            h.text_painted_in(pane, &expected_readout),
            "the pane never says which way the line faces (wanted \
             {expected_readout:?}); it painted {:?}",
            h.painted_text_strings_in(pane)
        );

        h.mouse_click(chip_rect(&h, pane, "\u{25c0}").center());
        h.frame();

        let line = h.section_line(0).expect("the step lost the line");
        assert_ne!(line, line_before, "the pan chip moved nothing");
        assert!(
            (crate::ui_section_edit::length_km(line)
                - crate::ui_section_edit::length_km(line_before))
            .abs()
                < 0.1,
            "a pan step stretched the line"
        );
        assert!(
            (crate::ui_section_edit::bearing_deg(line)
                - crate::ui_section_edit::bearing_deg(line_before))
            .abs()
                < 0.1,
            "a pan step turned the line"
        );
        // Perpendicular, to the left of A→B, by exactly one step.
        let mid_before = crate::ui_section_edit::midpoint(line_before);
        let mid_after = crate::ui_section_edit::midpoint(line);
        let (moved_bearing, moved_km) = rustdar_radar::beam::site_bearing_range_km(
            mid_before.lat,
            mid_before.lon,
            mid_after.lat,
            mid_after.lon,
        );
        let step =
            crate::ui_section_edit::pan_step_km(crate::ui_section_edit::length_km(line_before));
        assert!(
            (moved_km - step).abs() < step * 0.05,
            "one click moved the line {moved_km} km for a {step} km step"
        );
        let want_bearing =
            (crate::ui_section_edit::bearing_deg(line_before) - 90.0).rem_euclid(360.0);
        let off = (moved_bearing - want_bearing).rem_euclid(360.0);
        assert!(
            off.min(360.0 - off) < 1.0,
            "the ◀ chip moved the line on bearing {moved_bearing}\u{b0}, not \
             perpendicular-left at {want_bearing}\u{b0}"
        );
        // The picture stands until the re-cut lands.
        assert!(
            h.gui_mut()
                .pane(0)
                .and_then(|p| p.cross_section())
                .is_some_and(|s| s.texture.is_some()),
            "a pan step blanked the pane"
        );
    }

    /// 45k. **The sweep chips rotate the line about its midpoint by one step**
    ///      — the fine-grained spelling of the sweep, and the only one a
    ///      touch screen with no modifier keys gets.
    #[test]
    fn a_sweep_step_on_the_section_pane_rotates_about_the_midpoint() {
        let (mut h, line_before) = harness_with_section_pane();
        let pane = h.pane_rects()[0];

        h.mouse_click(chip_rect(&h, pane, "\u{21bb}").center());
        h.frame();

        let line = h.section_line(0).expect("the step lost the line");
        let turned = (crate::ui_section_edit::bearing_deg(line)
            - crate::ui_section_edit::bearing_deg(line_before))
        .rem_euclid(360.0);
        assert!(
            (turned - crate::ui_section_edit::SWEEP_STEP_DEG).abs() < 0.1,
            "the ↻ chip turned the line {turned}\u{b0} for a \
             {}\u{b0} step",
            crate::ui_section_edit::SWEEP_STEP_DEG
        );
        let mid_before = crate::ui_section_edit::midpoint(line_before);
        let mid_after = crate::ui_section_edit::midpoint(line);
        assert!(
            (mid_after.lat - mid_before.lat).abs() < 1e-6
                && (mid_after.lon - mid_before.lon).abs() < 1e-6,
            "a sweep step moved the pivot"
        );
        assert!(
            (crate::ui_section_edit::length_km(line)
                - crate::ui_section_edit::length_km(line_before))
            .abs()
                < 0.1,
            "a sweep step changed the length"
        );
    }

    /// 45l. **The grab radii in absolute points: a press 30 points off a cap
    ///      and 20 points off the body still pans.**
    ///
    ///      45e proves *a* pan survives, but its press sits ~107 points from
    ///      the body and ~236 from the caps — beyond any plausible radius —
    ///      and the unit probes compute their presses *from* the constants, so
    ///      they follow a mutated radius wherever it goes. This press is
    ///      absolute: close enough to the line that a radius grown to a few
    ///      dozen points would swallow it, far enough that the shipped radii
    ///      (14 and 8) must not. The module doc calls the radius "the whole
    ///      contract with panning"; this is that contract observed at the
    ///      numbers it is set at.
    #[test]
    fn a_press_thirty_points_off_a_cap_and_twenty_off_the_body_still_pans() {
        let (mut h, a, b) = harness_with_committed_section();
        let a_px = h.screen_of(0, a);
        let b_px = h.screen_of(0, b);
        // 20 points perpendicular off the track, at the along-track distance
        // that puts the press exactly 30 points from the A cap:
        // sqrt(30² − 20²) ≈ 22.4 points toward B.
        let along = (b_px - a_px).normalized();
        let across = egui::vec2(along.y, -along.x);
        let start = a_px + along * (30.0f32.powi(2) - 20.0f32.powi(2)).sqrt() + across * 20.0;
        assert!(
            h.pane_rects()[0].contains(start),
            "precondition: the press is inside pane 0"
        );
        assert!(
            ((start - a_px).length() - 30.0).abs() < 0.1,
            "precondition: the press sits 30 points from the A cap"
        );

        let centre_before = h.pane_center(0);
        h.mouse_move(start);
        h.frame();
        h.mouse_press(start);
        let pressed = h.frame();
        assert!(
            !pressed.resolved.suppress_pan,
            "a press 30 points from the cap and 20 from the body suppressed \
             panning: a grab radius has grown into the map's pan gesture"
        );
        for step in 1..=3 {
            h.mouse_move(start + egui::vec2(30.0 * step as f32, 15.0 * step as f32));
            h.frame();
        }
        h.mouse_release(start + egui::vec2(90.0, 45.0));
        h.frames_for(2, FRAME_DT);

        assert_ne!(
            h.pane_center(0),
            centre_before,
            "an ordinary pan 30 points off a cap went missing"
        );
        assert_eq!(
            h.section_line(1),
            Some(SectionLine::new(a, b).expect("the fixture's line")),
            "a pan beside the line rewrote it"
        );
    }

    /// 46. **A tap while armed is discarded, and the mode stays armed.**
    ///
    ///     A stray tap is the single most likely thing to happen right after
    ///     arming — it is how a user checks which pane they are on. Turning it
    ///     into a zero-ish-length section is wrong; *silently disarming* is
    ///     worse, because the intent the user just expressed is gone with
    ///     nothing on screen to say so.
    #[test]
    fn a_tap_while_armed_draws_nothing_and_leaves_the_mode_armed() {
        let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
        h.load_scan("KTLX");
        h.set_section_draw_armed(true);
        h.warm_up();

        let pane = h.pane_rects()[0];
        let at = pane.center();
        h.mouse_move(at);
        h.frame();
        h.mouse_press(at);
        h.frame();
        // Under `MIN_SECTION_DRAG_PT` (24), and deliberately not zero: a
        // threshold mutated to `> 0.0` would pass a test that never moved.
        h.mouse_move(at + egui::vec2(9.0, 6.0));
        h.frame();
        h.mouse_release(at + egui::vec2(9.0, 6.0));
        h.frames_for(2, FRAME_DT);

        assert!(
            h.pane_kinds().iter().all(|k| *k == PaneKind::Map),
            "an 11-point drag became a cross-section"
        );
        assert!(
            h.section_draw_armed(),
            "a discarded drag disarmed the mode, throwing away the intent"
        );

        // And a real drag straight afterwards still works, so "stays armed"
        // means armed rather than merely not-disarmed.
        let to = at + egui::vec2(150.0, 90.0);
        h.mouse_press(at);
        h.frame();
        h.mouse_move(to);
        h.frame();
        h.mouse_release(to);
        h.frames_for(2, FRAME_DT);
        assert!(
            h.pane_kinds().contains(&PaneKind::CrossSection),
            "the still-armed mode did not draw the next line"
        );
    }

    /// 47. **While armed, a press on a map fires no overlay click and the map
    ///     does not pan** — for every pane the frame resolves, not just the one
    ///     the line is on.
    ///
    ///     `ArmedSectionFrame` makes both properties of the returned value
    ///     rather than rules each caller remembers, and this reads them back out
    ///     of the probe `render_panes` records from the very locals that feed
    ///     `PaneRenderCtx` and `drag_pan_buttons`.
    #[test]
    fn an_armed_press_suppresses_panning_and_fires_no_overlay_click() {
        let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
        h.load_scan("KTLX");
        h.warm_up();

        let pane = h.pane_rects()[0];
        let at = pane.center();

        // Unarmed first, so the assertion below is about being armed rather
        // than about a click that never happens.
        let unarmed = h.mouse_click(at);
        assert!(
            unarmed.resolved.overlay_click_pos.is_some(),
            "precondition: an unarmed click must reach the overlays"
        );
        assert!(!unarmed.resolved.suppress_pan, "precondition");

        h.set_section_draw_armed(true);
        h.warm_up();
        h.mouse_move(at);
        h.frame();
        let pressed = {
            h.mouse_press(at);
            h.frame()
        };
        assert_eq!(
            pressed.resolved.overlay_click_pos, None,
            "a press that starts a section line also opened an overlay popup \
             over the map being drawn on"
        );
        assert!(
            pressed.resolved.suppress_pan,
            "the map was left free to pan while a line was being drawn"
        );
        h.mouse_release(at);
        h.frames_for(2, FRAME_DT);
    }

    /// 48. **A pane that is not a map ignores the armed mode entirely.**
    ///
    ///     A line is aimed with a projector and a section pane has none, so
    ///     arming the mode with one active leaves it exactly as it was — and in
    ///     particular does not suppress that pane's pointer or swallow its
    ///     clicks. The press that picks a map out of the layout is the same
    ///     press that starts the line, because `detect_active_pane_click` runs
    ///     at the top of the frame.
    #[test]
    fn arming_the_draw_changes_nothing_for_a_pane_with_no_map() {
        let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
        h.set_pane_count(2);
        h.load_scan("KTLX");
        h.make_pane_unaimed_cross_section(0);
        h.set_section_draw_armed(true);
        h.warm_up();

        let at = h.pane_rects()[0].center();
        h.mouse_move(at);
        h.frame();
        h.mouse_press(at);
        let pressed = h.frame();
        assert!(
            !pressed.resolved.suppress_pan,
            "arming the draw suppressed panning on a pane that cannot be drawn on"
        );
        h.mouse_release(at);
        h.frames_for(2, FRAME_DT);
        assert_eq!(
            h.section_line(0),
            None,
            "a drag on a section pane aimed it at itself"
        );
        assert!(h.section_draw_armed(), "the mode should still be waiting");
    }

    // --- The two armed modal drags, together --------------------------------
    //
    // Neither feature's author could have written these: the cross-section draw
    // and the 3D region drag were built on the same base, in parallel, and this
    // is the first branch on which both exist. Everything below is about the
    // pair rather than about either one.

    /// **The region checkbox closes the drawer on arm, exactly as the section
    /// checkbox does.**
    ///
    /// On every width where the drawer is the menu it covers the map the box
    /// has to be dragged on, so arming and leaving it open would arm a gesture
    /// the user cannot make. Un-ticking is the asymmetric half and it is pinned
    /// too: disarming needs no map, so the drawer stays open where the user is.
    #[test]
    fn the_drawers_checkbox_arms_the_region_drag_and_closes_the_drawer() {
        let mut h = compact_with_drawer();
        h.load_scan("KTLX");
        assert!(!h.region_arm(), "precondition: it starts unarmed");

        h.mouse_click(clickable_leaf(&h, crate::ui::REGION_ARM_LABEL).center());
        h.frames_for(3, FRAME_DT);

        assert!(h.region_arm(), "the checkbox did not arm the drag");
        assert_eq!(
            h.menu_leaf(crate::ui::REGION_ARM_LABEL),
            None,
            "the drawer stayed open over the map the box has to be dragged on"
        );

        // Re-opened, the checkbox shows the mode it turned on — which is what a
        // user who armed it by accident needs in order to un-tick it.
        h.set_drawer_open(true);
        h.frames_for(2, FRAME_DT);
        assert_eq!(
            h.menu_leaf(crate::ui::REGION_ARM_LABEL).map(|l| l.value),
            Some(Some(true)),
            "the checkbox does not show the mode it just turned on"
        );

        // Un-ticking disarms and leaves the drawer where the user is: only
        // arming needs the map underneath.
        h.mouse_click(clickable_leaf(&h, crate::ui::REGION_ARM_LABEL).center());
        h.frames_for(3, FRAME_DT);
        assert!(!h.region_arm(), "the checkbox could not turn it off");
        assert!(
            h.menu_leaf(crate::ui::REGION_ARM_LABEL).is_some(),
            "disarming needs no map, so it must not slam the drawer shut"
        );
    }

    /// **Arming either modal drag disarms the other, and the menu says so.**
    ///
    /// The two entries are adjacent checkboxes in the same submenu and they arm
    /// the same gesture — press, move, release, on a map pane. With both on, one
    /// drag would have to mean two things at once, so exactly one may be armed.
    ///
    /// Driven through the drawer's own checkboxes rather than through the
    /// setters, because the claim is about what a user sees: the box that
    /// un-ticked itself has to *read* as un-ticked, or the mode they think they
    /// are in is not the one a drag will do. A rule enforced only in the setter
    /// would leave two ticked boxes on screen and one working gesture.
    #[test]
    fn arming_either_modal_drag_un_ticks_the_other_in_the_menu() {
        let mut h = compact_with_drawer();
        h.load_scan("KTLX");
        assert!(!h.section_draw_armed() && !h.region_arm(), "both start off");

        // Region first, then section.
        h.mouse_click(clickable_leaf(&h, crate::ui::REGION_ARM_LABEL).center());
        h.frames_for(3, FRAME_DT);
        assert!(h.region_arm(), "the region checkbox did not arm the drag");

        h.set_drawer_open(true);
        h.frames_for(2, FRAME_DT);
        h.mouse_click(clickable_leaf(&h, crate::ui::DRAW_CROSS_SECTION_LABEL).center());
        h.frames_for(3, FRAME_DT);
        assert!(h.section_draw_armed(), "the section checkbox did not arm");
        assert!(
            !h.region_arm(),
            "both drags are armed: one press would anchor a line and start a box"
        );

        h.set_drawer_open(true);
        h.frames_for(2, FRAME_DT);
        assert_eq!(
            h.menu_leaf(crate::ui::REGION_ARM_LABEL).map(|l| l.value),
            Some(Some(false)),
            "the region checkbox still shows ticked after being un-armed"
        );
        assert_eq!(
            h.menu_leaf(crate::ui::DRAW_CROSS_SECTION_LABEL)
                .map(|l| l.value),
            Some(Some(true)),
            "the section checkbox does not show the mode it just turned on"
        );

        // And the other way round, which is not symmetric for free: the two
        // dispatcher arms are separate code.
        h.mouse_click(clickable_leaf(&h, crate::ui::REGION_ARM_LABEL).center());
        h.frames_for(3, FRAME_DT);
        assert!(h.region_arm(), "the region checkbox did not re-arm");
        assert!(
            !h.section_draw_armed(),
            "arming the region drag left the section draw armed"
        );

        h.set_drawer_open(true);
        h.frames_for(2, FRAME_DT);
        assert_eq!(
            h.menu_leaf(crate::ui::DRAW_CROSS_SECTION_LABEL)
                .map(|l| l.value),
            Some(Some(false)),
            "the section checkbox still shows ticked after being un-armed"
        );
    }

    /// **A back press cancels whichever modal drag is armed.**
    ///
    /// One layer for both, below every painted layer — see
    /// `Gui::dismiss_top_layer`. The reason it matters for the region drag is the
    /// reason it mattered for the section draw: on Android a back press with a
    /// mode on would otherwise exit the app, which is the reading of it least
    /// likely to be what was meant.
    #[test]
    fn a_back_press_cancels_whichever_modal_drag_is_armed() {
        let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
        h.load_scan("KTLX");

        h.set_region_arm(true);
        assert!(h.gui_mut().dismiss_top_layer(), "the region drag was armed");
        assert!(!h.region_arm());

        h.set_section_draw_armed(true);
        assert!(h.gui_mut().dismiss_top_layer(), "the draw was armed");
        assert!(!h.section_draw_armed());

        assert!(
            !h.gui_mut().dismiss_top_layer(),
            "with nothing left, a back press is a request to leave the app"
        );
    }

    /// **Two appliers, and never both with something to apply.**
    ///
    /// `Gui::ui` runs `apply_pending_section_line` and then
    /// `apply_pending_region` after the pane loop, and each of them can grow the
    /// layout. Two growths in one frame is the case neither feature was written
    /// for: the second applier's target rule would run against a layout the first
    /// had already changed, and in a full layout both rules' last resort is the
    /// same pane — so the second would convert the pane the first had just
    /// filled, and one of two completed gestures would visibly produce nothing.
    ///
    /// It cannot happen, and this is why: arming is exclusive, only an armed mode
    /// records a pending, and each pending is recorded and consumed inside one
    /// frame. So one drag, however the modes were armed, produces **one** new
    /// pane of **one** kind — the kind the mode armed *last* asked for.
    ///
    /// Asserted on the pane count as well as the kinds, because a rule that
    /// produced the right kind by converting a pane the other applier had just
    /// grown would leave the count at three and one of the two panes empty.
    #[test]
    fn two_appliers_never_both_have_something_to_apply() {
        for section_last in [true, false] {
            let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
            h.load_scan("KTLX");
            // Both armed, in turn, through the setters the menu uses. The second
            // call is what disarms the first, and that is the whole subject.
            if section_last {
                h.set_region_arm(true);
                h.set_section_draw_armed(true);
            } else {
                h.set_section_draw_armed(true);
                h.set_region_arm(true);
            }
            h.warm_up();
            let before = h.pane_kinds().len();
            assert_eq!(before, 1, "precondition: one map pane to drag on");

            let pane = h.pane_rects()[0];
            let from = pane.center() - egui::vec2(120.0, 60.0);
            let to = pane.center() + egui::vec2(120.0, 60.0);
            h.mouse_move(from);
            h.frame();
            h.mouse_press(from);
            h.frame();
            for step in 1..=4 {
                h.mouse_move(from + (to - from) * (step as f32 / 4.0));
                h.frame();
            }
            h.mouse_release(to);
            h.frames_for(2, FRAME_DT);

            let kinds = h.pane_kinds();
            assert_eq!(
                kinds.len(),
                before + 1,
                "one drag grew the layout to {} panes (section_last={section_last}): {kinds:?}",
                kinds.len(),
            );
            assert_eq!(kinds[0], PaneKind::Map, "the map under the drag was spent");
            let wanted = if section_last {
                PaneKind::CrossSection
            } else {
                PaneKind::Volume
            };
            assert_eq!(
                kinds[1], wanted,
                "the mode armed last is not the one the drag did \
                 (section_last={section_last})",
            );
            assert_eq!(
                kinds.iter().filter(|k| **k != PaneKind::Map).count(),
                1,
                "one drag produced two non-map panes: {kinds:?}",
            );
        }
    }

    /// **While the section draw is armed, a drag commits no region — and the
    /// converse.**
    ///
    /// The narrower claim under the test above, and the one that would break
    /// first. The two gestures are read by *different* code on different paths:
    /// the section draw goes through `InteractionState::resolve_armed`, while
    /// `handle_region_drag` reads `ui.ctx().input()` raw from inside `Map::show`.
    /// Neither path knows about the other, so if the exclusion at the menu were
    /// ever relaxed both would fire from the same press — and the symptom would
    /// be a section pane *and* a 3D pane from one drag, which is exactly what a
    /// reader would assume could not happen.
    #[test]
    fn an_armed_section_drag_leaves_no_region_behind_and_the_converse() {
        for section in [true, false] {
            let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
            h.load_scan("KTLX");
            if section {
                h.set_section_draw_armed(true);
            } else {
                h.set_region_arm(true);
            }
            h.warm_up();

            let pane = h.pane_rects()[0];
            let from = pane.center() - egui::vec2(120.0, 60.0);
            let to = pane.center() + egui::vec2(120.0, 60.0);
            h.mouse_move(from);
            h.frame();
            h.mouse_press(from);
            h.frame();
            for step in 1..=4 {
                h.mouse_move(from + (to - from) * (step as f32 / 4.0));
                h.frame();
            }
            h.mouse_release(to);
            h.frames_for(2, FRAME_DT);

            let panes = h.pane_kinds().len();
            let aimed_regions = (0..panes)
                .filter(|idx| {
                    h.gui_mut()
                        .pane(*idx)
                        .and_then(|p| p.volume())
                        .is_some_and(|v| v.region.is_some())
                })
                .count();
            let lines = (0..panes)
                .filter(|idx| h.section_line(*idx).is_some())
                .count();
            if section {
                assert_eq!(lines, 1, "the drag drew no section line");
                assert_eq!(
                    aimed_regions, 0,
                    "a section drag also committed a 3D region"
                );
            } else {
                assert_eq!(aimed_regions, 1, "the drag committed no region");
                assert_eq!(lines, 0, "a region drag also drew a section line");
            }
        }
    }
}
