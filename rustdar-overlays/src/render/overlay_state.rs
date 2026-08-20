//! The overlay registry — and the shim layer over the contract it holds.

use std::sync::Arc;

use rustdar_source::id::LayerId;
#[cfg(test)]
use rustdar_source::id::known;
use rustdar_source::job::{DescribedJob, JobCodec};
use rustdar_units::UserPreferences;

use crate::fetch_policy::{FetchError, FetchHealth, FetchRetry};
#[cfg(test)]
use crate::fetch_policy::{Assembled, Whole};
use crate::render::controls::{
    ControlEffect, ControlItem, ControlUpdate, PaneControlContext, PaneControlContextMut,
};
use crate::render::draw::{DrawPointContext, HoverContext, MapPoint, PointPainter};
use crate::types::OverlayLabel;

pub use rustdar_source::handler::{
    ClickableItem, FetchConfig, FetchPayload, FetchTask, OverlayItem, OverlayLegend, OverlayState,
    PopupAction, PopupActionKind, PopupContent, PopupSection, RasterizeContext, RenderMode, Signed,
    SourceHandler as OverlayHandler, Surface, TaskFuture,
};

/// What opens a layer-stack status line that is reporting a fault rather than a
/// count — see [`OverlayRegistry::status_line`].
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
    loaded_configs: std::collections::HashMap<LayerId, serde_json::Value>,
}

/// This crate's own eleven — and **only** those.
impl Default for OverlayRegistry {
    fn default() -> Self {
        Self::with_handlers(super::handlers::sources())
    }
}

impl OverlayRegistry {
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
    fn forget_loaded_config(&mut self, id: &LayerId) {
        self.loaded_configs.remove(id);
    }

    pub fn handlers(&self) -> impl Iterator<Item = &dyn OverlayHandler> {
        self.handlers.iter().map(|h| &**h)
    }

    /// The default draw order, bottom to top — every registered handler's id
    /// sorted by [`OverlayHandler::draw_order_weight`].
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

    pub fn handler_by_id(&self, id: &LayerId) -> Option<&dyn OverlayHandler> {
        self.handler(id)
    }

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

    #[doc(hidden)]
    pub fn nws_alerts_payload(alerts: Vec<crate::nws::alert::NwsAlert>) -> FetchPayload {
        Box::new(super::handlers::alert::NwsAlertFetchResult(Ok(
            crate::nws::fetch::ActiveAlerts::whole(alerts),
        )))
    }

    #[doc(hidden)]
    pub fn nws_alerts_partial_payload(
        alerts: Vec<crate::nws::alert::NwsAlert>,
        zones: crate::nws::zones::ZoneResolution,
    ) -> FetchPayload {
        Box::new(super::handlers::alert::NwsAlertFetchResult(Ok(
            crate::nws::fetch::ActiveAlerts { alerts, zones },
        )))
    }

    #[doc(hidden)]
    pub fn spc_discussions_payload(
        discussions: Vec<crate::spc::discussion::SpcDiscussion>,
    ) -> FetchPayload {
        Box::new(super::handlers::discussion::SpcDiscussionFetchResult(Ok(
            discussions,
        )))
    }

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
    pub fn clear_retry(&mut self, id: &LayerId) {
        if let Some(r) = self.handler_mut(id).and_then(|h| h.retry_mut()) {
            r.clear();
        }
    }

    /// File a failure against `kind`'s ladder from outside the handler.
    pub fn record_fetch_failure(&mut self, id: &LayerId, error: &FetchError) {
        if let Some(h) = self.handler_mut(id) {
            h.set_fetching(false);
            if let Some(r) = h.retry_mut() {
                r.record_failure(error);
            }
        }
    }

    /// What `kind`'s last fetch said.
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

    pub fn legend(&self, id: &LayerId) -> Option<Signed<OverlayLegend>> {
        self.handler(id).and_then(|h| h.legend())
    }

    /// [`OverlayHandler::theme_sensitive`] for `id`.
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
        self.forget_loaded_config(&id);
        if let Some(idx) = self.handlers.iter().position(|h| h.id() == id) {
            self.handlers[idx].apply_fetch_result(result.data);
            self.handlers[idx].retain_selections(&mut self.selected_overlays);
        }
        if self.selected_overlay_page >= self.selected_overlays.len().max(1) {
            self.selected_overlay_page = 0;
        }
    }

    pub fn prepare_job(&self, id: &LayerId, ctx: &RasterizeContext) -> Option<DescribedJob> {
        self.handler(id).and_then(|h| h.prepare_job(ctx))
    }

    pub fn job_codec(&self, id: &LayerId) -> Option<&'static JobCodec> {
        self.handler(id).and_then(|h| h.job_codec())
    }

    pub fn hit_items(&self, id: &LayerId) -> Option<Vec<Arc<dyn OverlayItem>>> {
        self.handler(id).and_then(|h| h.hit_items())
    }

    pub fn create_fetch_tasks(&self, id: &LayerId, ctx: &FetchConfig) -> Vec<FetchTask> {
        self.handler(id)
            .map_or_else(Vec::new, |h| h.create_fetch_tasks(ctx))
    }

    /// The handler's own options, with its fetch health prepended.
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
    /// bytes these maps have always been keyed by, so every existing config file keeps matching.
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

    fn mds_off(registry: &mut OverlayRegistry) -> std::collections::HashMap<LayerId, Value> {
        registry.set_enabled(&known::SPC_DISCUSSIONS, false);
        let configs = registry.save_pane_configs();
        registry.set_enabled(&known::SPC_DISCUSSIONS, true);
        configs
    }

    use serde_json::Value;

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

        registry.load_pane_configs(&configs);
        assert!(
            !registry.is_enabled(&known::SPC_DISCUSSIONS),
            "a repeat load changed the answer",
        );
    }

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

    #[test]
    fn a_handler_change_outside_the_config_is_still_undone_by_the_next_load() {
        let mut registry = OverlayRegistry::default();
        let configs = mds_off(&mut registry);
        registry.load_pane_configs(&configs);
        assert!(!registry.is_enabled(&known::SPC_DISCUSSIONS));

        registry.set_enabled(&known::SPC_DISCUSSIONS, true);
        registry.load_pane_configs(&configs);
        assert!(
            !registry.is_enabled(&known::SPC_DISCUSSIONS),
            "a `set_enabled` that never reached the config survived the \
             reload — the skip went stale",
        );

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

    #[test]
    fn a_layer_that_under_drew_says_so_on_its_row_and_in_its_options() {
        use crate::nws::zones::{ZoneFailure, ZoneResolution};

        let ctx = PaneControlContext {
            pane_idx: 0,
            pane_state: None,
        };
        let kind = known::NWS_ALERTS;
        let mut registry = OverlayRegistry::default();

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

        assert_eq!(
            registry.status_line(&kind).as_deref(),
            Some("! incomplete - 85 of 297 shown - W/Wa/Adv/Oth"),
            "a layer drawing 85 of 297 warnings must not read as healthy",
        );

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

        registry.clear_retry(&kind);
        assert_eq!(
            registry.status_line(&kind).as_deref(),
            Some("! incomplete - 85 of 297 shown - W/Wa/Adv/Oth"),
            "clearing the retry ladder marked the layer whole before the answer \
             that would make it whole had landed",
        );
    }

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

        state.set_data_with_coverage(vec![1], DataCompleteness::default());
        assert!(
            !state.enable_should_refetch(true),
            "a whole round must not spend a request on being switched on",
        );
    }

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

#[cfg(test)]
mod overlay_kind_stays_deleted_tests {
    const OVERLAY_STATE: &str = include_str!("overlay_state.rs");

    const KIND_DEF: &str = concat!("enum Overlay", "Kind");
    const KIND_NAME: &str = concat!("Overlay", "Kind");
    const REGISTRY_ANCHOR: &str = concat!("pub struct Overlay", "Registry");

    #[test]
    fn the_overlay_kind_enum_stays_deleted() {
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
