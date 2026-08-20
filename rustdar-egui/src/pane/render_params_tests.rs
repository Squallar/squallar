use super::*;
use rustdar_radar::sites::RadarSite;
use rustdar_radar::types::ScanInfo;

fn pane_listing(products: &[(RadarProduct, &[f32])]) -> PaneState {
    let mut pane = PaneState::with_site("KTLX".to_string());
    pane.scan_info = Some(ScanInfo {
        site: RadarSite {
            name: "KTLX",
            lat: 35.33,
            lon: -97.27,
            heights: None,
        },
        site_source: rustdar_radar::site_position::SitePositionSource::Table,
        site_position: None,
        timestamp: chrono::NaiveDate::from_ymd_opt(2026, 7, 26)
            .unwrap()
            .and_hms_opt(1, 48, 0)
            .unwrap(),
        vcp_number: 212,
        available_products: products.iter().map(|(p, _)| *p).collect(),
        product_elevations: products
            .iter()
            .map(|(p, angles)| (*p, angles.to_vec()))
            .collect(),
        status: String::new(),
    });
    pane
}

#[test]
fn a_selection_snaps_to_the_nearest_listed_tilt() {
    let mut pane = pane_listing(&[(RadarProduct::Reflectivity, &[0.5, 1.5, 2.4])]);
    pane.selected_elevation = 1.3;
    assert_eq!(
        pane.get_rendering_params(),
        Some((RadarProduct::Reflectivity, 1.5)),
    );
}

/// The parity case. `ScanInfo::from_scan` lists every Level III product the
/// moment a volume loads and fills its angle in only when the fetch lands, so
/// the selection must stand up before the angle does.
#[test]
fn a_listed_product_with_no_tilts_yet_still_renders_at_its_selection() {
    let mut pane = pane_listing(&[
        (RadarProduct::Reflectivity, &[0.5, 1.5]),
        (RadarProduct::EchoTops, &[]),
    ]);
    pane.selected_product = RadarProduct::EchoTops;
    pane.selected_elevation = 0.0;

    assert_eq!(
        pane.get_rendering_params(),
        Some((RadarProduct::EchoTops, 0.0)),
        "a product listed without angles must still resolve, or nothing is \
             ever dispatched for it",
    );

    pane.selected_elevation = 2.4;
    assert_eq!(
        pane.get_rendering_params(),
        Some((RadarProduct::EchoTops, 2.4)),
    );
}

/// A product the scan does not offer at all is still `None`: there is nothing
/// to render, which is a different answer from "not yet".
#[test]
fn a_product_the_scan_does_not_list_resolves_to_nothing() {
    let pane = pane_listing(&[(RadarProduct::Reflectivity, &[0.5])]);
    let mut absent = pane;
    absent.selected_product = RadarProduct::Velocity;
    assert_eq!(absent.get_rendering_params(), None);

    let empty = PaneState::with_site("KTLX".to_string());
    assert_eq!(empty.get_rendering_params(), None);
}

/// Under a loop the data line reports the playing frame, not the static render's.
#[test]
fn the_data_time_on_screen_follows_the_loop_when_one_is_running() {
    let volume = chrono::NaiveDate::from_ymd_opt(2026, 7, 26)
        .unwrap()
        .and_hms_opt(1, 48, 0)
        .unwrap();
    let frame = volume - chrono::Duration::minutes(20);

    let mut pane = PaneState::with_site("KTLX".to_string());
    pane.data_time = Some(volume);
    assert_eq!(pane.data_time_on_screen(), Some(volume), "no loop running");

    pane.loop_state = LoopPlaybackState::new_for_loop(
        600,
        &RadarSite {
            name: "KTLX",
            lat: 35.33,
            lon: -97.27,
            heights: None,
        },
        RenderView::PlanView,
    );
    assert_eq!(
        pane.data_time_on_screen(),
        None,
        "a loop with no frames yet has nothing on screen to date",
    );

    pane.loop_state.frames = vec![LoopFrame {
        timestamp: frame,
        image: None,
        render_in_flight: false,
        render_failed: false,
    }];
    assert_eq!(
        pane.data_time_on_screen(),
        Some(frame),
        "the animation's own frame, not the still it replaced",
    );
}
