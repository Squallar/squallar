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

                    let overlay_click_pos = pointer.overlay_click_pos;
                    let suppress_pan = pointer.suppress_pan;

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
                            let outcome = render_volume_pane(
                                &mut child_ui,
                                pane_rect,
                                pane_idx,
                                &mut pane,
                                painter.as_deref(),
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
    actions: &mut Vec<GuiAction>,
) -> Option<String> {
    let outcome = volume_pane_outcome(ui, pane_rect, pane_idx, pane, painter, actions);
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
    actions: &mut Vec<GuiAction>,
) -> Option<String> {
    use crate::pane::{OrbitDelta, VolumeStamp, VolumeTarget};
    use crate::volume_view::{VolumeFrameState, VolumePaint};

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
    if response.dragged() {
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
    let stamp = pane
        .scan_info
        .as_ref()
        .map(|scan_info| VolumeStamp {
            site: scan_info.site.name.to_string(),
            collected: scan_info.timestamp,
        });

    // Unreachable from the kind branch, which only enters here for a `Volume`
    // pane, and answered rather than unwrapped: this function takes a whole
    // `PaneState` and is the sort of thing a future caller invokes from
    // somewhere else.
    let Some(volume) = pane.volume_mut() else {
        return Some(VOLUME_EMPTY_STATE.to_owned());
    };
    volume.camera.nudge(delta);
    let camera = volume.camera;
    let already_rendered = volume.rendered_for.clone();

    // Everything below is a reason there is no picture, in the order the user
    // can act on them.
    let Some(painter) = painter else {
        return Some(VOLUME_EMPTY_STATE.to_owned());
    };
    let Some(volume_stamp) = stamp else {
        return Some(format!("Waiting for a volume from {site_code}"));
    };
    if rustdar_radar::sampler::samplable(product).is_none() {
        return Some(format!(
            "{} has no vertical structure to render in 3D — pick a moment the radar measures \
             directly",
            product.name(),
        ));
    }

    let target = VolumeTarget {
        volume: volume_stamp,
        product,
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
            None
        }
        VolumePaint::Empty(why) => Some(why),
    }
}

fn paint_pane_empty_state(ui: &mut egui::Ui, pane_rect: egui::Rect, text: &str) {
    ui.painter().text(
        pane_rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        egui::FontId::proportional(14.0),
        ui.visuals().weak_text_color(),
    );
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
