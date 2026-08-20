//! The [`Gui`] shell's state: the struct, its state types and its
//! constructor, split out of `ui.rs` at WO-E1. Every formerly-private
//! field is `pub(super)` — visible throughout `mod ui`'s subtree, exactly
//! the sphere it had when the struct lived in `ui.rs` — and the thirty
//! test-only probe fields live behind the one gated `probes` field.

#[cfg(test)]
use super::probes::FrameProbes;
use super::*;

/// Auto-polling timer state.
pub(crate) struct AutoPollState {
    pub(super) last_fetch_time: Option<web_time::Instant>,
    pub enabled: bool,
    pub(super) initial_fetch_done: bool,
    pub(super) interval_secs: u64,
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

    /// How long the event loop may sleep before [`should_poll`] would answer
    /// yes, or `None` when there is no timer to run out.
    ///
    /// The scheduling half of [`should_poll`], and it must agree with it
    /// exactly or the wake it grants is spent on a frame that polls nothing.
    /// `should_poll` compares whole seconds — `elapsed().as_secs() >=
    /// interval_secs` — which for an integer interval is the same test as
    /// `elapsed >= interval`, so this subtraction is neither early nor late.
    ///
    /// [`should_poll`]: Self::should_poll
    pub fn poll_delay(&self) -> Option<std::time::Duration> {
        if !self.enabled {
            return None;
        }
        let elapsed = self.last_fetch_time?.elapsed();
        Some(std::time::Duration::from_secs(self.interval_secs).saturating_sub(elapsed))
    }

    /// How long until the countdown the status bar prints changes, or `None`
    /// when the number on screen has stopped moving.
    ///
    /// The status bar renders `time_until_next` as `archive {n}s`, which is a
    /// whole second of `elapsed` — so the frame it needs is not "soon", it is
    /// the instant `elapsed` crosses the next second boundary. Anything faster
    /// redraws the same string; anything slower drops a number out of the
    /// count.
    ///
    /// `None` once the count bottoms out at zero. `time_until_next` saturates,
    /// so a poll that cannot fire — no pane viewing live — leaves `archive 0s`
    /// on screen indefinitely, and a tick scheduled for a string that will
    /// never change again is exactly the repaint this whole path exists to
    /// stop.
    ///
    /// [`time_until_next`]: Self::time_until_next
    pub fn countdown_tick_delay(&self) -> Option<std::time::Duration> {
        if !self.enabled {
            return None;
        }
        let elapsed = self.last_fetch_time?.elapsed();
        if elapsed.as_secs() >= self.interval_secs {
            return None;
        }
        // Strictly positive by construction — `subsec_nanos` is below a
        // second — so this term can never schedule a zero-length sleep.
        Some(std::time::Duration::from_nanos(u64::from(
            NANOS_PER_SEC - elapsed.subsec_nanos(),
        )))
    }
}

/// One second, for [`AutoPollState::countdown_tick_delay`]'s remainder.
const NANOS_PER_SEC: u32 = 1_000_000_000;

// The chunk-feed status vocabulary is defined beside its producer in
// `rustdar_radar::chunk_feed` (WO-RF1). WO-RA killed the temporary re-export
// pair (`ChunkFeedStatus`/`TiltFreshness`) this crate carried at its old
// published paths — one name, one path: consumers spell the radar path.
use rustdar_radar::chunk_feed::ChunkFeedStatus;

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

pub struct Gui {
    pub(super) radar: RadarState,
    pub(super) auto_poll: AutoPollState,
    /// See [`Gui::live_chunks_enabled`].
    pub(super) live_chunks: bool,
    /// See [`Gui::chunk_notifications_enabled`].
    pub(super) chunk_notifications: bool,
    /// See [`Gui::notifier_endpoint`].
    pub(super) notifier_endpoint: String,
    /// What the real-time feed is doing, refreshed each frame by the App.
    pub(super) chunk_status: ChunkFeedStatus,
    /// Each site's current-volume stamp, refreshed each frame by the App and
    /// advanced by every sealed sweep. A 3D pane names the volume it wants by
    /// [`CurrentVolumeStamp::newest`], which is what makes its rebuilds follow
    /// the live feed — see `App::base_scans` and `rustdar_radar::current` for
    /// what the stamp is a stamp *of*.
    pub(super) current_volumes: HashMap<String, CurrentVolumeStamp>,
    pub(super) time_dialog: TimeDialogState,
    pub(super) initial_zoom_set: bool,
    // --- Map tiles (shared across panes) ---
    pub(super) map_tiles: MapTileState,
    // User's GPS fix (full data from GPS receiver or Android LocationManager)
    pub(super) user_fix: Option<rustdar_location::Fix>,
    /// When [`user_fix`](Self::user_fix) arrived.
    ///
    /// Not `user_fix.timestamp`: that is the *receiver's* clock, it is absent
    /// on every source but serial NMEA, and it says when the position was
    /// measured rather than when this app last heard anything. The question the
    /// settings pane asks — "is location on but not producing?" — is about the
    /// second one.
    pub(super) user_fix_at: Option<web_time::Instant>,
    /// What the OS last said about this app's access to the user's location,
    /// pushed in by the frontend's location gate.
    ///
    /// Cached rather than queried because this crate cannot see a
    /// `PlatformBridge` — it is the crate the bridge's trait depends *on* — so
    /// a copy is the only thing available here. How fresh the copy is is the
    /// gate's poll cadence, which tightens while [`Gui::settings_visible`]
    /// answers true for exactly this reason.
    pub(super) location_permission: rustdar_location::LocationPermission,
    /// Whether the platform is currently delivering location fixes. A different
    /// question from the permission: every desktop process starts granted and
    /// silent.
    pub(super) location_active: bool,
    /// Whether this platform has a location settings page to offer.
    ///
    /// Pushed once at startup rather than with the two fields above, because it
    /// is a property of the build and not of the permission — it cannot change
    /// while the app runs, and nothing is served by re-asking it at the gate's
    /// cadence. `false` by default, so a bridge that has not been asked renders
    /// no button rather than one that does nothing.
    pub(super) location_settings_available: bool,
    /// Whether the site list is still only what this install has decoded,
    /// rather than the network.
    ///
    /// The site list reads the process-wide table, and the table cannot tell
    /// the two apart: two rows learned off two volumes look exactly like a
    /// network with two radars in it. That is what made the regression this
    /// exists for invisible — the caption read `2 shown - 2 sites` with a
    /// confidence it had not earned, while 203 radars sat in the cache
    /// unapplied. `false` by default, so a bridge that has not been asked
    /// states nothing rather than crying wolf.
    pub(super) catalogue_pending: bool,
    // Compass heading in degrees (0–360), from device compass sensor
    pub(super) user_heading: Option<f32>,
    // Overlay data (SPC outlooks, NWS alerts, SPC discussions)
    pub overlays: OverlayRegistry,
    // Multi-pane state
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
    ///
    /// Recorded inside `Map::show`, because that is the only place a
    /// `walkers::Projector` exists. Kept across frames rather than cleared,
    /// deliberately: a pane that is momentarily not drawn (a collapsed
    /// divider, a hidden tab) should leave its floor where it was rather than
    /// dropping it, and a stale entry costs six words of state.
    ///
    /// **The invariant is that a key here is a pane showing a floor right
    /// now**, not merely a pane that was one when the affine was taken.
    /// Entries are pruned at the top of the pane loop against the live pane
    /// count, the live [`crate::pane::PaneKind`] and the live floor toggle, so
    /// neither a layout that sheds panes — indices are reused, and a stale
    /// entry would read as *some other pane's* map rather than as absent — nor
    /// a pane converted back to a map can leave the mirror copying geography
    /// nothing on screen still has.
    pub(super) map_pane_geo: HashMap<usize, crate::volume_view::MapPaneGeo>,
    /// Why each 3D pane drew **no picture** on the last frame, by pane index.
    /// A pane that drew a volume has no entry.
    ///
    /// Not a test probe (`last_volume_arms` is that): the pane-properties
    /// sidebar reads this to explain its Map floor checkbox. The floor is drawn
    /// *by the raymarch*, inside the very callback an empty state means was
    /// never pushed — so in every one of those states the checkbox is a control
    /// that produces nothing, and a control that produces nothing has to say
    /// why.
    ///
    /// Recorded by the arm rather than re-derived in the sidebar. The arm's
    /// answer has five independent reasons in it — no painter, no published
    /// volume, a product with no vertical structure, a grid not built yet, a
    /// pane that is not 3D at all — and a second copy of them in the sidebar
    /// would drift from this one silently, in the direction of a checkbox that
    /// looks fine.
    ///
    /// **Last frame's, by construction.** The shell's sidebar pass runs before
    /// `render_panes` in [`Gui::ui`], so this frame's arm has not run when the
    /// checkbox is drawn. That is the right staleness rather than a tolerated
    /// one: the sidebar is describing the picture the user is looking at, and
    /// the picture the user is looking at is the one the last frame drew.
    ///
    /// Cleared and refilled by the pane loop each frame, so a pane that stops
    /// being 3D — or starts drawing — cannot leave an explanation behind.
    pub(super) volume_empty_states: HashMap<usize, String>,
    /// How much of egui's coordinate space the pane mirror has to cover, in
    /// points, as of the last frame: the frame itself, plus however far below
    /// it this frame's off-screen map strips reach. See
    /// [`Gui::mirror_size_points`].
    pub(super) mirror_size_points: egui::Vec2,
    /// How many slippy zoom levels deeper a **floor-source** map pane should
    /// fetch its raster tiles, from the renderer's last mirror plan.
    ///
    /// Set by the frontend, which is the only side that knows how much the 3D
    /// camera is magnifying the ground and how many texels the mirror could
    /// afford — see `egui_renderer::mirror`. Zero for every pane nothing is
    /// standing on, so a layout with no 3D view fetches exactly the tiles it
    /// always did.
    pub(super) floor_tile_zoom_bias: u8,
    /// Whether some feature consumed this frame's map click — written by the
    /// pane loop, read by [`Self::apply_fade_toggle`] (a consumed click while
    /// faded unfades; see `ui_fade.rs`) and by the harness's probe.
    pub(super) click_consumed_frame: bool,
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
    /// [`Self::request_pane_view`], one rule for all of them.
    ///
    /// It is the right shape for one reason more. The writers WP-G adds — an
    /// armed section drag resolving to a line, and the retarget rule that
    /// follows from it — run from **inside** `render_panes`' per-pane take,
    /// where the hazard is live and silent. And the ordering an interaction
    /// needs is the same one the pane count needs: growing it mid-loop moves
    /// the rects of panes the loop has not reached, desynchronising them from
    /// the ones `detect_active_pane_click` hit-tested this frame. One
    /// deferral point, applied at [`Self::apply_pending_pane_view`] after the
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
    ///
    /// A [`RenderView`](rustdar_radar::types::RenderView) rather than a
    /// `PaneKind`, because what the user picks from the pane menu is a
    /// *picture*: plan view, 3D volume, cross-section. Two of those three are
    /// the same kind of pane in different render modes, so a kind alone could
    /// not carry the request — and a pair of `(kind, Option<render>)` would make
    /// "cross-section in the volume render mode" expressible for no reason.
    pub(super) pending_pane_view: Option<(PaneId, rustdar_radar::types::RenderView)>,
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
    /// One of the two armed modal drags on a map pane; the other is
    /// [`region_pick_armed`](Self::region_pick_armed). They are held mutually
    /// exclusive by their setters, because one press cannot be two gestures,
    /// and they share one detector for the same reason — see
    /// [`crate::ui_input::ArmedDragGesture`].
    pub(super) section_draw_armed: bool,
    /// The in-flight draw: where it started, on which pane, and where the
    /// pointer is now.
    pub(super) section_anchor: Option<SectionAnchor>,
    /// A finished line and the map pane it was drawn on, applied **after** the
    /// pane loop.
    ///
    /// Deferred for the reason [`pending_pane_view`](Self::pending_pane_view) is,
    /// and one reason more that is specific to this writer. Applying a line can
    /// *grow the pane count*, and `PaneLayout::pane_rect` is a function of it —
    /// so a mid-loop growth silently moves the rects of every pane the loop has
    /// not reached yet, away from the ones `detect_active_pane_click`
    /// hit-tested at the top of this same frame. The panes drawn after the growth
    /// would be drawn in the right place and clicked in the wrong one, for one
    /// frame, with nothing to say so.
    pub(super) pending_section_line: Option<(PaneId, crate::pane::SectionLine)>,
    /// Whether the 3D region pick is **armed**: the next drag on a map pane
    /// draws the square of ground a 3D view will resample, rather than panning.
    ///
    /// Armed-modal rather than a modifier-drag, for the reason
    /// [`section_draw_armed`](Self::section_draw_armed) is — a shift-drag has
    /// no touch equivalent and one wasm binary serves phones and desktop
    /// browsers alike — and cancellable the same three ways: the toggle it was
    /// armed from, Escape, and Android's back button through
    /// [`Self::dismiss_top_layer`].
    ///
    /// # Why this mode exists at all
    ///
    /// The grid is a fixed cell count, so a picked region is not a crop — it is
    /// the **only** control that buys resolution. At the shipped 512 cells a
    /// 920 km ring is 1.80 km per cell; a 230 km region over the same cells is
    /// 0.45, and a 100 km one is 0.20. Zoom moves the eye and cannot do this,
    /// deliberately (see `ui_region`), which leaves this drag as the whole of
    /// the answer to "show me that storm in detail".
    ///
    /// Global rather than per-pane, like the section arm and for its reason:
    /// the pane it applies to is not knowable when it is ticked. The user arms
    /// the mode and *then* chooses a map to drag on, and choosing it is the
    /// same press that starts the box.
    pub(super) region_pick_armed: bool,
    /// The in-flight box: which pane it is being dragged on, where its centre
    /// was fixed, and how wide it currently stands. See
    /// [`crate::ui_region::RegionDrag`].
    pub(super) region_drag: Option<crate::ui_region::RegionDrag>,
    /// A finished region and the map pane it was dragged on, applied **after**
    /// the pane loop.
    ///
    /// Deferred for [`pending_section_line`](Self::pending_section_line)'s
    /// second reason exactly: applying one can *grow the pane count*, and
    /// `PaneLayout::pane_rect` is a function of it — so a mid-loop growth moves
    /// the rects of every pane the loop has not reached yet away from the ones
    /// `detect_active_pane_click` hit-tested at the top of this same frame.
    pub(super) pending_region: Option<(PaneId, crate::pane::VolumeRegion)>,
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
    pub(super) section_edit_drag: Option<crate::ui_section_edit::SectionEditDrag>,
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
    pub(super) section_handles: Vec<crate::ui_section_edit::SectionGrabZone>,
    /// A dropped handle's line and the section pane it belongs to, applied
    /// **after** the pane loop.
    ///
    /// Deferred for the reason every pending is
    /// ([`pending_pane_view`](Self::pending_pane_view)): the drop is recorded
    /// from inside `Map::show`, in the window where the map pane is
    /// `mem::take`n out of the vector. Unlike
    /// [`pending_section_line`](Self::pending_section_line) this can never grow
    /// the layout — it re-aims a section pane that already exists — and its
    /// applier writes the line and nothing else, so the ordinary staleness
    /// poll is what re-cuts. One deferral shape for every writer, rather than
    /// one careful exception.
    pub(super) pending_section_edit: Option<(PaneId, crate::pane::SectionLine)>,
    // The Gui-global `viewport_sync` / `sync_layers` toggles were retired in
    // M11: sync is per pane now — `PaneState::viewport_link`,
    // `PaneState::layer_link`, `PaneState::time_link` — and the old globals
    // survive only as read-only legacy fields on `UiConfig`, which seed the
    // per-pane links once on load (see `load_ui_config`).
    // --- Radar loop settings ---
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
    ///
    /// A separate field rather than a widened `drawer_open` because the two
    /// answer at different widths and remember independently: closing the
    /// sidebar on a desktop must not also close the drawer the same window
    /// gets when it narrows past the breakpoint.
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
    /// Session-only, like `drawer_open`: how a session left its chrome is not
    /// a preference.
    pub(super) timeline_collapsed: bool,
    /// Whether the transport's second row — the loop tuning — is shown.
    /// Session-only, on the same precedent.
    pub(super) timeline_row2: bool,
    /// The archive scrubber's in-flight drag position, as a fraction of the
    /// lookback window, or `None` when no drag is in flight. Remembered
    /// across frames so the handle follows the pointer instead of snapping
    /// back to the resting position every frame; the commit happens once, on
    /// release — see `render_timeline_scrubber`.
    pub(super) timeline_scrub: Option<f32>,
    /// Whether the floating status bar is collapsed to its ⏵ restore button.
    /// Session-only, on the same precedent as the timeline's collapse.
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
    ///
    /// That chip is the only thing in the app that changes with no input
    /// behind it — the `archive {n}s` countdown, and the age of the tilt a
    /// live feed last delivered — so it is the only thing that can oblige an
    /// otherwise idle app to draw again. The interval is decided by the code
    /// that writes the string, which is the only place that knows *which*
    /// string it wrote: a second for a count of seconds, a minute for a count
    /// of minutes, nothing at all for a number that has stopped moving.
    ///
    /// Written by `render_status_bar` for every outcome including the absences
    /// — Compact, faded out, collapsed, or a spinner in the chip's place — and
    /// read by [`Self::status_tick_delay`].
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
    /// Session-only, same terms as [`Self::catalog_query`].
    pub(super) site_query: String,
    /// The stack row being drag-reordered by its grip, if one is in flight.
    /// Session-only: a drag is a gesture, not a preference. The permute
    /// happens once, on release — see `ui_stack.rs`'s reorder note.
    pub(super) stack_drag: Option<rustdar_source::id::LayerId>,
    /// The pane whose pill row a first touch tap revealed, if any.
    /// Session-only: a reveal is a gesture in progress, not a preference.
    /// Cleared where the gestures that end it are resolved — a map click
    /// that switches panes, or a confirmed map tap (`ui_map.rs`).
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
    /// Persisted (`UiConfig::pin_pane_controls`); the settings body's
    /// Interface section is the one writer.
    pub(super) pin_pane_controls: bool,
    /// Whether the floating chrome is faded away (plan §1.8) — the map-first
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
    /// activates it (§1.8). Session-only bookkeeping.
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
    /// The user's saved presets (§3.11). Persisted; the built-ins are
    /// compiled in beside them (`catalog::builtin_presets`) and never saved.
    pub(super) presets: Vec<PresetConfig>,
    /// This build's loop frame cap, pushed in by the frontend from
    /// `constants::MAX_LOOP_FRAMES` — this crate cannot read that table (the
    /// dependency points the other way), and the timeline's row-2 caption
    /// wants to state the platform's real budget rather than a guess.
    /// Defaults to the desktop arm's value, which is what every headless
    /// test is.
    pub(super) loop_frame_budget: usize,
    /// Whether the top bar's ☰ dropdown was open on the last frame it drew.
    ///
    /// The dropdown's real state is egui popup memory, which this crate only
    /// touches mid-frame — but [`Self::dismiss_top_layer`] runs *between*
    /// frames, from the frontend's input handling, so it needs last frame's
    /// answer mirrored somewhere it can reach. Written every frame by
    /// `render_top_bar`, from the popup's own id.
    pub(super) menu_popup_open: bool,
    /// A dismiss was consumed against the open dropdown; the top bar honours
    /// this (and clears it) by force-closing the popup before next showing it.
    ///
    /// A request rather than a direct write because the popup's memory is
    /// keyed on a widget id that only exists mid-frame — see
    /// `render_top_bar_run`, where the two dismissal routes (Escape, which
    /// egui also sees and closes on itself, and Android's back, which never
    /// enters egui's queue) converge on this one flag.
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
    ///
    /// # Why a frame late, and why that is the right answer
    ///
    /// The bar is opaque, full-bleed and drawn on `Order::Middle`, over a map
    /// that is deliberately full-bleed under it: `ui_shell` gives the map
    /// everything below the top bar and floats all other chrome on top. That is
    /// the design and it stays. But the *legend* is not map — it is chrome
    /// painted into the map's own layer during the pane loop, which runs before
    /// `render_bottom_bar`, so it cannot ask how much of the bottom edge is
    /// about to be covered.
    ///
    /// It asks what was covered last frame instead. A one-frame lag on a chrome
    /// inset is invisible: the bar's height changes only when the width class
    /// or the font metrics change, both of which already cost a relayout, and
    /// the frame that changes them draws the legend at the previous inset and
    /// every frame after it at the new one. The alternative — re-deriving the
    /// bar's height from `BAR_ITEM_PADDING`, `BAR_ITEM_GAP` and two font
    /// galleys before the pane loop — is a second copy of a layout egui already
    /// does, and it would drift silently the first time an item changed.
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
    ///
    /// The default is the 0-6 km mean wind, which is the derived quantity
    /// measured closest to what the NWS publishes. The Bunkers right-mover is
    /// the other choice and it is a genuinely different thing to want — a
    /// supercell motion prediction rather than a stand-in for the average of
    /// the cells that were tracked — so it is a reader's setting and not an
    /// accuracy knob. See `rustdar_radar::srv::SrvFallback`.
    ///
    /// `pub` for the reason [`Self::storm_motion_override`] beside it is: the
    /// crate that owns the commit rule is `rustdar_app`.
    pub srv_fallback: rustdar_radar::srv::SrvFallback,
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
    /// crate that owns the commit rule is `rustdar_app`, and it has to be
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
            location_permission: rustdar_location::LocationPermission::default(),
            location_active: false,
            location_settings_available: false,
            catalogue_pending: false,
            user_heading: None,
            // The composed twelve, not `OverlayRegistry::default()`'s eleven:
            // `default()` is the overlay crate's own set, and radar is a
            // separate source crate since WO-M9. See `crate::sources`.
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
        gui
    }
}
