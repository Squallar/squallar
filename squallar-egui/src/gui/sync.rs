//! The [`Gui`]'s pane-link fan-outs: the time/loop target sets, layer and
//! viewport propagation, and the sync section's outcome.
//!
//! **Every predicate here is scoped to a group.** The three per-pane link
//! booleans say which dimensions a pane opts into; [`PaneState::group`] says
//! whom it opts in *with*. A pane in no group syncs with nobody however its
//! flags read, and two panes in different groups never reach each other —
//! which is the whole difference between this module and the one that had
//! only the flags.
use super::*;
use crate::pane::GroupId;

impl Gui {
    /// Whether panes `a` and `b` are in the same link group.
    ///
    /// One pane always answers **true** about itself, group or no group. The
    /// app-side dedup filters ask this of their own pane's queued work, and a
    /// pane that could not match itself would stop deduplicating its own
    /// renders the moment it left every group.
    ///
    /// An index no pane occupies also answers true, matching the `is_none_or`
    /// reading every per-pane predicate here has always used: a target that
    /// does not exist is not a target this function is entitled to exclude.
    pub fn panes_share_group(&self, a: PaneId, b: PaneId) -> bool {
        if a == b {
            return true;
        }
        match (self.panes.get(a), self.panes.get(b)) {
            (Some(x), Some(y)) => x.in_group_with(y),
            _ => true,
        }
    }

    /// Whether a layer-wide change on `src` may reach `other`: both panes
    /// layer-linked, and in the same group.
    pub fn panes_layer_linked(&self, src: PaneId, other: PaneId) -> bool {
        self.pane_layer_linked(src)
            && self.pane_layer_linked(other)
            && self.panes_share_group(src, other)
    }

    /// [`Self::panes_layer_linked`] for shared time.
    pub fn panes_time_linked(&self, src: PaneId, other: PaneId) -> bool {
        self.pane_time_linked(src)
            && self.pane_time_linked(other)
            && self.panes_share_group(src, other)
    }

    /// Every group that has a visible pane in it, in letter order.
    pub(crate) fn groups_in_use(&self) -> Vec<GroupId> {
        let mut seen: Vec<GroupId> = (0..self.visible_pane_count())
            .filter_map(|idx| self.panes.get(idx).and_then(|pane| pane.group))
            .collect();
        seen.sort_unstable();
        seen.dedup();
        seen
    }

    /// The pane indices shared time fans out over: the active pane, plus —
    /// with more than one pane — every visible pane in the active pane's
    /// group whose [`PaneState::time_link`] is still on. The retired
    /// `sync_layers` global no longer gates this: the per-pane link is the
    /// whole model, and a migrated old config with the global off arrives
    /// with every pane's link seeded off (see `load_ui_config`), which is
    /// the same fan-out it had.
    pub(super) fn time_sync_targets(&self) -> Vec<usize> {
        if self.pane_layout.pane_count > 1 {
            (0..self.pane_layout.pane_count)
                .filter(|&idx| {
                    idx == self.active_pane
                        || (self.panes.get(idx).is_none_or(|pane| pane.time_link)
                            && self.panes_share_group(self.active_pane, idx))
                })
                .collect()
        } else {
            vec![self.active_pane]
        }
    }

    /// **The panes a time-shaped change made *by pane `src`* reaches** — the
    /// visible time-linked panes when `src` is itself linked, or `src` alone
    /// when it is not.
    ///
    /// [`Self::time_sync_targets`] answers the same question about the
    /// *active* pane and is what the frame's own fan-outs use. This one takes
    /// its source as an argument, because an archive volume lands frames
    /// after the navigation that asked for it and the pane that asked is not
    /// necessarily the one active by then. It is the time-side twin of
    /// [`Gui::layer_sync_targets`], and deliberately spelled the same way.
    pub(crate) fn time_sync_targets_for(&self, src: usize) -> Vec<usize> {
        let count = self.visible_pane_count();
        if count > 1 && self.pane_time_linked(src) {
            (0..count)
                .filter(|&idx| idx == src || self.pane_time_linked(idx))
                .collect()
        } else {
            vec![src]
        }
    }

    /// [`Self::time_sync_targets`] narrowed to the panes a loop can feed —
    /// the fan-out for every loop action.
    ///
    /// **Narrowed to the source pane's transport layer** (WI-12): the frame
    /// indices these actions carry are indices into the *active* pane's
    /// transport timeline, so a linked pane whose transport addresses a
    /// different layer is not a target — seeking a radar pane to a forecast
    /// index parks it on whichever scan happens to sit at that offset.
    pub(super) fn loop_sync_targets(&self) -> Vec<usize> {
        let src_transport = self
            .panes
            .get(self.active_pane)
            .map(PaneState::transport_layer);
        self.time_sync_targets()
            .into_iter()
            .filter(|&idx| {
                idx == self.active_pane
                    || self.panes.get(idx).is_none_or(|pane| {
                        pane.can_loop() && Some(pane.transport_layer()) == src_transport
                    })
            })
            .collect()
    }

    /// **Set the timeline window, everywhere it is read.** The setting is
    /// persisted once, at the root of the config file, and every pane's
    /// posture carries a copy — so it is written to every pane, not to the
    /// sync group: the one number has always applied to the whole window,
    /// linked or not.
    ///
    /// The slider says so, in `ui_timeline.rs`'s `TUNING_SCOPE_CAPTION` —
    /// narrow this to the group and that caption becomes a lie.
    ///
    /// **This is the setting, not the window a loop is listed over.** That is
    /// [`Self::loop_span_secs_for`], which raises this to the addressed
    /// layer's own floor. The two are the same number for radar and for every
    /// layer that declares no minimum.
    pub(crate) fn set_loop_span_secs(&mut self, secs: u64) {
        self.loop_lookback_secs = secs;
        for pane in &mut self.panes {
            pane.time.span_secs = secs;
        }
    }

    /// **The window `pane_idx`'s loop is actually listed over**: the setting,
    /// raised to the floor the pane's *transport* layer declares.
    ///
    /// This is [`crate::pane::PaneState::loop_span_secs`] resolved by index,
    /// and the `max` itself lives there — the app layer arms loops from
    /// outside this crate and would otherwise need a second spelling of it,
    /// which is the two-authorities-on-one-window defect the time contract
    /// exists to remove (WO-T3.9). Only the out-of-range fallback is this
    /// function's own.
    ///
    /// Derived on every read rather than stored: nothing new persists, and the
    /// setting the file holds is still the one the slider shows.
    pub(crate) fn loop_span_secs_for(&self, pane_idx: usize) -> u64 {
        let Some(pane) = self.panes.get(pane_idx) else {
            return self.loop_lookback_secs;
        };
        pane.loop_span_secs(&self.overlays)
    }

    /// [`Self::set_loop_span_secs`] for the playback rate, and for the same
    /// reason.
    pub(crate) fn set_loop_speed_fps(&mut self, fps: f32) {
        self.loop_speed_fps = fps;
        for pane in &mut self.panes {
            pane.time.speed_fps = fps;
        }
    }

    /// **Propagate the active pane's posture to its group** — the layer half
    /// and the time half, each under its own predicate.
    ///
    /// The two used to be one: `viewing_live` and `time.step` were written
    /// inside the layer guard, so turning "Sync layers" off silently took
    /// half of time sync with it while `LAYER_LINK_NOTE` promised layers-off
    /// kept only "this pane's site, product, tilt and layers" its own. They
    /// are separate calls now, and either can run without the other.
    /// `pub(crate)` rather than `pub(super)` so
    /// `ui_config::measure_seed_tests` can ask what a seeded scene really
    /// looks like once the shell has run: the collapse this performs is not
    /// visible to a test that only loads the config, and scene C's three sites
    /// became one here for every row the campaign ever took.
    pub(crate) fn propagate_pane_sync(&mut self) {
        if self.pane_layout.pane_count <= 1 {
            return;
        }
        self.propagate_time_posture();
        self.propagate_layer_state();
    }

    /// The time half: `viewing_live` and the step size, to every pane in the
    /// active pane's group whose time link is on. **Under the time predicate
    /// alone** — the layer link has no say here, and the target set is the
    /// one [`Self::time_sync_targets`] names.
    fn propagate_time_posture(&mut self) {
        let src = &self.panes[self.active_pane];
        if src.group.is_none() {
            return;
        }
        let group = src.group;
        let active_viewing_live = src.viewing_live;
        let active_time_step = src.time.step;
        for (idx, p) in self.panes.iter_mut().enumerate() {
            if idx == self.active_pane || !p.time_link || p.group != group {
                continue;
            }
            p.viewing_live = active_viewing_live;
            p.time.step = active_time_step;
        }
    }

    /// The layer half: layer settings from a layer-linked active pane to the
    /// other layer-linked panes **in its group**. Also converges site and
    /// scan_info so the group displays the same radar site.
    fn propagate_layer_state(&mut self) {
        if !self.panes[self.active_pane].layer_link || self.panes[self.active_pane].group.is_none()
        {
            return;
        }
        let src = &self.panes[self.active_pane];
        let group = src.group;
        let active_site = src.site().to_string();
        let active_scan_info = src.scan_info.clone();
        let active_layers = src.layers.clone();
        let active_selected_product = src.selected_product();
        let active_selected_elevation = src.selected_elevation();

        // Sync per-pane fields including enabled overlays, configs, and radar
        // product/elevation. Not `content`: see the note on this function for
        // why the pane's kind is the one field sync deliberately leaves alone.
        for (idx, p) in self.panes.iter_mut().enumerate() {
            if idx == self.active_pane || !p.layer_link || p.group != group {
                continue;
            }
            p.set_site(active_site.clone());
            p.scan_info = active_scan_info.clone();
            // The copy arrives with configs but no state: a slot's
            // state is derived, never shared between panes.
            p.adopt_layers(&active_layers);
            p.set_selected_product(active_selected_product.clone());
            p.set_selected_elevation(active_selected_elevation);
            // This is the second way a pane's enabled map changes, and it is the
            // one that bypasses `write_pane_overlay` entirely: the map arrives
            // wholesale, with no kind named and no `on` to read. Without this
            // line the release would have a hole exactly the shape of a split —
            // the pane the user clicked lets its textures go and its linked
            // siblings, which just adopted the same off-switch, keep theirs.
            p.release_disabled_overlay_textures();
        }
    }

    /// Propagate the interacted pane's viewport (zoom + position) to the
    /// linked group — **resolved once per group**, not once per frame.
    ///
    /// The source scan used to break on the first moved pane in index order
    /// and then hold everybody from the active pane. With more than one group
    /// that is wrong at both ends: a move in group B would be read as group
    /// A's source, and the active pane's hold would reach panes it does not
    /// share a group with. Each group now finds its own source, and the hold
    /// is offered only to the group the active pane is actually in.
    pub(super) fn sync_viewports(
        &mut self,
        pre_zooms: &[f64],
        pre_positions: &[Option<walkers::Position>],
    ) {
        let pane_count = self.visible_pane_count();
        if pane_count <= 1 {
            return;
        }
        for group in self.groups_in_use() {
            let members: Vec<usize> = (0..pane_count)
                .filter(|&idx| self.panes[idx].group == Some(group))
                .collect();
            if members.len() < 2 {
                continue;
            }
            self.sync_one_group_viewport(&members, pre_zooms, pre_positions);
        }
    }

    /// [`Self::sync_viewports`] for one group's `members`, in index order.
    fn sync_one_group_viewport(
        &mut self,
        members: &[usize],
        pre_zooms: &[f64],
        pre_positions: &[Option<walkers::Position>],
    ) {
        let mut source_idx = None;
        for &idx in members {
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
        // Nothing in this group moved, so the active pane holds the others
        // where they are — but only if the active pane is one of them, and
        // unless it has no map, in which case its `map_memory` is not a
        // viewport anyone is looking at and there is nothing to propagate; or
        // its link is off, in which case its viewport is its own and holding
        // the group to it would be the unlinked pane driving after all.
        // Returning is the whole point: `unwrap_or(self.active_pane)` on its
        // own would make a non-map active pane the source on every frame,
        // which is the same failure as the source scan above with no
        // interaction needed at all.
        let Some(src) = source_idx.or_else(|| {
            if !members.contains(&self.active_pane) {
                return None;
            }
            let active = &self.panes[self.active_pane];
            (active.shares_viewport() && active.viewport_link).then_some(self.active_pane)
        }) else {
            return;
        };
        let zoom = self.panes[src].map_memory.zoom();
        let pos = self.panes[src].map_memory.detached();
        for &idx in members {
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
    /// Of the four rows only **Match all** is group-blind, and deliberately:
    /// it is the one row that promises to touch no link, so scoping it would
    /// leave a user who has split into groups no way to put every map back on
    /// one view. The three that write links are the group's business and stay
    /// inside it.
    pub(super) fn apply_sync_outcome(
        &mut self,
        outcome: &pills::SyncSectionOutcome,
        pane: &mut PaneState,
        idx: PaneId,
    ) {
        if let Some(group) = outcome.move_to_group {
            pane.group = group;
        }
        let count = self.visible_pane_count();
        if outcome.match_all || outcome.relink_all {
            let scoped = outcome.relink_all;
            let zoom = pane.map_memory.zoom();
            let pos = pane.map_memory.detached();
            for target in 0..count {
                if target == idx || !self.panes[target].shares_viewport() {
                    continue;
                }
                if scoped && !self.panes[target].in_group_with(pane) {
                    continue;
                }
                let _ = self.panes[target].map_memory.set_zoom(zoom);
                if let Some(p) = pos {
                    self.panes[target].map_memory.center_at(p);
                }
            }
        }
        // The two bulk rows are one another's inverse, and until now only the
        // first existed: the section could put a whole group back together and
        // offered no way to take it apart in one gesture.
        if outcome.relink_all || outcome.unlink_all {
            let on = outcome.relink_all;
            pane.viewport_link = on;
            pane.layer_link = on;
            pane.time_link = on;
            for target in 0..count {
                if target == idx || !self.panes[target].in_group_with(pane) {
                    continue;
                }
                let target = &mut self.panes[target];
                target.viewport_link = on;
                target.layer_link = on;
                target.time_link = on;
            }
        }
        if outcome.relink_all {
            self.active_pane = idx;
        }
    }
}
