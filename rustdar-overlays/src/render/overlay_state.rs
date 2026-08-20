//! The overlay registry — and the shim layer over the contract it holds.
//!
//! The contract itself moved out at WO-M9: `SourceHandler` (this module's
//! `OverlayHandler`) and every type its methods speak now live in
//! `rustdar_source::handler`, so `rustdar-radar` can implement the same trait
//! for the radar layer without either source crate depending on the other.
//! The re-exports below keep every path this module has published resolving,
//! which is why no consumer had to move with it.
//!
//! What stayed: [`OverlayRegistry`], the type-erased front the UI talks to;
//! [`OverlayFetchResult`]; [`STATUS_MARK`]; and this crate's own tests.

use std::sync::Arc;

use rustdar_source::id::LayerId;
#[cfg(test)]
use rustdar_source::id::known;
use rustdar_source::job::{DescribedJob, JobCodec};
use rustdar_units::UserPreferences;

use crate::fetch_policy::{FetchError, FetchHealth, FetchRetry};
// Round-shape markers: named only by this module's own test fixtures, which
// construct `OverlayState<T, S>` directly.
#[cfg(test)]
use crate::fetch_policy::{Assembled, Whole};
use crate::render::controls::{
    ControlEffect, ControlItem, ControlUpdate, PaneControlContext, PaneControlContextMut,
};
use crate::render::draw::{DrawPointContext, HoverContext, MapPoint, PointPainter};
use crate::types::OverlayLabel;

// ── The moved contract, re-exported at the paths it has always had ────────
//
// WO-M9 moved the trait and its whole vocabulary into `rustdar_source::handler`
// and renamed the trait `SourceHandler` at the definition. These re-exports are
// what let that be a move rather than a sweep: `rustdar-egui`, `rustdar-app`
// and `rustdar-worker` name these types at `rustdar_overlays::render::
// overlay_state::…` in ~30 places, and every one of them still resolves.
pub use rustdar_source::handler::{
    ClickableItem, FetchConfig, FetchPayload, FetchTask, OverlayItem, OverlayLegend, OverlayState,
    PopupAction, PopupActionKind, PopupContent, PopupSection, RasterizeContext, RenderMode, Signed,
    SourceHandler as OverlayHandler, Surface, TaskFuture,
};

/// What opens a layer-stack status line that is reporting a fault rather than a
/// count — see [`OverlayRegistry::status_line`].
///
/// A `const` and not a literal because the host **reads** it: the stack row
/// renders its status line `.small().weak()`, which is the same dim grey an
/// ordinary `3 shown - W/Wa` sits in, so a warning rendered that way is a
/// warning in the typeface of a footnote. `rustdar-egui` tests this prefix to
/// colour the line instead, and a mark the two crates spelled differently would
/// be a mark that silently stopped being legible.
pub const STATUS_MARK: &str = "!";

// ── Overlay registry ─────────────────────────────────────────────────────

pub struct OverlayRegistry {
    handlers: Vec<Box<dyn OverlayHandler>>,
    /// Populated by map clicks; paged through in the popup.
    pub selected_overlays: Vec<Arc<dyn OverlayItem>>,
    pub selected_overlay_page: usize,
    /// The config value each handler was last loaded from, for the handlers
    /// whose state has not moved since — the dirty half of
    /// [`load_pane_configs`], which every pane calls on every frame.
    ///
    /// An entry means "this handler holds exactly what deserializing that
    /// value would give it", so the load is a no-op and can be skipped. Every
    /// route that can move a handler's state removes its entry
    /// ([`forget_loaded_config`]), so the skip is only ever taken where
    /// re-deserializing would have changed nothing — the reload discipline
    /// (`Gui::write_pane_overlay`: a change that never reached the config is
    /// undone next frame) is preserved exactly, because a change that *did*
    /// happen always cleared its entry first.
    ///
    /// [`load_pane_configs`]: OverlayRegistry::load_pane_configs
    /// [`forget_loaded_config`]: OverlayRegistry::forget_loaded_config
    loaded_configs: std::collections::HashMap<LayerId, serde_json::Value>,
}

/// This crate's own eleven — and **only** those.
///
/// Since WO-M9 the twelfth layer, radar, is `rustdar_radar::source::
/// RadarSource`, and this crate cannot name it: the WO-M3 charter cuts the
/// overlays -> radar edge and `rustdar-source`'s `tests/charter.rs` keeps it
/// cut. A registry that has to hold the whole app's layer set is built by the
/// crate that can see both source crates — `rustdar_egui::sources::all` —
/// and handed here through [`with_handlers`](OverlayRegistry::with_handlers).
///
/// So this `Default` is the right registry for a test about overlay handlers
/// and the wrong one for anything counting the app's layers. `rustdar-egui`'s
/// `sources` tests are where the composed twelve are pinned.
impl Default for OverlayRegistry {
    fn default() -> Self {
        Self::with_handlers(super::handlers::sources())
    }
}

impl OverlayRegistry {
    /// A registry over exactly the handlers it is given — the seam WO-M9 opened
    /// so that the app's layer set is a *composition* of the source crates'
    /// `sources()` functions rather than one crate's private list.
    pub fn with_handlers(handlers: Vec<Box<dyn OverlayHandler>>) -> Self {
        Self {
            handlers,
            selected_overlays: Vec::new(),
            selected_overlay_page: 0,
            loaded_configs: std::collections::HashMap::new(),
        }
    }

    fn handler(&self, id: &LayerId) -> Option<&dyn OverlayHandler> {
        self.handlers.iter().find(|h| &h.id() == id).map(|h| &**h)
    }

    fn handler_mut(&mut self, id: &LayerId) -> Option<&mut dyn OverlayHandler> {
        self.forget_loaded_config(id);
        for handler in &mut self.handlers {
            if &handler.id() == id {
                return Some(&mut **handler);
            }
        }
        None
    }

    /// Drop `id`'s "already loaded" note, so the next
    /// [`load_pane_configs`](OverlayRegistry::load_pane_configs) re-applies
    /// its config rather than skipping it.
    ///
    /// Called from the one place every mutable handler borrow comes through
    /// ([`handler_mut`](OverlayRegistry::handler_mut)), so it cannot be
    /// forgotten by a new mutator: whatever a caller does with the borrow, the
    /// note is already gone.
    fn forget_loaded_config(&mut self, id: &LayerId) {
        self.loaded_configs.remove(id);
    }

    pub fn handlers(&self) -> impl Iterator<Item = &dyn OverlayHandler> {
        self.handlers.iter().map(|h| &**h)
    }

    /// The default draw order, bottom to top — every registered handler's id
    /// sorted by [`OverlayHandler::draw_order_weight`]. What a fresh pane
    /// starts from, and the order `reconcile_draw_order` inserts
    /// registered-but-missing ids by. The literal-list pin in
    /// `registry_identity_tests` holds this to the order users have always
    /// seen.
    pub fn default_draw_order(&self) -> Vec<LayerId> {
        let mut handlers: Vec<&dyn OverlayHandler> = self.handlers().collect();
        handlers.sort_by_key(|h| h.draw_order_weight());
        handlers.iter().map(|h| h.id()).collect()
    }

    pub fn get_handler(&self, id: &LayerId) -> Option<&dyn OverlayHandler> {
        self.handler(id)
    }

    pub fn get_handler_mut(&mut self, id: &LayerId) -> Option<&mut dyn OverlayHandler> {
        self.handler_mut(id)
    }

    /// The handler registered under `id`, if any — the open-string primary
    /// the M8b draw loop asks; an id no handler owns answers `None` (unknown
    /// ids are retained by callers and skipped at draw, never resolved here).
    pub fn handler_by_id(&self, id: &LayerId) -> Option<&dyn OverlayHandler> {
        self.handler(id)
    }

    /// The mutable half of [`handler_by_id`](Self::handler_by_id); routes
    /// through [`handler_mut`](Self::handler_mut), so the loaded-config note
    /// is dropped exactly as for every other mutable borrow.
    pub fn handler_by_id_mut(&mut self, id: &LayerId) -> Option<&mut dyn OverlayHandler> {
        self.handler_mut(id)
    }

    pub fn data_generation(&self, id: &LayerId) -> u64 {
        self.handler(id).map_or(0, |h| h.data_generation())
    }

    /// [`OverlayHandler::content_signature`] for `kind`; `0` for a kind with
    /// no handler.
    pub fn content_signature(&self, id: &LayerId) -> u64 {
        self.handler(id).map_or(0, |h| h.content_signature())
    }

    /// The NWS alert fetch payload for a known alert list, exactly as the
    /// network fetch would deliver it to [`apply_fetch_result`]. Public so a
    /// host (or its tests) can feed a chosen warning set through the one
    /// production ingest path instead of growing a parallel setter.
    ///
    /// [`apply_fetch_result`]: OverlayRegistry::apply_fetch_result
    #[doc(hidden)]
    pub fn nws_alerts_payload(alerts: Vec<crate::nws::alert::NwsAlert>) -> FetchPayload {
        Box::new(super::handlers::alert::NwsAlertFetchResult(Ok(
            crate::nws::fetch::ActiveAlerts::whole(alerts),
        )))
    }

    /// The NWS alert payload for a round whose **zone resolution came up
    /// short** — the alerts that did resolve, beside the report of what did
    /// not, exactly as `nws::fetch` delivers one.
    ///
    /// The counterpart to [`nws_alerts_payload`], for the same reason
    /// [`spc_discussions_failure_payload`] is the counterpart to its own: the
    /// half-delivered round is a production state, and a test that cannot build
    /// one has to reach past the ingest path and poke the ledger, which is
    /// exactly how a verdict stops reaching the UI without anything going red.
    ///
    /// [`nws_alerts_payload`]: OverlayRegistry::nws_alerts_payload
    /// [`spc_discussions_failure_payload`]: OverlayRegistry::spc_discussions_failure_payload
    #[doc(hidden)]
    pub fn nws_alerts_partial_payload(
        alerts: Vec<crate::nws::alert::NwsAlert>,
        zones: crate::nws::zones::ZoneResolution,
    ) -> FetchPayload {
        Box::new(super::handlers::alert::NwsAlertFetchResult(Ok(
            crate::nws::fetch::ActiveAlerts { alerts, zones },
        )))
    }

    /// The SPC Mesoscale Discussion fetch payload for a known MD list — the
    /// same seam as [`nws_alerts_payload`], for the same reason.
    ///
    /// [`nws_alerts_payload`]: OverlayRegistry::nws_alerts_payload
    #[doc(hidden)]
    pub fn spc_discussions_payload(
        discussions: Vec<crate::spc::discussion::SpcDiscussion>,
    ) -> FetchPayload {
        Box::new(super::handlers::discussion::SpcDiscussionFetchResult(Ok(
            discussions,
        )))
    }

    /// The SPC MD payload for a fetch that **failed**, exactly as the network
    /// path would deliver it. The counterpart to [`spc_discussions_payload`],
    /// and what lets a test drive real failing frames through the real ingest
    /// path rather than poking the ledger directly.
    ///
    /// [`spc_discussions_payload`]: OverlayRegistry::spc_discussions_payload
    #[doc(hidden)]
    pub fn spc_discussions_failure_payload(error: FetchError) -> FetchPayload {
        Box::new(super::handlers::discussion::SpcDiscussionFetchResult(Err(
            error,
        )))
    }

    /// Age `kind`'s retry ledger — see [`FetchRetry::rewind`].
    #[doc(hidden)]
    pub fn rewind_retry(&mut self, id: &LayerId, by: std::time::Duration) {
        if let Some(r) = self.handler_mut(id).and_then(|h| h.retry_mut()) {
            r.rewind(by);
        }
    }

    pub fn has_data(&self, id: &LayerId) -> bool {
        self.handler(id).is_some_and(|h| h.has_data())
    }

    pub fn is_fetching(&self, id: &LayerId) -> bool {
        self.handler(id).is_some_and(|h| h.is_fetching())
    }

    pub fn set_fetching(&mut self, id: &LayerId, fetching: bool) {
        if let Some(h) = self.handler_mut(id) {
            h.set_fetching(fetching);
        }
    }

    pub fn fetch_time(&self, id: &LayerId) -> Option<web_time::Instant> {
        self.handler(id).and_then(|h| h.fetch_time())
    }

    pub fn auto_poll_interval(&self, id: &LayerId) -> Option<u64> {
        self.handler(id).and_then(|h| h.auto_poll_interval())
    }

    /// [`OverlayHandler::auto_fetch_delay`] for `kind` — the one gate the
    /// automatic poll consults, and the only caller that may.
    pub fn auto_fetch_delay(&self, id: &LayerId) -> Option<std::time::Duration> {
        self.handler(id).and_then(|h| h.auto_fetch_delay())
    }

    /// Wipe `kind`'s retry ledger because the **user** asked for a fetch.
    ///
    /// Called from `push_user_overlay_fetch` and nowhere else, so that "a user
    /// action is never made to wait out a backoff" holds by construction.
    pub fn clear_retry(&mut self, id: &LayerId) {
        if let Some(r) = self.handler_mut(id).and_then(|h| h.retry_mut()) {
            r.clear();
        }
    }

    /// File a failure against `kind`'s ladder from outside the handler.
    ///
    /// The host uses this for failures that never reach `apply_fetch_result`
    /// because no task was ever built — see
    /// [`OverlayHandler::create_fetch_tasks`] returning empty.
    pub fn record_fetch_failure(&mut self, id: &LayerId, error: &FetchError) {
        if let Some(h) = self.handler_mut(id) {
            h.set_fetching(false);
            if let Some(r) = h.retry_mut() {
                r.record_failure(error);
            }
        }
    }

    /// What `kind`'s last fetch said.
    ///
    /// Read by `Gui::set_pane_overlay_with_fetch` to decide whether switching a
    /// layer on should re-ask the origin; the *rendering* of it goes through
    /// [`Self::controls`], which prepends
    /// [`FetchRetry::status_note`](crate::fetch_policy::FetchRetry::status_note)
    /// for every layer rather than trusting handlers to remember.
    pub fn fetch_health(&self, id: &LayerId) -> Option<&FetchHealth> {
        self.handler(id).and_then(|h| h.retry()).map(|r| r.health())
    }

    pub fn item_count(&self, id: &LayerId) -> usize {
        self.handler(id).map_or(0, |h| h.item_count())
    }

    pub fn is_enabled(&self, id: &LayerId) -> bool {
        self.handler(id).is_some_and(|h| h.is_enabled())
    }

    pub fn set_enabled(&mut self, id: &LayerId, enabled: bool) {
        if let Some(h) = self.handler_mut(id) {
            h.set_enabled(enabled);
        }
    }

    /// [`OverlayHandler::status_line`] for `kind`, marked when the layer is not
    /// updating; `None` for a kind with no handler.
    ///
    /// The companion to [`Self::controls`], and the reason both live here rather
    /// than in the handlers. `controls` carries the full sentence, but it is in
    /// the layer's **options panel** — a user has to select the layer to read
    /// it, and nobody selects a layer that looks fine. The stack row is the
    /// surface that is always on screen, so it is where "these warnings stopped
    /// updating" has to appear if it is to be seen at all. A frozen alert set
    /// and a current one are identical on the map; this is the only difference
    /// visible without a click.
    ///
    /// Short by design: the row is one line beside a name, and the sentence that
    /// explains it is one click away.
    ///
    /// Only for a layer that is **on** — a hidden layer draws nothing, so
    /// nothing it holds can be misread, and its row is already dimmed. Free
    /// while healthy: both tests are discriminant tests and the `format!` runs
    /// only when there is something to say, which matters because this is asked
    /// of every layer in the stack every frame.
    ///
    /// # Two marks, because there are two ways to be wrong
    ///
    /// `not updating` is the time axis and `incomplete` is the coverage axis,
    /// and a layer can carry both at once: `! not updating, incomplete`. They
    /// are not interchangeable and must not be collapsed into one word — stale
    /// means wait or refresh, incomplete means look at what is missing and why,
    /// and a mark that cannot tell a user which one they are looking at is a
    /// mark they cannot act on.
    ///
    /// `incomplete` is a verdict rather than a count on purpose. The counts are
    /// one click away in the layer's options
    /// ([`DataCompleteness::status_note`]), and the handler's own line is
    /// already standing right beside this saying how much of it drew —
    /// `! incomplete - 85 of 297 shown - W/Wa/Adv/Oth`.
    pub fn status_line(&self, id: &LayerId) -> Option<String> {
        let handler = self.handler(id)?;
        let line = handler.status_line();
        if !handler.is_enabled() {
            return line;
        }
        let retry = handler.retry();
        let stale = retry.is_some_and(FetchRetry::is_unhealthy);
        let incomplete = retry.and_then(|r| r.coverage().status_mark());
        let mark = match (stale, incomplete) {
            (false, None) => return line,
            (true, None) => format!("{STATUS_MARK} not updating"),
            (false, Some(mark)) => format!("{STATUS_MARK} {mark}"),
            (true, Some(mark)) => format!("{STATUS_MARK} not updating, {mark}"),
        };
        Some(match line {
            Some(line) => format!("{mark} - {line}"),
            None => mark,
        })
    }

    pub fn clickable_items(&self, id: &LayerId) -> Vec<ClickableItem<'_>> {
        self.handler(id)
            .map_or_else(Vec::new, |h| h.clickable_items())
    }

    /// [`OverlayHandler::map_labels`] for `kind`; empty for a kind with no
    /// handler.
    pub fn map_labels(&self, id: &LayerId) -> &[OverlayLabel] {
        self.handler(id).map_or(&[], |h| h.map_labels())
    }

    pub fn hover_value_at(&self, id: &LayerId, lat: f64, lon: f64) -> Option<String> {
        self.handler(id).and_then(|h| h.hover_value_at(lat, lon))
    }

    /// [`OverlayHandler::legend`] for `id`, signature and all — the wrapper is
    /// not unwrapped on the way past, because the caller is the one that can
    /// use it to skip.
    pub fn legend(&self, id: &LayerId) -> Option<Signed<OverlayLegend>> {
        self.handler(id).and_then(|h| h.legend())
    }

    /// [`OverlayHandler::theme_sensitive`] for `id`.
    ///
    /// An id with no registered handler answers `false`: an unknown layer has
    /// no raster in any cache, so there is nothing a theme flip could
    /// invalidate.
    pub fn theme_sensitive(&self, id: &LayerId) -> bool {
        self.handler(id).is_some_and(|h| h.theme_sensitive())
    }

    pub fn popup_content(
        &self,
        selected: &dyn OverlayItem,
        prefs: &UserPreferences,
    ) -> PopupContent {
        selected.popup_content(prefs)
    }

    /// Routes to the handler that owns `action.target`.
    pub fn handle_popup_action(&mut self, action: &PopupAction) -> bool {
        let id = action.target.layer_id();
        self.handler_mut(&id)
            .is_some_and(|h| h.handle_popup_action(action))
    }

    /// Re-runs `retain_selections` afterwards, since the data just changed.
    pub fn apply_fetch_result(&mut self, result: OverlayFetchResult) {
        let id = result.kind;
        // The one mutation route that reaches a handler without going through
        // `handler_mut` — it indexes, so that `retain_selections` can borrow
        // `selected_overlays` beside it. No shipped handler's
        // `apply_fetch_result` moves what `serialize_state` reports, so this
        // is belt-and-braces rather than a fix for a live bug; it is here so
        // "a handler's state moved ⇒ its note is gone" holds by construction
        // instead of by auditing twelve `apply_fetch_result` bodies.
        self.forget_loaded_config(&id);
        if let Some(idx) = self.handlers.iter().position(|h| h.id() == id) {
            self.handlers[idx].apply_fetch_result(result.data);
            self.handlers[idx].retain_selections(&mut self.selected_overlays);
        }
        if self.selected_overlay_page >= self.selected_overlays.len().max(1) {
            self.selected_overlay_page = 0;
        }
    }

    /// [`OverlayHandler::prepare_job`] through the registry — the only way a
    /// handler's raster is reached; the closure twin this sat beside is
    /// deleted.
    pub fn prepare_job(&self, id: &LayerId, ctx: &RasterizeContext) -> Option<DescribedJob> {
        self.handler(id).and_then(|h| h.prepare_job(ctx))
    }

    /// [`OverlayHandler::job_codec`] through the registry — the codec row the
    /// dispatch frames and labels `kind`'s described job with.
    pub fn job_codec(&self, id: &LayerId) -> Option<&'static JobCodec> {
        self.handler(id).and_then(|h| h.job_codec())
    }

    /// [`OverlayHandler::hit_items`] through the registry — the page-side
    /// half of a hit-map kind's described render, captured at the dispatch
    /// beside [`prepare_job`](Self::prepare_job).
    pub fn hit_items(&self, id: &LayerId) -> Option<Vec<Arc<dyn OverlayItem>>> {
        self.handler(id).and_then(|h| h.hit_items())
    }

    pub fn create_fetch_tasks(&self, id: &LayerId, ctx: &FetchConfig) -> Vec<FetchTask> {
        self.handler(id)
            .map_or_else(Vec::new, |h| h.create_fetch_tasks(ctx))
    }

    /// The handler's own options, with its fetch health prepended.
    ///
    /// The note is added **here** rather than in each handler because handlers
    /// forget. Exactly one of the six fetching handlers rendered
    /// [`FetchRetry::status_note`] — SPC discussions — so NWS alerts, storm
    /// reports, METAR and lightning could each be frozen on data hours old with
    /// nothing on screen but an "Updated 47m ago" line that reads as a fact
    /// about the weather rather than about the app. A handler cannot forget
    /// something it does not write.
    ///
    /// **First**, not last. It changes what everything under it means: an empty
    /// alerts layer is a quiet afternoon or an unreachable origin, and a full
    /// one is current warnings or a frozen copy of last hour's. A caveat below
    /// the thing it qualifies is a caveat most people do not read.
    ///
    /// Two notes, not one merged sentence, and a layer can carry both. Staleness
    /// leads because it is the older and broader claim; incompleteness follows
    /// it and above everything else, because `212 of 297 alerts missing` also
    /// changes what `85 alerts shown` beneath it means.
    pub fn controls(&self, id: &LayerId, ctx: &PaneControlContext<'_>) -> Vec<ControlItem> {
        let Some(handler) = self.handler(id) else {
            return Vec::new();
        };
        let mut items = handler.controls(ctx);
        // Inserted in reverse, so each lands above the one before it.
        if let Some(note) = handler.retry().and_then(|r| r.coverage().status_note()) {
            items.insert(0, ControlItem::InfoText { text: note });
        }
        if let Some(note) = handler.retry().and_then(FetchRetry::status_note) {
            items.insert(0, ControlItem::InfoText { text: note });
        }
        items
    }

    pub fn apply_control(
        &mut self,
        id: &LayerId,
        update: &ControlUpdate,
        ctx: &mut PaneControlContextMut<'_>,
    ) -> ControlEffect {
        if let Some(h) = self.handler_mut(id) {
            h.apply_control(update, ctx)
        } else {
            ControlEffect::None
        }
    }

    pub fn render_mode(&self, id: &LayerId) -> Option<RenderMode> {
        self.handler(id).map(|h| h.render_mode())
    }

    pub fn display_name(&self, id: &LayerId) -> &str {
        self.handler(id).map_or("Unknown", |h| h.display_name())
    }

    pub fn default_enabled(&self, id: &LayerId) -> bool {
        self.handler(id).is_some_and(|h| h.default_enabled())
    }

    /// Seeds a new pane's `enabled_overlays`; call after config deserialization.
    pub fn build_enabled_map(&self) -> std::collections::HashMap<LayerId, bool> {
        self.handlers
            .iter()
            .map(|h| (h.id(), h.is_enabled()))
            .collect()
    }

    pub fn save_pane_configs(&self) -> std::collections::HashMap<LayerId, serde_json::Value> {
        self.handlers
            .iter()
            .map(|h| (h.id(), h.serialize_state()))
            .collect()
    }

    /// Handlers absent from `configs` keep their current state.
    ///
    /// Every map pane calls this on every frame, for all twelve handlers, so
    /// the body has to be free when nothing changed: it used to deep-clone a
    /// `serde_json::Value` per handler and hand it to `deserialize_state`,
    /// which for two of them cloned again and rebuilt a `HashSet` through
    /// `serde_json::from_value` — config-changed-only work running at frame
    /// rate. A handler still holding what a value would give it is skipped by
    /// comparing against [`loaded_configs`], which allocates nothing.
    ///
    /// [`loaded_configs`]: OverlayRegistry::loaded_configs
    pub fn load_pane_configs(
        &mut self,
        configs: &std::collections::HashMap<LayerId, serde_json::Value>,
    ) {
        let Self {
            handlers,
            loaded_configs,
            ..
        } = self;
        for h in handlers {
            let id = h.id();
            let Some(val) = configs.get(&id) else {
                continue;
            };
            if loaded_configs.get(&id).is_some_and(|seen| seen == val) {
                continue;
            }
            h.deserialize_state(val.clone());
            loaded_configs.insert(id, val.clone());
        }
    }

    pub fn save_enabled_map(&self) -> std::collections::HashMap<LayerId, bool> {
        self.handlers
            .iter()
            .map(|h| (h.id(), h.is_enabled()))
            .collect()
    }

    // ── Per-frame point rendering delegates ───────────────────────────

    pub fn per_frame_points(&self, id: &LayerId) -> &[MapPoint] {
        self.handler(id).map_or(&[], |h| h.per_frame_points())
    }

    pub fn draw_point(
        &self,
        layer: &LayerId,
        id: u32,
        painter: &mut dyn PointPainter,
        ctx: &DrawPointContext,
    ) {
        if let Some(h) = self.handler(layer) {
            h.draw_point(id, painter, ctx);
        }
    }

    pub fn point_hit_radius(&self, id: &LayerId, zoom: f32) -> f32 {
        self.handler(id).map_or(0.0, |h| h.point_hit_radius(zoom))
    }

    pub fn hover_text(&self, layer: &LayerId, id: u32, ctx: &HoverContext<'_>) -> Option<String> {
        self.handler(layer).and_then(|h| h.hover_text(id, ctx))
    }

    // ── Config persistence ────────────────────────────────────────────

    /// Keyed by the layer id **string** ([`LayerId::as_str`]) — the exact
    /// bytes these maps have always been keyed by, so every existing config
    /// file keeps matching; `handler_state_keys_are_the_twelve_names_saved_configs_file_state_under`
    /// pins the live ids against that literal twelve. Renaming an id orphans
    /// its saved state; the ledger is append-only for exactly that reason.
    /// Null states are omitted.
    pub fn serialize_handler_states(&self) -> serde_json::Map<String, serde_json::Value> {
        let mut map = serde_json::Map::new();
        for h in &self.handlers {
            let val = h.serialize_state();
            if !val.is_null() {
                map.insert(h.id().as_str().to_string(), val);
            }
        }
        map
    }

    pub fn deserialize_handler_states(
        &mut self,
        states: &serde_json::Map<String, serde_json::Value>,
    ) {
        // A second source of handler state: whatever a pane config last put
        // there is no longer what the handlers hold.
        self.loaded_configs.clear();
        for h in &mut self.handlers {
            if let Some(val) = states.get(h.id().as_str()) {
                h.deserialize_state(val.clone());
            }
        }
    }
}

// ── Unified overlay fetch result ──────────────────────────────────────────

pub struct OverlayFetchResult {
    pub kind: LayerId,
    pub data: FetchPayload,
}

#[cfg(test)]
mod pane_config_tests {
    use super::*;

    /// The pane config for "MDs off, everything else as built".
    fn mds_off(registry: &mut OverlayRegistry) -> std::collections::HashMap<LayerId, Value> {
        registry.set_enabled(&known::SPC_DISCUSSIONS, false);
        let configs = registry.save_pane_configs();
        registry.set_enabled(&known::SPC_DISCUSSIONS, true);
        configs
    }

    use serde_json::Value;

    /// A load applies, and a second load of the same map leaves the same
    /// answer — the skip is not allowed to be visible.
    #[test]
    fn loading_a_config_twice_lands_where_loading_it_once_did() {
        let mut registry = OverlayRegistry::default();
        let configs = mds_off(&mut registry);
        assert!(
            registry.is_enabled(&known::SPC_DISCUSSIONS),
            "fixture: the handler is on, so the config has something to do",
        );

        registry.load_pane_configs(&configs);
        assert!(
            !registry.is_enabled(&known::SPC_DISCUSSIONS),
            "the first load must apply the config",
        );

        // The frame-rate case: the same map, again, with nothing having
        // happened in between.
        registry.load_pane_configs(&configs);
        assert!(
            !registry.is_enabled(&known::SPC_DISCUSSIONS),
            "a repeat load changed the answer",
        );
    }

    /// The one mutation route that reaches a handler without `handler_mut`
    /// still forgets what that handler was loaded from.
    ///
    /// Nothing observable depends on this today — no shipped
    /// `apply_fetch_result` writes a field its `deserialize_state` reads, so
    /// deleting the invalidation breaks no behaviour any test can see. That is
    /// exactly why the invariant is asserted directly: the skip at
    /// [`OverlayRegistry::load_pane_configs`] is built on "every mutation route
    /// forgets", and a route that quietly stops forgetting would only surface
    /// the first time a handler grew a field on both sides.
    #[test]
    fn a_fetch_result_forgets_the_config_the_handler_was_loaded_from() {
        let mut registry = OverlayRegistry::default();
        let configs = mds_off(&mut registry);

        registry.load_pane_configs(&configs);
        assert!(
            registry
                .loaded_configs
                .contains_key(&known::SPC_DISCUSSIONS),
            "fixture: a load has to record what it read for the skip to exist",
        );

        registry.apply_fetch_result(OverlayFetchResult {
            kind: known::SPC_DISCUSSIONS,
            data: OverlayRegistry::spc_discussions_payload(Vec::new()),
        });

        assert!(
            !registry
                .loaded_configs
                .contains_key(&known::SPC_DISCUSSIONS),
            "a fetch may move what `serialize_state` reports, so the next load \
             has to run rather than be skipped",
        );
    }

    /// The reload discipline survives the skip: a handler change that never
    /// reached the config is still undone by the next load.
    ///
    /// This is the one thing the "already loaded" note could have broken —
    /// skip a load whose handler has since moved and the change sticks
    /// forever, which is precisely the bug `Gui::write_pane_overlay`'s
    /// both-halves rule exists to prevent. Every mutable handler borrow drops
    /// the note, so there is no route to a stale skip; the two below are the
    /// routes the app actually takes.
    #[test]
    fn a_handler_change_outside_the_config_is_still_undone_by_the_next_load() {
        let mut registry = OverlayRegistry::default();
        let configs = mds_off(&mut registry);
        registry.load_pane_configs(&configs);
        assert!(!registry.is_enabled(&known::SPC_DISCUSSIONS));

        // Route 1: the registry's own setter — the layer-stack eye's half
        // that forgot to write the config.
        registry.set_enabled(&known::SPC_DISCUSSIONS, true);
        registry.load_pane_configs(&configs);
        assert!(
            !registry.is_enabled(&known::SPC_DISCUSSIONS),
            "a `set_enabled` that never reached the config survived the \
             reload — the skip went stale",
        );

        // Route 2: a raw mutable handler borrow, which anything may take and
        // do anything with.
        registry
            .handler_by_id_mut(&known::SPC_DISCUSSIONS)
            .expect("the MD handler is registered")
            .set_enabled(true);
        registry.load_pane_configs(&configs);
        assert!(
            !registry.is_enabled(&known::SPC_DISCUSSIONS),
            "a change made through `get_handler_mut` survived the reload",
        );
    }

    /// Two panes with different configs both get theirs, in either order —
    /// the note is per handler and per value, not "the last map I saw".
    #[test]
    fn alternating_two_panes_configs_gives_each_pane_its_own() {
        let mut registry = OverlayRegistry::default();
        let off = mds_off(&mut registry);
        let on = registry.save_pane_configs();
        assert!(
            registry.is_enabled(&known::SPC_DISCUSSIONS),
            "fixture: the two configs differ",
        );

        for _ in 0..3 {
            registry.load_pane_configs(&off);
            assert!(
                !registry.is_enabled(&known::SPC_DISCUSSIONS),
                "the off pane did not get its config",
            );
            registry.load_pane_configs(&on);
            assert!(
                registry.is_enabled(&known::SPC_DISCUSSIONS),
                "the on pane did not get its config",
            );
        }
    }
}

#[cfg(test)]
mod retry_ledger_tests {
    use super::*;
    use crate::fetch_policy::FetchError;
    use crate::render::handlers::sources;

    /// **The copy-paste guard.** Every handler that auto-polls must keep a
    /// retry ledger, and a transient failure must actually push its next
    /// automatic attempt into the future.
    ///
    /// Written over `sources()` rather than over a list of names, so a
    /// new overlay is covered the day it is registered. All six auto-polling
    /// handlers had the identical defect — log the error, clear `fetching`,
    /// leave `fetch_time` alone — because the shape was copied from whichever
    /// one came first, and nothing stopped the seventh from copying it too.
    /// (`SpcOutlook` writes the same error branch but declares no interval, so
    /// it never reached the poll gate; it is fixed alongside them rather than
    /// left as the one copy of the old shape.)
    ///
    /// A handler that keeps no ledger gets the old behaviour exactly: the poll
    /// gate falls back to `fetch_time`, which a failure never stamps, and the
    /// layer is due again on the next frame.
    #[test]
    fn every_auto_polling_handler_backs_off_after_a_failure() {
        let mut checked = 0;
        for handler in sources().iter_mut() {
            let Some(interval) = handler.auto_poll_interval() else {
                continue;
            };
            checked += 1;
            let id = handler.id();
            let id = id.as_str();

            assert!(
                handler.retry().is_some(),
                "{id} auto-polls every {interval}s but keeps no retry \
                 ledger, so a failed fetch leaves it due on every frame",
            );

            assert_eq!(
                handler.auto_fetch_delay(),
                Some(std::time::Duration::ZERO),
                "{id} has never been fetched, so it is due now",
            );

            handler
                .retry_mut()
                .expect("just asserted present")
                .record_failure(&FetchError::transient("network down"));

            let delay = handler
                .auto_fetch_delay()
                .expect("a transient failure is still owed an eventual retry");
            assert!(
                !delay.is_zero(),
                "{id} is due again immediately after a failed fetch — this \
                 is the per-frame retry storm",
            );
            assert!(
                delay <= std::time::Duration::from_secs(interval),
                "{id} backs off past its own {interval}s poll interval, so \
                 a failure recovers slower than an ordinary refresh: {delay:?}",
            );
        }
        assert_eq!(
            checked, 6,
            "the six auto-polling handlers that shared the defect must all \
             still be covered; a new one is not exempt, and a removed one \
             should be removed from this count deliberately",
        );
    }

    /// A failure must not leave the layer stuck "Fetching...", which is the
    /// other way to make the ledger moot: `is_fetching` suppresses the poll, so
    /// a handler that never clears it never polls again.
    #[test]
    fn recording_a_failure_ends_the_fetch() {
        let mut state: OverlayState<Vec<u8>, Whole> = OverlayState::new();
        state.fetching = true;
        state.record_failure(&FetchError::transient("network down"));
        assert!(!state.fetching);
        assert_eq!(state.fetch_time, None, "a failure must not stamp the clock");

        state.fetching = true;
        state.set_data(vec![1]);
        assert!(!state.fetching);
        assert!(state.fetch_time.is_some());
    }

    /// **The silence guard.** Every layer that can fail must *say* it is
    /// failing, in its own options, without its handler having written a line
    /// of code to do it.
    ///
    /// Exactly one of the six — SPC discussions — used to push
    /// `status_note()` itself. NWS alerts, storm reports, METAR and lightning
    /// pushed nothing, so a frozen warning set looked identical to a current
    /// one and the only thing on screen was an "Updated 47m ago" line that
    /// reads as a fact about the weather. Written over `sources()`, so
    /// a seventh overlay is covered the day it is registered rather than the
    /// day someone remembers.
    #[test]
    fn every_fetching_layer_says_so_when_it_is_failing() {
        use crate::render::controls::{ControlItem, PaneControlContext};

        let ctx = PaneControlContext {
            pane_idx: 0,
            pane_state: None,
        };
        let mut registry = OverlayRegistry::default();
        let kinds: Vec<LayerId> = registry
            .handlers()
            .filter(|h| h.retry().is_some())
            .map(|h| h.id())
            .collect();
        assert_eq!(
            kinds.len(),
            7,
            "the seven fetching handlers must all be covered",
        );

        for kind in kinds {
            let quiet = registry.controls(&kind, &ctx).len();

            registry.record_fetch_failure(&kind, &FetchError::transient("connection refused"));
            let note = registry
                .fetch_health(&kind)
                .and_then(|_| {
                    registry
                        .controls(&kind, &ctx)
                        .into_iter()
                        .find_map(|item| match item {
                            ControlItem::InfoText { text }
                                if text.contains("connection refused") =>
                            {
                                Some(text)
                            }
                            _ => None,
                        })
                })
                .unwrap_or_else(|| {
                    panic!(
                        "{kind:?} is failing and its options say nothing about it — \
                         a stale layer that looks current is the whole bug",
                    )
                });
            assert!(
                note.contains("stale"),
                "{kind:?} reports the error but not what it means for what is \
                 drawn: {note}",
            );
            assert_eq!(
                registry.controls(&kind, &ctx).len(),
                quiet + 1,
                "{kind:?} gained more than the one health line",
            );

            // First, above everything it qualifies.
            assert!(
                matches!(
                    registry.controls(&kind, &ctx).first(),
                    Some(ControlItem::InfoText { text }) if text.contains("connection refused"),
                ),
                "{kind:?} buried its health note below the options it changes the \
                 meaning of",
            );

            registry.clear_retry(&kind);
            assert_eq!(
                registry.controls(&kind, &ctx).len(),
                quiet,
                "{kind:?} kept a health line after recovering",
            );
        }
    }

    /// The enable-fetch rule, in the four states it has to tell apart.
    ///
    /// The third row is the fix: a layer *holding* data that has since gone
    /// stale must re-ask when the user switches it on. The rule was
    /// `!has_data` alone, so toggling a frozen alerts layer off and on did
    /// nothing whatsoever — in the one case where a user is most likely to try
    /// it, on the layer where being wrong matters most.
    #[test]
    fn switching_a_layer_on_re_asks_when_there_is_nothing_worth_trusting() {
        let cases = [
            (false, None, true, "nothing drawn: ask"),
            (
                true,
                None,
                false,
                "fresh data drawn: do not spend a request",
            ),
            (true, Some(false), true, "data drawn but failing: ask"),
            (true, Some(true), true, "data drawn but broken: ask"),
        ];
        for (has_data, unhealthy, expected, why) in cases {
            let mut state: OverlayState<Vec<u8>, Whole> = OverlayState::new();
            if has_data {
                state.set_data(vec![1]);
            }
            match unhealthy {
                None => {}
                Some(broken) => {
                    let n = if broken {
                        crate::fetch_policy::REFUSALS_BEFORE_BROKEN
                    } else {
                        1
                    };
                    for _ in 0..n {
                        state
                            .retry
                            .record_failure(&FetchError::permanent("HTTP 400"));
                    }
                    assert_eq!(state.retry.is_broken(), broken, "premise: {why}");
                }
            }
            assert_eq!(state.enable_should_refetch(has_data), expected, "{why}");
        }
    }

    /// **The always-visible half.** The full note is in the layer's options
    /// panel, which a user has to select the layer to reach — and nobody
    /// selects a layer that looks fine. The stack row is on screen the whole
    /// time, so a layer that stopped updating has to say so *there* or it is
    /// only discoverable by someone already suspicious.
    #[test]
    fn a_failing_layer_is_marked_on_the_always_visible_stack_row() {
        let mut registry = OverlayRegistry::default();
        let kinds: Vec<LayerId> = registry
            .handlers()
            .filter(|h| h.retry().is_some())
            .map(|h| h.id())
            .collect();

        for kind in kinds {
            registry.set_enabled(&kind, true);
            let healthy = registry.status_line(&kind);
            assert!(
                !healthy
                    .as_deref()
                    .is_some_and(|l| l.contains("not updating")),
                "{kind:?} claims to be failing before anything has failed: {healthy:?}",
            );

            registry.record_fetch_failure(&kind, &FetchError::transient("connection refused"));
            let marked = registry
                .status_line(&kind)
                .unwrap_or_else(|| panic!("{kind:?} says nothing on the row while failing"));
            assert!(
                marked.starts_with("! not updating"),
                "{kind:?}: the mark must lead the row, not trail whatever the \
                 layer was already saying: {marked}",
            );
            if let Some(healthy) = healthy.as_deref() {
                assert!(
                    marked.contains(healthy),
                    "{kind:?} lost its own status line to the mark: {marked}",
                );
            }

            registry.clear_retry(&kind);
            assert_eq!(
                registry.status_line(&kind),
                healthy,
                "{kind:?} kept the mark after recovering",
            );
        }
    }

    /// The alerts of one poll: `placed` of them carrying the outlines a
    /// zone-based alert only has once `resolve_zone_geometries` has fetched
    /// them, the rest carrying none — which is exactly what the handler is
    /// handed when zone boundaries fail.
    fn alerts_where_only_some_resolved(
        total: usize,
        placed: usize,
    ) -> Vec<crate::nws::alert::NwsAlert> {
        use crate::nws::alert::{AlertCategory, NwsAlert};
        use crate::types::{HatchPattern, OverlayFeature};
        (0..total)
            .map(|i| NwsAlert {
                id: format!("urn:oid:2.49.0.1.840.0.{i}"),
                event: "Tornado Warning".to_string(),
                category: AlertCategory::Warning,
                severity: "Severe".parse().unwrap(),
                urgency: "Immediate".parse().unwrap(),
                certainty: "Observed".parse().unwrap(),
                headline: None,
                description: String::new(),
                instruction: None,
                area_desc: String::new(),
                sender_name: String::new(),
                effective: String::new(),
                expires: String::new(),
                onset: None,
                ends: None,
                affected_zones: vec!["https://api.weather.gov/zones/county/OKC001".to_string()],
                features: std::sync::Arc::new(if i < placed {
                    vec![OverlayFeature::new(
                        vec![vec![vec![(35.0, -97.0), (36.0, -97.0), (36.0, -96.0)]]],
                        [0, 0, 0, 0],
                        [0, 0, 0, 0],
                        "Tornado Warning".to_string(),
                        String::new(),
                        HatchPattern::None,
                    )]
                } else {
                    Vec::new()
                }),
            })
            .collect()
    }

    /// **The bug, end to end, in the numbers it was observed in.**
    ///
    /// 297 warnings arrive, 212 of them referencing zone boundaries that would
    /// not resolve, so 85 are on the map. Every check in this crate passed: the
    /// alert fetch genuinely succeeded, so the ladder is clear, the clock is
    /// fresh and the health is `Ok`. The row said `297 shown - W/Wa/Adv` and the
    /// options said `Updated 0s ago`, and both were *true statements* about the
    /// fetch and lies about the map.
    ///
    /// Driven through [`OverlayRegistry::apply_fetch_result`] — the one path
    /// production takes — rather than by writing to the ledger, because every
    /// link is load-bearing and any of them going quiet reproduces the bug:
    /// the resolver counting, the fetch carrying, the handler filing through
    /// `set_data_with_coverage`, and the registry rendering. Delete any one and
    /// this fails.
    #[test]
    fn a_layer_that_under_drew_says_so_on_its_row_and_in_its_options() {
        use crate::nws::zones::{ZoneFailure, ZoneResolution};

        let ctx = PaneControlContext {
            pane_idx: 0,
            pane_state: None,
        };
        let kind = known::NWS_ALERTS;
        let mut registry = OverlayRegistry::default();

        // Healthy first, so the difference below is this poll's and not the
        // fixture's.
        registry.apply_fetch_result(OverlayFetchResult {
            kind: kind.clone(),
            data: OverlayRegistry::nws_alerts_payload(alerts_where_only_some_resolved(297, 297)),
        });
        assert_eq!(
            registry.status_line(&kind).as_deref(),
            Some("297 shown - W/Wa/Adv/Oth"),
            "a whole round must read as a plain count",
        );
        let quiet = registry.controls(&kind, &ctx).len();

        registry.apply_fetch_result(OverlayFetchResult {
            kind: kind.clone(),
            data: OverlayRegistry::nws_alerts_partial_payload(
                alerts_where_only_some_resolved(297, 85),
                ZoneResolution {
                    alerts_expected: 297,
                    alerts_complete: 85,
                    alerts_partial: 0,
                    alerts_missing: 212,
                    zones_requested: 1200,
                    zones_resolved: 995,
                    failures: vec![(ZoneFailure::Http(503), 198), (ZoneFailure::NoBoundary, 7)],
                },
            ),
        });

        // The always-visible half. Both halves of it: the mark, and a count
        // that no longer claims 297 warnings are on a map holding 85.
        assert_eq!(
            registry.status_line(&kind).as_deref(),
            Some("! incomplete - 85 of 297 shown - W/Wa/Adv/Oth"),
            "a layer drawing 85 of 297 warnings must not read as healthy",
        );

        // The one-click half: what is missing, why, and that it is not the
        // other fault.
        let note = registry
            .controls(&kind, &ctx)
            .into_iter()
            .find_map(|item| match item {
                ControlItem::InfoText { text } if text.starts_with("Incomplete") => Some(text),
                _ => None,
            })
            .expect("the options must say what the row is marking");
        for expected in [
            "missing 212 of 297 alerts",
            "995 of 1200 zone boundaries resolved",
            "198 HTTP 503",
            "7 no usable boundary",
            "Not the same as stale data",
        ] {
            assert!(
                note.contains(expected),
                "the note must be countable and say why - missing {expected:?}: {note}",
            );
        }
        assert_eq!(
            registry.controls(&kind, &ctx).len(),
            quiet + 1,
            "exactly one line was added, and the layer's own options are intact",
        );
        assert!(
            matches!(
                registry.controls(&kind, &ctx).first(),
                Some(ControlItem::InfoText { text }) if text.starts_with("Incomplete"),
            ),
            "the note must lead the options it changes the meaning of",
        );

        // Incomplete is **not** stale, and the ledger must not have confused
        // them: this round succeeded, so the clock is fresh and the ladder clear.
        assert_eq!(
            registry.fetch_health(&kind),
            Some(&FetchHealth::Ok),
            "a round that delivered 85 real warnings is a good answer, and \
             filing it as a failure would back the layer off from the retry \
             that could complete it",
        );
        let since = registry
            .fetch_time(&kind)
            .expect("a round that delivered data stamps the clock")
            .elapsed();
        assert!(
            since < std::time::Duration::from_secs(1),
            "the partial round must stamp its own clock: {since:?}",
        );

        // A recovered poll clears the mark without the handler saying so.
        registry.apply_fetch_result(OverlayFetchResult {
            kind: kind.clone(),
            data: OverlayRegistry::nws_alerts_payload(alerts_where_only_some_resolved(297, 297)),
        });
        assert_eq!(
            registry.status_line(&kind).as_deref(),
            Some("297 shown - W/Wa/Adv/Oth"),
            "the mark outlived the round it was about",
        );
    }

    /// The two faults are independent, and a layer with both says both.
    ///
    /// `! incomplete` and `! not updating` answer different questions — what is
    /// missing from the map, and whether what is on it is current — and a user
    /// looking at a warning layer needs to know which one they have. Collapsing
    /// them into one word would make the mark unactionable, and picking one
    /// would hide the other exactly when both are true: a layer that under-drew
    /// and has since stopped fetching is the worst state there is.
    #[test]
    fn a_layer_that_is_both_stale_and_incomplete_says_both() {
        use crate::nws::zones::{ZoneFailure, ZoneResolution};

        let kind = known::NWS_ALERTS;
        let mut registry = OverlayRegistry::default();
        registry.apply_fetch_result(OverlayFetchResult {
            kind: kind.clone(),
            data: OverlayRegistry::nws_alerts_partial_payload(
                alerts_where_only_some_resolved(297, 85),
                ZoneResolution {
                    alerts_expected: 297,
                    alerts_complete: 85,
                    alerts_missing: 212,
                    zones_requested: 1200,
                    zones_resolved: 995,
                    failures: vec![(ZoneFailure::Http(503), 205)],
                    ..ZoneResolution::default()
                },
            ),
        });
        assert_eq!(
            registry.status_line(&kind).as_deref(),
            Some("! incomplete - 85 of 297 shown - W/Wa/Adv/Oth"),
        );

        // The origin then goes away entirely. What is drawn is now both a
        // subset and out of date.
        registry.record_fetch_failure(&kind, &FetchError::transient("connection refused"));
        assert_eq!(
            registry.status_line(&kind).as_deref(),
            Some("! not updating, incomplete - 85 of 297 shown - W/Wa/Adv/Oth"),
            "a failure must not overwrite the coverage verdict, or the reverse",
        );

        let ctx = PaneControlContext {
            pane_idx: 0,
            pane_state: None,
        };
        let notes: Vec<String> = registry
            .controls(&kind, &ctx)
            .into_iter()
            .filter_map(|item| match item {
                ControlItem::InfoText { text } => Some(text),
                _ => None,
            })
            .collect();
        assert!(
            notes.iter().any(|n| n.contains("may be stale"))
                && notes.iter().any(|n| n.starts_with("Incomplete")),
            "both faults must be spelled out, not merged into one: {notes:?}",
        );

        // A user pressing Refresh has not yet been given the 212 zones that
        // failed. Clearing the ladder must not claim they arrived.
        registry.clear_retry(&kind);
        assert_eq!(
            registry.status_line(&kind).as_deref(),
            Some("! incomplete - 85 of 297 shown - W/Wa/Adv/Oth"),
            "clearing the retry ladder marked the layer whole before the answer \
             that would make it whole had landed",
        );
    }

    /// Switching a layer on when it is drawing 85 of 297 warnings must re-ask.
    ///
    /// The same argument as the staleness clause one axis over, and the same
    /// user: someone who can see the layer is wrong and toggles it, which is the
    /// first thing anyone tries. The zones that failed are not cached, so a
    /// re-ask retries precisely them.
    #[test]
    fn switching_on_a_layer_that_under_drew_re_asks() {
        use crate::fetch_policy::DataCompleteness;

        let mut state: OverlayState<Vec<u8>, Assembled> = OverlayState::new();
        state.set_data_with_coverage(
            vec![1],
            DataCompleteness {
                expected: 297,
                missing: 212,
                ..DataCompleteness::default()
            },
        );
        assert!(!state.retry.is_unhealthy(), "premise: the round succeeded");
        assert!(state.enable_should_refetch(true));

        // The recovered round, spelled the way an assembled layer has to spell
        // it: there is no `set_data` on this state, and the whole report is
        // what clears the mark.
        state.set_data_with_coverage(vec![1], DataCompleteness::default());
        assert!(
            !state.enable_should_refetch(true),
            "a whole round must not spend a request on being switched on",
        );
    }

    /// A hidden layer draws nothing, so nothing it holds can be misread, and
    /// its row is already dimmed. Marking it would put a warning on every layer
    /// a user has deliberately switched off.
    #[test]
    fn a_hidden_layer_is_not_marked_on_the_stack_row() {
        let mut registry = OverlayRegistry::default();
        let kind = known::NWS_ALERTS;
        registry.record_fetch_failure(&kind, &FetchError::transient("connection refused"));
        registry.set_enabled(&kind, false);
        assert!(
            !registry
                .status_line(&kind)
                .is_some_and(|l| l.contains("not updating")),
            "a layer that is switched off must not carry a staleness mark",
        );
    }

    /// A fetch already in flight is never doubled by switching the layer on,
    /// however unhealthy the ledger looks — the result that is coming is the
    /// answer.
    #[test]
    fn switching_a_layer_on_does_not_double_a_fetch_in_flight() {
        let mut state: OverlayState<Vec<u8>, Whole> = OverlayState::new();
        state
            .retry
            .record_failure(&FetchError::transient("timeout"));
        state.fetching = true;
        assert!(!state.enable_should_refetch(false));
    }
}

/// The closed enum of layer kinds stays deleted (WO-M8c).
///
/// It was the twelve-variant `enum` this file declared until M8c: a closed
/// set that every consumer matched on, which is why adding a layer used to
/// mean editing the UI crate, the dispatch and the config codec together. A
/// layer's whole identity is now one open `LayerId` string, the registry is
/// keyed by it, and the rigor the compiler used to supply — no duplicate
/// variants, a fixed draw order, a spelling nothing can typo — is supplied
/// instead by `rustdar_egui::sources::registry_identity_tests`
/// (`no_two_handlers_share_an_id`, `every_handlers_id_sits_in_the_ledger`,
/// `draw_order_weights_encode_the_default_draw_order`) — which moved there at
/// WO-M9 because their subject is the *composed* twelve, and this crate
/// registers eleven of them.
///
/// This scrape is what stops the enum growing back here. It replaces the
/// campaign's `KIND_MAX` occurrence ceiling (`rustdar-app/tests/
/// arch_ratchets.rs`, row 5a), retired in the same land: a ceiling that can
/// only be zero is an absence assertion wearing a number.
///
/// Needle hygiene, per the E0c discipline: both needles are built from split
/// literals, so this module never contains contiguously what it searches for
/// and the scrape cannot pass by matching its own source. The presence
/// control is split for the same reason — an anchor spelled contiguously here
/// would still be found after its subject moved away, which is a control that
/// cannot fail.
///
/// **Re-anchored at WO-M9.** The control used to anchor on this file's
/// declaration of the handler trait; M9 renamed that trait `SourceHandler` and
/// moved it to `rustdar_source::handler`, and M8c's own tamper round proved
/// this control goes red at exactly that moment rather than quietly reading a
/// file that no longer holds its subject. The anchor is now the registry this
/// file declares — the type whose handler list *is* the layer set, which is
/// the same claim the trait anchor made, and the last thing that would leave
/// this file while the deleted enum could still return to it.
///
/// (This paragraph may not spell the deleted type's name: the scrape asserts
/// its absence from this whole file, prose included, and caught the first
/// draft of this very comment doing it.)
#[cfg(test)]
mod overlay_kind_stays_deleted_tests {
    /// This file, read at compile time — the same self-scrape pattern as
    /// `handlers::round_delivery_tests::every_handler_module_is_on_the_delivery_list`.
    const OVERLAY_STATE: &str = include_str!("overlay_state.rs");

    /// The declaration that must never reappear.
    const KIND_DEF: &str = concat!("enum Overlay", "Kind");
    /// The type's bare name — the needle the retired `KIND_MAX` ceiling
    /// counted, now asserted at zero for this file.
    const KIND_NAME: &str = concat!("Overlay", "Kind");
    /// Presence control: the registry whose handler list IS the layer set, so
    /// the scrape is demonstrably reading the file that would host the enum.
    /// It replaced the handler-trait anchor at WO-M9, which moved the trait out
    /// of this file; a land that moves the registry out too re-anchors this
    /// again rather than deleting it.
    const REGISTRY_ANCHOR: &str = concat!("pub struct Overlay", "Registry");

    #[test]
    fn the_overlay_kind_enum_stays_deleted() {
        // Presence control first: an empty or moved haystack must fail here,
        // never pass the absence checks below by reading nothing.
        assert!(
            OVERLAY_STATE.contains(REGISTRY_ANCHOR),
            "control: overlay_state.rs no longer declares {REGISTRY_ANCHOR:?}, \
             so the absence checks below are reading the wrong file — re-anchor \
             this ratchet in the land that moved the registry",
        );

        assert!(
            !OVERLAY_STATE.contains(KIND_DEF),
            "overlay_state.rs declares `{KIND_DEF}` again. The closed layer \
             enum was deleted at WO-M8c: a layer's identity is an open \
             `LayerId` string, and a closed set here forces every consumer to \
             match on it again. Register the layer's handler and append its \
             spelling to `LAYER_ID_LEDGER` instead.",
        );
        assert!(
            !OVERLAY_STATE.contains(KIND_NAME),
            "overlay_state.rs names `{KIND_NAME}` again — the deleted layer \
             enum. Nothing should reference it, including prose: it no longer \
             exists to be read.",
        );
    }
}
