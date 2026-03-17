use std::any::Any;
use std::collections::HashMap;

use crate::render::layers::LayerManager;
use crate::render::overlay_state::{
    ClickableItem, FetchConfig, FetchTask, OverlayHandler, OverlayKind, OverlayState,
    PopupContent, PopupSection, RasterizeContext, SelectedOverlay,
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

pub(crate) struct SpcOutlookHandler {
    pub state: OverlayState<HashMap<(OutlookDay, OutlookProduct), SpcOutlook>>,
    /// Per-product data generation counters for fine-grained cache invalidation.
    per_product_generation: HashMap<(OutlookDay, OutlookProduct), u64>,
}

impl SpcOutlookHandler {
    pub fn new() -> Self {
        Self {
            state: OverlayState::new(),
            per_product_generation: HashMap::new(),
        }
    }

    fn combined_generation(&self) -> u64 {
        self.per_product_generation.values().sum()
    }
}

impl OverlayHandler for SpcOutlookHandler {
    fn kind(&self) -> OverlayKind {
        OverlayKind::SpcOutlook
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

    fn fetch_time(&self) -> Option<std::time::Instant> {
        self.state.fetch_time
    }

    fn item_count(&self) -> usize {
        self.state.data.len()
    }

    fn clickable_items(&self, layers: &LayerManager) -> Vec<ClickableItem<'_>> {
        let day = layers.spc_day;
        let mut items = Vec::new();
        for lk in layers.spc_layers_for_day() {
            if !layers.is_enabled(lk) {
                continue;
            }
            let Some(product) = lk.to_outlook_product() else { continue };
            let Some(outlook) = self.state.data.get(&(day, product)) else { continue };
            for feature in &outlook.features {
                items.push(ClickableItem {
                    features: vec![feature],
                    label: None,
                    id: SelectedOverlay::Outlook { label: feature.label.clone() },
                });
            }
        }
        items
    }

    fn popup_content(&self, selected: &SelectedOverlay) -> Option<PopupContent> {
        let SelectedOverlay::Outlook { label } = selected else { return None };
        Some(PopupContent {
            title: format!("SPC Outlook: {label}"),
            accent_rgb: [200, 200, 100],
            width: 300.0,
            sections: vec![PopupSection::Text("Outlook detail coming soon.".into())],
            actions: Vec::new(),
        })
    }

    fn apply_fetch_result(&mut self, result: Box<dyn Any + Send>) {
        let Some(fetch) = result.downcast::<SpcOutlookFetchResult>().ok() else {
            log::error!("SPC outlook handler received unexpected fetch result type");
            return;
        };
        match fetch.result {
            Ok(outlook) => {
                log::info!("Received SPC outlook: {:?} {:?}", fetch.day, fetch.product);
                self.state.data.insert((fetch.day, fetch.product), outlook);
                self.state.fetch_time = Some(std::time::Instant::now());
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

    fn retain_selections(&self, _selections: &mut Vec<SelectedOverlay>) {
        // Outlook selections are always valid (label-based, not ID-based)
    }

    fn prepare_rasterize(
        &self,
        ctx: &RasterizeContext,
    ) -> Option<Box<dyn FnOnce(&GeoBounds, u32, u32) -> RasterizeOutput + Send>> {
        let day = ctx.spc_day;
        let mut features = Vec::new();
        for &product in &ctx.enabled_spc_products {
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
        if ctx.spc_products.is_empty() {
            return Vec::new();
        }
        let day = ctx.spc_day;
        log::info!("Fetching SPC outlooks for {:?}: {:?}", day, ctx.spc_products);
        ctx.spc_products
            .iter()
            .map(|&product| {
                let client = ctx.client.clone();
                FetchTask {
                    kind: OverlayKind::SpcOutlook,
                    future: Box::pin(async move {
                        let result = crate::spc::fetch::fetch_outlook(&client, day, product)
                            .await
                            .map_err(|e| format!("{e}"));
                        Box::new(SpcOutlookFetchResult { day, product, result }) as Box<dyn Any + Send>
                    }),
                }
            })
            .collect()
    }
}
