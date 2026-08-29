//! Gates on the static PWA assets: `manifest.webmanifest`, `sw.js`,
//! `index.html`, `icons/`. Every test here reads *text*; a claim about what a
//! file *does* belongs in `tests/sw_routing.test.mjs` or
//! `tests/index_bootstrap.test.mjs`, which `sw_behaviour.rs` runs.
#![cfg(not(target_arch = "wasm32"))]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use squallar_source::origins::DataSources;

const MANIFEST: &str = include_str!("../manifest.webmanifest");
const SERVICE_WORKER: &str = include_str!("../sw.js");
const INDEX_HTML: &str = include_str!("../index.html");
const RASTER_WORKER: &str = include_str!("../worker.js");
const WORKER_PORT: &str = include_str!("../src/worker_port.rs");
const WORKER_PROTOCOL: &str = include_str!("../src/worker_protocol.rs");
const RASTER_WORKER_RS: &str = include_str!("../src/worker.rs");

fn web_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn manifest() -> serde_json::Value {
    serde_json::from_str(MANIFEST).expect("manifest.webmanifest is not valid JSON")
}

/// Strip `//` line comments so a comment's punctuation is not read as source.
fn without_line_comments(src: &str) -> String {
    src.lines()
        .map(|line| match line.find("//") {
            Some(i) => &line[..i],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The double-quoted string literals of the JS array or set literal that starts
/// at `marker`, up to its closing `]`.
fn js_string_list(src: &str, marker: &str) -> Vec<String> {
    let src = without_line_comments(src);
    let start = src
        .find(marker)
        .unwrap_or_else(|| panic!("sw.js no longer contains {marker:?}"))
        + marker.len();
    let end = start
        + src[start..]
            .find(']')
            .unwrap_or_else(|| panic!("unterminated list after {marker:?} in sw.js"));

    let body = &src[start..end];
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(open) = rest.find('"') {
        rest = &rest[open + 1..];
        let close = rest
            .find('"')
            .expect("unterminated string literal in sw.js list");
        out.push(rest[..close].to_string());
        rest = &rest[close + 1..];
    }
    out
}

/// [`js_string_list`] without the comment stripping, for a list whose entries
/// themselves contain `//` — URLs. The cost is that a `//`-commented-out entry
/// inside the brackets would still be read, so lists extracted this way must
/// not carry line comments between the entries.
fn js_url_list(src: &str, marker: &str) -> Vec<String> {
    let start = src
        .find(marker)
        .unwrap_or_else(|| panic!("sw.js no longer contains {marker:?}"))
        + marker.len();
    let end = start
        + src[start..]
            .find(']')
            .unwrap_or_else(|| panic!("unterminated list after {marker:?} in sw.js"));

    let mut out = Vec::new();
    let mut rest = &src[start..end];
    while let Some(open) = rest.find('"') {
        rest = &rest[open + 1..];
        let close = rest
            .find('"')
            .expect("unterminated string literal in sw.js list");
        out.push(rest[..close].to_string());
        rest = &rest[close + 1..];
    }
    out
}

/// The literal that follows `marker`, read out of source rather than restated.
fn literal_after(src: &str, what: &str, marker: &str) -> String {
    let start = src
        .find(marker)
        .unwrap_or_else(|| panic!("{what} no longer contains {marker:?}"))
        + marker.len();
    let len = src[start..]
        .find('"')
        .unwrap_or_else(|| panic!("unterminated string after {marker:?} in {what}"));
    src[start..start + len].to_string()
}

/// The worker script `worker_port.rs` asks the browser for.
fn requested_worker_url() -> String {
    literal_after(WORKER_PORT, "worker_port.rs", "const WORKER_URL: &str = \"")
}

/// The wasm-bindgen glue `index.html`'s module script imports.
fn page_module_specifier() -> String {
    literal_after(INDEX_HTML, "index.html", "import init, { start } from \"")
}

/// Width and height from a PNG's IHDR: an 8-byte signature, then IHDR's length
/// and type, then width and height as big-endian u32.
fn png_dimensions(path: &Path) -> (u32, u32) {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    assert!(
        bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]),
        "{} is not a PNG",
        path.display()
    );
    assert_eq!(
        &bytes[12..16],
        b"IHDR",
        "{} does not start with an IHDR chunk",
        path.display()
    );
    let w = u32::from_be_bytes(bytes[16..20].try_into().unwrap());
    let h = u32::from_be_bytes(bytes[20..24].try_into().unwrap());
    (w, h)
}

/// Every value of an HTML attribute that carries a URL.
fn html_url_attributes(html: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for attr in ["href", "src"] {
        let needle = format!("{attr}=\"");
        let mut rest = html;
        while let Some(i) = rest.find(&needle) {
            rest = &rest[i + needle.len()..];
            let end = rest
                .find('"')
                .expect("unterminated attribute in index.html");
            out.push((attr.to_string(), rest[..end].to_string()));
            rest = &rest[end..];
        }
    }
    out
}

/// The hostname squallar will contact for one declared origin: [`DataSources`]
/// stores S3 sources as bare bucket names, everything else as an https base.
fn host_of(source: &str) -> String {
    match source.strip_prefix("https://") {
        Some(rest) => rest.split('/').next().unwrap().to_string(),
        None => host_of(&DataSources::production().s3_bucket_url(source)),
    }
}

#[test]
fn manifest_start_url_and_scope_are_relative_so_a_subpath_deploy_resolves() {
    let m = manifest();
    for key in ["start_url", "scope"] {
        let value = m[key]
            .as_str()
            .unwrap_or_else(|| panic!("{key} is missing"));
        assert!(
            !value.starts_with('/') && !value.contains("://"),
            "manifest {key} is {value:?}; an absolute URL resolves to the origin \
             root and breaks the https://<user>.github.io/squallar/ deploy"
        );
    }
    // Installability requires start_url to be inside scope.
    assert_eq!(
        m["start_url"], m["scope"],
        "start_url must resolve within scope"
    );
}

#[test]
fn manifest_display_mode_is_one_that_makes_the_app_installable() {
    // `display: "browser"` is valid and is *not* installable.
    let installable = ["standalone", "fullscreen", "minimal-ui"];
    let m = manifest();
    let display = m["display"].as_str().expect("display is missing");
    assert!(
        installable.contains(&display),
        "manifest display is {display:?}, which is not an installable mode; \
         expected one of {installable:?}"
    );
}

#[test]
fn manifest_declares_the_icons_installability_requires() {
    let m = manifest();
    let icons = m["icons"].as_array().expect("icons is missing");

    let sized = |purpose: &str, want: u32| {
        icons.iter().any(|icon| {
            let p = icon["purpose"].as_str().unwrap_or("any");
            let s = icon["sizes"].as_str().unwrap_or("");
            p.split_whitespace().any(|w| w == purpose)
                && s.split_whitespace()
                    .any(|dim| dim == format!("{want}x{want}"))
        })
    };

    assert!(
        sized("any", 192),
        "no 192x192 `any` icon; Android's install \
        prompt and the home-screen shortcut both want one"
    );
    assert!(
        sized("any", 512),
        "no 512x512 `any` icon; this is the size the \
        install dialog and splash screen are generated from"
    );
    assert!(
        sized("maskable", 512),
        "no 512x512 `maskable` icon; without one \
        Android draws the square icon inside its adaptive mask, letterboxed"
    );
}

#[test]
fn every_manifest_icon_is_relative_and_the_file_matches_its_declared_size() {
    let m = manifest();
    for icon in m["icons"].as_array().expect("icons is missing") {
        let src = icon["src"].as_str().expect("icon has no src");
        assert!(
            !src.starts_with('/') && !src.contains("://"),
            "icon src {src:?} is absolute; it must resolve relative to the \
             manifest so the subpath deploy finds it"
        );

        let path = web_dir().join(src);
        assert!(
            path.is_file(),
            "icon {src} does not exist at {}",
            path.display()
        );

        let declared = icon["sizes"].as_str().expect("icon has no sizes");
        let (w, h) = png_dimensions(&path);
        assert_eq!(
            declared,
            format!("{w}x{h}"),
            "{src} declares sizes {declared:?} but the PNG is {w}x{h}"
        );
    }
}

#[test]
fn index_html_has_no_root_relative_urls() {
    // A single `href="/sw.js"` works at `python3 -m http.server` and 404s
    // under `/squallar/`, and nothing else in the build would notice.
    for (attr, value) in html_url_attributes(INDEX_HTML) {
        assert!(
            !value.starts_with('/'),
            "index.html has {attr}={value:?}; root-relative URLs break the \
             https://<user>.github.io/squallar/ deploy"
        );
    }
}

#[test]
fn index_html_links_the_manifest_and_registers_the_worker_relatively() {
    assert!(
        INDEX_HTML.contains(r#"rel="manifest""#)
            && INDEX_HTML.contains(r#"href="manifest.webmanifest""#),
        "index.html does not link manifest.webmanifest; without a manifest link \
         the page is not installable no matter what the manifest says"
    );
    assert!(
        INDEX_HTML.contains(r#"register("./sw.js")"#),
        "index.html does not register ./sw.js; the leading `./` is what makes \
         the worker's scope the deploy directory rather than the origin root"
    );
}

#[test]
fn index_html_carries_an_explicit_offline_state() {
    // squallar caches no weather data, so offline shows a map that has simply
    // stopped updating — indistinguishable from clear skies.
    assert!(
        INDEX_HTML.contains(r#"id="squallar-offline""#),
        "index.html has no offline banner element"
    );
    assert!(
        INDEX_HTML.contains(r#"addEventListener("offline""#),
        "nothing in index.html listens for the offline event, so the banner \
         would never appear"
    );
}

/// The canvas must opt out of the browser's own touch gestures: winit sets no
/// `touch-action`, and `preventDefault()` does not stop a two-finger page zoom.
#[test]
fn the_canvas_opts_out_of_browser_touch_gestures() {
    let canvas_rule = INDEX_HTML
        .split_once("#squallar-canvas {")
        .and_then(|(_, rest)| rest.split_once('}'))
        .map(|(body, _)| body)
        .expect("index.html has no #squallar-canvas style rule");

    assert!(
        canvas_rule.contains("touch-action: none"),
        "#squallar-canvas does not set `touch-action: none`, so the browser may \
         claim pinch as page zoom and never deliver the second pointer"
    );
}

/// Pinned so adding an origin to the Rust declaration fails here and forces a
/// decision about `sw.js`.
#[test]
fn data_sources_has_the_fields_the_worker_was_written_against() {
    let debug = format!("{:?}", DataSources::production());
    let found: BTreeSet<&str> = debug
        .split(['{', ',', '}'])
        .filter_map(|part| part.split_once(':'))
        .map(|(name, _)| name.trim())
        .filter(|name| !name.is_empty())
        .collect();

    let expected: BTreeSet<&str> = [
        "level2_bucket",
        "level2_chunks_bucket",
        "level3_bucket",
        "hrrr_bucket",
        "goes_east_bucket",
        "goes_west_bucket",
        "mrms_bucket",
        "gmgsi_bucket",
        "nws_api_base",
        "spc_base",
        "iem_base",
        "sounding_base",
        // Not an origin: the URL template the bucket names are addressed through.
        "s3_base",
        // Not origins: TLS-client flags, listed so this set stays an exact
        // match on the struct.
        "metar_sends_user_agent",
        "spc_sends_user_agent",
    ]
    .into_iter()
    .collect();

    assert_eq!(
        found, expected,
        "DataSources gained or lost a field. If it is a new network origin, add \
         its hostname to NEVER_CACHE_HOSTS in squallar-web/sw.js before updating \
         this list — the service worker must never cache weather data."
    );
}

/// Gates the *declaration*, not the behaviour. The behavioural gate is
/// `tests/sw_routing.test.mjs`.
#[test]
fn the_worker_states_every_production_data_origin_in_its_deny_list() {
    let denied = js_string_list(SERVICE_WORKER, "const NEVER_CACHE_HOSTS = new Set([");
    assert!(
        !denied.is_empty(),
        "NEVER_CACHE_HOSTS in sw.js parsed as empty"
    );

    // The one enumeration, shared with `network_security_config`'s coverage
    // pair. Both walkers restated it independently until WO-M15.6 — which is
    // exactly the drift they exist to catch.
    for origin in DataSources::production().origin_urls() {
        let host = host_of(&origin);
        assert!(
            denied.contains(&host),
            "{host} is a production data origin but is not in NEVER_CACHE_HOSTS \
             in squallar-web/sw.js. Routing is default-deny so it would not be \
             cached today, but the list is what states the policy."
        );
    }
}

/// **The second direction, which the worker's deny list did not have.**
///
/// The Android config is pinned both ways —
/// `every_live_origin_is_covered_by_the_network_security_config` and
/// `the_network_security_config_lists_nothing_unused`. `sw.js` had only the
/// first, so an origin removed from `DataSources` would linger in the deny list
/// for as long as nobody happened to read it. One-directional pins have hidden
/// the case they existed for three times on this campaign.
///
/// **Tile hosts are not expected here and must never be.** A basemap tile is
/// cached on purpose, by the `BASEMAP_HOST` regex and its own cache route; the
/// shared enumeration is data origins only, so this test does not demand them.
#[test]
fn the_worker_deny_list_names_nothing_that_is_not_a_data_origin() {
    let denied = js_string_list(SERVICE_WORKER, "const NEVER_CACHE_HOSTS = new Set([");
    assert!(
        !denied.is_empty(),
        "NEVER_CACHE_HOSTS in sw.js parsed as empty"
    );

    let live: BTreeSet<String> = DataSources::production()
        .origin_urls()
        .iter()
        .map(|origin| host_of(origin))
        .collect();
    // Non-triviality: an empty expectation would make every entry unused and
    // an empty deny list would make the walk vacuous.
    assert_eq!(
        live.len(),
        12,
        "the data-origin enumeration moved: {live:?}"
    );

    let unused: Vec<&String> = denied.iter().filter(|host| !live.contains(*host)).collect();
    assert!(
        unused.is_empty(),
        "NEVER_CACHE_HOSTS in squallar-web/sw.js names hosts no DataSources \
         origin resolves to; drop them or update DataSources: {unused:?}"
    );
}

/// The archive host in `sw.js` is the archive host in Rust, or the route is a
/// comment.
///
/// The only assertion in this file that can fail on a change nobody was
/// thinking about, which is the point: `BASEMAP_ARCHIVE_URL` moves when the
/// planet archive is regenerated, and `routeFor` is default-deny, so a moved
/// host would leave `isBasemapArchive` matching nothing and every routing test
/// still green. The pin is what makes the JS rule track the Rust const rather
/// than a memory of it.
///
/// Direction matters here and it is deliberately the *tight* one: the JS
/// constant must equal the const's host exactly, not contain it and not be
/// contained by it.
#[test]
fn the_worker_names_the_same_basemap_archive_host_the_client_reads() {
    let declared = literal_after(SERVICE_WORKER, "sw.js", "const BASEMAP_ARCHIVE_HOST = \"");

    let expected = host_of(squallar_egui::tiles::BASEMAP_ARCHIVE_URL);

    assert_eq!(
        declared, expected,
        "sw.js routes {declared:?} as the PMTiles archive, but \
         squallar_egui::tiles::BASEMAP_ARCHIVE_URL is served from {expected:?}. \
         The worker must not cache a range response; a stale host means the \
         rule matches nothing and only default-deny is holding the line."
    );

    // Non-triviality: `host_of` returning an empty string, or the literal
    // parse landing on an empty one, would make the equality above vacuous.
    assert!(
        expected.contains('.') && !expected.contains('/'),
        "the archive host parsed as {expected:?}, which is not a hostname"
    );
}

/// The block cache serves exactly the archives the client reads, in both
/// directions: every Rust archive const appears in `ARCHIVE_URLS`, and every
/// entry there is one of the consts.
///
/// This list is what `cachesToKeep` derives the CURRENT block-cache
/// generations from, so each direction has its own failure mode. A Rust const
/// missing from the list means that archive's blocks are purged on every
/// deploy — the symptom is a slow map, never an error. A stale entry lingering
/// after a regeneration means the retired generation's cache is kept forever
/// and the budget evicts live blocks to make room for dead ones.
#[test]
fn the_worker_block_caches_exactly_the_archives_the_client_reads() {
    let declared = js_url_list(SERVICE_WORKER, "const ARCHIVE_URLS = [");
    let expected: BTreeSet<String> = [
        squallar_egui::tiles::BASEMAP_ARCHIVE_URL.to_string(),
        squallar_egui::tiles::TERRAIN_ARCHIVE_URL.to_string(),
    ]
    .into_iter()
    .collect();

    let found: BTreeSet<String> = declared.iter().cloned().collect();
    assert_eq!(
        found, expected,
        "ARCHIVE_URLS in squallar-web/sw.js does not match the Rust archive \
         consts (BASEMAP_ARCHIVE_URL, TERRAIN_ARCHIVE_URL). The block cache \
         keeps exactly the generations this list names; a missing archive is \
         wiped on every deploy, a stale one is retained forever."
    );
    assert_eq!(declared.len(), 2, "ARCHIVE_URLS carries a duplicate entry");

    // Every listed archive is on the host the block route owns; an entry on
    // any other host would name a cache no request can ever fill.
    let route_host = literal_after(SERVICE_WORKER, "sw.js", "const BASEMAP_ARCHIVE_HOST = \"");
    for url in &declared {
        assert_eq!(
            host_of(url),
            route_host,
            "{url} in ARCHIVE_URLS is not on the archive host the block route \
             intercepts, so its blocks would never be cached"
        );
    }
}

/// The archive is a `Range` read, so it must not be in the shell or asset
/// lists — either would make the worker try to `Cache.put()` a 206.
#[test]
fn the_worker_never_lists_the_basemap_archive_as_a_cached_asset() {
    let host = host_of(squallar_egui::tiles::BASEMAP_ARCHIVE_URL);

    for (what, marker) in [
        ("SHELL_PATHS", "const SHELL_PATHS = ["),
        ("ASSET_PATHS", "const ASSET_PATHS = ["),
    ] {
        for entry in js_string_list(SERVICE_WORKER, marker) {
            assert!(
                !entry.contains(&host) && !entry.ends_with(".pmtiles"),
                "{what} in squallar-web/sw.js names {entry:?}, which is the PMTiles \
                 archive; a 206 cannot be stored in a Cache"
            );
        }
    }
}

#[test]
fn the_worker_shell_list_cannot_name_a_cross_origin_asset() {
    // `routeFor` builds shell URLs against the worker's own directory, so a
    // relative entry cannot match any other origin.
    for path in js_string_list(SERVICE_WORKER, "const SHELL_PATHS = [") {
        assert!(
            !path.starts_with('/'),
            "shell path {path:?} is root-relative; it would break the subpath \
             deploy and widen what the shell rule can match"
        );
        assert!(
            !path.contains("://") && !path.starts_with("//"),
            "shell path {path:?} names another origin; the app shell must be \
             same-origin only"
        );
    }
}

#[test]
fn every_shell_asset_that_is_not_build_output_exists() {
    let paths = js_string_list(SERVICE_WORKER, "const SHELL_PATHS = [");
    assert!(paths.len() > 1, "SHELL_PATHS in sw.js parsed as near-empty");

    for path in paths {
        // "" is the directory index; pkg/ is build output, not in the repo.
        if path.is_empty() || path.starts_with("pkg/") {
            continue;
        }
        let full = web_dir().join(&path);
        assert!(
            full.is_file(),
            "sw.js precaches {path:?}, which does not exist at {}. cache.addAll \
             is all-or-nothing, so one missing entry means the shell never \
             caches and offline support silently does nothing.",
            full.display()
        );
    }
}

#[test]
fn the_worker_precaches_the_manifest_and_both_halves_of_the_wasm_bundle() {
    let paths = js_string_list(SERVICE_WORKER, "const SHELL_PATHS = [");
    for required in [
        "manifest.webmanifest",
        "pkg/squallar_web.js",
        "pkg/squallar_web_bg.wasm",
    ] {
        assert!(
            paths.iter().any(|p| p == required),
            "sw.js does not precache {required:?}. The glue and the module must \
             be cached together: a shell holding one without the other is a \
             wasm-bindgen version mismatch and a blank page."
        );
    }
    assert!(
        paths.iter().any(|p| p.is_empty()),
        "sw.js does not precache the directory index (\"\"), which is the entry \
         every navigation is answered from"
    );
}

/// The zone pack is the one large same-origin asset the app fetches, and the
/// two lists it could be named in mean opposite things.
///
/// `ASSET_PATHS` is routed and cached on demand; `SHELL_PATHS` is precached
/// with all-or-nothing `cache.addAll`. Moving the pack into the shell list
/// would mean a pack that 404s — which is the normal state of a deploy that has
/// not run the converter — takes offline support for the whole app with it.
#[test]
fn the_zone_pack_is_a_routed_asset_and_never_part_of_the_all_or_nothing_shell() {
    let assets = js_string_list(SERVICE_WORKER, "const ASSET_PATHS = [");
    let shell = js_string_list(SERVICE_WORKER, "const SHELL_PATHS = [");
    assert!(!assets.is_empty(), "ASSET_PATHS in sw.js parsed as empty");

    let pack = zone_pack_file_name();
    assert!(
        assets.contains(&pack),
        "sw.js does not route {pack:?}, so every session would re-download it \
         and no offline session would have it at all",
    );
    assert!(
        !shell.contains(&pack),
        "sw.js precaches {pack:?} in the all-or-nothing shell install. A pack \
         that does not fetch is a supported state for the app, and must never \
         be able to cost it offline support.",
    );
    for path in &assets {
        assert!(
            !path.starts_with('/') && !path.contains("://"),
            "asset path {path:?} is not relative to the deploy directory",
        );
    }
}

/// `PACK_FILE_NAME` as `squallar-overlays` declares it, read out of the source
/// rather than linked: that crate is not in this test's graph, and a literal
/// here would be a second declaration for the first to drift from.
fn zone_pack_file_name() -> String {
    let workspace = web_dir()
        .parent()
        .expect("squallar-web sits in the workspace")
        .to_path_buf();
    let source = std::fs::read_to_string(workspace.join("squallar-overlays/src/nws/zone_pack.rs"))
        .expect("squallar-overlays declares the pack's file name");
    let marker = "pub const PACK_FILE_NAME: &str = \"";
    let start = source
        .find(marker)
        .expect("zone_pack.rs no longer declares PACK_FILE_NAME")
        + marker.len();
    let end = start + source[start..].find('"').expect("unterminated literal");
    source[start..end].to_string()
}

/// The rasterization worker keeps a ~160-190 ms Level II frame off the main
/// thread, and every way of losing it is silent.
#[test]
fn the_script_the_page_asks_for_is_shipped_and_precached() {
    let requested = requested_worker_url();
    let relative = requested.trim_start_matches("./");

    assert!(
        web_dir().join(relative).is_file(),
        "worker_port.rs starts {requested:?}, which is not a file in this crate. \
         The browser would 404 and rasterization would stay on the main thread."
    );
    let paths = js_string_list(SERVICE_WORKER, "const SHELL_PATHS = [");
    assert!(
        paths.iter().any(|p| p == relative),
        "sw.js does not precache {relative:?}, which worker_port.rs starts. \
         Offline, and on any load the shell answers, rasterization would \
         silently move back onto the main thread."
    );
}

/// One wasm module, instantiated twice: `sw.js` pins each client to a single
/// shell generation because a mismatched glue/module pair is a `LinkError`.
#[test]
fn the_rasterization_worker_loads_the_same_module_as_the_page() {
    let glue = page_module_specifier();
    assert!(
        RASTER_WORKER.contains(&format!("from \"{glue}\"")),
        "index.html imports {glue:?} but worker.js does not. A second wasm \
         artifact needs its own precache entries and its own place in sw.js's \
         per-client shell pinning."
    );
    assert!(
        RASTER_WORKER.contains("squallar_worker_main"),
        "worker.js does not call the worker entry point"
    );
}

/// The subpath rule, for the one file fetched by a Worker rather than the page.
#[test]
fn the_rasterization_worker_uses_only_relative_paths() {
    for (line_no, line) in RASTER_WORKER.lines().enumerate() {
        for needle in ["from \"/", "import(\"/", "new URL(\"/", "importScripts(\"/"] {
            assert!(
                !line.contains(needle),
                "worker.js:{} uses a root-absolute path ({needle}...). It resolves \
                 under `python3 -m http.server` and 404s under the project-Pages \
                 subpath.",
                line_no + 1
            );
        }
    }
}

/// The protocol is versionless, and the **build token** names the build:
/// `GITHUB_SHA` in CI, `wire_identity::wire_digest()` locally.
/// `worker_protocol` is `wasm32`-only, so a source scrape is the only
/// instrument here.
#[test]
fn the_worker_protocol_is_versionless_and_the_token_names_the_build() {
    // Split literal so this file does not itself contain the deleted constant.
    let version_const = concat!("PROTOCOL_", "VERSION");
    assert!(
        !WORKER_PROTOCOL.contains(version_const),
        "worker_protocol.rs names {version_const} again. The hand-versioned \
         protocol was deleted at M5: the build token carries GITHUB_SHA in CI \
         and the wire-identity digest locally, so a reintroduced version \
         constant is dead weight that will drift — nothing compares it any \
         more, and a number nobody compares reads like a guard while \
         guarding nothing."
    );

    let body = without_line_comments(WORKER_PROTOCOL);
    let body = body
        .split_once("pub fn build_token")
        .expect("worker_protocol.rs no longer declares build_token")
        .1;
    let body = body
        .split_once("\n}")
        .expect("build_token no longer ends with a column-0 closing brace")
        .0;
    for needle in ["GITHUB_SHA", "wire_identity::wire_digest"] {
        assert!(
            body.contains(needle),
            "build_token no longer reads {needle}. The token is the whole \
             deploy-skew protection now: GITHUB_SHA distinguishes deploys in \
             CI, and the wire-identity digest distinguishes local builds \
             whose pinned framing rows differ. Losing either half reopens \
             the silent mismatch the token exists to convert into a clean \
             termination and a respawn."
        );
    }
}

/// Every arm of the worker's reply writes every field. The page holds a render
/// slot and a pending-map entry against each id it posted, and only a reply
/// releases them, so a path that left a field absent wedges that pane.
#[test]
fn the_worker_reply_writes_every_field_on_every_arm() {
    let body = RASTER_WORKER_RS
        .split_once("fn post_result(")
        .expect("worker.rs no longer has a post_result")
        .1;
    let defaults = body
        .split_once("if let Some((kind, head, tails)) = result")
        .expect("post_result no longer branches on the result")
        .0;
    for field in [
        // Matched **with the trailing comma**, because `proto::OUT` is a
        // prefix of `proto::OUT_KIND`.
        "proto::OUT,",
        "proto::OUT_KIND,",
        "proto::TAILS,",
        // The loan is defaulted the same way and for a sharper reason: absent
        // and `NO_LOAN` must read alike, and a reply that reached the copying
        // fallback never writes the field again. A `LOAN` left over from an
        // arm that did not run would have the page release a loan the worker
        // never made — and then read a region the worker had re-lent.
        "proto::set_loan(",
    ] {
        assert!(
            defaults.contains(field),
            "{field} is not written before the answering arm in post_result, \
             so a path that does not set it leaves it absent and the page \
             cannot tell a null answer from a lost one"
        );
    }
}

/// The frame reply rides the `OUT`/`OUT_KIND`/`TAILS` trio, and neither of its
/// two wires copies a buffer it does not have to.
///
/// The **lending** wire (cross-origin isolated) posts views onto the worker's
/// own `SharedArrayBuffer` and copies nothing at all, so it must contain no
/// `Uint8Array::from` — that constructor is the copy — and nothing to transfer.
/// The **copying** fallback pays one `Uint8Array::from` per buffer and must
/// then transfer every one of them, which is why exactly ONE literal
/// `transfer.push(` site is expected and why it has to sit inside the loop over
/// all buffers: a buffer left off the list is structured-cloned, up to ~16 MiB
/// per image tail on top of the copy already paid.
///
/// Before WS3c both wires were one wire and the count here was 2 (a head push
/// and a per-tail push). The head is now the first element of the same
/// head-first list the loop walks, which is what collapsed it to 1.
#[test]
fn the_frame_reply_rides_the_out_pair_and_transfers_its_buffer() {
    let body = without_line_comments(RASTER_WORKER_RS);
    let answering_arm = body
        .split_once("if let Some((kind, head, tails)) = result")
        .expect("post_result no longer branches on the result")
        .1
        .split_once("scope.post_message_with_transfer")
        .expect("post_result no longer ends by posting the message it built")
        .0;

    let (lending, copying) = answering_arm
        .split_once("Err(buffers) =>")
        .expect("post_result's answering arm no longer has a copying fallback");
    assert!(
        lending.contains("shared_loan::lend("),
        "the answering arm no longer offers the reply to `shared_loan::lend`, \
         so every reply is copied out of the worker's memory again",
    );
    for copying_spelling in ["Uint8Array::from(", "transfer.push("] {
        assert!(
            !lending.contains(copying_spelling),
            "the LENDING arm names {copying_spelling}, so it is copying after \
             all — the whole claim of WS3c is that this arm copies nothing",
        );
    }

    assert_eq!(
        copying.matches("transfer.push(").count(),
        1,
        "the copying fallback must transfer through exactly one literal push \
         site, inside the loop over every buffer, so no head or tail can be \
         left behind to be structured-cloned",
    );
    assert!(
        copying.contains("for buffer in &buffers"),
        "the copying fallback's transfer push must sit inside the loop over \
         ALL buffers; a push outside it would move one and clone the rest",
    );
    for needle in ["proto::OUT,", "proto::OUT_KIND,", "proto::TAILS,"] {
        assert!(
            answering_arm.contains(needle),
            "the answering arm no longer writes {needle} — the \
             OUT/OUT_KIND/TAILS trio is the whole answer since WO-M7d",
        );
    }

    // The deletion pin. Split literals so this file never contains what it
    // forbids.
    for ident in [
        concat!("proto::", "IMAGE"),
        concat!("proto::", "POLAR"),
        concat!("proto::", "MAX_RANGE"),
        concat!("proto::", "NYQUIST"),
        concat!("proto::", "MELTING_LAYER"),
        concat!("proto::", "STORM_MOTION"),
    ] {
        assert!(
            !body.contains(ident),
            "worker.rs names {ident} again. The frame reply rides the \
             OUT/OUT_KIND/TAILS trio in the frame codec's head+tails form \
             since WO-M7d; a named frame field beside it is a second reply \
             shape the page would have to arbitrate against the codec's",
        );
    }
}

/// `ident -> wire key` for every `pub const <NAME>: &str = "<key>";` in
/// `worker_protocol.rs`. There is no serde on this boundary: both directions
/// set fields on a bare `js_sys::Object` by name.
fn worker_protocol_vocabulary() -> BTreeMap<String, String> {
    let src = without_line_comments(WORKER_PROTOCOL);
    let mut out = BTreeMap::new();
    for line in src.lines() {
        let Some(rest) = line.trim().strip_prefix("pub const ") else {
            continue;
        };
        let Some((ident, rest)) = rest.split_once(": &str = \"") else {
            continue;
        };
        let Some((key, _)) = rest.split_once('"') else {
            continue;
        };
        out.insert(ident.to_string(), key.to_string());
    }
    assert!(
        !out.is_empty(),
        "no `pub const <NAME>: &str = \"...\";` declarations found in \
         worker_protocol.rs. The field names moved or changed form, and every \
         extraction below is now reading nothing and would pass on an empty \
         set — fix this helper before trusting anything under it."
    );
    out
}

/// `post_result`'s body, cut at the `post_message` that ends it so the only
/// `&message` left belongs to a `set_field`.
fn post_result_body() -> String {
    let src = without_line_comments(RASTER_WORKER_RS);
    src.split_once("fn post_result(")
        .expect("worker.rs no longer has a post_result")
        .1
        .split_once("scope.post_message_with_transfer")
        .expect("post_result no longer ends by posting the message it built")
        .0
        .to_string()
}

/// That body split into the two places a field can be written: before the
/// answering arm, and the answering arm itself.
fn post_result_arms() -> Vec<(&'static str, String)> {
    let body = post_result_body();
    let (head, answer) = body
        .split_once("if let Some((kind, head, tails)) = result")
        .expect("post_result no longer branches on the result");
    vec![("head", head.to_string()), ("answer", answer.to_string())]
}

/// The key argument of every `set_field(&message, <KEY>, ..)` in one slice, in
/// source order. Reads the *argument*, not every `proto::` token.
fn message_field_idents(arm: &str) -> Vec<String> {
    let pieces: Vec<&str> = arm.split("&message,").collect();
    let mut out = Vec::new();
    for window in pieces.windows(2) {
        let (before, after) = (window[0], window[1]);
        // `set_loan` names its field in the SETTER rather than in an argument —
        // there is only one loan field, so it takes the id and nothing else.
        // Read from the call site behind the split, because reading only what
        // follows `&message,` would make every `LOAN` write invisible to the
        // shape pin below, and a pin with a hole reads green over the hole.
        if before.trim_end().ends_with("proto::set_loan(") {
            out.push(String::from("LOAN"));
        } else if let Some(rest) = after.trim_start().strip_prefix("proto::") {
            out.push(
                rest.chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect(),
            );
        }
    }
    out
}

/// The shape of a `done` reply, whole, pinned against the build that ships it.
/// The enumerations above are lists of names they expect to *find*; this
/// extracts the whole set. It watches the **field set** only — bytes inside a
/// field are covered by the per-codec layout digests.
#[test]
fn the_worker_reply_shape_is_the_one_this_build_ships() {
    let vocabulary = worker_protocol_vocabulary();
    let arms = post_result_arms();

    let mut written = Vec::new();
    for (arm, slice) in &arms {
        for ident in message_field_idents(slice) {
            let key = vocabulary.get(&ident).unwrap_or_else(|| {
                panic!(
                    "post_result's {arm} writes proto::{ident}, which is not a \
                     `pub const <NAME>: &str` in worker_protocol.rs. Every field \
                     on this wire is named by a constant there; a key that is \
                     not is one the page has no name for either."
                )
            });
            written.push(format!("{arm} | {ident} | {key}"));
        }
    }
    written.sort();

    // An unrecognised `set_field` would be silently skipped, and a guard with a
    // hole in it reads green over the hole.
    let body = post_result_body();
    let call_sites =
        body.matches("proto::set_field(").count() + body.matches("proto::set_loan(").count();
    assert_eq!(
        call_sites,
        written.len(),
        "post_result makes {call_sites} field-setting calls but only {} of them \
         were recognised as `set_field(&message, proto::NAME, ..)` or \
         `set_loan(&message, ..)`. The unrecognised ones are invisible to the \
         shape below, so fix the extraction (or the call) before reading its \
         verdict.",
        written.len()
    );

    assert_eq!(
        written,
        [
            // The answering arm's `LOAN` is written only where the reply was
            // LENT; the copying arm leaves the `NO_LOAN` the head wrote.
            "answer | LOAN | loan",
            "answer | OUT | out",
            "answer | OUT_KIND | outkind",
            "answer | TAILS | tails",
            "head | ID | id",
            "head | KIND | kind",
            "head | LOAN | loan",
            "head | OUT | out",
            "head | OUT_KIND | outkind",
            "head | TAILS | tails",
        ]
        .map(str::to_string),
        "the shape of a `done` reply is not the shape this list was last \
         told. Left is what worker.rs and worker_protocol.rs say today; right \
         is the pinned shape.\n\n\
         This is a within-build shape pin and refactor gate: update the list \
         to what the reply really carries, deliberately. Deploy-skew \
         protection is not this list's job and not any hand-kept number's — \
         it is the build token's (GITHUB_SHA in CI, the wire_identity digest \
         locally), which refuses a mismatched page/worker pair at the \
         handshake before either half reads a reply.\n\n\
         If the wire keys are unchanged and only an arm moved — a field \
         defaulted before the match that used to be written inside one arm — \
         then both halves still read the same message and only this list \
         moves."
    );
}

/// Every field an arm writes is also written before the match. An arm that
/// writes a field the defaults do not breaks that quietly: the page reads an
/// absent field and a null one through the same `as_f64` filter.
#[test]
fn no_arm_of_the_worker_reply_writes_a_field_the_defaults_do_not() {
    let arms = post_result_arms();
    let defaults: BTreeSet<String> = arms
        .iter()
        .find(|(arm, _)| *arm == "head")
        .map(|(_, slice)| message_field_idents(slice).into_iter().collect())
        .expect("post_result no longer has a block before the match");

    for (arm, slice) in arms.iter().filter(|(arm, _)| *arm != "head") {
        for ident in message_field_idents(slice) {
            assert!(
                defaults.contains(&ident),
                "post_result's {arm} arm writes proto::{ident}, and the block \
                 before the match does not. Every other path out of this \
                 function therefore posts a reply with that field absent \
                 rather than null, against the invariant stated over that \
                 block. Add `proto::set_field(&message, proto::{ident}, \
                 &JsValue::NULL);` there."
            );
        }
    }
}
