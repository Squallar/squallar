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

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use rustdar_radar::sources::DataSources;

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
/// [`DataSources`] stores S3 sources as bare bucket names — addressed through
/// its own `s3_base` template — and everything else as a full `https://` base,
/// exactly as [`DataSources::s3_object_url`] and the fetch modules consume
/// them.
fn host_of(source: &str) -> String {
    match source.strip_prefix("https://") {
        Some(rest) => rest.split('/').next().unwrap().to_string(),
        None => host_of(&DataSources::production().s3_bucket_url(source)),
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
        // Not an origin of its own: the URL template the six bucket names above
        // are addressed through, so every host it can produce is already listed
        // by one of them. It is here because it is what makes those buckets
        // injectable — see `DataSources::s3_bucket_url`.
        "s3_base",
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

/// The protocol version is a **number**, and this is the only instrument that
/// can check it.
///
/// `worker_protocol` is `#[cfg(target_arch = "wasm32")]`, so nothing a host
/// `cargo test` compiles can name `PROTOCOL_VERSION` at all — and the wasm rows
/// are `cargo check`, which runs no tests. A source scrape is therefore the
/// only place the value can be pinned, and it has to be pinned somewhere: every
/// other version-adjacent test in this workspace flips bytes and asserts a
/// refusal, which shows that *a* check exists, not what it says. A version that
/// silently failed to bump when the message shapes changed would leave a page
/// and a worker from opposite sides of a deploy exchanging replies one of them
/// cannot read — the failure `build_token` exists to convert into a clean
/// termination.
#[test]
fn the_worker_protocol_version_is_the_one_these_shapes_ship() {
    assert!(
        WORKER_PROTOCOL.contains("const PROTOCOL_VERSION: u32 = 12;"),
        "worker_protocol.rs does not declare PROTOCOL_VERSION 12. Version 3 \
         added the `nyq` field, where a plan-view reply began reporting the \
         fold limit of the sweep it drew; version 4 added `mls`, where it \
         began reporting which melting layer the classification stood on — a \
         reply that omits it is a reply whose classification cannot say \
         whether it was measured or guessed; version 5 widened `outkind` past \
         `RenderView`'s three codes, when a decoded Level II volume became an \
         output a worker could answer with. A version 4 worker has no encoder \
         for that and a version 4 page no decoder, and either mismatch is a \
         decode that silently produces nothing — the browser's whole radar \
         picture missing, with no error. Version 6 added `smv`, where a \
         storm-relative reply began reporting which storm motion vector it was \
         shifted by: the RPG's own, or one of the local stand-ins that disagree \
         with it on 83 % of gates and on more than half of them by two display \
         levels or more. A reply that omits it is one whose storm-relative \
         field cannot say which quantity it is showing. Version 7 added `sms` \
         and `smd`, the speed and direction of that vector, when the pane \
         stopped apologising for its storm motion and started reporting it: \
         the legend draws the numbers, and the two derived rungs are fitted \
         from a wind profile the page never sees, so a reply that omits them \
         is one whose storm-relative field shows no vector at all. Version 9 \
         added the overlay job (page->worker tag 8) and its reply, `outkind` \
         code 5 — a version 8 pair mismatched with a 9 exchanges jobs one half \
         refuses and payloads the other drops, which is a site-marker layer \
         that silently never draws. Version 10 widened tag 8's inner code \
         space with the three polygon overlay kinds (alerts, outlooks, \
         discussions, input codes 2-4): a version 9 worker refuses those \
         codes and answers a failed job, which is a warning layer that \
         silently never draws. Version 11 added the two hit-map kinds (storm \
         reports code 5, lightning code 6) and reframed `outkind` code 5's \
         payload: a hit-cells block now precedes the raw RGBA, and the GLM \
         input carries the page's clock so a worker never ages a flash \
         against its own. A version 10 pairing exchanges inner codes one \
         half refuses and reply payloads whose length check the other half \
         fails — a storm-reports layer that silently never draws. Version 12 \
         completed that inner code space with the HRRR model grid (input code \
         7, the grid's Lambert constants plus its projection window's \
         values), the last overlay kind that still rasterized inline on the \
         page: a version 11 worker refuses code 7 and answers a failed job, \
         which is a model layer that silently never draws. Changing \
         the message shapes without changing this number is the whole failure \
         it prevents.",
    );
}

/// Every arm of the worker's reply writes every field.
///
/// The page holds a render slot, a pane's in-flight mark and a pending-map
/// entry against each id it posted, and only a reply releases them — so a path
/// through `post_result` that left a field absent, or posted nothing at all,
/// wedges that pane forever with no error anywhere. The defaults are written
/// once before the match, which is what makes "every arm" a property of the
/// shape rather than of three arms agreeing; this pins that shape, because the
/// function is `wasm32`-only and no host test can run it.
#[test]
fn the_worker_reply_writes_every_field_on_every_arm() {
    let body = RASTER_WORKER_RS
        .split_once("fn post_result(")
        .expect("worker.rs no longer has a post_result")
        .1;
    let defaults = body
        .split_once("match result {")
        .expect("post_result no longer matches on the result")
        .0;
    for field in [
        // Matched **with the trailing comma**, because `proto::OUT` is a prefix
        // of `proto::OUT_KIND`: without it, deleting the `OUT` default outright
        // still satisfied this loop, which is exactly how that mutation
        // survived this test's first draft.
        "proto::IMAGE,",
        "proto::POLAR,",
        "proto::MAX_RANGE,",
        "proto::NYQUIST,",
        // The two provenance fields were missing from this list, which is the
        // one thing it exists to enumerate: both are written before the match
        // exactly as the others are, and neither was pinned. A default deleted
        // from either would have left every non-SRV, non-classification reply
        // carrying a stale provenance from whatever the worker answered last.
        "proto::MELTING_LAYER,",
        // The trailing comma is doing real work on these three: without it
        // `proto::STORM_MOTION,` would be satisfied by `proto::STORM_MOTION_SPEED`
        // and the plain provenance byte could vanish unnoticed.
        "proto::STORM_MOTION,",
        // The speed and direction shipped as version 7's whole reason and were
        // written only inside the `Frame` arm's `if let`, so every other reply
        // left them absent rather than null while `smv` beside them was null.
        "proto::STORM_MOTION_SPEED,",
        "proto::STORM_MOTION_DIR,",
        "proto::OUT,",
        "proto::OUT_KIND,",
    ] {
        assert!(
            defaults.contains(field),
            "{field} is not written before the match in post_result, so an arm \
             that does not set it leaves it absent and the page cannot tell a \
             null answer from a lost one"
        );
    }
}

/// The `Frame` arm still writes the four fields it always wrote, and still
/// transfers both buffers.
///
/// The second buffer changed shape without changing role: it carried the
/// `side²` `f32` raster value grid as a `Float32Array` and now carries the
/// gates behind those pixels as bytes — 16 MiB against about 5 MiB for the
/// widest sweep, and a few kilobytes for a loop frame, which sends geometry and
/// no values at all. What this test is about is unchanged, and is why the field
/// is still checked by name: it must be *written* and it must be
/// **transferred**, or the browser copies it per frame instead of moving it.
///
/// Widening the reply to carry sections and grids was supposed to leave the
/// working path untouched, and that claim is otherwise verified only by
/// reading the diff. A `Frame` arm that stopped setting `MAX_RANGE`, or that
/// lost a `transfer.push`, would not fail to compile and would not fail any
/// test that exists — it would copy 4 MiB per frame instead of moving it, or
/// report every plan view's range as the `0.0` default, which is a texture
/// projected at the wrong scale rather than an error.
///
/// `NYQUIST` joined the three when the plan view began reporting where the
/// sweep it drew folds. It is written here and nowhere else on this arm, and a
/// `Frame` reply that stopped writing it would leave every velocity pane in
/// the browser unable to say where its own picture wraps — silently, because
/// the default written before the match is a legitimate answer for the Level
/// III and volume products.
#[test]
fn the_frame_arm_of_the_worker_reply_is_unchanged() {
    let arm = RASTER_WORKER_RS
        .split_once("Some(JobOutput::Frame(RenderedFrame {")
        .expect("post_result no longer has a Frame arm")
        .1
        .split_once("Some(output) => {")
        .expect("the Frame arm is no longer followed by the out-of-band arm")
        .0;
    for needle in [
        "proto::IMAGE, &image",
        "proto::POLAR, &polar",
        "proto::MAX_RANGE,",
        "proto::NYQUIST,",
        "transfer.push(&image.buffer());",
        "transfer.push(&polar.buffer());",
    ] {
        assert!(
            arm.contains(needle),
            "the Frame arm no longer contains {needle:?}; the plan-view reply \
             is supposed to be byte-for-byte what it was before the widening"
        );
    }
    assert!(
        !arm.contains("proto::OUT"),
        "the Frame arm writes an out-of-band field; a frame travels in \
         IMAGE/POLAR/MAX_RANGE/NYQUIST and nothing else, and a reply carrying \
         both leaves the page arbitrating between two outputs for one job"
    );
}

// ---------------------------------------------------------------------------
// the reply's shape, read off the source rather than restated
// ---------------------------------------------------------------------------
//
// Everything above enumerates *names it expects to find*, which catches a
// deletion and is blind to an addition: the three tests before this one all
// stayed green through a version-7 reply that wrote two fields no list here
// mentioned. What follows extracts the whole set instead, so a field nobody
// added to a list is a field that fails.

/// `ident -> wire key` for every `pub const <NAME>: &str = "<key>";` in
/// `worker_protocol.rs` — the vocabulary a message may be built out of.
///
/// The declarations are the whole specification of this boundary. There is no
/// serde on it: both directions build a bare `js_sys::Object` and set fields on
/// it by name, so no type anywhere binds a reply's shape and nothing but a read
/// of this source can say what the shape is.
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

/// The `PROTOCOL_VERSION` literal, as a number.
fn declared_protocol_version() -> u32 {
    let src = without_line_comments(WORKER_PROTOCOL);
    let rest = src
        .split_once("const PROTOCOL_VERSION: u32 = ")
        .expect("worker_protocol.rs no longer declares PROTOCOL_VERSION: u32")
        .1;
    rest.chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .expect("PROTOCOL_VERSION is not a plain decimal literal")
}

/// `post_result`'s body, cut at the `post_message` that ends it so the only
/// `&message` left in it belongs to a `set_field`.
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

/// That body split into the three places a field can be written: before the
/// match, in the `Frame` arm, in the arm that carries everything else.
///
/// The same three slices the tests above take, by the same markers. The `None`
/// arm falls in no slice because it writes nothing; if it ever writes something
/// the call-site count in
/// [`the_worker_reply_shape_is_the_one_this_protocol_version_declares`] is what
/// notices, which is the point of counting.
fn post_result_arms() -> Vec<(&'static str, String)> {
    let body = post_result_body();
    let (head, tail) = body
        .split_once("match result {")
        .expect("post_result no longer matches on the result");
    let (frame, out) = tail
        .split_once("Some(JobOutput::Frame(RenderedFrame {")
        .expect("post_result no longer has a Frame arm")
        .1
        .split_once("Some(output) => {")
        .expect("the Frame arm is no longer followed by the out-of-band arm");
    vec![
        ("head", head.to_string()),
        ("frame", frame.to_string()),
        ("out", out.to_string()),
    ]
}

/// The key argument of every `set_field(&message, <KEY>, ..)` in one slice, in
/// source order.
///
/// Reads the *argument*, not every `proto::` token: `KIND`'s value is
/// `proto::DONE`, and a token scan would call that a field.
fn message_field_idents(arm: &str) -> Vec<String> {
    arm.split("&message,")
        .skip(1)
        .filter_map(|chunk| {
            let rest = chunk.trim_start().strip_prefix("proto::")?;
            Some(
                rest.chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect(),
            )
        })
        .collect()
}

/// The shape of a `done` reply, whole, pinned against the version that declares
/// it.
///
/// # What was blind, exactly
///
/// `the_worker_protocol_version_is_the_one_these_shapes_ship` asserts the
/// `PROTOCOL_VERSION` literal. It fires for someone who bumps the number and
/// forgets this file — the harmless direction. The direction that ships a
/// defect is the inverse: change what a reply carries, leave the number alone,
/// and a page from one side of a deploy and a worker from the other exchange a
/// message one of them cannot read, with `build_token` reporting a match. That
/// is the failure the number exists to prevent, and nothing checked it. The
/// three enumerations above are no help: each is a list of names it expects to
/// *find*, so a reply that grows a field passes all of them.
///
/// # What this still cannot see, and what now sees it instead
///
/// It watches the reply's **field set**, so it catches a field added, removed
/// or renamed. It cannot catch a change to the *bytes inside* a field, because
/// the field set is identical either side of one. `POLAR` carries
/// `rustdar_radar::render::polar::PolarField::to_bytes`, a hand-rolled layout
/// whose header grew an `f64` at protocol version 8 without a single name here
/// changing — this test passed, unmodified, across that change, and the bump
/// was made by hand.
///
/// That gap is now covered by a digest rather than by this test, because a
/// buffer's layout is not visible from a crate that cannot link the encoder.
/// Each of the three codecs that crosses this port inside a buffer pins the
/// bytes it produces over a fixture built from literals:
///
///   * `rustdar_radar::render::polar::tests::the_polar_wire_layout_is_the_one_this_protocol_ships`
///     for `POLAR`;
///   * `rustdar_radar::volume_wire::tests::the_volume_wire_layout_is_the_one_this_version_ships`
///     for the decoded volume `OUT` carries under code 4 (`OUT`'s other two
///     payloads, a section and a voxel grid, were already pinned by their own
///     crates);
///   * `rustdar_frontend::offload::tests::the_job_framing_is_the_one_this_protocol_ships`
///     for the page->worker direction named below.
///
/// The overlay payload `OUT` carries under code 5 was raw premultiplied RGBA
/// with no layout at all until protocol version 11, when the hit-map kinds'
/// cells started riding ahead of the pixels — the first reply-shape change
/// since these guards landed, and one this test sees not at all, since no
/// field name moved. It is digested now like the other three codecs:
/// `rustdar_frontend::offload::tests::the_overlay_reply_framing_is_the_one_this_protocol_ships`
/// pins `encode_overlay_out`'s bytes over a literal fixture. The RGBA tail
/// inside that framing still has no layout to digest, and its guard is still
/// the dispatching page's own `width × height × 4` length check — which is
/// also what fails, on either side, when a version 10 half meets a version 11
/// half over this code.
///
/// Each was measured against an uncooperative regressor: a same-width field
/// reorder made to encoder and decoder in step left every other test in its
/// crate green (890 of 891 in `rustdar-radar`, 807 of 808 in
/// `rustdar-frontend`) and these two guards here green as well, and only the
/// digest fell over. None of them can check that `PROTOCOL_VERSION` was
/// *raised* — it is `wasm32`-only and downstream of both crates — so what they
/// buy is that the person who changes a layout is stopped and told what they
/// owe, which is the direction that was missing.
///
/// # Why a list and not a digest
///
/// A hash of the shape would be exactly as binding and would say nothing. This
/// list makes the diff of a shape change read `+ "frame | HAIL_SIZE | hsz"`
/// beside `- "PROTOCOL_VERSION = 7"`, which is the sentence the author needs to
/// see, and it stays greppable: the wire key of any field is findable from
/// here.
///
/// # Why a source scrape
///
/// `worker_protocol` and `worker` are both `#[cfg(target_arch = "wasm32")]`, so
/// nothing a host `cargo test` compiles can name `post_result` or
/// `PROTOCOL_VERSION`, and the wasm CI rows are `wasm-pack build` and
/// `cargo check` — neither runs a test. There is no `wasm-bindgen-test` in this
/// workspace. Reading the source is the only instrument available, and the
/// tests above already do it.
///
/// # What this does not cover
///
/// The page→worker `job` direction and the `hello`/`fatal` messages. The `job`
/// direction is a byte codec rather than a field set, and it is pinned by
/// `rustdar_frontend`'s `the_job_framing_is_the_one_this_protocol_ships` — see
/// above. `hello` and `fatal` are still built elsewhere and are as unbound as
/// this one was.
#[test]
fn the_worker_reply_shape_is_the_one_this_protocol_version_declares() {
    let vocabulary = worker_protocol_vocabulary();
    let arms = post_result_arms();

    let mut shape = vec![format!(
        "PROTOCOL_VERSION = {}",
        declared_protocol_version()
    )];
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
    shape.extend(written.iter().cloned());

    // A `set_field` the extractor above did not recognise — a different target
    // than `&message`, a key that is not a `proto::` path — would otherwise be
    // silently skipped, and a guard with a hole in it is worse than none
    // because it reads green over the hole.
    let call_sites = post_result_body().matches("proto::set_field(").count();
    assert_eq!(
        call_sites,
        written.len(),
        "post_result makes {call_sites} set_field calls but only {} of them \
         were recognised as `set_field(&message, proto::NAME, ..)`. The \
         unrecognised ones are invisible to the shape below, so fix the \
         extraction (or the call) before reading its verdict.",
        written.len()
    );

    assert_eq!(
        shape,
        [
            "PROTOCOL_VERSION = 12",
            "frame | IMAGE | image",
            "frame | MAX_RANGE | range",
            "frame | MELTING_LAYER | mls",
            "frame | NYQUIST | nyq",
            "frame | POLAR | polar",
            "frame | STORM_MOTION | smv",
            "frame | STORM_MOTION_DIR | smd",
            "frame | STORM_MOTION_SPEED | sms",
            "head | ID | id",
            "head | IMAGE | image",
            "head | KIND | kind",
            "head | MAX_RANGE | range",
            "head | MELTING_LAYER | mls",
            "head | NYQUIST | nyq",
            "head | OUT | out",
            "head | OUT_KIND | outkind",
            "head | POLAR | polar",
            "head | STORM_MOTION | smv",
            "head | STORM_MOTION_DIR | smd",
            "head | STORM_MOTION_SPEED | sms",
            "out | OUT | out",
            "out | OUT_KIND | outkind",
        ]
        .map(str::to_string),
        "the shape of a `done` reply is not the shape PROTOCOL_VERSION says it \
         is. Left is what worker.rs and worker_protocol.rs say today; right is \
         what this list was last told.\n\n\
         If a wire key appeared or disappeared: bump PROTOCOL_VERSION in \
         worker_protocol.rs FIRST, then update this list to match — in that \
         order, because the number is the thing a page and a worker from \
         opposite sides of a deploy actually compare, and a list brought into \
         line without it leaves this test green over exactly the mismatch it \
         exists to catch.\n\n\
         If the wire keys are unchanged and only an arm moved — a field \
         defaulted before the match that used to be written inside one arm — \
         then both halves still read the same message, the version stands, and \
         only this list moves."
    );
}

/// Every field an arm writes is also written before the match.
///
/// This is the invariant `post_result` states in prose over its default block:
/// *written first and overwritten by the arm that has one, so no path out of
/// this function can leave a field absent*. An arm that writes a field the
/// defaults do not breaks it, and breaks it quietly — the page reads an absent
/// field and a null one through the same `as_f64` filter, so the reply that
/// omits one is indistinguishable from the reply that says "none" until some
/// later reader stops being lenient about the difference.
///
/// It is a derived check and needs no list: the two sets come out of the same
/// extraction the shape test uses. `smv`'s speed and direction were written on
/// the `Frame` arm and nowhere else, which is what this catches.
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
