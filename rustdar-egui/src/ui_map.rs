use crate::actions::GuiAction;
use crate::pane::PaneKind;
use rustdar_overlays::render::overlay_state::OverlayKind;
use rustdar_radar::beam;
use rustdar_radar::types::{IMAGE_SIZE, RadarProduct};
use rustdar_units::UserPreferences;

#[path = "ui_map_pane.rs"]
mod pane_render;

/// What a cross-section pane says while it has nothing to show.
///
/// Deliberately an instruction rather than an apology: a section pane with no
/// line is the ordinary state between converting a pane and aiming it, and the
/// line is drawn somewhere else (on a map pane), which is not guessable.
pub(crate) const CROSS_SECTION_EMPTY_STATE: &str =
    "Draw a line on a map pane to cut a cross-section";

/// What a 3D pane says while it has nothing to show.
///
/// Says unavailable, not "loading": whether a device can raymarch a volume at
/// all is decided by a capability check, and a pane that promises a picture it
/// cannot produce is worse than one that says so.
pub(crate) const VOLUME_EMPTY_STATE: &str = "3D volume view unavailable";

impl super::Gui {
    /// Draw every visible pane, whatever kind each one is.
    ///
    /// Named for panes rather than for maps because the pane loop below is
    /// shared by all three [`PaneKind`](crate::pane::PaneKind)s and only one of
    /// them is a map. Everything except the single `match` on the pane's kind —
    /// the rect, taking the pane, resolving the centre, taking `map_memory`,
    /// resolving the pointer, building the child `Ui`, putting it all back and
    /// drawing the border — is deliberately *not* per-kind: a section pane has a
    /// site, a viewport and a pointer just as a map pane does, and duplicating
    /// the frame around each arm is how those quietly drift apart.
    pub(super) fn render_panes(
        &mut self,
        ui: &mut egui::Ui,
        excluded_rects: &[egui::Rect],
    ) -> Vec<GuiAction> {
        use walkers::{Map, Position};

        let mut actions = Vec::new();
        let ctx = ui.ctx().clone();

        // What the map was *handed*, so a test can check the chrome's rects
        // actually arrive here. They reach every click handler from
        // `PaneRenderCtx::excluded_rects` below.
        #[cfg(test)]
        {
            self.last_map_excluded_rects = excluded_rects.to_vec();
        }

        // Detect current theme from egui context
        let is_dark_theme = ctx.global_style().visuals.dark_mode;

        // Initialize tiles via MapTileState
        self.map_tiles.ensure_base_tiles(is_dark_theme, &ctx);
        // Visible *map* panes only. `Gui::panes` because a pane remembered from a
        // wider split must not keep label-tile fetching alive; `is_map` for the
        // same reason and one more — a pane with no tiles has nowhere to put a
        // label, so a converted pane would go on fetching a tile pyramid nothing
        // draws. Its `enabled_overlays` is left as it is, so converting back
        // restores the layer: see `Gui::any_pane_has_overlay_enabled`.
        //
        // Read before the pane loop's `mem::take`, so the kind is the real one.
        let any_city_labels = self
            .panes()
            .iter()
            .any(|p| p.is_map() && p.is_overlay_enabled(OverlayKind::CityLabels));
        if any_city_labels {
            self.map_tiles.ensure_label_tiles(is_dark_theme, &ctx);
        }

        // Take tiles out of self so they can be reborrowed per-pane in the loop.
        let mut tiles_owned = self.map_tiles.take_base_tiles();
        let mut label_tiles = if any_city_labels {
            self.map_tiles.take_label_tiles()
        } else {
            None
        };

        // The visible slice's bound, not the layout's raw count: the loop below
        // indexes `self.panes[pane_idx]` directly, and `Gui::panes` documents
        // why slicing at `pane_layout.pane_count` alone could outrun the vector.
        let pane_count = self.visible_pane_count();
        // Resolved once for the frame, before the pane loop: every pane must
        // agree about what is pointing at the screen.
        let modality = self.layout.modality;
        // Read before the loop's `mem::take`, for the reason the kind branch
        // gives: inside the take a pane's slot holds a default map pane, so a 3D
        // pane's region read from `self.panes[..]` mid-loop would be `None`.
        let region_arm = self.region_arm;
        let committed_regions: Vec<(usize, crate::pane::VolumeRegion)> = self
            .panes()
            .iter()
            .filter_map(|p| {
                let volume = p.volume()?;
                Some((volume.source_pane?, volume.region?))
            })
            .collect();

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                let panel_rect = ui.max_rect();
                #[cfg(test)]
                {
                    self.last_map_panel_rect = panel_rect;
                }

                // One color-scale orientation for the whole grid, resolved from
                // the panel (not from each pane's rect) so every pane on screen
                // agrees and dragging a divider cannot flip the bars. See
                // `ColorScaleOrientation`.
                let horizontal_color_scale = self.color_scale_orientation.resolve(panel_rect);

                self.detect_active_pane_click(ui.ctx(), panel_rect);

                // Snapshot viewport state before rendering for sync detection
                let (pre_zooms, pre_positions): (Vec<f64>, Vec<Option<Position>>) =
                    if self.viewport_sync && pane_count > 1 {
                        self.panes
                            .iter()
                            .take(pane_count)
                            .map(|p| (p.map_memory.zoom(), p.map_memory.detached()))
                            .unzip()
                    } else {
                        (vec![], vec![])
                    };

                let pointer_available = self.dismiss_overlay_popups(ui.ctx());

                // Rects of floating chrome drawn over the map (the hamburger).
                // Clicks there must not become overlay polygon hit-tests.
                //
                // Supplied by the chrome that drew them rather than rebuilt
                // here from a second copy of its position constants — the two
                // copies could disagree silently, leaving a dead zone at the
                // old position and a live one under the button.

                for pane_idx in 0..pane_count {
                    let pane_rect = self.pane_layout.pane_rect(pane_idx, panel_rect);
                    let is_active = pane_idx == self.active_pane;

                    let mut pane = std::mem::take(&mut self.panes[pane_idx]);

                    // Determine the map center.
                    //
                    // The loaded scan is the best answer, but it is not available
                    // for the whole window between asking for a site and its
                    // volume arriving — and on a slow link, or a site whose fetch
                    // fails, that window is the entire experience. Falling
                    // straight to the geographic centre of the contiguous US
                    // there means the user watches the map sit in Kansas while
                    // the picker names the radar they asked for.
                    //
                    // The site's own coordinates are known from the moment it is
                    // named, so they bridge the gap: the map goes where it is
                    // going immediately and the scan simply confirms it. The US
                    // centre stays for the genuinely unplaceable case — a pane
                    // naming a site the table does not have.
                    let center = if let Some(scan_info) = &pane.scan_info {
                        Position::new(scan_info.site.lon, scan_info.site.lat)
                    } else if let Some(site) = rustdar_radar::sites::get_radar_site(&pane.site) {
                        Position::new(site.lon, site.lat)
                    } else {
                        Position::new(-98.5795, 39.8283) // Geographic center of contiguous USA
                    };

                    // Clone user location and heading for use in closure
                    let user_location = self.user_fix.as_ref().map(|f| (f.latitude, f.longitude));
                    let user_heading = self.gps_config.heading_source.effective_heading(
                        self.user_heading,
                        self.user_fix.as_ref().and_then(|f| f.heading_deg),
                        self.user_fix.as_ref().and_then(|f| f.speed_mps),
                    );
                    let user_fix = self.user_fix.clone();

                    // Take map_memory out so Map::new borrows it independently
                    // of the pane fields used in the render closure.
                    let mut map_memory = std::mem::take(&mut pane.map_memory);

                    // Resolve this pane's pointer state for the frame. Which
                    // pipeline runs is a *runtime* decision, taken once per
                    // frame by `LayoutCtx` and enforced by `InteractionState`:
                    // - Mouse: egui's built-in click detection (instant)
                    // - Touch: the gesture pipeline for the active pane
                    //   (deferred single-tap so double-tap-to-zoom doesn't open
                    //   popups, plus zoom-drag and long-press)
                    //
                    // Both paths run the click position through the canonical
                    // dialog-blocking gate (`ui_input::filter_dialog_blocked`),
                    // which discards clicks landing on a floating dialog or
                    // popup window. All handlers that receive overlay_click_pos
                    // from PaneRenderCtx automatically inherit this protection.
                    //
                    // CONVENTION: New map click handlers MUST use overlay_click_pos from
                    // PaneRenderCtx — never read raw click events via ctx.input() for
                    // map-level interactions, as that bypasses dialog blocking.
                    let pointer = if is_active {
                        self.interaction
                            .resolve_active(&ctx, modality, &mut map_memory, pane_rect)
                    } else {
                        self.interaction.resolve_inactive(&ctx, modality)
                    };

                    // Both gated on the armed mode, and both **unconditionally**
                    // rather than only while a drag is in flight.
                    //
                    // A press that is going to become a region drag is
                    // indistinguishable from one that is going to become a pan
                    // until the pointer moves, and by then the map has already
                    // slid under the anchor. The same holds for the click: a
                    // press-and-release inside a radar site's icon while armed is
                    // a discarded too-small region, not a request to switch site.
                    let overlay_click_pos = if region_arm {
                        None
                    } else {
                        pointer.overlay_click_pos
                    };
                    let suppress_pan = pointer.suppress_pan || region_arm;

                    // From the same locals that feed `PaneRenderCtx` and
                    // `drag_pan_buttons` below: after the gate, after
                    // `overlay_click_pos` is read out. See `PanePointerProbe`.
                    //
                    // Deliberately above the kind branch, so **every** pane
                    // reports a frame whatever it is. The whole `input_harness`
                    // suite reads the active pane's probe out of this vector,
                    // and `InputHarness::frame` panics when it finds none — so a
                    // kind whose arm forgot to push would take down ~4600 lines
                    // of pointer tests with a message about the pointer pipeline
                    // never running. Pinned by
                    // `every_pane_reports_a_pointer_frame_whatever_its_kind`.
                    #[cfg(test)]
                    self.last_pane_pointers
                        .push(crate::ui_input::PanePointerProbe {
                            pane_idx,
                            is_active,
                            modality,
                            frame: crate::ui_input::MapPointerFrame {
                                overlay_click_pos,
                                long_press_pos: pointer.long_press_pos,
                                suppress_pan,
                            },
                        });

                    // Create a child UI constrained to this pane's rect.
                    //
                    // `"pane_map"` is a **key, not a description**: it is the
                    // salt every widget inside this pane derives its egui `Id`
                    // from, so egui's memory of what the pane remembers —
                    // combo boxes it has open, scroll offsets, resized panels —
                    // hangs off it. Renaming it to something kind-neutral would
                    // re-key every one of those, turning "the user made pane 2 a
                    // 3D view" into "egui forgot everything pane 2 remembered",
                    // and would report the conversion as a widget-id change for
                    // no reason. It stays as it is, for all three kinds.
                    let mut child_ui = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(pane_rect)
                            .id_salt(("pane_map", pane_idx)),
                    );
                    child_ui.set_clip_rect(pane_rect);

                    // The single point in the UI that branches on pane kind.
                    //
                    // On `pane.kind()`, not `self.panes[pane_idx].kind()`: the
                    // pane was `mem::take`n above, so its slot holds a default
                    // `PaneState` — a *map* pane, whatever this one is — for the
                    // whole of this block. That is the same hazard `menu_model`
                    // has in `ui_chrome.rs`, and it has the same fix: read the
                    // value you took, never the slot you took it from. It fails
                    // silently in the direction that looks like it works, which
                    // is why `last_pane_content` records what each arm actually
                    // drew rather than what the branch was handed.
                    match pane.kind() {
                        PaneKind::Map => {
                            self.record_pane_content(pane_idx, PaneKind::Map, pane_rect);
                            if let Some(tiles) = tiles_owned.as_mut() {
                                Map::new(None, &mut map_memory, center)
                                    .with_layer(tiles, 1.0)
                                    // `zoom_with_ctrl(false)` is what puts us on walkers'
                                    // raw-scroll zoom path, and walkers 0.55 changed that
                                    // path's frame-time multiplier from
                                    // `stable_dt.max(predicted_dt * 1.5)` to
                                    // `stable_dt.clamp(predicted_dt * 0.5, predicted_dt * 2.0)`.
                                    // At a steady frame rate that is a uniform x0.667 on the
                                    // scroll-zoom step (60Hz: 0.025 -> 0.01667, so a wheel
                                    // notch that gave ~1.31x now gives ~1.21x); on a hitched
                                    // frame the old form grew unbounded and the new one is
                                    // capped, which is the bug being fixed.
                                    //
                                    // `Map::zoom_speed` (default 2.0) can compensate the
                                    // magnitude, but it is not an exact undo: it scales the
                                    // combined zoom delta, so pinch and double-click zoom
                                    // move with it. Left at the default deliberately.
                                    .zoom_with_ctrl(false)
                                    .panning(false)
                                    .drag_pan_buttons(if suppress_pan {
                                        egui::DragPanButtons::empty()
                                    } else {
                                        egui::DragPanButtons::PRIMARY
                                    })
                                    .show(&mut child_ui, |ui, _response, projector, memory| {
                                        let zoom = memory.zoom();

                                        let mut render_ctx = pane_render::PaneRenderCtx {
                                            pane_idx,
                                            pane: &mut pane,
                                            overlays: &mut self.overlays,
                                            user_location,
                                            user_heading,
                                            user_fix: user_fix.clone(),
                                            label_tiles: &mut label_tiles,
                                            actions: &mut actions,
                                            pane_rect,
                                            horizontal_color_scale,
                                            pointer_available,
                                            excluded_rects: excluded_rects.to_vec(),
                                            long_press_pos: pointer.long_press_pos,
                                            overlay_click_pos,
                                            preferences: &self.preferences,
                                            region: pane_render::RegionCtx {
                                                armed: region_arm,
                                                drag: &mut self.region_drag,
                                                pending: &mut self.pending_region,
                                                committed: &committed_regions,
                                            },
                                        };

                                        pane_render::render_pane_map_content(
                                            ui,
                                            projector,
                                            zoom,
                                            &mut render_ctx,
                                        );
                                    });
                            }
                        }
                        // The two kinds that exist as a shape and nothing more:
                        // each paints its empty state and stops. There is no
                        // sampler behind either one yet, and a pane that draws
                        // *something* while there is nothing to draw is how a
                        // fabricated picture ships.
                        PaneKind::CrossSection => {
                            self.record_pane_content(pane_idx, PaneKind::CrossSection, pane_rect);
                            paint_pane_empty_state(
                                &mut child_ui,
                                pane_rect,
                                CROSS_SECTION_EMPTY_STATE,
                            );
                        }
                        PaneKind::Volume => {
                            self.record_pane_content(pane_idx, PaneKind::Volume, pane_rect);
                            // Cloned rather than borrowed: `record_pane_content`
                            // above and the probe below both want `&mut self`,
                            // and an `Arc` clone is a refcount bump against a
                            // borrow that would otherwise have to span the whole
                            // arm.
                            let painter = self.volume_painter().cloned();
                            // Read here, beside the painter, and for the same
                            // reason: both want `&self` across a body that also
                            // wants `&mut self`, and both are cheap to copy out.
                            let archive_collected = self.archive_volume_for(&pane.site);
                            let outcome = render_volume_pane(
                                &mut child_ui,
                                pane_rect,
                                pane_idx,
                                &mut pane,
                                painter.as_deref(),
                                archive_collected,
                                &mut actions,
                            );
                            #[cfg(test)]
                            self.last_volume_arms
                                .push(VolumeArmProbe { pane_idx, outcome });
                            #[cfg(not(test))]
                            let _ = outcome;
                        }
                    }

                    // Restore map_memory and pane
                    pane.map_memory = map_memory;
                    self.panes[pane_idx] = pane;

                    if pane_count > 1 {
                        draw_pane_border(ui, pane_rect, is_active);
                    }
                } // end pane loop

                // Handle divider dragging on a foreground layer so they
                // take priority over map panning in the overlap zone.
                if pane_count > 1 {
                    let divider_layer =
                        egui::LayerId::new(egui::Order::Foreground, egui::Id::new("pane_dividers"));
                    let mut divider_ui = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(panel_rect)
                            .layer_id(divider_layer),
                    );
                    self.pane_layout
                        .handle_dividers(&mut divider_ui, panel_rect);
                }

                // Sync viewports: propagate the interacted pane's viewport to all others
                self.sync_viewports(&pre_zooms, &pre_positions);
            });

        // Restore tiles and label tiles
        self.map_tiles.restore_base_tiles(tiles_owned);
        if any_city_labels {
            self.map_tiles.restore_label_tiles(label_tiles);
        }

        actions
    }

    /// Detect which pane was clicked and make it the active pane.
    ///
    /// Bounded by [`Gui::visible_pane_count`], not by the layout's raw count, for
    /// the reason [`Gui::panes`] gives — and here the consequence is one step
    /// worse than a skipped update. This writes `active_pane`, and
    /// [`Gui::active_pane`] resolves it as `self.panes[self.active_pane]`: a rect
    /// the layout draws for a pane the vector does not hold would hand the index
    /// of a `PaneState` that does not exist to every reader downstream, and the
    /// first one to dereference it panics rather than doing nothing.
    ///
    /// Defensive rather than a live fix: no production writer can produce the
    /// skew today. Both of them (`load_ui_config` and the pane picker) grow
    /// `panes` to the requested count *before* assigning the layout, `panes` is
    /// never shortened anywhere, and `PaneLayout::for_count` clamps its count
    /// down — so the vector is if anything longer than the layout claims. The
    /// bound is here because that is a property of two call sites rather than of
    /// this type, and because a click is the one path that turns the skew from a
    /// pane nobody updates into a crash.
    fn detect_active_pane_click(&mut self, ctx: &egui::Context, panel_rect: egui::Rect) {
        let pane_count = self.visible_pane_count();
        if pane_count <= 1 {
            return;
        }
        if let Some(pos) = ctx.input(|i| {
            if i.pointer.primary_pressed() {
                i.pointer.interact_pos()
            } else {
                None
            }
        }) {
            // Don't switch panes when the click lands on a floating dialog or popup.
            if ctx
                .layer_id_at(pos)
                .is_some_and(|l| l.order > egui::Order::Background)
            {
                return;
            }
            for idx in 0..pane_count {
                let rect = self.pane_layout.pane_rect(idx, panel_rect);
                if rect.contains(pos) && idx != self.active_pane {
                    self.active_pane = idx;
                    break;
                }
            }
        }
    }

    /// Dismiss overlay popups when clicking outside them.
    /// Returns `true` when no popup is open (pointer is available for map interaction).
    fn dismiss_overlay_popups(&mut self, ctx: &egui::Context) -> bool {
        let pointer_available = self.overlays.selected_overlays.is_empty();
        if !pointer_available {
            let click_pos = ctx.input(|i| {
                if i.pointer.any_click() {
                    i.pointer.interact_pos()
                } else {
                    None
                }
            });
            if let Some(pos) = click_pos {
                let on_popup = ctx
                    .layer_id_at(pos)
                    .is_some_and(|l| l.order > egui::Order::Background);
                if !on_popup {
                    self.overlays.selected_overlays.clear();
                    self.overlays.selected_overlay_page = 0;
                }
            }
        }
        pointer_available
    }
}

/// Paint a pane's empty state: one line of centred, muted text and nothing
/// else.
///
/// Centred on the pane's own rect rather than on the `Ui`'s cursor, so the
/// message sits in the middle of the pane whatever shape the pane is.
///
/// Painted straight through `Painter` rather than laid out as a widget: an empty
/// state is not interactive, and a widget would consume one of the pane's
/// auto-ids — so every widget the real content adds later would be keyed one
/// step along from where it will finally sit, and the empty state going away
/// would re-key all of them.
/// Degrees of yaw per point of horizontal drag.
///
/// Sized so that a drag across a 900-point pane turns the box most of the way
/// round — enough to inspect a storm from every side in one gesture, short of
/// the full turn that would make the end of a drag ambiguous.
const ORBIT_YAW_DEG_PER_POINT: f32 = 0.4;
/// Degrees of pitch per point of vertical drag. Shallower than the yaw rate
/// because the usable pitch range is 178° against yaw's unbounded turn, so the
/// same rate would run into the clamp within a third of a pane.
const ORBIT_PITCH_DEG_PER_POINT: f32 = 0.25;
/// Zoom factor per point of scroll. `exp` of this times the scroll, so a notch
/// is a fixed *ratio* whatever the current distance — the same reason walkers'
/// wheel zoom is multiplicative.
const ORBIT_ZOOM_PER_SCROLL_POINT: f32 = 0.004;

/// Fingers a touch drag must have to pan a 3D pane.
///
/// Two, alongside the pinch that is already read from the same gesture: one
/// finger orbits, and it has to, because that is the gesture with no modifier
/// available on a touch screen and orbiting is the pane's primary verb. Two
/// fingers is what every 3D viewer on a touch device uses for the same reason,
/// and `MultiTouchInfo` reports the pinch and the translation from one gesture —
/// so a two-finger drag that also spreads does both, which is what a user
/// expects and what they will do without noticing.
const TOUCH_PAN_FINGERS: usize = 2;

/// What the 3D arm did with one pane on one frame.
///
/// `None` means it pushed a paint callback; `Some(reason)` means it painted the
/// empty state with that reason. Recorded because the two are indistinguishable
/// from outside — a callback whose payload nothing can draw paints exactly as
/// much as an empty state does — so a test that only looked at the screen could
/// not tell a working pane from a broken one.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct VolumeArmProbe {
    pub(crate) pane_idx: usize,
    pub(crate) outcome: Option<String>,
}

/// Draw one 3D pane: take its gesture, ask for its grid, and either push a
/// paint callback or say why there is not one.
///
/// Returns the empty-state reason, or `None` if a callback was pushed.
///
/// # Why the callback is built here and not before the frame
///
/// `painter.paint` is called with the camera **after** this frame's drag has
/// been folded in. Building the payload before `Gui::ui` ran would be tidier and
/// would leave the orbit one frame behind the pointer — which does not look like
/// a bug, it looks like input lag, and it gets "fixed" by turning the drag
/// sensitivity up rather than by fixing the order.
///
/// # Why the zoom gate is correctness
///
/// `Input::zoom_delta` is **global**: it reports the frame's pinch or
/// ctrl-scroll wherever on screen it happened. Without the
/// `hovered() || dragged()` gate a pinch over a map pane would orbit every 3D
/// pane on screen at once, which is the sort of thing that gets reported as
/// "the 3D view moves on its own".
fn render_volume_pane(
    ui: &mut egui::Ui,
    pane_rect: egui::Rect,
    pane_idx: usize,
    pane: &mut crate::pane::PaneState,
    painter: Option<&dyn crate::volume_view::VolumePainter>,
    archive_collected: Option<chrono::NaiveDateTime>,
    actions: &mut Vec<GuiAction>,
) -> Option<String> {
    let outcome = volume_pane_outcome(
        ui,
        pane_rect,
        pane_idx,
        pane,
        painter,
        archive_collected,
        actions,
    );
    if let Some(why) = outcome.as_deref() {
        paint_pane_empty_state(ui, pane_rect, why);
    }
    outcome
}

/// The 3D arm's decision, with the painting left to its caller so that every
/// path out of it is a `return` of a reason rather than a `return` plus a call
/// somebody can forget to make.
fn volume_pane_outcome(
    ui: &mut egui::Ui,
    pane_rect: egui::Rect,
    pane_idx: usize,
    pane: &mut crate::pane::PaneState,
    painter: Option<&dyn crate::volume_view::VolumePainter>,
    archive_collected: Option<chrono::NaiveDateTime>,
    actions: &mut Vec<GuiAction>,
) -> Option<String> {
    use crate::pane::{OrbitDelta, VolumeStamp, VolumeTarget};
    use crate::volume_view::{VolumeFrameState, VolumePaint};

    // The camera and the box as they stand *before* this frame's gesture, which
    // is what the pan has to be scaled against: the world distance a screen point
    // spans depends on where the eye is, and folding the drag in first would
    // measure it against a camera the user has not seen yet.
    //
    // Answered rather than unwrapped for the reason the `volume_mut` below gives.
    let Some((camera_before, box_size_km)) = pane.volume().map(|v| (v.camera, v.box_size_km()))
    else {
        return Some(VOLUME_EMPTY_STATE.to_owned());
    };

    // The gesture first, and unconditionally: the camera is the pane's own
    // state, it survives every reason there is nothing to draw, and a user who
    // orbits an empty box while a volume downloads should find it where they
    // left it when the volume lands.
    let response = ui.interact(
        pane_rect,
        ui.id().with(("volume_orbit", pane_idx)),
        egui::Sense::click_and_drag(),
    );
    let mut delta = OrbitDelta::default();
    // Primary drag orbits; secondary drag pans. Read as two separate questions
    // rather than as an if/else on one drag, because `dragged_by` is per-button
    // and a user with both buttons down means both.
    if response.dragged_by(egui::PointerButton::Primary) {
        let drag = response.drag_delta();
        // Grab-and-turn, in both axes: a point on the box's surface follows the
        // pointer. Dragging right swings the eye's bearing east, which brings
        // the box's eastern face round to face the viewer and carries every
        // surface point rightwards with the cursor; dragging down raises the
        // eye, which tips the top face towards the viewer and carries its far
        // edge down. Both signs are convention rather than arithmetic, so both
        // are pinned by a test — a sign error here still orbits perfectly well
        // and merely feels wrong, which is the kind of defect that survives
        // review.
        delta.yaw_deg = drag.x * ORBIT_YAW_DEG_PER_POINT;
        delta.pitch_deg = drag.y * ORBIT_PITCH_DEG_PER_POINT;
    }

    // The pan drag, in screen points, from whichever device produced one.
    //
    // Touch is checked first and wins, because `normalize_touch_devices` makes
    // egui synthesise a *primary* drag from a one-finger touch: a two-finger
    // gesture would otherwise be read as an orbit as well as a pan, and the box
    // would spin while it slid. `multi_touch()` is `Some` only while more than
    // one finger is down, so the one-finger orbit above is unaffected.
    let touch = ui.ctx().multi_touch();
    let pan_drag = match touch {
        Some(touch) if touch.num_touches >= TOUCH_PAN_FINGERS => {
            // Cancel the orbit this frame: the same fingers produced the
            // synthesised primary drag that the branch above already folded in.
            delta.yaw_deg = 0.0;
            delta.pitch_deg = 0.0;
            Some([touch.translation_delta.x, touch.translation_delta.y])
        }
        _ if response.dragged_by(egui::PointerButton::Secondary) => {
            let drag = response.drag_delta();
            Some([drag.x, drag.y])
        }
        _ => None,
    };
    if let Some(drag) = pan_drag
        // `None` for a pane with no height or a degenerate box — both transient,
        // and neither may put a NaN in the camera. The default is "did not pan",
        // which is what the frame should do while a divider drag has the pane
        // collapsed to nothing.
        && let Some(pan) = crate::volume_view::pan_for_drag(
            camera_before,
            box_size_km,
            pane_rect.height(),
            drag,
        )
    {
        delta.pan = pan;
    }

    if response.hovered() || response.dragged() {
        let (pinch, scroll) = ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta.y));
        // Multiplied, not chosen between: a trackpad can deliver both in one
        // frame, and `OrbitCamera::nudge` divides the distance by the product
        // exactly once.
        delta.zoom_factor = pinch * (scroll * ORBIT_ZOOM_PER_SCROLL_POINT).exp();
    }

    // Read before `volume_mut` borrows the pane: `site`, `scan_info` and
    // `selected_product` are flat fields beside `content`, and taking them first
    // is what keeps this one borrow deep rather than a clone of the pane.
    let site_code = pane.site.clone();
    let product = pane.selected_product;
    // The archive volume, **not** `scan_info`, and that is the archive-only
    // decision in one line. `scan_info` names whatever the plan view is drawing,
    // which the real-time chunk feed rewrites on every volume roll — so keying
    // off it would rebuild an 8 MiB grid on the frame thread every few minutes,
    // for a volume that may have been joined mid-flight and cannot be resampled
    // at all. The archive publishes only finished volumes, so this one is
    // buildable the instant it is named.
    //
    // The site comes from `pane.site` rather than from `scan_info.site`, because
    // this no longer follows the pane's displayed scan: a pane that has switched
    // site should ask for the new site's archive volume, not go on naming the old
    // one until a plan view catches up.
    let stamp = archive_collected.map(|collected| VolumeStamp {
        site: site_code.clone(),
        collected,
    });
    // What the plan view has, so the pane can say when the two differ. Read for
    // the caption only; nothing branches on it.
    let shown_collected = pane.scan_info.as_ref().map(|scan_info| scan_info.timestamp);

    // Unreachable from the kind branch, which only enters here for a `Volume`
    // pane, and answered rather than unwrapped: this function takes a whole
    // `PaneState` and is the sort of thing a future caller invokes from
    // somewhere else.
    let Some(volume) = pane.volume_mut() else {
        return Some(VOLUME_EMPTY_STATE.to_owned());
    };
    volume.camera.nudge(delta);
    let camera = volume.camera;
    let region = volume.region;
    let already_rendered = volume.rendered_for.clone();

    // Everything below is a reason there is no picture, in the order the user
    // can act on them.
    let Some(painter) = painter else {
        return Some(VOLUME_EMPTY_STATE.to_owned());
    };
    let Some(volume_stamp) = stamp else {
        // Says *which kind* of volume, because the honest answer to "there is a
        // live picture in the pane beside this one, why is this empty" is that
        // they are fed from different things on purpose.
        return Some(format!(
            "Waiting for a completed archive volume from {site_code}.\n\nThe 3D view is built \
             from finished Level II volumes only, so it appears when the next one is published \
             rather than tilt by tilt.",
        ));
    };
    if rustdar_radar::sampler::samplable(product).is_none() {
        return Some(format!(
            "{} has no vertical structure to render in 3D — pick a moment the radar measures \
             directly",
            product.name(),
        ));
    }

    let collected = volume_stamp.collected;
    let target = VolumeTarget {
        volume: volume_stamp,
        product,
        region,
    };
    if already_rendered.as_ref() != Some(&target) {
        // Level-triggered on purpose. See `GuiAction::PrepareVolume`: the
        // alternative is remembering an edge across a site switch, a volume
        // roll and a surface loss, which is three places to forget.
        actions.push(GuiAction::PrepareVolume {
            pane_idx,
            target: target.clone(),
        });
    }

    let pixels_per_point = ui.ctx().pixels_per_point();
    let size_px = [
        (pane_rect.width() * pixels_per_point).round().max(1.0) as u32,
        (pane_rect.height() * pixels_per_point).round().max(1.0) as u32,
    ];

    match painter.paint(&VolumeFrameState {
        pane_idx,
        target,
        camera,
        size_px,
    }) {
        VolumePaint::Callback(callback) => {
            // Hand-constructed, because `egui_wgpu::Callback` has a private
            // field and its only constructor wants the rect up front — so a
            // crate that cannot name `egui_wgpu` cannot make one. Both of
            // `PaintCallback`'s fields are public, which is the whole reason
            // this seam is an `Arc<dyn Any>` rather than a typed payload.
            ui.painter()
                .add(egui::Shape::Callback(egui::epaint::PaintCallback {
                    rect: pane_rect,
                    callback,
                }));
            // Over the callback, and only when there is a picture to caption: an
            // empty state already says everything, and a caption under it would
            // be two explanations of the same pane.
            paint_volume_caption(
                ui,
                pane_rect,
                &volume_caption(&site_code, collected, shown_collected, region, camera),
            );
            None
        }
        VolumePaint::Empty(why) => Some(why),
    }
}

/// The 3D pane's own controls: how far the vertical is stretched, and a way back
/// to the view it started at.
///
/// # Why the exaggeration is a slider and not a preset list
///
/// It is a continuous judgement about one picture. A forecaster reading a
/// supercell wants a different stretch from one reading a squall line's
/// cross-section, and the useful move is nudging it until the structure reads —
/// which is a drag, not a choice between three named values.
///
/// The range is `[1, 12]` and it starts at 3. 1 is true proportions, and it is
/// reachable on purpose: the flat picture is the honest one, and a view that
/// could not be turned back to it would be a view that had made exaggeration
/// compulsory.
///
/// # Why the reset returns four things
///
/// A pane that is lost — panned off the box, spun to a strange angle, tightened
/// onto a region that turned out to be empty — is one the user has no other way
/// back from. So this returns the *whole* view: angle, zoom, pivot **and**
/// region. Leaving the pivot out is the easy mistake, and the symptom is a reset
/// that visibly does something and still leaves the box off screen.
///
/// A free function rather than a `Gui` method because it touches nothing but the
/// pane it is handed — and the pane it is handed is the one the caller
/// `mem::take`n, which is the only correct thing to read during the UI pass.
pub(crate) fn render_volume_controls(ui: &mut egui::Ui, pane: &mut crate::pane::PaneState) {
    let Some(volume) = pane.volume_mut() else {
        return;
    };
    ui.add_space(6.0);
    ui.separator();
    ui.label("3D view");

    let mut exaggeration = volume.camera.vertical_exaggeration();
    let response = ui.add(
        egui::Slider::new(
            &mut exaggeration,
            crate::pane::MIN_VERTICAL_EXAGGERATION..=crate::pane::MAX_VERTICAL_EXAGGERATION,
        )
        .text("Vertical \u{d7}")
        .fixed_decimals(1),
    );
    if response.changed() {
        // Through the setter, which is the only writer and the only place the
        // clamp and the non-finite refusal live. Writing the field would work
        // here and would be a second copy of both.
        volume.camera.set_vertical_exaggeration(exaggeration);
    }
    response.on_hover_text(
        "Stretches the box vertically so storm structure is legible. Heights the pane reports \
         stay in real kft MSL at every setting.",
    );

    if ui
        .button("Reset view")
        .on_hover_text("Back to the default angle, zoom, centre and region.")
        .clicked()
    {
        reset_volume_view(volume);
    }
}

/// Put a 3D pane back to the view it opened at.
///
/// A named function rather than four lines inside the button, so that what the
/// button does is reachable from a test. The alternative is a test that restates
/// the assignments, which passes whatever the button actually does — and this is
/// exactly the kind of function that grows a field it forgets to clear.
///
/// **It returns the region as well as the camera**, and the pivot as well as the
/// angles. Both are easy to leave out and both fail the same way: a reset that
/// visibly changes something and leaves the pane still looking at the wrong
/// place, which reads as a control that half-works. A `source_pane` left behind
/// is quieter still — the next region dragged on that map would re-aim this pane
/// instead of opening one where it was dragged.
pub(crate) fn reset_volume_view(volume: &mut crate::pane::VolumePane) {
    volume.camera = crate::pane::OrbitCamera::default();
    volume.region = None;
    volume.source_pane = None;
}

/// Kilofeet per kilometre. The vertical readout is in kft MSL because that is
/// what a forecaster reads a storm top in, and because it is the unit the rest of
/// this application already uses for heights.
const KFT_PER_KM: f64 = 3.280_84;

/// What the pane says about the picture it is showing, one line per fact.
///
/// # Every number here is a real one
///
/// This is the counterweight to the vertical exaggeration, and it is the reason
/// the exaggeration is defensible at all. The height line is the box's true
/// extent in kft MSL, read from the same two constants the resample was given and
/// **never** multiplied by the stretch; the stretch is stated beside it as a
/// drawing convention, with its number, so that a reader can see both facts at
/// once and cannot mistake one for the other.
///
/// The same applies to the volume time. A 3D pane in live mode is showing an
/// archived volume some minutes behind the plan view next to it, and the one
/// thing that must not happen is for it to look current — so the time is always
/// named, and when the app has a different volume for the site the pane names
/// *that* time too, rather than leaving the user to compare two corners of the
/// screen. It names the time rather than claiming which panes are showing it,
/// because a per-site timestamp is all `shown` is; see the line itself.
///
/// # Why the resolution is here rather than inferred
///
/// The grid has a fixed cell count, so a tighter region buys detail instead of
/// saving memory. That is the main reason to pick a region at all, and it is
/// invisible unless it is written down: 0.63 km per cell at the default box
/// against 0.16 at a 20 km one is the difference between a smear and a storm.
///
/// A pure function of five values so that what the pane claims can be tested
/// without a GPU, a projector or a frame.
fn volume_caption(
    site: &str,
    collected: chrono::NaiveDateTime,
    shown: Option<chrono::NaiveDateTime>,
    region: Option<crate::pane::VolumeRegion>,
    camera: crate::pane::OrbitCamera,
) -> Vec<String> {
    let mut lines = vec![format!(
        "{site} archived volume {}Z",
        collected.format("%H:%M")
    )];

    // Only when they genuinely differ. A pane whose site is on the same volume
    // needs no explanation, and saying it anyway would train the reader to ignore
    // the line that matters.
    //
    // **The sentence names the volume, not a pane**, and that is deliberate.
    // `shown` is this pane's own `PaneState::scan_info`, which the frontend sets
    // for *every* pane on a site at once — so it is "the volume the app has
    // loaded for this site" and nothing more. It is not a survey of the map
    // panes: it says nothing about a map pane on a different site, and there
    // need not be a map pane at all. An earlier wording claimed the map panes
    // were on something else, which was true in the live-versus-archive case it
    // was written for and unsupported everywhere else. Stating the other time
    // instead is both exactly what is known and more use than the claim was —
    // the reader can now see the gap rather than being told there is one.
    if let Some(shown) = shown.filter(|shown| *shown != collected) {
        lines.push(format!(
            "The app's current {site} volume is {}Z",
            shown.format("%H:%M"),
        ));
    }

    let base = rustdar_radar::voxel::DEFAULT_BASE_KM_MSL * KFT_PER_KM;
    let top = rustdar_radar::voxel::DEFAULT_TOP_KM_MSL * KFT_PER_KM;
    lines.push(format!(
        "{base:.0}–{top:.0} kft MSL · vertical exaggeration {:.1}×",
        camera.vertical_exaggeration(),
    ));

    let half_width = region.map_or(crate::pane::DEFAULT_HALF_WIDTH_KM, |r| r.half_width_km());
    let cells = rustdar_radar::voxel::default_shape().nx;
    let resolution = region
        .unwrap_or(
            // The default box, expressed as a region purely so the two paths
            // divide by the same cell count. Infallible for a finite constant,
            // and answered rather than unwrapped because `new` is the gate and
            // nothing here should be able to bypass it.
            crate::pane::VolumeRegion::new(
                crate::pane::GeoPoint { lat: 0.0, lon: 0.0 },
                half_width,
            )
            .unwrap_or_else(|| unreachable!("the default half-width is finite and in range")),
        )
        .resolution_km(cells);
    match resolution {
        Some(km) => lines.push(format!("{:.0} km box · {km:.2} km/cell", 2.0 * half_width)),
        // A zero cell count is impossible for every named shape, and a caption is
        // not the place to fail over it.
        None => lines.push(format!("{:.0} km box", 2.0 * half_width)),
    }
    lines
}

/// Inset of the caption from the pane's top-left corner, points.
const CAPTION_MARGIN: f32 = 8.0;

/// Draw the caption in the pane's top-left corner, over the volume.
///
/// Behind a translucent plate rather than straight onto the render, because the
/// volume beneath it is an arbitrary colour: white text over a stratiform sheet
/// is unreadable, and a drop shadow only halves the problem. Painted rather than
/// laid out as widgets for the reason `paint_pane_empty_state` gives — a caption
/// is not interactive, and widgets here would consume the pane's auto-ids.
fn paint_volume_caption(ui: &egui::Ui, pane_rect: egui::Rect, lines: &[String]) {
    if lines.is_empty() {
        return;
    }
    let galley = ui.painter().layout(
        lines.join("\n"),
        egui::FontId::proportional(11.0),
        egui::Color32::from_rgb(235, 235, 235),
        pane_rect.width() - 2.0 * CAPTION_MARGIN,
    );
    let origin = pane_rect.left_top() + egui::vec2(CAPTION_MARGIN, CAPTION_MARGIN);
    ui.painter().rect_filled(
        egui::Rect::from_min_size(origin, galley.size()).expand(4.0),
        3.0,
        egui::Color32::from_black_alpha(160),
    );
    ui.painter()
        .galley(origin, galley, egui::Color32::PLACEHOLDER);
}

/// Fraction of a pane's width an empty-state message is laid out across.
///
/// Not the whole width: a paragraph running edge to edge in a wide pane is
/// unreadable, and the margin is also what keeps the text clear of the pane
/// border a multi-pane layout draws.
const EMPTY_STATE_WIDTH_FRACTION: f32 = 0.8;

/// Paint a centred, **wrapped** explanation in the middle of a pane.
///
/// Wrapped, and it has to be: `Painter::text` lays a string out on one line
/// whatever its length, centred — so a sentence wider than the pane runs off
/// *both* edges with its middle showing. That is not a hypothetical. The 3D
/// pane's palette refusal is a paragraph, and the first version of it rendered
/// as a strip of words with the beginning and end of every line cut away, which
/// reads as a rendering bug rather than as an explanation.
///
/// Newlines in the message survive, so a message can separate a headline from
/// its detail with a blank line.
fn paint_pane_empty_state(ui: &mut egui::Ui, pane_rect: egui::Rect, text: &str) {
    let galley = ui.painter().layout(
        text.to_owned(),
        egui::FontId::proportional(14.0),
        ui.visuals().weak_text_color(),
        pane_rect.width() * EMPTY_STATE_WIDTH_FRACTION,
    );
    let size = galley.size();
    let top_left = pane_rect.center() - 0.5 * size;
    ui.painter()
        .galley(top_left, galley, ui.visuals().weak_text_color());
}

/// Draw a border around a pane rect, highlighted when active.
fn draw_pane_border(ui: &mut egui::Ui, pane_rect: egui::Rect, is_active: bool) {
    let border_color = if is_active {
        egui::Color32::from_rgb(60, 140, 255)
    } else {
        egui::Color32::from_rgba_unmultiplied(128, 128, 128, 100)
    };
    let stroke_width = if is_active { 2.0 } else { 1.0 };
    ui.painter().rect_stroke(
        pane_rect,
        0.0,
        egui::Stroke::new(stroke_width, border_color),
        egui::StrokeKind::Outside,
    );
}

/// Context for computing hover info from radar value data.
pub(super) struct HoverInput {
    pub site_lat: f64,
    pub site_lon: f64,
    pub hover_lat: f64,
    pub hover_lon: f64,
    pub hover_pos: egui::Pos2,
    pub rect: egui::Rect,
}

/// Compute hover info string from raw value data and site coordinates.
///
/// The radar-relative half of the readout comes from
/// [`beam::site_bearing_range_km`], the crate's one spelling of "where is this
/// point, from the radar" — it used to be a second copy of that haversine and
/// forward azimuth inline here. Both spellings measure on
/// [`rustdar_radar::types::EARTH_RADIUS_KM`], and
/// `the_hover_readouts_polar_coordinates_are_bit_identical_to_the_deleted_copy`
/// pins that the readout's digits did not move.
pub(super) fn compute_hover_info_raw(
    value_data: &[f32],
    input: &HoverInput,
    product: RadarProduct,
    prefs: &UserPreferences,
) -> String {
    let (azimuth, distance_km) = beam::site_bearing_range_km(
        input.site_lat,
        input.site_lon,
        input.hover_lat,
        input.hover_lon,
    );

    let mut value_str = String::new();
    let frac_x = (input.hover_pos.x - input.rect.left()) / input.rect.width();
    let frac_y = (input.hover_pos.y - input.rect.top()) / input.rect.height();
    let px = (frac_x * IMAGE_SIZE as f32) as i32;
    let py = (frac_y * IMAGE_SIZE as f32) as i32;

    if px >= 0 && px < IMAGE_SIZE as i32 && py >= 0 && py < IMAGE_SIZE as i32 {
        let pixel_idx = py as usize * IMAGE_SIZE + px as usize;
        if pixel_idx < value_data.len() {
            let value = value_data[pixel_idx];
            if !value.is_nan() {
                value_str = format!("| {}", product.format_value(value, prefs));
            }
        }
    }

    let distance = prefs.distance.convert_from_km(distance_km);

    format!(
        "Lat: {:.4}\u{b0}, Lon: {:.4}\u{b0} | Range: {:.1}{}, Az: {:.1}\u{b0} {}",
        input.hover_lat,
        input.hover_lon,
        distance,
        prefs.distance.suffix(),
        azimuth,
        value_str
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The string the status bar shows, for hover points at hand-checkable
    /// offsets from a real site.
    ///
    /// The readout had no test of its own while it carried its own copy of the
    /// haversine and forward azimuth. `beam::tests::
    /// the_hover_readouts_polar_coordinates_are_bit_identical_to_the_deleted_copy`
    /// pins the two spellings against each other, which is what makes moving to
    /// the shared one provably not a change; this pins what a user reads, so the
    /// next edit to either has something behavioural to fail.
    #[test]
    fn the_hover_readout_reports_range_and_azimuth_from_the_site() {
        // KTLX. One degree due north is Rₑ·(π/180) = 111.19 km at azimuth 0; one
        // degree due east is shorter than the parallel it looks like it follows
        // and leaves *north* of east, because a great circle bows poleward.
        let (site_lat, site_lon) = (35.3333, -97.2778);
        let prefs = UserPreferences::default();
        let readout = |hover_lat: f64, hover_lon: f64| {
            compute_hover_info_raw(
                &[],
                &HoverInput {
                    site_lat,
                    site_lon,
                    hover_lat,
                    hover_lon,
                    // Outside the rect, so no gate value is appended and the
                    // assertion is on the geometry alone.
                    hover_pos: egui::pos2(-1.0, -1.0),
                    rect: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(100.0, 100.0)),
                },
                RadarProduct::Reflectivity,
                &prefs,
            )
        };

        assert_eq!(
            readout(site_lat + 1.0, site_lon),
            "Lat: 36.3333\u{b0}, Lon: -97.2778\u{b0} | Range: 111.2km, Az: 0.0\u{b0} ",
        );
        assert_eq!(
            readout(site_lat, site_lon + 1.0),
            "Lat: 35.3333\u{b0}, Lon: -96.2778\u{b0} | Range: 90.7km, Az: 89.7\u{b0} ",
        );
        // A site to itself: zero range, and the azimuth is unconstrained rather
        // than wrong, so only the range half is asserted.
        assert!(
            readout(site_lat, site_lon).contains("Range: 0.0km"),
            "a site is not at zero range from itself: {}",
            readout(site_lat, site_lon),
        );
    }
}

#[cfg(test)]
mod volume_arm_tests {
    use super::*;
    use crate::input_harness::InputHarness;
    use crate::pane::PaneKind;
    use crate::volume_view::{StubVolumePainter, VolumeFrameState};
    use std::sync::Arc;

    const FRAME_DT: f64 = 1.0 / 60.0;

    /// A harness with one map pane and one 3D pane, a scan loaded, and the given
    /// painter installed. Returns the painter so a test can read back what it
    /// was asked.
    fn volume_harness(painter: StubVolumePainter) -> (InputHarness, Arc<StubVolumePainter>) {
        let painter = Arc::new(painter);
        let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
        h.set_pane_count(2);
        h.make_pane_volume(1);
        h.load_scan("KTLX");
        h.gui_mut().set_volume_painter(Some(painter.clone()));
        h.frames_for(2, FRAME_DT);
        (h, painter)
    }

    /// The last frame the painter was asked about.
    fn last_seen(painter: &StubVolumePainter) -> VolumeFrameState {
        painter
            .seen
            .lock()
            .expect("stub painter mutex")
            .last()
            .cloned()
            .expect("the painter was never asked to paint")
    }

    fn camera_of(h: &mut InputHarness, idx: usize) -> crate::pane::OrbitCamera {
        h.gui_mut()
            .pane_mut(idx)
            .expect("a pane")
            .volume()
            .expect("a 3D pane")
            .camera
    }

    /// A 3D pane with a painter and a volume pushes a callback rather than an
    /// empty state.
    ///
    /// The baseline the rest of this suite is measured against: every other test
    /// here asserts that some condition *stops* this happening, and would pass
    /// vacuously if the happy path never worked.
    #[test]
    fn a_volume_pane_with_a_painter_pushes_a_callback() {
        let (h, _painter) = volume_harness(StubVolumePainter::painting());
        assert_eq!(
            h.volume_arms(),
            vec![VolumeArmProbe {
                pane_idx: 1,
                outcome: None,
            }],
            "the 3D arm should have painted, not explained itself",
        );
    }

    /// Every headless machine, every suspend and every surface loss lands here,
    /// so it is the ordinary state rather than the exceptional one.
    #[test]
    fn a_volume_pane_with_no_painter_says_it_is_unavailable() {
        let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
        h.set_pane_count(2);
        h.make_pane_volume(1);
        h.load_scan("KTLX");
        h.frames_for(2, FRAME_DT);
        assert_eq!(
            h.volume_arms(),
            vec![VolumeArmProbe {
                pane_idx: 1,
                outcome: Some(VOLUME_EMPTY_STATE.to_owned()),
            }],
        );
    }

    /// `clear_graphics_state` is the suspend and surface-loss path, and it must
    /// take the painter with it: every wgpu handle the painter can reach was
    /// made by the device that is going away.
    #[test]
    fn losing_the_graphics_state_stops_the_pane_drawing() {
        let (mut h, _painter) = volume_harness(StubVolumePainter::painting());
        assert_eq!(
            h.volume_arms()[0].outcome,
            None,
            "precondition: it was drawing",
        );

        h.gui_mut().clear_graphics_state();
        h.frames_for(2, FRAME_DT);
        assert_eq!(
            h.volume_arms()[0].outcome.as_deref(),
            Some(VOLUME_EMPTY_STATE),
            "a painter holding handles from a dead device must not be asked again",
        );
    }

    /// A pane with no archive volume says what it is waiting for, naming the
    /// site *and* saying that it is waiting for a finished one.
    ///
    /// The second half is the part worth pinning. A user watching a live plan
    /// view beside an empty 3D pane will read "waiting for a volume" as a bug,
    /// because there is plainly a volume on screen; the message has to say that
    /// the two are fed from different things on purpose.
    #[test]
    fn a_volume_pane_with_no_scan_names_the_site_it_is_waiting_for() {
        let painter = Arc::new(StubVolumePainter::painting());
        let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
        h.set_pane_count(2);
        h.make_pane_volume(1);
        h.gui_mut().set_volume_painter(Some(painter.clone()));
        h.frames_for(2, FRAME_DT);

        let outcome = h.volume_arms()[0].outcome.clone().expect("an empty state");
        assert!(
            outcome.contains("Waiting for a completed archive volume"),
            "expected a waiting message naming what it waits for, got {outcome:?}",
        );
        assert!(
            painter.seen.lock().unwrap().is_empty(),
            "the painter must not be asked for a volume that has not arrived",
        );
    }

    /// **A live volume is not a volume this pane will build from.**
    ///
    /// The pane has a `scan_info` — the plan view beside it is drawing a
    /// perfectly good volume — and no archive volume, which is exactly the state
    /// a site being watched through the real-time chunk feed is in before its
    /// first archive poll returns. The pane must wait rather than build.
    ///
    /// This is the whole of the archive-only decision as a test. The mutation it
    /// closes is the obvious simplification: keying the target off
    /// `pane.scan_info` again, which is what the code did before and which makes
    /// every other volume test pass.
    #[test]
    fn a_live_volume_is_not_one_the_pane_will_build_from() {
        let painter = Arc::new(StubVolumePainter::painting());
        let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
        h.set_pane_count(2);
        h.make_pane_volume(1);
        h.gui_mut().set_volume_painter(Some(painter.clone()));
        h.load_scan("KTLX");
        // The chunk feed's volume is on screen; the archive has published
        // nothing for this site yet. `load_scan` fills both halves — it stands in
        // for an archive fetch — so this is what takes them apart again.
        h.set_archive_volume("KTLX", None);
        // Everything the painter saw belongs to the archive volume `load_scan`
        // published. The assertion below is about what happens *after* it is
        // withdrawn, so the record starts here.
        painter.seen.lock().unwrap().clear();
        h.frames_for(2, FRAME_DT);

        assert!(
            h.gui_mut().pane(1).expect("pane 1").scan_info.is_some(),
            "precondition: the plan view has a volume",
        );
        let outcome = h.volume_arms()[0].outcome.clone().expect("an empty state");
        assert!(
            outcome.contains("Waiting for a completed archive volume"),
            "a pane with a live volume and no archive one must wait, got {outcome:?}",
        );
        assert!(
            painter.seen.lock().unwrap().is_empty(),
            "no grid may be asked for on the strength of a live volume",
        );
        assert!(
            !h.last_actions()
                .iter()
                .any(|a| matches!(a, GuiAction::PrepareVolume { .. })),
            "no build may be triggered by a live volume arriving",
        );
    }

    /// The pane names the **archive** volume, not the one the plan view shows.
    ///
    /// The two differ constantly in live mode — the archive publishes a volume
    /// only once every cut is finished, so it is by construction the one before
    /// the feed's — and a target built from the wrong one would ask the host for
    /// a volume it does not have.
    #[test]
    fn the_target_names_the_archive_volume_rather_than_the_displayed_one() {
        let painter = Arc::new(StubVolumePainter::painting());
        let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
        h.set_pane_count(2);
        h.make_pane_volume(1);
        h.gui_mut().set_volume_painter(Some(painter.clone()));
        h.load_scan("KTLX");
        let shown = h
            .gui_mut()
            .pane(1)
            .expect("pane 1")
            .scan_info
            .as_ref()
            .expect("a scan")
            .timestamp;
        // The archive is one volume behind, which is the steady state while a
        // real-time feed is running.
        let archived = shown - chrono::Duration::minutes(6);
        h.set_archive_volume("KTLX", Some(archived));
        h.frames_for(2, FRAME_DT);

        let seen = painter.seen.lock().unwrap();
        let frame = seen.last().expect("the painter was asked");
        assert_eq!(
            frame.target.volume.collected, archived,
            "the grid must be asked for against the archive volume, not the displayed one",
        );
        assert_eq!(frame.target.volume.site, "KTLX");
    }

    /// A moment the radar does not measure directly is refused by name, before
    /// anything asks for a grid `build_voxels` would decline to build.
    #[test]
    fn a_product_with_no_vertical_structure_is_refused_by_name() {
        let (mut h, painter) = volume_harness(StubVolumePainter::painting());
        // On every pane, not just the 3D one: `sync_layers` defaults on and
        // propagates the *active* pane's product to the rest, so writing it to
        // pane 1 alone is undone on the next frame by pane 0.
        for pane in h.gui_mut().panes_mut() {
            pane.selected_product = rustdar_radar::types::RadarProduct::EchoTops;
        }
        let before = painter.seen.lock().unwrap().len();
        h.frames_for(2, FRAME_DT);

        let outcome = h.volume_arms()[0].outcome.clone().expect("an empty state");
        assert!(
            outcome.contains("no vertical structure"),
            "expected the refusal to say why, got {outcome:?}",
        );
        assert_eq!(
            painter.seen.lock().unwrap().len(),
            before,
            "the painter must not be asked about a moment that cannot be sampled",
        );
    }

    /// The pane asks for its grid until it has one, and stops the moment the
    /// host records that it does.
    ///
    /// Level-triggered by design — see `GuiAction::PrepareVolume` — so the half
    /// worth testing is that it *stops*, which an edge-triggered implementation
    /// would get right for free and a broken level-triggered one would not.
    #[test]
    fn a_volume_pane_asks_for_its_grid_until_the_host_says_it_has_one() {
        let (mut h, _painter) = volume_harness(StubVolumePainter::painting());

        let asked: Vec<_> = h
            .last_actions()
            .iter()
            .filter_map(|a| match a {
                GuiAction::PrepareVolume { pane_idx, target } => Some((*pane_idx, target.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(asked.len(), 1, "the pane should have asked exactly once");
        let (pane_idx, target) = asked.into_iter().next().expect("one request");
        assert_eq!(pane_idx, 1);
        assert_eq!(target.volume.site, "KTLX");

        // What the host does when the build lands.
        h.gui_mut()
            .pane_mut(1)
            .expect("pane 1")
            .volume_mut()
            .expect("a 3D pane")
            .rendered_for = Some(target);
        h.frames_for(2, FRAME_DT);

        assert!(
            !h.last_actions()
                .iter()
                .any(|a| matches!(a, GuiAction::PrepareVolume { .. })),
            "a pane that has its grid must stop asking for it",
        );
    }

    /// Converting a 3D pane to something else releases its volume.
    ///
    /// The only moment a pane stops needing an 8 MiB grid without anything else
    /// noticing: it is still on screen, still on the same site, still live.
    #[test]
    fn converting_a_volume_pane_away_releases_its_volume() {
        let (mut h, _painter) = volume_harness(StubVolumePainter::painting());
        h.gui_mut().request_pane_kind(1, PaneKind::Map);
        h.frames_for(1, FRAME_DT);

        assert!(
            h.last_actions()
                .iter()
                .any(|a| matches!(a, GuiAction::ReleaseVolume { pane_idx: 1 })),
            "converting away from a 3D pane must release its volume, got {:?}",
            h.last_actions()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
        );
    }

    /// Converting a pane that was never a 3D pane releases nothing.
    ///
    /// The mutation this closes: dropping the `kind() == Volume` half of the
    /// guard leaves a `ReleaseVolume` on every conversion — harmless today, and
    /// a pane releasing a volume another pane is using the moment the store is
    /// keyed any other way.
    #[test]
    fn converting_a_map_pane_releases_nothing() {
        let (mut h, _painter) = volume_harness(StubVolumePainter::painting());
        h.gui_mut().request_pane_kind(0, PaneKind::CrossSection);
        h.frames_for(1, FRAME_DT);
        assert!(
            !h.last_actions()
                .iter()
                .any(|a| matches!(a, GuiAction::ReleaseVolume { .. })),
            "a map pane has no volume to release",
        );
    }

    /// The painter is asked with the camera **after** this frame's drag.
    ///
    /// The trap this closes is not a wrong picture but a *late* one: building
    /// the payload before the UI pass leaves the orbit one frame behind the
    /// pointer, which reads as input lag and gets "fixed" by turning the drag
    /// sensitivity up.
    #[test]
    fn the_painter_sees_the_camera_after_this_frames_drag() {
        let (mut h, painter) = volume_harness(StubVolumePainter::painting());
        let rect = h.pane_rects()[1];

        h.mouse_press(rect.center());
        h.frames_for(1, FRAME_DT);
        h.mouse_move(rect.center() + egui::vec2(120.0, 0.0));
        h.frames_for(1, FRAME_DT);

        let moved = camera_of(&mut h, 1);
        assert_ne!(
            moved,
            crate::pane::OrbitCamera::default(),
            "precondition: the drag must have moved the camera at all",
        );
        assert_eq!(
            last_seen(&painter).camera,
            moved,
            "the painter was handed a stale camera, so the volume lags the pointer by a frame",
        );
        h.mouse_release(rect.center() + egui::vec2(120.0, 0.0));
    }

    /// Dragging turns the box the way the pointer went, in both axes.
    ///
    /// Signs, not arithmetic. A sign error still orbits perfectly smoothly and
    /// merely feels inverted, which is the sort of defect that survives review
    /// and is reported months later as "the 3D view is backwards".
    #[test]
    fn dragging_turns_the_box_the_way_the_pointer_went() {
        for drag in [egui::vec2(120.0, 0.0), egui::vec2(0.0, 120.0)] {
            let (mut h, _painter) = volume_harness(StubVolumePainter::painting());
            let rect = h.pane_rects()[1];
            let before = camera_of(&mut h, 1);

            h.mouse_press(rect.center());
            h.frames_for(1, FRAME_DT);
            h.mouse_move(rect.center() + drag);
            h.frames_for(1, FRAME_DT);
            h.mouse_release(rect.center() + drag);
            h.frames_for(1, FRAME_DT);

            let after = camera_of(&mut h, 1);
            if drag.x != 0.0 {
                assert!(
                    after.yaw_deg() > before.yaw_deg(),
                    "dragging right should raise the eye's bearing: {} -> {}",
                    before.yaw_deg(),
                    after.yaw_deg(),
                );
                assert_eq!(
                    after.pitch_deg(),
                    before.pitch_deg(),
                    "a horizontal drag must not pitch",
                );
            } else {
                assert!(
                    after.pitch_deg() > before.pitch_deg(),
                    "dragging down should raise the eye: {} -> {}",
                    before.pitch_deg(),
                    after.pitch_deg(),
                );
                assert_eq!(
                    after.yaw_deg(),
                    before.yaw_deg(),
                    "a vertical drag must not yaw",
                );
            }
        }
    }

    /// Scrolling over the 3D pane zooms it; scrolling over another pane does
    /// not.
    ///
    /// `Input::zoom_delta` and the scroll delta are **global** — they report the
    /// frame's gesture wherever on screen it happened — so the
    /// `hovered() || dragged()` gate is correctness rather than politeness.
    /// Without it a wheel over a map pane would zoom every 3D pane on screen.
    #[test]
    fn only_a_gesture_over_the_pane_zooms_it() {
        let (mut h, _painter) = volume_harness(StubVolumePainter::painting());
        let rects = h.pane_rects();

        let before = camera_of(&mut h, 1).eye_distance();
        h.scroll_at(rects[0].center(), egui::vec2(0.0, 200.0));
        h.frames_for(2, FRAME_DT);
        assert_eq!(
            camera_of(&mut h, 1).eye_distance(),
            before,
            "a scroll over the map pane must not move the 3D pane's camera",
        );

        h.scroll_at(rects[1].center(), egui::vec2(0.0, 200.0));
        h.frames_for(2, FRAME_DT);
        let after = camera_of(&mut h, 1).eye_distance();
        assert!(
            after < before,
            "scrolling up over the 3D pane should bring the eye in: {before} -> {after}",
        );
    }

    /// The painter is told the pane's size in **physical** pixels, not points.
    ///
    /// The offscreen target is allocated from this number, so handing over
    /// points on a 2x display would allocate a quarter-sized texture and blit it
    /// stretched — which looks like the resolution rung working rather than like
    /// a bug.
    ///
    /// **Run at 2x deliberately.** At the harness's default scale points and
    /// pixels are the same number, so an assertion that multiplies by
    /// `pixels_per_point` passes whether the production code multiplies or not.
    /// The first version of this test did exactly that and could not see the
    /// mutation it is named for.
    #[test]
    fn the_painter_is_told_the_pane_size_in_physical_pixels() {
        let (mut h, painter) = volume_harness(StubVolumePainter::painting());
        h.set_pixels_per_point(2.0);
        h.frames_for(2, FRAME_DT);

        assert_eq!(
            h.pixels_per_point(),
            2.0,
            "precondition: points and pixels must differ, or this proves nothing",
        );
        let rect = h.pane_rects()[1];
        let seen = last_seen(&painter);
        assert_eq!(
            seen.size_px,
            [
                (rect.width() * 2.0).round() as u32,
                (rect.height() * 2.0).round() as u32,
            ],
            "the pane is {} x {} points, so at 2x it is twice that in pixels",
            rect.width(),
            rect.height(),
        );
        assert_eq!(seen.pane_idx, 1);
    }

    /// A long explanation is wrapped inside the pane, not laid out on one line
    /// that runs off both edges.
    ///
    /// Found by looking at the app rather than by reasoning: the 3D pane's
    /// palette refusal is a paragraph, and `Painter::text` centres a single
    /// unwrapped line — so it rendered as a strip of words with the start and
    /// end of every line cut away. That reads as a rendering bug, not as an
    /// explanation, which makes it worse than the empty box it replaced.
    #[test]
    fn a_long_empty_state_is_wrapped_inside_the_pane() {
        let long = "Velocity cannot be drawn as a volume yet. Its colour table is opaque at \
                    the bottom of its scale, so every boundary between measured and unmeasured \
                    air paints, and a volume is mostly unmeasured air.";
        let (h, _painter) = volume_harness(StubVolumePainter::empty(long));
        let pane = h.pane_rects()[1];

        let painted: Vec<_> = h
            .painted_text_rects()
            .into_iter()
            .filter(|(_, text)| text.contains("cannot be drawn"))
            .collect();
        assert_eq!(painted.len(), 1, "the refusal should be painted once");
        let (rect, _) = &painted[0];
        assert!(
            rect.width() <= pane.width(),
            "the message is {} wide in a {} pane, so it runs off both edges",
            rect.width(),
            pane.width(),
        );
        assert!(
            pane.contains_rect(*rect),
            "the message at {rect:?} is not inside its pane {pane:?}",
        );
    }

    /// Whatever the painter says is why the pane is empty is what the pane says.
    ///
    /// The renderer knows things this crate cannot name — a device error latched
    /// mid-session, a single-tilt volume, a grid still building — and every one
    /// of them is a different thing for the user to do about it.
    #[test]
    fn the_painters_own_reason_reaches_the_pane() {
        let (h, _painter) = volume_harness(StubVolumePainter::empty("a very specific reason"));
        assert_eq!(
            h.volume_arms()[0].outcome.as_deref(),
            Some("a very specific reason"),
        );
    }

    // --- The caption: everything the pane claims about the picture ----------

    /// **The height the pane reports is real at every exaggeration.**
    ///
    /// This is the counterweight that makes the exaggeration defensible at all.
    /// The stretch is a drawing convention; a stretched *number* would be a
    /// fabricated measurement, and 0–59 kft MSL is a figure a forecaster would
    /// read off the screen and act on.
    ///
    /// The mutation this closes is the tempting one — multiplying the top of the
    /// box by the exaggeration so the caption "matches what you see". At 3× that
    /// produces "0–177 kft MSL", which is above the Kármán line and still looks
    /// like a readout.
    #[test]
    fn the_height_the_pane_reports_is_real_at_every_exaggeration() {
        let mut seen = Vec::new();
        for ex in [1.0f32, 3.0, 12.0] {
            let mut camera = crate::pane::OrbitCamera::default();
            camera.set_vertical_exaggeration(ex);
            let lines = volume_caption("KTLX", at(33), None, None, camera);
            let height = lines
                .iter()
                .find(|l| l.contains("kft MSL"))
                .unwrap_or_else(|| panic!("no height line at {ex}x in {lines:?}"))
                .clone();
            assert!(
                height.starts_with("0–59 kft MSL"),
                "the height must be the box's true extent, not the drawn one: {height:?}",
            );
            assert!(
                height.contains(&format!("{ex:.1}×")),
                "the exaggeration must be stated beside it: {height:?}",
            );
            seen.push(height);
        }
        assert_eq!(
            seen.iter().filter(|h| h.starts_with("0–59")).count(),
            3,
            "every setting must report the same real height: {seen:?}",
        );
    }

    /// The caption names the archived volume and its time.
    ///
    /// A 3D pane in live mode is showing a volume some minutes behind the plan
    /// view beside it. The one thing that must not happen is for it to look
    /// current, so "archived" and the time are both in the first line.
    #[test]
    fn the_caption_names_the_archived_volume_and_when_it_was_collected() {
        let lines = volume_caption("KTLX", at(33), None, None, Default::default());
        assert!(
            lines[0].contains("KTLX")
                && lines[0].contains("archived")
                && lines[0].contains("22:33"),
            "the first line must name the site, that it is archived, and when: {lines:?}",
        );
    }

    /// When the app has another volume for the site, the pane names its time —
    /// and when it is the same one, it says nothing.
    ///
    /// Both halves matter. Without the first, a user comparing a live plan view
    /// with this pane has to notice two timestamps in opposite corners. Without
    /// the second, the line appears on every ordinary archive view and is
    /// promptly learned to be noise.
    ///
    /// The time is asserted, not just the line's presence: naming the *other*
    /// volume is the whole of what this can honestly say — `shown` is a per-site
    /// timestamp and knows nothing about which panes exist or what kind they are
    /// — and a line that appeared with the wrong number would be worse than no
    /// line at all.
    #[test]
    fn the_caption_names_the_other_volume_the_app_has_for_the_site() {
        let differs = volume_caption("KTLX", at(33), Some(at(39)), None, Default::default());
        let named = differs
            .iter()
            .find(|l| l.contains("22:39"))
            .unwrap_or_else(|| panic!("a divergence must be said out loud: {differs:?}"));
        assert!(
            named.contains("KTLX"),
            "the other volume must be named for a site, or a two-site layout \
             cannot tell whose it is: {named}",
        );
        assert!(
            !named.contains("22:33"),
            "the line must name the volume that is *not* on this pane: {named}",
        );

        let agrees = volume_caption("KTLX", at(33), Some(at(33)), None, Default::default());
        assert_eq!(
            agrees.len(),
            differs.len() - 1,
            "no divergence, no line: {agrees:?}",
        );
        assert!(
            !agrees.iter().any(|l| l.contains("current")),
            "no divergence, no line: {agrees:?}",
        );
    }

    /// The caption reports the resolution the region buys, and it moves with the
    /// region.
    ///
    /// The grid's cell count is fixed, so a tighter box spends the same cells
    /// over less ground — 0.63 km per cell at the default against 0.16 at 20 km.
    /// That is the main reason to pick a region, and it is invisible unless it is
    /// written down.
    #[test]
    fn the_caption_reports_the_resolution_the_region_buys() {
        let wide = volume_caption("KTLX", at(33), None, None, Default::default());
        assert!(
            wide.iter()
                .any(|l| l.contains("160 km box") && l.contains("km/cell")),
            "the default box must report its width and resolution: {wide:?}",
        );

        let tight = crate::pane::VolumeRegion::new(
            crate::pane::GeoPoint {
                lat: 35.3,
                lon: -97.3,
            },
            20.0,
        )
        .expect("a valid region");
        let tight_lines = volume_caption("KTLX", at(33), None, Some(tight), Default::default());
        let line = tight_lines
            .iter()
            .find(|l| l.contains("km box"))
            .expect("a box line");
        assert!(
            line.contains("40 km box"),
            "a 20 km half-width is a 40 km box: {line:?}",
        );
        // The whole point of the feature: a quarter of the width is four times
        // the resolution, and both figures are on screen.
        let cells = rustdar_radar::voxel::default_shape().nx as f64;
        assert!(
            line.contains(&format!("{:.2} km/cell", 40.0 / cells)),
            "the tighter box must report its finer cells: {line:?}",
        );
    }

    // --- The pan gesture ----------------------------------------------------

    /// A secondary drag pans and does not orbit; a primary drag orbits and does
    /// not pan.
    ///
    /// The two are separate verbs on separate buttons, and a mutation that made
    /// either drag do both would still move the picture — plausibly — while
    /// making the other gesture impossible to perform cleanly.
    #[test]
    fn the_secondary_drag_pans_and_the_primary_drag_orbits() {
        let mut h = volume_pane_harness();
        let rect = h.pane_rects()[1];
        let before = camera_of(&mut h, 1);

        h.mouse_press_secondary(rect.center());
        h.frames_for(1, FRAME_DT);
        h.mouse_move(rect.center() + egui::vec2(90.0, 0.0));
        h.frames_for(1, FRAME_DT);
        h.mouse_release_secondary(rect.center() + egui::vec2(90.0, 0.0));
        h.frames_for(1, FRAME_DT);

        let panned = camera_of(&mut h, 1);
        assert_ne!(panned.pivot(), before.pivot(), "a secondary drag must pan");
        assert_eq!(
            (panned.yaw_deg(), panned.pitch_deg()),
            (before.yaw_deg(), before.pitch_deg()),
            "a secondary drag must not orbit",
        );

        let before = panned;
        h.mouse_press(rect.center());
        h.frames_for(1, FRAME_DT);
        h.mouse_move(rect.center() + egui::vec2(90.0, 0.0));
        h.frames_for(1, FRAME_DT);
        h.mouse_release(rect.center() + egui::vec2(90.0, 0.0));
        h.frames_for(1, FRAME_DT);

        let orbited = camera_of(&mut h, 1);
        assert_ne!(
            orbited.yaw_deg(),
            before.yaw_deg(),
            "a primary drag must orbit"
        );
        assert_eq!(
            orbited.pivot(),
            before.pivot(),
            "a primary drag must not pan",
        );
    }

    /// The box travels the way the pointer went.
    ///
    /// Through the whole shipped path rather than through `pan_for_drag` alone,
    /// so a sign inverted between the two — the gesture reading the drag one way
    /// and the maths another — cannot hide.
    #[test]
    fn a_secondary_drag_carries_the_box_the_way_the_pointer_went() {
        let mut h = volume_pane_harness();
        let rect = h.pane_rects()[1];
        // Due south of the box looking north, so screen-right is due east and the
        // axis the pivot moves on is nameable.
        {
            let camera = &mut h
                .gui_mut()
                .pane_mut(1)
                .expect("pane 1")
                .volume_mut()
                .expect("a 3D pane")
                .camera;
            *camera =
                crate::pane::OrbitCamera::restore(180.0, 0.0, 2.5, [0.0; 3], 1.0).expect("finite");
        }
        h.frames_for(1, FRAME_DT);

        h.mouse_press_secondary(rect.center());
        h.frames_for(1, FRAME_DT);
        h.mouse_move(rect.center() + egui::vec2(80.0, 0.0));
        h.frames_for(1, FRAME_DT);

        assert!(
            camera_of(&mut h, 1).pivot()[0] < -1e-4,
            "dragging right must aim west so the box travels east: {:?}",
            camera_of(&mut h, 1).pivot(),
        );
    }

    /// A pane collapsed to nothing by a divider drag does not put a NaN in the
    /// camera.
    ///
    /// The realistic path to a zero viewport height, and the consequence of
    /// laundering it rather than refusing is a staleness key that never equals
    /// itself — a rebuild every frame, for ever, with a hot CPU as its only
    /// symptom.
    #[test]
    fn a_pane_with_no_height_pans_to_nothing_rather_than_to_nan() {
        let mut h = volume_pane_harness();
        let rect = h.pane_rects()[1];
        // The gesture still runs; only the geometry is degenerate.
        let pan = crate::volume_view::pan_for_drag(
            camera_of(&mut h, 1),
            [160.0, 160.0, 18.0],
            0.0,
            [rect.width(), 0.0],
        );
        assert_eq!(pan, None, "a zero-height pane must produce no pan at all");

        let mut camera = camera_of(&mut h, 1);
        camera.nudge(crate::pane::OrbitDelta {
            pan: [f32::NAN, 0.0, 0.0],
            ..Default::default()
        });
        assert!(
            camera.pivot().iter().all(|p| p.is_finite()),
            "a non-finite pan must be refused whole: {:?}",
            camera.pivot(),
        );
    }

    // --- Reset --------------------------------------------------------------

    /// The reset returns the pivot and the region, not only the angles.
    ///
    /// Through `reset_volume_view`, which is what the button calls — a test that
    /// restated the assignments would pass whatever the button actually did.
    ///
    /// Leaving the pivot out is the easy mistake and the one that matters: a
    /// pane panned to its clamp and then reset would visibly change angle and
    /// still be looking at the corner of the box, which reads as a reset that
    /// half-worked.
    #[test]
    fn the_reset_returns_the_pivot_and_the_region_as_well_as_the_angles() {
        let mut volume = crate::pane::VolumePane::default();
        volume.camera.nudge(crate::pane::OrbitDelta {
            yaw_deg: 40.0,
            pitch_deg: -15.0,
            zoom_factor: 1.4,
            pan: [0.6, -0.4, 0.3],
        });
        volume.camera.set_vertical_exaggeration(9.0);
        volume.region = crate::pane::VolumeRegion::new(
            crate::pane::GeoPoint {
                lat: 35.3,
                lon: -97.3,
            },
            25.0,
        );
        volume.source_pane = Some(0);
        assert_ne!(
            volume.camera.pivot(),
            [0.0; 3],
            "precondition: the view has been panned off centre",
        );

        reset_volume_view(&mut volume);

        assert_eq!(
            volume.camera.pivot(),
            [0.0; 3],
            "the pivot must come back, or the box stays off to one side",
        );
        assert_eq!(volume.camera, crate::pane::OrbitCamera::default());
        assert_eq!(volume.region, None, "the region must come back too");
        assert_eq!(
            volume.source_pane, None,
            "and its provenance, or the next drag on that map re-aims this pane",
        );
    }

    /// A region change invalidates the grid; a camera change does not.
    ///
    /// This is the line between the two halves of the feature — the region
    /// changes what is *sampled*, the camera only how it is *drawn* — and it is
    /// the one that costs 155 ms on the frame thread when it is drawn in the
    /// wrong place. Orbiting, panning or exaggerating must not rebuild.
    #[test]
    fn a_region_change_rebuilds_the_grid_and_a_camera_change_does_not() {
        let mut h = volume_pane_harness();
        h.frames_for(2, FRAME_DT);
        // Settle: the pane has asked for its grid and been told it has one.
        let target = h
            .last_actions()
            .iter()
            .find_map(|a| match a {
                GuiAction::PrepareVolume { target, .. } => Some(target.clone()),
                _ => None,
            })
            .expect("a build was asked for");
        h.gui_mut()
            .pane_mut(1)
            .expect("pane 1")
            .volume_mut()
            .expect("a 3D pane")
            .rendered_for = Some(target);
        h.frames_for(2, FRAME_DT);
        assert!(
            !h.last_actions()
                .iter()
                .any(|a| matches!(a, GuiAction::PrepareVolume { .. })),
            "precondition: a settled pane asks for nothing",
        );

        // Moving the camera every way there is.
        {
            let volume = h
                .gui_mut()
                .pane_mut(1)
                .expect("pane 1")
                .volume_mut()
                .expect("a 3D pane");
            volume.camera.nudge(crate::pane::OrbitDelta {
                yaw_deg: 30.0,
                pitch_deg: 10.0,
                zoom_factor: 1.5,
                pan: [0.5, 0.5, 0.5],
            });
            volume.camera.set_vertical_exaggeration(11.0);
        }
        h.frames_for(2, FRAME_DT);
        assert!(
            !h.last_actions()
                .iter()
                .any(|a| matches!(a, GuiAction::PrepareVolume { .. })),
            "orbiting, panning and exaggerating must all redraw from the grid in hand",
        );

        // Changing the region.
        h.gui_mut()
            .pane_mut(1)
            .expect("pane 1")
            .volume_mut()
            .expect("a 3D pane")
            .region = crate::pane::VolumeRegion::new(
            crate::pane::GeoPoint {
                lat: 35.3,
                lon: -97.3,
            },
            25.0,
        );
        h.frames_for(2, FRAME_DT);
        let asked = h
            .last_actions()
            .iter()
            .find_map(|a| match a {
                GuiAction::PrepareVolume { target, .. } => Some(target.clone()),
                _ => None,
            })
            .expect("a new region must trigger a rebuild");
        assert_eq!(
            asked.region.map(|r| r.half_width_km()),
            Some(25.0),
            "the rebuild must be for the region that was picked",
        );
    }

    /// A 3D pane on a 2-pane harness, with an archive volume and a painter.
    fn volume_pane_harness() -> InputHarness {
        let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
        h.set_pane_count(2);
        h.make_pane_volume(1);
        h.gui_mut()
            .set_volume_painter(Some(Arc::new(StubVolumePainter::painting())));
        h.load_scan("KTLX");
        h.frames_for(2, FRAME_DT);
        h
    }

    fn at(minute: u32) -> chrono::NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(2026, 7, 30)
            .expect("a real date")
            .and_hms_opt(22, minute, 0)
            .expect("a real time")
    }

    // --- The region drag, end to end ---------------------------------------

    /// Arming the mode and dragging on a map opens a 3D pane aimed at the ground
    /// that was dragged.
    ///
    /// The whole gesture through the shipped path: menu state, press, drag,
    /// release, deferred apply. Everything below picks at one part of it; this is
    /// the one that proves the parts are joined up.
    #[test]
    fn dragging_on_an_armed_map_aims_a_3d_pane_at_that_ground() {
        let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
        h.load_scan("KTLX");
        h.gui_mut().set_region_arm_for_test(true);
        h.frames_for(1, FRAME_DT);

        let rect = h.pane_rects()[0];
        drag_region(
            &mut h,
            rect.center(),
            rect.center() + egui::vec2(120.0, 0.0),
        );

        assert_eq!(
            h.pane_kinds().len(),
            2,
            "a drag with room in the layout must open a pane beside the map",
        );
        assert_eq!(h.pane_kinds()[1], PaneKind::Volume);
        let volume = h
            .gui_mut()
            .pane(1)
            .expect("the new pane")
            .volume()
            .expect("a 3D pane")
            .clone();
        let region = volume.region.expect("aimed at a region");
        assert!(
            region.half_width_km() >= rustdar_radar::voxel::MIN_HALF_WIDTH_KM,
            "the committed region must be one the resampler will honour: {}",
            region.half_width_km(),
        );
        assert_eq!(
            volume.source_pane,
            Some(0),
            "the pane must remember which map it was aimed from",
        );
    }

    /// A map pane with the region mode already armed.
    fn armed_map() -> InputHarness {
        let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
        h.load_scan("KTLX");
        h.gui_mut().set_region_arm_for_test(true);
        h.frames_for(1, FRAME_DT);
        h
    }

    /// The region the 3D pane a drag opened is aimed at.
    fn aimed_region(h: &mut InputHarness) -> crate::pane::VolumeRegion {
        h.gui_mut()
            .pane(1)
            .expect("the new pane")
            .volume()
            .expect("a 3D pane")
            .region
            .expect("aimed at a region")
    }

    /// **The committed region is centred on the ground the press landed on.**
    ///
    /// Every other test of this gesture presses at `rect.center()` — which is
    /// also the one point at which reading the pane's centre instead of the
    /// pointer cannot be told apart from reading the pointer. A regression there
    /// would centre every user-dragged region on the middle of the map with the
    /// whole suite green, and the symptom is a box near where it was drawn rather
    /// than nowhere, which is exactly the kind of wrongness that gets lived with.
    ///
    /// Pinned against the gesture's own ruler rather than against a projection
    /// this crate would have to re-derive: a drag from P to Q and a drag from Q to
    /// P commit two boxes of the same size, and their centres are exactly that
    /// size apart. Anchoring both presses at the pane's centre makes the
    /// separation zero.
    ///
    /// Two harnesses rather than two drags on one, because the first commit grows
    /// the layout and moves the map's rect out from under the second press.
    #[test]
    fn the_committed_region_is_centred_on_the_ground_the_press_landed_on() {
        let mut first = armed_map();
        let rect = first.pane_rects()[0];
        // Neither end is the pane's centre, and neither shares a coordinate with
        // it: a substitution has to be wrong in both latitude and longitude.
        let p = rect.center() + egui::vec2(-40.0, -25.0);
        let q = rect.center() + egui::vec2(20.0, 18.0);

        drag_region(&mut first, p, q);
        let from_p = aimed_region(&mut first);

        let mut second = armed_map();
        drag_region(&mut second, q, p);
        let from_q = aimed_region(&mut second);

        assert!(
            from_p.half_width_km() > rustdar_radar::voxel::MIN_HALF_WIDTH_KM
                && from_p.half_width_km() < rustdar_radar::voxel::MAX_HALF_WIDTH_KM,
            "precondition: the box must be strictly inside the resampler's clamp, \
             or its size is not a ruler: {} km",
            from_p.half_width_km(),
        );

        // Screen `y` runs down and `x` runs east, so the press up and to the left
        // is the one further north and further west.
        assert!(
            from_p.centre().lat > from_q.centre().lat,
            "the press higher up the pane must aim further north: {:?} vs {:?}",
            from_p.centre(),
            from_q.centre(),
        );
        assert!(
            from_p.centre().lon < from_q.centre().lon,
            "the press further left must aim further west: {:?} vs {:?}",
            from_p.centre(),
            from_q.centre(),
        );

        // And by exactly the ground the drag measured for itself.
        let mut apart = crate::ui_region::RegionDrag::begin(0, from_p.centre())
            .expect("a centre the projector placed on Earth");
        apart.extend_to(from_q.centre());
        assert!(
            (apart.half_width_km() - from_p.half_width_km()).abs() < 1e-6,
            "the two centres must be the box's own width apart — {} km against a \
             box of {} km. A press read at the pane's centre puts both boxes in \
             the same place.",
            apart.half_width_km(),
            from_p.half_width_km(),
        );
    }

    /// The mode stays armed after a commit, and after a discarded mis-drag.
    ///
    /// Aiming a second pane, or re-aiming one that came out wrong, is the normal
    /// next thing a user does. A mode that disarmed itself would make each of
    /// those two menu trips instead of none.
    #[test]
    fn the_mode_stays_armed_through_a_commit_and_through_a_mis_drag() {
        let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
        h.load_scan("KTLX");
        h.gui_mut().set_region_arm_for_test(true);
        h.frames_for(1, FRAME_DT);

        let rect = h.pane_rects()[0];
        drag_region(
            &mut h,
            rect.center(),
            rect.center() + egui::vec2(120.0, 0.0),
        );
        assert!(
            h.gui_mut().region_arm_for_test(),
            "a commit must leave the mode armed",
        );

        // A press and release with no movement at all: the mis-click.
        drag_region(&mut h, rect.center(), rect.center());
        assert!(
            h.gui_mut().region_arm_for_test(),
            "a discarded drag must leave the mode armed",
        );
    }

    /// A mis-drag commits nothing — it does not open a pane and does not re-aim
    /// one.
    #[test]
    fn a_mis_drag_leaves_the_layout_and_the_region_alone() {
        let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
        h.load_scan("KTLX");
        h.gui_mut().set_region_arm_for_test(true);
        h.frames_for(1, FRAME_DT);

        let rect = h.pane_rects()[0];
        let before = h.pane_kinds();
        // Two points apart, which at any plausible zoom is far under 10 km.
        drag_region(&mut h, rect.center(), rect.center() + egui::vec2(2.0, 0.0));

        assert_eq!(
            h.pane_kinds(),
            before,
            "a drag below the resampler's minimum must change nothing",
        );
    }

    /// **The anchor is the ground, not the pixel.**
    ///
    /// Pan is suppressed while armed but zoom is not, so a wheel notch mid-drag
    /// moves every pixel of the map while the ground stays where it is. A pixel
    /// anchor would silently re-aim the box to whatever is now under the old
    /// coordinate; a geographic one cannot.
    #[test]
    fn a_mid_drag_zoom_does_not_move_the_region_it_is_anchored_to() {
        let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
        h.load_scan("KTLX");
        h.gui_mut().set_region_arm_for_test(true);
        h.frames_for(1, FRAME_DT);
        let rect = h.pane_rects()[0];

        // A drag with no zoom in it, for the baseline.
        drag_region(
            &mut h,
            rect.center(),
            rect.center() + egui::vec2(120.0, 0.0),
        );
        let plain = h
            .gui_mut()
            .pane(1)
            .expect("the new pane")
            .volume()
            .expect("a 3D pane")
            .region
            .expect("aimed");

        // The same drag, with the map zoomed under it between press and release.
        let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
        h.load_scan("KTLX");
        h.gui_mut().set_region_arm_for_test(true);
        h.frames_for(1, FRAME_DT);
        h.mouse_press(rect.center());
        h.frames_for(1, FRAME_DT);
        h.scroll_at(rect.center(), egui::vec2(0.0, 40.0));
        h.frames_for(2, FRAME_DT);
        h.mouse_move(rect.center() + egui::vec2(120.0, 0.0));
        h.frames_for(1, FRAME_DT);
        h.mouse_release(rect.center() + egui::vec2(120.0, 0.0));
        h.frames_for(2, FRAME_DT);
        let zoomed = h
            .gui_mut()
            .pane(1)
            .expect("the new pane")
            .volume()
            .expect("a 3D pane")
            .region
            .expect("aimed");

        assert!(
            (plain.centre().lat - zoomed.centre().lat).abs() < 1e-9
                && (plain.centre().lon - zoomed.centre().lon).abs() < 1e-9,
            "the centre is the ground the press landed on and a zoom must not move it: \
             {:?} vs {:?}",
            plain.centre(),
            zoomed.centre(),
        );
    }

    /// While armed, the map does not pan and a click does not switch site.
    ///
    /// Both are unconditional — from the moment the mode is on, not from the
    /// moment a drag is recognised — because a press that will become a region
    /// drag is indistinguishable from one that will become a pan until the
    /// pointer moves, and by then the map has slid under the anchor.
    #[test]
    fn arming_the_mode_takes_the_drag_and_the_click_away_from_the_map() {
        let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
        h.load_scan("KTLX");
        h.frames_for(1, FRAME_DT);
        assert!(
            !h.frame().resolved.suppress_pan,
            "precondition: an unarmed map pans normally",
        );

        h.gui_mut().set_region_arm_for_test(true);
        h.frames_for(1, FRAME_DT);
        assert!(
            h.frame().resolved.suppress_pan,
            "arming the mode must take the pan away at once, before any drag",
        );

        let rect = h.pane_rects()[0];
        h.mouse_click(rect.center());
        h.frames_for(2, FRAME_DT);
        assert!(
            !h.last_actions()
                .iter()
                .any(|a| matches!(a, GuiAction::SwitchRadarSite { .. })),
            "a press while armed is a region gesture, not a click on the map",
        );
    }

    /// A committed region is drawn on the map it came from, and only on that
    /// one.
    ///
    /// A 3D pane whose box is invisible on the map is one the user cannot tell
    /// the provenance of — "where is this volume from" has no answer on screen.
    /// Drawing it on *every* map would be worse than not drawing it: two panes on
    /// different sites would each claim the other's box.
    #[test]
    fn a_committed_region_is_drawn_on_the_map_it_came_from() {
        let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
        h.set_pane_count(3);
        h.load_scan("KTLX");
        h.gui_mut().set_region_arm_for_test(true);
        h.frames_for(1, FRAME_DT);

        let source = h.pane_rects()[0];
        drag_region(
            &mut h,
            source.center(),
            source.center() + egui::vec2(120.0, 0.0),
        );
        h.gui_mut().set_region_arm_for_test(false);
        h.frames_for(2, FRAME_DT);

        // A stroked square whose two sides are within a point of each other,
        // sitting inside the source pane. Classified by geometry rather than by
        // colour, the way `color_scale_strips` classifies its bars.
        let square_in = |h: &mut InputHarness, pane: egui::Rect| {
            h.painted_rects()
                .iter()
                .filter(|r| {
                    pane.contains(r.center())
                        && r.width() > 8.0
                        && (r.width() - r.height()).abs() < 1.0
                })
                .count()
        };
        let others: Vec<egui::Rect> = h.pane_rects()[1..].to_vec();
        assert!(
            square_in(&mut h, source) > 0,
            "the region must be drawn on the map it was dragged on",
        );
        for (idx, rect) in others.iter().enumerate() {
            // Pane 1 became the 3D view; pane 2 is another map and must be clean.
            if h.pane_kinds()[idx + 1] != PaneKind::Map {
                continue;
            }
            assert_eq!(
                square_in(&mut h, *rect),
                0,
                "a map that did not produce the region must not draw it",
            );
        }
    }

    /// Press, drag and release on a map pane, then let the deferred apply run.
    fn drag_region(h: &mut InputHarness, from: egui::Pos2, to: egui::Pos2) {
        h.mouse_press(from);
        h.frames_for(1, FRAME_DT);
        h.mouse_move(to);
        h.frames_for(1, FRAME_DT);
        h.mouse_release(to);
        // Two frames: the commit is recorded on the release frame and applied
        // after that frame's pane loop, so the pane only reads as changed on the
        // next one.
        h.frames_for(2, FRAME_DT);
    }

    /// While armed, no click reaches the map's own handlers at all.
    ///
    /// Asserted on `overlay_click_pos` rather than on a downstream action,
    /// because that field is the *convention*: every map click handler consumes
    /// it, so nulling it is what takes the click away from all of them at once
    /// — including the ones added after this feature. A test that only checked
    /// that no site was switched would pass with the gate removed, because the
    /// radar-sites overlay is off by default.
    #[test]
    fn while_armed_no_click_reaches_the_maps_own_handlers() {
        let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
        h.load_scan("KTLX");
        let rect = h.pane_rects()[0];

        // Press and release on separate frames, as a pointer really does: egui
        // reports the click on the frame the button comes back up.
        h.mouse_press(rect.center());
        h.frames_for(1, FRAME_DT);
        h.mouse_release(rect.center());
        let unarmed = h.frame();
        assert!(
            unarmed.resolved.overlay_click_pos.is_some(),
            "precondition: an unarmed map delivers its clicks",
        );

        h.gui_mut().set_region_arm_for_test(true);
        h.frames_for(1, FRAME_DT);
        h.mouse_press(rect.center());
        h.frames_for(1, FRAME_DT);
        h.mouse_release(rect.center());
        let armed = h.frame();
        assert_eq!(
            armed.resolved.overlay_click_pos, None,
            "a press while armed is a region gesture and must reach no map handler",
        );
    }

    /// A discarded drag leaves nothing drawn on the map.
    ///
    /// The mutation this closes is forgetting to clear the in-flight drag on
    /// release. Nothing breaks immediately — the next press overwrites it — but
    /// the preview box stays painted over the map for as long as the mode is
    /// armed, which looks exactly like a committed region that was never
    /// committed.
    #[test]
    fn a_discarded_drag_leaves_no_box_behind_on_the_map() {
        let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
        h.load_scan("KTLX");
        h.gui_mut().set_region_arm_for_test(true);
        h.frames_for(1, FRAME_DT);
        let rect = h.pane_rects()[0];

        // Big enough to be drawn while in flight, small enough to be discarded.
        // 6 points is well under 10 km at this zoom.
        h.mouse_press(rect.center());
        h.frames_for(1, FRAME_DT);
        h.mouse_move(rect.center() + egui::vec2(6.0, 0.0));
        h.frames_for(1, FRAME_DT);
        h.mouse_release(rect.center() + egui::vec2(6.0, 0.0));
        h.frames_for(3, FRAME_DT);

        // Nothing square and region-sized anywhere near where the drag was. The
        // preview is a stroked square centred on the press, so it would sit
        // right here if it had survived.
        let squares = h
            .painted_rects()
            .iter()
            .filter(|r| {
                (r.width() - r.height()).abs() < 1.0
                    && r.width() > 2.0
                    && r.center().distance(rect.center()) < 40.0
            })
            .count();
        assert_eq!(
            squares, 0,
            "a drag that committed nothing must leave nothing drawn",
        );
    }
}
