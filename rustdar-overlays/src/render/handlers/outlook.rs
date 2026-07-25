use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::render::controls::{ControlButton, ControlEffect, ControlItem, ControlUpdate, ControlValue, PaneControlContext, PaneControlContextMut};
use crate::render::overlay_state::{
    FetchPayload,
    ClickableItem, FetchConfig, FetchTask, OverlayHandler, OverlayItem, OverlayKind,
    OverlayState, PopupContent, PopupSection, RasterizeContext, RasterizeFn, RenderMode,
};
use crate::render::rasterize::{self, RasterizeOutput};
use crate::spc::outlook::{OutlookDay, OutlookProduct, SpcOutlook};
use crate::types::GeoBounds;

/// Type-erased fetch result for SPC outlook data.
pub(crate) struct SpcOutlookFetchResult {
    pub day: OutlookDay,
    pub product: OutlookProduct,
    pub result: Result<SpcOutlook, String>,
}

/// Clickable item representing a single SPC outlook feature.
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
    /// Per-product data generation counters for fine-grained cache invalidation.
    per_product_generation: HashMap<(OutlookDay, OutlookProduct), u64>,
    /// Bumped when config (selected day, product set) changes without a data fetch.
    config_generation: u64,
    /// Currently selected outlook day.
    pub selected_day: OutlookDay,
    /// Which outlook products are enabled.
    pub enabled_products: HashSet<OutlookProduct>,
}

impl SpcOutlookHandler {
    pub fn new() -> Self {
        Self {
            state: OverlayState::new(),
            per_product_generation: HashMap::new(),
            config_generation: 0,
            selected_day: OutlookDay::Day1,
            enabled_products: HashSet::new(), // disabled by default
        }
    }

    fn combined_generation(&self) -> u64 {
        self.per_product_generation.values().sum::<u64>().wrapping_add(self.config_generation)
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
            let Some(outlook) = self.state.data.get(&(day, product)) else { continue };
            for feature in &outlook.features {
                items.push(ClickableItem {
                    features: vec![feature.clone()],
                    label: None,
                    item: Arc::new(OutlookItem { label: feature.label.clone() }) as Arc<dyn OverlayItem>,
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
                let counter = self.per_product_generation
                    .entry((fetch.day, fetch.product))
                    .or_insert(0);
                *counter = counter.wrapping_add(1);
            }
            Err(e) => {
                log::error!("SPC outlook fetch failed ({:?} {:?}): {}", fetch.day, fetch.product, e);
            }
        }
        self.state.fetching = false;
    }

    fn retain_selections(&self, _selections: &mut Vec<Arc<dyn OverlayItem>>) {
        // Outlook selections are always valid (label-based, not ID-based)
    }

    fn prepare_rasterize(
        &self,
        ctx: &RasterizeContext,
    ) -> Option<RasterizeFn> {
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
            let rgba = rasterize::rasterize_spc_outlooks(&features, bounds, width, height, hatch_color);
            RasterizeOutput { rgba, hit_map: None }
        }))
    }

    fn create_fetch_tasks(&self, ctx: &FetchConfig) -> Vec<FetchTask> {
        if self.enabled_products.is_empty() {
            return Vec::new();
        }
        let day = self.selected_day;
        let products: Vec<OutlookProduct> = self.enabled_products.iter().copied().collect();
        log::info!("Fetching SPC outlooks for {:?}: {:?}", day, products);
        products
            .into_iter()
            .map(|product| {
                let client = ctx.client.clone();
                FetchTask {
                    kind: OverlayKind::SpcOutlook,
                    future: Box::pin(async move {
                        let result = crate::spc::fetch::fetch_outlook(&client, day, product)
                            .await
                            .map_err(|e| e.to_string());
                        Box::new(SpcOutlookFetchResult { day, product, result }) as FetchPayload
                    }),
                }
            })
            .collect()
    }

    fn controls(&self, _ctx: &PaneControlContext<'_>) -> Vec<ControlItem> {
        let mut items = vec![
            ControlItem::Heading { text: "\u{26c8}  SPC Outlooks".into() },
        ];

        // Day selector buttons
        let buttons: Vec<ControlButton> = OutlookDay::all().iter().map(|&d| {
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
        }).collect();
        items.push(ControlItem::ButtonRow { buttons });

        // Product toggles for current day
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

        // Refresh button + fetching indicator when enabled
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
                items.push(ControlItem::InfoText { text: "Fetching\u{2026}".into() });
            }
        }

        items
    }

    fn apply_control(&mut self, update: &ControlUpdate, _ctx: &mut PaneControlContextMut<'_>) -> ControlEffect {
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
                    // Remove products not valid for the new day
                    let valid: HashSet<OutlookProduct> = new_day.products().iter().copied().collect();
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
        if let Some(day) = value.get("selected_day").and_then(|v| serde_json::from_value(v.clone()).ok()) {
            self.selected_day = day;
        }
        if let Some(products) = value.get("enabled_products").and_then(|v| serde_json::from_value(v.clone()).ok()) {
            self.enabled_products = products;
        }
    }
}
