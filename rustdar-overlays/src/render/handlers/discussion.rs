use std::any::Any;

use crate::render::layers::LayerManager;
use crate::render::overlay_state::{
    ClickableItem, FetchConfig, FetchTask, OverlayHandler, OverlayKind, OverlayState,
    PopupContent, PopupSection, RasterizeContext, SelectedOverlay,
};
use crate::render::rasterize::{self, RasterizeOutput};
use crate::spc::colors::md_stroke_color;
use crate::spc::discussion::SpcDiscussion;
use crate::types::{GeoBounds, OverlayLabel};

/// Type-erased fetch result for SPC Mesoscale Discussions.
pub(crate) struct SpcDiscussionFetchResult(pub Result<Vec<SpcDiscussion>, String>);

pub(crate) struct SpcDiscussionHandler {
    pub state: OverlayState<Vec<SpcDiscussion>>,
}

impl SpcDiscussionHandler {
    pub fn new() -> Self {
        Self {
            state: OverlayState::new(),
        }
    }
}

impl OverlayHandler for SpcDiscussionHandler {
    fn kind(&self) -> OverlayKind {
        OverlayKind::SpcDiscussions
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

    fn clickable_items(&self, _layers: &LayerManager) -> Vec<ClickableItem<'_>> {
        self.state.data.iter()
            .filter(|md| !md.polygon.is_empty())
            .map(|md| {
                let label = md.polygon.first()
                    .filter(|ring| !ring.is_empty())
                    .map(|ring| {
                        let n = ring.len() as f64;
                        let lat = ring.iter().map(|&(lat, _)| lat).sum::<f64>() / n;
                        let lon = ring.iter().map(|&(_, lon)| lon).sum::<f64>() / n;
                        OverlayLabel {
                            lat,
                            lon,
                            text: format!("MD {}", md.number),
                            color: md_stroke_color(&md.md_type),
                        }
                    });
                ClickableItem {
                    features: vec![&md.feature],
                    label,
                    id: SelectedOverlay::Discussion(md.number),
                }
            })
            .collect()
    }

    fn popup_content(&self, selected: &SelectedOverlay) -> Option<PopupContent> {
        let SelectedOverlay::Discussion(md_number) = selected else { return None };
        let md = self.state.data.iter().find(|d| d.number == *md_number)?;
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

        Some(PopupContent {
            title: format!("Mesoscale Discussion #{:04}", md.number),
            accent_rgb: [r, g, b],
            width: 420.0,
            sections,
            actions: Vec::new(),
        })
    }

    fn apply_fetch_result(&mut self, result: Box<dyn Any + Send>) {
        let Some(fetch) = result.downcast::<SpcDiscussionFetchResult>().ok() else {
            log::error!("SPC discussion handler received unexpected fetch result type");
            return;
        };
        match fetch.0 {
            Ok(discussions) => {
                log::info!("Received {} SPC Mesoscale Discussions", discussions.len());
                self.state.set_data(discussions);
            }
            Err(e) => {
                log::error!("SPC MD fetch failed: {}", e);
            }
        }
        self.state.fetching = false;
    }

    fn retain_selections(&self, selections: &mut Vec<SelectedOverlay>) {
        let numbers: std::collections::HashSet<u32> =
            self.state.data.iter().map(|d| d.number).collect();
        selections.retain(|sel| match sel {
            SelectedOverlay::Discussion(num) => numbers.contains(num),
            _ => true,
        });
    }

    fn prepare_rasterize(
        &self,
        _ctx: &RasterizeContext,
    ) -> Option<Box<dyn FnOnce(&GeoBounds, u32, u32) -> RasterizeOutput + Send>> {
        if self.state.data.is_empty() {
            return None;
        }
        let discussions = self.state.data.clone();
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
                    .map_err(|e| format!("{e}"));
                Box::new(SpcDiscussionFetchResult(result)) as Box<dyn Any + Send>
            }),
        }]
    }
}
