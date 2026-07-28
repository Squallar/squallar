//! Gates on the static PWA assets: `manifest.webmanifest`, `sw.js`,
//! `index.html`, `icons/`. Nothing in the Rust build reads them, so the two
//! silent failures worth catching mechanically are a root-relative URL (works at
//! `python3 -m http.server`, 404s under `https://<user>.github.io/rustdar/`) and
//! a new [`DataSources`] entry nobody thought to deny in `sw.js`.
//!
//! Every test here reads *text*. A claim about what a file says belongs here; a
//! claim about what it *does* belongs in `tests/sw_routing.test.mjs` or
//! `tests/index_bootstrap.test.mjs`, which `sw_behaviour.rs` runs. The caching
//! policy could be deleted outright and everything here would still pass.
#![cfg(not(target_arch = "wasm32"))]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use rustdar_radar::sources::DataSources;

const MANIFEST: &str = include_str!("../manifest.webmanifest");
const SERVICE_WORKER: &str = include_str!("../sw.js");
const INDEX_HTML: &str = include_str!("../index.html");
const RASTER_WORKER: &str = include_str!("../worker.js");
const WORKER_PORT: &str = include_str!("../src/worker_port.rs");

fn web_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn manifest() -> serde_json::Value {
    serde_json::from_str(MANIFEST).expect("manifest.webmanifest is not valid JSON")
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// Strip `//` line comments so a comment's punctuation cannot be mistaken for
/// source. Block comments are not stripped; none of the literals scanned below
/// sit inside one.
fn without_line_comments(src: &str) -> String {
    src.lines()
        .map(|line| match line.find("//") {
            Some(i) => &line[..i],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Collect the double-quoted string literals from the JS array or set literal
/// that starts at `marker`, up to its closing `]`.
///
/// A real JS parser would be overkill: `sw.js` is a file in this repository and
/// both literals it is pointed at are flat lists of plain strings. If that stops
/// being true the extraction returns nothing and every caller fails loudly,
/// which is the safe direction.
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

/// The single-quoted or double-quoted literal that follows `marker`.
///
/// Used to read a path out of source rather than restate it here: a test that
/// hardcodes the same string it is checking passes whether or not the two
/// files still agree.
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

/// Width and height from a PNG's IHDR.
///
/// Layout is fixed by the spec: an 8-byte signature, then IHDR's 4-byte length
/// and 4-byte type, then width and height as big-endian u32. Reading them is
/// what makes the icon test check the image rather than the filename.
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

/// The hostname rustdar will contact for one declared origin.
///
/// [`DataSources`] stores S3 sources as bare bucket names and everything else as
/// a full `https://` base, exactly as [`DataSources::s3_object_url`] and the
/// fetch modules consume them.
fn host_of(source: &str) -> String {
    match source.strip_prefix("https://") {
        Some(rest) => rest.split('/').next().unwrap().to_string(),
        None => format!("{source}.s3.amazonaws.com"),
    }
}

// ---------------------------------------------------------------------------
// manifest
// ---------------------------------------------------------------------------

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
             root and breaks the https://<user>.github.io/rustdar/ deploy"
        );
    }
    // Installability requires start_url to be inside scope. Both resolving to
    // the same directory is the strongest form of that.
    assert_eq!(
        m["start_url"], m["scope"],
        "start_url must resolve within scope"
    );
}

#[test]
fn manifest_display_mode_is_one_that_makes_the_app_installable() {
    // A manifest with `display: "browser"` is valid and is *not* installable:
    // Chromium and Firefox both require a display mode that leaves the browser
    // chrome behind.
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

// ---------------------------------------------------------------------------
// index.html
// ---------------------------------------------------------------------------

#[test]
fn index_html_has_no_root_relative_urls() {
    // The whole subpath story rests on this. A single `href="/sw.js"` works at
    // `python3 -m http.server` and 404s under `/rustdar/`, and nothing else in
    // the build would notice.
    for (attr, value) in html_url_attributes(INDEX_HTML) {
        assert!(
            !value.starts_with('/'),
            "index.html has {attr}={value:?}; root-relative URLs break the \
             https://<user>.github.io/rustdar/ deploy"
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
    // Not decoration. rustdar caches no weather data, so going offline shows a
    // map that has simply stopped updating — indistinguishable from clear skies
    // unless the page says so.
    assert!(
        INDEX_HTML.contains(r#"id="rustdar-offline""#),
        "index.html has no offline banner element"
    );
    assert!(
        INDEX_HTML.contains(r#"addEventListener("offline""#),
        "nothing in index.html listens for the offline event, so the banner \
         would never appear"
    );
}

/// The canvas must opt out of the browser's own touch gestures.
///
/// winit sets no `touch-action` and only calls `preventDefault()`, which for
/// pointer events does not stop a two-finger gesture being taken as page zoom.
/// Nothing in the Rust build reads this stylesheet, so losing the rule would
/// only ever show up as pinch mysteriously not reaching the app.
#[test]
fn the_canvas_opts_out_of_browser_touch_gestures() {
    let canvas_rule = INDEX_HTML
        .split_once("#rustdar-canvas {")
        .and_then(|(_, rest)| rest.split_once('}'))
        .map(|(body, _)| body)
        .expect("index.html has no #rustdar-canvas style rule");

    assert!(
        canvas_rule.contains("touch-action: none"),
        "#rustdar-canvas does not set `touch-action: none`, so the browser may \
         claim pinch as page zoom and never deliver the second pointer"
    );
}

// ---------------------------------------------------------------------------
// service worker
// ---------------------------------------------------------------------------

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
        "nws_api_base",
        "spc_base",
        "iem_base",
        "sounding_base",
        // Not origins: flags selecting which TLS client the SPC and METAR
        // fetches use. Listed so this set stays an exact match on the struct,
        // which is what makes a *new origin* impossible to add unnoticed.
        "metar_sends_user_agent",
        "spc_sends_user_agent",
    ]
    .into_iter()
    .collect();

    assert_eq!(
        found, expected,
        "DataSources gained or lost a field. If it is a new network origin, add \
         its hostname to NEVER_CACHE_HOSTS in rustdar-web/sw.js before updating \
         this list — the service worker must never cache weather data."
    );
}

/// Gates the *declaration*, not the behaviour: deleting the `NEVER_CACHE_HOSTS`
/// check from `routeFor` leaves this green. The behavioural gate is in
/// `tests/sw_routing.test.mjs` ("keeps the deny list load-bearing even when
/// rustdar is served from a data origin"), which roots the worker at
/// `https://api.weather.gov/rustdar/` — the one configuration where that check
/// is the only thing preventing a weather response from being cached.
#[test]
fn the_worker_states_every_production_data_origin_in_its_deny_list() {
    let denied = js_string_list(SERVICE_WORKER, "const NEVER_CACHE_HOSTS = new Set([");
    assert!(
        !denied.is_empty(),
        "NEVER_CACHE_HOSTS in sw.js parsed as empty"
    );

    let s = DataSources::production();
    let origins = [
        &s.level2_bucket,
        &s.level2_chunks_bucket,
        &s.level3_bucket,
        &s.hrrr_bucket,
        &s.goes_east_bucket,
        &s.goes_west_bucket,
        &s.nws_api_base,
        &s.spc_base,
        &s.iem_base,
        &s.sounding_base,
    ];

    for origin in origins {
        let host = host_of(origin);
        assert!(
            denied.contains(&host),
            "{host} is a production data origin but is not in NEVER_CACHE_HOSTS \
             in rustdar-web/sw.js. Routing is default-deny so it would not be \
             cached today, but the list is what states the policy."
        );
    }
}

#[test]
fn the_worker_shell_list_cannot_name_a_cross_origin_asset() {
    // `routeFor` builds shell URLs from SHELL_PATHS against the worker's own
    // directory, so while every entry is relative with no scheme and no leading
    // slash the shell rule cannot match any other origin.
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
        // "" is the directory index (index.html, served as `./`), and pkg/ is
        // what `wasm-pack build` writes and is not in the repository.
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
        "pkg/rustdar_web.js",
        "pkg/rustdar_web_bg.wasm",
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

/// The rasterization worker is what keeps a ~160-190 ms Level II frame off the
/// main thread. Every way of losing it is silent — the funnel just falls back
/// to rendering inline — so the script `worker_port.rs` asks for has to be a
/// file that exists and an entry the shell precaches, and both are read from
/// the source that names it rather than restated here.
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

/// One wasm module, instantiated twice.
///
/// `sw.js` pins each client to a single shell generation because a mismatched
/// glue/module pair is a `LinkError`, and that machinery is written for exactly
/// one `(glue, wasm)` pair. A worker built from a *second* artifact would need
/// its own pair kept atomic with the first — so if this assertion ever has to
/// change, `SHELL_PATHS`, the version probes and the per-client pinning all
/// need revisiting together.
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
        RASTER_WORKER.contains("rustdar_worker_main"),
        "worker.js does not call the worker entry point"
    );
}

/// The same subpath rule the rest of the deploy lives under, applied to the one
/// file that is fetched by a Worker rather than by the page.
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
