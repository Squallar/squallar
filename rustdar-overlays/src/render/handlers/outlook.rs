use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::render::controls::{
    ControlButton, ControlEffect, ControlItem, ControlUpdate, ControlValue, PaneControlContext,
    PaneControlContextMut,
};
use crate::render::overlay_state::{
    ClickableItem, FetchConfig, FetchPayload, FetchTask, OverlayHandler, OverlayItem, OverlayKind,
    OverlayState, PopupContent, PopupSection, RasterizeContext, RasterizeFn, RenderMode,
};
use crate::render::rasterize::{self, RasterizeOutput};
use crate::spc::outlook::{OutlookDay, OutlookProduct, SpcOutlook};
use crate::types::GeoBounds;

pub(crate) struct SpcOutlookFetchResult {
    pub day: OutlookDay,
    pub product: OutlookProduct,
    pub result: Result<SpcOutlook, crate::fetch_policy::FetchError>,
}

#[derive(Debug)]
pub(crate) struct OutlookItem {
    pub label: String,
    /// Which outlook the clicked feature came from — the popup's subject.
    pub day: OutlookDay,
    pub product: OutlookProduct,
    /// The outlook's own validity window, as parsed from the feed. `None`
    /// where the feed did not carry one; the grid says "Unknown" rather than
    /// omitting the row, so a missing time reads as the feed's gap and not as
    /// a shorter dialog.
    pub valid: Option<chrono::NaiveDateTime>,
    pub expire: Option<chrono::NaiveDateTime>,
}

/// The SPC page that shows `day`'s outlook — the popup's "Open on SPC
/// website" target.
///
/// A *website* link for a person, not a data fetch, so it does not route
/// through `DataSources::spc_base` (that table exists to keep fetch origins
/// browser-reachable; a link opens in the browser by definition). Days 1–3
/// each have their own page; days 4–8 share one experimental page.
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
    fn kind(&self) -> OverlayKind {
        OverlayKind::SpcOutlook
    }

    fn popup_content(&self, prefs: &rustdar_units::UserPreferences) -> PopupContent {
        // `None` prints as a word, not as absence — see the field note.
        let time = |t: Option<chrono::NaiveDateTime>| match t {
            Some(t) => prefs.timezone.format_naive_utc(t, "%b %d %Y %H:%M"),
            None => "Unknown".to_owned(),
        };
        PopupContent {
            title: format!("SPC Day {} {} Outlook", self.day.label(), self.product),
            accent_rgb: [200, 200, 100],
            width: 300.0,
            sections: vec![
                // The clicked feature's own label — the risk category or
                // probability band the user actually clicked on.
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
        // Day and product joined the identity with the real popup content: a
        // "5%" band exists in Tornado and Wind alike, and keeping one open
        // across a refetch must re-find *this* product's band, not whichever
        // same-labelled band lists first.
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

pub(crate) struct SpcOutlookHandler {
    pub state: OverlayState<HashMap<(OutlookDay, OutlookProduct), SpcOutlook>>,
    /// Per product, so one product's refetch does not invalidate the others.
    per_product_generation: HashMap<(OutlookDay, OutlookProduct), u64>,
    /// Bumped when day or product set changes without any fetch, which still
    /// changes what gets drawn.
    config_generation: u64,
    /// The last *failure* per product, `Absent` excluded — see
    /// [`Self::refile_round_health`].
    ///
    /// This layer is the only one that issues **several fetch tasks per round**,
    /// one per enabled product, and they all land on one shared `state.retry`.
    /// Filing each result as it arrived made the layer's health depend on which
    /// task happened to resolve last: three products succeeding and one 500ing
    /// showed a fault or showed nothing depending on the order. Keeping the
    /// failures per product and deriving the ledger from the whole map makes the
    /// answer a property of the round instead of a race.
    per_product_error: HashMap<(OutlookDay, OutlookProduct), crate::fetch_policy::FetchError>,
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
            selected_day: OutlookDay::Day1,
            enabled_products: HashSet::new(),
        }
    }

    /// Re-derive the layer's ledger from every product's last answer.
    ///
    /// Called after **each** task lands, and idempotent so repeating it does not
    /// climb the ladder: it files a failure only while one is outstanding, and
    /// the success path has already cleared the ledger by the time it runs. That
    /// is what makes the layer's final health a property of the round rather
    /// than of the order its tasks happened to resolve in — three products
    /// succeeding and one 500ing used to show a fault or show nothing depending
    /// on which landed last.
    ///
    /// Walks the day's own publication order rather than `enabled_products`'
    /// `HashSet` order, for the same reason
    /// [`status_line`](OverlayHandler::status_line) does: a message built from a
    /// `HashSet` walk jitters between frames.
    ///
    /// Scoped to the products currently asked for, so one the user has since
    /// unticked — or one belonging to a day they have navigated away from —
    /// cannot keep the layer reading as failing.
    ///
    /// [`Absent`](crate::fetch_policy::FetchFailure::Absent) never reaches the
    /// map this reads. SPC does not keep every product up at every hour, so
    /// "not published right now" for one of four is an answer about that
    /// product, not a fault in the layer.
    fn refile_round_health(&mut self) {
        let day = self.selected_day;
        let failed: Vec<(OutlookProduct, &crate::fetch_policy::FetchError)> = day
            .products()
            .iter()
            .copied()
            .filter(|p| self.enabled_products.contains(p))
            .filter_map(|p| self.per_product_error.get(&(day, p)).map(|e| (p, e)))
            .collect();
        if failed.is_empty() {
            return;
        }
        let asked = day
            .products()
            .iter()
            .filter(|p| self.enabled_products.contains(p))
            .count();
        let merged = crate::fetch_policy::FetchError {
            failure: crate::fetch_policy::FetchFailure::of_round(
                failed.iter().map(|(_, e)| e.failure),
            ),
            message: format!(
                "{} of {asked} outlook products did not load: {}",
                failed.len(),
                failed
                    .iter()
                    .map(|(p, e)| format!("{p:?}: {}", e.message))
                    .collect::<Vec<_>>()
                    .join("; ")
            ),
        };
        self.state.retry.record_failure(&merged);
    }

    fn combined_generation(&self) -> u64 {
        self.per_product_generation
            .values()
            .sum::<u64>()
            .wrapping_add(self.config_generation)
    }
}

impl OverlayHandler for SpcOutlookHandler {
    fn kind(&self) -> OverlayKind {
        OverlayKind::SpcOutlook
    }

    fn display_name(&self) -> &str {
        "SPC Outlooks"
    }

    fn render_mode(&self) -> RenderMode {
        RenderMode::Texture
    }

    fn is_enabled(&self) -> bool {
        !self.enabled_products.is_empty()
    }

    /// The master toggle over a layer whose "enabled" is really a product
    /// set — the same arrangement, and the same accepted forgetting, as
    /// `NwsAlertHandler::set_enabled`. On restores the selected day's
    /// *first* product, which is Categorical where the day publishes one and
    /// Probabilistic where that is all there is — the entry a user starting
    /// from nothing would tick.
    fn set_enabled(&mut self, enabled: bool) {
        if enabled {
            if self.enabled_products.is_empty()
                && let Some(&first) = self.selected_day.products().first()
            {
                self.enabled_products.insert(first);
                self.config_generation = self.config_generation.wrapping_add(1);
            }
        } else if !self.enabled_products.is_empty() {
            self.enabled_products.clear();
            self.config_generation = self.config_generation.wrapping_add(1);
        }
    }

    /// E.g. `"Day 1 - Categorical, Tornado"`. The products are named in the
    /// day's own publication order, not the `HashSet`'s, so the line cannot
    /// jitter between frames.
    fn status_line(&self) -> Option<String> {
        if !self.is_enabled() {
            return None;
        }
        let products: Vec<String> = self
            .selected_day
            .products()
            .iter()
            .filter(|p| self.enabled_products.contains(p))
            .map(|p| p.to_string())
            .collect();
        Some(format!("{} - {}", self.selected_day, products.join(", ")))
    }

    fn data_generation(&self) -> u64 {
        self.combined_generation()
    }

    /// Data **this selection** can draw, not data this layer has ever fetched.
    ///
    /// Every other handler's `has_data` is the same test its own
    /// `prepare_rasterize` opens with, and this one was not: outlooks are keyed
    /// by `(day, product)`, so a full `state.data` says nothing about whether
    /// the selected day crossed with the ticked products yields a single
    /// feature. Untick every product, or move to a day whose products are not
    /// ticked, and the old answer was `true` while `prepare_rasterize` returned
    /// `None`.
    ///
    /// That gap is not cosmetic. `ui_map_pane` reads this to decide both
    /// whether to dispatch a render *and* whether a settle render is still owed
    /// — and the second one asks for a repaint 100 ms out for as long as it is
    /// owed. An overlay that is asked for for ever and abandoned in
    /// `spawn_overlay_render` for ever is a permanent 10 Hz wakeup on an
    /// otherwise idle app, on the battery, with nothing on screen to say why.
    /// So this is the exact complement of `prepare_rasterize`'s own early
    /// return, and `every_texture_handler_agrees_with_its_own_rasterizer` is
    /// what keeps the two from drifting apart again.
    fn has_data(&self) -> bool {
        self.enabled_products.iter().any(|product| {
            self.state
                .data
                .get(&(self.selected_day, *product))
                .is_some_and(|outlook| !outlook.features.is_empty())
        })
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

    fn item_count(&self) -> usize {
        self.state.data.len()
    }

    fn clickable_items(&self) -> Vec<ClickableItem<'_>> {
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
        let Some(fetch) = result.downcast::<SpcOutlookFetchResult>().ok() else {
            log::error!("SPC outlook handler received unexpected fetch result type");
            return;
        };
        let key = (fetch.day, fetch.product);
        match fetch.result {
            Ok(outlook) => {
                log::info!("Received SPC outlook: {:?} {:?}", fetch.day, fetch.product);
                self.state.data.insert(key, outlook);
                self.per_product_error.remove(&key);
                self.state.record_success();
                let counter = self.per_product_generation.entry(key).or_insert(0);
                *counter = counter.wrapping_add(1);
            }
            Err(e) if e.failure == crate::fetch_policy::FetchFailure::Absent => {
                // An answer, about one product. Straight through: it stamps the
                // clock and reports "not published right now" without putting
                // the layer on a ladder.
                log::info!(
                    "SPC outlook not published ({:?} {:?}): {e}",
                    fetch.day,
                    fetch.product
                );
                self.per_product_error.remove(&key);
                self.state.record_failure(&e);
            }
            Err(e) => {
                log::error!(
                    "SPC outlook fetch failed ({:?} {:?}): {e}",
                    fetch.day,
                    fetch.product
                );
                // The ledger is **not** written here. One task's error is one
                // product's error, and this layer has several tasks in flight
                // at once; writing straight through made the layer's health a
                // race between them. `refile_round_health` derives it from all
                // of their answers instead. Only `fetching` is ended here,
                // which `record_failure` would otherwise have done — leaving it
                // set is the one absorbing state left in `auto_fetch_delay`.
                self.per_product_error.insert(key, e);
                self.state.fetching = false;
            }
        }
        self.refile_round_health();
    }

    fn retain_selections(&self, _selections: &mut Vec<Arc<dyn OverlayItem>>) {
        // Nothing to prune: outlook items match on day, product and label,
        // not on a data ID.
    }

    fn prepare_rasterize(&self, ctx: &RasterizeContext) -> Option<RasterizeFn> {
        let day = self.selected_day;
        let mut features = Vec::new();
        for &product in &self.enabled_products {
            if let Some(outlook) = self.state.data.get(&(day, product)) {
                features.extend(outlook.features.iter().cloned());
            }
        }
        if features.is_empty() {
            return None;
        }
        let hatch_color = if ctx.is_dark {
            [200, 200, 200, 180]
        } else {
            [60, 60, 60, 180]
        };
        Some(Box::new(move |bounds: &GeoBounds, width, height| {
            let rgba =
                rasterize::rasterize_spc_outlooks(&features, bounds, width, height, hatch_color);
            RasterizeOutput {
                rgba,
                hit_map: None,
                alpha: rasterize::AlphaMode::Premultiplied,
            }
        }))
    }

    fn create_fetch_tasks(&self, ctx: &FetchConfig) -> Vec<FetchTask> {
        if self.enabled_products.is_empty() {
            return Vec::new();
        }
        let day = self.selected_day;
        let products: Vec<OutlookProduct> = self.enabled_products.iter().copied().collect();
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
                    kind: OverlayKind::SpcOutlook,
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

    fn controls(&self, _ctx: &PaneControlContext<'_>) -> Vec<ControlItem> {
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

        // Only the products the selected day actually publishes.
        for &product in self.selected_day.products() {
            let id: &'static str = match product {
                OutlookProduct::Categorical => "cat",
                OutlookProduct::Tornado => "tor",
                OutlookProduct::Wind => "wind",
                OutlookProduct::Hail => "hail",
                OutlookProduct::Probabilistic => "prob",
            };
            items.push(ControlItem::Toggle {
                id,
                label: product.to_string(),
                enabled: self.enabled_products.contains(&product),
            });
        }

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

        items
    }

    fn apply_control(
        &mut self,
        update: &ControlUpdate,
        _ctx: &mut PaneControlContextMut<'_>,
    ) -> ControlEffect {
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
                    // Days publish different product sets; drop the ones the
                    // new day has no endpoint for.
                    let valid: HashSet<OutlookProduct> =
                        new_day.products().iter().copied().collect();
                    self.enabled_products.retain(|p| valid.contains(p));
                    self.config_generation = self.config_generation.wrapping_add(1);
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
                    if enabled {
                        return ControlEffect::Fetch;
                    }
                }
                ControlEffect::None
            }
            // Refreshing a layer with no product ticked has nothing to ask for.
            // Left as an unconditional `Fetch`, it reached
            // `create_fetch_tasks`, got an empty list, and the host recorded
            // that as a failure — which used to be invisible and is now a
            // "what is shown may be stale" line in this very panel, said about
            // a layer that is empty because the user emptied it.
            "refresh" if self.enabled_products.is_empty() => ControlEffect::None,
            "refresh" => ControlEffect::Fetch,
            _ => ControlEffect::None,
        }
    }

    fn serialize_state(&self) -> serde_json::Value {
        serde_json::json!({
            "selected_day": self.selected_day,
            "enabled_products": self.enabled_products,
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The master toggle restores the *selected day's* first product, not a
    /// hardcoded Categorical: days 4-8 publish only Probabilistic, and a
    /// master that inserted a product the day has no endpoint for would show
    /// an enabled layer that can never fetch anything.
    #[test]
    fn the_master_toggle_restores_a_product_the_day_actually_publishes() {
        let mut handler = SpcOutlookHandler::new();
        assert!(!handler.is_enabled(), "precondition: outlooks default off");

        handler.set_enabled(true);
        assert_eq!(
            handler.enabled_products.iter().copied().collect::<Vec<_>>(),
            vec![OutlookProduct::Categorical],
            "day 1's first product is Categorical"
        );

        handler.set_enabled(false);
        assert!(!handler.is_enabled());

        handler.selected_day = OutlookDay::Day5;
        handler.set_enabled(true);
        assert_eq!(
            handler.enabled_products.iter().copied().collect::<Vec<_>>(),
            vec![OutlookProduct::Probabilistic],
            "day 5 publishes only the probabilistic product"
        );
    }

    /// The popup names the outlook, states its window and links to SPC —
    /// this used to be a literal "coming soon" stub.
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

    /// Days 4–8 share one experimental SPC page, and a window the feed did
    /// not carry prints as a word rather than vanishing.
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

    /// The identity a kept-open popup re-finds across a refetch is the
    /// product's own band: a "5%" in Tornado is not the "5%" in Wind.
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

    /// `"Day N · <products>"`, in the day's own publication order — the
    /// status line under the stack's SPC Outlooks row.
    #[test]
    fn the_status_line_names_the_day_and_its_enabled_products() {
        let mut handler = SpcOutlookHandler::new();
        assert_eq!(handler.status_line(), None, "off means no line");

        handler.enabled_products.insert(OutlookProduct::Tornado);
        handler.enabled_products.insert(OutlookProduct::Categorical);
        assert_eq!(
            handler.status_line().as_deref(),
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

    /// Deliver one product's result through the real ingest path.
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

    /// A handler asking for all four of day 1's products.
    fn four_product_handler() -> SpcOutlookHandler {
        let mut h = SpcOutlookHandler::new();
        for &p in OutlookDay::Day1.products() {
            h.enabled_products.insert(p);
        }
        h
    }

    /// **The resolution-order test.** This layer is the only one that puts
    /// several fetch tasks in flight at once, and they all land on one shared
    /// `state.retry`. Three products succeeding and one failing must read the
    /// same either way round; it used to read as a fault or as nothing at all
    /// depending on which task the network happened to finish last.
    #[test]
    fn a_partly_failed_round_reads_the_same_whichever_task_lands_last() {
        use OutlookProduct::{Categorical, Hail, Tornado, Wind};
        let failure = || crate::fetch_policy::FetchError::transient("HTTP 500");

        let mut failure_first = four_product_handler();
        land(&mut failure_first, Tornado, Err(failure()));
        land(&mut failure_first, Categorical, Ok(outlook(Categorical)));
        land(&mut failure_first, Wind, Ok(outlook(Wind)));
        land(&mut failure_first, Hail, Ok(outlook(Hail)));

        let mut failure_last = four_product_handler();
        land(&mut failure_last, Categorical, Ok(outlook(Categorical)));
        land(&mut failure_last, Wind, Ok(outlook(Wind)));
        land(&mut failure_last, Hail, Ok(outlook(Hail)));
        land(&mut failure_last, Tornado, Err(failure()));

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
        // Both orders left all three good products drawable.
        assert_eq!(failure_first.state.data.len(), 3);
        assert_eq!(failure_last.state.data.len(), 3);
    }

    /// A round where everything arrives is clean, and a product that recovers
    /// takes the layer back to healthy rather than leaving a stuck note.
    #[test]
    fn a_recovered_product_clears_the_layers_verdict() {
        use OutlookProduct::{Categorical, Hail, Tornado, Wind};
        let mut h = four_product_handler();
        for p in [Categorical, Wind, Hail] {
            land(&mut h, p, Ok(outlook(p)));
        }
        land(
            &mut h,
            Tornado,
            Err(crate::fetch_policy::FetchError::transient("HTTP 500")),
        );
        assert!(h.state.retry.is_unhealthy(), "premise: one product failed");

        land(&mut h, Tornado, Ok(outlook(Tornado)));
        assert_eq!(
            h.state.retry.status_note(),
            None,
            "every product has now arrived; the layer must stop reporting a fault",
        );
    }

    /// "Not published right now" for one product is an answer about that
    /// product, not a fault in the layer. Days 4-8 publish one product and SPC
    /// does not keep every outlook up at every hour, so treating a routine 404
    /// as staleness would put a permanent warning on a working layer.
    #[test]
    fn an_absent_product_is_not_reported_as_the_layer_being_stale() {
        use OutlookProduct::{Categorical, Hail, Tornado, Wind};
        let mut h = four_product_handler();
        for p in [Categorical, Wind, Hail] {
            land(&mut h, p, Ok(outlook(p)));
        }
        land(
            &mut h,
            Tornado,
            Err(crate::fetch_policy::FetchError::absent("HTTP 404")),
        );
        assert!(
            !h.state.retry.is_unhealthy(),
            "an unpublished product must not read as the layer failing",
        );
    }

    /// Unticking the product that failed clears the layer's verdict: the
    /// round is scoped to what is actually being asked for.
    #[test]
    fn a_failure_from_an_unticked_product_stops_counting() {
        use OutlookProduct::{Categorical, Tornado};
        let mut h = four_product_handler();
        land(&mut h, Categorical, Ok(outlook(Categorical)));
        land(
            &mut h,
            Tornado,
            Err(crate::fetch_policy::FetchError::transient("HTTP 500")),
        );
        assert!(h.state.retry.is_unhealthy(), "premise");

        h.enabled_products.remove(&Tornado);
        h.refile_round_health();
        // The ledger still carries the old filing; what matters is that a
        // fresh round no longer re-files it.
        h.state.retry.clear();
        h.refile_round_health();
        assert_eq!(
            h.state.retry.status_note(),
            None,
            "a product the user unticked must not keep the layer reading as failing",
        );
    }
}
