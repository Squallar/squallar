use super::stamping_tests::{SITE, app_showing_site, tilt};
use rustdar_radar::types::RadarProduct;

fn landed(code: &str) -> crate::channels::Level3Response {
    crate::channels::Level3Response {
        generation: 0,
        code: code.to_string(),
        site: SITE.to_string(),
        result: Ok(tilt(5, "MPX_EET_2026_07_26_01_55_52")),
    }
}

#[test]
fn every_queued_level3_result_is_taken_in_the_frame_it_arrives_in() {
    let mut app = app_showing_site();
    for resp in [landed("DVL"), landed("DPR")] {
        app.channels.level3_sender.send(resp).unwrap();
    }

    app.poll_level3_results();

    let products = app
        .gui
        .get_scan_info_for_pane(0)
        .expect("the pane still has its scan info")
        .available_products
        .clone();
    for product in [
        RadarProduct::VerticallyIntegratedLiquid,
        RadarProduct::PrecipitationRate,
    ] {
        assert!(
            products.contains(&product),
            "{product:?} never reached the picker, so the rest of the burst \
                 is still sitting in the channel: {products:?}",
        );
    }
    assert!(
        app.channels.level3_receiver.try_recv().is_err(),
        "the frame ended with a Level III result still queued",
    );
}

#[test]
fn a_landed_object_offers_every_product_it_feeds() {
    let mut app = app_showing_site();
    app.channels.level3_sender.send(landed("DVL")).unwrap();
    app.poll_level3_results();

    let info = app
        .gui
        .get_scan_info_for_pane(0)
        .expect("the pane still has its scan info")
        .clone();
    for product in [
        RadarProduct::VerticallyIntegratedLiquid,
        RadarProduct::VilDensity,
    ] {
        assert!(
            info.available_products.contains(&product),
            "{product:?} reads DVL but never reached the picker: {:?}",
            info.available_products,
        );
        assert_eq!(
            info.product_elevations.get(&product).map(|e| e.as_slice()),
            Some(&[0.5f32][..]),
            "{product:?} must get the angle off the object's own PDB",
        );
    }
    assert_eq!(
        info.product_elevations.get(&RadarProduct::EchoTops),
        None,
        "a DVL object dated echo tops, which reads EET",
    );
}

#[ignore = "hits the live unidata-nexrad-level3 S3 bucket"]
#[test]
fn live_a_poll_fetches_each_object_once() {
    let want = RadarProduct::level3_codes_for(RadarProduct::all());
    let app = app_showing_site();
    app.spawn_level3_fetches(SITE);

    let mut codes: Vec<String> = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
    while codes.len() < want.len() && std::time::Instant::now() < deadline {
        while let Ok(resp) = app.channels.level3_receiver.try_recv() {
            println!("fetched {} for {}", resp.code, resp.site);
            codes.push(resp.code);
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    std::thread::sleep(std::time::Duration::from_secs(10));
    while let Ok(resp) = app.channels.level3_receiver.try_recv() {
        println!("fetched {} for {} (late)", resp.code, resp.site);
        codes.push(resp.code);
    }

    codes.sort();
    assert_eq!(
        codes,
        want,
        "one request per distinct object, once each — {} requests for {} \
             objects means the poll is still walking the per-product table",
        codes.len(),
        want.len(),
    );
}
