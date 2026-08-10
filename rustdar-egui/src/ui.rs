use crate::actions::{GuiAction, RadarConfig};
use rustdar_overlays::render::controls::{
    ControlEffect, ControlItem, ControlUpdate, ControlValue, PaneControlContext,
    PaneControlContextMut,
};

const DEFAULT_INITIAL_ZOOM: f64 = 7.0;

use crate::pane::{ColorScaleOrientation, PaneId, PaneKind, PaneLayout, PaneState};
use crate::tiles::MapTileState;
use crate::ui_layout::{LayoutCtx, ModalityLatch};
use chrono::{NaiveDateTime, Timelike};
use egui::Context;
use rustdar_overlays::render::overlay_state::{OverlayKind, OverlayRegistry};
use rustdar_radar::types::{RadarProduct, ScanInfo};
use rustdar_units::UserPreferences;
use std::collections::HashMap;

#[path = "ui_shell.rs"]
mod shell;
#[path = "ui_stack.rs"]
mod ui_stack;
/// What the stack drew last frame, for the input harness.
#[cfg(test)]
pub(crate) use ui_stack::{StackProbe, StackRowProbe};
#[path = "ui_inspector.rs"]
mod ui_inspector;
/// What the inspector drew last frame, for the input harness.
#[cfg(test)]
pub(crate) use ui_inspector::InspectorProbe;
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
/// The region-drag arming toggle's label, for the same reason — and for one
/// more: the tests that prove the two armed drags are mutually exclusive have to
/// look both entries up by name in the same menu.
#[cfg(test)]
pub(crate) use ui_menu::REGION_ARM_LABEL;
/// The 3D-pane toggle's label, for the input harness — so the tests that look
/// the entry up by name cannot go on passing after it is renamed.
#[cfg(test)]
pub(crate) use ui_menu::VOLUME_PANE_LABEL;
#[path = "ui_map.rs"]
pub(crate) mod map;
/// The 3D block's sidebar header, for the input harness — so the test that
/// pins the sidebar's shared structure names the header the panel really
/// draws rather than keeping its own copy of it.
#[cfg(test)]
pub(crate) use map::VOLUME_SIDEBAR_HEADER;
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
#[path = "ui_statusbar.rs"]
mod statusbar;
#[path = "ui_timeline.rs"]
mod timeline;
#[cfg(test)]
pub(crate) use timeline::TimelineProbe;
#[path = "ui_topbar.rs"]
mod topbar;
/// The top bar's layout floor — margins plus one interact row — for the M8
/// breathing-room pin.
#[cfg(test)]
pub(crate) use topbar::MIN_BAR_HEIGHT;
#[path = "ui_pills.rs"]
mod pills;
/// What a pane's own top-left content leaves clear for its pill row — read
/// by the section pane's layout and the 3D pane's caption.
#[cfg(test)]
pub(crate) use pills::PILL_ROW_CLEARANCE;
/// What the pill rows and their popovers drew last frame, for the input
/// harness.
#[cfg(test)]
pub(crate) use pills::{PillKind, PillPopoverProbe, PillRowProbe};
#[path = "ui_fade.rs"]
mod fade;
#[path = "ui_sheet.rs"]
mod sheet;
/// The sheet's snap extent — `Gui` holds one as session state.
pub(crate) use sheet::SheetExtent;
/// The sheet-page projection, for the input harness — production code names
/// it through `sheet::` directly.
#[cfg(test)]
pub(crate) use sheet::SheetPage;
/// What the bottom bar, the sheet and the error toast drew last frame, for
/// the input harness.
#[cfg(test)]
pub(crate) use sheet::{BottomBarProbe, ErrorToastProbe, SheetProbe};
#[path = "ui_catalog.rs"]
mod catalog;
/// The preset shape, re-used by the config writer.
pub(crate) use catalog::PresetConfig;
/// The compiled-in presets, for the parity walk — the catalog leg's Presets
/// inventory is the table the renderer draws, not a restated name list.
#[cfg(test)]
pub(crate) use catalog::builtin_presets;
/// What the catalog drew last frame, for the input harness.
#[cfg(test)]
pub(crate) use catalog::{CatalogGroup, CatalogProbe, CatalogTileProbe};

/// The sentence the settings pane puts under a refusal, for the same reason and
/// on the same terms as the two empty states above: where a refusal is undone
/// is `cfg`'d per platform, so a harness test that spelled it out would only
/// ever pin whichever row ran it.
#[cfg(test)]
pub(crate) use settings::LOCATION_DENIED_NOTE;
/// The settings window's row table and its drawn-row probe, for the parity
/// walk — the inventory it asserts is the table the renderer iterates, so a
/// row cannot be dropped from one without the other noticing.
#[cfg(test)]
pub(crate) use settings::{DrawnSettingsRow, SETTINGS_ROWS};

use crate::ui_input::InteractionState;

/// One pane-count button the picker drew, as it was drawn. See
/// [`ui_menu::DrawnMenuLeaf`] for the same shape and the reason for it.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PaneOptionProbe {
    pub count: usize,
    pub selected: bool,
    /// Whether the button could be clicked. The top bar draws every count up
    /// to the absolute maximum and disables the ones past this width's offer,
    /// so "the picker narrows on a phone" is now a claim about this flag.
    pub enabled: bool,
    pub rect: egui::Rect,
}

/// What the top bar drew: the rects a test drives it by, and the state each
/// toggle was showing. Reported by the renderer, never rebuilt by a test —
/// see [`ui_menu::DrawnMenuLeaf`] for the pattern.
///
/// The phone-only fields (`scan_text`, `collapse`, `hover`) stay at their
/// defaults on the wider widths, exactly as the desktop-only fields (the ☰
/// button, the Layers and Inspector toggles) stay at theirs on Compact —
/// which fields are live *is* the report of which bar drew.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TopBarProbe {
    /// The rect the docked panel claimed, straight off its own response.
    pub rect: egui::Rect,
    /// The ☰ button that opens the whole-menu dropdown.
    pub menu_button: egui::Rect,
    /// The Layers toggle, and whether it read as open.
    pub layers_toggle: (egui::Rect, bool),
    /// The largest pane count offered *enabled* at this width.
    pub pane_count_max: usize,
    /// The Region arm toggle, and whether it read as armed.
    pub region_arm: (egui::Rect, bool),
    /// The X-sec arm toggle, and whether it read as armed.
    pub section_arm: (egui::Rect, bool),
    /// The ⚙ Inspector toggle, and whether it read as open.
    pub inspector_toggle: (egui::Rect, bool),
    /// The phone bar's scan summary chip text, verbatim — the short form
    /// the compact status bar used to carry.
    pub scan_text: String,
    /// The phone bar's ◧ collapse/restore button — the status bar's own
    /// collapse state, applied to this bar on Compact (contract 75).
    pub collapse: egui::Rect,
    /// Whether the phone bar hosted the hover readout this frame (contract
    /// 25: the readout follows the modality, not the width).
    pub hover: bool,
}

#[cfg(test)]
impl Default for TopBarProbe {
    fn default() -> Self {
        Self {
            rect: egui::Rect::NOTHING,
            menu_button: egui::Rect::NOTHING,
            layers_toggle: (egui::Rect::NOTHING, false),
            pane_count_max: 0,
            region_arm: (egui::Rect::NOTHING, false),
            section_arm: (egui::Rect::NOTHING, false),
            inspector_toggle: (egui::Rect::NOTHING, false),
            scan_text: String::new(),
            collapse: egui::Rect::NOTHING,
            hover: false,
        }
    }
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
    /// The auto-poll chip's rect and text, when one was drawn. The chip
    /// replaced the checkbox with the full-bleed flip; the toggle itself
    /// lives in the ☰ menu.
    pub poll_chip: Option<(egui::Rect, String)>,
    /// The refresh button's rect — always drawn, so a test can click the real
    /// button rather than restating its position.
    pub refresh: egui::Rect,
    /// The ◧ collapse button's rect — the restore button while collapsed.
    pub collapse: egui::Rect,
    /// Whether the bar was collapsed to its restore button this frame.
    pub collapsed: bool,
    /// Whether the hover readout was drawn.
    pub hover: bool,
    /// The rect the floating bar actually claimed, straight off its own
    /// response — not the bottom slice of the screen worked out a second time.
    pub rect: egui::Rect,
}

#[cfg(test)]
impl Default for StatusBarProbe {
    fn default() -> Self {
        Self {
            scan_text: String::new(),
            product_age_text: None,
            poll_chip: None,
            refresh: egui::Rect::NOTHING,
            collapse: egui::Rect::NOTHING,
            collapsed: false,
            hover: false,
            rect: egui::Rect::NOTHING,
        }
    }
}

/// What the inspector's body is about: the app's settings, the active pane's
/// own properties, or one layer's options.
///
/// One selection for every width — the session state the crumb row renders
/// and the three body arms dispatch on. `AppSettings` is the default and the
/// state `✕` deselect returns to: it is the one body that is never about the
/// active pane, so it is the one that can never be wrong about it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InspectorSelection {
    /// The app's own settings — units, location, GPS, storm motion.
    AppSettings,
    /// The active pane's properties: kind, product, tilt, sync, and the
    /// kind-specific block.
    PaneProps,
    /// One layer's options, hosted by `render_overlay_controls_one`.
    Layer(OverlayKind),
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

/// One site's current-volume stamp, as the App publishes it each frame.
///
/// Two times because a merged volume makes two distinct truthful claims and a
/// caption must not fuse them: `newest` says when the radar last looked
/// *anywhere* in the volume, and `base_started` says which complete volume
/// the un-refreshed tilts still come from. Stating only the first would imply
/// the whole volume is that fresh, which is exactly the impression the
/// honesty devices exist to refuse.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CurrentVolumeStamp {
    /// Collection time of the newest data in the merged volume — the identity
    /// a 3D pane names its build by. Every sealed sweep advances it, which is
    /// what makes the 3D view rebuild in step with the map beside it.
    pub newest: NaiveDateTime,
    /// When the complete base volume under the merge began, where one
    /// contributes at all. `None` while the site's first volume is still
    /// filling: there is no complete volume yet and the caption says so.
    pub base_started: Option<NaiveDateTime>,
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
    /// Each site's current-volume stamp, refreshed each frame by the App and
    /// advanced by every sealed sweep. A 3D pane names the volume it wants by
    /// [`CurrentVolumeStamp::newest`], which is what makes its rebuilds follow
    /// the live feed — see `App::base_scans` and `rustdar_radar::current` for
    /// what the stamp is a stamp *of*.
    current_volumes: HashMap<String, CurrentVolumeStamp>,
    time_dialog: TimeDialogState,
    initial_zoom_set: bool,
    // --- Map tiles (shared across panes) ---
    map_tiles: MapTileState,
    // User's GPS fix (full data from GPS receiver or Android LocationManager)
    user_fix: Option<rustdar_gps::GpsFix>,
    /// When [`user_fix`](Self::user_fix) arrived.
    ///
    /// Not `user_fix.timestamp`: that is the *receiver's* clock, it is absent
    /// on every source but serial NMEA, and it says when the position was
    /// measured rather than when this app last heard anything. The question the
    /// settings pane asks — "is location on but not producing?" — is about the
    /// second one.
    user_fix_at: Option<web_time::Instant>,
    /// What the OS last said about this app's access to the user's location,
    /// pushed in by the frontend's location gate.
    ///
    /// Cached rather than queried because this crate cannot see a
    /// `PlatformBridge` — it is the crate the bridge's trait depends *on* — so
    /// a copy is the only thing available here. How fresh the copy is is the
    /// gate's poll cadence, which tightens while [`Gui::settings_visible`]
    /// answers true for exactly this reason.
    location_permission: rustdar_gps::LocationPermission,
    /// Whether the platform is currently delivering location fixes. A different
    /// question from the permission: every desktop process starts granted and
    /// silent.
    location_active: bool,
    /// Whether this platform has a location settings page to offer.
    ///
    /// Pushed once at startup rather than with the two fields above, because it
    /// is a property of the build and not of the permission — it cannot change
    /// while the app runs, and nothing is served by re-asking it at the gate's
    /// cadence. `false` by default, so a bridge that has not been asked renders
    /// no button rather than one that does nothing.
    location_settings_available: bool,
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
    /// Each map pane's Mercator affine and rect, as of the last frame that
    /// drew it — the registration a 3D pane's map floor is reprojected
    /// through, and the rects the frontend clips its mirror pass to.
    ///
    /// Recorded inside `Map::show`, because that is the only place a
    /// `walkers::Projector` exists. Kept across frames rather than cleared,
    /// deliberately: a pane that is momentarily not drawn (a collapsed
    /// divider, a hidden tab) should leave its 3D pane's floor where it was
    /// rather than dropping it, and a stale entry costs six words of state.
    ///
    /// **The invariant is that a key here is a pane that is a map right now**,
    /// not merely a pane that was one when the affine was taken. Entries are
    /// pruned at the top of the pane loop against both the live pane count and
    /// the live [`crate::pane::PaneKind`], so neither a layout that sheds panes
    /// nor a map pane converted to 3D or cross-section can leave a floor
    /// reprojecting through geography nothing on screen still has.
    map_pane_geo: HashMap<usize, crate::volume_view::MapPaneGeo>,
    /// How many slippy zoom levels deeper a **floor-source** map pane should
    /// fetch its raster tiles, from the renderer's last mirror plan.
    ///
    /// Set by the frontend, which is the only side that knows how much the 3D
    /// camera is magnifying the ground and how many texels the mirror could
    /// afford — see `egui_renderer::mirror`. Zero for every pane nothing is
    /// standing on, so a layout with no 3D view fetches exactly the tiles it
    /// always did.
    floor_tile_zoom_bias: u8,
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
    /// The pane borders the last frame painted: pane index, the stroke's
    /// painted bounds, and whether it was the active highlight. Only read by
    /// tests — the M8 pin that every border lies inside its pane, at every
    /// grid position (the outside-stroke bug clipped the outer edges away).
    #[cfg(test)]
    last_pane_borders: Vec<(usize, egui::Rect, bool)>,
    /// The section tracks the last frame painted over map panes: map pane,
    /// section pane, and the painted A and B endpoints. Only read by tests —
    /// the M8 pin that the release frame of a handle drag paints the dropped
    /// geometry, never the stale pre-drag line.
    #[cfg(test)]
    last_section_tracks: Vec<(usize, usize, egui::Pos2, egui::Pos2)>,
    /// The Volume Alpha corner buttons the last frame drew, per pane. Only
    /// read by tests — the M8 pin that the fade hides pane-borne chrome too.
    #[cfg(test)]
    last_alpha_buttons: Vec<(usize, egui::Rect)>,
    /// Each map pane's dispatched kinds in paint order, with the layer each
    /// painted into. Only read by tests — the draw-order pin; see
    /// `PaneRenderCtx::paint_order` for why the layer is the honest half.
    #[cfg(test)]
    last_paint_order: Vec<(usize, Vec<(OverlayKind, egui::LayerId)>)>,
    /// What the last frame's status bar actually drew. Only read by tests.
    #[cfg(test)]
    last_status_bar: StatusBarProbe,
    /// What the last frame's timeline transport actually drew. Only read by
    /// tests.
    #[cfg(test)]
    last_timeline: TimelineProbe,
    /// What the last frame's top bar actually drew. Only read by tests.
    #[cfg(test)]
    last_top_bar: TopBarProbe,
    /// What the last frame's layer stack actually drew. Only read by tests.
    #[cfg(test)]
    last_stack: StackProbe,
    /// What the last frame's inspector actually drew. Only read by tests —
    /// see [`InspectorProbe`] for why `mode` is written inside the body arms.
    #[cfg(test)]
    last_inspector: InspectorProbe,
    /// What the last frame's Add-layer catalog actually drew. Only read by
    /// tests.
    #[cfg(test)]
    last_catalog: CatalogProbe,
    /// What the last frame's pill rows actually drew, in pane order. Only
    /// read by tests.
    #[cfg(test)]
    last_pills: Vec<pills::PillRowProbe>,
    /// The pill popover the last frame drew, if one was open. Only read by
    /// tests.
    #[cfg(test)]
    last_pill_popover: Option<pills::PillPopoverProbe>,
    /// Whether some feature consumed this frame's map click — written by the
    /// pane loop, read by [`Self::apply_fade_toggle`] (a consumed click while
    /// faded unfades; see `ui_fade.rs`) and by the harness's probe.
    click_consumed_frame: bool,
    /// How many times handler `ControlItem`s were rendered this frame.
    ///
    /// The double-render guard: each render is a load→mutate→save round trip
    /// over the active pane's `overlay_configs`, so two passes in one frame
    /// fight over the handlers' state — the entanglement the plan's §3.8
    /// makes `render_overlay_controls_one` the only host to prevent. The
    /// harness asserts ≤ 1 after every frame.
    #[cfg(test)]
    control_render_passes: u32,
    /// Every handler dropdown the last frame drew, with the text its collapsed
    /// box showed. Only read by tests — see [`DrawnDropdown`].
    #[cfg(test)]
    last_dropdowns: Vec<DrawnDropdown>,
    /// Every control item the last frame's layers panel drew, whatever its
    /// shape — the generalisation of the field above. Only read by tests; see
    /// [`DrawnControlItem`].
    #[cfg(test)]
    last_control_items: Vec<DrawnControlItem>,
    /// Every settings row the last frame's settings window drew. Only read by
    /// tests — see [`settings::DrawnSettingsRow`].
    #[cfg(test)]
    last_settings_rows: Vec<settings::DrawnSettingsRow>,
    /// The action-button indices the last frame's detail popup reported as
    /// triggered, and the ones it actually handled. Only read by tests, which
    /// hold the second to at most one entry per frame — see the note on the
    /// handling in `ui_popups.rs`.
    #[cfg(test)]
    last_popup_triggered: Vec<usize>,
    /// See [`last_popup_triggered`](Self::last_popup_triggered).
    #[cfg(test)]
    last_popup_handled: Vec<usize>,
    /// A pane the user has asked to convert, applied once the UI pass is over.
    ///
    /// # Why the write is deferred, and what that is and is not protecting
    ///
    /// Two production paths hold a `PaneState` out of `Gui::panes` with
    /// `std::mem::take` for the whole of a pass — the shell's stack+inspector
    /// pass takes the active pane (`ui_shell.rs`), and `render_panes` takes
    /// each pane in turn — leaving a default `PaneState` in the slot. A
    /// `self.panes[idx].set_kind(..)` inside either window writes the
    /// **placeholder**, and the real pane going back afterwards discards it:
    /// no panic, no warning, and a control that will not stay set.
    ///
    /// **The menu dispatcher is not inside either window** — `render_top_bar`
    /// takes no pane at all, so a direct write from the volume toggle would in
    /// fact work today. The inspector's kind segmented control, though, runs
    /// from *inside* the shell's take, where the same direct write is
    /// silently discarded — which is why every kind writer goes through
    /// [`Self::request_pane_kind`], one rule for all of them.
    ///
    /// It is the right shape for one reason more. The writers WP-G adds — an
    /// armed section drag resolving to a line, and the retarget rule that
    /// follows from it — run from **inside** `render_panes`' per-pane take,
    /// where the hazard is live and silent. And the ordering an interaction
    /// needs is the same one the pane count needs: growing it mid-loop moves
    /// the rects of panes the loop has not reached, desynchronising them from
    /// the ones `detect_active_pane_click` hit-tested this frame. One
    /// deferral point, applied at [`Self::apply_pending_pane_kind`] after the
    /// pane loop, serves both.
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
    /// instead of panning the map — see `ui_region`. A successful commit
    /// disarms it ([`Self::apply_pending_region`]), exactly as the section draw
    /// disarms on drawing its line: the mode's job is done, and leaving it on
    /// would turn the next pan into a second box. A discarded mis-drag leaves
    /// it armed — a stray tap must not silently throw away the intent the user
    /// just expressed — and the menu it was armed from can always turn it off.
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
    /// An endpoint drag in flight on a committed section's ground track, or
    /// `None`.
    ///
    /// **Unarmed on purpose** — see `ui_section_edit`'s module doc: a handle is
    /// a visible target and proximity is the disambiguation, so an existing
    /// line's ends are always grabbable on the map pane that owns it. Advanced
    /// only from inside that pane's `Map::show` ([`map`]'s
    /// `track_section_edit`), where the projector is; cleared by both armed-drag
    /// setters, because one drag on one map pane cannot be two gestures, and by
    /// [`Self::dismiss_top_layer`], so Escape mid-drag means what it means
    /// everywhere else.
    section_edit_drag: Option<crate::ui_section_edit::SectionEditDrag>,
    /// Where every committed line's grabbable geometry was drawn **last
    /// frame**, in screen points — endpoints and body track alike.
    ///
    /// Written from inside `Map::show`, read by `render_panes`' pan-suppression
    /// decision *before* it — the press frame has to suppress the pan, and the
    /// press frame is the one frame that cannot yet ask the projector. One
    /// frame stale by construction, which for a press is harmless: a pointer
    /// about to press is not also flinging the viewport. Both readers go
    /// through [`SectionGrabZone::grab_at`], so the suppression and the
    /// authoritative in-show hit test cannot drift apart.
    ///
    /// [`SectionGrabZone::grab_at`]: crate::ui_section_edit::SectionGrabZone::grab_at
    section_handles: Vec<crate::ui_section_edit::SectionGrabZone>,
    /// A dropped handle's line and the section pane it belongs to, applied
    /// **after** the pane loop.
    ///
    /// Deferred for the reason every pending is
    /// ([`pending_pane_kind`](Self::pending_pane_kind)): the drop is recorded
    /// from inside `Map::show`, in the window where the map pane is
    /// `mem::take`n out of the vector. Unlike
    /// [`pending_section_line`](Self::pending_section_line) this can never grow
    /// the layout — it re-aims a section pane that already exists — and its
    /// applier writes the line and nothing else, so the ordinary staleness
    /// poll is what re-cuts. One deferral shape for every writer, rather than
    /// one careful exception.
    pending_section_edit: Option<(PaneId, crate::pane::SectionLine)>,
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
    /// The user's explicit say over the Expanded layers sidebar, from the top
    /// bar's Layers toggle. `None` is the shell default — open where the
    /// sidebar is persistent — and, like `drawer_open`, it is deliberately
    /// session-only: how a session left its panels is not a preference.
    ///
    /// A separate field rather than a widened `drawer_open` because the two
    /// answer at different widths and remember independently: closing the
    /// sidebar on a desktop must not also close the drawer the same window
    /// gets when it narrows past the breakpoint.
    stack_open: Option<bool>,
    /// Whether the inspector panel is open. Session-only, on the same
    /// precedent as `drawer_open`: closed by default at every width, opened
    /// by the top bar's ⚙ toggle, a stack row click ([`Self::select_layer`]),
    /// or the menu's Settings… entry.
    insp_open: bool,
    /// One-shot: the next inspector frame starts its body scrolled to the
    /// top. Set by every selection change, because the three bodies share one
    /// scroll area — its offset is the *panel's* memory, and carrying a deep
    /// settings scroll into a freshly selected layer's options would open
    /// them somewhere in the middle.
    insp_scroll_reset: bool,
    /// What the inspector's body is about while it is open — and what it will
    /// be about when next opened. Session-only, defaults to
    /// [`InspectorSelection::AppSettings`]; a dismissal resets it there (see
    /// [`Self::dismiss_top_layer`]), while the ⟩ collapse deliberately keeps
    /// it, because a collapse is not a deselection.
    inspector_sel: InspectorSelection,
    /// Whether the floating timeline transport is collapsed to its 🕐 chip.
    /// Session-only, like `drawer_open`: how a session left its chrome is not
    /// a preference.
    timeline_collapsed: bool,
    /// Whether the transport's second row — the loop tuning — is shown.
    /// Session-only, on the same precedent.
    timeline_row2: bool,
    /// The archive scrubber's in-flight drag position, as a fraction of the
    /// lookback window, or `None` when no drag is in flight. Remembered
    /// across frames so the handle follows the pointer instead of snapping
    /// back to the resting position every frame; the commit happens once, on
    /// release — see `render_timeline_scrubber`.
    timeline_scrub: Option<f32>,
    /// Whether the floating status bar is collapsed to its ⏵ restore button.
    /// Session-only, on the same precedent as the timeline's collapse.
    statusbar_collapsed: bool,
    /// The floating status bar's rect as drawn this frame, `None` while no
    /// bar is on screen (Compact, or fully faded). Written by
    /// `render_status_bar` before the timeline pass reads it: the collapsed
    /// time chip anchors above the bar's real top edge rather than a guessed
    /// constant (the M8 chip-overlap fix) — and only when it would otherwise
    /// land on the bar, since a bar collapsed to its restore button leaves
    /// the corner open map (M8.1).
    statusbar_rect: Option<egui::Rect>,
    /// Whether the Add-layer catalog is open. Session-only, like every other
    /// open-surface flag; opened by the stack's two `+ Add layer` buttons and
    /// closed by applying a tile, the `✕`, the backdrop, or
    /// [`Self::dismiss_top_layer`].
    catalog_open: bool,
    /// The catalog's search text. Session-only: a filter is a gesture in
    /// progress, not a preference.
    catalog_query: String,
    /// The name being typed into the catalog's "Save current view…" tile,
    /// and whether that inline editor is showing. Session-only, same terms.
    catalog_save_name: String,
    /// See [`Self::catalog_save_name`].
    catalog_saving: bool,
    /// The site list's search text — the inspector body and the site pill's
    /// popover filter through the one field, as they render the one list.
    /// Session-only, same terms as [`Self::catalog_query`].
    site_query: String,
    /// The stack row being drag-reordered by its grip, if one is in flight.
    /// Session-only: a drag is a gesture, not a preference. The permute
    /// happens once, on release — see `ui_stack.rs`'s reorder note.
    stack_drag: Option<OverlayKind>,
    /// The pane whose pill row a first touch tap revealed, if any.
    /// Session-only: a reveal is a gesture in progress, not a preference.
    /// Cleared where the gestures that end it are resolved — a map click
    /// that switches panes, or a confirmed map tap (`ui_map.rs`).
    pill_revealed: Option<PaneId>,
    /// How many pill rows the previous pills pass drew. The rows' areas are
    /// keyed on contiguous `0..pane_count`, so this count *is* the set of
    /// rows on screen last frame — and a pass drawing past it is a debut,
    /// which egui auto-tops. Session-only bookkeeping.
    pills_drawn_last_frame: usize,
    /// A panel raise owed to the next pills pass — armed by every rows'
    /// debut (startup, and any mid-session pane growth), performed one frame
    /// later; see `ui_pills.rs`'s module note on stacking for why the raise
    /// cannot happen on the debut frame itself. Session-only bookkeeping.
    pills_raise_pending: bool,
    /// Whether the pane pill rows render at full opacity unconditionally.
    /// Persisted (`UiConfig::pin_pane_controls`); the settings body's
    /// Interface section is the one writer.
    pin_pane_controls: bool,
    /// Whether the floating chrome is faded away (plan §1.8) — the map-first
    /// state one qualifying click enters and the next one leaves. Session-only
    /// like every open-surface flag: hiding the UI is a gesture, not a
    /// preference. Everything about it lives in `ui_fade.rs`.
    ui_faded: bool,
    /// The pane loop's verdict that this frame's click qualifies as the fade
    /// gesture — recorded in `render_panes` (which alone knows the click's
    /// pane, kind and consumption), resolved by [`Self::apply_fade_toggle`]
    /// after the pending appliers. One-shot per frame.
    fade_candidate: bool,
    /// Whether the most recent primary press was the one that switched the
    /// active pane — written by `detect_active_pane_click` on every press,
    /// read by the fade trigger so a first click on an inactive pane only
    /// activates it (§1.8). Session-only bookkeeping.
    press_switched_pane: bool,
    /// Whether an egui popup — a pill popover, the ☰ dropdown, an open combo
    /// — was open when the most recent primary press landed. Written beside
    /// [`Self::press_switched_pane`], read by the fade trigger: a click
    /// whose press found a popup open is that popup's dismissal (egui closes
    /// it on the click outside), not a fade gesture. Recorded at press time
    /// because by the time the click confirms — the release, or a touch
    /// tap's deferral later — the popup has already closed and the frame
    /// can no longer see what the press was aimed at.
    press_popup_open: bool,
    /// This frame's shared chrome opacity, resolved once at frame top by
    /// [`Self::enforce_fade_invariants`] from the fade animation: `1.0` fully
    /// present, `0.0` fully faded (surfaces skip rendering), in between a
    /// non-interactive transition. See `ui_fade.rs`.
    fade_factor: f32,
    /// The page the sheet last showed — what the sheet's fall animation
    /// renders after the flags have already closed (`ui_sheet.rs`); never
    /// read while a page is open. Session-only bookkeeping.
    sheet_last_page: Option<sheet::SheetPage>,
    /// The message the error toast last showed — what the toast's fade-out
    /// renders after the error has already cleared (`ui_sheet.rs`), on the
    /// same terms as [`Self::sheet_last_page`]; never read while an error is
    /// up. Session-only bookkeeping.
    toast_last_error: Option<String>,
    /// The user's saved presets (§3.11). Persisted; the built-ins are
    /// compiled in beside them (`catalog::builtin_presets`) and never saved.
    presets: Vec<PresetConfig>,
    /// This build's loop frame cap, pushed in by the frontend from
    /// `constants::MAX_LOOP_FRAMES` — this crate cannot read that table (the
    /// dependency points the other way), and the timeline's row-2 caption
    /// wants to state the platform's real budget rather than a guess.
    /// Defaults to the desktop arm's value, which is what every headless
    /// test is.
    loop_frame_budget: usize,
    /// Whether the top bar's ☰ dropdown was open on the last frame it drew.
    ///
    /// The dropdown's real state is egui popup memory, which this crate only
    /// touches mid-frame — but [`Self::dismiss_top_layer`] runs *between*
    /// frames, from the frontend's input handling, so it needs last frame's
    /// answer mirrored somewhere it can reach. Written every frame by
    /// `render_top_bar`, from the popup's own id.
    menu_popup_open: bool,
    /// A dismiss was consumed against the open dropdown; the top bar honours
    /// this (and clears it) by force-closing the popup before next showing it.
    ///
    /// A request rather than a direct write because the popup's memory is
    /// keyed on a widget id that only exists mid-frame — see
    /// `render_top_bar_run`, where the two dismissal routes (Escape, which
    /// egui also sees and closes on itself, and Android's back, which never
    /// enters egui's queue) converge on this one flag.
    menu_popup_close_requested: bool,
    /// Whether the phone sheet's Menu page is open. Session-only, on the
    /// `drawer_open` precedent — and Compact-only chrome: the ☰ Popup keeps
    /// its own egui-managed state on the wider widths (its dismiss handling
    /// is the pair of fields above, and the M1 fix depends on it), so this
    /// flag drives the sheet page alone. `Gui::ui` clears it whenever the
    /// width is not Compact, so a resize with the page open cannot strand a
    /// flag no surface renders consuming a back press.
    menu_open: bool,
    /// The phone sheet's snap position. Session-only: how a session left
    /// its sheet is not a preference.
    sheet_extent: SheetExtent,
    /// The sheet handle's in-flight drag travel in points, or `None` when no
    /// drag is running — the timeline scrubber's own shape, for the same
    /// reason: the commit happens once, on release.
    sheet_drag: Option<f32>,
    /// What the last frame's bottom bar drew. Only read by tests.
    #[cfg(test)]
    last_bottom_bar: BottomBarProbe,
    /// What the last frame's sheet drew. Only read by tests.
    #[cfg(test)]
    last_sheet: SheetProbe,
    /// What the last frame's phone error toast drew. Only read by tests.
    #[cfg(test)]
    last_error_toast: Option<ErrorToastProbe>,
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
    /// GPS configuration (port, baud, heading source).
    pub gps_config: rustdar_gps::GpsConfig,
    /// Storm motion the user typed in, overriding the RPG's SCIT average on
    /// every storm-relative velocity tilt — all four are derived, so all four
    /// take it. `None` means "use the vector the `N0S` product carries", which
    /// is the default and is what AWIPS calls the average storm motion.
    pub storm_motion_override: StormMotionOverride,
    /// Whether one of the storm-motion `DragValue`s is under the pointer or
    /// holding the keyboard *right now*. See [`Self::storm_motion_mid_edit`].
    ///
    /// Session-only and never persisted: it describes a widget's state this
    /// frame, not a setting. Written in two places, both in the frame path and
    /// both clearing it: `render_settings_body`, which clears it before every
    /// pass over the rows, and [`Self::ui`], which clears it for a frame where
    /// those rows do not draw at all. A latch with neither would stick the
    /// first time the panel closed mid-drag and the vector would never be
    /// applied again.
    ///
    /// `pub` for the reason [`Self::storm_motion_override`] beside it is: the
    /// crate that owns the commit rule is `rustdar_frontend`, and it has to be
    /// able to drive both halves of it in a test.
    pub storm_motion_editing: bool,
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
    /// The user's Volume Alpha curves, one per edited product. See
    /// [`crate::volume_alpha`]: absence means "render through the palette's
    /// own alpha, bit-exactly", which is why this is a store of exceptions
    /// rather than a curve per product.
    pub(crate) volume_alpha: crate::volume_alpha::AlphaCurves,
    /// The user's isosurface thresholds, one per edited product. See
    /// [`crate::volume_iso`]: absence means the argued per-product default,
    /// so this too is a store of exceptions.
    pub(crate) volume_iso: crate::volume_iso::IsoThresholds,
}

/// A storm motion vector the user may substitute for the RPG's.
///
/// The two numbers persist while the override is switched off so that toggling
/// it does not lose what was typed — and they persist across sessions too
/// (`UiConfig`), which closed the audit's known gap. `#[serde(default)]` on
/// the struct keeps a config written before any one field existed loading;
/// the writer guards the floats finite (see `ui_config_json`).
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
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

/// Every handler with controls, in the audit's canonical order.
///
/// Production no longer iterates this: the stack's rows walk the active
/// pane's own `draw_order`, and the inspector renders one selected handler.
/// It remains the parity walk's inventory — the list of handlers whose every
/// control must be reachable — which is why it is test-only now rather than
/// deleted: a handler dropped from it would silently leave the audit.
#[cfg(test)]
pub(crate) const OVERLAY_CONTROL_ORDER: &[OverlayKind] = &[
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

/// The widget shape a [`ControlItem`] rendered as.
///
/// Coarser than the model on purpose — a `ButtonRow` records one entry per
/// button, a `Separator` records nothing — because what a test needs is to name
/// the thing it expects on screen, not to reconstruct the tree.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DrawnControlKind {
    Checkbox,
    Slider,
    Button,
    InfoText,
    Heading,
    Dropdown,
    Section,
}

/// One control a handler's tree actually drew — the generalisation of
/// [`DrawnDropdown`] to every [`ControlItem`] shape: which handler's pass drew
/// it, what it read as, and where it landed so a test can scroll to it and
/// click it.
///
/// Reported by the renderer, like [`ui_menu::DrawnMenuLeaf`], rather than
/// rebuilt by a test from the [`ControlItem`] — a test that walked the model
/// itself would agree with a renderer that had stopped drawing part of it.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DrawnControlItem {
    /// The handler whose tree this item came from. `Some` for every item the
    /// current renderer draws; an `Option` so a control drawn outside any
    /// handler's tree can share the probe when one exists.
    pub handler: Option<OverlayKind>,
    pub label: String,
    pub kind: DrawnControlKind,
    pub rect: egui::Rect,
}

/// What one pass over a control tree drew. A no-op outside tests, like
/// [`ui_menu::MenuFrame`].
#[derive(Default)]
pub(crate) struct ControlProbe {
    #[cfg(test)]
    pub drawn: Vec<DrawnDropdown>,
    /// Every item drawn, whatever its shape. See [`DrawnControlItem`].
    #[cfg(test)]
    pub items: Vec<DrawnControlItem>,
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

    /// Record one drawn item. Test-only, so the call sites are gated too —
    /// unlike [`Self::record_dropdown`] this takes a test-only type.
    #[cfg(test)]
    #[inline]
    fn record_item(
        &mut self,
        handler: OverlayKind,
        kind: DrawnControlKind,
        label: &str,
        rect: egui::Rect,
    ) {
        self.items.push(DrawnControlItem {
            handler: Some(handler),
            label: label.to_owned(),
            kind,
            rect,
        });
    }
}

/// The line a 3D or section pane's sidebar shows where a map pane's layer
/// list would be.
///
/// The panel is titled "Layers", so for a pane whose kind has none the honest
/// presentations are a tree of disabled ghosts or an explained absence. The
/// convention here — for both non-map kinds alike — is the absence: every
/// entry in the tree is a layer drawn over map tiles, and a dozen disabled
/// rows would bury the controls that do apply under ones that never can. One
/// line keeps the void from reading as a broken panel.
pub(crate) const NON_MAP_LAYERS_NOTE: &str = "Map layers apply to map panes.";

/// The header over the section pane's sidebar block. Icon, two spaces, name —
/// the same shape as the loop transport's and the overlay rows' labels. The
/// icon is the top bar's own X-sec diagonal (`∕`): the demo's `✂` has no
/// glyph in egui's bundled fonts (see `ui_glyphs.rs`), and sharing the arm
/// toggle's glyph teaches which mode draws this pane's line.
pub(crate) const SECTION_SIDEBAR_HEADER: &str = "\u{2215}  Cross-section";

/// The identity line every pane kind's sidebar opens with: whose data this
/// pane shows and what the pane is, e.g. `KTLX · 3D volume`.
///
/// One function called before the kind branch rather than a line inside each
/// arm, so the three kinds keep one style and cannot drift into three
/// headers. For a map pane it is close to redundant — the panel under it is
/// full of self-describing map content — and that redundancy is the point:
/// the same line in the same place is what makes a converted pane's shorter
/// panel read as *this* panel showing fewer controls.
///
/// Reads only the `pane` it is handed: for the whole of the panel's pass the
/// active slot in `self.panes` holds a `mem::take` placeholder that reads as
/// a map pane on the default site.
fn render_pane_identity(ui: &mut egui::Ui, pane: &PaneState) {
    let kind = match pane.kind() {
        crate::pane::PaneKind::Map => "Map",
        crate::pane::PaneKind::CrossSection => "Cross-section",
        crate::pane::PaneKind::Volume => "3D volume",
    };
    ui.label(egui::RichText::new(format!("{} - {}", pane.site, kind)).strong());
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
            let response = ui.checkbox(&mut value, label.as_str());
            #[cfg(test)]
            probe.record_item(kind, DrawnControlKind::Checkbox, label, response.rect);
            if response.changed() {
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
            let response = ui.label(text.as_str());
            #[cfg(test)]
            probe.record_item(kind, DrawnControlKind::Heading, text, response.rect);
            #[cfg(not(test))]
            let _ = response;
        }
        ControlItem::InfoText { text } => {
            let response = ui.label(egui::RichText::new(text.as_str()).small().weak());
            #[cfg(test)]
            probe.record_item(kind, DrawnControlKind::InfoText, text, response.rect);
            #[cfg(not(test))]
            let _ = response;
        }
        ControlItem::ButtonRow { buttons } => {
            let any_highlighted = buttons.iter().any(|b| b.highlight);
            ui.horizontal_wrapped(|ui| {
                for btn in buttons {
                    let response = if any_highlighted {
                        ui.add_enabled(
                            btn.enabled,
                            egui::Button::new(btn.label.as_str()).selected(btn.highlight),
                        )
                    } else {
                        ui.add_enabled(btn.enabled, egui::Button::new(btn.label.as_str()))
                    };
                    #[cfg(test)]
                    probe.record_item(kind, DrawnControlKind::Button, &btn.label, response.rect);
                    if response.clicked() {
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
                #[cfg(test)]
                probe.record_item(kind, DrawnControlKind::Dropdown, label, combo.response.rect);
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
            let row = ui.horizontal(|ui| {
                ui.label(label.as_str());
                let mut slider = egui::Slider::new(&mut val, *min..=*max);
                if *logarithmic {
                    slider = slider.logarithmic(true);
                }
                ui.add(slider);
            });
            #[cfg(test)]
            probe.record_item(kind, DrawnControlKind::Slider, label, row.response.rect);
            #[cfg(not(test))]
            let _ = row;
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
                let collapsing = egui::CollapsingHeader::new(label.as_str())
                    .default_open(*expanded)
                    .show(ui, |ui| {
                        for child in items {
                            render_control_item(ui, kind, child, updates, probe);
                        }
                    });
                // The header's own rect, so a test can open a collapsed
                // section the way a user does — the children record
                // themselves only on a frame the body actually drew.
                #[cfg(test)]
                probe.record_item(
                    kind,
                    DrawnControlKind::Section,
                    label,
                    collapsing.header_response.rect,
                );
                #[cfg(not(test))]
                let _ = collapsing;
            } else {
                let group = ui.group(|ui| {
                    ui.label(egui::RichText::new(label.as_str()).strong());
                    for child in items {
                        render_control_item(ui, kind, child, updates, probe);
                    }
                });
                #[cfg(test)]
                probe.record_item(kind, DrawnControlKind::Section, label, group.response.rect);
                #[cfg(not(test))]
                let _ = group;
            }
        }
    }
}

/// Whether `item` is one of a handler's *master* controls — its heading, or
/// its whole-layer `enabled` toggle — which the inspector expresses as the
/// crumb and the "Show <layer>" toggle instead of rendering the handler's
/// copies.
///
/// One predicate, two callers: `render_overlay_controls_one` skips these and
/// the parity walk excludes them from its inventory. A copy in either place
/// would let the renderer and the audit disagree about what "every control"
/// means.
pub(crate) fn is_master_control(item: &ControlItem) -> bool {
    matches!(
        item,
        ControlItem::Heading { .. } | ControlItem::Toggle { id: "enabled", .. }
    )
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
            current_volumes: HashMap::new(),
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
            user_fix_at: None,
            location_permission: rustdar_gps::LocationPermission::default(),
            location_active: false,
            location_settings_available: false,
            user_heading: None,
            overlays: OverlayRegistry::default(),
            panes: vec![PaneState::new()],
            active_pane: 0,
            pane_layout: PaneLayout::default(),
            color_scale_orientation: ColorScaleOrientation::default(),
            map_pane_geo: HashMap::new(),
            floor_tile_zoom_bias: 0,
            #[cfg(test)]
            last_map_panel_rect: egui::Rect::ZERO,
            #[cfg(test)]
            widget_id_probes: Vec::new(),
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
            last_pane_borders: Vec::new(),
            #[cfg(test)]
            last_section_tracks: Vec::new(),
            #[cfg(test)]
            last_alpha_buttons: Vec::new(),
            #[cfg(test)]
            last_paint_order: Vec::new(),
            #[cfg(test)]
            last_status_bar: StatusBarProbe::default(),
            #[cfg(test)]
            last_timeline: TimelineProbe::default(),
            #[cfg(test)]
            last_top_bar: TopBarProbe::default(),
            #[cfg(test)]
            last_stack: StackProbe::default(),
            #[cfg(test)]
            last_inspector: InspectorProbe::default(),
            #[cfg(test)]
            last_catalog: CatalogProbe::default(),
            #[cfg(test)]
            last_pills: Vec::new(),
            #[cfg(test)]
            last_pill_popover: None,
            click_consumed_frame: false,
            #[cfg(test)]
            control_render_passes: 0,
            #[cfg(test)]
            last_dropdowns: Vec::new(),
            #[cfg(test)]
            last_control_items: Vec::new(),
            #[cfg(test)]
            last_settings_rows: Vec::new(),
            #[cfg(test)]
            last_popup_triggered: Vec::new(),
            #[cfg(test)]
            last_popup_handled: Vec::new(),
            pending_pane_kind: None,
            region_arm: false,
            region_drag: None,
            pending_region: None,
            section_draw_armed: false,
            section_anchor: None,
            pending_section_line: None,
            section_edit_drag: None,
            section_handles: Vec::new(),
            pending_section_edit: None,
            viewport_sync: true,
            sync_layers: true,
            loop_lookback_secs: 3600, // default 1 hour
            loop_speed_fps: 5.0,      // default 5 fps
            drawer_open: false,
            stack_open: None,
            insp_open: false,
            insp_scroll_reset: false,
            inspector_sel: InspectorSelection::AppSettings,
            timeline_collapsed: false,
            timeline_row2: false,
            timeline_scrub: None,
            statusbar_collapsed: false,
            statusbar_rect: None,
            catalog_open: false,
            catalog_query: String::new(),
            catalog_save_name: String::new(),
            catalog_saving: false,
            site_query: String::new(),
            stack_drag: None,
            pill_revealed: None,
            pills_drawn_last_frame: 0,
            pills_raise_pending: false,
            pin_pane_controls: false,
            ui_faded: false,
            fade_candidate: false,
            press_switched_pane: false,
            press_popup_open: false,
            fade_factor: 1.0,
            sheet_last_page: None,
            toast_last_error: None,
            presets: Vec::new(),
            // The desktop arm of `constants::MAX_LOOP_FRAMES`; the frontend
            // pushes the real target's value at startup.
            loop_frame_budget: 60,
            menu_popup_open: false,
            menu_popup_close_requested: false,
            menu_open: false,
            sheet_extent: SheetExtent::Half,
            sheet_drag: None,
            #[cfg(test)]
            last_bottom_bar: BottomBarProbe::default(),
            #[cfg(test)]
            last_sheet: SheetProbe::default(),
            #[cfg(test)]
            last_error_toast: None,
            safe_area_insets: (0.0, 0.0, 0.0, 0.0),
            supports_exit: true,
            modality: ModalityLatch::default(),
            layout: LayoutCtx::default(),
            interaction: InteractionState::default(),
            preferences: UserPreferences::default(),
            gps_config: rustdar_gps::GpsConfig::default(),
            storm_motion_override: StormMotionOverride::default(),
            storm_motion_editing: false,
            volume_painter: None,
            volume_alpha: crate::volume_alpha::AlphaCurves::default(),
            volume_iso: crate::volume_iso::IsoThresholds::default(),
        };
        gui.initialize_pane_enabled();
        gui
    }

    /// Create the UI using egui.
    pub fn ui(&mut self, ctx: &egui::Context) -> Vec<GuiAction> {
        let mut actions = Vec::new();

        // The second writer of `storm_motion_editing`, and the reason a latch
        // here cannot stick: the rows that clear it only run while the
        // settings body is drawn, so a panel closed mid-drag would leave the
        // commit deferred for ever. Cleared *before* the body draws, so a body
        // that does draw this frame still gets the last word.
        if !self.settings_visible() {
            self.storm_motion_editing = false;
        }

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
            // Per-frame paint records of the pane loop, on the same terms:
            // the borders, the section tracks and the Volume Alpha corner
            // buttons are all re-painted (or legitimately absent) each frame.
            self.last_pane_borders.clear();
            self.last_section_tracks.clear();
            self.last_alpha_buttons.clear();
            self.last_paint_order.clear();
            // Cleared like the rest: the picker redraws from the top bar every
            // frame, and appending over a stale list would report every button
            // twice.
            self.last_pane_options.clear();
            // The handler dropdowns only exist while the layers panel is on
            // screen, so a stale entry would report widgets that are not there.
            self.last_dropdowns.clear();
            // And its generalisation, for the same reason.
            self.last_control_items.clear();
            // Likewise: the settings rows only exist while the window is open.
            self.last_settings_rows.clear();
            // Per-frame records of the popup's action handling; a leftover
            // entry would report a button press that did not happen this frame.
            self.last_popup_triggered.clear();
            self.last_popup_handled.clear();
            // Per-frame records of the stack and inspector; a stale probe
            // would report a panel that is no longer on screen. Reset rather
            // than cleared, like the timeline's — `open: false` is a report,
            // not an absence.
            self.last_stack = StackProbe::default();
            self.last_inspector = InspectorProbe::default();
            self.last_catalog = CatalogProbe::default();
            // Per-frame records of the pill rows and their popover; a stale
            // entry would report a row for a pane no longer on screen.
            self.last_pills.clear();
            self.last_pill_popover = None;
            // The double-render guard's counter; see the field.
            self.control_render_passes = 0;
            // Per-frame records of the phone shell's bottom cluster; reset
            // like the stack's — `page: None` is a report, not an absence.
            self.last_bottom_bar = BottomBarProbe::default();
            self.last_sheet = SheetProbe::default();
            // And of its error toast — `None` is "no toast drew".
            self.last_error_toast = None;
        }

        // The sheet's Menu page is Compact chrome; on the wider widths the ☰
        // Popup owns the menu with its own egui-managed state. Clearing the
        // flag whenever the width says so is what keeps a resize with the
        // page open from stranding a flag no surface renders — which
        // `dismiss_top_layer` would then consume a back press against,
        // invisibly.
        if self.layout.width != crate::ui_layout::WidthClass::Compact {
            self.menu_open = false;
        }

        // The fade's frame-top pass: while faded nothing may be open — a
        // surface found open means the user acted through a route the
        // pointer guards cannot see, and the repair is to unfade — and the
        // frame's shared chrome opacity resolves here, once. See `ui_fade.rs`.
        self.enforce_fade_invariants(ctx);

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

        // The shell first: the docked top bar claims its space, the floating
        // surfaces — status bar, layer stack, inspector — position themselves
        // in what it left, and that remainder — `shell.map_rect` — is the
        // map's. See `ui_shell.rs`.
        let shell = self.render_shell(&mut root_ui);
        actions.extend(shell.actions);

        if let Some(action) = self.render_time_dialog(ctx) {
            actions.push(action);
        }

        actions.extend(self.render_panes(&mut root_ui, &shell.excluded_rects));

        // After the pane loop, and therefore after every `mem::take` window in
        // the frame has closed. See the `pending_pane_kind` field for why
        // converting a pane cannot be a direct write from the dispatcher that
        // asked for it.
        self.apply_pending_pane_kind(&mut actions);
        // Same window, and one thing more: this can grow `pane_count`, which
        // moves `pane_rect` for every pane. Inside the loop that would leave the
        // panes drawn after it hit-tested against rects they are no longer in.
        self.apply_pending_section_line();
        // After the modal-draw applier, so if both somehow fired in one frame
        // the dropped edit — the later write — would win. The case is
        // unreachable: an armed draw makes the handles inert, and beginning a
        // handle drag requires no mode to be armed, so the two cannot both
        // have a gesture to commit. This one can never grow the layout, so it
        // takes no part in the ordering argument below.
        self.apply_pending_section_edit();
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

        // The fade toggle, after the appliers like every other loop-recorded
        // intent: it needs the pane loop's final consumption verdict, and the
        // surfaces drawn below read the state it settles. See `ui_fade.rs`.
        self.apply_fade_toggle(ctx);

        // The pill rows, after the pane loop and the appliers: outside every
        // `mem::take` window, so a popover pick writes real panes, and after
        // the kind appliers so a row states the kind its pane ended the
        // frame as. See `ui_pills.rs`.
        self.render_pane_pills(ctx, shell.map_rect, &mut actions);

        // The phone shell's bottom bar, before the timeline so the inline
        // transport can position itself above the bar it just drew. Only on
        // Compact — the wider widths keep the floating bottom-centred
        // transport and no bar.
        let phone_bar_top = (self.layout.width == crate::ui_layout::WidthClass::Compact)
            .then(|| self.render_bottom_bar(ctx, shell.map_rect));

        // The timeline transport, after the pane loop and the appliers: every
        // `mem::take` window in the frame has closed, so it reads and writes
        // `self.panes[self.active_pane]` directly — the real pane, not a
        // placeholder. See `ui_timeline.rs`.
        self.render_timeline(ctx, shell.map_rect, phone_bar_top, &mut actions);

        // The sheet, above everything the phone shell floats: the Layers and
        // Inspector pages open their take window in here, so it must run
        // after the pane loop and the appliers on the same terms as the
        // shell's own pass — and the Catalog page's apply paths take panes
        // themselves, so no window may already be open. See `ui_sheet.rs`.
        if let Some(bar_top) = phone_bar_top {
            self.render_phone_error_toast(ctx, shell.map_rect, true);
            self.render_phone_sheet(ctx, shell.map_rect, bar_top, &mut actions);
        } else {
            // The error surface outranks the fade (the deliberate §1.8
            // refinement in `ui_fade.rs`): the wide widths normally carry the
            // error inside the status bar, which is faded — so while faded
            // the phone's own toast presentation carries it instead. Called
            // unconditionally so its rise and fall animate through the
            // fade/unfade handoff; unfaded it presents nothing.
            self.render_phone_error_toast(ctx, shell.map_rect, self.ui_faded);
        }

        // Floating windows last, so they layer above the chrome and the map.
        // (Settings are no longer a window of their own: they are the
        // inspector's App › Settings body, drawn by the shell above.) On
        // Compact both return without drawing — the sheet pages above are
        // their presentation there (plan §1.9).
        self.render_overlay_popup(ctx);

        // The Add-layer catalog, after the feature popup so it stacks above
        // one left open — matching `dismiss_top_layer`, which closes the
        // catalog first. Also after the appliers on its own account: applying
        // a preset writes pane kinds directly and can grow the pane count,
        // both of which are only safe once every take window has closed.
        self.render_catalog(ctx, &mut actions);

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

    /// The read-side context handlers are asked for their controls with, aimed
    /// at the active pane.
    ///
    /// One constructor for the renderer and the test accessors alike, so the
    /// model a test asks a handler for is built exactly as the renderer builds
    /// it — the two diverging is how an inventory drifts from the glass.
    fn active_pane_control_context(&self) -> PaneControlContext<'_> {
        PaneControlContext {
            pane_idx: self.active_pane,
            pane_state: None,
        }
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

    /// Publish each site's current-volume stamp — the identity and freshness
    /// of the merged volume a whole-volume pane may build from, advanced by
    /// every sealed sweep.
    ///
    /// Pushed in by the App each frame, the same arrangement as
    /// [`Self::set_chunk_status`] and for the same reason: the decoded volumes
    /// live there, and this crate holds only their names.
    pub fn set_current_volumes(&mut self, volumes: HashMap<String, CurrentVolumeStamp>) {
        self.current_volumes = volumes;
    }

    /// The stamp of `site`'s current volume, if this build holds one at all.
    ///
    /// `None` is an ordinary state and the reason a 3D pane says it is
    /// waiting: it is what a site looks like before its first volume — archive
    /// fetch or first sealed sweeps — has arrived.
    pub fn current_volume_for(&self, site: &str) -> Option<CurrentVolumeStamp> {
        self.current_volumes.get(site).copied()
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
    /// app — which is why a stray press with something open used to cost a
    /// whole relaunch on a phone, back going straight to minimise.
    ///
    /// Ordered topmost first — whatever is painted over everything else is
    /// what a press is aimed at — and exactly one layer closes per press.
    /// The full order (contract 65): the in-flight handle drag → the fade →
    /// the ☰ dropdown → catalog → feature → time → menu → inspector → the
    /// stack's drawer form → the armed drags.
    ///
    /// Not derived from the order `ui` calls them in, which is shell (stack
    /// and inspector included), then time dialog, then popup. The popup is
    /// `Order::Foreground`, so egui stacks it above the `Order::Middle`
    /// panels whatever the call order, and the time dialog sits between. This
    /// order is asserted rather than computed; see
    /// `a_back_press_closes_one_open_layer_at_a_time`.
    ///
    /// Below the Compact breakpoint the asserted chain gives way to the
    /// sheet's projection: every page flag presents as one sheet there, so
    /// the press pops exactly the page [`Gui::top_sheet_page`] reports on
    /// top, and only the non-page layers (the in-flight drag, the armed
    /// modes) keep their fixed places around it. See
    /// `a_back_press_walks_the_phone_sheet_pages_top_down`.
    ///
    /// Deliberately not reachable from `request_exit`: the window's close
    /// button and the menu's Exit item are unambiguous, and dismissing a dialog
    /// instead of honouring them would strand the user — the Exit item lives
    /// *inside* the ☰ dropdown this function closes first.
    pub fn dismiss_top_layer(&mut self) -> bool {
        // First, above everything painted: a handle drag in flight owns the
        // pointer right now, which makes it the most immediate thing a "back
        // out" gesture can be aimed at. Cancelling restores the line the drag
        // started from — the preview was never written anywhere.
        if self.section_edit_drag.is_some() {
            self.section_edit_drag = None;
            return true;
        }
        // The fade, next: while faded the invariant holds nothing else open
        // (`enforce_fade_invariants`), so a back press can only mean "restore
        // my UI" — the same reading every top-bar interaction gives it
        // (§3.6's unfade-before-acting), and consistent with the chain's
        // rule: the press is aimed at the most immediate state the user is
        // in. Only the handle drag outranks it, because a drag in flight can
        // exist *while* faded — the map stays interactive — and it owns the
        // pointer right now. The armed modes cannot coexist with the fade
        // (arming routes unfade first; an armed click never fades), so their
        // place below is never contested.
        if self.ui_faded {
            self.ui_faded = false;
            return true;
        }
        // The ☰ dropdown, above every dialog: it is `Order::Foreground` and
        // opened last, and it is the head of the plan's Esc chain (§3.4).
        //
        // egui's `Popup` closes itself on the Escape *it* sees, but that
        // covers one of this function's three routes. The frontend resolves
        // the same Escape press here independently, and without this layer
        // that resolution fell through to whatever sat beneath the popup —
        // two layers on one press. Android's back is worse: a logical event
        // that never enters egui's queue at all, so the popup would have
        // stayed open over a drawer this function closed behind it. Consuming
        // the press here and letting `render_top_bar_run` honour the request
        // makes all three routes close the popup, and the popup only — the
        // Escape egui also saw closes it twice over, idempotently.
        if self.menu_popup_open {
            self.menu_popup_open = false;
            self.menu_popup_close_requested = true;
            return true;
        }
        // On Compact every page flag presents as the sheet, so dismissal
        // reads the same projection the renderer does: pop exactly the page
        // `top_sheet_page` says is visibly on top. The fixed chain below
        // cannot serve here, because flags can stack out of its order —
        // flags set on a wider width and carried through a resize (the bar's
        // own pages are exclusive since contract 64's revision, but a
        // resize is not the bar) — and the chain would then pop a layer the
        // projection never shows, consuming a press invisibly. One rule
        // either side of the breakpoint: dismissal pops what is painted on
        // top.
        if self.layout.width == crate::ui_layout::WidthClass::Compact {
            if let Some(page) = self.top_sheet_page() {
                match page {
                    sheet::SheetPage::Feature => {
                        self.overlays.selected_overlays.clear();
                        self.overlays.selected_overlay_page = 0;
                    }
                    sheet::SheetPage::Time => self.time_dialog.show = false,
                    sheet::SheetPage::Catalog => self.catalog_open = false,
                    sheet::SheetPage::Menu => self.menu_open = false,
                    sheet::SheetPage::Inspector => {
                        // The same reset the wide arm below makes: a
                        // dismissal is a "back out", and what was backed out
                        // of should not lie in wait for the next open.
                        self.insp_open = false;
                        self.inspector_sel = InspectorSelection::AppSettings;
                    }
                    sheet::SheetPage::Layers => self.drawer_open = false,
                }
                return true;
            }
        } else {
            // The catalog, above the feature and time dialogs (plan §3.4 as
            // amended): it is the modal opened last when it is open at all,
            // and the frame draws it above a feature popup left open for the
            // same reason.
            if self.catalog_open {
                self.catalog_open = false;
                return true;
            }
            if !self.overlays.selected_overlays.is_empty() {
                self.overlays.selected_overlays.clear();
                self.overlays.selected_overlay_page = 0;
                return true;
            }
            if self.time_dialog.show {
                self.time_dialog.show = false;
                return true;
            }
            // The phone sheet's Menu page has no presentation up here, and
            // `Gui::ui` clears its flag on every wider frame — this arm only
            // covers a press landing between a resize and the frame that
            // normalises it.
            if self.menu_open {
                self.menu_open = false;
                return true;
            }
            // The inspector, below the dialogs: it is a side panel, not a
            // modal, so anything modal over the map outranks it. Closing
            // resets the selection to App › Settings (plan §3.4) — a
            // dismissal is a "back out", and what the user backed out of
            // should not lie in wait for the next open.
            if self.insp_open {
                self.insp_open = false;
                self.inspector_sel = InspectorSelection::AppSettings;
                return true;
            }
            // The stack, in its drawer form only — the presentation that
            // covers the map. The Expanded sidebar is deliberately not a
            // dismissal target: it is open by default, and an Escape with
            // nothing else open closing it would put the sidebar between
            // every desktop user and "Escape means leave".
            if self.drawer_open {
                self.drawer_open = false;
                return true;
            }
        }
        // Last, below every painted layer, because an armed drag is a *mode*
        // rather than something on screen: whatever is drawn over the map is
        // what a press is aimed at, and the ☰ dropdown in particular is one of
        // the two places the mode is armed from.
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
        // On Compact the sheet's Time page is the presentation (plan §1.9) —
        // the phone never draws this window.
        if !self.time_dialog.show || self.layout.width == crate::ui_layout::WidthClass::Compact {
            return None;
        }

        let mut action = None;
        egui::Window::new("Set Time")
            .collapsible(false)
            .resizable(false)
            .pivot(egui::Align2::CENTER_CENTER)
            // Centred in the content rect, not the viewport: on a device
            // with a notch or a nav bar those differ, and centring on the
            // viewport puts the dialog partly underneath them.
            .default_pos(self.layout.dialog_center())
            .show(ctx, |ui| {
                action = self.render_time_dialog_body(ui);
            });
        action
    }

    /// The Set Time dialog's body, host-free: the window above wraps it on
    /// the wider widths and the phone sheet's Time page hosts it verbatim,
    /// so the two presentations cannot drift.
    pub(super) fn render_time_dialog_body(&mut self, ui: &mut egui::Ui) -> Option<GuiAction> {
        let mut action = None;
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
                    if let Ok(timestamp) =
                        chrono::NaiveDateTime::parse_from_str(&datetime_str, "%Y-%m-%d %H:%M:%S")
                    {
                        self.radar.config.timestamp = timestamp;
                        if let Some(pane) = self.panes.get_mut(self.active_pane) {
                            pane.viewing_live = false;
                        }
                        action = Some(GuiAction::FetchRadarScan(self.radar.config.clone()));
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
        action
    }

    /// Whether the layers panel is on screen this frame, in either form.
    ///
    /// One question with two answers by width: on Expanded the panel is the
    /// sidebar, open unless [`Self::stack_open`] says otherwise; elsewhere it
    /// is the drawer, closed unless opened. The top bar's Layers toggle reads
    /// and writes through this split, so it is the one definition of "open".
    pub(super) fn layers_panel_visible(&self) -> bool {
        if self.layout.width.has_persistent_sidebar() {
            self.stack_open.unwrap_or(true)
        } else {
            self.drawer_open
        }
    }

    /// The cross-section pane's own sidebar block: what the pane is cutting
    /// along, in the same header-then-indent shape as every other block in the
    /// panel — the loop transport, the 3D view's knobs — so a section pane's
    /// sidebar reads as the normal panel showing this pane's controls rather
    /// than as a stub with most of the panel missing.
    ///
    /// It states rather than steers: a line is aimed by drawing it on a map,
    /// and a sidebar editor for its endpoints would be a second, worse way to
    /// do the same thing. The hint names the real control by its own menu
    /// label, so renaming the menu entry cannot strand the hint pointing at a
    /// control that no longer exists.
    ///
    /// Reads only the `pane` it is handed, never `self.panes` — the caller
    /// holds the active pane out of the vector for the whole panel pass.
    fn render_section_controls(&self, ui: &mut egui::Ui, pane: &PaneState) {
        ui.add_space(6.0);
        ui.separator();
        ui.label(SECTION_SIDEBAR_HEADER);
        ui.indent("section_controls", |ui| {
            match pane.cross_section().and_then(|section| section.line) {
                Some(line) => {
                    // The ends are named A and B because that is what the map
                    // paints at them; the length is the same haversine the
                    // hover readout uses rather than a second copy of it.
                    // ASCII prose throughout — the M8 glyph rules in
                    // `ui_glyphs.rs`.
                    let (_, km) = rustdar_radar::beam::site_bearing_range_km(
                        line.a().lat,
                        line.a().lon,
                        line.b().lat,
                        line.b().lon,
                    );
                    let unit = self.preferences.distance;
                    ui.label(format!(
                        "A - B: {:.0} {}",
                        unit.convert_from_km(km),
                        unit.suffix()
                    ));
                }
                None => {
                    ui.label("No line drawn yet");
                }
            }
            ui.label(
                egui::RichText::new(format!(
                    "Aim it: turn on \"{}\" and drag across a map.",
                    ui_menu::DRAW_CROSS_SECTION_LABEL
                ))
                .small()
                .weak(),
            );
        });
    }

    /// Render the radar product picker, and the tilt picker where a tilt means
    /// anything.
    ///
    /// Hosted by the inspector's Pane-properties body — the only caller since
    /// the stack/inspector split — but kept here beside the identity line and
    /// the section block it shares the panel with. The combo salts keep the
    /// `layers_` prefix they have always had, so the widget state egui stores
    /// under them survived the move.
    pub(super) fn render_radar_controls(
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
                    // The combo's body is the shared product list — the same
                    // function the product pill's popover renders — so the
                    // two routes offer one inventory by construction
                    // (`ui_pills.rs`'s module note).
                    let product_combo =
                        egui::ComboBox::from_id_salt(format!("{id_prefix}product_sel"))
                            .selected_text(pane.selected_product.name())
                            .width(combo_width)
                            .show_ui(ui, |ui| {
                                pills::product_list_ui(
                                    ui,
                                    &scan_info.available_products,
                                    pane.selected_product,
                                )
                                .picked
                            });
                    if let Some(Some(picked)) = product_combo.inner {
                        pane.selected_product = picked;
                    }
                    #[cfg(test)]
                    probes.push(("product_sel", product_combo.response.id));
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
                            // The shared tilt list — the tilt pill popover's
                            // own body — for the same one-inventory reason
                            // as the product combo above.
                            let shown = combo.show_ui(ui, |ui| {
                                pills::tilt_list_ui(ui, elevations, pane.selected_elevation).picked
                            });
                            if let Some(Some(angle)) = shown.inner {
                                pane.selected_elevation = angle;
                            }
                            shown.response.id
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

    /// Render **one** handler's controls — the only place handler
    /// [`ControlItem`]s render, hosted by the inspector's layer body.
    ///
    /// The round trip is the old 12-kind loop's, for one kind: load the
    /// active pane's config snapshot into the handlers, render the tree,
    /// apply updates, honour Fetch effects, then save the (possibly mutated)
    /// handler state back to the pane. This is what makes every sub-control
    /// (categories, day, products, etc.) per-pane when Sync Layers is off —
    /// and why there must be exactly one such pass per frame: each pass ends
    /// by overwriting the pane's configs with the handlers' state, so a
    /// second pass would save over the first's writes with whatever it had
    /// loaded before them. The `control_render_passes` counter holds the
    /// suite to that.
    ///
    /// The handler's own [`is_master_control`] items — its heading and its
    /// master `enabled` toggle — are skipped: the inspector's crumb names the
    /// layer and its "Show <layer>" toggle is the master, so rendering the
    /// handler's copies would put two of each on screen with only one wired
    /// to [`Self::select_layer`]'s discipline. The parity walk excludes them
    /// through the same predicate, so the two cannot drift.
    pub(super) fn render_overlay_controls_one(
        &mut self,
        ui: &mut egui::Ui,
        pane: &mut PaneState,
        kind: OverlayKind,
        actions: &mut Vec<GuiAction>,
    ) {
        #[cfg(test)]
        {
            self.control_render_passes += 1;
        }

        // Load this pane's config snapshot into the handlers.
        if !pane.overlay_configs.is_empty() {
            self.overlays.load_pane_configs(&pane.overlay_configs);
        }

        let ctx = self.active_pane_control_context();

        // Render controls and collect updates.
        let mut updates: Vec<(OverlayKind, ControlUpdate)> = Vec::new();
        let mut probe = ControlProbe::default();

        let controls = self.overlays.controls(kind, &ctx);
        for item in controls.iter().filter(|item| !is_master_control(item)) {
            render_control_item(ui, kind, item, &mut updates, &mut probe);
        }

        #[cfg(test)]
        {
            self.last_dropdowns.extend(probe.drawn.iter().cloned());
            self.last_control_items.extend(probe.items.iter().cloned());
        }
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

    /// The pane indices shared time fans out over: the active pane, plus —
    /// with Sync Layers on and more than one pane — every visible pane whose
    /// [`PaneState::time_link`] is still on (plan §3.7).
    ///
    /// The active pane is a target unconditionally, its own flag unread: it
    /// is the pane whose control was operated, and "the pane I am driving
    /// does not respond" is not a reading of unlink anyone means. Unlink says
    /// *don't drag me along* — the exclusion is from the fan-out, not from
    /// being driven directly.
    fn time_sync_targets(&self) -> Vec<usize> {
        if self.sync_layers && self.pane_layout.pane_count > 1 {
            (0..self.pane_layout.pane_count)
                .filter(|&idx| {
                    idx == self.active_pane || self.panes.get(idx).is_none_or(|pane| pane.time_link)
                })
                .collect()
        } else {
            vec![self.active_pane]
        }
    }

    /// [`Self::time_sync_targets`] narrowed to the panes a loop can feed —
    /// the fan-out for every loop action.
    ///
    /// Panes that cannot loop are left out ([`PaneKind::can_loop`]), which today
    /// means 3D volume panes. A loop is a sequence of rendered pictures and
    /// `dispatch_loop_renders` feeds only the kinds that have one — so enabling
    /// the loop with sync on would otherwise put every volume pane into
    /// `is_active()` with a frame list nothing ever fills, which is a spinner in
    /// the loop transport that never finishes and a download queue serving
    /// nobody.
    ///
    /// It was `is_map` until cross-sections learned to loop, and widening it was
    /// the *whole* of the change here: the narrowing has always been about which
    /// panes something renders frames for, never about which panes draw a map.
    ///
    /// The active pane is a target unconditionally and is deliberately **never
    /// tested**. The caller is now the floating timeline, which runs after
    /// every `mem::take` window has closed, so the slot could safely be asked —
    /// but the unconditional include stays correct and stays put: it is the
    /// pane whose own toggle was clicked, and the timeline disables that
    /// toggle for an active pane that cannot loop, which is the same guarantee the old
    /// layers-panel host expressed by omitting the control. (When this ran
    /// from inside the panel's take window, asking the slot would have read a
    /// default `PaneState` — a *map* pane whatever the real one was — which is
    /// why the rule was born this way round.)
    fn loop_sync_targets(&self) -> Vec<usize> {
        self.time_sync_targets()
            .into_iter()
            .filter(|&idx| {
                idx == self.active_pane || self.panes.get(idx).is_none_or(PaneState::can_loop)
            })
            .collect()
    }

    /// Turn an overlay on or off for `pane` — **both halves**, which is the
    /// whole discipline.
    ///
    /// `render_overlay_controls_one` reloads the handlers from
    /// `overlay_configs` every frame and saves the enabled map back over
    /// `enabled_overlays`, so a change that never reached the config is
    /// undone on the next frame. Everything that flips a layer goes through
    /// here: [`Self::set_active_pane_overlay`] for callers outside a take
    /// window, and the stack's eye / the inspector's Show-toggle directly,
    /// with the pane they hold `mem::take`n out of the vector — where
    /// indexing `self.panes` would write the placeholder and lose the click.
    ///
    /// An associated function rather than a method so it can borrow the
    /// registry while the caller holds the taken pane.
    pub(super) fn write_pane_overlay(
        overlays: &mut OverlayRegistry,
        pane: &mut PaneState,
        kind: OverlayKind,
        on: bool,
    ) {
        if !pane.overlay_configs.is_empty() {
            overlays.load_pane_configs(&pane.overlay_configs);
        }
        overlays.set_enabled(kind, on);
        pane.overlay_configs = overlays.save_pane_configs();
        pane.enabled_overlays = overlays.save_enabled_map();
    }

    /// [`Self::write_pane_overlay`] plus the enable-fetch rule, in one place
    /// for its three callers — the stack's eye, the inspector's Show toggle
    /// and the catalog's tiles.
    ///
    /// The rule: a layer turned on with nothing to draw yet fetches now
    /// rather than waiting out an auto-poll interval — the same effect its
    /// own sub-toggles ask for, and the only route for a layer (SPC
    /// outlooks) that never auto-polls. `pane` is the caller's — taken or
    /// not — and `pane_idx` is the index the fetch is attributed to, because
    /// two of the callers hold the pane out of the vector where `active_pane`
    /// cannot be assumed to be it (the preset applier walks every pane).
    ///
    /// One fetch per kind per frame: a second enable of the same kind in the
    /// same batch (a preset enabling it on every pane) finds the first's
    /// action already queued and does not queue another — the handlers are
    /// global, so one fetch serves every pane.
    pub(super) fn set_pane_overlay_with_fetch(
        &mut self,
        pane: &mut PaneState,
        pane_idx: usize,
        kind: OverlayKind,
        on: bool,
        actions: &mut Vec<GuiAction>,
    ) {
        Self::write_pane_overlay(&mut self.overlays, pane, kind, on);
        if on
            && !self.overlays.has_data(kind)
            && !self.overlays.is_fetching(kind)
            && !actions
                .iter()
                .any(|a| matches!(a, GuiAction::FetchOverlay { kind: k, .. } if *k == kind))
        {
            actions.push(GuiAction::FetchOverlay { kind, pane_idx });
        }
    }

    /// [`Self::write_pane_overlay`] aimed at the active pane, for callers
    /// outside every `mem::take` window — the menu dispatcher, today.
    fn set_active_pane_overlay(&mut self, kind: OverlayKind, on: bool) {
        let mut pane = std::mem::take(&mut self.panes[self.active_pane]);
        Self::write_pane_overlay(&mut self.overlays, &mut pane, kind, on);
        self.panes[self.active_pane] = pane;
    }

    /// Select `kind`'s options in the inspector and make sure it is open —
    /// what a stack row click means (plan §3.8).
    pub(super) fn select_layer(&mut self, kind: OverlayKind) {
        self.insp_scroll_reset = self.inspector_sel != InspectorSelection::Layer(kind);
        self.inspector_sel = InspectorSelection::Layer(kind);
        self.insp_open = true;
    }

    /// Select the active pane's properties in the inspector and make sure it
    /// is open — the stack header's click, and the inspector crumb's `Pane N`
    /// segment. The pills are the primary pane-properties route now (each
    /// pill *is* one of the properties; the pane-number pill activates, the
    /// kind pill converts) — these two stay as harmless secondary ways into
    /// the inspector body that shows them all at once.
    pub(super) fn select_pane_props(&mut self) {
        self.insp_scroll_reset = self.inspector_sel != InspectorSelection::PaneProps;
        self.inspector_sel = InspectorSelection::PaneProps;
        self.insp_open = true;
    }

    /// Open the inspector on the App › Settings body — what the menu's
    /// Settings… entry does, and the state a `✕` deselect returns to.
    pub fn open_settings(&mut self) {
        self.insp_scroll_reset = self.inspector_sel != InspectorSelection::AppSettings;
        self.inspector_sel = InspectorSelection::AppSettings;
        self.insp_open = true;
    }

    /// Whether the settings body is on screen: the inspector is open and
    /// showing App › Settings.
    ///
    /// The successor to the old `show_settings` field, and the one thing the
    /// frontend still reads: the location-permission gate polls faster while
    /// this is true, so the pane that renders the permission is looking at a
    /// fresh copy of it.
    pub fn settings_visible(&self) -> bool {
        self.insp_open && self.inspector_sel == InspectorSelection::AppSettings
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
    ///
    /// # `viewing_live` and `time_step_secs` honour the pane's time-link
    ///
    /// The two time fields fan out only to panes whose
    /// [`PaneState::time_link`] is on (plan §3.7): unlink means *frozen*, and
    /// a sync pass that dragged an unlinked pane back to live would undo the
    /// freeze from a setting that is about layers. Every other field still
    /// converges unconditionally — an unlinked pane is parked in time, not
    /// exempt from the layout.
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
            // The one gated pair — see the method note.
            if p.time_link {
                p.viewing_live = active_viewing_live;
                p.time_step_secs = active_time_step_secs;
            }
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

    /// The 2D map panes whose render some 3D pane is standing on this frame,
    /// as rects in **points**.
    ///
    /// This is the mirror pass's guest list. The 3D view's map floor is the
    /// source pane's own render copied into an offscreen texture, and the copy
    /// is clipped to exactly these rects — so the sidebar, the top bar, the
    /// other panes' chrome and the 3D panes themselves never land in it. A box
    /// whose footprint reaches past its source pane's edge finds nothing
    /// there, which is correct: that is ground the source pane is not
    /// currently showing.
    ///
    /// Empty means there is nothing to mirror, and the frontend skips the pass
    /// entirely rather than clearing a texture nobody reads.
    ///
    /// A source pane that is **not a map** contributes nothing, and the kind is
    /// re-read from the live pane here rather than inferred from the presence
    /// of a recorded affine. `render_panes` already drops the entry when a pane
    /// stops being a map, so this is belt and braces — but this is a `pub`
    /// reader the frontend calls at a point in the frame of its own choosing,
    /// and copying a 3D pane's own chrome onto another pane's ground is not a
    /// failure anyone would think to look for in a guest list.
    pub fn mirror_source_rects(&self) -> Vec<egui::Rect> {
        let panes = self.panes();
        let mut rects: Vec<egui::Rect> = Vec::new();
        for pane in panes {
            let Some(volume) = pane.volume() else {
                continue;
            };
            if volume.hide_floor {
                continue;
            }
            let Some(geo) = volume
                .source_pane
                .filter(|&i| panes.get(i).map(PaneState::kind) == Some(PaneKind::Map))
                .and_then(|i| self.map_pane_geo.get(&i))
            else {
                continue;
            };
            // Two 3D panes sourced from one map ask for one rect. Compared by
            // value rather than deduped by pane index because the index is not
            // carried this far and the rect is what the pass actually uses.
            if !rects.contains(&geo.rect) {
                rects.push(geo.rect);
            }
        }
        rects
    }

    /// Tell the UI how much extra tile detail the 3D floor can actually show.
    ///
    /// The renderer's side of the same decision that sizes the pane mirror: a
    /// rung with no matching tile bias buys interpolation rather than detail,
    /// and a bias with no rung buys four times the fetches for nothing. Both
    /// come off one `MirrorPlan`, so they cannot disagree.
    pub fn set_floor_tile_zoom_bias(&mut self, bias: u8) {
        self.floor_tile_zoom_bias = bias;
    }

    /// The tile zoom bias for one map pane: the frame's bias if some 3D pane is
    /// standing on it, zero otherwise.
    ///
    /// Per-pane rather than global on purpose. The extra detail is only ever
    /// looked at through a floor, so a second map pane with no 3D view over it
    /// would pay four times the fetches — against the one
    /// `tile_source::TILE_CACHE_ENTRIES` LRU both panes share — for a picture
    /// the user is already seeing at its native scale.
    pub(crate) fn tile_zoom_bias_for_pane(&self, pane_idx: usize) -> u8 {
        if self.floor_tile_zoom_bias == 0 || !self.is_floor_source(pane_idx) {
            return 0;
        }
        if self.floor_tile_working_set(self.floor_tile_zoom_bias)
            > crate::tile_source::TILE_CACHE_ENTRIES.get()
        {
            return 0;
        }
        self.floor_tile_zoom_bias
    }

    /// Whether some 3D pane is standing on this map pane's render.
    fn is_floor_source(&self, pane_idx: usize) -> bool {
        self.panes().iter().any(|pane| {
            pane.volume()
                .is_some_and(|volume| !volume.hide_floor && volume.source_pane == Some(pane_idx))
        })
    }

    /// How many tiles every floor-source pane together would keep resident at
    /// `bias`, across the raster layers each of them draws.
    ///
    /// Summed over panes rather than checked per pane because
    /// `tile_source::TILE_CACHE_ENTRIES` is **one** LRU for the whole
    /// application: two source panes that each fit it individually still evict
    /// each other. This is what stops a bias being taken on a frame where it
    /// would thrash — a large window, or a split with two 3D views on two
    /// different maps.
    fn floor_tile_working_set(&self, bias: u8) -> usize {
        self.panes()
            .iter()
            .enumerate()
            .filter(|(idx, pane)| pane.is_map() && self.is_floor_source(*idx))
            .map(|(idx, pane)| {
                // The basemap always, the label tiles only when the layer is on.
                let layers = 1 + usize::from(pane.is_overlay_enabled(OverlayKind::CityLabels));
                let rect = self
                    .map_pane_geo
                    .get(&idx)
                    .map_or(egui::Rect::ZERO, |geo| geo.rect);
                crate::tiles::tiles_resident_for(rect, bias, layers)
            })
            .sum()
    }

    /// How many panes are *remembered*, including the ones the current split is
    /// not showing.
    ///
    /// Almost every caller wants [`panes`](Self::panes) instead: a hidden pane
    /// is not on screen, does not want a render dispatched for it and does not
    /// take part in any sync. The exception is the GPU-handle lifecycle.
    /// [`clear_graphics_state`](Self::clear_graphics_state) deliberately reaches
    /// every remembered pane — a handle belonging to a pane the user split away
    /// from is just as invalid once the context is gone — so whatever puts those
    /// handles *back* has to reach exactly as far, or a pane split away and
    /// split back to comes up holding a released texture and no way to ask for
    /// another.
    pub fn remembered_pane_count(&self) -> usize {
        self.panes.len()
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
        // Disarmed by committing, exactly as the section draw disarms on
        // drawing its line (`track_section_draw`): the mode's job is done, and
        // leaving it on would turn the user's next pan into a second box. A
        // too-small drag never reaches here — it is discarded on release and
        // the mode stays armed, so a mis-click still costs nothing.
        //
        // Through the setter, not a field write: `set_region_arm` is one of the
        // two chokepoints the modes' mutual exclusion lives in, and it is also
        // what drops a drag in flight. Disarming here moves `region_arm`, which
        // is how the menu's checkbox visibly un-ticks itself.
        self.set_region_arm(false);
        // The box means ground on the *source map's* radar, so the pane that
        // shows it has to follow that map's site and moment — exactly as
        // `apply_pending_section_line` writes them for a line. Without this, a
        // sourceless pane on another site would be "re-aimed" to resample its
        // own radar over a box centred on this map's ground: an empty or sliver
        // grid, captioned with the wrong site. Read before the destination is
        // resolved, as the section applier reads its source, and a source pane
        // that vanished mid-frame drops the region rather than siting it off a
        // pane that no longer exists.
        let (source_site, source_scan) = match self.panes.get(pending.source_pane) {
            Some(pane) => (pane.site.clone(), pane.scan_info.clone()),
            None => {
                log::warn!(
                    "pane {} committed a 3D region and is already gone",
                    pending.source_pane
                );
                return;
            }
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
        pane.site = source_site;
        pane.scan_info = source_scan;
        // A pane that has just been converted or grown has the default camera,
        // which is what should happen — but one that is being *re-aimed* keeps
        // the angle the user set, and its product stays its own, like the
        // camera: a 3D pane's product is picked on the pane, where a section
        // slices whatever its map shows. Beyond the site and moment, only the
        // region and its provenance are written. No stale-picture clearing is
        // needed here as it is for a section: the 3D dispatch is
        // level-triggered off `pane.site` each frame, and the site write makes
        // the wanted `VolumeTarget` differ from `rendered_for`, so the rebuild
        // follows on its own.
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
            // A handle drag in flight is a third gesture the same press cannot
            // also be. Arming from the menu mid-drag is contrived but cheap to
            // be correct about: the drag dies, the pane's line is untouched.
            self.section_edit_drag = None;
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

    /// The in-flight section handle drag, if any — the state both armed-drag
    /// setters must clear, and the tests' way of watching them do it.
    #[cfg(test)]
    pub(crate) fn section_edit_drag_for_test(
        &self,
    ) -> Option<crate::ui_section_edit::SectionEditDrag> {
        self.section_edit_drag
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
            // Same as `set_region_arm`: an endpoint drag cannot share a map
            // with an armed draw, and the mode was asked for last.
            self.section_edit_drag = None;
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

    /// Write a dropped handle's line onto the section pane it belongs to.
    ///
    /// Called from [`Self::ui`] after the pane loop, where every pane is back
    /// in the vector. The write is the line and **nothing else** — no target
    /// rule (the drop already names its pane), no growth, and deliberately no
    /// clearing of the picture on screen:
    ///
    /// # Why the old picture stands until the new cut lands
    ///
    /// [`Self::apply_pending_section_line`] blanks the pane, because a freshly
    /// drawn line can be across the state from the old one and a picture of
    /// somewhere else entirely is wrong for as long as it stands. A handle
    /// drop is an *adjustment*: the new line overlaps the old one's ground,
    /// the user's eyes are on the track they just moved, and this drop is the
    /// repeating step of walking a line through a storm — blanking to
    /// "Cutting the cross-section…" on every drop would strobe the pane
    /// exactly when the user is using it most. The stale picture stands for
    /// the fraction of a second the re-cut takes, the same way a section of
    /// the previous *volume* stands while its successor is cut, and the
    /// staleness key — which carries the line — is what notices and re-cuts
    /// without any help from here.
    fn apply_pending_section_edit(&mut self) {
        let Some((pane_idx, line)) = self.pending_section_edit.take() else {
            return;
        };
        let Some(section) = self
            .panes
            .get_mut(pane_idx)
            .and_then(|p| p.cross_section_mut())
        else {
            // A pane-count change or a conversion in the same frame. Dropped
            // rather than retargeted: re-aiming a pane the user did not drag
            // on is worse than losing an adjustment they can repeat.
            log::warn!("pane {pane_idx} is no longer a section pane; dropping the edited line");
            return;
        };
        section.line = Some(line);
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

    /// The pane borders the last frame painted: pane index, the stroke's
    /// painted bounds, and whether it was the active highlight.
    #[cfg(test)]
    pub(crate) fn pane_borders_for_test(&self) -> &[(usize, egui::Rect, bool)] {
        &self.last_pane_borders
    }

    /// The section tracks the last frame painted: map pane, section pane,
    /// and the painted A and B endpoints.
    #[cfg(test)]
    pub(crate) fn section_tracks_for_test(&self) -> &[(usize, usize, egui::Pos2, egui::Pos2)] {
        &self.last_section_tracks
    }

    /// Pane `idx`'s dispatched kinds in paint order, with the layer each
    /// painted into — the draw-order pin's read side.
    #[cfg(test)]
    pub(crate) fn paint_order_for_test(&self, idx: usize) -> Vec<(OverlayKind, egui::LayerId)> {
        self.last_paint_order
            .iter()
            .find(|(pane, _)| *pane == idx)
            .map(|(_, order)| order.clone())
            .unwrap_or_default()
    }

    /// The Volume Alpha corner buttons the last frame drew, per pane.
    #[cfg(test)]
    pub(crate) fn alpha_buttons_for_test(&self) -> &[(usize, egui::Rect)] {
        &self.last_alpha_buttons
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

    /// Whether pane `idx` is a pane the **loop** machinery must skip: it exists,
    /// and its kind has no picture a loop can hold ([`PaneKind::can_loop`]).
    ///
    /// The sibling of [`Self::pane_has_no_plan_view`], and the distinction
    /// between them is the whole reason both exist. That one asks "does this
    /// pane draw an `IMAGE_SIZE` square raster of one tilt?" and gates the
    /// plan-view dispatch, the static sibling broadcast and the suspend/resume
    /// restore. This one asks "can a sequence of this pane's pictures be
    /// animated?" and gates the loop dispatch, the loop-frame broadcast, the
    /// readiness settle and the playback start. A cross-section pane answers
    /// *yes* to the first question's negation and *no* to this one's: it has no
    /// plan view and it can loop, and collapsing the two would either stop it
    /// looping or hand it a plan-view raster.
    ///
    /// Written in the negative, and an index past the end answers `false`, for
    /// exactly the reasons the sibling gives — each caller keeps its own
    /// out-of-range handling rather than having a second question folded in.
    ///
    /// The `mem::take` caveat on [`Self::pane`] applies in full.
    pub fn pane_cannot_loop(&self, idx: PaneId) -> bool {
        self.pane(idx).is_some_and(|pane| !pane.can_loop())
    }

    /// Whether the storm motion vector is being edited *right now*, so that a
    /// consumer which spends real work on a change can wait for the release.
    ///
    /// # Commit on release, and why this control needs it when the others do
    /// not
    ///
    /// Every other setting that invalidates a render is a click: a product, a
    /// tilt, a checkbox. This one is a `DragValue`, and a drag produces a new
    /// value *every frame*. `App::apply_storm_motion_override` answers a change
    /// by evicting every storm-relative grid and section, so a two-second drag
    /// used to evict and rebuild them sixty times over — 210 ms of re-cut per
    /// drag frame for a cross-section, and for a 3D loop the whole resident
    /// set: fourteen grids, ~2 s of resample, discarded and restarted on the
    /// next frame, for ever, so the loop would never finish building while a
    /// finger was on the widget.
    ///
    /// Holding the commit until the drag ends makes the cost proportional to
    /// the *edit* rather than to how long it took: one eviction and one
    /// rebuild, whatever route the number took to get there. The picture on
    /// screen goes on showing the previous vector until then, which is the
    /// honest state — it is what the data was derived with — and the widget
    /// shows the new number, so nothing claims otherwise.
    ///
    /// Deliberately not "the value has stopped changing for N frames": a
    /// timeout would fire mid-drag on a slow frame and would make the commit a
    /// function of frame rate.
    pub fn storm_motion_mid_edit(&self) -> bool {
        self.storm_motion_editing
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

    /// The shared tile sources, for a consumer outside the pane draw loop.
    ///
    /// The 3D floor's map composite reads tile *bytes* through this
    /// (`HttpsTiles::raster_bytes_at`) so the box's ground carries the same
    /// basemap and city-label tiles the 2D panes draw — the sources are the
    /// panes' own, so there is one cache, one fetch queue and one attribution
    /// story however many consumers stand on them.
    pub fn map_tiles_mut(&mut self) -> &mut MapTileState {
        &mut self.map_tiles
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

    /// What the last frame's timeline transport drew.
    #[cfg(test)]
    pub(crate) fn timeline_for_test(&self) -> &TimelineProbe {
        &self.last_timeline
    }

    /// What the last frame's top bar drew.
    #[cfg(test)]
    pub(crate) fn top_bar_for_test(&self) -> &TopBarProbe {
        &self.last_top_bar
    }

    /// What the last frame's bottom bar drew.
    #[cfg(test)]
    pub(crate) fn bottom_bar_for_test(&self) -> &BottomBarProbe {
        &self.last_bottom_bar
    }

    /// What the last frame's phone sheet drew.
    #[cfg(test)]
    pub(crate) fn sheet_for_test(&self) -> &SheetProbe {
        &self.last_sheet
    }

    /// What the last frame's phone error toast drew, if it drew.
    #[cfg(test)]
    pub(crate) fn error_toast_for_test(&self) -> Option<ErrorToastProbe> {
        self.last_error_toast
    }

    /// Open or close the sheet's Menu page directly, for the chain tests
    /// that build the full page stack without walking the bottom bar.
    #[cfg(test)]
    pub(crate) fn set_sheet_menu_open_for_test(&mut self, open: bool) {
        self.menu_open = open;
    }

    /// What the last frame's layer stack drew.
    #[cfg(test)]
    pub(crate) fn stack_for_test(&self) -> &StackProbe {
        &self.last_stack
    }

    /// What the last frame's inspector drew.
    #[cfg(test)]
    pub(crate) fn inspector_for_test(&self) -> &InspectorProbe {
        &self.last_inspector
    }

    /// What the last frame's Add-layer catalog drew.
    #[cfg(test)]
    pub(crate) fn catalog_for_test(&self) -> &CatalogProbe {
        &self.last_catalog
    }

    /// What the last frame's pill rows drew, in pane order.
    #[cfg(test)]
    pub(crate) fn pill_rows_for_test(&self) -> &[pills::PillRowProbe] {
        &self.last_pills
    }

    /// The pill popover the last frame drew, if one was open.
    #[cfg(test)]
    pub(crate) fn pill_popover_for_test(&self) -> Option<&pills::PillPopoverProbe> {
        self.last_pill_popover.as_ref()
    }

    /// Whether some feature consumed the last frame's map click — see the
    /// `click_consumed_frame` field.
    #[cfg(test)]
    pub(crate) fn click_consumed_for_test(&self) -> bool {
        self.click_consumed_frame
    }

    /// The user's saved presets, as the catalog holds them.
    #[cfg(test)]
    pub(crate) fn presets_for_test(&self) -> &[PresetConfig] {
        &self.presets
    }

    /// How many handler-control passes the last frame ran. The harness holds
    /// this to at most one after every frame — see the field.
    #[cfg(test)]
    pub(crate) fn control_render_passes_for_test(&self) -> u32 {
        self.control_render_passes
    }

    /// Open or close the Set Time dialog directly, for fixtures that need a
    /// centred floating dialog over the map — the settings window used to be
    /// the convenient one, and it is a side panel now.
    #[cfg(test)]
    pub(crate) fn set_time_dialog_open_for_test(&mut self, open: bool) {
        self.time_dialog.show = open;
    }

    /// Open or close the Add-layer catalog directly, for fixtures stacking
    /// layers the UI routes cannot stack — the Esc-chain walk opens it under
    /// a feature popup and a time dialog, whose windows would swallow the
    /// clicks the UI route needs.
    #[cfg(test)]
    pub(crate) fn set_catalog_open_for_test(&mut self, open: bool) {
        self.catalog_open = open;
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

    /// The global viewport-sync toggle, for the Sync popover's pin.
    #[cfg(test)]
    pub(crate) fn viewport_sync_for_test(&self) -> bool {
        self.viewport_sync
    }

    /// Set one pane's overlay state, writing the config as well as the enabled
    /// map — `render_overlay_controls_one` reloads the handlers from the config
    /// every frame it runs, so a write to `enabled_overlays` alone is undone.
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

    /// Open or close the layers drawer, as the top bar's Layers toggle does
    /// below the sidebar breakpoint.
    #[cfg(test)]
    pub(crate) fn set_drawer_open(&mut self, open: bool) {
        self.drawer_open = open;
    }

    /// Every handler dropdown the last frame drew. See [`DrawnDropdown`].
    #[cfg(test)]
    pub(crate) fn dropdowns_for_test(&self) -> &[DrawnDropdown] {
        &self.last_dropdowns
    }

    /// Every control item the last frame drew, whatever its shape. See
    /// [`DrawnControlItem`].
    #[cfg(test)]
    pub(crate) fn control_items_for_test(&self) -> &[DrawnControlItem] {
        &self.last_control_items
    }

    /// The [`ControlItem`] tree `kind`'s handler is currently offering — the
    /// *model* behind the [`DrawnControlItem`]s, asked of the handler rather
    /// than of the renderer, exactly as [`Self::dropdown_model_for_test`] asks
    /// for one dropdown.
    #[cfg(test)]
    pub(crate) fn control_item_model_for_test(&self, kind: OverlayKind) -> Vec<ControlItem> {
        let ctx = self.active_pane_control_context();
        self.overlays.controls(kind, &ctx)
    }

    /// Every settings row the last frame drew. See
    /// [`settings::DrawnSettingsRow`].
    #[cfg(test)]
    pub(crate) fn settings_rows_for_test(&self) -> &[settings::DrawnSettingsRow] {
        &self.last_settings_rows
    }

    /// What the last frame's detail popup did with its action buttons:
    /// `(triggered, handled)` indices. See the note on the handling in
    /// `ui_popups.rs` for why the second must hold at most one entry.
    #[cfg(test)]
    pub(crate) fn popup_actions_for_test(&self) -> (Vec<usize>, Vec<usize>) {
        (
            self.last_popup_triggered.clone(),
            self.last_popup_handled.clone(),
        )
    }

    /// The `(options, selected)` a handler is currently offering under `label`
    /// — the *model* behind a [`DrawnDropdown`], asked of the handler rather
    /// than of the renderer.
    #[cfg(test)]
    pub(crate) fn dropdown_model_for_test(
        &self,
        label: &str,
    ) -> Option<(Vec<(String, String)>, String)> {
        let ctx = self.active_pane_control_context();
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

    /// Tell the UI this build's loop frame cap (`constants::MAX_LOOP_FRAMES`),
    /// so the timeline's row-2 caption states the platform's real budget.
    /// Pushed in like [`set_supports_exit`](Self::set_supports_exit) and for
    /// the same reason: the constant lives in a crate that depends on this
    /// one.
    pub fn set_loop_frame_budget(&mut self, frames: usize) {
        self.loop_frame_budget = frames;
    }

    /// Set the user's GPS location for the blue dot indicator.
    pub fn set_gps_fix(&mut self, fix: rustdar_gps::GpsFix) {
        self.user_fix = Some(fix);
        self.user_fix_at = Some(web_time::Instant::now());
    }

    /// See [`set_gps_fix`](Self::set_gps_fix).
    pub fn gps_fix(&self) -> Option<&rustdar_gps::GpsFix> {
        self.user_fix.as_ref()
    }

    /// Take the blue dot off the map.
    ///
    /// For the case the dot has no other answer to: the user has withdrawn
    /// consent, or turned location off, and the last position delivered under
    /// the old permission is still on screen. Leaving it there is worse than a
    /// stale label — it is the app showing a position it has just been told it
    /// may not know.
    pub fn clear_gps_fix(&mut self) {
        self.user_fix = None;
        self.user_fix_at = None;
    }

    /// Cache what the platform location service is doing, for the settings
    /// pane to render.
    ///
    /// Pushed in rather than queried: this crate cannot name a
    /// `PlatformBridge`. See the fields.
    pub fn set_location_state(
        &mut self,
        permission: rustdar_gps::LocationPermission,
        active: bool,
    ) {
        self.location_permission = permission;
        self.location_active = active;
    }

    /// See [`set_location_state`](Self::set_location_state).
    pub fn location_permission(&self) -> rustdar_gps::LocationPermission {
        self.location_permission
    }

    /// See [`set_location_state`](Self::set_location_state).
    pub fn location_active(&self) -> bool {
        self.location_active
    }

    /// Cache whether this platform has a location settings page to offer.
    ///
    /// Separate from [`set_location_state`](Self::set_location_state) because
    /// it is answered once, at startup: the permission changes, the platform
    /// does not.
    pub fn set_location_settings_available(&mut self, available: bool) {
        self.location_settings_available = available;
    }

    /// See [`set_location_settings_available`](Self::set_location_settings_available).
    pub fn location_settings_available(&self) -> bool {
        self.location_settings_available
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
                frame.image = None;
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
mod chunk_scan_info_tests;

#[cfg(test)]
mod pane_slice_tests;

#[cfg(test)]
mod storm_motion_override_tests;
