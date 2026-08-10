use super::tests::{empty_scan, headless};
use super::*;
use crate::platform_double::TestBridge;

fn at(minute: u32) -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 7, 28)
        .unwrap()
        .and_hms_opt(18, minute, 0)
        .unwrap()
}

/// A live pane on KTLX already showing a volume assembled at `shown`.
fn app_showing(shown: chrono::NaiveDateTime) -> App {
    let mut app = headless(TestBridge::desktop());
    {
        let pane = app.gui.pane_mut(0).unwrap();
        pane.site = "KTLX".to_string();
        pane.viewing_live = true;
        pane.scan_info = Some(rustdar_radar::types::ScanInfo {
            site: rustdar_radar::sites::RadarSite {
                name: "KTLX",
                lat: 35.3,
                lon: -97.3,
                heights: None,
            },
            timestamp: shown,
            vcp_number: 212,
            available_products: Vec::new(),
            product_elevations: Default::default(),
            status: String::new(),
        });
    }
    app
}

fn send_archive(app: &App, timestamp: chrono::NaiveDateTime) {
    send_archive_scan(app, timestamp, empty_scan());
}

/// [`send_archive`] with a caller-chosen scan, for assertions that need
/// the volume to carry a radial — a stamp only resolves off real data.
fn send_archive_scan(app: &App, timestamp: chrono::NaiveDateTime, scan: nexrad_model::data::Scan) {
    let generation = app.render.fetch_generation_for("KTLX");
    app.channels
        .scan_sender
        .send(crate::channels::ScanResponse {
            generation,
            site: "KTLX".to_string(),
            result: Ok(crate::channels::ScanData {
                scan,
                declared_nyquist: Default::default(),
                site: "KTLX".to_string(),
                timestamp,
            }),
            is_auto_poll: false,
        })
        .unwrap();
}

/// The same archive volume, arriving from the auto-poll rather than from a
/// Refresh — which is the other arm that declines to put it on screen.
fn send_auto_poll_archive(app: &App, timestamp: chrono::NaiveDateTime) {
    let generation = app.render.fetch_generation_for("KTLX");
    app.channels
        .scan_sender
        .send(crate::channels::ScanResponse {
            generation,
            site: "KTLX".to_string(),
            result: Ok(crate::channels::ScanData {
                scan: empty_scan(),
                declared_nyquist: Default::default(),
                site: "KTLX".to_string(),
                timestamp,
            }),
            is_auto_poll: true,
        })
        .unwrap();
}

/// The bug this closes: pressing Refresh while the real-time feed was ahead
/// reverted the display to the previous archive volume.
///
/// The archive publishes a volume only once every cut is finished, so what a
/// Refresh returns while a feed is running is by construction the volume
/// *before* the one being assembled — several minutes older than what is on
/// screen.
#[test]
fn an_archive_volume_older_than_the_feed_does_not_replace_it() {
    let mut app = app_showing(at(10));
    app.chunk_feeds.ensure("KTLX");
    assert!(app.chunks_are_feeding("KTLX"), "precondition: feed running");

    send_archive(&app, at(5));
    app.poll_data_channels();

    assert_eq!(
        app.gui
            .pane(0)
            .unwrap()
            .scan_info
            .as_ref()
            .unwrap()
            .timestamp,
        at(10),
        "Refresh walked the display back to the previous archive volume"
    );
    assert!(
        !app.scan_data.contains_key("KTLX"),
        "and it replaced the volume the panes render from"
    );
}

/// The wait still has to end. A Refresh raises `fetching`, and
/// `check_auto_polls` refuses to poll while it is set, so a skipped apply
/// that left it up would wedge the archive poll behind a spinner that
/// nothing takes down.
#[test]
fn a_skipped_archive_volume_still_ends_the_wait_it_belonged_to() {
    let mut app = app_showing(at(10));
    app.chunk_feeds.ensure("KTLX");
    app.gui.set_fetching(true);
    app.gui.pane_mut(0).unwrap().loading_site = Some("KTLX".to_string());

    send_archive(&app, at(5));
    app.poll_data_channels();

    assert!(!app.gui.fetching(), "the spinner was left up");
    assert!(
        app.gui.pane(0).unwrap().loading_site.is_none(),
        "and the pane's loading marker with it"
    );
}

/// The counterweight: a genuinely newer archive volume is still applied, or
/// the guard would freeze the display whenever a feed existed.
#[test]
fn an_archive_volume_newer_than_the_feed_is_applied() {
    let mut app = app_showing(at(10));
    app.chunk_feeds.ensure("KTLX");

    send_archive(&app, at(15));
    app.poll_data_channels();

    assert!(
        app.scan_data.contains_key("KTLX"),
        "a newer archive volume was refused"
    );
}

/// And with no feed running the archive is authoritative, which is what the
/// fallback depends on — a retired feed leaves the site here.
#[test]
fn without_a_feed_the_archive_is_applied_unconditionally() {
    let mut app = app_showing(at(10));
    assert!(!app.chunks_are_feeding("KTLX"));

    send_archive(&app, at(5));
    app.poll_data_channels();

    assert!(
        app.scan_data.contains_key("KTLX"),
        "the fallback cannot restore a site if an older archive volume is \
             refused when no feed is running"
    );
}

/// **The overlay dies with the setting.** With live chunks toggled off,
/// `drive_chunk_feeds` returns before `retain_live`, so the feed map kept
/// its last assembler for the session — and no consumer of the merged
/// current volume gates on the setting, so the frozen partial overlay
/// went on standing over a base the archive polls keep rolling forward.
#[test]
fn turning_live_chunks_off_stops_the_overlay_from_standing() {
    let mut app = app_showing(at(10));
    // No sockets from a unit test: the notification driver runs ahead of
    // the enabled gate and would otherwise open real connections.
    app.gui.set_chunk_notifications(false);
    app.chunk_feeds.ensure("KTLX");
    app.chunk_feeds
        .force_serving("KTLX", Arc::new(empty_scan()));
    assert!(
        app.chunk_feeds.snapshot("KTLX").is_some(),
        "precondition: the feed is serving an overlay",
    );

    app.gui.set_live_chunks(false);
    app.drive_chunk_feeds();

    assert!(
        app.chunk_feeds.snapshot("KTLX").is_none(),
        "the setting went off and the last assembler kept serving its \
             frozen overlay to every consumer of the merged current volume",
    );
}

/// A picked region decides the ground that is resampled; without one, the
/// default box about the site does.
///
/// Both halves, because the two failure modes are opposite and both silent.
/// A region ignored resamples the default box, which looks exactly like a
/// region that was never committed. A default applied when a region was
/// picked is the same thing seen from the other side.
#[test]
fn a_picked_region_decides_the_ground_that_is_resampled() {
    use rustdar_egui::pane::{GeoPoint, VolumeRegion, VolumeStamp, VolumeTarget};

    let target = |region| VolumeTarget {
        volume: VolumeStamp {
            site: "KTLX".to_owned(),
            collected: chrono::NaiveDate::from_ymd_opt(2026, 7, 30)
                .expect("a real date")
                .and_hms_opt(22, 33, 0)
                .expect("a real time"),
        },
        product: rustdar_radar::types::RadarProduct::Reflectivity,
        region,
    };

    let default = voxel_request_for(&target(None), 35.33, -97.28);
    assert_eq!(default.centre, (35.33, -97.28), "no region means the site");
    assert_eq!(default.half_width_km, VOLUME_HALF_WIDTH_KM);

    let picked = VolumeRegion::new(
        GeoPoint {
            lat: 36.1,
            lon: -98.4,
        },
        22.5,
    )
    .expect("a valid region");
    let aimed = voxel_request_for(&target(Some(picked)), 35.33, -97.28);
    assert_eq!(
        aimed.centre,
        (36.1, -98.4),
        "a picked region must move the box off the site",
    );
    assert_eq!(aimed.half_width_km, 22.5);
}

/// The vertical extent is not part of the region pick.
///
/// It is a separate axis by design — the region changes what is sampled over
/// the ground, the exaggeration changes only how it is drawn — and a region
/// drag that also re-cut the column would silently change what heights the
/// pane reports.
#[test]
fn a_region_pick_does_not_move_the_top_or_the_bottom_of_the_box() {
    use rustdar_egui::pane::{GeoPoint, VolumeRegion, VolumeStamp, VolumeTarget};

    let make = |region| VolumeTarget {
        volume: VolumeStamp {
            site: "KTLX".to_owned(),
            collected: chrono::NaiveDate::from_ymd_opt(2026, 7, 30)
                .expect("a real date")
                .and_hms_opt(22, 33, 0)
                .expect("a real time"),
        },
        product: rustdar_radar::types::RadarProduct::Reflectivity,
        region,
    };
    let picked = VolumeRegion::new(
        GeoPoint {
            lat: 36.1,
            lon: -98.4,
        },
        15.0,
    );

    for target in [make(None), make(picked)] {
        let request = voxel_request_for(&target, 35.33, -97.28);
        assert_eq!(
            request.base_km_msl,
            rustdar_radar::voxel::DEFAULT_BASE_KM_MSL
        );
        assert_eq!(request.top_km_msl, rustdar_radar::voxel::DEFAULT_TOP_KM_MSL);
    }
}

/// The pane and the resampler agree about how big the default box is.
///
/// They have to: the pane does its own camera arithmetic against the box it
/// believes it has — the pan scale and the pivot are both fractions of it —
/// and a disagreement would show up as a pan that drifts against the picture,
/// which is the kind of thing that gets "fixed" by tuning a sensitivity.
#[test]
fn the_pane_and_the_resampler_agree_about_the_default_box() {
    assert_eq!(
        VOLUME_HALF_WIDTH_KM,
        rustdar_egui::pane::DEFAULT_HALF_WIDTH_KM,
    );
    let pane = rustdar_egui::pane::VolumePane::default();
    assert_eq!(
        pane.box_size_km(),
        [
            (2.0 * VOLUME_HALF_WIDTH_KM) as f32,
            (2.0 * VOLUME_HALF_WIDTH_KM) as f32,
            (rustdar_radar::voxel::DEFAULT_TOP_KM_MSL - rustdar_radar::voxel::DEFAULT_BASE_KM_MSL)
                as f32,
        ],
    );
}

/// **The 3D build reads `base_scans` and never `scan_data`.**
///
/// The completeness decision, stated as the behaviour a user gets: what
/// reaches the resampler must be a volume whose every flown cut sealed —
/// an archive decode or a whole closed chunk volume — and never whatever
/// partial snapshot the map panes happen to be drawing mid-volume. Reading
/// `scan_data` instead works, and silently: mid-volume it is the live
/// snapshot, so the grid would be built from however many cuts had sealed
/// by that frame, a plausible short volume with nothing to notice. (The
/// archive-only half of the old rule is gone on purpose — see
/// `base_scans` — but the partial-volume half is the one that was always
/// load-bearing, and it is what this pins.)
///
/// An empty scan cannot be resampled, so the store's answer here is a
/// `Refused` entry rather than a grid. That is the right discriminator
/// anyway: what is under test is whether the build was *reached*, and the
/// arm that finds no base volume deliberately stores nothing at all so
/// that the pane goes on asking.
/// A one-sweep volume whose single radial carries reflectivity and a real
/// collection stamp at `minute` — the smallest scan whose current-volume
/// stamp resolves, so a build can actually be dispatched against it.
fn stamped_scan(minute: u32) -> nexrad_model::data::Scan {
    use nexrad_model::data::{
        ChannelConfiguration, ElevationCut, MomentData, PulseWidth, Radial, RadialStatus, Scan,
        Sweep, VolumeCoveragePattern, WaveformType,
    };
    let stamp_ms = at(minute).and_utc().timestamp_millis();
    let radial = Radial::new(
        stamp_ms,
        1,
        0.0,
        1.0,
        RadialStatus::IntermediateRadialData,
        1,
        0.5,
        Some(MomentData::from_fixed_point(
            2,
            2125,
            250,
            8,
            2.0,
            66.0,
            vec![100, 120],
        )),
        None,
        None,
        None,
        None,
        None,
        None,
    );
    Scan::new(
        VolumeCoveragePattern::new(
            212,
            0,
            0.5,
            PulseWidth::Short,
            false,
            0,
            false,
            0,
            false,
            false,
            0,
            false,
            false,
            vec![ElevationCut::new(
                0.5,
                ChannelConfiguration::ConstantPhase,
                WaveformType::CS,
                20.0,
                true,
                true,
                false,
                false,
                1,
                20,
                0.0,
                0.0,
                0.0,
                0.0,
                0.0,
                0.0,
                false,
                0,
                false,
                0,
                false,
                false,
            )],
        ),
        vec![Sweep::new(1, vec![radial])],
    )
}

#[test]
fn the_3d_build_reads_the_base_volume_and_not_the_live_snapshot() {
    let target = rustdar_egui::pane::VolumeTarget {
        volume: rustdar_egui::pane::VolumeStamp {
            site: "KTLX".to_owned(),
            collected: at(10),
        },
        product: rustdar_radar::types::RadarProduct::Reflectivity,
        region: None,
    };

    // The volume the map panes are drawing, and nothing else. `scan_data`
    // is deliberately never consulted by the stamp or the extraction, so
    // no build can be reached from it.
    let mut live_only = headless(TestBridge::desktop());
    live_only
        .scan_data
        .insert("KTLX".to_string(), Arc::new(stamped_scan(10)));
    live_only.handle_prepare_volume(0, target.clone());
    assert!(
        live_only.volume_store.lookup(&target).is_none(),
        "a volume only the map panes hold was handed to the resampler",
    );

    // The same volume, arrived as the site's base. The build is reached:
    // a `Building` entry opens at dispatch, which is all a headless test
    // can — and need — observe.
    let mut based = headless(TestBridge::desktop());
    based.base_scans.insert(
        "KTLX".to_string(),
        (Arc::new(stamped_scan(10)), Default::default(), at(10)),
    );
    based.handle_prepare_volume(0, target.clone());
    assert!(
        based.volume_store.lookup(&target).is_some(),
        "the 3D pane was offered a base volume and the build never \
             reached it, so the pane waits for ever",
    );
}

/// **A budget-refused frame pays nothing for the refusal.** The voxel
/// path used to run `extract_current_volume` — the full merged-volume
/// walk and copy, multi-ms on the frame thread — and *then* ask
/// `spawn_voxel_build`, which refuses on a full budget with nothing
/// marked. `PrepareVolume` is level-triggered, so the pane re-asked and
/// the extraction repeated every frame until a slot freed — on wasm,
/// where the budget is 1, any in-flight render made a pending 3D rebuild
/// a per-frame multi-ms stall. The section path's shape is the model:
/// the budget gate runs before the extraction closure, so the walk is
/// paid exactly when a slot is actually taken.
#[test]
fn a_full_budget_refuses_the_3d_ask_before_paying_the_extraction() {
    use crate::constants::MAX_CONCURRENT_RENDERS;
    use std::sync::atomic::Ordering;

    let target = rustdar_egui::pane::VolumeTarget {
        volume: rustdar_egui::pane::VolumeStamp {
            site: "KTLX".to_owned(),
            collected: at(10),
        },
        product: rustdar_radar::types::RadarProduct::Reflectivity,
        region: None,
    };
    let mut app = headless(TestBridge::desktop());
    app.base_scans.insert(
        "KTLX".to_string(),
        (Arc::new(stamped_scan(10)), Default::default(), at(10)),
    );

    // Every slot is taken. Several frames of the level-triggered ask
    // arrive, as they do while a render is in flight.
    app.render
        .renders_in_flight
        .store(MAX_CONCURRENT_RENDERS, Ordering::Relaxed);
    for _ in 0..3 {
        app.handle_prepare_volume(0, target.clone());
    }
    assert_eq!(
        app.volume_extractions.get(),
        0,
        "a budget-refused frame paid the multi-ms merged-volume walk, and \
             the level-triggered pane repeats it every frame until a slot frees",
    );
    assert!(
        app.volume_store.lookup(&target).is_none(),
        "the ask must stay pending: nothing dispatched and nothing marked",
    );

    // A slot frees: exactly one extraction, and the build dispatches.
    app.render.renders_in_flight.store(0, Ordering::Relaxed);
    app.handle_prepare_volume(0, target.clone());
    assert_eq!(
        app.volume_extractions.get(),
        1,
        "the freed slot performs exactly one extraction",
    );
    assert!(
        app.volume_store.lookup(&target).is_some(),
        "the freed slot dispatches the build",
    );

    // And the next frame attaches to the `Building` entry rather than
    // extracting again — the dedupe gate stays ahead of the walk.
    app.handle_prepare_volume(0, target.clone());
    assert_eq!(
        app.volume_extractions.get(),
        1,
        "the level-triggered re-ask must attach, not re-extract",
    );
}

/// A pane is handed the volume it named, or none.
///
/// A target names one volume — the published stamp. Matching on the site
/// alone would hand a pane that asked for the 18:10 data the 18:15 build
/// the moment the next sweep sealed — and `mark_volume_rendered` would
/// then record that it had built the one it asked for, so the
/// substitution is invisible from every direction. Refusing instead is
/// self-healing: the pane re-asks next frame with the current stamp.
#[test]
fn a_3d_pane_is_not_handed_a_volume_other_than_the_one_it_asked_for() {
    let target = rustdar_egui::pane::VolumeTarget {
        volume: rustdar_egui::pane::VolumeStamp {
            site: "KTLX".to_owned(),
            collected: at(10),
        },
        product: rustdar_radar::types::RadarProduct::Reflectivity,
        region: None,
    };

    let mut app = headless(TestBridge::desktop());
    app.base_scans.insert(
        "KTLX".to_string(),
        (Arc::new(stamped_scan(15)), Default::default(), at(15)),
    );
    app.handle_prepare_volume(0, target.clone());

    assert!(
        app.volume_store.lookup(&target).is_none(),
        "the pane asked for the 18:10 volume and was built the 18:15 one",
    );
}

/// **Every archive path offers its volume to the 3D pane**, including the
/// two that decline to display it.
///
/// Recording inside the display branch instead of above it leaves a 3D pane
/// on a live-fed site waiting for ever: a site with a feed running takes the
/// `feed_is_ahead` arm on every poll, so the complete volume the app already
/// holds would simply never be offered. Each arm is entered through the same
/// door a user does, and each asserts its own precondition so that a test
/// which stopped reaching its arm fails rather than passes vacuously.
#[test]
fn every_archive_path_offers_its_volume_to_the_3d_pane() {
    let collected = |app: &App| app.base_scans.get("KTLX").map(|(_, _, at)| *at);

    // 1. The arm that displays it.
    let mut shown = app_showing(at(10));
    send_archive(&shown, at(15));
    shown.poll_data_channels();
    assert!(
        shown.scan_data.contains_key("KTLX"),
        "precondition: this is the arm that puts the volume on screen",
    );
    assert_eq!(collected(&shown), Some(at(15)));

    // 2. The arm that keeps the real-time volume on screen instead.
    let mut behind = app_showing(at(10));
    behind.chunk_feeds.ensure("KTLX");
    send_archive(&behind, at(5));
    behind.poll_data_channels();
    assert!(
        !behind.scan_data.contains_key("KTLX"),
        "precondition: this is the `feed_is_ahead` arm",
    );
    assert_eq!(
        collected(&behind),
        Some(at(5)),
        "a site with a feed running takes this arm on every poll, so a 3D \
             pane on it would never be offered a volume at all",
    );

    // 3. The arm that caches silently for a pane that is not viewing live.
    let mut historic = app_showing(at(10));
    historic.gui.pane_mut(0).unwrap().viewing_live = false;
    send_auto_poll_archive(&historic, at(15));
    historic.poll_data_channels();
    assert!(
        !historic.scan_data.contains_key("KTLX"),
        "precondition: this is the auto-poll-while-historic arm",
    );
    assert_eq!(collected(&historic), Some(at(15)));
}

/// **A Refresh in the pre-publication window must not walk the base back.**
/// The feed's whole closed volumes roll `base_scans` forward at volume
/// end, up to ~7 minutes before the archive publishes the same volume —
/// so in that window a manual Refresh returns the volume *before* the one
/// already based, and the drain's unconditional insert put the older
/// ladder back under every whole-volume consumer.
///
/// The guard is scoped to the feed-ahead window on purpose, and the third
/// phase is the boundary: with no feed ahead, the base still follows the
/// display backwards, because a historic navigation re-bases the substrate
/// on the volume shown — a section pane stamps its target with the pane's
/// own time while cutting from `base_scans`, so a base pinned newer than
/// the display would cut newer data under the navigated caption.
#[test]
fn a_refresh_in_the_pre_publication_window_does_not_walk_the_base_back() {
    let based = |app: &App| app.base_scans.get("KTLX").map(|(_, _, at)| *at);
    let mut app = app_showing(at(10));
    app.chunk_feeds.ensure("KTLX");
    // The feed's whole closed volume is already the merge base.
    app.base_scans.insert(
        "KTLX".to_string(),
        (Arc::new(stamped_scan(10)), Default::default(), at(10)),
    );

    // A manual Refresh in the window: the archive answers the previous
    // volume.
    send_archive(&app, at(5));
    app.poll_data_channels();
    assert_eq!(
        based(&app),
        Some(at(10)),
        "a manual Refresh in the pre-publication window regressed the \
             merge base one volume, under every whole-volume consumer",
    );

    // The counterweight: a genuinely newer archive volume still advances
    // it.
    send_archive(&app, at(15));
    app.poll_data_channels();
    assert_eq!(based(&app), Some(at(15)), "a newer volume was refused");

    // And with the feed no longer ahead, the base follows the display —
    // backwards included.
    app.chunk_feeds
        .force_retire_at("KTLX", std::time::Duration::from_secs(1));
    assert!(
        !app.chunks_are_feeding("KTLX"),
        "precondition: feed retired"
    );
    send_archive(&app, at(12));
    app.poll_data_channels();
    assert_eq!(
        based(&app),
        Some(at(12)),
        "with no feed ahead the base must follow the volume on display, \
             or a navigated section cuts newer data under an older caption",
    );
}

/// And the recorded volume reaches the pane that has to name it.
///
/// The decoded `Scan` stays in the frontend; `rustdar-egui` is told only
/// the *stamp*, and a 3D pane asks for a volume by it. So a recording that
/// is never published is a pane that never asks — the same silent wait as
/// never recording, one layer further out.
#[test]
fn the_recorded_base_volume_is_published_to_the_pane_that_names_it() {
    let mut app = app_showing(at(10));
    app.chunk_feeds.ensure("KTLX");
    assert_eq!(
        app.gui.current_volume_for("KTLX"),
        None,
        "precondition: nothing published yet",
    );

    send_archive_scan(&app, at(5), stamped_scan(5));
    app.poll_data_channels();

    let stamp = app
        .gui
        .current_volume_for("KTLX")
        .expect("the app holds a base volume the 3D pane is never told about");
    assert_eq!(
        stamp.newest,
        at(5),
        "the stamp must be the volume's own newest data time",
    );
    assert_eq!(
        stamp.base_started,
        Some(at(5)),
        "a pure base volume names itself as the base",
    );
}

// ── Manual navigation outranks the feed guard (M10) ──────────────────

/// A pane on `site` at `shown`, beside [`app_showing`]'s pane 0 — the state
/// a second linked-off or unlinked pane is in while its sibling navigates.
fn add_live_pane(app: &mut App, shown: chrono::NaiveDateTime) {
    let mut two = super::tests::two_pane_app("KTLX", "KTLX");
    std::mem::swap(&mut app.gui, &mut two.gui);
    for idx in [0, 1] {
        let pane = app.gui.pane_mut(idx).unwrap();
        pane.viewing_live = true;
        pane.scan_info = Some(rustdar_radar::types::ScanInfo {
            site: rustdar_radar::sites::RadarSite {
                name: "KTLX",
                lat: 35.3,
                lon: -97.3,
                heights: None,
            },
            timestamp: shown,
            vcp_number: 212,
            available_products: Vec::new(),
            product_elevations: Default::default(),
            status: String::new(),
        });
    }
    app.render.ensure_pane_count(2);
}

fn shown_stamp(app: &App) -> chrono::NaiveDateTime {
    app.gui
        .pane(0)
        .unwrap()
        .scan_info
        .as_ref()
        .unwrap()
        .timestamp
}

/// The M10 "time controls are inert" root cause, pinned at its site: the
/// feed guard read a manual navigation's answer as a stale "latest" and
/// threw it away. Two panes on one site, the second still live so the feed
/// never retires — Back's archive volume must still land.
#[test]
fn a_manual_navigation_outranks_the_feed_guard() {
    let mut app = app_showing(at(10));
    add_live_pane(&mut app, at(10));
    app.chunk_feeds.ensure("KTLX");
    assert!(app.chunks_are_feeding("KTLX"), "precondition: feed running");

    app.handle_gui_action(
        rustdar_egui::actions::GuiAction::NavigateTime {
            pane_idx: 0,
            step_secs: -600,
        },
        None,
    );
    assert!(
        app.manual_nav_pending,
        "precondition: the navigation marked itself pending"
    );
    send_archive(&app, at(0));
    app.poll_data_channels();

    assert_eq!(
        shown_stamp(&app),
        at(0),
        "the feed guard swallowed a manual navigation's volume - the \
         transport's Back is inert again"
    );
    assert!(
        !app.manual_nav_pending,
        "the applied navigation must clear its pending flag"
    );
    assert!(
        app.scan_data.contains_key("KTLX"),
        "the navigated volume must become the site's displayed scan"
    );
}

/// The single-pane race arm of the same break: the response drains on the
/// very frame the click was processed, before `drive_chunk_feeds` has had
/// a frame to retire the now-parked site's feed. "No live pane on the
/// site" must already disarm the guard.
#[test]
fn a_navigation_response_on_a_parked_site_applies_even_mid_retire() {
    let mut app = app_showing(at(10));
    app.chunk_feeds.ensure("KTLX");
    assert!(app.chunks_are_feeding("KTLX"), "precondition: feed running");

    app.handle_gui_action(
        rustdar_egui::actions::GuiAction::NavigateTime {
            pane_idx: 0,
            step_secs: -600,
        },
        None,
    );
    assert!(
        app.chunks_are_feeding("KTLX"),
        "precondition: the feed has not yet retired - this is the race"
    );
    send_archive(&app, at(0));
    app.poll_data_channels();

    assert_eq!(
        shown_stamp(&app),
        at(0),
        "a navigation on a parked site lost to a feed with no live viewer \
         left to protect"
    );
}

/// The exemption's own limit: an auto-poll result really is a "latest"
/// claim, so a pending navigation must not smuggle one past the guard.
#[test]
fn an_auto_poll_result_stays_behind_the_guard_even_mid_navigation() {
    let mut app = app_showing(at(10));
    app.chunk_feeds.ensure("KTLX");
    app.manual_nav_pending = true;

    send_auto_poll_archive(&app, at(5));
    app.poll_data_channels();

    assert_eq!(
        shown_stamp(&app),
        at(10),
        "an auto-poll volume walked a chunk-fed live display backwards \
         because a navigation happened to be in flight"
    );
}

/// Live on a chunk-fed site is a reattachment, not a fetch: the panes
/// already hold the feed's current volume, and the archive fallback would
/// return the volume *before* it — a walk backwards for the one click that
/// means "newest". No fetch generation may be spent on it.
#[test]
fn jump_to_live_on_a_serving_feed_reattaches_without_a_fetch() {
    let mut app = app_showing(at(10));
    add_live_pane(&mut app, at(10));
    app.gui.pane_mut(0).unwrap().viewing_live = false;
    app.chunk_feeds.ensure("KTLX");
    assert!(app.chunks_are_feeding("KTLX"), "precondition: feed running");
    let generation = app.render.fetch_generation_for("KTLX");

    app.handle_gui_action(
        rustdar_egui::actions::GuiAction::JumpToLive { pane_idx: 0 },
        None,
    );

    assert!(
        app.gui.pane(0).unwrap().viewing_live,
        "Live must reattach the pane to the feed"
    );
    assert_eq!(
        app.render.fetch_generation_for("KTLX"),
        generation,
        "Live on a serving feed spent a fetch on data already on screen"
    );
    assert!(
        !app.manual_nav_pending,
        "a reattachment leaves nothing pending for the scan drain to settle"
    );
    assert!(
        !app.gui.fetching(),
        "a reattachment must not raise the fetch spinner"
    );
}

/// With the site parked and its feed retired, Live still takes the archive
/// route: cached volume if one was kept, else a real fetch — the
/// pre-feed behaviour, unchanged.
#[test]
fn jump_to_live_with_the_feed_retired_still_fetches() {
    let mut app = app_showing(at(10));
    app.gui.pane_mut(0).unwrap().viewing_live = false;
    assert!(!app.chunks_are_feeding("KTLX"), "precondition: no feed");
    let generation = app.render.fetch_generation_for("KTLX");

    app.handle_gui_action(
        rustdar_egui::actions::GuiAction::JumpToLive { pane_idx: 0 },
        None,
    );

    assert!(app.gui.pane(0).unwrap().viewing_live);
    assert_eq!(
        app.render.fetch_generation_for("KTLX"),
        generation + 1,
        "with no feed serving, Live must fetch the latest volume"
    );
    assert!(
        app.manual_nav_pending,
        "the fetch settles through the drain"
    );
}

// ── The transport payloads, applied (M10) ────────────────────────────

/// `NavigateTime`'s payload, acted on: the step is relative to the pane's
/// own scan time, the pane parks out of live, and a fetch generation is
/// spent on the target moment.
#[test]
fn navigate_time_steps_relative_to_the_panes_scan_and_parks_it() {
    let mut app = app_showing(at(30));
    let generation = app.render.fetch_generation_for("KTLX");

    app.handle_gui_action(
        rustdar_egui::actions::GuiAction::NavigateTime {
            pane_idx: 0,
            step_secs: -600,
        },
        None,
    );

    assert!(!app.gui.pane(0).unwrap().viewing_live);
    assert!(app.gui.fetching());
    assert!(app.manual_nav_pending);
    assert_eq!(app.render.fetch_generation_for("KTLX"), generation + 1);
    // The UI config's timestamp is the fetch target in local time — the
    // pane's scan time stepped back, not "now minus step".
    let expected = chrono::TimeZone::from_utc_datetime(&chrono::Local, &at(20)).naive_local();
    assert_eq!(
        app.gui.get_radar_config().timestamp,
        expected,
        "the fetch target must be the pane's scan time stepped by the payload"
    );
}

/// `NavigateOneScan` spends a generation on the adjacent-scan lookup and
/// marks the navigation pending; a pane with no scan yet is a silent no-op
/// rather than a fetch for a site with no reference moment.
#[test]
fn navigate_one_scan_spends_a_generation_and_marks_pending() {
    let mut app = app_showing(at(30));
    let generation = app.render.fetch_generation_for("KTLX");

    app.handle_gui_action(
        rustdar_egui::actions::GuiAction::NavigateOneScan {
            pane_idx: 0,
            forward: false,
        },
        None,
    );
    assert!(app.manual_nav_pending);
    assert!(app.gui.fetching());
    assert_eq!(app.render.fetch_generation_for("KTLX"), generation + 1);

    let mut bare = headless(TestBridge::desktop());
    let generation = bare.render.fetch_generation_for("KTLX");
    bare.handle_gui_action(
        rustdar_egui::actions::GuiAction::NavigateOneScan {
            pane_idx: 0,
            forward: true,
        },
        None,
    );
    assert!(
        !bare.manual_nav_pending && bare.render.fetch_generation_for("KTLX") == generation,
        "a pane with no scan info must not spend a fetch on an adjacent-scan \
         lookup with no reference moment"
    );
}

/// The loop transport's per-frame payloads, acted on: toggle drives the
/// phase state machine, step wraps at both ends, seek clamps to the frame
/// list. These are the frontend halves of the timeline's row-2 emissions.
#[test]
fn the_loop_transport_payloads_drive_the_playback_state() {
    use rustdar_egui::actions::GuiAction;
    use rustdar_egui::pane::{LoopFrame, LoopPhase, LoopPlaybackState};

    let mut app = app_showing(at(10));
    let site = rustdar_radar::sites::get_radar_site("KTLX").unwrap();
    {
        let mut state =
            LoopPlaybackState::new_for_loop(3600, site, rustdar_radar::types::RenderView::PlanView);
        state.phase = LoopPhase::Ready;
        state.frames = (0..3)
            .map(|i| LoopFrame {
                timestamp: at(i),
                image: None,
                render_in_flight: false,
                render_failed: false,
            })
            .collect();
        app.gui.pane_mut(0).unwrap().loop_state = state;
    }
    let phase = |app: &App| app.gui.pane(0).unwrap().loop_state.phase;
    let frame = |app: &App| app.gui.pane(0).unwrap().loop_state.current_frame;

    app.handle_gui_action(GuiAction::ToggleLoopPlayback { pane_idx: 0 }, None);
    assert_eq!(phase(&app), LoopPhase::Playing, "Ready + toggle = Playing");
    app.handle_gui_action(GuiAction::ToggleLoopPlayback { pane_idx: 0 }, None);
    assert_eq!(phase(&app), LoopPhase::Paused, "Playing + toggle = Paused");

    app.handle_gui_action(
        GuiAction::StepLoopFrame {
            pane_idx: 0,
            forward: false,
        },
        None,
    );
    assert_eq!(frame(&app), 2, "backward from 0 wraps to the last frame");
    app.handle_gui_action(
        GuiAction::StepLoopFrame {
            pane_idx: 0,
            forward: true,
        },
        None,
    );
    assert_eq!(frame(&app), 0, "forward from the last frame wraps to 0");

    app.handle_gui_action(
        GuiAction::SeekLoopFrame {
            pane_idx: 0,
            frame_index: 1,
        },
        None,
    );
    assert_eq!(frame(&app), 1, "seek lands on the asked-for frame");
    app.handle_gui_action(
        GuiAction::SeekLoopFrame {
            pane_idx: 0,
            frame_index: 99,
        },
        None,
    );
    assert_eq!(frame(&app), 1, "an out-of-range seek changes nothing");
}
