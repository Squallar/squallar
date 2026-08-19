//! Every test a comment names is a test that exists, enforced by scanning the
//! workspace.
//!
//! The convention everywhere in this tree is that a doc comment says which
//! test pins the behaviour it describes. That convention is load-bearing: it is
//! how a reader tells a guarantee from an intention. It is also unenforced, and
//! two hand sweeps across `rustdar-radar` and `rustdar-frontend` found four
//! citations pointing at nothing —
//!
//! * `sampler.rs` cited a refusal test whose replacement asserts the opposite,
//! * `eet.rs` cited a per-site datum check whose successor is strictly weaker,
//! * `sampler.rs` named one seam rule twice, including inside the comment
//!   explaining what that rule pins,
//! * `derive/tests.rs` closed a loop by citing a resampling test that has never
//!   existed under that name, so the loop was never closed.
//!
//! Every one of those would have failed this scan the day the test was renamed
//! or deleted. A citation that resolves to nothing silently converts a
//! guarantee into a claim, and nothing else in the build notices.
//!
//! # What it checks, and what it cannot
//!
//! Two things, and neither is that the sentence is true.
//!
//! **One**: a cited name **resolves** to a `fn` or `mod` that exists. That is a
//! weak property and it is worth being blunt about the gap: two of the four
//! findings above had a *resolvable* predecessor that asserted the opposite of
//! what the prose claimed, and a scan of this shape would have passed both
//! right up until the rename. What it catches is renames and deletions — the
//! two ways a true citation rots without anyone touching the prose. It does not
//! and cannot check that the test asserts what the sentence says it asserts.
//! Do not read a green run here as "the docs are true".
//!
//! **Two**: a citation to an `#[ignore]`d test **says that it is ignored** —
//! see [`a_citation_to_an_ignored_test_says_that_it_is_ignored`]. Resolution
//! alone leaves one door open, and it is the same door: "this is pinned by
//! `foo`" passes in full while `foo` never runs in the default `cargo test`
//! row, so the prose promises a guard the build does not provide. 79 tests here
//! carry the attribute and nearly all of them earn it — a real adapter, the
//! network, an archived volume — so the defect is never the `#[ignore]`, only
//! the sentence that reads as though CI were watching. 23 citations pointed at
//! one when this was written and 18 of them said nothing about it.
//!
//! # What counts as a citation
//!
//! A backticked span inside a `//`, `///` or `//!` comment whose final `::`
//! segment is snake_case with at least [`MIN_SEGMENTS`] underscore-separated
//! segments. The threshold is what separates a test name from an ordinary
//! identifier, and it was measured rather than chosen — by this scan, on this
//! tree, with [`ALLOWED`] as it stands:
//!
//! ```text
//! segments  candidates  unallowed  allowances that stop matching
//!   >= 4         1141        127    0
//!   >= 5          490          5    0
//!   >= 6          370          5   10
//! ```
//!
//! Four segments is where the hand sweeps drew the line, and the *idea* is
//! right — test names in this tree are sentences, ordinary identifiers are
//! compound nouns. On two crates it held. On the whole workspace it drags in
//! the four-segment upstream API surface this codebase talks about constantly:
//! `max_texture_dimension_2d` alone appears 15 times, and
//! `animate_bool_with_time`, `get_mapped_range_mut`, `copy_buffer_to_texture`
//! and `next_auto_id_salt` behave the same way. Those are real names owned by
//! wgpu and egui, they are not in this tree and never will be, and each would
//! need its own permanent allowance — 122 of them, against the 5 real findings
//! in the same column. An allowance list that size is not a list anyone reads.
//!
//! Six segments looks equally good in the middle column and is not: **ten of
//! the allowances stop matching anything**, because the citations they excuse
//! drop out of the candidate set entirely. Those nine are still cited, still
//! unresolvable, and simply no longer looked at — the scan would be quieter
//! while checking less. That column is the reason the threshold is not simply
//! "as high as stays green".
//!
//! # The false-positive rate, stated honestly
//!
//! Of 470 candidates at five segments, **41 did not resolve** when this scan
//! was first run across the workspace. Of those, **17 were real defects** (16
//! sites; one name was cited twice) and **24 were not** — upstream API names,
//! ORPG spec identifiers, instruments that live on `campaign-harness` by
//! policy, and prose narrating a deletion.
//!
//! So: 5.1 % of all candidates were false positives, but **59 % of the raw
//! flags were**. The second number is the one that matters to whoever runs
//! this first, and it is why [`ALLOWED`] is not a wart — it is the one-time
//! cost of a scan that has no way to read a sentence. Once each standing
//! category is named, a *new* flag is overwhelmingly likely to be a real
//! rename or deletion, because the four things that legitimately dangle have
//! already been written down.
//!
//! # Shape
//!
//! Deliberately `geodesy_one_definition.rs`'s, which is this tree's existing
//! source scan: walk the workspace, judge each hit, and let an allowance
//! through only with the reason it is not a defect. Its lesson about
//! `.claude/` is inherited too, and generalised — see [`descends_into`].
#![cfg(not(target_arch = "wasm32"))]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// How many underscore-separated segments a backticked name needs before this
/// scan treats it as a citation rather than as an ordinary identifier.
///
/// See the module docs for the measurement behind the number. Raising it hides
/// real citations; lowering it turns the allowance list into a catalogue of
/// wgpu and egui method names.
const MIN_SEGMENTS: usize = 5;

/// Every citation in the workspace that resolves to nothing on purpose, with
/// the reason it is not a defect.
///
/// Matched as `(path suffix, cited name, reason)`. Naming the file as well as
/// the name keeps an allowance to the one site that earned it: the same name
/// going stale somewhere else is still a finding.
///
/// There is no wildcard, and no entry here is a licence to leave a broken
/// citation alone — the four the hand sweeps found were fixed, not allowed.
const ALLOWED: &[(&str, &str, &str)] = &[
    // ── Instruments that live on `campaign-harness` by policy ───────────
    //
    // These are not missing. Campaign harnesses never ship on `main`, so the
    // module doc that reports what one measured cites a name this tree
    // deliberately does not contain. Deleting the citation would delete the
    // provenance of a measured figure, which is worse than the dangling name.
    (
        "rustdar-radar/src/srm.rs",
        "live_lowest_tilt_across_volumes",
        "A live-validation harness on branch `campaign-harness`; the doc cites \
         it as the source of the forty-volume survey printed beside it. \
         Re-measuring means that branch.",
    ),
    (
        "rustdar-radar/src/srm.rs",
        "live_storm_motion_volume_pairing_rate",
        "As above — the harness that fetched the pairing-rate figures this \
         module doc quotes.",
    ),
    (
        "rustdar-radar/src/vil.rs",
        "live_derived_vild_on_the_2022_05_04_outbreak",
        "The `campaign-harness` VILD validation run, named so the outbreak \
         figures in this doc can be reproduced.",
    ),
    (
        "rustdar-radar/src/vil.rs",
        "live_hca_precip_site_scan",
        "The `campaign-harness` site scan that chose the >= 35 dBZ lowest-cut \
         threshold quoted here.",
    ),
    (
        "rustdar-radar/src/hca.rs",
        "live_hca_precip_site_scan",
        "The same harness, cited from the module whose thresholds it set.",
    ),
    // ── Names owned by a dependency or an OS ────────────────────────────
    //
    // Sentence-shaped by accident of the upstream API. They resolve for a
    // reader and cannot resolve for this scan, which only reads this tree.
    (
        "rustdar-volumetric/src/lib.rs",
        "max_uniform_buffer_binding_size",
        "`wgpu::Limits`' own field. The doc is explaining which device limit \
         the uniform block is sized against.",
    ),
    (
        "rustdar-volumetric/src/lib.rs",
        "max_sampled_textures_per_shader_stage",
        "`wgpu::Limits`' own field, named where the probe's scan explains \
         which limit each binding class is priced against. Its sibling \
         `max_uniform_buffer_binding_size` is allowed just above for the same \
         reason; the scan that counts bindings out of the shader had to name \
         all three.",
    ),
    (
        "rustdar-volumetric/src/lib.rs",
        "max_samplers_per_shader_stage",
        "As above. This is the limit whose constant was undercounted at 2 \
         against the raymarch's three samplers, so the paragraph that fixed it \
         necessarily names it.",
    ),
    (
        "rustdar-frontend/src/app.rs",
        "new_without_display_handle_from_env",
        "`egui_winit`'s constructor, named where this crate explains which of \
         the two it calls and why.",
    ),
    (
        "rustdar-frontend/src/app_render.rs",
        "get_texture_latest_submission_index",
        "wgpu-core's own lifetime bookkeeping, walked through in the comment \
         that argues why dropping a replaced texture immediately is safe. \
         Naming the upstream function is the whole point of that paragraph.",
    ),
    (
        "rustdar-egui/src/input_harness.rs",
        "warn_if_rect_changes_id",
        "`egui::context`'s own debug assertion, cited to explain what the \
         harness has to keep quiet.",
    ),
    (
        "rustdar-gpu/src/egui_renderer/texture_upload.rs",
        "update_egui_texture_from_wgpu_texture_with_sampler_options",
        "`egui_wgpu::Renderer`'s own method, and the pivot the whole module \
         turns on: it is the only public way to bind a texture this crate owns \
         to a `TextureId` egui minted, and every constraint that follows — the \
         1x1 stand-in, sticky ownership, the restated sampler — is a \
         consequence of what it will and will not do. The paragraphs that \
         explain those cannot avoid naming it.",
    ),
    (
        "rustdar-gpu/src/egui_renderer/texture_upload/tests.rs",
        "update_egui_texture_from_wgpu_texture_with_sampler_options",
        "The same upstream method, named from the test that exists because it \
         takes a `wgpu::SamplerDescriptor` rather than egui's `TextureOptions` \
         — which is why the mapping between them is restated code, and why the \
         test checks it.",
    ),
    (
        "rustdar-frontend/src/platform.rs",
        "attach_current_thread_for_scope",
        "The `jni` crate's scoped attachment call — the one this module must \
         use instead of the permanent form.",
    ),
    (
        "rustdar-platform/src/os_location/linux.rs",
        "xdp_app_info_is_host",
        "An xdg-desktop-portal D-Bus predicate. Names the portal-side check \
         this code depends on for the un-sandboxed case.",
    ),
    (
        "rustdar-radar/src/hail.rs",
        "warn_thr_sel_mod_coef",
        "An ORPG adaptable-parameter name from the hail algorithm spec, \
         lowercase in the source document. A spec identifier, not a test.",
    ),
    (
        "rustdar-radar/src/hail.rs",
        "warn_thr_sel_mod_off",
        "The coefficient's sibling in the same `hail.alg` stanza, and the one \
         parameter this module does NOT take from the paper: its `value =` is \
         empty and its per-site `default =` table is what every RPG resolves. \
         The paragraphs establishing that the offset is site-adaptable cannot \
         avoid naming it. A spec identifier, not a test.",
    ),
    (
        "rustdar-radar/src/hca.rs",
        "rpg_b21_0r1_7_pub_src",
        "The filename of the ORPG build-21 source drop the HCA twin is \
         checked against. A document, not a test.",
    ),
    // ── Prose that narrates a deletion ──────────────────────────────────
    //
    // The one category this scan genuinely cannot judge, because "`X` pins
    // this" and "`X` used to pin this, and here is why it no longer has to"
    // are the same shape to a scanner and opposite claims to a reader. Every
    // entry below is the second kind: the dead name is the *subject* of a
    // sentence about its own removal, and deleting it would delete what the
    // paragraph is about. Each was read before it was allowed.
    //
    // This is also the only category here that grows over time. If it ever
    // gets long enough to hide something, the fix is a convention rather than
    // a longer list — backticks reserved for names that resolve, a deleted
    // test named in plain prose — and that is a house-style decision, not one
    // this file should make on its own.
    (
        "rustdar-radar/src/voxel.rs",
        "no_data_blends_at_ramp_bottom",
        "The doc says this table is **obsolete** and explains what replaced \
         it. The name is the subject of a sentence about its own removal, not \
         a claim that it still pins anything — deleting the name would delete \
         what the paragraph is about.",
    ),
    (
        "rustdar-radar/src/voxel/tests.rs",
        "no_data_blends_at_ramp_bottom",
        "The same census, named from the test that replaced it. The sentence \
         is \"That table is gone\".",
    ),
    (
        "rustdar-radar/src/voxel/tests.rs",
        "only_the_bottom_transparent_sequential_ramps_may_blend_into_no_data",
        "\"This replaces …\" — the replacement's doc naming what it replaced, \
         and why the per-product decision it pinned no longer exists.",
    ),
    (
        "rustdar-radar/src/sites.rs",
        "only_a_volume_can_answer_the_base_datum",
        "\"This replaces …\", and what it replaced asserted the *opposite* of \
         the truth: that a published station record leaves the base datum \
         unknown, when the record's one elevation **is** the base. The \
         successor cannot explain why it exists without naming it.",
    ),
    (
        "rustdar-volumetric/src/volume_bridge/tests.rs",
        "no_data_blends_at_ramp_bottom",
        "A sketch of the shader code that used to name it, in the comment \
         above the assertion that the shader no longer does. The live check is \
         the `body.contains` a few lines below, which is code and so is not \
         read by this scan.",
    ),
    (
        "rustdar-volumetric/src/volume_bridge/tests.rs",
        "only_reflectivity_clears_the_fade_bar",
        "The successor's doc calls itself \"the deliberate flip of the \
         original\" and quotes that original's own words back. The name is the \
         subject, and the flip is the point of the paragraph.",
    ),
    (
        "rustdar-radar/src/sites.rs",
        "every_site_records_an_elevation",
        "\"The successor to …, which walked the compiled-in table.\" The \
         paragraph exists to say what the predecessor covered and why the \
         successor is wider.",
    ),
    (
        "rustdar-frontend/tests/floor_alignment.rs",
        "the_sites_pixel_lands_in_the_middle_of_a_site_centred_floor",
        "Names the test in the **deleted** `volume_floor/tests.rs` that this \
         one was re-pinned from, so a reader can see which half of the old \
         contract survived the floor texture's removal.",
    ),
    (
        "rustdar-frontend/tests/floor_alignment.rs",
        "a_tile_pixel_and_a_radar_gate_at_the_same_ground_land_on_the_same_texel",
        "As above, for the gate/pixel coincidence pin. The old contract's \
         tile route went with the compositor; the paragraph says so and keeps \
         the name that identifies it.",
    ),
    (
        "rustdar-location/src/hint.rs",
        "anchors_sit_within_plausible_radar_range",
        "The hermetic remnant's doc, saying what is *left* of the deleted \
         network test that can be checked without a network. Naming the \
         predecessor is how a reader knows what this half does not cover.",
    ),
    (
        "rustdar-egui/src/ui_map/volume_arm_tests.rs",
        "a_scroll_that_stops_stops_re_cutting_the_box",
        "\"used to stand here.\" The note explains why the settle it pinned \
         cannot arise now — there is no derivation and no quantum — and names \
         the stronger property's test in the same sentence.",
    ),
    (
        "rustdar-device-profile/src/constants/tests.rs",
        "an_axis_outside_the_guarantee_is_refused",
        "\"used to assert with a literal 257\", in the doc explaining why the \
         device guarantee moved to the crate that meets a device. The \
         paragraph says neither test was dropped and where each went.",
    ),
    (
        "rustdar-egui/src/input_harness/tests.rs",
        "the_phone_hover_readout_never_paints_over_the_arm_toggles",
        "\"retired (synthesis-m9)\": the hover readout it guarded was removed, \
         so the overlap cannot recur. The note names what covers the bar's \
         other strings instead.",
    ),
    (
        "rustdar-frontend/src/loop_downloads.rs",
        "clear_all_empties_every_sites_state",
        "\"The successor to …, extended rather than deleted, and inverted \
         where its premise was the defect.\" It pinned \
         `LoopDownloadManager::clear_all`, which a site switch called for every \
         pane and which therefore emptied the loops of panes that had not \
         switched at all. That method is gone; the successor keeps its \
         assertions aimed at the per-pane call and adds the complement it \
         lacked. The paragraph is about which of the predecessor's claims \
         survived and which one *was* the bug, and it cannot make that \
         distinction without naming it.",
    ),
    (
        "rustdar-frontend/src/app_render/loop_level3_tests.rs",
        "clear_all_empties_the_level3_state_as_well",
        "The Level III half of the same replacement, split along the line the \
         keys draw: the pane-keyed queues go with `remove_pending`, the \
         site-keyed caches and day listings go on the eviction sweep. Naming \
         the predecessor is how a reader sees that one pin became two rather \
         than that a guard was dropped.",
    ),
];

/// Directory names the walk never descends into.
const SKIPPED_DIRS: &[&str] = &["target", ".git", ".claude", "node_modules"];

/// Whether the walk descends into `dir`.
///
/// Two rules, and the second is the one that matters.
///
/// `.claude` holds agent worktrees — 191 full checkouts of this repository when
/// this was written, each at whatever commit it forked from. `geodesy_one_
/// definition` learned this the expensive way (1566 findings on a developer box,
/// none in CI). Measured for *this* scan, on the primary checkout, the same
/// walk with and without them:
///
/// ```text
///                          files scanned   findings   distinct names
///   excluding worktrees              340         39               33
///   walking them                  41 091       1822               16
/// ```
///
/// The middle column is the obvious harm and the right-hand one is the harm
/// worth naming: walking those trees does not merely add noise, it **hides
/// real findings**. A test deleted here still exists in somebody's stale
/// checkout, so its dangling citation resolves against that copy and goes
/// quiet. Seventeen of the names this scan is meant to catch disappear. The
/// trap manufactures false negatives and false positives at the same time.
///
/// It also cannot be caught by observing that a run is clean: `.claude/` is
/// gitignored, so it exists in the primary checkout and in no worktree of this
/// repo — and most runs happen in a worktree. Whoever is best placed to notice
/// is the one person who cannot. [`the_walk_stops_at_nested_checkouts`] tests
/// this predicate directly for that reason.
///
/// The `.git` rule generalises it: **any** nested checkout is skipped, wherever
/// somebody puts one, because a directory carrying its own `.git` is a
/// different repository's working tree and its contents are not this
/// workspace's to police.
fn descends_into(dir: &Path) -> bool {
    let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
    !SKIPPED_DIRS.contains(&name) && !dir.join(".git").exists()
}

/// Every `.rs` and `.wgsl` file in the workspace, skipping build output and
/// nested checkouts.
fn sources(dir: &Path, into: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
    for entry in entries {
        let path = entry.expect("a readable directory entry").path();
        if path.is_dir() {
            if descends_into(&path) {
                sources(&path, into);
            }
        } else if matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("rs" | "wgsl")
        ) {
            into.push(path);
        }
    }
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("rustdar-radar sits one level under the workspace root")
        .to_path_buf()
}

/// Every name a citation may resolve to: `fn` and `mod` definitions anywhere in
/// the workspace, `vendor/` included.
///
/// `mod` counts because a doc may cite a whole suite rather than one case —
/// `restore_describes_its_image_tests` is a test module declared with `#[path]`
/// and pointed at by name from a neighbouring file, and that is a citation that
/// resolves.
///
/// The index reads `vendor/` even though [`scanned`] does not scan it, so a
/// citation reaching into a vendored crate's own tests still resolves.
///
/// This is a text match and not a parse: it will accept a name defined behind
/// a `cfg` that never builds on this platform, which is the behaviour worth
/// having. A citation to a test that only runs on macOS is still a true
/// citation, and this scan runs on one host at a time.
fn definitions(files: &[PathBuf]) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for path in files {
        let Ok(src) = std::fs::read_to_string(path) else {
            continue;
        };
        for keyword in ["fn", "mod"] {
            let mut rest = src.as_str();
            while let Some(at) = rest.find(keyword) {
                let (before, after) = rest.split_at(at);
                let after = &after[keyword.len()..];
                let boundary_before = before
                    .chars()
                    .next_back()
                    .is_none_or(|c| !c.is_alphanumeric() && c != '_');
                let mut chars = after.chars();
                let boundary_after = chars.next().is_some_and(char::is_whitespace);
                if boundary_before && boundary_after {
                    let name: String = after
                        .trim_start()
                        .chars()
                        .take_while(|c| c.is_alphanumeric() || *c == '_')
                        .collect();
                    if !name.is_empty() {
                        names.insert(name);
                    }
                }
                rest = &rest[at + keyword.len()..];
            }
        }
    }
    names
}

/// The name declared by a `fn` on this line, if there is one.
///
/// [`definitions`]' boundary rule, over a single line: `fn` has to be a whole
/// token, so `transfn` and `fnord` are not declarations. Split out because
/// [`ignored_tests`] needs the same judgement one line at a time, walking
/// forward through an attribute run rather than over a whole file.
fn declared_fn(line: &str) -> Option<String> {
    let mut rest = line;
    while let Some(at) = rest.find("fn") {
        let (before, after) = rest.split_at(at);
        let after = &after["fn".len()..];
        let boundary_before = before
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_alphanumeric() && c != '_');
        if boundary_before && after.starts_with(char::is_whitespace) {
            let name: String = after
                .trim_start()
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if !name.is_empty() {
                return Some(name);
            }
        }
        rest = &rest[at + "fn".len()..];
    }
    None
}

/// How far past an `#[ignore]` this will look for the function it decorates.
///
/// The attribute run between the two is a handful of lines at most — a
/// `#[test]`, a `#[cfg]`, an `#[allow]`. The bound is what stops a stray
/// `#[ignore` at the end of a file from claiming an unrelated function far
/// below it, which would silently widen the gate onto a test that is not
/// ignored at all.
const ATTRIBUTE_RUN_LINES: usize = 12;

/// Every test the workspace declares `#[ignore]`d.
///
/// # Why the attribute has to start its line
///
/// Matching `#[ignore` anywhere in the source instead over-reports badly, and
/// it does it silently: prose that *mentions* `#[ignore]` is followed by
/// whatever function comes next, and that function gets indexed as ignored. On
/// this tree that is the difference between 79 real entries and 96, and the 17
/// extras are names like `mercator_y`, `union`, `positions` and `gpu_lock` —
/// none of them tests, all of them the first `fn` under a comment discussing
/// the attribute. Requiring the attribute to open its own line is what
/// separates a declaration from a sentence about one.
///
/// Like [`definitions`] this is a text match and not a parse, and for the same
/// reason: a test ignored behind a `cfg` that never builds on this host is
/// still an ignored test.
fn ignored_tests(files: &[PathBuf]) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for path in files {
        let Ok(src) = std::fs::read_to_string(path) else {
            continue;
        };
        let lines: Vec<&str> = src.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            if !line.trim_start().starts_with("#[ignore") {
                continue;
            }
            // From the attribute's own line, so `#[ignore] fn f()` is caught
            // alongside the usual attribute run below it.
            for candidate in lines.iter().skip(index).take(ATTRIBUTE_RUN_LINES) {
                if let Some(name) = declared_fn(candidate) {
                    names.insert(name);
                    break;
                }
            }
        }
    }
    names
}

/// Consecutive comment lines, joined into blocks, with the line each block
/// starts on.
///
/// Blocks exist so a name the comment wrapped across two lines is still one
/// token. Only `//`-family comments are read: this tree contains no `/** */`
/// or `/*! */` doc comment at all, so line prefixes are complete coverage
/// rather than an approximation.
///
/// A run of `///` and a run of `//` are separate blocks, because a doc comment
/// and the implementation note under it are separate thoughts and joining them
/// could splice two half-names into one.
fn comment_blocks(src: &str) -> Vec<(usize, String)> {
    let mut blocks: Vec<(usize, String)> = Vec::new();
    let mut current: Option<(usize, &'static str, String)> = None;
    for (index, line) in src.lines().enumerate() {
        let trimmed = line.trim_start();
        let (kind, body) = if let Some(rest) = trimmed.strip_prefix("///") {
            ("///", rest)
        } else if let Some(rest) = trimmed.strip_prefix("//!") {
            ("//!", rest)
        } else if let Some(rest) = trimmed.strip_prefix("//") {
            ("//", rest)
        } else {
            if let Some((start, _, text)) = current.take() {
                blocks.push((start, text));
            }
            continue;
        };
        match &mut current {
            Some((_, open, text)) if *open == kind => {
                text.push('\n');
                text.push_str(body);
            }
            _ => {
                if let Some((start, _, text)) = current.take() {
                    blocks.push((start, text));
                }
                current = Some((index + 1, kind, body.to_string()));
            }
        }
    }
    if let Some((start, _, text)) = current {
        blocks.push((start, text));
    }
    blocks
}

/// Blank out fenced code blocks, preserving line breaks.
///
/// A ```` ``` ```` fence in a doc comment holds sample code or a data table,
/// not a sentence making a claim, and its contents would otherwise pair up with
/// the fence's own backticks and produce spans that were never citations.
fn blank_fenced(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut fenced = false;
    for (index, line) in text.lines().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if !fenced {
            out.push_str(line);
        }
    }
    out
}

/// Undo the line wrapping inside a backticked span.
///
/// A newline and the indentation after it are removed rather than replaced
/// with a space, which is what rejoins a name the comment broke
/// mid-identifier. Prose keeps its internal spaces and so still fails
/// [`is_sentence_shaped`].
fn rejoin(span: &str) -> String {
    let mut joined = String::with_capacity(span.len());
    let mut after_break = false;
    for c in span.chars() {
        if c == '\n' {
            after_break = true;
            continue;
        }
        if after_break {
            if c.is_whitespace() {
                continue;
            }
            after_break = false;
        }
        joined.push(c);
    }
    joined
}

/// The name a backticked span cites, if it cites one.
///
/// Takes the last `::` segment, so `beam::tests::the_rule_holds` is checked as
/// `the_rule_holds`: the module path in a citation is prose about where to look
/// and goes stale for reasons — a module rename, a file move — that are not
/// what this scan is for.
fn cited_name(span: &str) -> Option<String> {
    let joined = rejoin(span);
    let tail = joined.rsplit("::").next()?;
    let tail = tail
        .split(['(', '<', '['])
        .next()?
        .trim_end_matches('!')
        .trim();
    is_sentence_shaped(tail).then(|| tail.to_string())
}

/// Whether a name reads as one of this tree's test names: snake_case, all
/// lowercase, and at least [`MIN_SEGMENTS`] segments long.
///
/// Case is what keeps type and constant names out — `EARTH_RADIUS_KM` and
/// `VolumeGrid` are neither — and the segment count is what keeps ordinary
/// lowercase identifiers out.
fn is_sentence_shaped(name: &str) -> bool {
    let mut segments = 0;
    for (index, segment) in name.split('_').enumerate() {
        if segment.is_empty()
            || !segment
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        {
            return false;
        }
        if index == 0 && !segment.starts_with(|c: char| c.is_ascii_lowercase()) {
            return false;
        }
        segments += 1;
    }
    segments >= MIN_SEGMENTS
}

/// Backticked spans in a comment block, as `(line offset within block, text)`.
///
/// A run of two or more backticks is a fence remnant or an escaped literal and
/// opens nothing.
fn backticked(text: &str) -> Vec<(usize, String)> {
    let chars: Vec<char> = text.chars().collect();
    let mut spans = Vec::new();
    let mut line = 0usize;
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i] == '\n' {
            line += 1;
            i += 1;
            continue;
        }
        if chars[i] != '`' {
            i += 1;
            continue;
        }
        if chars.get(i + 1) == Some(&'`') {
            while i < chars.len() && chars[i] == '`' {
                i += 1;
            }
            continue;
        }
        let open_line = line;
        let mut j = i + 1;
        let mut span = String::new();
        let mut inner_lines = 0usize;
        while j < chars.len() && chars[j] != '`' {
            if chars[j] == '\n' {
                inner_lines += 1;
            }
            span.push(chars[j]);
            j += 1;
        }
        if j >= chars.len() {
            break;
        }
        spans.push((open_line, span));
        line += inner_lines;
        i = j + 1;
    }
    spans
}

/// The files this scan reads comments in: everything [`sources`] found, minus
/// `vendor/`.
///
/// Vendored crates are upstream code this workspace deliberately keeps close to
/// its published form — `VENDORED.md` in each one carries the diff list, and
/// the whole point is that the diff stays small enough to send upstream.
/// Policing upstream's prose would mean either editing it or allowing it, and
/// both make that diff worse. Their definitions are still indexed, so a
/// citation *into* a vendored crate resolves.
fn scanned(root: &Path, files: &[PathBuf]) -> Vec<PathBuf> {
    let vendor = root.join("vendor");
    files
        .iter()
        .filter(|path| !path.starts_with(&vendor))
        .filter(|path| path.extension().and_then(|e| e.to_str()) == Some("rs"))
        .filter(|path| !path.ends_with(file!()))
        .cloned()
        .collect()
}

/// One citation that resolves to nothing, with everything needed to judge it.
struct Citation {
    path: String,
    line_no: usize,
    span: String,
    name: String,
}

/// Every citation the workspace makes: how many were considered, and the ones
/// that resolve to nothing.
///
/// The total is returned and not just the failures because it is the only
/// thing that can tell a clean workspace from a broken extractor. See
/// [`the_scan_still_sees_a_population_of_citations`].
fn scan() -> (usize, Vec<Citation>) {
    let root = workspace_root();
    let mut files = Vec::new();
    sources(&root, &mut files);
    files.sort();
    let known = definitions(&files);

    let mut considered = 0usize;
    let mut dangling = Vec::new();
    for path in scanned(&root, &files) {
        let Ok(src) = std::fs::read_to_string(&path) else {
            continue;
        };
        let relative = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        for (start, text) in comment_blocks(&src) {
            for (offset, span) in backticked(&blank_fenced(&text)) {
                let Some(name) = cited_name(&span) else {
                    continue;
                };
                considered += 1;
                if !known.contains(&name) {
                    dangling.push(Citation {
                        path: relative.clone(),
                        line_no: start + offset,
                        span: rejoin(&span),
                        name,
                    });
                }
            }
        }
    }
    (considered, dangling)
}

/// No comment in the workspace names a test that does not exist, unless
/// [`ALLOWED`] says why.
#[test]
fn every_test_a_comment_names_is_a_test_that_exists() {
    let mut broken = Vec::new();
    for citation in scan().1 {
        let allowed = ALLOWED
            .iter()
            .any(|(path, name, _)| citation.path.ends_with(path) && citation.name == *name);
        if !allowed {
            // The span is only worth printing when the citation said more than
            // the name — a module path, or a name embedded in a sketch of a
            // call. Otherwise it would repeat the line above it.
            let written = if citation.span == citation.name {
                String::new()
            } else {
                format!("  (written as `{}`)", citation.span)
            };
            broken.push(format!(
                "  {}:{}  `{}` is not a `fn` or `mod` anywhere in the workspace{}",
                citation.path, citation.line_no, citation.name, written,
            ));
        }
    }

    assert!(
        broken.is_empty(),
        "{} comment(s) cite a test that does not exist:\n\n{}\n\nA citation that \
         resolves to nothing turns a guarantee into a claim, and it is almost always a \
         rename or a deletion that nobody carried into the prose. Point it at the test \
         that pins the behaviour now — and read that test, because the four citations \
         this scan was written for included two whose replacement asserted the \
         *opposite* of what the sentence claimed. If the name is genuinely not a test in \
         this tree — an upstream API, a spec parameter, a `campaign-harness` instrument, \
         or a sentence narrating its own deletion — add it to `ALLOWED` in {}, with the \
         reason. There is no wildcard on purpose.",
        broken.len(),
        broken.join("\n"),
        file!(),
    );
}

/// The token a comment block has to contain for a citation to an `#[ignore]`d
/// test to count as disclosed.
///
/// One word, matched case-insensitively, so `#[ignore]`, "ignored" and
/// `-- --ignored` all satisfy it. Deliberately not a list that also accepts
/// "needs a real adapter" or "on a real device": those say *why* a test is
/// gated, which a careful reader can already infer, and leave the fact that
/// matters — **the default `cargo test` row does not run it** — as an
/// inference. The convention this tree already states for live harnesses is
/// that the invocation goes in the doc comment, and that convention writes
/// this word for free.
const DISCLOSURE: &str = "ignore";

/// One citation to a test that does not run by default, in prose that does not
/// say so.
struct Undisclosed {
    path: String,
    line_no: usize,
    name: String,
}

/// Every citation to an `#[ignore]`d test whose comment block never discloses
/// the gating.
///
/// Disclosure is judged per **block**, not per line: a citation two sentences
/// under "run with `-- --ignored`" is disclosed, and splitting the rule finer
/// would force the word into the middle of every sentence that names a test.
///
/// The two halves read different text on purpose. Citations come out of
/// [`blank_fenced`], so a name inside a fenced example cannot invent one;
/// disclosure is read off the **raw** block, because the invocation a doc
/// comment is supposed to carry is usually *inside* a fence, and blanking it
/// would hide the very sentence this asks for.
fn undisclosed_ignored_citations() -> Vec<Undisclosed> {
    let root = workspace_root();
    let mut files = Vec::new();
    sources(&root, &mut files);
    files.sort();
    let ignored = ignored_tests(&files);

    let mut found = Vec::new();
    for path in scanned(&root, &files) {
        let Ok(src) = std::fs::read_to_string(&path) else {
            continue;
        };
        let relative = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        for (start, text) in comment_blocks(&src) {
            if text.to_ascii_lowercase().contains(DISCLOSURE) {
                continue;
            }
            for (offset, span) in backticked(&blank_fenced(&text)) {
                let Some(name) = cited_name(&span) else {
                    continue;
                };
                if ignored.contains(&name) {
                    found.push(Undisclosed {
                        path: relative.clone(),
                        line_no: start + offset,
                        name,
                    });
                }
            }
        }
    }
    found
}

/// A comment that cites an `#[ignore]`d test says that it is ignored.
///
/// # The gap this closes
///
/// [`every_test_a_comment_names_is_a_test_that_exists`] checks that a cited
/// name resolves. Resolution is all it checks — so "this is pinned by `foo`"
/// passes in full while `foo` carries an `#[ignore]` and the default
/// `cargo test` row never runs it. The prose promises a guard; the build
/// provides none; nothing anywhere notices. That is the same conversion of a
/// guarantee into a claim this file was written to stop, arriving through the
/// one door the resolution check leaves open.
///
/// **This is not a claim that those tests are wrong to be ignored.** 79 tests
/// in this tree carry the attribute and nearly all of them earn it: they need a
/// real adapter, or the network, or an archived volume on disk. The defect is
/// never the `#[ignore]` — it is a sentence that reads as though CI is watching
/// when it is not. So the fix is one honest clause, not an un-ignored test, and
/// there is no allowance list here for the same reason: every site can satisfy
/// this by saying what is true.
#[test]
fn a_citation_to_an_ignored_test_says_that_it_is_ignored() {
    let undisclosed = undisclosed_ignored_citations();
    let report: Vec<String> = undisclosed
        .iter()
        .map(|u| format!("  {}:{}  `{}`", u.path, u.line_no, u.name))
        .collect();

    assert!(
        report.is_empty(),
        "{} comment(s) cite a test that does not run by default, without saying so:\n\n{}\n\n\
         Each of those names carries an `#[ignore]`, so the default `cargo test` row skips \
         it and the sentence beside it promises a guard the build does not provide. Say so \
         in the same comment block — the word `{}` anywhere in the block satisfies this, and \
         the useful form is the invocation, e.g. \"`#[ignore]`d; run with \
         `cargo test -p rustdar-frontend --test volume_gpu -- --ignored`\". Do **not** \
         un-ignore the test to silence this: a test that needs a real adapter is right to be \
         ignored, and the thing being fixed is the prose. There is no allowance list, \
         because every site can satisfy this by writing what is true.",
        report.len(),
        report.join("\n"),
        DISCLOSURE,
    );
}

/// The ignored index finds a test behind its attribute run, and does not invent
/// one out of prose that merely mentions the attribute.
///
/// Both halves matter. An index that found nothing would make
/// [`a_citation_to_an_ignored_test_says_that_it_is_ignored`] pass over the
/// whole workspace while checking nothing — the same silent-green failure
/// [`the_index_finds_functions_and_modules`] exists to catch. An index that
/// found too much would flag citations to tests that run perfectly well, and
/// the fix a reader would reach for is to write a false sentence.
#[test]
fn the_ignored_index_finds_tests_and_not_sentences_about_them() {
    let root = workspace_root();
    let mut files = Vec::new();
    sources(&root, &mut files);
    let ignored = ignored_tests(&files);

    assert!(
        ignored.len() > 40,
        "this tree carries far more than 40 `#[ignore]`d tests; found {} — the \
         index is broken and the gate would pass without checking anything",
        ignored.len(),
    );
    for name in [
        // A GPU test: `#[ignore]` above `#[test]`, then the `fn`.
        "the_pipelines_build_on_a_real_device",
        // A live-network probe in a `#[cfg(test)]` module.
        "probe_list_files_installs_ring",
    ] {
        assert!(
            ignored.contains(name),
            "`{name}` is `#[ignore]`d and the index missed it",
        );
    }
    for name in [
        // The first `fn` under a comment that discusses `#[ignore]`. Indexing
        // these is what matching the attribute mid-line would have done.
        "mercator_y",
        "gpu_lock",
        "union",
    ] {
        assert!(
            !ignored.contains(name),
            "`{name}` is not an ignored test — the index is reading prose about \
             the attribute as a declaration of it",
        );
    }
}

/// A disclosure inside a fenced block still counts, because that is where an
/// invocation is usually written.
///
/// [`blank_fenced`] runs over the citation half of
/// [`undisclosed_ignored_citations`] and not over the disclosure half. Without
/// that asymmetry the recommended fix — putting the `-- --ignored` command line
/// in a fence — would leave the gate failing, and the only way to satisfy it
/// would be to repeat the word outside the fence.
#[test]
fn a_fenced_invocation_discloses() {
    let fenced = "/// Pinned by `a_name_with_five_or_more_segments`.\n\
                  ///\n\
                  /// ```text\n\
                  /// cargo test -- --ignored\n\
                  /// ```\n";
    let blocks = comment_blocks(fenced);
    assert_eq!(blocks.len(), 1, "the fixture is one comment block");
    let (_, text) = &blocks[0];
    assert!(
        text.to_ascii_lowercase().contains(DISCLOSURE),
        "the raw block carries the invocation and so discloses",
    );
    assert!(
        !blank_fenced(text).to_ascii_lowercase().contains(DISCLOSURE),
        "and the blanked block does not — which is exactly why disclosure is \
         read off the raw text, and why this test exists to pin the asymmetry",
    );
}

/// Every [`ALLOWED`] entry still matches a real citation, so the list cannot rot
/// into a set of licences for prose that no longer exists.
#[test]
fn no_allowance_outlives_the_citation_it_excuses() {
    let citations = scan().1;
    let stale: Vec<_> = ALLOWED
        .iter()
        .filter(|(path, name, _)| {
            !citations
                .iter()
                .any(|c| c.path.ends_with(path) && c.name == *name)
        })
        .map(|(path, name, _)| format!("  {path}  —  `{name}`"))
        .collect();

    assert!(
        stale.is_empty(),
        "{} allowance(s) in {} match nothing any more; delete them rather than \
         leaving a licence lying about:\n{}",
        stale.len(),
        file!(),
        stale.join("\n"),
    );
}

/// The walk stops at agent worktrees and at any other nested checkout.
///
/// This is the expensive lesson from the geodesy scan, and it cannot be checked
/// by observing that a run is clean: `.claude/` is gitignored, so it exists in
/// the primary checkout and in no worktree of this repo. Whoever runs the
/// suite from a worktree — which is most agents, most of the time — would see
/// nothing either way. So the predicate is tested directly.
#[test]
fn the_walk_stops_at_nested_checkouts() {
    let scratch = std::env::temp_dir().join(format!(
        "rustdar-doc-citations-{}-{}",
        std::process::id(),
        line!()
    ));
    let nested = scratch.join("some-nested-worktree");
    std::fs::create_dir_all(&nested).expect("a writable temp directory");
    // A worktree's `.git` is a file pointing at the real one, not a directory;
    // an ordinary clone's is a directory. Both must stop the walk, so the
    // predicate tests for existence and not for kind.
    std::fs::write(nested.join(".git"), "gitdir: /elsewhere\n").expect("a writable temp file");
    let ordinary = scratch.join("just-a-module");
    std::fs::create_dir_all(&ordinary).expect("a writable temp directory");

    assert!(
        !descends_into(&nested),
        "a directory carrying its own `.git` is another checkout and must not be scanned",
    );
    assert!(
        descends_into(&ordinary),
        "an ordinary source directory must still be scanned",
    );
    for skipped in SKIPPED_DIRS {
        assert!(
            !descends_into(&scratch.join(skipped)),
            "`{skipped}` must never be walked; `.claude` alone held 191 agent \
             worktrees when this was written, and walking them took this scan \
             from 39 findings to 1822 while dropping 17 of the real ones — \
             see `descends_into`",
        );
    }

    std::fs::remove_dir_all(&scratch).expect("the scratch directory to be removable");
}

/// The candidate filter admits this tree's test names and rejects the things
/// that sit next to them in prose.
///
/// The rejections are the half that would fail silently: a filter that admitted
/// everything would need an allowance for every upstream method this codebase
/// discusses, and one that admitted nothing would pass the workspace for ever.
#[test]
fn only_sentence_shaped_names_are_treated_as_citations() {
    for name in [
        "the_seam_rule_is_strictly_stronger_than_the_spread_rule",
        "every_site_answers_the_feedhorn_datum",
        "a_reconstructed_render_input_scan_is_refused",
        "the_derived_products_resample_their_own_field_not_the_raw_one",
        "live_derived_vild_on_the_2022_05_04_outbreak",
    ] {
        assert!(
            is_sentence_shaped(name),
            "`{name}` is exactly the shape this scan exists to check",
        );
    }

    for name in [
        // Ordinary identifiers: compound nouns, not sentences.
        "max_texture_dimension_2d",
        "km_per_degree_lat",
        "render_gate",
        // Types and constants are not tests.
        "EARTH_RADIUS_KM",
        "VoxelGrid",
        "RenderInput",
        // Prose in backticks is not an identifier.
        "a real sentence about a test",
        // Degenerate spellings.
        "",
        "_leading",
        "trailing_",
        "double__underscore",
    ] {
        assert!(
            !is_sentence_shaped(name),
            "`{name}` is not a test name and must not need an allowance",
        );
    }
}

/// A citation that wrapped across two comment lines is still one name, and the
/// finding still reports the line the name opened on.
///
/// Both halves matter. Without the rejoin, every long citation — which is to
/// say every citation, since the names are sentences — would look unresolvable
/// the moment rustfmt wrapped it. Without the line accounting, a finding in a
/// 200-line module doc points at the top of the block and the reader has to
/// hunt.
#[test]
fn a_wrapped_citation_is_one_name_and_reports_its_own_line() {
    let src = "\
//! First line of the module doc.
//! Second line, and then a name that wraps:
//! `the_seam_rule_is_strictly_stronger_than_
//! the_spread_rule` pins it.
//!
//! And `every_site_answers_the_feedhorn_datum` here.
";
    let blocks = comment_blocks(src);
    assert_eq!(blocks.len(), 1, "one unbroken run of `//!` is one block");
    let (start, text) = &blocks[0];
    assert_eq!(*start, 1);

    let found: Vec<(usize, String)> = backticked(&blank_fenced(text))
        .into_iter()
        .filter_map(|(offset, span)| cited_name(&span).map(|name| (start + offset, name)))
        .collect();

    assert_eq!(
        found,
        vec![
            (
                3,
                "the_seam_rule_is_strictly_stronger_than_the_spread_rule".to_string()
            ),
            (6, "every_site_answers_the_feedhorn_datum".to_string()),
        ],
        "the wrapped name must rejoin, and both must report the line they open on",
    );
}

/// Code is not prose: a citation is only read from a comment, so a test name
/// appearing in a string literal or in an expression is not a claim about
/// anything.
///
/// This is what lets the failure message above quote the very names it is
/// complaining about without the scan finding itself.
#[test]
fn only_comments_are_read() {
    let src = "\
fn f() -> &'static str { \"`a_name_that_does_not_exist_at_all` in a string\" }
let x = a_name_that_does_not_exist_at_all();
";
    let blocks = comment_blocks(src);
    assert!(
        blocks.is_empty(),
        "there is no comment here, so there is nothing to cite: {blocks:?}",
    );
}

/// A fenced block is sample code or a data table, and its contents are not
/// citations — including the table in `srm.rs` whose columns would otherwise
/// pair up with the fence's own backticks.
#[test]
fn fenced_blocks_are_inert() {
    let text = "\nA table:\n```text\nsite   `weird_name_inside_a_fence_here`\n```\nAnd `a_real_citation_outside_the_fence` after.";
    let names: Vec<String> = backticked(&blank_fenced(text))
        .into_iter()
        .filter_map(|(_, span)| cited_name(&span))
        .collect();
    assert_eq!(
        names,
        vec!["a_real_citation_outside_the_fence".to_string()],
        "only the citation outside the fence is one",
    );
}

/// The scan still sees a population of citations to check.
///
/// This is the canary for the whole file, and the failure it exists to catch is
/// the quiet one. Every other test here fails loudly when something breaks;
/// an extractor that stopped finding candidates — a comment marker that changed,
/// a backtick pair that stopped matching, a walk that returned no files — would
/// make [`every_test_a_comment_names_is_a_test_that_exists`] pass on an empty
/// set and read as a clean bill of health for ever.
///
/// 470 candidates when this was written. The bound is deliberately loose: it
/// is a check that the machinery is alive, not a headcount nobody may change.
#[test]
fn the_scan_still_sees_a_population_of_citations() {
    let (considered, _) = scan();
    assert!(
        considered > 250,
        "the scan found only {considered} citations to check, against 470 when \
         this was written. Something in the extraction has stopped working, and \
         the consequence is that the guard passes without checking anything.",
    );
}

/// The index really does find the definitions it claims to, including the two
/// shapes that are easy to miss: a `mod` and an attribute-decorated `fn`.
///
/// Without this, an index that silently found nothing would pass the whole
/// workspace — every citation would resolve to nothing, [`ALLOWED`] would be
/// the only thing keeping the suite green, and the failure would look like a
/// clean bill of health.
#[test]
fn the_index_finds_functions_and_modules() {
    let root = workspace_root();
    let mut files = Vec::new();
    sources(&root, &mut files);
    let known = definitions(&files);

    assert!(
        known.len() > 1000,
        "a workspace this size has far more than 1000 `fn` and `mod` \
         definitions; found {} — the index is broken and every citation would \
         look dangling",
        known.len(),
    );
    for name in [
        // This file's own tests, which are `fn`s behind a `#[test]`.
        "the_walk_stops_at_nested_checkouts",
        "every_test_a_comment_names_is_a_test_that_exists",
        // A `mod`, and one declared with `#[path]` at that.
        "restore_describes_its_image_tests",
        // A vendored crate's definition, proving the index reads past the
        // files `scanned` declines to scan.
        "decompress",
    ] {
        assert!(
            known.contains(name),
            "`{name}` is defined in this workspace but the index missed it",
        );
    }
}
