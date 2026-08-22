//! Every test a comment names is a test that exists, enforced by scanning the
//! workspace.
//!
//! The convention everywhere in this tree is that a doc comment says which test
//! pins the behaviour it describes. This scan checks two things, and neither is
//! that the sentence is true.
//!
//! **One**: a cited name **resolves** to a `fn` or `mod` that exists. That
//! catches renames and deletions — the two ways a true citation rots without
//! anyone touching the prose. It cannot check that the test asserts what the
//! sentence says. Do not read a green run here as "the docs are true".
//!
//! **Two**: a citation to an `#[ignore]`d test **says that it is ignored** —
//! see [`a_citation_to_an_ignored_test_says_that_it_is_ignored`]. "This is
//! pinned by `foo`" otherwise passes in full while `foo` never runs in the
//! default `cargo test` row.
//!
//! A citation is a backticked span inside a `//`, `///` or `//!` comment whose
//! final `::` segment is snake_case with at least [`MIN_SEGMENTS`]
//! underscore-separated segments. Five is where the false-positive cost turns:
//! at four segments the upstream API surface this codebase discusses
//! (`max_texture_dimension_2d` and the like) drags in 122 permanent
//! allowances against 5 real findings; at six, ten of the allowances stop
//! matching anything at all, so the scan would be quieter while checking less.
//!
//! Of 470 candidates at five segments, 41 did not resolve on the first
//! workspace-wide run: 17 real defects and 24 not — upstream API names, ORPG
//! spec identifiers, instruments that live on a harness branch by policy, and
//! prose narrating a deletion. That is 5.1 % of candidates but 59 % of the raw
//! flags, which is why [`ALLOWED`] exists.
//!
//! Shaped after `geodesy_one_definition.rs`, this tree's existing source scan:
//! walk the workspace, judge each hit, and let an allowance through only with
//! the reason it is not a defect. Its lesson about `.claude/` is inherited too
//! — see [`descends_into`].
#![cfg(not(target_arch = "wasm32"))]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// How many underscore-separated segments a backticked name needs before this
/// scan treats it as a citation rather than as an ordinary identifier.
const MIN_SEGMENTS: usize = 5;

/// Every citation in the workspace that resolves to nothing on purpose, with
/// the reason it is not a defect.
const ALLOWED: &[(&str, &str, &str)] = &[
    // ── Instruments that live on `campaign-harness` by policy ───────────
    // ── Names owned by a dependency or an OS ────────────────────────────
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
        "rustdar-egui/src/input_harness.rs",
        "warn_if_rect_changes_id",
        "`egui::context`'s own debug assertion, cited to explain what the \
         harness has to keep quiet.",
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
    // ── Named source MUTATIONS, which are not tests ─────────────────────
    //
    // A floor states the edit that turns its assertion red, and gives that
    // edit a name so the report and the doc are talking about the same thing.
    // The name is a label for a hand-applied source change, never a `fn` —
    // most in this tree happen to be four segments and slip under
    // `MIN_SEGMENTS`; these are the ones that do not, and they are no
    // different in kind. Shortening a label to duck the threshold would hide
    // the class rather than record it.
    (
        "rustdar-app/src/app_render/loop_overlay_render_tests.rs",
        "price_the_overlay_as_radar",
        "WB-7 floor: make `overlay_frame_bytes` answer \
         `LoopFrameModel::plan_view` instead of the pane's measured texture. \
         A mutation, not a test.",
    ),
    (
        "rustdar-app/src/app_render/loop_supply_tests.rs",
        "price_the_overlay_as_radar",
        "The same WB-7 mutation, named in the suite whose expected counts move \
         under it (32 frames become 36).",
    ),
    (
        "rustdar-app/src/loop_pool/tests.rs",
        "price_the_overlay_as_radar",
        "The same WB-7 mutation at its source: `LoopFrameModel`'s `overlay` \
         arm answering `Budgets::loop_frame_bytes()`.",
    ),
    // ── Prose that narrates a deletion ──────────────────────────────────
    (
        "rustdar-volumetric/src/volume_bridge/tests.rs",
        "only_reflectivity_clears_the_fade_bar",
        "The successor's doc calls itself \"the deliberate flip of the \
         original\" and quotes that original's own words back. The name is the \
         subject, and the flip is the point of the paragraph.",
    ),
    (
        "rustdar-gpu/tests/floor_alignment.rs",
        "the_sites_pixel_lands_in_the_middle_of_a_site_centred_floor",
        "Names the test in the **deleted** `volume_floor/tests.rs` that this \
         one was re-pinned from, so a reader can see which half of the old \
         contract survived the floor texture's removal.",
    ),
    (
        "rustdar-gpu/tests/floor_alignment.rs",
        "a_tile_pixel_and_a_radar_gate_at_the_same_ground_land_on_the_same_texel",
        "As above, for the gate/pixel coincidence pin. The old contract's \
         tile route went with the compositor; the paragraph says so and keeps \
         the name that identifies it.",
    ),
    (
        "rustdar-egui/src/ui_map/volume_arm_tests.rs",
        "a_scroll_that_stops_stops_re_cutting_the_box",
        "\"used to stand here.\" The note explains why the settle it pinned \
         cannot arise now — there is no derivation and no quantum — and names \
         the stronger property's test in the same sentence.",
    ),
    // ── WO-SITE: the app-wide site, and the four rows that named it ─────
    //
    // The one-for-one mapping, kept here rather than only in a land message,
    // because this is the file that will notice when it rots. Each successor
    // names its predecessor in its own doc; the predecessors are gone because
    // the app-wide site they were about is gone.
    (
        "rustdar-egui/src/ui_config/fixture_tests.rs",
        "navigating_in_time_leaves_the_global_site_alone",
        "→ `navigating_in_time_leaves_every_panes_site_alone`. Same property, \
         wider subject: the clock must not reach a radar selection, and there \
         are now as many selections as panes rather than one app-wide one.",
    ),
    (
        "rustdar-egui/src/ui_config/fixture_tests.rs",
        "the_persisted_site_is_the_global_one_and_it_seeds_a_pane_that_names_none",
        "→ `the_save_names_no_app_wide_site_and_each_pane_carries_its_own` \
         (save half, **assertion inverted**: it pinned the top-level `site` \
         key as the global one, and the successor pins that no such key is \
         written) and → \
         `a_pane_the_file_never_described_opens_on_the_first_pane_it_did` \
         (load half: the seed for a pane the file says nothing about is the \
         file's own first site, not the retired key). Cited from both \
         successors.",
    ),
    (
        "rustdar-egui/src/input_harness/tests.rs",
        "refresh_fetches_the_active_panes_site_not_the_global_one",
        "→ `refresh_fetches_the_active_panes_site_not_another_panes`. The \
         property is unchanged; only its contrast partner moved, because the \
         global it named no longer exists and another pane's site is now the \
         site that could wrongly be fetched.",
    ),
    (
        "rustdar-egui/src/input_harness/tests.rs",
        "the_time_dialogs_ok_fetches_the_global_site_not_the_active_panes",
        "→ `the_time_dialogs_ok_fetches_the_active_panes_site`. **READ THIS \
         ONE.** The successor asserts the OPPOSITE of the predecessor, and \
         deliberately: the old row pinned that this single control fetched \
         the persisted global rather than the pane in front of the user. The \
         ruling that nothing is app-wide retires the global, so the dialog \
         fetches its pane like every other control. An authorised behaviour \
         change, not a rename.",
    ),
];

/// Directory names the walk never descends into.
const SKIPPED_DIRS: &[&str] = &["target", ".git", ".claude", "node_modules"];

/// Whether the walk descends into `dir`.
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
const ATTRIBUTE_RUN_LINES: usize = 12;

/// Every test the workspace declares `#[ignore]`d.
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
         `cargo test -p rustdar-gpu --test volume_gpu -- --ignored`\". Do **not** \
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
        "VolumeGrid",
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
#[test]
fn the_scan_still_sees_a_population_of_citations() {
    let (considered, _) = scan();
    assert!(
        considered > 50,
        "the scan found only {considered} citations to check, against 95 after \
         the comment-reduction pass re-baselined this. Something in the \
         extraction has stopped working, and the consequence is that the guard \
         passes without checking anything.",
    );
}

/// The index really does find the definitions it claims to, including the two
/// shapes that are easy to miss: a `mod` and an attribute-decorated `fn`.
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
