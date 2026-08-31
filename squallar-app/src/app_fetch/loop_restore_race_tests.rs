//! **A restored playing loop survives booting before the scan list** — the
//! reopen-1:1 race `loop_persistence_tests` cannot see.
//!
//! The config restore runs on the first redraw; the site's first scan arrives
//! whenever the network answers. A pane persisted with `loop_playback:
//! "playing"` therefore reaches `handle_enable_loop` with `scan_info` still
//! `None`, and "no scan to anchor on YET" must read as *defer and retry when
//! one lands*, not as the `TransportUnlistable` exit that drops the loop for
//! the whole session.
//!
//! The twin below holds the other half in place: a transport that genuinely
//! cannot list — no scan will ever fix it — still leaves loop mode through
//! the existing path, with nothing parked to retry.

use squallar_egui::pane::{LoopArm, LoopPhase};
use squallar_overlays::render::overlay_state::{
    FetchConfig, FetchTask, OverlayHandler, OverlayRegistry, RenderMode, SourceEvent, Surface,
};
use squallar_source::handler::{FetchPayload, PaneRef};
use squallar_source::id::{LayerId, known};
use squallar_source::time::{FrameListing, FrameSource, FrameStamp};

use crate::app::tests::{n_pane_app, scan_info_for};

/// **A radar-shaped stand-in registered under radar's own id**, so every
/// `layer == known::RADAR` gate in the arming path engages, while the listing
/// task it builds resolves in closed form instead of touching the archive —
/// the same bargain `loop_supply_tests`' `SupplyLayer` makes for a forecast
/// transport. `listable: false` is the genuinely-impossible transport: the
/// registry answers no `FrameSource` at all, which no arriving scan can fix.
struct StubRadar {
    listed: Vec<chrono::NaiveDateTime>,
    listable: bool,
}

impl StubRadar {
    fn scans(
        &self,
        range: (chrono::NaiveDateTime, chrono::NaiveDateTime),
    ) -> Vec<(chrono::NaiveDateTime, squallar_radar::archive::Identifier)> {
        self.listed
            .iter()
            .filter(|valid| range.0 <= **valid && **valid <= range.1)
            .map(|valid| {
                (
                    *valid,
                    squallar_radar::archive::Identifier::new(format!("KTLX{valid}")),
                )
            })
            .collect()
    }
}

impl FrameSource for StubRadar {
    fn latest_at(&self, _pane: &PaneRef<'_>, t: chrono::NaiveDateTime) -> Option<FrameStamp> {
        let mut stamps: Vec<FrameStamp> = self
            .listed
            .iter()
            .map(|valid| FrameStamp {
                valid: *valid,
                run: None,
            })
            .collect();
        stamps.sort_by_key(|stamp| stamp.valid);
        squallar_source::time::newest_at_or_before(&stamps, t)
    }

    fn retain_frames(&mut self, _pane: &PaneRef<'_>, _keep: &[FrameStamp]) {}

    fn apply_frame_listing(
        &mut self,
        _listing: FrameListing,
        _scope: FetchPayload,
        _pane: &PaneRef<'_>,
    ) {
    }

    fn apply_frame(&mut self, _stamp: FrameStamp, _data: FetchPayload, _pane: &PaneRef<'_>) {}

    fn list_frames(
        &self,
        _ctx: &FetchConfig,
        _pane: &PaneRef<'_>,
        range: (chrono::NaiveDateTime, chrono::NaiveDateTime),
    ) -> FrameListing {
        FrameListing {
            range,
            frames: self
                .listed
                .iter()
                .filter(|valid| range.0 <= **valid && **valid <= range.1)
                .map(|valid| FrameStamp {
                    valid: *valid,
                    run: None,
                })
                .collect(),
            complete: true,
        }
    }

    fn frame_horizon(&self, _pane: &PaneRef<'_>) -> chrono::Duration {
        chrono::Duration::zero()
    }

    /// Closed form, echoing the asked window verbatim — the scope is radar's
    /// own `RadarListing` because that is what the `Ingest` drain files a
    /// radar arrival's site and range out of.
    fn create_frame_list_task(
        &self,
        _ctx: &FetchConfig,
        _pane: &PaneRef<'_>,
        range: (chrono::NaiveDateTime, chrono::NaiveDateTime),
    ) -> Option<FetchTask> {
        let scans = self.scans(range);
        let frames: Vec<FrameStamp> = scans
            .iter()
            .map(|(valid, _)| FrameStamp {
                valid: *valid,
                run: None,
            })
            .collect();
        Some(squallar_source::handler::FrameListingResult::task(
            known::RADAR,
            async move {
                squallar_source::handler::FrameListingResult {
                    listing: FrameListing {
                        range,
                        frames,
                        complete: true,
                    },
                    scope: Box::new(squallar_radar::source::RadarListing {
                        site: "KTLX".to_string(),
                        range,
                        scans,
                    }),
                }
            },
        ))
    }

    fn frames_resident(&self, _pane: &PaneRef<'_>) -> Vec<FrameStamp> {
        Vec::new()
    }

    fn fetch_frame(
        &self,
        _ctx: &FetchConfig,
        _pane: &PaneRef<'_>,
        _stamp: &FrameStamp,
    ) -> Option<FetchTask> {
        None
    }
}

impl OverlayHandler for StubRadar {
    fn id(&self) -> LayerId {
        known::RADAR
    }
    fn surface(&self) -> Surface {
        Surface::Ground
    }
    fn draw_order_weight(&self) -> u32 {
        999
    }
    fn display_name(&self) -> &str {
        "StubRadar"
    }
    fn render_mode(&self) -> RenderMode {
        RenderMode::Texture
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
    fn apply_fetch_result(&mut self, _result: FetchPayload, _pane: &PaneRef<'_>) {}
    fn retain_selections(
        &self,
        _selections: &mut Vec<
            std::sync::Arc<dyn squallar_overlays::render::overlay_state::OverlayItem>,
        >,
        _pane: &PaneRef<'_>,
    ) {
    }

    fn time_axis(&self) -> squallar_source::time::TimeAxis {
        squallar_source::time::TimeAxis::FrameSeries {
            typical_step: std::time::Duration::from_secs(300),
            extends_future: false,
        }
    }

    fn frames(&self) -> Option<&dyn FrameSource> {
        if self.listable { Some(self) } else { None }
    }

    fn frames_mut(&mut self) -> Option<&mut dyn FrameSource> {
        if self.listable { Some(self) } else { None }
    }
}

/// A one-pane app on KTLX whose radar is the stub above, with the restored
/// wish parked exactly where `App::new` leaves it.
fn app_restored_wanting_a_loop(
    listed: Vec<chrono::NaiveDateTime>,
    listable: bool,
) -> crate::app::App {
    let mut app = n_pane_app(1, "KTLX");
    app.gui.overlays =
        OverlayRegistry::with_handlers(vec![Box::new(StubRadar { listed, listable })]);
    app.loop_arm_pending
        .push((0, LoopArm { playing: true }, 600));
    app
}

/// When the scan the loop anchors on is stamped, and so where its window ends.
fn scan_stamp() -> chrono::NaiveDateTime {
    scan_info_for("KTLX").timestamp
}

/// **The defect, end to end**: restore before any scan, deliver the scan, and
/// the loop must be armed with its play request intact — not dropped by the
/// boot-time "leaving loop mode" exit this suite was written against.
#[test]
fn a_restored_playing_loop_survives_booting_before_the_scan_list() {
    let end = scan_stamp();
    let listed: Vec<_> = [8i64, 4, 0]
        .iter()
        .map(|m| end - chrono::Duration::minutes(*m))
        .collect();
    let mut app = app_restored_wanting_a_loop(listed.clone(), true);
    assert!(
        app.gui
            .pane(0)
            .expect("the fixture built one pane")
            .scan_info
            .is_none(),
        "precondition: the restore must run before any scan has landed",
    );

    // The boot redraw: the restore races the scan list and loses.
    app.hydrate_parked_panes();

    let pane = app.gui.pane(0).expect("the fixture built one pane");
    assert_eq!(
        pane.transport_state().phase,
        LoopPhase::Inactive,
        "nothing can be armed yet: there is no scan to anchor the window on",
    );
    assert_eq!(
        pane.loop_arm_pending,
        Some(LoopArm { playing: true }),
        "the wish must stay parked on the pane while the scan is on the wire \
         — this is what a save made during the wait writes back",
    );
    assert!(
        !app.loop_arm_pending.is_empty(),
        "and the retry must stay queued — a drained queue here is the loop \
         dropped for the whole session",
    );

    // The scan lands; its arrival notifies a redraw, which is the next
    // hydrate pass.
    app.gui
        .pane_mut(0)
        .expect("the fixture built one pane")
        .scan_info = Some(scan_info_for("KTLX"));
    app.hydrate_parked_panes();

    let pane = app.gui.pane(0).expect("the fixture built one pane");
    let ls = pane.time_state(&known::RADAR);
    assert_eq!(
        ls.phase,
        LoopPhase::FetchingScanList,
        "the deferred wish must arm the moment a scan exists",
    );
    assert!(
        ls.autoplay_on_ready,
        "a loop persisted PLAYING must still be asking to play once ready — \
         arming replaced the transport's timeline state, so the request has \
         to be written after the arm, not before",
    );
    let range = ls.asked_range.expect("an armed loop recorded its ask");
    assert!(
        app.loop_arm_pending.is_empty() && pane.loop_arm_pending.is_none(),
        "the wish is spent by the arm; nothing is left to act on it twice",
    );

    // The listing lands on the one arrival path, over the window the arm
    // recorded — the same delivery `a_listing_that_arrives_on_the_source_path_
    // builds_the_loop_waiting_for_it` makes.
    let scans: Vec<_> = listed
        .iter()
        .map(|valid| {
            (
                *valid,
                squallar_radar::archive::Identifier::new(format!("KTLX{valid}")),
            )
        })
        .collect();
    app.channels
        .overlay_fetch_sender
        .send(SourceEvent::Frames {
            id: known::RADAR,
            listing: FrameListing {
                range,
                frames: listed
                    .iter()
                    .map(|valid| FrameStamp {
                        valid: *valid,
                        run: None,
                    })
                    .collect(),
                complete: true,
            },
            scope: Box::new(squallar_radar::source::RadarListing {
                site: "KTLX".to_string(),
                range,
                scans,
            }),
        })
        .expect("the receiver is alive");
    app.poll_overlay_fetch_results();
    app.accept_loop_scan_listings();

    let ls = app
        .gui
        .pane(0)
        .expect("the fixture built one pane")
        .time_state(&known::RADAR);
    assert_eq!(
        ls.phase,
        LoopPhase::Rendering,
        "the listing must build the loop the deferred arm was waiting for",
    );
    assert!(
        !ls.frames.is_empty(),
        "with the listed frames on the pane, not an empty timeline",
    );
    assert!(
        ls.autoplay_on_ready,
        "and the play request must survive the listing's landing — the \
         readiness pass is what spends it, into LoopPhase::Playing",
    );
}

/// **The untouched twin**: a transport that genuinely cannot list still exits
/// loop mode through the existing path — refused once, with nothing parked,
/// not retried forever.
#[test]
fn a_transport_that_cannot_list_still_leaves_loop_mode() {
    let mut app = app_restored_wanting_a_loop(Vec::new(), false);
    // The scan is HERE: what fails is the listing itself, which no arriving
    // scan can fix.
    app.gui
        .pane_mut(0)
        .expect("the fixture built one pane")
        .scan_info = Some(scan_info_for("KTLX"));

    app.hydrate_parked_panes();

    let pane = app.gui.pane(0).expect("the fixture built one pane");
    assert_eq!(
        pane.transport_state().phase,
        LoopPhase::Inactive,
        "the loop was refused, exactly as before",
    );
    assert!(
        app.loop_arm_pending.is_empty() && pane.loop_arm_pending.is_none(),
        "a refusal parks nothing: an impossible listing must not become an \
         infinite retry",
    );
}
