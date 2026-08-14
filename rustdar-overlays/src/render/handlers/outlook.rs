use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::fetch_policy::Assembled;
use crate::render::controls::{
    ControlButton, ControlEffect, ControlItem, ControlUpdate, ControlValue, PaneControlContext,
    PaneControlContextMut,
};
use crate::render::overlay_state::{
    ClickableItem, FetchConfig, FetchPayload, FetchTask, HandlerJobInput, OverlayHandler,
    OverlayItem, OverlayKind, OverlayState, PopupContent, PopupSection, RasterizeContext,
    RenderMode,
};
use crate::render::rasterize;
use crate::spc::outlook::{OutlookDay, OutlookProduct, SpcOutlook};

/// `pub` for the reason `NwsAlertFetchResult` is: the frontend's described-job
/// dispatch tests seed a live registry through `apply_fetch_result`, whose
/// payload the handler downcasts to exactly this.
pub struct SpcOutlookFetchResult {
    pub day: OutlookDay,
    pub product: OutlookProduct,
    pub result: Result<SpcOutlook, crate::fetch_policy::FetchError>,
}
/// [`Assembled`], and the shape is the **round's**, not this struct's.
///
/// One of these is one product's answer; the layer's round is up to four of
/// them in flight at once, and any can fail while its siblings draw. That is
/// the assembled shape however few requests a single payload represents, and
/// the state it lands in says so — see `SpcOutlookHandler::file_round_verdict`
/// for where this layer writes its ledger, which is once per round rather than
/// once per payload.
///
/// [`Assembled`]: crate::fetch_policy::Assembled
impl crate::fetch_policy::FetchRound for SpcOutlookFetchResult {
    type Shape = crate::fetch_policy::Assembled;
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

/// What one round's answers add up to, before anything is written to the
/// ledger.
///
/// Split out so the *derivation* has one expression and the two callers can
/// differ on what they are allowed to do with it: a completed round may climb
/// the ladder, a selection change may only take the layer back down. See
/// [`SpcOutlookHandler::round_verdict`].
enum RoundVerdict {
    /// Nothing the layer asks for is failing.
    Clear,
    /// Nothing failed, and what did answer said "not published right now" —
    /// which is an answer, and resets the ladder rather than climbing it.
    NotPublished(crate::fetch_policy::FetchError),
    /// At least one product the layer asks for did not load.
    Failed(crate::fetch_policy::FetchError),
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
    ///
    /// This layer is the only one that issues **several fetch tasks per round**,
    /// one per enabled product, and they all land on one shared `state.retry`.
    /// Filing each result as it arrived made the layer's health depend on which
    /// task happened to resolve last: three products succeeding and one 500ing
    /// showed a fault or showed nothing depending on the order. Keeping the
    /// failures per product and deriving the ledger from the whole map makes the
    /// answer a property of the round instead of a race.
    per_product_error: HashMap<(OutlookDay, OutlookProduct), crate::fetch_policy::FetchError>,
    /// How many of this layer's fetch tasks are still in flight.
    ///
    /// A **count**, where every other handler keeps a bool, because a round here
    /// is one task per enabled product. As a bool the first task to land cleared
    /// it while three were still outstanding — so the layer stopped reading as
    /// fetching before it had finished, and the ledger was rewritten once per
    /// landing rather than once per round. That second part is what made the
    /// verdict order-dependent as soon as *two* products failed: an attempt
    /// count of one or two for the identical round, depending on whether the
    /// successes landed before or after the failures.
    ///
    /// Kept in step with `state.fetching` by [`Self::set_outstanding`], so the
    /// shared field is never a lie for anything that reads it.
    outstanding: usize,
    /// Whether anything the layer is **currently** asking for has answered since
    /// the last round verdict. See [`Self::file_round_verdict`].
    round_answered_in_scope: bool,
    /// Failures from the current round for products the layer has stopped
    /// asking for mid-flight. See [`Self::file_round_verdict`].
    round_stray_failures: Vec<crate::fetch_policy::FetchError>,
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
            outstanding: 0,
            round_answered_in_scope: false,
            round_stray_failures: Vec::new(),
            selected_day: OutlookDay::Day1,
            enabled_products: HashSet::new(),
        }
    }

    /// Move the outstanding-task count, keeping `state.fetching` in step.
    ///
    /// The bool is what `OverlayState::enable_should_refetch` and every generic
    /// reader consults; the count is this handler's own, and two records of the
    /// same fact that can disagree is how the first version of this went wrong.
    fn set_outstanding(&mut self, outstanding: usize) {
        self.outstanding = outstanding;
        self.state.fetching = outstanding > 0;
    }

    /// Is this key something the layer is asking for *right now*?
    ///
    /// Both halves matter: a product the user unticked and a product belonging
    /// to a day they have navigated away from are equally out of scope, and a
    /// task for either can still be in flight when they do it.
    fn in_scope(&self, key: &(OutlookDay, OutlookProduct)) -> bool {
        key.0 == self.selected_day && self.enabled_products.contains(&key.1)
    }

    /// What every product's last answer adds up to, as a **property of the
    /// selection** rather than of the order its tasks resolved in.
    ///
    /// Pure, and derived from state that outlives any one round, so it gives
    /// the same answer however many times it is asked and whenever it is asked.
    /// That is what lets [`Self::file_round_verdict`] and
    /// [`Self::refile_after_selection_change`] share it while differing only in
    /// what they are permitted to write.
    ///
    /// Walks the day's own publication order rather than `enabled_products`'
    /// `HashSet` order, for the same reason
    /// [`status_line`](OverlayHandler::status_line) does: a message built from a
    /// `HashSet` walk jitters between frames.
    ///
    /// [`Absent`](crate::fetch_policy::FetchFailure::Absent) is never counted as
    /// a failure. SPC does not keep every product up at every hour, so "not
    /// published right now" for one of four is an answer about that product and
    /// not a fault in the layer — and it only becomes the *layer's* answer when
    /// nothing else in scope drew.
    fn round_verdict(&self) -> RoundVerdict {
        let day = self.selected_day;
        let scope: Vec<OutlookProduct> = day
            .products()
            .iter()
            .copied()
            .filter(|p| self.enabled_products.contains(p))
            .collect();
        let asked = scope.len();

        let mut failed: Vec<(OutlookProduct, &crate::fetch_policy::FetchError)> = Vec::new();
        let mut absent: Vec<(OutlookProduct, &crate::fetch_policy::FetchError)> = Vec::new();
        let mut drew = false;
        for product in scope {
            let key = (day, product);
            match self.per_product_error.get(&key) {
                Some(e) if e.failure == crate::fetch_policy::FetchFailure::Absent => {
                    absent.push((product, e));
                }
                Some(e) => failed.push((product, e)),
                // No failure on file *and* something to draw: this product
                // answered. A product that has simply never been asked yet is
                // neither, and must not count as a good answer.
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
    ///
    /// The coverage axis for the one layer that builds its map a product at a
    /// time. [`round_verdict`](Self::round_verdict) above is the time axis and
    /// stays exactly as it is: a round in which one of four products refused
    /// did not complete, and the ladder has to hear about it or a genuinely
    /// dead SPC endpoint never reaches
    /// [`Broken`](crate::fetch_policy::FetchHealth::Broken). But that verdict
    /// was the *whole* of what the row said, and `! not updating` is the wrong
    /// half of the answer when three products are fresh and the fourth is
    /// simply not there: the layer is updating, and what a user needs is which
    /// product they are not looking at. The row names all four.
    ///
    /// So a product counts as missing here only when **it has never answered
    /// for this day** — `state.data` has no entry for it. One that failed this
    /// round while its previous outlook is still there is drawn and stale,
    /// which is precisely what the health axis already says; counting it on
    /// both would put two marks on the row for one fault and make neither mean
    /// anything.
    ///
    /// Entry presence, not `!features.is_empty()`, and that difference is
    /// deliberate: SPC publishing a product with no risk areas is an answer,
    /// and the empty map it produces is the **right** map. Marking it would put
    /// a fault on the row of a layer drawing exactly what was issued, which is
    /// the same argument [`Absent`] gets below. It is also the reading
    /// [`round_verdict`](Self::round_verdict)'s `drew` takes, and the two must
    /// agree or the layer would call one round both complete and empty.
    ///
    /// A product with no failure on file and nothing in `state.data` is not
    /// missing either — it has not been asked yet, which is the same reading
    /// `round_verdict` takes of it.
    ///
    /// [`Absent`] is not missing, for the reason
    /// [`StormReportRound::completeness`] gives at length: SPC does not keep
    /// every product up at every hour, and a mark that is on when nothing is
    /// wrong is a mark nobody reads on the day something is.
    ///
    /// [`Absent`]: crate::fetch_policy::FetchFailure::Absent
    /// [`StormReportRound::completeness`]: crate::spc::reports::StormReportRound::completeness
    fn round_coverage(&self) -> crate::fetch_policy::DataCompleteness {
        let day = self.selected_day;
        let mut expected = 0;
        let mut missing = 0;
        let mut reasons = Vec::new();
        for &product in day.products() {
            if !self.enabled_products.contains(&product) {
                continue;
            }
            expected += 1;
            let key = (day, product);
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
    ///
    /// One writing per round is the whole point. Rewriting the ledger on every
    /// landing made an identical round read as one failed attempt or as two
    /// depending on arrival order, and made four sibling requests refused by the
    /// same CDN edge in the same instant reach
    /// [`Broken`](crate::fetch_policy::FetchHealth::Broken) inside a single
    /// round — which is exactly the WAF blip
    /// [`REFUSALS_BEFORE_BROKEN`](crate::fetch_policy::REFUSALS_BEFORE_BROKEN)
    /// exists to survive, and it says so: "asking a second time separates the
    /// two at a cost of exactly one extra request".
    ///
    /// **A round that answered nothing in scope still files.** The user can
    /// untick a product while its task is in flight, and its error then belongs
    /// to no product the layer is asking for — but it is still evidence about
    /// the origin, and a failure that files nothing is the storm shape this
    /// crate exists to prevent: `auto_fetch_delay` reads an unstamped clock and
    /// an empty ladder as "due now", which on the web build measured 3089
    /// requests in 105 s. In-scope answers win when there are any: three
    /// products that arrived from the same origin in the same round are stronger
    /// evidence than the fourth that did not.
    fn file_round_verdict(&mut self) {
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
            match self.round_verdict() {
                RoundVerdict::Failed(e) | RoundVerdict::NotPublished(e) => {
                    self.state.retry.record_failure(&e);
                }
                RoundVerdict::Clear => self.state.retry.record_success(),
            }
        }
        // The other axis, written in the same one-writing-per-round, and the
        // half this layer never had: the verdict above says whether the round
        // completed, and this says which of the products the row is naming are
        // not on the map.
        //
        // Filed on **every** ending, including the round that answered nothing
        // in scope, which is the one place the verdict above cannot be written.
        // A user can untick the product that failed, or leave its day, while
        // its request is still on the wire: the selection change defers to this
        // round (`refile_after_selection_change` returns while `outstanding` is
        // non-zero), and then this round answers nothing it is still asked
        // about. Skipping the report here left `missing 1 of 2 outlook
        // products` in the options panel of a layer with that product no longer
        // ticked — the stuck mark this file already has one function for, on
        // the axis that function does not write. Safe because
        // [`round_coverage`](Self::round_coverage) is derived from the
        // selection as it stands now, not from anything this round did.
        let coverage = self.round_coverage();
        self.state.record_coverage(coverage);
    }

    /// Every enabled product's features, concatenated in the order they will be
    /// painted.
    ///
    /// Walks the day's own publication order, not `enabled_products`' — the
    /// same rule [`Self::status_line`] and [`Self::round_verdict`] follow, and
    /// for a sharper reason here: this vector **is** the paint order
    /// (`rasterize::rasterize_spc_outlooks` draws in list order) and
    /// `HashSet`'s iteration order is seeded per process. Walking the set
    /// directly let one selection paint in a different order after a restart,
    /// so a reopened session was not the 1:1 image it closed as.
    ///
    /// It is also what puts Day 3's significant-severe hatching on top:
    /// [`ConditionalIntensity`](OutlookProduct::ConditionalIntensity) is last
    /// in `OutlookDay::Day3.products()`, so its overlay tier lands above the
    /// probabilistic fills rather than under whichever product the hash
    /// happened to yield last.
    fn features_in_paint_order(&self) -> Vec<crate::types::OverlayFeature> {
        let day = self.selected_day;
        let mut features = Vec::new();
        for &product in day.products() {
            if !self.enabled_products.contains(&product) {
                continue;
            }
            if let Some(outlook) = self.state.data.get(&(day, product)) {
                features.extend(outlook.features.iter().cloned());
            }
        }
        features
    }

    /// What the rasterizer reads, captured once — the **one** builder
    /// `prepare_job` answers from, kept a private helper so a second dispatch
    /// path could not quietly capture different state.
    ///
    /// The hatch colour is a page-side fact (the theme) resolved **here**, at
    /// capture time, and carried as the resolved value: the worker a described
    /// job may run in has no theme to consult, and everything the hatch pass
    /// reads beyond it — each feature's own `HatchPattern` and rings — is on
    /// the feature list itself.
    fn paint_input(&self, ctx: &RasterizeContext) -> Option<rasterize::OutlooksInput> {
        let features = self.features_in_paint_order();
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

    /// Bring the products that are not independently selectable into line with
    /// the ones that are, and drop any that the selected day does not publish.
    ///
    /// [`OutlookProduct::implied_by`] names a governing product for each such
    /// product — today only Day 3's
    /// [`ConditionalIntensity`](OutlookProduct::ConditionalIntensity), governed
    /// by [`Probabilistic`](OutlookProduct::Probabilistic). This makes the
    /// implied product's membership of `enabled_products` mirror its parent's
    /// exactly, so that it takes part in the fetch scope, the `outstanding`
    /// count and the `per_product_error` ledger like any other product while
    /// never appearing as a toggle of its own.
    ///
    /// This is the **only** thing that inserts or removes an implied product,
    /// and three paths need it:
    ///
    /// * the day buttons — arriving on Day 3 from a day whose only product is
    ///   `Probabilistic` retains a parent with no child;
    /// * the product toggles — ticking `Probabilistic` inserts a parent with no
    ///   child;
    /// * [`deserialize_state`](Self::deserialize_state) — a session persisted
    ///   before Day 3 had a significant-severe product at all restores a parent
    ///   with no child, and without this would never fetch `_cigprob` again.
    ///
    /// The first two both run through
    /// [`refile_after_selection_change`](Self::refile_after_selection_change),
    /// which is documented as being called from every path that moves
    /// `selected_day` or `enabled_products`, so calling it there covers both.
    /// `set_enabled` cannot break the invariant — it either clears everything
    /// or inserts the day's first product, which is always a selectable one —
    /// but it runs this too rather than rely on that staying true.
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

    /// Drop what is no longer asked for, and take the layer back off the ledger
    /// if nothing that is left is failing.
    ///
    /// Called from every path that moves `selected_day` or `enabled_products`.
    /// Without it, unticking the one product that failed left `! not updating`
    /// on the stack row **for ever**: unticking returns
    /// [`ControlEffect::None`], so no round follows, and this layer declares no
    /// `auto_poll_interval`, so nothing automatic will ever land an `Ok` to
    /// clear the ledger either. The layer drew exactly the fresh product it was
    /// asked for and went on saying it had stopped updating.
    ///
    /// Never climbs the ladder. A selection change is not new evidence about the
    /// origin, so a verdict that is still [`Failed`](RoundVerdict::Failed) is
    /// left exactly as the round that earned it wrote it — including its
    /// sentence, which describes the round that was actually asked for and is
    /// replaced by the next one.
    ///
    /// Defers entirely while a round is in flight: that round files its own
    /// verdict when its last task lands, from the scope as it stands then.
    fn refile_after_selection_change(&mut self) {
        self.sync_implied_products();
        let day = self.selected_day;
        let enabled = self.enabled_products.clone();
        self.per_product_error
            .retain(|(d, p), _| *d == day && enabled.contains(p));
        if self.outstanding > 0 {
            return;
        }
        match self.round_verdict() {
            RoundVerdict::Failed(_) => {}
            RoundVerdict::NotPublished(e) => self.state.retry.record_failure(&e),
            RoundVerdict::Clear => self.state.retry.clear(),
        }
        // Coverage moves here even though the ladder does not, because what the
        // layer was *asked* for has changed: unticking the product that would
        // not load leaves the layer drawing everything it asks for, and a mark
        // that outlived the selection it was about is the stuck `! not
        // updating` this function exists for, one axis over.
        let coverage = self.round_coverage();
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
    ///
    /// Deliberately does **not** re-derive the ledger the way the product
    /// toggles do ([`Self::refile_after_selection_change`]). The eye is a
    /// visibility switch, not a change to what the layer asks for, and the
    /// re-ask rule reads exactly this ledger to decide that switching a stale
    /// layer back on should go to the origin rather than trust what is already
    /// drawn — clearing it here is how "toggling a frozen layer does nothing"
    /// comes back.
    fn set_enabled(&mut self, enabled: bool) {
        if enabled {
            if self.enabled_products.is_empty()
                && let Some(&first) = self.selected_day.products().first()
            {
                self.enabled_products.insert(first);
                self.sync_implied_products();
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
        // Named the way the toggles are named: an implied product is not
        // something the user chose, so listing it would describe a selection
        // that is not on offer. Days 1-2 name no significant-severe product
        // here either, for the same reason.
        let products: Vec<String> = self
            .selected_day
            .products()
            .iter()
            .filter(|p| p.is_selectable() && self.enabled_products.contains(p))
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
    /// `prepare_job` opens with, and this one was not: outlooks are keyed
    /// by `(day, product)`, so a full `state.data` says nothing about whether
    /// the selected day crossed with the ticked products yields a single
    /// feature. Untick every product, or move to a day whose products are not
    /// ticked, and the old answer was `true` while the rasterize dispatch
    /// answered `None`.
    ///
    /// That gap is not cosmetic. `ui_map_pane` reads this to decide both
    /// whether to dispatch a render *and* whether a settle render is still owed
    /// — and the second one asks for a repaint 100 ms out for as long as it is
    /// owed. An overlay that is asked for for ever and abandoned in
    /// `spawn_overlay_render` for ever is a permanent 10 Hz wakeup on an
    /// otherwise idle app, on the battery, with nothing on screen to say why.
    /// So this is the exact complement of `prepare_job`'s own early
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
        self.outstanding > 0
    }

    /// The host says a round has started or been abandoned; this layer's round
    /// is one task per enabled product, so the count moves by that many.
    ///
    /// `+=` rather than `=`: pressing Refresh while a round is in flight really
    /// does put both rounds' tasks on the wire, and the verdict is owed when the
    /// last of *all* of them lands. The number is the same expression
    /// [`create_fetch_tasks`](OverlayHandler::create_fetch_tasks) builds its
    /// list from, which
    /// `the_outstanding_count_is_the_number_of_tasks_actually_built` pins.
    /// Floored at one so that "the host marked me fetching" is never silently
    /// nothing.
    fn set_fetching(&mut self, fetching: bool) {
        if fetching {
            self.set_outstanding(self.outstanding + self.enabled_products.len().max(1));
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
        let Some(fetch) = self.state.downcast_round::<SpcOutlookFetchResult>(result) else {
            log::error!("SPC outlook handler received unexpected fetch result type");
            return;
        };
        let key = (fetch.day, fetch.product);
        let in_scope = self.in_scope(&key);
        // The **ledger** is never written here: one task's answer is one
        // product's answer, and this layer has several tasks in flight at once.
        // `file_round_verdict` writes it once, below, when the last of them
        // lands. The clock is the layer's own and *is* stamped here — an answer
        // arrived, whichever product it was about, and a round that answered
        // and left the clock unstamped is due again immediately.
        match fetch.result {
            Ok(outlook) => {
                log::info!("Received SPC outlook: {:?} {:?}", fetch.day, fetch.product);
                self.state.data.insert(key, outlook);
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
                    // The user unticked this product, or left this day, while
                    // its request was on the wire. It belongs to no product the
                    // layer asks for, so it cannot join the round's verdict —
                    // but it is still evidence about the origin, and dropping
                    // it entirely is a failure that files nothing at all. See
                    // `file_round_verdict`.
                    self.round_stray_failures.push(e);
                }
            }
        }
        if in_scope {
            self.round_answered_in_scope = true;
        }
        // Zero either because this was the round's last task or because the
        // host never marked one — the seeding paths in tests land a lone result
        // that way, and a lone result is a round of one.
        self.set_outstanding(self.outstanding.saturating_sub(1));
        if self.outstanding == 0 {
            self.file_round_verdict();
        }
    }

    fn retain_selections(&self, _selections: &mut Vec<Arc<dyn OverlayItem>>) {
        // Nothing to prune: outlook items match on day, product and label,
        // not on a data ID.
    }

    fn prepare_job(&self, ctx: &RasterizeContext) -> Option<HandlerJobInput> {
        self.paint_input(ctx).map(HandlerJobInput::Outlooks)
    }

    fn create_fetch_tasks(&self, ctx: &FetchConfig) -> Vec<FetchTask> {
        if self.enabled_products.is_empty() {
            return Vec::new();
        }
        let day = self.selected_day;
        // Publication order, not the `HashSet`'s: `sync_implied_products` keeps
        // `enabled_products` a subset of `day.products()`, so this builds the
        // same number of tasks `set_fetching` counted while giving the log line
        // and the task order the same shape on every run.
        let products: Vec<OutlookProduct> = day
            .products()
            .iter()
            .copied()
            .filter(|p| self.enabled_products.contains(p))
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

        // Only the products the selected day actually publishes, and only the
        // ones the user picks directly. Day 3's `ConditionalIntensity` is
        // fetched and accounted for like any other product but has no toggle:
        // it follows `Probabilistic`, exactly as the same features do on Days
        // 1-2 where they ride inside the hazard products. See
        // [`OutlookProduct::implied_by`].
        for &product in self
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
                    // What the layer is asking for changed, so what its ledger
                    // is a verdict about changed with it.
                    self.refile_after_selection_change();
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
                    // Unticking is the case that used to leave `! not updating`
                    // on the row for ever: it returns `None` below, so no round
                    // follows it, and this layer has no automatic poll to land
                    // an `Ok` later.
                    self.refile_after_selection_change();
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
        // A session persisted before Day 3 had a significant-severe product
        // restores `Probabilistic` with no `ConditionalIntensity` beside it.
        // Without this the layer would reopen looking exactly as it did and
        // quietly never ask for `_cigprob` again.
        self.sync_implied_products();
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

    /// A handler on Day 3 with the probabilistic product ticked, through the
    /// real control path so the implied product is filled in the way the app
    /// fills it.
    fn day3_probabilistic() -> SpcOutlookHandler {
        let mut h = SpcOutlookHandler::new();
        h.selected_day = OutlookDay::Day3;
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
        handler.apply_fetch_result(Box::new(SpcOutlookFetchResult {
            day: OutlookDay::Day3,
            product,
            result,
        }));
    }

    /// Day 3's significant-severe area is a separate endpoint, and until this
    /// was fixed it was never requested at all: `outlook_url` built only
    /// `_cat` and `_prob`, and neither carries it.
    ///
    /// It must be `_cigprob` and not `_sigprob`. `_sigprob` still answers 200
    /// with a real `SIGN` polygon but has not been re-issued since 2026-03-03,
    /// so asking for it would paint a months-old hazard area as current.
    #[test]
    fn day_3_asks_for_the_conditional_intensity_endpoint_not_the_frozen_one() {
        let sources = rustdar_radar::sources::DataSources::default();
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

    /// The product is fetched and accounted for, but the user never sees a
    /// toggle for it — Days 1-2 carry the same features inline with no toggle
    /// of their own, and an extra switch on Day 3 alone would be an asymmetry
    /// in the one place the user looks.
    #[test]
    fn the_significant_area_is_fetched_but_has_no_toggle_of_its_own() {
        let handler = day3_probabilistic();
        assert!(
            handler
                .enabled_products
                .contains(&OutlookProduct::ConditionalIntensity),
            "ticking Probabilistic must bring the significant area into scope"
        );

        let ids: Vec<&str> = handler
            .controls(&PaneControlContext {
                pane_idx: 0,
                pane_state: None,
            })
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
            handler.status_line().as_deref(),
            Some("Day 3 - Probabilistic"),
            "the status line names the selection, not the implied product"
        );
    }

    /// One product, one URL, one task, one ledger entry. Merging the two
    /// requests behind the `Probabilistic` key would have put two fetches
    /// behind one error slot, which is how a partial round comes to read as a
    /// complete one.
    #[test]
    fn the_significant_area_is_its_own_task_and_its_own_ledger_entry() {
        let mut handler = day3_probabilistic();
        let ctx = FetchConfig {
            client: Default::default(),
            zone_cache_dir: None,
            sources: rustdar_radar::sources::DataSources::default(),
            viewport: None,
        };
        assert_eq!(
            handler.create_fetch_tasks(&ctx).len(),
            2,
            "Probabilistic and its significant area are two tasks"
        );

        // The significant area fails; the probabilistic field succeeds.
        handler.set_fetching(true);
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

    /// The invariant has three ways in, and the persisted one is the quiet
    /// one: a session saved before this product existed restores
    /// `Probabilistic` alone and would otherwise never ask for `_cigprob`
    /// again, looking exactly as it did when it closed.
    #[test]
    fn every_path_that_enables_the_parent_brings_the_significant_area() {
        // 1. the product toggle
        let handler = day3_probabilistic();
        assert!(
            handler
                .enabled_products
                .contains(&OutlookProduct::ConditionalIntensity),
            "toggle path"
        );

        // 2. the day buttons, arriving from a day whose only product is
        //    Probabilistic
        let mut from_day5 = SpcOutlookHandler::new();
        from_day5.selected_day = OutlookDay::Day5;
        from_day5.set_enabled(true);
        assert_eq!(
            from_day5
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
                .enabled_products
                .contains(&OutlookProduct::ConditionalIntensity),
            "day-button path"
        );

        // 3. a session persisted before this product existed
        let mut reopened = SpcOutlookHandler::new();
        reopened.deserialize_state(serde_json::json!({
            "selected_day": "Day3",
            "enabled_products": ["Probabilistic"],
        }));
        assert!(
            reopened
                .enabled_products
                .contains(&OutlookProduct::ConditionalIntensity),
            "reopen path: a pre-change session must start asking for _cigprob"
        );
    }

    /// The mirror holds in both directions, and the implied product never
    /// survives onto a day that does not publish it.
    #[test]
    fn the_significant_area_leaves_when_its_parent_or_its_day_does() {
        let mut handler = day3_probabilistic();
        toggle(&mut handler, "prob", false);
        assert!(
            !handler
                .enabled_products
                .contains(&OutlookProduct::ConditionalIntensity),
            "unticking Probabilistic drops the significant area with it"
        );

        let mut handler = day3_probabilistic();
        toggle(&mut handler, "day1", true);
        assert!(
            !handler
                .enabled_products
                .contains(&OutlookProduct::ConditionalIntensity),
            "day 1 carries its CIG features inline and publishes no such product"
        );
    }

    /// `features` is the paint order, and `HashSet` iteration is seeded per
    /// process — so walking the set directly repainted the same selection in a
    /// different order after a restart. The significant-severe hatching is the
    /// overlay tier and must land on top, which the day's publication order
    /// gives for free.
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

        // A hash-order walk is stable within one process, so a single run of
        // this could pass by luck; what it pins is the *rule* — the order is
        // read off `OutlookDay::Day3.products()` and cannot depend on the set.
        let order: Vec<String> = handler
            .features_in_paint_order()
            .into_iter()
            .map(|f| f.label)
            .collect();
        assert_eq!(
            order,
            vec!["cat", "prob", "cig"],
            "publication order, with the significant-severe overlay last"
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

    /// One whole round through the real path: the host marks the layer fetching
    /// once, one task per enabled product, and the results land in the given
    /// order.
    ///
    /// Declaring the round is not test ceremony — it is what `App::fetch_overlay`
    /// does, and it is the only thing that tells the handler how many answers
    /// this round is owed.
    fn round(
        handler: &mut SpcOutlookHandler,
        results: Vec<(
            OutlookProduct,
            Result<SpcOutlook, crate::fetch_policy::FetchError>,
        )>,
    ) {
        handler.set_fetching(true);
        for (product, result) in results {
            land(handler, product, result);
        }
    }

    /// A handler asking for all four of day 1's products.
    fn four_product_handler() -> SpcOutlookHandler {
        let mut h = SpcOutlookHandler::new();
        for &p in OutlookDay::Day1.products() {
            h.enabled_products.insert(p);
        }
        h
    }

    fn transient() -> crate::fetch_policy::FetchError {
        crate::fetch_policy::FetchError::transient("HTTP 500")
    }

    /// Press one of the layer's own controls, exactly as the options panel does.
    fn toggle(handler: &mut SpcOutlookHandler, id: &'static str, on: bool) -> ControlEffect {
        let mut ctx = PaneControlContextMut {
            pane_idx: 0,
            pane_state: None,
        };
        handler.apply_control(
            &ControlUpdate {
                id,
                value: ControlValue::Bool(on),
            },
            &mut ctx,
        )
    }

    /// **The resolution-order test.** This layer is the only one that puts
    /// several fetch tasks in flight at once, and they all land on one shared
    /// `state.retry`. Three products succeeding and one failing must read the
    /// same either way round; it used to read as a fault or as nothing at all
    /// depending on which task the network happened to finish last.
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
        // Both orders left all three good products drawable.
        assert_eq!(failure_first.state.data.len(), 3);
        assert_eq!(failure_last.state.data.len(), 3);
    }

    /// **The order test again, with the second failure that broke it.**
    ///
    /// `refile_round_health`'s claim to be idempotent held only while *exactly
    /// one* product failed, which is all its own test exercised. With two, the
    /// ledger was rewritten once per landing: the successes reset the ladder in
    /// between, so the identical round read as one failed attempt when the
    /// failures landed first and as two when they landed last. The mark on the
    /// row and the merged sentence were right either way, which is what made it
    /// invisible — only the attempt count, and on an auto-polling layer the
    /// ladder rung it buys, differed.
    ///
    /// A round is one attempt because it is one ask.
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

    /// **Four sibling requests refused at once are one refusal.**
    ///
    /// `REFUSALS_BEFORE_BROKEN` is two because one is not evidence: the origins
    /// here are public services behind CDNs, and a single 4xx is far more often
    /// a WAF rule or a bad edge node than a real change in what is published.
    /// "Asking a second time separates the two at a cost of exactly one extra
    /// request" — but the four products of a round leave in the same instant and
    /// hit the same edge, so a per-landing filing reached `Broken` on the
    /// *second landing of the first round*, which is not asking again at all.
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

    /// A round where everything arrives is clean, and a product that recovers
    /// takes the layer back to healthy rather than leaving a stuck note.
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

    /// "Not published right now" for one product is an answer about that
    /// product, not a fault in the layer. Days 4-8 publish one product and SPC
    /// does not keep every outlook up at every hour, so treating a routine 404
    /// as staleness would put a permanent warning on a working layer.
    ///
    /// And it is the layer's *own* answer only when nothing in scope drew: three
    /// products on the map beside one unpublished fourth is a layer that is
    /// working, whichever order the four landed in.
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

        // The whole selection unpublished *is* the layer's answer, and says so
        // without climbing the ladder.
        let mut alone = SpcOutlookHandler::new();
        alone.enabled_products.insert(Tornado);
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

    /// **Unticking the product that failed takes the mark off the row.**
    ///
    /// The row read `! not updating` for ever on a layer drawing exactly the
    /// fresh product it was asked for. Unticking returns `ControlEffect::None`,
    /// so no round follows it; the ledger was only ever re-derived from
    /// `apply_fetch_result`; and this layer declares no `auto_poll_interval`, so
    /// nothing automatic would ever land an `Ok` to clear it either. Only
    /// Refresh, or switching the whole layer off and on again, cleared it.
    ///
    /// Driven through `apply_control`, because the defect was in which paths
    /// re-derive and not in the derivation.
    #[test]
    fn unticking_the_product_that_failed_stops_the_layer_reading_as_stale() {
        use OutlookProduct::{Categorical, Tornado};
        let mut h = SpcOutlookHandler::new();
        h.enabled_products.insert(Categorical);
        h.enabled_products.insert(Tornado);
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

    /// Leaving the day the failure belonged to is the same fact wearing a
    /// different control.
    #[test]
    fn navigating_to_another_day_leaves_the_old_days_failure_behind() {
        use OutlookProduct::Categorical;
        let mut h = SpcOutlookHandler::new();
        h.enabled_products.insert(Categorical);
        round(&mut h, vec![(Categorical, Err(transient()))]);
        assert!(h.state.retry.is_unhealthy(), "premise");

        let mut ctx = PaneControlContextMut {
            pane_idx: 0,
            pane_state: None,
        };
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

    /// **A failure that arrives after its product was unticked still counts.**
    ///
    /// The user can untick a product while its request is on the wire. Scoping
    /// the round's verdict to what is still asked for — which is what fixes the
    /// stuck mark above — filed that error precisely nowhere: no ladder, no
    /// clock, `health` back to `Ok`. That is the storm shape this crate exists
    /// to prevent, because `auto_fetch_delay` reads an unstamped clock and an
    /// empty ladder as "due now" and would ask again on the very next frame.
    #[test]
    fn a_failure_that_lands_after_its_product_was_unticked_still_reaches_the_ladder() {
        use OutlookProduct::Tornado;
        let mut h = SpcOutlookHandler::new();
        h.enabled_products.insert(Tornado);

        h.set_fetching(true);
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

    /// In-scope answers are the round's evidence when it has any: a failure for
    /// a product the user has just unticked must not condemn a round that three
    /// live products answered.
    #[test]
    fn a_stray_failure_does_not_condemn_a_round_that_otherwise_answered() {
        use OutlookProduct::{Categorical, Hail, Tornado, Wind};
        let mut h = four_product_handler();
        h.set_fetching(true);
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

    /// **The sixth silence, and the one the shapes surfaced.**
    ///
    /// This layer's round is up to four products at once, so it is
    /// [`Assembled`](crate::fetch_policy::Assembled) — and it declared nothing
    /// at all on the coverage axis. A tornado outlook that would not load left
    /// the row saying `! not updating`, which is the *other* fault: three of
    /// the four products were fresh, the layer was updating, and the status
    /// line went on naming a product that was not on the map anywhere.
    ///
    /// Both axes now, because the round is both things at once: it did not
    /// complete, so the ladder hears about it and a dead endpoint can still
    /// reach `Broken`; and one of the four products the row names is absent, so
    /// the row says that too.
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

        // A product that failed while its **previous** outlook is still on
        // file is stale, not missing, and must not be counted on both axes.
        // `outlook()` builds an outlook with no features, which is the harder
        // half of that rule on purpose: a product SPC published with no risk
        // areas has answered, and the empty map it produces is the right map.
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

        // Unticking the product that would not load leaves the layer drawing
        // everything it asks for, on both axes.
        assert_eq!(toggle(&mut h, "tor", false), ControlEffect::None);
        assert!(
            !h.state.retry.is_incomplete(),
            "the mark outlived the selection it was about",
        );
    }

    /// **The round that answers nothing it is still asked about.**
    ///
    /// A selection change while a round is in flight defers to that round —
    /// `refile_after_selection_change` returns on `outstanding > 0` — and the
    /// round then lands entirely out of scope, so `file_round_verdict` takes
    /// its `!answered` path. That path files a stray failure and used to file
    /// nothing else, which left the previous round's coverage report standing
    /// about products nobody is asking for any more: `missing 1 of 2 outlook
    /// products: Tornado` in the options panel of a layer with no product
    /// ticked at all.
    ///
    /// The same stuck mark `refile_after_selection_change` was written for, on
    /// the axis it does not write, which is why the report is filed on **every**
    /// ending of a round and not only on the ones that answered.
    #[test]
    fn a_round_that_lands_wholly_out_of_scope_still_retires_its_coverage_report() {
        use OutlookProduct::{Categorical, Tornado};
        let mut h = SpcOutlookHandler::new();
        h.enabled_products.insert(Categorical);
        h.enabled_products.insert(Tornado);
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

        // A second round goes out, and the user unticks both products while it
        // is on the wire. Nothing the round is about is asked for any more.
        h.set_fetching(true);
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

    /// The count `set_fetching` adds is the number of tasks the round really
    /// puts on the wire. Two expressions of one fact, and a round that waits for
    /// more answers than it asked for never files a verdict at all.
    #[test]
    fn the_outstanding_count_is_the_number_of_tasks_actually_built() {
        use crate::render::overlay_state::FetchConfig;
        rustdar_radar::tls::init();
        let ctx = FetchConfig {
            client: reqwest::Client::builder()
                .build()
                .expect("a client with no options set"),
            zone_cache_dir: None,
            sources: rustdar_radar::sources::DataSources::production(),
            viewport: None,
        };
        for products in 1..=OutlookDay::Day1.products().len() {
            let mut h = SpcOutlookHandler::new();
            for &p in &OutlookDay::Day1.products()[..products] {
                h.enabled_products.insert(p);
            }
            let built = h.create_fetch_tasks(&ctx).len();
            h.set_fetching(true);
            assert_eq!(
                h.outstanding, built,
                "the round waits for {} answers and asked {built} questions",
                h.outstanding,
            );
        }
    }
}
