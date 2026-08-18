//! The [`Gui`]'s pane-link fan-outs, split out of `ui.rs` at WO-E1: the
//! time/loop target sets, layer and viewport propagation, and the sync
//! section's outcome. Bodies are verbatim moves.
use super::*;

impl Gui {
    /// The pane indices shared time fans out over: the active pane, plus —
    /// with more than one pane — every visible pane whose
    /// [`PaneState::time_link`] is still on (plan §3.7). The retired
    /// `sync_layers` global no longer gates this: the per-pane link is the
    /// whole model, and a migrated old config with the global off arrives
    /// with every pane's link seeded off (see `load_ui_config`), which is
    /// the same fan-out it had.
    ///
    /// The active pane is a target unconditionally, its own flag unread: it
    /// is the pane whose control was operated, and "the pane I am driving
    /// does not respond" is not a reading of unlink anyone means. Unlink says
    /// *don't drag me along* — the exclusion is from the fan-out, not from
    /// being driven directly.
    pub(super) fn time_sync_targets(&self) -> Vec<usize> {
        if self.pane_layout.pane_count > 1 {
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
    pub(super) fn loop_sync_targets(&self) -> Vec<usize> {
        self.time_sync_targets()
            .into_iter()
            .filter(|&idx| {
                idx == self.active_pane || self.panes.get(idx).is_none_or(PaneState::can_loop)
            })
            .collect()
    }

    /// Propagate layer settings from a layer-linked active pane to the other
    /// layer-linked panes. Also converges site and scan_info so the linked
    /// group displays the same radar site.
    ///
    /// # Per-pane gating (M11)
    ///
    /// [`PaneState::layer_link`] replaces the retired `sync_layers` global on
    /// **both ends**. An unlinked *source* — the active pane with its link
    /// off — propagates nothing: its edits are its own. An unlinked *target*
    /// is never written: the group's edits leave it alone. Every call site
    /// (the shell and sheet panel passes, the menu dispatcher, the pill
    /// popovers, the catalog appliers) inherits the gate from here, which is
    /// what makes it one rule instead of eight.
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
    pub(super) fn propagate_layer_sync(&mut self) {
        if self.pane_layout.pane_count <= 1 || !self.panes[self.active_pane].layer_link {
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
        // Hidden panes past the layout's count still converge when linked —
        // the pre-M11 behaviour, kept so a re-split restores a pane that was
        // moving with the group rather than one parked months in the past.
        for (idx, p) in self.panes.iter_mut().enumerate() {
            if idx == self.active_pane || !p.layer_link {
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
            // This is the second way a pane's enabled map changes, and it is the
            // one that bypasses `write_pane_overlay` entirely: the map arrives
            // wholesale, with no kind named and no `on` to read. Without this
            // line the release would have a hole exactly the shape of a split —
            // the pane the user clicked lets its textures go and its linked
            // siblings, which just adopted the same off-switch, keep theirs.
            // Worse for the hidden ones, which nothing else would ever clear.
            p.release_disabled_overlay_textures();
        }
    }

    /// Propagate the interacted pane's viewport (zoom + position) to the
    /// linked group.
    ///
    /// Bounded by [`Self::visible_pane_count`], not the layout's raw count:
    /// hidden panes are neither read as a sync source nor written to, and a
    /// count that ran ahead of the vector cannot index past its end.
    ///
    /// # The group is per-pane now (M11)
    ///
    /// The linked group is the visible map panes with
    /// [`PaneState::viewport_link`] on — the retired `viewport_sync` global's
    /// successor. Three rules, each pinned:
    ///
    /// * a change on a **linked** pane drives the group — the interacted pane
    ///   must be linked to be the source;
    /// * a change on an **unlinked** pane moves only itself — the scan still
    ///   spots it first, and returning there (rather than falling through to
    ///   the active-pane hold) is what keeps its local move local;
    /// * an unlinked pane is never a **target** — the group's convergence
    ///   writes the linked panes and no one else.
    ///
    /// # Why panes that do not share a viewport are excluded from both ends
    ///
    /// The membership test is [`PaneState::shares_viewport`], which is where
    /// the decision and its reasons are written down — including why a 3D pane
    /// is out of the group even though it has a viewport of its own, and why
    /// `draws_ground` is the wrong question to ask here. What follows is the
    /// older half of it, about panes with no viewport at all.
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
    /// on screen is re-centred and re-zoomed to it. `viewport_link` defaults
    /// **on**, so that is the shipped default behaviour, not an opt-in.
    ///
    /// Excluded as a *target* as well, for a quieter reason: a converted pane's
    /// viewport is what it comes back to when it is converted back to a map, and
    /// it is persisted per pane. Overwriting it would silently move a map the
    /// user is not looking at yet.
    pub(super) fn sync_viewports(
        &mut self,
        pre_zooms: &[f64],
        pre_positions: &[Option<walkers::Position>],
    ) {
        let pane_count = self.visible_pane_count();
        if pane_count <= 1 {
            return;
        }
        let mut source_idx = None;
        for idx in 0..pane_count {
            if !self.panes[idx].shares_viewport() {
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
        // A move on an unlinked pane is the pane's own: neither drive the
        // group from it nor fall through to the active-pane hold, which would
        // spend the frame fighting nobody on the linked panes while the local
        // move stays local anyway — returning says what happened.
        if let Some(idx) = source_idx
            && !self.panes[idx].viewport_link
        {
            return;
        }
        // Nothing moved, so the active pane holds the others where they are —
        // unless it has no map, in which case its `map_memory` is not a viewport
        // anyone is looking at and there is nothing to propagate; or its link
        // is off, in which case its viewport is its own and holding the group
        // to it would be the unlinked pane driving after all. Returning is
        // the whole point: `unwrap_or(self.active_pane)` on its own would make a
        // non-map active pane the source on every frame, which is the same
        // failure as the source scan above with no interaction needed at all.
        let Some(src) = source_idx.or_else(|| {
            let active = &self.panes[self.active_pane];
            (active.shares_viewport() && active.viewport_link).then_some(self.active_pane)
        }) else {
            return;
        };
        let zoom = self.panes[src].map_memory.zoom();
        let pos = self.panes[src].map_memory.detached();
        for idx in 0..pane_count {
            if idx != src && self.panes[idx].shares_viewport() && self.panes[idx].viewport_link {
                let _ = self.panes[idx].map_memory.set_zoom(zoom);
                if let Some(p) = pos {
                    self.panes[idx].map_memory.center_at(p);
                }
            }
        }
    }

    /// Apply a sync section's action rows (`pills::sync_section_ui`), with
    /// `pane` as the section's own pane — held **out of the vector** by both
    /// callers (the pill popover takes it for the section's duration, the
    /// inspector's pass holds it throughout), so `self.panes[idx]` is a
    /// placeholder this function must never read; it skips `idx` everywhere
    /// and writes the source's own fields through `pane`.
    ///
    /// **Match all panes to this view**: copy `pane`'s zoom — and its centre,
    /// when it has panned off its site — to every visible pane that shares a
    /// viewport ([`PaneState::shares_viewport`]), links untouched. The one-shot
    /// alignment: a following source hands out its zoom and leaves each
    /// target's centre alone, exactly as [`Self::sync_viewports`] would, and it
    /// skips the same panes for the same written-down reasons.
    ///
    /// **Re-link all here**: that copy, plus all three links turned on for
    /// `pane` and every visible pane — and this pane made active, so the
    /// standard convergence that follows (`propagate_layer_sync`, the
    /// viewport hold) reads *this* pane as the group's reference. "Here" is a
    /// place: everything comes home to it. `viewport_link` is written on a 3D
    /// pane too, where it is inert until the pane shows the map again: the
    /// field is the pane's stored intent, and leaving it off would mean
    /// "re-link all" quietly excepted a pane that is going to rejoin the group
    /// the moment it is switched back.
    pub(super) fn apply_sync_outcome(
        &mut self,
        outcome: &pills::SyncSectionOutcome,
        pane: &mut PaneState,
        idx: PaneId,
    ) {
        let count = self.visible_pane_count();
        if outcome.match_all || outcome.relink_all {
            let zoom = pane.map_memory.zoom();
            let pos = pane.map_memory.detached();
            for target in 0..count {
                if target == idx || !self.panes[target].shares_viewport() {
                    continue;
                }
                let _ = self.panes[target].map_memory.set_zoom(zoom);
                if let Some(p) = pos {
                    self.panes[target].map_memory.center_at(p);
                }
            }
        }
        if outcome.relink_all {
            pane.viewport_link = true;
            pane.layer_link = true;
            pane.time_link = true;
            for target in 0..count {
                if target == idx {
                    continue;
                }
                let target = &mut self.panes[target];
                target.viewport_link = true;
                target.layer_link = true;
                target.time_link = true;
            }
            self.active_pane = idx;
        }
    }
}
