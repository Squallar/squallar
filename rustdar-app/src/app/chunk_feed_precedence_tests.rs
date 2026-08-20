use super::tests::{empty_scan, headless};
use super::*;
use crate::platform_double::TestBridge;

/// The cell budget a device that says nothing about itself resolves.
const SHIPPED_CELLS: [u32; 3] = rustdar_device_profile::constants::VOLUME_GRID_CELLS;

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
        pane.set_site("KTLX".to_string());
        pane.viewing_live = true;
        pane.scan_info = Some(rustdar_radar::types::ScanInfo {
            site_source: rustdar_radar::site_position::SitePositionSource::Table,
            site_position: None,
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

/// The wait still has to end.
#[test]
fn a_skipped_archive_volume_still_ends_the_wait_it_belonged_to() {
    let mut app = app_showing(at(10));
    app.chunk_feeds.ensure("KTLX");
    app.gui
        .apply(rustdar_egui::shell_api::GuiEvent::Fetching(true));
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

/// **The overlay dies with the setting.**
#[test]
fn turning_live_chunks_off_stops_the_overlay_from_standing() {
    let mut app = app_showing(at(10));
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

/// The 3D texture limit these fixtures name when they ask what would be requested.
const DEVICE_AXIS: u32 = 2048;

/// The device's own limit reaches the request, and the shape it produces is the
/// one that device can hold.
#[test]
fn the_requested_shape_is_the_one_this_device_can_hold() {
    use rustdar_egui::pane::{VolumeStamp, VolumeTarget};

    let target = VolumeTarget {
        volume: VolumeStamp {
            site: "KTLX".to_owned(),
            collected: chrono::NaiveDate::from_ymd_opt(2026, 7, 30)
                .expect("a real date")
                .and_hms_opt(22, 33, 0)
                .expect("a real time"),
        },
        product: rustdar_radar::types::RadarProduct::Reflectivity,
        region: None,
    };

    for axis in [256u32, 512, 2048] {
        let request = voxel_request_for(&target, 35.33, -97.28, SHIPPED_CELLS, axis);
        assert_eq!(
            request.shape,
            rustdar_device_profile::constants::volume_grid_shape(axis),
            "a {axis}-reporting device must be asked for the shape its own \
             limit and this target's budget produce",
        );
        for (name, n) in [
            ("nx", request.shape.nx),
            ("ny", request.shape.ny),
            ("nz", request.shape.nz),
        ] {
            assert!(
                n as u32 <= axis,
                "a {axis}-reporting device was asked for {n} cells of {name}",
            );
        }
    }
    assert_eq!(
        voxel_request_for(
            &target,
            35.33,
            -97.28,
            SHIPPED_CELLS,
            rustdar_device_profile::constants::WEBGL2_MAX_TEXTURE_DIMENSION_3D,
        )
        .shape,
        rustdar_device_profile::constants::VOLUME_GRID_FLOOR_SHAPE,
    );
}

/// A picked region decides the ground that is resampled; without one, the
/// default box about the site does.
#[test]
fn a_picked_region_decides_the_ground_that_is_resampled() {
    use rustdar_egui::pane::{VolumeRegion, VolumeStamp, VolumeTarget};
    use rustdar_geo::GeoPoint;

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

    let default = voxel_request_for(&target(None), 35.33, -97.28, SHIPPED_CELLS, DEVICE_AXIS);
    assert_eq!(default.centre, (35.33, -97.28), "no region means the site");
    assert_eq!(
        default.half_extent_km, None,
        "no region must leave the extent to `build_voxels`, which is the only \
         side of the seam that holds the volume the reach comes from",
    );

    let picked = VolumeRegion::new(
        GeoPoint {
            lat: 36.1,
            lon: -98.4,
        },
        rustdar_radar::voxel::HalfExtentKm::square(22.5),
    )
    .expect("a valid region");
    let aimed = voxel_request_for(
        &target(Some(picked)),
        35.33,
        -97.28,
        SHIPPED_CELLS,
        DEVICE_AXIS,
    );
    assert_eq!(
        aimed.centre,
        (36.1, -98.4),
        "a picked region must move the box off the site",
    );
    assert_eq!(
        aimed.half_extent_km,
        Some(rustdar_radar::voxel::HalfExtentKm::square(22.5)),
    );
}

/// The vertical extent is not part of the region pick.
#[test]
fn a_region_pick_does_not_move_the_top_or_the_bottom_of_the_box() {
    use rustdar_egui::pane::{VolumeRegion, VolumeStamp, VolumeTarget};
    use rustdar_geo::GeoPoint;

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
        rustdar_radar::voxel::HalfExtentKm::square(15.0),
    );

    for target in [make(None), make(picked)] {
        let request = voxel_request_for(&target, 35.33, -97.28, SHIPPED_CELLS, DEVICE_AXIS);
        assert_eq!(
            request.base_km_msl,
            rustdar_radar::voxel::DEFAULT_BASE_KM_MSL
        );
        assert_eq!(request.top_km_msl, rustdar_radar::voxel::DEFAULT_TOP_KM_MSL);
    }
}

/// The pane and the resampler agree about the box a pane has before its first
/// grid lands.
#[test]
fn the_pane_and_the_resampler_agree_about_the_stand_in_box() {
    let base = rustdar_egui::pane::BASE_HALF_WIDTH_KM;
    assert_eq!(base, rustdar_radar::voxel::box_half_width_km(f64::NAN));
    assert_eq!(
        rustdar_egui::pane::box_size_km(None),
        [
            (2.0 * base) as f32,
            (2.0 * base) as f32,
            (rustdar_radar::voxel::DEFAULT_TOP_KM_MSL - rustdar_radar::voxel::DEFAULT_BASE_KM_MSL)
                as f32,
        ],
    );
}

/// **The 3D build reads `base_scans` and never `scan_data`.**
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

    let mut live_only = headless(TestBridge::desktop());
    live_only.scan_data.insert(
        "KTLX".to_string(),
        (Arc::new(stamped_scan(10)), Default::default()),
    );
    live_only.handle_prepare_volume(0, target.clone());
    assert!(
        live_only.volume_store.lookup(&target).is_none(),
        "a volume only the map panes hold was handed to the resampler",
    );

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

/// **A budget-refused frame pays nothing for the refusal.**
#[test]
fn a_full_budget_refuses_the_3d_ask_before_paying_the_extraction() {
    use rustdar_device_profile::constants::MAX_CONCURRENT_RENDERS;
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

    app.handle_prepare_volume(0, target.clone());
    assert_eq!(
        app.volume_extractions.get(),
        1,
        "the level-triggered re-ask must attach, not re-extract",
    );
}

/// A pane is handed the volume it named, or none.
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
#[test]
fn every_archive_path_offers_its_volume_to_the_3d_pane() {
    let collected = |app: &App| app.base_scans.get("KTLX").map(|(_, _, at)| *at);

    let mut shown = app_showing(at(10));
    send_archive(&shown, at(15));
    shown.poll_data_channels();
    assert!(
        shown.scan_data.contains_key("KTLX"),
        "precondition: this is the arm that puts the volume on screen",
    );
    assert_eq!(collected(&shown), Some(at(15)));

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
#[test]
fn a_refresh_in_the_pre_publication_window_does_not_walk_the_base_back() {
    let based = |app: &App| app.base_scans.get("KTLX").map(|(_, _, at)| *at);
    let mut app = app_showing(at(10));
    app.chunk_feeds.ensure("KTLX");
    app.base_scans.insert(
        "KTLX".to_string(),
        (Arc::new(stamped_scan(10)), Default::default(), at(10)),
    );

    send_archive(&app, at(5));
    app.poll_data_channels();
    assert_eq!(
        based(&app),
        Some(at(10)),
        "a manual Refresh in the pre-publication window regressed the \
             merge base one volume, under every whole-volume consumer",
    );

    send_archive(&app, at(15));
    app.poll_data_channels();
    assert_eq!(based(&app), Some(at(15)), "a newer volume was refused");

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
    app.push_frame_inputs();

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

/// A pane on `site` at `shown`, beside [`app_showing`]'s pane 0 — the state
/// a second linked-off or unlinked pane is in while its sibling navigates.
fn add_live_pane(app: &mut App, shown: chrono::NaiveDateTime) {
    let mut two = super::tests::two_pane_app("KTLX", "KTLX");
    std::mem::swap(&mut app.gui, &mut two.gui);
    for idx in [0, 1] {
        let pane = app.gui.pane_mut(idx).unwrap();
        pane.viewing_live = true;
        pane.scan_info = Some(rustdar_radar::types::ScanInfo {
            site_source: rustdar_radar::site_position::SitePositionSource::Table,
            site_position: None,
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

/// The "time controls are inert" root cause, pinned at its site.
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

/// The single-pane race arm of the same break: the response drains on the very frame
/// the click was processed.
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

/// Live on a chunk-fed site is a reattachment, not a fetch: the panes already hold the
/// feed's current volume.
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

/// With the site parked and its feed retired, Live still takes the archive route:
/// cached volume if one was kept, else a real fetch.
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

/// `NavigateTime`'s payload, acted on: the step is relative to the pane's own scan
/// time, the pane parks out of live.
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
    let expected = chrono::TimeZone::from_utc_datetime(&chrono::Local, &at(20)).naive_local();
    assert_eq!(
        app.gui.get_radar_config().timestamp,
        expected,
        "the fetch target must be the pane's scan time stepped by the payload"
    );
}

/// `NavigateOneScan` spends a generation on the adjacent-scan lookup and marks the
/// navigation pending.
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

/// The loop transport's per-frame payloads, acted on: toggle drives the phase state
/// machine, step wraps at both ends.
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

/// A volume whose flight runs from `first` to `last` — two sweeps, dated
/// minutes apart, as a real VCP-212 volume's are.
fn spanning_scan(first: u32, last: u32) -> nexrad_model::data::Scan {
    use nexrad_model::data::{
        ChannelConfiguration, ElevationCut, MomentData, PulseWidth, Radial, RadialStatus, Scan,
        Sweep, VolumeCoveragePattern, WaveformType,
    };
    let sweep = |number: u8, elevation: f32, minute: u32| {
        let radials = (0..8u16)
            .map(|i| {
                Radial::new(
                    at(minute).and_utc().timestamp_millis() + i64::from(i),
                    i + 1,
                    f32::from(i) * 45.0,
                    45.0,
                    RadialStatus::IntermediateRadialData,
                    number,
                    elevation,
                    Some(MomentData::from_fixed_point(
                        4,
                        2125,
                        250,
                        8,
                        2.0,
                        66.0,
                        vec![120, 140, 160, 180],
                    )),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                )
            })
            .collect();
        Sweep::new(number, radials)
    };
    let cut = |angle: f64| {
        ElevationCut::new(
            angle,
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
        )
    };
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
            vec![cut(0.5), cut(1.5)],
        ),
        vec![sweep(1, 0.5, first), sweep(2, 1.5, last)],
    )
}

fn volume_target(collected: chrono::NaiveDateTime) -> rustdar_egui::pane::VolumeTarget {
    rustdar_egui::pane::VolumeTarget {
        volume: rustdar_egui::pane::VolumeStamp {
            site: "KTLX".to_owned(),
            collected,
        },
        product: rustdar_radar::types::RadarProduct::Reflectivity,
        region: None,
    }
}

/// **A 3D pane navigated off live is served the volume it names.**
#[test]
fn a_navigated_3d_pane_is_served_the_volume_it_names() {
    let mut app = headless(TestBridge::desktop());
    app.base_scans.insert(
        "KTLX".to_string(),
        (Arc::new(spanning_scan(10, 14)), Default::default(), at(10)),
    );
    let newest = app
        .current_volume_stamp("KTLX")
        .expect("the fixture volume resolves a stamp")
        .newest;
    assert!(
        newest >= at(14),
        "precondition: the volume's newest data time must be the end of the \
         flight and not the key it is held under ({}), or the two source \
         decisions cannot be told apart — got {newest}",
        at(10),
    );

    let navigated = volume_target(at(10));
    app.handle_prepare_volume(0, navigated.clone());
    assert!(
        app.volume_store.lookup(&navigated).is_some(),
        "the pane named the volume it is showing and the host had it in hand, \
         and no build was reached — so a scrubbed 3D pane waits for ever",
    );

    let live = volume_target(newest);
    app.handle_prepare_volume(1, live.clone());
    assert!(
        app.volume_store.lookup(&live).is_some(),
        "the live target stopped being served",
    );

    let unheld = volume_target(at(30));
    app.handle_prepare_volume(2, unheld.clone());
    assert!(
        app.volume_store.lookup(&unheld).is_none(),
        "a volume the app does not hold was answered for, which stops the pane \
         ever asking again",
    );
}

/// **The request carries the budget this device resolved, not the one this
/// binary compiled.**
#[test]
fn the_requested_shape_is_the_budget_this_device_resolved() {
    use rustdar_egui::pane::{VolumeStamp, VolumeTarget};

    let target = VolumeTarget {
        volume: VolumeStamp {
            site: "KTLX".to_owned(),
            collected: chrono::NaiveDate::from_ymd_opt(2026, 7, 30)
                .expect("a real date")
                .and_hms_opt(22, 33, 0)
                .expect("a real time"),
        },
        product: rustdar_radar::types::RadarProduct::Reflectivity,
        region: None,
    };

    let web = |two_d: u32, three_d: u32| {
        rustdar_device_profile::budget::resolve(&rustdar_device_profile::budget::DeviceProfile {
            limits: rustdar_device_profile::budget::BudgetLimits::WASM,
            platform: rustdar_device_profile::budget::Platform::Web,
            adapter: rustdar_device_profile::budget::AdapterCeilings {
                max_texture_dimension_2d: two_d,
                max_texture_dimension_3d: three_d,
            },
            ..rustdar_device_profile::budget::DeviceProfile::for_target()
        })
    };
    let phone = web(2048, 256);
    let desktop = web(
        rustdar_device_profile::budget::DESKTOP_CLASS_REPORT.max_texture_dimension_2d,
        rustdar_device_profile::budget::DESKTOP_CLASS_REPORT.max_texture_dimension_3d,
    );

    let cells_on = |budgets: rustdar_device_profile::budget::Budgets, axis: u32| {
        voxel_request_for(&target, 35.33, -97.28, budgets.grid_cells, axis)
            .shape
            .cells()
    };
    assert!(
        cells_on(desktop, 8192) > cells_on(phone, 256),
        "a browser on desktop-class silicon is put on the wire asking for the \
         same grid as one at the spec floor",
    );

    let call = include_str!("../app.rs")
        .split_once("let request = voxel_request_for(")
        .map(|(_, rest)| rest.split_once(");").expect("a call site").0)
        .expect("`voxel_request_for` is still called from `prepare_volume`");
    assert!(
        call.contains("self.budgets.grid_cells"),
        "the production call site passes `{call}` — the resolved budget is the \
         only thing that carries a promotion this far",
    );
}

/// **The budgets are re-resolved before anything downstream reads them.**
#[test]
fn the_device_profile_is_folded_in_before_any_budget_is_spent() {
    let body = include_str!("../app.rs")
        .split_once("fn install_volume_bridge(&mut self)")
        .and_then(|(_, rest)| rest.split_once("\n    }"))
        .map(|(body, _)| body)
        .expect("`install_volume_bridge` is still a method on `App`");

    let update = body
        .find("self.update_device_profile(")
        .expect("`install_volume_bridge` no longer re-resolves the budgets");
    for spender in [
        "LoopPoolLimits::from_budgets",
        "self.budgets.quality_ceiling",
    ] {
        let at = body
            .find(spender)
            .unwrap_or_else(|| panic!("`{spender}` is no longer read there"));
        assert!(
            update < at,
            "`{spender}` is read before the adapter's own reading is folded in, \
             so that one budget stays at the floor while the rest are promoted",
        );
    }
}

/// **A lost surface steps the whole budget set down, not just the loop pool.**
#[test]
fn a_lost_surface_steps_the_budgets_down_a_rung_and_writes_it_at_once() {
    use rustdar_device_profile::quality::GradientShading;
    use rustdar_kv::KvStore;

    let platform = TestBridge::desktop();
    let store = platform.store();
    let mut app = headless(platform);

    app.loop_pool = crate::loop_pool::LoopPool::for_promotion(
        rustdar_device_profile::budget::Promotion::Ceiling,
        None,
        crate::loop_pool::LoopPoolLimits::from_budgets(&app.budgets),
    );
    let before = app.budgets;
    let pool_before = app.loop_pool.bytes();
    assert_eq!(
        before.quality_ceiling.shading,
        GradientShading::On,
        "precondition: this build's desktop bracket starts at the cloud rung",
    );

    app.back_off_budgets();

    assert_eq!(
        app.budgets.quality_ceiling.shading,
        GradientShading::Off,
        "a lost surface left the most expensive rung in the application in \
         place, so the next one costs the same and the ladder never runs",
    );
    assert_eq!(
        app.budgets.offscreen_bytes, before.offscreen_bytes,
        "the first rung took two things at once, so a device that only needed \
         to give up its lighting also lost its resolution",
    );
    assert!(
        app.loop_pool.bytes() < pool_before,
        "the pool did not halve beside it",
    );
    assert_eq!(
        store.load(crate::budget_memo::BUDGET_MEMO_KEY).as_deref(),
        Some("1"),
        "the rung was not persisted at the moment of the decision",
    );
    assert!(store.load(crate::loop_pool::LOOP_POOL_KEY).is_some());
}

/// **What was learned by crashing is in force from the first paint of the next
/// session.**
#[test]
fn a_backed_off_machine_reopens_where_it_left_off() {
    use rustdar_device_profile::quality::GradientShading;

    let first = TestBridge::desktop();
    let store = first.store();
    let mut app = headless(first);
    app.back_off_budgets();
    app.back_off_budgets();
    let settled = app.budgets;
    assert_eq!(settled.steps_back, 2);

    let reopened = headless(TestBridge::desktop().with_store(store));
    assert_eq!(
        reopened.budgets.steps_back, 2,
        "the ladder position was re-probed instead of remembered",
    );
    assert_eq!(reopened.budgets.quality_ceiling, settled.quality_ceiling);
    assert_eq!(reopened.budgets.offscreen_bytes, settled.offscreen_bytes);
    assert_eq!(
        reopened.budgets.quality_ceiling.shading,
        GradientShading::Off,
    );
}

/// **A machine that keeps failing stops writing, rather than counting for ever.**
#[test]
fn the_ladder_position_stops_rising_once_every_rung_is_at_its_stop() {
    use rustdar_kv::KvStore;

    let platform = TestBridge::desktop();
    let store = platform.store();
    let mut app = headless(platform);

    for _ in 0..12 {
        app.back_off_budgets();
    }
    let settled = app.budgets.steps_back;
    assert!(
        settled > 0 && settled < 12,
        "twelve lost surfaces resolved to rung {settled}, which is either no \
         ladder at all or a failure counter wearing one as a hat",
    );
    assert_eq!(
        store.load(crate::budget_memo::BUDGET_MEMO_KEY).as_deref(),
        Some(settled.to_string().as_str()),
    );
    let shipped = rustdar_device_profile::budget::BudgetLimits::for_target();
    assert_eq!(app.budgets.grid_cells, shipped.grid_cells.floor);
    assert_eq!(app.budgets.offscreen_bytes, shipped.offscreen_bytes.floor);
    assert_eq!(
        app.budgets.raster_side_ceiling_px,
        shipped.long_range_image_side_px.floor,
    );
}
