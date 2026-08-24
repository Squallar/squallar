//! **A non-radar loop gains frames as its source publishes them.**
//!
//! `append_polled_frame_to_loops` read radar's slot by name —
//! and gated the append on `radar_layer::site(ls) != site`. A non-radar
//! timeline's anchor is `Box::new(())`, so `radar_layer::site` answers `""` and
//! the guard rejected unconditionally: a satellite, MRMS or model loop was
//! frozen for its whole life to the window captured at `handle_enable_loop`.
//!
//! Radar's own append is pinned next door in `loop_pane_tests.rs`, against a
//! registry that answers nothing — which is the honest double for radar, whose
//! `frames_resident` is empty by its own written contract.

use super::*;
use squallar_overlays::render::overlay_state::OverlayRegistry;
use squallar_source::handler::{FetchTask, PaneRef};
use squallar_source::id::{LayerId, known};
use squallar_source::time::{FrameListing, FrameSource, FrameStamp, TimeAxis};
use std::sync::{Arc, Mutex};

/// The stamps a handler is holding data for, mutable from the test so an hour
/// can be made to "publish" between polls.
type Published = Arc<Mutex<Vec<chrono::NaiveDateTime>>>;

/// **A framed layer whose resident set the test drives.** Everything else is
/// the minimum a handler needs to be registered.
struct PublishingLayer {
    id: LayerId,
    published: Published,
}

impl PublishingLayer {
    fn resident(&self) -> Vec<chrono::NaiveDateTime> {
        let mut stamps = self.published.lock().expect("no poisoned lock").clone();
        stamps.sort_unstable();
        stamps
    }
}

impl squallar_overlays::render::overlay_state::OverlayHandler for PublishingLayer {
    fn id(&self) -> LayerId {
        self.id.clone()
    }
    fn surface(&self) -> squallar_overlays::render::overlay_state::Surface {
        squallar_overlays::render::overlay_state::Surface::Ground
    }
    fn draw_order_weight(&self) -> u32 {
        998
    }
    fn display_name(&self) -> &str {
        "Publishing Layer"
    }
    fn render_mode(&self) -> squallar_overlays::render::overlay_state::RenderMode {
        squallar_overlays::render::overlay_state::RenderMode::Texture
    }
    fn data_generation(&self) -> u64 {
        0
    }
    fn has_data(&self, _pane: &PaneRef<'_>) -> bool {
        true
    }
    fn is_fetching(&self) -> bool {
        false
    }
    fn set_fetching(&mut self, _f: bool, _pane: &PaneRef<'_>) {}
    fn fetch_time(&self) -> Option<web_time::Instant> {
        None
    }
    fn apply_fetch_result(
        &mut self,
        _result: squallar_overlays::render::overlay_state::FetchPayload,
        _pane: &PaneRef<'_>,
    ) {
    }
    fn retain_selections(
        &self,
        _selections: &mut Vec<
            std::sync::Arc<dyn squallar_overlays::render::overlay_state::OverlayItem>,
        >,
        _pane: &PaneRef<'_>,
    ) {
    }

    fn time_axis(&self) -> TimeAxis {
        TimeAxis::FrameSeries {
            typical_step: std::time::Duration::from_secs(3600),
            extends_future: false,
        }
    }

    /// The frame-series answer, through the one `<=` in the workspace.
    fn residency_for(
        &self,
        pane: &PaneRef<'_>,
        stops: &[chrono::NaiveDateTime],
    ) -> squallar_source::time::Residency {
        squallar_source::time::frame_residency(self, pane, stops)
    }

    fn frames(&self) -> Option<&dyn FrameSource> {
        Some(self)
    }

    fn frames_mut(&mut self) -> Option<&mut dyn FrameSource> {
        Some(self)
    }
}

impl FrameSource for PublishingLayer {
    fn latest_at(&self, _pane: &PaneRef<'_>, t: chrono::NaiveDateTime) -> Option<FrameStamp> {
        let stamps: Vec<FrameStamp> = self
            .resident()
            .into_iter()
            .map(|valid| FrameStamp { valid, run: None })
            .collect();
        squallar_source::time::newest_at_or_before(&stamps, t)
    }

    fn list_frames(
        &self,
        _ctx: &squallar_overlays::render::overlay_state::FetchConfig,
        _pane: &PaneRef<'_>,
        range: (chrono::NaiveDateTime, chrono::NaiveDateTime),
    ) -> FrameListing {
        FrameListing::empty(range)
    }

    fn create_frame_list_task(
        &self,
        _ctx: &squallar_overlays::render::overlay_state::FetchConfig,
        _pane: &PaneRef<'_>,
        _range: (chrono::NaiveDateTime, chrono::NaiveDateTime),
    ) -> Option<FetchTask> {
        None
    }

    fn fetch_frame(
        &self,
        _ctx: &squallar_overlays::render::overlay_state::FetchConfig,
        _pane: &PaneRef<'_>,
        _stamp: &FrameStamp,
    ) -> Option<FetchTask> {
        None
    }

    fn frames_resident(&self, _pane: &PaneRef<'_>) -> Vec<FrameStamp> {
        self.resident()
            .into_iter()
            .map(|valid| FrameStamp { valid, run: None })
            .collect()
    }

    fn retain_frames(&mut self, _pane: &PaneRef<'_>, _keep: &[FrameStamp]) {}

    fn apply_frame_listing(
        &mut self,
        _listing: FrameListing,
        _scope: squallar_overlays::render::overlay_state::FetchPayload,
        _pane: &PaneRef<'_>,
    ) {
    }

    fn apply_frame(
        &mut self,
        _stamp: FrameStamp,
        _data: squallar_overlays::render::overlay_state::FetchPayload,
        _pane: &PaneRef<'_>,
    ) {
    }

    fn frame_horizon(&self, _pane: &PaneRef<'_>) -> chrono::Duration {
        chrono::Duration::zero()
    }
}

/// Hour `h` of a fixed day.
fn hour(h: i64) -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2024, 5, 1)
        .expect("a real date")
        .and_hms_opt(0, 0, 0)
        .expect("a real time")
        + chrono::Duration::hours(h)
}

fn frame(at: chrono::NaiveDateTime) -> squallar_egui::pane::LoopFrame {
    squallar_egui::pane::LoopFrame {
        timestamp: at,
        image: None,
        render_in_flight: false,
        render_failed: false,
    }
}

/// Twelve hours, the window a thirteen-frame hourly loop is armed over.
const WIDE: u64 = 12 * 3600;

fn satellite() -> LayerId {
    LayerId::new("test/satellite")
}

/// A pane animating `satellite()` over `frames`, and the handler's published
/// set, which starts as exactly those frames.
fn satellite_pane(frames: &[i64]) -> (squallar_egui::pane::PaneState, Published, OverlayRegistry) {
    let published: Published = Arc::new(Mutex::new(frames.iter().map(|&h| hour(h)).collect()));
    let registry = OverlayRegistry::with_handlers(vec![Box::new(PublishingLayer {
        id: satellite(),
        published: Arc::clone(&published),
    })]);
    let mut pane = squallar_egui::pane::PaneState::with_site("KTLX".to_string());
    let ls = pane.time_state_mut(&satellite());
    ls.phase = squallar_egui::pane::LoopPhase::Ready;
    ls.span_secs = WIDE;
    ls.frames = frames.iter().map(|&h| frame(hour(h))).collect();
    pane.set_transport_layer(satellite());
    (pane, published, registry)
}

fn frame_times(pane: &squallar_egui::pane::PaneState, id: &LayerId) -> Vec<chrono::NaiveDateTime> {
    pane.time_state(id)
        .frames
        .iter()
        .map(|f| f.timestamp)
        .collect()
}

fn poll(
    panes: &mut [squallar_egui::pane::PaneState],
    registry: &OverlayRegistry,
    site: &str,
    at: chrono::NaiveDateTime,
) {
    append_polled_frame_to_loops(
        panes,
        registry,
        site,
        at,
        crate::app::render::test_loop_allocation(),
        &crate::app::render::test_budgets(),
    );
}

/// **The acceptance.** A satellite loop armed over hours 0-3 gains hour 4 when
/// the source publishes it.
///
/// Before the walk landed, the loop's frame list was whatever
/// `handle_enable_loop` captured and stayed that way until the loop was
/// switched off: `radar_layer::site` answered `""` for this timeline and the
/// guard rejected every poll.
#[test]
fn a_satellite_loop_gains_frames_as_the_hours_publish() {
    let (pane, published, registry) = satellite_pane(&[0, 1, 2, 3]);
    let mut panes = [pane];

    // A poll before anything new has published changes nothing, so the gain
    // below is the publication and not the poll.
    poll(&mut panes, &registry, "KTLX", hour(3));
    assert_eq!(
        frame_times(&panes[0], &satellite()),
        vec![hour(0), hour(1), hour(2), hour(3)],
        "nothing published, nothing gained",
    );

    published.lock().expect("no poisoned lock").push(hour(4));
    poll(&mut panes, &registry, "KTLX", hour(4));

    assert_eq!(
        frame_times(&panes[0], &satellite()),
        vec![hour(0), hour(1), hour(2), hour(3), hour(4)],
        "the hour that published must join the loop",
    );

    // And again, so "gains frames" is a standing property rather than one
    // catch-up.
    published.lock().expect("no poisoned lock").push(hour(5));
    poll(&mut panes, &registry, "KTLX", hour(5));
    assert_eq!(
        frame_times(&panes[0], &satellite()).last().copied(),
        Some(hour(5)),
        "and the next one after it",
    );
}

/// **The polled site is radar's, and it is nothing to do with this layer.** A
/// scan polled from a site this pane's satellite loop never heard of still
/// carries the layer's own published hours into it.
///
/// Without this, "compare the polled site to the loop's site" passes the
/// acceptance whenever the fixture happens to poll the pane's own site.
#[test]
fn a_satellite_loop_does_not_care_which_site_the_poll_came_from() {
    let (pane, published, registry) = satellite_pane(&[0, 1]);
    let mut panes = [pane];

    published.lock().expect("no poisoned lock").push(hour(2));
    poll(&mut panes, &registry, "KOUN", hour(2));

    assert_eq!(
        frame_times(&panes[0], &satellite()),
        vec![hour(0), hour(1), hour(2)],
        "a satellite granule is not a KTLX granule or a KOUN one",
    );
}

/// **A scrubbed pane's window is closed at both ends.** An hour published
/// *after* the instant the pane is parked on is not one the pane can stop on,
/// and must not join its loop.
///
/// The live case is the acceptance above; this is the other half, and it is
/// what stops "take everything the handler holds" passing.
#[test]
fn a_scrubbed_satellite_loop_does_not_gain_hours_it_cannot_reach() {
    let (mut pane, published, registry) = satellite_pane(&[0, 1, 2]);
    pane.set_time_mode(squallar_egui::pane::TimeMode::AsOf(hour(2)));
    let mut panes = [pane];

    published
        .lock()
        .expect("no poisoned lock")
        .extend([hour(3), hour(4)]);
    poll(&mut panes, &registry, "KTLX", hour(4));

    assert_eq!(
        frame_times(&panes[0], &satellite()),
        vec![hour(0), hour(1), hour(2)],
        "a pane parked at 02:00 cannot stop on 03:00 or 04:00",
    );
}

/// **The floor.** A radar-transport pane's frame list is what it always was:
/// the polled stamp joins, and nothing the registry says changes it.
///
/// Radar's `frames_resident` is empty by its own contract — its decoded volumes
/// live above its handler — so this is asserted against the **live** registry
/// rather than an empty one, which is the only way the claim is about radar's
/// answer instead of about there being no answer.
#[test]
fn a_radar_loop_appends_exactly_the_polled_stamp_and_nothing_else() {
    let site = squallar_radar::sites::RadarSite {
        name: "KTLX",
        network: squallar_radar::sites::RadarNetwork::of_id("KTLX"),
        lat: 35.33,
        lon: -97.27,
        heights: None,
    };
    let mut pane = squallar_egui::pane::PaneState::with_site("KTLX".to_string());
    *pane.time_state_mut(&known::RADAR) = squallar_egui::radar_layer::begin_loop(
        3600,
        &site,
        squallar_radar::types::RenderView::PlanView,
    );
    let live = OverlayRegistry::with_handlers(squallar_egui::sources::all());
    let mut panes = [pane];

    for minute in [0i64, 5, 10] {
        poll(
            &mut panes,
            &live,
            "KTLX",
            hour(0) + chrono::Duration::minutes(minute),
        );
    }

    assert_eq!(
        frame_times(&panes[0], &known::RADAR),
        vec![
            hour(0),
            hour(0) + chrono::Duration::minutes(5),
            hour(0) + chrono::Duration::minutes(10),
        ],
        "three polls, three frames, and the live registry adds none of its own",
    );
    // The site guard is still the site guard.
    poll(
        &mut panes,
        &live,
        "KOUN",
        hour(0) + chrono::Duration::minutes(15),
    );
    assert_eq!(
        frame_times(&panes[0], &known::RADAR).len(),
        3,
        "a KOUN scan must not reach a KTLX loop",
    );
}
