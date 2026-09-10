//! **What a pane parked in the past costs, and what the fires-counter reads.**
//!
//! Two questions this binary exists to answer with a reading rather than an
//! argument, both of which need a process nothing else polls GLM in:
//!
//! 1. **The held bytes**, before and after the demand-ordered eviction landed.
//!    This is a memory campaign and the fix widens what a parked pane retains
//!    from nothing at all to its own window, so that widening has to be priced.
//!    Blocks and bytes are reported **separately** and never added — they are
//!    different currencies.
//! 2. **The fires-counter**, which the process-global gauge cannot answer in
//!    the library's own test binary: `squallar-overlays`' unit tests poll GLM
//!    concurrently, and a counter that only ever adds turns any neighbour's
//!    poll into a false green. Here the only GLM poll is this one's.
//!
//! The allocator is `squallar_alloc::Counting`, the one the shipped binaries
//! install, so `live_bytes()` is a real `GlobalAlloc` reading that knows
//! nothing about GLM and is able to disagree with the fix.

#![cfg(not(target_arch = "wasm32"))]

use chrono::{NaiveDateTime, TimeDelta};
use squallar_overlays::glm::fetch::{
    FLASH_BYTES, GlmCache, MAX_RETAINED_FLASHES, gauge, poll_glm_into_store,
};
use squallar_overlays::glm::{GlmDataLevel, GlmFlash, GlmSatellite};

#[global_allocator]
static ALLOCATOR: squallar_alloc::Counting = squallar_alloc::Counting;

const PER_GRANULE: usize = 80_000;

fn day() -> chrono::NaiveDate {
    chrono::NaiveDate::from_ymd_opt(2026, 4, 27).unwrap()
}

fn granule_key(start: NaiveDateTime) -> String {
    format!(
        "GLM-L2-LCFA/{}/OR_GLM-L2-LCFA_G19_s{}0_e{}0_c{}0.nc",
        start.format("%Y/%j/%H"),
        start.format("%Y%j%H%M%S"),
        (start + TimeDelta::seconds(20)).format("%Y%j%H%M%S"),
        (start + TimeDelta::seconds(21)).format("%Y%j%H%M%S"),
    )
}

fn flash_at(time: NaiveDateTime) -> GlmFlash {
    GlmFlash {
        lat: 35.0,
        lon: -97.0,
        energy: 1.0e-14,
        area: 128.0,
        time,
        satellite: GlmSatellite::GoesEast,
        level: GlmDataLevel::Flash,
    }
}

/// A granule body carrying one flash, ten seconds into the span `start` opens.
fn one_flash_granule(start: NaiveDateTime) -> Vec<u8> {
    let mut file = hdf5_pure::FileBuilder::new();
    file.set_attr(
        "time_coverage_start",
        hdf5_pure::AttrValue::String(format!("{}Z", start.format("%Y-%m-%dT%H:%M:%S.0"))),
    );
    {
        let mut put = |name: &str, value: f32, units: Option<&str>| {
            let var = file.create_dataset(name);
            var.with_f32_data(&[value]);
            if let Some(u) = units {
                var.set_attr("units", hdf5_pure::AttrValue::String(u.into()));
            }
        };
        put("flash_lat", 35.0, None);
        put("flash_lon", -97.0, None);
        put("flash_energy", 1.0e-14, Some("J"));
        put("flash_area", 128.0, Some("km2"));
        put("flash_time_offset_of_first_event", 10.0, None);
    }
    file.finish().expect("write granule")
}

fn http_response(status_line: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status_line}\r\nContent-Type: application/xml\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len(),
    )
}

fn archive_reply(line: &str, granules: &[(String, Vec<u8>)]) -> Vec<u8> {
    let path = line.split_whitespace().nth(1).unwrap_or("");
    if let Some(rest) = path.split("prefix=").nth(1) {
        let prefix = rest.split('&').next().unwrap_or("");
        let mut body = String::from(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
             <ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
             <Name>bucket</Name><IsTruncated>false</IsTruncated>",
        );
        for key in granules
            .iter()
            .map(|(k, _)| k)
            .filter(|k| k.starts_with(prefix))
        {
            body.push_str(&format!(
                "<Contents><Key>{key}</Key><Size>1</Size></Contents>"
            ));
        }
        body.push_str("</ListBucketResult>");
        return http_response("200 OK", &body).into_bytes();
    }
    match granules.iter().find(|(k, _)| path.ends_with(k.as_str())) {
        Some((_, bytes)) => {
            let mut out = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n",
                bytes.len(),
            )
            .into_bytes();
            out.extend_from_slice(bytes);
            out
        }
        None => http_response("404 Not Found", "<Error/>").into_bytes(),
    }
}

fn s3_archive(granules: Vec<(String, Vec<u8>)>) -> squallar_source::origins::DataSources {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let port = listener.local_addr().expect("local addr").port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut scratch = [0u8; 8192];
            let read = stream.read(&mut scratch).unwrap_or(0);
            let request = String::from_utf8_lossy(&scratch[..read]).to_string();
            let line = request.lines().next().unwrap_or("").to_string();
            let _ = stream.write_all(&archive_reply(&line, &granules));
            let _ = stream.flush();
        }
    });
    squallar_source::origins::DataSources {
        goes_east_bucket: "east".into(),
        goes_west_bucket: "west".into(),
        s3_base: format!("http://127.0.0.1:{port}/{{bucket}}").into(),
        ..squallar_source::origins::DataSources::production()
    }
}

fn loopback_client() -> reqwest::Client {
    squallar_source::tls::init();
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .expect("client")
}

/// **The parked pane's price, and the ceiling that bounds it.**
///
/// The scene is the rig's: a store already carrying granules newer than
/// anything the parked pane depicts, and a pane parked at 06:00Z asking for its
/// own window. Before this landing that pane retained **nothing** of what it
/// downloaded — 84 zero-flash deliveries out of 85 — so the honest before/after
/// on held bytes is "nothing" against "its own window", and what has to be
/// named is the ceiling on the second figure.
///
/// **The ceiling is unchanged and it is what bounds this.** Demand reorders
/// eviction; `evict_oldest_over` still loops on `total > cap`. So the bound on
/// a parked pane after this change is the same `MAX_RETAINED_FLASHES *
/// FLASH_BYTES` it was before, and the reading below is against that number
/// rather than against an argument that the code path is unchanged.
#[test]
fn a_parked_pane_is_lit_and_priced_against_the_ceiling_it_may_not_pass() {
    let parked_at = day().and_hms_opt(6, 0, 0).unwrap();
    let live_starts = [
        day().and_hms_opt(12, 0, 0).unwrap(),
        day().and_hms_opt(12, 30, 0).unwrap(),
        day().and_hms_opt(13, 0, 0).unwrap(),
        day().and_hms_opt(13, 30, 0).unwrap(),
    ];

    let store = squallar_overlays::glm::fetch::GlmStore::default();
    store.with_mut(|cache: &mut GlmCache| {
        for start in live_starts {
            let flashes: Vec<GlmFlash> = (0..PER_GRANULE)
                .map(|i| flash_at(start + TimeDelta::milliseconds(i as i64)))
                .collect();
            cache.insert(granule_key(start), start, flashes);
        }
    });
    let seeded = store.with_mut(|c| c.flash_count());
    assert!(
        seeded > MAX_RETAINED_FLASHES,
        "non-triviality floor: the seed must overfill the ceiling, or the \
         parked pane's arrivals fit beside it whatever the order is",
    );

    let archived = [
        parked_at - TimeDelta::seconds(40),
        parked_at - TimeDelta::seconds(20),
        parked_at,
    ];
    let sources = s3_archive(
        archived
            .iter()
            .map(|&start| (granule_key(start), one_flash_granule(start)))
            .collect(),
    );
    let client = loopback_client();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");

    // ── The reading, on the process's own allocator ──
    let bytes_before = squallar_alloc::live_bytes().expect("this binary installs the counter");
    let blocks_before = squallar_alloc::live_large_blocks();
    let held_before = store.retained_bytes();
    let fires_before = gauge::read().20;

    let outcome = runtime
        .block_on(poll_glm_into_store(
            &store,
            &client,
            &sources,
            &[GlmSatellite::GoesEast],
            &[GlmDataLevel::Flash],
            parked_at,
            squallar_source::time::Residency::over([(
                parked_at - TimeDelta::seconds(1860),
                parked_at + TimeDelta::seconds(1800),
            )]),
            1,
        ))
        .expect("the archive listing answered");

    let bytes_after = squallar_alloc::live_bytes().expect("still counting");
    let blocks_after = squallar_alloc::live_large_blocks();
    let held_after = store.retained_bytes();
    let fires_after = gauge::read().20;

    // **Two currencies, reported apart and never summed.**
    println!(
        "parked pane: live_bytes {bytes_before} -> {bytes_after} B; \
         live_large_blocks {blocks_before} -> {blocks_after}; \
         store retained {held_before} -> {held_after} B; \
         delivered {} flashes; fires-counter {fires_before} -> {fires_after}",
        outcome.flashes.len(),
    );

    assert_eq!(
        outcome.flashes.len(),
        archived.len(),
        "the parked pane drew {} of the {} granules served inside its window: \
         the reading below would price a picture with holes in it",
        outcome.flashes.len(),
        archived.len(),
    );

    // ── The named ceiling ──
    assert!(
        held_after <= MAX_RETAINED_FLASHES * FLASH_BYTES,
        "the store holds {held_after} B against the ceiling of {} B \
         ({MAX_RETAINED_FLASHES} rows at {FLASH_BYTES} B). A parked pane that \
         is lit and unbounded is not a fix",
        MAX_RETAINED_FLASHES * FLASH_BYTES,
    );

    // ── The fires-counter, in the only process that can answer for it ──
    //
    // The gauge is process-global and only ever adds, so a neighbour's poll
    // would turn any delta into a false green. This binary polls GLM exactly
    // once, which is what makes the figure this poll's own.
    assert!(
        fires_after > fires_before,
        "the fires-counter read {fires_before} -> {fires_after}: this poll \
         admitted no granule that an oldest-first trim would have refused, so \
         either its precondition did not hold on this arm or the mechanism did \
         not fire. A cut whose counter cannot report its own absence is the \
         cut that ships a nothing",
    );
}

/// **The same scene under the order this file shipped with** — the before, in
/// the same binary and against the same allocator as the after.
///
/// An empty demand is not a *model* of the shipped behaviour, it **is** it:
/// `shares` returns an empty map so every granule ranks unwanted at equal
/// depth and the sort key collapses to `(newest, key)` ascending; `demanded`
/// answers `false` for everything so `FloorCell::refuses` applies its mark
/// exactly as before; and only unwanted evictions raise the floor, which under
/// an empty demand is all of them. Every branch this landing added is off.
///
/// So the pair of readings this file prints is one scene measured twice, which
/// is the only kind of before/after that prices anything in this app.
#[test]
fn the_shipped_order_leaves_the_same_parked_pane_dark() {
    let parked_at = day().and_hms_opt(6, 0, 0).unwrap();
    let live_starts = [
        day().and_hms_opt(12, 0, 0).unwrap(),
        day().and_hms_opt(12, 30, 0).unwrap(),
        day().and_hms_opt(13, 0, 0).unwrap(),
        day().and_hms_opt(13, 30, 0).unwrap(),
    ];
    let mut cache = GlmCache::default();
    for start in live_starts {
        let flashes: Vec<GlmFlash> = (0..PER_GRANULE)
            .map(|i| flash_at(start + TimeDelta::milliseconds(i as i64)))
            .collect();
        cache.insert(granule_key(start), start, flashes);
    }

    let archived = [
        parked_at - TimeDelta::seconds(40),
        parked_at - TimeDelta::seconds(20),
        parked_at,
    ];
    let sources = s3_archive(
        archived
            .iter()
            .map(|&start| (granule_key(start), one_flash_granule(start)))
            .collect(),
    );
    let client = loopback_client();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");

    let held_before = cache.retained_bytes();
    let outcome = runtime
        .block_on(squallar_overlays::glm::fetch::fetch_glm_flashes(
            &client,
            &sources,
            &[GlmSatellite::GoesEast],
            &[GlmDataLevel::Flash],
            &mut cache,
            parked_at,
            squallar_source::time::Residency::over([(
                parked_at - TimeDelta::seconds(1860),
                parked_at + TimeDelta::seconds(1800),
            )]),
            // The shipped order, exactly.
            &[],
        ))
        .expect("the archive listing answered");
    let held_after = cache.retained_bytes();

    println!(
        "shipped order: store retained {held_before} -> {held_after} B; \
         delivered {} flashes of {} granules served in the parked window",
        outcome.flashes.len(),
        archived.len(),
    );
    assert!(
        outcome.flashes.is_empty(),
        "the shipped order delivered {} flashes — if this is no longer dark, \
         the before this file prints is not the before, and the pair of \
         readings prices nothing",
        outcome.flashes.len(),
    );
    assert!(
        held_after <= MAX_RETAINED_FLASHES * FLASH_BYTES,
        "premise: the shipped order also honoured the ceiling, so the after is \
         compared against the same bound",
    );
}
