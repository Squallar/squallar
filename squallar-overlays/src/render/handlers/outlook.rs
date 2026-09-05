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
use squallar_source::id::{LayerId, known};
use squallar_source::job::{DescribedJob, JobCodec};
use squallar_source::time::TimeAxis;

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

    fn popup_content(&self, prefs: &squallar_units::UserPreferences) -> PopupContent {
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

/// **The whole per-pane state of the outlook layer**: which day this pane is
/// looking at and which of that day's products it lets through. Both were
/// fields of the handler, so two panes could never sit on two days.
///
/// There is no `enabled` beside the set — for this layer "on" **is** a
/// non-empty product set, and a bool next to it is a second copy free to
/// disagree with the thing it was derived from.
///
/// The round bookkeeping is deliberately NOT here: one fetch round is one
/// request per product for the whole application, and every pane's selection
/// contributes to it. It stays on the handler and is scoped by the **union**
/// of the panes — see [`SpcOutlookHandler::union_scope`].
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct OutlookPaneState {
    pub selected_day: OutlookDay,
    pub enabled_products: HashSet<OutlookProduct>,
}

impl OutlookPaneState {
    /// A pane that has saved nothing. `enabled` is the pane's own slot flag,
    /// and for this layer "on" means the day's first product — the same
    /// answer `set_enabled(true)` gives.
    fn new(enabled: bool) -> Self {
        let mut state = Self {
            selected_day: OutlookDay::Day1,
            enabled_products: HashSet::new(),
        };
        if enabled && let Some(&first) = state.selected_day.products().first() {
            state.enabled_products.insert(first);
            state.sync_implied_products();
        }
        state
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

    /// The `(day, product)` keys **this pane** is asking for, appended in
    /// publication order and never duplicated — so a one-pane union is
    /// byte-for-byte the walk this layer has always made.
    fn extend_scope(&self, into: &mut Vec<(OutlookDay, OutlookProduct)>) {
        for &product in self.selected_day.products() {
            if !self.enabled_products.contains(&product) {
                continue;
            }
            let key = (self.selected_day, product);
            if !into.contains(&key) {
                into.push(key);
            }
        }
    }
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
    /// **The registry's own copy**, used only where no pane is supplied. The
    /// config swap keeps it in step until WO-M10c deletes the swap; every
    /// answer prefers [`PaneRef::state`] when there is one.
    pub defaults: OutlookPaneState,
    /// The last built paint input per (combined generation, scope, in-force
    /// set, theme, device scale) — see [`Self::prepare_job`].
    pub(crate) job_memo: crate::render::signature_memo::JobMemo,
}

impl SpcOutlookHandler {
    pub fn new() -> Self {
        Self {
            // Parked, because this handler implements `take_retired`:
            // the two are set together, so a park always has a drain.
            state: OverlayState::parked(),
            per_product_generation: HashMap::new(),
            config_generation: 0,
            per_product_error: HashMap::new(),
            outstanding: 0,
            round_answered_in_scope: false,
            round_stray_failures: Vec::new(),
            defaults: OutlookPaneState::new(false),
            job_memo: crate::render::signature_memo::JobMemo::new(
                crate::render::footprint::outlooks_job,
            ),
        }
    }

    /// **This pane's answer, or the registry's own copy** when no pane was
    /// supplied.
    fn view<'a>(&'a self, pane: &PaneRef<'a>) -> &'a OutlookPaneState {
        pane.state_as::<OutlookPaneState>()
            .unwrap_or(&self.defaults)
    }

    /// Edit this pane's state, falling back to the registry's copy for a
    /// caller that supplied no pane.
    fn edit(&mut self, pane: &mut PaneMut<'_>, f: impl FnOnce(&mut OutlookPaneState)) {
        match pane.state_as::<OutlookPaneState>() {
            Some(state) => f(state),
            None => f(&mut self.defaults),
        }
    }

    /// **Everything ANY pane is asking for**, in publication order, deduped.
    ///
    /// The round is one request per product for the whole application, so the
    /// scope it is judged against is the union: a product pane 1 still wants
    /// is in scope even when pane 0 has stopped asking, and a failure any
    /// pane's selection carries keeps the layer on the ledger. Narrowing this
    /// to one pane is how an edit in one pane takes the layer off a ledger
    /// another pane is still on.
    fn union_scope(&self, pane: &PaneRef<'_>) -> Vec<(OutlookDay, OutlookProduct)> {
        let mut scope = Vec::new();
        let mut answered = false;
        for state in pane.all_as::<OutlookPaneState>() {
            answered = true;
            state.extend_scope(&mut scope);
        }
        if !answered {
            self.defaults.extend_scope(&mut scope);
        }
        scope
    }

    /// **The one encoder** for a selection, so the registry's copy and a
    /// pane's cannot write different bytes for the same selection.
    ///
    /// Declaration order, never the `HashSet`'s: set iteration order is
    /// per-instance noise, and writing it raw makes save→load→save produce a
    /// different file every reopen.
    fn save_selection(state: &OutlookPaneState) -> serde_json::Value {
        let mut enabled: Vec<OutlookProduct> = state.enabled_products.iter().copied().collect();
        enabled.sort_by_key(|product| *product as u8);
        serde_json::json!({
            "selected_day": state.selected_day,
            "enabled_products": enabled,
        })
    }

    /// **The one decoder**, the exact inverse of [`Self::save_selection`]. A
    /// member the value does not name is left as it was, and the implied
    /// products are brought into line afterwards.
    fn restore_selection(state: &mut OutlookPaneState, value: &serde_json::Value) {
        if let Some(day) = value
            .get("selected_day")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
        {
            state.selected_day = day;
        }
        if let Some(products) = value
            .get("enabled_products")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
        {
            state.enabled_products = products;
        }
        state.sync_implied_products();
    }

    /// Move the outstanding-task count, keeping `state.fetching` in step.
    fn set_outstanding(&mut self, outstanding: usize) {
        self.outstanding = outstanding;
        self.state.fetching = outstanding > 0;
    }

    /// What every product's last answer adds up to, as a **property of the
    /// selection** rather than of the order its tasks resolved in.
    fn round_verdict(&self, scope: &[(OutlookDay, OutlookProduct)]) -> RoundVerdict {
        let asked = scope.len();

        let mut failed: Vec<(OutlookProduct, &crate::fetch_policy::FetchError)> = Vec::new();
        let mut absent: Vec<(OutlookProduct, &crate::fetch_policy::FetchError)> = Vec::new();
        let mut drew = false;
        for &key in scope {
            let product = key.1;
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
    fn round_coverage(
        &self,
        scope: &[(OutlookDay, OutlookProduct)],
    ) -> crate::fetch_policy::DataCompleteness {
        let mut expected = 0;
        let mut missing = 0;
        let mut reasons = Vec::new();
        for &key in scope {
            let product = key.1;
            expected += 1;
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
    fn file_round_verdict(&mut self, scope: &[(OutlookDay, OutlookProduct)]) {
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
            match self.round_verdict(scope) {
                RoundVerdict::Failed(e) | RoundVerdict::NotPublished(e) => {
                    self.state.retry.record_failure(&e);
                }
                RoundVerdict::Clear => self.state.retry.record_success(),
            }
        }
        let coverage = self.round_coverage(scope);
        self.state.record_coverage(coverage);
    }

    /// Every enabled product's features, concatenated in the order they will be
    /// painted — for the products whose issuance is **in force at `as_of`**.
    ///
    /// The as-of filter (WB-5, [`TimeAxis::EventLifetime`]): two `Option`
    /// comparisons against `valid`/`expire`, parsed once at fetch. A side
    /// that did not parse passes on that side — an issuance is never dropped
    /// for want of a readable time (the `NwsAlert` rule). The `expire` half
    /// is what keeps a later instant from wearing an outlook that has lapsed;
    /// a `valid`-only filter would draw every held issuance forever forward.
    fn features_in_paint_order(
        &self,
        view: &OutlookPaneState,
        as_of: chrono::NaiveDateTime,
    ) -> Vec<crate::types::OverlayFeature> {
        let mut features = Vec::new();
        for (_, outlook) in self.in_force_in_paint_order(view, as_of) {
            features.extend(outlook.features.iter().cloned());
        }
        features
    }

    /// The enabled products whose held issuance is in force at `as_of`, in
    /// paint order, with the product each came from. The one walk behind
    /// [`Self::features_in_paint_order`] and the job memo's key, so the rows
    /// that travel are the rows that are keyed.
    fn in_force_in_paint_order(
        &self,
        view: &OutlookPaneState,
        as_of: chrono::NaiveDateTime,
    ) -> impl Iterator<Item = (OutlookProduct, &SpcOutlook)> + '_ {
        let day = view.selected_day;
        let enabled = view.enabled_products.clone();
        day.products()
            .iter()
            .filter(move |product| enabled.contains(product))
            .filter_map(move |&product| {
                let outlook = self.state.data.get(&(day, product))?;
                let in_force = outlook.valid.is_none_or(|valid| valid <= as_of)
                    && outlook.expire.is_none_or(|expire| as_of < expire);
                in_force.then_some((product, outlook))
            })
    }

    /// Every term of the picture other than the data itself: the day, the
    /// products in force at `as_of` in paint order, the theme (the hatch
    /// colour reads it) and the device scale. O(products), no allocation.
    fn paint_key(&self, view: &OutlookPaneState, ctx: &RasterizeContext) -> u64 {
        use std::hash::{DefaultHasher, Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        view.selected_day.hash(&mut hasher);
        for (product, _) in self.in_force_in_paint_order(view, ctx.as_of) {
            product.hash(&mut hasher);
        }
        ctx.is_dark.hash(&mut hasher);
        ctx.device_scale.to_bits().hash(&mut hasher);
        hasher.finish()
    }

    fn paint_input(
        &self,
        ctx: &RasterizeContext,
        view: &OutlookPaneState,
    ) -> Option<rasterize::OutlooksInput> {
        let features = self.features_in_paint_order(view, ctx.as_of);
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

    /// Drop what **no pane** is asking for any more, and take the layer back
    /// off the ledger if nothing that is left is failing.
    ///
    /// Scoped by the union, not by the pane that was just edited: the ledger
    /// is one ledger, and clearing it because *this* pane's new selection is
    /// clean would hide a failure another pane's selection still carries.
    fn refile_after_selection_change(&mut self, pane: &mut PaneMut<'_>) {
        self.edit(pane, OutlookPaneState::sync_implied_products);
        let scope = self.union_scope(&pane.as_ref());
        self.per_product_error.retain(|key, _| scope.contains(key));
        if self.outstanding > 0 {
            return;
        }
        match self.round_verdict(&scope) {
            RoundVerdict::Failed(_) => {}
            RoundVerdict::NotPublished(e) => self.state.retry.record_failure(&e),
            RoundVerdict::Clear => self.state.retry.clear(),
        }
        let coverage = self.round_coverage(&scope);
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

    /// An outlook is an issuance with a validity window (`valid`/`expire`,
    /// parsed at fetch), and the picture at `as_of` is which held issuances
    /// are in force then (WB-5) — filtered in
    /// [`Self::features_in_paint_order`]. Under this arm the day-1/2/3
    /// selector reads as a *horizon* control: the clock, not the dropdown,
    /// decides whether the selected day's issuance is in force at the
    /// depicted instant.
    ///
    /// Only the **current** issuance per product is held. SPC's convective
    /// GeoJSON archive is real
    /// (`products/outlook/archive/{year}/day1otlk_{date}_{time}_{product}.lyr.geojson`),
    /// so a fetch-follows-clock supply in the GLM shape is possible — it is
    /// not wired yet, and a scrubbed instant outside the held windows draws
    /// nothing rather than guessing.
    fn time_axis(&self) -> TimeAxis {
        TimeAxis::EventLifetime
    }

    /// **One instant per stop.** An outlook is an issuance with a
    /// `valid`/`expire` window, and the picture at a stop is which held
    /// issuances are in force then — a question about the stop itself, with
    /// no stretch of source time behind it.
    ///
    /// **This does not become wider when the archive is wired.** SPC's
    /// convective GeoJSON archive is real and a fetch-follows-clock supply in
    /// the GLM shape is possible; what such a supply would fetch is still the
    /// issuances covering each stop, so the ask stays the stops and the
    /// number of round trips stays the number of distinct issuances, not the
    /// hours between them.
    fn residency_for(
        &self,
        _pane: &PaneRef<'_>,
        stops: &[chrono::NaiveDateTime],
    ) -> squallar_source::time::Residency {
        squallar_source::time::Residency::over(stops.iter().map(|&stop| (stop, stop)))
    }

    fn theme_sensitive(&self) -> bool {
        true
    }

    fn is_enabled(&self, pane: &PaneRef<'_>) -> bool {
        !self.view(pane).enabled_products.is_empty()
    }

    fn set_enabled(&mut self, enabled: bool, pane: &mut PaneMut<'_>) {
        let mut moved = false;
        self.edit(pane, |state| {
            if enabled {
                if state.enabled_products.is_empty()
                    && let Some(&first) = state.selected_day.products().first()
                {
                    state.enabled_products.insert(first);
                    state.sync_implied_products();
                    moved = true;
                }
            } else if !state.enabled_products.is_empty() {
                state.enabled_products.clear();
                moved = true;
            }
        });
        if moved {
            // Global on purpose: a generation is a cache-invalidation counter,
            // and re-rasterizing a pane whose selection did not move is
            // wasteful, never wrong. What separates two panes' textures is the
            // pane-aware `content_signature` below, not this.
            self.config_generation = self.config_generation.wrapping_add(1);
        }
    }

    /// **This pane's day and product set are in the token.** The render
    /// dispatch groups panes by it and hands one raster to the whole group, so
    /// a token that carried only `combined_generation` — which moves for every
    /// pane at once — would give one pane the other's outlook.
    fn content_signature(&self, pane: &PaneRef<'_>) -> u64 {
        use std::hash::{DefaultHasher, Hash, Hasher};
        let view = self.view(pane);
        let mut hasher = DefaultHasher::new();
        (view.selected_day as u8).hash(&mut hasher);
        // Walked from the day's own product list, never the `HashSet`'s
        // iteration order, which is per-instance noise and would make one
        // pane's token move between frames.
        for &product in view.selected_day.products() {
            if view.enabled_products.contains(&product) {
                (product as u8).hash(&mut hasher);
            }
        }
        self.combined_generation() ^ hasher.finish()
    }

    /// E.g. `"Day 1 - Categorical, Tornado"`.
    fn status_line(&self, pane: &PaneRef<'_>) -> Option<String> {
        let view = self.view(pane);
        if view.enabled_products.is_empty() {
            return None;
        }
        let products: Vec<String> = view
            .selected_day
            .products()
            .iter()
            .filter(|p| p.is_selectable() && view.enabled_products.contains(p))
            .map(|p| p.to_string())
            .collect();
        Some(format!("{} - {}", view.selected_day, products.join(", ")))
    }

    fn data_generation(&self) -> u64 {
        self.combined_generation()
    }

    /// Data **this selection** can draw, not data this layer has ever fetched.
    fn has_data(&self, pane: &PaneRef<'_>) -> bool {
        let view = self.view(pane);
        view.enabled_products.iter().any(|product| {
            self.state
                .data
                .get(&(view.selected_day, *product))
                .is_some_and(|outlook| !outlook.features.is_empty())
        })
    }

    fn is_fetching(&self) -> bool {
        self.outstanding > 0
    }

    /// The host says a round has started or been abandoned; this layer's round
    /// is one task per enabled product, so the count moves by that many.
    fn set_fetching(&mut self, fetching: bool, pane: &PaneRef<'_>) {
        if fetching {
            // **This pane's** count, not the union: the round that just
            // started is the one `create_fetch_tasks` built for this pane, and
            // that is how many answers are owed.
            let asked = self.view(pane).enabled_products.len().max(1);
            self.set_outstanding(self.outstanding + asked);
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

    fn clickable_items<'a>(&'a self, pane: &PaneRef<'_>) -> Vec<ClickableItem<'a>> {
        let view = self.view(pane);
        let day = view.selected_day;
        let mut items = Vec::new();
        for &product in &view.enabled_products {
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

    /// The generation this layer's state parked and the inputs its memo
    /// retired, handed back for the app to free off the frame thread — see
    /// [`OverlayHandler::take_retired`].
    /// **No pane draws this layer, so its round goes** — parked for the
    /// discard seam, not freed here. See [`OverlayHandler::release_data`].
    fn release_data(&mut self) -> bool {
        if !self.state.release_data() {
            return false;
        }
        // The built inputs were made from the data that just went away, and
        // nothing dispatches this layer any more, so no later `get_or_build`
        // would retire them.
        self.job_memo.retire_live_rows();
        true
    }

    fn take_retired(&self) -> Vec<Box<dyn std::any::Any + Send>> {
        crate::render::overlay_state::retired_batch(
            self.state.take_retired(),
            self.job_memo.take_retired(),
        )
    }

    fn apply_fetch_result(&mut self, result: FetchPayload, pane: &PaneRef<'_>) {
        let Some(fetch) = self.state.downcast_round::<SpcOutlookFetchResult>(result) else {
            log::error!("SPC outlook handler received unexpected fetch result type");
            return;
        };
        let key = (fetch.day, fetch.product);
        // The union: a product pane 1 still wants is in scope even when pane
        // 0 has stopped asking for it, and an arrival for it is this round's
        // answer rather than a stray.
        let scope = self.union_scope(pane);
        let in_scope = scope.contains(&key);
        match fetch.result {
            Ok(outlook) => {
                log::info!("Received SPC outlook: {:?} {:?}", fetch.day, fetch.product);
                self.state.data.insert(key, outlook);
                // This layer stamps its own map one product per payload
                // rather than replacing it, so the bytes moved without an
                // install and the heap level has to be told.
                self.state.reprice();
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
            self.file_round_verdict(&scope);
        }
    }

    fn retain_selections(&self, _selections: &mut Vec<Arc<dyn OverlayItem>>, _pane: &PaneRef<'_>) {
        // Nothing to prune: outlook items match on day, product and label,
        // not on a data ID.
    }

    /// **Built once per picture, not once per dispatch.** The input deep-clones
    /// every feature of every in-force product — polygons and both labels —
    /// and its terms are the combined generation and [`Self::paint_key`].
    fn prepare_job(&self, ctx: &RasterizeContext, pane: &PaneRef<'_>) -> Option<DescribedJob> {
        let view = self.view(pane);
        self.job_memo.get_or_build(
            self.combined_generation(),
            self.paint_key(view, ctx),
            || self.paint_input(ctx, view).map(DescribedJob::new),
        )
    }

    fn job_codec(&self) -> Option<&'static JobCodec> {
        crate::render::jobs::JOB_CODECS
            .iter()
            .find(|row| row.label == "overlay/outlooks")
    }

    fn create_fetch_tasks(&self, ctx: &FetchConfig, pane: &PaneRef<'_>) -> Vec<FetchTask> {
        let view = self.view(pane);
        if view.enabled_products.is_empty() {
            return Vec::new();
        }
        let day = view.selected_day;
        let products: Vec<OutlookProduct> = day
            .products()
            .iter()
            .copied()
            .filter(|p| view.enabled_products.contains(p))
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

    fn controls(&self, pane: &PaneRef<'_>) -> Vec<ControlItem> {
        let view = self.view(pane);
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
                    highlight: d == view.selected_day,
                }
            })
            .collect();
        items.push(ControlItem::ButtonRow { buttons });

        for &product in view
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
                enabled: view.enabled_products.contains(&product),
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

    fn apply_control(&mut self, update: &ControlUpdate, pane: &mut PaneMut<'_>) -> ControlEffect {
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
                if new_day != self.view(&pane.as_ref()).selected_day {
                    self.edit(pane, |state| {
                        state.selected_day = new_day;
                        let valid: HashSet<OutlookProduct> =
                            new_day.products().iter().copied().collect();
                        state.enabled_products.retain(|p| valid.contains(p));
                    });
                    self.config_generation = self.config_generation.wrapping_add(1);
                    self.refile_after_selection_change(pane);
                    if !self.view(&pane.as_ref()).enabled_products.is_empty() {
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
                    self.edit(pane, |state| {
                        if enabled {
                            state.enabled_products.insert(product);
                        } else {
                            state.enabled_products.remove(&product);
                        }
                    });
                    self.config_generation = self.config_generation.wrapping_add(1);
                    self.refile_after_selection_change(pane);
                    if enabled {
                        return ControlEffect::Fetch;
                    }
                }
                ControlEffect::None
            }
            "refresh" if self.view(&pane.as_ref()).enabled_products.is_empty() => {
                ControlEffect::None
            }
            "refresh" => ControlEffect::Fetch,
            _ => ControlEffect::None,
        }
    }

    // ── Per-pane state (WO-M10c) ──────────────────────────

    fn create_pane_state(&self, enabled: bool) -> Option<FetchPayload> {
        Some(Box::new(OutlookPaneState::new(enabled)))
    }

    /// Field for field what `deserialize_state` does, against the pane's own
    /// state — **except that the pane's slot flag dominates the product set.**
    ///
    /// For this layer the flag and the set are the *same fact* stored twice:
    /// "on" **is** a non-empty product set. They disagree only when the config
    /// did not come from this pane — `initialize_pane_enabled` seeds a pane
    /// that has saved nothing with the *registry's* serialize. The flag is the
    /// half that is the pane's, so it wins. The **day** is a fact of its own
    /// and survives either way: a pane that is off still remembers which day
    /// it was looking at.
    fn deserialize_pane_state(
        &self,
        value: serde_json::Value,
        enabled: bool,
    ) -> Option<FetchPayload> {
        let mut state = OutlookPaneState::new(enabled);
        Self::restore_selection(&mut state, &value);
        if !enabled {
            state.enabled_products.clear();
        } else if state.enabled_products.is_empty()
            && let Some(&first) = state.selected_day.products().first()
        {
            state.enabled_products.insert(first);
            state.sync_implied_products();
        }
        Some(Box::new(state))
    }

    /// **Byte-identical to `serialize_state`** — same members, same order,
    /// same values. The corpus is what says so.
    fn serialize_pane_state(&self, state: &dyn Any) -> serde_json::Value {
        match state.downcast_ref::<OutlookPaneState>() {
            Some(state) => Self::save_selection(state),
            None => serde_json::Value::Null,
        }
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
            handler
                .defaults
                .enabled_products
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![OutlookProduct::Categorical],
            "day 1's first product is Categorical"
        );

        handler.set_enabled(false, &mut PaneMut::bare(0));
        assert!(!handler.is_enabled(&PaneRef::bare(0)));

        handler.defaults.selected_day = OutlookDay::Day5;
        handler.set_enabled(true, &mut PaneMut::bare(0));
        assert_eq!(
            handler
                .defaults
                .enabled_products
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![OutlookProduct::Probabilistic],
            "day 5 publishes only the probabilistic product"
        );
    }

    fn day3_probabilistic() -> SpcOutlookHandler {
        let mut h = SpcOutlookHandler::new();
        h.defaults.selected_day = OutlookDay::Day3;
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
        handler.apply_fetch_result(
            Box::new(SpcOutlookFetchResult {
                day: OutlookDay::Day3,
                product,
                result,
            }),
            &PaneRef::across(&[]),
        );
    }

    /// It must be `_cigprob` and not `_sigprob`. `_sigprob` still answers 200
    /// with a real `SIGN` polygon but has not been re-issued since 2026-03-03,
    /// so asking for it would paint a months-old hazard area as current.
    #[test]
    fn day_3_asks_for_the_conditional_intensity_endpoint_not_the_frozen_one() {
        let sources = squallar_source::origins::DataSources::default();
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
                .defaults
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
        // Without a rustls provider installed, building a `reqwest::Client`
        // panics — so this test was green only when an earlier test in the
        // same binary happened to install one. Pre-existing; found by WO-M11's
        // tamper rounds, where a filtered single-test run made it visible.
        squallar_source::tls::init();
        let ctx = FetchConfig {
            client: Default::default(),
            zone_cache_dir: None,
            sources: squallar_source::origins::DataSources::default(),
            viewport: None,
            as_of: chrono::Utc::now().naive_utc(),
            depicted_span_secs: None,
            depicted_frames: Vec::new(),
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
                .defaults
                .enabled_products
                .contains(&OutlookProduct::ConditionalIntensity),
            "toggle path"
        );

        let mut from_day5 = SpcOutlookHandler::new();
        from_day5.defaults.selected_day = OutlookDay::Day5;
        from_day5.set_enabled(true, &mut PaneMut::bare(0));
        assert_eq!(
            from_day5
                .defaults
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
                .defaults
                .enabled_products
                .contains(&OutlookProduct::ConditionalIntensity),
            "day-button path"
        );

        // The reopen path is `deserialize_pane_state` since WO-M10c: the
        // selection is the PANE's, and the global `serialize_state` no longer
        // carries it.
        let reopened = SpcOutlookHandler::new()
            .deserialize_pane_state(
                serde_json::json!({
                    "selected_day": "Day3",
                    "enabled_products": ["Probabilistic"],
                }),
                true,
            )
            .expect("the outlook keeps per-pane state");
        assert!(
            reopened
                .downcast_ref::<OutlookPaneState>()
                .expect("the outlook layer's own state type")
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
                .defaults
                .enabled_products
                .contains(&OutlookProduct::ConditionalIntensity),
            "unticking Probabilistic drops the significant area with it"
        );

        let mut handler = day3_probabilistic();
        toggle(&mut handler, "day1", true);
        assert!(
            !handler
                .defaults
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
            .features_in_paint_order(&handler.defaults, chrono::Utc::now().naive_utc())
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
        let prefs = squallar_units::UserPreferences {
            timezone: squallar_units::TimezonePreference::Utc,
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
        let prefs = squallar_units::UserPreferences::default();
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

        handler
            .defaults
            .enabled_products
            .insert(OutlookProduct::Tornado);
        handler
            .defaults
            .enabled_products
            .insert(OutlookProduct::Categorical);
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
        handler.apply_fetch_result(
            Box::new(SpcOutlookFetchResult {
                day: OutlookDay::Day1,
                product,
                result,
            }),
            &PaneRef::across(&[]),
        );
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
            h.defaults.enabled_products.insert(p);
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
        alone.defaults.enabled_products.insert(Tornado);
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
        h.defaults.enabled_products.insert(Categorical);
        h.defaults.enabled_products.insert(Tornado);
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
        h.defaults.enabled_products.insert(Categorical);
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
        h.defaults.enabled_products.insert(Tornado);

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
        h.defaults.enabled_products.insert(Categorical);
        h.defaults.enabled_products.insert(Tornado);
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
        squallar_source::tls::init();
        let ctx = FetchConfig {
            client: reqwest::Client::builder()
                .build()
                .expect("a client with no options set"),
            zone_cache_dir: None,
            sources: squallar_source::origins::DataSources::production(),
            viewport: None,
            as_of: chrono::Utc::now().naive_utc(),
            depicted_span_secs: None,
            depicted_frames: Vec::new(),
        };
        for products in 1..=OutlookDay::Day1.products().len() {
            let mut h = SpcOutlookHandler::new();
            for &p in &OutlookDay::Day1.products()[..products] {
                h.defaults.enabled_products.insert(p);
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

    // ── Per-pane state (WO-M10c) ──────────────────────────────────────

    fn outlook_pane(day: OutlookDay, products: &[OutlookProduct]) -> FetchPayload {
        let mut state = OutlookPaneState {
            selected_day: day,
            enabled_products: products.iter().copied().collect(),
        };
        state.sync_implied_products();
        Box::new(state)
    }

    fn pane_ref<'a>(p: &'a FetchPayload, idx: usize) -> PaneRef<'a> {
        PaneRef {
            state: Some(&**p),
            ..PaneRef::bare(idx)
        }
    }

    /// **Two panes on two outlook days**, which the config swap could only
    /// fake. Equal first, then diverged through the handler's own control
    /// route; `defaults` is asserted untouched, which fires the moment one of
    /// these methods writes a per-pane value to `&mut self`.
    #[test]
    fn two_panes_hold_different_outlook_days_and_the_registry_keeps_neither() {
        let mut h = SpcOutlookHandler::new();
        let a = outlook_pane(OutlookDay::Day1, &[OutlookProduct::Categorical]);
        let mut b = outlook_pane(OutlookDay::Day1, &[OutlookProduct::Categorical]);
        assert_eq!(
            h.status_line(&pane_ref(&a, 0)),
            h.status_line(&pane_ref(&b, 1)),
            "premise: two panes on the same day and set answer the same",
        );

        h.apply_control(
            &ControlUpdate {
                id: "day3",
                value: ControlValue::Bool(true),
            },
            &mut PaneMut {
                pane_idx: 1,
                state: Some(&mut *b),
                peers: &[&*a],
            },
        );

        let pane_a = pane_ref(&a, 0);
        let pane_b = pane_ref(&b, 1);
        assert!(
            h.status_line(&pane_a)
                .is_some_and(|line| line.starts_with(&OutlookDay::Day1.to_string())),
            "pane 0's day: {:?}",
            h.status_line(&pane_a),
        );
        assert!(
            h.status_line(&pane_b)
                .is_some_and(|line| line.starts_with(&OutlookDay::Day3.to_string())),
            "pane 1's day: {:?}",
            h.status_line(&pane_b),
        );
        // The cache token is what the render dispatch groups panes by: an
        // equal token here is one pane drawing the other pane's outlook.
        assert_ne!(
            h.content_signature(&pane_a),
            h.content_signature(&pane_b),
            "two panes on two outlook days shared one cache token",
        );
        assert_eq!(
            h.serialize_pane_state(&*a)["selected_day"],
            serde_json::to_value(OutlookDay::Day1).unwrap(),
            "pane 0's saved bytes",
        );
        assert_eq!(
            h.serialize_pane_state(&*b)["selected_day"],
            serde_json::to_value(OutlookDay::Day3).unwrap(),
            "pane 1's saved bytes",
        );
        assert_eq!(
            h.defaults.selected_day,
            OutlookDay::Day1,
            "the registry's own copy took one of the panes' edits",
        );
        assert!(
            h.defaults.enabled_products.is_empty(),
            "the registry's own copy took one of the panes' product sets",
        );
    }

    /// **One pane's edit must not clear a failure another pane's selection is
    /// still carrying.** The round ledger is one ledger for the whole
    /// application, so the scope it is refiled against is the UNION of the
    /// panes — narrowing it to the pane that was edited is the "dropping what
    /// one pane still selects to satisfy another" failure mode.
    ///
    /// Non-triviality floor: the failing product is enabled in **pane 1 only**
    /// and in neither the edited pane nor the registry's own copy, so a
    /// pane-0-scoped refile drops it for certain.
    #[test]
    fn an_edit_in_one_pane_keeps_the_failure_another_panes_selection_carries() {
        let mut h = SpcOutlookHandler::new();
        let mut a = outlook_pane(OutlookDay::Day1, &[OutlookProduct::Categorical]);
        let b = outlook_pane(OutlookDay::Day1, &[OutlookProduct::Tornado]);

        let failing = (OutlookDay::Day1, OutlookProduct::Tornado);
        let error = crate::fetch_policy::FetchError::transient("HTTP 500");
        h.per_product_error.insert(failing, error.clone());
        h.state.retry.record_failure(&error);
        assert!(
            h.state.retry.failures() > 0,
            "premise: the layer is on the ledger",
        );

        // Pane 0 turns a product ON. Its own selection is clean; pane 1's is not.
        h.apply_control(
            &ControlUpdate {
                id: "hail",
                value: ControlValue::Bool(true),
            },
            &mut PaneMut {
                pane_idx: 0,
                state: Some(&mut *a),
                peers: &[&*b],
            },
        );

        assert!(
            h.per_product_error.contains_key(&failing),
            "pane 1's failing product was dropped from the ledger by pane 0's edit",
        );
        assert!(
            h.state.retry.failures() > 0,
            "the layer came off the retry ledger while pane 1's selection was \
             still failing",
        );
    }

    // ── The as-of window (WB-5) ───────────────────────────────────────────

    fn at(d: u32, h: u32) -> chrono::NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(2026, 8, d)
            .unwrap()
            .and_hms_opt(h, 0, 0)
            .unwrap()
    }

    fn paint_ctx(as_of: chrono::NaiveDateTime) -> RasterizeContext {
        RasterizeContext {
            is_dark: false,
            zoom: 6.0,
            device_scale: 1.0,
            now: as_of,
            as_of,
            frame: None,
        }
    }

    /// A Day-1 categorical handler holding one issuance with the given window.
    fn day1_with_window(
        valid: Option<chrono::NaiveDateTime>,
        expire: Option<chrono::NaiveDateTime>,
    ) -> SpcOutlookHandler {
        let mut handler = SpcOutlookHandler::new();
        handler.defaults = OutlookPaneState::new(true);
        let feature = crate::types::OverlayFeature {
            polygons: vec![vec![vec![(35.0, -97.0), (35.5, -97.0), (35.5, -96.5)]]],
            fill_rgba: [255, 0, 0, 80],
            stroke_rgba: [255, 0, 0, 255],
            label: "SLGT".into(),
            label2: String::new(),
            hatch: crate::types::HatchPattern::None,
            geo_bounds: None,
        };
        handler.state.data.insert(
            (OutlookDay::Day1, OutlookProduct::Categorical),
            SpcOutlook {
                day: OutlookDay::Day1,
                product: OutlookProduct::Categorical,
                valid,
                expire,
                features: vec![feature],
            },
        );
        handler
    }

    fn labels_at(handler: &SpcOutlookHandler, as_of: chrono::NaiveDateTime) -> Vec<String> {
        handler
            .paint_input(&paint_ctx(as_of), &handler.defaults)
            .map(|input| input.features.into_iter().map(|f| f.label).collect())
            .unwrap_or_default()
    }

    /// **The WB-5 floor, both halves.** An issuance valid 13Z->12Z draws at
    /// no instant before its `valid`, at every instant inside the window, and
    /// at no instant from `expire` on. The last reading — a LATER instant —
    /// is the half a `valid`-only filter passes: dropping the `expire`
    /// comparison keeps 18Z green and turns both day-2 readings red.
    #[test]
    fn an_outlook_draws_only_while_its_issuance_is_in_force() {
        let handler = day1_with_window(Some(at(22, 13)), Some(at(23, 12)));

        assert_eq!(
            labels_at(&handler, at(22, 11)),
            Vec::<String>::new(),
            "before `valid` the issuance is not yet in force",
        );
        assert_eq!(
            labels_at(&handler, at(22, 18)),
            vec!["SLGT".to_owned()],
            "inside the window it draws",
        );
        assert_eq!(
            labels_at(&handler, at(23, 12)),
            Vec::<String>::new(),
            "`expire` is exclusive: the expiry instant is the first without it",
        );
        assert_eq!(
            labels_at(&handler, at(23, 18)),
            Vec::<String>::new(),
            "an instant PAST the window must not wear a lapsed outlook - the \
             reading a valid-only filter gets wrong",
        );
    }

    /// **The cross-cutting non-triviality: a LIVE pane is byte-identical.**
    /// At the live instant, an issuance whose window contains now paints the
    /// same input as one with no parsed window at all — which is the
    /// pre-WB-5 picture, since the fields were previously popup-only.
    #[test]
    fn a_live_pane_paints_the_unwindowed_picture() {
        let now = chrono::Utc::now().naive_utc();
        let windowed = day1_with_window(
            Some(now - chrono::Duration::hours(6)),
            Some(now + chrono::Duration::hours(6)),
        );
        let unwindowed = day1_with_window(None, None);
        let live = windowed.paint_input(&paint_ctx(now), &windowed.defaults);
        assert!(
            live.as_ref()
                .is_some_and(|input| !input.features.is_empty()),
            "non-triviality floor: the live picture has features in it",
        );
        assert_eq!(
            live,
            unwindowed.paint_input(&paint_ctx(now), &unwindowed.defaults),
            "the as-of window leaked into the live pane's paint input",
        );
    }

    /// **The dispatch's build, memoised.** The input deep-clones every
    /// in-force feature; a second dispatch under the same day, products,
    /// in-force set, theme and scale hands back the first's allocation. The
    /// theme is a term here — the hatch colour reads it — and so is the
    /// window: an instant past `expire` describes no job, and an instant
    /// back inside it is a hit on the held row, not a rebuild.
    #[test]
    fn the_built_input_is_reused_until_a_term_of_the_picture_moves() {
        let mut handler = day1_with_window(Some(at(22, 13)), Some(at(23, 12)));
        let pane = PaneRef::bare(0);
        let first = handler.prepare_job(&paint_ctx(at(22, 14)), &pane).unwrap();
        let second = handler.prepare_job(&paint_ctx(at(22, 20)), &pane).unwrap();
        assert!(
            Arc::ptr_eq(&first.0, &second.0),
            "same picture, same allocation"
        );
        assert_eq!(handler.job_memo.builds.get(), 1);

        // A `None` answer runs the closure and counts as a build; it is not
        // remembered, so the count below steps on every such ask.
        assert!(
            handler.prepare_job(&paint_ctx(at(23, 12)), &pane).is_none(),
            "past expire nothing is in force and no job is described",
        );
        assert_eq!(handler.job_memo.builds.get(), 2);
        let back = handler.prepare_job(&paint_ctx(at(22, 21)), &pane).unwrap();
        assert!(
            Arc::ptr_eq(&first.0, &back.0),
            "the in-force row was still held"
        );

        let dark = RasterizeContext {
            is_dark: true,
            ..paint_ctx(at(22, 21))
        };
        handler.prepare_job(&dark, &pane);
        assert_eq!(
            handler.job_memo.builds.get(),
            3,
            "the theme reaches the hatch colour"
        );

        handler
            .defaults
            .enabled_products
            .remove(&OutlookProduct::Categorical);
        assert!(
            handler.prepare_job(&dark, &pane).is_none(),
            "no enabled product draws nothing",
        );
        assert_eq!(handler.job_memo.builds.get(), 4);
        handler
            .defaults
            .enabled_products
            .insert(OutlookProduct::Categorical);
        handler.prepare_job(&dark, &pane);
        assert_eq!(
            handler.job_memo.builds.get(),
            4,
            "the dark row was still held"
        );

        land(
            &mut handler,
            OutlookProduct::Categorical,
            Ok(SpcOutlook {
                day: OutlookDay::Day1,
                product: OutlookProduct::Categorical,
                valid: None,
                expire: None,
                features: Vec::new(),
            }),
        );
        assert!(
            handler.prepare_job(&dark, &pane).is_none(),
            "a refetch that landed an empty issuance draws nothing",
        );
        assert!(
            !handler.job_memo.take_retired().is_empty(),
            "the refetch moved the generation and parked the old rows",
        );
    }
}
