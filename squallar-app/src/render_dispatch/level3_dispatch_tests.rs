use super::*;
use nexrad_level3::model::{
    DataLayer, DataPacket, Level3Message, MessageHeader, ProductDescriptionBlock, RadialPacket,
    RadialRun, SymbologyBlock,
};
use squallar_radar::level3::ProductStamp;

fn product(product_code: i16, elevation_tenths: i16, elevation_number: u16) -> Level3Product {
    let pdb = ProductDescriptionBlock {
        block_divider: -1,
        latitude: 44.849,
        longitude: -93.565,
        height: 1000,
        product_code,
        operational_mode: 2,
        vcp: 212,
        sequence_number: 0,
        volume_scan_number: 39,
        volume_scan_date: 20661,
        volume_scan_time: 7108,
        generation_date: 20661,
        generation_time: 7108,
        product_specific_1: 0,
        product_specific_2: 0,
        elevation_number,
        product_specific_3: elevation_tenths,
        thresholds: [0u16; 16],
        product_specific_47_53: [0; 7],
        version: 0,
        spot_blank: 0,
        symbology_offset: 60,
        graphic_offset: 0,
        tabular_offset: 0,
    };
    Level3Product {
        message: Level3Message {
            header: MessageHeader {
                message_code: product_code,
                date_of_message: 20661,
                time_of_message: 7108,
                message_length: 0,
                source_id: 0,
                destination_id: 0,
                number_of_blocks: 3,
            },
            pdb,
            symbology: Some(SymbologyBlock {
                block_id: 1,
                block_length: 0,
                num_layers: 1,
                layers: vec![DataLayer {
                    layer_length: 0,
                    packets: vec![DataPacket::DigitalRadial(RadialPacket {
                        first_range_bin: 0,
                        num_range_bins: 2,
                        i_center: 0,
                        j_center: 0,
                        scale_factor: 0.999,
                        is_legacy: false,
                        xdr_data_scale: None,
                        xdr_data_offset: None,
                        radials: vec![RadialRun {
                            start_angle: 0.0,
                            angle_delta: 1.0,
                            gate_values: vec![129, 140],
                        }],
                    })],
                }],
            }),
        },
        stamp: ProductStamp::from_key("MPX_EET_2026_07_26_01_55_52"),
        bytes: std::sync::Arc::new(Vec::new()),
    }
}

fn cache(d: &mut RenderDispatcher, code: &str, site: &str, l3: Level3Product) {
    d.cache_level3(code.to_string(), site.to_string(), l3);
}

#[test]
fn every_level3_product_resolves_from_its_cache() {
    let mut d = RenderDispatcher::new();
    for (radar_product, code, product_code) in [
        (RadarProduct::SpecificDifferentialPhase, "N0K", 163i16),
        (RadarProduct::EchoTops, "EET", 135),
        (RadarProduct::VerticallyIntegratedLiquid, "DVL", 134),
        (RadarProduct::PrecipitationRate, "DPR", 176),
    ] {
        let p = product(product_code, 5, 1);
        cache(&mut d, code, "KMPX", p);
        let picked = d
            .nearest_tilt(radar_product, "KMPX", 0.5)
            .unwrap_or_else(|| panic!("{code} must render"));
        assert_eq!(picked.message.pdb.product_code, product_code);
    }
    for (radar_product, code, product_code) in [
        (RadarProduct::SpecificDifferentialPhase, "N0K", 163i16),
        (RadarProduct::EchoTops, "EET", 135),
        (RadarProduct::VerticallyIntegratedLiquid, "DVL", 134),
        (RadarProduct::PrecipitationRate, "DPR", 176),
    ] {
        assert_eq!(
            d.nearest_tilt(radar_product, "KMPX", 0.5)
                .map(|p| p.message.pdb.product_code),
            Some(product_code),
            "{} resolved something other than its own {code}",
            radar_product.name(),
        );
    }
    assert!(
        d.nearest_tilt(RadarProduct::StormRelativeVelocity, "KMPX", 0.5)
            .is_none(),
        "nothing was cached for SRV, and nothing ever is: it derives from Level II",
    );

    for p in RadarProduct::all().iter().filter(|p| p.is_level3()) {
        let codes = p
            .level3_products()
            .unwrap_or_else(|| panic!("{} is Level III but names no codes", p.name()));
        if codes.len() == 1 {
            assert!(
                d.nearest_tilt(*p, "KMPX", 0.5).is_some(),
                "{} names one object but did not resolve by nearest tilt",
                p.name(),
            );
        } else {
            assert_eq!(
                *p,
                RadarProduct::VilDensity,
                "{} names {codes:?} — a new multi-object product needs a \
                     resolution path in `try_spawn_level3_render`",
                p.name(),
            );
        }
    }
}

#[test]
fn vil_density_needs_both_of_its_objects() {
    let mut d = RenderDispatcher::new();
    assert!(
        d.cached_by_code(RadarProduct::VilDensity, "KMPX", "DVL")
            .is_none(),
        "nothing cached yet",
    );

    cache(&mut d, "DVL", "KMPX", product(134, 0, 0));
    assert_eq!(
        d.cached_by_code(RadarProduct::VilDensity, "KMPX", "DVL")
            .map(|p| p.message.pdb.product_code),
        Some(134),
    );
    assert!(
        d.cached_by_code(RadarProduct::VilDensity, "KMPX", "EET")
            .is_none(),
        "the denominator has not landed — nothing to divide by",
    );

    cache(&mut d, "EET", "KMPX", product(135, 0, 0));
    assert_eq!(
        d.cached_by_code(RadarProduct::VilDensity, "KMPX", "EET")
            .map(|p| p.message.pdb.product_code),
        Some(135),
    );
    assert_eq!(
        d.cached_by_code(RadarProduct::VilDensity, "KMPX", "DVL")
            .map(|p| p.message.pdb.product_code),
        Some(134),
    );

    assert!(
        d.cached_by_code(RadarProduct::VilDensity, "KTLX", "DVL")
            .is_none(),
    );

    assert!(!RadarProduct::VilDensity.reads_whole_volume());
    assert!(RadarProduct::VilDensity.is_level3());
}

#[test]
fn one_object_serves_every_product_that_reads_it() {
    let mut d = RenderDispatcher::new();
    cache(&mut d, "DVL", "KMPX", product(134, 0, 0));
    cache(&mut d, "EET", "KMPX", product(135, 0, 0));

    let vil = d
        .nearest_tilt(RadarProduct::VerticallyIntegratedLiquid, "KMPX", 0.5)
        .expect("VIL reads DVL");
    let numerator = d
        .cached_by_code(RadarProduct::VilDensity, "KMPX", "DVL")
        .expect("VIL density's numerator is the same DVL");
    assert!(
        Arc::ptr_eq(&vil, &numerator),
        "VIL and VIL density resolved different DVL objects, so the poll is \
             still fetching it twice",
    );

    let eet = d
        .nearest_tilt(RadarProduct::EchoTops, "KMPX", 0.5)
        .expect("echo tops reads EET");
    let denominator = d
        .cached_by_code(RadarProduct::VilDensity, "KMPX", "EET")
        .expect("VIL density's denominator is the same EET");
    assert!(Arc::ptr_eq(&eet, &denominator));

    assert!(
        d.cached_by_code(RadarProduct::EchoTops, "KMPX", "DVL")
            .is_none(),
        "echo tops names EET only — DVL is not its field to draw",
    );
    assert!(
        d.cached_by_code(RadarProduct::VerticallyIntegratedLiquid, "KMPX", "EET")
            .is_none(),
    );
    assert!(
        d.nearest_tilt(RadarProduct::PrecipitationRate, "KMPX", 0.5)
            .is_none(),
        "no DPR landed, and neither DVL nor EET stands in for one",
    );
    assert!(
        d.nearest_tilt(RadarProduct::Reflectivity, "KMPX", 0.5)
            .is_none(),
        "a Level II product names no codes and resolves nothing here",
    );
}

#[test]
fn a_tilt_is_never_taken_from_another_site() {
    let mut d = RenderDispatcher::new();
    cache(&mut d, "EET", "KMPX", product(135, 5, 1));
    let mut other = product(135, 5, 1);
    other.message.pdb.volume_scan_time = 9999;
    cache(&mut d, "EET", "KFSD", other);

    let picked = d
        .nearest_tilt(RadarProduct::EchoTops, "KFSD", 0.5)
        .expect("KFSD has an EET");
    assert_eq!(
        picked.message.pdb.volume_scan_time, 9999,
        "took KMPX's product"
    );
    assert!(
        d.nearest_tilt(RadarProduct::EchoTops, "KABR", 0.5)
            .is_none()
    );
    assert!(
        d.nearest_tilt(RadarProduct::PrecipitationRate, "KMPX", 0.5)
            .is_none()
    );
}

#[test]
fn two_resolvable_objects_pick_the_same_one_every_time() {
    for round in 0..60 {
        let mut d = RenderDispatcher::new();
        let mut inputs = [("DVL", 134i16), ("EET", 135)];
        if round % 2 == 1 {
            inputs.reverse();
        }
        for (code, product_code) in inputs {
            cache(&mut d, code, "KMPX", product(product_code, 0, 0));
        }
        assert_eq!(
            d.nearest_tilt(RadarProduct::VilDensity, "KMPX", 0.0)
                .expect("both of VIL density's inputs are cached")
                .message
                .pdb
                .product_code,
            134,
            "round {round}: VIL density must date itself from the numerator \
                 every time, not from whichever input the hash happened to yield",
        );

        let mut d = RenderDispatcher::new();
        let mut cuts = [("DVL", 9u16), ("EET", 3)];
        if round % 2 == 1 {
            cuts.reverse();
        }
        for (code, elev_num) in cuts {
            cache(&mut d, code, "KMPX", product(135, 13, elev_num));
        }
        assert_eq!(
            d.nearest_tilt(RadarProduct::VilDensity, "KMPX", 1.3)
                .expect("both objects are at 1.3°")
                .message
                .pdb
                .elevation_number,
            3,
            "round {round}: the lower cut number must break the tie ahead of \
                 the code",
        );
    }
}

fn rendered(product: RadarProduct, elevation: f32) -> CachedPaneRender {
    CachedPaneRender {
        image: Arc::new(egui::ColorImage::default()),
        max_range_km: 230.0,
        hover: Arc::new(squallar_radar::hover::HoverSource::empty()),
        product,
        elevation,
        nyquist_ms: None,
        melting_layer_source: None,
        storm_motion: None,
    }
}

fn pane_on(site: &str) -> squallar_egui::pane::PaneState {
    squallar_egui::pane::PaneState::with_site(site.to_string())
}

fn volume_time() -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 7, 26)
        .unwrap()
        .and_hms_opt(1, 48, 0)
        .unwrap()
}

fn pane_with_volume(site: &str) -> squallar_egui::pane::PaneState {
    let mut pane = pane_on(site);
    pane.scan_info = Some(squallar_radar::types::ScanInfo {
        site_source: squallar_radar::site_position::SitePositionSource::Table,
        site_position: None,
        site: squallar_radar::sites::get_radar_site(site)
            .cloned()
            .unwrap_or(squallar_radar::sites::RadarSite {
                name: "KMPX",
                network: squallar_radar::sites::RadarNetwork::of_id("KMPX"),
                lat: 44.849,
                lon: -93.565,
                heights: None,
            }),
        timestamp: volume_time(),
        vcp_number: 212,
        available_products: vec![RadarProduct::Reflectivity],
        product_elevations: HashMap::new(),
        status: String::new(),
    });
    pane
}

#[test]
fn a_render_stamps_its_pane_with_its_own_datas_time() {
    let mut d = RenderDispatcher::new();
    cache(&mut d, "EET", "KMPX", product(135, 5, 1));

    let pane = pane_with_volume("KMPX");
    let l3_time = d
        .data_time_for_render(&pane, &rendered(RadarProduct::EchoTops, 0.5))
        .expect("the EET stamp is readable");
    assert_ne!(
        l3_time,
        volume_time(),
        "the object's own time, not the volume it sits beside",
    );

    let elsewhere = pane_with_volume("KTLX");
    assert_eq!(
        d.data_time_for_render(&elsewhere, &rendered(RadarProduct::EchoTops, 0.5)),
        None,
        "another site's products are not this pane's, and its volume is not a \
             substitute for the object it has not got",
    );

    let srv = pane_with_volume("KMPX");
    assert_eq!(
        d.data_time_for_render(&srv, &rendered(RadarProduct::EchoTops, 0.5)),
        Some(l3_time),
        "precondition: it was dated",
    );
    assert_eq!(
        d.data_time_for_render(&srv, &rendered(RadarProduct::StormRelativeVelocity, 0.5)),
        Some(volume_time()),
        "SRV derives from the Level II volume, so that is the age of what is drawn",
    );
}

#[test]
fn an_unreadable_key_reports_no_time_rather_than_the_volumes() {
    let mut d = RenderDispatcher::new();
    let mut p = product(135, 5, 1);
    p.stamp = ProductStamp::from_key("not-a-key");
    cache(&mut d, "EET", "KMPX", p);

    assert!(
        d.nearest_tilt(RadarProduct::EchoTops, "KMPX", 0.5)
            .is_some(),
        "precondition: the product is still drawn — an unreadable key is worth \
             rendering, just not worth dating",
    );
    let pane = pane_with_volume("KMPX");
    assert!(
        pane.scan_info.is_some(),
        "precondition: a volume time is in reach and must not be borrowed",
    );
    assert_eq!(
        d.data_time_for_render(&pane, &rendered(RadarProduct::EchoTops, 0.5)),
        None,
    );
}

#[test]
fn the_override_routes_into_the_level2_render_params() {
    let mut d = RenderDispatcher::new();
    assert_eq!(d.storm_motion_override_kt(), None, "no override, Bunkers");

    d.set_storm_motion_choice_default(Some(
        StormMotionSample::user_override(45.0, 210.0).expect("finite"),
    ));
    assert_eq!(d.storm_motion_override_kt(), Some((45.0, 210.0)));

    d.set_storm_motion_choice_default(None);
    assert_eq!(d.storm_motion_override_kt(), None);
}

#[test]
fn changing_the_override_invalidates_the_storm_relative_renders() {
    let mut d = RenderDispatcher::new();
    d.ensure_pane_count(3);
    d.pane_render[0].last_rendered = Some((RadarProduct::StormRelativeVelocity, 1.3));
    d.pane_render[1].last_rendered = Some((RadarProduct::Reflectivity, 0.5));
    d.pane_render[2].last_rendered = Some((RadarProduct::StormRelativeVelocity, 2.4));
    d.cache_render(
        "KMPX",
        RadarProduct::StormRelativeVelocity,
        squallar_radar::types::RenderView::PlanView,
        1.3,
        output(),
    );
    d.cache_render(
        "KMPX",
        RadarProduct::Reflectivity,
        squallar_radar::types::RenderView::PlanView,
        0.5,
        output(),
    );

    assert!(d.set_storm_motion_choice_default(Some(
        StormMotionSample::user_override(30.0, 240.0).expect("finite")
    )));
    assert_eq!(d.pane_render[0].last_rendered, None);
    assert_eq!(
        d.pane_render[1].last_rendered,
        Some((squallar_radar::types::RadarProduct::Reflectivity, 0.5)),
        "an unrelated product must not be re-rendered",
    );
    assert_eq!(d.pane_render[2].last_rendered, None);
    assert!(
        d.get_cached_render(
            "KMPX",
            RadarProduct::StormRelativeVelocity,
            squallar_radar::types::RenderView::PlanView,
            1.3
        )
        .is_none(),
        "the shared cache is keyed on (site, product, elevation), which the vector is \
             not part of, so a stale entry would be handed straight back",
    );
    assert!(
        d.get_cached_render(
            "KMPX",
            RadarProduct::Reflectivity,
            squallar_radar::types::RenderView::PlanView,
            0.5
        )
        .is_some()
    );
}

#[test]
fn an_unchanged_override_invalidates_nothing() {
    let mut d = RenderDispatcher::new();
    d.ensure_pane_count(1);
    let o = Some(StormMotionSample::user_override(30.0, 240.0).expect("finite"));
    assert!(d.set_storm_motion_choice_default(o));
    d.pane_render[0].last_rendered = Some((RadarProduct::StormRelativeVelocity, 1.3));
    assert!(!d.set_storm_motion_choice_default(o));
    assert_eq!(
        d.pane_render[0].last_rendered,
        Some((RadarProduct::StormRelativeVelocity, 1.3))
    );
    assert!(d.set_storm_motion_choice_default(None));
    assert_eq!(d.pane_render[0].last_rendered, None);
}

fn output() -> CachedRenderOutput {
    CachedRenderOutput {
        image: Arc::new(egui::ColorImage::default()),
        max_range_km: 230.0,
        hover: Arc::new(squallar_radar::hover::HoverSource::empty()),
        nyquist_ms: None,
        melting_layer_source: None,
        storm_motion: None,
    }
}
