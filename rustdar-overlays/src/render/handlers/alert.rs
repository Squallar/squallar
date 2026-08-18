use std::any::Any;
use std::collections::HashSet;
use std::sync::Arc;

use crate::fetch_policy::Assembled;
use crate::nws::alert::{AlertCategory, NwsAlert};
use crate::render::controls::{
    ControlButton, ControlEffect, ControlItem, ControlUpdate, ControlValue, PaneControlContext,
    PaneControlContextMut,
};
use crate::render::overlay_state::{
    ClickableItem, FetchConfig, FetchPayload, FetchTask, HandlerJobInput, OverlayHandler,
    OverlayItem, OverlayKind, OverlayState, PopupAction, PopupActionKind, PopupContent,
    PopupSection, RasterizeContext, RenderMode,
};
use crate::render::rasterize;

/// `pub`, not `pub(crate)`: `rustdar-frontend`'s described-job dispatch tests
/// seed a live registry with alerts through `apply_fetch_result`, which takes
/// a type-erased payload the handler downcasts to exactly this — so the type
/// has to be nameable where the test constructs it.
pub struct NwsAlertFetchResult(
    pub Result<crate::nws::fetch::ActiveAlerts, crate::fetch_policy::FetchError>,
);

/// [`Assembled`]: the national alert feed is one request, and the UGC zone
/// boundaries most alerts reference instead of carrying geometry are one
/// request **each** — a thousand or more on a busy day, any of which can fail
/// on its own. `ActiveAlerts::zones` is the report of what that pass managed,
/// and the layer holding this round has no `set_data` left to ignore it with.
///
/// Observed before it did: **212 of 297 warnings absent from the map under a
/// fully green status line.**
///
/// # That number's causes are closed, and this declaration is not thereby dead
///
/// The zone-geometry measurement then put every cause against all 11,651
/// published NWS zones plus two live rounds, and found the two real ones were
/// **ours**: 227 zones served as a `GeometryCollection` the parser did not
/// know, and a simplifier eating 26,963 of 44,579 polygon parts. Its top row is
/// the one to read here — [`Http`], [`Unreachable`] and [`Unreadable`] fired
/// **0** times, twice — and set beside the paragraph above, it says the
/// opposite of what that paragraph says.
///
/// Both are true. A round is assembled because its parts **can** fail
/// separately, never because they were failing on the day somebody measured.
/// Zero across two rounds from one machine is not a property of a thousand
/// independent requests over a network; the 212-of-297 event was itself
/// 198 × `Http(503)`, which is the row now reading zero; and the web build
/// reaches these same URLs through CORS, where a refusal arrives as
/// `Unreachable` wearing the browser's opaque `TypeError`.
///
/// What the zone work changed is how *often* this report comes back non-empty,
/// which was never the argument for keeping it. What would change the shape is
/// the zone pass ceasing to be one request per zone.
///
/// [`Assembled`]: crate::fetch_policy::Assembled
/// [`Http`]: crate::nws::zones::ZoneFailure::Http
/// [`Unreachable`]: crate::nws::zones::ZoneFailure::Unreachable
/// [`Unreadable`]: crate::nws::zones::ZoneFailure::Unreadable
impl crate::fetch_policy::FetchRound for NwsAlertFetchResult {
    type Shape = crate::fetch_policy::Assembled;
}

#[derive(Debug)]
pub(crate) struct AlertItem {
    pub alert: NwsAlert,
}

impl OverlayItem for AlertItem {
    fn kind(&self) -> OverlayKind {
        OverlayKind::NwsAlerts
    }

    fn popup_content(&self, prefs: &rustdar_units::UserPreferences) -> PopupContent {
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
            // The CAP triple, parsed since the beginning and never shown
            // until now. `Debug` is the variant name — the CAP vocabulary
            // itself ("Severe", "Immediate", "Observed") — and an alert whose
            // value the parser did not recognise honestly reads "Unknown".
            ("Severity".into(), format!("{:?}", alert.severity)),
            ("Urgency".into(), format!("{:?}", alert.urgency)),
            ("Certainty".into(), format!("{:?}", alert.certainty)),
        ];
        // Onset and ends are optional in the feed; a row is added only where
        // the alert carries one — unlike the CAP triple, which every alert
        // has, absence here is the alert's own shape rather than a gap.
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

pub(crate) struct NwsAlertHandler {
    pub state: OverlayState<Vec<Arc<AlertItem>>, Assembled>,
    /// User-dismissed alert IDs. Pruned on refetch so an ID reused upstream
    /// does not stay hidden forever.
    pub hidden_alerts: HashSet<String>,
    /// Empty means the whole overlay is off — see `is_enabled`.
    pub enabled_categories: HashSet<AlertCategory>,
}

impl NwsAlertHandler {
    pub fn new() -> Self {
        Self {
            state: OverlayState::new(),
            hidden_alerts: HashSet::new(),
            enabled_categories: AlertCategory::ALL.into_iter().collect(),
        }
    }

    /// Whether this alert would paint: its category is on and the user has not
    /// hidden it. The one filter, so the count, the signature, the status line
    /// and the clickable set cannot drift apart.
    fn is_drawn(&self, item: &AlertItem) -> bool {
        self.enabled_categories.contains(&item.alert.category)
            && !self.hidden_alerts.contains(&item.alert.id)
    }

    /// How many alerts the user's filters let through — allocation-free, unlike
    /// counting `clickable_items()`, which builds a `Vec` and an `Arc` per
    /// alert for a number.
    ///
    /// Not the same as [`painted_count`](NwsAlertHandler::painted_count), and
    /// the difference is the bug: an alert whose zone boundaries did not resolve
    /// is let through by every filter and paints nothing.
    fn drawn_count(&self) -> usize {
        self.state
            .data
            .iter()
            .filter(|item| self.is_drawn(item))
            .count()
    }

    /// How many alerts actually put ink on the map: let through *and* holding
    /// geometry.
    ///
    /// A zone-based alert whose boundaries all failed to resolve keeps its place
    /// in the list — it is not dropped and nothing is invented for it — so it is
    /// `is_drawn` and paints nothing. Counting it as shown is what let the row
    /// read `297 shown` over a map with 85 warnings on it.
    fn painted_count(&self) -> usize {
        self.state
            .data
            .iter()
            .filter(|item| self.is_drawn(item) && !item.alert.features.is_empty())
            .count()
    }

    /// What the rasterizer reads, captured once — the **one** builder
    /// `prepare_job` answers from, kept a private helper so a second dispatch
    /// path could not quietly capture different state.
    ///
    /// The rows are [`rasterize::AlertPaint`], not whole [`NwsAlert`]s: the
    /// id, the category and the geometry are everything the raster reads, and
    /// the described job serialises this struct onto a message port — prose
    /// fields the raster never draws would be bytes on the wire per raster.
    ///
    /// The geometry rides an `Arc` from parse time, so the features column of
    /// a row is a refcount bump — this method runs per raster dispatch on the
    /// frame thread, and the national feed is thousands of rings on an active
    /// day.
    fn paint_input(&self, ctx: &RasterizeContext) -> Option<rasterize::AlertsInput> {
        if self.state.data.is_empty() {
            return None;
        }
        Some(rasterize::AlertsInput {
            alerts: self
                .state
                .data
                .iter()
                .map(|i| rasterize::AlertPaint {
                    id: i.alert.id.clone(),
                    category: i.alert.category,
                    features: Arc::clone(&i.alert.features),
                })
                .collect(),
            enabled_categories: self.enabled_categories.iter().copied().collect(),
            hidden_ids: self.hidden_alerts.clone(),
            device_scale: ctx.device_scale,
        })
    }
}

impl OverlayHandler for NwsAlertHandler {
    fn kind(&self) -> OverlayKind {
        OverlayKind::NwsAlerts
    }

    fn display_name(&self) -> &str {
        "NWS Alerts"
    }

    fn render_mode(&self) -> RenderMode {
        RenderMode::Texture
    }

    fn default_enabled(&self) -> bool {
        true
    }

    fn is_enabled(&self) -> bool {
        !self.enabled_categories.is_empty()
    }

    /// The master toggle over a layer whose "enabled" is really a category
    /// set. Off clears the set; on restores [`AlertCategory::ALL`] **only when
    /// the set is empty**, so flipping the master off and on loses the user's
    /// subset — accepted, because remembering it would be a shadow copy of
    /// `enabled_categories` that persistence and the category toggles would
    /// both have to keep honest.
    fn set_enabled(&mut self, enabled: bool) {
        let was = self.is_enabled();
        if enabled {
            if self.enabled_categories.is_empty() {
                self.enabled_categories.extend(AlertCategory::ALL);
            }
        } else {
            self.enabled_categories.clear();
        }
        // The drawn set changed, so cached textures must know — the same bump
        // the per-category toggles make in `apply_control`.
        if was != self.is_enabled() {
            self.state.data_generation = self.state.data_generation.wrapping_add(1);
        }
    }

    /// E.g. `"3 shown - W/Wa/Adv/Oth"`: how many alerts are on the map, and which
    /// categories are letting them through. Counted through
    /// [`painted_count`] and [`drawn_count`], not by taking the length of
    /// `clickable_items`, which builds a `Vec` and an `Arc` per alert for a
    /// number this reads straight off the data.
    ///
    /// **`"85 of 297 shown"`** when the two disagree, which is the honest
    /// reading of a poll whose zone boundaries did not all resolve. It used to
    /// say `297 shown` there: every one of those alerts passed every filter, and
    /// 212 of them had no shape to put on the map. The registry adds the
    /// `! incomplete` mark ahead of this line and the reasons behind the layer's
    /// options; the split count is the part that is countable at a glance.
    ///
    /// [`drawn_count`]: NwsAlertHandler::drawn_count
    /// [`painted_count`]: NwsAlertHandler::painted_count
    fn status_line(&self) -> Option<String> {
        if !self.is_enabled() {
            return None;
        }
        let allowed = self.drawn_count();
        let painted = self.painted_count();
        let shown = if painted == allowed {
            format!("{allowed}")
        } else {
            format!("{painted} of {allowed}")
        };
        // Walked from `AlertCategory::ALL`, not from a list spelled out here:
        // a hand-written list is what let `Other` be enabled and still go
        // unnamed, and before that unenabled and uncounted.
        let cats: Vec<&str> = AlertCategory::ALL
            .into_iter()
            .filter(|category| self.enabled_categories.contains(category))
            .map(AlertCategory::short_name)
            .collect();
        Some(format!("{shown} shown - {}", cats.join("/")))
    }

    fn data_generation(&self) -> u64 {
        self.state.data_generation
    }

    /// The **warning-set** signature: a fold over the alerts that would draw —
    /// category-enabled and not hidden — rather than the fetch counter. NWS
    /// alerts auto-poll every two minutes and the active set is usually
    /// unchanged, so a consumer keyed on [`data_generation`] would re-render on
    /// every poll for nothing; this token moves when the drawn set moves (a
    /// warning issued, expired, hidden, or a whole category toggled off). XOR
    /// of per-alert hashes, so it is order-free the way a set is, and ids are
    /// unique within a response.
    ///
    /// # The id alone is not the picture, and that made a warning invisible
    ///
    /// Folding only the id was wrong for a **zone-based** alert, and those are
    /// ordinary: [`crate::nws::alert`] admits an alert with `affectedZones` and
    /// no `geometry`, carrying **no features at all**, and
    /// [`crate::nws::zones::resolve_zone_geometries`] fills them in per poll —
    /// silently dropping any zone whose fetch failed, with no retry. So the
    /// same id legitimately arrives drawing nothing on one poll and drawing its
    /// counties on the next. Same id, same drawn count, and before
    /// `features.len()` joined the fold, the same token: the consumer kept its
    /// raster and the warning stayed invisible until the user happened to pan
    /// or zoom. Counting the features also catches the partial case — three of
    /// five zones resolved, then all five.
    ///
    /// What it does **not** distinguish is a same-length set of *different*
    /// zones (one zone's fetch failing as another's recovers). That is a
    /// narrower residue than the bug it closes, and the alternative — hashing
    /// the rings themselves — is a walk over every polygon in the country on a
    /// national-coverage day, on a method whose contract is that it is cheap
    /// enough to call every frame.
    ///
    /// A polygon *moving* under a stable id is not a case at all: `id` is the
    /// NWS per-message URN, and an updated warning arrives as a new message
    /// under a new id.
    ///
    /// [`data_generation`]: OverlayHandler::data_generation
    fn content_signature(&self) -> u64 {
        use std::hash::{DefaultHasher, Hash, Hasher};
        let mut folded = 0u64;
        let mut visible = 0u64;
        for item in &self.state.data {
            if self.is_drawn(item) {
                let mut hasher = DefaultHasher::new();
                item.alert.id.hash(&mut hasher);
                item.alert.features.len().hash(&mut hasher);
                folded ^= hasher.finish();
                visible += 1;
            }
        }
        folded ^ visible.rotate_left(32)
    }

    fn has_data(&self) -> bool {
        !self.state.data.is_empty()
    }

    fn is_fetching(&self) -> bool {
        self.state.fetching
    }

    fn set_fetching(&mut self, fetching: bool) {
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

    fn item_count(&self) -> usize {
        self.state.data.len()
    }

    /// The alerts a click can land on, each borrowing its own polygons.
    ///
    /// The national feed carries one feature per affected zone, so this is
    /// thousands of rings on an active day; they are lent, never copied, and
    /// the call only happens on a frame with a click to resolve.
    fn clickable_items(&self) -> Vec<ClickableItem<'_>> {
        self.state
            .data
            .iter()
            .filter(|item| self.is_drawn(item))
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

    fn apply_fetch_result(&mut self, result: FetchPayload) {
        let Some(fetch) = self.state.downcast_round::<NwsAlertFetchResult>(result) else {
            log::error!("NWS alert handler received unexpected fetch result type");
            return;
        };
        match fetch.0 {
            Ok(fetched) => {
                let crate::nws::fetch::ActiveAlerts { alerts, zones } = fetched;
                log::info!("Received {} NWS alerts", alerts.len());
                let current_ids: HashSet<String> = alerts.iter().map(|a| a.id.clone()).collect();
                self.hidden_alerts.retain(|id| current_ids.contains(id));
                let items = alerts
                    .into_iter()
                    .map(|alert| Arc::new(AlertItem { alert }))
                    .collect();
                // The coverage report travels with the data it describes: a
                // round that placed 85 of 297 warnings is a good answer, keeps
                // its fresh clock, and is still marked for the 212 it did not.
                self.state
                    .set_data_with_coverage(items, zones.completeness());
            }
            Err(e) => {
                log::error!("NWS alerts fetch failed: {e}");
                self.state.record_failure(&e);
            }
        }
    }

    fn retain_selections(&self, selections: &mut Vec<Arc<dyn OverlayItem>>) {
        selections.retain(|sel| {
            if sel.kind() != OverlayKind::NwsAlerts {
                return true;
            }
            self.state
                .data
                .iter()
                .any(|item| item.matches(sel.as_ref()))
        });
    }

    fn prepare_job(&self, ctx: &RasterizeContext) -> Option<HandlerJobInput> {
        self.paint_input(ctx).map(HandlerJobInput::Alerts)
    }

    fn create_fetch_tasks(&self, ctx: &FetchConfig) -> Vec<FetchTask> {
        log::info!("Fetching NWS active alerts");
        let client = ctx.client.clone();
        let sources = ctx.sources.clone();
        let zone_cache = ctx.zone_cache_dir.clone();
        vec![FetchTask {
            kind: OverlayKind::NwsAlerts,
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

    fn controls(&self, _ctx: &PaneControlContext<'_>) -> Vec<ControlItem> {
        let mut items = vec![ControlItem::Heading {
            text: "NWS Alerts".into(),
        }];
        // One toggle per category, walked from `AlertCategory::ALL` rather
        // than written out: a category with no toggle is a category the user
        // cannot turn on, and `Other` had none.
        items.extend(
            AlertCategory::ALL
                .into_iter()
                .map(|category| ControlItem::Toggle {
                    id: category.control_id(),
                    label: category.plural_label().into(),
                    enabled: self.enabled_categories.contains(&category),
                }),
        );

        // Ungated on enabled (the every-option rule, M9.1): a hidden
        // layer's options stay visible and editable - edits take effect
        // when the eye shows it again - Refresh still fetches (nothing
        // on the fetch path reads enabled), and the status lines keep
        // reporting.
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
        if self.has_data() {
            // `drawn_count`, not `clickable_items().len()`: the inspector
            // panel rebuilds its controls every frame it is open, and the old
            // spelling allocated a `Vec` and an `Arc` per alert to read a
            // length off it.
            let allowed = self.drawn_count();
            let painted = self.painted_count();
            items.push(ControlItem::InfoText {
                text: if painted == allowed {
                    format!("{allowed} alerts shown")
                } else {
                    // The same split the stack row carries. The completeness
                    // note above says why; this says how many, in the same
                    // words as the row that sent the user here.
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

    fn apply_control(
        &mut self,
        update: &ControlUpdate,
        _ctx: &mut PaneControlContextMut<'_>,
    ) -> ControlEffect {
        if update.id == "refresh" {
            return ControlEffect::Fetch;
        }
        // Resolved through `AlertCategory::from_control_id`, so the set of
        // toggles this accepts is the same enumeration `controls` offers. The
        // two used to be independent lists of three ids each, which is how a
        // fourth category ended up with no way to turn it on.
        let Some(category) = AlertCategory::from_control_id(update.id) else {
            return ControlEffect::None;
        };
        if let ControlValue::Bool(enabled) = update.value {
            let was_enabled = self.is_enabled();
            if enabled {
                self.enabled_categories.insert(category);
            } else {
                self.enabled_categories.remove(&category);
            }
            self.state.data_generation = self.state.data_generation.wrapping_add(1);
            if !was_enabled
                && self.is_enabled()
                && self.state.enable_should_refetch(self.has_data())
            {
                return ControlEffect::Fetch;
            }
        }
        ControlEffect::None
    }

    /// `known_categories` records which categories *this build offered a
    /// toggle for*, beside the ones the user left on. It exists so
    /// [`deserialize_state`] can tell "the user turned this off" apart from
    /// "the build that saved this had no way to turn it on" — see there.
    ///
    /// [`deserialize_state`]: OverlayHandler::deserialize_state
    fn serialize_state(&self) -> serde_json::Value {
        // In `ALL`'s declaration order, never the `HashSet`'s: a set's
        // iteration order is per-instance noise, so writing it raw makes
        // save→load→save produce a *different file* every reopen — the
        // instability the config fixpoint test
        // (`a_current_config_reaches_its_save_fixpoint_in_one_round_trip`)
        // exists to catch. The value is a set either way; only the wire
        // order is pinned here.
        let enabled: Vec<AlertCategory> = AlertCategory::ALL
            .into_iter()
            .filter(|category| self.enabled_categories.contains(category))
            .collect();
        serde_json::json!({
            "enabled_categories": enabled,
            "known_categories": AlertCategory::ALL,
        })
    }

    /// A category the saving build never offered is **not** a category the
    /// user declined, so it comes back on.
    ///
    /// Without this, the fix for the unreachable `Other` category would have
    /// reached only new installs: every existing user carries a persisted set
    /// of exactly three, saved by a build whose control list had three
    /// toggles, and restoring it verbatim would keep every Air Quality Alert
    /// off their map forever while the new toggle sat there reading "off" as
    /// though they had chosen it.
    ///
    /// An **empty** set is left alone: that is the master toggle off, a
    /// deliberate state, and widening it would turn the whole layer back on.
    fn deserialize_state(&mut self, value: serde_json::Value) {
        if let Some(cats) = value
            .get("enabled_categories")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
        {
            self.enabled_categories = cats;
            let known: HashSet<AlertCategory> = value
                .get("known_categories")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_default();
            if !self.enabled_categories.is_empty() {
                self.enabled_categories.extend(
                    AlertCategory::ALL
                        .into_iter()
                        .filter(|c| !known.contains(c)),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nws::fetch::ActiveAlerts;
    use crate::nws::zones::ZoneResolution;
    use crate::types::{HatchPattern, OverlayFeature};

    /// A minimal alert with geometry, identified by `id`.
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

    /// One good round of alerts that asked nothing of zone resolution — every
    /// alert carrying its own geometry.
    fn whole(alerts: Vec<NwsAlert>) -> FetchPayload {
        Box::new(NwsAlertFetchResult(Ok(ActiveAlerts::whole(alerts))))
    }

    /// A round whose zone resolution came up short, exactly as the fetch path
    /// delivers one: the alerts that did resolve, beside the report of what did
    /// not.
    fn with_zones(alerts: Vec<NwsAlert>, zones: ZoneResolution) -> FetchPayload {
        Box::new(NwsAlertFetchResult(Ok(ActiveAlerts { alerts, zones })))
    }

    fn handler_with(alerts: Vec<NwsAlert>) -> NwsAlertHandler {
        let mut handler = NwsAlertHandler::new();
        handler.apply_fetch_result(whole(alerts));
        handler
    }

    /// A **zone-based** alert exactly as the parser admits one: `affectedZones`
    /// listed, no `geometry`, and `zone_count` features — which is `0` on a
    /// poll whose zone fetches all failed, and one per zone once they resolve.
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
    /// or it stays invisible.**
    ///
    /// The id alone cannot see this and the drawn count cannot either.
    /// `nws::alert` admits a zone-based warning with no geometry and no
    /// features; `nws::zones::resolve_zone_geometries` fills them per poll and
    /// silently drops a zone whose fetch failed, with no retry. So poll *N* is
    /// the same id drawing nothing and poll *N+1* is the same id drawing three
    /// counties — one alert either way, `is_drawn` true either way. A consumer
    /// keyed on a token that did not move keeps its raster, and the warning is
    /// absent from the map until the user happens to pan or zoom.
    ///
    /// The partial recovery is asserted too: two of three zones is not the same
    /// picture as three of three, and the fetch really does deliver that.
    #[test]
    fn a_warning_that_gains_its_zone_polygons_moves_the_signature() {
        let mut handler = handler_with(vec![zone_alert("a", "Tornado Warning", 0)]);
        let unresolved = handler.content_signature();
        assert_eq!(
            handler.drawn_count(),
            1,
            "fixture: the alert must count as drawn with no features, or the \
             count would move on its own and this proves nothing",
        );

        handler.apply_fetch_result(whole(vec![zone_alert("a", "Tornado Warning", 3)]));
        let resolved = handler.content_signature();
        assert_eq!(
            handler.drawn_count(),
            1,
            "fixture: still one drawn alert, so only the geometry moved",
        );
        assert_ne!(
            resolved, unresolved,
            "a warning arrived with its counties and the token did not move, so \
             nothing re-rasterizes and the warning stays off the map",
        );

        // And the partial case: some zones resolved, then the rest.
        handler.apply_fetch_result(whole(vec![zone_alert("a", "Tornado Warning", 2)]));
        assert_ne!(
            handler.content_signature(),
            resolved,
            "two of three zones is not the picture three of three is",
        );
    }

    /// The signature names the **set**, not the fetch: a refetch returning
    /// the same warning ids must keep it, which is exactly what
    /// `data_generation` — bumped on every `set_data` — cannot do. This is
    /// the mutation the method exists to be different from: swap the body
    /// for `self.data_generation()` and this test fails on the second
    /// fetch.
    #[test]
    fn a_refetch_of_the_same_warning_set_keeps_the_signature() {
        let mut handler = handler_with(vec![alert("a", "Tornado Warning")]);
        let first = handler.content_signature();
        let generation_before = handler.data_generation();
        handler.apply_fetch_result(whole(vec![alert("a", "Tornado Warning")]));
        assert_ne!(
            handler.data_generation(),
            generation_before,
            "fixture: the refetch really did bump the generation",
        );
        assert_eq!(
            handler.content_signature(),
            first,
            "an unchanged warning set across a poll must keep its signature",
        );
    }

    /// Every way the drawn set can change moves the signature: a new
    /// warning, an expiry, a hide, a category turned off.
    #[test]
    fn every_change_to_the_drawn_set_moves_the_signature() {
        let mut handler = handler_with(vec![alert("a", "Tornado Warning")]);
        let one_warning = handler.content_signature();

        // A second warning issues mid-session.
        handler.apply_fetch_result(whole(vec![
            alert("a", "Tornado Warning"),
            alert("b", "Severe Thunderstorm Warning"),
        ]));
        let two_warnings = handler.content_signature();
        assert_ne!(two_warnings, one_warning, "a new warning must move it");

        // The first expires out of the feed.
        handler.apply_fetch_result(whole(vec![alert("b", "Severe Thunderstorm Warning")]));
        let b_only = handler.content_signature();
        assert_ne!(b_only, two_warnings, "an expiry must move it");
        assert_ne!(
            b_only, one_warning,
            "a different single warning is a different set",
        );

        // The user hides the survivor.
        handler.hidden_alerts.insert("b".to_string());
        assert_ne!(
            handler.content_signature(),
            b_only,
            "hiding an alert must move it",
        );
        handler.hidden_alerts.clear();

        // The whole category goes off.
        handler.enabled_categories.remove(&AlertCategory::Warning);
        assert_ne!(
            handler.content_signature(),
            b_only,
            "disabling the category must move it",
        );
    }

    /// **`shown` means on the map, not past the filters.**
    ///
    /// The two used to be the same number and they are not: an alert whose zone
    /// boundaries did not resolve is let through by every filter and paints
    /// nothing. Counting it as shown is what let this row read `297 shown` over
    /// a map with 85 warnings on it — a lie assembled entirely out of true
    /// statements about the feed.
    ///
    /// The split spelling appears only when the two disagree, so a healthy layer
    /// still reads `2 shown` rather than the arithmetic `2 of 2`.
    #[test]
    fn the_status_line_splits_what_is_on_the_map_from_what_passed_the_filters() {
        let mut handler = NwsAlertHandler::new();
        handler.apply_fetch_result(with_zones(
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
        ));
        assert_eq!(
            handler.drawn_count(),
            3,
            "premise: every filter lets all three through",
        );
        assert_eq!(
            handler.status_line().as_deref(),
            Some("1 of 3 shown - W/Wa/Adv/Oth"),
            "two alerts with no shape must not be counted as shown",
        );

        // The user hides the one alert that *is* drawn: now nothing is on the
        // map and only two alerts are even eligible.
        handler.hidden_alerts.insert("a".to_string());
        assert_eq!(
            handler.status_line().as_deref(),
            Some("0 of 2 shown - W/Wa/Adv/Oth"),
            "both numbers must move with the filters, not just the denominator",
        );

        // And once the boundaries arrive, the split spelling goes away.
        handler.hidden_alerts.clear();
        handler.apply_fetch_result(whole(vec![zone_alert("a", "Tornado Warning", 3)]));
        assert_eq!(
            handler.status_line().as_deref(),
            Some("1 shown - W/Wa/Adv/Oth")
        );
    }

    /// The status line counts what would *draw* — category-filtered and
    /// hide-filtered — and names the categories letting it, so the row under
    /// "NWS Alerts" reads as the map's own state rather than the feed's.
    #[test]
    fn the_status_line_counts_the_drawn_set_and_names_the_categories() {
        let mut handler = handler_with(vec![
            alert("a", "Tornado Warning"),
            alert("b", "Severe Thunderstorm Warning"),
        ]);
        assert_eq!(
            handler.status_line().as_deref(),
            Some("2 shown - W/Wa/Adv/Oth")
        );

        handler.hidden_alerts.insert("b".to_string());
        assert_eq!(
            handler.status_line().as_deref(),
            Some("1 shown - W/Wa/Adv/Oth"),
            "a hidden alert is not shown, so it must not be counted as shown"
        );

        handler.enabled_categories.remove(&AlertCategory::Advisory);
        handler.enabled_categories.remove(&AlertCategory::Watch);
        assert_eq!(handler.status_line().as_deref(), Some("1 shown - W/Oth"));

        handler.enabled_categories.remove(&AlertCategory::Other);
        assert_eq!(handler.status_line().as_deref(), Some("1 shown - W"));

        handler.enabled_categories.clear();
        assert_eq!(
            handler.status_line(),
            None,
            "a disabled layer's dimmed row carries no status line"
        );
    }

    /// The master toggle round-trips through the category set: off clears it,
    /// on restores the defaults — and on over a *partial* set leaves the
    /// user's subset alone, because the layer is already on.
    #[test]
    fn the_master_toggle_clears_and_restores_the_category_set() {
        let mut handler = NwsAlertHandler::new();
        assert!(handler.is_enabled(), "precondition: defaults on");

        handler.set_enabled(false);
        assert!(!handler.is_enabled());
        assert!(handler.enabled_categories.is_empty());

        handler.set_enabled(true);
        assert!(handler.is_enabled());
        assert_eq!(
            handler.enabled_categories.len(),
            AlertCategory::ALL.len(),
            "on from nothing restores every category"
        );

        handler.enabled_categories.remove(&AlertCategory::Advisory);
        handler.set_enabled(true);
        assert_eq!(
            handler.enabled_categories.len(),
            AlertCategory::ALL.len() - 1,
            "on over a live subset must not widen the user's selection"
        );
    }

    /// The popup's grid carries the CAP severity/urgency/certainty triple —
    /// parsed since the beginning, displayed only now — and the optional
    /// onset/ends rows exactly where the alert carries them.
    #[test]
    fn the_popup_grid_carries_the_cap_triple_and_the_optional_times() {
        let mut with_times = alert("a", "Tornado Warning");
        with_times.onset = Some("2026-08-10T18:00:00-05:00".to_string());
        with_times.ends = Some("2026-08-10T19:30:00-05:00".to_string());
        let prefs = rustdar_units::UserPreferences::default();

        let grid = |alert: &NwsAlert| -> Vec<(String, String)> {
            AlertItem {
                alert: alert.clone(),
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
    ///
    /// The panel used to say `clickable_items().len()`, which was the same
    /// number by construction and built a `Vec` and an `Arc` per alert, every
    /// frame the panel was open, to read a length off it. Now that they are
    /// two pieces of code they can disagree, so this is the pin that they do
    /// not.
    #[test]
    fn the_shown_count_is_the_clickable_set() {
        let mut handler = handler_with(vec![
            alert("a", "Tornado Warning"),
            alert("b", "Tornado Watch"),
            alert("c", "Flood Advisory"),
        ]);
        let agree = |h: &NwsAlertHandler, expected: usize, why: &str| {
            assert_eq!(h.drawn_count(), expected, "{why}");
            assert_eq!(
                h.drawn_count(),
                h.clickable_items().len(),
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

        handler.enabled_categories.remove(&AlertCategory::Advisory);
        agree(
            &handler,
            1,
            "a category turned off takes its alerts with it",
        );

        handler.enabled_categories.clear();
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

    /// **Every product NWS can send must be paintable by a fresh install.**
    ///
    /// This is the check that was missing, and it is deliberately driven by
    /// *NWS's* enumeration rather than by `AlertCategory::ALL`: asserting that
    /// our four variants are all enabled would only assert that our list
    /// equals our list, and the defect lived precisely in the gap between the
    /// enum and the set of toggles somebody wrote out by hand.
    ///
    /// Before the fix, 11 of the 111 published event types classified as
    /// `Other` — Air Quality Alert, Civil Emergency Message, Evacuation
    /// Immediate, Child Abduction Emergency, Blue Alert, Local Area Emergency,
    /// 911 Telephone Outage, Administrative Message, Extreme Fire Danger,
    /// Short Term Forecast, Test — and no code path anywhere inserted `Other`
    /// into `enabled_categories`, so all 11 were unpaintable by construction.
    /// In the live sample that found it, 25 of 271 active alerts were Air
    /// Quality Alerts.
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
            .filter(|(_, category)| !handler.enabled_categories.contains(category))
            .collect();
        assert!(
            orphaned.is_empty(),
            "these NWS products classify into a category no default install \
             enables, so they can never be drawn: {orphaned:?}",
        );
    }

    /// **Every category must have a toggle, and the toggle must work.**
    ///
    /// The other half of the same defect: `Other` had no `ControlItem::Toggle`
    /// and `apply_control` accepted no id that resolved to it, so even a user
    /// who knew alerts were missing had no way to ask for them. Driving both
    /// the control list and the handler from `AlertCategory::ALL` is what
    /// makes that unrepresentable; this pins it.
    #[test]
    fn every_category_has_a_toggle_that_turns_it_on_and_off() {
        let mut handler = NwsAlertHandler::new();
        let offered: Vec<&str> = handler
            .controls(&PaneControlContext {
                pane_idx: 0,
                pane_state: None,
            })
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

            let mut ctx = PaneControlContextMut {
                pane_idx: 0,
                pane_state: None,
            };
            handler.apply_control(
                &ControlUpdate {
                    id: category.control_id(),
                    value: ControlValue::Bool(false),
                },
                &mut ctx,
            );
            assert!(
                !handler.enabled_categories.contains(&category),
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
                handler.enabled_categories.contains(&category),
                "{category}'s toggle did not turn it back on",
            );
        }
    }

    /// **The count that would have revealed the gap excluded it too.**
    ///
    /// `drawn_count` and `painted_count` both filter on `is_drawn`, which was
    /// permanently false for `Other`, so the status line reported a confident
    /// number that was wrong by exactly the set the user could not see. An Air
    /// Quality Alert now counts and is clickable like any other product, and
    /// turning its category off takes it out of both.
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
            handler.drawn_count(),
            2,
            "the air quality alert is missing from the count that would have \
             shown it missing from the map",
        );
        assert_eq!(handler.clickable_items().len(), 2);
        assert_eq!(
            handler.status_line().as_deref(),
            Some("2 shown - W/Wa/Adv/Oth"),
        );

        // And it is a real filter, not an unconditional pass.
        handler.enabled_categories.remove(&AlertCategory::Other);
        assert_eq!(handler.drawn_count(), 1);
        assert_eq!(handler.status_line().as_deref(), Some("1 shown - W/Wa/Adv"));
    }

    /// A set persisted by a build that had three toggles is not a user who
    /// turned the fourth off — they were never offered it — so restoring it
    /// must not carry the invisible category forward. Without this the fix
    /// reaches new installs only, and every existing user keeps a map with no
    /// air quality alerts on it.
    ///
    /// An empty set is the master toggle off and stays off.
    #[test]
    fn a_category_the_saved_build_never_offered_comes_back_on() {
        let legacy = serde_json::json!({
            "enabled_categories": ["Warning", "Watch", "Advisory"],
        });
        let mut handler = NwsAlertHandler::new();
        handler.deserialize_state(legacy);
        assert!(
            handler.enabled_categories.contains(&AlertCategory::Other),
            "a category with no toggle in the saving build was never declined",
        );

        // A choice this build *did* offer is honoured.
        let mut handler = NwsAlertHandler::new();
        handler.deserialize_state(serde_json::json!({
            "enabled_categories": ["Warning"],
            "known_categories": AlertCategory::ALL,
        }));
        assert_eq!(
            handler.enabled_categories,
            HashSet::from([AlertCategory::Warning]),
            "the user turned three categories off and they must stay off",
        );

        // The master toggle off stays off.
        let mut handler = NwsAlertHandler::new();
        handler.deserialize_state(serde_json::json!({ "enabled_categories": [] }));
        assert!(
            handler.enabled_categories.is_empty(),
            "an empty set is a deliberate state, not a build to migrate",
        );
    }

    /// The fold is order-free: the same set in another order is the same
    /// signature — feed order is not part of what gets drawn.
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
        assert_eq!(forward.content_signature(), backward.content_signature());
    }
}
