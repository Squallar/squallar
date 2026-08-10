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
use crate::spc::colors::md_stroke_color;
use crate::spc::discussion::SpcDiscussion;
use crate::types::{GeoBounds, OverlayLabel};

pub(crate) struct SpcDiscussionFetchResult(pub Result<Vec<SpcDiscussion>, String>);

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

    // A simple toggle handler, like `sites` and `labels`: without this
    // override, `set_active_pane_overlay`'s `set_enabled` is a silent no-op
    // for MDs and the saved config keeps the old value.
    fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    fn data_generation(&self) -> u64 {
        self.state.data_generation
    }

    /// What this handler would draw, not what it fetched: an order-free fold
    /// over the numbers of the MDs that would paint — those with a polygon,
    /// the same filter [`clickable_items`] applies — and `0` while the
    /// toggle is off. SPC discussions poll every two minutes and mostly
    /// return the same set, and a floor recomposed on every poll is a floor
    /// recomposed for nothing; the signature moves exactly when an MD
    /// issues, expires, or the checkbox flips.
    ///
    /// [`clickable_items`]: SpcDiscussionHandler::clickable_items
    fn content_signature(&self) -> u64 {
        use std::hash::{DefaultHasher, Hash, Hasher};
        if !self.enabled {
            return 0;
        }
        let mut folded = 0u64;
        let mut visible = 0u64;
        for item in &self.state.data {
            if item.md.polygon.is_empty() {
                continue;
            }
            let mut hasher = DefaultHasher::new();
            item.md.number.hash(&mut hasher);
            folded ^= hasher.finish();
            visible += 1;
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

    fn fetch_time(&self) -> Option<web_time::Instant> {
        self.state.fetch_time
    }

    fn auto_poll_interval(&self) -> Option<u64> {
        Some(120)
    }

    fn item_count(&self) -> usize {
        self.state.data.len()
    }

    fn clickable_items(&self) -> Vec<ClickableItem> {
        self.state
            .data
            .iter()
            .filter(|item| !item.md.polygon.is_empty())
            .map(|item| {
                let label = item
                    .md
                    .polygon
                    .first()
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

    fn apply_fetch_result(&mut self, result: FetchPayload) {
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
            if sel.kind() != OverlayKind::SpcDiscussions {
                return true;
            }
            self.state
                .data
                .iter()
                .any(|item| item.matches(sel.as_ref()))
        });
    }

    fn prepare_rasterize(&self, _ctx: &RasterizeContext) -> Option<RasterizeFn> {
        if self.state.data.is_empty() {
            return None;
        }
        let discussions: Vec<SpcDiscussion> =
            self.state.data.iter().map(|i| i.md.clone()).collect();
        Some(Box::new(move |bounds: &GeoBounds, width, height| {
            let rgba = rasterize::rasterize_spc_discussions(&discussions, bounds, width, height);
            RasterizeOutput {
                rgba,
                hit_map: None,
            }
        }))
    }

    fn create_fetch_tasks(&self, ctx: &FetchConfig) -> Vec<FetchTask> {
        log::info!("Fetching SPC Mesoscale Discussions");
        // NOT `ctx.client`: SPC answers OPTIONS with 403, so a `User-Agent`
        // makes this fail in the browser. See `spc::fetch`.
        let client = match crate::spc::fetch::spc_client(&ctx.sources) {
            Ok(c) => c,
            Err(e) => {
                log::error!("{e}");
                return Vec::new();
            }
        };
        let sources = ctx.sources.clone();
        vec![FetchTask {
            kind: OverlayKind::SpcDiscussions,
            future: Box::pin(async move {
                let result = crate::spc::fetch::fetch_active_discussions(&client, &sources)
                    .await
                    .map_err(|e| e.to_string());
                Box::new(SpcDiscussionFetchResult(result)) as FetchPayload
            }),
        }]
    }

    fn controls(&self, _ctx: &PaneControlContext<'_>) -> Vec<ControlItem> {
        let count = self.state.data.len();
        let label = if count == 0 {
            "Mesoscale Disc.".to_string()
        } else {
            format!("Mesoscale Disc. ({count})")
        };

        let mut items = vec![ControlItem::Toggle {
            id: "enabled",
            label,
            enabled: self.enabled,
        }];

        if self.enabled {
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

    fn apply_control(
        &mut self,
        update: &ControlUpdate,
        _ctx: &mut PaneControlContextMut<'_>,
    ) -> ControlEffect {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spc::colors::{md_fill_color, md_stroke_color};
    use crate::spc::discussion::MdType;
    use crate::types::HatchPattern;

    /// A minimal convective MD with a polygon, identified by `number`.
    fn md(number: u32) -> SpcDiscussion {
        let md_type = MdType::Convective;
        let polygon = vec![vec![(35.0, -97.0), (35.5, -97.0), (35.5, -96.5)]];
        let feature = crate::types::OverlayFeature::new(
            vec![polygon.clone()],
            md_fill_color(&md_type),
            md_stroke_color(&md_type),
            format!("MD {number}"),
            String::new(),
            HatchPattern::None,
        );
        SpcDiscussion {
            number,
            title: format!("Mesoscale Discussion #{number:04}"),
            text: String::new(),
            link: String::new(),
            md_type,
            polygon,
            feature,
            concerning: None,
        }
    }

    fn handler_with(mds: Vec<SpcDiscussion>) -> SpcDiscussionHandler {
        let mut handler = SpcDiscussionHandler::new();
        handler.apply_fetch_result(Box::new(SpcDiscussionFetchResult(Ok(mds))));
        handler
    }

    /// The signature names the **set**, not the fetch: MDs poll every two
    /// minutes, and a refetch returning the same discussions must keep the
    /// signature — which `data_generation`, bumped on every `set_data`,
    /// cannot do. Swap the body for `self.data_generation()` and this test
    /// fails on the second fetch.
    #[test]
    fn a_refetch_of_the_same_discussion_set_keeps_the_signature() {
        let mut handler = handler_with(vec![md(101), md(102)]);
        let first = handler.content_signature();
        handler.apply_fetch_result(Box::new(SpcDiscussionFetchResult(Ok(vec![
            md(101),
            md(102),
        ]))));
        assert_ne!(
            handler.data_generation(),
            1,
            "the fixture must have refetched",
        );
        assert_eq!(
            handler.content_signature(),
            first,
            "an unchanged MD set must keep its signature across a refetch",
        );
    }

    /// Every change to what would draw moves the signature: an MD issuing,
    /// one expiring, and the checkbox flipping off (the floor follows the
    /// handler's global toggle, and `clickable_items` does not read it).
    #[test]
    fn every_change_to_the_drawn_set_moves_the_signature() {
        let mut handler = handler_with(vec![md(101)]);
        let one = handler.content_signature();

        handler.apply_fetch_result(Box::new(SpcDiscussionFetchResult(Ok(vec![
            md(101),
            md(102),
        ]))));
        let two = handler.content_signature();
        assert_ne!(one, two, "an MD issuing must move the signature");

        handler.apply_fetch_result(Box::new(SpcDiscussionFetchResult(Ok(vec![md(102)]))));
        assert_ne!(
            handler.content_signature(),
            two,
            "an MD expiring must move the signature",
        );

        handler.set_enabled(false);
        assert_eq!(
            handler.content_signature(),
            0,
            "the toggle off must zero the signature — the floor would draw nothing",
        );
    }

    /// A set fold, not a sequence fold: the fetch order of the same MDs is
    /// not a picture change.
    #[test]
    fn the_signature_is_a_set_signature_not_a_sequence_signature() {
        let forward = handler_with(vec![md(101), md(102), md(103)]);
        let reversed = handler_with(vec![md(103), md(102), md(101)]);
        assert_eq!(
            forward.content_signature(),
            reversed.content_signature(),
            "the same MDs in another order draw the same picture",
        );
    }
}
