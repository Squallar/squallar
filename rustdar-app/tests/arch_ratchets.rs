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
//! Baseline RE-MEASURED 2026-08-19 at main @ b85bfa2d, on the tree left by the
//! comment pass `be5b203a`..`ee8bcc7c` (443 files, 99,212 deletions measured
//! `0e45ccb5`..`ee8bcc7c`). Every needle is counted in comments as in code,
//! so a pass that deletes prose lowers these counts without changing one line
//! of behaviour. The ceilings move down with them: the land that earns a lower
//! count takes it, and this land earned two. The `was` column is the
//! 2026-08-18 reading at main @ 854f4a64, kept so the size of the prose's
//! share stays visible.
//!
//! ```text
//!  #   metric                                        value  was  command (run from the workspace root)
//!  1a  App-pokes-Gui occurrences, rustdar-app          185  191  rg -o 'self\.''gui\.' rustdar-app --glob '*.rs' | wc -l
//!  1b  ... excluding test-named paths                  180  186  rg -o 'self\.''gui\.' rustdar-app --glob '*.rs' -g '!*tests*' | wc -l
//!  2   Gui setter fns in rustdar-egui/src/ui.rs          3    3  rg -o 'pub fn ''set_' rustdar-egui/src/ui.rs | wc -l
//!  3   wasm-cfg lines per crate  [NOT ASSERTED]          -    -  rg -c 'target_arch = "wasm''32"' "$c" --glob '*.rs'
//!  4a  product-enum occurrences in rustdar-egui        440  444  rg -o 'Radar''Product' rustdar-egui --glob '*.rs' | wc -l
//!  4b  ... files containing it (info)                   29   29  rg -l 'Radar''Product' rustdar-egui --glob '*.rs' | wc -l
//!  6   ChannelHub receiver fields                       18   18  rg -o '_receiver: ''Receiver<' rustdar-app/src/channels.rs | wc -l
//!  7a  overlays-crate path occurrences in offload.rs     0    0  rg -o 'rustdar_''overlays::' rustdar-worker/src/offload.rs | wc -l
//!  7b  radar-crate path occurrences in offload.rs        0    0  rg -o 'rustdar_''radar::' rustdar-worker/src/offload.rs | wc -l
//!  8   config-swap occurrences, six crates                0    -  rg -o 'load_pane''_configs|save_pane''_configs|loaded_''configs' rustdar-{overlays,egui,app,radar,source,worker} --glob '*.rs' | wc -l
//!  9a  radar-geometry definitions in rustdar-radar        1    -  rg -o 'struct Loop''Geometry' rustdar-radar --glob '*.rs' | wc -l
//!  9b  ... in rustdar-egui                                 0    1  rg -o 'struct Loop''Geometry' rustdar-egui --glob '*.rs' | wc -l
//! 10a loop-frame-arm occurrences, rustdar-app             8   17  rg -o 'Loop''FrameData|L3Frame''Key|Cached''Volume' rustdar-app --glob '*.rs' | wc -l
//! 10b ... excluding test-named paths                      2    6  rg -o 'Loop''FrameData|L3Frame''Key|Cached''Volume' rustdar-app --glob '*.rs' -g '!*tests*' | wc -l
//! ```
//!
//! Rows 1b, 2, 6, 7a and 7b were already pinned at their measured values and do
//! not move: the comment pass left no prose mention of those needles to delete.
//! Row 4a reads 440 rather than the pass's 438 because WO-E6a's two accessor
//! signatures land in this crate — see the note on the const itself.
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
/// Row 2's widened half: an inherent `impl` block on the `Gui` type, in either
/// spelling this crate uses — `impl Gui {` inside the `ui` module and
/// `impl super::Gui {` in the files hung off it. A trait impl
/// (`impl Default for Gui {`) is deliberately not one: it can carry no
/// inherent setter, so counting it would inflate the ceiling with rows the
/// property is not about.
const IMPL_KW: &str = concat!("impl", " ");
const GUI_IMPL_TAIL: &str = concat!("Gui", " {");
const TRAIT_IMPL_INFIX: &str = concat!(" ", "for", " ");
/// Row 2's replacement: the one generic write door the deleted setters became.
const GENERIC_CONTROL_DOOR: &str = concat!("pub fn apply_layer", "_control");
const UI_MOD_ANCHOR: &str = concat!("mod ", "ui;");
const HUB_ANCHOR: &str = concat!("struct ", "ChannelHub");
const OFFLOAD_ANCHOR: &str = concat!("pub fn ", "offload_job(");
const PRODUCT_DEF_ANCHOR: &str = concat!("enum Radar", "Product");

// Row 8 — the config swap, deleted at WO-M10c. Split so this file never holds
// a needle contiguously, and so a future grep for the deleted names stays
// clean here too.
const SWAP_LOAD: &str = concat!("load_pane", "_configs");
const SWAP_SAVE: &str = concat!("save_pane", "_configs");
const SWAP_MEMO: &str = concat!("loaded_", "configs");
/// The presence control for row 8: the pane-state hook that REPLACED the swap.
/// If the walk stops seeing this, it is reading the wrong tree and the three
/// zeroes below mean nothing.
const SWAP_REPLACEMENT: &str = concat!("serialize_pane", "_state");

/// Row 9 — the radar geometry type's DEFINITION, wherever it lives. Split so
/// neither half can be satisfied by a haystack the walk never reached: the
/// definition must be found in `rustdar-radar` and must not be found in
/// `rustdar-egui`.
const GEOMETRY_DEF: &str = concat!("struct Loop", "Geometry");

/// Row 10 — the closed arms a loop frame's data comes in, and the two aliases
/// that key their caches. Split so this file never holds one contiguously.
const LOOP_FRAME_ARMS: [&str; 3] = [
    concat!("Loop", "FrameData"),
    concat!("L3Frame", "Key"),
    concat!("Cached", "Volume"),
];
/// The presence control for row 10: the manager whose caches those three name.
/// If the walk stops seeing this in `rustdar-radar`, the needles have rotted
/// and both counts below mean nothing.
const LOOP_MANAGER_DEF: &str = concat!("struct LoopDownload", "Manager");

// --------------------------------------------------------------------------- Ceilings
// — at-land measurements (see the table above).

/// Row 1a.
const SELF_GUI_MAX: usize = 185;
/// Row 1b — the same needle outside test-named paths.
const SELF_GUI_NON_TEST_MAX: usize = 180;
/// Row 2a — **`ui.rs`'s own `impl Gui` block, and only that file**.
///
/// **0 since WO-E8b**, which is where the plan said it would land. The last
/// three were `set_live_chunks`, `set_chunk_notifications` and
/// `set_notifier_endpoint`; their fields are now `RadarSource`'s and the one
/// write door is `Gui::apply_layer_control(kind, update)`, which names a
/// layer and a control update rather than a field.
///
/// **This ceiling is only worth 0 if the needle was not moved instead of the
/// coupling.** The setters were not renamed or relocated out of the walk: the
/// shell's own call — `rustdar-app`'s `app.gui.set_live_chunks(false)` — is
/// gone rather than re-spelled, and what replaced it is generic.
///
/// **What this figure is NOT** is the whole `Gui`. It counts one file, and
/// until WO-E8d it claimed to count "the `Gui` impl, wherever spelled" — a
/// claim [`GUI_IMPL_SETTER_MAX`] measured and found false. The doc now says
/// what the walk does; the wider claim is the constant below, which makes it
/// and pays for it.
const UI_SETTER_MAX: usize = 0;
/// Row 2b — **every inherent `impl Gui` block in `rustdar-egui`, wherever
/// spelled**: the claim [`UI_SETTER_MAX`]'s doc used to make about itself.
///
/// **1, measured, not chosen.** `Gui::set_initial_site` lives on the
/// `impl super::Gui` block in `ui_config.rs` and is called from
/// `rustdar-app/src/app.rs` on a first run with no stored config — it points
/// every pane at the site nearest the host's timezone. It was never inside
/// the walked file: it is absent from `ui.rs` at WO-E8a's SHA and at WO-E8c's,
/// so nothing was relocated to reach a zero, and this is not needle-hiding.
/// **The gate was over-claiming; the code was not lying.**
///
/// **This is the floor, not a raise.** WO-E8b's 0 is untouched above, over
/// the same file it always covered; this row is a *second, wider* measurement
/// whose denominator is 22 impl blocks across 21 files rather than one. The
/// two figures are not the same figure and must never be compared as one.
///
/// The remainder is named because a ceiling may under-claim and may never
/// over-claim: driving this to 0 means the first-run site reaching the panes
/// without a `Gui` setter, which is a boot-path question this order had no
/// mandate to open. Lower it in the land that earns it; never raise it
/// without a written plan amendment.
const GUI_IMPL_SETTER_MAX: usize = 1;
/// Row 4a.
///
/// **440, not 438: WO-E6a's accessors add exactly two occurrences** — the
/// return type of `PaneState::selected_product` and the parameter of
/// `PaneState::set_selected_product`. There is no spelling of an accessor for
/// a `RadarProduct`-typed field that does not name the type. The amendment
/// ratified +2 against the pre-pass 444, i.e. 446; the comment pass then took
/// the row down to 438, so the amendment lands at 440 and spends 6 less than
/// it was granted. Anything beyond +2 from WO-E6a is not this amendment.
///
/// **439 since WO-E7e**: its two new pins name the enum twice, and hoisting the
/// repeated product bindings in `target_matching_tolerates_elevation_jitter_only`
/// gave back three — lowered in the land that earned it, never raised.
const PRODUCT_IN_EGUI_MAX: usize = 439;
/// Row 6.
///
/// **17 since WO-M12b**: the loop scan-list channel is gone — a radar frame
/// listing arrives on the one source path now — so the hub holds one pair
/// fewer. Lowered in the land that earned it, never raised.
const HUB_RECEIVER_MAX: usize = 17;

/// Row 10a — **8 since WO-M12d, down from 17, and this ceiling records a real
/// remainder rather than pretending it is gone.**
///
/// All eight are test-side reads of radar's own frame payload; the remainder is
/// what could not be shed without moving an assertion value, which ruling
/// (23)(b) forbids: three pin suites match the arms directly
/// (`loop_dispatch_tests` twice, `loop_level3_tests`, `loop_scan_cache_tests`)
/// and two build a cached volume through its alias. Re-pointing those to an
/// accessor would change what they assert, not how it is spelled.
///
/// **What the number does NOT include, because it is 0**: any production
/// occurrence — see [`LOOP_FRAME_ARMS_NON_TEST_MAX`]. The remaining reachable
/// step is the decoded-volume cache move (`frames_resident`/`retain_frames`),
/// blocked with M12c's Level III collapse; ruling (25) named both as parts of
/// radar's bespoke half.
const LOOP_FRAME_ARMS_MAX: usize = 8;
/// Row 10b — the same needles outside test-named paths.
///
/// **2, and both are `#[cfg(test)]` items sitting in a production FILE**: the
/// `use` at the top of `app_render.rs` and the return type of the `frame_data`
/// wrapper below it, kept there so the three suites that reach them through
/// `use super::*` need no edit at all. **Production loop dispatch names neither
/// arm**: it asks the layer for the described render job a frame's data makes
/// (`LoopDownloadManager::frame_render_job`) and hands it to the funnel with
/// its input type erased, which is what WO-M12d's ratchet was for.
///
/// This may only reach 0 by those two items moving to a test-named module —
/// worth doing in a land that has a reason to open the file, and NOT worth a
/// glob re-export whose only effect is to hide a needle from this walk.
const LOOP_FRAME_ARMS_NON_TEST_MAX: usize = 2;

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

/// The header line of an inherent `impl` block on `Gui` — see [`IMPL_KW`].
fn is_gui_impl_header(line: &str) -> bool {
    let line = line.trim_end();
    line.starts_with(IMPL_KW) && line.ends_with(GUI_IMPL_TAIL) && !line.contains(TRAIT_IMPL_INFIX)
}

/// Every inherent `impl Gui` block in the walked tree, as (file, body) pairs.
///
/// **Why lines and not a parser.** These blocks are top-level items in a
/// `rustfmt`-shaped tree, so each opens at column 0 and closes at the next
/// `}` at column 0. That assumption is not taken on trust: the caller asserts
/// no extracted body contains another header, which is exactly what an extent
/// that overran its own closing brace would produce.
fn gui_impl_blocks(files: &[(PathBuf, String)]) -> Vec<(PathBuf, String)> {
    let mut blocks = Vec::new();
    for (path, text) in files {
        let lines: Vec<&str> = text.lines().collect();
        for (start, line) in lines.iter().enumerate() {
            if !is_gui_impl_header(line) {
                continue;
            }
            let body = &lines[start + 1..];
            let end = body
                .iter()
                .position(|l| *l == "}")
                .map_or(lines.len(), |offset| start + 1 + offset);
            blocks.push((path.clone(), lines[start + 1..end].join("\n")));
        }
    }
    blocks
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

/// Row 8 — **the config swap stays deleted**, everywhere.
///
/// The swap installed one pane's saved state into the shared handler before
/// each call and took it out again. It was a CORRECTNESS mechanism, not
/// plumbing: any method it covered that stayed global answered for one pane
/// and acted for every pane the moment it died. WO-M10c moved all twelve
/// handlers' per-pane state into the pane, so the mechanism has nothing left
/// to do — and a single call site coming back is a handler reading a global
/// again.
///
/// The zero is checked against a **positive control on the same walk**: the
/// hook that replaced the swap must be found, and found many times, or the
/// three absence checks are reading an empty or wrong haystack and pass for
/// the wrong reason.
#[test]
fn the_config_swap_stays_deleted() {
    let crates = [
        "rustdar-overlays",
        "rustdar-egui",
        "rustdar-app",
        "rustdar-radar",
        "rustdar-source",
        "rustdar-worker",
    ];
    let mut swap = 0usize;
    let mut replacement = 0usize;
    let mut walked = 0usize;
    for name in crates {
        let files = load_tree(&Path::new(ROOT).join(name));
        walked += files.len();
        swap += count(&files, SWAP_LOAD) + count(&files, SWAP_SAVE) + count(&files, SWAP_MEMO);
        replacement += count(&files, SWAP_REPLACEMENT);
    }
    // Two controls, because a zero is only as good as the haystack under it.
    assert!(
        walked > 200,
        "presence control: only {walked} .rs files were walked across {} \
         crates — the walk is not reaching the tree, so the zero below would \
         be a zero about nothing",
        crates.len(),
    );
    assert!(
        replacement >= 12,
        "presence control: the walk found {SWAP_REPLACEMENT:?} only \
         {replacement} times. Every handler that keeps per-pane state defines \
         it, so a count this low means the walk is reading the wrong files \
         and the absence check below proves nothing.",
    );
    assert_eq!(
        swap, 0,
        "the config swap is back ({swap} occurrence(s) of {SWAP_LOAD:?}, \
         {SWAP_SAVE:?} or {SWAP_MEMO:?}). It installed one pane's state into \
         the shared handler before each call, which is how two panes came to \
         share one answer; a handler's per-pane state belongs in the pane, \
         reached through `PaneRef`/`PaneMut`. See WO-M10b/WO-M10c.",
    );
}

/// Row 2 — setter fns on the Gui shell.
#[test]
fn the_gui_setter_surface_never_grows() {
    let ui_rs = Path::new(ROOT).join("rustdar-egui/src/ui.rs");
    let text = anchored_file(&ui_rs, GUI_IMPL_ANCHOR);
    // **Neither half passes on an empty haystack.** `anchored_file` proves
    // the walked file is still the Gui's; this proves the door the message
    // sends the reader to is real. A zero here has to mean "the setters are
    // gone and the generic write exists", never "the walk read a file that
    // was not there".
    let glue = read(&Path::new(ROOT).join("rustdar-egui/src/gui/layer_glue.rs"));
    assert!(
        glue.contains(GENERIC_CONTROL_DOOR),
        "presence control: {GENERIC_CONTROL_DOOR:?} is gone from the layer          glue, so a setter-free `Gui` would mean the write door was deleted          rather than replaced.",
    );
    let n = text.matches(PUB_FN_SET).count();
    // `assert_eq!` rather than `<=`, because the ceiling is 0: a `<=` against
    // the minimum of the type is a comparison that cannot fail, which is the
    // vacuity this suite exists to refuse.
    assert_eq!(
        n, UI_SETTER_MAX,
        "the Gui setter surface in `ui.rs` is {n}, not {UI_SETTER_MAX}. WO-E2 \
         Land 2 left 3; WO-E8b reached 0, and 0 is where it stays. A switch a \
         layer owns is written through `Gui::apply_layer_control`, not through \
         a setter beside it. Never raise it without a written plan amendment."
    );

    // ----------------------------------------------------------------- 2b:
    // the same needle over EVERY `impl Gui` block, which is the claim the
    // constant above used to make about itself and could not keep. A setter
    // moved from `ui.rs` into any sibling file passes 2a and fails here.
    let files = load_tree(&Path::new(ROOT).join("rustdar-egui"));
    assert_anchored(&files, "rustdar-egui/src/ui.rs", GUI_IMPL_ANCHOR);
    let blocks = gui_impl_blocks(&files);

    // Extent control: a body that swallowed its own closing brace would run
    // on into the next block's header. None may.
    for (path, body) in &blocks {
        assert!(
            !body.lines().any(is_gui_impl_header),
            "extent control: an `impl Gui` body in {} ran past its closing \
             brace and swallowed the next block's header, so this walk's \
             count covers text it does not mean to. Re-anchor it in the land \
             that reshaped the file.",
            path.display(),
        );
    }

    // Widening control: the whole point of 2b is that it reads more than the
    // one file 2a reads. A walk that collapsed back onto `ui.rs` would count
    // 0 and read green while proving nothing 2a had not already proved.
    let walked: std::collections::BTreeSet<&PathBuf> = blocks.iter().map(|(p, _)| p).collect();
    assert!(
        walked.len() > 1,
        "widening control: the `impl Gui` walk found blocks in {} file(s). It \
         exists to cover every file that carries one; a single-file answer is \
         the narrow walk this row was added to replace.",
        walked.len(),
    );

    // Presence control: the extractor must really yield `pub fn` bodies, and
    // from a file that is NOT `ui.rs`. The generic write door is one, and it
    // lives in the layer glue. Without this, a `gui_impl_blocks` that
    // returned empty bodies would count 0 and pass.
    let bodies: String = blocks
        .iter()
        .map(|(_, body)| body.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        bodies.contains(GENERIC_CONTROL_DOOR),
        "presence control: {GENERIC_CONTROL_DOOR:?} is not inside any walked \
         `impl Gui` body, so this walk is reading empty or wrong extents and \
         its zero would mean nothing.",
    );

    let wide = bodies.matches(PUB_FN_SET).count();
    // `assert_eq!` again, and for a second reason beyond vacuity: this row's
    // value is a measured floor with a named remainder, so a DROP is news the
    // constant must be lowered to record, not silently absorbed.
    assert_eq!(
        wide, GUI_IMPL_SETTER_MAX,
        "the Gui setter surface across every `impl Gui` block is {wide}, not \
         {GUI_IMPL_SETTER_MAX}. The named remainder is `set_initial_site` in \
         `ui_config.rs`; anything above that is a new setter on the shell, \
         which is what WO-E8b removed the last of. If the count FELL, lower \
         the constant in this land and strike the remainder from its doc."
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

/// Row 9 — **a radar type is not defined in the presentation crate.**
///
/// `LoopGeometry` is the site code and the coordinates a radar loop's frames
/// are projected about. It was parked in `rustdar-egui/src/radar_layer.rs`
/// because WO-E7a's fence forbade touching `rustdar-radar`; WO-M12e moved it
/// to `rustdar_radar::loop_geometry`, where the type's own crate owns it and
/// the presentation crate only reads it back out of a timeline's anchor.
///
/// The zero is checked against the definition being found in radar on the
/// SAME needle — a definition that moved back, or a needle that rotted, fails
/// one half or the other rather than passing both for the wrong reason.
#[test]
fn the_radar_geometry_type_is_defined_in_radar_and_not_in_egui() {
    let radar = load_tree(&Path::new(ROOT).join("rustdar-radar"));
    let egui = load_tree(&Path::new(ROOT).join("rustdar-egui"));
    assert!(
        radar.len() > 20 && egui.len() > 20,
        "presence control: the walk reached {} radar and {} egui .rs files, \
         which is too few to be the real trees — both counts below would be \
         about nothing",
        radar.len(),
        egui.len(),
    );
    assert_eq!(
        count(&radar, GEOMETRY_DEF),
        1,
        "the radar geometry type is not defined in rustdar-radar. Either it \
         moved back out of its own crate, or the needle {GEOMETRY_DEF:?} \
         rotted — a dead needle would leave the zero below green over \
         anything. See WO-M12e.",
    );
    assert_eq!(
        count(&egui, GEOMETRY_DEF),
        0,
        "a radar type is defined in the presentation crate again \
         ({GEOMETRY_DEF:?}). Radar's own types belong in rustdar-radar; \
         rustdar-egui reads this one back out of `LayerTimeState::anchor` and \
         never declares it. See WO-M12e, and ruling (15) as amended by (23).",
    );
}

/// Row 10 — the closed arms a loop frame's data comes in stay radar's.
///
/// Split so neither half can pass on a haystack the walk never reached: the
/// needles must be ALIVE in `rustdar-radar` (they are radar's own vocabulary
/// and are not going away), and the counts in `rustdar-app` are what may only
/// fall. See WO-M12d and ruling (25).
#[test]
fn the_loop_frame_arms_stay_radars_own_vocabulary() {
    let crate_root = Path::new(ROOT).join("rustdar-app");
    let app = load_tree(&crate_root);
    let radar = load_tree(&Path::new(ROOT).join("rustdar-radar"));
    assert_anchored(&app, "src/app_render.rs", concat!("fn frame_", "sweep("));
    assert_eq!(
        count(&radar, LOOP_MANAGER_DEF),
        1,
        "presence control: the loop download manager is not defined in \
         rustdar-radar. Either it moved, or the needle {LOOP_MANAGER_DEF:?} \
         rotted — and a rotted needle would leave both counts below green over \
         anything.",
    );
    for needle in LOOP_FRAME_ARMS {
        assert!(
            count(&radar, needle) > 0,
            "presence control: {needle:?} is not named anywhere in \
             rustdar-radar, so counting it in rustdar-app is counting a dead \
             needle. Re-anchor this ratchet in the land that renamed it.",
        );
    }

    let total: usize = LOOP_FRAME_ARMS.iter().map(|n| count(&app, n)).sum();
    let non_test: usize = app
        .iter()
        .filter(|(p, _)| !in_test_path(p, &crate_root))
        .map(|(_, t)| {
            LOOP_FRAME_ARMS
                .iter()
                .map(|n| t.matches(*n).count())
                .sum::<usize>()
        })
        .sum();

    assert!(
        non_test <= LOOP_FRAME_ARMS_NON_TEST_MAX,
        "a production file in rustdar-app names a loop frame's closed arms \
         again: {non_test} occurrences > ceiling \
         {LOOP_FRAME_ARMS_NON_TEST_MAX}. The dispatch path asks the layer for \
         the described job a frame's data makes and never holds the arms — see \
         WO-M12d.",
    );
    assert!(
        total <= LOOP_FRAME_ARMS_MAX,
        "the loop-frame vocabulary shared with rustdar-app grew: {total} \
         occurrences > ceiling {LOOP_FRAME_ARMS_MAX}. Lower this in the land \
         that sheds one; never raise it.",
    );
}
