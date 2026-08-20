//! Architectural ratchets: ceilings on coupling metrics, enforced as tests.
//!
//! Every ceiling may only move DOWN — the land that earns a lower count lowers
//! the MAX const with it. Each ceiling sits beside a positive check on the same
//! haystack, so a moved or renamed haystack fails loudly instead of counting
//! zero and going silently green.
//!
//! Needle hygiene: every counted pattern here is built from split literals
//! (`concat!("self.", "gui.")` style) and the walker skips this file, so the
//! file never contains a counted pattern contiguously.
//!
//! Counting semantics: the asserted metrics count OCCURRENCES (`rg -o … | wc -l`
//! on the command side, summed `str::matches` here). The wasm-cfg rows count
//! matching LINES per crate (`rg -c` summed). Commands run from the workspace
//! root; the walker skips dirs named `target`/`pkg` and never leaves the
//! workspace.
//!
//! Baseline measured 2026-08-18 at main @ 854f4a64:
//!
//! ```text
//!  #   metric                                        value  command (run from the workspace root)
//!  1a  App-pokes-Gui occurrences, rustdar-app          192  rg -o 'self\.''gui\.' rustdar-app --glob '*.rs' | wc -l
//!  1b  ... excluding test-named paths                  186  rg -o 'self\.''gui\.' rustdar-app --glob '*.rs' -g '!*tests*' | wc -l
//!  2   Gui setter fns in rustdar-egui/src/ui.rs          3  rg -o 'pub fn ''set_' rustdar-egui/src/ui.rs | wc -l
//!  3   wasm-cfg lines per crate  [NOT ASSERTED]          -  rg -c 'target_arch = "wasm''32"' "$c" --glob '*.rs'
//!  4a  product-enum occurrences in rustdar-egui        444  rg -o 'Radar''Product' rustdar-egui --glob '*.rs' | wc -l
//!  4b  ... files containing it (info)                   29  rg -l 'Radar''Product' rustdar-egui --glob '*.rs' | wc -l
//!  6   ChannelHub receiver fields                       18  rg -o '_receiver: ''Receiver<' rustdar-app/src/channels.rs | wc -l
//!  7a  overlays-crate path occurrences in offload.rs     0  rg -o 'rustdar_''overlays::' rustdar-worker/src/offload.rs | wc -l
//!  7b  radar-crate path occurrences in offload.rs        0  rg -o 'rustdar_''radar::' rustdar-worker/src/offload.rs | wc -l
//! ```
//!
//! Row 3 is recorded, not asserted, by user ruling: no count ratchets on style
//! metrics. Row 5 (the overlay-kind enum) is retired — the enum is gone, and
//! `rustdar_overlays::render::overlay_state::overlay_kind_stays_deleted_tests`
//! holds its absence.

use std::fs;
use std::path::{Path, PathBuf};

/// Workspace root: this integration test's manifest dir is `rustdar-app/`.
const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/..");

// ---------------------------------------------------------------------------
// Needles — split literals so this file never contains what it counts.

const SELF_GUI: &str = concat!("self.", "gui.");
const PUB_FN_SET: &str = concat!("pub fn ", "set_");
const PRODUCT_ENUM: &str = concat!("Radar", "Product");
const RECEIVER_FIELD: &str = concat!("_receiver: ", "Receiver<");
const OVERLAYS_PATH: &str = concat!("rustdar_", "overlays::");
const RADAR_PATH: &str = concat!("rustdar_", "radar::");

// Presence anchors (the positive half of each check; definition anchors are
// split so a future absence-grep for a deleted definition stays clean).
const APP_ANCHOR: &str = concat!("pub struct ", "App");
const GUI_IMPL_ANCHOR: &str = concat!("impl ", "Gui");
const UI_MOD_ANCHOR: &str = concat!("mod ", "ui;");
const HUB_ANCHOR: &str = concat!("struct ", "ChannelHub");
const OFFLOAD_ANCHOR: &str = concat!("pub fn ", "offload_job(");
const PRODUCT_DEF_ANCHOR: &str = concat!("enum Radar", "Product");

// --------------------------------------------------------------------------- Ceilings
// — at-land measurements (see the table above).

/// Row 1a.
const SELF_GUI_MAX: usize = 192;
/// Row 1b — the same needle outside test-named paths.
const SELF_GUI_NON_TEST_MAX: usize = 186;
/// Row 2.
const UI_SETTER_MAX: usize = 3;
/// Row 4a.
const PRODUCT_IN_EGUI_MAX: usize = 444;
/// Row 6.
const HUB_RECEIVER_MAX: usize = 18;

// --------------------------------------------------------------------------- Walker +
// counters (std-only, pure file reads).

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| {
        panic!(
            "presence control: cannot read {} ({e}) — the haystack moved; a ceiling \
             that counts zero on a missing haystack is a dead guard. Re-anchor this \
             ratchet in the land that moves the file.",
            path.display()
        )
    })
}

/// Every `.rs` file under `dir`, recursively, skipping dirs named `target` or `pkg`
/// (build output — the same set ripgrep ignores here).
fn rs_files_under(dir: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let entries = fs::read_dir(dir).unwrap_or_else(|e| {
            panic!(
                "presence control: haystack dir {} is unreadable ({e}) — the tree \
                 moved; re-anchor this ratchet in the land that moves it.",
                dir.display()
            )
        });
        for entry in entries {
            let entry = entry.expect("readable directory entry");
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if path.is_dir() {
                if name == "target" || name == "pkg" {
                    continue;
                }
                walk(&path, out);
            } else if name.ends_with(".rs") && name != "arch_ratchets.rs" {
                out.push(path);
            }
        }
    }
    let mut files = Vec::new();
    walk(dir, &mut files);
    assert!(
        !files.is_empty(),
        "presence control: haystack {} holds no .rs files — a ceiling over an empty \
         haystack is a dead guard; re-anchor this ratchet in the land that emptied it.",
        dir.display()
    );
    files
}

fn load_tree(dir: &Path) -> Vec<(PathBuf, String)> {
    rs_files_under(dir)
        .into_iter()
        .map(|p| {
            let text = read(&p);
            (p, text)
        })
        .collect()
}

fn count(files: &[(PathBuf, String)], needle: &str) -> usize {
    files.iter().map(|(_, t)| t.matches(needle).count()).sum()
}

/// The positive half of a walked-haystack check: the anchor file must be in the WALKED
/// set, so a broken walker fails here.
fn assert_anchored(files: &[(PathBuf, String)], suffix: &str, anchor: &str) {
    let (path, text) = files
        .iter()
        .find(|(p, _)| p.ends_with(suffix))
        .unwrap_or_else(|| {
            panic!(
                "presence control: no walked file ends with {suffix} — the anchor \
                 file moved or the walker broke; re-anchor this ratchet in the land \
                 that moved it."
            )
        });
    assert!(
        text.contains(anchor),
        "presence control: {} no longer contains the anchor {anchor:?} — re-anchor \
         this ratchet in the land that changed it.",
        path.display()
    );
}

/// Single-file haystack: read (loud on a moved file) + anchor check.
fn anchored_file(path: &Path, anchor: &str) -> String {
    let text = read(path);
    assert!(
        text.contains(anchor),
        "presence control: {} no longer contains the anchor {anchor:?} — re-anchor \
         this ratchet in the land that changed it.",
        path.display()
    );
    text
}

/// Mirrors ripgrep's `-g '!*tests*'`: a path is test-side when any component
/// under the crate root has a name containing "tests".
fn in_test_path(path: &Path, crate_root: &Path) -> bool {
    path.strip_prefix(crate_root)
        .expect("walked file lies under its crate root")
        .components()
        .any(|c| c.as_os_str().to_string_lossy().contains("tests"))
}

// ---------------------------------------------------------------------------
// The ratchets.

/// Row 1 — the App-pokes-Gui coupling (occurrences of the split needle in rustdar-app).
#[test]
fn the_app_pokes_gui_coupling_never_grows() {
    let crate_root = Path::new(ROOT).join("rustdar-app");
    let files = load_tree(&crate_root);
    assert_anchored(&files, "src/app.rs", APP_ANCHOR);

    let total = count(&files, SELF_GUI);
    let non_test: usize = files
        .iter()
        .filter(|(p, _)| !in_test_path(p, &crate_root))
        .map(|(_, t)| t.matches(SELF_GUI).count())
        .sum();

    assert!(
        total <= SELF_GUI_MAX,
        "the App-pokes-Gui coupling grew: {total} occurrences > ceiling {SELF_GUI_MAX}. \
         WO-E2/WO-E8 drive this to 0 via GuiEvent. Lower the MAX in the land that \
         earns it; never raise it without a written plan amendment."
    );
    assert!(
        non_test <= SELF_GUI_NON_TEST_MAX,
        "the App-pokes-Gui coupling grew outside tests: {non_test} occurrences > \
         ceiling {SELF_GUI_NON_TEST_MAX}. WO-E2/WO-E8 drive this to 0 via GuiEvent. \
         Lower the MAX in the land that earns it; never raise it without a written \
         plan amendment."
    );
}

/// Row 2 — setter fns on the Gui shell.
#[test]
fn the_gui_setter_surface_never_grows() {
    let ui_rs = Path::new(ROOT).join("rustdar-egui/src/ui.rs");
    let text = anchored_file(&ui_rs, GUI_IMPL_ANCHOR);
    let n = text.matches(PUB_FN_SET).count();
    assert!(
        n <= UI_SETTER_MAX,
        "the Gui setter surface grew: {n} setter fns > ceiling {UI_SETTER_MAX}. \
         WO-E2 Land 2 leaves 3; WO-E8b reaches 0. Lower the MAX in the land that \
         earns it; never raise it without a written plan amendment."
    );
}

/// Row 4 — occurrences of the product enum's name inside rustdar-egui.
#[test]
fn the_product_enum_never_spreads_further_into_egui() {
    let root = Path::new(ROOT);
    // Needle-definition control: the counted name must still be the live enum.
    let types_rs = root.join("rustdar-radar/src/types.rs");
    anchored_file(&types_rs, PRODUCT_DEF_ANCHOR);

    let files = load_tree(&root.join("rustdar-egui"));
    assert_anchored(&files, "rustdar-egui/src/lib.rs", UI_MOD_ANCHOR);
    let n = count(&files, PRODUCT_ENUM);
    assert!(
        n <= PRODUCT_IN_EGUI_MAX,
        "the product enum spread further into rustdar-egui: {n} occurrences > \
         ceiling {PRODUCT_IN_EGUI_MAX}. WO-E9 (FieldId) drives this to 0. Lower the \
         MAX in the land that earns it; never raise it without a written plan \
         amendment."
    );
}

// Row 5 — retired; the enum it counted is gone.

/// Row 6 — ChannelHub receiver-field count, ceiling 18.
#[test]
fn the_channel_hub_never_grows_past_eighteen_receiver_pairs() {
    let channels_rs = Path::new(ROOT).join("rustdar-app/src/channels.rs");
    let text = anchored_file(&channels_rs, HUB_ANCHOR);
    let n = text.matches(RECEIVER_FIELD).count();
    assert!(
        n <= HUB_RECEIVER_MAX,
        "ChannelHub grew: {n} receiver fields > ceiling {HUB_RECEIVER_MAX}. New \
         channels ride existing rows or stay local to their orchestrator (the \
         WO-E4.9 precedent); the field-list pin lands at WO-E3, verified at \
         WO-M13b. Never raise this without a written plan amendment."
    );
}

/// Row 7 — offload.rs names ZERO source-crate types, in EITHER direction.
#[test]
fn offload_names_zero_source_crate_types() {
    let offload_rs = Path::new(ROOT).join("rustdar-worker/src/offload.rs");
    let text = anchored_file(&offload_rs, OFFLOAD_ANCHOR);

    for (needle, crate_name) in [(OVERLAYS_PATH, "overlays"), (RADAR_PATH, "radar")] {
        let n = text.matches(needle).count();
        assert_eq!(
            n, 0,
            "offload.rs speaks {n} {crate_name}-crate path(s) where the pin is \
             ZERO, in either direction, prose included (WO-M7c closed the \
             reply direction; the request direction closed at WO-M7.2). A job \
             kind's types belong beside its pipeline, reached through the \
             composed registry (`job_registry.rs`) — never named in the \
             funnel. Never raise this without a written plan amendment."
        );
    }

    // Presence control: the needles are alive — the composition module names both
    // crates by construction.
    let registry_rs = Path::new(ROOT).join("rustdar-worker/src/job_registry.rs");
    let registry = read(&registry_rs);
    for (needle, what) in [(OVERLAYS_PATH, "overlays"), (RADAR_PATH, "radar")] {
        assert!(
            registry.matches(needle).count() > 0,
            "the {what} needle no longer matches job_registry.rs, which \
             composes both source-crate registries by name. Either the \
             composition moved (re-point this control) or the needle rotted \
             (fix it) — a dead needle would leave the zero pin above green \
             over anything."
        );
    }
}
