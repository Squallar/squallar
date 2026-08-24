//! SPC Fire Weather Outlooks — the layer.
//!
//! Modelled on [`outlook`](super::outlook), and deliberately **not** sharing
//! its state type: the convective layer's product set carries an implied
//! member (the significant-severe area), and fire weather has no such thing.
//! See [`crate::spc::firewx`] for the tier finding that settles that.
//!
//! **The raster is the convective layer's.** A fire outlook is a list of
//! [`OverlayFeature`](crate::types::OverlayFeature)s in SPC's own colours,
//! which is exactly what [`rasterize::OutlooksInput`] carries, so this layer
//! describes that input and rides the `overlay/outlooks` codec row rather than
//! adding a second wire form for byte-identical content. The pairing is
//! written down in
//! `texture_tests::every_texture_handler_owns_exactly_one_codec_row`.

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
use crate::render::overlay_state::{PaneMut, PaneRef};
use crate::render::rasterize;
use crate::spc::firewx::{
    FireDay, FireHazard, FireProduct, SpcFireOutlook, firewx_page_url, products_for,
};
use squallar_source::id::{LayerId, known};
use squallar_source::job::{DescribedJob, JobCodec};
use squallar_source::time::TimeAxis;

/// One published file: a day, a hazard, and which of that hazard's two forms.
/// Days 1-2 publish only the categorical form, so the third member is constant
/// there — see [`crate::spc::firewx::FireProduct`].
type Key = (FireDay, FireHazard, FireProduct);

pub struct SpcFireFetchResult {
    pub day: FireDay,
    pub hazard: FireHazard,
    pub product: FireProduct,
    pub result: Result<SpcFireOutlook, crate::fetch_policy::FetchError>,
}
impl crate::fetch_policy::FetchRound for SpcFireFetchResult {
    type Shape = Assembled;
}

#[derive(Debug)]
pub(crate) struct FireOutlookItem {
    pub label: String,
    pub label2: String,
    pub day: FireDay,
    pub hazard: FireHazard,
    pub product: FireProduct,
    pub valid: Option<chrono::NaiveDateTime>,
    pub expire: Option<chrono::NaiveDateTime>,
}

impl OverlayItem for FireOutlookItem {
    fn layer_id(&self) -> LayerId {
        known::SPC_FIRE_OUTLOOK
    }

    fn popup_content(&self, prefs: &squallar_units::UserPreferences) -> PopupContent {
        let time = |t: Option<chrono::NaiveDateTime>| match t {
            Some(t) => prefs.timezone.format_naive_utc(t, "%b %d %Y %H:%M"),
            None => "Unknown".to_owned(),
        };
        // The long label is the readable one ("Elevated Fire Risk"); the short
        // one ("ELEV") is what the polygon is keyed by. Show the long one when
        // SPC published it and fall back to the short one when it did not.
        let heading = if self.label2.is_empty() {
            self.label.clone()
        } else {
            self.label2.clone()
        };
        PopupContent {
            title: format!("SPC Day {} {} Fire Outlook", self.day.label(), self.hazard),
            accent_rgb: [220, 140, 60],
            width: 300.0,
            sections: vec![
                PopupSection::Heading(heading),
                PopupSection::KeyValueGrid(vec![
                    ("Day".into(), self.day.to_string()),
                    ("Hazard".into(), self.hazard.to_string()),
                    ("Product".into(), self.product.to_string()),
                    ("Valid".into(), time(self.valid)),
                    ("Expires".into(), time(self.expire)),
                ]),
                PopupSection::Separator,
                PopupSection::Link {
                    label: "Open on SPC website".into(),
                    url: firewx_page_url(self.day),
                },
            ],
            actions: Vec::new(),
        }
    }

    fn matches(&self, other: &dyn OverlayItem) -> bool {
        other
            .as_any()
            .downcast_ref::<FireOutlookItem>()
            .is_some_and(|o| {
                o.label == self.label
                    && o.day == self.day
                    && o.hazard == self.hazard
                    && o.product == self.product
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

/// **The whole per-pane state of the fire weather layer**: which day this pane
/// is looking at and which hazards it lets through.
///
/// There is no `enabled` beside the set — for this layer "on" **is** a
/// non-empty hazard set, and a bool next to it is a second copy free to
/// disagree with the thing it was derived from.
///
/// **The hazard is the selection; the product is not.** A day-3-to-8 hazard
/// publishes a categorical and a probabilistic file and both are part of that
/// hazard's picture, so enabling a hazard asks for both. Days 1-2 publish one
/// file per hazard, so there the two collapse to one request. That is a
/// property of `products_for`, not of an arm here.
///
/// The round bookkeeping is deliberately NOT here: one fetch round is one
/// request per published file for the whole application, and every pane's
/// selection contributes to it. It stays on the handler and is scoped by the
/// **union** of the panes — see [`SpcFireOutlookHandler::union_scope`].
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FirePaneState {
    pub selected_day: FireDay,
    pub enabled_hazards: HashSet<FireHazard>,
}

impl FirePaneState {
    /// A pane that has saved nothing. `enabled` is the pane's own slot flag,
    /// and for this layer "on" means the day's first hazard — the same answer
    /// `set_enabled(true)` gives.
    fn new(enabled: bool) -> Self {
        let mut state = Self {
            selected_day: FireDay::Day1,
            enabled_hazards: HashSet::new(),
        };
        if enabled && let Some(first) = first_hazard(state.selected_day) {
            state.enabled_hazards.insert(first);
        }
        state
    }

    /// The keys **this pane** is asking for, appended in publication order and
    /// never duplicated — so a one-pane union is byte-for-byte the walk this
    /// layer would make on its own.
    fn extend_scope(&self, into: &mut Vec<Key>) {
        for &(hazard, product) in products_for(self.selected_day) {
            if !self.enabled_hazards.contains(&hazard) {
                continue;
            }
            let key = (self.selected_day, hazard, product);
            if !into.contains(&key) {
                into.push(key);
            }
        }
    }

    /// How many requests this pane's selection is worth on `day`.
    fn asked(&self) -> usize {
        let mut scope = Vec::new();
        self.extend_scope(&mut scope);
        scope.len()
    }
}

/// The first hazard `day` publishes, in publication order. Every day publishes
/// both, so this is `Some` for every day — read off the table rather than
/// written as a constant, so a day that stopped publishing one does not hand
/// this layer a hazard it cannot fetch.
fn first_hazard(day: FireDay) -> Option<FireHazard> {
    products_for(day).first().map(|&(hazard, _)| hazard)
}

pub(crate) struct SpcFireOutlookHandler {
    pub state: OverlayState<HashMap<Key, SpcFireOutlook>, Assembled>,
    /// Per file, so one file's refetch does not invalidate the others.
    per_product_generation: HashMap<Key, u64>,
    /// Bumped when day or hazard set changes without any fetch, which still
    /// changes what gets drawn.
    config_generation: u64,
    /// The last answer per file that was **not** a success, including
    /// [`Absent`](crate::fetch_policy::FetchFailure::Absent) — see
    /// [`Self::round_verdict`], which is what splits the two apart.
    per_product_error: HashMap<Key, crate::fetch_policy::FetchError>,
    /// How many of this layer's fetch tasks are still in flight.
    outstanding: usize,
    /// Whether anything the layer is **currently** asking for has answered
    /// since the last round verdict. See [`Self::file_round_verdict`].
    round_answered_in_scope: bool,
    /// Failures from the current round for files the layer has stopped asking
    /// for mid-flight. See [`Self::file_round_verdict`].
    round_stray_failures: Vec<crate::fetch_policy::FetchError>,
    /// **The registry's own copy**, used only where no pane is supplied. Every
    /// answer prefers [`PaneRef::state`] when there is one.
    pub defaults: FirePaneState,
}

impl SpcFireOutlookHandler {
    pub fn new() -> Self {
        Self {
            state: OverlayState::new(),
            per_product_generation: HashMap::new(),
            config_generation: 0,
            per_product_error: HashMap::new(),
            outstanding: 0,
            round_answered_in_scope: false,
            round_stray_failures: Vec::new(),
            defaults: FirePaneState::new(false),
        }
    }

    /// **This pane's answer, or the registry's own copy** when no pane was
    /// supplied.
    fn view<'a>(&'a self, pane: &PaneRef<'a>) -> &'a FirePaneState {
        pane.state_as::<FirePaneState>().unwrap_or(&self.defaults)
    }

    /// Edit this pane's state, falling back to the registry's copy for a
    /// caller that supplied no pane.
    fn edit(&mut self, pane: &mut PaneMut<'_>, f: impl FnOnce(&mut FirePaneState)) {
        match pane.state_as::<FirePaneState>() {
            Some(state) => f(state),
            None => f(&mut self.defaults),
        }
    }

    /// **Everything ANY pane is asking for**, in publication order, deduped.
    ///
    /// The round is one request per file for the whole application, so the
    /// scope it is judged against is the union: a file pane 1 still wants is
    /// in scope even when pane 0 has stopped asking, and a failure any pane's
    /// selection carries keeps the layer on the ledger. Narrowing this to one
    /// pane is how an edit in one pane takes the layer off a ledger another
    /// pane is still on.
    fn union_scope(&self, pane: &PaneRef<'_>) -> Vec<Key> {
        let mut scope = Vec::new();
        let mut answered = false;
        for state in pane.all_as::<FirePaneState>() {
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
    /// per-instance noise, and writing it raw makes save->load->save produce a
    /// different file every reopen.
    fn save_selection(state: &FirePaneState) -> serde_json::Value {
        let enabled: Vec<FireHazard> = FireHazard::all()
            .iter()
            .copied()
            .filter(|h| state.enabled_hazards.contains(h))
            .collect();
        serde_json::json!({
            "selected_day": state.selected_day,
            "enabled_hazards": enabled,
        })
    }

    /// **The one decoder**, the exact inverse of [`Self::save_selection`]. A
    /// member the value does not name is left as it was.
    fn restore_selection(state: &mut FirePaneState, value: &serde_json::Value) {
        if let Some(day) = value
            .get("selected_day")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
        {
            state.selected_day = day;
        }
        if let Some(hazards) = value
            .get("enabled_hazards")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
        {
            state.enabled_hazards = hazards;
        }
    }

    /// Move the outstanding-task count, keeping `state.fetching` in step.
    fn set_outstanding(&mut self, outstanding: usize) {
        self.outstanding = outstanding;
        self.state.fetching = outstanding > 0;
    }

    /// What every file's last answer adds up to, as a **property of the
    /// selection** rather than of the order its tasks resolved in.
    fn round_verdict(&self, scope: &[Key]) -> RoundVerdict {
        let asked = scope.len();

        let mut failed: Vec<(Key, &crate::fetch_policy::FetchError)> = Vec::new();
        let mut absent: Vec<(Key, &crate::fetch_policy::FetchError)> = Vec::new();
        let mut drew = false;
        for &key in scope {
            match self.per_product_error.get(&key) {
                Some(e) if e.failure == crate::fetch_policy::FetchFailure::Absent => {
                    absent.push((key, e));
                }
                Some(e) => failed.push((key, e)),
                None if self.state.data.contains_key(&key) => drew = true,
                None => {}
            }
        }

        let listed = |parts: &[(Key, &crate::fetch_policy::FetchError)]| {
            parts
                .iter()
                .map(|((_, hazard, product), e)| format!("{hazard} {product}: {}", e.message))
                .collect::<Vec<_>>()
                .join("; ")
        };

        if !failed.is_empty() {
            return RoundVerdict::Failed(crate::fetch_policy::FetchError {
                failure: crate::fetch_policy::FetchFailure::of_round(
                    failed.iter().map(|(_, e)| e.failure),
                ),
                message: format!(
                    "{} of {asked} fire outlook products did not load: {}",
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
                "{} of {asked} fire outlook products are not published right now: {}",
                absent.len(),
                listed(&absent),
            )));
        }
        RoundVerdict::Clear
    }

    /// What of this selection is **not on the map**, as distinct from what is
    /// merely out of date.
    fn round_coverage(&self, scope: &[Key]) -> crate::fetch_policy::DataCompleteness {
        let mut expected = 0;
        let mut missing = 0;
        let mut reasons = Vec::new();
        for &key in scope {
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
            let (_, hazard, product) = key;
            reasons.push((format!("{hazard} {product}: {}", error.message), 1));
        }
        crate::fetch_policy::DataCompleteness {
            expected,
            missing,
            unit: "fire outlook products",
            reasons,
            ..crate::fetch_policy::DataCompleteness::default()
        }
    }

    /// File the round's verdict on the ledger — **once**, when the last of its
    /// tasks lands.
    fn file_round_verdict(&mut self, scope: &[Key]) {
        let answered = std::mem::take(&mut self.round_answered_in_scope);
        let strays = std::mem::take(&mut self.round_stray_failures);
        if !answered {
            if !strays.is_empty() {
                let merged = crate::fetch_policy::FetchError::of_round(
                    &strays,
                    format!(
                        "{} fire outlook request(s) failed for products the \
                         layer no longer asks for",
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

    /// Every enabled file's features, concatenated in the order they will be
    /// painted.
    /// Every selected file's features, in paint order — for the files whose
    /// issuance is **in force at `as_of`** (WB-5, [`TimeAxis::EventLifetime`]):
    /// the same two-`Option` window filter as the convective handler, against
    /// `valid`/`expire` parsed once at fetch, a missing side passing.
    fn features_in_paint_order(
        &self,
        view: &FirePaneState,
        as_of: chrono::NaiveDateTime,
    ) -> Vec<crate::types::OverlayFeature> {
        let mut scope = Vec::new();
        view.extend_scope(&mut scope);
        let mut features = Vec::new();
        for key in scope {
            if let Some(outlook) = self.state.data.get(&key) {
                if !(outlook.valid.is_none_or(|valid| valid <= as_of)
                    && outlook.expire.is_none_or(|expire| as_of < expire))
                {
                    continue;
                }
                features.extend(outlook.features.iter().cloned());
            }
        }
        features
    }

    fn paint_input(
        &self,
        ctx: &RasterizeContext,
        view: &FirePaneState,
    ) -> Option<rasterize::OutlooksInput> {
        let features = self.features_in_paint_order(view, ctx.as_of);
        if features.is_empty() {
            return None;
        }
        Some(rasterize::OutlooksInput {
            features,
            // **Inert, and deliberately not read off the theme.** Fire weather
            // is one tier and nothing in it is ever hatched
            // (`crate::spc::firewx`), so this colour reaches no pixel. Reading
            // `ctx.is_dark` here would put a term in the bytes that this
            // layer's cache token does not carry — see `theme_sensitive`.
            hatch_color: [0, 0, 0, 0],
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

/// The control id for a day button. Distinct from the convective layer's
/// `day1`..`day8` on purpose: both handlers see every [`ControlUpdate`], so a
/// colliding id would drive the other layer's day too.
fn day_control_id(day: FireDay) -> &'static str {
    match day {
        FireDay::Day1 => "fireday1",
        FireDay::Day2 => "fireday2",
        FireDay::Day3 => "fireday3",
        FireDay::Day4 => "fireday4",
        FireDay::Day5 => "fireday5",
        FireDay::Day6 => "fireday6",
        FireDay::Day7 => "fireday7",
        FireDay::Day8 => "fireday8",
    }
}

fn day_for_control_id(id: &str) -> Option<FireDay> {
    FireDay::all()
        .iter()
        .copied()
        .find(|&day| day_control_id(day) == id)
}

/// The control id for a hazard toggle. `dryt`/`windrh` are SPC's own path
/// fragments and collide with nothing the convective layer offers.
fn hazard_control_id(hazard: FireHazard) -> &'static str {
    match hazard {
        FireHazard::DryThunderstorm => "dryt",
        FireHazard::WindRh => "windrh",
    }
}

fn hazard_for_control_id(id: &str) -> Option<FireHazard> {
    FireHazard::all()
        .iter()
        .copied()
        .find(|&hazard| hazard_control_id(hazard) == id)
}

impl OverlayHandler for SpcFireOutlookHandler {
    fn id(&self) -> LayerId {
        known::SPC_FIRE_OUTLOOK
    }

    fn surface(&self) -> Surface {
        Surface::Ground
    }

    /// Above the convective outlooks (20), below radar (30). Weights are
    /// unique across the whole registry — pinned by
    /// `sources::draw_order_weights_encode_the_default_draw_order`.
    fn draw_order_weight(&self) -> u32 {
        25
    }

    fn display_name(&self) -> &str {
        "SPC Fire Weather"
    }

    fn render_mode(&self) -> RenderMode {
        RenderMode::Texture
    }

    /// A fire outlook is an issuance with a validity window, and the picture
    /// at `as_of` is which held issuances are in force then (WB-5) — filtered
    /// in [`Self::features_in_paint_order`], the same arm as the convective
    /// handler.
    ///
    /// Only the **current** issuance is ever held, and here that is the whole
    /// story: SPC publishes **no fire-weather GeoJSON archive** (probed
    /// 2026-08-22 — 404 under `products/fire_wx/{year}/...`,
    /// `products/fire_wx/archive/...` and `products/exper/fire_wx/archive/...`),
    /// so a scrubbed instant outside the held windows draws nothing and no
    /// archive fetch can exist to fill it.
    fn time_axis(&self) -> TimeAxis {
        TimeAxis::EventLifetime
    }

    /// **One instant per stop**, for the reason the convective handler gives:
    /// the picture at a stop is which held issuances are in force then, and
    /// no stretch of source time behind the stop feeds it.
    ///
    /// Only the current issuance is ever held, and SPC publishes **no
    /// fire-weather GeoJSON archive** to widen that (probed 2026-08-22, see
    /// [`Self::time_axis`]). A stop outside the held windows still asks for
    /// itself: what the layer needs to draw an instant does not shrink
    /// because nothing can supply it, and an ask no supply answers is a
    /// reviewable fact rather than a silence.
    fn residency_for(
        &self,
        _pane: &PaneRef<'_>,
        stops: &[chrono::NaiveDateTime],
    ) -> squallar_source::time::Residency {
        squallar_source::time::Residency::over(stops.iter().map(|&stop| (stop, stop)))
    }

    /// **False, and it is a fact rather than an omission.** The only
    /// theme-dependent term in `OutlooksInput` is the hatch colour, and no
    /// fire feature is ever hatched — see `paint_input`.
    fn theme_sensitive(&self) -> bool {
        false
    }

    fn is_enabled(&self, pane: &PaneRef<'_>) -> bool {
        !self.view(pane).enabled_hazards.is_empty()
    }

    fn set_enabled(&mut self, enabled: bool, pane: &mut PaneMut<'_>) {
        let mut moved = false;
        self.edit(pane, |state| {
            if enabled {
                if state.enabled_hazards.is_empty()
                    && let Some(first) = first_hazard(state.selected_day)
                {
                    state.enabled_hazards.insert(first);
                    moved = true;
                }
            } else if !state.enabled_hazards.is_empty() {
                state.enabled_hazards.clear();
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

    /// **This pane's day and hazard set are in the token.** The render
    /// dispatch groups panes by it and hands one raster to the whole group, so
    /// a token that carried only `combined_generation` — which moves for every
    /// pane at once — would give one pane the other's outlook.
    fn content_signature(&self, pane: &PaneRef<'_>) -> u64 {
        use std::hash::{DefaultHasher, Hash, Hasher};
        let view = self.view(pane);
        let mut hasher = DefaultHasher::new();
        (view.selected_day as u8).hash(&mut hasher);
        // Walked from the declaration order, never the `HashSet`'s iteration
        // order, which is per-instance noise and would make one pane's token
        // move between frames.
        for &hazard in FireHazard::all() {
            if view.enabled_hazards.contains(&hazard) {
                (hazard as u8).hash(&mut hasher);
            }
        }
        self.combined_generation() ^ hasher.finish()
    }

    /// E.g. `"Day 1 - Dry Thunderstorm, Wind/RH"`.
    fn status_line(&self, pane: &PaneRef<'_>) -> Option<String> {
        let view = self.view(pane);
        if view.enabled_hazards.is_empty() {
            return None;
        }
        let hazards: Vec<String> = FireHazard::all()
            .iter()
            .filter(|h| view.enabled_hazards.contains(h))
            .map(|h| h.to_string())
            .collect();
        Some(format!("{} - {}", view.selected_day, hazards.join(", ")))
    }

    fn data_generation(&self) -> u64 {
        self.combined_generation()
    }

    /// Data **this selection** can draw, not data this layer has ever fetched.
    fn has_data(&self, pane: &PaneRef<'_>) -> bool {
        let view = self.view(pane);
        let mut scope = Vec::new();
        view.extend_scope(&mut scope);
        scope.iter().any(|key| {
            self.state
                .data
                .get(key)
                .is_some_and(|outlook| !outlook.features.is_empty())
        })
    }

    fn is_fetching(&self) -> bool {
        self.outstanding > 0
    }

    /// The host says a round has started or been abandoned; this layer's round
    /// is one task per published file the selection names, so the count moves
    /// by that many.
    fn set_fetching(&mut self, fetching: bool, pane: &PaneRef<'_>) {
        if fetching {
            // **This pane's** count, not the union: the round that just
            // started is the one `create_fetch_tasks` built for this pane, and
            // that is how many answers are owed.
            let asked = self.view(pane).asked().max(1);
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
        let mut scope = Vec::new();
        view.extend_scope(&mut scope);
        let mut items = Vec::new();
        for key in scope {
            let Some(outlook) = self.state.data.get(&key) else {
                continue;
            };
            let (day, hazard, product) = key;
            for feature in &outlook.features {
                items.push(ClickableItem {
                    features: std::slice::from_ref(feature),
                    item: Arc::new(FireOutlookItem {
                        label: feature.label.clone(),
                        label2: feature.label2.clone(),
                        day,
                        hazard,
                        product,
                        valid: outlook.valid,
                        expire: outlook.expire,
                    }) as Arc<dyn OverlayItem>,
                });
            }
        }
        items
    }

    fn apply_fetch_result(&mut self, result: FetchPayload, pane: &PaneRef<'_>) {
        let Some(fetch) = self.state.downcast_round::<SpcFireFetchResult>(result) else {
            log::error!("SPC fire weather handler received unexpected fetch result type");
            return;
        };
        let key = (fetch.day, fetch.hazard, fetch.product);
        // The union: a file pane 1 still wants is in scope even when pane 0
        // has stopped asking for it, and an arrival for it is this round's
        // answer rather than a stray.
        let scope = self.union_scope(pane);
        let in_scope = scope.contains(&key);
        match fetch.result {
            Ok(outlook) => {
                log::info!("Received SPC fire outlook: {key:?}");
                self.state.data.insert(key, outlook);
                self.per_product_error.remove(&key);
                self.state.fetch_time = Some(web_time::Instant::now());
                let counter = self.per_product_generation.entry(key).or_insert(0);
                *counter = counter.wrapping_add(1);
            }
            Err(e) if e.failure == crate::fetch_policy::FetchFailure::Absent => {
                log::info!("SPC fire outlook not published ({key:?}): {e}");
                self.state.fetch_time = Some(web_time::Instant::now());
                if in_scope {
                    self.per_product_error.insert(key, e);
                }
            }
            Err(e) => {
                log::error!("SPC fire outlook fetch failed ({key:?}): {e}");
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
        // Nothing to prune: fire outlook items match on day, hazard, product
        // and label, not on a data ID.
    }

    fn prepare_job(&self, ctx: &RasterizeContext, pane: &PaneRef<'_>) -> Option<DescribedJob> {
        self.paint_input(ctx, self.view(pane))
            .map(DescribedJob::new)
    }

    /// **Shared with the convective layer, deliberately.** A fire outlook and
    /// a convective one are the same bytes on the wire — a feature list in
    /// SPC's own colours — so they ride one row rather than two. The pairing
    /// is spelled out in
    /// `texture_tests::every_texture_handler_owns_exactly_one_codec_row`.
    fn job_codec(&self) -> Option<&'static JobCodec> {
        crate::render::jobs::JOB_CODECS
            .iter()
            .find(|row| row.label == "overlay/outlooks")
    }

    fn create_fetch_tasks(&self, ctx: &FetchConfig, pane: &PaneRef<'_>) -> Vec<FetchTask> {
        let view = self.view(pane);
        if view.enabled_hazards.is_empty() {
            return Vec::new();
        }
        let mut scope = Vec::new();
        view.extend_scope(&mut scope);
        log::info!("Fetching SPC fire weather outlooks: {scope:?}");
        // NOT `ctx.client`: SPC answers OPTIONS with 403, so a `User-Agent`
        // makes every one of these fail in the browser. See `spc::fetch`.
        let client = match crate::spc::fetch::spc_client(&ctx.sources) {
            Ok(c) => c,
            Err(e) => {
                log::error!("{e}");
                return Vec::new();
            }
        };
        scope
            .into_iter()
            .map(|(day, hazard, product)| {
                let client = client.clone();
                let sources = ctx.sources.clone();
                FetchTask {
                    kind: known::SPC_FIRE_OUTLOOK,
                    future: Box::pin(async move {
                        let result = crate::spc::fetch::fetch_firewx(
                            &client, &sources, day, hazard, product,
                        )
                        .await;
                        Box::new(SpcFireFetchResult {
                            day,
                            hazard,
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
            text: "SPC Fire Weather".into(),
        }];

        let buttons: Vec<ControlButton> = FireDay::all()
            .iter()
            .map(|&d| ControlButton {
                id: day_control_id(d),
                label: d.label().to_string(),
                enabled: true,
                highlight: d == view.selected_day,
            })
            .collect();
        items.push(ControlItem::ButtonRow { buttons });

        for &hazard in FireHazard::all() {
            items.push(ControlItem::Toggle {
                id: hazard_control_id(hazard),
                label: hazard.to_string(),
                enabled: view.enabled_hazards.contains(&hazard),
            });
        }

        items.push(ControlItem::ButtonRow {
            buttons: vec![ControlButton {
                id: "firerefresh",
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
        if let Some(new_day) = day_for_control_id(update.id) {
            if new_day != self.view(&pane.as_ref()).selected_day {
                self.edit(pane, |state| state.selected_day = new_day);
                self.config_generation = self.config_generation.wrapping_add(1);
                self.refile_after_selection_change(pane);
                if !self.view(&pane.as_ref()).enabled_hazards.is_empty() {
                    return ControlEffect::Fetch;
                }
            }
            return ControlEffect::None;
        }
        if let Some(hazard) = hazard_for_control_id(update.id) {
            if let ControlValue::Bool(enabled) = update.value {
                self.edit(pane, |state| {
                    if enabled {
                        state.enabled_hazards.insert(hazard);
                    } else {
                        state.enabled_hazards.remove(&hazard);
                    }
                });
                self.config_generation = self.config_generation.wrapping_add(1);
                self.refile_after_selection_change(pane);
                if enabled {
                    return ControlEffect::Fetch;
                }
            }
            return ControlEffect::None;
        }
        match update.id {
            "firerefresh" if self.view(&pane.as_ref()).enabled_hazards.is_empty() => {
                ControlEffect::None
            }
            "firerefresh" => ControlEffect::Fetch,
            _ => ControlEffect::None,
        }
    }

    // ── Per-pane state ────────────────────────────────────────────────

    fn create_pane_state(&self, enabled: bool) -> Option<FetchPayload> {
        Some(Box::new(FirePaneState::new(enabled)))
    }

    /// **The pane's slot flag dominates the hazard set.** For this layer the
    /// flag and the set are the *same fact* stored twice: "on" **is** a
    /// non-empty hazard set. They disagree only when the config did not come
    /// from this pane, and the flag is the half that is the pane's, so it
    /// wins. The **day** is a fact of its own and survives either way: a pane
    /// that is off still remembers which day it was looking at.
    fn deserialize_pane_state(
        &self,
        value: serde_json::Value,
        enabled: bool,
    ) -> Option<FetchPayload> {
        let mut state = FirePaneState::new(enabled);
        Self::restore_selection(&mut state, &value);
        if !enabled {
            state.enabled_hazards.clear();
        } else if state.enabled_hazards.is_empty()
            && let Some(first) = first_hazard(state.selected_day)
        {
            state.enabled_hazards.insert(first);
        }
        Some(Box::new(state))
    }

    fn serialize_pane_state(&self, state: &dyn Any) -> serde_json::Value {
        match state.downcast_ref::<FirePaneState>() {
            Some(state) => Self::save_selection(state),
            None => serde_json::Value::Null,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use FireHazard::{DryThunderstorm, WindRh};

    fn transient() -> crate::fetch_policy::FetchError {
        crate::fetch_policy::FetchError::transient("HTTP 500")
    }

    fn outlook(day: FireDay, hazard: FireHazard, product: FireProduct) -> SpcFireOutlook {
        SpcFireOutlook {
            day,
            hazard,
            product,
            valid: None,
            expire: None,
            features: Vec::new(),
        }
    }

    fn land(
        handler: &mut SpcFireOutlookHandler,
        key: Key,
        result: Result<SpcFireOutlook, crate::fetch_policy::FetchError>,
    ) {
        let (day, hazard, product) = key;
        handler.apply_fetch_result(
            Box::new(SpcFireFetchResult {
                day,
                hazard,
                product,
                result,
            }),
            &PaneRef::across(&[]),
        );
    }

    fn round(
        handler: &mut SpcFireOutlookHandler,
        results: Vec<(Key, Result<SpcFireOutlook, crate::fetch_policy::FetchError>)>,
    ) {
        handler.set_fetching(true, &PaneRef::bare(0));
        for (key, result) in results {
            land(handler, key, result);
        }
    }

    fn toggle(handler: &mut SpcFireOutlookHandler, id: &'static str, on: bool) -> ControlEffect {
        let mut ctx = PaneMut::bare(0);
        handler.apply_control(
            &ControlUpdate {
                id,
                value: ControlValue::Bool(on),
            },
            &mut ctx,
        )
    }

    /// A handler on `day` with every hazard on. Days 1-2 give two keys, days
    /// 3-8 give four.
    fn both_hazards(day: FireDay) -> SpcFireOutlookHandler {
        let mut h = SpcFireOutlookHandler::new();
        h.defaults.selected_day = day;
        h.defaults.enabled_hazards = FireHazard::all().iter().copied().collect();
        h
    }

    fn keys(day: FireDay) -> Vec<Key> {
        products_for(day)
            .iter()
            .map(|&(hazard, product)| (day, hazard, product))
            .collect()
    }

    #[test]
    fn the_master_toggle_puts_a_hazard_on_and_takes_it_off_again() {
        let mut handler = SpcFireOutlookHandler::new();
        assert!(
            !handler.is_enabled(&PaneRef::bare(0)),
            "precondition: fire weather defaults off",
        );

        handler.set_enabled(true, &mut PaneMut::bare(0));
        assert_eq!(
            handler
                .defaults
                .enabled_hazards
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![DryThunderstorm],
            "day 1's first hazard is the dry thunderstorm outlook",
        );

        handler.set_enabled(false, &mut PaneMut::bare(0));
        assert!(!handler.is_enabled(&PaneRef::bare(0)));
    }

    /// **The hazard is the selection, the product is not.** One hazard on a
    /// day-3-to-8 pane is two requests; the same hazard on a day-1-to-2 pane
    /// is one.
    #[test]
    fn one_hazard_asks_for_both_forms_only_where_both_are_published() {
        let mut near = SpcFireOutlookHandler::new();
        near.defaults.selected_day = FireDay::Day1;
        near.defaults.enabled_hazards.insert(WindRh);
        assert_eq!(near.defaults.asked(), 1);

        let mut extended = SpcFireOutlookHandler::new();
        extended.defaults.selected_day = FireDay::Day4;
        extended.defaults.enabled_hazards.insert(WindRh);
        assert_eq!(
            extended.defaults.asked(),
            2,
            "day 4's Wind/RH hazard publishes a categorical and a \
             probabilistic file, and both are part of that hazard's picture",
        );
    }

    #[test]
    fn the_outstanding_count_is_the_number_of_tasks_actually_built() {
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
        for day in [FireDay::Day1, FireDay::Day5] {
            for hazards in 1..=FireHazard::all().len() {
                let mut h = SpcFireOutlookHandler::new();
                h.defaults.selected_day = day;
                for &hazard in &FireHazard::all()[..hazards] {
                    h.defaults.enabled_hazards.insert(hazard);
                }
                let built = h.create_fetch_tasks(&ctx, &PaneRef::bare(0)).len();
                assert!(
                    built > 0,
                    "premise: {day} with {hazards} hazard(s) asks for something"
                );
                h.set_fetching(true, &PaneRef::bare(0));
                assert_eq!(
                    h.outstanding, built,
                    "the round waits for {} answers and asked {built} questions",
                    h.outstanding,
                );
            }
        }
    }

    #[test]
    fn a_partly_failed_round_reads_the_same_whichever_task_lands_last() {
        let day = FireDay::Day5;
        let all = keys(day);
        let bad = all[0];

        let mut failure_first = both_hazards(day);
        let mut ordered: Vec<(Key, Result<SpcFireOutlook, crate::fetch_policy::FetchError>)> =
            vec![(bad, Err(transient()))];
        ordered.extend(all[1..].iter().map(|&k| (k, Ok(outlook(k.0, k.1, k.2)))));
        round(&mut failure_first, ordered);

        let mut failure_last = both_hazards(day);
        let mut ordered: Vec<(Key, Result<SpcFireOutlook, crate::fetch_policy::FetchError>)> = all
            [1..]
            .iter()
            .map(|&k| (k, Ok(outlook(k.0, k.1, k.2))))
            .collect();
        ordered.push((bad, Err(transient())));
        round(&mut failure_last, ordered);

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
            note.contains("Dry Thunderstorm"),
            "the note must name the product that did not load: {note}",
        );
        assert!(
            note.contains("1 of 4"),
            "the note must say how much of the round is missing: {note}",
        );
        assert_eq!(failure_first.state.data.len(), 3);
        assert_eq!(failure_last.state.data.len(), 3);
    }

    /// Ported from `outlook::tests::a_round_with_two_failures_is_one_attempt_whichever_order_they_land_in`.
    #[test]
    fn two_failures_are_one_attempt_whichever_order_they_land_in() {
        let day = FireDay::Day5;
        let all = keys(day);

        let mut failures_first = both_hazards(day);
        let mut ordered: Vec<(Key, Result<SpcFireOutlook, crate::fetch_policy::FetchError>)> =
            vec![(all[0], Err(transient())), (all[1], Err(transient()))];
        ordered.extend(all[2..].iter().map(|&k| (k, Ok(outlook(k.0, k.1, k.2)))));
        round(&mut failures_first, ordered);

        let mut failures_last = both_hazards(day);
        let mut ordered: Vec<(Key, Result<SpcFireOutlook, crate::fetch_policy::FetchError>)> = all
            [2..]
            .iter()
            .map(|&k| (k, Ok(outlook(k.0, k.1, k.2))))
            .collect();
        ordered.push((all[0], Err(transient())));
        ordered.push((all[1], Err(transient())));
        round(&mut failures_last, ordered);

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
        let day = FireDay::Day5;
        let refused = || crate::fetch_policy::FetchError::permanent("HTTP 400");
        let all_four = || -> Vec<(Key, Result<SpcFireOutlook, crate::fetch_policy::FetchError>)> {
            keys(day).into_iter().map(|k| (k, Err(refused()))).collect()
        };

        let mut h = both_hazards(day);
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
        let day = FireDay::Day5;
        let all = keys(day);
        let mut h = both_hazards(day);

        let mut ordered: Vec<(Key, Result<SpcFireOutlook, crate::fetch_policy::FetchError>)> = all
            [..3]
            .iter()
            .map(|&k| (k, Ok(outlook(k.0, k.1, k.2))))
            .collect();
        ordered.push((all[3], Err(transient())));
        round(&mut h, ordered);
        assert!(h.state.retry.is_unhealthy(), "premise: one product failed");

        round(
            &mut h,
            all.iter()
                .map(|&k| (k, Ok(outlook(k.0, k.1, k.2))))
                .collect(),
        );
        assert_eq!(
            h.state.retry.status_note(),
            None,
            "every product has now arrived; the layer must stop reporting a fault",
        );
    }

    /// Ported from `outlook::tests::an_absent_product_is_not_reported_as_the_layer_being_stale`.
    #[test]
    fn an_absent_product_is_not_stale() {
        let day = FireDay::Day5;
        let all = keys(day);
        let mut h = both_hazards(day);
        let mut ordered: Vec<(Key, Result<SpcFireOutlook, crate::fetch_policy::FetchError>)> = all
            [..3]
            .iter()
            .map(|&k| (k, Ok(outlook(k.0, k.1, k.2))))
            .collect();
        ordered.push((
            all[3],
            Err(crate::fetch_policy::FetchError::absent("HTTP 404")),
        ));
        round(&mut h, ordered);
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

        let mut alone = SpcFireOutlookHandler::new();
        alone.defaults.selected_day = FireDay::Day1;
        alone.defaults.enabled_hazards.insert(WindRh);
        round(
            &mut alone,
            vec![(
                (FireDay::Day1, WindRh, FireProduct::Categorical),
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
    fn unticking_the_hazard_that_failed_stops_the_layer_reading_as_stale() {
        let day = FireDay::Day1;
        let mut h = both_hazards(day);
        round(
            &mut h,
            vec![
                (
                    (day, DryThunderstorm, FireProduct::Categorical),
                    Ok(outlook(day, DryThunderstorm, FireProduct::Categorical)),
                ),
                ((day, WindRh, FireProduct::Categorical), Err(transient())),
            ],
        );
        assert!(h.state.retry.is_unhealthy(), "premise: one product failed");

        assert_eq!(
            toggle(&mut h, "windrh", false),
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
            h.state
                .data
                .contains_key(&(day, DryThunderstorm, FireProduct::Categorical)),
            "premise: the layer holds the product that is left",
        );
    }

    /// Ported from `outlook::tests::navigating_to_another_day_leaves_the_old_days_failure_behind`.
    #[test]
    fn a_day_change_drops_the_old_days_failure() {
        let mut h = SpcFireOutlookHandler::new();
        h.defaults.enabled_hazards.insert(DryThunderstorm);
        round(
            &mut h,
            vec![(
                (FireDay::Day1, DryThunderstorm, FireProduct::Categorical),
                Err(transient()),
            )],
        );
        assert!(h.state.retry.is_unhealthy(), "premise");

        let mut ctx = PaneMut::bare(0);
        let effect = h.apply_control(
            &ControlUpdate {
                id: "fireday2",
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
    fn a_failure_that_lands_after_its_hazard_was_unticked_still_reaches_the_ladder() {
        let mut h = SpcFireOutlookHandler::new();
        h.defaults.enabled_hazards.insert(WindRh);

        h.set_fetching(true, &PaneRef::bare(0));
        assert!(h.is_fetching(), "premise: the request is on the wire");
        assert_eq!(toggle(&mut h, "windrh", false), ControlEffect::None);

        land(
            &mut h,
            (FireDay::Day1, WindRh, FireProduct::Categorical),
            Err(transient()),
        );

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
             frame",
        );
    }

    /// Ported from `outlook::tests::a_stray_failure_does_not_condemn_a_round_that_otherwise_answered`.
    #[test]
    fn a_late_stray_failure_does_not_file_against_the_new_scope() {
        let day = FireDay::Day1;
        let mut h = both_hazards(day);
        h.set_fetching(true, &PaneRef::bare(0));
        assert_eq!(toggle(&mut h, "windrh", false), ControlEffect::None);

        land(
            &mut h,
            (day, DryThunderstorm, FireProduct::Categorical),
            Ok(outlook(day, DryThunderstorm, FireProduct::Categorical)),
        );
        land(
            &mut h,
            (day, WindRh, FireProduct::Categorical),
            Err(transient()),
        );

        assert!(
            !h.state.retry.is_unhealthy(),
            "every product the layer asks for arrived in this very round",
        );
    }

    #[test]
    fn a_product_that_would_not_load_is_missing_from_the_map_and_not_merely_stale() {
        let day = FireDay::Day5;
        let all = keys(day);
        let failing_round =
            || -> Vec<(Key, Result<SpcFireOutlook, crate::fetch_policy::FetchError>)> {
                let mut v: Vec<_> = all[..3]
                    .iter()
                    .map(|&k| (k, Ok(outlook(k.0, k.1, k.2))))
                    .collect();
                v.push((all[3], Err(transient())));
                v
            };

        let mut h = both_hazards(day);
        round(&mut h, failing_round());
        assert!(
            h.state.retry.is_unhealthy(),
            "premise: the round did not complete, and the ladder still hears it",
        );
        assert!(
            h.state.retry.is_incomplete(),
            "the product is on no map anywhere and the layer said only that it \
             had stopped updating",
        );
        let note = h
            .state
            .retry
            .coverage()
            .status_note()
            .expect("the options must say which product is not drawn");
        for expected in ["missing 1 of 4 fire outlook products", "Wind/RH"] {
            assert!(
                note.contains(expected),
                "the note must name what is off the map - missing {expected:?}: {note}",
            );
        }

        let mut drawn = both_hazards(day);
        round(
            &mut drawn,
            all.iter()
                .map(|&k| (k, Ok(outlook(k.0, k.1, k.2))))
                .collect(),
        );
        assert!(!drawn.state.retry.is_incomplete(), "premise: all four drew");
        round(&mut drawn, failing_round());
        assert!(
            drawn.state.retry.is_unhealthy(),
            "premise: the second round failed",
        );
        assert!(
            !drawn.state.retry.is_incomplete(),
            "the product has answered for this day and what it answered is \
             stale, which is what the health axis is for",
        );

        assert_eq!(toggle(&mut h, "windrh", false), ControlEffect::None);
        assert!(
            !h.state.retry.is_incomplete(),
            "the mark outlived the selection it was about",
        );
    }

    #[test]
    fn a_round_that_lands_wholly_out_of_scope_still_retires_its_coverage_report() {
        let day = FireDay::Day1;
        let dryt = (day, DryThunderstorm, FireProduct::Categorical);
        let windrh = (day, WindRh, FireProduct::Categorical);
        let mut h = both_hazards(day);
        round(
            &mut h,
            vec![
                (
                    dryt,
                    Ok(outlook(day, DryThunderstorm, FireProduct::Categorical)),
                ),
                (windrh, Err(transient())),
            ],
        );
        assert!(
            h.state.retry.is_incomplete(),
            "premise: the Wind/RH outlook did not load and is on no map",
        );

        h.set_fetching(true, &PaneRef::bare(0));
        assert_eq!(toggle(&mut h, "windrh", false), ControlEffect::None);
        assert_eq!(toggle(&mut h, "dryt", false), ControlEffect::None);
        land(
            &mut h,
            dryt,
            Ok(outlook(day, DryThunderstorm, FireProduct::Categorical)),
        );
        land(&mut h, windrh, Err(transient()));

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
    fn the_status_line_names_the_day_and_its_enabled_hazards() {
        let mut handler = SpcFireOutlookHandler::new();
        assert_eq!(
            handler.status_line(&PaneRef::bare(0)),
            None,
            "off means no line",
        );

        handler.defaults.enabled_hazards.insert(WindRh);
        handler.defaults.enabled_hazards.insert(DryThunderstorm);
        assert_eq!(
            handler.status_line(&PaneRef::bare(0)).as_deref(),
            Some("Day 1 - Dry Thunderstorm, Wind/RH"),
            "declaration order, not set-iteration order",
        );
    }

    #[test]
    fn the_outlooks_paint_in_publication_order_not_hash_order() {
        let day = FireDay::Day5;
        let mut handler = both_hazards(day);

        let feature = |label: &str| crate::types::OverlayFeature {
            polygons: Vec::new(),
            fill_rgba: [0, 0, 0, 0],
            stroke_rgba: [0, 0, 0, 0],
            label: label.to_string(),
            label2: String::new(),
            hatch: crate::types::HatchPattern::None,
            geo_bounds: None,
        };
        for (i, key) in keys(day).into_iter().enumerate() {
            let mut o = outlook(key.0, key.1, key.2);
            o.features.push(feature(&format!("f{i}")));
            handler.state.data.insert(key, o);
        }

        let order: Vec<String> = handler
            .features_in_paint_order(&handler.defaults, chrono::Utc::now().naive_utc())
            .into_iter()
            .map(|f| f.label)
            .collect();
        assert_eq!(
            order,
            vec!["f0", "f1", "f2", "f3"],
            "the day's own publication order, dry thunder before wind/RH and \
             categorical before probabilistic",
        );
    }

    #[test]
    fn the_popup_states_the_window_and_links_to_spc() {
        let item = FireOutlookItem {
            label: "ELEV".into(),
            label2: "Elevated Fire Risk".into(),
            day: FireDay::Day1,
            hazard: WindRh,
            product: FireProduct::Categorical,
            valid: chrono::NaiveDate::from_ymd_opt(2026, 8, 22)
                .and_then(|d| d.and_hms_opt(12, 0, 0)),
            expire: chrono::NaiveDate::from_ymd_opt(2026, 8, 23)
                .and_then(|d| d.and_hms_opt(12, 0, 0)),
        };
        // Pinned to UTC so the asserted dates cannot shift with the machine's
        // own timezone.
        let prefs = squallar_units::UserPreferences {
            timezone: squallar_units::TimezonePreference::Utc,
            ..Default::default()
        };
        let content = item.popup_content(&prefs);
        assert_eq!(content.title, "SPC Day 1 Wind/RH Fire Outlook");
        assert!(
            content
                .sections
                .iter()
                .any(|s| matches!(s, PopupSection::Heading(h) if h == "Elevated Fire Risk")),
            "the readable label is the heading",
        );

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
        assert_eq!(row("Hazard"), "Wind/RH");
        assert_eq!(row("Product"), "Categorical");
        assert!(
            row("Valid").starts_with("Aug 22 2026"),
            "the valid time must be the parsed field, got {:?}",
            row("Valid"),
        );
        assert!(row("Expires").starts_with("Aug 23 2026"));

        let url = content
            .sections
            .iter()
            .find_map(|s| match s {
                PopupSection::Link { url, .. } => Some(url.clone()),
                _ => None,
            })
            .expect("the popup links to the SPC website");
        assert_eq!(url, "https://www.spc.noaa.gov/products/fire_wx/fwdy1.html");
    }

    #[test]
    fn an_extended_day_links_to_the_shared_page_and_owns_its_gaps() {
        let item = FireOutlookItem {
            label: "0.40".into(),
            label2: String::new(),
            day: FireDay::Day5,
            hazard: WindRh,
            product: FireProduct::Probabilistic,
            valid: None,
            expire: None,
        };
        let prefs = squallar_units::UserPreferences::default();
        let content = item.popup_content(&prefs);
        assert_eq!(content.title, "SPC Day 5 Wind/RH Fire Outlook");
        assert!(
            content
                .sections
                .iter()
                .any(|s| matches!(s, PopupSection::Heading(h) if h == "0.40")),
            "with no long label SPC's short one is the heading, not a blank",
        );
        assert!(content.sections.iter().any(|s| matches!(
            s,
            PopupSection::Link { url, .. }
                if url == "https://www.spc.noaa.gov/products/exper/fire_wx/"
        )));
        assert!(
            content.sections.iter().any(|s| matches!(
                s,
                PopupSection::KeyValueGrid(rows)
                    if rows.iter().any(|(k, v)| k == "Valid" && v == "Unknown")
            )),
            "a missing window must read as the feed's gap, not as a shorter dialog",
        );
    }

    #[test]
    fn a_band_matches_only_its_own_hazard_and_form() {
        let band = |hazard: FireHazard, product: FireProduct| FireOutlookItem {
            label: "ELEV".into(),
            label2: String::new(),
            day: FireDay::Day5,
            hazard,
            product,
            valid: None,
            expire: None,
        };
        let cat = band(WindRh, FireProduct::Categorical);
        assert!(cat.matches(&band(WindRh, FireProduct::Categorical)));
        assert!(!cat.matches(&band(WindRh, FireProduct::Probabilistic)));
        assert!(!cat.matches(&band(DryThunderstorm, FireProduct::Categorical)));
    }

    /// **Control ids must not cross-drive the convective layer.** Both
    /// handlers see every `ControlUpdate`, so a shared id would move both
    /// layers' days at once.
    #[test]
    fn no_control_id_collides_with_the_convective_outlook_layer() {
        let ids = |items: Vec<ControlItem>| -> Vec<&'static str> {
            items
                .into_iter()
                .flat_map(|item| match item {
                    ControlItem::ButtonRow { buttons } => {
                        buttons.into_iter().map(|b| b.id).collect::<Vec<_>>()
                    }
                    ControlItem::Toggle { id, .. } => vec![id],
                    _ => Vec::new(),
                })
                .collect()
        };
        let convective = super::super::outlook::SpcOutlookHandler::new();
        let mut fire = SpcFireOutlookHandler::new();
        // Day 3 so the convective layer offers its widest control set.
        fire.defaults.selected_day = FireDay::Day3;
        let theirs = ids(convective.controls(&PaneRef::bare(0)));
        let mine = ids(fire.controls(&PaneRef::bare(0)));
        assert!(
            !theirs.is_empty() && !mine.is_empty(),
            "non-triviality floor: both layers offer controls to collide",
        );
        for id in &mine {
            assert!(
                !theirs.contains(id),
                "{id:?} is offered by both the fire and the convective outlook \
                 layer; a click on one would drive the other",
            );
        }
    }

    // ── Per-pane state ────────────────────────────────────────────────

    fn fire_pane(day: FireDay, hazards: &[FireHazard]) -> FetchPayload {
        Box::new(FirePaneState {
            selected_day: day,
            enabled_hazards: hazards.iter().copied().collect(),
        })
    }

    fn pane_ref<'a>(p: &'a FetchPayload, idx: usize) -> PaneRef<'a> {
        PaneRef {
            state: Some(&**p),
            ..PaneRef::bare(idx)
        }
    }

    /// **Two panes on two fire days**, diverged through the handler's own
    /// control route; `defaults` is asserted untouched, which fires the moment
    /// one of these methods writes a per-pane value to `&mut self`.
    #[test]
    fn two_panes_hold_different_fire_days_and_the_registry_keeps_neither() {
        let mut h = SpcFireOutlookHandler::new();
        let a = fire_pane(FireDay::Day1, &[DryThunderstorm]);
        let mut b = fire_pane(FireDay::Day1, &[DryThunderstorm]);
        assert_eq!(
            h.status_line(&pane_ref(&a, 0)),
            h.status_line(&pane_ref(&b, 1)),
            "premise: two panes on the same day and set answer the same",
        );

        h.apply_control(
            &ControlUpdate {
                id: "fireday3",
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
                .is_some_and(|line| line.starts_with(&FireDay::Day1.to_string())),
            "pane 0's day: {:?}",
            h.status_line(&pane_a),
        );
        assert!(
            h.status_line(&pane_b)
                .is_some_and(|line| line.starts_with(&FireDay::Day3.to_string())),
            "pane 1's day: {:?}",
            h.status_line(&pane_b),
        );
        // The cache token is what the render dispatch groups panes by: an
        // equal token here is one pane drawing the other pane's outlook.
        assert_ne!(
            h.content_signature(&pane_a),
            h.content_signature(&pane_b),
            "two panes on two fire days shared one cache token",
        );
        assert_eq!(
            h.serialize_pane_state(&*a)["selected_day"],
            serde_json::to_value(FireDay::Day1).unwrap(),
            "pane 0's saved bytes",
        );
        assert_eq!(
            h.serialize_pane_state(&*b)["selected_day"],
            serde_json::to_value(FireDay::Day3).unwrap(),
            "pane 1's saved bytes",
        );
        assert_eq!(
            h.defaults.selected_day,
            FireDay::Day1,
            "the registry's own copy took one of the panes' edits",
        );
        assert!(
            h.defaults.enabled_hazards.is_empty(),
            "the registry's own copy took one of the panes' hazard sets",
        );
    }

    /// **One pane's edit must not clear a failure another pane's selection is
    /// still carrying.** The round ledger is one ledger for the whole
    /// application, so the scope it is refiled against is the UNION of the
    /// panes.
    ///
    /// Non-triviality floor: the failing hazard is enabled in **pane 1 only**
    /// and in neither the edited pane nor the registry's own copy, so a
    /// pane-0-scoped refile drops it for certain.
    #[test]
    fn an_edit_in_one_pane_keeps_the_failure_another_panes_selection_carries() {
        let mut h = SpcFireOutlookHandler::new();
        let mut a = fire_pane(FireDay::Day1, &[DryThunderstorm]);
        let b = fire_pane(FireDay::Day1, &[WindRh]);

        let failing = (FireDay::Day1, WindRh, FireProduct::Categorical);
        let error = crate::fetch_policy::FetchError::transient("HTTP 500");
        h.per_product_error.insert(failing, error.clone());
        h.state.retry.record_failure(&error);
        assert!(
            h.state.retry.failures() > 0,
            "premise: the layer is on the ledger",
        );

        // Pane 0 turns its own hazard OFF and back ON. Its own selection is
        // clean; pane 1's is not.
        h.apply_control(
            &ControlUpdate {
                id: "dryt",
                value: ControlValue::Bool(false),
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

    /// Reopen is exactly 1:1: the day and the hazard set survive a
    /// save/load round trip.
    #[test]
    fn a_reopened_pane_restores_its_day_and_hazards() {
        let h = SpcFireOutlookHandler::new();
        let saved = SpcFireOutlookHandler::save_selection(&FirePaneState {
            selected_day: FireDay::Day6,
            enabled_hazards: [WindRh].into_iter().collect(),
        });
        let restored = h
            .deserialize_pane_state(saved, true)
            .expect("the fire layer keeps per-pane state");
        let state = restored
            .downcast_ref::<FirePaneState>()
            .expect("the fire layer's own state type");
        assert_eq!(state.selected_day, FireDay::Day6);
        assert_eq!(
            state.enabled_hazards.iter().copied().collect::<Vec<_>>(),
            vec![WindRh],
        );

        // A pane whose slot flag says off keeps its day and drops its set.
        let saved = SpcFireOutlookHandler::save_selection(state);
        let off = h
            .deserialize_pane_state(saved, false)
            .expect("the fire layer keeps per-pane state");
        let off = off
            .downcast_ref::<FirePaneState>()
            .expect("the fire layer's own state type");
        assert_eq!(off.selected_day, FireDay::Day6);
        assert!(off.enabled_hazards.is_empty());
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

    /// A Day-1 handler holding one issuance of the day's first file, with the
    /// given window.
    fn day1_with_window(
        valid: Option<chrono::NaiveDateTime>,
        expire: Option<chrono::NaiveDateTime>,
    ) -> SpcFireOutlookHandler {
        let mut handler = SpcFireOutlookHandler::new();
        handler.defaults = FirePaneState::new(true);
        let &(hazard, product) = products_for(FireDay::Day1)
            .first()
            .expect("day 1 publishes at least one file");
        let feature = crate::types::OverlayFeature {
            polygons: vec![vec![vec![(35.0, -102.0), (35.5, -102.0), (35.5, -101.5)]]],
            fill_rgba: [255, 120, 0, 80],
            stroke_rgba: [255, 120, 0, 255],
            label: "ELEV".into(),
            label2: String::new(),
            hatch: crate::types::HatchPattern::None,
            geo_bounds: None,
        };
        handler.state.data.insert(
            (FireDay::Day1, hazard, product),
            SpcFireOutlook {
                day: FireDay::Day1,
                hazard,
                product,
                valid,
                expire,
                features: vec![feature],
            },
        );
        handler
    }

    fn labels_at(handler: &SpcFireOutlookHandler, as_of: chrono::NaiveDateTime) -> Vec<String> {
        handler
            .paint_input(&paint_ctx(as_of), &handler.defaults)
            .map(|input| input.features.into_iter().map(|f| f.label).collect())
            .unwrap_or_default()
    }

    /// **The WB-5 floor for the fire layer.** The same 0/1/0/0 walk as the
    /// convective handler's — and since there is NO fire-weather GeoJSON
    /// archive (probed 2026-08-22, 404 on every candidate prefix), the two
    /// empty readings outside the held window are the whole truth of a
    /// scrubbed pane there, not a gap an archive fetch will later fill.
    #[test]
    fn a_fire_outlook_draws_only_while_its_issuance_is_in_force() {
        let handler = day1_with_window(Some(at(22, 12)), Some(at(23, 12)));

        assert_eq!(
            labels_at(&handler, at(22, 11)),
            Vec::<String>::new(),
            "before `valid` the issuance is not yet in force",
        );
        assert_eq!(
            labels_at(&handler, at(22, 20)),
            vec!["ELEV".to_owned()],
            "inside the window it draws",
        );
        assert_eq!(
            labels_at(&handler, at(23, 18)),
            Vec::<String>::new(),
            "an instant past the window draws nothing - there is no archive \
             to reach for, and a lapsed issuance must not stand in",
        );
    }

    /// **The cross-cutting non-triviality: a LIVE pane is byte-identical** —
    /// same statement as the convective handler's, over this handler's own
    /// paint input.
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
}
