use crate::actions::{GuiAction, RadarConfig};
use rustdar_overlays::render::controls::{
    ControlEffect, ControlItem, ControlUpdate, ControlValue, PaneControlContext,
    PaneControlContextMut,
};

const DEFAULT_INITIAL_ZOOM: f64 = 7.0;

use crate::pane::{ColorScaleOrientation, PaneId, PaneLayout, PaneState};
use crate::tiles::MapTileState;
use crate::ui_layout::{LayoutCtx, ModalityLatch};
use chrono::Timelike;
use egui::Context;
use rustdar_overlays::render::overlay_state::{OverlayKind, OverlayRegistry};
use rustdar_radar::types::{RadarProduct, ScanInfo};
use rustdar_units::UserPreferences;

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
/// What the menu presentations actually drew last frame, for the input harness.
#[cfg(test)]
pub(crate) use ui_menu::DrawnMenuLeaf;
#[path = "ui_map.rs"]
mod map;
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

/// Time editing dialog state.
pub(super) struct TimeDialogState {
    pub date_string: String,
    pub time_string: String,
    pub show: bool,
}

pub struct Gui {
    radar: RadarState,
    auto_poll: AutoPollState,
    /// See [`Gui::live_chunks_enabled`].
    live_chunks: bool,
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
    /// by tests, which need the same rects `render_map` used.
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
    /// The pointer state `render_map` resolved for each pane on the last frame,
    /// in pane order. Only read by tests — and the *only* honest way for one to
    /// observe the modality gate, since resolving it a second time alongside
    /// `Gui::ui` would assert on a replica.
    #[cfg(test)]
    last_pane_pointers: Vec<crate::ui_input::PanePointerProbe>,
    /// The pane-count buttons the picker actually drew last frame. Only read by
    /// tests, which check the picker narrows on a phone while the config clamp
    /// does not, and that clicking one takes effect.
    #[cfg(test)]
    last_pane_options: Vec<PaneOptionProbe>,
    /// The excluded rects `render_map` was actually handed. Only read by tests,
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
            last_pane_options: Vec::new(),
            #[cfg(test)]
            last_map_excluded_rects: Vec::new(),
            #[cfg(test)]
            last_status_bar: StatusBarProbe::default(),
            #[cfg(test)]
            last_dropdowns: Vec::new(),
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

        actions.extend(self.render_map(&mut root_ui, &chrome.excluded_rects));

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
        false
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
                    self.panes[self.active_pane] = std::mem::take(pane);
                    let active_site = self.panes[self.active_pane].site.clone();
                    let active_scan_info = self.panes[self.active_pane].scan_info.clone();
                    while self.panes.len() < count {
                        let mut new_pane = PaneState::with_site(active_site.clone());
                        new_pane.scan_info = active_scan_info.clone();
                        self.panes.push(new_pane);
                    }
                    // A pane born here has empty overlay maps, and
                    // `is_overlay_enabled` reads a missing entry as *off* — so
                    // with layer sync disabled it would draw no overlays at
                    // all, Radar included. Seed it from the handlers, which
                    // hold the active pane's state (reloaded at the end of
                    // every frame in `Gui::ui`), the same way startup does.
                    self.initialize_pane_enabled();
                    self.pane_layout = PaneLayout::for_count(count);
                    if self.active_pane >= count {
                        self.active_pane = 0;
                    }
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
    fn render_layer_controls(
        &mut self,
        ui: &mut egui::Ui,
        pane: &mut PaneState,
        combo_width: f32,
        id_prefix: &str,
        actions: &mut Vec<GuiAction>,
    ) {
        self.render_radar_controls(ui, pane, combo_width, id_prefix);

        // --- Time navigation (forward/back/live) ---
        self.render_time_navigation(ui, pane, id_prefix, actions);

        // --- Radar loop controls ---
        self.render_loop_controls(ui, pane, actions);

        ui.add_space(6.0);
        ui.separator();

        // --- Handler-backed overlay controls (generic) ---
        self.render_overlay_controls(ui, pane, actions);

        ui.add_space(6.0);
        ui.separator();

        // --- Viewport sync ---
        if self.pane_layout.pane_count > 1 {
            ui.checkbox(&mut self.viewport_sync, "\u{1f517}  Sync Viewports");
            ui.checkbox(&mut self.sync_layers, "\u{1f517}  Sync Layers");
            ui.separator();
        }
    }

    /// Render radar product/elevation combo boxes (shown when Radar is enabled).
    fn render_radar_controls(
        &self,
        ui: &mut egui::Ui,
        pane: &mut PaneState,
        combo_width: f32,
        id_prefix: &str,
    ) {
        if pane.is_overlay_enabled(OverlayKind::Radar) {
            ui.indent(format!("{id_prefix}radar_controls"), |ui| {
                if let Some(scan_info) = &pane.scan_info {
                    let prev_product = pane.selected_product;
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
                    if prev_product != pane.selected_product {
                        pane.selected_elevation = 0.0;
                    }

                    if let Some(elevations) =
                        scan_info.product_elevations.get(&pane.selected_product)
                        && !elevations.is_empty()
                    {
                        let selected_angle = elevations
                            .iter()
                            .min_by(|a, b| {
                                ((**a - pane.selected_elevation).abs())
                                    .total_cmp(&((**b - pane.selected_elevation).abs()))
                            })
                            .copied()
                            .unwrap_or(0.0);

                        egui::ComboBox::from_id_salt(format!("{id_prefix}elev_sel"))
                            .selected_text(format!("{:.1}\u{b0}", selected_angle))
                            .width(combo_width)
                            .show_ui(ui, |ui| {
                                for angle in elevations.iter() {
                                    ui.selectable_value(
                                        &mut pane.selected_elevation,
                                        *angle,
                                        format!("{:.1}\u{b0}", angle),
                                    );
                                }
                            });
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
    fn loop_sync_targets(&self) -> Vec<usize> {
        if self.sync_layers && self.pane_layout.pane_count > 1 {
            (0..self.pane_layout.pane_count).collect()
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

        // Sync per-pane fields including enabled overlays, configs, and radar product/elevation.
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
    pub fn any_pane_has_overlay_enabled(&self, kind: OverlayKind) -> bool {
        self.panes
            .iter()
            .take(self.pane_layout.pane_count)
            .any(|p| p.is_overlay_enabled(kind))
    }

    /// Returns the index of the first pane that has the given overlay kind enabled,
    /// or `None` if no pane has it enabled.
    pub fn first_pane_with_overlay_enabled(&self, kind: OverlayKind) -> Option<usize> {
        self.panes
            .iter()
            .take(self.pane_layout.pane_count)
            .position(|p| p.is_overlay_enabled(kind))
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

    /// The pointer state `render_map` resolved for each pane last frame.
    #[cfg(test)]
    pub(crate) fn pane_pointers_for_test(&self) -> &[crate::ui_input::PanePointerProbe] {
        &self.last_pane_pointers
    }

    /// The pane-count buttons the picker drew on the last frame.
    #[cfg(test)]
    pub(crate) fn pane_options_for_test(&self) -> &[PaneOptionProbe] {
        &self.last_pane_options
    }

    /// The excluded rects `render_map` was handed on the last frame.
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
    /// `render_map` computes them.
    #[cfg(test)]
    pub(crate) fn pane_rects_for_test(&self) -> Vec<egui::Rect> {
        let panel = self.last_map_panel_rect;
        (0..self.pane_layout.pane_count)
            .map(|idx| self.pane_layout.pane_rect(idx, panel))
            .collect()
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
        }
        self.map_tiles.clear();
    }

    /// Propagate the interacted pane's viewport (zoom + position) to all other panes.
    ///
    /// Bounded by [`Self::visible_pane_count`], not the layout's raw count:
    /// hidden panes are neither read as a sync source nor written to, and a
    /// count that ran ahead of the vector cannot index past its end.
    fn sync_viewports(&mut self, pre_zooms: &[f64], pre_positions: &[Option<walkers::Position>]) {
        let pane_count = self.visible_pane_count();
        if !self.viewport_sync || pane_count <= 1 {
            return;
        }
        let mut source_idx = None;
        for idx in 0..pane_count {
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
        let src = source_idx.unwrap_or(self.active_pane);
        let zoom = self.panes[src].map_memory.zoom();
        let pos = self.panes[src].map_memory.detached();
        for idx in 0..pane_count {
            if idx != src {
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
        gui.pane_layout = PaneLayout::for_count(4);

        assert_eq!(gui.panes().len(), 1);
        assert_eq!(gui.panes_mut().len(), 1);
    }

    /// `sync_viewports` reads and writes panes by raw index, so it takes its
    /// bound from the visible slice rather than the layout's claim — with the
    /// raw count, the same ran-ahead layout as above panicked mid-frame.
    #[test]
    fn viewport_sync_never_outruns_the_pane_vector() {
        let mut gui = Gui::new();
        gui.set_pane_count_for_test(2);
        gui.viewport_sync = true;
        gui.pane_layout = PaneLayout::for_count(4);

        // Snapshots sized to the layout's claim, exactly as `render_map` would
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
