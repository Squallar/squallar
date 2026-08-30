//! The two things about loading a pack that can be wrong without anyone
//! noticing: the URL a relative asset resolves to, and a load that reports
//! success having installed nothing.
#![cfg(not(target_arch = "wasm32"))]

use super::*;
use crate::nws::zone_pack::{Coding, Kind, PackedZone, ZonePack};

/// Held by every test that calls [`zone_pack::install`] (directly or through
/// the loader) from install to last assertion. Defined beside the slot it
/// guards so `zone_pack`'s own installer test can hold the same one; a copy
/// private to this module left that test unserialised against these.
use crate::nws::zone_pack::hold_install_slot;

/// A pack holding one county square under `ugc` — a UGC no other test in this
/// binary uses. The installed pack is process-wide, so an id shared with
/// another test's fixture would let this one answer a lookup that test
/// arranged to have fail.
fn drawable_pack_bytes_for(ugc: &str) -> Vec<u8> {
    let square = vec![vec![vec![
        (35.0, -97.0),
        (35.0, -96.0),
        (36.0, -96.0),
        (36.0, -97.0),
        (35.0, -97.0),
    ]]];
    let entries: Vec<PackedZone> = vec![(zone_pack::key(Kind::County, ugc).expect("key"), square)];
    zone_pack::write(&entries, Coding::Varint, 5, 0.005)
}

fn drawable_pack_bytes() -> Vec<u8> {
    drawable_pack_bytes_for("ZZC999")
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a tokio runtime")
}

/// `tls::init()` even for cleartext URLs: with `rustls-no-provider` and
/// aws-lc-rs out of the graph, `Client::new()` panics without a provider.
fn loopback_client() -> reqwest::Client {
    squallar_source::tls::init();
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .expect("client")
}

/// Serve one canned response — status and raw byte body — to every request,
/// forever. Byte-bodied where `zones.rs`'s stub is string-bodied, because a
/// pack is not UTF-8.
fn serve_bytes(status: u16, body: Vec<u8>) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let port = listener.local_addr().expect("local addr").port();
    std::thread::spawn(move || {
        use std::io::{Read, Write};
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut scratch = [0u8; 4096];
            let _ = stream.read(&mut scratch);
            let header = format!(
                "HTTP/1.1 {status} .\r\nContent-Type: binary/octet-stream\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n",
                body.len(),
            );
            let _ = stream.write_all(header.as_bytes());
            let _ = stream.write_all(&body);
            let _ = stream.flush();
        }
    });
    format!("http://127.0.0.1:{port}/{PACK_FILE_NAME}")
}

/// The deploy is served from `/squallar/` on Pages and from `/` elsewhere, and
/// an asset URL that got that wrong would 404 on one of them — silently, since
/// a missing pack is a supported state.
#[test]
fn an_asset_url_resolves_the_way_a_relative_link_would() {
    let cases = [
        (
            "https://host.example/squallar/",
            "https://host.example/squallar/zones.pack",
        ),
        (
            "https://host.example/squallar/index.html",
            "https://host.example/squallar/zones.pack",
        ),
        ("https://host.example/", "https://host.example/zones.pack"),
        ("http://127.0.0.1:8080/", "http://127.0.0.1:8080/zones.pack"),
        (
            "https://host.example/squallar/index.html?v=2#map",
            "https://host.example/squallar/zones.pack",
        ),
        // No path at all: the origin's root, not a sibling of the host.
        ("https://host.example", "https://host.example/zones.pack"),
    ];
    let mut resolved = 0usize;
    for (page, want) in cases {
        assert_eq!(
            asset_url(page, PACK_FILE_NAME).as_deref(),
            Some(want),
            "{page}",
        );
        resolved += 1;
    }
    assert_eq!(resolved, cases.len(), "the loop compared nothing");

    for not_a_page in ["", "zones.pack", "/squallar/", "host.example/x"] {
        assert_eq!(
            asset_url(not_a_page, PACK_FILE_NAME),
            None,
            "{not_a_page:?} is not a page URL and must not produce an asset URL",
        );
    }
}

#[test]
fn the_pack_sits_beside_the_zone_cache_and_not_inside_it() {
    assert_eq!(
        pack_beside_cache(Path::new("/home/x/.cache/squallar/zones")),
        PathBuf::from("/home/x/.cache/squallar/zones.pack"),
    );
    // A relative directory with no parent still names a file, rather than
    // producing an empty path that would read the process's own directory.
    assert_eq!(
        pack_beside_cache(Path::new("zones")),
        PathBuf::from("zones.pack"),
    );
}

/// A file arm that reported success on a file it never opened would be the
/// worst kind of green. Both directions, with the same call.
#[test]
fn a_file_source_installs_a_real_pack_and_a_missing_one_is_not_an_error() {
    let _slot = hold_install_slot();
    let dir = std::env::temp_dir().join(format!("squallar-a243-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join(PACK_FILE_NAME);
    let _ = std::fs::remove_file(&path);

    let runtime = runtime();
    let client = loopback_client();

    let absent = runtime.block_on(load(&client, &PackSource::File(path.clone())));
    assert!(
        matches!(absent, Err(LoadError::Unavailable(_))),
        "a pack that is not there must be unavailable, not a panic: {absent:?}",
    );

    std::fs::write(&path, drawable_pack_bytes()).expect("write pack");
    let installed = runtime
        .block_on(load(&client, &PackSource::File(path.clone())))
        .expect("a real pack on disk installs");
    assert_eq!(installed, 1, "the pack's one zone must have been read");
    assert!(
        zone_pack::installed()
            .expect("installed")
            .get(Kind::County, "ZZC999")
            .is_some(),
        "the loaded pack must be the one a lookup now goes through",
    );

    // And rubbish on disk is refused rather than installed.
    std::fs::write(&path, b"not a pack at all").expect("write rubbish");
    let rejected = runtime.block_on(load(&client, &PackSource::File(path.clone())));
    assert!(
        matches!(rejected, Err(LoadError::Rejected(_))),
        "{rejected:?}",
    );

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(&dir);
}

/// The native gap, closed and pinned: a pack sitting at `pack_beside_cache`
/// installs through the **real** source resolution — nothing here names a
/// `PackSource`, only a cache directory, exactly what the alerts round passes
/// — and zone resolution then answers from it without a request.
#[test]
fn a_pack_beside_the_cache_installs_through_the_real_source_resolution() {
    use crate::nws::alert::{AlertCategory, NwsAlert};
    use std::sync::Arc;

    let _slot = hold_install_slot();
    let dir = std::env::temp_dir().join(format!("squallar-zps0-{}", std::process::id()));
    let cache_dir = dir.join("zones");
    std::fs::create_dir_all(&cache_dir).expect("temp dirs");
    std::fs::write(
        pack_beside_cache(&cache_dir),
        drawable_pack_bytes_for("ZZC998"),
    )
    .expect("write pack");

    let runtime = runtime();
    let client = loopback_client();

    let zones = runtime
        .block_on(install_from(&client, Some(&cache_dir), None))
        .expect("a pack beside the cache must install with no source configured");
    assert_eq!(zones, 1);
    assert!(
        zone_pack::installed()
            .expect("installed")
            .get(Kind::County, "ZZC998")
            .is_some(),
        "the file beside the cache must be the pack lookups now go through",
    );

    // And the layer above: an alert referencing that zone resolves from the
    // pack. The client points at a port nothing listens on, so a resolver
    // that reached for HTTP instead would fail this, not quietly pass it.
    let mut alerts = [NwsAlert {
        id: "urn:squallar:test:zps0".to_string(),
        event: "Tornado Warning".to_string(),
        category: AlertCategory::Warning,
        severity: "Severe".parse().unwrap(),
        urgency: "Immediate".parse().unwrap(),
        certainty: "Observed".parse().unwrap(),
        headline: None,
        description: String::new(),
        instruction: None,
        area_desc: String::new(),
        sender_name: String::new(),
        effective: String::new(),
        expires: String::new(),
        onset: None,
        ends: None,
        valid_from: None,
        valid_until: None,
        affected_zones: vec!["http://127.0.0.1:9/zones/county/ZZC998".to_string()],
        features: Arc::new(Vec::new()),
    }];
    let resolution = runtime.block_on(crate::nws::zones::resolve_zone_geometries(
        &client,
        &mut alerts,
        None,
    ));
    assert_eq!(resolution.zones_resolved, 1, "{resolution:?}");
    assert!(
        !alerts[0].features.is_empty(),
        "the resolved geometry must reach the alert itself",
    );

    let _ = std::fs::remove_file(pack_beside_cache(&cache_dir));
    let _ = std::fs::remove_dir(&cache_dir);
    let _ = std::fs::remove_dir(&dir);
}

/// The download fallback: no file, a named URL — the pack installs for this
/// session AND lands beside the cache, published by rename, so the next
/// session (connected or not) is the file case above.
#[test]
fn a_missing_file_downloads_installs_and_keeps_the_pack_beside_the_cache() {
    let _slot = hold_install_slot();
    let dir = std::env::temp_dir().join(format!("squallar-zps1-{}", std::process::id()));
    let cache_dir = dir.join("zones");
    let pack_path = pack_beside_cache(&cache_dir);
    let _ = std::fs::remove_file(&pack_path);

    let runtime = runtime();
    let client = loopback_client();
    let url = serve_bytes(200, drawable_pack_bytes_for("ZZC997"));

    let zones = runtime
        .block_on(install_from(&client, Some(&cache_dir), Some(&url)))
        .expect("an absent file with a named download must install");
    assert_eq!(zones, 1);
    assert!(
        zone_pack::installed()
            .expect("installed")
            .get(Kind::County, "ZZC997")
            .is_some(),
        "the downloaded pack must be the one lookups now go through",
    );

    let kept = std::fs::read(&pack_path)
        .expect("the downloaded pack must have been kept beside the cache");
    assert_eq!(
        ZonePack::open(kept)
            .expect("the kept file must open")
            .zone_count(),
        1,
        "what is on disk must be the pack that was served",
    );
    assert!(
        !pack_path.with_extension("pack.part").exists(),
        "the temp file must not survive the rename that published it",
    );

    // Next session, no network named: the kept file is the source.
    let offline = runtime
        .block_on(install_from(&client, Some(&cache_dir), None))
        .expect("the kept file must install with no download at all");
    assert_eq!(offline, 1);

    let _ = std::fs::remove_file(&pack_path);
    let _ = std::fs::remove_dir(&cache_dir);
    let _ = std::fs::remove_dir(&dir);
}

/// Failure changes nothing: a 404 degrades exactly as an absent pack always
/// has, and rubbish bytes are refused *before* anything touches the disk — a
/// bad response must not leave a file every later session refuses.
#[test]
fn a_failed_download_leaves_no_file_and_no_pack() {
    let dir = std::env::temp_dir().join(format!("squallar-zps2-{}", std::process::id()));
    let cache_dir = dir.join("zones");
    let pack_path = pack_beside_cache(&cache_dir);
    let _ = std::fs::remove_file(&pack_path);

    let runtime = runtime();
    let client = loopback_client();

    let missing = serve_bytes(404, b"no such pack".to_vec());
    let http = runtime.block_on(install_from(&client, Some(&cache_dir), Some(&missing)));
    assert!(matches!(http, Err(LoadError::Http(404))), "{http:?}");

    let rubbish = serve_bytes(200, b"not a pack at all".to_vec());
    let rejected = runtime.block_on(install_from(&client, Some(&cache_dir), Some(&rubbish)));
    assert!(
        matches!(rejected, Err(LoadError::Rejected(_))),
        "{rejected:?}"
    );

    assert!(
        !pack_path.exists(),
        "no failure mode may leave a pack file behind",
    );
    assert!(
        !pack_path.with_extension("pack.part").exists(),
        "no failure mode may leave a temp file behind",
    );
}

/// The one line the native entry points call, wired to the slot
/// `ensure_installed` reads.
#[test]
fn naming_a_download_url_reaches_the_loader() {
    use_download_url("https://host.example/zones.pack".to_string());
    assert_eq!(
        download_url().as_deref(),
        Some("https://host.example/zones.pack"),
    );
}

/// The bytes a real converter run produced, read back through the loader.
///
/// `#[ignore]`, not a silent skip: a test that quietly passes when its artifact
/// is absent reads as green having checked nothing. Run it deliberately —
///
/// ```text
/// SQUALLAR_ZONE_PACK=/path/to/zones.pack \
///   cargo test -p squallar-overlays --lib zone_pack_source -- --ignored
/// ```
#[test]
#[ignore = "needs a real pack; see the doc comment"]
fn a_real_pack_on_disk_opens_and_draws() {
    let path = std::env::var("SQUALLAR_ZONE_PACK")
        .expect("SQUALLAR_ZONE_PACK must name a pack for this test to mean anything");
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let byte_len = bytes.len();
    let pack = ZonePack::open(bytes).expect("the real pack must open and draw");
    assert!(
        pack.zone_count() > 5_000,
        "a real pack carries thousands of zones, not {}",
        pack.zone_count(),
    );

    // Every zone decoded, not a sample: this is the run that would catch a
    // blob the 64-zone probe stepped over.
    let mut vertices = 0usize;
    for i in 0..pack.zone_count() {
        let polygons = pack
            .at(i)
            .unwrap_or_else(|| panic!("zone {i} does not decode"));
        assert!(!polygons.is_empty(), "zone {i} decoded to no polygons");
        vertices += polygons.iter().flatten().map(Vec::len).sum::<usize>();
    }
    assert!(
        vertices > 100_000,
        "{} zones decoded to only {vertices} vertices",
        pack.zone_count(),
    );
    println!(
        "{} zones, {vertices} vertices, {byte_len} bytes",
        pack.zone_count(),
    );
}
