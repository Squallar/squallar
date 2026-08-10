use crate::actions::GuiAction;
use crate::pane::PaneKind;
use rustdar_overlays::render::overlay_state::OverlayKind;
use rustdar_radar::beam;
use rustdar_radar::types::{IMAGE_SIZE, RadarProduct};
use rustdar_units::UserPreferences;

#[path = "ui_map_pane.rs"]
mod pane_render;

#[path = "ui_section_pane.rs"]
pub(crate) mod section_render;

#[path = "ui_volume_alpha.rs"]
pub(crate) mod volume_alpha_editor;

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

/// The header over the 3D pane's sidebar block. Icon, two spaces, name — the
/// same shape as [`super::SECTION_SIDEBAR_HEADER`] and the overlay rows'
/// labels, which is what keeps the block reading as part of the one panel.
pub(crate) const VOLUME_SIDEBAR_HEADER: &str = "\u{26f6}  3D view";

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

                // Whether some feature consumed this frame's confirmed map
                // click — an overlay polygon hit, a radar-site icon. One flag
                // for the whole pane loop, threaded through `PaneRenderCtx`:
                // the fade trigger is "an unconsumed click on the
                // already-active pane", and this is the consumption half of
                // it (`ui_fade.rs`).
                let mut click_consumed = false;
                // ...and the rest of the fade sentence, recorded pane by pane
                // below: a confirmed click on the already-active *map* pane
                // that no dialog outranks. Folded with the consumption
                // verdict after the loop — consumption is decided by the
                // handlers that run after the candidate is spotted.
                let mut fade_candidate = false;

                // Rects of chrome painted over the map with no layer of its
                // own. Clicks there must not become overlay polygon hit-tests.
                // The list is empty since the top bar replaced the hamburger,
                // but the plumbing stays warm for the next painted-in-pane
                // chrome — see `ShellOutput::excluded_rects`.
                //
                // Supplied by the chrome that drew them rather than rebuilt
                // here from a second copy of its position constants — the two
                // copies could disagree silently, leaving a dead zone at the
                // old position and a live one under the widget.

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
                    // And every handler that ACTS on overlay_click_pos MUST set
                    // `*ctx.click_consumed = true` when it does, so the fade
                    // (`ui_fade.rs`) can tell a click a feature answered from one
                    // that fell through to the bare map. Current consumers: the overlay feature
                    // hit-testing (where `selected_overlays` is pushed) and the
                    // radar-site icon clicks, both in `ui_map_pane.rs`.
                    //
                    // While the cross-section draw is armed, the active *map*
                    // pane resolves through the line detector instead — a third
                    // resolver, not a filter over the other two. The two touch
                    // gestures are spelled with exactly the press-and-move a
                    // section line is spelled with, so running them alongside it
                    // would make every line drawn on a phone also a zoom or a
                    // value tooltip.
                    //
                    // Only a map pane: the line is aimed with a projector, and a
                    // section or volume pane has none. Arming the mode with one
                    // of those active therefore leaves it exactly as it was,
                    // and the press that picks a map pane out of the layout is
                    // the same press that starts the line — `detect_active_pane_click`
                    // runs at the top of this frame.
                    let armed_draw = self.section_draw_armed()
                        && is_active
                        && matches!(pane.kind(), PaneKind::Map);
                    let (pointer, gesture) = if armed_draw {
                        let armed = self.interaction.resolve_armed(&ctx, modality);
                        (armed.pointer(), Some(armed.gesture()))
                    } else if is_active {
                        (
                            self.interaction.resolve_active(
                                &ctx,
                                modality,
                                &mut map_memory,
                                pane_rect,
                            ),
                            None,
                        )
                    } else {
                        (self.interaction.resolve_inactive(&ctx, modality), None)
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
                    //
                    // # The two armed modes never overlap here
                    //
                    // `region_arm` and `armed_draw` gate the same two fields, and
                    // this composes with the block above rather than fighting it:
                    // `ArmedSectionFrame` has already set `suppress_pan` and
                    // cleared `overlay_click_pos`, so an armed *section* frame
                    // passes through unchanged, and the `||` cannot un-suppress
                    // anything. It is not merely that the two agree, though — they
                    // are mutually exclusive at the source. Arming either mode
                    // disarms the other (`Gui::set_region_arm`), because one drag
                    // on one map pane cannot be both a line and a box: the section
                    // pipeline would anchor a line while `handle_region_drag`,
                    // which reads the pointer raw inside `Map::show`, started a
                    // box from the same press, and the release would commit both.
                    // The two gates below are therefore never *both* the reason a
                    // field is cleared, and only one of `pending_section_line` and
                    // `pending_region` can be recorded in a frame — which is what
                    // `Gui::ui`'s two appliers rely on.
                    let overlay_click_pos = if region_arm {
                        None
                    } else {
                        pointer.overlay_click_pos
                    };
                    // A confirmed map tap puts a touch-revealed pill row
                    // back to sleep: the reveal was granted for a glance at
                    // this pane's controls, and a tap that reached the map
                    // is the user working the map again. A tap on the row
                    // itself never gets here — the pills are an egui layer,
                    // and the click gate above already dropped it.
                    if overlay_click_pos.is_some() {
                        self.pill_revealed = None;
                    }
                    // The third reason a map pane's pan is suppressed, and the
                    // only unarmed one: a drag on a section handle. Two halves,
                    // because the decision is needed *now* and the authoritative
                    // hit-test lives inside `Map::show` where the projector is:
                    //
                    // * a drag already in flight on this pane owns the pointer
                    //   until it ends, wherever the pointer wanders;
                    // * a press landing on a handle **this frame** is caught
                    //   against last frame's recorded handle positions
                    //   (`Gui::section_handles`), because by the time the
                    //   projector can confirm it, walkers has already read the
                    //   press with panning enabled — and a press frame that
                    //   pans is a map that slides out from under the grab.
                    //
                    // Deliberately not gated on `is_active`: the handles belong
                    // to the pane, not to the focus, and the press that grabs
                    // one is the same press `detect_active_pane_click` used to
                    // focus the pane at the top of this frame.
                    let section_editing = self
                        .section_edit_drag
                        .as_ref()
                        .is_some_and(|d| d.map_pane == pane_idx);
                    let handle_press = !armed_draw
                        && !region_arm
                        && !section_editing
                        && self.section_handle_pressed(&ctx, pane_idx);
                    let suppress_pan =
                        pointer.suppress_pan || region_arm || section_editing || handle_press;

                    // The fade gesture (plan §1.8, `ui_fade.rs`): a confirmed
                    // click on the already-active map pane's own rect. The
                    // resolvers upstream have already made it a *click* (drags
                    // discarded) off every floating layer, and the armed
                    // modes never deliver one (`region_arm` clears it above,
                    // the armed-draw resolver never reports one) — the
                    // remaining conditions are spelled here: the pane is
                    // active and was active before this press
                    // (`fade_gesture_allowed` checks the press record), the
                    // click is inside this pane, no section-handle gesture
                    // owns it, no feature popup was up to be dismissed by it
                    // (`pointer_available`), and no dialog outranks it.
                    // Map panes only: the fade is a gesture on the *map*
                    // (§1.8's wording), and a click on a 3D or section pane
                    // is that pane's own business.
                    if is_active
                        && pointer_available
                        && matches!(pane.kind(), PaneKind::Map)
                        && !section_editing
                        && !handle_press
                        && self.fade_gesture_allowed()
                        && overlay_click_pos.is_some_and(|pos| pane_rect.contains(pos))
                    {
                        fade_candidate = true;
                    }

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
                    // has in `ui_shell.rs`'s pass, and it has the same fix: read the
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

                                        // Inside `Map::show`, because this is
                                        // the only place a projector exists —
                                        // and on the frame the gesture happens,
                                        // because a pixel names different ground
                                        // one wheel notch later. See
                                        // `SectionAnchor`.
                                        if let Some(gesture) = gesture {
                                            self.track_section_draw(pane_idx, gesture, projector);
                                        }

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
                                            click_consumed: &mut click_consumed,
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

                                        // Before the tracks are drawn, so the
                                        // preview this frame paints is the one
                                        // this frame's pointer produced. Inside
                                        // `Map::show` for the same reason the
                                        // armed draw is: the projector is the
                                        // only thing that can turn a pointer
                                        // into ground, and the handles' screen
                                        // positions are recorded here for the
                                        // next frame's pan-suppression call.
                                        self.track_section_edit(
                                            ui,
                                            projector,
                                            pane_idx,
                                            pane_rect,
                                            excluded_rects,
                                        );

                                        // Last, over the radar image and every
                                        // overlay: a section line the user is
                                        // dragging that disappeared under a
                                        // storm would be undrawable exactly
                                        // where it matters.
                                        self.draw_section_tracks(
                                            ui, projector, pane_idx, pane_rect,
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
                            section_render::render_cross_section(
                                &mut child_ui,
                                &mut pane,
                                pane_rect,
                                horizontal_color_scale,
                                &self.preferences,
                            );
                            // The plan view's own colour bar, reused verbatim:
                            // a section and a map of the same moment are the
                            // same scale, and two spellings of one legend is
                            // how they come to disagree.
                            pane_render::render_color_scale(
                                child_ui.painter(),
                                pane_rect,
                                horizontal_color_scale,
                                &pane,
                                &self.preferences,
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
                            let current_stamp = self.current_volume_for(&pane.site);
                            // The fade factor, read once for the pane-borne
                            // chrome inside: the Volume Alpha corner button
                            // is floating chrome over the picture and fades
                            // with the rest of it (§1.8 — the M8 addition).
                            let chrome = self.chrome_fade();
                            let outcome = render_volume_pane(
                                &mut child_ui,
                                pane_rect,
                                pane_idx,
                                &mut pane,
                                painter.as_deref(),
                                current_stamp,
                                chrome,
                                &mut actions,
                                &mut self.volume_alpha,
                                &self.volume_iso,
                                #[cfg(test)]
                                &mut self.last_alpha_buttons,
                            );
                            #[cfg(test)]
                            self.last_volume_arms
                                .push(VolumeArmProbe { pane_idx, outcome });
                            #[cfg(not(test))]
                            let _ = outcome;
                        }
                    }

                    // The armed-tool hint chip (plan §1.7): while a modal
                    // drag is armed, the active map pane says what the drag
                    // will do — centred, painted, non-interactive, in the
                    // armed mode's own colours. Only the active map pane: it
                    // is the pane the arm's gesture is aimed at, and a chip
                    // on every pane would read as five armed modes. Painted
                    // on its own sub-layer, like the pending-render notice,
                    // so nothing drawn later in the pane can cover it.
                    if is_active && matches!(pane.kind(), PaneKind::Map) {
                        if region_arm {
                            paint_armed_hint_chip(
                                &ctx,
                                pane_idx,
                                pane_rect,
                                &region_arm_hint(),
                                // The armed region drag's own box colour.
                                crate::ui_region::REGION_ARM_COLOR,
                            );
                        } else if self.section_draw_armed() {
                            paint_armed_hint_chip(
                                &ctx,
                                pane_idx,
                                pane_rect,
                                SECTION_ARM_HINT,
                                SECTION_TRACK_COLOR,
                            );
                        }
                    }

                    // Restore map_memory and pane
                    pane.map_memory = map_memory;
                    self.panes[pane_idx] = pane;

                    if pane_count > 1 {
                        let painted = draw_pane_border(ui, pane_rect, is_active);
                        #[cfg(test)]
                        self.last_pane_borders.push((pane_idx, painted, is_active));
                        #[cfg(not(test))]
                        let _ = painted;
                    }
                } // end pane loop

                // What the loop's consumers decided, folded into the fade
                // verdict: a click a feature answered is not a fade gesture,
                // and the consumption flag itself is what a consumed click
                // *while* faded unfades on (`Gui::apply_fade_toggle`).
                self.click_consumed_frame = click_consumed;
                self.fade_candidate = fade_candidate && !click_consumed;

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

    /// Advance the armed cross-section draw by one frame's gesture.
    ///
    /// Called from inside `Map::show`, which is the only place a
    /// `walkers::Projector` exists — and therefore the only place a pointer
    /// position can be turned into ground. Both conversions happen on the frame
    /// their gesture happened, for the reason [`SectionAnchor`] gives at length:
    /// the draw suppresses panning but not zooming, so a pixel held across a
    /// wheel notch names somewhere else.
    ///
    /// [`SectionAnchor`]: super::SectionAnchor
    fn track_section_draw(
        &mut self,
        pane_idx: usize,
        gesture: crate::ui_input::SectionGesture,
        projector: &walkers::Projector,
    ) {
        use crate::ui_input::{MIN_SECTION_DRAG_PT, SectionGesture};

        let ground = |pos: egui::Pos2| {
            // `walkers::Position` is a `geo_types::Point`, so latitude is `y`
            // and longitude is `x` — the same reading `render_pane_map_content`
            // takes off `unproject`.
            let position = projector.unproject(egui::vec2(pos.x, pos.y));
            crate::pane::GeoPoint {
                lat: position.y(),
                lon: position.x(),
            }
        };

        match gesture {
            SectionGesture::Idle => {}
            SectionGesture::Anchored(pos) => {
                self.section_anchor = Some(super::SectionAnchor {
                    pane_idx,
                    ground: ground(pos),
                    screen: pos,
                    current: pos,
                });
            }
            SectionGesture::Dragging(pos) => {
                if let Some(anchor) = self.section_anchor.as_mut()
                    && anchor.pane_idx == pane_idx
                {
                    anchor.current = pos;
                }
            }
            SectionGesture::Released(pos) => {
                let Some(anchor) = self.section_anchor.take() else {
                    return;
                };
                if anchor.pane_idx != pane_idx {
                    return;
                }
                // The length test is on the *gesture*, in points, so it means
                // the same thing at every zoom and on every display density.
                if (pos - anchor.screen).length() < MIN_SECTION_DRAG_PT {
                    // Discarded, and **the mode stays armed**. A stray tap is
                    // the likeliest thing to happen right after arming — it is
                    // how a user checks which pane they are on — and disarming
                    // there would silently throw away an intent they had just
                    // expressed, leaving them to work out from nothing that the
                    // checkbox had un-ticked itself.
                    return;
                }
                let Some(line) = crate::pane::SectionLine::new(anchor.ground, ground(pos)) else {
                    // Only reachable from a projector answering outside the
                    // world, which `walkers` does not do — but the constructor
                    // is the one gate on a line that cannot be cut, so the
                    // refusal is honoured rather than unwrapped. The mode stays
                    // armed, as for any other discarded drag.
                    log::warn!("a drawn section line was not a line; discarding it");
                    return;
                };
                self.pending_section_line = Some((pane_idx, line));
                // Disarmed by drawing: the mode's job is done, and leaving it on
                // would turn the user's next pan into a second section.
                self.set_section_draw_armed(false);
            }
            SectionGesture::Cancelled => {
                if self
                    .section_anchor
                    .as_ref()
                    .is_some_and(|a| a.pane_idx == pane_idx)
                {
                    self.section_anchor = None;
                }
            }
        }
    }

    /// Whether this frame's press landed on a section handle recorded last
    /// frame — the press-frame half of the pan-suppression rule; see the call
    /// site in `render_panes`.
    ///
    /// Against **last** frame's positions because the projector that could
    /// confirm them does not exist yet this frame. One frame of staleness is
    /// harmless for a press (a pointer about to press is not flinging the
    /// viewport), and the worst case of a miss is one frame of pan under a
    /// grab that the in-flight arm then suppresses.
    fn section_handle_pressed(&self, ctx: &egui::Context, pane_idx: usize) -> bool {
        let Some(pos) = ctx.input(|i| {
            if i.pointer.primary_pressed() {
                i.pointer.interact_pos()
            } else {
                None
            }
        }) else {
            return false;
        };
        self.section_handles
            .iter()
            .any(|zone| zone.map_pane == pane_idx && zone.grab_at(pos).is_some())
    }

    /// Advance the unarmed endpoint drag on this pane by one frame, and record
    /// where this pane's handles are for the next frame's press test.
    ///
    /// # Why the pointer is read raw here
    ///
    /// The same answer `handle_region_drag` gives: the click convention is
    /// about *clicks*, and a drag is a press frame, a stream of moved frames
    /// and a release frame, none of which a confirmed-tap position can
    /// express. The dialog gate the convention enforces is applied explicitly
    /// on the press, via [`is_pos_blocked`].
    ///
    /// # What each frame kind does
    ///
    /// * **Press** on a handle (inside this pane, not over a dialog, no drag
    ///   already in flight) begins a drag carrying the line as it stands.
    /// * **Moved** frames re-anchor the grabbed end to the ground under the
    ///   pointer — and only moved frames: a zoom-only frame must not slide
    ///   the endpoint to whatever ground its pixel names now
    ///   ([`SectionEditDrag::pointer_moved`]).
    /// * **Release** commits through [`SectionEditDrag::commit`] — which
    ///   refuses an unchanged or under-length line — into
    ///   `pending_section_edit`, applied after the pane loop. The pane's
    ///   stored line is untouched until then, which is the whole re-cut-on-
    ///   drop contract: the staleness key carries the line, so nothing is
    ///   extracted while the drag is in flight.
    /// * **A pointer that vanishes** (touch cancel, cursor leaving the
    ///   window) drops the drag and the preview with it.
    ///
    /// While either armed modal drag is on, this records nothing and advances
    /// nothing: the armed mode owns the pane's gestures, and it was asked for
    /// last (both setters also clear any drag in flight).
    ///
    /// [`SectionEditDrag::commit`]: crate::ui_section_edit::SectionEditDrag::commit
    /// [`SectionEditDrag::pointer_moved`]: crate::ui_section_edit::SectionEditDrag::pointer_moved
    /// [`is_pos_blocked`]: super::map_overlays::is_pos_blocked
    fn track_section_edit(
        &mut self,
        ui: &egui::Ui,
        projector: &walkers::Projector,
        pane_idx: usize,
        pane_rect: egui::Rect,
        excluded_rects: &[egui::Rect],
    ) {
        use crate::ui_section_edit::{SectionEditDrag, SectionGrabZone};

        // Re-recorded every frame, armed or not: zones left over from before a
        // mode change would go on suppressing pans near handles that are no
        // longer live.
        self.section_handles.retain(|z| z.map_pane != pane_idx);
        if self.section_draw_armed || self.region_arm {
            return;
        }

        let project =
            |p: crate::pane::GeoPoint| projector.project(walkers::lat_lon(p.lat, p.lon)).to_pos2();
        // Every committed line this map owns, with its projected geometry —
        // the same polyline the track is drawn from, so what is grabbable is
        // exactly what is visible. Reading `self.panes` mid-loop is safe here
        // for the reason `draw_section_tracks` gives: the taken slot reads as
        // a default map pane, which has no cross-section to offer.
        let lines: Vec<(usize, crate::pane::SectionLine)> = self
            .panes()
            .iter()
            .enumerate()
            .filter_map(|(idx, other)| {
                let section = other.cross_section()?;
                if section.source_pane != Some(pane_idx) {
                    return None;
                }
                Some((idx, section.line?))
            })
            .collect();
        let zones: Vec<(SectionGrabZone, crate::pane::SectionLine)> = lines
            .into_iter()
            .map(|(section_pane, line)| {
                let track = great_circle_track(line, project);
                (
                    SectionGrabZone {
                        map_pane: pane_idx,
                        section_pane,
                        a_px: track[0],
                        b_px: track[track.len() - 1],
                        track,
                    },
                    line,
                )
            })
            .collect();
        self.section_handles
            .extend(zones.iter().map(|(zone, _)| zone.clone()));

        let (pressed, down, released, pos, shift) = ui.ctx().input(|i| {
            (
                i.pointer.primary_pressed(),
                i.pointer.primary_down(),
                i.pointer.primary_released(),
                i.pointer.interact_pos(),
                i.modifiers.shift,
            )
        });
        let ground = |p: egui::Pos2| {
            let position = projector.unproject(egui::vec2(p.x, p.y));
            crate::pane::GeoPoint {
                lat: position.y(),
                lon: position.x(),
            }
        };

        // The press frame. Gated on this pane's own rect and on the dialog
        // layer, exactly as the region drag's press is. Shift at the press
        // picks the body drag's verb — sweep about the midpoint instead of
        // translate — and is latched into the drag: see
        // `SectionEditDrag::begin`.
        if pressed
            && self.section_edit_drag.is_none()
            && let Some(pos) = pos
            && pane_rect.contains(pos)
            && !super::map_overlays::is_pos_blocked(ui.ctx(), pos, pane_rect, excluded_rects)
        {
            for (zone, line) in &zones {
                if let Some(grab) = zone.grab_at(pos) {
                    self.section_edit_drag = Some(SectionEditDrag::begin(
                        pane_idx,
                        zone.section_pane,
                        grab,
                        *line,
                        pos,
                        ground(pos),
                        shift,
                    ));
                    break;
                }
            }
        }

        // Everything below belongs to *this* pane's drag only, and — like the
        // region drag — it is not gated on the pane's rect: dragging an
        // endpoint past a pane edge is ordinary, and stopping there would read
        // as the gesture being dropped.
        let Some(drag) = self
            .section_edit_drag
            .as_mut()
            .filter(|d| d.map_pane == pane_idx)
        else {
            return;
        };

        if (down || released)
            && let Some(pos) = pos
            && drag.pointer_moved(pos)
        {
            drag.drag_to(pos, ground(pos));
        }

        if released {
            let drag = self
                .section_edit_drag
                .take()
                .expect("filtered Some above; nothing between takes it");
            if let Some(line) = drag.commit() {
                self.pending_section_edit = Some((drag.section_pane, line));
            }
        } else if !down {
            // The pointer went away without releasing — a cancelled touch, or
            // the cursor leaving the window. The preview dies with the drag;
            // the pane's line was never touched.
            self.section_edit_drag = None;
        }
    }

    /// Draw the rubber band of an in-flight draw and the ground track of every
    /// section cut from this map.
    ///
    /// # The track is ~258 m inside the range ring, deliberately
    ///
    /// `render_radar_range_ring` places its circle with `MAX_RANGE_KM / 111.32`
    /// degrees of latitude, and 111.32 km per degree is a sphere of 6378.1 km —
    /// the WGS84 equatorial radius. The section's geometry, and this track with
    /// it, walks [`rustdar_radar::types::EARTH_RADIUS_KM`], which is 6371. So a
    /// track drawn all the way to the edge of coverage lands
    /// 230 × (1 − 6371/6378.1) ≈ 0.26 km inside the ring, which is 1.15 px at
    /// the zoom where the whole ring fits a 2048-pixel pane.
    ///
    /// Measured rather than assumed, and left alone rather than reconciled:
    /// changing either constant to match the other moves a number that is
    /// correct for what it describes. The ring is a rendering convenience; the
    /// track is where the beam went.
    ///
    /// # And the track is a polyline, because the cut is a great circle
    ///
    /// A straight segment between the two projected endpoints is a **rhumb
    /// line**: straight in Web Mercator is constant bearing, not shortest path.
    /// The section is cut along a great circle
    /// ([`rustdar_radar::beam::great_circle_point`], the same walk
    /// `tilt_curves` samples), and the two part company in the middle. Measured
    /// on a 229 km line at 41 °N — a full-range line at the latitude of the
    /// northern-tier sites — the peak separation is **894 m** running east-west
    /// and 907 m running north-east. That is ~3.5× the 258 m ring offset above,
    /// which has a doc block of its own, and about 2.9 px at a zoom filling the
    /// pane. It is also the one error a user is placed to notice, because the
    /// track is drawn over the echo the section was aimed at.
    ///
    /// So the track is subdivided rather than documented. At
    /// [`SECTION_TRACK_SAMPLES`] segments the residual falls as the square of
    /// the count — under a metre, comfortably inside the ring offset that is
    /// already accepted — and it costs 32 projections per track per frame.
    ///
    /// The **rubber band is deliberately still a straight segment.** It is a
    /// preview of a gesture in progress and its endpoints are pixels on purpose
    /// (see `Gui::section_rubber_band`), so it tracks the finger exactly even on
    /// a frame where a wheel-zoom moved the map. The line only becomes a claim
    /// about ground when it is committed, and that is the one this curves.
    ///
    /// # Reading `self.panes` from inside the pane loop is safe *here*
    ///
    /// The loop has `mem::take`n the pane being drawn, so its slot reads as a
    /// default map pane — the module's standing hazard. It costs nothing here
    /// because only [`PaneState::cross_section`] is read and the taken pane is a
    /// map pane by construction: this runs inside `Map::show`, which only the map
    /// arm reaches. A section pane can never be the one held out.
    fn draw_section_tracks(
        &mut self,
        ui: &egui::Ui,
        projector: &walkers::Projector,
        pane_idx: usize,
        pane_rect: egui::Rect,
    ) {
        let painter = ui.painter();
        let project =
            |p: crate::pane::GeoPoint| projector.project(walkers::lat_lon(p.lat, p.lon)).to_pos2();

        #[cfg(test)]
        let mut painted: Vec<(usize, usize, egui::Pos2, egui::Pos2)> = Vec::new();

        // Committed sections first, so a band being dragged over one is on top.
        for (idx, other) in self.panes().iter().enumerate() {
            let Some(section) = other.cross_section() else {
                continue;
            };
            if section.source_pane != Some(pane_idx) {
                continue;
            }
            let Some(committed) = section.line else {
                continue;
            };
            // An endpoint drag in flight replaces this track with its live
            // preview — geographic, like the committed line, so a mid-drag
            // zoom moves both together. The pane's stored line is untouched
            // until the drop, which is what keeps the cut off this path.
            //
            // On the release frame neither exists yet: the drag was taken
            // and committed into `pending_section_edit`, and the applier
            // that writes it to the pane runs after this loop (`Gui::ui`).
            // Painting the pending line bridges that frame — without it the
            // release frame painted the stale pre-drag `committed`, a
            // visible pop-back before the applier's write landed (the M8
            // first-run finding; the gap predates the Synthesis rebuild).
            let editing = self
                .section_edit_drag
                .filter(|d| d.map_pane == pane_idx && d.section_pane == idx);
            let dropped = self
                .pending_section_edit
                .filter(|&(pane, _)| pane == idx)
                .map(|(_, line)| line);
            let line = editing
                .map(|d| d.preview())
                .or(dropped)
                .unwrap_or(committed);
            let track = great_circle_track(line, project);
            paint_section_track(painter, &track, pane_rect);
            #[cfg(test)]
            if let (Some(&a), Some(&b)) = (track.first(), track.last()) {
                painted.push((pane_idx, idx, a, b));
            }
            // The ends are handles now, and drawn like it: a cap that looks
            // identical to every other map decoration is an affordance nobody
            // finds.
            paint_section_handles(painter, &track, pane_rect, editing.map(|d| d.grab));
        }

        if let Some((from, to)) = self.section_rubber_band(pane_idx) {
            paint_section_track(painter, &[from, to], pane_rect);
        }

        #[cfg(test)]
        self.last_section_tracks.extend(painted);
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
        let Some(pos) = ctx.input(|i| {
            if i.pointer.primary_pressed() {
                i.pointer.interact_pos()
            } else {
                None
            }
        }) else {
            return;
        };
        // A fresh press starts a fresh record: whether *this* press is the
        // one that activates its pane, and whether it landed with a popup
        // open, are what the fade trigger asks later, when the press has
        // become a click (`ui_fade.rs` — the first click on an inactive pane
        // only activates, and a click that dismissed a popover dismissed).
        // The popup state must be read *now*: egui closes the popup on this
        // very click, so by the confirm frame the evidence is gone.
        self.press_switched_pane = false;
        self.press_popup_open = egui::Popup::is_any_open(ctx);
        // Don't switch panes when the click lands on a floating dialog or popup.
        if ctx
            .layer_id_at(pos)
            .is_some_and(|l| l.order > egui::Order::Background)
        {
            return;
        }
        let pane_count = self.visible_pane_count();
        if pane_count <= 1 {
            return;
        }
        for idx in 0..pane_count {
            let rect = self.pane_layout.pane_rect(idx, panel_rect);
            if rect.contains(pos) && idx != self.active_pane {
                self.active_pane = idx;
                self.press_switched_pane = true;
                // A pane switch ends a touch reveal: the revealed row
                // belonged to the pane the user has just left for
                // another.
                self.pill_revealed = None;
                break;
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
#[allow(clippy::too_many_arguments)]
fn render_volume_pane(
    ui: &mut egui::Ui,
    pane_rect: egui::Rect,
    pane_idx: usize,
    pane: &mut crate::pane::PaneState,
    painter: Option<&dyn crate::volume_view::VolumePainter>,
    current_stamp: Option<crate::ui::CurrentVolumeStamp>,
    chrome: Option<f32>,
    actions: &mut Vec<GuiAction>,
    alpha_curves: &mut crate::volume_alpha::AlphaCurves,
    iso_thresholds: &crate::volume_iso::IsoThresholds,
    #[cfg(test)] alpha_buttons: &mut Vec<(usize, egui::Rect)>,
) -> Option<String> {
    let outcome = volume_pane_outcome(
        ui,
        pane_rect,
        pane_idx,
        pane,
        painter,
        current_stamp,
        actions,
        alpha_curves,
        iso_thresholds,
    );
    if let Some(why) = outcome.empty.as_deref() {
        paint_pane_empty_state(ui, pane_rect, why);
    }
    // The Volume Alpha editor, after the pane's own painting so its button and
    // window sit over the picture. It needs the target the arm just resolved —
    // the palette it shows is the grid's own, looked up by that target — and
    // the product, which the curves are keyed by. `chrome` is the frame's
    // fade: the button is floating chrome and hides with the rest (§1.8).
    volume_alpha_editor::editor_ui(
        ui,
        pane_rect,
        pane_idx,
        pane,
        painter,
        outcome.target.as_ref(),
        chrome,
        alpha_curves,
        #[cfg(test)]
        alpha_buttons,
    );
    outcome.empty
}

/// What the 3D arm resolved for one pane on one frame: the empty-state reason
/// if there was one, and the target it aimed at if it got far enough to name
/// one. The target is what the Volume Alpha editor looks the palette up by —
/// re-deriving it there would be a second copy of the stamp-and-region logic
/// that could drift from this one.
struct VolumeOutcome {
    empty: Option<String>,
    target: Option<crate::pane::VolumeTarget>,
}

impl VolumeOutcome {
    fn empty_state(why: String) -> Self {
        Self {
            empty: Some(why),
            target: None,
        }
    }
}

/// The 3D arm's decision, with the painting left to its caller so that every
/// path out of it is a `return` of a reason rather than a `return` plus a call
/// somebody can forget to make.
#[allow(clippy::too_many_arguments)]
fn volume_pane_outcome(
    ui: &mut egui::Ui,
    pane_rect: egui::Rect,
    pane_idx: usize,
    pane: &mut crate::pane::PaneState,
    painter: Option<&dyn crate::volume_view::VolumePainter>,
    current_stamp: Option<crate::ui::CurrentVolumeStamp>,
    actions: &mut Vec<GuiAction>,
    alpha_curves: &crate::volume_alpha::AlphaCurves,
    iso_thresholds: &crate::volume_iso::IsoThresholds,
) -> VolumeOutcome {
    use crate::pane::{OrbitDelta, VolumeStamp, VolumeTarget};
    use crate::volume_view::{VolumeFrameState, VolumePaint};

    // The camera and the box as they stand *before* this frame's gesture, which
    // is what the pan has to be scaled against: the world distance a screen point
    // spans depends on where the eye is, and folding the drag in first would
    // measure it against a camera the user has not seen yet.
    //
    // Answered rather than unwrapped for the reason the `volume_mut` below gives.
    let Some((camera_before, box_size_km, view_mode)) = pane
        .volume()
        .map(|v| (v.camera, v.box_size_km(), v.view_mode))
    else {
        return VolumeOutcome::empty_state(VOLUME_EMPTY_STATE.to_owned());
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

    // The pane-rect gate alone stopped being enough at the full-bleed flip:
    // pane rects now run under the floating chrome (timeline, status bar,
    // layers panel), so "the pointer is over this pane" no longer implies
    // "the pointer is over the *map*". The topmost-layer check is the same
    // rule `filter_dialog_blocked` applies to clicks — a position covered by
    // any layer above `Background` belongs to that layer, and a wheel there
    // must work the chrome, not fly the camera under it. `hovered()` already
    // answers through egui's layer-aware hit test, so the check's own ground
    // is the `dragged()` arm: a drag keeps this response resolving after the
    // pointer has wandered over the chrome, and without the layer check a
    // wheel spun there mid-orbit would still zoom the box.
    let pointer_on_map_layer = ui.ctx().pointer_latest_pos().is_some_and(|pos| {
        !ui.ctx()
            .layer_id_at(pos)
            .is_some_and(|l| l.order > egui::Order::Background)
    });
    if (response.hovered() || response.dragged()) && pointer_on_map_layer {
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
    // The published current-volume stamp, **not** `scan_info`. `scan_info`
    // names whatever the plan view is drawing and freezes for a whole volume;
    // the stamp is the newest data time of the site's merged volume and
    // advances on every sealed sweep — it is what makes the 3D pane rebuild in
    // step with the map beside it, and what a build is deduplicated by.
    //
    // The site comes from `pane.site` rather than from `scan_info.site`,
    // because a pane that has switched site should ask for the new site's
    // volume, not go on naming the old one until a plan view catches up.
    let stamp = current_stamp.map(|stamp| {
        (
            VolumeStamp {
                site: site_code.clone(),
                collected: stamp.newest,
            },
            stamp.base_started,
        )
    });

    // Unreachable from the kind branch, which only enters here for a `Volume`
    // pane, and answered rather than unwrapped: this function takes a whole
    // `PaneState` and is the sort of thing a future caller invokes from
    // somewhere else.
    let Some(volume) = pane.volume_mut() else {
        return VolumeOutcome::empty_state(VOLUME_EMPTY_STATE.to_owned());
    };
    volume.camera.nudge(delta);
    let camera = volume.camera;
    let region = volume.region;
    let floor = !volume.hide_floor;
    let already_rendered = volume.rendered_for.clone();

    // Everything below is a reason there is no picture, in the order the user
    // can act on them.
    let Some(painter) = painter else {
        return VolumeOutcome::empty_state(VOLUME_EMPTY_STATE.to_owned());
    };
    let Some((volume_stamp, base_started)) = stamp else {
        // No volume at all yet — the cold-start window between choosing a site
        // and its first data landing. The download is already in flight (a
        // site switch fires the archive fetch immediately), so this is the one
        // state where waiting is the truth.
        return VolumeOutcome::empty_state(format!(
            "Downloading the first {site_code} volume...\n\nThe 3D view builds the moment it \
             lands, then updates tilt by tilt as new sweeps arrive.",
        ));
    };
    // `volume_slot`, not `samplable`: the derived products (SRV, NROT, KDP)
    // render through the worker-side derivation layer, so only the products
    // with no per-tilt field at all are refused here — and the message says
    // which kind of field the pane needs.
    if rustdar_radar::derive::volume_slot(product).is_none() {
        return VolumeOutcome::empty_state(format!(
            "{} has no vertical structure to render in 3D - pick a moment the radar measures \
             or derives tilt by tilt",
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

    let empty = match painter.paint(&VolumeFrameState {
        pane_idx,
        target: target.clone(),
        camera,
        size_px,
        floor,
        // The user's Volume Alpha curve for this product, or `None` for an
        // untouched editor — which the painter is obliged to render
        // bit-exactly through the palette's own alpha.
        alpha: alpha_curves.get(product),
        view_mode,
        iso_threshold: iso_thresholds.get(product),
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
                &volume_caption(&site_code, collected, base_started, region, camera),
            );
            None
        }
        VolumePaint::Empty(why) => Some(why),
    };
    VolumeOutcome {
        empty,
        target: Some(target),
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
pub(crate) fn render_volume_controls(
    ui: &mut egui::Ui,
    pane: &mut crate::pane::PaneState,
    iso_thresholds: &mut crate::volume_iso::IsoThresholds,
    alpha_curves: &crate::volume_alpha::AlphaCurves,
) {
    let product = pane.selected_product;
    let Some(volume) = pane.volume_mut() else {
        return;
    };
    ui.add_space(6.0);
    ui.separator();
    ui.label(VOLUME_SIDEBAR_HEADER);

    // Header-then-indent, like the loop transport and the section block: the
    // 3D knobs are one more block of the one panel, not a panel of their own.
    // The slider sits behind a "Vertical:" label the way "Lookback:" and
    // "Speed:" do, rather than carrying its own trailing text into a column
    // too narrow for both.
    ui.indent("volume_controls", |ui| {
        let mut exaggeration = volume.camera.vertical_exaggeration();
        ui.horizontal(|ui| {
            ui.label("Vertical:");
            let response = ui.add(
                egui::Slider::new(
                    &mut exaggeration,
                    crate::pane::MIN_VERTICAL_EXAGGERATION..=crate::pane::MAX_VERTICAL_EXAGGERATION,
                )
                .suffix("\u{d7}")
                .fixed_decimals(1),
            );
            if response.changed() {
                // Through the setter, which is the only writer and the only
                // place the clamp and the non-finite refusal live. Writing the
                // field would work here and would be a second copy of both.
                volume.camera.set_vertical_exaggeration(exaggeration);
            }
            response.on_hover_text(
                "Stretches the box vertically so storm structure is legible. Heights the pane \
                 reports stay in real kft MSL at every setting.",
            );
        });

        // The view mode: GR2Analyst's own pair, as a two-way radio. The mode
        // is per pane (a posture of this picture, persisted with the pane);
        // the threshold is per product (a judgement about a moment's scale,
        // shared by every pane showing it — the alpha curves' arrangement).
        ui.horizontal(|ui| {
            ui.label("Mode:");
            ui.radio_value(
                &mut volume.view_mode,
                crate::pane::VolumeViewMode::LitVolume,
                "Lit volume",
            )
            .on_hover_text("The translucent accumulation: cloud shaped by the product's transparency profile and your Volume Alpha curve.");
            ui.radio_value(
                &mut volume.view_mode,
                crate::pane::VolumeViewMode::Isosurface,
                "Isosurface",
            )
            .on_hover_text("One opaque, lit surface at the threshold below - the shell of everything at or beyond it.");
        });
        if volume.view_mode == crate::pane::VolumeViewMode::Isosurface {
            let (prefix, suffix) = crate::volume_iso::slider_labels(product);
            let mut threshold = iso_thresholds.get(product);
            ui.horizontal(|ui| {
                ui.label(format!("{prefix}:"));
                let response = ui.add(
                    egui::Slider::new(&mut threshold, crate::volume_iso::slider_range(product))
                        .suffix(suffix)
                        .fixed_decimals(if *crate::volume_iso::slider_range(product).end() <= 4.0 {
                            2
                        } else {
                            0
                        }),
                );
                if response.changed() {
                    iso_thresholds.set(product, threshold);
                }
                response.on_hover_text(format!(
                    "Where {}'s surface sits. Per product - every 3D pane showing this \
                     product shares it.",
                    product.name(),
                ));
            });
            // The honest word about the other control: the surface reads the
            // data, so a drawn curve changes nothing in this mode.
            if alpha_curves.is_edited(product) {
                ui.label(
                    egui::RichText::new(
                        "The isosurface reads the data itself; your Volume Alpha curve \
                         applies to the lit volume only.",
                    )
                    .small()
                    .weak(),
                );
            }
        }

        // Positive in the UI, inverted in storage — see `VolumePane::hide_floor`
        // for why the stored form is the negation.
        let mut show_floor = !volume.hide_floor;
        if ui
            .checkbox(&mut show_floor, "Map floor")
            .on_hover_text(
                "Draws the ground under the volume: the basemap, SPC outlooks, the base \
                 reflectivity as the 2D map shows it, the range ring, mesoscale discussion, \
                 warning and watch polygons, and city labels, registered to the box. \
                 Warnings and discussions refresh on the floor as they issue and expire. \
                 Always the full composition - the map panes' layer toggles do not apply to \
                 it, the same way they do not apply to this pane's volume.",
            )
            .changed()
        {
            volume.hide_floor = !show_floor;
        }

        if ui
            .button("Reset view")
            .on_hover_text("Back to the default angle, zoom, centre and region.")
            .clicked()
        {
            reset_volume_view(volume);
        }
    });
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
    // `view_mode` stays, deliberately: the reset is for a pane that is *lost*
    // — angle, zoom, centre, region — and the view mode is not a way to be
    // lost, it is a choice of picture. A reset that also flipped an
    // isosurface pane back to the lit volume would un-choose something the
    // user chose on purpose.
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
/// The same applies to the volume time, which for a merged volume is **two**
/// truthful claims that must not be fused. "Newest data" is when the radar
/// last looked anywhere in the volume — it advances on every sealed sweep, and
/// stating it alone would let the whole volume borrow that freshness. "Base
/// volume" is the complete volume the un-refreshed tilts still come from. The
/// two lines together let a reader see the span of what is on screen; while a
/// site's first volume is still filling there is no base at all, and the
/// caption says that instead — the earlier wording ("archived volume", plus a
/// warning naming the app's other volume) described the archive-only design
/// this superseded, in which built and current genuinely differed.
///
/// # Why the resolution is here rather than inferred
///
/// The grid has a fixed cell count, so a tighter region buys detail instead of
/// saving memory. That is the main reason to pick a region at all, and it is
/// invisible unless it is written down: 1.80 km per cell at the whole-scan
/// default box against 0.16 at a 20 km one is the difference between a smear
/// and a storm.
///
/// A pure function of five values so that what the pane claims can be tested
/// without a GPU, a projector or a frame.
fn volume_caption(
    site: &str,
    newest: chrono::NaiveDateTime,
    base_started: Option<chrono::NaiveDateTime>,
    region: Option<crate::pane::VolumeRegion>,
    camera: crate::pane::OrbitCamera,
) -> Vec<String> {
    let mut lines = vec![format!(
        "{site} volume - newest data {}Z",
        newest.format("%H:%M")
    )];

    match base_started {
        Some(base) => lines.push(format!("base volume {}Z", base.format("%H:%M"))),
        // No complete volume yet: the ladder is only what the current flight
        // has sealed, and the picture must not read as a full atmosphere.
        None => lines.push("no complete volume yet - showing the tilts flown so far".to_owned()),
    }

    let base = rustdar_radar::voxel::DEFAULT_BASE_KM_MSL * KFT_PER_KM;
    let top = rustdar_radar::voxel::DEFAULT_TOP_KM_MSL * KFT_PER_KM;
    lines.push(format!(
        "{base:.0}-{top:.0} kft MSL - vertical exaggeration {:.1}×",
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
        Some(km) => lines.push(format!("{:.0} km box - {km:.2} km/cell", 2.0 * half_width)),
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
    // Below the pane's pill row, not at the very corner — the row is an
    // egui layer over the pane and would cover the caption (see
    // `ui_pills::PILL_ROW_CLEARANCE`).
    let origin = pane_rect.left_top() + egui::vec2(CAPTION_MARGIN, crate::ui::PILL_ROW_CLEARANCE);
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

/// The colour a section's ground track and its end caps are drawn in.
///
/// Warm, and nothing else on the map is: every overlay in the registry is a
/// hazard colour or a muted grey, and the radar image underneath spans the whole
/// spectrum. A track has to stay findable over a 70 dBZ core.
const SECTION_TRACK_COLOR: egui::Color32 = egui::Color32::from_rgb(255, 214, 10);

/// What the armed cross-section draw's hint chip says.
pub(crate) const SECTION_ARM_HINT: &str = "Drag A-B to draw cross-section";

/// What the armed region drag's hint chip says: the gesture, then the box
/// sizes the resampler will actually honour — computed from the same
/// constants `VolumeRegion::new` clamps by and `box_size_km` falls back to,
/// so the chip cannot state sizes the drag will not deliver.
pub(crate) fn region_arm_hint() -> String {
    format!(
        "Drag to pick 3D region - box {:.0}-{:.0} km (default {:.0} km)",
        2.0 * rustdar_radar::voxel::MIN_HALF_WIDTH_KM,
        2.0 * rustdar_radar::voxel::MAX_HALF_WIDTH_KM,
        2.0 * crate::pane::DEFAULT_HALF_WIDTH_KM,
    )
}

/// Padding between the hint chip's text and its dashed border, each axis.
const ARMED_HINT_PADDING: egui::Vec2 = egui::vec2(12.0, 8.0);

/// Dash and gap of the chip's border, points.
const ARMED_HINT_DASH: f32 = 6.0;
const ARMED_HINT_GAP: f32 = 4.0;

/// Paint the armed-tool hint chip: a centred, non-interactive dashed-border
/// chip naming the drag the armed mode is waiting for.
///
/// Painter only — no widget: the chip explains a gesture, and a widget here
/// would both consume one of the pane's auto-ids and sit in the very spot
/// the drag it describes starts in. The colours are the armed modes' own —
/// the region drag's box yellow or [`SECTION_TRACK_COLOR`] — over the same
/// translucent black the section track's halo uses, so the chip reads over
/// any radar core without adding a colour the map does not already have.
///
/// On its own sub-layer (the pending-render notice's arrangement), clipped
/// to the pane, so a pane's later drawing cannot cover it and it cannot leak
/// into the pane next door.
fn paint_armed_hint_chip(
    ctx: &egui::Context,
    pane_idx: usize,
    pane_rect: egui::Rect,
    text: &str,
    color: egui::Color32,
) {
    let layer = egui::LayerId::new(
        egui::Order::Background,
        egui::Id::new(("armed_hint_chip", pane_idx)),
    );
    let painter = ctx.layer_painter(layer).with_clip_rect(pane_rect);
    let galley = painter.layout_no_wrap(text.to_owned(), egui::FontId::proportional(13.0), color);
    let rect =
        egui::Rect::from_center_size(pane_rect.center(), galley.size() + 2.0 * ARMED_HINT_PADDING);
    // The section-track halo's translucent black, as a fill.
    painter.rect_filled(
        rect,
        4.0,
        egui::Color32::from_rgba_unmultiplied(0, 0, 0, 140),
    );
    let stroke = egui::Stroke::new(1.0, color);
    for (a, b) in [
        (rect.left_top(), rect.right_top()),
        (rect.right_top(), rect.right_bottom()),
        (rect.right_bottom(), rect.left_bottom()),
        (rect.left_bottom(), rect.left_top()),
    ] {
        painter.extend(egui::Shape::dashed_line(
            &[a, b],
            stroke,
            ARMED_HINT_DASH,
            ARMED_HINT_GAP,
        ));
    }
    painter.galley(
        rect.min + ARMED_HINT_PADDING,
        galley,
        egui::Color32::PLACEHOLDER,
    );
}

/// Segments a committed ground track is drawn with.
///
/// The chord error of a subdivided great circle falls as the square of the
/// count, so 32 turns the 894 m peak measured in `draw_section_tracks` into
/// under a metre — an order of magnitude inside the 258 m range-ring offset that
/// module already accepts and documents.
const SECTION_TRACK_SAMPLES: usize = 32;

/// The screen polyline of the great circle a section is cut along.
///
/// Split out from the painting for the same reason `tilt_curves` is: the
/// geometry is the part that can be wrong, and a wrongness that only shows up as
/// "the line looked slightly off" is one nothing can fail on.
fn great_circle_track(
    line: crate::pane::SectionLine,
    project: impl Fn(crate::pane::GeoPoint) -> egui::Pos2,
) -> Vec<egui::Pos2> {
    let a = (line.a().lat, line.a().lon);
    let b = (line.b().lat, line.b().lon);
    (0..=SECTION_TRACK_SAMPLES)
        .map(|i| {
            let t = i as f64 / SECTION_TRACK_SAMPLES as f64;
            let (lat, lon) = rustdar_radar::beam::great_circle_point(a, b, t);
            project(crate::pane::GeoPoint { lat, lon })
        })
        .collect()
}

/// Paint one section ground track: a polyline with a cap at each end.
///
/// Clipped to the pane rather than to the whole panel, so a track belonging to a
/// map in one pane cannot be drawn across the pane beside it — the projector is
/// per-pane and happily projects to coordinates outside its own map.
///
/// The end caps are what make the track readable as a *section* rather than as
/// one more line on a busy map: they mark which end is the left-hand column of
/// the picture, which is otherwise unguessable.
fn paint_section_track(painter: &egui::Painter, points: &[egui::Pos2], pane_rect: egui::Rect) {
    let (Some(&from), Some(&to)) = (points.first(), points.last()) else {
        return;
    };
    let painter = painter.with_clip_rect(pane_rect);
    // A dark halo under the line, so it reads over both a light basemap and a
    // dark radar core without the line itself having to be thick.
    painter.add(egui::Shape::line(
        points.to_vec(),
        egui::Stroke::new(4.0, egui::Color32::from_rgba_unmultiplied(0, 0, 0, 140)),
    ));
    painter.add(egui::Shape::line(
        points.to_vec(),
        egui::Stroke::new(2.0, SECTION_TRACK_COLOR),
    ));
    for (pos, label) in [(from, "A"), (to, "B")] {
        painter.circle_filled(pos, 4.0, SECTION_TRACK_COLOR);
        painter.circle_stroke(pos, 4.0, egui::Stroke::new(1.0, egui::Color32::BLACK));
        painter.text(
            pos + egui::vec2(0.0, -12.0),
            egui::Align2::CENTER_CENTER,
            label,
            egui::FontId::proportional(11.0),
            SECTION_TRACK_COLOR,
        );
    }
}

/// Paint the two grab handles over a track's end caps.
///
/// A ring around the cap rather than a bigger cap: the ring reads as "this is
/// a control" the way a plain dot never does, and it leaves the A/B labels and
/// the cap [`paint_section_track`] drew exactly where they were. `active` is
/// the grabbed end of an in-flight drag, drawn heavier so the user can see
/// which end they are holding through a busy storm.
///
/// The ring's radius is **visual**, deliberately smaller than
/// [`ENDPOINT_GRAB_RADIUS_PT`](crate::ui_section_edit::ENDPOINT_GRAB_RADIUS_PT):
/// the hit target forgives half a finger of aim, and drawing the forgiveness
/// would bury the map under two thumbprint-sized discs.
fn paint_section_handles(
    painter: &egui::Painter,
    points: &[egui::Pos2],
    pane_rect: egui::Rect,
    active: Option<crate::ui_section_edit::SectionGrab>,
) {
    use crate::ui_section_edit::SectionGrab;
    let (Some(&a), Some(&b)) = (points.first(), points.last()) else {
        return;
    };
    let painter = painter.with_clip_rect(pane_rect);
    for (pos, grab) in [(a, SectionGrab::A), (b, SectionGrab::B)] {
        let grabbed = active == Some(grab);
        let ring = if grabbed { 9.0 } else { 7.0 };
        // The same halo-under-bright trick every line on this map uses, so the
        // ring survives both a light basemap and a 70 dBZ core.
        painter.circle_stroke(
            pos,
            ring,
            egui::Stroke::new(3.5, egui::Color32::from_rgba_unmultiplied(0, 0, 0, 140)),
        );
        painter.circle_stroke(
            pos,
            ring,
            egui::Stroke::new(if grabbed { 2.5 } else { 1.5 }, SECTION_TRACK_COLOR),
        );
    }
}

/// Draw a border around a pane rect, highlighted when active. Returns the
/// painted stroke's bounds, for the M8 containment pin.
///
/// `StrokeKind::Inside`, deliberately: the pane rects tile the map content
/// rect edge to edge since the full-bleed flip, so an outside stroke lay
/// entirely in the neighbouring pane or beyond the content rect — clipped
/// away on every outer edge, overpainted by later panes on the inner ones,
/// which left the active highlight visible only where an adjacent pane's gap
/// happened to show it (the first-run finding: the top-left pane showed no
/// border at all). Inside the rect, every pane shows all four edges at every
/// grid position, painted after the pane's own content so nothing covers it.
fn draw_pane_border(ui: &mut egui::Ui, pane_rect: egui::Rect, is_active: bool) -> egui::Rect {
    let border_color = if is_active {
        egui::Color32::from_rgb(60, 140, 255)
    } else {
        egui::Color32::from_rgba_unmultiplied(128, 128, 128, 100)
    };
    let stroke_width = if is_active { 2.0 } else { 1.0 };
    let kind = egui::StrokeKind::Inside;
    ui.painter().rect_stroke(
        pane_rect,
        0.0,
        egui::Stroke::new(stroke_width, border_color),
        kind,
    );
    // The painted bounds follow from the kind the stroke was really drawn
    // with, so the probe cannot claim containment the paint call breaks.
    match kind {
        egui::StrokeKind::Inside => pane_rect,
        egui::StrokeKind::Middle => pane_rect.expand(stroke_width / 2.0),
        egui::StrokeKind::Outside => pane_rect.expand(stroke_width),
    }
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

#[path = "ui_map/tests.rs"]
#[cfg(test)]
mod tests;

#[path = "ui_map/volume_arm_tests.rs"]
#[cfg(test)]
mod volume_arm_tests;
