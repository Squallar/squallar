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

/// A cost table with `spare` on both pools and `arm_loop` priced for pane 0.
fn admission_table(generation: u64, spare_bytes: u64) -> squallar_egui::admission::AdmissionCosts {
    squallar_egui::admission::AdmissionCosts {
        generation,
        spare: squallar_device_profile::admit::Spare {
            gpu_bytes: Some(spare_bytes),
            host_bytes: Some(spare_bytes),
            joint_bytes: None,
        },
        panes: vec![squallar_egui::admission::PaneAdmission {
            arm_loop: squallar_device_profile::admit::Increment::host(512 * 1024 * 1024),
            cadence_secs: Some(230),
            // Room for any listing: this fixture is the ARM door's, and a
            // listing door biting here would be answering a different
            // question from the one under test.
            loop_frames_allowed: usize::MAX,
            loop_frame_reserve_bytes: 80 * 1024 * 1024,
            ..Default::default()
        }],
        new_pane: squallar_device_profile::admit::Increment::ZERO,
        layer_grids: Vec::new(),
        frames: squallar_device_profile::admit::LoopFrames {
            render_budget: 14,
            reachable: 14,
        },
        requested_percent: (50, 50),
    }
}

/// **A refused loop is not lost for the session — the user lowers their ask
/// and gets it on the next table.**
///
/// This is the behaviour a refusal took away, and it is one layer above the
/// queue: asserting only that the entry was re-parked would pass on a build
/// where nothing ever drained it again. So the whole gesture is driven — the
/// refusal, the user freeing room, the App's next telemetry tick — and what
/// is asserted at the end is that the loop is **armed**.
///
/// Until 2026-09-07 the refusal returned without re-queueing and
/// `hydrate_parked_panes` had already taken the queue by `mem::take`, so the
/// only way to ask a second time was to restart the app. The wish is
/// persisted, so that was not one session: it was every session.
#[test]
fn a_refused_loop_arms_once_the_user_makes_room_for_it() {
    let end = scan_stamp();
    let listed: Vec<_> = [8i64, 4, 0]
        .iter()
        .map(|m| end - chrono::Duration::minutes(*m))
        .collect();
    let mut app = app_restored_wanting_a_loop(listed, true);
    // The scan the loop anchors on has landed, so nothing but the door can
    // stop the arm: this test must not pass on `TransportNotReady`.
    {
        let pane = app.gui.pane_mut(0).expect("the fixture built one pane");
        pane.scan_info = Some(scan_info_for("KTLX"));
        // What `Gui::load_ui_config` writes on the pane itself, which the
        // shared fixture above does not: the restore parks the wish in BOTH
        // of its homes, and half of the property under test is that a
        // refusal spends neither.
        pane.loop_arm_pending = Some(LoopArm { playing: true });
    }

    // A table with no room, and an arm priced well past it.
    app.admission.adopt(&admission_table(1, 8 * 1024 * 1024));
    app.hydrate_parked_panes();

    assert_eq!(
        app.gui
            .pane(0)
            .expect("the fixture built one pane")
            .time_state(&known::RADAR)
            .phase,
        LoopPhase::Inactive,
        "precondition: the door must actually have refused this arm",
    );
    assert!(
        !app.loop_arm_pending.is_empty(),
        "a refused loop that leaves the retry queue is a loop the user \
         cannot ask for again without restarting the app",
    );
    assert_eq!(
        app.gui
            .pane(0)
            .expect("the fixture built one pane")
            .loop_arm_pending,
        Some(LoopArm { playing: true }),
        "and the pane keeps the wish, so a save during the wait still writes \
         the loop back",
    );
    assert!(
        app.admission
            .notice(web_time::Instant::now())
            .is_some_and(|notice| notice.text.contains("MB")),
        "a refusal the user cannot see is worse than the allocation it \
         prevented",
    );

    // **Redraws are not verdicts.** Twenty more hydrate passes against the
    // same table must not re-take the decision - nothing that could change
    // it has happened. This is the ledger's `(act, pane)` memo doing the
    // work: the door is still entered every pass, and answers from the memo
    // without logging, re-stamping the notice or counting a second time.
    let after_first = app.admission.counts();
    for _ in 0..20 {
        app.hydrate_parked_panes();
    }
    assert_eq!(
        app.admission.counts(),
        after_first,
        "the door was re-asked by the redraw rate rather than by the scene",
    );
    assert!(!app.loop_arm_pending.is_empty(), "and it is still queued");

    // The user turns off a layer; the next telemetry tick publishes the room.
    app.admission.adopt(&admission_table(2, 1024 * 1024 * 1024));
    app.hydrate_parked_panes();

    let pane = app.gui.pane(0).expect("the fixture built one pane");
    assert_eq!(
        pane.time_state(&known::RADAR).phase,
        LoopPhase::FetchingScanList,
        "the loop the user re-asked for must arm - this is the function a \
         refusal used to take for the rest of the session",
    );
    assert!(
        pane.time_state(&known::RADAR).autoplay_on_ready,
        "and a loop persisted PLAYING must still be asking to play",
    );
    assert!(
        app.loop_arm_pending.is_empty(),
        "an armed loop leaves the retry queue",
    );
}

/// A table for the LISTING door: no cadence yet, so the arm is admitted
/// provisionally, and `allowed` frames of room when the listing lands.
fn listing_table(allowed: usize) -> squallar_egui::admission::AdmissionCosts {
    let mut table = admission_table(1, u64::MAX);
    table.panes[0].cadence_secs = None;
    table.panes[0].loop_frames_allowed = allowed;
    table
}

/// The scene both listing-door tests drive: a pane restored wanting a loop,
/// its anchor scan already landed, and a table that allows `allowed` frames.
///
/// Returns the app after the listing has been delivered, so each test reads
/// the outcome rather than re-deriving the setup.
fn app_after_a_listing(listed: &[chrono::NaiveDateTime], allowed: usize) -> crate::app::App {
    let mut app = app_restored_wanting_a_loop(listed.to_vec(), true);
    app.gui
        .pane_mut(0)
        .expect("the fixture built one pane")
        .scan_info = Some(scan_info_for("KTLX"));
    app.admission.adopt(&listing_table(allowed));
    app.hydrate_parked_panes();
    assert_eq!(
        app.gui
            .pane(0)
            .expect("the fixture built one pane")
            .time_state(&known::RADAR)
            .phase,
        LoopPhase::FetchingScanList,
        "precondition: the arm must be admitted provisionally with no \
         cadence known, or the listing door is never reached",
    );
    deliver_radar_listing(&mut app, listed.to_vec());
    app
}

/// How many frames a pane's radar loop ended up holding.
fn frames_held(app: &crate::app::App) -> usize {
    app.gui
        .pane(0)
        .expect("the fixture built one pane")
        .time_state(&known::RADAR)
        .frames
        .len()
}

/// The stamps the stub lists. What the loop actually holds is fewer — the
/// arm's own lookback window decides that — which is why every count below is
/// **read off the admitting run** rather than written here.
fn listed_stamps() -> Vec<chrono::NaiveDateTime> {
    let end = scan_stamp();
    (0..6)
        .map(|m| end - chrono::Duration::minutes(m * 4))
        .collect()
}

/// **The listing door, end to end: refused, and not one volume is fetched.**
///
/// The whole mechanism driven the way production drives it. The loop arms
/// with no cadence known — so the arm door admits provisionally, because the
/// only frame count available to it there is the render budget's ceiling and
/// refusing on that turned away loops that would have fitted. The listing
/// lands, `build_loop_frames` records the site's cadence off it, and the whole
/// loop is priced for the first time. It does not fit, so the pane leaves loop
/// mode **before** a `FramePlan` reaches the download manager.
///
/// `plan_frame_count` is the assertion that matters: a door that refused
/// after the downloads were queued would prevent nothing, and the 1 GiB wall
/// the Tier-2 `long` leg hit is reached by fetching volumes, not by planning
/// to. It is deliberately **not** `pending_queue_count` — see the comment at
/// the assertion for why that spelling could not have failed.
#[test]
fn a_listing_too_big_to_hold_is_refused_before_a_single_volume_is_fetched() {
    let listed = listed_stamps();
    let app = app_after_a_listing(&listed, 0);

    let ls = app
        .gui
        .pane(0)
        .expect("the fixture built one pane")
        .time_state(&known::RADAR);
    assert_eq!(
        ls.phase,
        LoopPhase::Inactive,
        "a loop this session cannot hold must leave loop mode at its listing",
    );
    assert!(ls.frames.is_empty(), "and hold no frames");
    // **AND HAND THE DOWNLOAD MANAGER NOTHING** - a door that refuses after
    // the volumes are on the wire prevents nothing at all, and the 1 GiB wall
    // the Tier-2 `long` leg hit is reached by fetching volumes rather than by
    // planning to.
    //
    // `plan_frame_count` and not `pending_queue_count`, and the difference is
    // the whole assertion. The pending QUEUE is "queued and undispatched":
    // `accept_loop_scan_listings` calls `dispatch_pending_loop_downloads`
    // immediately after `set_plan`, which drains it, so it reads zero on the
    // admitting path too - a refusal asserted against it would have passed
    // whatever this door did. The PLAN is written by `set_plan` and cleared
    // only by an explicit `remove_pending`, so it is what actually separates
    // "the manager was given this loop" from "it never heard of it".
    assert_eq!(
        app.loop_mgr.plan_frame_count(0),
        0,
        "a refused listing must never reach the download manager",
    );
    let notice = app
        .admission
        .notice(web_time::Instant::now())
        .expect("the reader must be told")
        .text
        .clone();
    assert!(
        notice.contains("this loop"),
        "the notice must name the act that was refused: {notice}",
    );
    // **Both actions, because nobody retries this one for them.** The lever
    // alone would be a trap: the user lowers the lookback, nothing happens,
    // and they have done exactly what the glass told them to. Asserted beside
    // the no-retry behaviour below so the two cannot drift - the day an
    // automatic re-drive lands, this is what says to drop the clause.
    assert!(
        notice.contains("Then turn the loop back on."),
        "a refusal nobody retries must name the second action: {notice}",
    );
    assert_eq!(app.admission.counts().refused, 1);

    // **And it stays refused without re-listing.** A refusal at the listing
    // spends the pane's parked wish - the arm it followed succeeded - so
    // nothing re-drives it, and that is deliberate: a re-park here would send
    // `handle_enable_loop` back through `begin_loop_for_pane` on every
    // redraw, and each pass would put a fresh frame listing ON THE NETWORK.
    // The user re-enables the loop themselves, having been told what to
    // change. Unlike the arm door's refusal, this one is not retried for
    // them.
    let mut app = app;
    let settled = app.admission.counts();
    for _ in 0..20 {
        app.hydrate_parked_panes();
    }
    assert_eq!(
        app.admission.counts(),
        settled,
        "twenty redraws after a refused listing must take no further \
         verdicts",
    );
    assert_eq!(
        app.gui
            .pane(0)
            .expect("the fixture built one pane")
            .time_state(&known::RADAR)
            .phase,
        LoopPhase::Inactive,
        "and the loop must not creep back on by itself",
    );
    assert_eq!(app.loop_mgr.plan_frame_count(0), 0);
}

/// **The admit arm, and the boundary between them — both counts read off the
/// run rather than written here.**
///
/// Over-firing is the worse direction, so the admitting row is what says the
/// refusal above happened for the reason it claims and not because this path
/// refuses everything. And an admit with frames to spare would pass on a door
/// biased by one, or by three, or on one that never refuses at all — so the
/// count the loop actually holds is measured on the roomy run and then used
/// as the boundary: exactly enough admits, one fewer refuses.
#[test]
fn the_same_listing_arms_where_there_is_room_and_the_door_resolves_one_frame() {
    let listed = listed_stamps();

    // Room for anything: what does this scene's loop actually hold?
    let roomy = app_after_a_listing(&listed, usize::MAX);
    let held = frames_held(&roomy);
    assert!(
        held >= 2,
        "control: the listing must produce frames for a door to refuse, not \
         an empty loop that would read as a refusal",
    );
    assert_ne!(
        roomy
            .gui
            .pane(0)
            .expect("the fixture built one pane")
            .time_state(&known::RADAR)
            .phase,
        LoopPhase::Inactive,
        "a loop that fits must not be turned away",
    );
    assert_eq!(
        roomy.loop_mgr.plan_frame_count(0),
        held,
        "and must have handed the download manager the frames it means to \
         draw - exactly the ones the door was asked about",
    );
    assert_eq!(roomy.admission.counts().refused, 0);
    assert!(roomy.admission.notice(web_time::Instant::now()).is_none());

    // Exactly enough: still admitted. The boundary belongs on the admitting
    // side, or the door costs a frame nobody spent.
    let exact = app_after_a_listing(&listed, held);
    assert_eq!(frames_held(&exact), held, "room for exactly what it holds");
    assert_eq!(exact.loop_mgr.plan_frame_count(0), held);
    assert_eq!(exact.admission.counts().refused, 0);

    // One fewer: refused. This is the assertion that earns the two above.
    let tight = app_after_a_listing(&listed, held - 1);
    assert_eq!(
        tight
            .gui
            .pane(0)
            .expect("the fixture built one pane")
            .time_state(&known::RADAR)
            .phase,
        LoopPhase::Inactive,
        "room for one frame fewer than the loop holds must refuse it, or \
         this door cannot see a one-frame difference and its admits prove \
         nothing",
    );
    assert_eq!(tight.loop_mgr.plan_frame_count(0), 0);
    assert_eq!(tight.admission.counts().refused, 1);
}

/// Put a RADAR listing on the one arrival path and drain it — the same two
/// calls the frame pump makes, over the window the pane's arm recorded.
///
/// The scope is `RadarListing`, which is what makes this the radar arm of
/// `accept_loop_scan_listings` rather than the layer-agnostic one: only that
/// arm reaches `accept_scan_listing`, and only that arm has volumes to fetch.
fn deliver_radar_listing(app: &mut crate::app::App, listed: Vec<chrono::NaiveDateTime>) {
    let range = app
        .gui
        .pane(0)
        .expect("the fixture built one pane")
        .time_state(&known::RADAR)
        .asked_range
        .expect("an armed loop recorded its ask");
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
}
