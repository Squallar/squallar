use std::any::Any;
use std::sync::Arc;

use crate::render::controls::{ControlButton, ControlEffect, ControlItem, ControlUpdate, ControlValue, PaneControlContext, PaneControlContextMut};
use crate::render::overlay_state::{
    ClickableItem, FetchConfig, FetchTask, OverlayHandler, OverlayItem, OverlayKind,
    OverlayState, PopupContent, PopupSection, RasterizeContext, RenderMode,
};
use crate::render::rasterize::{self, RasterizeOutput};
use crate::spc::colors::md_stroke_color;
use crate::spc::discussion::SpcDiscussion;
use crate::types::{GeoBounds, OverlayLabel};

/// Type-erased fetch result for SPC Mesoscale Discussions.
pub(crate) struct SpcDiscussionFetchResult(pub Result<Vec<SpcDiscussion>, String>);

/// Clickable item representing a single SPC Mesoscale Discussion.
#[derive(Debug)]
pub(crate) struct DiscussionItem {
    pub md: SpcDiscussion,
}

impl OverlayItem for DiscussionItem {
    fn kind(&self) -> OverlayKind {
        OverlayKind::SpcDiscussions
    }

    fn popup_content(&self, _prefs: &rustdar_units::UserPreferences) -> PopupContent {
        let md = &self.md;
        let [r, g, b, _] = md_stroke_color(&md.md_type);

        let mut sections = Vec::new();

        sections.push(PopupSection::ColoredText {
            text: format!("Type: {}", md.md_type),
            rgb: [r, g, b],
            bold: true,
        });

        if let Some(ref concerning) = md.concerning {
            sections.push(PopupSection::Heading(format!("Concerning: {}", concerning)));
        }

        sections.push(PopupSection::Separator);

        sections.push(PopupSection::ScrollableText {
            text: md.text.clone(),
            monospace: true,
            max_height: 350.0,
        });

        sections.push(PopupSection::Separator);

        if !md.link.is_empty() {
            sections.push(PopupSection::Link {
                label: "Open on SPC website".into(),
                url: md.link.clone(),
            });
        }

        PopupContent {
            title: format!("Mesoscale Discussion #{:04}", md.number),
            accent_rgb: [r, g, b],
            width: 420.0,
            sections,
            actions: Vec::new(),
        }
    }

    fn matches(&self, other: &dyn OverlayItem) -> bool {
        other
            .as_any()
            .downcast_ref::<DiscussionItem>()
            .is_some_and(|o| o.md.number == self.md.number)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

pub(crate) struct SpcDiscussionHandler {
    pub state: OverlayState<Vec<Arc<DiscussionItem>>>,
    pub enabled: bool,
}

impl SpcDiscussionHandler {
    pub fn new() -> Self {
        Self {
            state: OverlayState::new(),
            enabled: true,
        }
    }
}

impl OverlayHandler for SpcDiscussionHandler {
    fn kind(&self) -> OverlayKind {
        OverlayKind::SpcDiscussions
    }

    fn display_name(&self) -> &str {
        "SPC Mesoscale Discussions"
    }

    fn render_mode(&self) -> RenderMode {
        RenderMode::Texture
    }

    fn default_enabled(&self) -> bool {
        true
    }

    fn is_enabled(&self) -> bool {
        self.enabled
    }

    fn data_generation(&self) -> u64 {
        self.state.data_generation
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

    fn auto_poll_interval(&self) -> Option<u64> {
        Some(120)
    }

    fn item_count(&self) -> usize {
        self.state.data.len()
    }

    fn clickable_items(&self) -> Vec<ClickableItem> {
        self.state.data.iter()
            .filter(|item| !item.md.polygon.is_empty())
            .map(|item| {
                let label = item.md.polygon.first()
                    .filter(|ring| !ring.is_empty())
                    .map(|ring| {
                        let n = ring.len() as f64;
                        let lat = ring.iter().map(|&(lat, _)| lat).sum::<f64>() / n;
                        let lon = ring.iter().map(|&(_, lon)| lon).sum::<f64>() / n;
                        OverlayLabel {
                            lat,
                            lon,
                            text: format!("MD {}", item.md.number),
                            color: md_stroke_color(&item.md.md_type),
                        }
                    });
                ClickableItem {
                    features: vec![item.md.feature.clone()],
                    label,
                    item: item.clone() as Arc<dyn OverlayItem>,
                }
            })
            .collect()
    }

    fn apply_fetch_result(&mut self, result: Box<dyn Any + Send>) {
        let Some(fetch) = result.downcast::<SpcDiscussionFetchResult>().ok() else {
            log::error!("SPC discussion handler received unexpected fetch result type");
            return;
        };
        match fetch.0 {
            Ok(discussions) => {
                log::info!("Received {} SPC Mesoscale Discussions", discussions.len());
                let items = discussions
                    .into_iter()
                    .map(|md| Arc::new(DiscussionItem { md }))
                    .collect();
                self.state.set_data(items);
            }
            Err(e) => {
                log::error!("SPC MD fetch failed: {}", e);
            }
        }
        self.state.fetching = false;
    }

    fn retain_selections(&self, selections: &mut Vec<Arc<dyn OverlayItem>>) {
        selections.retain(|sel| {
            // Keep non-discussion selections
            if sel.kind() != OverlayKind::SpcDiscussions {
                return true;
            }
            // Keep only if still in our data
            self.state.data.iter().any(|item| item.matches(sel.as_ref()))
        });
    }

    fn prepare_rasterize(
        &self,
        _ctx: &RasterizeContext,
    ) -> Option<Box<dyn FnOnce(&GeoBounds, u32, u32) -> RasterizeOutput + Send>> {
        if self.state.data.is_empty() {
            return None;
        }
        let discussions: Vec<SpcDiscussion> = self.state.data.iter().map(|i| i.md.clone()).collect();
        Some(Box::new(move |bounds: &GeoBounds, width, height| {
            let rgba = rasterize::rasterize_spc_discussions(&discussions, bounds, width, height);
            RasterizeOutput { rgba, hit_map: None }
        }))
    }

    fn create_fetch_tasks(&self, ctx: &FetchConfig) -> Vec<FetchTask> {
        log::info!("Fetching SPC Mesoscale Discussions");
        let client = ctx.client.clone();
        vec![FetchTask {
            kind: OverlayKind::SpcDiscussions,
            future: Box::pin(async move {
                let result = crate::spc::fetch::fetch_active_discussions(&client)
                    .await
                    .map_err(|e| e.to_string());
                Box::new(SpcDiscussionFetchResult(result)) as Box<dyn Any + Send>
            }),
        }]
    }

    fn controls(&self, _ctx: &PaneControlContext<'_>) -> Vec<ControlItem> {
        let count = self.state.data.len();
        let label = if count == 0 {
            "\u{1f4cb}  Mesoscale Disc.".to_string()
        } else {
            format!("\u{1f4cb}  Mesoscale Disc. ({count})")
        };

        let mut items = vec![
            ControlItem::Toggle { id: "enabled", label, enabled: self.enabled },
        ];

        if self.enabled {
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
            if let Some(t) = self.state.fetch_time {
                let secs = t.elapsed().as_secs();
                let text = if secs < 60 {
                    format!("Updated {secs}s ago")
                } else {
                    format!("Updated {}m ago", secs / 60)
                };
                items.push(ControlItem::InfoText { text });
            }
        }

        items
    }

    fn apply_control(&mut self, update: &ControlUpdate, _ctx: &mut PaneControlContextMut<'_>) -> ControlEffect {
        match update.id {
            "enabled" => {
                if let ControlValue::Bool(val) = update.value {
                    self.enabled = val;
                    if val && !self.has_data() && !self.state.fetching {
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
        serde_json::json!({ "enabled": self.enabled })
    }

    fn deserialize_state(&mut self, value: serde_json::Value) {
        if let Some(enabled) = value.get("enabled").and_then(|v| v.as_bool()) {
            self.enabled = enabled;
        }
    }
}
