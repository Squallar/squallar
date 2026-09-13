//! **The wall rig's two promises that no browser leg can check.**
//!
//! M1 adds a page that deliberately grows one shared linear memory to 4 GiB of
//! incompressible bytes, and a console beacon that forwards the app's memory
//! lines to a box on the LAN. Both are rig tools, and both would be harmful on
//! the public site: the calibrate page kills tabs by design, and it is served
//! from the same origin as the app. So two things are held here, from text,
//! where a green browser leg says nothing about either:
//!
//! 1. **The calibrate page is never deployed and never precached.** It lives in
//!    `.github/browser-rig/` and `serve.py` serves it from there. The web deploy
//!    stages an opt-in list out of `squallar-web/`, and `sw.js` precaches only
//!    what its lists name. Each of those is checked, each with a doctored copy
//!    that must go red.
//! 2. **Every console needle the beacon forwards as `written` names a line the
//!    app really writes.** A needle nobody writes forwards nothing forever and
//!    reads, on a phone with no driver, exactly like a device that never booted.
#![cfg(not(target_arch = "wasm32"))]

use std::path::{Path, PathBuf};

const BUILD_YAML: &str = include_str!("../../.github/workflows/build.yaml");
const SERVICE_WORKER: &str = include_str!("../sw.js");
const SERVE_PY: &str = include_str!("../../.github/browser-rig/serve.py");
const CALIBRATE_PAGE: &str = include_str!("../../.github/browser-rig/calibrate.html");

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("squallar-web sits inside the workspace")
        .to_path_buf()
}

/// The `web-wasm32` matrix row of `build.yaml`: from its `- name:` line to the
/// next matrix entry at the same indentation.
fn web_row(yaml: &str) -> Option<&str> {
    const HEAD: &str = "          - name: web-wasm32\n";
    let start = yaml.find(HEAD)?;
    let after = start + HEAD.len();
    let end = yaml[after..]
        .find("\n          - name: ")
        .map_or(yaml.len(), |at| after + at);
    Some(&yaml[start..end])
}

/// Why `yaml` could ship the calibrate page, one sentence a defect.
fn deploy_defects(yaml: &str) -> Vec<String> {
    let Some(row) = web_row(yaml) else {
        return vec![
            "build.yaml has no `web-wasm32` row, so nothing here knows what the web \
             deploy stages"
                .to_string(),
        ];
    };
    let mut defects = Vec::new();
    let copies: Vec<&str> = row
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("cp "))
        .collect();
    if copies.len() < 3 {
        defects.push(format!(
            "only {} `cp` staging lines were read out of the web row; the staging \
             moved and this check is reading nothing",
            copies.len()
        ));
    }
    for line in &copies {
        if line.contains("calibrate") || line.contains("browser-rig") {
            defects.push(format!("the web row stages a rig file: {line:?}"));
        }
        // Every source is under squallar-web/: the opt-in list is what keeps a
        // rig directory out of `dist/`, so a copy from anywhere else is the
        // shape a leak would take even before it names this page.
        for word in line.split_whitespace().skip(1) {
            if word.starts_with('-') || word.starts_with("dist") {
                continue;
            }
            if !word.starts_with("squallar-web/") {
                defects.push(format!(
                    "the web row copies {word:?}, which is not under squallar-web/: {line:?}"
                ));
            }
        }
    }
    for (n, line) in yaml.lines().enumerate() {
        if line.contains("calibrate.html") {
            defects.push(format!(
                "build.yaml:{} names the calibrate page: {line:?}",
                n + 1
            ));
        }
    }
    defects
}

/// The string literals of the JS array that starts at `marker`, up to its `]`.
fn js_list(src: &str, marker: &str) -> Option<Vec<String>> {
    let start = src.find(marker)? + marker.len();
    let end = start + src[start..].find(']')?;
    let mut out = Vec::new();
    let mut rest = &src[start..end];
    while let Some(open) = rest.find('"') {
        rest = &rest[open + 1..];
        let close = rest.find('"')?;
        out.push(rest[..close].to_string());
        rest = &rest[close + 1..];
    }
    Some(out)
}

/// Why `sw` could precache or route the calibrate page.
fn service_worker_defects(sw: &str) -> Vec<String> {
    let mut defects = Vec::new();
    let mut entries = 0;
    for marker in ["SHELL_PATHS = [", "SHELL_DIRS = [", "ASSET_PATHS = ["] {
        let Some(list) = js_list(sw, marker) else {
            defects.push(format!("sw.js no longer has `{marker}`"));
            continue;
        };
        entries += list.len();
        for entry in list {
            if entry.contains("calibrate") || entry.starts_with("rig/") || entry.contains("/rig/") {
                defects.push(format!("sw.js `{marker}` names a rig page: {entry:?}"));
            }
        }
    }
    if entries < 3 {
        defects.push(format!(
            "only {entries} entries were read out of sw.js's cache lists; the parse \
             stopped matching"
        ));
    }
    if sw.contains("calibrate") {
        defects.push("sw.js mentions the calibrate page".to_string());
    }
    defects
}

/// **No deploy row stages the calibrate page, and a doctored one is caught.**
#[test]
fn the_calibrate_page_is_staged_by_no_deploy() {
    assert_eq!(deploy_defects(BUILD_YAML), Vec::<String>::new());

    // Presence control: the leak, planted where it would go.
    let row = web_row(BUILD_YAML).expect("checked above");
    let anchor = row
        .lines()
        .find(|l| l.trim().starts_with("cp squallar-web/index.html"))
        .expect("the web row copies index.html");
    let doctored = BUILD_YAML.replacen(
        anchor,
        &format!("{anchor}\n              cp .github/browser-rig/calibrate.html dist/"),
        1,
    );
    assert_ne!(doctored, BUILD_YAML, "the tamper did not apply");
    let defects = deploy_defects(&doctored);
    assert!(
        defects.iter().any(|d| d.contains("stages a rig file")),
        "a web row staging the calibrate page went unnoticed: {defects:?}"
    );
}

/// **`sw.js` precaches and routes no rig page, and a doctored one is caught.**
#[test]
fn the_calibrate_page_is_precached_by_no_service_worker_list() {
    assert_eq!(service_worker_defects(SERVICE_WORKER), Vec::<String>::new());

    let doctored = SERVICE_WORKER.replacen(
        "SHELL_PATHS = [",
        "SHELL_PATHS = [\"rig/calibrate.html\", ",
        1,
    );
    assert_ne!(doctored, SERVICE_WORKER, "the tamper did not apply");
    let defects = service_worker_defects(&doctored);
    assert!(
        defects.iter().any(|d| d.contains("SHELL_PATHS")),
        "a precached calibrate page went unnoticed: {defects:?}"
    );
}

/// Every file under `dir` (recursively), skipping wasm-pack's generated `pkg/`.
fn files_under(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "pkg") {
                continue;
            }
            files_under(&path, out);
        } else {
            out.push(path);
        }
    }
}

/// **The page lives in the rig directory, and `serve.py` serves it from there.**
///
/// The deploy stages from `squallar-web/`, so a copy of the page landing there
/// is one `cp` line away from shipping. Checked by walking the directory rather
/// than by listing it, so a copy under any name containing `calibrate` counts.
#[test]
fn the_calibrate_page_lives_outside_the_served_web_directory() {
    assert!(
        CALIBRATE_PAGE.contains("NEVER DEPLOYED, NEVER PRECACHED"),
        "calibrate.html no longer says what it is at its top"
    );
    let mut files = Vec::new();
    files_under(Path::new(env!("CARGO_MANIFEST_DIR")), &mut files);
    assert!(
        files.len() > 5,
        "walked only {} files under squallar-web/",
        files.len()
    );
    let copies: Vec<_> = files
        .iter()
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.contains("calibrate"))
                && p.extension().is_some_and(|e| e != "rs")
        })
        .collect();
    assert!(
        copies.is_empty(),
        "a calibrate page sits under squallar-web/, where the deploy stages from: {copies:?}"
    );
    for line in [
        "CALIBRATE_ROUTE = \"/rig/calibrate.html\"",
        "CALIBRATE_FILE = os.path.join(RIG_DIR, \"calibrate.html\")",
    ] {
        assert!(
            SERVE_PY.contains(line),
            "serve.py no longer carries `{line}`: the page is served from somewhere \
             this pin does not know about"
        );
    }
}

/// `(needle, status, where)` rows out of serve.py's `CONSOLE_BEACON_NEEDLES`.
///
/// One row per `    ("` at the tuple's indentation; every string literal in
/// the row is collected, so a `where` spelled as Python's implicit
/// concatenation across lines arrives whole.
fn beacon_needles(serve: &str) -> Vec<(String, String, String)> {
    let Some(start) = serve.find("CONSOLE_BEACON_NEEDLES = (") else {
        return Vec::new();
    };
    let block = &serve[start..];
    let block = &block[..block.find("\n)\n").unwrap_or(block.len())];
    let mut rows = Vec::new();
    for row in block.split("\n    (\"").skip(1) {
        let row = format!("\"{row}");
        let mut strings = Vec::new();
        let mut rest = row.as_str();
        while let Some(open) = rest.find('"') {
            rest = &rest[open + 1..];
            let Some(close) = rest.find('"') else {
                break;
            };
            strings.push(rest[..close].to_string());
            rest = &rest[close + 1..];
        }
        if strings.len() >= 3 {
            rows.push((
                strings[0].clone(),
                strings[1].clone(),
                strings[2..].concat(),
            ));
        }
    }
    rows
}

/// Why one needle row does not name a line the app writes.
fn needle_defect(needle: &str, status: &str, where_: &str, root: &Path) -> Option<String> {
    let files: Vec<PathBuf> = where_
        .split([',', ' ', '(', ')'])
        .filter(|w| w.contains('/') && (w.ends_with(".rs") || w.ends_with(".js")))
        .map(|w| root.join(w))
        .collect();
    match status {
        "written" => {
            let texts: Vec<String> = files
                .iter()
                .filter_map(|p| std::fs::read_to_string(p).ok())
                .collect();
            if texts.is_empty() {
                return Some(format!(
                    "needle {needle:?} names no readable source file in {where_:?}"
                ));
            }
            if texts.iter().any(|t| t.contains(needle)) {
                return None;
            }
            // `<kind> took`: the kind is a job label and the tail a formatter.
            if let Some(kind) = needle.strip_suffix(" took") {
                let label = format!("const LABEL: &'static str = \"{kind}\";");
                if texts.iter().any(|t| t.contains(&label))
                    && texts.iter().any(|t| t.contains("took {} ms off the frame"))
                {
                    return None;
                }
            }
            Some(format!(
                "needle {needle:?} is marked written and none of {where_:?} writes it"
            ))
        }
        "forward" => None,
        other => Some(format!("needle {needle:?} has status {other:?}")),
    }
}

/// **Every `written` beacon needle is a line the app writes; the forward one is
/// not written yet; and a doctored needle is caught.**
#[test]
fn every_written_beacon_needle_is_a_line_the_app_writes() {
    let root = repo_root();
    let rows = beacon_needles(SERVE_PY);
    assert!(
        rows.len() >= 8,
        "read {} needle rows out of serve.py's CONSOLE_BEACON_NEEDLES: {rows:?}",
        rows.len()
    );
    let defects: Vec<String> = rows
        .iter()
        .filter_map(|(n, s, w)| needle_defect(n, s, w, &root))
        .collect();
    assert_eq!(defects, Vec::<String>::new());
    assert!(
        !rows.iter().any(|(n, _, _)| n == "render took"),
        "`render took` is forwarded, and no job kind is named `render`"
    );

    // The forward needle is forward: the day heap.js or the web crate writes
    // it, its status must say `written` so the check above holds it.
    let forward: Vec<&str> = rows
        .iter()
        .filter(|(_, s, _)| s == "forward")
        .map(|(n, _, _)| n.as_str())
        .collect();
    assert_eq!(forward, ["linear memory ladder:"]);
    let mut web = Vec::new();
    files_under(&root.join("squallar-web/src"), &mut web);
    web.push(root.join("squallar-web/heap.js"));
    for path in &web {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        assert!(
            !text.contains("linear memory ladder:"),
            "{} now writes `linear memory ladder:`; flip its needle to `written`",
            path.display()
        );
    }

    // Presence control: one needle respelled must be named.
    let doctored = SERVE_PY.replacen(
        "(\"budget state:\", \"written\"",
        "(\"budget stat3:\", \"written\"",
        1,
    );
    assert_ne!(doctored, SERVE_PY, "the tamper did not apply");
    let red: Vec<String> = beacon_needles(&doctored)
        .iter()
        .filter_map(|(n, s, w)| needle_defect(n, s, w, &root))
        .collect();
    assert!(
        red.iter().any(|d| d.contains("budget stat3:")),
        "a needle nobody writes passed: {red:?}"
    );
}
