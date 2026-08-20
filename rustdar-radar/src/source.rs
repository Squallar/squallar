//! The radar layer as a source — its registration, its toggle and its saved
//! state, and nothing else.

use rustdar_source::handler::{PaneMut, PaneRef, PaneToggle};
use std::collections::HashMap;
use std::sync::Arc;

use crate::archive::Identifier;
use chrono::NaiveDateTime;
use rustdar_source::controls::{ControlEffect, ControlItem, ControlUpdate, ControlValue};
use rustdar_source::fetch_policy::FetchRetry;
use rustdar_source::handler::{
    FetchConfig, FetchPayload, FetchTask, FrameListingResult, OverlayItem, RenderMode,
    SourceHandler, Surface,
};
use rustdar_source::id::{LayerId, known};
use rustdar_source::time::{FrameListing, FrameStamp};

/// **The radar layer's registration — one row, and the only one this crate
/// has.**
pub fn sources() -> Vec<Box<dyn SourceHandler>> {
    vec![Box::new(RadarSource::new())]
}

/// How often a live pane asks the archive whether a newer volume exists. The
/// WSR-88D publishes a completed volume every few minutes; a minute is the
/// coarsest check that never leaves one sitting unnoticed for a whole scan.
const ARCHIVE_POLL_SECS: u64 = 60;

/// The control id the archive poll's switch is written and read through — one
/// field, two surfaces (the ☰ menu and the settings row), no copy to drift.
pub const AUTO_POLL_CONTROL: &str = "auto_poll";

/// The one spelling of that switch's label, so the menu leaf, the settings row
/// and the inspector row cannot disagree about what it is called.
pub const AUTO_POLL_LABEL: &str = "Auto-poll";

/// **What one archive listing said, filed under the site it was listed for.**
///
/// The scope half of [`rustdar_source::handler::SourceEvent::Frames`] for this
/// layer. The generic half carries [`FrameStamp`]s and by its own contract
/// names no site; this carries the site — **captured at dispatch, never read
/// back off the pane on arrival** — and the archive object each stamp is a
/// statement about, which is the half only radar can interpret.
pub struct RadarListing {
    /// NEXRAD site the listing was requested for. A listing is an
    /// uncancellable round trip and a pane's loop can be rebuilt for another
    /// site while it is in the air, so without this the receiver would file
    /// one site's list under another's — the fixed bug `loop_downloads`
    /// records, in its other half.
    pub site: String,
    /// The window that was listed, echoed.
    pub range: (NaiveDateTime, NaiveDateTime),
    /// Volume start and archive object per scan, ascending.
    pub scans: Vec<(NaiveDateTime, Identifier)>,
}

/// **One loop frame's archive object as it comes off the wire, undecoded.**
///
/// The payload half of [`rustdar_source::handler::SourceEvent::FrameReady`]
/// for this layer. The decode is deliberately *not* here: it rides the
/// worker funnel from the arrival path, so a frame's bytes cross the channel
/// once and the decode is scheduled beside every other offloaded job.
///
/// `site` is the site of the **listing the identifier came from**, carried
/// rather than re-read on arrival, for the reason [`RadarListing::site`]
/// gives.
pub struct RadarFrameFetch {
    pub site: String,
    pub timestamp: NaiveDateTime,
    /// `None` when the download failed. The failure still arrives, because it
    /// is the only thing that clears the frame's in-flight mark.
    pub archive: Option<Vec<u8>>,
}

/// Toggle and config state only. Radar fetching, rendering and per-frame
pub struct RadarSource {
    pub enabled: bool,
    /// **The archive object each listed volume is, keyed by the site the
    /// listing was made for and the instant the volume starts.**
    ///
    /// The site is in the key and never absent from it: a `FrameStamp` alone
    /// is never a radar cache key, because two sites' volumes routinely share
    /// a timestamp and keying on the timestamp alone once rendered one
    /// radar's data about another's coordinates, undetectably.
    ///
    /// Identifiers are radar-private — nothing above this layer holds one —
    /// which is what makes [`SourceHandler::fetch_frame`] the only door to a
    /// frame's bytes.
    listings: HashMap<(String, NaiveDateTime), Identifier>,
    /// The windows each site has a **covering** listing for, so
    /// [`SourceHandler::list_frames`] can say `complete` about a window
    /// rather than about whatever it happens to hold.
    ///
    /// Merged rather than replaced: two panes looping one site with two
    /// windows must not evict each other's frames. Nothing prunes this
    /// within a session — a listed site-day is ~288 stamps and one identifier
    /// each — and the eviction door for it is
    /// [`SourceHandler::retain_frames`], which this layer does not yet
    /// answer.
    covered: HashMap<String, Vec<(NaiveDateTime, NaiveDateTime)>>,
    /// **Whether the archive poll runs at all** — the ☰ menu's "Auto-poll" and
    /// the settings row are two surfaces over this one field, so neither can
    /// drift from the other. Off means this layer declares no poll interval,
    /// which is what makes [`SourceHandler::auto_fetch_delay`] answer `None`.
    auto_poll_enabled: bool,
    /// **When a round was last asked for**, which is what this layer's poll
    /// clock counts from — see [`SourceHandler::set_fetching`] for why the ask
    /// and not the answer.
    last_round: Option<web_time::Instant>,
    /// Whether a *tracked* round is in flight. The 60 s check is not one: see
    /// [`SourceHandler::set_fetching`].
    fetching: bool,
    /// The failure ladder every auto-polling layer carries, so a failed round
    /// is not retried on the next frame.
    retry: FetchRetry,
}

impl RadarSource {
    pub fn new() -> Self {
        Self {
            enabled: true,
            listings: HashMap::new(),
            covered: HashMap::new(),
            auto_poll_enabled: true,
            last_round: None,
            fetching: false,
            retry: FetchRetry::new(),
        }
    }

    /// **The site this pane is on**, from the slot config `publish_radar_selection`
    /// keeps current — the only thing that names a pane's selection to a
    /// handler. `None` on a pane whose slots have never been hydrated, and on
    /// the `PaneRef::across` union, whose config is null by construction.
    fn site_of<'a>(pane: &PaneRef<'a>) -> Option<&'a str> {
        pane.config.get("site")?.as_str()
    }

    /// Whether `site` has a listing covering the whole of `range`.
    fn covers(&self, site: &str, range: (NaiveDateTime, NaiveDateTime)) -> bool {
        self.covered
            .get(site)
            .is_some_and(|windows| windows.iter().any(|w| w.0 <= range.0 && w.1 >= range.1))
    }

    /// The archive object `site`'s volume starting at `valid` is, if a listing
    /// has named it.
    pub fn identifier(&self, site: &str, valid: NaiveDateTime) -> Option<&Identifier> {
        self.listings.get(&(site.to_string(), valid))
    }
}

impl Default for RadarSource {
    fn default() -> Self {
        Self::new()
    }
}

impl SourceHandler for RadarSource {
    fn id(&self) -> LayerId {
        known::RADAR
    }
    fn surface(&self) -> Surface {
        Surface::Ground
    }
    fn draw_order_weight(&self) -> u32 {
        30
    }
    fn display_name(&self) -> &str {
        "Radar"
    }
    fn render_mode(&self) -> RenderMode {
        RenderMode::Texture
    }
    /// Discrete stamped volumes, never ahead of the wall clock. The nominal
    /// step is the WSR-88D precipitation cadence; the measured truth for a
    /// window is the loop's own `cadence_secs`, which this never overrides.
    fn time_axis(&self) -> rustdar_source::time::TimeAxis {
        rustdar_source::time::TimeAxis::FrameSeries {
            typical_step: std::time::Duration::from_secs(300),
            extends_future: false,
        }
    }
    fn default_enabled(&self) -> bool {
        true
    }

    // ── Frame supply (WO-M12b) ────────────────────────────────────────
    //
    // The three doors a `FrameSeries` layer answers through. `list_frames` is
    // the synchronous read, `create_frame_list_task` the listing that fills
    // it, `fetch_frame` the one route to a frame's bytes. Every one of them
    // is scoped by the SITE, which the stamps themselves never carry.

    /// The volumes this site has listed inside `range`, ascending.
    ///
    /// `complete` is `true` only where a listing covering the whole window has
    /// landed for this pane's site — never merely because the answer is
    /// non-empty, and never for a window a listing failed on.
    fn list_frames(
        &self,
        ctx: &FetchConfig,
        pane: &PaneRef<'_>,
        range: (NaiveDateTime, NaiveDateTime),
    ) -> FrameListing {
        let _ = ctx;
        let Some(site) = Self::site_of(pane) else {
            return FrameListing::empty(range);
        };
        let mut frames: Vec<FrameStamp> = self
            .listings
            .keys()
            .filter(|(listed, valid)| listed == site && *valid >= range.0 && *valid <= range.1)
            .map(|(_, valid)| FrameStamp {
                valid: *valid,
                run: None,
            })
            .collect();
        frames.sort_by_key(|frame| frame.valid);
        FrameListing {
            range,
            frames,
            complete: self.covers(site, range),
        }
    }

    /// **One S3 LIST per UTC day the window touches**, for the site this pane
    /// is on.
    ///
    /// The site is captured HERE, at dispatch, and travels back in the scope:
    /// the round trip is uncancellable and the pane can be rebuilt for
    /// another site while it is in the air. No dedupe of its own — a listing
    /// is asked for when a loop is built, and a loop is built once, which is
    /// exactly the cadence the direct call had.
    fn create_frame_list_task(
        &self,
        ctx: &FetchConfig,
        pane: &PaneRef<'_>,
        range: (NaiveDateTime, NaiveDateTime),
    ) -> Option<FetchTask> {
        let _ = ctx;
        let site = Self::site_of(pane)?.to_string();
        Some(FrameListingResult::task(known::RADAR, async move {
            let (scans, complete) =
                match crate::scan::list_scans_for_range(&site, range.0, range.1).await {
                    Ok(scans) => {
                        log::info!("Loop: found {} {site} scans in range", scans.len());
                        (scans, true)
                    }
                    Err(e) => {
                        // An empty list is how a failed listing reaches the pane,
                        // and `complete: false` is how it stays honest about why:
                        // "I found none" is not "none exist".
                        log::error!("Loop scan listing failed for {site}: {e:?}");
                        (Vec::new(), false)
                    }
                };
            let frames = scans
                .iter()
                .map(|(valid, _id)| FrameStamp {
                    valid: *valid,
                    run: None,
                })
                .collect();
            FrameListingResult {
                listing: FrameListing {
                    range,
                    frames,
                    complete,
                },
                scope: Box::new(RadarListing { site, range, scans }),
            }
        }))
    }

    /// **The one door to a loop frame's bytes**, undecoded.
    ///
    /// `None` when no listing has named that volume for this pane's site —
    /// which is also the answer for a pane whose loop is being rebuilt for
    /// another site while its old queue is still draining: the identifier is
    /// filed under the site it was listed for, and this pane no longer names
    /// that site.
    fn fetch_frame(
        &self,
        ctx: &FetchConfig,
        pane: &PaneRef<'_>,
        stamp: &FrameStamp,
    ) -> Option<FetchTask> {
        let _ = ctx;
        let site = Self::site_of(pane)?.to_string();
        let identifier = self.identifier(&site, stamp.valid)?.clone();
        let timestamp = stamp.valid;
        Some(FetchTask {
            kind: known::RADAR,
            future: Box::pin(async move {
                let archive = match crate::scan::fetch_scan_object(identifier).await {
                    Ok(archive) => Some(archive),
                    Err(e) => {
                        log::error!("Loop scan download failed for {site} @ {timestamp}: {e:?}");
                        None
                    }
                };
                Box::new(RadarFrameFetch {
                    site,
                    timestamp,
                    archive,
                }) as FetchPayload
            }),
        })
    }

    /// File a listing under the site it was **listed for**, from the scope,
    /// never from the pane: the `PaneRef` an arrival carries is the union
    /// across panes and its config is null by construction.
    fn apply_frame_listing(
        &mut self,
        listing: FrameListing,
        scope: FetchPayload,
        pane: &PaneRef<'_>,
    ) {
        let _ = pane;
        let Ok(scope) = scope.downcast::<RadarListing>() else {
            log::error!("a frame listing reached the radar layer under another layer's scope");
            return;
        };
        let RadarListing { site, range, scans } = *scope;
        for (valid, identifier) in scans {
            self.listings.insert((site.clone(), valid), identifier);
        }
        // Coverage is recorded only for a listing that really covered the
        // window. A failure arrives empty so the pane can retire its loop,
        // and must not leave `list_frames` claiming the window is settled.
        if listing.complete {
            self.covered.entry(site).or_default().push(range);
        }
    }
    fn is_enabled(&self, pane: &PaneRef<'_>) -> bool {
        PaneToggle::is_on(pane, self.enabled)
    }
    fn set_enabled(&mut self, enabled: bool, pane: &mut PaneMut<'_>) {
        if !PaneToggle::set(pane, enabled) {
            self.enabled = enabled;
        }
    }

    fn data_generation(&self) -> u64 {
        0
    }
    fn has_data(&self, _pane: &PaneRef<'_>) -> bool {
        true
    }
    fn is_fetching(&self) -> bool {
        self.fetching
    }
    /// **The rising edge stamps this layer's poll clock**, and that is the
    /// whole reason this is not the default no-op.
    ///
    /// Every other layer's clock stamps on the *answer*, because every other
    /// layer's round always produces one. Radar's 60 s check does not: the
    /// archive is asked whether anything is newer than what the pane already
    /// has, and a check that finds nothing sends nothing back. A clock that
    /// waited for a delivery would therefore still read "never fetched" on the
    /// next frame, and the layer would be due again immediately — the
    /// per-frame poll storm [`SourceHandler::auto_fetch_delay`] exists to
    /// stop. So the clock counts from the **ask**, which is exactly what the
    /// state this replaced recorded.
    fn set_fetching(&mut self, fetching: bool, _pane: &PaneRef<'_>) {
        if fetching {
            self.last_round = Some(web_time::Instant::now());
        }
        self.fetching = fetching;
    }
    fn fetch_time(&self) -> Option<web_time::Instant> {
        self.last_round
    }

    /// `None` when the user has switched the poll off, which is how the
    /// trait's own [`SourceHandler::auto_fetch_delay`] comes to answer `None`
    /// without a second copy of the two-term policy living here.
    fn auto_poll_interval(&self) -> Option<u64> {
        self.auto_poll_enabled.then_some(ARCHIVE_POLL_SECS)
    }

    fn retry(&self) -> Option<&FetchRetry> {
        Some(&self.retry)
    }

    fn retry_mut(&mut self) -> Option<&mut FetchRetry> {
        Some(&mut self.retry)
    }

    fn rewind_fetch_time(&mut self, by: std::time::Duration) {
        if let Some(at) = self.last_round {
            self.last_round = Some(at - by);
        }
    }

    fn apply_fetch_result(&mut self, _result: FetchPayload, _pane: &PaneRef<'_>) {}
    fn retain_selections(&self, _selections: &mut Vec<Arc<dyn OverlayItem>>, _pane: &PaneRef<'_>) {}

    fn controls(&self, pane: &PaneRef<'_>) -> Vec<ControlItem> {
        vec![
            ControlItem::Toggle {
                id: "enabled",
                label: "Radar".to_string(),
                enabled: self.is_enabled(pane),
            },
            ControlItem::Toggle {
                id: AUTO_POLL_CONTROL,
                label: AUTO_POLL_LABEL.to_string(),
                enabled: self.auto_poll_enabled,
            },
        ]
    }

    fn apply_control(&mut self, update: &ControlUpdate, pane: &mut PaneMut<'_>) -> ControlEffect {
        match update.id {
            "enabled" => {
                if let ControlValue::Bool(val) = update.value
                    && !PaneToggle::set(pane, val)
                {
                    self.enabled = val;
                }
            }
            AUTO_POLL_CONTROL => {
                if let ControlValue::Bool(val) = update.value {
                    self.auto_poll_enabled = val;
                }
            }
            _ => {}
        }
        ControlEffect::None
    }

    // ── Per-pane state (WO-M10b) ──────────────────────────────────────
    //
    // This layer's only per-pane fact is whether the pane draws it, so its
    // state IS the toggle. `self.enabled` survives as the LAYER'S DEFAULT for
    // a caller that supplies no pane; nothing reads it into a pane, and the
    // global `serialize_state` no longer carries it — the pane's slot does.

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
    use rustdar_source::handler::PaneRef;

    fn ts(minute: u32) -> NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(2024, 1, 1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            + chrono::Duration::minutes(i64::from(minute))
    }

    /// A pane whose radar slot publishes `site`, built here rather than taken
    /// from any shared table: this suite must mean the same thing run alone as
    /// run beside its neighbours.
    fn on_site(site: &str) -> serde_json::Value {
        serde_json::json!({ "site": site })
    }

    fn pane<'a>(config: &'a serde_json::Value) -> PaneRef<'a> {
        PaneRef {
            pane_idx: 0,
            config,
            state: None,
            slots: &[],
            loading_site: None,
            peers: &[],
        }
    }

    fn listing(site: &str, minutes: &[u32], complete: bool) -> (FrameListing, FetchPayload) {
        let scans: Vec<(NaiveDateTime, Identifier)> = minutes
            .iter()
            .map(|&m| (ts(m), Identifier::new(format!("{site}-{m:02}"))))
            .collect();
        let range = (ts(0), ts(10));
        (
            FrameListing {
                range,
                frames: scans
                    .iter()
                    .map(|(valid, _)| FrameStamp {
                        valid: *valid,
                        run: None,
                    })
                    .collect(),
                complete,
            },
            Box::new(RadarListing {
                site: site.to_string(),
                range,
                scans,
            }),
        )
    }

    fn ctx() -> FetchConfig {
        // A `reqwest::Client` cannot be built before the process has a rustls
        // provider, and a test that builds one is otherwise green only when
        // some EARLIER test in the same binary happened to install it.
        rustdar_source::tls::init();
        FetchConfig {
            client: Default::default(),
            zone_cache_dir: None,
            sources: rustdar_source::origins::DataSources::default(),
            viewport: None,
        }
    }

    /// **Two sites' listings do not collide, and the volume one site holds at
    /// an instant is never handed out as the other's.**
    ///
    /// This is the fixed bug `loop_downloads` records, on the listing side:
    /// two radars routinely start volumes at the same instant, and a cache
    /// keyed on the timestamp alone once rendered one radar's data about the
    /// other's coordinates, undetectably.
    #[test]
    fn one_sites_listing_is_never_handed_out_as_anothers() {
        let mut source = RadarSource::new();
        let ktlx = on_site("KTLX");
        let koun = on_site("KOUN");
        let (l, scope) = listing("KTLX", &[0, 2], true);
        source.apply_frame_listing(l, scope, &pane(&ktlx));
        let (l, scope) = listing("KOUN", &[0, 2], true);
        source.apply_frame_listing(l, scope, &pane(&koun));

        assert_eq!(
            source.identifier("KTLX", ts(0)).map(Identifier::name),
            Some("KTLX-00"),
            "the volume KTLX starts at this instant is not the one it listed",
        );
        assert_eq!(
            source.identifier("KOUN", ts(0)).map(Identifier::name),
            Some("KOUN-00"),
            "one site's listing displaced another's at the same instant — the \
             timestamp alone is never a radar cache key",
        );
        assert_eq!(
            source.identifier("KFWS", ts(0)).map(Identifier::name),
            None,
            "a site nothing listed answers with another site's object",
        );
    }

    /// **`list_frames` answers for the pane's own site, and for no other.**
    #[test]
    fn a_pane_is_told_only_its_own_sites_frames() {
        let mut source = RadarSource::new();
        let ktlx = on_site("KTLX");
        let (l, scope) = listing("KTLX", &[0, 2, 4], true);
        source.apply_frame_listing(l, scope, &pane(&ktlx));
        let (l, scope) = listing("KOUN", &[1, 3], true);
        source.apply_frame_listing(l, scope, &pane(&on_site("KOUN")));

        let answered = source.list_frames(&ctx(), &pane(&ktlx), (ts(0), ts(10)));
        assert_eq!(
            answered.frames.iter().map(|f| f.valid).collect::<Vec<_>>(),
            vec![ts(0), ts(2), ts(4)],
            "the pane was told frames its own site never listed, or told them \
             out of order",
        );
        assert!(
            answered.frames.iter().all(|f| f.run.is_none()),
            "an observed volume has no model run",
        );
    }

    /// **The window is clipped, inclusive at both ends** — the bounds the
    /// direct listing call has always used.
    #[test]
    fn the_window_is_clipped_inclusively() {
        let mut source = RadarSource::new();
        let ktlx = on_site("KTLX");
        let (l, scope) = listing("KTLX", &[0, 2, 4, 6], true);
        source.apply_frame_listing(l, scope, &pane(&ktlx));

        let answered = source.list_frames(&ctx(), &pane(&ktlx), (ts(2), ts(4)));
        assert_eq!(
            answered.frames.iter().map(|f| f.valid).collect::<Vec<_>>(),
            vec![ts(2), ts(4)],
            "the bounds are not inclusive at both ends",
        );
        assert_eq!(
            answered.range,
            (ts(2), ts(4)),
            "the window asked about is not echoed back",
        );
    }

    /// **`complete` is a statement about the WINDOW, not about the answer.**
    ///
    /// A window no listing covered is answered honestly — "at least these" —
    /// even when the handler happens to hold frames inside it.
    #[test]
    fn a_window_no_listing_covered_is_not_called_complete() {
        let mut source = RadarSource::new();
        let ktlx = on_site("KTLX");
        let (l, scope) = listing("KTLX", &[0, 2], true);
        source.apply_frame_listing(l, scope, &pane(&ktlx));

        assert!(
            source
                .list_frames(&ctx(), &pane(&ktlx), (ts(0), ts(10)))
                .complete,
            "the window that was listed is not called complete",
        );
        let wider = source.list_frames(&ctx(), &pane(&ktlx), (ts(0), ts(20)));
        assert!(
            !wider.complete,
            "a window reaching past every listing was called complete",
        );
        assert_eq!(
            wider.frames.len(),
            2,
            "an incomplete answer must still name what it holds",
        );
    }

    /// **A listing that failed files its site's objects for nobody and leaves
    /// the window open**, so the next attempt is still owed.
    #[test]
    fn a_failed_listing_leaves_its_window_open() {
        let mut source = RadarSource::new();
        let ktlx = on_site("KTLX");
        let (l, scope) = listing("KTLX", &[], false);
        source.apply_frame_listing(l, scope, &pane(&ktlx));

        let answered = source.list_frames(&ctx(), &pane(&ktlx), (ts(0), ts(10)));
        assert!(
            !answered.complete,
            "a window a listing failed on was recorded as settled",
        );
        assert!(answered.frames.is_empty());
    }

    /// **A pane that publishes no site is told nothing and asked nothing.**
    ///
    /// The `PaneRef::across` union is exactly this shape — its config is null
    /// by construction — so a handler that read a site out of the pane on an
    /// arrival would read none, and this is what says so.
    #[test]
    fn a_pane_that_names_no_site_gets_no_frames_and_no_task() {
        let mut source = RadarSource::new();
        let ktlx = on_site("KTLX");
        let (l, scope) = listing("KTLX", &[0, 2], true);
        source.apply_frame_listing(l, scope, &pane(&ktlx));

        let null = serde_json::Value::Null;
        let answered = source.list_frames(&ctx(), &pane(&null), (ts(0), ts(10)));
        assert!(
            answered.frames.is_empty() && !answered.complete,
            "a pane naming no site was answered with some site's frames",
        );
        assert!(
            source
                .create_frame_list_task(&ctx(), &pane(&null), (ts(0), ts(10)))
                .is_none(),
            "a listing was dispatched for a pane that names no site",
        );
        assert!(
            source
                .fetch_frame(
                    &ctx(),
                    &pane(&null),
                    &FrameStamp {
                        valid: ts(0),
                        run: None,
                    },
                )
                .is_none(),
            "a frame was fetched for a pane that names no site",
        );
    }

    /// **`fetch_frame` is the door, and it opens only on a volume this pane's
    /// own site listed.**
    #[test]
    fn a_frame_is_fetchable_only_where_this_panes_site_listed_one() {
        let mut source = RadarSource::new();
        let ktlx = on_site("KTLX");
        let (l, scope) = listing("KTLX", &[0, 2], true);
        source.apply_frame_listing(l, scope, &pane(&ktlx));

        let stamp = |m| FrameStamp {
            valid: ts(m),
            run: None,
        };
        assert!(
            source
                .fetch_frame(&ctx(), &pane(&ktlx), &stamp(0))
                .is_some(),
            "a listed volume could not be fetched",
        );
        assert!(
            source
                .fetch_frame(&ctx(), &pane(&ktlx), &stamp(3))
                .is_none(),
            "a volume no listing named was fetched anyway",
        );
        assert!(
            source
                .fetch_frame(&ctx(), &pane(&on_site("KOUN")), &stamp(0))
                .is_none(),
            "a pane on another site was handed KTLX's archive object — the \
             fixed bug, on the fetch side",
        );
    }

    /// **The listing a pane asks for is dispatched for the site it publishes.**
    #[test]
    fn a_pane_that_names_a_site_can_ask_for_its_listing() {
        let source = RadarSource::new();
        assert!(
            source
                .create_frame_list_task(&ctx(), &pane(&on_site("KTLX")), (ts(0), ts(10)))
                .is_some(),
            "a pane on a real site could not ask for a listing",
        );
    }
}
