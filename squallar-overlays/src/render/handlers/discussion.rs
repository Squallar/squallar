use crate::render::overlay_state::{PaneMut, PaneRef, PaneToggle};
use std::sync::Arc;

use crate::fetch_policy::{FetchError, FetchRetry, Whole};
use crate::render::controls::{
    ControlButton, ControlEffect, ControlItem, ControlUpdate, ControlValue,
};
use crate::render::overlay_state::Surface;
use crate::render::overlay_state::{
    ClickableItem, FetchConfig, FetchPayload, FetchTask, OverlayHandler, OverlayItem, OverlayState,
    PopupContent, PopupSection, RasterizeContext, RenderMode,
};
use crate::render::rasterize;
use crate::spc::colors::md_stroke_color;
use crate::spc::discussion::SpcDiscussion;
use crate::types::OverlayLabel;
use squallar_source::id::{LayerId, known};
use squallar_source::job::{DescribedJob, JobCodec};
use squallar_source::time::TimeAxis;

pub struct SpcDiscussionFetchResult(pub Result<Vec<SpcDiscussion>, FetchError>);
impl crate::fetch_policy::FetchRound for SpcDiscussionFetchResult {
    type Shape = crate::fetch_policy::Whole;
}

#[derive(Debug)]
pub(crate) struct DiscussionItem {
    pub md: SpcDiscussion,
}

impl OverlayItem for DiscussionItem {
    fn layer_id(&self) -> LayerId {
        known::SPC_DISCUSSIONS
    }

    fn popup_content(&self, _prefs: &squallar_units::UserPreferences) -> PopupContent {
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
    pub state: OverlayState<Vec<Arc<DiscussionItem>>, Whole>,
    /// **The registry's own copy**, used only where no pane is supplied. The
    /// config swap keeps it in step until WO-M10c deletes the swap; every
    /// answer prefers [`PaneRef::state`] when there is one.
    pub enabled: bool,
    /// The "MD 1234" map labels, rebuilt whenever `state.data` is — never per frame.
    labels: Vec<OverlayLabel>,
    signature_memo: crate::render::signature_memo::SignatureMemo,
    /// How many items [`Self::content_signature`]'s fold has walked, for the
    /// memo gate: an unchanged (generation, view) call must add zero.
    #[cfg(test)]
    pub(crate) sig_item_visits: std::cell::Cell<u64>,
}

impl SpcDiscussionHandler {
    pub fn new() -> Self {
        Self {
            state: OverlayState::new(),
            enabled: true,
            labels: Vec::new(),
            signature_memo: crate::render::signature_memo::SignatureMemo::new(),
            #[cfg(test)]
            sig_item_visits: std::cell::Cell::new(0),
        }
    }

    fn paint_input(&self, ctx: &RasterizeContext) -> Option<rasterize::DiscussionsInput> {
        if self.state.data.is_empty() {
            return None;
        }
        Some(rasterize::DiscussionsInput {
            discussions: self
                .state
                .data
                .iter()
                // **The as-of filter**: two `Option` comparisons against the
                // window parsed once at fetch time from the product's own
                // `VALID` line. A discussion unbounded on a side passes on that
                // side. On a live pane `as_of` is the wall clock and the feed
                // only carries discussions that are in force, so the rows are
                // exactly what they were before this filter existed.
                .filter(|i| {
                    i.md.valid_from.is_none_or(|from| from <= ctx.as_of)
                        && i.md.valid_until.is_none_or(|until| ctx.as_of < until)
                })
                .map(|i| rasterize::DiscussionPaint {
                    md_type: i.md.md_type,
                    polygon: i.md.polygon.clone(),
                })
                .collect(),
            device_scale: ctx.device_scale,
        })
    }
}

/// The label for one MD: its first ring's centroid, its number, its type
/// colour — or `None` where there is no ring to place it on.
fn md_label(md: &SpcDiscussion) -> Option<OverlayLabel> {
    let ring = md.polygon.first().filter(|ring| !ring.is_empty())?;
    let n = ring.len() as f64;
    let lat = ring.iter().map(|&(lat, _)| lat).sum::<f64>() / n;
    let lon = ring.iter().map(|&(_, lon)| lon).sum::<f64>() / n;
    Some(OverlayLabel {
        lat,
        lon,
        text: format!("MD {}", md.number),
        color: md_stroke_color(&md.md_type),
    })
}

impl OverlayHandler for SpcDiscussionHandler {
    fn id(&self) -> LayerId {
        known::SPC_DISCUSSIONS
    }
    fn surface(&self) -> Surface {
        Surface::Ground
    }
    fn draw_order_weight(&self) -> u32 {
        40
    }

    fn display_name(&self) -> &str {
        "SPC Mesoscale Discussions"
    }

    fn render_mode(&self) -> RenderMode {
        RenderMode::Texture
    }

    /// Every discussion carries a validity window — its `VALID DDHHMM - DDHHMM`
    /// line — and the picture is which of them are in force at the depicted
    /// instant. That is the definition of [`TimeAxis::EventLifetime`], and
    /// declaring it is what makes a scrubbed pane's `as_of` reach this layer at
    /// all: the caller hands a `Live` layer the wall clock by contract, so
    /// while this said `Live` the archive branch in `create_fetch_tasks` below
    /// was unreachable and a pane parked on a 2013 storm drew today's
    /// discussions over it.
    fn time_axis(&self) -> TimeAxis {
        TimeAxis::EventLifetime
    }

    /// **One instant per stop, and that is the whole ask.**
    ///
    /// Same shape and same reason as the NWS alerts layer: a discussion carries
    /// its own `VALID` window, and the picture at a stop is which discussions
    /// are in force *then*. Nothing about a stop obliges this layer to hold a
    /// *stretch* of source time the way lightning's fade ramp does — a
    /// discussion issued two hours before a stop and one issued a minute before
    /// it are both drawn at that stop if the stop is inside their windows, and
    /// neither is found by reaching further back from it.
    ///
    /// A zero-width range is a real ask, not an empty one.
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
        PaneToggle::is_on(pane, self.enabled)
    }

    // A simple toggle handler, like `sites` and `labels`: without this
    // override, `set_active_pane_overlay`'s `set_enabled` is a silent no-op
    // for MDs and the saved config keeps the old value.
    fn set_enabled(&mut self, enabled: bool, pane: &mut PaneMut<'_>) {
        if !PaneToggle::set(pane, enabled) {
            self.enabled = enabled;
        }
    }

    fn data_generation(&self) -> u64 {
        self.state.data_generation
    }

    /// Memoized on the generation alone: the fold below is O(discussions) and
    /// its callers ask per pane per layer per frame, while the set moves only
    /// on a poll. The layer's one view input is the toggle, and the off arm
    /// answers before the memo — every enabled view folds the same set.
    fn content_signature(&self, pane: &PaneRef<'_>) -> u64 {
        use std::hash::{DefaultHasher, Hash, Hasher};
        if !self.is_enabled(pane) {
            return 0;
        }
        self.signature_memo
            .get_or_compute(self.state.data_generation, 0, || {
                let mut folded = 0u64;
                let mut visible = 0u64;
                for item in &self.state.data {
                    #[cfg(test)]
                    self.sig_item_visits.set(self.sig_item_visits.get() + 1);
                    if item.md.polygon.is_empty() {
                        continue;
                    }
                    let mut hasher = DefaultHasher::new();
                    item.md.number.hash(&mut hasher);
                    folded ^= hasher.finish();
                    visible += 1;
                }
                folded ^ visible.rotate_left(32)
            })
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

    fn retry(&self) -> Option<&FetchRetry> {
        Some(&self.state.retry)
    }

    fn retry_mut(&mut self) -> Option<&mut FetchRetry> {
        Some(&mut self.state.retry)
    }

    fn fetch_time(&self) -> Option<web_time::Instant> {
        self.state.fetch_time
    }

    fn auto_poll_interval(&self) -> Option<u64> {
        Some(120)
    }

    fn item_count(&self, _pane: &PaneRef<'_>) -> usize {
        self.state.data.len()
    }

    fn clickable_items<'a>(&'a self, _pane: &PaneRef<'_>) -> Vec<ClickableItem<'a>> {
        self.state
            .data
            .iter()
            .filter(|item| !item.md.polygon.is_empty())
            .map(|item| ClickableItem {
                features: std::slice::from_ref(&item.md.feature),
                item: item.clone() as Arc<dyn OverlayItem>,
            })
            .collect()
    }

    fn map_labels(&self) -> &[OverlayLabel] {
        &self.labels
    }

    fn apply_fetch_result(&mut self, result: FetchPayload, _pane: &PaneRef<'_>) {
        let Some(fetch) = self
            .state
            .downcast_round::<SpcDiscussionFetchResult>(result)
        else {
            log::error!("SPC discussion handler received unexpected fetch result type");
            return;
        };
        match fetch.0 {
            Ok(discussions) => {
                log::info!("Received {} SPC Mesoscale Discussions", discussions.len());
                self.labels = discussions
                    .iter()
                    .filter(|md| !md.polygon.is_empty())
                    .filter_map(md_label)
                    .collect();
                let items = discussions
                    .into_iter()
                    .map(|md| Arc::new(DiscussionItem { md }))
                    .collect();
                self.state.set_data(items);
            }
            Err(e) => {
                log::error!("SPC MD fetch failed: {e}");
                self.state.record_failure(&e);
            }
        }
    }

    fn retain_selections(&self, selections: &mut Vec<Arc<dyn OverlayItem>>, _pane: &PaneRef<'_>) {
        selections.retain(|sel| {
            if sel.layer_id() != known::SPC_DISCUSSIONS {
                return true;
            }
            self.state
                .data
                .iter()
                .any(|item| item.matches(sel.as_ref()))
        });
    }

    fn prepare_job(&self, ctx: &RasterizeContext, _pane: &PaneRef<'_>) -> Option<DescribedJob> {
        self.paint_input(ctx).map(DescribedJob::new)
    }

    fn job_codec(&self) -> Option<&'static JobCodec> {
        crate::render::jobs::JOB_CODECS
            .iter()
            .find(|row| row.label == "overlay/discussions")
    }

    fn create_fetch_tasks(&self, ctx: &FetchConfig, _pane: &PaneRef<'_>) -> Vec<FetchTask> {
        // THE ARCHIVE, FOR A PANE THAT IS NOT LOOKING AT NOW.
        //
        // `spcmdrss.xml` is a standing feed of what is active at the moment it
        // is asked and holds no history, so a pane scrubbed to a past storm drew
        // today's discussions over it. Same shape, same threshold and same
        // reason as the NWS warnings above it in the stack — see
        // `alert.rs`'s `ARCHIVE_CUTOFF_MINUTES`, which this shares so the two
        // layers can never disagree about which side of "now" a pane is on.
        let archived_before = ctx.as_of
            < chrono::Utc::now().naive_utc()
                - chrono::Duration::minutes(super::alert::ARCHIVE_CUTOFF_MINUTES);
        if archived_before {
            let sources = ctx.sources.clone();
            let at = ctx.as_of;
            return vec![FetchTask {
                kind: known::SPC_DISCUSSIONS,
                future: Box::pin(async move {
                    let result =
                        crate::spc::discussion_archive::fetch_archived_discussions(&sources, at)
                            .await;
                    Box::new(SpcDiscussionFetchResult(result)) as FetchPayload
                }),
            }];
        }

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
            kind: known::SPC_DISCUSSIONS,
            future: Box::pin(async move {
                let result = crate::spc::fetch::fetch_active_discussions(&client, &sources).await;
                Box::new(SpcDiscussionFetchResult(result)) as FetchPayload
            }),
        }]
    }

    fn controls(&self, pane: &PaneRef<'_>) -> Vec<ControlItem> {
        let count = self.state.data.len();
        let label = if count == 0 {
            "Mesoscale Disc.".to_string()
        } else {
            format!("Mesoscale Disc. ({count})")
        };

        let mut items = vec![ControlItem::Toggle {
            id: "enabled",
            label,
            enabled: self.is_enabled(pane),
        }];

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

        items
    }

    fn apply_control(&mut self, update: &ControlUpdate, pane: &mut PaneMut<'_>) -> ControlEffect {
        match update.id {
            "enabled" => {
                if let ControlValue::Bool(val) = update.value {
                    if !PaneToggle::set(pane, val) {
                        self.enabled = val;
                    }
                    if val
                        && self
                            .state
                            .enable_should_refetch(self.has_data(&pane.as_ref()))
                    {
                        return ControlEffect::Fetch;
                    }
                }
                ControlEffect::None
            }
            "refresh" => ControlEffect::Fetch,
            _ => ControlEffect::None,
        }
    }

    // ── Per-pane state (WO-M10c) ──────────────────────────
    //
    // This layer's only per-pane fact is whether the pane draws it, so its
    // state IS the toggle. `self.enabled` survives as the registry's own copy
    // until the swap dies in this same order; every answer above prefers the
    // pane's when a pane is supplied.

    fn create_pane_state(&self, enabled: bool) -> Option<FetchPayload> {
        PaneToggle::create(enabled)
    }

    fn deserialize_pane_state(
        &self,
        value: serde_json::Value,
        enabled: bool,
    ) -> Option<FetchPayload> {
        PaneToggle::restore(&value, enabled)
    }

    fn serialize_pane_state(&self, state: &dyn std::any::Any) -> serde_json::Value {
        PaneToggle::save(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spc::colors::{md_fill_color, md_stroke_color};
    use crate::spc::discussion::MdType;
    use crate::types::HatchPattern;

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
            // Unbounded: these fixtures are about the drawn set, not the
            // window, and an unbounded discussion passes the as-of filter at
            // every instant.
            valid_from: None,
            valid_until: None,
        }
    }

    fn handler_with(mds: Vec<SpcDiscussion>) -> SpcDiscussionHandler {
        let mut handler = SpcDiscussionHandler::new();
        handler.apply_fetch_result(
            Box::new(SpcDiscussionFetchResult(Ok(mds))),
            &PaneRef::across(&[]),
        );
        handler
    }

    #[test]
    fn a_refetch_of_the_same_discussion_set_keeps_the_signature() {
        let mut handler = handler_with(vec![md(101), md(102)]);
        let first = handler.content_signature(&PaneRef::bare(0));
        handler.apply_fetch_result(
            Box::new(SpcDiscussionFetchResult(Ok(vec![md(101), md(102)]))),
            &PaneRef::across(&[]),
        );
        assert_ne!(
            handler.data_generation(),
            1,
            "the fixture must have refetched",
        );
        assert_eq!(
            handler.content_signature(&PaneRef::bare(0)),
            first,
            "an unchanged MD set must keep its signature across a refetch",
        );
    }

    #[test]
    fn every_change_to_the_drawn_set_moves_the_signature() {
        let mut handler = handler_with(vec![md(101)]);
        let one = handler.content_signature(&PaneRef::bare(0));

        handler.apply_fetch_result(
            Box::new(SpcDiscussionFetchResult(Ok(vec![md(101), md(102)]))),
            &PaneRef::across(&[]),
        );
        let two = handler.content_signature(&PaneRef::bare(0));
        assert_ne!(one, two, "an MD issuing must move the signature");

        handler.apply_fetch_result(
            Box::new(SpcDiscussionFetchResult(Ok(vec![md(102)]))),
            &PaneRef::across(&[]),
        );
        assert_ne!(
            handler.content_signature(&PaneRef::bare(0)),
            two,
            "an MD expiring must move the signature",
        );

        handler.set_enabled(false, &mut PaneMut::bare(0));
        assert_eq!(
            handler.content_signature(&PaneRef::bare(0)),
            0,
            "the toggle off must zero the signature — the floor would draw nothing",
        );
    }

    #[test]
    fn the_map_labels_follow_the_discussion_set() {
        let mut no_ring = md(103);
        no_ring.polygon = Vec::new();
        let mut handler = handler_with(vec![md(101), md(102), no_ring]);

        let text: Vec<String> = handler
            .map_labels()
            .iter()
            .map(|l| l.text.clone())
            .collect();
        assert_eq!(
            text,
            vec!["MD 101".to_string(), "MD 102".to_string()],
            "an MD with no ring has nowhere to put a label, so it gets none",
        );

        let ring = &md(101).polygon[0];
        let n = ring.len() as f64;
        let label = &handler.map_labels()[0];
        assert!(
            (label.lat - ring.iter().map(|&(lat, _)| lat).sum::<f64>() / n).abs() < 1e-9
                && (label.lon - ring.iter().map(|&(_, lon)| lon).sum::<f64>() / n).abs() < 1e-9,
            "the label is not on its ring's centroid",
        );

        // 101 expires; 104 issues.
        handler.apply_fetch_result(
            Box::new(SpcDiscussionFetchResult(Ok(vec![md(102), md(104)]))),
            &PaneRef::across(&[]),
        );
        let text: Vec<String> = handler
            .map_labels()
            .iter()
            .map(|l| l.text.clone())
            .collect();
        assert_eq!(
            text,
            vec!["MD 102".to_string(), "MD 104".to_string()],
            "a refetch must replace the labels, not accumulate them",
        );
    }

    /// The fold's inputs move on a poll or the toggle, but its callers ask per
    /// pane per layer per frame — so a repeat ask must not walk the items.
    #[test]
    fn an_unchanged_generation_and_view_never_revisits_the_items() {
        let handler = handler_with(vec![md(101), md(102)]);
        let first = handler.content_signature(&PaneRef::bare(0));
        let warmed = handler.sig_item_visits.get();
        assert!(
            warmed > 0,
            "fixture: the first call really folded the items"
        );

        let second = handler.content_signature(&PaneRef::bare(0));
        assert_eq!(second, first);
        assert_eq!(
            handler.sig_item_visits.get(),
            warmed,
            "an unchanged generation and view walked the items again",
        );
    }

    /// One recompute per data move, however many panes ask: three calls after
    /// a refetch walk the two items exactly once between them.
    #[test]
    fn a_generation_bump_refolds_exactly_once() {
        let mut handler = handler_with(vec![md(101)]);
        handler.content_signature(&PaneRef::bare(0));

        handler.apply_fetch_result(
            Box::new(SpcDiscussionFetchResult(Ok(vec![md(101), md(102)]))),
            &PaneRef::across(&[]),
        );
        let before = handler.sig_item_visits.get();
        for _ in 0..3 {
            handler.content_signature(&PaneRef::bare(0));
        }
        assert_eq!(
            handler.sig_item_visits.get() - before,
            2,
            "three asks after one refetch must fold the two items exactly once",
        );
    }

    #[test]
    fn the_signature_is_a_set_signature_not_a_sequence_signature() {
        let forward = handler_with(vec![md(101), md(102), md(103)]);
        let reversed = handler_with(vec![md(103), md(102), md(101)]);
        assert_eq!(
            forward.content_signature(&PaneRef::bare(0)),
            reversed.content_signature(&PaneRef::bare(0)),
            "the same MDs in another order draw the same picture",
        );
    }
}
