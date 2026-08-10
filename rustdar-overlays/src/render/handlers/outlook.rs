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
    pub result: Result<SpcOutlook, String>,
}

#[derive(Debug)]
pub(crate) struct OutlookItem {
    pub label: String,
}

impl OverlayItem for OutlookItem {
    fn kind(&self) -> OverlayKind {
        OverlayKind::SpcOutlook
    }

    fn popup_content(&self, _prefs: &rustdar_units::UserPreferences) -> PopupContent {
        PopupContent {
            title: format!("SPC Outlook: {}", self.label),
            accent_rgb: [200, 200, 100],
            width: 300.0,
            sections: vec![PopupSection::Text("Outlook detail coming soon.".into())],
            actions: Vec::new(),
        }
    }

    fn matches(&self, other: &dyn OverlayItem) -> bool {
        other
            .as_any()
            .downcast_ref::<OutlookItem>()
            .is_some_and(|o| o.label == self.label)
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
            selected_day: OutlookDay::Day1,
            enabled_products: HashSet::new(),
        }
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

    /// E.g. `"Day 1 · Categorical, Tornado"`. The products are named in the
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
        Some(format!("{} \u{b7} {}", self.selected_day, products.join(", ")))
    }

    fn data_generation(&self) -> u64 {
        self.combined_generation()
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

    fn fetch_time(&self) -> Option<web_time::Instant> {
        self.state.fetch_time
    }

    fn item_count(&self) -> usize {
        self.state.data.len()
    }

    fn clickable_items(&self) -> Vec<ClickableItem> {
        let day = self.selected_day;
        let mut items = Vec::new();
        for &product in &self.enabled_products {
            let Some(outlook) = self.state.data.get(&(day, product)) else {
                continue;
            };
            for feature in &outlook.features {
                items.push(ClickableItem {
                    features: vec![feature.clone()],
                    label: None,
                    item: Arc::new(OutlookItem {
                        label: feature.label.clone(),
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
        match fetch.result {
            Ok(outlook) => {
                log::info!("Received SPC outlook: {:?} {:?}", fetch.day, fetch.product);
                self.state.data.insert((fetch.day, fetch.product), outlook);
                self.state.fetch_time = Some(web_time::Instant::now());
                let counter = self
                    .per_product_generation
                    .entry((fetch.day, fetch.product))
                    .or_insert(0);
                *counter = counter.wrapping_add(1);
            }
            Err(e) => {
                log::error!(
                    "SPC outlook fetch failed ({:?} {:?}): {}",
                    fetch.day,
                    fetch.product,
                    e
                );
            }
        }
        self.state.fetching = false;
    }

    fn retain_selections(&self, _selections: &mut Vec<Arc<dyn OverlayItem>>) {
        // Nothing to prune: outlook items match on label, not on a data ID.
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
                            crate::spc::fetch::fetch_outlook(&client, &sources, day, product)
                                .await
                                .map_err(|e| e.to_string());
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
            text: "\u{26c8}  SPC Outlooks".into(),
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

        if self.is_enabled() {
            items.push(ControlItem::ButtonRow {
                buttons: vec![ControlButton {
                    id: "refresh",
                    label: "\u{1f504} Refresh".into(),
                    enabled: !self.state.fetching,
                    highlight: false,
                }],
            });
            if self.state.fetching {
                items.push(ControlItem::InfoText {
                    text: "Fetching\u{2026}".into(),
                });
            }
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
            Some("Day 1 \u{b7} Categorical, Tornado"),
            "publication order, not set-iteration order"
        );
    }
}
