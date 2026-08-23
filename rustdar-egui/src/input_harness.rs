//! Headless input harness for [`Gui::ui`].
//!
//! Drives the real UI through a real [`egui::Context`] with hand-constructed
//! [`egui::RawInput`] — no window, no winit, no wgpu. Each [`InputHarness::frame`]
//! runs one full egui pass, and `render_panes` records the pointer state it
//! resolved for each pane. [`FrameOutcome::resolved`],
//! [`FrameOutcome::resolved_inactive`], [`FrameOutcome::modality`] and
//! [`FrameOutcome::resolved_zoom`] are reads of *that* — the shipped decision,
//! not a second one taken here. Anything claiming to be what the app does must
//! be read back out of [`Gui`].
//!
//! [`FrameOutcome::mouse`] and [`FrameOutcome::touch`] are the exceptions: they
//! drive each pipeline directly to say what it *would* have done. They are
//! ungated and no test may read them as the app's behaviour.
//!
//! # Event fidelity
//!
//! The pointer helpers emit exactly the event sequences the real integrations
//! produce. `egui-winit` 0.35.0 (`src/lib.rs`):
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
//! eframe 0.35.0's web canvas (`src/web/events.rs`):
//!
//! | DOM event     | emitted here                                                |
//! |---------------|-------------------------------------------------------------|
//! | `touchstart`  | `PointerButton{down}` **then** `Touch{Start}` — order flipped |
//! | `touchmove`   | `PointerMoved`, `Touch{Move}`                               |
//! | `touchend`    | `PointerButton{up}`, `PointerGone`, `Touch{End}`            |
//! | `touchcancel` | `Touch{Cancel}` **alone** — no release, no `PointerGone`    |
//! | `mousemove`   | `PointerMoved`                                              |
//!
//! A cancelled touch never reports a release and egui does not clear
//! `pointer.down` on `PointerGone`, so any gesture that only exits on "pointer
//! up" stays stuck forever; on the web there is no `PointerGone` either.

use crate::Gui;
use crate::pane::{PaneKind, SectionLine};
use crate::ui::DrawnMenuLeaf;
use crate::ui_input::{MapPointerFrame, TouchGestures};
use crate::ui_layout::PointerModality;
use rustdar_geo::GeoPoint;
use rustdar_radar::fields as radar_fields;
use rustdar_source::id::LayerId;

/// The radar layer's own field value for an id.
///
/// A [`ScanInfo`](rustdar_radar::types::ScanInfo) is radar's fact about a scan
/// and its tables are keyed by radar's own field, so these fixtures resolve
/// ids through the one door instead of naming the layer's type. A macro rather
/// than a function because the answer's type is the layer's, and a function
/// would have to write it down.
macro_rules! resolve {
    ($id:expr) => {
        rustdar_radar::fields::product_for($id).expect("a registered field")
    };
}

/// Viewport size used by the harness — a landscape desktop-ish window.
const SCREEN_SIZE: egui::Vec2 = egui::vec2(1024.0, 768.0);

/// Nominal seconds between harness frames (only used by [`InputHarness::frame`]).
const FRAME_DT: f64 = 1.0 / 60.0;

/// The pane pointer state produced by one harness frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct FrameOutcome {
    /// Pointer resolution from the mouse path, driven unconditionally.
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
    /// The fill colour each of [`last_rects`](Self::last_rects) was painted
    /// with, in the same order. Separate so the many rect-only readers keep
    /// their shape; a styling test zips the two.
    last_rect_fills: Vec<egui::Color32>,
    /// `RawInput::max_texture_side` — what `egui_winit` is handed from
    /// `device.limits().max_texture_dimension_2d`, and what
    /// `plan_overlay_texture` reads back through `ui.ctx().input(..)`.
    max_texture_side: Option<usize>,
    /// The [`GuiAction`]s `Gui::ui` returned from the last frame.
    last_actions: Vec<crate::actions::GuiAction>,
    /// Every text run painted during the last frame, with its layout rect.
    last_texts: Vec<(egui::Rect, String)>,
    /// Every textured quad painted during the last frame — see [`PaintedImage`].
    last_images: Vec<PaintedImage>,
    /// Every line segment painted during the last frame, with its stroke.
    last_segments: Vec<(egui::Pos2, egui::Pos2, egui::Stroke)>,
    /// The soonest repaint any viewport asked for on the last frame.
    last_repaint_delay: std::time::Duration,
    /// Rects that came back under a different widget id between passes,
    /// accumulated over every frame since the last [`InputHarness::clear_id_changes`].
    id_changes: Vec<egui::Rect>,
    /// The previous pass's widget bookkeeping, diffed against each new pass by
    /// [`id_changes_between`] to feed [`InputHarness::id_changes`].
    prev_widgets: egui::WidgetRects,
    /// The frame-input facts this harness owns, mirroring the `App`'s own
    /// fields. See [`FrameFactsForTest`].
    facts: FrameFactsForTest,
}

/// The harness's copy of the App-owned frame-input facts.
struct FrameFactsForTest {
    safe_area_insets: (f32, f32, f32, f32),
    supports_exit: bool,
    loop_frame_budget: usize,
    concurrent_renders: usize,
    location_settings_available: bool,
    location: (rustdar_location::LocationPermission, bool),
    gps: Option<(rustdar_location::Fix, web_time::Instant)>,
    user_heading: Option<f32>,
    catalogue_pending: bool,
    /// The radar layer's own liveness, held in its own type and published
    /// through the opaque seam by [`Self::liveness`] below — the harness
    /// composes what the App composes (WO-E8c).
    radar_liveness: crate::radar_layer::RadarLiveness,
    floor_tile_zoom_bias: u8,
}

impl Default for FrameFactsForTest {
    fn default() -> Self {
        Self {
            safe_area_insets: (0.0, 0.0, 0.0, 0.0),
            // `Gui::new`'s own answers, so default-facts over a fresh `Gui`
            // is the identity.
            supports_exit: true,
            loop_frame_budget: 60,
            concurrent_renders: rustdar_device_profile::constants::MAX_CONCURRENT_RENDERS,
            location_settings_available: false,
            location: (rustdar_location::LocationPermission::default(), false),
            gps: None,
            user_heading: None,
            catalogue_pending: false,
            radar_liveness: crate::radar_layer::RadarLiveness::default(),
            floor_tile_zoom_bias: 0,
        }
    }
}

/// A textured quad the last frame painted: where it went, and **which way up**
/// its texture was mapped onto it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PaintedImage {
    /// The screen rect the quad covers.
    pub rect: egui::Rect,
    /// The texture coordinate at [`rect`](Self::rect)'s top-left corner. `(0,0)`
    /// for an unflipped image, because egui's uv origin is the texture's top
    /// left.
    pub uv_at_top_left: egui::Pos2,
    /// The texture coordinate at [`rect`](Self::rect)'s bottom-right corner.
    pub uv_at_bottom_right: egui::Pos2,
    /// **Which texture was sampled.** The identity, not the geometry: two
    /// frames of one loop paint the same rect from different pictures, so a
    /// test that reads only [`rect`](Self::rect) cannot tell a moving playhead
    /// from a stuck one.
    pub texture: egui::TextureId,
}

/// Read a textured quad's geometry back off the mesh `Painter::image` built.
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
        texture: mesh.texture_id,
    })
}

/// The finished pass's widget bookkeeping, read back out of the context.
fn pass_widgets(ctx: &egui::Context) -> egui::WidgetRects {
    ctx.viewport(|viewport| viewport.prev_pass.widgets.clone())
}

/// Rects that came back under a different widget id while staying put: the
/// verdict of `egui::context::warn_if_rect_changes_id` — the check that logs
/// `Widget rect … changed id between passes` on device — mirrored condition
/// for condition (`egui-0.35.0/src/context.rs:4177`) over the same
/// [`egui::WidgetRects`] bookkeeping it runs on.
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

/// The radars every harness in this crate draws, placed once.
fn install_radars() {
    use rustdar_radar::site_position::SitePosition;
    use rustdar_radar::sites::SiteFix;

    /// `(ICAO, latitude, longitude, site_height_m, tower_height_m)`.
    const SITES: [(&str, i32, i32, i32, i32); 12] = [
        ("KTLX", 35_333_060, -97_277_500, 370, 19),
        ("TOKC", 35_276_000, -97_510_000, 386, 386),
        ("KOUN", 35_236_000, -97_463_000, 357, 19),
        ("KINX", 36_175_000, -95_565_000, 204, 30),
        ("KVNX", 36_741_000, -98_128_000, 369, 30),
        ("KDDC", 37_761_000, -99_969_000, 789, 24),
        ("KAMA", 35_233_000, -101_709_000, 1094, 24),
        ("KABX", 35_150_000, -106_824_000, 1789, 24),
        ("KDMX", 41_731_000, -93_723_000, 299, 30),
        ("KMPX", 44_849_000, -93_566_000, 288, 30),
        ("KABR", 45_455_830, -98_413_330, 397, 24),
        ("KMKX", 42_967_000, -88_550_000, 292, 30),
    ];

    // Idempotent anyway — `resolve` builds nothing when the fixes reproduce
    // the rows already there — but tests run in parallel and this is the
    // cheaper way to say so.
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        rustdar_radar::sites::resolve(SITES.map(
            |(name, lat_udeg, lon_udeg, site_height_m, tower_height_m)| {
                (
                    name,
                    SiteFix::Learned(SitePosition {
                        lat_udeg,
                        lon_udeg,
                        site_height_m,
                        tower_height_m,
                    }),
                )
            },
        ));
        // One radar the catalogue could list and never place, so the site
        // list's second inventory is exercised by the ordinary harness rather
        // than only by a test that goes looking for it. `KCRI` is a real one.
        rustdar_radar::sites::resolve([("KCRI", SiteFix::Unplaced)]);
        // The places the station record publishes for a handful of the rows
        // above, so the place-name search and the place-bearing row label are
        // exercised by the ordinary harness rather than only where a test
        // installs one. Real names, verbatim: the feed publishes one free-text
        // field per station and there is no state to split off.
        //
        // `SiteFix::Network` ranks BELOW `Learned`, so not one position moves
        // — the fixes above keep every row. Only `places` gains entries.
        const PLACES: [(&str, i32, i32, i32, &str); 4] = [
            ("KTLX", 35_333_060, -97_277_500, 370, "Twin Lakes"),
            ("KINX", 36_175_000, -95_565_000, 204, "Tulsa"),
            ("KMPX", 44_849_000, -93_566_000, 288, "Minneapolis"),
            ("KMKX", 42_967_000, -88_550_000, 292, "Milwaukee"),
        ];
        rustdar_radar::sites::resolve(PLACES.map(
            |(name, lat_udeg, lon_udeg, elevation_m, place)| {
                (
                    name,
                    SiteFix::Network {
                        lat_udeg,
                        lon_udeg,
                        elevation_m,
                        place: Some(place),
                    },
                )
            },
        ));
    });
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
        install_radars();
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
            last_rect_fills: Vec::new(),
            max_texture_side: None,
            last_actions: Vec::new(),
            last_texts: Vec::new(),
            last_images: Vec::new(),
            last_segments: Vec::new(),
            last_repaint_delay: std::time::Duration::MAX,
            id_changes: Vec::new(),
            prev_widgets: egui::WidgetRects::default(),
            facts: FrameFactsForTest::default(),
        };
        harness.warm_up();
        // The first frame's `check_auto_polls` starts the initial fetch and
        // nothing here ever completes it, so without this every harness runs
        // with `fetching` latched true forever: the refresh button is
        // permanently `add_enabled(false)`, the status bar shows a spinner
        // instead of the auto-poll checkbox, and `FetchRadarScan`'s click path
        // is unreachable. Settling it puts the harness in the steady state the
        // app spends its life in rather than a transient no test intended.
        harness
            .gui
            .apply(crate::shell_api::GuiEvent::Fetching(false));
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

    /// Re-apply the whole fact set, as the App's per-frame compose
    /// (`push_frame_inputs`) does. Every fact-mutating helper funnels
    /// through here, so the `Gui` can never see half an update.
    fn apply_facts(&mut self) {
        let liveness = vec![crate::radar_layer::liveness_entry(
            self.facts.radar_liveness.clone(),
        )];
        self.gui.apply_frame_inputs(crate::shell_api::FrameInputs {
            safe_area_insets: self.facts.safe_area_insets,
            supports_exit: self.facts.supports_exit,
            loop_frame_budget: self.facts.loop_frame_budget,
            concurrent_renders: self.facts.concurrent_renders,
            location_settings_available: self.facts.location_settings_available,
            location: self.facts.location,
            gps: self.facts.gps.clone(),
            user_heading: self.facts.user_heading,
            catalogue_pending: self.facts.catalogue_pending,
            liveness: &liveness,
            floor_tile_zoom_bias: self.facts.floor_tile_zoom_bias,
        });
    }

    /// State that the site list is still short of the network, as `App::new`
    /// and `App::adopt_the_first_catalogue` do — through the frame-input
    /// facts, the route the App's own compose takes.
    pub(crate) fn set_catalogue_pending(&mut self, pending: bool) {
        self.facts.catalogue_pending = pending;
        self.apply_facts();
        self.warm_up();
    }

    /// Report host safe-area insets, as the Android side channel does.
    pub(crate) fn set_safe_area_insets(&mut self, top: f32, bottom: f32, left: f32, right: f32) {
        self.facts.safe_area_insets = (top, bottom, left, right);
        self.apply_facts();
        self.warm_up();
    }

    /// State what the platform location service is doing, as the App's
    /// per-frame compose reads it off the location gate.
    pub(crate) fn set_location_state(
        &mut self,
        permission: rustdar_location::LocationPermission,
        active: bool,
    ) {
        self.facts.location = (permission, active);
        self.apply_facts();
    }

    /// State whether this platform has a location settings page, as `App::new`
    /// captures it once at startup.
    pub(crate) fn set_location_settings_available(&mut self, available: bool) {
        self.facts.location_settings_available = available;
        self.apply_facts();
    }

    /// Deliver a GPS fix, stamped at arrival exactly as `poll_platform_state`
    /// stamps one — the instant travels with the fix through every re-apply,
    /// so the settings pane's staleness question stays honest in tests too.
    pub(crate) fn set_gps_fix(&mut self, fix: rustdar_location::Fix) {
        self.facts.gps = Some((fix, web_time::Instant::now()));
        self.apply_facts();
    }

    /// State this build's loop frame cap, as `App::new` captures it from the
    /// resolved budgets.
    pub(crate) fn set_loop_frame_budget(&mut self, frames: usize) {
        self.facts.loop_frame_budget = frames;
        self.apply_facts();
    }

    /// Whether egui has a real widget registered under `id` from the last
    /// frame.
    pub(crate) fn widget_exists(&self, id: egui::Id) -> bool {
        self.ctx.read_response(id).is_some()
    }

    /// The scroll offset egui has stored under `id`, if any.
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
        kind: &LayerId,
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

    /// Scroll at `pos` in `step` increments until `pred` passes, or give up
    /// after `max_steps` frames. Returns whether the predicate ever passed.
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
    pub(crate) fn stack_row(&self, kind: &LayerId) -> Option<crate::ui::StackRowProbe> {
        self.stack().rows.into_iter().find(|row| row.kind == *kind)
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

    /// Close the inspector the user's way — its own `×`, which every host
    /// draws, the sheet included. A no-op when it is closed.
    pub(crate) fn close_inspector(&mut self) {
        let probe = self.inspector();
        if !probe.open {
            return;
        }
        assert!(
            probe.close.is_positive(),
            "the open inspector drew no × to close it with"
        );
        self.mouse_click(probe.close.center());
        self.warm_up();
        assert!(
            !self.inspector().open,
            "closing the inspector did not close it"
        );
    }

    /// Select `kind`'s options in the inspector the user's way: open the
    /// stack, scroll its row on screen, click it. Asserts the inspector's
    /// body arm for exactly that layer drew.
    pub(crate) fn open_layer_in_inspector(&mut self, kind: &LayerId) {
        // An inspector left open from a previous selection covers the rows —
        // as the right slide-over on Medium, and as the sheet's Inspector
        // page over its Layers page on Compact.
        self.close_inspector();
        // **A layer the active pane does not hold has no row, and the user's
        // route to it is the catalogue.** Since the stack became a curated
        // list, a registered layer that ships disabled is not in a fresh
        // pane's stack at all, so "scroll to its row" is not a walk that can
        // start — the tile that puts the row there is the step before it. Taken
        // through the catalogue rather than through the pane API on purpose:
        // it makes the parity walk prove, for every registered layer at every
        // width, that the catalogue's add really lands a row.
        if self.stack_row(kind).is_none() {
            self.add_layer_from_catalog(kind);
        }
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
        // The row's eye is the layer's one visibility switch since the Show
        // toggle's de-dup (contract 86) — asserted on the route every caller
        // takes, so no walk can reach a layer body whose on/off went missing.
        assert_ne!(
            row.eye,
            egui::Rect::NOTHING,
            "{kind:?}'s row carries no visibility eye"
        );
        self.mouse_click(row.rect.center());
        self.warm_up();
        assert_eq!(
            self.inspector().mode,
            Some(crate::ui::InspectorSelection::Layer(kind.clone())),
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
    pub(crate) fn pill(
        &self,
        idx: usize,
        kind: crate::ui::PillKind,
    ) -> Option<(String, egui::Rect)> {
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

    /// Whether the UI is faded — the state the fade contracts
    /// assert beside the probes' drawn/not-drawn evidence.
    pub(crate) fn faded(&self) -> bool {
        self.gui.ui_faded_for_test()
    }

    /// What the last frame's layer catalog drew.
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

    /// Open the layer catalog the user's way: the stack's top
    /// `+ Show a layer` button. Asserts it really opened.
    pub(crate) fn open_catalog(&mut self) {
        if self.catalog().open {
            return;
        }
        self.open_layers();
        let add = self.stack().add_top;
        assert!(
            add.is_positive(),
            "the stack drew no catalog button to open the catalog with"
        );
        self.mouse_click(add.center());
        self.warm_up();
        assert!(
            self.catalog().open,
            "clicking {} did not open the catalog",
            crate::ui::ADD_LAYER_LABEL,
        );
    }

    /// **Put `kind` in the active pane's stack the user's way**: open the
    /// catalog, click its tile, and assert a row appeared for it.
    ///
    /// The real route, not `PaneState::add_layer` — a helper that reached
    /// past the chrome would let the chrome's own add rot untested while every
    /// caller of this went on passing.
    pub(crate) fn add_layer_from_catalog(&mut self, kind: &LayerId) {
        let name = self.overlay_display_name(kind).to_owned();
        self.open_catalog();
        let tile = self
            .catalog_tile(crate::ui::CatalogGroup::Layers, &name)
            .unwrap_or_else(|| panic!("the catalog drew no Overlays tile for {kind:?} ({name:?})"));
        self.mouse_click(tile.rect.center());
        self.warm_up();
        assert!(
            self.gui
                .pane(self.active_pane_index())
                .is_some_and(|pane| { pane.draw_order().any(|held| held == kind) }),
            "the catalog's {name:?} tile did not put {kind:?} in the active \
             pane's stack",
        );
    }

    /// Put `kind` in pane `idx`'s stack without going through the chrome — for
    /// the callers that need a layer *present but off*, which the catalog's
    /// tile cannot produce because it switches what it adds on.
    pub(crate) fn add_layer_to_pane(&mut self, idx: usize, kind: &LayerId) {
        self.gui.add_layer_on_pane_for_test(idx, kind);
        self.warm_up();
    }

    /// **Fill the active pane's stack with every registered layer**, through
    /// the same door the catalog uses.
    ///
    /// For the tests whose subject is a *long* list — the panel's scroll, its
    /// height against the bottom clearance, the id stability of a scrolled
    /// body. A curated stack starts at the handful of layers that ship
    /// enabled, which is short enough that a panel need not scroll at all, and
    /// a scroll test on a list that fits is a test that cannot fail. This is
    /// what those tests reach for instead of the old always-complete stack,
    /// and it is a state a user can really be in: everything in the catalog,
    /// added.
    pub(crate) fn fill_stack(&mut self) {
        let idx = self.active_pane_index();
        let every: Vec<LayerId> = self.gui.overlays.handlers().map(|h| h.id()).collect();
        for id in &every {
            self.gui.add_layer_on_pane_for_test(idx, id);
        }
        self.warm_up();
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
    pub(crate) fn painted_text_strings_in(&self, rect: egui::Rect) -> Vec<String> {
        self.last_texts
            .iter()
            .filter(|(r, _)| rect.contains(r.center()))
            .map(|(_, text)| text.clone())
            .collect()
    }

    /// Lay the frames out at a scale other than 1 physical pixel per point.
    pub(crate) fn set_pixels_per_point(&mut self, ppp: f32) {
        self.ctx.set_pixels_per_point(ppp);
        self.warm_up();
    }

    /// Every text run the last frame painted, with the rect it occupies.
    pub(crate) fn painted_text_rects(&self) -> Vec<(egui::Rect, String)> {
        self.last_texts.clone()
    }

    /// Every textured quad the last frame painted whose rect is inside `rect`.
    pub(crate) fn painted_images_in(&self, rect: egui::Rect) -> Vec<PaintedImage> {
        self.last_images
            .iter()
            .copied()
            .filter(|image| rect.contains(image.rect.center()))
            .collect()
    }

    /// Every line segment the last frame painted inside `rect` with `color`.
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

    /// Every line segment the last frame painted inside `rect`, with the
    /// stroke each was painted with — [`Self::painted_segments_in`] without a
    /// colour to look for.
    pub(crate) fn all_segments_in(
        &self,
        rect: egui::Rect,
    ) -> Vec<(egui::Pos2, egui::Pos2, egui::Stroke)> {
        self.last_segments
            .iter()
            .filter(|(a, b, _)| rect.contains(*a) && rect.contains(*b))
            .copied()
            .collect()
    }

    /// Whether `needle` was painted anywhere inside `rect`.
    pub(crate) fn text_painted_in(&self, rect: egui::Rect, needle: &str) -> bool {
        self.last_texts
            .iter()
            .any(|(r, text)| rect.contains(r.center()) && text.contains(needle))
    }

    /// The display name the registry gives `kind` — what the stack rows and
    /// the catalog's overlay tiles both print.
    pub(crate) fn overlay_display_name(&self, kind: &LayerId) -> &str {
        self.gui.overlays.display_name(kind)
    }

    /// Whether the **live** active pane has `kind` on — the state the menu
    /// checkbox claims to be showing.
    pub(crate) fn overlay_enabled(&self, kind: &LayerId) -> bool {
        self.gui.active_pane().is_overlay_enabled(kind)
    }

    /// Whether pane `idx` has `kind` on, whichever pane is active.
    pub(crate) fn overlay_enabled_on(&self, idx: usize, kind: &LayerId) -> bool {
        self.gui
            .pane(idx)
            .unwrap_or_else(|| panic!("no pane {idx}"))
            .is_overlay_enabled(kind)
    }

    /// Which pane is currently active.
    pub(crate) fn active_pane_index(&self) -> usize {
        self.gui.active_pane_index_for_test()
    }

    /// Set every pane's layer link at once — off lets the panes disagree
    /// (no source propagates, no target is written), on restores the
    /// default convergence. The one-call stand-in for the retired
    /// `sync_layers` global.
    pub(crate) fn set_layer_links(&mut self, on: bool) {
        self.gui.set_layer_links_for_test(on);
        self.warm_up();
    }

    /// Whether every pane's layer link is on — the default the sync
    /// contracts assert before driving the fan-out.
    pub(crate) fn all_layer_linked(&self) -> bool {
        self.gui.all_layer_linked_for_test()
    }

    /// Set one pane's overlay state directly, writing both the enabled map and
    /// the config the layers panel reloads from each frame — otherwise the
    /// next frame undoes it.
    pub(crate) fn set_overlay_on_pane(&mut self, idx: usize, kind: &LayerId, on: bool) {
        self.gui.set_overlay_on_pane_for_test(idx, kind, on);
        self.warm_up();
    }

    /// The pane-count buttons the picker drew on the last frame.
    pub(crate) fn pane_options(&self) -> Vec<crate::ui::PaneOptionProbe> {
        self.gui.pane_options_for_test().to_vec()
    }

    /// Just the counts, in draw order.
    /// The split-orientation buttons the last frame drew, in draw order.
    pub(crate) fn split_options(&self) -> Vec<crate::ui::SplitOptionProbe> {
        self.gui.split_options_for_test().to_vec()
    }

    /// One split-orientation button by the orientation it sets.
    pub(crate) fn split_option(
        &self,
        orientation: crate::pane::SplitOrientation,
    ) -> Option<crate::ui::SplitOptionProbe> {
        self.split_options()
            .into_iter()
            .find(|probe| probe.orientation == orientation)
    }

    /// The grid the layout is actually laid out on: columns per row.
    pub(crate) fn pane_grid(&self) -> Vec<usize> {
        self.gui.pane_layout_for_test().grid().to_vec()
    }

    /// The egui context the harness drives frames through — what a call into
    /// the `Gui` that closes popovers needs.
    pub(crate) fn ctx(&self) -> &egui::Context {
        &self.ctx
    }

    pub(crate) fn pane_option_counts(&self) -> Vec<usize> {
        self.pane_options().iter().map(|o| o.count).collect()
    }

    /// The number of panes the layout is currently split into.
    pub(crate) fn pane_count(&self) -> usize {
        self.gui.pane_count()
    }

    /// What kind each visible pane *is* — the **input** to `render_panes`' kind
    /// branch, read off the live pane state.
    pub(crate) fn pane_kinds(&self) -> Vec<rustdar_radar::types::RenderView> {
        self.gui
            .panes()
            .iter()
            .map(|pane| pane.render_view())
            .collect()
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
            // No clocks: the harness draws geometry, and an age it
            // invented would be a number a pixel test could pin.
            vec![0; rungs.len()],
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
    pub(crate) fn set_section_draw_armed(&mut self, armed: bool) {
        self.gui.set_section_draw_armed(armed);
    }

    /// Whether the 3D region pick is armed, as the toggle sets it.
    pub(crate) fn region_pick_armed(&self) -> bool {
        self.gui.region_pick_armed()
    }

    /// Arm or disarm the 3D region pick.
    pub(crate) fn set_region_pick_armed(&mut self, armed: bool) {
        self.gui.set_region_pick_armed(armed);
    }

    /// The region pane `idx` has stored, or `None` for the volume's own reach.
    pub(crate) fn volume_region(&self, idx: usize) -> Option<crate::pane::VolumeRegion> {
        self.gui.pane(idx)?.volume()?.region
    }

    /// The map pane `idx`'s region was picked on, if it is a 3D pane that was
    /// aimed from one.
    pub(crate) fn volume_source_pane(&self, idx: usize) -> Option<usize> {
        self.gui.pane(idx)?.volume()?.source_pane
    }

    /// Drag a square out while the region pick is armed: press at `centre`,
    /// move to `corner`, release there.
    pub(crate) fn drag_region(&mut self, centre: egui::Pos2, corner: egui::Pos2) {
        self.mouse_move(centre);
        self.frame();
        self.mouse_press(centre);
        self.frame();
        self.mouse_move(corner);
        self.frame();
        self.mouse_release(corner);
        self.frames_for(2, FRAME_DT);
    }

    /// The line pane `idx` is aimed along, if it is a section pane with one.
    pub(crate) fn section_line(&self, idx: usize) -> Option<SectionLine> {
        self.gui.pane(idx)?.cross_section()?.line
    }

    /// Pane `idx`'s own map centre, as the shipped `render_panes` left it.
    pub(crate) fn pane_center(&self, idx: usize) -> Option<walkers::Position> {
        self.gui.pane(idx)?.map_memory.detached()
    }

    /// Where pane `idx` is looking at `pos`, on the ground.
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
            .set_view(rustdar_radar::types::RenderView::Volume);
        self.warm_up();
    }

    /// Convert pane `idx` back to a plan-view map pane.
    pub(crate) fn make_pane_map(&mut self, idx: usize) {
        self.gui
            .pane_mut(idx)
            .unwrap_or_else(|| panic!("no pane {idx}"))
            .set_view(rustdar_radar::types::RenderView::PlanView);
        self.warm_up();
    }

    /// What the 3D arm decided for each volume pane on the last frame.
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

    /// The pane borders the last frame painted: pane index, the stroke's
    /// painted bounds, and what the border encodes about links.
    pub(crate) fn pane_borders(&self) -> Vec<(usize, egui::Rect, crate::ui::map::PaneBorderMarks)> {
        self.gui.pane_borders_for_test().to_vec()
    }

    /// The section tracks the last frame painted over map panes: map pane,
    /// section pane, and the painted A and B endpoints.
    pub(crate) fn section_tracks(&self) -> Vec<(usize, usize, egui::Pos2, egui::Pos2)> {
        self.gui.section_tracks_for_test().to_vec()
    }

    /// The Volume Alpha corner buttons the last frame drew, per pane.
    pub(crate) fn alpha_buttons(&self) -> Vec<(usize, egui::Rect)> {
        self.gui.alpha_buttons_for_test().to_vec()
    }

    /// Pane `idx`'s dispatched kinds in paint order, with the layer each
    /// painted into — the draw-order pin's read side.
    pub(crate) fn paint_order(&self, idx: usize) -> Vec<(LayerId, egui::LayerId)> {
        self.gui.paint_order_for_test(idx)
    }

    /// Deliver a scan for `site`, through the host's own delivery path.
    pub(crate) fn load_scan(&mut self, site: &str) {
        let radar_site = rustdar_radar::sites::get_radar_site(site).expect("unknown radar site");
        let info = rustdar_radar::types::ScanInfo {
            site: radar_site.clone(),
            site_source: rustdar_radar::site_position::SitePositionSource::Table,
            site_position: None,
            timestamp: chrono::NaiveDate::from_ymd_opt(2026, 7, 24)
                .unwrap()
                .and_hms_opt(18, 30, 0)
                .unwrap(),
            vcp_number: 212,
            available_products: vec![
                resolve!(&radar_fields::known::REFLECTIVITY),
                resolve!(&radar_fields::known::VELOCITY),
            ],
            product_elevations: Default::default(),
            status: String::new(),
        };
        // The host matches panes by site, so point them at it first.
        for pane in self.gui.panes_mut() {
            pane.set_site(site.to_owned());
        }
        let collected = info.timestamp;
        self.gui.apply(crate::shell_api::GuiEvent::ScanInfoForSite {
            site: site.to_owned(),
            info,
        });
        // And the substrate half, because that is what a volume arrival does:
        // `App` writes its base holder and applies `ScanInfoForSite` from the
        // same arm and publishes the current-volume stamp each frame. A harness
        // that filled only the plan view's half would leave a 3D pane waiting
        // for a volume that, in production, had already landed.
        self.set_current_volume(site, Some(collected));
        self.warm_up();
    }

    /// Say what `site`'s current-volume stamp is, or that the site has no
    /// volume at all yet. **Intent unmoved** (WO-E8c): the same fact, now
    /// carried in the radar layer's own payload rather than a typed member of
    /// the seam.
    pub(crate) fn set_current_volume(
        &mut self,
        site: &str,
        collected: Option<chrono::NaiveDateTime>,
    ) {
        let mut volumes = std::collections::HashMap::new();
        if let Some(collected) = collected {
            volumes.insert(
                site.to_owned(),
                crate::radar_layer::CurrentVolumeStamp {
                    newest: collected,
                    // A pure base volume: what an archive arrival publishes.
                    base_started: Some(collected),
                },
            );
        }
        self.facts.radar_liveness.current_volumes = volumes;
        self.apply_facts();
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
    pub(crate) fn offer_product(
        &mut self,
        idx: usize,
        product: &rustdar_source::product::FieldId,
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
        let resolved = resolve!(&product);
        if !info.available_products.contains(&resolved) {
            info.available_products.push(resolved);
            // Sorted by the field's own registered order, read from the
            // registry rather than from a method on the layer's enum.
            info.available_products
                .sort_by_key(|p| rustdar_radar::fields::spec(*p).sort_order);
        }
        let angles = info.product_elevations.entry(resolved).or_default();
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
        product: &rustdar_source::product::FieldId,
    ) {
        let pane = self
            .gui
            .pane_mut(idx)
            .unwrap_or_else(|| panic!("no pane {idx}"));
        if pane.selected_product() != *product {
            pane.set_selected_product(product.clone());
            pane.set_selected_elevation(0.0);
        }
        self.warm_up();
    }

    /// Place a finished radar image on pane `idx`, as `apply_render_to_pane` does
    /// when a render lands: a texture in the pane's Radar overlay cache, with the
    /// metadata that says what it depicts.
    pub(crate) fn place_radar_image(
        &mut self,
        idx: usize,
        product: &rustdar_source::product::FieldId,
        elevation: f32,
        nyquist_ms: Option<f64>,
        melting_layer_source: Option<rustdar_radar::hca::MeltingLayerSource>,
        storm_motion: Option<rustdar_radar::srv::SrvMotion>,
    ) {
        use crate::overlay_cache::{OverlayTextureData, RadarTextureMeta};
        use rustdar_radar::types::{BASE_EXTENT_KM, ImageBounds};

        // The extent this fixture's image was "projected" at. One name for the
        // bounds and the metadata, because production derives both from the
        // render's single `max_range_km` and a fixture that let them drift
        // would be modelling a state the host cannot reach.
        let extent_km = BASE_EXTENT_KM;

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
        let bounds = ImageBounds::from_radar_site(lat, lon, extent_km);
        let cache = self
            .gui
            .pane_mut(idx)
            .unwrap()
            .overlay_cache_mut(&rustdar_source::id::known::RADAR);
        cache.show(OverlayTextureData {
            texture,
            placed: rustdar_geo::PlacedRaster::of(rustdar_geo::GeoBounds {
                min_lat: bounds.min_lat,
                max_lat: bounds.max_lat,
                min_lon: bounds.min_lon,
                max_lon: bounds.max_lon,
            }),
            data_generation: 0,
            render_zoom: 0,
            width: 1,
            height: 1,
            radar_meta: Some(RadarTextureMeta {
                hover: std::sync::Arc::new(rustdar_radar::hover::HoverSource::empty()),
                lat,
                lon,
                max_range_km: extent_km,
                nyquist_ms,
                melting_layer_source,
                storm_motion,
                product: product.clone(),
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

    /// The fill of each of [`Self::painted_rects`], in the same order.
    pub(crate) fn painted_fills(&self) -> &[egui::Color32] {
        &self.last_rect_fills
    }

    /// Where `text` was painted inside `rect`, if it was.
    pub(crate) fn text_rect_in(&self, rect: egui::Rect, text: &str) -> Option<egui::Rect> {
        self.last_texts
            .iter()
            .find(|(bounds, painted)| painted == text && rect.contains(bounds.center()))
            .map(|(bounds, _)| *bounds)
    }

    /// **Take the OS theme the app has no override for.** `features.md` says
    /// there is no in-app theme switch, so both are reachable and neither is
    /// opt-in — which is why anything the app paints for itself has to be
    /// checked in both.
    pub(crate) fn set_os_theme(&mut self, dark: bool) {
        self.ctx.set_theme(if dark {
            egui::ThemePreference::Dark
        } else {
            egui::ThemePreference::Light
        });
        self.warm_up();
    }

    /// Rects that came back under a different widget id between passes, since
    /// the last [`InputHarness::clear_id_changes`].
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

    /// The style's minimum interact size, as the running context resolves it
    /// — the floor the transport's buttons are held to.
    pub(crate) fn interact_size(&self) -> egui::Vec2 {
        self.ctx.global_style().spacing.interact_size
    }

    /// The handle shape the live style draws sliders with — what
    /// `ui_timeline::slider_travel_px` needs to say where the rail's travel
    /// begins and ends.
    pub(crate) fn handle_shape(&self) -> egui::style::HandleShape {
        self.ctx.global_style().visuals.handle_shape
    }

    /// The trough colour the live style paints an untouched slider's rail
    /// with, read from the same context the frame was painted from.
    pub(crate) fn inactive_bg_fill(&self) -> egui::Color32 {
        self.ctx.global_style().visuals.widgets.inactive.bg_fill
    }

    /// How tall the live style makes a slider's rail band.
    pub(crate) fn slider_rail_height(&self) -> f32 {
        self.ctx.global_style().spacing.slider_rail_height
    }

    /// Forget the actions seen so far, so a second gesture in one test is
    /// read on its own rather than against the first one's leftovers.
    pub(crate) fn clear_actions(&mut self) {
        self.last_actions.clear();
    }

    /// The style's selection background fill — the colour a
    /// `Button::selected(true)` frames itself with, read from the same
    /// context the frame was painted from.
    pub(crate) fn selection_bg_fill(&self) -> egui::Color32 {
        self.ctx.global_style().visuals.selection.bg_fill
    }

    /// The fill colours of every rect the last frame painted whose bounds sit
    /// inside `rect` (with `slack` points of tolerance), in paint order.
    pub(crate) fn painted_fills_within(&self, rect: egui::Rect, slack: f32) -> Vec<egui::Color32> {
        self.last_rects
            .iter()
            .zip(&self.last_rect_fills)
            .filter(|(r, _)| rect.expand(slack).contains_rect(**r))
            .map(|(_, fill)| *fill)
            .collect()
    }

    /// The colour-scale legend bars painted inside `pane`, classified by the
    /// axis they run along.
    pub(crate) fn color_scale_bars(&self, pane: egui::Rect) -> (usize, usize) {
        let mut horizontal = 0;
        let mut vertical = 0;
        for image in &self.last_images {
            let rect = image.rect;
            if !pane.contains(rect.center()) {
                continue;
            }
            let (w, h) = (rect.width(), rect.height());
            if (h - 20.0).abs() < 0.5 && w > 40.0 {
                horizontal += 1;
            } else if (w - 20.0).abs() < 0.5 && h > 40.0 {
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

    /// Read access to the UI under test (e.g. to assert what a frame left).
    pub(crate) fn gui(&self) -> &Gui {
        &self.gui
    }

    /// Mutable access to the UI under test (e.g. to open a dialog).
    pub(crate) fn gui_mut(&mut self) -> &mut Gui {
        &mut self.gui
    }

    /// Whether a floating layer (dialog / popup) currently covers `pos`.
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
        (self.last_rects, self.last_rect_fills) = full_output
            .shapes
            .iter()
            .filter_map(|clipped| match &clipped.shape {
                egui::Shape::Rect(rect_shape) => Some((rect_shape.rect, rect_shape.fill)),
                _ => None,
            })
            .unzip();
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
        self.last_repaint_delay = full_output
            .viewport_output
            .values()
            .map(|v| v.repaint_delay)
            .min()
            .unwrap_or(std::time::Duration::MAX);
        outcome
    }

    /// The soonest repaint the last frame asked for; `Duration::MAX` if none.
    pub(crate) fn repaint_delay(&self) -> std::time::Duration {
        self.last_repaint_delay
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

/// The pane grid at the width it is drawn at, the toggle over it, and closing
/// one specific pane.
#[cfg(test)]
mod pane_layout_tests;

#[cfg(test)]
mod tests;

/// Which picture a non-radar textured layer puts on the map — the WI-6 draw
/// fork, read off the frame that was really painted.
#[cfg(test)]
mod loop_overlay_draw_tests;

/// **The loading state** (WI-7): the quantity a pane shows while a loop's
/// data is on its way, read off the painted glass.
#[cfg(test)]
mod loop_loading_tests;

/// **A layer's own loop window** (WB-6): the one Lookback number, raised to the
/// floor the addressed layer declares, read off the action the ∞ button emits.
#[cfg(test)]
mod loop_span_floor_tests;
