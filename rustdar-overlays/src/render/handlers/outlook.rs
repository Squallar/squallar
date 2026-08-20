use crate::render::overlay_state::{PaneMut, PaneRef};
use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::fetch_policy::Assembled;
use crate::render::controls::{
    ControlButton, ControlEffect, ControlItem, ControlUpdate, ControlValue,
};
use crate::render::overlay_state::Surface;
use crate::render::overlay_state::{
    ClickableItem, FetchConfig, FetchPayload, FetchTask, OverlayHandler, OverlayItem, OverlayState,
    PopupContent, PopupSection, RasterizeContext, RenderMode,
};
use crate::render::rasterize;
use crate::spc::outlook::{OutlookDay, OutlookProduct, SpcOutlook};
use rustdar_source::id::{LayerId, known};
use rustdar_source::job::{DescribedJob, JobCodec};

pub struct SpcOutlookFetchResult {
    pub day: OutlookDay,
    pub product: OutlookProduct,
    pub result: Result<SpcOutlook, crate::fetch_policy::FetchError>,
}
impl crate::fetch_policy::FetchRound for SpcOutlookFetchResult {
    type Shape = crate::fetch_policy::Assembled;
}

#[derive(Debug)]
pub(crate) struct OutlookItem {
    pub label: String,
    pub day: OutlookDay,
    pub product: OutlookProduct,
    pub valid: Option<chrono::NaiveDateTime>,
    pub expire: Option<chrono::NaiveDateTime>,
}

/// The SPC page that shows `day`'s outlook — the popup's "Open on SPC
/// website" target.
fn outlook_page_url(day: OutlookDay) -> String {
    if day.is_extended() {
        "https://www.spc.noaa.gov/products/exper/day4-8/".to_owned()
    } else {
        format!(
            "https://www.spc.noaa.gov/products/outlook/day{}otlk.html",
            day.label()
        )
    }
}

impl OverlayItem for OutlookItem {
    fn layer_id(&self) -> LayerId {
        known::SPC_OUTLOOK
    }

    fn popup_content(&self, prefs: &rustdar_units::UserPreferences) -> PopupContent {
        let time = |t: Option<chrono::NaiveDateTime>| match t {
            Some(t) => prefs.timezone.format_naive_utc(t, "%b %d %Y %H:%M"),
            None => "Unknown".to_owned(),
        };
        PopupContent {
            title: format!("SPC Day {} {} Outlook", self.day.label(), self.product),
            accent_rgb: [200, 200, 100],
            width: 300.0,
            sections: vec![
                PopupSection::Heading(self.label.clone()),
                PopupSection::KeyValueGrid(vec![
                    ("Day".into(), self.day.to_string()),
                    ("Product".into(), self.product.to_string()),
                    ("Valid".into(), time(self.valid)),
                    ("Expires".into(), time(self.expire)),
                ]),
                PopupSection::Separator,
                PopupSection::Link {
                    label: "Open on SPC website".into(),
                    url: outlook_page_url(self.day),
                },
            ],
            actions: Vec::new(),
        }
    }

    fn matches(&self, other: &dyn OverlayItem) -> bool {
        other
            .as_any()
            .downcast_ref::<OutlookItem>()
            .is_some_and(|o| {
                o.label == self.label && o.day == self.day && o.product == self.product
            })
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// What one round's answers add up to, before anything is written to the
/// ledger.
enum RoundVerdict {
    /// Nothing the layer asks for is failing.
    Clear,
    /// Nothing failed, and what did answer said "not published right now" —
    /// which is an answer, and resets the ladder rather than climbing it.
    NotPublished(crate::fetch_policy::FetchError),
    /// At least one product the layer asks for did not load.
    Failed(crate::fetch_policy::FetchError),
}

pub(crate) struct SpcOutlookHandler {
    pub state: OverlayState<HashMap<(OutlookDay, OutlookProduct), SpcOutlook>, Assembled>,
    /// Per product, so one product's refetch does not invalidate the others.
    per_product_generation: HashMap<(OutlookDay, OutlookProduct), u64>,
    /// Bumped when day or product set changes without any fetch, which still
    /// changes what gets drawn.
    config_generation: u64,
    /// The last answer per product that was **not** a success, including
    /// [`Absent`](crate::fetch_policy::FetchFailure::Absent) — see
    /// [`Self::round_verdict`], which is what splits the two apart.
    per_product_error: HashMap<(OutlookDay, OutlookProduct), crate::fetch_policy::FetchError>,
    /// How many of this layer's fetch tasks are still in flight.
    outstanding: usize,
    /// Whether anything the layer is **currently** asking for has answered since
    /// the last round verdict. See [`Self::file_round_verdict`].
    round_answered_in_scope: bool,
    /// Failures from the current round for products the layer has stopped
    /// asking for mid-flight. See [`Self::file_round_verdict`].
    round_stray_failures: Vec<crate::fetch_policy::FetchError>,
    pub selected_day: OutlookDay,
    /// Empty means the whole overlay is off — see `is_enabled`.
    pub enabled_products: HashSet<OutlookProduct>,
}

impl SpcOutlookHandler {
    pub fn new() -> Self {
        Self {
            state: OverlayState::new(),
            per_product_generation: HashMap::new(),
            config_generation: 0,
            per_product_error: HashMap::new(),
            outstanding: 0,
            round_answered_in_scope: false,
            round_stray_failures: Vec::new(),
            selected_day: OutlookDay::Day1,
            enabled_products: HashSet::new(),
        }
    }

    /// Move the outstanding-task count, keeping `state.fetching` in step.
    fn set_outstanding(&mut self, outstanding: usize) {
        self.outstanding = outstanding;
        self.state.fetching = outstanding > 0;
    }

    /// Is this key something the layer is asking for *right now*?
    fn in_scope(&self, key: &(OutlookDay, OutlookProduct)) -> bool {
        key.0 == self.selected_day && self.enabled_products.contains(&key.1)
    }

    /// What every product's last answer adds up to, as a **property of the
    /// selection** rather than of the order its tasks resolved in.
    fn round_verdict(&self) -> RoundVerdict {
        let day = self.selected_day;
        let scope: Vec<OutlookProduct> = day
            .products()
            .iter()
            .copied()
            .filter(|p| self.enabled_products.contains(p))
            .collect();
        let asked = scope.len();

        let mut failed: Vec<(OutlookProduct, &crate::fetch_policy::FetchError)> = Vec::new();
        let mut absent: Vec<(OutlookProduct, &crate::fetch_policy::FetchError)> = Vec::new();
        let mut drew = false;
        for product in scope {
            let key = (day, product);
            match self.per_product_error.get(&key) {
                Some(e) if e.failure == crate::fetch_policy::FetchFailure::Absent => {
                    absent.push((product, e));
                }
                Some(e) => failed.push((product, e)),
                None if self.state.data.contains_key(&key) => drew = true,
                None => {}
            }
        }

        let listed = |parts: &[(OutlookProduct, &crate::fetch_policy::FetchError)]| {
            parts
                .iter()
                .map(|(p, e)| format!("{p:?}: {}", e.message))
                .collect::<Vec<_>>()
                .join("; ")
        };

        if !failed.is_empty() {
            return RoundVerdict::Failed(crate::fetch_policy::FetchError {
                failure: crate::fetch_policy::FetchFailure::of_round(
                    failed.iter().map(|(_, e)| e.failure),
                ),
                message: format!(
                    "{} of {asked} outlook products did not load: {}",
                    failed.len(),
                    listed(&failed),
                ),
            });
        }
        if drew {
            return RoundVerdict::Clear;
        }
        if !absent.is_empty() {
            return RoundVerdict::NotPublished(crate::fetch_policy::FetchError::absent(format!(
                "{} of {asked} outlook products are not published right now: {}",
                absent.len(),
                listed(&absent),
            )));
        }
        RoundVerdict::Clear
    }

    /// What of this selection is **not on the map**, as distinct from what is
    /// merely out of date.
    fn round_coverage(&self) -> crate::fetch_policy::DataCompleteness {
        let day = self.selected_day;
        let mut expected = 0;
        let mut missing = 0;
        let mut reasons = Vec::new();
        for &product in day.products() {
            if !self.enabled_products.contains(&product) {
                continue;
            }
            expected += 1;
            let key = (day, product);
            let Some(error) = self.per_product_error.get(&key) else {
                continue;
            };
            if error.failure == crate::fetch_policy::FetchFailure::Absent
                || self.state.data.contains_key(&key)
            {
                continue;
            }
            missing += 1;
            reasons.push((format!("{product:?}: {}", error.message), 1));
        }
        crate::fetch_policy::DataCompleteness {
            expected,
            missing,
            unit: "outlook products",
            reasons,
            ..crate::fetch_policy::DataCompleteness::default()
        }
    }

    /// File the round's verdict on the ledger — **once**, when the last of its
    /// tasks lands.
    fn file_round_verdict(&mut self) {
        let answered = std::mem::take(&mut self.round_answered_in_scope);
        let strays = std::mem::take(&mut self.round_stray_failures);
        if !answered {
            if !strays.is_empty() {
                let merged = crate::fetch_policy::FetchError::of_round(
                    &strays,
                    format!(
                        "{} outlook request(s) failed for products the layer no longer asks for",
                        strays.len(),
                    ),
                );
                self.state.retry.record_failure(&merged);
            }
        } else {
            match self.round_verdict() {
                RoundVerdict::Failed(e) | RoundVerdict::NotPublished(e) => {
                    self.state.retry.record_failure(&e);
                }
                RoundVerdict::Clear => self.state.retry.record_success(),
            }
        }
        let coverage = self.round_coverage();
        self.state.record_coverage(coverage);
    }

    /// Every enabled product's features, concatenated in the order they will be
    /// painted.
    fn features_in_paint_order(&self) -> Vec<crate::types::OverlayFeature> {
        let day = self.selected_day;
        let mut features = Vec::new();
        for &product in day.products() {
            if !self.enabled_products.contains(&product) {
                continue;
            }
            if let Some(outlook) = self.state.data.get(&(day, product)) {
                features.extend(outlook.features.iter().cloned());
            }
        }
        features
    }

    fn paint_input(&self, ctx: &RasterizeContext) -> Option<rasterize::OutlooksInput> {
        let features = self.features_in_paint_order();
        if features.is_empty() {
            return None;
        }
        let hatch_color = if ctx.is_dark {
            [200, 200, 200, 180]
        } else {
            [60, 60, 60, 180]
        };
        Some(rasterize::OutlooksInput {
            features,
            hatch_color,
            device_scale: ctx.device_scale,
        })
    }

    /// Bring the products that are not independently selectable into line with
    /// the ones that are, and drop any that the selected day does not publish.
    fn sync_implied_products(&mut self) {
        let published = self.selected_day.products();
        self.enabled_products
            .retain(|p| p.is_selectable() || published.contains(p));
        for &product in published {
            let Some(parent) = product.implied_by() else {
                continue;
            };
            if self.enabled_products.contains(&parent) {
                self.enabled_products.insert(product);
            } else {
                self.enabled_products.remove(&product);
            }
        }
    }

    /// Drop what is no longer asked for, and take the layer back off the ledger
    /// if nothing that is left is failing.
    fn refile_after_selection_change(&mut self) {
        self.sync_implied_products();
        let day = self.selected_day;
        let enabled = self.enabled_products.clone();
        self.per_product_error
            .retain(|(d, p), _| *d == day && enabled.contains(p));
        if self.outstanding > 0 {
            return;
        }
        match self.round_verdict() {
            RoundVerdict::Failed(_) => {}
            RoundVerdict::NotPublished(e) => self.state.retry.record_failure(&e),
            RoundVerdict::Clear => self.state.retry.clear(),
        }
        let coverage = self.round_coverage();
        self.state.record_coverage(coverage);
    }

    fn combined_generation(&self) -> u64 {
        self.per_product_generation
            .values()
            .sum::<u64>()
            .wrapping_add(self.config_generation)
    }
}

impl OverlayHandler for SpcOutlookHandler {
    fn id(&self) -> LayerId {
        known::SPC_OUTLOOK
    }
    fn surface(&self) -> Surface {
        Surface::Ground
    }
    fn draw_order_weight(&self) -> u32 {
        20
    }

    fn display_name(&self) -> &str {
        "SPC Outlooks"
    }

    fn render_mode(&self) -> RenderMode {
        RenderMode::Texture
    }

    fn theme_sensitive(&self) -> bool {
        true
    }

    fn is_enabled(&self, _pane: &PaneRef<'_>) -> bool {
        !self.enabled_products.is_empty()
    }

    fn set_enabled(&mut self, enabled: bool, _pane: &mut PaneMut<'_>) {
        if enabled {
            if self.enabled_products.is_empty()
                && let Some(&first) = self.selected_day.products().first()
            {
                self.enabled_products.insert(first);
                self.sync_implied_products();
                self.config_generation = self.config_generation.wrapping_add(1);
            }
        } else if !self.enabled_products.is_empty() {
            self.enabled_products.clear();
            self.config_generation = self.config_generation.wrapping_add(1);
        }
    }

    /// E.g. `"Day 1 - Categorical, Tornado"`.
    fn status_line(&self, pane: &PaneRef<'_>) -> Option<String> {
        if !self.is_enabled(pane) {
            return None;
        }
        let products: Vec<String> = self
            .selected_day
            .products()
            .iter()
            .filter(|p| p.is_selectable() && self.enabled_products.contains(p))
            .map(|p| p.to_string())
            .collect();
        Some(format!("{} - {}", self.selected_day, products.join(", ")))
    }

    fn data_generation(&self) -> u64 {
        self.combined_generation()
    }

    /// Data **this selection** can draw, not data this layer has ever fetched.
    fn has_data(&self, _pane: &PaneRef<'_>) -> bool {
        self.enabled_products.iter().any(|product| {
            self.state
                .data
                .get(&(self.selected_day, *product))
                .is_some_and(|outlook| !outlook.features.is_empty())
        })
    }

    fn is_fetching(&self) -> bool {
        self.outstanding > 0
    }

    /// The host says a round has started or been abandoned; this layer's round
    /// is one task per enabled product, so the count moves by that many.
    fn set_fetching(&mut self, fetching: bool, _pane: &PaneRef<'_>) {
        if fetching {
            self.set_outstanding(self.outstanding + self.enabled_products.len().max(1));
        } else {
            self.set_outstanding(0);
            self.round_answered_in_scope = false;
            self.round_stray_failures.clear();
        }
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

    fn item_count(&self, _pane: &PaneRef<'_>) -> usize {
        self.state.data.len()
    }

    fn clickable_items<'a>(&'a self, _pane: &PaneRef<'_>) -> Vec<ClickableItem<'a>> {
        let day = self.selected_day;
        let mut items = Vec::new();
        for &product in &self.enabled_products {
            let Some(outlook) = self.state.data.get(&(day, product)) else {
                continue;
            };
            for feature in &outlook.features {
                items.push(ClickableItem {
                    features: std::slice::from_ref(feature),
                    item: Arc::new(OutlookItem {
                        label: feature.label.clone(),
                        day,
                        product,
                        valid: outlook.valid,
                        expire: outlook.expire,
                    }) as Arc<dyn OverlayItem>,
                });
            }
        }
        items
    }

    fn apply_fetch_result(&mut self, result: FetchPayload) {
        let Some(fetch) = self.state.downcast_round::<SpcOutlookFetchResult>(result) else {
            log::error!("SPC outlook handler received unexpected fetch result type");
            return;
        };
        let key = (fetch.day, fetch.product);
        let in_scope = self.in_scope(&key);
        match fetch.result {
            Ok(outlook) => {
                log::info!("Received SPC outlook: {:?} {:?}", fetch.day, fetch.product);
                self.state.data.insert(key, outlook);
                self.per_product_error.remove(&key);
                self.state.fetch_time = Some(web_time::Instant::now());
                let counter = self.per_product_generation.entry(key).or_insert(0);
                *counter = counter.wrapping_add(1);
            }
            Err(e) if e.failure == crate::fetch_policy::FetchFailure::Absent => {
                log::info!(
                    "SPC outlook not published ({:?} {:?}): {e}",
                    fetch.day,
                    fetch.product
                );
                self.state.fetch_time = Some(web_time::Instant::now());
                if in_scope {
                    self.per_product_error.insert(key, e);
                }
            }
            Err(e) => {
                log::error!(
                    "SPC outlook fetch failed ({:?} {:?}): {e}",
                    fetch.day,
                    fetch.product
                );
                if in_scope {
                    self.per_product_error.insert(key, e);
                } else {
                    self.round_stray_failures.push(e);
                }
            }
        }
        if in_scope {
            self.round_answered_in_scope = true;
        }
        self.set_outstanding(self.outstanding.saturating_sub(1));
        if self.outstanding == 0 {
            self.file_round_verdict();
        }
    }

    fn retain_selections(&self, _selections: &mut Vec<Arc<dyn OverlayItem>>, _pane: &PaneRef<'_>) {
        // Nothing to prune: outlook items match on day, product and label,
        // not on a data ID.
    }

    fn prepare_job(&self, ctx: &RasterizeContext, _pane: &PaneRef<'_>) -> Option<DescribedJob> {
        self.paint_input(ctx).map(DescribedJob::new)
    }

    fn job_codec(&self) -> Option<&'static JobCodec> {
        crate::render::jobs::JOB_CODECS
            .iter()
            .find(|row| row.label == "overlay/outlooks")
    }

    fn create_fetch_tasks(&self, ctx: &FetchConfig, _pane: &PaneRef<'_>) -> Vec<FetchTask> {
        if self.enabled_products.is_empty() {
            return Vec::new();
        }
        let day = self.selected_day;
        let products: Vec<OutlookProduct> = day
            .products()
            .iter()
            .copied()
            .filter(|p| self.enabled_products.contains(p))
            .collect();
        log::info!("Fetching SPC outlooks for {:?}: {:?}", day, products);
        // NOT `ctx.client`: SPC answers OPTIONS with 403, so a `User-Agent`
        // makes every one of these fail in the browser. See `spc::fetch`.
        let client = match crate::spc::fetch::spc_client(&ctx.sources) {
            Ok(c) => c,
            Err(e) => {
                log::error!("{e}");
                return Vec::new();
            }
        };
        products
            .into_iter()
            .map(|product| {
                let client = client.clone();
                let sources = ctx.sources.clone();
                FetchTask {
                    kind: known::SPC_OUTLOOK,
                    future: Box::pin(async move {
                        let result =
                            crate::spc::fetch::fetch_outlook(&client, &sources, day, product).await;
                        Box::new(SpcOutlookFetchResult {
                            day,
                            product,
                            result,
                        }) as FetchPayload
                    }),
                }
            })
            .collect()
    }

    fn controls(&self, _ctx: &PaneRef<'_>) -> Vec<ControlItem> {
        let mut items = vec![ControlItem::Heading {
            text: "SPC Outlooks".into(),
        }];

        let buttons: Vec<ControlButton> = OutlookDay::all()
            .iter()
            .map(|&d| {
                let id: &'static str = match d {
                    OutlookDay::Day1 => "day1",
                    OutlookDay::Day2 => "day2",
                    OutlookDay::Day3 => "day3",
                    OutlookDay::Day4 => "day4",
                    OutlookDay::Day5 => "day5",
                    OutlookDay::Day6 => "day6",
                    OutlookDay::Day7 => "day7",
                    OutlookDay::Day8 => "day8",
                };
                ControlButton {
                    id,
                    label: d.label().to_string(),
                    enabled: true,
                    highlight: d == self.selected_day,
                }
            })
            .collect();
        items.push(ControlItem::ButtonRow { buttons });

        for &product in self
            .selected_day
            .products()
            .iter()
            .filter(|p| p.is_selectable())
        {
            let id: &'static str = match product {
                OutlookProduct::Categorical => "cat",
                OutlookProduct::Tornado => "tor",
                OutlookProduct::Wind => "wind",
                OutlookProduct::Hail => "hail",
                OutlookProduct::Probabilistic => "prob",
                OutlookProduct::ConditionalIntensity => continue,
            };
            items.push(ControlItem::Toggle {
                id,
                label: product.to_string(),
                enabled: self.enabled_products.contains(&product),
            });
        }

        items.push(ControlItem::ButtonRow {
            buttons: vec![ControlButton {
                id: "refresh",
                label: "\u{21bb} Refresh".into(),
                enabled: !self.is_fetching(),
                highlight: false,
            }],
        });
        if self.is_fetching() {
            items.push(ControlItem::InfoText {
                text: "Fetching...".into(),
            });
        }

        items
    }

    fn apply_control(&mut self, update: &ControlUpdate, _ctx: &mut PaneMut<'_>) -> ControlEffect {
        match update.id {
            "day1" | "day2" | "day3" | "day4" | "day5" | "day6" | "day7" | "day8" => {
                let new_day = match update.id {
                    "day1" => OutlookDay::Day1,
                    "day2" => OutlookDay::Day2,
                    "day3" => OutlookDay::Day3,
                    "day4" => OutlookDay::Day4,
                    "day5" => OutlookDay::Day5,
                    "day6" => OutlookDay::Day6,
                    "day7" => OutlookDay::Day7,
                    "day8" => OutlookDay::Day8,
                    _ => return ControlEffect::None,
                };
                if new_day != self.selected_day {
                    self.selected_day = new_day;
                    let valid: HashSet<OutlookProduct> =
                        new_day.products().iter().copied().collect();
                    self.enabled_products.retain(|p| valid.contains(p));
                    self.config_generation = self.config_generation.wrapping_add(1);
                    self.refile_after_selection_change();
                    if !self.enabled_products.is_empty() {
                        return ControlEffect::Fetch;
                    }
                }
                ControlEffect::None
            }
            "cat" | "tor" | "wind" | "hail" | "prob" => {
                let product = match update.id {
                    "cat" => OutlookProduct::Categorical,
                    "tor" => OutlookProduct::Tornado,
                    "wind" => OutlookProduct::Wind,
                    "hail" => OutlookProduct::Hail,
                    "prob" => OutlookProduct::Probabilistic,
                    _ => return ControlEffect::None,
                };
                if let ControlValue::Bool(enabled) = update.value {
                    if enabled {
                        self.enabled_products.insert(product);
                    } else {
                        self.enabled_products.remove(&product);
                    }
                    self.config_generation = self.config_generation.wrapping_add(1);
                    self.refile_after_selection_change();
                    if enabled {
                        return ControlEffect::Fetch;
                    }
                }
                ControlEffect::None
            }
            "refresh" if self.enabled_products.is_empty() => ControlEffect::None,
            "refresh" => ControlEffect::Fetch,
            _ => ControlEffect::None,
        }
    }

    fn serialize_state(&self) -> serde_json::Value {
        // Declaration order, never the `HashSet`'s: set iteration order is per-instance noise.
        let mut enabled: Vec<OutlookProduct> = self.enabled_products.iter().copied().collect();
        enabled.sort_by_key(|product| *product as u8);
        serde_json::json!({
            "selected_day": self.selected_day,
            "enabled_products": enabled,
        })
    }

    fn deserialize_state(&mut self, value: serde_json::Value) {
        if let Some(day) = value
            .get("selected_day")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
        {
            self.selected_day = day;
        }
        if let Some(products) = value
            .get("enabled_products")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
        {
            self.enabled_products = products;
        }
        self.sync_implied_products();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_master_toggle_restores_a_product_the_day_actually_publishes() {
        let mut handler = SpcOutlookHandler::new();
        assert!(
            !handler.is_enabled(&PaneRef::bare(0)),
            "precondition: outlooks default off"
        );

        handler.set_enabled(true, &mut PaneMut::bare(0));
        assert_eq!(
            handler.enabled_products.iter().copied().collect::<Vec<_>>(),
            vec![OutlookProduct::Categorical],
            "day 1's first product is Categorical"
        );

        handler.set_enabled(false, &mut PaneMut::bare(0));
        assert!(!handler.is_enabled(&PaneRef::bare(0)));

        handler.selected_day = OutlookDay::Day5;
        handler.set_enabled(true, &mut PaneMut::bare(0));
        assert_eq!(
            handler.enabled_products.iter().copied().collect::<Vec<_>>(),
            vec![OutlookProduct::Probabilistic],
            "day 5 publishes only the probabilistic product"
        );
    }

    fn day3_probabilistic() -> SpcOutlookHandler {
        let mut h = SpcOutlookHandler::new();
        h.selected_day = OutlookDay::Day3;
        toggle(&mut h, "prob", true);
        h
    }

    fn day3_outlook(product: OutlookProduct) -> SpcOutlook {
        SpcOutlook {
            day: OutlookDay::Day3,
            product,
            valid: None,
            expire: None,
            features: Vec::new(),
        }
    }

    fn land_day3(
        handler: &mut SpcOutlookHandler,
        product: OutlookProduct,
        result: Result<SpcOutlook, crate::fetch_policy::FetchError>,
    ) {
        handler.apply_fetch_result(Box::new(SpcOutlookFetchResult {
            day: OutlookDay::Day3,
            product,
            result,
        }));
    }

    /// It must be `_cigprob` and not `_sigprob`. `_sigprob` still answers 200
    /// with a real `SIGN` polygon but has not been re-issued since 2026-03-03,
    /// so asking for it would paint a months-old hazard area as current.
    #[test]
    fn day_3_asks_for_the_conditional_intensity_endpoint_not_the_frozen_one() {
        let sources = rustdar_source::origins::DataSources::default();
        let url = crate::spc::outlook::outlook_url(
            &sources,
            OutlookDay::Day3,
            OutlookProduct::ConditionalIntensity,
        );
        assert!(
            url.ends_with("/day3otlk_cigprob.lyr.geojson"),
            "day 3's significant area comes from _cigprob, got {url}"
        );
        assert!(
            !url.contains("sigprob"),
            "_sigprob is frozen at 2026-03-03 and must never be requested: {url}"
        );
    }

    #[test]
    fn the_significant_area_is_fetched_but_has_no_toggle_of_its_own() {
        let handler = day3_probabilistic();
        assert!(
            handler
                .enabled_products
                .contains(&OutlookProduct::ConditionalIntensity),
            "ticking Probabilistic must bring the significant area into scope"
        );

        let ids: Vec<&str> = handler
            .controls(&PaneRef::bare(0))
            .into_iter()
            .filter_map(|item| match item {
                ControlItem::Toggle { id, .. } => Some(id),
                _ => None,
            })
            .collect();
        assert_eq!(
            ids,
            vec!["cat", "prob"],
            "day 3 offers exactly the two products the user picks"
        );

        assert_eq!(
            handler.status_line(&PaneRef::bare(0)).as_deref(),
            Some("Day 3 - Probabilistic"),
            "the status line names the selection, not the implied product"
        );
    }

    #[test]
    fn the_significant_area_is_its_own_task_and_its_own_ledger_entry() {
        let mut handler = day3_probabilistic();
        let ctx = FetchConfig {
            client: Default::default(),
            zone_cache_dir: None,
            sources: rustdar_source::origins::DataSources::default(),
            viewport: None,
        };
        assert_eq!(
            handler.create_fetch_tasks(&ctx, &PaneRef::bare(0)).len(),
            2,
            "Probabilistic and its significant area are two tasks"
        );

        handler.set_fetching(true, &PaneRef::bare(0));
        land_day3(
            &mut handler,
            OutlookProduct::Probabilistic,
            Ok(day3_outlook(OutlookProduct::Probabilistic)),
        );
        land_day3(
            &mut handler,
            OutlookProduct::ConditionalIntensity,
            Err(transient()),
        );

        assert!(
            handler
                .per_product_error
                .contains_key(&(OutlookDay::Day3, OutlookProduct::ConditionalIntensity)),
            "the failure is filed against the product that failed"
        );
        assert!(
            handler.state.retry.is_incomplete(),
            "a round that lost the significant area must not read as complete"
        );
    }

    #[test]
    fn every_path_that_enables_the_parent_brings_the_significant_area() {
        let handler = day3_probabilistic();
        assert!(
            handler
                .enabled_products
                .contains(&OutlookProduct::ConditionalIntensity),
            "toggle path"
        );

        let mut from_day5 = SpcOutlookHandler::new();
        from_day5.selected_day = OutlookDay::Day5;
        from_day5.set_enabled(true, &mut PaneMut::bare(0));
        assert_eq!(
            from_day5
                .enabled_products
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![OutlookProduct::Probabilistic],
            "premise: day 5 publishes only the probabilistic product"
        );
        toggle(&mut from_day5, "day3", true);
        assert!(
            from_day5
                .enabled_products
                .contains(&OutlookProduct::ConditionalIntensity),
            "day-button path"
        );

        let mut reopened = SpcOutlookHandler::new();
        reopened.deserialize_state(serde_json::json!({
            "selected_day": "Day3",
            "enabled_products": ["Probabilistic"],
        }));
        assert!(
            reopened
                .enabled_products
                .contains(&OutlookProduct::ConditionalIntensity),
            "reopen path: a pre-change session must start asking for _cigprob"
        );
    }

    #[test]
    fn the_significant_area_leaves_when_its_parent_or_its_day_does() {
        let mut handler = day3_probabilistic();
        toggle(&mut handler, "prob", false);
        assert!(
            !handler
                .enabled_products
                .contains(&OutlookProduct::ConditionalIntensity),
            "unticking Probabilistic drops the significant area with it"
        );

        let mut handler = day3_probabilistic();
        toggle(&mut handler, "day1", true);
        assert!(
            !handler
                .enabled_products
                .contains(&OutlookProduct::ConditionalIntensity),
            "day 1 carries its CIG features inline and publishes no such product"
        );
    }

    #[test]
    fn the_outlooks_paint_in_publication_order_not_hash_order() {
        let mut handler = day3_probabilistic();
        toggle(&mut handler, "cat", true);

        let feature = |label: &str| crate::types::OverlayFeature {
            polygons: Vec::new(),
            fill_rgba: [0, 0, 0, 0],
            stroke_rgba: [0, 0, 0, 0],
            label: label.to_string(),
            label2: String::new(),
            hatch: crate::types::HatchPattern::None,
            geo_bounds: None,
        };
        for (product, label) in [
            (OutlookProduct::Categorical, "cat"),
            (OutlookProduct::Probabilistic, "prob"),
            (OutlookProduct::ConditionalIntensity, "cig"),
        ] {
            let mut o = day3_outlook(product);
            o.features.push(feature(label));
            handler.state.data.insert((OutlookDay::Day3, product), o);
        }

        let order: Vec<String> = handler
            .features_in_paint_order()
            .into_iter()
            .map(|f| f.label)
            .collect();
        assert_eq!(
            order,
            vec!["cat", "prob", "cig"],
            "publication order, with the significant-severe overlay last"
        );
    }

    #[test]
    fn the_popup_states_the_outlooks_window_and_links_to_spc() {
        let item = OutlookItem {
            label: "SLGT".into(),
            day: OutlookDay::Day1,
            product: OutlookProduct::Categorical,
            valid: chrono::NaiveDate::from_ymd_opt(2026, 8, 10)
                .and_then(|d| d.and_hms_opt(12, 0, 0)),
            expire: chrono::NaiveDate::from_ymd_opt(2026, 8, 11)
                .and_then(|d| d.and_hms_opt(12, 0, 0)),
        };
        // Pinned to UTC so the asserted dates cannot shift with the machine's
        // own timezone.
        let prefs = rustdar_units::UserPreferences {
            timezone: rustdar_units::TimezonePreference::Utc,
            ..Default::default()
        };
        let content = item.popup_content(&prefs);
        assert_eq!(content.title, "SPC Day 1 Categorical Outlook");

        let grid = content
            .sections
            .iter()
            .find_map(|s| match s {
                PopupSection::KeyValueGrid(rows) => Some(rows.clone()),
                _ => None,
            })
            .expect("the popup carries a key-value grid");
        let row = |key: &str| {
            grid.iter()
                .find(|(k, _)| k == key)
                .unwrap_or_else(|| panic!("the grid has no {key:?} row"))
                .1
                .clone()
        };
        assert_eq!(row("Day"), "Day 1");
        assert_eq!(row("Product"), "Categorical");
        assert!(
            row("Valid").starts_with("Aug 10 2026"),
            "the valid time must be the parsed field, got {:?}",
            row("Valid"),
        );
        assert!(row("Expires").starts_with("Aug 11 2026"));

        let url = content
            .sections
            .iter()
            .find_map(|s| match s {
                PopupSection::Link { url, .. } => Some(url.clone()),
                _ => None,
            })
            .expect("the popup links to the SPC website");
        assert_eq!(
            url,
            "https://www.spc.noaa.gov/products/outlook/day1otlk.html"
        );
    }

    #[test]
    fn an_extended_day_links_to_the_shared_page_and_owns_its_gaps() {
        let item = OutlookItem {
            label: "15%".into(),
            day: OutlookDay::Day5,
            product: OutlookProduct::Probabilistic,
            valid: None,
            expire: None,
        };
        let prefs = rustdar_units::UserPreferences::default();
        let content = item.popup_content(&prefs);
        assert_eq!(content.title, "SPC Day 5 Probabilistic Outlook");
        assert!(content.sections.iter().any(|s| matches!(
            s,
            PopupSection::Link { url, .. }
                if url == "https://www.spc.noaa.gov/products/exper/day4-8/"
        )));
        assert!(
            content.sections.iter().any(|s| matches!(
                s,
                PopupSection::KeyValueGrid(rows)
                    if rows.iter().any(|(k, v)| k == "Valid" && v == "Unknown")
            )),
            "a missing window must read as the feed's gap, not as a shorter dialog"
        );
    }

    #[test]
    fn a_band_matches_only_its_own_days_product() {
        let band = |product: OutlookProduct| OutlookItem {
            label: "5%".into(),
            day: OutlookDay::Day1,
            product,
            valid: None,
            expire: None,
        };
        let tornado = band(OutlookProduct::Tornado);
        assert!(tornado.matches(&band(OutlookProduct::Tornado)));
        assert!(!tornado.matches(&band(OutlookProduct::Wind)));
    }

    #[test]
    fn the_status_line_names_the_day_and_its_enabled_products() {
        let mut handler = SpcOutlookHandler::new();
        assert_eq!(
            handler.status_line(&PaneRef::bare(0)),
            None,
            "off means no line"
        );

        handler.enabled_products.insert(OutlookProduct::Tornado);
        handler.enabled_products.insert(OutlookProduct::Categorical);
        assert_eq!(
            handler.status_line(&PaneRef::bare(0)).as_deref(),
            Some("Day 1 - Categorical, Tornado"),
            "publication order, not set-iteration order"
        );
    }

    fn outlook(product: OutlookProduct) -> SpcOutlook {
        SpcOutlook {
            day: OutlookDay::Day1,
            product,
            valid: None,
            expire: None,
            features: Vec::new(),
        }
    }

    fn land(
        handler: &mut SpcOutlookHandler,
        product: OutlookProduct,
        result: Result<SpcOutlook, crate::fetch_policy::FetchError>,
    ) {
        handler.apply_fetch_result(Box::new(SpcOutlookFetchResult {
            day: OutlookDay::Day1,
            product,
            result,
        }));
    }

    fn round(
        handler: &mut SpcOutlookHandler,
        results: Vec<(
            OutlookProduct,
            Result<SpcOutlook, crate::fetch_policy::FetchError>,
        )>,
    ) {
        handler.set_fetching(true, &PaneRef::bare(0));
        for (product, result) in results {
            land(handler, product, result);
        }
    }

    fn four_product_handler() -> SpcOutlookHandler {
        let mut h = SpcOutlookHandler::new();
        for &p in OutlookDay::Day1.products() {
            h.enabled_products.insert(p);
        }
        h
    }

    fn transient() -> crate::fetch_policy::FetchError {
        crate::fetch_policy::FetchError::transient("HTTP 500")
    }

    fn toggle(handler: &mut SpcOutlookHandler, id: &'static str, on: bool) -> ControlEffect {
        let mut ctx = PaneMut::bare(0);
        handler.apply_control(
            &ControlUpdate {
                id,
                value: ControlValue::Bool(on),
            },
            &mut ctx,
        )
    }

    #[test]
    fn a_partly_failed_round_reads_the_same_whichever_task_lands_last() {
        use OutlookProduct::{Categorical, Hail, Tornado, Wind};

        let mut failure_first = four_product_handler();
        round(
            &mut failure_first,
            vec![
                (Tornado, Err(transient())),
                (Categorical, Ok(outlook(Categorical))),
                (Wind, Ok(outlook(Wind))),
                (Hail, Ok(outlook(Hail))),
            ],
        );

        let mut failure_last = four_product_handler();
        round(
            &mut failure_last,
            vec![
                (Categorical, Ok(outlook(Categorical))),
                (Wind, Ok(outlook(Wind))),
                (Hail, Ok(outlook(Hail))),
                (Tornado, Err(transient())),
            ],
        );

        assert_eq!(
            failure_first.state.retry.health(),
            failure_last.state.retry.health(),
            "the layer's health depends on which task resolved last",
        );
        let note = failure_first
            .state
            .retry
            .status_note()
            .expect("one product of four failed; the layer must say so");
        assert!(
            note.contains("Tornado"),
            "the note must name the product that did not load: {note}",
        );
        assert!(
            note.contains("1 of 4"),
            "the note must say how much of the round is missing: {note}",
        );
        assert_eq!(failure_first.state.data.len(), 3);
        assert_eq!(failure_last.state.data.len(), 3);
    }

    #[test]
    fn a_round_with_two_failures_is_one_attempt_whichever_order_they_land_in() {
        use OutlookProduct::{Categorical, Hail, Tornado, Wind};

        let mut failures_first = four_product_handler();
        round(
            &mut failures_first,
            vec![
                (Tornado, Err(transient())),
                (Wind, Err(transient())),
                (Categorical, Ok(outlook(Categorical))),
                (Hail, Ok(outlook(Hail))),
            ],
        );

        let mut failures_last = four_product_handler();
        round(
            &mut failures_last,
            vec![
                (Categorical, Ok(outlook(Categorical))),
                (Hail, Ok(outlook(Hail))),
                (Tornado, Err(transient())),
                (Wind, Err(transient())),
            ],
        );

        assert_eq!(
            failures_first.state.retry.failures(),
            failures_last.state.retry.failures(),
            "the same round bought a different number of attempts depending on \
             the order its tasks resolved in",
        );
        assert_eq!(
            failures_first.state.retry.failures(),
            1,
            "one round is one attempt, however many of its products failed",
        );
        assert_eq!(
            failures_first.state.retry.status_note(),
            failures_last.state.retry.status_note(),
        );
        let note = failures_first
            .state
            .retry
            .status_note()
            .expect("two products of four failed");
        assert!(
            note.contains("2 of 4"),
            "the note must say how much of the round is missing: {note}",
        );
    }

    #[test]
    fn a_round_of_refusals_is_believed_only_when_a_second_round_repeats_it() {
        use OutlookProduct::{Categorical, Hail, Tornado, Wind};
        let refused = || crate::fetch_policy::FetchError::permanent("HTTP 400");
        let all_four = || {
            vec![
                (Categorical, Err(refused())),
                (Tornado, Err(refused())),
                (Wind, Err(refused())),
                (Hail, Err(refused())),
            ]
        };

        let mut h = four_product_handler();
        round(&mut h, all_four());
        assert!(
            h.state.retry.is_unhealthy(),
            "a refused round must read as failing",
        );
        assert!(
            !h.state.retry.is_broken(),
            "one round is one refusal: a CDN blip refusing all four siblings at \
             once must not condemn the layer without being asked twice",
        );

        round(&mut h, all_four());
        assert!(
            h.state.retry.is_broken(),
            "a second round that is refused just the same must still be believed",
        );
    }

    #[test]
    fn a_recovered_product_clears_the_layers_verdict() {
        use OutlookProduct::{Categorical, Hail, Tornado, Wind};
        let mut h = four_product_handler();
        round(
            &mut h,
            vec![
                (Categorical, Ok(outlook(Categorical))),
                (Wind, Ok(outlook(Wind))),
                (Hail, Ok(outlook(Hail))),
                (Tornado, Err(transient())),
            ],
        );
        assert!(h.state.retry.is_unhealthy(), "premise: one product failed");

        round(
            &mut h,
            vec![
                (Categorical, Ok(outlook(Categorical))),
                (Wind, Ok(outlook(Wind))),
                (Hail, Ok(outlook(Hail))),
                (Tornado, Ok(outlook(Tornado))),
            ],
        );
        assert_eq!(
            h.state.retry.status_note(),
            None,
            "every product has now arrived; the layer must stop reporting a fault",
        );
    }

    #[test]
    fn an_absent_product_is_not_reported_as_the_layer_being_stale() {
        use OutlookProduct::{Categorical, Hail, Tornado, Wind};
        let mut h = four_product_handler();
        round(
            &mut h,
            vec![
                (Categorical, Ok(outlook(Categorical))),
                (Wind, Ok(outlook(Wind))),
                (Hail, Ok(outlook(Hail))),
                (
                    Tornado,
                    Err(crate::fetch_policy::FetchError::absent("HTTP 404")),
                ),
            ],
        );
        assert!(
            !h.state.retry.is_unhealthy(),
            "an unpublished product must not read as the layer failing",
        );
        assert_eq!(
            h.state.retry.status_note(),
            None,
            "three of four products drew; the layer is not the thing that is \
             unpublished",
        );

        let mut alone = SpcOutlookHandler::new();
        alone.enabled_products.insert(Tornado);
        round(
            &mut alone,
            vec![(
                Tornado,
                Err(crate::fetch_policy::FetchError::absent("HTTP 404")),
            )],
        );
        assert_eq!(
            alone.state.retry.health(),
            &crate::fetch_policy::FetchHealth::Absent,
        );
        assert_eq!(alone.state.retry.failures(), 0);
    }

    #[test]
    fn unticking_the_product_that_failed_stops_the_layer_reading_as_stale() {
        use OutlookProduct::{Categorical, Tornado};
        let mut h = SpcOutlookHandler::new();
        h.enabled_products.insert(Categorical);
        h.enabled_products.insert(Tornado);
        round(
            &mut h,
            vec![
                (Categorical, Ok(outlook(Categorical))),
                (Tornado, Err(transient())),
            ],
        );
        assert!(h.state.retry.is_unhealthy(), "premise: one product failed");

        assert_eq!(
            toggle(&mut h, "tor", false),
            ControlEffect::None,
            "unticking asks for nothing, which is why nothing else could clear \
             the ledger",
        );
        assert!(
            !h.state.retry.is_unhealthy(),
            "the layer is drawing every product it asks for and still says it \
             stopped updating",
        );
        assert_eq!(h.state.retry.status_note(), None);
        assert!(
            h.state.data.contains_key(&(OutlookDay::Day1, Categorical)),
            "premise: the layer holds the product that is left",
        );
    }

    #[test]
    fn navigating_to_another_day_leaves_the_old_days_failure_behind() {
        use OutlookProduct::Categorical;
        let mut h = SpcOutlookHandler::new();
        h.enabled_products.insert(Categorical);
        round(&mut h, vec![(Categorical, Err(transient()))]);
        assert!(h.state.retry.is_unhealthy(), "premise");

        let mut ctx = PaneMut::bare(0);
        let effect = h.apply_control(
            &ControlUpdate {
                id: "day2",
                value: ControlValue::Action,
            },
            &mut ctx,
        );
        assert_eq!(effect, ControlEffect::Fetch, "a new day is a new ask");
        assert!(
            !h.state.retry.is_unhealthy(),
            "day 1's failure must not be reported against day 2, which has not \
             been asked yet",
        );
    }

    #[test]
    fn a_failure_that_lands_after_its_product_was_unticked_still_reaches_the_ladder() {
        use OutlookProduct::Tornado;
        let mut h = SpcOutlookHandler::new();
        h.enabled_products.insert(Tornado);

        h.set_fetching(true, &PaneRef::bare(0));
        assert!(h.is_fetching(), "premise: the request is on the wire");
        assert_eq!(toggle(&mut h, "tor", false), ControlEffect::None);

        land(&mut h, Tornado, Err(transient()));

        assert!(!h.is_fetching(), "the round is over");
        assert_eq!(
            h.state.retry.failures(),
            1,
            "the origin failed and the layer recorded nothing at all",
        );
        assert!(h.state.retry.is_unhealthy());
        assert!(
            !h.state
                .retry
                .backoff_remaining(std::time::Duration::from_secs(120))
                .is_zero(),
            "a failure that files nothing leaves the layer due on the next \
             frame — 3089 requests in 105 s is what that costs",
        );
    }

    #[test]
    fn a_stray_failure_does_not_condemn_a_round_that_otherwise_answered() {
        use OutlookProduct::{Categorical, Hail, Tornado, Wind};
        let mut h = four_product_handler();
        h.set_fetching(true, &PaneRef::bare(0));
        assert_eq!(toggle(&mut h, "tor", false), ControlEffect::None);

        for p in [Categorical, Wind, Hail] {
            land(&mut h, p, Ok(outlook(p)));
        }
        land(&mut h, Tornado, Err(transient()));

        assert!(
            !h.state.retry.is_unhealthy(),
            "every product the layer asks for arrived in this very round",
        );
    }

    #[test]
    fn a_product_that_would_not_load_is_missing_from_the_map_and_not_merely_stale() {
        use OutlookProduct::{Categorical, Hail, Tornado, Wind};
        let mut h = four_product_handler();
        round(
            &mut h,
            vec![
                (Categorical, Ok(outlook(Categorical))),
                (Wind, Ok(outlook(Wind))),
                (Hail, Ok(outlook(Hail))),
                (Tornado, Err(transient())),
            ],
        );

        assert!(
            h.state.retry.is_unhealthy(),
            "premise: the round did not complete, and the ladder still hears it",
        );
        assert!(
            h.state.retry.is_incomplete(),
            "the tornado outlook is on no map anywhere and the layer said only \
             that it had stopped updating",
        );
        let note = h
            .state
            .retry
            .coverage()
            .status_note()
            .expect("the options must say which product is not drawn");
        for expected in ["missing 1 of 4 outlook products", "Tornado"] {
            assert!(
                note.contains(expected),
                "the note must name what is off the map - missing {expected:?}: {note}",
            );
        }

        let mut drawn = four_product_handler();
        round(
            &mut drawn,
            OutlookDay::Day1
                .products()
                .iter()
                .map(|&p| (p, Ok(outlook(p))))
                .collect(),
        );
        assert!(!drawn.state.retry.is_incomplete(), "premise: all four drew");
        round(
            &mut drawn,
            vec![
                (Categorical, Ok(outlook(Categorical))),
                (Wind, Ok(outlook(Wind))),
                (Hail, Ok(outlook(Hail))),
                (Tornado, Err(transient())),
            ],
        );
        assert!(
            drawn.state.retry.is_unhealthy(),
            "premise: the second round failed",
        );
        assert!(
            !drawn.state.retry.is_incomplete(),
            "the tornado product has answered for this day and what it \
             answered is stale, which is what the health axis is for",
        );

        assert_eq!(toggle(&mut h, "tor", false), ControlEffect::None);
        assert!(
            !h.state.retry.is_incomplete(),
            "the mark outlived the selection it was about",
        );
    }

    #[test]
    fn a_round_that_lands_wholly_out_of_scope_still_retires_its_coverage_report() {
        use OutlookProduct::{Categorical, Tornado};
        let mut h = SpcOutlookHandler::new();
        h.enabled_products.insert(Categorical);
        h.enabled_products.insert(Tornado);
        round(
            &mut h,
            vec![
                (Categorical, Ok(outlook(Categorical))),
                (Tornado, Err(transient())),
            ],
        );
        assert!(
            h.state.retry.is_incomplete(),
            "premise: the tornado outlook did not load and is on no map",
        );

        h.set_fetching(true, &PaneRef::bare(0));
        assert_eq!(toggle(&mut h, "tor", false), ControlEffect::None);
        assert_eq!(toggle(&mut h, "cat", false), ControlEffect::None);
        land(&mut h, Categorical, Ok(outlook(Categorical)));
        land(&mut h, Tornado, Err(transient()));

        assert!(
            !h.state.retry.is_incomplete(),
            "the layer asks for no product at all and its options still name \
             one as missing from the map",
        );
        assert_eq!(
            h.state.retry.coverage().status_note(),
            None,
            "a report about a selection that no longer exists",
        );
    }

    #[test]
    fn the_outstanding_count_is_the_number_of_tasks_actually_built() {
        use crate::render::overlay_state::FetchConfig;
        rustdar_source::tls::init();
        let ctx = FetchConfig {
            client: reqwest::Client::builder()
                .build()
                .expect("a client with no options set"),
            zone_cache_dir: None,
            sources: rustdar_source::origins::DataSources::production(),
            viewport: None,
        };
        for products in 1..=OutlookDay::Day1.products().len() {
            let mut h = SpcOutlookHandler::new();
            for &p in &OutlookDay::Day1.products()[..products] {
                h.enabled_products.insert(p);
            }
            let built = h.create_fetch_tasks(&ctx, &PaneRef::bare(0)).len();
            h.set_fetching(true, &PaneRef::bare(0));
            assert_eq!(
                h.outstanding, built,
                "the round waits for {} answers and asked {built} questions",
                h.outstanding,
            );
        }
    }
}
