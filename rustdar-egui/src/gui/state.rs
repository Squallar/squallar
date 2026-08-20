//! The [`Gui`] shell's state: the struct, its state types and its
//! constructor. Every field is `pub(super)` — visible throughout `mod ui`'s
//! subtree — and the thirty test-only probe fields live behind the one gated
//! `probes` field.

#[cfg(test)]
use super::probes::FrameProbes;
use super::*;

pub struct Gui {
    pub(super) radar: RadarState,
    /// **What each layer says it is doing**, re-stated each frame by the App.
    ///
    /// Opaque by construction (WO-E8c): an entry is a [`LayerId`] and the
    /// layer's own payload, and the only readers are the layers' own glue
    /// modules. This replaced two radar-shaped fields — the chunk-feed status
    /// and the per-site volume stamps — which is why nothing here names
    /// either.
    pub(super) liveness: Vec<rustdar_source::liveness::SourceLiveness>,
    pub(super) time_dialog: TimeDialogState,
    pub(super) initial_zoom_set: bool,
    pub(super) map_tiles: MapTileState,
    pub(super) user_fix: Option<rustdar_location::Fix>,
    /// When [`user_fix`](Self::user_fix) arrived.
    pub(super) user_fix_at: Option<web_time::Instant>,
    /// What the OS last said about this app's access to the user's location,
    /// pushed in by the frontend's location gate.
    pub(super) location_permission: rustdar_location::LocationPermission,
    /// Whether the platform is currently delivering location fixes. A different
    /// question from the permission: every desktop process starts granted and
    /// silent.
    pub(super) location_active: bool,
    /// Whether this platform has a location settings page to offer.
    pub(super) location_settings_available: bool,
    /// Whether the site list is still only what this install has decoded,
    /// rather than the network.
    pub(super) catalogue_pending: bool,
    // Compass heading in degrees (0–360), from device compass sensor
    pub(super) user_heading: Option<f32>,
    pub overlays: OverlayRegistry,
    pub(super) panes: Vec<PaneState>,
    pub(super) active_pane: PaneId,
    pub(super) pane_layout: PaneLayout,
    /// Remembered color-scale bar orientation for the map panel (hysteresis, so
    /// a resize near the boundary cannot make the bars hop).
    pub(super) color_scale_orientation: ColorScaleOrientation,
    /// Each 3D pane's own map strip: the Mercator affine it drew that map
    /// through, and the off-screen rect it drew it *into*. This is both the
    /// registration the pane's floor is reprojected by and the rect the
    /// frontend clips its mirror pass to.
    pub(super) map_pane_geo: HashMap<usize, crate::volume_view::MapPaneGeo>,
    /// Why each 3D pane drew **no picture** on the last frame, by pane index.
    pub(super) volume_empty_states: HashMap<usize, String>,
    /// How much of egui's coordinate space the pane mirror has to cover, in
    /// points, as of the last frame: the frame itself, plus however far below
    /// it this frame's off-screen map strips reach. See
    /// [`Gui::mirror_size_points`].
    pub(super) mirror_size_points: egui::Vec2,
    /// How many slippy zoom levels deeper a **floor-source** map pane should
    /// fetch its raster tiles, from the renderer's last mirror plan.
    pub(super) floor_tile_zoom_bias: u8,
    /// Whether some feature consumed this frame's map click — written by the
    /// pane loop, read by [`Self::apply_fade_toggle`] (a consumed click while
    /// faded unfades; see `ui_fade.rs`) and by the harness's probe.
    pub(super) click_consumed_frame: bool,
    /// A pane the user has asked to convert, applied once the UI pass is over.
    pub(super) pending_pane_view: Option<(PaneId, rustdar_radar::types::RenderView)>,
    /// Whether the cross-section draw is **armed**: the next drag on a map pane
    /// is a section line rather than a pan.
    pub(super) section_draw_armed: bool,
    /// The in-flight draw: where it started, on which pane, and where the
    /// pointer is now.
    pub(super) section_anchor: Option<SectionAnchor>,
    /// A finished line and the map pane it was drawn on, applied **after** the
    /// pane loop.
    pub(super) pending_section_line: Option<(PaneId, crate::pane::SectionLine)>,
    /// Whether the 3D region pick is **armed**: the next drag on a map pane
    /// draws the square of ground a 3D view will resample, rather than panning.
    pub(super) region_pick_armed: bool,
    /// The in-flight box: which pane it is being dragged on, where its centre
    /// was fixed, and how wide it currently stands. See
    /// [`crate::ui_region::RegionDrag`].
    pub(super) region_drag: Option<crate::ui_region::RegionDrag>,
    /// A finished region and the map pane it was dragged on, applied **after**
    /// the pane loop.
    pub(super) pending_region: Option<(PaneId, crate::pane::VolumeRegion)>,
    /// An endpoint drag in flight on a committed section's ground track, or
    /// `None`.
    pub(super) section_edit_drag: Option<crate::ui_section_edit::SectionEditDrag>,
    /// Where every committed line's grabbable geometry was drawn **last
    /// frame**, in screen points — endpoints and body track alike.
    pub(super) section_handles: Vec<crate::ui_section_edit::SectionGrabZone>,
    /// A dropped handle's line and the section pane it belongs to, applied
    /// **after** the pane loop.
    pub(super) pending_section_edit: Option<(PaneId, crate::pane::SectionLine)>,
    // The Gui-global `viewport_sync` / `sync_layers` toggles were retired in
    // M11: sync is per pane now — `PaneState::viewport_link`,
    // `PaneState::layer_link`, `PaneState::time_link` — and the old globals
    // survive only as read-only legacy fields on `UiConfig`, which seed the
    // per-pane links once on load (see `load_ui_config`).
    /// How far back (in seconds) to fetch historical scans for the loop.
    pub loop_lookback_secs: u64,
    /// Animation speed in frames per second.
    pub loop_speed_fps: f32,
    /// Whether the slide-out layers drawer is open. Only consulted when the
    /// layout has no persistent sidebar.
    pub(super) drawer_open: bool,
    /// The user's explicit say over the Expanded layers sidebar, from the top
    /// bar's Layers toggle. `None` is the shell default — open where the
    /// sidebar is persistent — and, like `drawer_open`, it is deliberately
    /// session-only: how a session left its panels is not a preference.
    pub(super) stack_open: Option<bool>,
    /// Whether the inspector panel is open. Session-only, on the same
    /// precedent as `drawer_open`: closed by default at every width, opened
    /// by the top bar's ⚙ toggle, a stack row click ([`Self::select_layer`]),
    /// or the menu's Settings… entry.
    pub(super) insp_open: bool,
    /// One-shot: the next inspector frame starts its body scrolled to the
    /// top. Set by every selection change, because the three bodies share one
    /// scroll area — its offset is the *panel's* memory, and carrying a deep
    /// settings scroll into a freshly selected layer's options would open
    /// them somewhere in the middle.
    pub(super) insp_scroll_reset: bool,
    /// What the inspector's body is about while it is open — and what it will
    /// be about when next opened. Session-only, defaults to
    /// [`InspectorSelection::AppSettings`]; a dismissal resets it there (see
    /// [`Self::dismiss_top_layer`]), while the ⟩ collapse deliberately keeps
    /// it, because a collapse is not a deselection.
    pub(super) inspector_sel: InspectorSelection,
    /// Whether the floating timeline transport is collapsed to its 🕐 chip.
    pub(super) timeline_collapsed: bool,
    /// Whether the transport's second row — the loop tuning — is shown.
    pub(super) timeline_row2: bool,
    /// The archive scrubber's in-flight drag position, as a fraction of the
    /// lookback window, or `None` when no drag is in flight. Remembered
    /// across frames so the handle follows the pointer instead of snapping
    /// back to the resting position every frame; the commit happens once, on
    /// release — see `render_timeline_scrubber`.
    pub(super) timeline_scrub: Option<f32>,
    /// Whether the floating status bar is collapsed to its ⏵ restore button.
    pub(super) statusbar_collapsed: bool,
    /// The floating status bar's rect as drawn this frame, `None` while no
    /// bar is on screen (Compact, or fully faded). Written by
    /// `render_status_bar` before the timeline pass reads it: the collapsed
    /// time chip anchors above the bar's real top edge rather than a guessed
    /// constant (the M8 chip-overlap fix) — and only when it would otherwise
    /// land on the bar, since a bar collapsed to its restore button leaves
    /// the corner open map (M8.1).
    pub(super) statusbar_rect: Option<egui::Rect>,
    /// How long until the text the status bar's auto-poll chip drew this frame
    /// would read differently, or `None` when nothing it drew restates the
    /// clock.
    pub(super) status_bar_tick: Option<std::time::Duration>,
    /// Whether the Add-layer catalog is open. Session-only, like every other
    /// open-surface flag; opened by the stack's two `+ Add layer` buttons and
    /// closed by applying a tile, the `✕`, the backdrop, or
    /// [`Self::dismiss_top_layer`].
    pub(super) catalog_open: bool,
    /// The catalog's search text. Session-only: a filter is a gesture in
    /// progress, not a preference.
    pub(super) catalog_query: String,
    /// The name being typed into the catalog's "Save current view…" tile,
    /// and whether that inline editor is showing. Session-only, same terms.
    pub(super) catalog_save_name: String,
    /// See [`Self::catalog_save_name`].
    pub(super) catalog_saving: bool,
    /// The site list's search text — the inspector body and the site pill's
    /// popover filter through the one field, as they render the one list.
    pub(super) site_query: String,
    /// The stack row being drag-reordered by its grip, if one is in flight.
    pub(super) stack_drag: Option<rustdar_source::id::LayerId>,
    /// The pane whose pill row a first touch tap revealed, if any.
    pub(super) pill_revealed: Option<PaneId>,
    /// How many pill rows the previous pills pass drew. The rows' areas are
    /// keyed on contiguous `0..pane_count`, so this count *is* the set of
    /// rows on screen last frame — and a pass drawing past it is a debut,
    /// which egui auto-tops. Session-only bookkeeping.
    pub(super) pills_drawn_last_frame: usize,
    /// A panel raise owed to the next pills pass — armed by every rows'
    /// debut (startup, and any mid-session pane growth), performed one frame
    /// later; see `ui_pills.rs`'s module note on stacking for why the raise
    /// cannot happen on the debut frame itself. Session-only bookkeeping.
    pub(super) pills_raise_pending: bool,
    /// Whether the pane pill rows render at full opacity unconditionally.
    pub(super) pin_pane_controls: bool,
    /// Whether the floating chrome is faded away — the map-first
    /// state one qualifying click enters and the next one leaves. Session-only
    /// like every open-surface flag: hiding the UI is a gesture, not a
    /// preference. Everything about it lives in `ui_fade.rs`.
    pub(super) ui_faded: bool,
    /// The pane loop's verdict that this frame's click qualifies as the fade
    /// gesture — recorded in `render_panes` (which alone knows the click's
    /// pane, kind and consumption), resolved by [`Self::apply_fade_toggle`]
    /// after the pending appliers. One-shot per frame.
    pub(super) fade_candidate: bool,
    /// Whether the most recent primary press was the one that switched the
    /// active pane — written by `detect_active_pane_click` on every press,
    /// read by the fade trigger so a first click on an inactive pane only
    /// activates it (the plan). Session-only bookkeeping.
    pub(super) press_switched_pane: bool,
    /// Whether an egui popup — a pill popover, the ☰ dropdown, an open combo
    /// — was open when the most recent primary press landed. Written beside
    /// [`Self::press_switched_pane`], read by the fade trigger: a click
    /// whose press found a popup open is that popup's dismissal (egui closes
    /// it on the click outside), not a fade gesture. Recorded at press time
    /// because by the time the click confirms — the release, or a touch
    /// tap's deferral later — the popup has already closed and the frame
    /// can no longer see what the press was aimed at.
    pub(super) press_popup_open: bool,
    /// This frame's shared chrome opacity, resolved once at frame top by
    /// [`Self::enforce_fade_invariants`] from the fade animation: `1.0` fully
    /// present, `0.0` fully faded (surfaces skip rendering), in between a
    /// non-interactive transition. See `ui_fade.rs`.
    pub(super) fade_factor: f32,
    /// The page the sheet last showed — what the sheet's fall animation
    /// renders after the flags have already closed (`ui_sheet.rs`); never
    /// read while a page is open. Session-only bookkeeping.
    pub(super) sheet_last_page: Option<sheet::SheetPage>,
    /// The message the error toast last showed — what the toast's fade-out
    /// renders after the error has already cleared (`ui_sheet.rs`), on the
    /// same terms as [`Self::sheet_last_page`]; never read while an error is
    /// up. Session-only bookkeeping.
    pub(super) toast_last_error: Option<String>,
    /// The user's saved presets (the plan). Persisted; the built-ins are
    /// compiled in beside them (`catalog::builtin_presets`) and never saved.
    pub(super) presets: Vec<PresetConfig>,
    /// This build's loop frame cap, pushed in by the frontend from
    /// `constants::MAX_LOOP_FRAMES` — this crate cannot read that table (the
    /// dependency points the other way), and the timeline's row-2 caption
    /// wants to state the platform's real budget rather than a guess.
    pub(super) loop_frame_budget: usize,
    /// Whether the top bar's ☰ dropdown was open on the last frame it drew.
    pub(super) menu_popup_open: bool,
    /// A dismiss was consumed against the open dropdown; the top bar honours
    /// this (and clears it) by force-closing the popup before next showing it.
    pub(super) menu_popup_close_requested: bool,
    /// Whether the phone sheet's Menu page is open. Session-only, on the
    /// `drawer_open` precedent — and Compact-only chrome: the ☰ Popup keeps
    /// its own egui-managed state on the wider widths (its dismiss handling
    /// is the pair of fields above, and the M1 fix depends on it), so this
    /// flag drives the sheet page alone. `Gui::ui` clears it whenever the
    /// width is not Compact, so a resize with the page open cannot strand a
    /// flag no surface renders consuming a back press.
    pub(super) menu_open: bool,
    /// The phone sheet's snap position. Session-only: how a session left
    /// its sheet is not a preference.
    pub(super) sheet_extent: SheetExtent,
    /// The sheet handle's in-flight drag travel in points, or `None` when no
    /// drag is running — the timeline scrubber's own shape, for the same
    /// reason: the commit happens once, on release.
    pub(super) sheet_drag: Option<f32>,
    /// How tall the phone shell's bottom bar drew **last** frame, in points, and
    /// `0.0` on any frame that drew none — a wider width class, or the chrome
    /// fully faded.
    pub(super) phone_bar_height: f32,
    // Safe area insets in logical pixels (top, bottom, left, right)
    // Used on Android to avoid drawing under system bars.
    pub(super) safe_area_insets: (f32, f32, f32, f32),
    /// Whether this platform can quit at all. Pushed in by the frontend from
    /// the bridge, which this crate cannot see. `false` hides the menu's Exit.
    pub(super) supports_exit: bool,
    /// Remembers whether a mouse or a finger is driving, across frames.
    pub(super) modality: ModalityLatch,
    /// This frame's resolved layout. Written once at the top of [`Gui::ui`] and
    /// read by everything below it; never recomputed further down.
    pub(super) layout: LayoutCtx,
    /// Pointer/gesture resolution for the map, gated on the modality.
    pub(super) interaction: InteractionState,
    /// User unit and timezone preferences.
    pub preferences: UserPreferences,
    /// Serial GPS configuration (port, baud).
    pub serial_config: rustdar_nmea_serial::SerialConfig,
    /// How the directional heading is determined.
    pub heading_source: rustdar_location::HeadingSource,
    /// Storm motion the user typed in, overriding the RPG's SCIT average on
    /// every storm-relative velocity tilt — all four are derived, so all four
    /// take it. `None` means "use the vector the `N0S` product carries", which
    /// is the default and is what AWIPS calls the average storm motion.
    pub storm_motion_override: StormMotionOverride,
    /// Which derived rung storm-relative velocity falls to when the reader has
    /// entered no override and the volume brought no NWS vector.
    pub srv_fallback: rustdar_radar::srv::SrvFallback,
    /// Whether one of the storm-motion `DragValue`s is under the pointer or
    /// holding the keyboard *right now*. See [`Self::storm_motion_mid_edit`].
    pub storm_motion_editing: bool,
    /// Whatever can actually draw a 3D pane, or `None` on a machine or a frame
    /// where nothing can.
    pub(super) volume_painter: Option<std::sync::Arc<dyn crate::volume_view::VolumePainter>>,
    /// The user's Volume Alpha curves, one per edited product. See
    /// [`crate::volume_alpha`]: absence means "render through the palette's
    /// own alpha, bit-exactly", which is why this is a store of exceptions
    /// rather than a curve per product.
    pub(crate) volume_alpha: crate::volume_alpha::AlphaCurves,
    /// The user's isosurface thresholds, one per edited product. See
    /// [`crate::volume_iso`]: absence means the argued per-product default,
    /// so this too is a store of exceptions.
    pub(crate) volume_iso: crate::volume_iso::IsoThresholds,
    /// Top-level config keys the loaded file carried that this build cannot
    /// name, verbatim — the file-scope half of
    /// [`crate::pane::PaneConfigBaggage`]'s story. Written by
    /// `load_ui_config`, handed back untouched by `ui_config_json`, and
    /// never acted on in between: preserving what a newer build wrote is
    /// what makes running this build against its file safe.
    pub(super) config_unknown_fields: serde_json::Map<String, serde_json::Value>,
    /// `overlay_states` entries the loaded file carried for handlers this
    /// build does not have. The save writes the live handlers' state *over*
    /// these, so a kind this build serves is described by its handler and
    /// one it does not is handed back exactly as it arrived.
    pub(super) overlay_states_baggage: serde_json::Map<String, serde_json::Value>,
    /// Every record of what the last frame drew, for the input harness —
    /// the thirty test-only probe fields collapsed into one. See
    /// [`FrameProbes`].
    #[cfg(test)]
    pub(super) probes: FrameProbes,
}

/// A storm motion vector the user may substitute for the RPG's.
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

impl Gui {
    pub fn new() -> Self {
        // The same two defaults the radar config used to carry, now taken
        // from it one last time so the opening site and the opening time are
        // still declared in exactly one place.
        let RadarConfig { site, timestamp } = RadarConfig::default();

        let mut gui = Self {
            radar: RadarState {
                site,
                error_message: None,
            },
            liveness: Vec::new(),
            time_dialog: TimeDialogState {
                timestamp,
                date_string: timestamp.format("%Y-%m-%d").to_string(),
                time_string: timestamp.format("%H:%M:%S").to_string(),
                show: false,
            },
            initial_zoom_set: false,
            map_tiles: MapTileState::default(),
            user_fix: None,
            user_fix_at: None,
            location_permission: rustdar_location::LocationPermission::default(),
            location_active: false,
            location_settings_available: false,
            catalogue_pending: false,
            user_heading: None,
            // The composed twelve, not `OverlayRegistry::default()`'s eleven:
            // `default()` is the overlay crate's own set, and radar is a
            // separate source crate. See `crate::sources`.
            overlays: OverlayRegistry::with_handlers(crate::sources::all()),
            panes: vec![PaneState::new()],
            active_pane: 0,
            pane_layout: PaneLayout::default(),
            color_scale_orientation: ColorScaleOrientation::default(),
            map_pane_geo: HashMap::new(),
            volume_empty_states: HashMap::new(),
            mirror_size_points: egui::Vec2::ZERO,
            floor_tile_zoom_bias: 0,
            click_consumed_frame: false,
            pending_pane_view: None,
            section_draw_armed: false,
            section_anchor: None,
            pending_section_line: None,
            region_pick_armed: false,
            region_drag: None,
            pending_region: None,
            section_edit_drag: None,
            section_handles: Vec::new(),
            pending_section_edit: None,
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
            status_bar_tick: None,
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
            phone_bar_height: 0.0,
            safe_area_insets: (0.0, 0.0, 0.0, 0.0),
            supports_exit: true,
            modality: ModalityLatch::default(),
            layout: LayoutCtx::default(),
            interaction: InteractionState::default(),
            preferences: UserPreferences::default(),
            serial_config: rustdar_nmea_serial::SerialConfig::default(),
            heading_source: rustdar_location::HeadingSource::default(),
            storm_motion_override: StormMotionOverride::default(),
            srv_fallback: rustdar_radar::srv::SrvFallback::default(),
            storm_motion_editing: false,
            volume_painter: None,
            volume_alpha: crate::volume_alpha::AlphaCurves::default(),
            volume_iso: crate::volume_iso::IsoThresholds::default(),
            config_unknown_fields: serde_json::Map::new(),
            overlay_states_baggage: serde_json::Map::new(),
            #[cfg(test)]
            probes: FrameProbes::default(),
        };
        gui.initialize_pane_enabled();
        // The site layer draws from its own copy of the table; without this it
        // has no rows and answers `has_data` false, which is a map with no
        // site markers on it.
        gui.publish_radar_sites();
        gui
    }
}
