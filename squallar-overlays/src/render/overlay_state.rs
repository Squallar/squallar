//! The overlay registry — and the shim layer over the contract it holds.

use std::sync::Arc;

use squallar_source::id::LayerId;
#[cfg(test)]
use squallar_source::id::known;
use squallar_source::job::{DescribedJob, JobCodec};
use squallar_source::product::{FieldId, ProductSpec};
use squallar_source::time::{FrameListing, FrameSource, FrameStamp, Residency};
use squallar_units::UserPreferences;

#[cfg(test)]
use crate::fetch_policy::{Assembled, Whole};
use crate::fetch_policy::{FetchError, FetchHealth, FetchRetry};
use crate::render::controls::{ControlEffect, ControlItem, ControlUpdate};
use crate::render::draw::{DrawPointContext, HoverContext, MapPoint, PointPainter};
use crate::types::OverlayLabel;

pub use squallar_source::handler::{
    ClickableItem, FetchConfig, FetchPayload, FetchTask, FrameListingResult, OverlayFetchResult,
    OverlayItem, OverlayLegend, OverlayState, PaneMut, PaneRef, PaneToggle, PopupAction,
    PopupActionKind, PopupContent, PopupSection, RasterizeContext, RenderMode, Signed, SourceEvent,
    SourceHandler as OverlayHandler, Surface, TaskFuture,
};
pub use squallar_source::hit::{HitItems, HitResolve};

/// What opens a layer-stack status line that is reporting a fault rather than a
/// count — see [`OverlayRegistry::status_line`].
pub const STATUS_MARK: &str = "!";

/// **Bytes of overlay ITEM data installed across every layer on this
/// instance** — the `overlay items` heap-census family.
///
/// Not a fold: a level the install path maintains, so this is a load
/// whatever the scene. Disjoint from
/// [`OverlayRegistry::resident_source_bytes`], which is the gridded layers'
/// own family and prices the same handlers' grids; the two may be read
/// together and never overlap.
pub fn installed_item_bytes() -> u64 {
    squallar_source::footprint::installed_item_bytes()
}

/// **One layer's retired batch, boxed**: the generation its state parked and
/// the rows its memo parked, in one list for the app to file.
///
/// Spelled once rather than in each handler so a layer that gains a second
/// memo adds a chain rather than a shape.
pub(crate) fn retired_batch<M: Send + 'static>(
    parked: Option<Box<dyn std::any::Any + Send>>,
    memo_rows: Vec<M>,
) -> Vec<Box<dyn std::any::Any + Send>> {
    parked
        .into_iter()
        .chain(
            memo_rows
                .into_iter()
                .map(|row| Box::new(row) as Box<dyn std::any::Any + Send>),
        )
        .collect()
}

/// **Bytes of retired item data and built paint inputs waiting on the discard
/// seam** — the `overlay parked` heap-census family. A level, like its
/// neighbour above.
pub fn parked_item_bytes() -> u64 {
    squallar_source::footprint::parked_item_bytes()
        .saturating_add(crate::render::signature_memo::parked_input_bytes())
}

// ── Overlay registry ─────────────────────────────────────────────────────

pub struct OverlayRegistry {
    handlers: Vec<Box<dyn OverlayHandler>>,
    /// Populated by map clicks; paged through in the popup.
    pub selected_overlays: Vec<Arc<dyn OverlayItem>>,
    pub selected_overlay_page: usize,
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
        }
    }

    fn handler(&self, id: &LayerId) -> Option<&dyn OverlayHandler> {
        self.handlers.iter().find(|h| &h.id() == id).map(|h| &**h)
    }

    fn handler_mut(&mut self, id: &LayerId) -> Option<&mut dyn OverlayHandler> {
        for handler in &mut self.handlers {
            if &handler.id() == id {
                return Some(&mut **handler);
            }
        }
        None
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

    /// **The field read contract**: every registered layer's fields, in registry
    /// order, paired with the layer that owns each.
    ///
    /// This is the whole surface the UI needs to build pickers, legends and
    /// catalogue tiles. It hands out [`ProductSpec`] rows — *data* — so a
    /// consumer never matches on which field it has, and a source crate can add
    /// one without an arm anywhere above it.
    pub fn fields(&self) -> impl Iterator<Item = (LayerId, &'static ProductSpec)> + '_ {
        self.handlers()
            .flat_map(|h| h.products().iter().map(move |s| (h.id(), s)))
    }

    /// The field `id` names, with its owning layer, or `None` for an id this
    /// build does not register.
    ///
    /// An id read from a config file need not be one this build has: the
    /// open-id doctrine says an unknown field is preserved inert, not an error,
    /// so this answers `None` rather than refusing.
    pub fn field(&self, id: &FieldId) -> Option<(LayerId, &'static ProductSpec)> {
        self.fields().find(|(_, s)| s.id == *id)
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
    pub fn content_signature(&self, id: &LayerId, pane: &PaneRef<'_>) -> u64 {
        self.handler(id).map_or(0, |h| h.content_signature(pane))
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

    /// Age `kind`'s poll clock — see [`OverlayHandler::rewind_fetch_time`].
    #[doc(hidden)]
    pub fn rewind_fetch_time(&mut self, id: &LayerId, by: std::time::Duration) {
        if let Some(h) = self.handler_mut(id) {
            h.rewind_fetch_time(by);
        }
    }

    pub fn has_data(&self, id: &LayerId, pane: &PaneRef<'_>) -> bool {
        self.handler(id).is_some_and(|h| h.has_data(pane))
    }

    pub fn is_fetching(&self, id: &LayerId) -> bool {
        self.handler(id).is_some_and(|h| h.is_fetching())
    }

    pub fn set_fetching(&mut self, id: &LayerId, fetching: bool, pane: &PaneRef<'_>) {
        if let Some(h) = self.handler_mut(id) {
            h.set_fetching(fetching, pane);
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

    /// A good answer arrived for `kind` from outside the handler: the round
    /// ends, the ladder resets and the layer returns to its interval. The twin
    /// of [`Self::record_fetch_failure`], for a layer whose answer arrives
    /// somewhere other than `apply_fetch_result`.
    pub fn record_fetch_success(&mut self, id: &LayerId, pane: &PaneRef<'_>) {
        if let Some(h) = self.handler_mut(id) {
            h.set_fetching(false, pane);
            if let Some(r) = h.retry_mut() {
                r.record_success();
            }
        }
    }

    /// File a failure against `kind`'s ladder from outside the handler.
    pub fn record_fetch_failure(&mut self, id: &LayerId, error: &FetchError, pane: &PaneRef<'_>) {
        if let Some(h) = self.handler_mut(id) {
            h.set_fetching(false, pane);
            if let Some(r) = h.retry_mut() {
                r.record_failure(error);
            }
        }
    }

    /// What `kind`'s last fetch said.
    pub fn fetch_health(&self, id: &LayerId) -> Option<&FetchHealth> {
        self.handler(id).and_then(|h| h.retry()).map(|r| r.health())
    }

    pub fn item_count(&self, id: &LayerId, pane: &PaneRef<'_>) -> usize {
        self.handler(id).map_or(0, |h| h.item_count(pane))
    }

    pub fn is_enabled(&self, id: &LayerId, pane: &PaneRef<'_>) -> bool {
        self.handler(id).is_some_and(|h| h.is_enabled(pane))
    }

    pub fn set_enabled(&mut self, id: &LayerId, enabled: bool, pane: &mut PaneMut<'_>) {
        if let Some(h) = self.handler_mut(id) {
            h.set_enabled(enabled, pane);
        }
    }

    /// [`OverlayHandler::status_line`] for `kind`, marked when the layer is not
    /// updating; `None` for a kind with no handler.
    pub fn status_line(&self, id: &LayerId, pane: &PaneRef<'_>) -> Option<String> {
        let handler = self.handler(id)?;
        let line = handler.status_line(pane);
        if !handler.is_enabled(pane) {
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

    pub fn clickable_items<'a>(
        &'a self,
        id: &LayerId,
        pane: &PaneRef<'_>,
    ) -> Vec<ClickableItem<'a>> {
        self.handler(id)
            .map_or_else(Vec::new, |h| h.clickable_items(pane))
    }

    /// [`OverlayHandler::map_labels`] for `kind`; empty for a kind with no
    /// handler.
    pub fn map_labels(&self, id: &LayerId) -> &[OverlayLabel] {
        self.handler(id).map_or(&[], |h| h.map_labels())
    }

    pub fn hover_value_at(
        &self,
        id: &LayerId,
        lat: f64,
        lon: f64,
        pane: &PaneRef<'_>,
    ) -> Option<String> {
        self.handler(id)
            .and_then(|h| h.hover_value_at(lat, lon, pane))
    }

    pub fn legend(&self, id: &LayerId, pane: &PaneRef<'_>) -> Option<Signed<OverlayLegend>> {
        self.handler(id).and_then(|h| h.legend(pane))
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
    ///
    /// `pane` is a [`PaneRef::across`]: an arrival names a layer and no pane,
    /// and the two questions a handler asks of it — what is still being asked
    /// for, what the shared cache must not evict — are about every pane at
    /// once. Both are answered by the **union** through
    /// [`PaneRef::all_as`]; `selected_overlays` is one global list, so a
    /// `retain_selections` that ever starts filtering by pane must keep what
    /// any pane keeps.
    pub fn apply_fetch_result(&mut self, result: OverlayFetchResult, pane: &PaneRef<'_>) {
        let id = result.kind;
        if let Some(idx) = self.handlers.iter().position(|h| h.id() == id) {
            self.handlers[idx].apply_fetch_result(result.data, pane);
            self.handlers[idx].retain_selections(&mut self.selected_overlays, pane);
        }
        if self.selected_overlay_page >= self.selected_overlays.len().max(1) {
            self.selected_overlay_page = 0;
        }
    }

    /// **A frame listing's arrival**, routed to the layer that asked for it.
    ///
    /// The listing half of [`SourceEvent`]. **What identifies this delivery is
    /// the `scope`, not the pane** — the handler captured it at dispatch and
    /// files the listing under it, which is what lets two panes on two sites
    /// keep two frame sets when the listing itself names neither. `pane` is
    /// still the [`PaneRef::across`] union the other two arrivals take, and a
    /// handler must not read a site out of it: the union's config is null by
    /// construction.
    pub fn apply_frames(
        &mut self,
        id: &LayerId,
        listing: FrameListing,
        scope: FetchPayload,
        pane: &PaneRef<'_>,
    ) {
        if let Some(frames) = self.handler_mut(id).and_then(|h| h.frames_mut()) {
            frames.apply_frame_listing(listing, scope, pane);
        }
    }

    /// **One frame's data arriving**, routed to the layer that asked for it.
    /// The other half of [`Self::apply_frames`], and dark for the same reason.
    pub fn apply_frame(
        &mut self,
        id: &LayerId,
        stamp: FrameStamp,
        data: FetchPayload,
        pane: &PaneRef<'_>,
    ) {
        if let Some(frames) = self.handler_mut(id).and_then(|h| h.frames_mut()) {
            frames.apply_frame(stamp, data, pane);
        }
    }

    pub fn prepare_job(
        &self,
        id: &LayerId,
        ctx: &RasterizeContext,
        pane: &PaneRef<'_>,
    ) -> Option<DescribedJob> {
        self.handler(id).and_then(|h| h.prepare_job(ctx, pane))
    }

    pub fn job_codec(&self, id: &LayerId) -> Option<&'static JobCodec> {
        self.handler(id).and_then(|h| h.job_codec())
    }

    pub fn hit_items(&self, id: &LayerId) -> Option<HitItems> {
        self.handler(id).and_then(|h| h.hit_items())
    }

    pub fn create_fetch_tasks(
        &self,
        id: &LayerId,
        ctx: &FetchConfig,
        pane: &PaneRef<'_>,
    ) -> Vec<FetchTask> {
        self.handler(id)
            .map_or_else(Vec::new, |h| h.create_fetch_tasks(ctx, pane))
    }

    /// [`SourceHandler::residency_for`] for `id` — what that layer must be
    /// holding to draw `stops`, or [`Residency::none`] for an id this build
    /// does not register.
    ///
    /// **Not on the frame forwarder above**, and the same reason the trait
    /// method is not on [`FrameSource`]: the layer this question was written
    /// for is [`TimeAxis::EventLifetime`] and has no frames at all. An
    /// unregistered id and a [`TimeAxis::Live`] layer answer the same empty
    /// [`Residency`], which is the correct answer for both — neither is
    /// obliged to hold any slice of source time by any set of stops.
    ///
    /// [`SourceHandler::residency_for`]: squallar_source::handler::SourceHandler::residency_for
    /// [`TimeAxis::EventLifetime`]: squallar_source::time::TimeAxis::EventLifetime
    /// [`TimeAxis::Live`]: squallar_source::time::TimeAxis::Live
    pub fn residency_for(
        &self,
        id: &LayerId,
        pane: &PaneRef<'_>,
        stops: &[chrono::NaiveDateTime],
    ) -> Residency {
        self.handler(id)
            .map_or_else(Residency::none, |h| h.residency_for(pane, stops))
    }

    /// **This layer's frame supply**, or `None` for an id this build does not
    /// register **and** for one whose layer does not come in stamped frames.
    ///
    /// The two are one answer on purpose: everything below routes through this
    /// and treats both as "no frames", which is what makes a frame question
    /// safe to ask of any id.
    pub fn frames(&self, id: &LayerId) -> Option<&dyn FrameSource> {
        self.handler(id).and_then(|h| h.frames())
    }

    /// [`FrameSource::latest_at`] for `id` — what this layer would draw at
    /// `t`. `None` from a layer with no frames, and `None` from a framed layer
    /// that knows of none at or before `t`.
    pub fn latest_at(
        &self,
        id: &LayerId,
        pane: &PaneRef<'_>,
        t: chrono::NaiveDateTime,
    ) -> Option<FrameStamp> {
        self.frames(id)?.latest_at(pane, t)
    }

    /// [`FrameSource::list_frames`] for `id` — the synchronous read over
    /// what that layer already knows, never a fetch. A layer with no frame
    /// supply knows nothing, which is [`FrameListing::empty`].
    pub fn list_frames(
        &self,
        id: &LayerId,
        ctx: &FetchConfig,
        pane: &PaneRef<'_>,
        range: (chrono::NaiveDateTime, chrono::NaiveDateTime),
    ) -> FrameListing {
        self.frames(id).map_or_else(
            || FrameListing::empty(range),
            |f| f.list_frames(ctx, pane, range),
        )
    }

    /// [`FrameSource::frames_resident`] for `id` — which of this layer's
    /// frames the pane already holds data for. A layer with no frame supply
    /// holds nothing, and that is not the same statement as "nothing is
    /// coming": the caller that turns this into a settle verdict pairs it with
    /// the layer's own arrival question.
    pub fn frames_resident(&self, id: &LayerId, pane: &PaneRef<'_>) -> Vec<FrameStamp> {
        self.frames(id)
            .map_or_else(Vec::new, |f| f.frames_resident(pane))
    }

    /// [`FrameSource::retain_frames`] for `id` — the one eviction door into a
    /// layer's own frame store.
    ///
    /// **No production caller yet**, and that is a stated open item rather than
    /// an oversight: non-radar frame residency is still governed by the byte
    /// budget inside each handler, which knows nothing about the pane's window.
    /// Wiring the window to this door is its own work order. The forwarder
    /// exists because the door does: a trait method the registry cannot reach
    /// is a door with no corridor to it.
    pub fn retain_frames(&mut self, id: &LayerId, pane: &PaneRef<'_>, keep: &[FrameStamp]) {
        if let Some(frames) = self.handler_mut(id).and_then(|h| h.frames_mut()) {
            frames.retain_frames(pane, keep);
        }
    }

    /// [`FrameSource::create_frame_list_task`] for `id`.
    pub fn create_frame_list_task(
        &self,
        id: &LayerId,
        ctx: &FetchConfig,
        pane: &PaneRef<'_>,
        range: (chrono::NaiveDateTime, chrono::NaiveDateTime),
    ) -> Option<FetchTask> {
        self.frames(id)
            .and_then(|f| f.create_frame_list_task(ctx, pane, range))
    }

    /// [`FrameSource::fetch_frame`] for `id`.
    pub fn fetch_frame(
        &self,
        id: &LayerId,
        ctx: &FetchConfig,
        pane: &PaneRef<'_>,
        stamp: &FrameStamp,
    ) -> Option<FetchTask> {
        self.frames(id)
            .and_then(|f| f.fetch_frame(ctx, pane, stamp))
    }

    /// The handler's own options, with its fetch health prepended.
    pub fn controls(&self, id: &LayerId, pane: &PaneRef<'_>) -> Vec<ControlItem> {
        let Some(handler) = self.handler(id) else {
            return Vec::new();
        };
        let mut items = handler.controls(pane);
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
        pane: &mut PaneMut<'_>,
    ) -> ControlEffect {
        if let Some(h) = self.handler_mut(id) {
            h.apply_control(update, pane)
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
    pub fn build_enabled_map(
        &self,
        pane: &PaneRef<'_>,
    ) -> std::collections::HashMap<LayerId, bool> {
        self.handlers
            .iter()
            .map(|h| (h.id(), h.is_enabled(pane)))
            .collect()
    }

    /// **A pane's live state for `id`** — the handler's saved config decoded,
    /// or a fresh one when the pane has nothing saved. `None` from a handler
    /// that keeps no per-pane state, which is what every handler answered
    /// before WO-M10.
    pub fn create_pane_state(
        &self,
        id: &LayerId,
        config: &serde_json::Value,
        enabled: bool,
    ) -> Option<FetchPayload> {
        let handler = self.handler(id)?;
        if !config.is_null()
            && let Some(state) = handler.deserialize_pane_state(config.clone(), enabled)
        {
            return Some(state);
        }
        handler.create_pane_state(enabled)
    }

    /// `id`'s per-pane state, back as the JSON its slot persists.
    pub fn serialize_pane_state(
        &self,
        id: &LayerId,
        state: &dyn std::any::Any,
    ) -> serde_json::Value {
        self.handler(id)
            .map_or(serde_json::Value::Null, |h| h.serialize_pane_state(state))
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

    /// **Bytes of decoded SOURCE data every registered layer is holding on
    /// this instance's heap**, summed —
    /// **Every layer's retired batch**, for the app's discard seam.
    ///
    /// Called once a frame, and it MUST be a per-frame path: a layer's state
    /// retires at delivery and its memos retire at dispatch, so a drain
    /// hanging off either one alone finds the other empty every time. See
    /// [`OverlayHandler::take_retired`].
    pub fn take_retired(&self) -> Vec<Box<dyn std::any::Any + Send>> {
        self.handlers
            .iter()
            .flat_map(|handler| handler.take_retired())
            .collect()
    }

    /// [`OverlayHandler::resident_source_bytes`] over the handlers.
    ///
    /// The three gridded layers are the bulk of it: MRMS at 49 MB a mosaic,
    /// GMGSI at 15 MB a blend — one byte a point, the width its values are —
    /// HRRR at 7.6 MB a grid. **And one that is not gridded**: the lightning
    /// layer's S3 granule cache, up to `MAX_RETAINED_FLASHES` rows at 48 bytes
    /// apiece — 12 MB — held beside its `OverlayState` and therefore in no
    /// other family. Every other handler takes the trait's `0` default, which
    /// is a claim about scale rather than an omission — a few hundred parsed
    /// alert polygons do not move a figure read in megabytes.
    ///
    /// **What this is not**: the pictures rasterized from these grids (the
    /// overlay picture batch prices those) and the textures those pictures
    /// became (the GPU's). Nothing here is a worker instance's; the page's
    /// handlers are the page's.
    ///
    /// **Cost**: O(registered handlers), and each answer is a field read or a
    /// walk of a cache holding at most a handful of entries — those byte
    /// budgets are two or four grids of 15-49 MB. No grid contents are
    /// touched, and **no answer takes a lock**: the lightning cache sits
    /// behind one a poll holds, and it answers off an atomic level maintained
    /// beside the map rather than by trying for the lock and reporting a false
    /// zero when it misses. So this is safe on the frame thread's telemetry
    /// tick.
    pub fn resident_source_bytes(&self) -> u64 {
        self.handlers()
            .map(OverlayHandler::resident_source_bytes)
            .fold(0u64, u64::saturating_add)
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
        for h in &mut self.handlers {
            if let Some(val) = states.get(h.id().as_str()) {
                h.deserialize_state(val.clone());
            }
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
            checked, 8,
            "the eight auto-polling handlers must all still be covered; a new \
             one is not exempt, and a removed one should be removed from this \
             count deliberately",
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
        use crate::render::controls::ControlItem;

        let ctx = PaneRef::bare(0);
        let mut registry = OverlayRegistry::default();
        let kinds: Vec<LayerId> = registry
            .handlers()
            .filter(|h| h.retry().is_some())
            .map(|h| h.id())
            .collect();
        assert_eq!(
            kinds.len(),
            10,
            "the ten fetching handlers must all be covered",
        );

        for kind in kinds {
            let quiet = registry.controls(&kind, &ctx).len();

            registry.record_fetch_failure(
                &kind,
                &FetchError::transient("connection refused"),
                &PaneRef::bare(0),
            );
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
            registry.set_enabled(&kind, true, &mut PaneMut::bare(0));
            let healthy = registry.status_line(&kind, &PaneRef::bare(0));
            assert!(
                !healthy
                    .as_deref()
                    .is_some_and(|l| l.contains("not updating")),
                "{kind:?} claims to be failing before anything has failed: {healthy:?}",
            );

            registry.record_fetch_failure(
                &kind,
                &FetchError::transient("connection refused"),
                &PaneRef::bare(0),
            );
            let marked = registry
                .status_line(&kind, &PaneRef::bare(0))
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
                registry.status_line(&kind, &PaneRef::bare(0)),
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
                valid_from: None,
                valid_until: None,
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

        let ctx = PaneRef::bare(0);
        let kind = known::NWS_ALERTS;
        let mut registry = OverlayRegistry::default();

        registry.apply_fetch_result(
            OverlayFetchResult {
                kind: kind.clone(),
                data: OverlayRegistry::nws_alerts_payload(alerts_where_only_some_resolved(
                    297, 297,
                )),
            },
            &PaneRef::bare(0),
        );
        assert_eq!(
            registry.status_line(&kind, &PaneRef::bare(0)).as_deref(),
            Some("297 shown - W/Wa/Adv/Oth"),
            "a whole round must read as a plain count",
        );
        let quiet = registry.controls(&kind, &ctx).len();

        registry.apply_fetch_result(
            OverlayFetchResult {
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
            },
            &PaneRef::bare(0),
        );

        assert_eq!(
            registry.status_line(&kind, &PaneRef::bare(0)).as_deref(),
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

        registry.apply_fetch_result(
            OverlayFetchResult {
                kind: kind.clone(),
                data: OverlayRegistry::nws_alerts_payload(alerts_where_only_some_resolved(
                    297, 297,
                )),
            },
            &PaneRef::bare(0),
        );
        assert_eq!(
            registry.status_line(&kind, &PaneRef::bare(0)).as_deref(),
            Some("297 shown - W/Wa/Adv/Oth"),
            "the mark outlived the round it was about",
        );
    }

    #[test]
    fn a_layer_that_is_both_stale_and_incomplete_says_both() {
        use crate::nws::zones::{ZoneFailure, ZoneResolution};

        let kind = known::NWS_ALERTS;
        let mut registry = OverlayRegistry::default();
        registry.apply_fetch_result(
            OverlayFetchResult {
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
            },
            &PaneRef::bare(0),
        );
        assert_eq!(
            registry.status_line(&kind, &PaneRef::bare(0)).as_deref(),
            Some("! incomplete - 85 of 297 shown - W/Wa/Adv/Oth"),
        );

        registry.record_fetch_failure(
            &kind,
            &FetchError::transient("connection refused"),
            &PaneRef::bare(0),
        );
        assert_eq!(
            registry.status_line(&kind, &PaneRef::bare(0)).as_deref(),
            Some("! not updating, incomplete - 85 of 297 shown - W/Wa/Adv/Oth"),
            "a failure must not overwrite the coverage verdict, or the reverse",
        );

        let ctx = PaneRef::bare(0);
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
            registry.status_line(&kind, &PaneRef::bare(0)).as_deref(),
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
        registry.record_fetch_failure(
            &kind,
            &FetchError::transient("connection refused"),
            &PaneRef::bare(0),
        );
        registry.set_enabled(&kind, false, &mut PaneMut::bare(0));
        assert!(
            !registry
                .status_line(&kind, &PaneRef::bare(0))
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
