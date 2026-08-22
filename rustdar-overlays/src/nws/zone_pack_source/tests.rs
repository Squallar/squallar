//! The two things about loading a pack that can be wrong without anyone
//! noticing: the URL a relative asset resolves to, and a load that reports
//! success having installed nothing.
#![cfg(not(target_arch = "wasm32"))]

use super::*;
use crate::nws::zone_pack::{Coding, Kind, PackedZone, ZonePack};

fn drawable_pack_bytes() -> Vec<u8> {
    let square = vec![vec![vec![
        (35.0, -97.0),
        (35.0, -96.0),
        (36.0, -96.0),
        (36.0, -97.0),
        (35.0, -97.0),
    ]]];
    let entries: Vec<PackedZone> =
        vec![(zone_pack::key(Kind::County, "OKC001").expect("key"), square)];
    zone_pack::write(&entries, Coding::Varint, 5, 0.005)
}

/// The deploy is served from `/rustdar/` on Pages and from `/` elsewhere, and
/// an asset URL that got that wrong would 404 on one of them — silently, since
/// a missing pack is a supported state.
#[test]
fn an_asset_url_resolves_the_way_a_relative_link_would() {
    let cases = [
        (
            "https://host.example/rustdar/",
            "https://host.example/rustdar/zones.pack",
        ),
        (
            "https://host.example/rustdar/index.html",
            "https://host.example/rustdar/zones.pack",
        ),
        ("https://host.example/", "https://host.example/zones.pack"),
        ("http://127.0.0.1:8080/", "http://127.0.0.1:8080/zones.pack"),
        (
            "https://host.example/rustdar/index.html?v=2#map",
            "https://host.example/rustdar/zones.pack",
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

    for not_a_page in ["", "zones.pack", "/rustdar/", "host.example/x"] {
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
        pack_beside_cache(Path::new("/home/x/.cache/rustdar/zones")),
        PathBuf::from("/home/x/.cache/rustdar/zones.pack"),
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
    let dir = std::env::temp_dir().join(format!("rustdar-a243-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join(PACK_FILE_NAME);
    let _ = std::fs::remove_file(&path);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a tokio runtime");
    // Required even though nothing here is https: with `rustls-no-provider`
    // and aws-lc-rs out of the graph, `Client::new()` panics without a provider.
    rustdar_source::tls::init();
    let client = reqwest::Client::new();

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
            .get(Kind::County, "OKC001")
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

/// The bytes a real converter run produced, read back through the loader.
///
/// `#[ignore]`, not a silent skip: a test that quietly passes when its artifact
/// is absent reads as green having checked nothing. Run it deliberately —
///
/// ```text
/// RUSTDAR_ZONE_PACK=/path/to/zones.pack \
///   cargo test -p rustdar-overlays --lib zone_pack_source -- --ignored
/// ```
#[test]
#[ignore = "needs a real pack; see the doc comment"]
fn a_real_pack_on_disk_opens_and_draws() {
    let path = std::env::var("RUSTDAR_ZONE_PACK")
        .expect("RUSTDAR_ZONE_PACK must name a pack for this test to mean anything");
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
