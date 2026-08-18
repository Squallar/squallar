//! WO-E0c — the campaign's architectural ratchets: the enforced starting line.
//!
//! Six metric families are ASSERTED as ceilings from measured at-land
//! baselines; one family (the wasm-cfg line counts, row 3) is RECORDED below
//! and deliberately NOT asserted. Every ceiling may only move DOWN: the phase
//! that earns a lower count lowers the MAX const in the same land; a MAX is
//! never raised without a written plan amendment.
//!
//! Discipline, borrowed from the tree's two proven patterns:
//! - grep-ratchet with presence controls: every ceiling sits beside a
//!   positive check on the same haystack, so a moved or renamed haystack
//!   fails loudly instead of counting zero and going silently green
//!   (`the_opaque_overlay_path_stays_deleted`,
//!   rustdar-frontend/src/app_render/frame_thread_conversion_tests.rs:200).
//! - self-verifying inventory (`every_colour_scale_static_is_registered`,
//!   rustdar-radar/src/palette.rs:946).
//!
//! Needle hygiene: every counted pattern in this file is built from split
//! literals (`concat!("self.", "gui.")` style), and the commands in the
//! table spell their patterns with shell string concatenation — `'Radar'`
//! immediately followed by `'Product'` is one contiguous shell argument.
//! This file therefore never contains a counted pattern contiguously: the
//! walker additionally skips the file itself (belt and braces), the table's
//! commands reproduce the same values after this file lands, and the
//! campaign-close zero-greps need no test-file exclusion.
//!
//! Counting semantics: the asserted metrics count OCCURRENCES — `rg -o … |
//! wc -l` on the command side, summed `str::matches` here (identical for
//! these needles). The wasm-cfg rows count matching LINES per crate (`rg -c`
//! summed). Commands run from the workspace root; the walker skips dirs
//! named `target`/`pkg` and never leaves the workspace (sibling checkouts
//! stay out of scope by construction). Verified at land: ripgrep ignores
//! nothing else under these trees, so the walker's file set is identical to
//! the commands'.
//!
//! # Baseline of record
//!
//! Measured 2026-08-18 at main @ 854f4a64 (the WO-M2/E0a land — E0c runs
//! last in Phase 0 precisely so these counts include M2's tolerance module);
//! every value below is the command's live output at that tree, per the
//! order's re-count-at-land rule. Reference values from 2026-08-18 @
//! 42e90efd drifted exactly where predicted: overlay-kind 745 -> 762 and
//! product-enum 440 -> 444 (M2's tolerance module + tests), web wasm-cfg
//! 30 -> 31 (M1's tier-1 wasm tests).
//!
//! ```text
//!  #   metric                                        value  command (run from the workspace root)
//!  1a  App-pokes-Gui occurrences, rustdar-frontend     204  rg -o 'self\.''gui\.' rustdar-frontend --glob '*.rs' | wc -l
//!  1b  ... excluding test-named paths                  198  rg -o 'self\.''gui\.' rustdar-frontend --glob '*.rs' -g '!*tests*' | wc -l
//!  2   Gui setter fns in rustdar-egui/src/ui.rs         23  rg -o 'pub fn ''set_' rustdar-egui/src/ui.rs | wc -l
//!  3   wasm-cfg lines per crate                          -  for c in rustdar-frontend rustdar-radar rustdar-egui rustdar-web rustdar-overlays; do
//!      [RECORDED, NOT ASSERTED]                             printf '%s ' "$c"; rg -c 'target_arch = "wasm''32"' "$c" --glob '*.rs' \
//!      frontend 165, radar 54, egui 40, web 31,             | awk -F: '{s+=$2} END {print s+0}'; done
//!      overlays 25 (sum 315)
//!  3b  ... frontend/src/constants.rs alone              42  rg -c 'target_arch = "wasm''32"' rustdar-frontend/src/constants.rs
//!      [RECORDED, NOT ASSERTED]
//!  4a  product-enum occurrences in rustdar-egui        444  rg -o 'Radar''Product' rustdar-egui --glob '*.rs' | wc -l
//!  4b  ... files containing it (info)                   29  rg -l 'Radar''Product' rustdar-egui --glob '*.rs' | wc -l
//!  5a  overlay-kind occurrences, whole tree            762  rg -o 'Overlay''Kind' . --glob '*.rs' | wc -l
//!  5b  ... files containing it (info)                   56  rg -l 'Overlay''Kind' . --glob '*.rs' | wc -l
//!  6   ChannelHub receiver fields                       18  rg -o '_receiver: ''Receiver<' rustdar-frontend/src/channels.rs | wc -l
//!  7a  overlays-crate path occurrences in offload.rs    70  rg -o 'rustdar_''overlays::' rustdar-frontend/src/offload.rs | wc -l
//!  7b  radar-crate path occurrences in offload.rs       57  rg -o 'rustdar_''radar::' rustdar-frontend/src/offload.rs | wc -l
//!  7c  ... distinct overlays paths (info)               38  rg -o 'rustdar_''overlays::[A-Za-z0-9_:]+' rustdar-frontend/src/offload.rs | sort -u | wc -l
//!  7d  ... distinct radar paths (info)                  37  rg -o 'rustdar_''radar::[A-Za-z0-9_:]+' rustdar-frontend/src/offload.rs | sort -u | wc -l
//! ```
//!
//! Notes:
//! - Rows 1a/1b are transitional migration scaffolding: WO-E2/WO-E8 drive
//!   the App-pokes-Gui coupling to 0 via GuiEvent, and these rows and their
//!   test are DELETED at campaign close — they track migration progress, not
//!   permanent architecture.
//! - Row 2's trajectory: WO-E2 Land 2 leaves 3 (the chunk-settings setters);
//!   WO-E8b reaches 0.
//! - Row 3 is recorded, not asserted, by user ruling: no count ratchets on
//!   style metrics ("ratchets for things like this seem super sketchy and
//!   inappropriate for the rust ecosystem"). The qualitative rule — a cfg
//!   may select a value, a dependency, or a type alias, never fork behaviour
//!   inside a fn body — lives in ARCHITECTURE.md and review. constants.rs's
//!   42 cfg lines dissolve into a profile module as refactor work,
//!   unratcheted.
//! - Row 4a reaches 0 at WO-E9 (FieldId adoption); the enum itself stays pub
//!   in rustdar-radar through the campaign.
//! - Row 5a's command runs over `.`; measured at land, every occurrence
//!   lives inside the five application crates, so the test's five-crate walk
//!   equals the command. WO-M8c deletes the enum and re-shapes this metric
//!   to enum-absent in the same land.
//! - Row 6 is a COUNT ceiling only. The literal shrink-only field-list pin
//!   LANDS at WO-E3 and is VERIFIED at WO-M13b. WO-E4.9's extract-results
//!   channel is a RenderOrchestrator-local mpsc, not a hub pair — the hub
//!   stays at 18 through the campaign.
//! - Rows 7a/7b track the REQUEST direction of the offload wire until
//!   WO-M7c, which drives them to 0.

use std::fs;
use std::path::{Path, PathBuf};

/// Workspace root: this integration test's manifest dir is `rustdar-frontend/`.
const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/..");

/// The five application crates the campaign's counters walk.
const CRATES: [&str; 5] = [
    "rustdar-frontend",
    "rustdar-radar",
    "rustdar-egui",
    "rustdar-web",
    "rustdar-overlays",
];

// ---------------------------------------------------------------------------
// Needles — split literals so this file never contains what it counts.
// ---------------------------------------------------------------------------

const SELF_GUI: &str = concat!("self.", "gui.");
const PUB_FN_SET: &str = concat!("pub fn ", "set_");
const PRODUCT_ENUM: &str = concat!("Radar", "Product");
const KIND_ENUM: &str = concat!("Overlay", "Kind");
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
const KIND_DEF_ANCHOR: &str = concat!("enum Overlay", "Kind");

// ---------------------------------------------------------------------------
// Ceilings — at-land measurements (see the table above). Lower the MAX in
// the land that earns it; never raise one without a written plan amendment.
// ---------------------------------------------------------------------------

/// Row 1a. Transitional scaffolding: WO-E2/WO-E8 drive it to 0 via GuiEvent;
/// the metric is deleted at campaign close.
const SELF_GUI_MAX: usize = 204;
/// Row 1b — the same needle outside test-named paths.
const SELF_GUI_NON_TEST_MAX: usize = 198;
/// Row 2. WO-E2 Land 2 leaves 3; WO-E8b reaches 0.
const UI_SETTER_MAX: usize = 23;
/// Row 4a. WO-E9 (FieldId adoption) reaches 0.
const PRODUCT_IN_EGUI_MAX: usize = 444;
/// Row 5a. WO-M8 shrinks it; WO-M8c deletes the enum and re-shapes the
/// metric to enum-absent in the same land. Re-baselined 762 -> 766 per the
/// 2026-08-18 KIND_MAX amendment in the campaign log: WO-THEME's
/// plan-mandated regression tests name the enum (+4, minimized); the
/// ceiling ratchets only downward from 766.
const KIND_MAX: usize = 766;
/// Row 6. COUNT ceiling only — the shrink-only field-list pin lands at
/// WO-E3 and is verified at WO-M13b; the hub stays at 18 (WO-E4.9's extract
/// channel is orchestrator-local, not a hub pair).
const HUB_RECEIVER_MAX: usize = 18;
/// Row 7a. Request direction shrinks through WO-M5/M6/M7; WO-M7c reaches 0.
const OFFLOAD_OVERLAYS_PATH_MAX: usize = 70;
/// Row 7b. Same trajectory as row 7a.
const OFFLOAD_RADAR_PATH_MAX: usize = 57;

// ---------------------------------------------------------------------------
// Walker + counters (std-only, pure file reads: the coverage job runs this
// binary, so it stays fast and hermetic).
// ---------------------------------------------------------------------------

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

/// Every `.rs` file under `dir`, recursively, skipping dirs named `target`
/// or `pkg` (build output — the same set ripgrep ignores here, verified at
/// land) and this ratchet file itself.
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

/// The positive half of a walked-haystack check: the anchor file must be in
/// the WALKED set (so a broken walker fails here, not by counting zero) and
/// must still contain its anchor string.
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
// ---------------------------------------------------------------------------

/// Row 1 — the App-pokes-Gui coupling (occurrences of the split needle in
/// rustdar-frontend). WO-E2/WO-E8 drive it to 0 via GuiEvent; transitional
/// scaffolding, deleted at campaign close.
#[test]
fn the_app_pokes_gui_coupling_never_grows() {
    let crate_root = Path::new(ROOT).join("rustdar-frontend");
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

/// Row 2 — setter fns on the Gui shell. WO-E2 Land 2 leaves 3 (the
/// chunk-settings setters); WO-E8b reaches 0. WO-E1 split ui.rs (the struct
/// now lives in gui/state.rs) and re-anchored this presence control, in the
/// same land, on the `impl` block over `Gui` that still hosts every setter.
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
/// WO-E9 (FieldId adoption) drives this to 0; the enum itself stays pub in
/// rustdar-radar through the campaign, so its definition anchors the needle.
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

/// Row 5 — occurrences of the overlay-kind enum's name across the five
/// application crates (measured at land: equal to the whole-tree count).
/// WO-M8 shrinks it; WO-M8c deletes the enum and re-shapes this metric to
/// enum-absent in the same land.
#[test]
fn the_overlay_kind_enum_never_spreads_further() {
    let root = Path::new(ROOT);
    let mut files = Vec::new();
    for c in CRATES {
        files.extend(load_tree(&root.join(c)));
    }
    assert_anchored(
        &files,
        "rustdar-overlays/src/render/overlay_state.rs",
        KIND_DEF_ANCHOR,
    );
    let n = count(&files, KIND_ENUM);
    assert!(
        n <= KIND_MAX,
        "the overlay-kind enum spread further: {n} occurrences > ceiling {KIND_MAX}. \
         WO-M8 shrinks this; WO-M8c deletes the enum and re-shapes this metric in \
         the same land. Lower the MAX in the land that earns it; never raise it \
         without a written plan amendment."
    );
}

/// Row 6 — ChannelHub receiver-field count, ceiling 18. COUNT ceiling only:
/// the literal shrink-only field-list pin LANDS at WO-E3 and is VERIFIED at
/// WO-M13b. WO-E4.9's extract-results channel is a RenderOrchestrator-local
/// mpsc, not a hub pair — the hub stays at 18 through the campaign.
#[test]
fn the_channel_hub_never_grows_past_eighteen_receiver_pairs() {
    let channels_rs = Path::new(ROOT).join("rustdar-frontend/src/channels.rs");
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

/// Row 7 — the offload wire's crate-path vocabulary (request direction).
/// WO-M5/M6/M7 shrink it; WO-M7c reaches 0.
#[test]
fn the_offload_wire_vocabulary_never_grows() {
    let offload_rs = Path::new(ROOT).join("rustdar-frontend/src/offload.rs");
    let text = anchored_file(&offload_rs, OFFLOAD_ANCHOR);

    let overlays_n = text.matches(OVERLAYS_PATH).count();
    let radar_n = text.matches(RADAR_PATH).count();
    assert!(
        overlays_n <= OFFLOAD_OVERLAYS_PATH_MAX,
        "offload.rs speaks more overlays-crate paths: {overlays_n} occurrences > \
         ceiling {OFFLOAD_OVERLAYS_PATH_MAX}. WO-M5/M6/M7 shrink the request \
         direction; WO-M7c reaches 0. Lower the MAX in the land that earns it; \
         never raise it without a written plan amendment."
    );
    assert!(
        radar_n <= OFFLOAD_RADAR_PATH_MAX,
        "offload.rs speaks more radar-crate paths: {radar_n} occurrences > ceiling \
         {OFFLOAD_RADAR_PATH_MAX}. WO-M5/M6/M7 shrink the request direction; \
         WO-M7c reaches 0. Lower the MAX in the land that earns it; never raise it \
         without a written plan amendment."
    );
}
