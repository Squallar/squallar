//! The [`Gui`]'s pane-link fan-outs: the time/loop target sets, layer and
//! viewport propagation, and the sync section's outcome.
use super::*;

impl Gui {
    /// The pane indices shared time fans out over: the active pane, plus —
    /// with more than one pane — every visible pane whose
    /// [`PaneState::time_link`] is still on. The retired
    /// `sync_layers` global no longer gates this: the per-pane link is the
    /// whole model, and a migrated old config with the global off arrives
    /// with every pane's link seeded off (see `load_ui_config`), which is
    /// the same fan-out it had.
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
    pub(super) fn loop_sync_targets(&self) -> Vec<usize> {
        self.time_sync_targets()
            .into_iter()
            .filter(|&idx| {
                idx == self.active_pane || self.panes.get(idx).is_none_or(PaneState::can_loop)
            })
            .collect()
    }

    /// **Set the timeline window, everywhere it is read.** The setting is
    /// persisted once, at the root of the config file, and every pane's
    /// posture carries a copy — so it is written to every pane, not to the
    /// sync group: the one number has always applied to the whole window,
    /// linked or not.
    pub(crate) fn set_loop_span_secs(&mut self, secs: u64) {
        self.loop_lookback_secs = secs;
        for pane in &mut self.panes {
            pane.time.span_secs = secs;
        }
    }

    /// [`Self::set_loop_span_secs`] for the playback rate, and for the same
    /// reason.
    pub(crate) fn set_loop_speed_fps(&mut self, fps: f32) {
        self.loop_speed_fps = fps;
        for pane in &mut self.panes {
            pane.time.speed_fps = fps;
        }
    }

    /// Propagate layer settings from a layer-linked active pane to the other
    /// layer-linked panes. Also converges site and scan_info so the linked
    /// group displays the same radar site.
    pub(super) fn propagate_layer_sync(&mut self) {
        if self.pane_layout.pane_count <= 1 || !self.panes[self.active_pane].layer_link {
            return;
        }
        let src = &self.panes[self.active_pane];
        let active_site = src.site().to_string();
        let active_scan_info = src.scan_info.clone();
        let active_viewing_live = src.viewing_live;
        let active_time_step = src.time.step;
        let active_layers = src.layers.clone();
        let active_selected_product = src.selected_product();
        let active_selected_elevation = src.selected_elevation();

        // Sync per-pane fields including enabled overlays, configs, and radar
        // product/elevation. Not `content`: see the note on this function for
        // why the pane's kind is the one field sync deliberately leaves alone.
        for (idx, p) in self.panes.iter_mut().enumerate() {
            if idx == self.active_pane || !p.layer_link {
                continue;
            }
            p.set_site(active_site.clone());
            p.scan_info = active_scan_info.clone();
            // The one gated pair — see the method note.
            if p.time_link {
                p.viewing_live = active_viewing_live;
                p.time.step = active_time_step;
            }
            // The copy arrives with configs but no state: a slot's
            // state is derived, never shared between panes.
            p.adopt_layers(&active_layers);
            p.set_selected_product(active_selected_product);
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
    /// linked group.
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
