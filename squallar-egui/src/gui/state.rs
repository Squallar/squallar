//! The [`Gui`] shell's state: the struct, its state types and its
//! constructor. Every field is `pub(super)` — visible throughout `mod ui`'s
//! subtree — and the thirty test-only probe fields live behind the one gated
//! `probes` field.

#[cfg(test)]
use super::probes::FrameProbes;
use super::*;

pub struct Gui {
    /// **What each layer says it is doing**, re-stated each frame by the App.
    ///
    /// Opaque by construction (WO-E8c): an entry is a [`LayerId`] and the
    /// layer's own payload, and the only readers are the layers' own glue
    /// modules. This replaced two radar-shaped fields — the chunk-feed status
    /// and the per-site volume stamps — which is why nothing here names
    /// either.
    pub(super) liveness: Vec<squallar_source::liveness::SourceLiveness>,
    pub(super) time_dialog: TimeDialogState,
    pub(super) initial_zoom_set: bool,
    pub(super) map_tiles: MapTileState,
    /// Where downloaded offline basemap areas persist on this platform, or
    /// `None` where there is nowhere to put them (web; an Android whose
    /// entry never learnt its files root). A platform fact, not UI state:
    /// handed over once at construction ([`Gui::with_basemap_dir`]), never
    /// persisted, never poked through a setter. The directory may not exist
    /// yet — creating it is the download engine's job, which is also the
    /// only reader.
    pub(super) basemap_dir: Option<std::path::PathBuf>,
    pub(super) user_fix: Option<squallar_location::Fix>,
    /// When [`user_fix`](Self::user_fix) arrived.
    pub(super) user_fix_at: Option<web_time::Instant>,
    /// What the OS last said about this app's access to the user's location,
    /// pushed in by the frontend's location gate.
    pub(super) location_permission: squallar_location::LocationPermission,
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
    /// **How the user wants a multi-pane window split**, or `Auto` to let the
    /// width class answer. App-wide rather than per-pane: it describes the
    /// window, not any one pane. Persisted; see `ui_config`.
    pub(super) split_orientation: crate::pane::SplitOrientation,
    /// Divider positions carried out of a config file and not yet adopted:
    /// the grid they belong to is only known once the first frame has resolved
    /// the real width class, which is later than the load. One shot — see
    /// `Gui::settle_pane_layout`.
    pub(super) restored_ratios: Option<(Vec<f32>, Vec<Vec<f32>>)>,
    /// A pane the user has asked to close, applied at the end of the frame —
    /// after every `mem::take`n pane is back in the vector, and with the whole
    /// frame's action list in hand to invalidate. See `Gui::close_pane`.
    pub(super) pending_pane_close: Option<PaneId>,
    /// Remembered color-scale bar orientation for the map panel (hysteresis, so
    /// a resize near the boundary cannot make the bars hop).
    pub(super) color_scale_orientation: ColorScaleOrientation,
    /// Each 3D pane's own map strip: the Mercator affine it drew that map
    /// through, and the off-screen rect it drew it *into*. This is both the
    /// registration the pane's floor is reprojected by and the rect the
    /// frontend clips its mirror pass to.
    pub(super) map_pane_geo: HashMap<usize, crate::volume_view::MapPaneGeo>,
    /// The basemap's laid-out place names, kept across frames.
    ///
    /// On `Gui` rather than inside the pane walk because it is only worth
    /// anything if it survives the frame: the same names are laid out again
    /// every frame, and `Context::fonts_mut` — an exclusive lock on the whole
    /// egui context — is what each fresh layout costs. Held by the shell for
    /// the same reason nothing else here is a thread-local: one owner, visible
    /// lifetime, and a test can build its own.
    pub(super) galley_cache: walkers::GalleyCache,
    /// The floor-strip cache: per-pane content keys, the all-or-nothing
    /// frame verdict, and the repaint-force latch. See
    /// [`map::FloorStrips`].
    pub(super) floor_strips: map::FloorStrips,
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
    pub(super) pending_pane_view: Option<(PaneId, squallar_radar::types::RenderView)>,
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
    /// Whether the **offline download** pick is armed: the next drag on a map
    /// pane draws the square of ground to make available offline, rather than
    /// panning. The 3D pick's twin, and mutually exclusive with it — one drag,
    /// two arms, each with its own bounds.
    pub(super) download_pick_armed: bool,
    /// The in-flight download box, on the same
    /// [`crate::ui_region::RegionDrag`] the 3D pick uses — with the download
    /// arm's bounds, which have no 10 km floor.
    pub(super) download_drag: Option<crate::ui_region::RegionDrag>,
    /// The committed download box, as the drag described it.
    ///
    /// **App-wide rather than per-pane, and persisted**: an area is a fact
    /// about the device, and reopening puts the box and its level list back
    /// exactly where they were. Absent from an older config, which loads as
    /// "nothing picked".
    pub(super) download_pick: Option<crate::ui_download_area::PickedBox>,
    /// Which detail level the level list has selected. Persisted; absence
    /// loads as [`crate::ui_download_area::DetailLevel::default`].
    pub(super) download_detail: crate::ui_download_area::DetailLevel,
    /// Whether the download includes the terrain hillshade, once the user has
    /// said — `None` meaning **follow the terrain switch**.
    ///
    /// Three states rather than a bool, because the honest default is not a
    /// constant: a download should hold what the user actually looks at, so an
    /// untouched checkbox tracks the Base Map inspector's "Terrain shading"
    /// live rather than latching whatever it read the instant the box was
    /// drawn. Persisted, so a deliberate choice reopens exactly as it was
    /// left; absence loads as `None`, which reopens tracking the switch — and
    /// the switch itself persists, so the checkbox is 1:1 either way.
    pub(super) download_terrain: Option<bool>,
    /// The exact size figure for the picked box, measured off the frame
    /// thread. Derived, never persisted — it is the archive's answer, not the
    /// user's choice.
    pub(super) download_size: crate::ui_download_area::AreaSizeProbe,
    /// What the origin's storage has, when a platform can say.
    ///
    /// `None` everywhere today: the only platform with an origin quota is
    /// web, and the Rust side cannot reach the service worker's store yet.
    /// See `Gui::set_download_quota` for what is owed and why no `pub` seam
    /// was invented to carry it.
    pub(super) download_quota: Option<crate::basemap_download::OfflineQuota>,
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
    /// **Which theme the map is drawn in**, or `System` to follow the desktop.
    ///
    /// The chrome is dark whatever this says; what it moves is the basemap and
    /// every theme-sensitive raster keyed on it. `System` is the default and is
    /// what everyone gets until they choose otherwise.
    pub theme: crate::pane::ThemeChoice,
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
    /// [`InspectorSelection::AppSettings`].
    ///
    /// **No close route rewrites it.** Closing and reopening returns the body
    /// you left. There used to be two closes — the crumb's `›`, which kept the
    /// selection, and a dismissal, which reset it to App › Settings — and the
    /// difference was defended as "a collapse is not a deselection". The panel
    /// has one close now (`ui_inspector.rs`) and deselection is the crumb's
    /// job, so the distinction has nothing left to name: a close that silently
    /// re-aims the panel is a close that loses the user's place.
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
    /// Whether the layer catalog is open. Session-only, like every other
    /// open-surface flag; opened by the stack's two `+ Show a layer` buttons
    /// and closed by applying a tile, the `✕`, the backdrop, or
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
    ///
    /// **Session-only, and cleared at the end of the gesture that used it**: a
    /// filter is a gesture in progress, not a preference, and a stale one
    /// reads as a broken list on the next open. See
    /// [`Self::clear_site_query`].
    pub(super) site_query: String,
    /// Which pass each autofocusing search field last drew on.
    ///
    /// A field that did not draw on the *previous* pass is opening now, and
    /// that is the one pass it takes keyboard focus. A standing
    /// `request_focus` would take focus back from whatever the user reached
    /// for next; asking for it every frame is the defect, not the ask.
    /// Session-only bookkeeping — see [`Self::focus_search_on_open`].
    pub(super) search_focus_pass: std::collections::HashMap<SearchField, u64>,
    /// The user's starred radar sites, bare ICAO identifiers, in the order
    /// they were starred.
    ///
    /// **App-wide rather than per-pane**: a favourite is a fact about the
    /// person, not about a window. Persisted; absent from an older config,
    /// which loads as "nothing starred" — what those sessions were.
    pub(super) favorite_sites: Vec<String>,
    /// The offline basemap areas this device holds, in the order they
    /// finished — see [`Gui::downloaded_areas`].
    ///
    /// **App-wide rather than per-pane**: a downloaded area is a fact about
    /// the device, not about a window. Persisted; absent from an older config,
    /// which loads as "no downloaded areas". Every entry says what its
    /// download *asked for* — never whether the bytes are still there, which
    /// is recomputed from the store.
    pub(super) downloaded_areas: Vec<crate::basemap_download::DownloadedArea>,
    /// The Downloaded areas screen's off-frame-thread arm, built on the first
    /// frame that draws the screen and only where this platform has a store.
    ///
    /// **Not UI state and never persisted**: what it holds is the store's own
    /// answer about which segments are present, recomputed every session. A
    /// persisted copy would be exactly the stale completeness flag the
    /// download engine refuses to write.
    pub(super) area_maintenance: Option<crate::basemap_areas::AreaMaintenance>,
    /// The one offline-area download running, if any.
    ///
    /// Not persisted either, and deliberately: a run does not survive a
    /// launch, and the record that does is written only by a `Complete`
    /// outcome. The next session offers the resume rather than taking it.
    pub(super) active_download: Option<crate::basemap_areas::ActiveDownload>,
    /// The stack row being drag-reordered by its grip, if one is in flight.
    pub(super) stack_drag: Option<squallar_source::id::LayerId>,
    /// A layer whose stack row the next stack pass should scroll into view,
    /// then forget. Written when the catalog turns a layer on, so the row the
    /// tile refers to is on screen when the modal closes.
    pub(super) stack_scroll_to: Option<squallar_source::id::LayerId>,
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
    /// Whether the frame diagnostics overlay is showing — the Interface
    /// section's "Show frame diagnostics" switch. Persisted.
    pub(super) diagnostics_panel: bool,
    /// The overlay's trailing-window state. Session-only bookkeeping —
    /// emptied whenever the overlay is hidden.
    pub(super) diagnostics: diagnostics::DiagnosticsState,
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
    /// Which failure the user has dismissed the banner for, per layer.
    ///
    /// Keyed on [`FetchRetry::last_failure`] rather than on the message,
    /// because two failures can carry the same words: dismissing hides *that*
    /// failure, and the next one -- new instant -- comes back. Session state,
    /// as the field it replaced was: nothing here is persisted.
    ///
    /// Generic on purpose. The banner is sourced from the radar layer today,
    /// which is the only layer that has one, but nothing in this map is.
    pub(super) dismissed_errors: std::collections::HashMap<LayerId, web_time::Instant>,
    /// The user's saved presets (the plan). Persisted; the built-ins are
    /// compiled in beside them (`catalog::builtin_presets`) and never saved.
    pub(super) presets: Vec<PresetConfig>,
    /// This build's loop frame cap, pushed in by the frontend from
    /// `constants::MAX_LOOP_FRAMES` — this crate cannot read that table (the
    /// dependency points the other way), and the timeline's row-2 caption
    /// wants to state the platform's real budget rather than a guess.
    pub(super) loop_frame_budget: usize,
    /// This device's `Budgets::concurrent_renders`, pushed in by the frontend —
    /// see [`crate::shell_api::FrameInputs::concurrent_renders`]. Bounds how
    /// many overlay rasters one pane and layer may have out; see
    /// [`crate::overlay_cache::RendersInFlight`].
    pub(super) concurrent_renders: usize,
    /// The overdraw fraction a whole-picture overlay raster asks for, per
    /// side, pushed in by the frontend — see
    /// [`crate::shell_api::FrameInputs::overlay_overdraw`]. Handed to
    /// `crate::overlay_cache::plan_overlay_texture` for every pane's plan.
    pub(super) overlay_overdraw: f32,
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
    pub serial_config: squallar_nmea_serial::SerialConfig,
    /// The serial-port list the GPS settings row offers, scanned off the
    /// frame thread. Owned per `Gui` rather than kept in a `static`, so two
    /// of them in one process cannot read each other's scans.
    #[cfg(feature = "gps-serial")]
    pub(super) gps_ports: squallar_nmea_serial::GpsPortScanner,
    /// How the directional heading is determined.
    pub heading_source: squallar_location::HeadingSource,
    /// Storm motion the user typed in, overriding the RPG's SCIT average on
    /// every storm-relative velocity tilt — all four are derived, so all four
    /// take it. `None` means "use the vector the `N0S` product carries", which
    /// is the default and is what AWIPS calls the average storm motion.
    pub storm_motion_override: StormMotionOverride,
    /// Which derived rung storm-relative velocity falls to when the reader has
    /// entered no override and the volume brought no NWS vector.
    pub srv_fallback: squallar_radar::srv::SrvFallback,
    /// Whether one of the storm-motion `DragValue`s is under the pointer or
    /// holding the keyboard *right now*. See [`Self::storm_motion_mid_edit`].
    pub storm_motion_editing: bool,
    /// Whatever can actually draw a 3D pane, or `None` on a machine or a frame
    /// where nothing can.
    pub(super) volume_painter: Option<std::sync::Arc<dyn crate::volume_view::VolumePainter>>,
    /// Whatever can draw a vector tile's fills from the GPU, or `None` on a
    /// build or a frame where nothing can — see [`crate::tile_mesh`].
    pub(super) tile_mesh_painter: Option<std::sync::Arc<dyn crate::tile_mesh::TileMeshPainter>>,
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
    /// Which site table [`Self::publish_radar_sites`] last copied — see
    /// [`squallar_radar::sites::table_generation`]. Compared once per frame;
    /// when it has moved, the layer's copy is retaken.
    pub(super) published_sites_generation: u64,
    /// Every record of what the last frame drew, for the input harness —
    /// the thirty test-only probe fields collapsed into one. See
    /// [`FrameProbes`].
    #[cfg(test)]
    pub(super) probes: FrameProbes,
}

/// A search field the chrome focuses on the pass its surface opens.
///
/// One variant per *surface*, not per widget: the site search is one string
/// ([`Gui::site_query`]) rendered by two hosts, and each host opens and closes
/// on its own, so each owns its own grab.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum SearchField {
    /// One pane's site pill popover.
    SitePopover(PaneId),
    /// The inspector's Pane-properties site search.
    InspectorSite,
    /// The layer catalog's search, in the modal and the sheet page alike.
    Catalog,
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
    pub fn sample(&self) -> Option<squallar_radar::srm::StormMotionSample> {
        if !self.enabled {
            return None;
        }
        // The constructor rejects non-finite values too; this is the boundary,
        // that is the invariant.
        squallar_radar::srm::StormMotionSample::user_override(self.speed_kt, self.direction_deg)
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
        // still declared in exactly one place. The site goes straight into
        // the one pane a fresh `Gui` has: there is no app-wide site for it to
        // sit in.
        let RadarConfig { site, timestamp } = RadarConfig::default();

        let mut gui = Self {
            liveness: Vec::new(),
            time_dialog: TimeDialogState {
                timestamp,
                date_string: timestamp.format("%Y-%m-%d").to_string(),
                time_string: timestamp.format("%H:%M:%S").to_string(),
                show: false,
            },
            initial_zoom_set: false,
            map_tiles: MapTileState::default(),
            basemap_dir: None,
            user_fix: None,
            user_fix_at: None,
            location_permission: squallar_location::LocationPermission::default(),
            location_active: false,
            location_settings_available: false,
            catalogue_pending: false,
            user_heading: None,
            // The composed twelve, not `OverlayRegistry::default()`'s eleven:
            // `default()` is the overlay crate's own set, and radar is a
            // separate source crate. See `crate::sources`.
            overlays: OverlayRegistry::with_handlers(crate::sources::all()),
            panes: vec![PaneState::with_site(site)],
            active_pane: 0,
            pane_layout: PaneLayout::default(),
            split_orientation: crate::pane::SplitOrientation::default(),
            restored_ratios: None,
            pending_pane_close: None,
            color_scale_orientation: ColorScaleOrientation::default(),
            map_pane_geo: HashMap::new(),
            galley_cache: walkers::GalleyCache::default(),
            floor_strips: map::FloorStrips::default(),
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
            download_pick_armed: false,
            download_drag: None,
            download_pick: None,
            download_detail: crate::ui_download_area::DetailLevel::default(),
            download_terrain: None,
            download_size: crate::ui_download_area::AreaSizeProbe::new(),
            download_quota: None,
            section_edit_drag: None,
            section_handles: Vec::new(),
            pending_section_edit: None,
            loop_lookback_secs: 3600, // default 1 hour
            loop_speed_fps: crate::pane::DEFAULT_LOOP_SPEED_FPS,
            theme: crate::pane::ThemeChoice::System,
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
            search_focus_pass: std::collections::HashMap::new(),
            favorite_sites: Vec::new(),
            downloaded_areas: Vec::new(),
            area_maintenance: None,
            active_download: None,
            stack_drag: None,
            stack_scroll_to: None,
            pill_revealed: None,
            pills_drawn_last_frame: 0,
            pills_raise_pending: false,
            pin_pane_controls: false,
            diagnostics_panel: false,
            diagnostics: diagnostics::DiagnosticsState::default(),
            ui_faded: false,
            fade_candidate: false,
            press_switched_pane: false,
            press_popup_open: false,
            fade_factor: 1.0,
            sheet_last_page: None,
            toast_last_error: None,
            dismissed_errors: std::collections::HashMap::new(),
            presets: Vec::new(),
            // The desktop arm of `constants::MAX_LOOP_FRAMES`; the frontend
            // pushes the real target's value at startup.
            loop_frame_budget: 60,
            // The compile-time arm of the same axis, so a `Gui` nobody has
            // pushed facts into behaves like this target rather than like a
            // device with no render budget at all.
            concurrent_renders: squallar_device_profile::constants::MAX_CONCURRENT_RENDERS,
            // The renderer's own ceiling, which is the ladder's top rung.
            overlay_overdraw: crate::overlay_cache::OVERDRAW_FRACTION,
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
            serial_config: squallar_nmea_serial::SerialConfig::default(),
            #[cfg(feature = "gps-serial")]
            gps_ports: squallar_nmea_serial::GpsPortScanner::new(),
            heading_source: squallar_location::HeadingSource::default(),
            storm_motion_override: StormMotionOverride::default(),
            srv_fallback: squallar_radar::srv::SrvFallback::default(),
            storm_motion_editing: false,
            volume_painter: None,
            tile_mesh_painter: None,
            volume_alpha: crate::volume_alpha::AlphaCurves::default(),
            volume_iso: crate::volume_iso::IsoThresholds::default(),
            config_unknown_fields: serde_json::Map::new(),
            overlay_states_baggage: serde_json::Map::new(),
            published_sites_generation: 0,
            #[cfg(test)]
            probes: FrameProbes::default(),
        };
        gui.initialize_pane_enabled();
        // The site layer draws from its own copy of the table; without this it
        // has no rows and answers `has_data` false, which is a map with no
        // site markers on it. **A seed, not the whole story** — a Gui is
        // routinely built before anything has resolved a table (`App::new`
        // does exactly that), so the copy taken here is often of nothing.
        // What makes it right in the end is the per-frame re-read in
        // [`Self::republish_radar_sites_if_the_table_moved`].
        gui.publish_radar_sites();
        gui
    }

    /// The download store this `Gui` was constructed over, in one move — the
    /// construction-time route the seam ratchet leaves open: `app.rs` chains
    /// this onto [`Gui::new`], so no `set_` push ever crosses the App→Gui
    /// seam for it. `None` is the bridge answering "no filesystem for it",
    /// and every `Gui` a test builds bare.
    pub fn with_basemap_dir(mut self, dir: Option<std::path::PathBuf>) -> Self {
        // The base tile slot reads its downloaded areas back out of the same
        // directory the download engine writes them into, so it is handed the
        // path here rather than growing a second route to it.
        self.map_tiles.set_basemap_dir(dir.clone());
        self.basemap_dir = dir;
        self
    }

    /// Where downloaded offline basemap areas persist, or `None` when this
    /// platform has nowhere to put them. See [`Self::basemap_dir`] (the
    /// field) for the contract; the download engine is the consumer.
    pub fn basemap_dir(&self) -> Option<&std::path::Path> {
        self.basemap_dir.as_deref()
    }

    /// [`Self::with_basemap_dir`]'s two writes, after construction — **for
    /// the test harness only**, which builds its `Gui` before a test knows
    /// where its temporary directory is.
    ///
    /// Gated to the test build so the production route stays the
    /// construction-time chain and no `set_` ever crosses the App-Gui
    /// seam for it.
    #[cfg(test)]
    pub(crate) fn set_basemap_dir_for_test(&mut self, dir: std::path::PathBuf) {
        self.map_tiles.set_basemap_dir(Some(dir.clone()));
        self.basemap_dir = Some(dir);
    }

    /// Never build a live tile source under this `Gui`.
    ///
    /// **For test harnesses that drive [`Self::ui`], and nothing else** —
    /// theirs is the one `Gui` whose frames must not open sockets, because
    /// what live tiles paint depends on how much wall-clock time the test
    /// took. See [`crate::tiles::MapTileState::go_offline_for_tests`] for the
    /// measured failure this removes.
    pub fn go_offline_for_tests(&mut self) {
        self.map_tiles.go_offline_for_tests();
    }
}

#[cfg(test)]
mod basemap_dir_tests {
    use super::*;

    /// The construction pass-through, both ways: the one route the path has
    /// into this crate is `Gui::new().with_basemap_dir(..)`, so a `Gui` built
    /// with a path must expose it and a `Gui` built bare must expose `None` —
    /// there is no setter to correct either afterwards.
    #[test]
    fn a_gui_built_with_a_basemap_dir_exposes_it() {
        let dir = std::path::PathBuf::from("/somewhere/basemap-downloads");
        let gui = Gui::new().with_basemap_dir(Some(dir.clone()));
        assert_eq!(gui.basemap_dir(), Some(dir.as_path()));
    }

    #[test]
    fn a_gui_built_without_a_basemap_dir_exposes_none() {
        assert_eq!(Gui::new().basemap_dir(), None);
    }
}
