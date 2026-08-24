use crate::render::overlay_state::{PaneMut, PaneRef};
use std::any::Any;
use std::collections::HashSet;
use std::sync::Arc;

use crate::fetch_policy::Assembled;
use crate::nws::alert::{AlertCategory, NwsAlert};
use crate::render::controls::{
    ControlButton, ControlEffect, ControlItem, ControlUpdate, ControlValue,
};
use crate::render::overlay_state::Surface;
use crate::render::overlay_state::{
    ClickableItem, FetchConfig, FetchPayload, FetchTask, OverlayHandler, OverlayItem, OverlayState,
    PopupAction, PopupActionKind, PopupContent, PopupSection, RasterizeContext, RenderMode,
};
use crate::render::rasterize;
use squallar_source::id::{LayerId, known};
use squallar_source::job::{DescribedJob, JobCodec};
use squallar_source::time::TimeAxis;

/// `pub`, not `pub(crate)`: `squallar-app`'s described-job dispatch tests
/// construct this type directly.
/// How far behind the wall clock a pane's instant must be before its warnings
/// are fetched from the archive rather than the live feed.
///
/// Generous on purpose. A live pane's `as_of` is the frame's clock and lags by
/// seconds to minutes; a pane genuinely parked in the past is parked by hours at
/// least. Anything in between is served by the live feed, which is the safer
/// wrong answer of the two -- it carries watches and advisories that the
/// storm-based-warning archive does not.
pub(crate) const ARCHIVE_CUTOFF_MINUTES: i64 = 30;

pub struct NwsAlertFetchResult(
    pub Result<crate::nws::fetch::ActiveAlerts, crate::fetch_policy::FetchError>,
);

/// [`Assembled`]: the national alert feed is one request, and the UGC zone
/// boundaries most alerts reference are one request **each** — a thousand or
/// more on a busy day. Observed before this: **212 of 297 warnings absent from
/// the map under a fully green status line.**
/// [`Assembled`]: crate::fetch_policy::Assembled
impl crate::fetch_policy::FetchRound for NwsAlertFetchResult {
    type Shape = crate::fetch_policy::Assembled;
}

/// How long an item outlives the **earlier** of its `valid_until` and its
/// departure from the feed, in hours, before eviction (WB-4): the depth of
/// backward scrub over expired alerts. A full convective day covers any
/// within-session scrub the timeline offers while bounding the retained set
/// to roughly one day of national issuance. The eviction clock is the wall
/// clock at each poll, never `as_of`.
const RETAINED_HOURS: i64 = 24;

#[derive(Debug)]
pub(crate) struct AlertItem {
    pub alert: NwsAlert,
    /// When this alert left the active feed — the wall clock of the first
    /// poll that no longer carried it — or `None` while the feed still does
    /// (WB-4). The closest observable stand-in for a cancellation time.
    ///
    /// **A departed item is history**: it draws only at a depicted instant
    /// before its departure, and every other surface — counts, clicks,
    /// signature, status line — sees the active feed exactly as wholesale
    /// replacement showed it. That containment is what keeps a live pane
    /// byte-identical while retention exists.
    pub departed: Option<chrono::NaiveDateTime>,
}

impl OverlayItem for AlertItem {
    fn layer_id(&self) -> LayerId {
        known::NWS_ALERTS
    }

    fn popup_content(&self, prefs: &squallar_units::UserPreferences) -> PopupContent {
        let alert = &self.alert;
        let [r, g, b, _] = alert
            .features
            .first()
            .map(|f| f.stroke_rgba)
            .unwrap_or([200, 200, 200, 255]);

        let mut sections = Vec::new();

        if let Some(headline) = &alert.headline {
            sections.push(PopupSection::Heading(headline.clone()));
        }

        let mut grid = vec![
            ("Areas".into(), alert.area_desc.clone()),
            ("Issued by".into(), alert.sender_name.clone()),
            (
                "Effective".into(),
                prefs.timezone.format_rfc3339(&alert.effective),
            ),
            (
                "Expires".into(),
                prefs.timezone.format_rfc3339(&alert.expires),
            ),
            // The CAP triple. `Debug` is the variant name — the CAP vocabulary
            // itself — and an unrecognised value honestly reads "Unknown".
            ("Severity".into(), format!("{:?}", alert.severity)),
            ("Urgency".into(), format!("{:?}", alert.urgency)),
            ("Certainty".into(), format!("{:?}", alert.certainty)),
        ];
        // Onset and ends are optional in the feed; a row is added only where
        // the alert carries one.
        if let Some(onset) = &alert.onset {
            grid.push(("Onset".into(), prefs.timezone.format_rfc3339(onset)));
        }
        if let Some(ends) = &alert.ends {
            grid.push(("Ends".into(), prefs.timezone.format_rfc3339(ends)));
        }
        sections.push(PopupSection::KeyValueGrid(grid));

        sections.push(PopupSection::Separator);

        sections.push(PopupSection::ScrollableText {
            text: alert.description.clone(),
            monospace: false,
            max_height: 250.0,
        });

        if let Some(instruction) = &alert.instruction {
            sections.push(PopupSection::Separator);
            sections.push(PopupSection::ColoredText {
                text: instruction.clone(),
                rgb: [r, g, b],
                bold: true,
            });
        }

        PopupContent {
            title: alert.event.clone(),
            accent_rgb: [r, g, b],
            width: 380.0,
            sections,
            actions: vec![PopupAction {
                label: "Hide from map".into(),
                target: Arc::new(AlertItem {
                    alert: alert.clone(),
                    departed: self.departed,
                }),
                kind: PopupActionKind::HideFromMap,
            }],
        }
    }

    fn matches(&self, other: &dyn OverlayItem) -> bool {
        other
            .as_any()
            .downcast_ref::<AlertItem>()
            .is_some_and(|o| o.alert.id == self.alert.id)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// **The whole per-pane state of the alerts layer**: which categories this
/// pane lets through. There is no `enabled` beside it — for this layer "on"
/// **is** a non-empty set, and a bool next to the set is a second copy free to
/// disagree with the thing it was derived from.
///
/// `hidden_alerts` is deliberately NOT here: dismissing an alert is a
/// statement about the alert, not about one pane's view of it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AlertPaneState {
    pub enabled_categories: HashSet<AlertCategory>,
}

impl AlertPaneState {
    /// A pane that has saved nothing. `enabled` is the pane's own slot flag,
    /// and for this layer it means "all categories" or "none" — the same two
    /// answers `set_enabled` gives.
    fn new(enabled: bool) -> Self {
        Self {
            enabled_categories: if enabled {
                AlertCategory::ALL.into_iter().collect()
            } else {
                HashSet::new()
            },
        }
    }
}

pub(crate) struct NwsAlertHandler {
    pub state: OverlayState<Vec<Arc<AlertItem>>, Assembled>,
    /// User-dismissed alert IDs, pruned on refetch.
    pub hidden_alerts: HashSet<String>,
    /// **The registry's own copy**, used only where no pane is supplied. The
    /// config swap keeps it in step until WO-M10c deletes the swap; every
    /// answer prefers [`PaneRef::state`] when there is one.
    pub defaults: AlertPaneState,
}

impl NwsAlertHandler {
    pub fn new() -> Self {
        Self {
            state: OverlayState::new(),
            hidden_alerts: HashSet::new(),
            defaults: AlertPaneState::new(true),
        }
    }

    /// **This pane's answer, or the registry's own copy** when no pane was
    /// supplied.
    fn view<'a>(&'a self, pane: &PaneRef<'a>) -> &'a AlertPaneState {
        pane.state_as::<AlertPaneState>().unwrap_or(&self.defaults)
    }

    /// Edit this pane's state, falling back to the registry's copy for a
    /// caller that supplied no pane.
    fn edit(&mut self, pane: &mut PaneMut<'_>, f: impl FnOnce(&mut AlertPaneState)) {
        match pane.state_as::<AlertPaneState>() {
            Some(state) => f(state),
            None => f(&mut self.defaults),
        }
    }

    /// Whether this alert would paint **in this pane**: in the active feed,
    /// category on there and not hidden anywhere. The one filter, so count,
    /// signature, status line and clickable set cannot drift.
    ///
    /// `departed.is_none()` first: a retained item that left the feed exists
    /// only for the as-of paint path ([`Self::paint_input`]) and must be
    /// invisible to every surface this predicate feeds.
    fn is_drawn(&self, view: &AlertPaneState, item: &AlertItem) -> bool {
        item.departed.is_none()
            && view.enabled_categories.contains(&item.alert.category)
            && !self.hidden_alerts.contains(&item.alert.id)
    }

    /// How many alerts the user's filters let through. Not the same as
    /// [`painted_count`](NwsAlertHandler::painted_count): an alert whose zone
    /// boundaries did not resolve passes every filter and paints nothing.
    fn drawn_count(&self, view: &AlertPaneState) -> usize {
        self.state
            .data
            .iter()
            .filter(|item| self.is_drawn(view, item))
            .count()
    }

    /// How many alerts actually put ink on the map: let through *and* holding
    /// geometry. A zone-based alert whose boundaries all failed is `is_drawn`
    /// and paints nothing.
    fn painted_count(&self, view: &AlertPaneState) -> usize {
        self.state
            .data
            .iter()
            .filter(|item| self.is_drawn(view, item) && !item.alert.features.is_empty())
            .count()
    }

    /// What the rasterizer reads, captured once. The rows are
    /// [`rasterize::AlertPaint`], not whole [`NwsAlert`]s: prose fields the
    /// raster never draws would be bytes on the wire per raster.
    ///
    /// **Rows outside the depicted instant do not travel.** This is the
    /// layer's [`TimeAxis::EventLifetime`] arm: the picture is which alerts
    /// are valid at `ctx.as_of`, and on a live pane `as_of` is the wall clock,
    /// so the bytes are the bytes this layer has always sent.
    fn paint_input(
        &self,
        ctx: &RasterizeContext,
        view: &AlertPaneState,
    ) -> Option<rasterize::AlertsInput> {
        if self.state.data.is_empty() {
            return None;
        }
        Some(rasterize::AlertsInput {
            alerts: self
                .state
                .data
                .iter()
                // **The as-of filter, and the whole of it**: two `Option`
                // comparisons against fields parsed once at fetch time. An
                // alert unbounded on a side passes on that side. Nothing is
                // parsed, formatted or allocated here.
                .filter(|i| {
                    i.alert.valid_from.is_none_or(|from| from <= ctx.as_of)
                        && i.alert.valid_until.is_none_or(|until| ctx.as_of < until)
                        // A departed item is history (WB-4): visible only
                        // before the poll that lost it. On a live pane
                        // `as_of` is the wall clock, which is never before a
                        // past departure — so the live rows are exactly the
                        // active feed's rows and the bytes are unchanged.
                        && i.departed.is_none_or(|gone| ctx.as_of < gone)
                })
                .map(|i| rasterize::AlertPaint {
                    id: i.alert.id.clone(),
                    category: i.alert.category,
                    features: Arc::clone(&i.alert.features),
                })
                .collect(),
            // **In `ALL`'s declaration order, never the `HashSet`'s.** The
            // set round-trips either way, but a `HashSet`'s iteration order
            // is seeded per process, so iterating it raw made one picture
            // encode to different bytes on every run — measured on
            // main@62289151: four runs of one fixture, four digests, one
            // length. The codec sorts `hidden_ids` for exactly this reason
            // and says so; this is the same rule on the field the codec
            // cannot sort, because by then it is a `Vec` whose order is
            // already the answer.
            enabled_categories: AlertCategory::ALL
                .into_iter()
                .filter(|category| view.enabled_categories.contains(category))
                .collect(),
            hidden_ids: self.hidden_alerts.clone(),
            device_scale: ctx.device_scale,
        })
    }

    /// **The one encoder** for a category set, so the registry's copy and a
    /// pane's cannot write different bytes for the same set.
    ///
    /// In `ALL`'s declaration order, never the `HashSet`'s: a set's iteration
    /// order is per-instance noise, so writing it raw makes save→load→save
    /// produce a *different file* every reopen. `known_categories` records
    /// which categories *this build offered a toggle for*, so the decoder can
    /// tell "the user turned this off" apart from "the build that saved this
    /// had no way to turn it on".
    fn save_categories(enabled: &HashSet<AlertCategory>) -> serde_json::Value {
        let ordered: Vec<AlertCategory> = AlertCategory::ALL
            .into_iter()
            .filter(|category| enabled.contains(category))
            .collect();
        serde_json::json!({
            "enabled_categories": ordered,
            "known_categories": AlertCategory::ALL,
        })
    }

    /// **The one decoder**, the exact inverse of [`Self::save_categories`].
    /// A value that names no set at all leaves `into` as it was.
    fn restore_categories(into: &mut HashSet<AlertCategory>, value: &serde_json::Value) {
        let Some(cats) = value
            .get("enabled_categories")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
        else {
            return;
        };
        *into = cats;
        let known: HashSet<AlertCategory> = value
            .get("known_categories")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        if !into.is_empty() {
            into.extend(
                AlertCategory::ALL
                    .into_iter()
                    .filter(|c| !known.contains(c)),
            );
        }
    }
}

impl OverlayHandler for NwsAlertHandler {
    fn id(&self) -> LayerId {
        known::NWS_ALERTS
    }
    fn surface(&self) -> Surface {
        Surface::Ground
    }
    fn draw_order_weight(&self) -> u32 {
        50
    }

    fn display_name(&self) -> &str {
        "NWS Alerts"
    }

    fn render_mode(&self) -> RenderMode {
        RenderMode::Texture
    }

    /// Every alert carries a validity window, and the picture is which of them
    /// are true at the depicted instant — the definition of
    /// [`TimeAxis::EventLifetime`]. The as-of filter is in
    /// [`Self::paint_input`]; the quantum is the trait's 60 s default, which
    /// is the resolution NWS lifetimes are actually published at. Backward
    /// reach is session-held retention, not an archive: [`AlertItem::departed`]
    /// keeps an alert that left the feed drawable at instants inside its
    /// window, for `RETAINED_HOURS` past its end.
    fn time_axis(&self) -> TimeAxis {
        TimeAxis::EventLifetime
    }

    /// **One instant per stop, and that is the whole ask.**
    ///
    /// This layer's items carry their own validity windows, and the picture
    /// at a stop is which of them are in force *then*. Nothing about a stop
    /// obliges the layer to hold a *stretch* of source time the way
    /// lightning's fade ramp does — an alert issued yesterday and an alert
    /// issued a minute ago are both drawn at a stop inside their windows, and
    /// neither is found by reaching further back from it.
    ///
    /// So a caller turning this into a retention rule keeps every alert whose
    /// window **overlaps** a range here, which for `RETAINED_HOURS` of
    /// departed alerts is exactly the set [`AlertItem::departed`] already
    /// preserves. A zero-width range is a real ask, not an empty one:
    /// `Residency::is_empty` is false and `covers(stop)` is true.
    fn residency_for(
        &self,
        _pane: &PaneRef<'_>,
        stops: &[chrono::NaiveDateTime],
    ) -> squallar_source::time::Residency {
        squallar_source::time::Residency::over(stops.iter().map(|&stop| (stop, stop)))
    }

    fn default_enabled(&self) -> bool {
        true
    }

    fn is_enabled(&self, pane: &PaneRef<'_>) -> bool {
        !self.view(pane).enabled_categories.is_empty()
    }

    /// The master toggle over a layer whose "enabled" is really a category set.
    /// Off clears the set; on restores [`AlertCategory::ALL`] **only when the
    /// set is empty**, so flipping the master off and on loses the user's subset.
    fn set_enabled(&mut self, enabled: bool, pane: &mut PaneMut<'_>) {
        let was = self.is_enabled(&pane.as_ref());
        self.edit(pane, |state| {
            if enabled {
                if state.enabled_categories.is_empty() {
                    state.enabled_categories.extend(AlertCategory::ALL);
                }
            } else {
                state.enabled_categories.clear();
            }
        });
        // The drawn set changed, so cached textures must know.
        if was != self.is_enabled(&pane.as_ref()) {
            self.state.data_generation = self.state.data_generation.wrapping_add(1);
        }
    }

    /// E.g. `"3 shown - W/Wa/Adv/Oth"`. **`"85 of 297 shown"`** when
    /// [`painted_count`] and [`drawn_count`] disagree, which is the honest
    /// reading of a poll whose zone boundaries did not all resolve.
    ///
    /// [`drawn_count`]: NwsAlertHandler::drawn_count
    /// [`painted_count`]: NwsAlertHandler::painted_count
    fn status_line(&self, pane: &PaneRef<'_>) -> Option<String> {
        let view = self.view(pane);
        if view.enabled_categories.is_empty() {
            return None;
        }
        let allowed = self.drawn_count(view);
        let painted = self.painted_count(view);
        let shown = if painted == allowed {
            format!("{allowed}")
        } else {
            format!("{painted} of {allowed}")
        };
        // Walked from `AlertCategory::ALL`, not from a list spelled out here.
        let cats: Vec<&str> = AlertCategory::ALL
            .into_iter()
            .filter(|category| view.enabled_categories.contains(category))
            .map(AlertCategory::short_name)
            .collect();
        Some(format!("{shown} shown - {}", cats.join("/")))
    }

    fn data_generation(&self) -> u64 {
        self.state.data_generation
    }

    /// The **warning-set** signature: a fold over the alerts that would draw,
    /// rather than the fetch counter — NWS alerts poll every two minutes and the
    /// active set is usually unchanged. XOR of per-alert hashes, so it is
    /// order-free the way a set is.
    ///
    /// `features.len()` is in the fold as well as the id: a **zone-based** alert
    /// legitimately draws nothing on one poll and its counties on the next under
    /// the same id, so an id-only fold left the warning invisible.
    fn content_signature(&self, pane: &PaneRef<'_>) -> u64 {
        use std::hash::{DefaultHasher, Hash, Hasher};
        let view = self.view(pane);
        let mut folded = 0u64;
        let mut visible = 0u64;
        for item in &self.state.data {
            if self.is_drawn(view, item) {
                let mut hasher = DefaultHasher::new();
                item.alert.id.hash(&mut hasher);
                item.alert.features.len().hash(&mut hasher);
                folded ^= hasher.finish();
                visible += 1;
            }
        }
        folded ^ visible.rotate_left(32)
    }

    fn has_data(&self, _pane: &PaneRef<'_>) -> bool {
        !self.state.data.is_empty()
    }

    fn is_fetching(&self) -> bool {
        self.state.fetching
    }

    fn set_fetching(&mut self, fetching: bool, _pane: &PaneRef<'_>) {
        self.state.fetching = fetching;
    }

    fn retry(&self) -> Option<&crate::fetch_policy::FetchRetry> {
        Some(&self.state.retry)
    }

    fn retry_mut(&mut self) -> Option<&mut crate::fetch_policy::FetchRetry> {
        Some(&mut self.state.retry)
    }

    fn fetch_time(&self) -> Option<web_time::Instant> {
        self.state.fetch_time
    }

    fn auto_poll_interval(&self) -> Option<u64> {
        Some(120)
    }

    fn item_count(&self, _pane: &PaneRef<'_>) -> usize {
        // The active feed only: retained history (WB-4) is invisible to
        // every surface but an as-of paint.
        self.state
            .data
            .iter()
            .filter(|i| i.departed.is_none())
            .count()
    }

    /// The alerts a click can land on, each borrowing its own polygons —
    /// thousands of rings on an active day, lent and never copied.
    fn clickable_items<'a>(&'a self, pane: &PaneRef<'_>) -> Vec<ClickableItem<'a>> {
        let view = self.view(pane);
        self.state
            .data
            .iter()
            .filter(|item| self.is_drawn(view, item))
            .map(|item| ClickableItem {
                features: &item.alert.features,
                item: item.clone() as Arc<dyn OverlayItem>,
            })
            .collect()
    }

    fn handle_popup_action(&mut self, action: &PopupAction) -> bool {
        match action.kind {
            PopupActionKind::HideFromMap => {
                if let Some(alert_item) = action.target.as_any().downcast_ref::<AlertItem>() {
                    self.hidden_alerts.insert(alert_item.alert.id.clone());
                    self.state.data_generation = self.state.data_generation.wrapping_add(1);
                    return true;
                }
                false
            }
        }
    }

    fn apply_fetch_result(&mut self, result: FetchPayload, _pane: &PaneRef<'_>) {
        let Some(fetch) = self.state.downcast_round::<NwsAlertFetchResult>(result) else {
            log::error!("NWS alert handler received unexpected fetch result type");
            return;
        };
        match fetch.0 {
            Ok(fetched) => {
                let crate::nws::fetch::ActiveAlerts { alerts, zones } = fetched;
                log::info!("Received {} NWS alerts", alerts.len());
                let now = chrono::Utc::now().naive_utc();
                let current_ids: HashSet<String> = alerts.iter().map(|a| a.id.clone()).collect();
                // **Merge, not wholesale replace (WB-4).** The active feed
                // first, in feed order — on a live pane these are the only
                // rows that paint, so the live bytes are the bytes wholesale
                // replacement produced. An id that reappears takes its fresh
                // copy (zone geometry included) and stops being departed.
                let mut items: Vec<Arc<AlertItem>> = alerts
                    .into_iter()
                    .map(|alert| {
                        Arc::new(AlertItem {
                            alert,
                            departed: None,
                        })
                    })
                    .collect();
                // Then the history: an alert that left the feed — expired or
                // cancelled between polls — keeps drawing at instants inside
                // its window, so backward scrub shows the warnings in force
                // then. Marked with the poll that lost it, and EVICTED
                // `RETAINED_HOURS` past the earlier of that mark and its own
                // `valid_until` — bounded retention, never "keep forever".
                for old in std::mem::take(&mut self.state.data) {
                    if current_ids.contains(&old.alert.id) {
                        continue;
                    }
                    let departed = old.departed.unwrap_or(now);
                    let end = old.alert.valid_until.map_or(departed, |u| u.min(departed));
                    if now - end >= chrono::Duration::hours(RETAINED_HOURS) {
                        continue;
                    }
                    items.push(if old.departed.is_some() {
                        old
                    } else {
                        Arc::new(AlertItem {
                            alert: old.alert.clone(),
                            departed: Some(departed),
                        })
                    });
                }
                // Pruned against what is RETAINED, not against the feed: a
                // dismissed alert must stay dismissed while scrub can still
                // reach it, and is forgotten only when its item is evicted.
                self.hidden_alerts
                    .retain(|id| items.iter().any(|i| i.alert.id == *id));
                // The coverage report travels with the data it describes: a
                // round that placed 85 of 297 warnings keeps its fresh clock.
                self.state
                    .set_data_with_coverage(items, zones.completeness());
            }
            Err(e) => {
                log::error!("NWS alerts fetch failed: {e}");
                self.state.record_failure(&e);
            }
        }
    }

    fn retain_selections(&self, selections: &mut Vec<Arc<dyn OverlayItem>>, _pane: &PaneRef<'_>) {
        selections.retain(|sel| {
            if sel.layer_id() != known::NWS_ALERTS {
                return true;
            }
            self.state
                .data
                .iter()
                .any(|item| item.matches(sel.as_ref()))
        });
    }

    fn prepare_job(&self, ctx: &RasterizeContext, pane: &PaneRef<'_>) -> Option<DescribedJob> {
        self.paint_input(ctx, self.view(pane))
            .map(DescribedJob::new)
    }

    fn job_codec(&self) -> Option<&'static JobCodec> {
        crate::render::jobs::JOB_CODECS
            .iter()
            .find(|row| row.label == "overlay/alerts")
    }

    fn create_fetch_tasks(&self, ctx: &FetchConfig, _pane: &PaneRef<'_>) -> Vec<FetchTask> {
        let client = ctx.client.clone();
        let sources = ctx.sources.clone();
        let zone_cache = ctx.zone_cache_dir.clone();

        // THE ARCHIVE, FOR A PANE THAT IS NOT LOOKING AT NOW.
        //
        // `/alerts/active` answers with what is in force at the moment it is
        // asked and has no archive at all, so a pane scrubbed to a storm years
        // gone fetched today's polygons and the as-of filter dropped every one:
        // the layer reported hundreds shown and drew nothing over the volume it
        // was pinned to. `ctx.as_of` is documented for exactly this choice —
        // "a source whose archive is addressable by time reads this to choose
        // *which* archive objects to ask for".
        //
        // The threshold is a tolerance, not a policy: a live pane's `as_of` is
        // the wall clock and arrives a little stale, so anything inside the last
        // few minutes is still "now" and takes the live feed, which is the one
        // that carries watches and advisories as well as polygons.
        let archived_before = ctx.as_of
            < chrono::Utc::now().naive_utc() - chrono::Duration::minutes(ARCHIVE_CUTOFF_MINUTES);
        if archived_before {
            let at = ctx.as_of;
            return vec![FetchTask {
                kind: known::NWS_ALERTS,
                future: Box::pin(async move {
                    let result = crate::nws::archive::fetch_archived_alerts(&sources, at).await;
                    Box::new(NwsAlertFetchResult(result)) as FetchPayload
                }),
            }];
        }

        log::info!("Fetching NWS active alerts");
        vec![FetchTask {
            kind: known::NWS_ALERTS,
            future: Box::pin(async move {
                let result = crate::nws::fetch::fetch_active_alerts(
                    &client,
                    &sources,
                    zone_cache.as_deref(),
                )
                .await;
                Box::new(NwsAlertFetchResult(result)) as FetchPayload
            }),
        }]
    }

    fn controls(&self, pane: &PaneRef<'_>) -> Vec<ControlItem> {
        let view = self.view(pane);
        let mut items = vec![ControlItem::Heading {
            text: "NWS Alerts".into(),
        }];
        // One toggle per category, walked from `AlertCategory::ALL`: a category
        // with no toggle is a category the user cannot turn on.
        items.extend(
            AlertCategory::ALL
                .into_iter()
                .map(|category| ControlItem::Toggle {
                    id: category.control_id(),
                    label: category.plural_label().into(),
                    enabled: view.enabled_categories.contains(&category),
                }),
        );

        // Ungated on enabled: a hidden layer's options stay visible and
        // editable, and the status lines keep reporting.
        items.push(ControlItem::ButtonRow {
            buttons: vec![ControlButton {
                id: "refresh",
                label: "\u{21bb} Refresh".into(),
                enabled: !self.state.fetching,
                highlight: false,
            }],
        });
        if self.state.fetching {
            items.push(ControlItem::InfoText {
                text: "Fetching...".into(),
            });
        }
        if self.has_data(pane) {
            let allowed = self.drawn_count(view);
            let painted = self.painted_count(view);
            items.push(ControlItem::InfoText {
                text: if painted == allowed {
                    format!("{allowed} alerts shown")
                } else {
                    format!("{painted} of {allowed} alerts shown")
                },
            });
        }
        if let Some(t) = self.state.fetch_time {
            let secs = t.elapsed().as_secs();
            let text = if secs < 60 {
                format!("Updated {secs}s ago")
            } else {
                format!("Updated {}m ago", secs / 60)
            };
            items.push(ControlItem::InfoText { text });
        }

        items
    }

    fn apply_control(&mut self, update: &ControlUpdate, pane: &mut PaneMut<'_>) -> ControlEffect {
        if update.id == "refresh" {
            return ControlEffect::Fetch;
        }
        // Resolved through `AlertCategory::from_control_id`, so the toggles this
        // accepts are the same enumeration `controls` offers.
        let Some(category) = AlertCategory::from_control_id(update.id) else {
            return ControlEffect::None;
        };
        if let ControlValue::Bool(enabled) = update.value {
            let was_enabled = self.is_enabled(&pane.as_ref());
            self.edit(pane, |state| {
                if enabled {
                    state.enabled_categories.insert(category);
                } else {
                    state.enabled_categories.remove(&category);
                }
            });
            self.state.data_generation = self.state.data_generation.wrapping_add(1);
            if !was_enabled
                && self.is_enabled(&pane.as_ref())
                && self
                    .state
                    .enable_should_refetch(self.has_data(&pane.as_ref()))
            {
                return ControlEffect::Fetch;
            }
        }
        ControlEffect::None
    }

    // ── Per-pane state (WO-M10c) ──────────────────────────

    fn create_pane_state(&self, enabled: bool) -> Option<FetchPayload> {
        Some(Box::new(AlertPaneState::new(enabled)))
    }

    /// Field for field what `deserialize_state` does, against the pane's own
    /// state — **except that the pane's slot flag dominates.**
    ///
    /// For this layer the flag and the set are the *same fact* stored twice:
    /// "on" **is** a non-empty set, and `adopt_handler_state` always writes
    /// both together, so the pane's own saved bytes can never disagree with
    /// its own flag. They disagree only when the config did not come from
    /// this pane — `initialize_pane_enabled` seeds a pane that has saved
    /// nothing with the *registry's* serialize, which knows nothing about
    /// this pane. The flag is the half that is the pane's, so it wins: a pane
    /// that is off comes up with an empty set whatever set the config names,
    /// and a pane that is on and whose config names none comes up with
    /// `ALL` — the same reading `set_enabled(true)` gives.
    fn deserialize_pane_state(
        &self,
        value: serde_json::Value,
        enabled: bool,
    ) -> Option<FetchPayload> {
        let mut state = AlertPaneState::new(enabled);
        if !enabled {
            return Some(Box::new(state));
        }
        Self::restore_categories(&mut state.enabled_categories, &value);
        if state.enabled_categories.is_empty() {
            state.enabled_categories.extend(AlertCategory::ALL);
        }
        Some(Box::new(state))
    }

    /// **Byte-identical to `serialize_state`** — same members, same order,
    /// same values. The corpus is what says so.
    fn serialize_pane_state(&self, state: &dyn Any) -> serde_json::Value {
        match state.downcast_ref::<AlertPaneState>() {
            Some(state) => Self::save_categories(&state.enabled_categories),
            None => serde_json::Value::Null,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nws::fetch::ActiveAlerts;
    use crate::nws::zones::ZoneResolution;
    use crate::types::{HatchPattern, OverlayFeature};

    fn alert(id: &str, event: &str) -> NwsAlert {
        let polygons = vec![vec![vec![(35.0, -97.0), (35.5, -97.0), (35.5, -96.5)]]];
        let (fill, stroke) = crate::nws::colors::alert_color(event);
        NwsAlert {
            id: id.to_string(),
            event: event.to_string(),
            category: AlertCategory::from_event(event),
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
            affected_zones: Vec::new(),
            features: Arc::new(vec![OverlayFeature::new(
                polygons,
                fill,
                stroke,
                event.to_string(),
                String::new(),
                HatchPattern::None,
            )]),
        }
    }

    fn whole(alerts: Vec<NwsAlert>) -> FetchPayload {
        Box::new(NwsAlertFetchResult(Ok(ActiveAlerts::whole(alerts))))
    }

    fn with_zones(alerts: Vec<NwsAlert>, zones: ZoneResolution) -> FetchPayload {
        Box::new(NwsAlertFetchResult(Ok(ActiveAlerts { alerts, zones })))
    }

    /// **Asking for no *time* is not asking for nothing**, and this is the
    /// shape three of the five `EventLifetime` layers answer in.
    ///
    /// An alert carries its own validity window, so the picture at a stop is
    /// a function of the stop and of nothing behind it. The ask is one
    /// zero-width range per stop: `is_empty` false, `total` zero, and every
    /// stop covered. A conformance walk reading "non-empty" as "asks for some
    /// duration" would call this layer silent when it is not.
    #[test]
    fn an_alert_stop_asks_for_the_instant_and_no_stretch_behind_it() {
        use squallar_source::handler::SourceHandler;

        let handler = NwsAlertHandler::new();
        let pane = PaneRef::bare(0);
        let at = |h: u32| {
            chrono::NaiveDate::from_ymd_opt(2026, 8, 22)
                .expect("a real date")
                .and_hms_opt(h, 0, 0)
                .expect("a real time")
        };
        let stops: Vec<chrono::NaiveDateTime> = (0..13).map(at).collect();

        let residency = handler.residency_for(&pane, &stops);
        assert!(
            !residency.is_empty(),
            "a layer that reads the clock asks for something at every stop",
        );
        assert_eq!(
            residency.total(),
            chrono::Duration::zero(),
            "and none of that something is a stretch of archive",
        );
        assert_eq!(residency.ranges().len(), 13);
        for stop in &stops {
            assert!(residency.covers(*stop), "the stop at {stop}");
        }
        assert!(
            !residency.covers(at(3) + chrono::Duration::minutes(30)),
            "an instant the pane cannot stop on is not asked for",
        );
    }

    fn handler_with(alerts: Vec<NwsAlert>) -> NwsAlertHandler {
        let mut handler = NwsAlertHandler::new();
        handler.apply_fetch_result(whole(alerts), &PaneRef::across(&[]));
        handler
    }

    /// A **zone-based** alert exactly as the parser admits one: `affectedZones`
    /// listed, no `geometry`, and `zone_count` features.
    fn zone_alert(id: &str, event: &str, zone_count: usize) -> NwsAlert {
        let mut alert = alert(id, event);
        alert.affected_zones = (0..3)
            .map(|i| format!("https://api.weather.gov/zones/county/OKC{i:03}"))
            .collect();
        let (fill, stroke) = crate::nws::colors::alert_color(event);
        alert.features = Arc::new(
            (0..zone_count)
                .map(|i| {
                    let lat = 35.0 + i as f64;
                    OverlayFeature::new(
                        vec![vec![vec![
                            (lat, -97.0),
                            (lat + 0.5, -97.0),
                            (lat + 0.5, -96.5),
                        ]]],
                        fill,
                        stroke,
                        event.to_string(),
                        String::new(),
                        HatchPattern::None,
                    )
                })
                .collect(),
        );
        alert
    }

    /// **A warning that gains its polygons across a poll must move the token,
    /// or it stays invisible.** Poll *N* is the same id drawing nothing and poll
    /// *N+1* the same id drawing three counties.
    #[test]
    fn a_warning_that_gains_its_zone_polygons_moves_the_signature() {
        let mut handler = handler_with(vec![zone_alert("a", "Tornado Warning", 0)]);
        let unresolved = handler.content_signature(&PaneRef::bare(0));
        assert_eq!(
            handler.drawn_count(&handler.defaults),
            1,
            "fixture: the alert must count as drawn with no features, or the \
             count would move on its own and this proves nothing",
        );

        handler.apply_fetch_result(
            whole(vec![zone_alert("a", "Tornado Warning", 3)]),
            &PaneRef::across(&[]),
        );
        let resolved = handler.content_signature(&PaneRef::bare(0));
        assert_eq!(
            handler.drawn_count(&handler.defaults),
            1,
            "fixture: still one drawn alert, so only the geometry moved",
        );
        assert_ne!(
            resolved, unresolved,
            "a warning arrived with its counties and the token did not move, so \
             nothing re-rasterizes and the warning stays off the map",
        );

        handler.apply_fetch_result(
            whole(vec![zone_alert("a", "Tornado Warning", 2)]),
            &PaneRef::across(&[]),
        );
        assert_ne!(
            handler.content_signature(&PaneRef::bare(0)),
            resolved,
            "two of three zones is not the picture three of three is",
        );
    }

    /// The signature names the **set**, not the fetch: a refetch returning the
    /// same warning ids must keep it, which `data_generation` cannot do.
    #[test]
    fn a_refetch_of_the_same_warning_set_keeps_the_signature() {
        let mut handler = handler_with(vec![alert("a", "Tornado Warning")]);
        let first = handler.content_signature(&PaneRef::bare(0));
        let generation_before = handler.data_generation();
        handler.apply_fetch_result(
            whole(vec![alert("a", "Tornado Warning")]),
            &PaneRef::across(&[]),
        );
        assert_ne!(
            handler.data_generation(),
            generation_before,
            "fixture: the refetch really did bump the generation",
        );
        assert_eq!(
            handler.content_signature(&PaneRef::bare(0)),
            first,
            "an unchanged warning set across a poll must keep its signature",
        );
    }

    #[test]
    fn every_change_to_the_drawn_set_moves_the_signature() {
        let mut handler = handler_with(vec![alert("a", "Tornado Warning")]);
        let one_warning = handler.content_signature(&PaneRef::bare(0));

        handler.apply_fetch_result(
            whole(vec![
                alert("a", "Tornado Warning"),
                alert("b", "Severe Thunderstorm Warning"),
            ]),
            &PaneRef::across(&[]),
        );
        let two_warnings = handler.content_signature(&PaneRef::bare(0));
        assert_ne!(two_warnings, one_warning, "a new warning must move it");

        handler.apply_fetch_result(
            whole(vec![alert("b", "Severe Thunderstorm Warning")]),
            &PaneRef::across(&[]),
        );
        let b_only = handler.content_signature(&PaneRef::bare(0));
        assert_ne!(b_only, two_warnings, "an expiry must move it");
        assert_ne!(
            b_only, one_warning,
            "a different single warning is a different set",
        );

        handler.hidden_alerts.insert("b".to_string());
        assert_ne!(
            handler.content_signature(&PaneRef::bare(0)),
            b_only,
            "hiding an alert must move it",
        );
        handler.hidden_alerts.clear();

        handler
            .defaults
            .enabled_categories
            .remove(&AlertCategory::Warning);
        assert_ne!(
            handler.content_signature(&PaneRef::bare(0)),
            b_only,
            "disabling the category must move it",
        );
    }

    /// **`shown` means on the map, not past the filters.** An alert whose zone
    /// boundaries did not resolve is let through by every filter and paints
    /// nothing. The split spelling appears only when the two disagree.
    #[test]
    fn the_status_line_splits_what_is_on_the_map_from_what_passed_the_filters() {
        let mut handler = NwsAlertHandler::new();
        handler.apply_fetch_result(
            with_zones(
                vec![
                    zone_alert("a", "Tornado Warning", 3),
                    zone_alert("b", "Tornado Warning", 0),
                    zone_alert("c", "Tornado Warning", 0),
                ],
                ZoneResolution {
                    alerts_expected: 3,
                    alerts_complete: 1,
                    alerts_missing: 2,
                    zones_requested: 9,
                    zones_resolved: 3,
                    ..ZoneResolution::default()
                },
            ),
            &PaneRef::across(&[]),
        );
        assert_eq!(
            handler.drawn_count(&handler.defaults),
            3,
            "premise: every filter lets all three through",
        );
        assert_eq!(
            handler.status_line(&PaneRef::bare(0)).as_deref(),
            Some("1 of 3 shown - W/Wa/Adv/Oth"),
            "two alerts with no shape must not be counted as shown",
        );

        handler.hidden_alerts.insert("a".to_string());
        assert_eq!(
            handler.status_line(&PaneRef::bare(0)).as_deref(),
            Some("0 of 2 shown - W/Wa/Adv/Oth"),
            "both numbers must move with the filters, not just the denominator",
        );

        handler.hidden_alerts.clear();
        handler.apply_fetch_result(
            whole(vec![zone_alert("a", "Tornado Warning", 3)]),
            &PaneRef::across(&[]),
        );
        assert_eq!(
            handler.status_line(&PaneRef::bare(0)).as_deref(),
            Some("1 shown - W/Wa/Adv/Oth")
        );
    }

    #[test]
    fn the_status_line_counts_the_drawn_set_and_names_the_categories() {
        let mut handler = handler_with(vec![
            alert("a", "Tornado Warning"),
            alert("b", "Severe Thunderstorm Warning"),
        ]);
        assert_eq!(
            handler.status_line(&PaneRef::bare(0)).as_deref(),
            Some("2 shown - W/Wa/Adv/Oth")
        );

        handler.hidden_alerts.insert("b".to_string());
        assert_eq!(
            handler.status_line(&PaneRef::bare(0)).as_deref(),
            Some("1 shown - W/Wa/Adv/Oth"),
            "a hidden alert is not shown, so it must not be counted as shown"
        );

        handler
            .defaults
            .enabled_categories
            .remove(&AlertCategory::Advisory);
        handler
            .defaults
            .enabled_categories
            .remove(&AlertCategory::Watch);
        assert_eq!(
            handler.status_line(&PaneRef::bare(0)).as_deref(),
            Some("1 shown - W/Oth")
        );

        handler
            .defaults
            .enabled_categories
            .remove(&AlertCategory::Other);
        assert_eq!(
            handler.status_line(&PaneRef::bare(0)).as_deref(),
            Some("1 shown - W")
        );

        handler.defaults.enabled_categories.clear();
        assert_eq!(
            handler.status_line(&PaneRef::bare(0)),
            None,
            "a disabled layer's dimmed row carries no status line"
        );
    }

    /// The master toggle round-trips through the category set: off clears it, on
    /// restores the defaults, and on over a *partial* set changes nothing.
    #[test]
    fn the_master_toggle_clears_and_restores_the_category_set() {
        let mut handler = NwsAlertHandler::new();
        assert!(
            handler.is_enabled(&PaneRef::bare(0)),
            "precondition: defaults on"
        );

        handler.set_enabled(false, &mut PaneMut::bare(0));
        assert!(!handler.is_enabled(&PaneRef::bare(0)));
        assert!(handler.defaults.enabled_categories.is_empty());

        handler.set_enabled(true, &mut PaneMut::bare(0));
        assert!(handler.is_enabled(&PaneRef::bare(0)));
        assert_eq!(
            handler.defaults.enabled_categories.len(),
            AlertCategory::ALL.len(),
            "on from nothing restores every category"
        );

        handler
            .defaults
            .enabled_categories
            .remove(&AlertCategory::Advisory);
        handler.set_enabled(true, &mut PaneMut::bare(0));
        assert_eq!(
            handler.defaults.enabled_categories.len(),
            AlertCategory::ALL.len() - 1,
            "on over a live subset must not widen the user's selection"
        );
    }

    #[test]
    fn the_popup_grid_carries_the_cap_triple_and_the_optional_times() {
        let mut with_times = alert("a", "Tornado Warning");
        with_times.onset = Some("2026-08-10T18:00:00-05:00".to_string());
        with_times.ends = Some("2026-08-10T19:30:00-05:00".to_string());
        let prefs = squallar_units::UserPreferences::default();

        let grid = |alert: &NwsAlert| -> Vec<(String, String)> {
            AlertItem {
                alert: alert.clone(),
                departed: None,
            }
            .popup_content(&prefs)
            .sections
            .iter()
            .find_map(|s| match s {
                PopupSection::KeyValueGrid(rows) => Some(rows.clone()),
                _ => None,
            })
            .expect("the alert popup carries a key-value grid")
        };

        let rows = grid(&with_times);
        let value = |key: &str| {
            rows.iter()
                .find(|(k, _)| k == key)
                .unwrap_or_else(|| panic!("the grid has no {key:?} row"))
                .1
                .clone()
        };
        assert_eq!(value("Severity"), "Severe");
        assert_eq!(value("Urgency"), "Immediate");
        assert_eq!(value("Certainty"), "Observed");
        assert!(!value("Onset").is_empty());
        assert!(!value("Ends").is_empty());

        // An alert without onset/ends draws no row for them: absence is the
        // alert's own shape, not a blank.
        let bare = grid(&alert("b", "Tornado Warning"));
        assert!(bare.iter().all(|(k, _)| k != "Onset" && k != "Ends"));
        assert!(
            bare.iter().any(|(k, _)| k == "Severity"),
            "the CAP triple is unconditional — every alert has one"
        );
    }

    /// The count the inspector reads and the set a click can land on are the
    /// same set, under every combination of the two filters.
    #[test]
    fn the_shown_count_is_the_clickable_set() {
        let mut handler = handler_with(vec![
            alert("a", "Tornado Warning"),
            alert("b", "Tornado Watch"),
            alert("c", "Flood Advisory"),
        ]);
        let agree = |h: &NwsAlertHandler, expected: usize, why: &str| {
            assert_eq!(h.drawn_count(&h.defaults), expected, "{why}");
            assert_eq!(
                h.drawn_count(&h.defaults),
                h.clickable_items(&PaneRef::bare(0)).len(),
                "the count and the clickable set disagree: {why}",
            );
        };

        agree(
            &handler,
            3,
            "all three categories are on and nothing is hidden",
        );

        handler.hidden_alerts.insert("b".to_string());
        agree(
            &handler,
            2,
            "a hidden alert is neither counted nor clickable",
        );

        handler
            .defaults
            .enabled_categories
            .remove(&AlertCategory::Advisory);
        agree(
            &handler,
            1,
            "a category turned off takes its alerts with it",
        );

        handler.defaults.enabled_categories.clear();
        agree(&handler, 0, "the whole layer off draws and answers nothing");
    }

    /// Real NWS event names, from the same capture of
    /// `api.weather.gov/alerts/types` that `nws::colors` checks its table
    /// against. Provenance and refresh instructions are in the file's header.
    const EVENT_TYPES_FIXTURE: &str = include_str!("../../nws/event_types.txt");

    fn every_nws_event_type() -> Vec<&'static str> {
        EVENT_TYPES_FIXTURE
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect()
    }

    /// **Every product NWS can send must be paintable by a fresh install**, and
    /// driven by *NWS's* enumeration rather than by `AlertCategory::ALL`:
    /// asserting our four variants are all enabled would only assert that our
    /// list equals our list. 11 of the 111 published event types classify as
    /// `Other`, and 25 of 271 active alerts in the live sample were Air Quality.
    #[test]
    fn every_nws_event_type_lands_in_a_category_a_fresh_install_draws() {
        let handler = NwsAlertHandler::new();
        let events = every_nws_event_type();
        assert!(
            events.len() > 100,
            "fixture: expected the full NWS product list, got {}; an empty \
             fixture would make this pass vacuously",
            events.len(),
        );

        let orphaned: Vec<(&str, AlertCategory)> = events
            .iter()
            .map(|e| (*e, AlertCategory::from_event(e)))
            .filter(|(_, category)| !handler.defaults.enabled_categories.contains(category))
            .collect();
        assert!(
            orphaned.is_empty(),
            "these NWS products classify into a category no default install \
             enables, so they can never be drawn: {orphaned:?}",
        );
    }

    /// **Every category must have a toggle, and the toggle must work.** `Other`
    /// had no `ControlItem::Toggle` and `apply_control` accepted no id resolving
    /// to it.
    #[test]
    fn every_category_has_a_toggle_that_turns_it_on_and_off() {
        let mut handler = NwsAlertHandler::new();
        let offered: Vec<&str> = handler
            .controls(&PaneRef::bare(0))
            .into_iter()
            .filter_map(|item| match item {
                ControlItem::Toggle { id, .. } => Some(id),
                _ => None,
            })
            .collect();

        for category in AlertCategory::ALL {
            assert!(
                offered.contains(&category.control_id()),
                "{category} has no toggle; the panel offers {offered:?}",
            );

            let mut ctx = PaneMut::bare(0);
            handler.apply_control(
                &ControlUpdate {
                    id: category.control_id(),
                    value: ControlValue::Bool(false),
                },
                &mut ctx,
            );
            assert!(
                !handler.defaults.enabled_categories.contains(&category),
                "{category}'s toggle did not turn it off",
            );
            handler.apply_control(
                &ControlUpdate {
                    id: category.control_id(),
                    value: ControlValue::Bool(true),
                },
                &mut ctx,
            );
            assert!(
                handler.defaults.enabled_categories.contains(&category),
                "{category}'s toggle did not turn it back on",
            );
        }
    }

    /// **The count that would have revealed the gap excluded it too**:
    /// `drawn_count` and `painted_count` both filter on `is_drawn`, which was
    /// permanently false for `Other`.
    #[test]
    fn an_air_quality_alert_is_drawn_counted_and_clickable() {
        let mut handler = handler_with(vec![
            alert("a", "Tornado Warning"),
            alert("b", "Air Quality Alert"),
        ]);
        assert_eq!(
            AlertCategory::from_event("Air Quality Alert"),
            AlertCategory::Other,
            "fixture: this is the category the whole defect was about",
        );

        assert_eq!(
            handler.drawn_count(&handler.defaults),
            2,
            "the air quality alert is missing from the count that would have \
             shown it missing from the map",
        );
        assert_eq!(handler.clickable_items(&PaneRef::bare(0)).len(), 2);
        assert_eq!(
            handler.status_line(&PaneRef::bare(0)).as_deref(),
            Some("2 shown - W/Wa/Adv/Oth"),
        );

        handler
            .defaults
            .enabled_categories
            .remove(&AlertCategory::Other);
        assert_eq!(handler.drawn_count(&handler.defaults), 1);
        assert_eq!(
            handler.status_line(&PaneRef::bare(0)).as_deref(),
            Some("1 shown - W/Wa/Adv")
        );
    }

    /// A set persisted by a build that had three toggles is not a user who
    /// turned the fourth off — they were never offered it. An empty set is the
    /// master toggle off and stays off.
    ///
    /// Read through `deserialize_pane_state`, which is where a saved set lands
    /// since WO-M10c: the set is the PANE's, and the global `serialize_state`
    /// no longer carries it.
    #[test]
    fn a_category_the_saved_build_never_offered_comes_back_on() {
        let restored = |value: serde_json::Value, on: bool| {
            let state = NwsAlertHandler::new()
                .deserialize_pane_state(value, on)
                .expect("alerts keep per-pane state");
            state
                .downcast_ref::<AlertPaneState>()
                .expect("the alert layer's own state type")
                .enabled_categories
                .clone()
        };

        assert!(
            restored(
                serde_json::json!({
                    "enabled_categories": ["Warning", "Watch", "Advisory"],
                }),
                true,
            )
            .contains(&AlertCategory::Other),
            "a category with no toggle in the saving build was never declined",
        );

        assert_eq!(
            restored(
                serde_json::json!({
                    "enabled_categories": ["Warning"],
                    "known_categories": AlertCategory::ALL,
                }),
                true,
            ),
            HashSet::from([AlertCategory::Warning]),
            "the user turned three categories off and they must stay off",
        );

        // An empty set is the master toggle off, and the pane's own flag says
        // so too — the two are one fact, and `adopt_handler_state` writes them
        // together.
        assert!(
            restored(serde_json::json!({ "enabled_categories": [] }), false).is_empty(),
            "an empty set is a deliberate state, not a build to migrate",
        );
    }

    #[test]
    fn the_signature_is_a_set_signature_not_a_sequence_signature() {
        let forward = handler_with(vec![
            alert("a", "Tornado Warning"),
            alert("b", "Flash Flood Warning"),
        ]);
        let backward = handler_with(vec![
            alert("b", "Flash Flood Warning"),
            alert("a", "Tornado Warning"),
        ]);
        assert_eq!(
            forward.content_signature(&PaneRef::bare(0)),
            backward.content_signature(&PaneRef::bare(0))
        );
    }

    // ── Per-pane state (WO-M10c) ──────────────────────────────────────

    fn pane_with(categories: &[AlertCategory]) -> FetchPayload {
        Box::new(AlertPaneState {
            enabled_categories: categories.iter().copied().collect(),
        })
    }

    /// **Two panes filtering the same alerts differently**, which the config
    /// swap could only fake by re-installing one pane's category set before
    /// every read.
    ///
    /// The panes are asserted **equal first**, and `defaults` is asserted
    /// untouched at the end — the assertion that fires the moment one of these
    /// methods writes a per-pane value to `&mut self`.
    #[test]
    fn two_panes_hold_different_alert_categories_and_the_registry_keeps_neither() {
        let mut handler = handler_with(vec![
            alert("w", "Tornado Warning"),
            alert("a", "Flood Advisory"),
        ]);
        assert_eq!(
            handler.state.data.len(),
            2,
            "premise: one alert of each category is on the map",
        );

        let all = AlertCategory::ALL;
        let a = pane_with(&all);
        let mut b = pane_with(&all);
        fn ref_of<'a>(p: &'a FetchPayload, idx: usize) -> PaneRef<'a> {
            PaneRef {
                state: Some(&**p),
                ..PaneRef::bare(idx)
            }
        }
        assert_eq!(
            handler.status_line(&ref_of(&a, 0)),
            handler.status_line(&ref_of(&b, 1)),
            "premise: two panes with the same categories answer the same",
        );

        // Diverge through the handler's own control route, not a field write.
        handler.apply_control(
            &ControlUpdate {
                id: AlertCategory::Advisory.control_id(),
                value: ControlValue::Bool(false),
            },
            &mut PaneMut {
                pane_idx: 1,
                state: Some(&mut *b),
                peers: &[&*a],
            },
        );

        let pane_a = ref_of(&a, 0);
        let pane_b = ref_of(&b, 1);
        assert_eq!(
            handler.drawn_count(handler.view(&pane_a)),
            2,
            "pane 0 still lets both categories through",
        );
        assert_eq!(
            handler.drawn_count(handler.view(&pane_b)),
            1,
            "pane 1 turned advisories off",
        );
        assert_eq!(
            handler.clickable_items(&pane_a).len(),
            2,
            "pane 0's clickable set",
        );
        assert_eq!(
            handler.clickable_items(&pane_b).len(),
            1,
            "pane 1's clickable set",
        );
        // The cache token is what the render dispatch groups panes by: an
        // equal token here is one pane drawing the other pane's alerts.
        assert_ne!(
            handler.content_signature(&pane_a),
            handler.content_signature(&pane_b),
            "two panes filtering different categories shared one cache token",
        );
        assert_eq!(
            handler.serialize_pane_state(&*b)["enabled_categories"]
                .as_array()
                .map(Vec::len),
            Some(all.len() - 1),
            "pane 1's saved bytes",
        );
        assert_eq!(
            handler.defaults.enabled_categories.len(),
            all.len(),
            "the registry's own copy took one of the panes' edits",
        );
    }

    // ── The as-of filter (WO-M11) ─────────────────────────────────────────

    fn at(h: u32, m: u32) -> chrono::NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(2026, 8, 20)
            .unwrap()
            .and_hms_opt(h, m, 0)
            .unwrap()
    }

    /// A live pane's context: `as_of` **is** the clock.
    fn live_ctx(clock: chrono::NaiveDateTime) -> RasterizeContext {
        RasterizeContext {
            is_dark: false,
            zoom: 7.0,
            device_scale: 1.0,
            now: clock,
            as_of: clock,
            frame: None,
        }
    }

    /// `alert()` with the four CAP time strings filled in, and the parsed
    /// window derived **through the parser's own function** so a fixture can
    /// never disagree with what a real fetch would produce.
    fn timed_alert(
        id: &str,
        effective: &str,
        expires: &str,
        onset: Option<&str>,
        ends: Option<&str>,
    ) -> NwsAlert {
        let mut a = alert(id, "Tornado Warning");
        let (from, until) = crate::nws::alert::parse_valid_window(effective, expires, onset, ends);
        a.effective = effective.to_owned();
        a.expires = expires.to_owned();
        a.onset = onset.map(str::to_owned);
        a.ends = ends.map(str::to_owned);
        a.valid_from = from;
        a.valid_until = until;
        a
    }

    fn rows_at(h: &NwsAlertHandler, clock: chrono::NaiveDateTime) -> Vec<String> {
        let job = h.prepare_job(&live_ctx(clock), &PaneRef::bare(0));
        job.map(|job| {
            job.downcast_ref::<rasterize::AlertsInput>()
                .expect("the alerts row")
                .alerts
                .iter()
                .map(|a| a.id.clone())
                .collect()
        })
        .unwrap_or_default()
    }

    /// **0 / 1 / 0 across one alert's own window.** Before onset it is not
    /// yet true, inside it is, at `ends` it stops — the end is exclusive, so
    /// the instant an alert ends is the first instant it is gone.
    ///
    /// The three readings come from one handler and one alert, so the only
    /// thing that differs between them is the depicted instant.
    #[test]
    fn an_alert_appears_at_its_onset_and_is_gone_at_its_end() {
        let h = handler_with(vec![timed_alert(
            "urn:straddle",
            "2026-08-20T14:00:00Z",
            "2026-08-20T23:00:00Z",
            Some("2026-08-20T18:00:00Z"),
            Some("2026-08-20T22:00:00Z"),
        )]);

        assert_eq!(
            rows_at(&h, at(17, 59)),
            Vec::<String>::new(),
            "an alert whose onset has not arrived was drawn anyway",
        );
        assert_eq!(
            rows_at(&h, at(18, 0)),
            vec!["urn:straddle".to_owned()],
            "the onset instant itself is inside the window (`from <= as_of`)",
        );
        assert_eq!(
            rows_at(&h, at(21, 59)),
            vec!["urn:straddle".to_owned()],
            "the last minute of the window",
        );
        assert_eq!(
            rows_at(&h, at(22, 0)),
            Vec::<String>::new(),
            "the end instant is exclusive (`as_of < until`); an alert that \
             has ended was still drawn",
        );
        assert_eq!(
            rows_at(&h, at(23, 30)),
            Vec::<String>::new(),
            "an alert past even its message expiry was still drawn",
        );
    }

    /// **A garbage window costs the alert nothing, at any instant.** Unbounded
    /// on both sides is always valid, which is the failure posture: an alert
    /// the parser could not time is an alert the user still sees.
    ///
    /// Paired with a *timed* alert in the same handler, so this is not a test
    /// that the filter is off — the timed one comes and goes across the same
    /// three readings while this one never does.
    #[test]
    fn an_alert_with_four_unreadable_times_is_drawn_at_every_instant() {
        let h = handler_with(vec![
            timed_alert(
                "urn:garbage",
                "soon",
                "later",
                Some("whenever"),
                Some("eventually"),
            ),
            timed_alert(
                "urn:timed",
                "2026-08-20T14:00:00Z",
                "2026-08-20T23:00:00Z",
                Some("2026-08-20T18:00:00Z"),
                Some("2026-08-20T22:00:00Z"),
            ),
        ]);

        for clock in [at(0, 0), at(19, 0), at(23, 59)] {
            assert!(
                rows_at(&h, clock).contains(&"urn:garbage".to_owned()),
                "the untimed alert vanished at {clock}",
            );
        }
        assert_eq!(
            rows_at(&h, at(0, 0)).len(),
            1,
            "control: the timed alert IS filtered at 00:00, so the reading \
             above is the filter passing it and not the filter being off",
        );
        assert_eq!(rows_at(&h, at(19, 0)).len(), 2, "control: both inside");
    }

    /// FNV-1a 64 over the encoded job, so a byte that moves is a number that
    /// moves. A copy of the house hash; `squallar_radar::wire::layout_digest`
    /// is `#[cfg(test)]` in another crate.
    fn fnv1a64(bytes: &[u8]) -> u64 {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for &byte in bytes {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash
    }

    /// The alerts every other fixture in this workspace is made of: no time
    /// strings at all, which is the shape **every** alert had before WO-M11,
    /// since nothing parsed the four fields.
    fn untimed_fixture() -> Vec<NwsAlert> {
        vec![
            alert("urn:oid:2.49.0.1.840.0001", "Tornado Warning"),
            alert("urn:oid:2.49.0.1.840.0002", "Severe Thunderstorm Watch"),
            alert("urn:oid:2.49.0.1.840.0003", "Flood Advisory"),
        ]
    }

    fn encoded_job(h: &NwsAlertHandler, clock: chrono::NaiveDateTime) -> (usize, u64) {
        use squallar_source::job::{EncodeCtx, JobGeometry};
        let job = h
            .prepare_job(&live_ctx(clock), &PaneRef::bare(0))
            .expect("the fixture has data, so it describes a job");
        let row = h.job_codec().expect("the alerts row");
        let mut bytes = Vec::new();
        (row.encode)(
            &job,
            &EncodeCtx {
                geometry: JobGeometry {
                    width: 512,
                    height: 512,
                    bounds: squallar_geo::GeoBounds {
                        min_lat: 34.0,
                        max_lat: 37.0,
                        min_lon: -99.0,
                        max_lon: -95.0,
                    },
                    side_ceiling_px: 2048,
                },
            },
            &mut bytes,
        );
        (bytes.len(), fnv1a64(&bytes))
    }

    /// **The dark-land pin.** Length and FNV-1a-64 of the encoded alerts job
    /// for [`untimed_fixture`], **measured on `main@62289151`** — the commit
    /// before WO-M11, with no `valid_from`, no `valid_until` and no as-of
    /// filter — by running this same body there against a `RasterizeContext`
    /// that had only `now`. If WO-M11 changed one byte a live pane puts on
    /// the wire, this number moves.
    ///
    /// **The baseline carries this land's category-ordering fix and nothing
    /// else**, because without it main has no single answer to measure: on
    /// `main@62289151` unmodified, four runs of this fixture gave four
    /// digests — 0x8bfce14b813b2793, 0x94366c1cca5e64eb, 0x4f7aa91cb2f75513,
    /// 0x37cd579f64c53c0b — and one length, 488, because `paint_input` built
    /// `enabled_categories` by iterating a per-process-seeded `HashSet`. With
    /// the ordering fix applied to `main` alone, three runs there and three
    /// here agree on this exact pair, so the as-of filter moves nothing.
    ///
    /// It pins `prepare_job`'s **output**, not the codec: the codec is
    /// untouched this step, and `squallar_worker::wire_identity`'s framing rows
    /// pin that separately.
    const UNTIMED_JOB_PRE_M11: (usize, u64) = (488, 0x660e_6d95_ce1d_1641);

    #[test]
    fn a_live_pane_encodes_the_bytes_it_encoded_before_the_as_of_filter_existed() {
        let h = handler_with(untimed_fixture());
        assert_eq!(
            encoded_job(&h, at(19, 0)),
            UNTIMED_JOB_PRE_M11,
            "a live pane's alerts job is no longer the bytes main@62289151 \
             produced for the same alerts. WO-M11 dark-lands: with \
             `as_of == now` and alerts that name no window, the filter is \
             `true && true` and nothing may move.",
        );
        assert_eq!(
            encoded_job(&h, at(3, 0)),
            UNTIMED_JOB_PRE_M11,
            "an alert with no readable window is instant-independent; the \
             clock changed the bytes, so something is reading `as_of` that \
             should not be",
        );
    }

    /// Non-triviality floor for the pin above: the digest is a function of the
    /// rows, so a fixture that lost an alert would be *caught* rather than
    /// silently pinned. A four-alert set must not hash to the three-alert one.
    #[test]
    fn the_dark_land_pin_is_a_function_of_the_rows_it_pins() {
        let three = handler_with(untimed_fixture());
        let mut four = untimed_fixture();
        four.push(alert("urn:oid:2.49.0.1.840.0004", "Tornado Warning"));
        let four = handler_with(four);
        assert_ne!(
            encoded_job(&three, at(19, 0)),
            encoded_job(&four, at(19, 0)),
            "the digest does not see the alert rows, so the pin above would \
             hold over a filter that dropped every one of them",
        );
    }

    /// **The category set travels in `ALL`'s declaration order, never the
    /// `HashSet`'s.** Found on `main@62289151`: `paint_input` built this `Vec`
    /// by iterating a per-process-seeded `HashSet`, so one picture encoded to
    /// a different byte string on every run — the exact defect the codec's own
    /// comment cites as its reason for sorting `hidden_ids`, on the one field
    /// the codec cannot sort because by then it is a `Vec` whose order **is**
    /// the answer.
    ///
    /// Walked over all fifteen non-empty subsets: with the fix every one comes
    /// out in declaration order, and a raw set iteration would have to land on
    /// the right order fifteen times running to survive.
    #[test]
    fn the_category_set_travels_in_declaration_order_not_the_sets_own() {
        for mask in 1u8..16 {
            let chosen: Vec<AlertCategory> = AlertCategory::ALL
                .into_iter()
                .enumerate()
                .filter(|(i, _)| mask & (1 << i) != 0)
                .map(|(_, c)| c)
                .collect();
            let mut state = AlertPaneState::new(true);
            state.enabled_categories = chosen.iter().copied().collect();
            let h = handler_with(untimed_fixture());
            let pane = PaneRef {
                state: Some(&state),
                ..PaneRef::bare(0)
            };
            let job = h
                .prepare_job(&live_ctx(at(19, 0)), &pane)
                .expect("the fixture has data");
            assert_eq!(
                job.downcast_ref::<rasterize::AlertsInput>()
                    .expect("the alerts row")
                    .enabled_categories,
                chosen,
                "subset {mask:#06b} reached the wire in the set's own order, \
                 so one picture has more than one byte string",
            );
        }
    }

    // ── Retention (WB-4) ──────────────────────────────────────────────────
    //
    // These fixtures date their windows off the REAL wall clock, because the
    // departure mark and the eviction clock are `chrono::Utc::now()` inside
    // `apply_fetch_result` — a pinned calendar date would sit a day or more
    // past `RETAINED_HOURS` and be evicted at the first merge.

    /// `alert()` with a parsed window at an offset from now, in hours.
    fn windowed_alert(id: &str, from_hours_ago: i64, until_hours_ago: i64) -> NwsAlert {
        let now = chrono::Utc::now().naive_utc();
        let mut a = alert(id, "Tornado Warning");
        a.valid_from = Some(now - chrono::Duration::hours(from_hours_ago));
        a.valid_until = Some(now - chrono::Duration::hours(until_hours_ago));
        a
    }

    /// **The WB-4 floor: an alert that left the active feed still draws at an
    /// instant inside its window.** Under wholesale replacement the second
    /// poll erased it and backward scrub showed an empty map where a warning
    /// had been in force.
    ///
    /// The other half of the same test: at the LIVE instant the departed
    /// alert is gone — departure caps its visibility, so a cancelled warning
    /// does not linger on a live pane until its printed expiry.
    #[test]
    fn an_alert_that_left_the_feed_still_draws_inside_its_window() {
        // In force from 2 h ago until 1 h from now; departs on the second poll.
        let mut h = handler_with(vec![
            windowed_alert("urn:departing", 2, -1),
            windowed_alert("urn:staying", 2, -1),
        ]);
        h.apply_fetch_result(
            whole(vec![windowed_alert("urn:staying", 2, -1)]),
            &PaneRef::across(&[]),
        );
        let now = chrono::Utc::now().naive_utc();

        let an_hour_ago = now - chrono::Duration::hours(1);
        assert_eq!(
            rows_at(&h, an_hour_ago),
            vec!["urn:staying".to_owned(), "urn:departing".to_owned()],
            "an instant inside the departed alert's window must still show \
             it (feed rows first, retained history after)",
        );
        assert_eq!(
            rows_at(&h, now),
            vec!["urn:staying".to_owned()],
            "at the live instant the departed alert must be gone - departure \
             caps its window",
        );
        assert_eq!(
            h.item_count(&PaneRef::bare(0)),
            1,
            "every surface but the as-of paint sees only the active feed",
        );
        assert_eq!(h.drawn_count(&h.defaults), 1);
    }

    /// **The control that makes retention bounded**: an alert whose window
    /// closed more than `RETAINED_HOURS` ago is GONE from the handler
    /// entirely — so "retain everything forever" fails here, and the floor
    /// above fails under wholesale replacement. One eviction, one retention,
    /// same merge.
    #[test]
    fn an_alert_a_day_past_its_window_is_evicted_a_recent_one_is_kept() {
        let mut h = handler_with(vec![
            windowed_alert("urn:stale", 30, 25),  // ended 25 h ago
            windowed_alert("urn:recent", 30, 23), // ended 23 h ago
            windowed_alert("urn:staying", 2, -1),
        ]);
        h.apply_fetch_result(
            whole(vec![windowed_alert("urn:staying", 2, -1)]),
            &PaneRef::across(&[]),
        );

        assert!(
            h.state.data.iter().all(|i| i.alert.id != "urn:stale"),
            "an alert {RETAINED_HOURS} h past its window must be evicted, \
             not retained forever",
        );
        assert!(
            h.state.data.iter().any(|i| i.alert.id == "urn:recent"),
            "an alert still inside the retention margin must survive the \
             same merge - otherwise this eviction test would also pass by \
             evicting everything",
        );
        let inside_recent_window = chrono::Utc::now().naive_utc() - chrono::Duration::hours(24);
        assert_eq!(
            rows_at(&h, inside_recent_window),
            vec!["urn:recent".to_owned()],
            "and still draw at an instant inside its own window",
        );
    }

    /// **The cross-cutting non-triviality: a LIVE pane is byte-identical.**
    /// A handler that lived through a departure paints, at the live instant,
    /// the same wire bytes as a handler that only ever saw the current feed —
    /// which is the wholesale-replacement picture. Compared over the encoded
    /// job (length and digest of the real bytes), not over a re-statement of
    /// the filter.
    #[test]
    fn a_live_pane_paints_the_bytes_wholesale_replacement_painted() {
        let mut with_history = handler_with(vec![
            windowed_alert("urn:staying", 2, -1),
            windowed_alert("urn:departing", 2, -1),
        ]);
        with_history.apply_fetch_result(
            whole(vec![windowed_alert("urn:staying", 2, -1)]),
            &PaneRef::across(&[]),
        );
        let feed_only = handler_with(vec![windowed_alert("urn:staying", 2, -1)]);

        let now = chrono::Utc::now().naive_utc();
        assert_eq!(
            rows_at(&with_history, now),
            vec!["urn:staying".to_owned()],
            "non-triviality floor: the live picture has a row in it",
        );
        assert_eq!(
            encoded_job(&with_history, now),
            encoded_job(&feed_only, now),
            "retention leaked into the live pane's bytes",
        );
    }
}
