use crate::actions::{GuiAction, RadarConfig};
use rustdar_overlays::render::controls::{
    ControlEffect, ControlItem, ControlUpdate, ControlValue, PaneControlContext,
    PaneControlContextMut,
};

const DEFAULT_INITIAL_ZOOM: f64 = 7.0;

use crate::pane::{ColorScaleOrientation, PaneId, PaneLayout, PaneState};
use crate::tiles::MapTileState;
use crate::ui_layout::{LayoutCtx, ModalityLatch};
use chrono::{NaiveDateTime, Timelike};
use egui::Context;
use rustdar_overlays::render::overlay_state::{OverlayKind, OverlayRegistry};
use rustdar_radar::types::{RadarProduct, ScanInfo};
use rustdar_units::UserPreferences;
use std::collections::HashMap;

#[path = "ui_chrome.rs"]
mod chrome;
#[path = "ui_config.rs"]
mod config;
#[path = "ui_map_overlays.rs"]
mod map_overlays;
#[path = "ui_popups.rs"]
mod popups;
#[path = "ui_menu.rs"]
mod ui_menu;
/// The cross-section arming toggle's label, for the same reason.
#[cfg(test)]
pub(crate) use ui_menu::DRAW_CROSS_SECTION_LABEL;
/// What the menu presentations actually drew last frame, for the input harness.
#[cfg(test)]
pub(crate) use ui_menu::DrawnMenuLeaf;
/// The 3D-pane toggle's label, for the input harness — so the tests that look
/// the entry up by name cannot go on passing after it is renamed.
#[cfg(test)]
pub(crate) use ui_menu::VOLUME_PANE_LABEL;
#[path = "ui_map.rs"]
mod map;
/// Re-exported so the input harness can name it: `map` is private to this
/// module, and the probe is the only thing outside it that has to be.
#[cfg(test)]
pub(crate) use map::VolumeArmProbe;
/// The copy the two non-map pane arms paint, for the input harness — so a test
/// can require the text to have been painted inside a given pane's rect without
/// keeping its own copy of the sentence. Same arrangement as [`DrawnMenuLeaf`].
#[cfg(test)]
pub(crate) use map::{CROSS_SECTION_EMPTY_STATE, VOLUME_EMPTY_STATE};
#[path = "ui_settings.rs"]
mod settings;

use crate::ui_input::InteractionState;

/// One pane-count button the picker drew, as it was drawn. See
/// [`ui_menu::DrawnMenuLeaf`] for the same shape and the reason for it.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PaneOptionProbe {
    pub count: usize,
    pub selected: bool,
    pub rect: egui::Rect,
}

/// Which render arm ran for one pane, recorded **inside the arm itself**.
///
/// The point is the asymmetry. `panes[i].kind()` is the *input* to
/// `render_panes`' single kind branch, so a test reading it back proves nothing
/// about the branch: a mis-wired arm, or an arm reading the kind off the
/// `mem::take`n slot instead of the taken value, agrees with it perfectly. Each
/// arm writes its own kind as a literal, so what this reports is the arm that
/// actually drew — the one thing a wrong branch cannot fake.
///
/// The rect comes along because "which arm ran" and "where it drew" are the two
/// halves of the same claim: an arm that painted the right thing into another
/// pane's rect is still wrong.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PaneContentProbe {
    pub pane_idx: usize,
    /// The kind the arm that ran is *for*, written by that arm.
    pub kind: crate::pane::PaneKind,
    pub rect: egui::Rect,
}

/// What the status bar drew, rather than the flags that decided it.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct StatusBarProbe {
    /// The scan summary text, verbatim — long or short form.
    pub scan_text: String,
    /// The Level III product age line, when one was drawn.
    pub product_age_text: Option<String>,
    /// The auto-poll checkbox's rect, when one was drawn.
    pub auto_poll: Option<egui::Rect>,
    /// The refresh button's rect — always drawn, so a test can click the real
    /// button rather than restating its position.
    pub refresh: egui::Rect,
    /// Whether the hover readout was drawn.
    pub hover: bool,
    /// The rect the panel actually claimed, straight off its own response —
    /// not the bottom slice of the screen worked out a second time.
    pub rect: egui::Rect,
}

#[cfg(test)]
impl Default for StatusBarProbe {
    fn default() -> Self {
        Self {
            scan_text: String::new(),
            product_age_text: None,
            auto_poll: None,
            refresh: egui::Rect::NOTHING,
            hover: false,
            rect: egui::Rect::NOTHING,
        }
    }
}

/// Radar fetch lifecycle state.
pub(super) struct RadarState {
    pub config: RadarConfig,
    pub fetching: bool,
    pub error_message: Option<String>,
}

/// Auto-polling timer state.
pub(super) struct AutoPollState {
    last_fetch_time: Option<web_time::Instant>,
    pub enabled: bool,
    initial_fetch_done: bool,
    interval_secs: u64,
}

impl AutoPollState {
    /// Record that a fetch was just dispatched.
    pub fn record_fetch(&mut self) {
        self.last_fetch_time = Some(web_time::Instant::now());
    }

    /// Call when a scan loads successfully — resets backoff to the base interval.
    pub fn on_success(&mut self) {
        self.interval_secs = 60;
    }

    /// Call on fetch failure — exponential backoff capped at 5 minutes.
    pub fn on_error(&mut self) {
        self.interval_secs = (self.interval_secs * 2).min(300);
    }

    /// Whether the poll timer has elapsed and a new check should fire.
    pub fn should_poll(&self) -> bool {
        self.enabled
            && self
                .last_fetch_time
                .is_some_and(|t| t.elapsed().as_secs() >= self.interval_secs)
    }

    /// Seconds remaining until the next poll, if a timer is running.
    pub fn time_until_next(&self) -> Option<u64> {
        self.last_fetch_time
            .map(|t| self.interval_secs.saturating_sub(t.elapsed().as_secs()))
    }

    /// Whether auto-poll has started (initial fetch done) and is enabled.
    pub fn is_active(&self) -> bool {
        self.enabled && self.initial_fetch_done
    }
}

/// How fresh the tilt on screen is.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct TiltFreshness {
    /// The elevation the active pane is actually rendering — the snapped angle,
    /// not the one the user selected.
    pub elevation: f32,
    /// Seconds since the radar collected the newest radial in that sweep.
    ///
    /// Counts up between cuts and drops back when the beam returns, so it reads
    /// as the real cadence of the tilt rather than as a countdown to a poll.
    /// This is the number the feature exists to make small.
    pub data_age_secs: u64,
}

/// What the real-time chunk feed is doing for the pane on screen.
///
/// Deliberately about *the tilt being shown* rather than about the feed's
/// progress through the volume. A count of completed cuts is operator jargon and
/// answers the wrong question: what a user needs to know is whether the image in
/// front of them is current, and a volume can be most of the way assembled while
/// their own tilt is still minutes old.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ChunkFeedStatus {
    /// Some live site is being fed from the real-time bucket.
    pub feeding: bool,
    /// A live site had its feed retired and fell back to the archive. Worth
    /// saying out loud: it is a silent drop from seconds of latency to minutes.
    pub retired: bool,
    /// The feed's own poll cadence, in seconds.
    pub interval_secs: u64,
    /// A push-notification socket is open, so chunks are fetched on arrival
    /// rather than on the next tick.
    pub pushed: bool,
    /// The active pane's tilt, once the feed has delivered it at least once.
    pub tilt: Option<TiltFreshness>,
}

/// Time editing dialog state.
pub(super) struct TimeDialogState {
    pub date_string: String,
    pub time_string: String,
    pub show: bool,
}

/// Where an in-flight cross-section draw started.
///
/// # `ground` is the endpoint; `screen` is only the gesture
///
/// The two are not redundant and they answer different questions.
///
/// `ground` is what the finished line is built from, and it is converted from
/// the pointer **inside `Map::show` on the press frame**, where the projector
/// is in hand. A pixel denotes different ground after any viewport change, and
/// an armed draw suppresses panning but *not* zooming — walkers reads the wheel
/// itself — so a pixel anchor held across a mid-drag zoom would silently re-aim
/// the line's near end while the far end tracked the finger. The user would get
/// a section of somewhere they never pointed at, with a perfectly convincing
/// picture of it.
///
/// `screen` is the anchor's position *as a gesture*, and it is the right
/// coordinate for exactly one question: did the finger travel far enough to mean
/// a line rather than a tap ([`MIN_SECTION_DRAG_PT`]). That is a question about
/// the hand, not about the ground, and re-deriving it from `ground` each frame
/// would make the threshold depend on the zoom level.
///
/// [`MIN_SECTION_DRAG_PT`]: crate::ui_input::MIN_SECTION_DRAG_PT
struct SectionAnchor {
    /// The map pane the draw started on.
    pane_idx: PaneId,
    /// Where it started, on the ground.
    ground: crate::pane::GeoPoint,
    /// Where it started, on screen.
    screen: egui::Pos2,
    /// Where the pointer is now, on screen. The far end of the rubber band.
    current: egui::Pos2,
}

pub struct Gui {
    radar: RadarState,
    auto_poll: AutoPollState,
    /// See [`Gui::live_chunks_enabled`].
    live_chunks: bool,
    /// See [`Gui::chunk_notifications_enabled`].
    chunk_notifications: bool,
    /// See [`Gui::notifier_endpoint`].
    notifier_endpoint: String,
    /// What the real-time feed is doing, refreshed each frame by the App.
    chunk_status: ChunkFeedStatus,
    /// When the archive last published a volume for each site, refreshed each
    /// frame by the App. The only volumes a 3D pane is built from — see
    /// `App::archive_scans` for why a live one is not one of them.
    archive_volumes: HashMap<String, NaiveDateTime>,
    time_dialog: TimeDialogState,
    initial_zoom_set: bool,
    // --- Map tiles (shared across panes) ---
    map_tiles: MapTileState,
    // User's GPS fix (full data from GPS receiver or Android LocationManager)
    user_fix: Option<rustdar_gps::GpsFix>,
    // Compass heading in degrees (0–360), from device compass sensor
    user_heading: Option<f32>,
    // Overlay data (SPC outlooks, NWS alerts, SPC discussions)
    pub overlays: OverlayRegistry,
    // Multi-pane state
    panes: Vec<PaneState>,
    active_pane: PaneId,
    pane_layout: PaneLayout,
    /// Remembered color-scale bar orientation for the map panel (hysteresis, so
    /// a resize near the boundary cannot make the bars hop).
    color_scale_orientation: ColorScaleOrientation,
    /// The map panel rect the last frame laid its pane grid out in. Only read
    /// by tests, which need the same rects `render_panes` used.
    #[cfg(test)]
    last_map_panel_rect: egui::Rect,
    /// egui `Id`s the last frame's layers panel actually resolved, in render
    /// order. Only read by tests, which compare them either side of a resize:
    /// an `Id` that moved with the layout silently discards the widget memory
    /// egui keyed on it.
    #[cfg(test)]
    widget_id_probes: Vec<(&'static str, egui::Id)>,
    /// The floating-chrome rects the last frame's chrome reported. Only read by
    /// tests, which check they match what was painted.
    #[cfg(test)]
    last_excluded_rects: Vec<egui::Rect>,
    /// Every menu leaf the last frame actually drew — whichever of the two
    /// presentations was on screen — with the bool each checkbox was really
    /// handed and the rect it landed in. Only read by tests, which need the
    /// state the *renderer* saw rather than the model a test rebuilt.
    #[cfg(test)]
    last_menu_leaves: Vec<ui_menu::DrawnMenuLeaf>,
    /// The pointer state `render_panes` resolved for each pane on the last frame,
    /// in pane order. Only read by tests — and the *only* honest way for one to
    /// observe the modality gate, since resolving it a second time alongside
    /// `Gui::ui` would assert on a replica.
    #[cfg(test)]
    last_pane_pointers: Vec<crate::ui_input::PanePointerProbe>,
    /// Which render arm ran for each pane on the last frame, in the order the
    /// pane loop reached them. Only read by tests — see [`PaneContentProbe`] for
    /// why this is written inside the arms rather than derived from
    /// `panes[i].kind()`.
    #[cfg(test)]
    last_pane_content: Vec<PaneContentProbe>,
    /// What the 3D arm decided for each volume pane on the last frame. Only read
    /// by tests, and it is the only thing that can tell "drew a volume" from
    /// "drew nothing" — see [`map::VolumeArmProbe`].
    #[cfg(test)]
    pub(crate) last_volume_arms: Vec<map::VolumeArmProbe>,
    /// The pane-count buttons the picker actually drew last frame. Only read by
    /// tests, which check the picker narrows on a phone while the config clamp
    /// does not, and that clicking one takes effect.
    #[cfg(test)]
    last_pane_options: Vec<PaneOptionProbe>,
    /// The excluded rects `render_panes` was actually handed. Only read by tests,
    /// which check the chrome's rects reach the map's click filter rather than
    /// stopping at the call site.
    #[cfg(test)]
    last_map_excluded_rects: Vec<egui::Rect>,
    /// What the last frame's status bar actually drew. Only read by tests.
    #[cfg(test)]
    last_status_bar: StatusBarProbe,
    /// Every handler dropdown the last frame drew, with the text its collapsed
    /// box showed. Only read by tests — see [`DrawnDropdown`].
    #[cfg(test)]
    last_dropdowns: Vec<DrawnDropdown>,
    /// A pane the user has asked to convert, applied once the UI pass is over.
    ///
    /// # Why the write is deferred, and what that is and is not protecting
    ///
    /// Two production paths hold a `PaneState` out of `Gui::panes` with
    /// `std::mem::take` for the whole of a pass — `render_layers_panel` takes the
    /// active pane, and `render_panes` takes each pane in turn — leaving a default
    /// `PaneState` in the slot. A `self.panes[idx].set_kind(..)` inside either
    /// window writes the **placeholder**, and the real pane going back afterwards
    /// discards it: no panic, no warning, and a control that will not stay set.
    ///
    /// **Today's menu dispatcher is not inside either window, and it is worth
    /// being exact about that rather than leaving a scarier map behind.**
    /// `render_layers_panel` takes the pane at `ui_chrome.rs:363` and puts it back
    /// at `:425`; `apply_menu_event` is not called until `:438`, with the real pane
    /// in the vector. The other dispatch site, `render_menu_bar_panel`, takes no
    /// pane at all. So a direct write from the volume toggle would in fact work
    /// today, and swapping this machinery out for one fails no behavioural test.
    ///
    /// It is still the right shape, for two reasons that are about the future
    /// rather than the present. The writers WP-G adds — an armed section drag
    /// resolving to a line, and the retarget rule that follows from it — run from
    /// **inside** `render_panes`' per-pane take, where the hazard is live and
    /// silent. And the ordering an interaction needs is the same one the pane count
    /// needs: growing it mid-loop moves the rects of panes the loop has not
    /// reached, desynchronising them from the ones `detect_active_pane_click`
    /// hit-tested this frame. One deferral point, applied at
    /// [`Self::apply_pending_pane_kind`] after the pane loop, serves both.
    ///
    /// The cost is one frame of latency in the current path: the dispatcher records
    /// during chrome, and the conversion lands after `render_panes` — the same
    /// frame, but the panes were already drawn from the old kind.
    ///
    /// One request at a time, not a queue. The requests are per pane and
    /// idempotent, they can only come from a single click, and a queue would let
    /// one frame convert a pane twice — which would throw away the per-kind state
    /// the intermediate kind had just been given.
    ///
    /// The deferral's *mechanism* is pinned by
    /// `a_pane_kind_request_survives_the_pane_being_held_out_of_the_vector`, which
    /// builds the take window by hand precisely because no production caller
    /// currently provides one.
    pending_pane_kind: Option<(PaneId, crate::pane::PaneKind)>,
    /// Whether the "pick a 3D region" mode is armed.
    ///
    /// While it is, a drag on a map pane draws the box a 3D pane resamples
    /// instead of panning the map — see `ui_region`. It stays armed through a
    /// commit *and* through a discarded mis-drag, and is turned off from the menu
    /// it was turned on from: a mode that disarmed itself would make aiming two
    /// panes, or re-aiming one that came out wrong, four clicks instead of two.
    /// [`Self::dismiss_top_layer`] also cancels it, so Escape and Android's back
    /// button mean here what they mean everywhere else — the same layer the
    /// cross-section draw sits on, and for the same reason.
    ///
    /// **Never on at the same time as [`section_draw_armed`](Self::section_draw_armed).**
    /// Both are armed modal drags on a map pane, and one drag cannot be two
    /// gestures — see [`Self::set_region_arm`].
    region_arm: bool,
    /// The region drag in flight, if any.
    ///
    /// Here rather than on the pane because it is a property of the gesture. It
    /// is written from inside `Map::show`, which is the only place a `Projector`
    /// exists and therefore the only place a pointer position can be turned into
    /// the ground it is over.
    region_drag: Option<crate::ui_region::RegionDrag>,
    /// A committed region waiting for the pane loop to end.
    ///
    /// The same deferral, and the same reason, as
    /// [`pending_pane_kind`](Self::pending_pane_kind): applying it can grow the
    /// pane count, which changes `pane_rect` for every pane not yet drawn.
    pending_region: Option<crate::ui_region::PendingRegion>,
    /// Whether the cross-section draw is **armed**: the next drag on a map pane
    /// is a section line rather than a pan.
    ///
    /// # Why armed-modal and not a modifier-drag
    ///
    /// A shift-drag is the obvious desktop spelling and it has no touch
    /// equivalent at all. This binary ships to phones, from one wasm build that
    /// also serves desktop browsers, so a gesture only a keyboard can express is
    /// a feature only half the users have.
    ///
    /// A mode has its own failure — the user forgets they are in it — and the
    /// answers to that are both here: the arming control is a **checkbox**, so
    /// the state is visible and turning it off is discoverable in the place it
    /// was turned on; and [`Self::dismiss_top_layer`] cancels it, so Escape and
    /// Android's back button both mean what they mean everywhere else.
    ///
    /// **Never on at the same time as [`region_arm`](Self::region_arm).** Both
    /// are armed modal drags on a map pane, and one drag cannot be two gestures —
    /// see [`Self::set_section_draw_armed`].
    section_draw_armed: bool,
    /// The in-flight draw: where it started, on which pane, and where the
    /// pointer is now.
    section_anchor: Option<SectionAnchor>,
    /// A finished line and the map pane it was drawn on, applied **after** the
    /// pane loop.
    ///
    /// Deferred for the reason [`pending_pane_kind`](Self::pending_pane_kind) is,
    /// and one reason more that is specific to this writer. Applying a line can
    /// *grow the pane count*, and `PaneLayout::pane_rect` is a function of it —
    /// so a mid-loop growth silently moves the rects of every pane the loop has
    /// not reached yet, away from the ones `detect_active_pane_click`
    /// hit-tested at the top of this same frame. The panes drawn after the growth
    /// would be drawn in the right place and clicked in the wrong one, for one
    /// frame, with nothing to say so.
    pending_section_line: Option<(PaneId, crate::pane::SectionLine)>,
    viewport_sync: bool,
    sync_layers: bool,
    // --- Radar loop settings ---
    /// How far back (in seconds) to fetch historical scans for the loop.
    pub loop_lookback_secs: u64,
    /// Animation speed in frames per second.
    pub loop_speed_fps: f32,
    /// Whether the slide-out layers drawer is open. Only consulted when the
    /// layout has no persistent sidebar.
    drawer_open: bool,
    // Safe area insets in logical pixels (top, bottom, left, right)
    // Used on Android to avoid drawing under system bars.
    safe_area_insets: (f32, f32, f32, f32),
    /// Whether this platform can quit at all. Pushed in by the frontend from
    /// the bridge, which this crate cannot see. `false` hides the menu's Exit.
    supports_exit: bool,
    /// Remembers whether a mouse or a finger is driving, across frames.
    modality: ModalityLatch,
    /// This frame's resolved layout. Written once at the top of [`Gui::ui`] and
    /// read by everything below it; never recomputed further down.
    layout: LayoutCtx,
    /// Pointer/gesture resolution for the map, gated on the modality.
    interaction: InteractionState,
    /// User unit and timezone preferences.
    pub preferences: UserPreferences,
    /// Whether the settings panel is open.
    pub show_settings: bool,
    /// GPS configuration (port, baud, heading source).
    pub gps_config: rustdar_gps::GpsConfig,
    /// Storm motion the user typed in, overriding the RPG's SCIT average on
    /// every storm-relative velocity tilt — all four are derived, so all four
    /// take it. `None` means "use the vector the `N0S` product carries", which
    /// is the default and is what AWIPS calls the average storm motion.
    pub storm_motion_override: StormMotionOverride,
    /// Whatever can actually draw a 3D pane, or `None` on a machine or a frame
    /// where nothing can.
    ///
    /// `None` is the state **every headless test sees**, and the state after
    /// every suspend and surface loss (`clear_graphics_state` drops it), so the
    /// empty path is the ordinary path rather than the exceptional one.
    ///
    /// Not a constructor argument: the painter owns GPU handles, and those
    /// arrive with the renderer several frames after the `Gui` exists — on the
    /// web, asynchronously. A `Gui` that could not be built until a device
    /// existed would be a `Gui` no test could build at all.
    volume_painter: Option<std::sync::Arc<dyn crate::volume_view::VolumePainter>>,
}

/// A storm motion vector the user may substitute for the RPG's.
///
/// The two numbers persist while the override is switched off so that toggling
/// it does not lose what was typed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StormMotionOverride {
    pub enabled: bool,
    /// Knots.
    pub speed_kt: f32,
    /// Degrees, meteorological convention — the direction the storm is coming
    /// *from*, matching halfword 52 of the RPG's own product.
    pub direction_deg: f32,
}

impl Default for StormMotionOverride {
    fn default() -> Self {
        Self {
            enabled: false,
            speed_kt: 30.0,
            direction_deg: 240.0,
        }
    }
}

impl StormMotionOverride {
    /// The vector to apply, or `None` to use the one the `N0S` product carries.
    ///
    /// Rejects non-finite values rather than passing them on. `DragValue`
    /// parses `"nan"` and `"inf"`, and `f32::clamp` propagates NaN, so a typed
    /// `nan` reaches the renderer as a whole field of NaN — and, because
    /// `NaN != NaN`, makes the change detector in `set_storm_motion_override`
    /// fire on every frame, re-rendering every storm-relative pane forever.
    pub fn sample(&self) -> Option<rustdar_radar::srm::StormMotionSample> {
        if !self.enabled {
            return None;
        }
        // The constructor rejects non-finite values too; this is the boundary,
        // that is the invariant.
        rustdar_radar::srm::StormMotionSample::user_override(self.speed_kt, self.direction_deg)
    }
}

impl Default for Gui {
    fn default() -> Self {
        Self::new()
    }
}

/// The order the layers panel renders each handler's controls in.
const OVERLAY_CONTROL_ORDER: &[OverlayKind] = &[
    OverlayKind::Radar,
    OverlayKind::ModelData,
    OverlayKind::SpcOutlook,
    OverlayKind::SpcDiscussions,
    OverlayKind::NwsAlerts,
    OverlayKind::StormReports,
    OverlayKind::Lightning,
    OverlayKind::Metar,
    OverlayKind::CityLabels,
    OverlayKind::RadarSites,
    OverlayKind::UserLocation,
    OverlayKind::ColorScale,
];

/// The label the open list puts against `value`, or the raw value for one the
/// handler did not offer.
///
/// The single source of the text for a [`ControlItem::Dropdown`]: both the
/// collapsed box and the list read it, which is the whole point of it existing.
fn dropdown_option_label<'a>(options: &'a [(String, String)], value: &'a str) -> &'a str {
    options
        .iter()
        .find(|(v, _)| v == value)
        .map_or(value, |(_, display)| display.as_str())
}

/// One dropdown a control tree actually drew: the text the *collapsed* box
/// showed, and where it landed so a test can open it for real.
///
/// Reported by the renderer, like [`ui_menu::DrawnMenuLeaf`], rather than
/// rebuilt by a test from the [`ControlItem`] — a test that reformatted the
/// model itself would agree with a renderer that had stopped doing so.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DrawnDropdown {
    pub id: &'static str,
    pub label: String,
    pub selected_text: String,
    pub rect: egui::Rect,
}

/// What one pass over a control tree drew. A no-op outside tests, like
/// [`ui_menu::MenuFrame`].
#[derive(Default)]
pub(crate) struct ControlProbe {
    #[cfg(test)]
    pub drawn: Vec<DrawnDropdown>,
}

impl ControlProbe {
    #[inline]
    fn record_dropdown(
        &mut self,
        _id: &'static str,
        _label: &str,
        _selected_text: &str,
        _rect: egui::Rect,
    ) {
        #[cfg(test)]
        self.drawn.push(DrawnDropdown {
            id: _id,
            label: _label.to_owned(),
            selected_text: _selected_text.to_owned(),
            rect: _rect,
        });
    }
}

/// Render a single declarative [`ControlItem`] into the UI, collecting any
/// resulting [`ControlUpdate`]s into `updates`.
fn render_control_item(
    ui: &mut egui::Ui,
    kind: OverlayKind,
    item: &ControlItem,
    updates: &mut Vec<(OverlayKind, ControlUpdate)>,
    probe: &mut ControlProbe,
) {
    match item {
        ControlItem::Toggle { id, label, enabled } => {
            let mut value = *enabled;
            if ui.checkbox(&mut value, label.as_str()).changed() {
                updates.push((
                    kind,
                    ControlUpdate {
                        id,
                        value: ControlValue::Bool(value),
                    },
                ));
            }
        }
        ControlItem::Heading { text } => {
            ui.label(text.as_str());
        }
        ControlItem::InfoText { text } => {
            ui.label(egui::RichText::new(text.as_str()).small().weak());
        }
        ControlItem::ButtonRow { buttons } => {
            let any_highlighted = buttons.iter().any(|b| b.highlight);
            ui.horizontal_wrapped(|ui| {
                for btn in buttons {
                    let clicked = if any_highlighted {
                        ui.add_enabled(
                            btn.enabled,
                            egui::Button::new(btn.label.as_str()).selected(btn.highlight),
                        )
                        .clicked()
                    } else {
                        ui.add_enabled(btn.enabled, egui::Button::new(btn.label.as_str()))
                            .clicked()
                    };
                    if clicked {
                        updates.push((
                            kind,
                            ControlUpdate {
                                id: btn.id,
                                value: ControlValue::Action,
                            },
                        ));
                    }
                }
            });
        }
        ControlItem::Separator => {
            ui.separator();
        }
        ControlItem::Dropdown {
            id,
            label,
            options,
            selected,
        } => {
            let mut sel = selected.clone();
            let original = sel.clone();
            ui.horizontal(|ui| {
                ui.label(label.as_str());
                // One formatter for both halves. `selected_text` used to be the
                // raw option *value*, so the collapsed box read `sbcin` and
                // `both` while the list it opened said "Surface-Based CIN" and
                // "Both".
                let shown = dropdown_option_label(options, &sel).to_owned();
                let combo = egui::ComboBox::from_id_salt(format!("{kind:?}_{id}"))
                    .selected_text(shown.as_str())
                    .show_ui(ui, |ui| {
                        for (value, display) in options {
                            ui.selectable_value(&mut sel, value.clone(), display.as_str());
                        }
                    });
                probe.record_dropdown(id, label, &shown, combo.response.rect);
            });
            if sel != original {
                updates.push((
                    kind,
                    ControlUpdate {
                        id,
                        value: ControlValue::String(sel),
                    },
                ));
            }
        }
        ControlItem::Slider {
            id,
            label,
            min,
            max,
            value,
            logarithmic,
            ..
        } => {
            let mut val = *value;
            let original = val;
            ui.horizontal(|ui| {
                ui.label(label.as_str());
                let mut slider = egui::Slider::new(&mut val, *min..=*max);
                if *logarithmic {
                    slider = slider.logarithmic(true);
                }
                ui.add(slider);
            });
            if (val - original).abs() > f64::EPSILON {
                updates.push((
                    kind,
                    ControlUpdate {
                        id,
                        value: ControlValue::Float(val),
                    },
                ));
            }
        }
        ControlItem::Section {
            label,
            collapsible,
            expanded,
            items,
        } => {
            if *collapsible {
                egui::CollapsingHeader::new(label.as_str())
                    .default_open(*expanded)
                    .show(ui, |ui| {
                        for child in items {
                            render_control_item(ui, kind, child, updates, probe);
                        }
                    });
            } else {
                ui.group(|ui| {
                    ui.label(egui::RichText::new(label.as_str()).strong());
                    for child in items {
                        render_control_item(ui, kind, child, updates, probe);
                    }
                });
            }
        }
    }
}

impl Gui {
    pub fn new() -> Self {
        let radar_config = RadarConfig::default();
        let date_string = radar_config.timestamp.format("%Y-%m-%d").to_string();
        let time_string = radar_config.timestamp.format("%H:%M:%S").to_string();

        let mut gui = Self {
            radar: RadarState {
                config: radar_config,
                fetching: false,
                error_message: None,
            },
            live_chunks: true,
            chunk_notifications: true,
            notifier_endpoint: crate::DEFAULT_NOTIFIER_ENDPOINT.to_string(),
            chunk_status: ChunkFeedStatus::default(),
            archive_volumes: HashMap::new(),
            auto_poll: AutoPollState {
                last_fetch_time: None,
                enabled: true,
                initial_fetch_done: false,
                interval_secs: 60,
            },
            time_dialog: TimeDialogState {
                date_string,
                time_string,
                show: false,
            },
            initial_zoom_set: false,
            map_tiles: MapTileState::default(),
            user_fix: None,
            user_heading: None,
            overlays: OverlayRegistry::default(),
            panes: vec![PaneState::new()],
            active_pane: 0,
            pane_layout: PaneLayout::default(),
            color_scale_orientation: ColorScaleOrientation::default(),
            #[cfg(test)]
            last_map_panel_rect: egui::Rect::ZERO,
            #[cfg(test)]
            widget_id_probes: Vec::new(),
            #[cfg(test)]
            last_excluded_rects: Vec::new(),
            #[cfg(test)]
            last_menu_leaves: Vec::new(),
            #[cfg(test)]
            last_pane_pointers: Vec::new(),
            #[cfg(test)]
            last_pane_content: Vec::new(),
            #[cfg(test)]
            last_volume_arms: Vec::new(),
            #[cfg(test)]
            last_pane_options: Vec::new(),
            #[cfg(test)]
            last_map_excluded_rects: Vec::new(),
            #[cfg(test)]
            last_status_bar: StatusBarProbe::default(),
            #[cfg(test)]
            last_dropdowns: Vec::new(),
            pending_pane_kind: None,
            region_arm: false,
            region_drag: None,
            pending_region: None,
            section_draw_armed: false,
            section_anchor: None,
            pending_section_line: None,
            viewport_sync: true,
            sync_layers: true,
            loop_lookback_secs: 3600, // default 1 hour
            loop_speed_fps: 5.0,      // default 5 fps
            drawer_open: false,
            safe_area_insets: (0.0, 0.0, 0.0, 0.0),
            supports_exit: true,
            modality: ModalityLatch::default(),
            layout: LayoutCtx::default(),
            interaction: InteractionState::default(),
            preferences: UserPreferences::default(),
            show_settings: false,
            gps_config: rustdar_gps::GpsConfig::default(),
            storm_motion_override: StormMotionOverride::default(),
            volume_painter: None,
        };
        gui.initialize_pane_enabled();
        gui
    }

    /// Create the UI using egui.
    pub fn ui(&mut self, ctx: &egui::Context) -> Vec<GuiAction> {
        let mut actions = Vec::new();

        self.check_auto_polls(&mut actions);

        // Resolve the frame's layout exactly once, before anything draws. Every
        // responsive decision below reads `self.layout`; nothing recomputes a
        // width or a modality of its own.
        self.layout = LayoutCtx::resolve(ctx, &mut self.modality, self.safe_area_insets);
        #[cfg(test)]
        {
            self.widget_id_probes.clear();
            self.last_menu_leaves.clear();
            self.last_pane_pointers.clear();
            // Cleared beside the pointer probes, and for the same reason: both
            // are per-pane records of one frame's pane loop, so a leftover entry
            // would report an arm that did not run this frame.
            self.last_pane_content.clear();
            // Same reason as the line above: a per-frame record of the pane
            // loop, so a leftover entry would report a 3D arm that did not run.
            self.last_volume_arms.clear();
            // Cleared like the rest: the picker only draws when the layers
            // panel is on screen, so a stale value would report buttons that
            // are not there — a compact layout with the drawer shut offers
            // nothing at all.
            self.last_pane_options.clear();
            // Same reason: the handler dropdowns only exist while the panel is
            // on screen.
            self.last_dropdowns.clear();
        }

        // Create a root Ui to host the panels. Since egui 0.35 the Context-taking
        // `Panel::show` is gone and panels are Ui-scoped only, so this root Ui is
        // the only way in.
        //
        // The root rect is the *content* rect, so every `Panel` nested inside it
        // is inset from the system bars and the notch for free. That is what
        // replaced the hand-rolled `add_space(top_inset)` calls the mobile UI
        // used to carry at each panel's top edge.
        let mut root_ui = egui::Ui::new(
            ctx.clone(),
            egui::Id::new("rustdar_root"),
            egui::UiBuilder::new()
                .layer_id(egui::LayerId::background())
                .max_rect(self.layout.content_rect),
        );

        // Chrome first: panels claim space in call order, and what is left is
        // the map's. See `ui_chrome.rs`.
        let chrome = self.render_chrome(&mut root_ui);
        actions.extend(chrome.actions);
        #[cfg(test)]
        {
            self.last_excluded_rects = chrome.excluded_rects.clone();
        }

        if let Some(action) = self.render_time_dialog(ctx) {
            actions.push(action);
        }

        actions.extend(self.render_panes(&mut root_ui, &chrome.excluded_rects));

        // After the pane loop, and therefore after every `mem::take` window in
        // the frame has closed. See the `pending_pane_kind` field for why
        // converting a pane cannot be a direct write from the dispatcher that
        // asked for it.
        self.apply_pending_pane_kind(&mut actions);
        // Same window, and one thing more: this can grow `pane_count`, which
        // moves `pane_rect` for every pane. Inside the loop that would leave the
        // panes drawn after it hit-tested against rects they are no longer in.
        self.apply_pending_section_line();
        // After the kind conversion, so a region that lands on a pane the same
        // frame converted it finds a 3D pane rather than the map it used to be.
        //
        // # Two appliers, and why their order is not a design decision
        //
        // Both of these can grow the layout, and running two growths in one frame
        // would be a case neither was written for: the second one's target rule
        // would run against a layout the first had already changed, and in a full
        // layout each rule's last resort is *the same pane* — so the second would
        // convert the pane the first had just filled, and the user would see one
        // of two completed gestures produce nothing.
        //
        // It cannot happen, and the reason is upstream of here: the two modes are
        // mutually exclusive (see [`Self::set_section_draw_armed`]), only an armed
        // mode can record a pending, and each pending is recorded and consumed
        // inside a single frame. So at most one of these two lines does anything
        // on any frame. Pinned by
        // `two_appliers_never_both_have_something_to_apply`, which drives the two
        // toggles rather than writing the flags, because the invariant belongs to
        // the arming rule rather than to this call order.
        self.apply_pending_region();

        // Floating windows last, so they layer above the chrome and the map.
        self.render_overlay_popup(ctx);
        self.render_settings(ctx, &mut actions);

        // Ensure the handler state reflects the active pane's config at frame
        // end, so any deferred actions (FetchOverlay, etc.) processed after the
        // frame use the correct per-pane state.
        let active = &self.panes[self.active_pane];
        if !active.overlay_configs.is_empty() {
            let configs = active.overlay_configs.clone();
            self.overlays.load_pane_configs(&configs);
        }

        actions
    }

    /// The config a radar fetch on the active pane's behalf must use: the
    /// shared `radar.config` with the active pane's site substituted in.
    ///
    /// `config.site` is a *global* last-switched site — the frontend's
    /// `SwitchRadarSite` writes it even when layer sync is off — so with
    /// per-pane sites it can name a site the active pane is not viewing.
    /// Both Refresh entry points (status bar and menu) and the initial
    /// auto-fetch route through here rather than cloning the config verbatim,
    /// so they cannot drift apart.
    pub(super) fn active_pane_fetch_config(&self) -> RadarConfig {
        let mut config = self.radar.config.clone();
        config.site = self.active_pane().site.clone();
        config
    }

    /// Check timers and emit fetch actions for auto-polling radar scans,
    /// NWS alerts, and SPC discussions.
    fn check_auto_polls(&mut self, actions: &mut Vec<GuiAction>) {
        // Auto-fetch on first load
        if !self.auto_poll.initial_fetch_done && !self.radar.fetching {
            self.radar.fetching = true;
            self.auto_poll.initial_fetch_done = true;
            self.auto_poll.record_fetch();
            actions.push(GuiAction::FetchRadarScan(self.active_pane_fetch_config()));
        }

        // Poll for new scans at the current poll interval (only when any pane is viewing live)
        if self.is_any_pane_live() && self.auto_poll.should_poll() && !self.radar.fetching {
            // Check for new files without downloading — emit one check per unique live site
            let now = chrono::Local::now().naive_local();
            let current_scan_time = now
                .with_second(0)
                .and_then(|t| t.with_nanosecond(0))
                .unwrap_or(now);

            let mut seen_sites: Vec<&str> = Vec::with_capacity(self.pane_layout.pane_count);
            for pane in self.panes.iter().take(self.pane_layout.pane_count) {
                if pane.viewing_live && !seen_sites.contains(&pane.site.as_str()) {
                    seen_sites.push(&pane.site);
                    let config = RadarConfig {
                        site: pane.site.clone(),
                        timestamp: current_scan_time,
                    };
                    actions.push(GuiAction::CheckForNewScans(config));
                }
            }

            // Reset timer to avoid spamming checks
            self.auto_poll.record_fetch();
        }

        // Auto-refresh overlay data when layers are enabled and refresh interval elapsed
        for &kind in OverlayKind::all() {
            if let Some(interval) = self.overlays.auto_poll_interval(kind)
                && let Some(pane_idx) = self.first_pane_with_overlay_enabled(kind)
                && !self.overlays.is_fetching(kind)
                && self
                    .overlays
                    .fetch_time(kind)
                    .is_none_or(|t| t.elapsed().as_secs() >= interval)
            {
                actions.push(GuiAction::FetchOverlay { kind, pane_idx });
            }
        }
    }

    /// Update the scan info for all panes viewing the given site.
    pub fn set_scan_info_for_site(&mut self, site: &str, info: ScanInfo) {
        for pane in &mut self.panes {
            if pane.site == site {
                pane.scan_info = Some(info.clone());
            }
        }
        self.radar.fetching = false;
        self.auto_poll.on_success();
        self.claim_initial_zoom();
    }

    /// Zoom to the radar on the first scan of a session and never again, so a
    /// later load does not throw away the user's navigation.
    ///
    /// Factored out of [`Self::set_scan_info_for_site`] because
    /// [`Self::apply_chunk_scan_info`] shares this one behaviour and none of the
    /// others — and with chunks feeding live mode, the first data of a session
    /// can arrive through either.
    fn claim_initial_zoom(&mut self) {
        if !self.initial_zoom_set {
            for pane in &mut self.panes {
                let _ = pane.map_memory.set_zoom(DEFAULT_INITIAL_ZOOM);
            }
            self.initial_zoom_set = true;
        }
    }

    /// Apply scan info for a volume still being assembled from the real-time
    /// chunk feed.
    ///
    /// Two differences from [`Self::set_scan_info_for_site`], both deliberate.
    ///
    /// **It does not take the spinner down or reset the archive backoff.** Those
    /// belong to a fetch someone is waiting on; this happens on its own every
    /// few seconds. Clearing `fetching` here would cancel the spinner of a manual
    /// Refresh still in flight and unblock the auto-poll queued behind it, and
    /// `auto_poll.on_success()` would undo exactly the retreat the archive
    /// fallback depends on.
    ///
    /// **It merges the product and elevation lists rather than replacing them.**
    /// A partial volume knows only the cuts that have completed, so replacing
    /// would shrink the tilt picker every few seconds and let it regrow — and
    /// `PaneState::get_rendering_params` snaps to the nearest *listed* angle, so
    /// every pane would walk up the VCP once per volume. It would also wipe the
    /// Level III products and elevations that `poll_level3_results` accumulates
    /// into `ScanInfo` in place, freezing every L3 pane until the volume closed.
    /// The union keeps both and still gains a tilt the moment one first appears.
    ///
    /// At volume completion the caller uses `set_scan_info_for_site` with a
    /// plain `from_scan` instead, so the steady state after every volume is
    /// exactly what the archive path produces — which is what makes a fallback
    /// invisible.
    pub fn apply_chunk_scan_info(&mut self, site: &str, fresh: ScanInfo) {
        for pane in &mut self.panes {
            if pane.site != site {
                continue;
            }
            let merged = match pane.scan_info.take() {
                None => fresh.clone(),
                Some(mut existing) => {
                    existing.timestamp = fresh.timestamp;
                    existing.vcp_number = fresh.vcp_number;
                    existing.status = fresh.status.clone();
                    for product in &fresh.available_products {
                        if !existing.available_products.contains(product) {
                            existing.available_products.push(*product);
                        }
                    }
                    existing.available_products.sort_by_key(|p| p.sort_order());
                    for (product, angles) in &fresh.product_elevations {
                        let known = existing.product_elevations.entry(*product).or_default();
                        for angle in angles {
                            if !known.iter().any(|k| (k - angle).abs() < 0.05) {
                                known.push(*angle);
                            }
                        }
                        known.sort_by(|a, b| a.total_cmp(b));
                    }
                    existing
                }
            };
            pane.scan_info = Some(merged);
        }
        self.claim_initial_zoom();
    }

    /// Whether live panes should be fed from the real-time chunk bucket.
    ///
    /// Persisted as `UiConfig::live_chunks`, default on. Turning it off leaves
    /// live mode on the archive path, which is the same code that serves the
    /// time picker and history — so the fallback is never a separate,
    /// less-exercised route.
    pub fn live_chunks_enabled(&self) -> bool {
        self.live_chunks
    }

    /// Set by the settings UI and by the config load.
    pub fn set_live_chunks(&mut self, enabled: bool) {
        self.live_chunks = enabled;
    }

    /// Whether to subscribe to the push-notification service.
    ///
    /// Purely an accelerator: it makes a chunk fetch start the moment the chunk
    /// exists rather than on the next five-second tick. Turning it off, or
    /// failing to reach the service, leaves the polling feed running exactly as
    /// it is — which is why it can default on without making a third-party
    /// deployment load-bearing.
    pub fn chunk_notifications_enabled(&self) -> bool {
        self.chunk_notifications
    }

    pub fn set_chunk_notifications(&mut self, enabled: bool) {
        self.chunk_notifications = enabled;
    }

    /// Where the notifier service lives.
    ///
    /// Settable because it is one person's deployment rather than a NOAA
    /// endpoint: a user behind a network that cannot reach it, or one running
    /// their own, needs to be able to point elsewhere. An empty value falls back
    /// to the default rather than disabling the feature, so a cleared box is not
    /// a silent off switch.
    pub fn notifier_endpoint(&self) -> &str {
        if self.notifier_endpoint.trim().is_empty() {
            crate::DEFAULT_NOTIFIER_ENDPOINT
        } else {
            self.notifier_endpoint.trim()
        }
    }

    pub fn set_notifier_endpoint(&mut self, endpoint: impl Into<String>) {
        self.notifier_endpoint = endpoint.into();
    }

    /// Publish what the real-time feed is doing, so the status bar can say so.
    ///
    /// Pushed in by the App each frame rather than pulled: the feeds live there,
    /// and this crate has no business reaching into them.
    pub fn set_chunk_status(&mut self, status: ChunkFeedStatus) {
        self.chunk_status = status;
    }

    pub fn chunk_status(&self) -> &ChunkFeedStatus {
        &self.chunk_status
    }

    /// Publish the most recent archive volume for each site — the only volumes a
    /// 3D pane is built from.
    ///
    /// Pushed in by the App each frame, the same arrangement as
    /// [`Self::set_chunk_status`] and for the same reason: the decoded volumes
    /// live there, and this crate holds only their names.
    pub fn set_archive_volumes(&mut self, volumes: HashMap<String, NaiveDateTime>) {
        self.archive_volumes = volumes;
    }

    /// When the archive last published a volume for `site`, if this build has it.
    ///
    /// `None` is an ordinary state and the reason a 3D pane says "waiting": it is
    /// what a site looks like before its first archive poll returns, including
    /// while the real-time feed is already drawing a plan view beside it.
    pub fn archive_volume_for(&self, site: &str) -> Option<NaiveDateTime> {
        self.archive_volumes.get(site).copied()
    }

    /// The distinct sites some pane is watching live — the unit the chunk feed
    /// and the archive auto-poll both work in.
    pub fn live_sites(&self) -> Vec<String> {
        let mut sites: Vec<String> = Vec::new();
        for pane in self.panes.iter().take(self.pane_layout.pane_count) {
            if pane.viewing_live && !sites.iter().any(|s| s == &pane.site) {
                sites.push(pane.site.clone());
            }
        }
        sites
    }

    /// Update the scan info for a specific pane.
    pub fn set_scan_info_for_pane(&mut self, pane_idx: usize, info: ScanInfo) {
        if let Some(pane) = self.panes.get_mut(pane_idx) {
            pane.scan_info = Some(info);
        }
    }

    /// Close the topmost thing the user has open, and say whether there was
    /// one.
    ///
    /// What Escape and Android's back both mean: back out of the thing I am
    /// in. Only when this returns `false` is the press a request to leave the
    /// app — which is why the drawer used to cost a whole launch on a phone,
    /// where the drawer *is* the menu at every width the Fold 7 reaches.
    ///
    /// Ordered topmost first — whatever is painted over everything else is
    /// what a press is aimed at — and exactly one layer closes per press.
    ///
    /// Not derived from the order `ui` calls them in, which is time dialog,
    /// then popup, then settings. The popup and the settings window are both
    /// `Order::Foreground`, so egui stacks them by area recency rather than by
    /// call order, and the time dialog sits below both. This order is asserted
    /// rather than computed; see `a_back_press_closes_one_open_layer_at_a_time`.
    ///
    /// Deliberately not reachable from `request_exit`: the window's close
    /// button and the menu's Exit item are unambiguous, and dismissing a dialog
    /// instead of honouring them would strand the user — the Exit item lives
    /// *inside* the drawer.
    pub fn dismiss_top_layer(&mut self) -> bool {
        if !self.overlays.selected_overlays.is_empty() {
            self.overlays.selected_overlays.clear();
            self.overlays.selected_overlay_page = 0;
            return true;
        }
        if self.show_settings {
            self.show_settings = false;
            return true;
        }
        if self.time_dialog.show {
            self.time_dialog.show = false;
            return true;
        }
        if self.drawer_open {
            self.drawer_open = false;
            return true;
        }
        // Last, below every painted layer, because an armed drag is a *mode*
        // rather than something on screen: whatever is drawn over the map is what
        // a press is aimed at, and the drawer in particular is where the mode was
        // armed.
        //
        // Being here at all is what makes an armed drag cancellable by the two
        // gestures that mean "back out" everywhere else — and on Android it is
        // what stops the back button from exiting the app while a mode is on,
        // which is the reading of a back press least likely to be what was meant.
        //
        // **One layer for both modes, not two.** They are mutually exclusive (see
        // `Gui::set_region_arm`), so at most one of these ever fires and giving
        // them separate layers would only invite a reader to wonder which order
        // they are in. A back press cancels whichever armed drag is on, and there
        // is never more than one.
        if self.section_draw_armed {
            self.set_section_draw_armed(false);
            return true;
        }
        if self.region_arm {
            self.set_region_arm(false);
            return true;
        }
        false
    }

    /// Whether a fetch someone is waiting on is in flight.
    ///
    /// Global rather than per-site, and it gates `check_auto_polls` — so any
    /// path that raises it has to lower it on every exit.
    pub fn fetching(&self) -> bool {
        self.radar.fetching
    }

    /// Set fetching status
    pub fn set_fetching(&mut self, fetching: bool) {
        self.radar.fetching = fetching;
    }

    /// Set an error message
    pub fn set_error(&mut self, error: String) {
        self.radar.error_message = Some(error);
        self.radar.fetching = false;
        self.auto_poll.on_error();
    }

    fn render_time_dialog(&mut self, ctx: &Context) -> Option<GuiAction> {
        let mut action = None;

        if self.time_dialog.show {
            egui::Window::new("Set Time")
                .collapsible(false)
                .resizable(false)
                .pivot(egui::Align2::CENTER_CENTER)
                // Centred in the content rect, not the viewport: on a device
                // with a notch or a nav bar those differ, and centring on the
                // viewport puts the dialog partly underneath them.
                .default_pos(self.layout.dialog_center())
                .show(ctx, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.heading("Select Time");
                        ui.add_space(10.0);

                        ui.label("Date:");
                        ui.text_edit_singleline(&mut self.time_dialog.date_string);

                        ui.add_space(5.0);

                        ui.label("Time:");
                        ui.text_edit_singleline(&mut self.time_dialog.time_string);

                        ui.add_space(10.0);

                        if ui.button("Use Current Time").clicked() {
                            self.radar.config.timestamp = chrono::Local::now().naive_local();
                            self.time_dialog.date_string =
                                self.radar.config.timestamp.format("%Y-%m-%d").to_string();
                            self.time_dialog.time_string =
                                self.radar.config.timestamp.format("%H:%M:%S").to_string();
                        }

                        ui.add_space(15.0);

                        ui.horizontal(|ui| {
                            if ui.button("OK").clicked() {
                                // Try to parse the date and time strings
                                let datetime_str = format!(
                                    "{} {}",
                                    self.time_dialog.date_string, self.time_dialog.time_string
                                );
                                if let Ok(timestamp) = chrono::NaiveDateTime::parse_from_str(
                                    &datetime_str,
                                    "%Y-%m-%d %H:%M:%S",
                                ) {
                                    self.radar.config.timestamp = timestamp;
                                    if let Some(pane) = self.panes.get_mut(self.active_pane) {
                                        pane.viewing_live = false;
                                    }
                                    action =
                                        Some(GuiAction::FetchRadarScan(self.radar.config.clone()));
                                }
                                self.time_dialog.show = false;
                            }

                            if ui.button("Cancel").clicked() {
                                // Restore the original strings from the current config
                                self.time_dialog.date_string =
                                    self.radar.config.timestamp.format("%Y-%m-%d").to_string();
                                self.time_dialog.time_string =
                                    self.radar.config.timestamp.format("%H:%M:%S").to_string();
                                self.time_dialog.show = false;
                            }
                        });
                    });
                });
        }

        action
    }

    /// Render pane count buttons and active-pane selector.
    ///
    /// Shared by desktop and mobile layers panels. The caller must pass the
    /// currently-taken `pane` by mutable reference so this method can swap
    /// it back into `self.panes` when the active pane changes.
    fn render_pane_selector(&mut self, ui: &mut egui::Ui, pane: &mut PaneState) {
        let max_panes = self.layout.width.max_panes();
        ui.horizontal(|ui| {
            ui.label("Panes:");
            for count in 1..=max_panes {
                let button =
                    ui.selectable_label(self.pane_layout.pane_count == count, format!("{count}"));
                // The button that was drawn: which count, whether it read as
                // selected, and where it landed so a test can click it. A probe
                // built from `max_panes` instead would be a restatement of the
                // line above and could not see the loop at all.
                #[cfg(test)]
                self.last_pane_options.push(PaneOptionProbe {
                    count,
                    selected: self.pane_layout.pane_count == count,
                    rect: button.rect,
                });
                if button.clicked() && self.pane_layout.pane_count != count {
                    // The pane goes back into the vector first: `set_pane_count`
                    // seeds the new panes from the *active* one, and a
                    // `mem::take`n slot would seed them from a default.
                    self.panes[self.active_pane] = std::mem::take(pane);
                    // The answer is ignored here and only here: the picker's
                    // counts come from the width class's own list, so the clamp
                    // in `PaneLayout::for_count` can never bite.
                    let _ = self.set_pane_count(count);
                    *pane = std::mem::take(&mut self.panes[self.active_pane]);
                }
            }
        });
        if self.pane_layout.pane_count > 1 {
            ui.horizontal(|ui| {
                ui.label("Pane:");
                for i in 0..self.pane_layout.pane_count {
                    if ui
                        .selectable_label(self.active_pane == i, format!("{}", i + 1))
                        .clicked()
                        && self.active_pane != i
                    {
                        self.panes[self.active_pane] = std::mem::take(pane);
                        self.active_pane = i;
                        *pane = std::mem::take(&mut self.panes[i]);
                    }
                }
            });
        }
        ui.separator();
    }

    /// Render the layer controls shared by desktop and mobile panels.
    ///
    /// Covers: radar product/elevation, radar loop, SPC outlooks, SPC discussions,
    /// NWS alerts, city labels, radar sites, and viewport sync toggles.
    ///
    /// # The kind-specific block goes in one child scope
    ///
    /// Which controls make sense depends on what the pane *is*, so the block that
    /// draws them sits inside a single `scope_builder` — and that, rather than the
    /// id form, is the load-bearing part. `Ui::new_child` folds the parent's
    /// `next_auto_id_salt` into every child's registered id, so drawn straight
    /// onto this `Ui` the two branches would advance that counter by different
    /// amounts: a map pane draws a loop transport and the whole overlay tree, a
    /// volume pane draws neither. **Everything after them would then come back
    /// under new ids the moment a pane was converted**, including the drawer menu
    /// below, which at every width without a menu bar is the only route to Exit
    /// and Settings. One child scope advances the counter by exactly one whichever
    /// branch ran. Pinned by
    /// `converting_the_active_pane_does_not_re_key_the_drawer_menu`.
    ///
    /// The scope's id is [`egui::UiBuilder::id`] rather than `id_salt`, and what
    /// that buys is independence from *what precedes it*. `id` is the one form
    /// taking `IdSource::Explicit`, which makes the child's `unique_id` equal its
    /// `stable_id`; `id_salt` leaves `unique_id` folded together with the parent's
    /// `next_auto_id_salt`, and hence seeds this scope's own auto-id counter from
    /// it. `render_pane_selector` above draws a button per offered pane count plus
    /// a second row once the layout is split, so the counter at this position moves
    /// whenever the pane count does — and only the explicit form keeps this scope's
    /// children out of that. It is the same reason `ui_chrome.rs`'s `status_error`
    /// note records for choosing it there.
    ///
    /// It is **defence rather than a fix for a live difference**, worth stating so
    /// nobody reads more into it: mutating `id` into `id_salt` fails no test. Note
    /// that this is *not* because the two produce the same id — they do not, since
    /// `Id::with` wraps its argument in a second `IdSalt::new`, so an `id_salt`
    /// scope's `stable_id` hashes the salt twice and lands somewhere else entirely.
    /// It is because nothing in here is keyed to a *particular* value: the two
    /// combo boxes and the time-step picker key off `stable_id`, which both forms
    /// keep stable across a conversion, and stability is all they need.
    fn render_layer_controls(
        &mut self,
        ui: &mut egui::Ui,
        pane: &mut PaneState,
        combo_width: f32,
        id_prefix: &str,
        actions: &mut Vec<GuiAction>,
    ) {
        let kind_scope = egui::UiBuilder::new().id(ui.id().with("pane_kind_controls"));
        ui.scope_builder(kind_scope, |ui| {
            // On `pane`, the value the caller took out of the vector — never
            // `self.panes[..]`, which for the whole of this pass holds a default
            // `PaneState` and therefore reads as a *map* pane whatever the real
            // one is. This is the same hazard `menu_model` has, with the same fix.
            match pane.kind() {
                crate::pane::PaneKind::Map => {
                    self.render_radar_controls(ui, pane, combo_width, id_prefix);

                    // --- Time navigation (forward/back/live) ---
                    self.render_time_navigation(ui, pane, id_prefix, actions);

                    // --- Radar loop controls ---
                    self.render_loop_controls(ui, pane, actions);

                    ui.add_space(6.0);
                    ui.separator();

                    // --- Handler-backed overlay controls (generic) ---
                    self.render_overlay_controls(ui, pane, actions);
                }
                // A section and a volume pane get the product picker and time
                // navigation, and nothing else.
                //
                // No tilt picker: both read the whole ladder, so there is nothing
                // to choose — see `render_radar_controls`. No loop transport: a
                // loop frame *is* a rendered plan-view tilt, and both
                // `loop_sync_targets` and `App::dispatch_loop_renders` now decline
                // to feed a pane like this, so the control would enable a loop
                // that never fills. No overlay tree: every entry in it is a layer
                // drawn over map tiles, geo-positioned against a projector this
                // pane does not have.
                crate::pane::PaneKind::CrossSection => {
                    self.render_radar_controls(ui, pane, combo_width, id_prefix);
                    self.render_time_navigation(ui, pane, id_prefix, actions);
                }
                // The same two, plus the knobs that only mean something for a
                // box being looked at from outside.
                crate::pane::PaneKind::Volume => {
                    self.render_radar_controls(ui, pane, combo_width, id_prefix);
                    self.render_time_navigation(ui, pane, id_prefix, actions);
                    map::render_volume_controls(ui, pane);
                }
            }
        });

        ui.add_space(6.0);
        ui.separator();

        // --- Viewport sync ---
        //
        // Outside the kind scope: both settings are properties of the *layout*
        // rather than of the active pane, and they stay meaningful with a non-map
        // pane on screen — `sync_layers` still converges site, product and time
        // across every pane, and `sync_viewports` still holds the map panes
        // together while leaving this one alone.
        if self.pane_layout.pane_count > 1 {
            ui.checkbox(&mut self.viewport_sync, "\u{1f517}  Sync Viewports");
            ui.checkbox(&mut self.sync_layers, "\u{1f517}  Sync Layers");
            ui.separator();
        }
    }

    /// Render the radar product picker, and the tilt picker where a tilt means
    /// anything.
    fn render_radar_controls(
        &mut self,
        ui: &mut egui::Ui,
        pane: &mut PaneState,
        combo_width: f32,
        id_prefix: &str,
    ) {
        // The Radar overlay toggle governs whether the *map* draws the radar
        // image over its tiles, which is not a question a pane with no map has.
        // Gated on it, a section or a volume pane converted while the toggle
        // happened to be off would have no way to choose a product at all — a
        // control that is simply absent, for a reason nothing on screen explains.
        if pane.is_map() && !pane.is_overlay_enabled(OverlayKind::Radar) {
            return;
        }
        // A whole-volume pane has no tilt to pick: it reads the entire ladder,
        // which is what `PaneKind::consumes_whole_volume` means, so every entry in
        // the combo would select the same picture. `selected_elevation` stays on
        // the pane, inert, so converting back to a map restores the tilt it had.
        let offer_tilt = !pane.kind().consumes_whole_volume();
        // Reported the way `time_step_sel` is, and for the same reason: a test
        // rebuilding these ids from the same format strings could agree with a
        // panel that drew neither control. *Which* of the two appear is how a test
        // sees the product picker survive a conversion while the tilt picker does
        // not.
        #[cfg(test)]
        let probes = &mut self.widget_id_probes;
        {
            ui.indent(format!("{id_prefix}radar_controls"), |ui| {
                if let Some(scan_info) = &pane.scan_info {
                    let prev_product = pane.selected_product;
                    let product_combo =
                        egui::ComboBox::from_id_salt(format!("{id_prefix}product_sel"))
                            .selected_text(pane.selected_product.name())
                            .width(combo_width)
                            .show_ui(ui, |ui| {
                                for product in &scan_info.available_products {
                                    ui.selectable_value(
                                        &mut pane.selected_product,
                                        *product,
                                        product.name(),
                                    );
                                }
                            });
                    #[cfg(test)]
                    probes.push(("product_sel", product_combo.response.id));
                    #[cfg(not(test))]
                    let _ = product_combo;
                    if prev_product != pane.selected_product {
                        pane.selected_elevation = 0.0;
                    }

                    // The tilt picker is drawn for every listed product, including
                    // one whose angles have not arrived yet.
                    //
                    // Skipping it while the list was empty made the control vanish
                    // and the panel reflow around it — for a Level III product on
                    // first selection, and again on every archive poll, which
                    // rebuilds `ScanInfo` from the volume alone and so drops the
                    // angles `poll_level3_results` had filled in. Present but
                    // unpopulated is the honest state: the product is selected, the
                    // selection stands (`get_rendering_params` leaves it unsnapped),
                    // and there is nothing to choose between yet.
                    if let Some(elevations) = offer_tilt
                        .then(|| scan_info.product_elevations.get(&pane.selected_product))
                        .flatten()
                    {
                        let selected_angle = elevations
                            .iter()
                            .min_by(|a, b| {
                                ((**a - pane.selected_elevation).abs())
                                    .total_cmp(&((**b - pane.selected_elevation).abs()))
                            })
                            .copied()
                            .unwrap_or(pane.selected_elevation);

                        let combo = egui::ComboBox::from_id_salt(format!("{id_prefix}elev_sel"))
                            .selected_text(format!("{:.1}\u{b0}", selected_angle))
                            .width(combo_width);
                        let elev_combo = if elevations.is_empty() {
                            // Nothing to pick from, so the control is inert rather
                            // than an empty menu that opens onto nothing.
                            let scope = ui.add_enabled_ui(false, |ui| combo.show_ui(ui, |_| {}));
                            let id = scope.inner.response.id;
                            scope
                                .response
                                .on_hover_text("Waiting for this product's data");
                            id
                        } else {
                            combo
                                .show_ui(ui, |ui| {
                                    for angle in elevations.iter() {
                                        ui.selectable_value(
                                            &mut pane.selected_elevation,
                                            *angle,
                                            format!("{:.1}\u{b0}", angle),
                                        );
                                    }
                                })
                                .response
                                .id
                        };
                        // Both branches, so the probe reports the control existing
                        // rather than the elevation list happening to be populated.
                        #[cfg(test)]
                        probes.push(("elev_sel", elev_combo));
                        #[cfg(not(test))]
                        let _ = elev_combo;
                    }
                } else {
                    ui.label("No scan loaded");
                }
            });
        }
    }

    /// Available time step options: (seconds, label). 0 = "one scan".
    const TIME_STEP_OPTIONS: &[(i64, &str)] = &[
        (0, "1 scan"),
        (600, "10 min"),
        (1800, "30 min"),
        (3600, "1 hr"),
        (7200, "2 hr"),
        (21600, "6 hr"),
        (43200, "12 hr"),
    ];

    /// Render forward / live / back navigation buttons with a time step dropdown.
    fn render_time_navigation(
        &mut self,
        ui: &mut egui::Ui,
        pane: &mut PaneState,
        id_prefix: &str,
        actions: &mut Vec<GuiAction>,
    ) {
        #[cfg(test)]
        let probes = &mut self.widget_id_probes;
        ui.add_space(4.0);

        // Time step dropdown
        let step_label = Self::TIME_STEP_OPTIONS
            .iter()
            .find(|(s, _)| *s == pane.time_step_secs)
            .map(|(_, l)| *l)
            .unwrap_or("10 min");

        ui.horizontal(|ui| {
            ui.label("Step:");
            // Prefixed like the rest. It used to be bare, which was only safe
            // because the desktop and mobile panels could never both exist.
            let combo = egui::ComboBox::from_id_salt(format!("{id_prefix}time_step_sel"))
                .selected_text(step_label)
                .show_ui(ui, |ui| {
                    for &(secs, label) in Self::TIME_STEP_OPTIONS {
                        ui.selectable_value(&mut pane.time_step_secs, secs, label);
                    }
                });
            // Report the id the combo box really resolved, rather than building
            // a second one from the same format string: the two could disagree
            // silently, and a test comparing reconstructions either side of a
            // resize would then prove nothing about the state egui actually
            // keyed on. `layers_scroll` does the same.
            #[cfg(test)]
            probes.push(("time_step_sel", combo.response.id));
            #[cfg(not(test))]
            let _ = combo;
        });

        // Navigation buttons
        let active_pane_idx = self.active_pane;
        ui.horizontal(|ui| {
            // Back button
            if ui.button("\u{25c0} Back").clicked() {
                pane.viewing_live = false;
                if pane.time_step_secs == 0 {
                    actions.push(GuiAction::NavigateOneScan {
                        pane_idx: active_pane_idx,
                        forward: false,
                    });
                } else {
                    actions.push(GuiAction::NavigateTime {
                        pane_idx: active_pane_idx,
                        step_secs: -pane.time_step_secs,
                    });
                }
            }

            // Live button — highlighted when NOT live to indicate "click to return"
            let live_button = if pane.viewing_live {
                egui::Button::new("\u{23fa} Live")
            } else {
                egui::Button::new(egui::RichText::new("\u{23fa} Live").color(egui::Color32::WHITE))
                    .fill(egui::Color32::from_rgb(200, 50, 50))
            };
            if ui.add(live_button).clicked() && !pane.viewing_live {
                actions.push(GuiAction::JumpToLive {
                    pane_idx: active_pane_idx,
                });
            }

            // Forward button — disabled when live
            if ui
                .add_enabled(!pane.viewing_live, egui::Button::new("Forward \u{25b6}"))
                .clicked()
            {
                if pane.time_step_secs == 0 {
                    actions.push(GuiAction::NavigateOneScan {
                        pane_idx: active_pane_idx,
                        forward: true,
                    });
                } else {
                    actions.push(GuiAction::NavigateTime {
                        pane_idx: active_pane_idx,
                        step_secs: pane.time_step_secs,
                    });
                }
            }
        });
    }

    /// Render radar loop controls: enable/disable, lookback slider, speed slider,
    /// transport buttons (play/pause, step, seek), and frame progress.
    fn render_loop_controls(
        &mut self,
        ui: &mut egui::Ui,
        pane: &mut PaneState,
        actions: &mut Vec<GuiAction>,
    ) {
        ui.add_space(4.0);
        let loop_active = pane.loop_state.is_active();

        // Enable/disable toggle
        let mut enabled = loop_active;
        if ui.checkbox(&mut enabled, "\u{1f501}  Radar Loop").changed() {
            if enabled {
                for pane_idx in self.loop_sync_targets() {
                    actions.push(GuiAction::EnableLoop {
                        pane_idx,
                        lookback_secs: self.loop_lookback_secs,
                    });
                }
            } else {
                for pane_idx in self.loop_sync_targets() {
                    actions.push(GuiAction::DisableLoop { pane_idx });
                }
            }
        }

        if loop_active {
            ui.indent("loop_controls", |ui| {
                // Lookback duration slider
                let mut lookback_mins = (self.loop_lookback_secs as f32 / 60.0).round();
                ui.horizontal(|ui| {
                    ui.label("Lookback:");
                    if ui
                        .add(
                            egui::Slider::new(&mut lookback_mins, 5.0..=1440.0)
                                .logarithmic(true)
                                .suffix(" min")
                                .clamping(egui::SliderClamping::Always),
                        )
                        .drag_stopped()
                    {
                        let new_secs = (lookback_mins * 60.0) as u64;
                        if new_secs != self.loop_lookback_secs {
                            self.loop_lookback_secs = new_secs;
                            for pane_idx in self.loop_sync_targets() {
                                actions.push(GuiAction::EnableLoop {
                                    pane_idx,
                                    lookback_secs: new_secs,
                                });
                            }
                        }
                    }
                });

                // Speed slider
                ui.horizontal(|ui| {
                    ui.label("Speed:");
                    ui.add(
                        egui::Slider::new(&mut self.loop_speed_fps, 1.0..=30.0)
                            .suffix(" fps")
                            .clamping(egui::SliderClamping::Always),
                    );
                });

                {
                    let ls = &pane.loop_state;
                    // Frame status
                    let rendered = ls.frames.iter().filter(|f| f.texture.is_some()).count();
                    let total = ls.frames.len();
                    let rendering = total > 0 && !ls.is_render_ready();
                    if ls.is_fetching() {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("Loading scan list...");
                        });
                    } else if total == 0 {
                        ui.label("No frames found");
                    } else {
                        // Progress bar when rendering, plain text when done
                        if rendering {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label(format!("Rendering {}/{}...", rendered, total));
                            });
                            ui.add(
                                egui::ProgressBar::new(rendered as f32 / total as f32)
                                    .show_percentage(),
                            );
                        } else {
                            ui.label(format!("{}/{} frames rendered", rendered, total));
                        }

                        // Transport controls
                        ui.horizontal(|ui| {
                            // Step backward
                            if ui
                                .button("\u{23ee}")
                                .on_hover_text("Previous frame")
                                .clicked()
                            {
                                for pane_idx in self.loop_sync_targets() {
                                    actions.push(GuiAction::StepLoopFrame {
                                        pane_idx,
                                        forward: false,
                                    });
                                }
                            }

                            // Play/pause
                            let play_label = if ls.is_playing() {
                                "\u{23f8}"
                            } else {
                                "\u{25b6}"
                            };
                            let play_hover = if ls.is_playing() {
                                "Pause".to_string()
                            } else if rendering {
                                format!("Waiting for renders ({}/{})", rendered, total)
                            } else {
                                "Play".to_string()
                            };
                            let play_btn = egui::Button::new(play_label);
                            let resp = ui
                                .add_enabled(!rendering || ls.is_playing(), play_btn)
                                .on_hover_text(play_hover);
                            if resp.clicked() {
                                for pane_idx in self.loop_sync_targets() {
                                    actions.push(GuiAction::ToggleLoopPlayback { pane_idx });
                                }
                            }

                            // Step forward
                            if ui.button("\u{23ed}").on_hover_text("Next frame").clicked() {
                                for pane_idx in self.loop_sync_targets() {
                                    actions.push(GuiAction::StepLoopFrame {
                                        pane_idx,
                                        forward: true,
                                    });
                                }
                            }
                        });

                        // Frame seek slider
                        let mut frame_idx = ls.current_frame;
                        if ui
                            .add(
                                egui::Slider::new(&mut frame_idx, 0..=(total - 1))
                                    .show_value(false),
                            )
                            .changed()
                        {
                            for pane_idx in self.loop_sync_targets() {
                                actions.push(GuiAction::SeekLoopFrame {
                                    pane_idx,
                                    frame_index: frame_idx,
                                });
                            }
                        }

                        // Current frame timestamp
                        if let Some(frame) = ls.frames.get(ls.current_frame) {
                            ui.label(
                                egui::RichText::new(
                                    self.preferences
                                        .timezone
                                        .format_naive_utc(frame.timestamp, "%H:%M:%S"),
                                )
                                .small(),
                            );
                        }
                    }
                }
            });
        }
    }

    /// Render controls for all handler-backed overlays generically.
    ///
    /// Loads the active pane's overlay config snapshot into the handlers,
    /// renders each handler's controls, applies updates, then saves the
    /// resulting config back to the pane. This makes every sub-control
    /// (categories, day, products, etc.) per-pane when Sync Layers is off.
    fn render_overlay_controls(
        &mut self,
        ui: &mut egui::Ui,
        pane: &mut PaneState,
        actions: &mut Vec<GuiAction>,
    ) {
        // Load this pane's config snapshot into the handlers.
        if !pane.overlay_configs.is_empty() {
            self.overlays.load_pane_configs(&pane.overlay_configs);
        }

        let ctx = PaneControlContext {
            pane_idx: self.active_pane,
            pane_state: None,
        };

        // Render controls and collect updates.
        let mut updates: Vec<(OverlayKind, ControlUpdate)> = Vec::new();
        let mut probe = ControlProbe::default();

        for (i, &kind) in OVERLAY_CONTROL_ORDER.iter().enumerate() {
            if i > 0 {
                ui.add_space(6.0);
                ui.separator();
            }
            let controls = self.overlays.controls(kind, &ctx);

            for item in &controls {
                render_control_item(ui, kind, item, &mut updates, &mut probe);
            }
        }

        #[cfg(test)]
        self.last_dropdowns.extend(probe.drawn.iter().cloned());
        #[cfg(not(test))]
        let _ = probe;

        // Apply updates and handle effects.
        let mut pane_ctx = PaneControlContextMut {
            pane_idx: self.active_pane,
            pane_state: None,
        };

        for (kind, update) in updates {
            let effect = self.overlays.apply_control(kind, &update, &mut pane_ctx);
            if matches!(effect, ControlEffect::Fetch) {
                actions.push(GuiAction::FetchOverlay {
                    kind,
                    pane_idx: self.active_pane,
                });
            }
        }

        // Save the (possibly mutated) handler state back to the pane.
        pane.overlay_configs = self.overlays.save_pane_configs();
        pane.enabled_overlays = self.overlays.save_enabled_map();
    }

    /// Return the pane indices that loop actions should target.
    /// When `sync_layers` is on and there are multiple panes, returns all pane indices;
    /// otherwise returns only the active pane.
    ///
    /// Panes with no plan view are left out. A loop is a sequence of rendered
    /// plan-view tilts, and `dispatch_loop_renders` no longer feeds a non-map
    /// pane — so enabling the loop with sync on would otherwise put every
    /// section and volume pane into `is_active()` with a frame list nothing ever
    /// fills, which is a spinner in the loop transport that never finishes and a
    /// download queue serving nobody.
    ///
    /// The active pane is a target unconditionally and is deliberately **never
    /// tested**. This runs from inside `render_loop_controls`, which the layers
    /// panel calls while the active pane is held out by `mem::take` — so
    /// `self.panes[self.active_pane]` is a default `PaneState`, a *map* pane
    /// whatever the real one is. Reading `is_map()` off that slot would be
    /// reading the placeholder, and it would agree with reality only by
    /// coincidence (the loop control is drawn for map panes only). Including the
    /// index without asking is correct either way: it is the pane whose own
    /// checkbox was clicked.
    fn loop_sync_targets(&self) -> Vec<usize> {
        if self.sync_layers && self.pane_layout.pane_count > 1 {
            (0..self.pane_layout.pane_count)
                .filter(|&idx| {
                    idx == self.active_pane || self.panes.get(idx).is_none_or(PaneState::is_map)
                })
                .collect()
        } else {
            vec![self.active_pane]
        }
    }

    /// Turn an overlay on or off for the active pane, the way the layers panel
    /// does.
    ///
    /// Both halves must be written: `render_layer_controls` reloads the
    /// handlers from `overlay_configs` every frame and saves the enabled map
    /// back over `enabled_overlays`, so a change that never reached the config
    /// is undone on the next frame.
    fn set_active_pane_overlay(&mut self, kind: OverlayKind, on: bool) {
        let configs = self.panes[self.active_pane].overlay_configs.clone();
        if !configs.is_empty() {
            self.overlays.load_pane_configs(&configs);
        }
        self.overlays.set_enabled(kind, on);

        let configs = self.overlays.save_pane_configs();
        let enabled = self.overlays.save_enabled_map();
        let pane = &mut self.panes[self.active_pane];
        pane.overlay_configs = configs;
        pane.enabled_overlays = enabled;
    }

    /// Propagate layer settings from the active pane to all others (when sync is enabled).
    /// Also converges site and scan_info so all panes display the same radar site.
    ///
    /// # `content` is deliberately not one of the fields
    ///
    /// `PaneContent` derives `Clone`, so copying it costs nothing and the
    /// omission is a decision rather than a limitation. What sync means here is
    /// *what every pane is looking at* — the same radar, the same volume, the
    /// same moment, the same time — and a pane's **kind** is not that. It is how
    /// this pane presents it.
    ///
    /// Copying it would defeat the feature outright: a user splits the screen and
    /// converts pane 2 to a 3D view precisely in order to see the volume
    /// *alongside* the plan view on pane 1. Propagating the kind would convert
    /// pane 1 as well, leaving two identical 3D panes and no map — from a
    /// setting called "Sync Layers", with nothing to say what happened.
    ///
    /// The consequence, accepted: synced panes disagree about kind, and
    /// per-kind state (a section's line, a volume's camera) is per pane. That is
    /// the intended reading. Each still converges on site, scan, product,
    /// elevation, live-or-parked, step and overlays, so the *subject* is shared
    /// and only the presentation differs.
    ///
    /// `selected_elevation` is propagated to non-map panes too, even though a
    /// whole-volume pane has no tilt. It is inert there rather than wrong, and
    /// keeping it means a pane converted back to a map lands on the tilt its
    /// siblings are showing instead of on whatever it held before.
    fn propagate_layer_sync(&mut self) {
        if !self.sync_layers || self.pane_layout.pane_count <= 1 {
            return;
        }
        let src = &self.panes[self.active_pane];
        let active_site = src.site.clone();
        let active_scan_info = src.scan_info.clone();
        let active_viewing_live = src.viewing_live;
        let active_time_step_secs = src.time_step_secs;
        let active_draw_order = src.draw_order.clone();
        let active_enabled_overlays = src.enabled_overlays.clone();
        let active_overlay_configs = src.overlay_configs.clone();
        let active_selected_product = src.selected_product;
        let active_selected_elevation = src.selected_elevation;

        // Sync per-pane fields including enabled overlays, configs, and radar
        // product/elevation. Not `content`: see the note on this function for
        // why the pane's kind is the one field sync deliberately leaves alone.
        for (idx, p) in self.panes.iter_mut().enumerate() {
            if idx == self.active_pane {
                continue;
            }
            p.site = active_site.clone();
            p.scan_info = active_scan_info.clone();
            p.viewing_live = active_viewing_live;
            p.time_step_secs = active_time_step_secs;
            p.draw_order = active_draw_order.clone();
            p.enabled_overlays = active_enabled_overlays.clone();
            p.overlay_configs = active_overlay_configs.clone();
            p.selected_product = active_selected_product;
            p.selected_elevation = active_selected_elevation;
        }
    }

    /// Initialize per-pane `enabled_overlays` from the current handler states.
    ///
    /// Called after `new()`, after `load_ui_config()` (backward compatibility
    /// for configs without per-pane maps), and when the pane-count picker
    /// grows the vector — anywhere a pane could otherwise be left with an
    /// empty map that `is_overlay_enabled` reads as everything-off.
    pub fn initialize_pane_enabled(&mut self) {
        let defaults = self.overlays.build_enabled_map();
        let default_configs = self.overlays.save_pane_configs();
        for pane in &mut self.panes {
            for (&kind, &enabled) in &defaults {
                pane.enabled_overlays.entry(kind).or_insert(enabled);
            }
            // Seed overlay configs from handler defaults for panes with empty configs.
            if pane.overlay_configs.is_empty() {
                pane.overlay_configs = default_configs.clone();
            }
        }
    }

    /// Returns `true` if any pane has the given overlay kind enabled.
    ///
    /// Used for auto-poll decisions: we should fetch data for an overlay
    /// if at least one pane wants to display it.
    ///
    /// # Why a pane with no map does not count, while keeping its toggles
    ///
    /// This and [`Self::first_pane_with_overlay_enabled`] ask "is this overlay
    /// being *drawn* anywhere?", and every overlay is a layer over map tiles,
    /// geo-positioned against a projector a section or a volume pane does not
    /// have. So a converted pane must not keep an overlay's auto-poll timer
    /// running, or be the pane a `FetchOverlay` is attributed to.
    ///
    /// Its `enabled_overlays` is deliberately left alone rather than cleared,
    /// which is the same choice `set_kind` makes about the viewport and the tilt:
    /// it is the user's remembered answer to "which layers do I want", and it
    /// becomes meaningful again the instant the pane is converted back. Filtering
    /// the readers keeps both properties; clearing the record would lose one.
    ///
    /// Both are called from `check_auto_polls`, at the very top of [`Self::ui`]
    /// before any pane is `mem::take`n, so reading the kind through `self.panes`
    /// is safe here — see [`PaneContent`](crate::pane::PaneContent)'s module docs
    /// for why that is worth checking rather than assuming.
    pub fn any_pane_has_overlay_enabled(&self, kind: OverlayKind) -> bool {
        self.panes
            .iter()
            .take(self.pane_layout.pane_count)
            .any(|p| p.is_map() && p.is_overlay_enabled(kind))
    }

    /// Returns the index of the first pane that has the given overlay kind enabled,
    /// or `None` if no pane has it enabled.
    ///
    /// Panes with no map are skipped; see [`Self::any_pane_has_overlay_enabled`].
    pub fn first_pane_with_overlay_enabled(&self, kind: OverlayKind) -> Option<usize> {
        self.panes
            .iter()
            .take(self.pane_layout.pane_count)
            .position(|p| p.is_map() && p.is_overlay_enabled(kind))
    }

    /// Get the active pane (immutable).
    pub fn active_pane(&self) -> &PaneState {
        &self.panes[self.active_pane]
    }

    /// Index of the active pane, for the `GuiAction`s that address one by index.
    pub fn active_pane_idx(&self) -> usize {
        self.active_pane
    }

    /// Get the active pane (mutable).
    pub fn active_pane_mut(&mut self) -> &mut PaneState {
        &mut self.panes[self.active_pane]
    }

    /// Every pane the layout is currently showing, in pane-index order.
    ///
    /// Splitting to fewer panes leaves the extra `PaneState`s in the vector so a
    /// re-split remembers them, and they are neither drawn nor updated — so the
    /// slice stops at `pane_count`, and code that acts on "all panes" must go
    /// through here rather than iterating `panes` directly.
    ///
    /// One caveat, shared with [`Self::pane`] and [`Self::pane_mut`]: while the
    /// settings panel is drawing, the active pane is held out of the vector by
    /// `mem::take` and its slot is a default `PaneState`. Nothing that reaches these
    /// accessors runs in that window — the loop and scan paths run either side of
    /// the egui pass, never inside it — but a future caller inside the UI pass would
    /// read a blank pane rather than the live one.
    pub fn panes(&self) -> &[PaneState] {
        &self.panes[..self.visible_pane_count()]
    }

    /// [`Self::panes`] for the paths that update pane state (loop frames, scan
    /// info), with the same bound.
    pub fn panes_mut(&mut self) -> &mut [PaneState] {
        let count = self.visible_pane_count();
        &mut self.panes[..count]
    }

    /// `pane_count` clamped to what the vector actually holds. The two are kept in
    /// step by every path that changes the layout, but slicing past the end would
    /// panic, and no pane update is worth a crash.
    fn visible_pane_count(&self) -> usize {
        self.pane_layout.pane_count.min(self.panes.len())
    }

    /// Get a specific pane by index (immutable), or `None` if out of bounds.
    pub fn pane(&self, idx: usize) -> Option<&PaneState> {
        self.panes.get(idx)
    }

    /// Get a specific pane by index (mutable), or `None` if out of bounds.
    pub fn pane_mut(&mut self, idx: usize) -> Option<&mut PaneState> {
        self.panes.get_mut(idx)
    }

    /// Ask for pane `pane_idx` to become `kind`, taking effect at the end of the
    /// frame.
    ///
    /// **The only route by which the UI may change a pane's kind.**
    /// `PaneState::set_kind` is the mechanism and stays reachable for the config
    /// loader and for test fixtures; nothing drawing a frame calls it, because two
    /// UI paths hold the pane it would write out of the vector as a `mem::take`
    /// placeholder about to be thrown away. The menu dispatcher, as it happens, is
    /// *not* inside either window today — a direct write from it would work — so
    /// this is one rule for both dispatch and the writers WP-G adds inside
    /// `render_panes`' take, rather than a fix for a live bug on this path. The
    /// [`pending_pane_kind`](Self::pending_pane_kind) field lays out which is
    /// which.
    ///
    /// Out-of-range indices are recorded and dropped on application rather than
    /// refused here, so a caller inside the UI pass never has to know whether the
    /// vector currently holds the pane it is drawing.
    pub(crate) fn request_pane_kind(&mut self, pane_idx: PaneId, kind: crate::pane::PaneKind) {
        self.pending_pane_kind = Some((pane_idx, kind));
    }

    /// Grow or shrink the layout to `count` panes, seeding any new ones, and
    /// report whether the layout actually reached that count.
    ///
    /// **The one writer of the pane count.** Factored out of the pane picker
    /// rather than left inline because the picker is no longer the only thing
    /// that changes it: a region drag on a layout with room in it opens a 3D pane
    /// beside the map, and a section line does the same for a cross-section.
    /// Three copies of this would be three places to remember
    /// [`Self::initialize_pane_enabled`], and forgetting it in one of them
    /// produces a pane that draws no overlays at all — Radar included — which
    /// reads as a broken pane rather than as a missing seed. It is not a compile
    /// error and not a panic; it is a blank pane, from one missing call.
    ///
    /// **The caller must have put any `mem::take`n pane back first.** This indexes
    /// `self.panes` directly, and a taken pane's slot holds a default map pane
    /// whose site a new pane would then be seeded from.
    ///
    /// Returns `false` when the layout could not reach `count` —
    /// `PaneLayout::for_count` clamps, so asking for more than it allows leaves
    /// the count where it was rather than producing panes no rect is drawn for.
    /// The active-pane bound is checked against the **clamped** count for the same
    /// reason: comparing against the requested one would leave `active_pane`
    /// pointing past the end of a layout that refused to grow.
    fn set_pane_count(&mut self, count: usize) -> bool {
        let active_site = self.panes[self.active_pane].site.clone();
        let active_scan_info = self.panes[self.active_pane].scan_info.clone();
        while self.panes.len() < count {
            let mut new_pane = PaneState::with_site(active_site.clone());
            new_pane.scan_info = active_scan_info.clone();
            self.panes.push(new_pane);
        }
        // A pane born here has empty overlay maps, and `is_overlay_enabled` reads
        // a missing entry as *off* — so with layer sync disabled it would draw no
        // overlays at all, Radar included. Seed it from the handlers, which hold
        // the active pane's state (reloaded at the end of every frame in
        // `Gui::ui`), the same way startup does.
        self.initialize_pane_enabled();
        self.pane_layout = PaneLayout::for_count(count);
        if self.active_pane >= self.pane_layout.pane_count {
            self.active_pane = 0;
        }
        self.pane_layout.pane_count == count
    }

    /// Aim a 3D pane at the region the frame committed, if any.
    ///
    /// Called from [`Self::ui`] after the pane loop and after
    /// [`Self::apply_pending_pane_kind`], where every pane is back in the vector
    /// and growing the count is safe. `ui_region::destination_for` holds the
    /// decision about *which* pane and the reasoning for it; this is only the
    /// edit.
    fn apply_pending_region(&mut self) {
        let Some(pending) = self.pending_region.take() else {
            return;
        };
        let max_panes = self.layout.width.max_panes();
        let Some(destination) =
            crate::ui_region::destination_for(self.panes(), pending.source_pane, max_panes)
        else {
            log::warn!("no pane to put a 3D region on; dropping it");
            return;
        };
        let pane_idx = match destination {
            crate::ui_region::RegionDestination::Existing(idx) => idx,
            crate::ui_region::RegionDestination::Grow(count) => {
                if !self.set_pane_count(count) {
                    log::warn!("the layout refused to grow to {count}; dropping a 3D region");
                    return;
                }
                count - 1
            }
            crate::ui_region::RegionDestination::Convert(idx) => idx,
        };
        let Some(pane) = self.panes.get_mut(pane_idx) else {
            log::warn!("pane {pane_idx} is gone; not aiming a 3D region at it");
            return;
        };
        // Idempotent when it is already a 3D view, and the direct call is safe
        // here for the reason `request_pane_kind` names: this runs after the pane
        // loop, so nothing is `mem::take`n and the write lands in the vector
        // rather than in a placeholder about to be discarded.
        pane.set_kind(crate::pane::PaneKind::Volume);
        // A pane that has just been converted or grown has the default camera,
        // which is what should happen — but one that is being *re-aimed* keeps
        // the angle the user set. Only the region and its provenance are written.
        if let Some(volume) = pane.volume_mut() {
            volume.region = Some(pending.region);
            volume.source_pane = Some(pending.source_pane);
        }
        // The pane the region was drawn *from* stays active. A region drag is an
        // instruction about another pane, not a request to go and look at it, and
        // stealing focus mid-gesture is how a user loses the map they were
        // working on.
    }

    /// Arm or disarm the region drag.
    ///
    /// Disarming throws away any drag in flight rather than committing it: a user
    /// who reaches for the menu with the button still down is cancelling, and a
    /// box that appeared because of it would be one nobody asked for.
    ///
    /// # Arming this disarms the cross-section draw
    ///
    /// The two are the only armed modal drags on a map pane, and they are spelled
    /// identically — press, move, release, on the same pane, with the same button
    /// or the same finger. With both on, one drag would have to mean two things:
    /// the section pipeline would anchor a line while `handle_region_drag` read
    /// the same press raw and started a box, and the release would commit both. A
    /// single gesture would then grow the layout twice, and in a full layout the
    /// second applier's last resort is the pane the first one just filled — so one
    /// of the two completed gestures would visibly produce nothing.
    ///
    /// Turning the other off is the only rule that keeps the menu honest, because
    /// both entries are checkboxes: whichever the user ticked last is the one
    /// showing ticked, and it is the one a drag will do. Silently ignoring the
    /// second arm, or refusing it, would leave a ticked box that does nothing.
    ///
    /// Written as a direct field write rather than as a call to
    /// [`Self::set_section_draw_armed`], so the two setters cannot recurse into
    /// each other.
    pub(crate) fn set_region_arm(&mut self, on: bool) {
        self.region_arm = on;
        if on {
            self.section_draw_armed = false;
            self.section_anchor = None;
        } else {
            self.region_drag = None;
        }
    }

    /// [`Self::set_region_arm`] under the name the region tests already use.
    #[cfg(test)]
    pub(crate) fn set_region_arm_for_test(&mut self, on: bool) {
        self.set_region_arm(on);
    }

    /// Whether the region drag is armed.
    #[cfg(test)]
    pub(crate) fn region_arm_for_test(&self) -> bool {
        self.region_arm
    }

    /// Apply the pane conversion the frame asked for, if any.
    ///
    /// Called from [`Self::ui`] after the pane loop, where every pane is back in
    /// the vector. Converting a pane keeps everything about what it is looking
    /// at — see `PaneState::set_kind` — so there is nothing else to carry across.
    fn apply_pending_pane_kind(&mut self, actions: &mut Vec<GuiAction>) {
        let Some((pane_idx, kind)) = self.pending_pane_kind.take() else {
            return;
        };
        match self.panes.get_mut(pane_idx) {
            Some(pane) => {
                // Before the conversion, because after it the pane no longer
                // remembers it was a 3D view. A voxel grid is 1–8 MiB of host
                // memory plus a GPU texture, refcounted by the volume it was
                // built from, and this is the only moment a pane can stop
                // needing one without anything else noticing: the pane is still
                // on screen, still on the same site, still live. Nothing else in
                // the frame is going to come back and ask.
                if pane.kind() == crate::pane::PaneKind::Volume
                    && kind != crate::pane::PaneKind::Volume
                {
                    actions.push(GuiAction::ReleaseVolume { pane_idx });
                }
                pane.set_kind(kind);
            }
            // A pane the layout no longer holds, which a pane-count change in the
            // same frame can produce. Dropped rather than clamped to another
            // index: converting a pane the user did not point at is worse than
            // converting none.
            None => log::warn!("pane {pane_idx} is gone; not converting it to {kind:?}"),
        }
    }

    /// Whether the cross-section draw is armed.
    pub fn section_draw_armed(&self) -> bool {
        self.section_draw_armed
    }

    /// Arm or disarm the cross-section draw.
    ///
    /// Disarming drops any half-drawn line: the anchor means nothing once the
    /// mode it belongs to is off, and leaving it would make re-arming resume a
    /// drag the user abandoned minutes ago.
    ///
    /// Arming it disarms the 3D region drag, and drops any box in flight, for the
    /// reason [`Self::set_region_arm`] gives at length: one drag on one map pane
    /// cannot be both a section line and a region box. Direct field writes rather
    /// than a call to that setter, so the two cannot recurse into each other.
    pub fn set_section_draw_armed(&mut self, armed: bool) {
        self.section_draw_armed = armed;
        if armed {
            self.region_arm = false;
            self.region_drag = None;
        } else {
            self.section_anchor = None;
        }
    }

    /// The rubber band to draw on pane `pane_idx`, in screen points, or `None`.
    ///
    /// Both endpoints are pixels rather than ground, deliberately: this is a
    /// preview of a gesture in progress, and it should track the finger exactly
    /// even on the frame a wheel-zoom has moved the map under it. The *stored*
    /// anchor is geographic — see [`SectionAnchor`] — and it is that one the
    /// committed line is built from.
    pub(crate) fn section_rubber_band(&self, pane_idx: PaneId) -> Option<(egui::Pos2, egui::Pos2)> {
        let anchor = self.section_anchor.as_ref()?;
        (anchor.pane_idx == pane_idx).then_some((anchor.screen, anchor.current))
    }

    /// Give the line this frame drew to a pane, converting or creating one if
    /// need be.
    ///
    /// Called from [`Self::ui`] after the pane loop, where every pane is back in
    /// the vector and growing the count can no longer desynchronise a rect from
    /// the click that was hit-tested against it.
    ///
    /// # The target rule is total
    ///
    /// A drawn line always lands somewhere. Four steps, in order, and the order
    /// is the whole design:
    ///
    /// 1. **A section pane already sourced from this map.** Drawing a second
    ///    line on a map the user has already sectioned means "cut *there*
    ///    instead", not "give me another section pane" — otherwise three lines
    ///    fill the screen with panes nobody asked for.
    /// 2. **Grow the layout.** A section beside the map it was cut from is the
    ///    picture the feature is for, and it costs the user nothing they had.
    /// 3. **The lowest-indexed section pane.** The layout is full; re-aiming an
    ///    existing section is the cheapest thing that can still answer.
    /// 4. **The highest-indexed pane that is not the one drawn on.** Converting
    ///    a map is a real loss, so it is last — but it is *there*, because the
    ///    alternative is a drag that silently does nothing. The pane drawn on is
    ///    excluded because taking away the map under the line, while other panes
    ///    exist to take instead, is the one conversion that is certainly wrong.
    /// 5. **The pane drawn on.** Reachable only in a one-pane layout that cannot
    ///    grow — a phone in portrait — and right there: on a screen with room
    ///    for one thing, asking for a section is asking to look at a section.
    ///    The pane's site, product and viewport all survive the conversion, so
    ///    turning the checkbox back off restores the map it was.
    fn apply_pending_section_line(&mut self) {
        let Some((source, line)) = self.pending_section_line.take() else {
            return;
        };

        // Whatever the source map is looking at, so a line drawn on a
        // reflectivity map cuts reflectivity. A product with no vertical
        // structure is carried across too, rather than quietly swapped: the
        // pane says which product it cannot slice and offers the picker to
        // change it, where a silent substitution would leave the user reading a
        // moment they did not ask for.
        let (source_product, source_site, source_scan) = match self.panes.get(source) {
            Some(pane) => (
                pane.selected_product,
                pane.site.clone(),
                pane.scan_info.clone(),
            ),
            None => {
                log::warn!("pane {source} drew a section line and is already gone");
                return;
            }
        };

        let target = self
            .section_pane_sourced_from(source)
            .or_else(|| self.grown_section_pane())
            .or_else(|| self.lowest_section_pane())
            .or_else(|| self.highest_pane_other_than(source))
            // Total by construction: `highest_pane_other_than` only answers
            // `None` in a one-pane layout, and in one the source *is* the only
            // pane there is. A drawn line is never silently dropped.
            .unwrap_or(source);

        let Some(pane) = self.panes.get_mut(target) else {
            log::warn!("no pane could hold the section drawn on pane {source}");
            return;
        };
        pane.set_kind(crate::pane::PaneKind::CrossSection);
        pane.selected_product = source_product;
        pane.site = source_site;
        pane.scan_info = source_scan;
        if let Some(section) = pane.cross_section_mut() {
            section.line = Some(line);
            section.source_pane = Some(source);
            // The picture on screen is of the old line. Cleared rather than
            // left to the staleness comparison, because a section pane whose
            // texture outlives its line shows a cut through ground the user is
            // no longer pointing at, for as long as the re-cut takes.
            section.section = None;
            section.texture = None;
            section.unavailable = None;
            section.rendered_for = None;
        }
        self.active_pane = target;
    }

    /// The first section pane whose line was drawn on `source`.
    fn section_pane_sourced_from(&self, source: PaneId) -> Option<PaneId> {
        (0..self.visible_pane_count()).find(|&idx| {
            self.panes[idx]
                .cross_section()
                .is_some_and(|s| s.source_pane == Some(source))
        })
    }

    /// A new pane at the end of the layout, or `None` if the layout is full.
    fn grown_section_pane(&mut self) -> Option<PaneId> {
        let wanted = self.pane_layout.pane_count + 1;
        if wanted > self.layout.width.max_panes() {
            return None;
        }
        self.set_pane_count(wanted).then(|| wanted - 1)
    }

    /// The lowest-indexed section pane, whatever it was aimed at.
    fn lowest_section_pane(&self) -> Option<PaneId> {
        (0..self.visible_pane_count()).find(|&idx| self.panes[idx].cross_section().is_some())
    }

    /// The highest-indexed visible pane that is not `source`.
    fn highest_pane_other_than(&self, source: PaneId) -> Option<PaneId> {
        (0..self.visible_pane_count())
            .rev()
            .find(|&idx| idx != source)
    }

    /// The pane conversion this frame recorded and has not applied yet.
    ///
    /// Read by `ui_menu`'s dispatcher fingerprint, which has to be able to see
    /// that the toggle's arm did something: recording the request *is* what that
    /// arm does, and applying it is a separate step with its own test. Nothing in
    /// production reads it — the applier takes the field directly.
    #[cfg(test)]
    pub(crate) fn pending_pane_kind_for_test(&self) -> Option<(PaneId, crate::pane::PaneKind)> {
        self.pending_pane_kind
    }

    /// What the 3D arm decided for each volume pane on the last frame.
    #[cfg(test)]
    pub(crate) fn volume_arms_for_test(&self) -> &[VolumeArmProbe] {
        &self.last_volume_arms
    }

    /// Whether pane `idx` is a pane the **plan-view** pipeline must skip: it
    /// exists, and it is not a map.
    ///
    /// One predicate for the seven frontend loops that dispatch, cache, broadcast
    /// or gate on a plan-view raster: `dispatch_pane_renders`, the sibling
    /// broadcast in `poll_render_results`, both halves of `dispatch_loop_renders`,
    /// the loop-frame broadcast in `poll_loop_render_results`,
    /// `restore_cached_render`, and `sync_loop_playback_start`. Named once because
    /// they have to agree: a pane that is dispatched to but not broadcast to, or
    /// broadcast to but never dispatched, is a pane wedged with
    /// `render_in_flight` set forever — and one counted as a loop participant
    /// while nothing renders its frames holds every *other* pane's loop back.
    ///
    /// Written in the negative on purpose. An index past the end answers
    /// `false` — "not a pane to skip" — which leaves out-of-range handling
    /// exactly where each caller already had it, rather than folding a second,
    /// different question into this one. `dispatch_pane_renders` in particular
    /// iterates the layout's raw `pane_count`, which can outrun the vector, and
    /// its own `else` branch is what deals with that.
    ///
    /// The `mem::take` caveat on [`Self::pane`] applies in full: during the UI
    /// pass a taken pane reads as a map. Every caller of this runs from the
    /// frontend's frame loop, outside the egui pass, which is what makes it
    /// safe — see [`PaneContent`](crate::pane::PaneContent)'s module docs.
    pub fn pane_has_no_plan_view(&self, idx: PaneId) -> bool {
        self.pane(idx).is_some_and(|pane| !pane.is_map())
    }

    /// Whether pane `idx` needs every cut of its site's volume rather than the
    /// one tilt it has selected, because of *what kind of pane it is*.
    ///
    /// The view-side half of the whole-volume safety property;
    /// [`RadarProduct::reads_whole_volume`] is the data-side half, and
    /// `App::cut_selection_for` has to honour both. An index past the end needs
    /// nothing.
    pub fn pane_consumes_whole_volume(&self, idx: PaneId) -> bool {
        self.pane(idx)
            .is_some_and(|pane| pane.kind().consumes_whole_volume())
    }

    /// Get the rendering params for a specific pane.
    pub fn get_rendering_params_for_pane(&self, pane_idx: PaneId) -> Option<(RadarProduct, f32)> {
        self.panes
            .get(pane_idx)
            .and_then(|p| p.get_rendering_params())
    }

    /// Number of active panes.
    pub fn pane_count(&self) -> usize {
        self.pane_layout.pane_count
    }

    /// Split the map into `count` panes, as the settings UI's pane picker does.
    #[cfg(test)]
    pub(crate) fn set_pane_count_for_test(&mut self, count: usize) {
        while self.panes.len() < count {
            self.panes.push(PaneState::new());
        }
        self.pane_layout = PaneLayout::for_count(count);
        if self.active_pane >= count {
            self.active_pane = 0;
        }
    }

    /// The rect the pane grid was laid out in on the last frame.
    #[cfg(test)]
    pub(crate) fn map_panel_rect_for_test(&self) -> egui::Rect {
        self.last_map_panel_rect
    }

    /// The floating-chrome rects the last frame excluded from map clicks.
    #[cfg(test)]
    pub(crate) fn excluded_rects_for_test(&self) -> &[egui::Rect] {
        &self.last_excluded_rects
    }

    /// The egui `Id`s the last frame's layers panel resolved.
    #[cfg(test)]
    pub(crate) fn widget_id_probes(&self) -> &[(&'static str, egui::Id)] {
        &self.widget_id_probes
    }

    /// Every menu leaf the last frame actually drew, as the renderer reported
    /// it — see [`ui_menu::DrawnMenuLeaf`].
    #[cfg(test)]
    pub(crate) fn menu_leaves_for_test(&self) -> &[ui_menu::DrawnMenuLeaf] {
        &self.last_menu_leaves
    }

    /// The pointer state `render_panes` resolved for each pane last frame.
    #[cfg(test)]
    pub(crate) fn pane_pointers_for_test(&self) -> &[crate::ui_input::PanePointerProbe] {
        &self.last_pane_pointers
    }

    /// Which render arm ran for each pane last frame. See [`PaneContentProbe`].
    #[cfg(test)]
    pub(crate) fn pane_content_for_test(&self) -> &[PaneContentProbe] {
        &self.last_pane_content
    }

    /// Whether a label-tile source has been created, which is the observable half
    /// of "is this app fetching the city-label tile pyramid?".
    ///
    /// `MapTileState::ensure_label_tiles` only ever *creates* the source, so this
    /// answering `false` after a frame means no fetch was ever started.
    #[cfg(test)]
    pub(crate) fn label_tiles_made_for_test(&self) -> bool {
        self.map_tiles.label_tiles_light.is_some() || self.map_tiles.label_tiles_dark.is_some()
    }

    /// Record that the arm for `kind` drew pane `pane_idx` into `rect`.
    ///
    /// Called from inside each arm of `render_panes`' kind branch, with the
    /// kind written out as a literal there rather than passed down from the
    /// branch's subject — that literal is the whole reason the probe can catch a
    /// mis-wired arm. A no-op outside tests, like `ControlProbe::record_dropdown`.
    #[inline]
    pub(super) fn record_pane_content(
        &mut self,
        _pane_idx: usize,
        _kind: crate::pane::PaneKind,
        _rect: egui::Rect,
    ) {
        #[cfg(test)]
        self.last_pane_content.push(PaneContentProbe {
            pane_idx: _pane_idx,
            kind: _kind,
            rect: _rect,
        });
    }

    /// The pane-count buttons the picker drew on the last frame.
    #[cfg(test)]
    pub(crate) fn pane_options_for_test(&self) -> &[PaneOptionProbe] {
        &self.last_pane_options
    }

    /// The excluded rects `render_panes` was handed on the last frame.
    #[cfg(test)]
    pub(crate) fn map_excluded_rects_for_test(&self) -> &[egui::Rect] {
        &self.last_map_excluded_rects
    }

    /// What the last frame's status bar drew.
    #[cfg(test)]
    pub(crate) fn status_bar_for_test(&self) -> &StatusBarProbe {
        &self.last_status_bar
    }

    /// Which pane is currently active.
    #[cfg(test)]
    pub(crate) fn active_pane_index_for_test(&self) -> PaneId {
        self.active_pane
    }

    /// Turn layer sync between panes on or off, as its checkbox does.
    #[cfg(test)]
    pub(crate) fn set_sync_layers_for_test(&mut self, on: bool) {
        self.sync_layers = on;
    }

    /// Set one pane's overlay state, writing the config as well as the enabled
    /// map — `render_layer_controls` reloads the handlers from the config every
    /// frame, so a write to `enabled_overlays` alone is undone immediately.
    #[cfg(test)]
    pub(crate) fn set_overlay_on_pane_for_test(&mut self, idx: usize, kind: OverlayKind, on: bool) {
        let configs = self.panes[idx].overlay_configs.clone();
        if !configs.is_empty() {
            self.overlays.load_pane_configs(&configs);
        }
        self.overlays.set_enabled(kind, on);
        let configs = self.overlays.save_pane_configs();
        let enabled = self.overlays.save_enabled_map();
        let pane = &mut self.panes[idx];
        pane.overlay_configs = configs;
        pane.enabled_overlays = enabled;
    }

    /// Open or close the layers drawer, as the hamburger does.
    #[cfg(test)]
    pub(crate) fn set_drawer_open(&mut self, open: bool) {
        self.drawer_open = open;
    }

    /// Every handler dropdown the last frame drew. See [`DrawnDropdown`].
    #[cfg(test)]
    pub(crate) fn dropdowns_for_test(&self) -> &[DrawnDropdown] {
        &self.last_dropdowns
    }

    /// The `(options, selected)` a handler is currently offering under `label`
    /// — the *model* behind a [`DrawnDropdown`], asked of the handler rather
    /// than of the renderer.
    #[cfg(test)]
    pub(crate) fn dropdown_model_for_test(
        &self,
        label: &str,
    ) -> Option<(Vec<(String, String)>, String)> {
        let ctx = PaneControlContext {
            pane_idx: self.active_pane,
            pane_state: None,
        };
        fn find(items: &[ControlItem], label: &str) -> Option<(Vec<(String, String)>, String)> {
            for item in items {
                match item {
                    ControlItem::Dropdown {
                        label: l,
                        options,
                        selected,
                        ..
                    } if l == label => {
                        return Some((options.clone(), selected.clone()));
                    }
                    ControlItem::Section { items, .. } => {
                        if let Some(found) = find(items, label) {
                            return Some(found);
                        }
                    }
                    _ => {}
                }
            }
            None
        }
        OVERLAY_CONTROL_ORDER
            .iter()
            .find_map(|&kind| find(&self.overlays.controls(kind, &ctx), label))
    }

    /// This frame's resolved layout, for tests asserting on the breakpoint.
    #[cfg(test)]
    pub(crate) fn layout_for_test(&self) -> LayoutCtx {
        self.layout
    }

    /// The pane rects the layout produces inside the map panel, as
    /// `render_panes` computes them.
    ///
    /// "As `render_panes` computes them" is the whole contract, so the bound is
    /// [`Self::visible_pane_count`] like the real loop's: with the raw count a
    /// test would be handed rects for panes no frame ever drew, and any test that
    /// clicked one would be asserting about a pane the app does not have.
    #[cfg(test)]
    pub(crate) fn pane_rects_for_test(&self) -> Vec<egui::Rect> {
        let panel = self.last_map_panel_rect;
        (0..self.visible_pane_count())
            .map(|idx| self.pane_layout.pane_rect(idx, panel))
            .collect()
    }

    /// Claim `count` panes in the layout **without** growing the pane vector.
    ///
    /// The skew `visible_pane_count` exists for, built on purpose. No production
    /// writer can reach it — see `detect_active_pane_click` — so a test that wants
    /// it has to say so, which is also what keeps the difference between "clamped
    /// by a caller" and "clamped by the type" visible.
    #[cfg(test)]
    pub(crate) fn claim_pane_count_for_test(&mut self, count: usize) {
        self.pane_layout = PaneLayout::for_count(count);
    }

    /// Turn a texture overlay on for every pane, as ticking its layer toggle does.
    ///
    /// The handler's own state has to be written back into each pane's
    /// `overlay_configs`, not just into `enabled_overlays`: every frame reloads the
    /// registry from the pane's configs and then saves the enabled map back out, so
    /// a pane whose config still says "off" turns itself off again on the next frame.
    #[cfg(test)]
    pub(crate) fn enable_overlay_for_test(&mut self, kind: OverlayKind) {
        self.overlays.set_enabled(kind, true);
        let configs = self.overlays.save_pane_configs();
        let enabled = self.overlays.save_enabled_map();
        for pane in &mut self.panes {
            pane.overlay_configs = configs.clone();
            pane.enabled_overlays = enabled.clone();
        }
    }

    /// Whether viewport sync is enabled (all panes share the same map viewport).
    pub fn is_viewport_sync(&self) -> bool {
        self.viewport_sync
    }

    /// Whether layer sync is enabled (layer changes propagate to all panes).
    pub fn is_sync_layers(&self) -> bool {
        self.sync_layers
    }

    /// Get the current radar config
    pub fn get_radar_config(&self) -> &RadarConfig {
        &self.radar.config
    }

    /// Set the radar config
    pub fn set_radar_config(&mut self, config: RadarConfig) {
        let date = config.timestamp.format("%Y-%m-%d").to_string();
        let time = config.timestamp.format("%H:%M:%S").to_string();
        self.radar.config = config;
        self.time_dialog.date_string = date;
        self.time_dialog.time_string = time;
    }

    /// Clear loading_site on all panes viewing the given site.
    pub fn clear_loading_site_for_site(&mut self, site: &str) {
        for pane in &mut self.panes {
            if pane.site == site {
                pane.loading_site = None;
                pane.radar_sites_render_gen = pane.radar_sites_render_gen.wrapping_add(1);
            }
        }
    }

    /// Bump the RadarSites texture generation on all panes (e.g. on theme change).
    pub fn bump_all_radar_sites_gen(&mut self) {
        for pane in &mut self.panes {
            pane.radar_sites_render_gen = pane.radar_sites_render_gen.wrapping_add(1);
        }
    }

    /// Set safe area insets in logical pixels (top, bottom, left, right).
    /// On Android, this compensates for the status bar and navigation bar.
    pub fn set_safe_area_insets(&mut self, top: f32, bottom: f32, left: f32, right: f32) {
        self.safe_area_insets = (top, bottom, left, right);
    }

    /// The insets currently in force, in the same order they are set in.
    ///
    /// This and the three getters below it are the read half of the setters
    /// they sit beside, and they exist for one reason: all four values are
    /// pushed in from the host through a platform bridge this crate cannot
    /// see, and the frontend's tests need somewhere to observe that the
    /// hand-off happened at all. What the UI then *does* with them is covered
    /// here, against the drawn chrome (see `input_harness`), never against
    /// these.
    pub fn safe_area_insets(&self) -> (f32, f32, f32, f32) {
        self.safe_area_insets
    }

    /// Tell the UI whether this platform can quit. `false` drops Exit from the
    /// menu; on iOS the action is a no-op, so rendering it is a dead button.
    pub fn set_supports_exit(&mut self, supported: bool) {
        self.supports_exit = supported;
    }

    /// See [`set_supports_exit`](Self::set_supports_exit).
    pub fn supports_exit(&self) -> bool {
        self.supports_exit
    }

    /// Set the user's GPS location for the blue dot indicator.
    pub fn set_gps_fix(&mut self, fix: rustdar_gps::GpsFix) {
        self.user_fix = Some(fix);
    }

    /// See [`set_gps_fix`](Self::set_gps_fix).
    pub fn gps_fix(&self) -> Option<&rustdar_gps::GpsFix> {
        self.user_fix.as_ref()
    }

    pub fn set_user_heading(&mut self, heading: f32) {
        self.user_heading = Some(heading);
    }

    /// See [`set_user_heading`](Self::set_user_heading).
    pub fn user_heading(&self) -> Option<f32> {
        self.user_heading
    }

    /// Whether the active pane is showing the most recent (live) scan.
    pub fn is_viewing_live(&self) -> bool {
        self.panes
            .get(self.active_pane)
            .is_some_and(|p| p.viewing_live)
    }

    /// Whether any pane is viewing live (for auto-poll gating).
    pub fn is_any_pane_live(&self) -> bool {
        self.panes
            .iter()
            .take(self.pane_layout.pane_count)
            .any(|p| p.viewing_live)
    }

    /// Set live/historic viewing mode for a specific pane.
    pub fn set_viewing_live_for_pane(&mut self, pane_idx: usize, live: bool) {
        if let Some(pane) = self.panes.get_mut(pane_idx) {
            pane.viewing_live = live;
        }
    }

    /// Get the scan info for the active pane.
    pub fn get_scan_info(&self) -> Option<&ScanInfo> {
        self.panes
            .get(self.active_pane)
            .and_then(|p| p.scan_info.as_ref())
    }

    /// Get the scan info for a specific pane.
    pub fn get_scan_info_for_pane(&self, pane_idx: usize) -> Option<&ScanInfo> {
        self.panes.get(pane_idx).and_then(|p| p.scan_info.as_ref())
    }

    /// Whether auto-poll is active and the event loop should keep waking
    pub fn is_auto_poll_active(&self) -> bool {
        self.auto_poll.is_active()
            || OverlayKind::all().iter().any(|&kind| {
                self.overlays.auto_poll_interval(kind).is_some()
                    && self.any_pane_has_overlay_enabled(kind)
            })
    }

    /// Whether any pane has a loop that is playing or has in-flight work.
    pub fn any_loop_active(&self) -> bool {
        self.panes.iter().any(|p| {
            let ls = &p.loop_state;
            ls.is_active()
                && (ls.is_playing()
                    || ls.is_fetching()
                    || ls.frames.iter().any(|f| f.render_in_flight))
        })
    }

    pub fn clear_graphics_state(&mut self) {
        for pane in &mut self.panes {
            pane.loading_site = None;
            pane.radar_sites_render_gen = pane.radar_sites_render_gen.wrapping_add(1);
            // Clear loop frame textures so they get re-rendered on resume.
            // The frame list and scan cache survive, so dispatch_loop_renders()
            // will re-upload textures automatically.
            for frame in &mut pane.loop_state.frames {
                frame.texture = None;
                frame.render_in_flight = false;
            }
            // Clear overlay texture caches — handles become invalid when the
            // egui context is destroyed. needs_rerender() will trigger fresh
            // background renders.
            for cache in pane.overlay_textures.values_mut() {
                cache.current = None;
                cache.render_in_flight = false;
            }
            // And whatever the pane's *kind* holds — today, a section pane's
            // raster. This is the only place a pane-held handle is released when
            // the egui context dies. Note that every arm deliberately keeps
            // enough to put its picture *back*: the frontend's
            // `restore_section_textures` re-uploads a section from the
            // `CrossSection` this leaves behind, exactly as the loop above
            // relies on `dispatch_loop_renders` re-uploading a loop frame. See
            // `PaneContent::release_textures`.
            pane.content.release_textures();
        }
        self.map_tiles.clear();
        // The painter holds wgpu handles made by the device that is going away,
        // and every one of them — pipelines, the offscreen targets, the uploaded
        // grid — is invalid the moment it does. Dropping the whole painter is
        // the release: the frontend installs a fresh one when the renderer comes
        // back, and until then every 3D pane says so instead of drawing with a
        // dangling handle. This is the surface-loss and suspend/resume half of
        // `ReleaseVolume`.
        self.volume_painter = None;
    }

    /// Install what can draw 3D panes, or take it away.
    ///
    /// Called by the frontend when a renderer is created and, with `None`, when
    /// one is lost. Every 3D pane on screen picks the change up on the next
    /// frame with no other bookkeeping, because the painter is consulted afresh
    /// inside each pane's arm rather than cached anywhere.
    pub fn set_volume_painter(
        &mut self,
        painter: Option<std::sync::Arc<dyn crate::volume_view::VolumePainter>>,
    ) {
        self.volume_painter = painter;
    }

    /// Whatever can draw 3D panes this frame.
    pub(crate) fn volume_painter(
        &self,
    ) -> Option<&std::sync::Arc<dyn crate::volume_view::VolumePainter>> {
        self.volume_painter.as_ref()
    }

    /// Propagate the interacted pane's viewport (zoom + position) to all other panes.
    ///
    /// Bounded by [`Self::visible_pane_count`], not the layout's raw count:
    /// hidden panes are neither read as a sync source nor written to, and a
    /// count that ran ahead of the vector cannot index past its end.
    ///
    /// # Why panes with no map are excluded from both ends
    ///
    /// This is the all-panes site a non-map pane breaks the moment one can
    /// exist, and it breaks it in the direction that looks like a bug in the
    /// *other* panes. Every pane carries a `map_memory` whatever its kind —
    /// they are flat fields, deliberately — and `render_panes` resolves the
    /// active pane's pointer through `InteractionState::resolve_active`, which
    /// on the touch path hands that `map_memory` to `TouchGestures::update` and
    /// lets it write a zoom. So a double-tap-drag on a section pane moves a
    /// viewport nothing is drawing, this function then picks that pane as the
    /// **source** because it is the first whose zoom changed, and every map pane
    /// on screen is re-centred and re-zoomed to it. `viewport_sync` defaults
    /// **on**, so that is the shipped default behaviour, not an opt-in.
    ///
    /// Excluded as a *target* as well, for a quieter reason: a converted pane's
    /// viewport is what it comes back to when it is converted back to a map, and
    /// it is persisted per pane. Overwriting it would silently move a map the
    /// user is not looking at yet.
    fn sync_viewports(&mut self, pre_zooms: &[f64], pre_positions: &[Option<walkers::Position>]) {
        let pane_count = self.visible_pane_count();
        if !self.viewport_sync || pane_count <= 1 {
            return;
        }
        let mut source_idx = None;
        for idx in 0..pane_count {
            if !self.panes[idx].is_map() {
                continue;
            }
            if idx < pre_zooms.len() {
                let zoom_diff = (self.panes[idx].map_memory.zoom() - pre_zooms[idx]).abs();
                if zoom_diff > 0.0001 {
                    source_idx = Some(idx);
                    break;
                }
                let prev_pos = &pre_positions[idx];
                let curr_pos = self.panes[idx].map_memory.detached();
                let pos_changed = match (prev_pos, &curr_pos) {
                    (Some(p1), Some(p2)) => {
                        (p1.x() - p2.x()).abs() > 0.00001 || (p1.y() - p2.y()).abs() > 0.00001
                    }
                    (None, Some(_)) | (Some(_), None) => true,
                    _ => false,
                };
                if pos_changed {
                    source_idx = Some(idx);
                    break;
                }
            }
        }
        // Nothing moved, so the active pane holds the others where they are —
        // unless it has no map, in which case its `map_memory` is not a viewport
        // anyone is looking at and there is nothing to propagate. Returning is
        // the whole point: `unwrap_or(self.active_pane)` on its own would make a
        // non-map active pane the source on every frame, which is the same
        // failure as the source scan above with no interaction needed at all.
        let Some(src) = source_idx.or_else(|| {
            self.panes[self.active_pane]
                .is_map()
                .then_some(self.active_pane)
        }) else {
            return;
        };
        let zoom = self.panes[src].map_memory.zoom();
        let pos = self.panes[src].map_memory.detached();
        for idx in 0..pane_count {
            if idx != src && self.panes[idx].is_map() {
                let _ = self.panes[idx].map_memory.set_zoom(zoom);
                if let Some(p) = pos {
                    self.panes[idx].map_memory.center_at(p);
                }
            }
        }
    }
}

#[cfg(test)]
mod storm_motion_override_tests {
    use super::*;

    /// Disabled means "use the vector the RPG published", not "use zero".
    #[test]
    fn a_disabled_override_yields_no_sample() {
        let o = StormMotionOverride::default();
        assert!(!o.enabled, "the RPG's own SCIT average is the default");
        assert!(o.sample().is_none());
        let on = StormMotionOverride { enabled: true, ..o };
        let s = on.sample().expect("enabled");
        assert_eq!(s.motion.speed_kt, o.speed_kt);
        assert_eq!(s.motion.direction_deg, o.direction_deg);
        assert!(!s.motion.is_scit_average, "a typed vector is not the RPG's");
    }

    /// `DragValue` parses "nan" and "inf", and `f32::clamp` propagates NaN.
    /// A NaN reaching the dispatcher renders an all-NaN field *and*, because
    /// `NaN != NaN`, makes its change detector fire every frame — an unbounded
    /// re-render of every storm-relative pane that never settles.
    #[test]
    fn a_non_finite_override_is_refused_rather_than_propagated() {
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let speed = StormMotionOverride {
                enabled: true,
                speed_kt: bad,
                direction_deg: 240.0,
            };
            assert!(speed.sample().is_none(), "speed {bad}");
            let dir = StormMotionOverride {
                enabled: true,
                speed_kt: 30.0,
                direction_deg: bad,
            };
            assert!(dir.sample().is_none(), "direction {bad}");
        }
        // The counterweight: ordinary values still pass, so "reject everything"
        // is not how the test above is satisfied.
        let ok = StormMotionOverride {
            enabled: true,
            speed_kt: 0.0,
            direction_deg: 0.0,
        };
        assert!(ok.sample().is_some(), "zero is a legitimate vector");
    }

    /// Two equal overrides must produce equal samples, or the dispatcher's
    /// change detector re-renders every frame even without a NaN.
    #[test]
    fn equal_overrides_produce_equal_samples() {
        let a = StormMotionOverride {
            enabled: true,
            speed_kt: 31.5,
            direction_deg: 287.5,
        };
        let b = a;
        assert_eq!(a.sample(), b.sample());
        let c = StormMotionOverride {
            speed_kt: 31.6,
            ..a
        };
        assert_ne!(a.sample(), c.sample());
    }

    /// The widget's ceiling is the one `DERIVED_OFFSET` was sized against. If
    /// this drifts upward, the worst-case derived value starts clamping and
    /// paints as data at the clamp instead of at its real magnitude.
    #[test]
    fn the_speed_ceiling_is_the_one_the_encoding_was_sized_for() {
        assert_eq!(rustdar_radar::srm::MAX_OVERRIDE_SPEED_KT, 200.0);
    }
}

#[cfg(test)]
mod pane_slice_tests {
    use super::*;

    /// Splitting to fewer panes leaves the extra `PaneState`s in the vector so a
    /// re-split can restore them. They are not drawn and not updated, so the
    /// "every pane" slice must stop at the layout's count — otherwise a polled
    /// scan appends loop frames to panes nobody is looking at.
    #[test]
    fn the_pane_slices_stop_at_the_visible_count() {
        let mut gui = Gui::new();
        gui.set_pane_count_for_test(4);
        for (idx, pane) in gui.panes_mut().iter_mut().enumerate() {
            pane.site = format!("PANE{idx}");
        }

        // Split back down: panes 2 and 3 are remembered but no longer shown.
        gui.set_pane_count_for_test(2);

        assert_eq!(gui.panes().len(), 2);
        assert_eq!(gui.panes_mut().len(), 2);
        assert_eq!(
            gui.panes()
                .iter()
                .map(|p| p.site.as_str())
                .collect::<Vec<_>>(),
            ["PANE0", "PANE1"],
        );
        assert_eq!(
            gui.pane(3).map(|p| p.site.as_str()),
            Some("PANE3"),
            "precondition: the hidden pane is still there to be reached by index"
        );
    }

    /// The count and the vector are kept in step by every path that changes the
    /// layout, but slicing past the end would panic, and no pane update is worth
    /// a crash.
    #[test]
    fn the_pane_slices_never_outrun_the_vector() {
        let mut gui = Gui::new();
        assert_eq!(gui.panes().len(), 1, "a fresh Gui has one pane");
        // A layout claiming more panes than the vector holds, as a config whose
        // pane_count ran ahead of its pane list would leave it.
        gui.claim_pane_count_for_test(4);

        assert_eq!(gui.panes().len(), 1);
        assert_eq!(gui.panes_mut().len(), 1);
    }

    /// The rects a test clicks are the rects the frame drew, so the helper that
    /// produces them takes the visible slice's bound too. With the raw count it
    /// handed back a rect per *claimed* pane, and a test clicking the last of them
    /// would have been driving a pane no frame ever rendered.
    #[test]
    fn the_pane_rects_a_test_sees_are_only_the_ones_a_frame_drew() {
        let mut gui = Gui::new();
        gui.set_pane_count_for_test(2);
        gui.last_map_panel_rect =
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        assert_eq!(
            gui.pane_rects_for_test().len(),
            2,
            "precondition: two real panes give two rects"
        );

        gui.claim_pane_count_for_test(4);

        assert_eq!(gui.pane_rects_for_test().len(), 2);
    }

    /// `sync_viewports` reads and writes panes by raw index, so it takes its
    /// bound from the visible slice rather than the layout's claim — with the
    /// raw count, the same ran-ahead layout as above panicked mid-frame.
    #[test]
    fn viewport_sync_never_outruns_the_pane_vector() {
        let mut gui = Gui::new();
        gui.set_pane_count_for_test(2);
        gui.viewport_sync = true;
        gui.claim_pane_count_for_test(4);

        // Snapshots sized to the layout's claim, exactly as `render_panes` would
        // have taken them had it trusted the raw count too. All-zero zooms
        // make every pane look interacted, so the source scan runs as deep as
        // its bound allows.
        gui.sync_viewports(&[0.0; 4], &[None; 4]);

        // The panes that are really there still synced to a common zoom.
        assert_eq!(
            gui.pane(0).unwrap().map_memory.zoom(),
            gui.pane(1).unwrap().map_memory.zoom(),
        );
    }

    /// A pane conversion asked for during the UI pass lands on the **real** pane,
    /// not on the placeholder standing in for it.
    ///
    /// This pins the write half of the `mem::take` hazard: the thing the type
    /// system cannot help with. Two production paths hold a `PaneState` out of the
    /// vector for a whole pass — `render_layers_panel` takes the active pane,
    /// `render_panes` takes each pane in turn — leaving a default `PaneState` in
    /// the slot. Inside either window the obvious implementation of the toggle's
    /// arm,
    ///
    /// ```ignore
    /// self.panes[self.active_pane].set_kind(kind);
    /// ```
    ///
    /// writes the *placeholder*, and the line that puts the real pane back discards
    /// it: no panic, no warning, and a control that will not stay set.
    ///
    /// # This test builds the window itself, because no caller currently provides
    /// one
    ///
    /// Read the `std::mem::take` below as the load-bearing part of the fixture
    /// rather than as scene-setting. Today's menu dispatch is **outside** both
    /// windows — `render_layers_panel` restores the pane at `ui_chrome.rs:425` and
    /// dispatches at `:438`, and `render_menu_bar_panel` takes no pane — so a
    /// direct write from `apply_menu_event` would pass every behavioural test in
    /// the suite, this one included, if this one did not hold the pane out by hand.
    ///
    /// That makes this a test of the *mechanism* and not of user-visible
    /// behaviour, which is a thing worth saying out loud: it is here because
    /// WP-G's writers run inside `render_panes`' take, where the same direct write
    /// is silently discarded, and a test written after that code would be a test
    /// written after the bug. Driven through `apply_menu_event` rather than
    /// `request_pane_kind` so it covers the arm and the deferral together. The
    /// end-to-end behavioural version, which passes either way, is
    /// `converting_the_active_pane_from_the_drawer_makes_it_a_volume_pane`.
    #[test]
    fn a_pane_kind_request_survives_the_pane_being_held_out_of_the_vector() {
        use super::ui_menu::{MenuEvent, MenuToggle};
        use crate::pane::PaneKind;

        let mut gui = Gui::new();
        gui.set_pane_count_for_test(2);
        gui.active_pane = 1;
        assert_eq!(
            gui.pane(1).unwrap().kind(),
            PaneKind::Map,
            "precondition: the pane starts as a map"
        );
        // Something on the real pane that the placeholder does not have, so the
        // restore below can be shown to have really put the original back rather
        // than to have left a default in place.
        gui.pane_mut(1).unwrap().site = "KDDC".to_owned();

        let held = std::mem::take(&mut gui.panes[gui.active_pane]);
        assert_eq!(
            gui.panes[1].site, "KTLX",
            "precondition: the slot now holds a default PaneState, which is what \
             makes a direct write vanish"
        );

        let mut actions = Vec::new();
        gui.apply_menu_event(
            MenuEvent::Toggled(MenuToggle::VolumePane, true),
            &mut actions,
        );

        // The restore, which throws the placeholder away.
        gui.panes[gui.active_pane] = held;
        gui.apply_pending_pane_kind(&mut Vec::new());

        assert_eq!(
            gui.pane(1).unwrap().site,
            "KDDC",
            "precondition: the original pane must be the one back in the slot"
        );
        assert_eq!(
            gui.pane(1).unwrap().kind(),
            PaneKind::Volume,
            "the conversion was written to the pane that was held out and thrown \
             away, so the menu item silently did nothing"
        );
        assert_eq!(
            gui.pending_pane_kind_for_test(),
            None,
            "the request must be consumed, or every later frame re-converts the \
             pane and any per-kind state it gathers is discarded each time"
        );
        assert_eq!(
            gui.pane(0).unwrap().kind(),
            PaneKind::Map,
            "the request converted a pane other than the one it named"
        );
    }

    /// A request naming a pane the layout no longer has is dropped, not clamped.
    ///
    /// Reachable in one frame: the pane picker can shrink the layout after the
    /// menu event was recorded. Converting whichever pane happens to be at a
    /// nearby index would convert one the user never pointed at.
    #[test]
    fn a_pane_kind_request_for_a_pane_that_is_gone_converts_nothing() {
        use crate::pane::PaneKind;

        let mut gui = Gui::new();
        gui.request_pane_kind(7, PaneKind::Volume);
        gui.apply_pending_pane_kind(&mut Vec::new());

        assert_eq!(gui.pane(0).unwrap().kind(), PaneKind::Map);
        assert_eq!(gui.pending_pane_kind_for_test(), None);
    }

    /// A line for the target rule to place, and the pane it was drawn on.
    fn drawn_line() -> crate::pane::SectionLine {
        crate::pane::SectionLine::new(
            crate::pane::GeoPoint {
                lat: 35.0,
                lon: -97.8,
            },
            crate::pane::GeoPoint {
                lat: 35.6,
                lon: -96.9,
            },
        )
        .expect("a fixture line must be finite and have two distinct ends")
    }

    /// A cut of the right shape and no content, so a fixture can hold a picture
    /// for a retarget to throw away.
    ///
    /// Full size — `from_parts` refuses anything else, because a mis-shaped
    /// section reaches `ColorImage::from_rgba_unmultiplied`'s `assert_eq!` on
    /// the main thread. `NoCoverage` everywhere, which is what an empty volume
    /// really renders as.
    fn blank_section() -> rustdar_radar::xsect::CrossSection {
        use rustdar_radar::sampler::SampleStatus;
        use rustdar_radar::xsect::{SECTION_HEIGHT, SECTION_WIDTH, SectionAxes};
        let pixels = SECTION_WIDTH * SECTION_HEIGHT;
        rustdar_radar::xsect::CrossSection::from_parts(
            vec![0u8; pixels * 4],
            vec![f32::NAN; pixels],
            vec![SampleStatus::NoCoverage.wire_code(); pixels],
            SectionAxes {
                length_km: 100.0,
                base_km_msl: 0.4,
                top_km_msl: 20.4,
                near_ground_range_km: 10.0,
                far_ground_range_km: 110.0,
                coverage_ground_range_km: 0.0,
                cone_of_silence_km: 0.0,
                tilt_count: 1,
                widest_tilt_gap_deg: 0.0,
            },
        )
        .expect("a full-size, all-NoCoverage section is well formed")
    }

    /// A second line, distinguishable from [`drawn_line`], for a section that
    /// belongs to another map and must be left alone.
    fn other_line() -> crate::pane::SectionLine {
        crate::pane::SectionLine::new(
            crate::pane::GeoPoint {
                lat: 40.0,
                lon: -100.0,
            },
            crate::pane::GeoPoint {
                lat: 41.0,
                lon: -99.0,
            },
        )
        .expect("a fixture line must be finite and have two distinct ends")
    }

    fn wide(count: usize) -> Gui {
        let mut gui = Gui::new();
        gui.layout.width = crate::ui_layout::WidthClass::Expanded;
        gui.set_pane_count_for_test(count);
        gui
    }

    /// Step 1: a second line on the same map re-aims the section already cut
    /// from it, rather than filling the screen with panes nobody asked for.
    #[test]
    fn a_second_line_on_one_map_re_aims_the_section_it_already_feeds() {
        let mut gui = wide(2);
        gui.panes[1].set_kind(crate::pane::PaneKind::CrossSection);
        gui.panes[1].cross_section_mut().unwrap().source_pane = Some(0);
        let before = gui.pane_count();

        gui.pending_section_line = Some((0, drawn_line()));
        gui.apply_pending_section_line();

        assert_eq!(gui.pane_count(), before, "the layout grew for a re-aim");
        assert_eq!(
            gui.pane(1).unwrap().cross_section().unwrap().line,
            Some(drawn_line())
        );
    }

    /// Step 2: with no section fed by *this* map, the layout grows — even when
    /// another map's section is sitting right there.
    ///
    /// The pane count is the load-bearing assertion, and the second half of the
    /// fixture is what makes it one: a section pane exists, but it belongs to
    /// pane 1, and stealing it would silently re-aim a picture the user is
    /// still using. Only once the layout cannot grow (the test below) is that
    /// the right answer.
    #[test]
    fn a_line_with_nowhere_to_go_grows_the_layout_rather_than_taking_a_map() {
        let mut gui = wide(1);

        gui.pending_section_line = Some((0, drawn_line()));
        gui.apply_pending_section_line();

        assert_eq!(gui.pane_count(), 2, "the layout did not grow");
        assert_eq!(
            gui.pane(0).unwrap().kind(),
            crate::pane::PaneKind::Map,
            "the map survived"
        );
        assert_eq!(
            gui.pane(1).unwrap().kind(),
            crate::pane::PaneKind::CrossSection
        );
        assert_eq!(
            gui.pane(1).unwrap().cross_section().unwrap().source_pane,
            Some(0),
            "the section must remember its map, or the next line converts \
             another pane instead of re-aiming this one"
        );
        assert_eq!(
            gui.active_pane, 1,
            "the pane the user just asked for is not the one they are looking at"
        );

        // The same, with another map's section already on screen and room still
        // to grow. Growing must still win: re-aiming pane 2 would throw away a
        // picture pane 1 is still using, silently.
        let mut gui = wide(3);
        gui.panes[2].set_kind(crate::pane::PaneKind::CrossSection);
        gui.panes[2].cross_section_mut().unwrap().source_pane = Some(1);
        gui.panes[2].cross_section_mut().unwrap().line = Some(other_line());
        gui.pending_section_line = Some((0, drawn_line()));
        gui.apply_pending_section_line();

        assert_eq!(
            gui.pane_count(),
            4,
            "the layout had room and did not use it: another map's section was \
             taken instead"
        );
        assert_eq!(
            gui.pane(2).unwrap().cross_section().unwrap().line,
            Some(other_line()),
            "pane 1's section was re-aimed at a line drawn on pane 0"
        );
        assert_eq!(
            gui.pane(3).unwrap().cross_section().unwrap().line,
            Some(drawn_line())
        );
    }

    /// Steps 3 and 4: a full layout re-aims the lowest section before it
    /// converts any map, and converts the *highest* map rather than the one
    /// under the line.
    #[test]
    fn a_full_layout_re_aims_a_section_before_it_takes_a_map() {
        let full = crate::ui_layout::WidthClass::Expanded.max_panes();

        // Step 3: a section exists somewhere, aimed from another map.
        let mut gui = wide(full);
        gui.panes[2].set_kind(crate::pane::PaneKind::CrossSection);
        gui.panes[2].cross_section_mut().unwrap().source_pane = Some(1);
        gui.pending_section_line = Some((0, drawn_line()));
        gui.apply_pending_section_line();
        assert_eq!(gui.pane_count(), full, "a full layout cannot grow");
        assert_eq!(
            gui.pane(2).unwrap().cross_section().unwrap().source_pane,
            Some(0),
            "the existing section should have been re-aimed and re-sourced"
        );
        assert!(
            (0..full)
                .filter(|&i| gui.pane(i).unwrap().kind() == crate::pane::PaneKind::Map)
                .count()
                == full - 1,
            "a map was converted while a section was there to re-aim"
        );

        // Step 4: no section anywhere. The highest-indexed pane converts, and
        // the map the line was drawn on is left alone.
        let mut gui = wide(full);
        gui.pending_section_line = Some((0, drawn_line()));
        gui.apply_pending_section_line();
        assert_eq!(
            gui.pane(0).unwrap().kind(),
            crate::pane::PaneKind::Map,
            "the map under the line was taken"
        );
        assert_eq!(
            gui.pane(full - 1).unwrap().kind(),
            crate::pane::PaneKind::CrossSection
        );
    }

    /// The rule is **total**: a drawn line always lands somewhere, at every
    /// pane count either width class can reach.
    ///
    /// The one that most needs saying is a compact layout already at its own
    /// ceiling — a phone that has split as far as it is allowed to. There, every
    /// earlier step has failed and the only answer left is to convert a map. A
    /// silent no-op is the failure this is written against: a drag that produced
    /// nothing, with nothing on screen to explain it, after the user had gone to
    /// the menu to arm a mode.
    ///
    /// **What is not covered, and cannot be.** The final `unwrap_or(source)` —
    /// converting the pane drawn on — needs `max_panes() == 1`, and no
    /// [`WidthClass`](crate::ui_layout::WidthClass) reports that: `Compact` is 4
    /// and the others 6. It is unreachable today and stays because
    /// `highest_pane_other_than` returning `None` must mean *something* other
    /// than dropping the line.
    #[test]
    fn a_drawn_line_lands_somewhere_at_every_reachable_pane_count() {
        use crate::ui_layout::WidthClass;
        for width in [WidthClass::Compact, WidthClass::Expanded] {
            for count in 1..=width.max_panes() {
                let mut gui = Gui::new();
                gui.layout.width = width;
                gui.set_pane_count_for_test(count);

                gui.pending_section_line = Some((0, drawn_line()));
                gui.apply_pending_section_line();

                let sections = gui
                    .panes()
                    .iter()
                    .filter(|p| p.kind() == crate::pane::PaneKind::CrossSection)
                    .count();
                assert_eq!(
                    sections, 1,
                    "{width:?} with {count} panes placed {sections} sections for one line"
                );
                assert_eq!(
                    gui.pane(0).unwrap().kind(),
                    crate::pane::PaneKind::Map,
                    "{width:?} with {count} panes took the map the line was drawn on"
                );
                // Grown while it could, and only converted once it could not.
                let expected = (count + 1).min(width.max_panes());
                assert_eq!(
                    gui.pane_count(),
                    expected,
                    "{width:?} with {count} panes should have ended at {expected}"
                );
            }
        }
    }

    /// The section a line lands in adopts the drawing map's site and moment, and
    /// throws away the picture it was showing.
    ///
    /// A section is cut from a *site's* volume, so a target pane that kept its
    /// own site would cut the line's ground out of the wrong radar — a picture
    /// that renders perfectly and means nothing. Clearing the old raster matters
    /// for the interval before the new cut lands: a section of the previous line
    /// left on screen is of ground the user is no longer pointing at.
    #[test]
    fn a_retargeted_section_takes_the_maps_site_and_drops_the_old_picture() {
        let ctx = egui::Context::default();
        let mut gui = wide(2);
        gui.panes[0].site = "KTLX".to_owned();
        gui.panes[0].selected_product = RadarProduct::Velocity;
        gui.panes[1].site = "KINX".to_owned();
        gui.panes[1].set_kind(crate::pane::PaneKind::CrossSection);
        {
            let section = gui.panes[1].cross_section_mut().unwrap();
            section.source_pane = Some(0);
            section.unavailable = Some(crate::pane::SectionUnavailable::RenderFailed);
            // A picture and a key for the *previous* line, which is the state a
            // retarget has to clear. Without them in the fixture both fields are
            // `None` before and after, and the assertions below hold for a build
            // that clears neither — the exact shape of test that looks like it
            // is watching something and is not. (Found by mutation: dropping
            // both clears survived until this fixture had something to drop.)
            section.rendered_for = Some(crate::pane::SectionTarget {
                volume: crate::pane::VolumeStamp {
                    site: "KINX".to_owned(),
                    collected: chrono::NaiveDate::from_ymd_opt(2026, 7, 30)
                        .unwrap()
                        .and_hms_opt(18, 30, 0)
                        .unwrap(),
                },
                product: RadarProduct::Reflectivity,
                line: other_line(),
                sweeps: 9,
            });
            section.section = Some(std::sync::Arc::new(blank_section()));
            // And the raster, which needs a `Context` and is the reason the
            // first repair of this fixture stopped at `section`. Without it,
            // deleting `section.texture = None` from the retarget passes: the
            // pane would go on painting the *previous* line's picture, with the
            // new line's caption over it, for as long as the re-cut takes.
            section.texture = Some(ctx.load_texture(
                "retarget-fixture",
                egui::ColorImage::filled([1, 1], egui::Color32::WHITE),
                egui::TextureOptions::NEAREST,
            ));
        }

        gui.pending_section_line = Some((0, drawn_line()));
        gui.apply_pending_section_line();

        let pane = gui.pane(1).unwrap();
        assert_eq!(pane.site, "KTLX");
        assert_eq!(pane.selected_product, RadarProduct::Velocity);
        let section = pane.cross_section().unwrap();
        assert_eq!(section.line, Some(drawn_line()));
        assert!(
            section.section.is_none(),
            "the previous line's cut is still what a hover reads"
        );
        assert!(
            section.texture.is_none(),
            "the previous line's picture is still on screen under the new line's \
             caption"
        );
        assert_eq!(
            section.rendered_for, None,
            "a stale key would stop the dispatcher ever cutting the new line"
        );
        assert_eq!(
            section.unavailable, None,
            "a reason from the previous line outlived its cause"
        );
    }

    /// Escape and Android's back cancel the armed draw — last, below every
    /// painted layer, because it is a mode rather than something on screen.
    ///
    /// Being in the chain at all is what stops the back button from exiting the
    /// app while a mode is on, which is the reading of a back press least likely
    /// to be what was meant.
    #[test]
    fn a_back_press_cancels_an_armed_draw_after_it_has_closed_every_layer() {
        let mut gui = Gui::new();
        gui.set_section_draw_armed(true);
        gui.drawer_open = true;

        assert!(gui.dismiss_top_layer(), "the drawer was open");
        assert!(
            gui.section_draw_armed(),
            "closing the drawer must not also disarm: one layer per press"
        );
        assert!(gui.dismiss_top_layer(), "the mode was armed");
        assert!(!gui.section_draw_armed());
        assert!(
            !gui.dismiss_top_layer(),
            "with nothing left, a back press is a request to leave the app"
        );
    }

    /// Converting a pane keeps everything it was looking at, and tears down the
    /// one thing a non-map pane cannot have: a running animation loop.
    ///
    /// The root fix for a family of eight consumers with one cause. A loop left
    /// running on a pane nothing renders frames for is not idle: it blocks every
    /// *other* pane's loop through `sync_loop_playback_start`'s all-or-nothing
    /// rule, keeps `Gui::any_loop_active` true so the event loop wakes at loop
    /// frame rate, reads "Rendering n/m" for ever with no transport drawn to
    /// cancel it, and goes on spending the shared download budget. Enforced at the
    /// transition so the state is not representable, rather than filtered at each
    /// consumer. `SwitchRadarSite` resets `loop_state` for the same reason.
    ///
    /// The counterweight matters as much: every *other* field must survive, which
    /// is the promise `set_kind` exists to make.
    #[test]
    fn converting_a_pane_tears_down_its_loop_and_nothing_else() {
        use crate::pane::{LoopPhase, PaneKind};

        for kind in [PaneKind::CrossSection, PaneKind::Volume] {
            let mut gui = Gui::new();
            {
                let pane = gui.pane_mut(0).unwrap();
                pane.site = "KDDC".to_owned();
                pane.selected_product = RadarProduct::Velocity;
                pane.selected_elevation = 1.5;
                pane.viewing_live = false;
                pane.time_step_secs = 1800;
                pane.loop_state.phase = LoopPhase::Playing;
                assert!(
                    pane.loop_state.is_active(),
                    "precondition: the loop must be running, or there is nothing \
                     to tear down"
                );
            }

            gui.pane_mut(0).unwrap().set_kind(kind);

            let pane = gui.pane(0).unwrap();
            assert!(
                !pane.loop_state.is_active(),
                "{kind:?}: the loop survived, so it will hold every other pane's \
                 loop back and never finish"
            );
            assert_eq!(pane.site, "KDDC", "{kind:?}: the site went with the loop");
            assert_eq!(pane.selected_product, RadarProduct::Velocity);
            assert_eq!(pane.selected_elevation, 1.5);
            assert!(!pane.viewing_live);
            assert_eq!(pane.time_step_secs, 1800);

            // …and converting back does not resurrect it. A torn-down loop is torn
            // down; re-enabling it is the transport's job.
            gui.pane_mut(0).unwrap().set_kind(PaneKind::Map);
            assert!(!gui.pane(0).unwrap().loop_state.is_active());
        }
    }

    /// Overlay auto-poll and the pane a fetch is attributed to both skip panes
    /// with no map, while the panes keep their layer toggles.
    ///
    /// Both questions are "is this overlay being *drawn* anywhere?", and every
    /// overlay is a layer over map tiles positioned against a projector a non-map
    /// pane does not have — so a converted pane must not keep an auto-poll timer
    /// alive or be handed a `FetchOverlay`.
    ///
    /// `enabled_overlays` is deliberately *not* cleared, which is the second half
    /// here: it is the user's remembered answer to "which layers do I want", it
    /// becomes meaningful again the moment the pane converts back, and it is the
    /// same choice `set_kind` makes about the viewport and the tilt.
    #[test]
    fn overlay_polling_skips_panes_with_no_map_but_keeps_their_toggles() {
        use crate::pane::PaneKind;

        let kind = OverlayKind::CityLabels;
        let mut gui = Gui::new();
        gui.set_pane_count_for_test(2);
        for idx in 0..2 {
            gui.pane_mut(idx)
                .unwrap()
                .enabled_overlays
                .insert(kind, true);
        }
        assert!(
            gui.any_pane_has_overlay_enabled(kind),
            "precondition: two map panes want the layer"
        );
        assert_eq!(gui.first_pane_with_overlay_enabled(kind), Some(0));

        gui.pane_mut(0).unwrap().set_kind(PaneKind::Volume);
        assert_eq!(
            gui.first_pane_with_overlay_enabled(kind),
            Some(1),
            "a fetch was attributed to a pane that cannot draw the overlay"
        );
        assert!(gui.any_pane_has_overlay_enabled(kind));

        gui.pane_mut(1).unwrap().set_kind(PaneKind::CrossSection);
        assert!(
            !gui.any_pane_has_overlay_enabled(kind),
            "no pane on screen can draw this overlay, yet its auto-poll timer is \
             still being kept alive"
        );
        assert_eq!(gui.first_pane_with_overlay_enabled(kind), None);

        // The toggles themselves are untouched, so converting back restores the
        // layer rather than losing the user's choice.
        for idx in 0..2 {
            assert!(
                gui.pane(idx).unwrap().is_overlay_enabled(kind),
                "pane {idx} lost its remembered layer choice"
            );
        }
        gui.pane_mut(0).unwrap().set_kind(PaneKind::Map);
        assert_eq!(gui.first_pane_with_overlay_enabled(kind), Some(0));
    }

    /// A pane with no map neither drives the shared viewport nor follows it.
    ///
    /// This is the all-panes site that goes live the instant a non-map pane can
    /// exist, and it fails in the direction that looks like a bug in the *other*
    /// panes. `render_panes` hands the active pane's `map_memory` to
    /// `InteractionState::resolve_active` whatever kind the pane is, and on the
    /// touch path `TouchGestures::update` writes a zoom into it — so a
    /// double-tap-drag on a section pane moves a viewport nothing draws.
    /// Unfiltered, `sync_viewports` then reads that pane as the **source**,
    /// because it is the first whose zoom moved, and re-centres and re-zooms
    /// every map pane on screen. `viewport_sync` defaults *on*, so this is the
    /// shipped default rather than something a user opts into.
    ///
    /// Both directions are asserted, and each one fails on its own: the source
    /// scan skipping non-map panes, and the write loop skipping them. The second
    /// matters because a converted pane's viewport is what it comes back to —
    /// `a_converted_pane_keeps_its_site_and_viewport` is the promise — and it is
    /// persisted per pane.
    #[test]
    fn a_pane_with_no_map_neither_drives_nor_follows_the_shared_viewport() {
        use crate::pane::PaneKind;

        // Zoom 4.0 is `DEFAULT_PANE_ZOOM`; 4.0 +/- 2.0 is well inside walkers'
        // accepted range, so `set_zoom` below cannot silently clamp and turn a
        // real move into no move at all.
        let moved_to = 6.0;
        let untouched = 4.0;

        let mut gui = Gui::new();
        gui.set_pane_count_for_test(3);
        gui.viewport_sync = true;
        gui.pane_mut(1).unwrap().set_kind(PaneKind::CrossSection);
        for idx in 0..3 {
            assert_eq!(
                gui.pane(idx).unwrap().map_memory.zoom(),
                untouched,
                "precondition: every pane starts at the same zoom"
            );
        }

        // The gesture: the *section* pane's viewport moved and nobody else's,
        // exactly as a double-tap-drag on it leaves things.
        gui.pane_mut(1)
            .unwrap()
            .map_memory
            .set_zoom(moved_to)
            .expect("precondition: the test zoom must be in range");
        assert_eq!(
            gui.pane(1).unwrap().map_memory.zoom(),
            moved_to,
            "precondition: walkers clamped the test zoom, so nothing moved"
        );

        gui.sync_viewports(&[untouched; 3], &[None; 3]);

        assert_eq!(
            (0..3)
                .map(|idx| gui.pane(idx).unwrap().map_memory.zoom())
                .collect::<Vec<_>>(),
            vec![untouched, moved_to, untouched],
            "a gesture on a pane with no map re-zoomed the map panes to it"
        );

        // The same pane as the *target*: now a map pane moves, and the section
        // pane must not be dragged along with the other map.
        gui.pane_mut(0)
            .unwrap()
            .map_memory
            .set_zoom(7.0)
            .expect("in range");
        gui.sync_viewports(&[untouched, moved_to, untouched], &[None; 3]);
        assert_eq!(
            (0..3)
                .map(|idx| gui.pane(idx).unwrap().map_memory.zoom())
                .collect::<Vec<_>>(),
            vec![7.0, moved_to, 7.0],
            "the section pane's own viewport was overwritten by the sync"
        );
    }

    /// With nothing moved and a non-map pane active, there is no source at all.
    ///
    /// The fallback used to be `source_idx.unwrap_or(self.active_pane)`, which
    /// made a non-map active pane the source on *every* frame — the same failure
    /// as the source scan, reached with no interaction whatsoever, and therefore
    /// the more likely of the two to be seen.
    #[test]
    fn a_non_map_active_pane_is_not_the_fallback_sync_source() {
        use crate::pane::PaneKind;

        let mut gui = Gui::new();
        gui.set_pane_count_for_test(2);
        gui.viewport_sync = true;
        gui.pane_mut(1).unwrap().set_kind(PaneKind::Volume);
        gui.active_pane = 1;

        // Deliberately out of step with pane 0, and deliberately *not* reported
        // as moved: `pre_zooms` says nothing changed this frame, so the only way
        // this value can escape is through the no-source fallback.
        gui.pane_mut(1)
            .unwrap()
            .map_memory
            .set_zoom(9.0)
            .expect("in range");

        gui.sync_viewports(&[4.0, 9.0], &[None; 2]);

        assert_eq!(
            gui.pane(0).unwrap().map_memory.zoom(),
            4.0,
            "the active pane has no map, so its viewport propagated to a map \
             pane that nothing had interacted with"
        );
    }

    /// Loop actions never target a pane that draws no plan-view frames.
    ///
    /// A loop frame *is* a rendered plan-view tilt, and
    /// `App::dispatch_loop_renders` skips panes with no plan view — so a
    /// non-map pane in this list would be put into `is_active()` with a frame
    /// list nothing ever fills: a loop transport stuck at "waiting", and a
    /// download queue fetching volumes for a pane nobody is looking at.
    ///
    /// The active pane is included without being asked, which the second half
    /// below pins. This runs from `render_loop_controls`, inside the layers
    /// panel's `mem::take` window, where `self.panes[self.active_pane]` is a
    /// default `PaneState` and therefore reads as a *map* whatever the real pane
    /// is — so testing it would be testing the placeholder.
    #[test]
    fn loop_actions_skip_panes_that_draw_no_frames() {
        use crate::pane::PaneKind;

        let mut gui = Gui::new();
        gui.set_pane_count_for_test(4);
        gui.sync_layers = true;
        gui.pane_mut(1).unwrap().set_kind(PaneKind::CrossSection);
        gui.pane_mut(2).unwrap().set_kind(PaneKind::Volume);

        assert_eq!(gui.loop_sync_targets(), vec![0, 3]);

        // Sync off narrows to the active pane, whatever kind it is: it is the
        // pane whose own checkbox was clicked.
        gui.sync_layers = false;
        gui.active_pane = 2;
        assert_eq!(gui.loop_sync_targets(), vec![2]);

        // And with sync back on, the active pane is still in the list even
        // though its slot says it is not a map — because the index is included
        // rather than tested.
        gui.sync_layers = true;
        assert_eq!(gui.loop_sync_targets(), vec![0, 2, 3]);
    }

    /// The graphics-state reset reaches panes of every kind, including the ones
    /// the layout is not currently showing.
    ///
    /// [`Gui::clear_graphics_state`] is the only place a pane-held
    /// `egui::TextureHandle` is released when the egui context dies, and
    /// `PaneContent::release_textures` is called from inside this same loop —
    /// so if the loop skipped non-map panes, or stopped at the visible count,
    /// that guard would read as covered while never running. Asserted through
    /// `radar_sites_render_gen`, which the loop bumps on its way past: it is a
    /// side effect of *this* loop body, so it cannot agree with a loop that
    /// stopped short.
    ///
    /// Hidden panes are included deliberately. A handle belonging to a pane the
    /// user split away from is just as invalid once the context is gone, and a
    /// re-split would hand it straight back to the renderer.
    #[test]
    fn clearing_graphics_state_reaches_panes_of_every_kind() {
        use crate::pane::PaneKind;

        let mut gui = Gui::new();
        gui.set_pane_count_for_test(4);
        gui.pane_mut(1).unwrap().set_kind(PaneKind::CrossSection);
        gui.pane_mut(2).unwrap().set_kind(PaneKind::Volume);
        // Split back down, so panes 2 and 3 are remembered but not shown.
        gui.set_pane_count_for_test(2);

        let before: Vec<u64> = gui
            .panes
            .iter()
            .map(|pane| pane.radar_sites_render_gen)
            .collect();
        assert_eq!(before.len(), 4, "precondition: four panes to reach");
        assert_eq!(
            gui.panes.iter().map(|pane| pane.kind()).collect::<Vec<_>>(),
            [
                PaneKind::Map,
                PaneKind::CrossSection,
                PaneKind::Volume,
                PaneKind::Map
            ],
            "precondition: one pane of each kind, two of them hidden"
        );

        gui.clear_graphics_state();

        for (idx, was) in before.iter().enumerate() {
            assert_eq!(
                gui.panes[idx].radar_sites_render_gen,
                was + 1,
                "pane {idx} ({:?}) was not reached by the graphics-state reset, \
                 so nothing released whatever its kind is holding",
                gui.panes[idx].kind(),
            );
        }
    }
}

#[cfg(test)]
mod chunk_scan_info_tests {
    use super::*;
    use rustdar_radar::sites::RadarSite;
    use rustdar_radar::types::RadarProduct;
    use std::collections::HashMap;

    fn site() -> RadarSite {
        RadarSite {
            name: "KTLX",
            lat: 35.3,
            lon: -97.3,
            elev: None,
        }
    }

    fn at(minute: u32) -> chrono::NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(2026, 7, 28)
            .unwrap()
            .and_hms_opt(18, minute, 0)
            .unwrap()
    }

    fn info(minute: u32, products: &[(RadarProduct, &[f32])]) -> ScanInfo {
        let mut product_elevations = HashMap::new();
        for (product, angles) in products {
            product_elevations.insert(*product, angles.to_vec());
        }
        ScanInfo {
            site: site(),
            timestamp: at(minute),
            vcp_number: 212,
            available_products: products.iter().map(|(p, _)| *p).collect(),
            product_elevations,
            status: format!("minute {minute}"),
        }
    }

    fn gui_with(existing: ScanInfo) -> Gui {
        let mut gui = Gui::new();
        let pane = gui.pane_mut(0).expect("a fresh Gui has one pane");
        pane.site = "KTLX".to_string();
        pane.scan_info = Some(existing);
        gui
    }

    /// The mutation this kills: replacing `product_elevations` wholesale. A
    /// volume still being assembled knows only the cuts that have completed, so
    /// a replace would shrink the picker to one entry every few seconds and let
    /// it regrow — and `get_rendering_params` snaps to the nearest *listed*
    /// angle, so every pane would walk up the VCP once per volume.
    #[test]
    fn a_partial_volume_does_not_shrink_the_tilt_list() {
        let full = info(
            0,
            &[(RadarProduct::Reflectivity, &[0.5, 1.5, 2.4, 3.4, 4.3])],
        );
        let mut gui = gui_with(full);

        // The next volume has only completed its lowest cut.
        gui.apply_chunk_scan_info("KTLX", info(5, &[(RadarProduct::Reflectivity, &[0.5])]));

        let merged = gui.pane(0).unwrap().scan_info.clone().unwrap();
        assert_eq!(
            merged.product_elevations[&RadarProduct::Reflectivity],
            vec![0.5, 1.5, 2.4, 3.4, 4.3],
            "the tilt list shrank to the cuts assembled so far"
        );
        assert_eq!(
            merged.timestamp,
            at(5),
            "but the timestamp is the new volume's"
        );
        assert_eq!(merged.status, "minute 5");
    }

    /// Level III products and their elevations are accumulated into `ScanInfo`
    /// *in place* by `poll_level3_results`, and the chunk feed only refetches
    /// them when a volume closes. Replacing would freeze every L3 pane —
    /// `get_rendering_params` returns `None` with no elevations — for the rest
    /// of the volume.
    #[test]
    fn a_partial_volume_keeps_the_level3_products_already_registered() {
        let existing = info(
            0,
            &[
                (RadarProduct::Reflectivity, &[0.5, 1.5]),
                (RadarProduct::StormRelativeVelocity, &[0.5]),
            ],
        );
        let mut gui = gui_with(existing);

        gui.apply_chunk_scan_info("KTLX", info(5, &[(RadarProduct::Reflectivity, &[0.5])]));

        let merged = gui.pane(0).unwrap().scan_info.clone().unwrap();
        assert!(
            merged
                .available_products
                .contains(&RadarProduct::StormRelativeVelocity),
            "the Level III product was dropped by a Level II cut completing"
        );
        assert_eq!(
            merged.product_elevations[&RadarProduct::StormRelativeVelocity],
            vec![0.5],
            "and its tilt list with it"
        );
    }

    /// The counterweight: a tilt the assembling volume reveals for the first
    /// time still has to appear, or a new cut in a changed VCP would never be
    /// selectable.
    #[test]
    fn a_newly_seen_tilt_is_added_to_the_list() {
        let mut gui = gui_with(info(0, &[(RadarProduct::Reflectivity, &[0.5])]));
        gui.apply_chunk_scan_info(
            "KTLX",
            info(5, &[(RadarProduct::Reflectivity, &[0.5, 6.4])]),
        );
        assert_eq!(
            gui.pane(0)
                .unwrap()
                .scan_info
                .as_ref()
                .unwrap()
                .product_elevations[&RadarProduct::Reflectivity],
            vec![0.5, 6.4]
        );
    }

    /// A chunk round happens on its own every few seconds. Taking the spinner
    /// down would cancel a manual Refresh still in flight and unblock the
    /// auto-poll queued behind it; resetting the backoff would undo exactly the
    /// retreat the archive fallback depends on.
    #[test]
    fn a_chunk_update_leaves_the_fetch_spinner_and_the_backoff_alone() {
        let mut gui = gui_with(info(0, &[(RadarProduct::Reflectivity, &[0.5])]));
        gui.radar.fetching = true;
        gui.auto_poll.on_error();
        let backed_off = gui.auto_poll.interval_secs;
        assert!(backed_off > 60, "the fixture must actually be backed off");

        gui.apply_chunk_scan_info("KTLX", info(5, &[(RadarProduct::Reflectivity, &[0.5])]));

        assert!(
            gui.radar.fetching,
            "a chunk update cancelled a manual fetch's spinner"
        );
        assert_eq!(
            gui.auto_poll.interval_secs, backed_off,
            "a chunk update reset the archive poll's backoff"
        );
    }

    /// The one behaviour it does share with `set_scan_info_for_site`: with
    /// chunks feeding live mode, the first data of a session arrives here.
    #[test]
    fn the_first_chunk_volume_of_a_session_still_claims_the_initial_zoom() {
        let mut gui = gui_with(info(0, &[(RadarProduct::Reflectivity, &[0.5])]));
        gui.initial_zoom_set = false;
        gui.apply_chunk_scan_info("KTLX", info(5, &[(RadarProduct::Reflectivity, &[0.5])]));
        assert!(gui.initial_zoom_set);
    }

    /// A pane on another site is not touched.
    #[test]
    fn a_chunk_update_only_reaches_its_own_site() {
        let mut gui = gui_with(info(0, &[(RadarProduct::Reflectivity, &[0.5])]));
        gui.pane_mut(0).unwrap().site = "KOUN".to_string();
        gui.apply_chunk_scan_info("KTLX", info(5, &[(RadarProduct::Reflectivity, &[0.5])]));
        assert_eq!(
            gui.pane(0).unwrap().scan_info.as_ref().unwrap().timestamp,
            at(0)
        );
    }

    /// Only panes viewing live are fed, and each site is asked for once.
    #[test]
    fn live_sites_are_distinct_and_exclude_historic_panes() {
        let mut gui = Gui::new();
        gui.set_pane_count_for_test(3);
        for (idx, site) in ["KTLX", "KTLX", "KOUN"].iter().enumerate() {
            let pane = gui.pane_mut(idx).unwrap();
            pane.site = (*site).to_string();
            pane.viewing_live = true;
        }
        assert_eq!(gui.live_sites(), vec!["KTLX", "KOUN"]);

        gui.pane_mut(2).unwrap().viewing_live = false;
        assert_eq!(gui.live_sites(), vec!["KTLX"]);
    }
}
