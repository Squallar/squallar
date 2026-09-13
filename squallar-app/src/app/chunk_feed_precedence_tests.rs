use super::tests::{empty_scan, headless};
use super::*;
use crate::platform_double::TestBridge;
use squallar_source::id::known;

/// The cell budget a device that says nothing about itself resolves.
const SHIPPED_CELLS: [u32; 3] = squallar_device_profile::constants::VOLUME_GRID_CELLS;

/// **The request the seam shapes for `target` on a device with this budget.**
///
/// Both halves of the production path and nothing else: `volume_job_context`
/// is exactly what `prepare_volume` hands over, and `request_for` is exactly
/// what `RadarSource::volume_job` makes of it before wrapping it in an
/// envelope. The moment is no longer an argument — it comes off the target's
/// own field, which is the one place a 3D ask names a field at all.
///
/// **The payload is a stand-in, and that is a statement rather than a
/// shortcut**: `request_for` is a pure function of the ground, the budget and
/// the field, and it never reads the payload. The downcast half — where the
/// payload is the whole question — is pinned separately in
/// `volume_layer_tests`, against a real extracted volume.
///
/// The **site** is handed over separately because the box is a rectangle in the
/// site's tangent frame and its floor is derived over that frame.
/// `RadarSource::volume_job` takes it off the payload, which a stand-in payload
/// cannot answer; this helper is already given the site's own coordinates and
/// passes them, so the request it shapes is the one production would shape.
fn shaped_request(
    target: &squallar_egui::pane::VolumeTarget,
    site_lat: f64,
    site_lon: f64,
    cells: [u32; 3],
    max_axis: u32,
) -> squallar_radar::voxel::VoxelRequest {
    let ctx = volume_job_context(target, site_lat, site_lon, cells, max_axis, Box::new(()));
    squallar_radar::voxel::request_for(&ctx, (site_lat, site_lon))
        .expect("Reflectivity is a field radar registers")
}

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
        pane.set_viewing_live(true);
        pane.scan_info = Some(squallar_radar::types::ScanInfo {
            site_source: squallar_radar::site_position::SitePositionSource::Table,
            site_position: None,
            site: squallar_radar::sites::RadarSite {
                name: "KTLX",
                network: squallar_radar::sites::RadarNetwork::of_id("KTLX"),
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
            requester: crate::channels::FetchRequester::Site,
            result: Ok(crate::channels::ScanData {
                scan,
                declared_nyquist: Default::default(),
                site: "KTLX".to_string(),
                timestamp,
                // An archive arrival carries the bytes it was decoded from,
                // which is what lets the loop cache hold it compressed.
                archive: Some(std::sync::Arc::new(vec![0u8; 64])),
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
            requester: crate::channels::FetchRequester::Site,
            result: Ok(crate::channels::ScanData {
                scan: empty_scan(),
                declared_nyquist: Default::default(),
                site: "KTLX".to_string(),
                timestamp,
                archive: Some(std::sync::Arc::new(vec![0u8; 64])),
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
        !app.volumes.holds_any_still("KTLX"),
        "and it replaced the volume the panes render from"
    );
}

/// The wait still has to end.
#[test]
fn a_skipped_archive_volume_still_ends_the_wait_it_belonged_to() {
    let mut app = app_showing(at(10));
    app.chunk_feeds.ensure("KTLX");
    app.gui
        .apply(squallar_egui::shell_api::GuiEvent::Fetching(true));
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
        app.volumes.holds_any_still("KTLX"),
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
        app.volumes.holds_any_still("KTLX"),
        "the fallback cannot restore a site if an older archive volume is \
             refused when no feed is running"
    );
}

/// **The overlay dies with the setting.**
#[test]
fn turning_live_chunks_off_stops_the_overlay_from_standing() {
    let mut app = app_showing(at(10));
    app.gui.apply_layer_control(
        &squallar_egui::radar_layer::POLL_LAYER,
        &squallar_egui::radar_layer::chunk_notifications_update(false),
    );
    app.chunk_feeds.ensure("KTLX");
    app.chunk_feeds
        .force_serving("KTLX", Arc::new(empty_scan()));
    assert!(
        app.chunk_feeds.snapshot("KTLX").is_some(),
        "precondition: the feed is serving an overlay",
    );

    app.gui.apply_layer_control(
        &squallar_egui::radar_layer::POLL_LAYER,
        &squallar_egui::radar_layer::live_chunks_update(false),
    );
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
    use squallar_egui::pane::{VolumeStamp, VolumeTarget};

    let target = VolumeTarget {
        volume: VolumeStamp {
            site: "KTLX".to_owned(),
            collected: chrono::NaiveDate::from_ymd_opt(2026, 7, 30)
                .expect("a real date")
                .and_hms_opt(22, 33, 0)
                .expect("a real time"),
        },
        product: squallar_radar::fields::known::REFLECTIVITY,
        region: None,
    };

    for axis in [256u32, 512, 2048] {
        let request = shaped_request(&target, 35.33, -97.28, SHIPPED_CELLS, axis);
        assert_eq!(
            request.shape,
            squallar_device_profile::constants::volume_grid_shape(axis),
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
        shaped_request(
            &target,
            35.33,
            -97.28,
            SHIPPED_CELLS,
            squallar_device_profile::constants::WEBGL2_MAX_TEXTURE_DIMENSION_3D
        )
        .shape,
        squallar_device_profile::constants::VOLUME_GRID_FLOOR_SHAPE,
    );
}

/// A picked region decides the ground that is resampled; without one, the
/// default box about the site does.
#[test]
fn a_picked_region_decides_the_ground_that_is_resampled() {
    use squallar_egui::pane::{VolumeRegion, VolumeStamp, VolumeTarget};
    use squallar_geo::GeoPoint;

    let target = |region| VolumeTarget {
        volume: VolumeStamp {
            site: "KTLX".to_owned(),
            collected: chrono::NaiveDate::from_ymd_opt(2026, 7, 30)
                .expect("a real date")
                .and_hms_opt(22, 33, 0)
                .expect("a real time"),
        },
        product: squallar_radar::fields::known::REFLECTIVITY,
        region,
    };

    let default = shaped_request(&target(None), 35.33, -97.28, SHIPPED_CELLS, DEVICE_AXIS);
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
        squallar_radar::voxel::HalfExtentKm::square(22.5),
    )
    .expect("a valid region");
    let aimed = shaped_request(
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
        Some(squallar_radar::voxel::HalfExtentKm::square(22.5)),
    );
}

/// The vertical extent is not part of the region pick.
#[test]
fn a_region_pick_does_not_move_the_top_or_the_bottom_of_the_box() {
    use squallar_egui::pane::{VolumeRegion, VolumeStamp, VolumeTarget};
    use squallar_geo::GeoPoint;

    let make = |region| VolumeTarget {
        volume: VolumeStamp {
            site: "KTLX".to_owned(),
            collected: chrono::NaiveDate::from_ymd_opt(2026, 7, 30)
                .expect("a real date")
                .and_hms_opt(22, 33, 0)
                .expect("a real time"),
        },
        product: squallar_radar::fields::known::REFLECTIVITY,
        region,
    };
    let picked = VolumeRegion::new(
        GeoPoint {
            lat: 36.1,
            lon: -98.4,
        },
        squallar_radar::voxel::HalfExtentKm::square(15.0),
    );

    for target in [make(None), make(picked)] {
        let request = shaped_request(&target, 35.33, -97.28, SHIPPED_CELLS, DEVICE_AXIS);
        assert_eq!(
            request.base_km_msl,
            squallar_radar::voxel::DEFAULT_BASE_KM_MSL
        );
        assert_eq!(
            request.top_km_msl,
            squallar_radar::voxel::DEFAULT_TOP_KM_MSL
        );
    }
}

/// The pane and the resampler agree about the box a pane has before its first
/// grid lands.
#[test]
fn the_pane_and_the_resampler_agree_about_the_stand_in_box() {
    let base = squallar_egui::pane::BASE_HALF_WIDTH_KM;
    assert_eq!(base, squallar_radar::voxel::box_half_width_km(f64::NAN));
    assert_eq!(
        squallar_egui::pane::box_size_km(None, None),
        [
            (2.0 * base) as f32,
            (2.0 * base) as f32,
            (squallar_radar::voxel::DEFAULT_TOP_KM_MSL - squallar_radar::voxel::DEFAULT_BASE_KM_MSL)
                as f32,
        ],
    );
}

/// **The 3D build reads the merge base and never the still store.**
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
    let target = squallar_egui::pane::VolumeTarget {
        volume: squallar_egui::pane::VolumeStamp {
            site: "KTLX".to_owned(),
            collected: at(10),
        },
        product: squallar_radar::fields::known::REFLECTIVITY,
        region: None,
    };

    let mut live_only = headless(TestBridge::desktop());
    drop(live_only.volumes.install_still(
        "KTLX".to_string(),
        at(10),
        (Arc::new(stamped_scan(10)), Default::default()),
    ));
    live_only.handle_prepare_volume(0, &squallar_source::id::known::RADAR, target.clone());
    assert!(
        live_only.volume_store.lookup(&target).is_none(),
        "a volume only the map panes hold was handed to the resampler",
    );

    let mut based = headless(TestBridge::desktop());
    based.volumes.install_base(
        "KTLX".to_string(),
        (Arc::new(stamped_scan(10)), Default::default(), at(10)),
    );
    based.handle_prepare_volume(0, &squallar_source::id::known::RADAR, target.clone());
    assert!(
        based.volume_store.lookup(&target).is_some(),
        "the 3D pane was offered a base volume and the build never \
             reached it, so the pane waits for ever",
    );
}

/// **A budget-refused frame pays nothing for the refusal.**
#[test]
fn a_full_budget_refuses_the_3d_ask_before_paying_the_extraction() {
    use squallar_device_profile::constants::MAX_CONCURRENT_RENDERS;
    use std::sync::atomic::Ordering;

    let target = squallar_egui::pane::VolumeTarget {
        volume: squallar_egui::pane::VolumeStamp {
            site: "KTLX".to_owned(),
            collected: at(10),
        },
        product: squallar_radar::fields::known::REFLECTIVITY,
        region: None,
    };
    let mut app = headless(TestBridge::desktop());
    app.volumes.install_base(
        "KTLX".to_string(),
        (Arc::new(stamped_scan(10)), Default::default(), at(10)),
    );

    app.render
        .renders_in_flight
        .store(MAX_CONCURRENT_RENDERS, Ordering::Relaxed);
    for _ in 0..3 {
        app.handle_prepare_volume(0, &squallar_source::id::known::RADAR, target.clone());
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
    app.handle_prepare_volume(0, &squallar_source::id::known::RADAR, target.clone());
    assert_eq!(
        app.volume_extractions.get(),
        1,
        "the freed slot performs exactly one extraction",
    );
    assert!(
        app.volume_store.lookup(&target).is_some(),
        "the freed slot dispatches the build",
    );

    app.handle_prepare_volume(0, &squallar_source::id::known::RADAR, target.clone());
    assert_eq!(
        app.volume_extractions.get(),
        1,
        "the level-triggered re-ask must attach, not re-extract",
    );
}

/// A pane is handed the volume it named, or none.
#[test]
fn a_3d_pane_is_not_handed_a_volume_other_than_the_one_it_asked_for() {
    let target = squallar_egui::pane::VolumeTarget {
        volume: squallar_egui::pane::VolumeStamp {
            site: "KTLX".to_owned(),
            collected: at(10),
        },
        product: squallar_radar::fields::known::REFLECTIVITY,
        region: None,
    };

    let mut app = headless(TestBridge::desktop());
    app.volumes.install_base(
        "KTLX".to_string(),
        (Arc::new(stamped_scan(15)), Default::default(), at(15)),
    );
    app.handle_prepare_volume(0, &squallar_source::id::known::RADAR, target.clone());

    assert!(
        app.volume_store.lookup(&target).is_none(),
        "the pane asked for the 18:10 volume and was built the 18:15 one",
    );
}

/// **Every archive path offers its volume to the 3D pane**, including the
/// two that decline to display it.
#[test]
fn every_archive_path_offers_its_volume_to_the_3d_pane() {
    let collected = |app: &App| app.volumes.base_collected_at("KTLX");

    let mut shown = app_showing(at(10));
    send_archive(&shown, at(15));
    shown.poll_data_channels();
    assert!(
        shown.volumes.holds_any_still("KTLX"),
        "precondition: this is the arm that puts the volume on screen",
    );
    assert_eq!(collected(&shown), Some(at(15)));

    let mut behind = app_showing(at(10));
    behind.chunk_feeds.ensure("KTLX");
    send_archive(&behind, at(5));
    behind.poll_data_channels();
    assert!(
        !behind.volumes.holds_any_still("KTLX"),
        "precondition: this is the `feed_is_ahead` arm",
    );
    assert_eq!(
        collected(&behind),
        Some(at(5)),
        "a site with a feed running takes this arm on every poll, so a 3D \
             pane on it would never be offered a volume at all",
    );

    let mut historic = app_showing(at(10));
    historic.gui.pane_mut(0).unwrap().set_viewing_live(false);
    send_auto_poll_archive(&historic, at(15));
    historic.poll_data_channels();
    assert!(
        !historic.volumes.holds_any_still("KTLX"),
        "precondition: this is the auto-poll-while-historic arm",
    );
    assert_eq!(collected(&historic), Some(at(15)));
}

/// **A Refresh in the pre-publication window must not walk the base back.**
#[test]
fn a_refresh_in_the_pre_publication_window_does_not_walk_the_base_back() {
    let based = |app: &App| app.volumes.base_collected_at("KTLX");
    let mut app = app_showing(at(10));
    app.chunk_feeds.ensure("KTLX");
    app.volumes.install_base(
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
        squallar_egui::radar_layer::current_volume_for(app.gui.liveness(), "KTLX"),
        None,
        "precondition: nothing published yet",
    );

    send_archive_scan(&app, at(5), stamped_scan(5));
    app.poll_data_channels();
    app.push_frame_inputs();

    let stamp = squallar_egui::radar_layer::current_volume_for(app.gui.liveness(), "KTLX")
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
        pane.set_viewing_live(true);
        pane.scan_info = Some(squallar_radar::types::ScanInfo {
            site_source: squallar_radar::site_position::SitePositionSource::Table,
            site_position: None,
            site: squallar_radar::sites::RadarSite {
                name: "KTLX",
                network: squallar_radar::sites::RadarNetwork::of_id("KTLX"),
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
        squallar_egui::actions::GuiAction::NavigateTime {
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
        app.volumes.holds_any_still("KTLX"),
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
        squallar_egui::actions::GuiAction::NavigateTime {
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
    app.gui.pane_mut(0).unwrap().set_viewing_live(false);
    app.chunk_feeds.ensure("KTLX");
    assert!(app.chunks_are_feeding("KTLX"), "precondition: feed running");
    let generation = app.render.fetch_generation_for("KTLX");

    app.handle_gui_action(
        squallar_egui::actions::GuiAction::JumpToLive { pane_idx: 0 },
        None,
    );

    assert!(
        app.gui.pane(0).unwrap().viewing_live(),
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
    app.gui.pane_mut(0).unwrap().set_viewing_live(false);
    assert!(!app.chunks_are_feeding("KTLX"), "precondition: no feed");
    let generation = app.render.fetch_generation_for("KTLX");

    app.handle_gui_action(
        squallar_egui::actions::GuiAction::JumpToLive { pane_idx: 0 },
        None,
    );

    assert!(app.gui.pane(0).unwrap().viewing_live());
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
        squallar_egui::actions::GuiAction::NavigateTime {
            pane_idx: 0,
            step_secs: -600,
        },
        None,
    );

    assert!(!app.gui.pane(0).unwrap().viewing_live());
    assert!(app.gui.fetching());
    assert!(app.manual_nav_pending);
    assert_eq!(app.render.fetch_generation_for("KTLX"), generation + 1);
    let expected = chrono::TimeZone::from_utc_datetime(&chrono::Local, &at(20)).naive_local();
    assert_eq!(
        app.gui.selected_timestamp(),
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
        squallar_egui::actions::GuiAction::NavigateOneScan {
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
        squallar_egui::actions::GuiAction::NavigateOneScan {
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
    use squallar_egui::actions::GuiAction;
    use squallar_egui::pane::{LoopFrame, LoopPhase};

    let mut app = app_showing(at(10));
    let site = squallar_radar::sites::get_radar_site("KTLX").unwrap();
    {
        let mut state = squallar_egui::radar_layer::begin_loop(
            3600,
            site,
            squallar_radar::types::RenderView::PlanView,
        );
        state.phase = LoopPhase::Ready;
        state.frames = (0..3)
            .map(|i| LoopFrame {
                timestamp: at(i),
                image: None,
                render_in_flight: false,
                render_failed: false,
            })
            .collect();
        let pane = app.gui.pane_mut(0).unwrap();
        // Armed the way `App::handle_enable_loop` arms a loop: the lineage door first.
        pane.begin_or_continue_loop();
        *pane.time_state_mut(&known::RADAR) = state;
    }
    let phase = |app: &App| app.gui.pane(0).unwrap().time_state(&known::RADAR).phase;
    let frame = |app: &App| {
        app.gui
            .pane(0)
            .unwrap()
            .time_state(&known::RADAR)
            .current_frame()
    };

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

fn volume_target(collected: chrono::NaiveDateTime) -> squallar_egui::pane::VolumeTarget {
    squallar_egui::pane::VolumeTarget {
        volume: squallar_egui::pane::VolumeStamp {
            site: "KTLX".to_owned(),
            collected,
        },
        product: squallar_radar::fields::known::REFLECTIVITY,
        region: None,
    }
}

/// **A 3D pane navigated off live is served the volume it names.**
#[test]
fn a_navigated_3d_pane_is_served_the_volume_it_names() {
    let mut app = headless(TestBridge::desktop());
    app.volumes.install_base(
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
    app.handle_prepare_volume(0, &squallar_source::id::known::RADAR, navigated.clone());
    assert!(
        app.volume_store.lookup(&navigated).is_some(),
        "the pane named the volume it is showing and the host had it in hand, \
         and no build was reached — so a scrubbed 3D pane waits for ever",
    );

    let live = volume_target(newest);
    app.handle_prepare_volume(1, &squallar_source::id::known::RADAR, live.clone());
    assert!(
        app.volume_store.lookup(&live).is_some(),
        "the live target stopped being served",
    );

    let unheld = volume_target(at(30));
    app.handle_prepare_volume(2, &squallar_source::id::known::RADAR, unheld.clone());
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
    use squallar_egui::pane::{VolumeStamp, VolumeTarget};

    let target = VolumeTarget {
        volume: VolumeStamp {
            site: "KTLX".to_owned(),
            collected: chrono::NaiveDate::from_ymd_opt(2026, 7, 30)
                .expect("a real date")
                .and_hms_opt(22, 33, 0)
                .expect("a real time"),
        },
        product: squallar_radar::fields::known::REFLECTIVITY,
        region: None,
    };

    let web = |two_d: u32, three_d: u32| {
        squallar_device_profile::budget::resolve(&squallar_device_profile::budget::DeviceProfile {
            limits: squallar_device_profile::budget::BudgetLimits::WASM,
            platform: squallar_device_profile::budget::Platform::Web,
            adapter: squallar_device_profile::budget::AdapterCeilings {
                max_texture_dimension_2d: two_d,
                max_texture_dimension_3d: three_d,
            },
            ..squallar_device_profile::budget::DeviceProfile::for_target()
        })
    };
    let phone = web(2048, 256);
    let desktop = web(
        squallar_device_profile::budget::DESKTOP_CLASS_REPORT.max_texture_dimension_2d,
        squallar_device_profile::budget::DESKTOP_CLASS_REPORT.max_texture_dimension_3d,
    );

    let cells_on = |budgets: squallar_device_profile::budget::Budgets, axis: u32| {
        shaped_request(&target, 35.33, -97.28, budgets.grid_cells, axis)
            .shape
            .cells()
    };
    assert!(
        cells_on(desktop, 8192) > cells_on(phone, 256),
        "a browser on desktop-class silicon is put on the wire asking for the \
         same grid as one at the spec floor",
    );

    let call = include_str!("../app.rs")
        .split_once("let ctx = volume_job_context(")
        .map(|(_, rest)| rest.split_once(");").expect("a call site").0)
        .expect("`volume_job_context` is still called from `prepare_volume`");
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
    // The pool is sized off `self.budgets` inside `pool_for_scene`.
    for spender in ["self.pool_for_scene(", "self.budgets.quality_ceiling"] {
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

/// One shared render in the dispatcher's cache — economy for a pressure event
/// to evict. A 64 px raster, so the byte figure the cache charges is non-zero
/// and the eviction is of something rather than of nothing.
fn seed_render_cache(app: &mut App) {
    use crate::render_dispatch::CachedRenderOutput;
    let key = crate::render_key::render_cache_key(
        "KTLX",
        &squallar_radar::fields::known::REFLECTIVITY,
        squallar_radar::types::RenderView::PlanView,
        0.5,
    );
    app.render.render_cache.insert(
        key,
        CachedRenderOutput {
            surface: crate::channels::StillSurface::Raster(std::sync::Arc::new(
                egui::ColorImage::new([64, 64], vec![egui::Color32::BLACK; 64 * 64]),
            )),
            max_range_km: 230.0,
            hover: std::sync::Arc::new(squallar_radar::hover::HoverSource::empty()),
            nyquist_ms: None,
            melting_layer_source: None,
            storm_motion: None,
        },
        &[],
    );
    assert_eq!(
        app.render.render_cache.entry_count(),
        1,
        "precondition: the fixture did not land in the cache"
    );
}

/// A headless application with `panes` KTLX plan-view panes, the first
/// `looping` of them running a two-hour loop with no cadence yet — each wants
/// the whole render budget — so on the desktop bracket the scene costs
/// `looping x 36 x 16 MiB` of loop frames beside `panes x 256 MiB` of static
/// render. Restored from the store the way a real launch restores its layout.
fn app_with_looping_panes(platform: TestBridge, panes: usize, looping: usize) -> App {
    use squallar_egui::UI_CONFIG_KEY;
    use squallar_kv::KvStore;

    let store = platform.store();
    let listed = (0..panes)
        .map(|_| r#"{"site":"KTLX"}"#)
        .collect::<Vec<_>>()
        .join(",");
    store
        .store(
            UI_CONFIG_KEY,
            &format!(r#"{{"pane_count":{panes},"site":"KTLX","panes":[{listed}]}}"#),
        )
        .expect("the memory store cannot fail");
    let mut app = headless(platform);
    assert_eq!(
        app.gui.pane_count(),
        panes,
        "precondition: the fixture must really have {panes} panes",
    );
    app.render.ensure_pane_count(panes);
    let site = squallar_radar::sites::get_radar_site("KTLX").expect("KTLX is a known site");
    for idx in 0..looping {
        *app.gui.pane_mut(idx).unwrap().time_state_mut(&known::RADAR) =
            squallar_egui::radar_layer::begin_loop(
                7200,
                site,
                squallar_radar::types::RenderView::PlanView,
            );
    }
    assert_eq!(
        app.scene_of()
            .panes
            .iter()
            .filter(|pane| pane.looping)
            .count(),
        looping,
        "precondition: the scene does not see the loops the fixture started",
    );
    app
}

/// **A lost surface evicts economy, lowers the session's capacity presumption,
/// re-fits the scene to it, and writes nothing.** The headless application's
/// scene is one pane and nothing looping — 256 MiB of static render against a
/// 3840 MiB presumption — so the eviction is the whole answer: the presumption
/// comes down one economy fraction to 3456 MiB, the scene still fits it, and
/// **no rung moves**. Nothing is learned across sessions: the presumption lives
/// on the `App` for this process, and the store never hears of it.
#[test]
fn a_lost_surface_evicts_economy_and_refits_and_writes_nothing() {
    use squallar_device_profile::constants::ECONOMY_FRACTION;
    use squallar_device_profile::quality::GradientShading;
    use squallar_kv::KvStore;

    let platform = TestBridge::desktop();
    let store = platform.store();
    let mut app = headless(platform);
    seed_render_cache(&mut app);
    let before = app.budgets;
    let pool_before = app.loop_pool;
    let presumed = app.capacity().allowance();
    assert_eq!(
        before.quality_ceiling.shading,
        GradientShading::On,
        "precondition: this build's desktop bracket starts at the cloud rung",
    );
    assert_eq!(
        app.capacity_modulation.gpu_ceiling, None,
        "precondition: a fresh session holds no ceiling over the bracket"
    );
    let scene = app.scene_of();
    assert_eq!(scene.panes.len(), 1);
    assert!(scene.panes.iter().all(|pane| !pane.looping));

    app.on_pressure(crate::pressure::Pressure::SurfaceLost);

    assert_eq!(
        app.render.render_cache.entry_count(),
        0,
        "economy survived a pressure event: the render cache still holds its entry",
    );
    assert_eq!(app.render.extract_cache_len(), 0);
    assert_eq!(
        app.capacity_modulation.gpu_ceiling,
        Some(presumed / ECONOMY_FRACTION.1 * ECONOMY_FRACTION.0),
        "the session's gpu ceiling did not come down by one economy fraction",
    );
    assert_eq!(
        app.budgets, before,
        "a scene that still fits the lowered presumption was shed a rung anyway: \
         with 256 MiB of need against 3456 MiB the eviction was the whole answer",
    );
    assert_eq!(app.budgets.steps_back, 0);
    assert_eq!(
        app.loop_pool, pool_before,
        "nothing loops, so the pool had nothing to follow",
    );
    // Nothing is learned across sessions (user ruling, 2026-09-01): the
    // presumption is this process's alone, and the store has no entry to
    // carry it.
    assert_eq!(
        store.load(crate::budget_memo::BUDGET_MEMO_KEY),
        None,
        "the ladder position was written to the store",
    );
    assert_eq!(
        store.load(crate::loop_pool::LOOP_POOL_KEY),
        None,
        "the pool size was written to the store",
    );
}

/// **A scene that fits exactly, then a lost surface: the re-fit sheds the loop
/// history first.** Four two-hour loops beside two still panes cost
/// 4 x 576 + 6 x 256 = 3840 MiB — the desktop presumption to the byte — so
/// they fit at the class rung and nothing is shed. The event lowers the
/// presumption to 3456 MiB; the ladder's first three steps are 3D rungs that
/// take nothing from a 2D scene — lighting, then resolution twice — and the
/// fourth halves the loop history to 18 frames: 4 x 288 + 1536 = 2688 MiB fits. The grid, the raster and the tiles
/// are untouched, and the pool follows the loops down: min(4 x 288, 3456 -
/// 1536) = 1152 MiB.
#[test]
fn a_lost_surface_refits_a_scene_the_lowered_presumption_no_longer_holds() {
    use crate::loop_pool::GRID_BYTES;
    use squallar_device_profile::fit::{fit, need};
    use squallar_device_profile::quality::{GradientShading, ResolutionRung};

    let mut app = app_with_looping_panes(TestBridge::desktop(), 6, 4);
    let scene = app.scene_of();
    let at_the_class_rung = need(&scene, &app.budgets, GRID_BYTES).gpu_bytes;
    assert_eq!(at_the_class_rung, (4 * 36 * 16 + 6 * 256) * MIB);
    assert_eq!(
        at_the_class_rung,
        app.capacity().allowance(),
        "precondition: the scene fits the presumption exactly",
    );
    assert_eq!(
        fit(&scene, &app.device_profile, &app.capacity(), GRID_BYTES),
        app.budgets,
        "precondition: a scene that fits is left at the class rung",
    );
    let before = app.budgets;

    app.on_pressure(crate::pressure::Pressure::SurfaceLost);

    assert_eq!(
        app.budgets.steps_back, 7,
        "lighting, resolution twice, the overlay margin twice, the tiles, the \
         raster ceiling — four until 2026-09-06, when ruling 15 took the loop \
         history out of the ladder and the walk had to reach the picture",
    );
    assert_eq!(app.budgets.quality_ceiling.shading, GradientShading::Off);
    assert_eq!(
        app.budgets.quality_ceiling.resolution,
        ResolutionRung::Quarter
    );
    assert_eq!(
        app.budgets.loop_render_budget, before.loop_render_budget,
        "no governor path lowers a granted loop's frame count",
    );
    assert_eq!(
        app.budgets.raster_side_ceiling_px,
        squallar_device_profile::budget::BudgetLimits::for_target()
            .long_range_image_side_px
            .floor,
        "the raster ceiling is the rung that pays for a 2D scene now",
    );
    assert!(app.budgets.tile_whole_zoom);
    let after = need(&app.scene_of(), &app.budgets, GRID_BYTES).gpu_bytes;
    assert_eq!(after, (4 * 36 * 16 + 6 * 64) * MIB);
    assert!(after <= app.capacity().allowance());
    assert_eq!(
        app.loop_pool.bytes() as u64,
        4 * 36 * 16 * MIB,
        "the loops kept their frames: min(4 x 576, 3456 - 384)",
    );
}

/// **A reopen fits the same scene to the same budgets, whatever the store
/// holds.** Determinism replaces persistence: `fit` is pure, so two fresh
/// applications on one bracket resolve identical budgets and pools with
/// nothing remembered between them, and a stale entry from an older install is
/// ignored, not honoured — and not deleted either, since the store cannot.
///
/// The seeded pool size sits strictly between the desktop floor and ceiling,
/// so honouring it would be visible: a value at or under the floor would be
/// held to the floor and read exactly like the scene's own answer.
#[test]
fn a_reopen_fits_the_same_scene_to_the_same_budgets() {
    use crate::loop_pool::GRID_BYTES;
    use squallar_device_profile::fit::fit;
    use squallar_device_profile::quality::GradientShading;
    use squallar_kv::KvStore;

    let platform = TestBridge::desktop();
    let store = platform.store();
    store
        .store(crate::budget_memo::BUDGET_MEMO_KEY, "2")
        .expect("the memory store cannot fail");
    store
        .store(crate::loop_pool::LOOP_POOL_KEY, "1024")
        .expect("the memory store cannot fail");
    let limits =
        crate::loop_pool::LoopPoolLimits::from_budgets(&squallar_device_profile::budget::resolve(
            &squallar_device_profile::budget::DeviceProfile::for_target(),
        ));
    let seeded = 1024 * 1024 * 1024;
    assert!(
        limits.floor < seeded && seeded < limits.ceiling,
        "the fixture's stale size ({seeded}) is not strictly inside the pool \
         limits ({limits:?}), so honouring it would be invisible here",
    );

    let reopened = headless(platform);
    let fresh = headless(TestBridge::desktop());

    assert_eq!(
        reopened.budgets.steps_back, 0,
        "a stale ladder position in the store was honoured",
    );
    assert_eq!(reopened.budgets, fresh.budgets);
    assert_eq!(
        reopened.budgets.quality_ceiling.shading,
        GradientShading::On,
    );
    assert_eq!(
        reopened.capacity_modulation.gpu_ceiling, None,
        "a capacity ceiling outlived the session that learned it",
    );
    assert_eq!(
        reopened.loop_pool, fresh.loop_pool,
        "a stale pool size in the store was honoured",
    );
    assert_ne!(reopened.loop_pool.bytes(), seeded);
    // The property a memo used to carry, now carried by arithmetic: the same
    // scene against the same capacity fits to the same budgets, twice.
    let scene = reopened.scene_of();
    assert_eq!(
        scene,
        fresh.scene_of(),
        "two fresh applications see two scenes"
    );
    let first = fit(
        &scene,
        &reopened.device_profile,
        &reopened.capacity(),
        GRID_BYTES,
    );
    let second = fit(&scene, &fresh.device_profile, &fresh.capacity(), GRID_BYTES);
    assert_eq!(first, second, "one scene, one capacity, two answers");
    assert_eq!(first, reopened.budgets);
    // Ignored is not deleted: the entries are still there, and harmless.
    assert_eq!(
        store.load(crate::budget_memo::BUDGET_MEMO_KEY).as_deref(),
        Some("2"),
    );
    assert_eq!(
        store.load(crate::loop_pool::LOOP_POOL_KEY).as_deref(),
        Some("1024"),
    );
}

/// **A session that keeps failing settles at the ladder's floor rather than
/// counting for ever**, and never writes. Six two-hour loops cost 4992 MiB at
/// the class rung; each event lowers the presumption by a tenth — 3456, 3110,
/// 2799, 2519, 2267, 2041, 1837, 1653 MiB — and the re-fit follows it down the
/// ladder: 18 frames, then 9, 4 and 2, the tiles snapped, the raster to 4096,
/// **One looping pane, not six, since 2026-09-06.** Ruling 15 makes a loop's
/// frame count irreducible, so six two-hour loops need
/// 6 x (36 x 16 + 64) = 3840 MiB at the ladder's own floor — the whole desktop
/// presumption — and `refit_under_pressure`'s floor (`floor_need`, which
/// exists so a presumption never drops below a wall the scene was never going
/// to fit under) then pins the presumption where it started. That is the
/// correct answer for that scene and it leaves nothing for this test to watch:
/// what it is about is the decay's shape, so it runs on one loop, whose floor
/// need of 960 MiB sits under everything twelve events reach.
///
/// at which point the ladder has nothing left and the rung stays at its
/// length — **seven** on this bracket: lighting, resolution twice, two of the
/// overlay margin, the snap, a pinned grid that costs no step, the raster.
/// Eleven until 2026-09-06, when ruling 15 took the loop history's four
/// halvings out of it. The rung never comes back up under pressure, and the
/// presumption is exactly twelve tenths off, in integer steps.
#[test]
fn a_session_that_keeps_failing_settles_at_the_floor_and_never_writes() {
    use squallar_device_profile::constants::ECONOMY_FRACTION;
    use squallar_device_profile::fit::every_rung_at_its_stop;
    use squallar_kv::KvStore;

    let platform = TestBridge::desktop();
    let store = platform.store();
    let mut app = app_with_looping_panes(platform, 6, 1);
    let presumed = app.capacity().allowance();

    let mut rungs = Vec::new();
    for _ in 0..12 {
        app.on_pressure(crate::pressure::Pressure::SurfaceLost);
        rungs.push(app.budgets.steps_back);
    }
    let settled = app.budgets.steps_back;
    assert!(
        settled > 0 && settled < 12,
        "twelve lost surfaces resolved to rung {settled}, which is either no \
         ladder at all or a failure counter wearing one as a hat",
    );
    // Re-argued twice: when the overlay-oversampling rung landed between the
    // history and the snap, and on 2026-09-06 when ruling 15 removed the
    // history itself. A lost surface is the GPU axis, and the margin rung
    // lowers both axes — the picture is a GPU texture — so a GPU walk takes
    // its two steps (1.5x to 1.25x to 1x) straight after the resolution rungs
    // and before the tiles snap, exactly where the counted ladder has them.
    assert_eq!(
        settled, 7,
        "lighting, resolution twice, two of the overlay margin, the snap, a pinned \
         grid that costs no step, the raster: {rungs:?}",
    );
    assert_eq!(app.budgets.overlay_oversample_percent, 100);
    assert!(
        rungs.windows(2).all(|pair| pair[0] <= pair[1]),
        "the rung came back up under pressure: {rungs:?}",
    );
    assert!(every_rung_at_its_stop(
        &app.budgets,
        &app.device_profile.limits
    ));
    assert_eq!(
        store.load(crate::budget_memo::BUDGET_MEMO_KEY),
        None,
        "twelve pressure events, and one of them wrote the store",
    );
    assert_eq!(store.load(crate::loop_pool::LOOP_POOL_KEY), None);
    let shipped = squallar_device_profile::budget::BudgetLimits::for_target();
    assert_eq!(app.budgets.grid_cells, shipped.grid_cells.floor);
    assert_eq!(app.budgets.offscreen_bytes, shipped.offscreen_bytes.floor);
    assert_eq!(
        app.budgets.raster_side_ceiling_px,
        shipped.long_range_image_side_px.floor,
    );
    assert_eq!(
        app.budgets.loop_render_budget,
        squallar_device_profile::budget::resolve(&app.device_profile).loop_render_budget,
        "twelve pressure events lowered a granted loop's frame count",
    );
    assert!(app.budgets.tile_whole_zoom);
    let mut expected = presumed;
    for _ in 0..12 {
        expected = expected / ECONOMY_FRACTION.1 * ECONOMY_FRACTION.0;
    }
    assert_eq!(app.capacity_modulation.gpu_ceiling, Some(expected));
}

/// **The GPU decay has a floor: what the ladder's floor rung needs for this
/// scene.** The step is geometric with nothing under it — seven events halve
/// the presumption, thirty leave about four percent of it — and below the
/// floor rung's need the ladder has nothing left to shed, so every further
/// lowering buys no rung and only makes the readout lie about a wall the
/// scene was never going to fit under.
///
/// **One looping pane, not six, since 2026-09-06** — see
/// `a_session_that_keeps_failing_settles_at_the_floor_and_never_writes` for
/// why six no longer leaves the presumption anywhere to go.
///
/// Thirty events on one two-hour loop: the presumption settles at the figure
/// whose allowance covers the floor rung's need, stays there for the last
/// events, and the rung is at every rung's stop. Without the floor the same
/// thirty events land at `0.9^30` of the presumption — 4.2 % of 3840 MiB,
/// which is 161 MiB, well under the floor need.
#[test]
fn thirty_events_leave_the_presumption_at_the_floor_rungs_need_and_not_below() {
    use crate::loop_pool::GRID_BYTES;
    use squallar_device_profile::constants::ECONOMY_FRACTION;
    use squallar_device_profile::fit::{every_rung_at_its_stop, floor_need};

    let mut app = app_with_looping_panes(TestBridge::desktop(), 6, 1);
    let scene = app.scene_of();
    let floor = app
        .capacity()
        .gpu_bytes_for_allowance(floor_need(&scene, &app.device_profile, GRID_BYTES).gpu_bytes);
    let presumed = app.capacity().gpu_bytes;
    assert!(
        floor > 0 && floor < presumed,
        "fixture: the floor need ({floor}) must sit under the presumption ({presumed})",
    );

    let mut readings = Vec::new();
    for _ in 0..30 {
        app.on_pressure(crate::pressure::Pressure::SurfaceLost);
        readings.push(
            app.capacity_modulation
                .gpu_ceiling
                .expect("an event sets the ceiling"),
        );
    }

    assert_eq!(
        app.capacity_modulation.gpu_ceiling,
        Some(floor),
        "thirty events settled at {:?}, not at the floor rung's need {floor}: {readings:?}",
        app.capacity_modulation.gpu_ceiling,
    );
    assert!(
        readings.windows(2).all(|pair| pair[0] >= pair[1]),
        "the presumption came back up under pressure: {readings:?}",
    );
    assert!(
        every_rung_at_its_stop(&app.budgets, &app.device_profile.limits),
        "the ladder is not at its floor, so the presumption stopped early",
    );

    // What the unfloored geometric decay would have reached, so the floor is
    // shown to bite rather than to coincide with where the decay stopped.
    let mut unfloored = presumed;
    for _ in 0..30 {
        unfloored = unfloored / ECONOMY_FRACTION.1 * ECONOMY_FRACTION.0;
    }
    assert!(
        unfloored < floor,
        "the unfloored decay ({unfloored}) did not go below the floor ({floor}), \
         so this test cannot tell a floor from its absence",
    );
}

/// **The lift reaches `App::capacity()`, which is the whole point and the
/// trap.**
///
/// The figure the GPU governor writes is one `min` term on a chain of them
/// (`App::capacity`). Before this work the same figure was ALSO held by a
/// latched `session_capacity` beside it, and a governor that lifted its own
/// term while a latch held the other would have been a silent no-op: every
/// test of the governor in isolation would pass, the counters would report
/// restorations, and the capacity the scene is actually fitted against would
/// never move. So what is asserted here is not the governor's state but
/// `capacity().gpu_bytes` itself, before and after.
#[test]
fn a_dwell_with_no_new_event_gives_back_the_capacity_the_pressure_took() {
    let mut app = app_with_looping_panes(TestBridge::desktop(), 6, 1);
    let before = app.capacity().gpu_bytes;

    app.on_pressure(crate::pressure::Pressure::SurfaceLost);
    let squeezed = app.capacity().gpu_bytes;
    assert!(
        squeezed < before,
        "the event did not lower the capacity the scene is fitted against: \
         {squeezed} of {before}",
    );

    // A look at the clock with no time passed restores nothing: the dwell is
    // the rule, and a tick is only how often it is read.
    let now = web_time::Instant::now();
    app.observe_gpu_recovery(now);
    assert_eq!(
        app.capacity().gpu_bytes,
        squeezed,
        "a step came back with no quiet behind it",
    );

    app.observe_gpu_recovery(now + crate::recovery::GPU_RECOVERY_DWELL);
    assert_eq!(
        app.capacity().gpu_bytes,
        before,
        "the dwell did not give the capacity back: either the governor \
         released nothing, or a term beside it is still holding the figure \
         down",
    );
    assert_eq!(
        app.capacity_modulation.gpu_ceiling, None,
        "the last step popped left a ceiling behind",
    );
}

/// **The floor holds through as many rounds of squeeze and restore as a
/// session has, and the whole ladder comes back.**
///
/// Two properties in one walk, because they are the two halves of the same
/// claim. Going down: thirty events settle at the floor rung's need and
/// nothing takes the ceiling below it — pinned on its own by
/// `thirty_events_leave_the_presumption_at_the_floor_rungs_need_and_not_below`,
/// re-checked here after the stack has been popped and pushed again, since a
/// floor that only holds on the way down is a floor that a restoration can
/// step off.
///
/// Coming back: one step per dwell until the stack is empty, at which point
/// the session holds nothing at all against the device profile. **That is the
/// ratchet being broken**, stated as the property a reader cares about rather
/// than as a count of steps.
#[test]
fn the_floor_holds_across_rounds_of_squeeze_and_restore_and_the_ladder_comes_all_the_way_back() {
    use crate::loop_pool::GRID_BYTES;
    use crate::recovery::GPU_RECOVERY_MAX_STEPS;
    use squallar_device_profile::fit::floor_need;

    let mut app = app_with_looping_panes(TestBridge::desktop(), 6, 1);
    let scene = app.scene_of();
    let presumed = app.capacity().gpu_bytes;
    let floor = app
        .capacity()
        .gpu_bytes_for_allowance(floor_need(&scene, &app.device_profile, GRID_BYTES).gpu_bytes);
    assert!(
        floor > 0 && floor < presumed,
        "fixture: the floor need ({floor}) must sit under the capacity ({presumed})",
    );

    for _ in 0..30 {
        app.on_pressure(crate::pressure::Pressure::SurfaceLost);
    }
    assert_eq!(app.capacity_modulation.gpu_ceiling, Some(floor));
    assert_eq!(
        app.gpu_recovery.level() as usize,
        GPU_RECOVERY_MAX_STEPS,
        "thirty events did not stop stacking at the cap",
    );

    // One step per dwell, and the dwell is the capped one by now. Every
    // ceiling on the way back is at or above the floor and never below the
    // one before it.
    let mut at = web_time::Instant::now();
    let mut climb = Vec::new();
    for _ in 0..GPU_RECOVERY_MAX_STEPS {
        at += app.gpu_recovery.dwell();
        app.observe_gpu_recovery(at);
        climb.push(app.capacity().gpu_bytes);
    }
    assert!(
        climb.iter().all(|figure| *figure >= floor),
        "a restoration stepped the capacity below the floor: {climb:?}",
    );
    assert!(
        climb.windows(2).all(|pair| pair[0] <= pair[1]),
        "the capacity fell while nothing was pressing it: {climb:?}",
    );
    assert_eq!(
        app.capacity_modulation.gpu_ceiling, None,
        "sixteen dwells did not empty a sixteen-step stack",
    );
    assert_eq!(
        app.capacity().gpu_bytes,
        presumed,
        "the session still holds a figure against the device profile after \
         every step was restored",
    );

    // And the floor is still the floor: a further event steps down to it and
    // no further, from a capacity that has been all the way back up.
    for _ in 0..30 {
        app.on_pressure(crate::pressure::Pressure::SurfaceLost);
    }
    assert_eq!(
        app.capacity_modulation.gpu_ceiling,
        Some(floor),
        "the floor did not hold through a second round of events",
    );
}

/// **An out-of-memory error, however many times the device raised it in one
/// frame, is one event on that frame — one lowering of the presumption, one
/// re-fit — and writes nothing.** Four two-hour loops on six panes cost the
/// 3840 MiB presumption exactly, so the re-fit against 3456 MiB walks the
/// whole ladder to step 7; two events would have reached 3110 MiB. The next
/// frame, with nothing new noted, holds.
///
/// **Four loops, not six, since 2026-09-06**: ruling 15 makes the frame count
/// irreducible, and six of them need the whole presumption even at the
/// ladder's floor, so `refit_under_pressure`'s floor pins the presumption and
/// one event becomes indistinguishable from two.
///
/// Step 4 and a `loop_render_budget` of 18 until 2026-09-06: ruling 15 took
/// the loop history out of the ladder, so what the walk reaches for instead is
/// the picture, and it reaches the end of the ladder to do it.
///
/// The counter is process-global; this is the only test in this binary that
/// notes into it or takes from it, and the frame path that takes it never
/// runs headless.
#[test]
fn an_out_of_memory_error_refits_once_per_frame_and_writes_nothing() {
    use squallar_kv::KvStore;

    let platform = TestBridge::desktop();
    let store = platform.store();
    let mut app = app_with_looping_panes(platform, 6, 4);
    assert_eq!(app.budgets.steps_back, 0);

    squallar_gpu::pressure::note_out_of_memory();
    squallar_gpu::pressure::note_out_of_memory();
    app.absorb_gpu_pressure();
    assert_eq!(
        app.budgets.steps_back, 7,
        "two errors on one frame are one event, not two",
    );
    assert_eq!(
        app.budgets.loop_render_budget,
        squallar_device_profile::budget::resolve(&app.device_profile).loop_render_budget,
        "a pressure re-fit lowered a granted loop's frame count",
    );
    assert_eq!(
        app.capacity_modulation.gpu_ceiling,
        Some(3456 * MIB),
        "two errors on one frame lowered the gpu ceiling twice",
    );

    // The next frame, with nothing new noted: the presumption and the budgets
    // hold.
    app.absorb_gpu_pressure();
    assert_eq!(app.budgets.steps_back, 7);
    assert_eq!(app.capacity_modulation.gpu_ceiling, Some(3456 * MIB));

    assert_eq!(store.load(crate::budget_memo::BUDGET_MEMO_KEY), None);
    assert_eq!(store.load(crate::loop_pool::LOOP_POOL_KEY), None);
}

/// **A platform memory warning evicts economy and re-fits**, and what it
/// evicts leaves through the deferred-drop path rather than on this thread.
/// The headless scene fits its lowered presumption, so the eviction is the
/// whole answer and the budgets stand.
///
/// `memory_warning` itself is one line handing `Pressure::MemoryWarning` to
/// the handler; it takes an `ActiveEventLoop` no host test can build, so the
/// handler is driven directly and the wiring is read from the source.
#[test]
fn a_memory_warning_evicts_economy_and_refits() {
    use squallar_device_profile::constants::ECONOMY_FRACTION;

    let mut app = headless(TestBridge::desktop());
    seed_render_cache(&mut app);
    let before = app.budgets;
    let presumed = app.capacity().allowance();

    app.on_pressure(crate::pressure::Pressure::MemoryWarning);

    assert_eq!(
        app.render.render_cache.entry_count(),
        0,
        "the render cache survived a memory warning",
    );
    assert_eq!(
        app.capacity_modulation.gpu_ceiling,
        Some(presumed / ECONOMY_FRACTION.1 * ECONOMY_FRACTION.0),
    );
    assert_eq!(
        app.budgets, before,
        "one still pane fits the lowered presumption; the eviction was the whole answer",
    );

    let handler = include_str!("../app.rs")
        .split_once("fn memory_warning(")
        .map(|(_, rest)| rest.split_once("\n    }").expect("a body").0)
        .expect("`memory_warning` is implemented on `App`");
    assert!(
        handler.contains("Pressure::MemoryWarning"),
        "the platform's memory warning no longer reaches the pressure handler: {handler}",
    );
    let body = include_str!("../app_render.rs")
        .split_once("fn on_pressure(")
        .map(|(_, rest)| rest.split_once("\n    }").expect("a body").0)
        .expect("`on_pressure` is implemented on `App`");
    assert_eq!(
        body.matches("squallar_worker::offload::discard_each(")
            .count(),
        2,
        "an evicted cache is freed on the frame thread rather than handed to \
         the deferred-drop path: {body}",
    );
    assert!(
        body.contains("self.evict_unneeded_loop_scans();"),
        "the loop caches' sweep is no longer part of the pressure answer: {body}",
    );
}

/// **The liveness entry is rebuilt when it CHANGES, not once a frame.**
///
/// WO-E8c's payload is an `Arc` the shell re-states every frame, and the map
/// inside it is a per-site `HashMap` the frame path recomputes anyway. A
/// per-frame rebuild would be a per-frame allocation and a per-frame clone for
/// a value that moves on the order of once a scan, so the shell compares
/// before it publishes.
///
/// The instrument is pointer identity: same `Arc`, no rebuild. It has a
/// **non-triviality floor** — the second act moves the volume and asserts the
/// pointer *did* change, so a build that never published at all could not pass
/// the first half by publishing nothing.
#[test]
fn an_unchanged_liveness_answer_is_restated_and_not_rebuilt() {
    fn entry(app: &App) -> std::sync::Arc<dyn std::any::Any + Send + Sync> {
        app.gui
            .liveness()
            .iter()
            .find(|e| e.id == squallar_egui::radar_layer::POLL_LAYER)
            .expect("the radar layer publishes its liveness")
            .payload
            .clone()
    }

    let mut app = app_showing(at(10));
    app.chunk_feeds.ensure("KTLX");
    send_archive_scan(&app, at(5), stamped_scan(5));
    app.poll_data_channels();
    app.push_frame_inputs();
    let first = entry(&app);

    // A frame that changes nothing.
    app.poll_data_channels();
    app.push_frame_inputs();
    assert!(
        std::sync::Arc::ptr_eq(&first, &entry(&app)),
        "the shell rebuilt the radar layer's liveness payload on a frame \
         where nothing about it moved",
    );

    // …and one that changes something.
    send_archive_scan(&app, at(15), stamped_scan(15));
    app.poll_data_channels();
    app.push_frame_inputs();
    let moved = entry(&app);
    assert!(
        !std::sync::Arc::ptr_eq(&first, &moved),
        "a newer volume did not reach the seam — the comparison above would \
         pass on a build that never published at all",
    );
    assert_eq!(
        squallar_egui::radar_layer::current_volume_for(app.gui.liveness(), "KTLX")
            .expect("a stamp")
            .newest,
        at(15),
        "the rebuilt payload does not carry the newer volume",
    );
}

/// **The chunk feed's status reaches the chip that prints it.**
///
/// Registered as a gap this land found rather than one it made: WO-E8c's
/// tamper — the shell never publishing the status at all — came back **green**
/// on the whole workspace. The sentinel-expression contract test drives
/// `FrameInputs` by hand and so pins the `Gui`'s half; nothing pinned the
/// App's, so `drive_chunk_feeds` could compute a status and drop it on the
/// floor with every gate still reading green.
///
/// The precondition is the non-triviality floor: the resting answer is `false`
/// on both halves, so the assertion below cannot pass on a default.
#[test]
fn the_chunk_feeds_status_reaches_the_seam_that_publishes_it() {
    let mut app = app_showing(at(10));
    app.drive_chunk_feeds();
    app.push_frame_inputs();
    let resting = squallar_egui::radar_layer::chunk_status(app.gui.liveness());
    assert!(
        !resting.feeding,
        "precondition: nothing is feeding yet, or the assertion below could \
         pass off a status that was already true; got {resting:?}",
    );
    assert!(
        resting.interval_secs > 0,
        "the shell published `ChunkFeedStatus::default()` rather than the \
         status it computed — the feed's own cadence is never zero; got \
         {resting:?}",
    );

    app.chunk_feeds.ensure("KTLX");
    app.chunk_feeds
        .force_serving("KTLX", Arc::new(empty_scan()));
    app.drive_chunk_feeds();
    app.push_frame_inputs();

    let live = squallar_egui::radar_layer::chunk_status(app.gui.liveness());
    assert!(
        live.feeding,
        "the feed is serving and the status bar would still say it is not: \
         the shell computed a status and never published it; got {live:?}",
    );
}

// ── The job-shaping seam (WO-M14b-2) ────────────────────────────────────

/// **A volume-capable layer that answers "I cannot shape this".**
///
/// It publishes radar's own registered rows, so the resolution's field gate
/// passes and this fixture varies exactly one thing: what `volume_job`
/// answers. That is the arm this build's ONE implementor cannot reach —
/// radar refuses an unregistered field earlier, at the extraction gate — and
/// it is the arm a second implementor will reach first.
struct RefusingVolumeLayer;

const REFUSING_LAYER: squallar_source::id::LayerId =
    squallar_source::id::LayerId::from_static("test.refuses-to-shape");

impl squallar_source::volume::VolumeCapable for RefusingVolumeLayer {
    fn volume_job(
        &self,
        _ctx: squallar_source::volume::VolumeJobContext,
    ) -> Option<squallar_source::job::DescribedJob> {
        None
    }
}

impl squallar_overlays::render::overlay_state::OverlayHandler for RefusingVolumeLayer {
    fn id(&self) -> squallar_source::id::LayerId {
        REFUSING_LAYER
    }
    /// It holds neither frames nor items with windows — it exists to refuse a
    /// job — so `Live` is the true answer and not merely the inherited one.
    fn time_axis(&self) -> squallar_source::time::TimeAxis {
        squallar_source::time::TimeAxis::Live
    }
    fn surface(&self) -> squallar_source::handler::Surface {
        squallar_source::handler::Surface::Ground
    }
    fn draw_order_weight(&self) -> u32 {
        0
    }
    fn display_name(&self) -> &str {
        "Refusing"
    }
    fn render_mode(&self) -> squallar_source::handler::RenderMode {
        squallar_source::handler::RenderMode::Texture
    }
    fn data_generation(&self) -> u64 {
        0
    }
    fn has_data(&self, _pane: &squallar_source::handler::PaneRef<'_>) -> bool {
        true
    }
    fn is_fetching(&self) -> bool {
        false
    }
    fn set_fetching(&mut self, _fetching: bool, _pane: &squallar_source::handler::PaneRef<'_>) {}
    fn fetch_time(&self) -> Option<web_time::Instant> {
        None
    }
    fn apply_fetch_result(
        &mut self,
        _result: squallar_source::handler::FetchPayload,
        _pane: &squallar_source::handler::PaneRef<'_>,
    ) {
    }
    fn retain_selections(
        &self,
        _selections: &mut Vec<std::sync::Arc<dyn squallar_source::handler::OverlayItem>>,
        _pane: &squallar_source::handler::PaneRef<'_>,
    ) {
    }
    fn products(&self) -> &'static [squallar_source::product::ProductSpec] {
        squallar_radar::fields::products()
    }
    fn volume(&self) -> Option<&dyn squallar_source::volume::VolumeCapable> {
        Some(self)
    }
}

/// **A layer that cannot shape the job is refused into the store, not left
/// pending.**
///
/// The seam's whole point is that the frontend does not know how a volume is
/// built — so it cannot know in advance that a build is impossible, and the
/// only honest answer to `None` is a refusal the pane can show. Left as
/// `Waiting` instead, the level-trigger would re-ask every frame and pay the
/// extraction every time, for ever.
///
/// **The preconditions are asserted, not assumed**: the ask must get past the
/// budget gate and past extraction, or this would be green over an arm it
/// never reached.
#[test]
fn a_capable_layer_that_cannot_shape_a_job_is_refused_rather_than_left_pending() {
    let target = squallar_egui::pane::VolumeTarget {
        volume: squallar_egui::pane::VolumeStamp {
            site: "KTLX".to_owned(),
            collected: at(10),
        },
        product: squallar_radar::fields::known::REFLECTIVITY,
        region: None,
    };
    let mut app = headless(TestBridge::desktop());
    let mut handlers = squallar_egui::sources::all();
    handlers.push(Box::new(RefusingVolumeLayer));
    app.gui.overlays =
        squallar_overlays::render::overlay_state::OverlayRegistry::with_handlers(handlers);
    app.volumes.install_base(
        "KTLX".to_string(),
        (Arc::new(stamped_scan(10)), Default::default(), at(10)),
    );

    app.handle_prepare_volume(0, &REFUSING_LAYER, target.clone());

    assert_eq!(
        app.volume_extractions.get(),
        1,
        "precondition: the ask must have got past the budget gate and paid \
         the extraction, or the refusal below is about some earlier gate",
    );
    let entry = app
        .volume_store
        .lookup(&target)
        .expect("a layer that cannot shape a job must still answer into the store")
        .entry
        .clone();
    let squallar_volumetric::bridge::VolumeEntry::Refused(why) = entry else {
        panic!("the store answered with a build rather than the refusal the layer gave");
    };
    assert!(
        why.contains(REFUSING_LAYER.as_str()),
        "the refusal must name the layer that could not build it; got {why:?}",
    );

    // The level-trigger is quiesced: a second ask attaches to the refusal
    // rather than paying the walk again.
    app.handle_prepare_volume(0, &REFUSING_LAYER, target.clone());
    assert_eq!(
        app.volume_extractions.get(),
        1,
        "the refusal did not quiesce the level-trigger: the pane re-extracts \
         a multi-ms merged volume every frame for a build that can never \
         happen",
    );
}

/// **An asymmetric reach survives the seam with its axes in place.**
///
/// The reach crosses as a bare pair — `squallar-source` cannot name
/// `HalfExtentKm` — so it is taken apart on this side and put back together
/// on radar's. **Two swaps would cancel**, and every other region fixture in
/// this workspace is square, where neither swap is visible; this drives both
/// halves of the round trip with `east != north`.
#[test]
fn an_asymmetric_region_survives_the_seam_with_its_axes_in_place() {
    use squallar_egui::pane::{VolumeRegion, VolumeStamp, VolumeTarget};
    use squallar_geo::GeoPoint;

    let picked = VolumeRegion::new(
        GeoPoint {
            lat: 36.1,
            lon: -98.4,
        },
        squallar_radar::voxel::HalfExtentKm {
            east_km: 12.5,
            north_km: 47.5,
        },
    )
    .expect("a valid region");
    let target = VolumeTarget {
        volume: VolumeStamp {
            site: "KTLX".to_owned(),
            collected: at(10),
        },
        product: squallar_radar::fields::known::REFLECTIVITY,
        region: Some(picked),
    };
    let request = shaped_request(&target, 35.33, -97.28, SHIPPED_CELLS, DEVICE_AXIS);
    assert_eq!(
        request.half_extent_km,
        Some(squallar_radar::voxel::HalfExtentKm {
            east_km: 12.5,
            north_km: 47.5,
        }),
        "a swap on either side alone gives (47.5, 12.5); a swap on BOTH gives \
         this. MEASURED, not argued: with both sides swapped this test is \
         green and radar's own \
         `the_reach_that_crosses_the_seam_keeps_east_east_and_north_north` is \
         what fires. This pin is not sufficient on its own.",
    );
}

/// **The dispatch side names no voxel job type.**
///
/// Since WO-M14b-2 the layer hands back an envelope already shaped, and this
/// side owns the render slot, the run envelope and the reply — nothing about
/// what is being resampled. The zero is checked against a presence control on
/// the SAME scrape: the file still names other radar job rows, which are other
/// orders' business, so a zero here is a moved type rather than a needle that
/// rotted or a file the walk never read.
#[test]
fn the_voxel_dispatch_names_no_voxel_job_type() {
    let dispatch = include_str!("../render_dispatch.rs");
    assert!(
        dispatch.contains("fn spawn_voxel_build"),
        "control: the scrape is not reading the dispatch it exists to guard",
    );
    let others = dispatch.matches("squallar_radar::jobs::").count();
    assert!(
        others > 0,
        "control: the file names no radar job row at all, so the zero below \
         would be about a haystack that moved rather than about the voxel job",
    );
    assert_eq!(
        dispatch.matches("VoxelJob").count(),
        0,
        "the voxel job type is named in the dispatch again. Shaping the job \
         is the answering layer's — `VolumeCapable::volume_job` — and this \
         side receives it as a `DescribedJob` it cannot look inside.",
    );
    assert_eq!(
        dispatch.matches("VoxelRequest").count(),
        0,
        "the voxel request type is named in the dispatch again; see above.",
    );
}

/// **The field that crosses the seam is the target's own**, not a constant.
///
/// The ask names a field, the walk matched it against the answering layer's
/// own rows, and the handover has to carry that one rather than whatever the
/// 3D view happens to open on. A build of the wrong moment looks entirely
/// plausible on screen.
#[test]
fn the_field_the_seam_carries_is_the_targets_own() {
    for (id, product) in [
        (
            squallar_radar::fields::known::VELOCITY,
            squallar_radar::types::RadarProduct::Velocity,
        ),
        (
            squallar_radar::fields::known::CORRELATION_COEFFICIENT,
            squallar_radar::types::RadarProduct::CorrelationCoefficient,
        ),
    ] {
        let target = squallar_egui::pane::VolumeTarget {
            volume: squallar_egui::pane::VolumeStamp {
                site: "KTLX".to_owned(),
                collected: at(10),
            },
            product: id,
            region: None,
        };
        assert_eq!(
            shaped_request(&target, 35.33, -97.28, SHIPPED_CELLS, DEVICE_AXIS).product,
            product,
            "the handover carried a moment other than the one the target names",
        );
    }
}

/// **The volume the frontend extracted is what the layer is handed** — and the
/// proof is that the real handler accepts it.
///
/// The payload crosses as `dyn Any`, so nothing about the handover is checked
/// by the compiler: a stand-in, a stale buffer or another layer's type would
/// all compile, and every one of them would come back as a refusal that reads
/// exactly like a build the app never started. This drives the whole
/// production path — seed, extract, hand over, shape, dispatch — and asserts
/// the store is left BUILDING.
#[test]
fn the_volume_the_frontend_extracted_is_what_the_layer_is_handed() {
    let target = squallar_egui::pane::VolumeTarget {
        volume: squallar_egui::pane::VolumeStamp {
            site: "KTLX".to_owned(),
            collected: at(10),
        },
        product: squallar_radar::fields::known::REFLECTIVITY,
        region: None,
    };
    let mut app = headless(TestBridge::desktop());
    app.volumes.install_base(
        "KTLX".to_string(),
        (Arc::new(stamped_scan(10)), Default::default(), at(10)),
    );

    app.handle_prepare_volume(0, &squallar_source::id::known::RADAR, target.clone());

    assert_eq!(
        app.volume_extractions.get(),
        1,
        "precondition: the extraction must have happened, or what follows is \
         about an ask that never got that far",
    );
    let entry = app
        .volume_store
        .lookup(&target)
        .expect("the ask must have been answered into the store")
        .entry
        .clone();
    assert!(
        matches!(entry, squallar_volumetric::bridge::VolumeEntry::Building),
        "the layer did not accept the payload the frontend extracted, so the \
         pane is showing a refusal where a build belongs",
    );
}

/// One telemetry tick, asked for rather than waited on: the 2 s cadence is a
/// clock `telemetry_is_due` reads, and clearing it is what makes the next
/// report due — the same reason that predicate takes both instants.
fn tick(app: &mut App) {
    app.frame_telemetry_said = None;
    app.report_frame_telemetry();
}

const MIB: u64 = 1 << 20;

/// The linear-memory ceiling these web-bridge readings are judged against.
///
/// **A value the page reported, not a constant it looked up.** A browser
/// instance's maximum is chosen per device before the module exists
/// (`squallar-web/heap.js`), so what the app judges a reading against is
/// whatever arrived on that reading. A desktop's budget policy is the arm
/// these fixtures model; a handheld's page would carry 512 MiB here and its
/// worker 256, and every line below would fall proportionally.
const WEB_HEAP_MAX: u64 = squallar_device_profile::constants::WASM_POLICY_HEAP_BYTES;

/// What a desktop page's memory was constructed with beside that policy: the
/// top rung of `squallar-web/heap.js`'s ladder. Telemetry only — nothing
/// these tests exercise reads it, which is the property the fixtures carry.
const WEB_HEAP_RESERVED: u64 = squallar_device_profile::constants::WASM_LINKED_MAX_BYTES;

/// **On the measured arm a pressure event lowers the capacity by one economy
/// fraction of the card, and the allowance follows at three quarters.** A
/// discrete class with a 24 GiB reading: one lost surface takes this session's
/// capacity to 0.9 x 24576 = 22118 MiB and the allowance to three quarters of
/// that, 16588 MiB — one economy fraction below the 18432 MiB it allowed
/// before, which is the step the presumed arm takes too. Six two-hour loops
/// cost 4992 MiB and still fit, so nothing is shed. Lowering the allowance's
/// own figure and allowing three quarters of *that* would have compounded the
/// step to 0.675 on every event.
#[test]
fn a_pressure_event_on_the_measured_arm_lowers_the_capacity_by_one_economy_fraction() {
    use crate::platform::GpuCapacitySource;
    use squallar_device_profile::constants::{ECONOMY_FRACTION, NEED_FRACTION};
    use squallar_device_profile::quality::DeviceClass;
    use squallar_device_profile::scene::CapacitySource;

    let platform = TestBridge::desktop().with_gpu_capacity(24 << 30, GpuCapacitySource::Measured);
    let mut app = app_with_looping_panes(platform, 6, 6);
    app.device_profile.class = DeviceClass::Discrete;
    app.adopt_gpu_capacity(Some((24 << 30, GpuCapacitySource::Measured)));
    let before = app.capacity();
    assert_eq!(before.source, CapacitySource::Measured);
    assert_eq!(before.gpu_bytes, 24576 * MIB);
    assert_eq!(before.allowance(), 18432 * MIB);

    app.on_pressure(crate::pressure::Pressure::SurfaceLost);

    let after = app.capacity();
    assert_eq!(
        after.source,
        CapacitySource::Measured,
        "lowering keeps the arm"
    );
    assert_eq!(
        after.gpu_bytes,
        before.gpu_bytes / ECONOMY_FRACTION.1 * ECONOMY_FRACTION.0,
        "the capacity figure came down by one economy fraction",
    );
    assert_eq!(app.capacity_modulation.gpu_ceiling, Some(after.gpu_bytes));
    assert_eq!(
        after.allowance(),
        after.gpu_bytes / NEED_FRACTION.1 * NEED_FRACTION.0,
    );
    // In MiB, because the two orders of integer division differ by a few
    // bytes: 0.9 x 18432 = 16588.8. The compounded step would read 12441.
    assert_eq!(
        after.allowance() / MIB,
        16588,
        "the allowance fell by one economy fraction, not by one compounded with the \
         need fraction",
    );
    assert_eq!(
        before.allowance() / ECONOMY_FRACTION.1 * ECONOMY_FRACTION.0 / MIB,
        16588
    );
    // 4992 MiB of need still fits 16588 MiB: the eviction was the whole answer.
    let scene = app.scene_of();
    assert_eq!(
        squallar_device_profile::fit::need(&scene, &app.budgets, crate::loop_pool::GRID_BYTES)
            .gpu_bytes,
        4992 * MIB,
    );
    assert_eq!(app.budgets.steps_back, 0);
    assert_eq!(app.budgets.loop_render_budget, 36);
    assert_eq!(app.loop_pool.bytes() as u64, 3456 * MIB);
}

/// A heap reading with the worker instance at `worker_mib` and the page well
/// under every line, so what is judged is the worker's own watermark.
fn worker_heap(worker_mib: u64) -> crate::platform::LinearMemory {
    crate::platform::LinearMemory {
        page_bytes: 100 * MIB,
        page_max_bytes: WEB_HEAP_MAX,
        page_reserved_bytes: WEB_HEAP_RESERVED,
        worker_bytes: Some(worker_mib * MIB),
        worker_max_bytes: WEB_HEAP_MAX,
        worker_reserved_bytes: WEB_HEAP_RESERVED,
        worker_live_bytes: None,
        page_live_bytes: None,
    }
}

/// **The worker's watermark at the action line evicts economy and holds
/// every presumption**, on the telemetry tick, from a reading rather than a
/// failure — and a second tick at the same reading does nothing more, because
/// a wasm heap that has acted once would otherwise act on every tick for the
/// rest of the session. Nothing is written to the store.
///
/// Re-argued when the two instances stopped being judged as one: no lever of
/// this application reaches the worker's heap (its consumer is an MRMS grid
/// decode, answered upstream), so lowering the page's host presumption or the
/// card's for it would trade the page's picture for a wall the page is not
/// against. The economy is evicted as for any event; both presumptions stand;
/// the worker's own watch remembers the mark and the page's watch is untouched.
#[test]
fn a_heap_watermark_at_the_act_line_evicts_economy_and_lowers_the_presumption_once() {
    use squallar_kv::KvStore;

    let platform = TestBridge::web().with_linear_memory(worker_heap(891));
    let store = platform.store();
    let mut app = headless(platform);
    seed_render_cache(&mut app);
    assert_eq!(app.budgets.steps_back, 0);
    assert_eq!(app.capacity_modulation.gpu_ceiling, None);
    assert_eq!(app.capacity_modulation.host_ceiling, None);

    tick(&mut app);

    assert_eq!(
        app.capacity_modulation.gpu_ceiling, None,
        "a worker heap reading lowered the card's ceiling",
    );
    // The page's host figure moved from a session presumption to the
    // recovery's modulation ceiling (`crate::recovery`), which is the same
    // clamp on the same chain and the only one that can lift again. What is
    // asserted about a worker reading is unchanged: it moves neither.
    assert_eq!(
        app.capacity_modulation.host_ceiling, None,
        "a worker heap reading lowered the page's host ceiling",
    );
    assert!(
        !app.host_recovery.is_squeezed(),
        "a worker heap reading squeezed the page's tile economy",
    );
    assert_eq!(app.host_recovery.acts(), 0);
    assert_eq!(app.budgets.steps_back, 0);
    assert_eq!(
        app.render.render_cache.entry_count(),
        0,
        "economy survived the watermark: the render cache still holds its entry",
    );
    assert_eq!(app.worker_memory_watch.last_acted_at(), Some(891 * MIB));
    assert_eq!(
        app.linear_memory_watch,
        crate::pressure::LinearMemoryWatch::default(),
        "the page's watch moved on the worker's reading",
    );

    seed_render_cache(&mut app);
    tick(&mut app);
    assert_eq!(
        app.render.render_cache.entry_count(),
        1,
        "the same reading on the next tick evicted again",
    );

    assert_eq!(store.load(crate::budget_memo::BUDGET_MEMO_KEY), None);
    assert_eq!(store.load(crate::loop_pool::LOOP_POOL_KEY), None);
}

/// **A page heap that grows past the refire step acts again**; one that
/// grows less does not. The page instance is the one judged here, so both
/// instances are exercised across this test and the one above.
///
/// What a page-heap action moves on this bridge: the tile economies are
/// squeezed (once), the render cache goes, and the page's watch remembers
/// the mark. The host presumption does not move, and that is the bracket's
/// doing rather than the event's: the headless bridge resolves the host's
/// desktop bracket, which carries no host figure, so there is nothing to
/// hold down — `a_page_heap_event_lowers_the_host_ceiling_on_the_wasm_bracket`
/// is the arm where there is. The card's presumption is never touched by
/// a page-heap event.
#[test]
fn a_heap_that_grows_past_the_refire_step_acts_again() {
    use squallar_device_profile::linear_memory::LINEAR_MEMORY_REFIRE_STEP_BYTES;
    use squallar_kv::KvStore;

    let platform = TestBridge::web();
    let gauge = platform.linear_memory_gauge();
    let store = platform.store();
    let page = |bytes: u64| {
        Some(crate::platform::LinearMemory {
            page_bytes: bytes,
            page_max_bytes: WEB_HEAP_MAX,
            page_reserved_bytes: WEB_HEAP_RESERVED,
            worker_bytes: Some(50 * MIB),
            worker_max_bytes: WEB_HEAP_MAX,
            worker_reserved_bytes: WEB_HEAP_RESERVED,
            worker_live_bytes: None,
            page_live_bytes: None,
        })
    };
    gauge.set(page(891 * MIB));
    let mut app = headless(platform);
    seed_render_cache(&mut app);
    assert_eq!(
        app.capacity().host_bytes,
        None,
        "precondition: the desktop bracket"
    );

    tick(&mut app);
    assert_eq!(app.linear_memory_watch.last_acted_at(), Some(891 * MIB));
    assert!(
        app.host_recovery.is_squeezed(),
        "the first page-heap action did not squeeze"
    );
    assert_eq!(app.render.render_cache.entry_count(), 0);
    assert_eq!(
        app.capacity_modulation.gpu_ceiling, None,
        "a page-heap event lowered the card's ceiling"
    );
    // A bracket with no host figure has nothing to hold down, so the ceiling
    // stays absent even though the level is up: the economy squeeze is the
    // whole of what this arm's event bought.
    assert_eq!(app.capacity_modulation.host_ceiling, None);
    assert_eq!(app.worker_memory_watch.last_acted_at(), None);

    seed_render_cache(&mut app);
    gauge.set(page(891 * MIB + LINEAR_MEMORY_REFIRE_STEP_BYTES - 1));
    tick(&mut app);
    assert_eq!(
        app.render.render_cache.entry_count(),
        1,
        "growth short of the refire step acted again",
    );

    gauge.set(page(891 * MIB + LINEAR_MEMORY_REFIRE_STEP_BYTES));
    tick(&mut app);
    assert_eq!(
        app.render.render_cache.entry_count(),
        0,
        "growth past the refire step did not act again",
    );
    assert_eq!(
        app.linear_memory_watch.last_acted_at(),
        Some(891 * MIB + LINEAR_MEMORY_REFIRE_STEP_BYTES),
    );
    assert_eq!(store.load(crate::budget_memo::BUDGET_MEMO_KEY), None);
}

/// **A page-heap event lowers the host presumption, and only that one**,
/// on the bracket that has a host figure: the wasm bracket's declared 1 GiB.
///
/// **From the mark, not from the constant.** The reading is 891 MiB, so the
/// presumption becomes nine tenths of THAT (840,853,089 B) rather than nine
/// tenths of the declared ceiling (966,367,638 B), and the second event
/// takes nine tenths of the first (756,767,772 B). A wasm heap only grows,
/// so a reading is a floor under what this page has already needed, and a
/// presumption stepped down from a constant the page is nowhere near stays
/// above every need the fit can price — which is how the Tier-2 `huge` leg
/// of 2026-09-03 ran the whole ladder at rung 0 while its page sat at 1011
/// of 1024 MiB. The scene's own terms are a minority of a page heap; the
/// mark is the only figure in reach that prices the rest.
///
/// The card's presumption never moves; the squeeze fires once and the tile
/// allowances the next loop walk hands the caches are nothing but the rung.
/// The headless scene shows no picture, so its headroom is zero and the
/// action line is the percentage line — the same 891 MiB the fixed line
/// pinned. Nothing is written to the store.
///
/// **Renamed from `..._lowers_the_host_presumption_...`, and every figure in
/// it is unchanged.** The host figure moved from a latched session
/// presumption to the recovery governor's modulation ceiling
/// (`crate::recovery`) — the same `min` on the same chain, and the only term
/// that can lift again — so `session_host_capacity` became
/// `capacity_modulation.host_ceiling` and the word in the name went with it.
/// 840,853,089 and 756,767,772 are the same two bytes figures this pin
/// carried before; a latch was replaced, not an arithmetic.
#[test]
fn a_page_heap_event_lowers_the_host_ceiling_on_the_wasm_bracket() {
    use squallar_device_profile::budget::BudgetLimits;
    use squallar_device_profile::linear_memory::LINEAR_MEMORY_REFIRE_STEP_BYTES;
    use squallar_kv::KvStore;

    let platform = TestBridge::web();
    let gauge = platform.linear_memory_gauge();
    let store = platform.store();
    let page = |bytes: u64| {
        Some(crate::platform::LinearMemory {
            page_bytes: bytes,
            page_max_bytes: WEB_HEAP_MAX,
            page_reserved_bytes: WEB_HEAP_RESERVED,
            worker_bytes: None,
            worker_max_bytes: 0,
            worker_reserved_bytes: 0,
            worker_live_bytes: None,
            page_live_bytes: None,
        })
    };
    let mut app = headless(platform);
    app.device_profile.limits = BudgetLimits::WASM;
    app.adopt_budgets(squallar_device_profile::budget::resolve(
        &app.device_profile,
    ));
    assert_eq!(
        app.capacity().host_bytes,
        Some(1 << 30),
        "precondition: the page's ceiling"
    );
    assert_eq!(
        app.host_headroom_bytes, 0,
        "a scene with no picture has no batch"
    );
    let before = app.budgets;
    let tile_cache_before = app.tile_cache_budget;
    assert!(
        tile_cache_before.styled_bytes > 0,
        "precondition: an allowance to squeeze"
    );

    // `min(host, mark) / 10 * 9`, floor first. The mark is the lower of the
    // two here, so it is the mark that is stepped down — nine tenths of the
    // 891 MiB read, not nine tenths of the declared GiB.
    gauge.set(page(891 * MIB));
    tick(&mut app);
    assert_eq!(app.capacity_modulation.host_ceiling, Some(840_853_089));
    assert_eq!(
        app.capacity_modulation.gpu_ceiling, None,
        "the card's ceiling moved for the page's heap"
    );
    assert_eq!(app.capacity().host_bytes, Some(840_853_089));
    assert_eq!(
        app.capacity().gpu_bytes,
        before.app_texture_ceiling_bytes as u64
    );
    assert!(app.host_recovery.is_squeezed());
    assert_eq!(app.host_recovery.level(), 1);
    let _ = app.observe_loop_demand();
    assert_eq!(
        app.tile_cache_budget,
        squallar_device_profile::budget::TileCacheBudget {
            styled_bytes: 0,
            parsed_bytes: 0,
            terrain_bytes: 0,
            whole_zoom: tile_cache_before.whole_zoom,
        },
        "the squeeze did not hold the tile allowances at nothing",
    );
    assert_eq!(
        app.budgets, before,
        "a still pane with no picture shed a rung"
    );

    gauge.set(page(891 * MIB + LINEAR_MEMORY_REFIRE_STEP_BYTES));
    tick(&mut app);
    assert_eq!(app.capacity_modulation.host_ceiling, Some(756_767_772));
    assert_eq!(app.capacity_modulation.gpu_ceiling, None);
    assert_eq!(
        app.host_recovery.level(),
        2,
        "the second event stacked a step the first one could not release",
    );

    assert_eq!(store.load(crate::budget_memo::BUDGET_MEMO_KEY), None);
    assert_eq!(store.load(crate::loop_pool::LOOP_POOL_KEY), None);
}

/// **One scene, one rung — however much of it happens to be on the heap.**
///
/// The regression this gates was measured, not imagined. Two Tier-2 `huge`
/// passes on 2026-09-04, same bundle and same box, read `steps 0 /
/// oversample 150` and `steps 2 / oversample 100`: the fit was priced from
/// how many pictures were resident when the walk ran, and the upload drain
/// lands one band a frame, so that count passes through every value from one
/// to the layer total on its way to steady state. The user sees the
/// oversampling, so the race was visible as picture sharpness changing
/// between runs of the same scene.
///
/// The fix is the distinction, not a lock: `fit` prices what the scene SHOWS
/// (enabled texture layers, saved pane state) and the watermark judges what
/// the heap HOLDS. So the assertion is that moving the resident count — one
/// dispatch on the record, then thirteen — moves nothing about the budgets.
/// The picture footprint is read off the record too, and every plan on a
/// pane agrees by construction, so one recorded dispatch is enough to fix it
/// and the twelve that follow add nothing.
#[test]
fn the_rung_is_the_same_however_many_pictures_are_resident() {
    use squallar_device_profile::budget::BudgetLimits;

    let shown = [
        squallar_source::id::known::NWS_ALERTS,
        squallar_source::id::known::STORM_REPORTS,
        squallar_source::id::known::SPC_OUTLOOK,
        squallar_source::id::known::SPC_FIRE_OUTLOOK,
        squallar_source::id::known::SPC_DISCUSSIONS,
        squallar_source::id::known::MRMS,
        squallar_source::id::known::GMGSI,
        squallar_source::id::known::MODEL_DATA,
        squallar_source::id::known::LIGHTNING,
        squallar_source::id::known::METAR,
        squallar_source::id::known::CITY_LABELS,
        squallar_source::id::known::RADAR_SITES,
        squallar_source::id::known::RADAR_COVERAGE,
    ];
    let plan = crate::app::fetch::OverlayRenderRequest {
        geo_bounds: squallar_geo::GeoBounds {
            min_lat: 33.0,
            max_lat: 37.0,
            min_lon: -99.0,
            max_lon: -96.0,
        },
        texture: squallar_egui::overlay_cache::OverlayTexturePlan {
            width: 4317,
            height: 2416,
            overdraw: 0.5,
            pixels_per_point: 1.0,
            pane_px: [2878, 1611],
        },
        data_generation: 1,
        zoom: 32,
    };

    let mut app = headless(TestBridge::web());
    app.device_profile.limits = BudgetLimits::WASM;
    app.adopt_budgets(squallar_device_profile::budget::resolve(
        &app.device_profile,
    ));
    let pane = app
        .gui
        .pane_mut(0)
        .expect("the headless app lays out a pane");
    for id in &shown {
        pane.set_overlay_enabled(id.clone(), true);
        let _ = pane.overlay_cache_mut(id);
    }

    // One picture on the heap.
    app.render
        .record_overlay_dispatch(0, &shown[0], plan.clone());
    assert_eq!(app.render.resident_overlay_pictures().0, 1);
    let _ = app.observe_loop_demand();
    let with_one = app.budgets;
    let scene_with_one = app.scene_of();

    // Thirteen on the heap, same scene.
    for id in &shown[1..] {
        app.render.record_overlay_dispatch(0, id, plan.clone());
    }
    assert_eq!(app.render.resident_overlay_pictures().0, 13);
    let _ = app.observe_loop_demand();

    assert_eq!(
        app.scene_of().panes[0].overlay_pictures,
        scene_with_one.panes[0].overlay_pictures,
        "the scene's priced picture count moved with the upload drain, which \
         is the race: it must be the layers the pane shows",
    );
    assert_eq!(
        app.budgets.overlay_oversample_percent, with_one.overlay_oversample_percent,
        "the oversampling rung moved with how much of the scene happened to \
         be resident: the user sees this as sharpness changing between runs",
    );
    assert_eq!(
        app.budgets, with_one,
        "some budget moved with the resident count",
    );
    assert_eq!(
        scene_with_one.panes[0].overlay_pictures, 13,
        "the priced count is the thirteen layers the pane shows, not the one \
         picture that had reached the heap",
    );
}

/// **A scene of six whole-picture overlays on a wasm bracket** — five shown
/// plus the arrival in flight, on the `huge` leg's own pane and picture size
/// — built so that a page-heap event has a rung to shed and a recovery has
/// one to give back. Returns the app with the pictures recorded and one loop
/// walk taken.
///
/// **The count is chosen against the promotion rule's own arithmetic, and it
/// fell from ten shown to five on 2026-09-06** when the renderer's upload
/// queue was priced (`fit::NeedTerms::upload_pending_host`): a shown batch is
/// charged twice now, once where the dispatch holds it and once where the
/// renderer is banding it to the GPU. `promotion_qualifies` asks for
/// `allowance - need_after >= (need_after - need_now) + one refire step`,
/// which with `k` pictures of `P` bytes at each rung and `F` of fixed host
/// terms is `805,306,368 - F - 33,554,432 >= k x (2 x P150 - P125)`. On this
/// pane that is `620,756,992 >= k x 54,475,932`, so `k <= 11`: five shown
/// pictures and the arrival. At ten the scene costs 1,027,104,192 B at the
/// whole rung, over the allowance before any event, and the rule is right to
/// refuse a promotion into it — which would make these tests assert that the
/// recovery does nothing.
///
/// **THE PRODUCT CONSEQUENCE, which is a finding and not a fixture detail:
/// there is a shown-picture count above which the margin recovery can never
/// fire at all, it is derivable, and on this pane it just fell from eleven to
/// five.** A web user showing more than that on a 2878 x 1611 canvas sheds
/// the pan margin on the first page-heap event and never gets it back,
/// however much the page frees — because the promotion rule asks whether the
/// *promoted* rung would still be comfortable, and above that count it never
/// would. Nothing warns them and nothing in the readout names the count.
///
/// The arithmetic, both sides of the change. Before the upload queue was
/// priced the inequality was `805,306,368 - 67,108,864 - 33,554,432 >=
/// (N + 1) x 54,475,932`, so `N <= 11` and this fixture's ten shown pictures
/// sat one under the wall. With the batch charged twice and the parked
/// still's volume charged at all, it is `805,306,368 - 150,994,944 -
/// 33,554,432 >= (2N + 1) x 54,475,932`, so `N <= 5`. **The wall did not
/// appear; it was always there and the fixture was always just under it.**
/// What moved is the price, and with it the count at which a user meets it.
///
/// **MRMS and GMGSI are deliberately not among them, and the reason is the
/// same shape.** Those two are the gridded sources, and their source budgets
/// alone price 694,870,912 B of host need, so a scene holding them does not
/// hold its whole pan margin at ANY capacity this bracket can offer. `fit` is
/// right to keep it at the floor rung and a promotion rule is right to refuse
/// it; neither says anything about the rule these tests are for.
fn a_six_picture_web_app(platform: TestBridge) -> App {
    use squallar_device_profile::budget::BudgetLimits;

    let shown = [
        squallar_source::id::known::NWS_ALERTS,
        squallar_source::id::known::STORM_REPORTS,
        squallar_source::id::known::SPC_OUTLOOK,
        squallar_source::id::known::SPC_FIRE_OUTLOOK,
        squallar_source::id::known::SPC_DISCUSSIONS,
    ];
    let plan = crate::app::fetch::OverlayRenderRequest {
        geo_bounds: squallar_geo::GeoBounds {
            min_lat: 33.0,
            max_lat: 37.0,
            min_lon: -99.0,
            max_lon: -96.0,
        },
        texture: squallar_egui::overlay_cache::OverlayTexturePlan {
            width: 4317,
            height: 2416,
            overdraw: 0.5,
            pixels_per_point: 1.0,
            pane_px: [2878, 1611],
        },
        data_generation: 1,
        zoom: 32,
    };

    let mut app = headless(platform);
    app.device_profile.limits = BudgetLimits::WASM;
    app.adopt_budgets(squallar_device_profile::budget::resolve(
        &app.device_profile,
    ));
    let pane = app
        .gui
        .pane_mut(0)
        .expect("the headless app lays out a pane");
    for id in &shown {
        pane.set_overlay_enabled(id.clone(), true);
        let _ = pane.overlay_cache_mut(id);
    }
    for id in &shown {
        app.render.record_overlay_dispatch(0, id, plan.clone());
    }
    let _ = app.observe_loop_demand();
    app
}

/// A page reading with `page_bytes` of `byteLength` and `live` of live bytes,
/// on the desktop wall. The two are separate arguments because the whole of
/// the recovery rule turns on their being different figures: `byteLength`
/// only grows and live bytes fall.
fn page_and_live(page_bytes: u64, live: Option<u64>) -> Option<crate::platform::LinearMemory> {
    Some(crate::platform::LinearMemory {
        page_bytes,
        page_max_bytes: WEB_HEAP_MAX,
        page_reserved_bytes: WEB_HEAP_RESERVED,
        worker_bytes: None,
        worker_max_bytes: 0,
        worker_reserved_bytes: 0,
        worker_live_bytes: None,
        page_live_bytes: live,
    })
}

/// **The spike, chosen against the scene's own arithmetic.** 800 MiB of
/// `byteLength` is past the action line the whole rung's batch sets — 1024
/// MiB less the six pictures' 250,316,928 B, so 785.3 MiB — and low enough
/// that one economy fraction of it (754,974,720 B, allowing 566,231,040) no
/// longer covers the scene at the whole rung. So the event acts AND costs
/// exactly one rung: 150 % to 125 %, the scene's 609,909,312 B of host need
/// down to 469,588,428.
///
/// **Every figure here rose with the upload-queue term**, because the action
/// line is set by the batch and the ceiling has to fall below a need that now
/// charges that batch twice. The shape of the choice is unchanged.
const SPIKE: u64 = 800 * MIB;

/// A second spike past the action line the SHED rung sets — 1024 MiB less the
/// 125 % batch's 173,778,264 B, so 858.3 MiB — and more than one refire step
/// above the first.
const DEEPER_SPIKE: u64 = 880 * MIB;

/// Live bytes while the page is really full, and after it has come back. The
/// promotion threshold on this scene falls at 619.5 MiB: the released rung's
/// action line is 823,424,896 B and the margin it must leave is 173,875,316.
const LIVE_FULL: u64 = 700 * MIB;
const LIVE_RECOVERED: u64 = 60 * MIB;

/// **THE USER'S COMPLAINT, END TO END: a shed pan margin comes back.**
///
/// A page-heap spike sheds the overlay-oversampling rung, which is the margin
/// a picture is rasterised with beyond the viewport, and before this the rung
/// stayed shed for the life of the tab — "even the smallest of pans causes
/// tiles to be re-rendered and nws alerts redrawn", twice, from the user.
/// Here the spike passes, this instance's live bytes fall, and the rung comes
/// back: the ceiling lifts on the dwell's reading, the tile economies step up
/// with it, and the ladder re-fits upward on its own frame dwell with no
/// counter of its own.
///
/// Every figure that decides it is the application's: `byteLength` HOLDS at
/// its spike for the whole trace, so nothing about this recovery is visible
/// to the monotone high-water mark, and the promotion is carried entirely by
/// the falling figure.
#[test]
fn a_shed_pan_margin_comes_back_once_live_bytes_fall_and_hold() {
    let platform = TestBridge::web();
    let gauge = platform.linear_memory_gauge();
    let mut app = a_six_picture_web_app(platform);
    let whole = app.budgets.overlay_oversample_percent;
    assert_eq!(whole, 150, "precondition: the top rung's pan margin");
    assert!(app.host_headroom_bytes > 0, "precondition: a priced batch");

    // The spike. `byteLength` past the action line the scene's own batch sets.
    gauge.set(page_and_live(SPIKE, Some(LIVE_FULL)));
    tick(&mut app);
    let _ = app.observe_loop_demand();
    assert_eq!(app.host_recovery.level(), 1, "the spike shed nothing");
    assert!(
        app.capacity_modulation.host_ceiling.is_some(),
        "the ceiling was not written",
    );
    let shed = app.budgets.overlay_oversample_percent;
    assert!(
        shed < whole,
        "the spike did not cost the pan margin: {shed} percent",
    );
    assert!(
        app.host_recovery.is_squeezed(),
        "the tile economies were not squeezed",
    );

    // The recovery: `byteLength` HOLDS at the spike — the high-water mark
    // sees nothing at all — and live bytes fall. One reading short of the
    // dwell gives nothing back.
    let dwell = app.host_recovery.dwell();
    gauge.set(page_and_live(SPIKE, Some(LIVE_RECOVERED)));
    for reading in 1..dwell {
        tick(&mut app);
        let _ = app.observe_loop_demand();
        assert_eq!(
            app.host_recovery.level(),
            1,
            "the ceiling lifted on reading {reading} of a {dwell}-reading dwell",
        );
        assert_eq!(app.host_recovery.held(), reading);
    }

    tick(&mut app);
    assert_eq!(
        app.host_recovery.level(),
        0,
        "the dwell's reading gave nothing back"
    );
    assert_eq!(app.capacity_modulation.host_ceiling, None);
    assert_eq!(app.host_recovery.promotions(), 1);
    assert!(
        !app.host_recovery.is_squeezed(),
        "the tile economies did not step back up with the ceiling",
    );

    // And the ladder follows, on `refit_to_scene`'s own frame dwell: the
    // recovery writes a ceiling and nothing else, and `fit` is asked the same
    // question against a larger capacity.
    for _ in 0..squallar_device_profile::constants::LOOP_POOL_DWELL_FRAMES + 1 {
        let _ = app.observe_loop_demand();
    }
    assert_eq!(
        app.budgets.overlay_oversample_percent, whole,
        "the pan margin did not come back with the ceiling",
    );
    assert!(
        app.tile_cache_budget.styled_bytes > 0,
        "the tile economy stayed at nothing"
    );
}

/// **DEFENDS AGAINST a frozen instrument promoting by doing nothing.** The
/// dwell counts capacity READINGS, so a tick the bridge did not answer on
/// must not count toward one. `App` hands the tick's own answer to the
/// recovery rather than the remembered `page_heap_reading`, so a reader that
/// stops answering leaves the bank exactly where it was — neither advanced
/// nor spent — and a promotion still needs its full count of real
/// observations.
///
/// Value identity is deliberately NOT the test: an idle page reports the same
/// figures twice, and refusing to count a repeat would refuse to recover
/// where recovery is safest. What is required is that the observation was
/// taken now.
#[test]
fn a_tick_the_bridge_did_not_answer_on_does_not_advance_the_dwell() {
    let platform = TestBridge::web();
    let gauge = platform.linear_memory_gauge();
    let mut app = a_six_picture_web_app(platform);

    gauge.set(page_and_live(SPIKE, Some(LIVE_FULL)));
    tick(&mut app);
    let _ = app.observe_loop_demand();
    assert_eq!(
        app.host_recovery.level(),
        1,
        "precondition: a step to release"
    );

    // A reading that qualifies, banked.
    gauge.set(page_and_live(SPIKE, Some(LIVE_RECOVERED)));
    tick(&mut app);
    assert_eq!(app.host_recovery.held(), 1);

    // The instrument dies. Fifty ticks with nothing behind them.
    gauge.set(None);
    for _ in 0..50 {
        tick(&mut app);
    }
    assert_eq!(
        app.host_recovery.held(),
        1,
        "a tick with no reading behind it advanced the dwell",
    );
    assert_eq!(app.host_recovery.level(), 1, "a dead instrument promoted");

    // It comes back. The bank is where it was, and the dwell is still owed
    // its full count of real observations.
    gauge.set(page_and_live(SPIKE, Some(LIVE_RECOVERED)));
    let dwell = app.host_recovery.dwell();
    for _ in 0..dwell - 2 {
        tick(&mut app);
    }
    assert_eq!(app.host_recovery.level(), 1);
    tick(&mut app);
    assert_eq!(app.host_recovery.level(), 0);
    assert_eq!(app.host_recovery.promotions(), 1);
}

/// **DEFENDS AGAINST a dwell that slows the response to pressure.** The
/// recovery is a promotion rule and nothing else: a page-heap event still
/// takes its step on the tick it lands on, and a session that has banked most
/// of a dwell loses the bank rather than trading it against the event.
#[test]
fn pressure_still_steps_on_the_tick_it_lands_on_however_much_dwell_is_banked() {
    use squallar_device_profile::linear_memory::LINEAR_MEMORY_REFIRE_STEP_BYTES;

    let platform = TestBridge::web();
    let gauge = platform.linear_memory_gauge();
    let mut app = a_six_picture_web_app(platform);

    gauge.set(page_and_live(SPIKE, Some(LIVE_FULL)));
    tick(&mut app);
    let _ = app.observe_loop_demand();
    assert_eq!(app.host_recovery.level(), 1);
    let first = app.capacity_modulation.host_ceiling;

    // Bank one reading short of the dwell.
    let dwell = app.host_recovery.dwell();
    gauge.set(page_and_live(SPIKE, Some(LIVE_RECOVERED)));
    for _ in 0..dwell - 1 {
        tick(&mut app);
        let _ = app.observe_loop_demand();
    }
    assert_eq!(app.host_recovery.held(), dwell - 1);
    assert_eq!(app.host_recovery.level(), 1);

    // A second event, on the very next reading: the step is taken at once,
    // and the bank is gone.
    const { assert!(DEEPER_SPIKE - SPIKE >= LINEAR_MEMORY_REFIRE_STEP_BYTES) };
    gauge.set(page_and_live(DEEPER_SPIKE, Some(LIVE_FULL)));
    tick(&mut app);
    assert_eq!(app.host_recovery.level(), 2, "the dwell delayed a demotion");
    assert_eq!(app.host_recovery.acts(), 2);
    assert_eq!(
        app.host_recovery.held(),
        0,
        "a banked dwell survived an event"
    );
    assert!(
        app.capacity_modulation.host_ceiling < first,
        "the second event did not lower the ceiling further",
    );
    assert_eq!(
        app.host_recovery.dwell(),
        dwell * 2,
        "a repeat squeeze did not lengthen the dwell",
    );
}

/// **A native profile has no heap reading and is never pressured by the
/// tick.** The bridge answers the platform question — `None` — and the
/// watermark is never consulted, however many ticks pass.
#[test]
fn a_native_profile_with_no_heap_reading_is_never_pressured_by_the_tick() {
    use squallar_kv::KvStore;

    let platform = TestBridge::desktop();
    let store = platform.store();
    let mut app = headless(platform);
    assert_eq!(app.platform.linear_memory(), None, "precondition");
    seed_render_cache(&mut app);

    for _ in 0..10 {
        tick(&mut app);
    }

    assert_eq!(app.budgets.steps_back, 0);
    assert_eq!(app.render.render_cache.entry_count(), 1);
    assert_eq!(
        app.linear_memory_watch,
        crate::pressure::LinearMemoryWatch::default(),
        "the watermark moved on a bridge that reads no heap",
    );
    assert_eq!(store.load(crate::budget_memo::BUDGET_MEMO_KEY), None);
}

/// **A reading at the warning line is noted and steps nothing.** The line is
/// said once per crossing — `the_warn_line_is_said_once_per_crossing` holds
/// the once — and this holds that the tick reaches the watch and stops there.
#[test]
fn a_heap_at_the_warn_line_is_noted_and_steps_nothing() {
    use squallar_kv::KvStore;

    let platform = TestBridge::web().with_linear_memory(worker_heap(800));
    let store = platform.store();
    let mut app = headless(platform);
    seed_render_cache(&mut app);
    assert!(!app.worker_memory_watch.has_warned());

    tick(&mut app);
    tick(&mut app);

    // The worker's watch is the one that saw 800 MiB; the page's saw 100.
    assert!(
        app.worker_memory_watch.has_warned(),
        "a reading past the warning line was not noted",
    );
    assert_eq!(app.worker_memory_watch.last_acted_at(), None);
    assert!(!app.linear_memory_watch.has_warned());
    assert_eq!(app.linear_memory_watch.last_acted_at(), None);
    assert_eq!(app.budgets.steps_back, 0, "a warning stepped the ladder");
    assert_eq!(app.render.render_cache.entry_count(), 1);
    assert_eq!(store.load(crate::budget_memo::BUDGET_MEMO_KEY), None);
}

/// **A page at 90 % of its wall, with a scene whose pictures the ladder can
/// shrink, acts: it says `budget pressure:` and frees.**
///
/// The gate the `huge` leg needed and did not have. Its last telemetry read
/// `steps 0` and `oversample 150` at 1011 of 1024 MiB, and the whole page
/// path was reachable — this test is what would have gone red before that
/// leg ran rather than after.
///
/// Three things are asserted and they are three different failures. That the
/// watermark **acted at all** on the page's own instance (its watch
/// remembers the mark, and the worker's is untouched). That the levers
/// **freed something** — the render cache is emptied and the tile economies
/// squeezed, both of them page-heap bytes. And that the presumption came
/// down **from the mark**, which is the arithmetic that makes the ladder
/// converge: nine tenths of 922 MiB, not nine tenths of the declared GiB.
///
/// The scene is thirteen whole-picture layers on one 2878 x 1611 pane, the
/// `huge` leg's own, and the batch that prices the action line is what the
/// walk read off the dispatch record: fourteen pictures of 41,719,488 B, the
/// figure both legs reported and Firefox's allocation failures named. Which
/// rung a scene of that size then takes is the fit's arithmetic and is pinned
/// where the fit lives
/// (`squallar_device_profile::fit::tests::the_huge_leg_fits_at_no_host_rung_and_the_one_picture_undercount_fitted`);
/// what is pinned here is that the page path reaches it.
#[test]
fn a_page_at_ninety_percent_with_levers_says_so_and_frees_something() {
    use squallar_device_profile::budget::BudgetLimits;

    let platform = TestBridge::web();
    let gauge = platform.linear_memory_gauge();
    let mut app = headless(platform);
    app.device_profile.limits = BudgetLimits::WASM;
    app.adopt_budgets(squallar_device_profile::budget::resolve(
        &app.device_profile,
    ));

    // The leg's thirteen whole-picture layers on its one pane, recorded the
    // way the application records them.
    let huge_plan = crate::app::fetch::OverlayRenderRequest {
        geo_bounds: squallar_geo::GeoBounds {
            min_lat: 33.0,
            max_lat: 37.0,
            min_lon: -99.0,
            max_lon: -96.0,
        },
        texture: squallar_egui::overlay_cache::OverlayTexturePlan {
            width: 4317,
            height: 2416,
            overdraw: 0.5,
            pixels_per_point: 1.0,
            pane_px: [2878, 1611],
        },
        data_generation: 1,
        zoom: 32,
    };
    let shown = [
        squallar_source::id::known::NWS_ALERTS,
        squallar_source::id::known::STORM_REPORTS,
        squallar_source::id::known::SPC_OUTLOOK,
        squallar_source::id::known::SPC_FIRE_OUTLOOK,
        squallar_source::id::known::SPC_DISCUSSIONS,
        squallar_source::id::known::MRMS,
        squallar_source::id::known::GMGSI,
        squallar_source::id::known::MODEL_DATA,
        squallar_source::id::known::LIGHTNING,
        squallar_source::id::known::METAR,
        squallar_source::id::known::CITY_LABELS,
        squallar_source::id::known::RADAR_SITES,
        squallar_source::id::known::RADAR_COVERAGE,
    ];
    // **Two seedings, because the two figures have two sources on purpose.**
    // The pane's enabled slots and texture-cache keys are what the fit
    // prices — a property of the scene, which is why it does not move with an
    // upload. The dispatch record is what the telemetry reports resident and
    // what the pane's picture footprint is read from. A test that seeded only
    // the record would pass against the racing count this replaced.
    let pane = app
        .gui
        .pane_mut(0)
        .expect("the headless app lays out a pane");
    for id in &shown {
        pane.set_overlay_enabled(id.clone(), true);
        let _ = pane.overlay_cache_mut(id);
    }
    for id in &shown {
        app.render.record_overlay_dispatch(0, id, huge_plan.clone());
    }
    assert_eq!(
        app.render.resident_overlay_pictures(),
        (13, 542_353_344),
        "precondition: the leg's own picture load",
    );

    // The walk prices the batch the action line is derived from. A pane walk
    // that counted panes would put 41,719,488 x 2 here and the line 500 MB
    // too high.
    //
    // **And it is the same figure twice, from two independent seedings** —
    // thirteen shown layers priced by the fit, thirteen dispatches reported
    // resident — which is the agreement the reader needs and the racing
    // version could not promise.
    let _ = app.observe_loop_demand();
    assert_eq!(
        app.host_headroom_bytes, 584_072_832,
        "the scene's next picture batch is not fourteen pictures: this is the \
         figure the action line is the wall less, and pricing it per pane is \
         what let the `huge` leg trap under a quiet watermark",
    );
    assert_eq!(app.capacity_modulation.host_ceiling, None);
    seed_render_cache(&mut app);
    assert_eq!(app.render.render_cache.entry_count(), 1);
    assert!(app.tile_cache_budget.styled_bytes > 0);

    // Ninety percent of the page's declared wall.
    let ninety = 1024 * MIB / 10 * 9;
    gauge.set(Some(crate::platform::LinearMemory {
        page_bytes: ninety,
        page_max_bytes: WEB_HEAP_MAX,
        page_reserved_bytes: WEB_HEAP_RESERVED,
        worker_bytes: Some(50 * MIB),
        worker_max_bytes: WEB_HEAP_MAX,
        worker_reserved_bytes: WEB_HEAP_RESERVED,
        worker_live_bytes: None,
        page_live_bytes: None,
    }));
    tick(&mut app);

    assert_eq!(
        app.linear_memory_watch.last_acted_at(),
        Some(ninety),
        "the page's watermark did not act at 90 % of its wall with a 557 MiB \
         batch in front of it",
    );
    assert_eq!(
        app.worker_memory_watch.last_acted_at(),
        None,
        "the worker's watch moved on the page's reading",
    );
    assert_eq!(
        app.render.render_cache.entry_count(),
        0,
        "the action freed nothing: the render cache still holds its entry",
    );
    assert!(
        app.host_recovery.is_squeezed(),
        "the action freed nothing: the tile economies were not squeezed",
    );
    assert_eq!(
        app.capacity_modulation.host_ceiling,
        Some(ninety / 10 * 9),
        "the host ceiling was not lowered from the mark the heap reached",
    );
    assert_eq!(
        app.capacity_modulation.gpu_ceiling, None,
        "a page-heap event lowered the card's ceiling",
    );

    // The sentence the reader gets. A `budget pressure:` line with a page
    // cause, the economy it took and the rung in force — the line whose
    // absence from both `huge` legs was the first thing that was looked for.
    let line = crate::pressure::pressure_line(
        crate::pressure::Pressure::LinearMemory {
            used: ninety,
            max: WEB_HEAP_MAX,
        },
        crate::pressure::Reclaimed {
            render_entries: 1,
            render_bytes: 0,
            extracts: 0,
            tile_economy_bytes: 0,
            staging_released_bytes: 0,
            oversample_percent: app.budgets.overlay_oversample_percent,
        },
        app.budgets.steps_back,
    );
    assert!(
        line.starts_with("budget pressure: linear memory"),
        "the page's action does not announce itself as one: {line}",
    );
    assert!(line.contains("oversample "), "{line}");
}
