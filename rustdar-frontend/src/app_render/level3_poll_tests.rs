use super::stamping_tests::{SITE, app_showing_site, tilt};
use rustdar_radar::types::RadarProduct;

/// A finished fetch of one AWIPS object, as `spawn_level3_fetches` produces
/// one.
///
/// Generation 0 is what a site nothing has re-fetched carries, so nothing
/// here is discarded as stale. The object's contents are the same whichever
/// code is named: what a response is *of* is decided by the code beside it,
/// and which products that feeds is derived on arrival.
fn landed(code: &str) -> crate::channels::Level3Response {
    crate::channels::Level3Response {
        generation: 0,
        code: code.to_string(),
        site: SITE.to_string(),
        result: Ok(tilt(5, "MPX_EET_2026_07_26_01_55_52")),
    }
}

/// Every Level III result queued for a frame is taken in it.
///
/// One Level II scan spawns a fetch per distinct AWIPS code and they land in
/// a burst. Taking one per frame filled the product picker an entry per
/// redraw, and stopped filling it at all on the frame after which nothing
/// schedules another: `handle_redraw` re-arms only for a render in flight,
/// auto-poll, or an active loop, and a pane sitting on a finished scan is
/// none of those.
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

/// **One landed object offers every product it feeds.**
///
/// The picker is filled from the object's readers, not from the product a
/// fetch was spawned "for", because there is no longer one such product: the
/// single `DVL` fetch a poll issues is VIL's whole field *and* VIL density's
/// numerator. Keying this off one product would leave the other permanently
/// absent from the picker — selectable never, whatever landed.
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
    // Echo tops is listed by the fixture's scan info, as `from_scan` lists
    // every Level III product the moment a volume loads — but it does not read
    // `DVL`, so this landing must not fill its angle in. That is the half of
    // the dispatch a code-keyed fetch could get wrong in the other direction:
    // an object credited to every Level III product rather than to its readers.
    assert_eq!(
        info.product_elevations.get(&RadarProduct::EchoTops),
        None,
        "a DVL object dated echo tops, which reads EET",
    );
}

/// The de-duplication against the live bucket: one poll, one request per
/// object.
///
/// `spawn_level3_fetches` sends exactly one `Level3Response` per fetch it
/// spawns — success *and* failure, so a site that served nothing still
/// answers — which makes the responses a count of the requests that were
/// really issued. Before this, `DVL` and `EET` each arrived twice: once for
/// the single-field product and once for VIL density.
///
/// Run with:
///   cargo test -p rustdar-frontend --lib -- --ignored --nocapture live_a_poll
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
    // A duplicate would land alongside its twin, not minutes later, but give
    // the slower of a pair time to arrive before declaring there was none.
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
