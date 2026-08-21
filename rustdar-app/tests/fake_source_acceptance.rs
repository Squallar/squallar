//! **The C1 acceptance suite: the wire half, and the pin that measures the
//! cost.**
//!
//! `rustdar-overlays`' `fake-source` feature registers a layer no crate above
//! it has an arm for. `rustdar-egui/src/acceptance_fake_source.rs` holds the
//! UI half — catalogue, draw, clock and config. This file holds:
//!
//! * **criterion 5**, the via-wire-vs-direct parity every codec row owes, run
//!   from *this* crate because this is where the composed registry, the
//!   offload funnel and the forwarding feature all exist at once (ON arm); and
//! * **criterion 6**, the footprint pin — what adding a source actually costs,
//!   file by file, in both arms.
//!
//! **These files are campaign infrastructure and land once.** They are the
//! files the footprint pin allows to name the fake; adding a *second* source
//! must not add a line to any of them.
//!
//! # What criterion 5 never names
//!
//! The parity test below reaches the fake **entirely through the trait**: it
//! lists frames, fetches one, applies it, asks for a job and hands the job to
//! the funnel, without naming a single type this crate could not have named
//! before the feature existed. That is not tidiness — it is the assertion.
//! `rustdar-overlays`' own `fake::tests::the_worker_path_and_the_direct_path_
//! paint_the_same_raster` proves the *row's* codec against the rasterizer it
//! calls in process; this proves the *composed registry* resolves the row off
//! the wire code, which is the half that lives up here.

// ─────────────────────────────────────────────────────────────────────────────
// Criterion 5: the wire
// ─────────────────────────────────────────────────────────────────────────────

/// **The fake's codec row passes the same via-wire-vs-direct parity every row
/// has** — `execute_bytes` on the job's own wire form against `execute` on the
/// job itself.
///
/// The job is built by the layer, from data the layer fetched, off stamps the
/// layer listed. Nothing here constructs the fake's input: a test that did
/// would be pinning its own fixture's encode rather than the one a running app
/// produces.
#[cfg(feature = "fake-source")]
#[test]
fn the_fakes_job_is_byte_identical_direct_and_through_the_composed_registry() {
    use rustdar_overlays::render::overlay_state::{FetchConfig, OverlayHandler, PaneRef};
    use rustdar_overlays::render::rasterize::RasterizeOutput;
    use rustdar_source::id::LayerId;
    use rustdar_source::job::JobGeometry;
    use rustdar_worker::offload::{JobRequest, execute, execute_bytes};

    const W: u32 = 96;
    const H: u32 = 64;

    // A `reqwest::Client` cannot be built before the process has a rustls
    // provider, and a test that builds one is otherwise green only when some
    // earlier test in the same binary happened to install it.
    rustdar_source::tls::init();
    let config = FetchConfig {
        client: Default::default(),
        zone_cache_dir: None,
        sources: rustdar_source::origins::DataSources::default(),
        viewport: None,
    };
    let pane = PaneRef::bare(0);
    let bounds = rustdar_geo::GeoBounds {
        min_lat: 34.0,
        max_lat: 36.0,
        min_lon: -99.0,
        max_lon: -97.0,
    };
    let geometry = JobGeometry {
        width: W,
        height: H,
        bounds,
        side_ceiling_px: 0,
    };

    // Reached by id through the registry — never by naming its type.
    let fake = LayerId::new("FakeSource");
    let mut handlers = rustdar_overlays::render::handlers::sources();
    let handler: &mut Box<dyn OverlayHandler> = handlers
        .iter_mut()
        .find(|h| h.id() == fake)
        .expect("this build registers the fake source");

    // A frame it has, off its own listing, fetched and applied through the
    // frame contract — which is what gives `prepare_job` something to draw.
    let base = chrono::DateTime::from_timestamp(1_700_000_000, 0)
        .expect("a literal inside chrono's range")
        .naive_utc();
    let window = (base, base + chrono::Duration::seconds(3600));
    let listing = handler.list_frames(&config, &pane, window);
    let stamp = *listing
        .frames
        .first()
        .expect("the fake lists frames over an hour-wide window");
    let task = handler
        .fetch_frame(&config, &pane, &stamp)
        .expect("a stamp the layer does not hold yet is fetchable");
    let data = pollster::block_on(task.future);
    handler.apply_frame(stamp, data, &pane);
    assert!(
        handler.has_data(&pane),
        "the applied frame did not make the layer hold data, so the job below \
         would be `None` for a reason that has nothing to do with the wire",
    );

    let ctx = rustdar_overlays::render::overlay_state::RasterizeContext {
        is_dark: true,
        zoom: 7.0,
        device_scale: 2.0,
        now: base,
        as_of: base,
    };
    let job = handler
        .prepare_job(&ctx, &pane)
        .expect("a layer holding a frame describes a job for it");
    let request = JobRequest { job, geometry };

    // The row really is in the composed registry, addressed by a wire code
    // this build allocates — the framing `execute_bytes` has to invert.
    let bytes = request.to_bytes();
    assert!(
        !bytes.is_empty(),
        "the fake's job encoded to nothing; the parity below would be between \
         two refusals",
    );

    let direct = execute(&request)
        .and_then(|out| out.take::<RasterizeOutput>())
        .expect("the fake's row runs in process");
    let via_wire = execute_bytes(&bytes)
        .and_then(|out| out.take::<RasterizeOutput>())
        .expect(
            "the composed registry did not resolve the fake's job off its own \
             wire form - the row is registered for dispatch and not for decode",
        );

    // Non-vacuity floor, first: an all-transparent raster would satisfy every
    // equality below without either path having painted anything.
    let painted = |rgba: &[u8]| rgba.chunks_exact(4).filter(|px| px[3] != 0).count();
    assert_eq!(
        direct.rgba.len(),
        (W * H * 4) as usize,
        "the direct run did not produce a raster of the geometry it was given",
    );
    assert!(
        painted(&direct.rgba) > 0,
        "the fixture painted nothing, so byte-identity is vacuous",
    );

    assert_eq!(
        via_wire.rgba, direct.rgba,
        "the fake's raster differs between the direct call and the wire - the \
         two paths have stopped being one renderer",
    );
    assert_eq!(
        via_wire.alpha, direct.alpha,
        "the alpha convention moved across the wire",
    );
    assert_eq!(
        via_wire.hit_cells.is_none(),
        direct.hit_cells.is_none(),
        "the reply's hit cells differ across the wire",
    );

    // And the codec is carrying the job's CONTENT, not merely its shape: a
    // second job built from a different frame must decode to a different
    // picture. Without this leg an encoder that dropped every member would
    // pass, because both paths would drop the same ones.
    let other = *listing
        .frames
        .last()
        .expect("the listing is non-empty, asserted above");
    assert_ne!(
        other, stamp,
        "the window listed one frame, so there is no second picture to \
         distinguish this one from",
    );
    let task = handler
        .fetch_frame(&config, &pane, &other)
        .expect("a second unheld stamp is fetchable");
    let data = pollster::block_on(task.future);
    handler.apply_frame(other, data, &pane);
    let second = JobRequest {
        job: handler
            .prepare_job(&ctx, &pane)
            .expect("still holding frames"),
        geometry,
    };
    let second_via_wire = execute_bytes(&second.to_bytes())
        .and_then(|out| out.take::<RasterizeOutput>())
        .expect("the second job also decodes");
    assert_ne!(
        second_via_wire.rgba, via_wire.rgba,
        "two different frames produced the same raster through the wire, so \
         the parity above says nothing about what the codec carries",
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Criterion 6: the footprint pin
// ─────────────────────────────────────────────────────────────────────────────

/// Workspace root: this integration test's manifest dir is `rustdar-app/`.
const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/..");

/// **The fake's own TYPES and module paths.** These are the spellings that must
/// never leave `rustdar-overlays`: a crate above that names one has an arm for
/// this layer, which is precisely the coupling C1 says a new source must not
/// create.
const TYPE_NEEDLES: &[&str] = &[
    "FakeSourceHandler",
    "FakeInput",
    "FakeTint",
    "FakeJob",
    "FakeFrameData",
    "FakePaneState",
    "rasterize_fake",
    "FAKE_LABEL",
    "handlers::fake",
    "mod fake",
    "fake::",
];

/// **The fake's NAMES** — the id string, the feature, the field id, the wire
/// label, the file. Unlike the types these legitimately reach outside the
/// crate: an id string is a *reserved spelling*, and a count pin holds them
/// rather than a zero.
///
/// Deliberately narrow. A bare `Fake` needle also matches `FakePort`, the
/// unrelated `JobSink` double in `rustdar-worker`'s offload suite — the
/// substring collision that inflated this campaign's pin list twice before.
const NAME_NEEDLES: &[&str] = &[
    "FakeSource",
    "fake-source",
    "FakeField",
    "overlay/fake",
    "fake.rs",
    "fake_source",
    "\"Fake\"",
];

/// **The measured footprint, outside `rustdar-overlays`, in lines.**
///
/// Counting rule: a line counts if it contains any [`TYPE_NEEDLES`] or
/// [`NAME_NEEDLES`] spelling. **`#[cfg]` attribute lines count**, and so do
/// comment lines — this is the whole cost of the layer's existence to crates
/// that do not own it, not the subset somebody would call "real code".
///
/// | file | lines | what they are |
/// |---|---|---|
/// | `rustdar-source/src/id.rs` | 5 | the `known::FAKE_SOURCE` const, its doc, and the unconditional `LAYER_ID_LEDGER` row (minor fix 8: a ledger row **reserves** a spelling, and reserving one under a `cfg` would let a feature-off build hand the same string to something else) |
/// | `rustdar-egui/src/sources.rs` | 18 | the `cfg!`/`#[cfg]` arms of the composed registry's own pins — the layer count, the state keys, the draw order, the time axis, the group count |
/// | `rustdar-egui/src/lib.rs` | 2 | the acceptance module's declaration |
/// | `rustdar-egui/src/ui_config/fixture_tests.rs` | 1 | one `cfg!` term in a slot-count assertion |
/// | `rustdar-egui/src/ui_map_pane/as_of_token_tests.rs` | 1 | one `cfg!` term in a layer-count assertion |
/// | `rustdar-app/src/app_fetch/as_of_dispatch_tests.rs` | 1 | one `cfg!` term in a layer-count assertion |
/// | `rustdar-app/src/channels.rs` | 1 | a comment recording that the fake needed **no** channel — the receiver count did not move |
///
/// **Every one of these is a TEST-side count term or a reserved spelling.**
/// Not one is production behaviour: no crate above `rustdar-overlays` branches
/// on this layer, and [`no_file_outside_overlays_names_the_fakes_types`] is the
/// assertion that says so.
const FOOTPRINT_OUTSIDE: &[(&str, usize)] = &[
    ("rustdar-source/src/id.rs", 5),
    ("rustdar-egui/src/sources.rs", 18),
    ("rustdar-egui/src/lib.rs", 2),
    ("rustdar-egui/src/ui_config/fixture_tests.rs", 1),
    ("rustdar-egui/src/ui_map_pane/as_of_token_tests.rs", 1),
    ("rustdar-app/src/app_fetch/as_of_dispatch_tests.rs", 1),
    ("rustdar-app/src/channels.rs", 1),
];

/// **The registration sites inside `rustdar-overlays`, in lines.**
///
/// `render/handlers/fake.rs` is not here: it is the layer itself, not a
/// registration of it, and its size is the source's own business.
///
/// | file | lines | what they are |
/// |---|---|---|
/// | `render/handlers/mod.rs` | 7 | `#[cfg]` + `pub mod fake;`, `#[cfg]` + the `rows.push`, the unconditional `HANDLER_SOURCES` row, and two comment lines explaining the first two |
/// | `render/jobs.rs` | 22 | the **second `#[cfg]`'d definition** of `JOB_CODECS` (amendment C4: `#[cfg]` on an array element is not stable), `FAKE_LABEL`, the `FakeJob` `JobSpec`/`JobOutCodec` impls, and the second `#[cfg]`'d definition of the label pin's expected list |
/// | `render/handlers/texture_tests.rs` | 14 | the crate's own fixture-coverage walk reaching the fake's rasterizer |
const FOOTPRINT_INSIDE: &[(&str, usize)] = &[
    ("rustdar-overlays/src/render/handlers/mod.rs", 7),
    ("rustdar-overlays/src/render/jobs.rs", 22),
    ("rustdar-overlays/src/render/handlers/texture_tests.rs", 14),
];

/// The acceptance suite itself — campaign infrastructure that landed once at
/// WO-E10.3 and is **not** a per-source cost. A second source adds no line to
/// any of these.
const ACCEPTANCE_FILES: &[&str] = &[
    "rustdar-egui/src/acceptance_fake_source.rs",
    "rustdar-app/tests/fake_source_acceptance.rs",
];

/// The forwarding-feature entries amendment M1 requires — one manifest line
/// each, so a crate's own suite can turn the layer on. Not `.rs`, so the walk
/// below never sees them; pinned here because they are part of the cost.
const FORWARDING_MANIFESTS: &[(&str, usize)] = &[
    ("rustdar-egui/Cargo.toml", 3),
    ("rustdar-app/Cargo.toml", 1),
];

/// What the failure of any footprint assertion means. One sentence, quoted by
/// every one of them, because the two readings are genuinely different actions.
const VERDICT: &str = "adding a source grew a registration point - either the \
                       architecture regressed or N must be consciously \
                       re-pinned.";

fn count_needles(src: &str) -> usize {
    src.lines()
        .filter(|line| {
            TYPE_NEEDLES.iter().any(|n| line.contains(n))
                || NAME_NEEDLES.iter().any(|n| line.contains(n))
        })
        .count()
}

fn read(path: &str) -> String {
    let full = std::path::Path::new(ROOT).join(path);
    std::fs::read_to_string(&full)
        .unwrap_or_else(|e| panic!("the pinned path {} is unreadable: {e}", full.display()))
}

/// Every tracked-looking `.rs` file in the workspace, workspace-relative.
/// Skips `target`, `pkg`, `vendor` and dot-directories, and never leaves the
/// workspace.
fn workspace_rs_files() -> Vec<String> {
    fn walk(dir: &std::path::Path, root: &std::path::Path, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if path.is_dir() {
                if name == "target" || name == "pkg" || name == "vendor" || name.starts_with('.') {
                    continue;
                }
                walk(&path, root, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(
                    path.strip_prefix(root)
                        .expect("the walk never leaves the workspace")
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
    }
    let root = std::path::Path::new(ROOT)
        .canonicalize()
        .expect("the workspace root exists");
    let mut out = Vec::new();
    walk(&root, &root, &mut out);
    out.sort();
    out
}

/// **The strong half: the fake's TYPES never leave `rustdar-overlays`.**
///
/// This is a zero, and it is the clause that is actually about architecture. A
/// crate above naming `FakeInput` or `handlers::fake` has an arm for this
/// layer — the coupling the whole feature exists to prove absent.
///
/// The id STRING is a different question and is not asserted here: it is a
/// reserved spelling, and the ledger reserves it unconditionally on purpose.
/// [`the_fakes_footprint_outside_overlays_is_exactly_what_is_pinned`] counts
/// those instead.
#[test]
fn no_file_outside_overlays_names_the_fakes_types() {
    let files = workspace_rs_files();
    assert!(
        files.len() > 100,
        "the walk found only {} .rs files, so it is not walking the workspace \
         and every zero below is the walk's, not the code's",
        files.len(),
    );
    // Non-triviality: the walk must be able to see a file that DOES name them.
    let home = "rustdar-overlays/src/render/handlers/fake.rs";
    assert!(
        files.iter().any(|f| f == home),
        "the walk did not reach {home}, so it could not have found a type \
         mention anywhere",
    );
    assert!(
        count_needles(&read(home)) > 0,
        "{home} names none of the pinned needles, so the needles no longer \
         describe this layer and every zero below is vacuous",
    );

    let mut offenders: Vec<String> = Vec::new();
    for file in &files {
        if file.starts_with("rustdar-overlays/") || ACCEPTANCE_FILES.contains(&file.as_str()) {
            continue;
        }
        let src = read(file);
        for (n, line) in src.lines().enumerate() {
            if let Some(needle) = TYPE_NEEDLES.iter().find(|n| line.contains(**n)) {
                offenders.push(format!("{file}:{}: {needle}", n + 1));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "a crate outside rustdar-overlays names one of the fake source's own \
         types or module paths, so it has an ARM for a layer it is supposed to \
         be blind to - {VERDICT}\n{}",
        offenders.join("\n"),
    );
}

/// **The counted half: every file outside `rustdar-overlays` that names the
/// fake at all, with its exact line count.**
///
/// The order this test was written for asked for a zero here too. It is not a
/// zero and it never could have been: the `LAYER_ID_LEDGER` row is
/// unconditional by ruling, and the composed registry's count pins each need a
/// `cfg!(feature = "fake-source") as usize` term or they would be wrong in one
/// arm. So the pin is the **table**, which is strictly stronger than "none":
/// it fails on a file that starts naming the fake, on a file that stops, and
/// on any file that grows or shrinks its share.
#[test]
fn the_fakes_footprint_outside_overlays_is_exactly_what_is_pinned() {
    let files = workspace_rs_files();
    let pinned: std::collections::BTreeMap<&str, usize> =
        FOOTPRINT_OUTSIDE.iter().copied().collect();
    assert_eq!(
        pinned.len(),
        FOOTPRINT_OUTSIDE.len(),
        "the pinned table names a file twice",
    );

    let mut measured: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for file in &files {
        if file.starts_with("rustdar-overlays/") || ACCEPTANCE_FILES.contains(&file.as_str()) {
            continue;
        }
        let count = count_needles(&read(file));
        if count > 0 {
            measured.insert(file.clone(), count);
        }
    }

    let expected: std::collections::BTreeMap<String, usize> = pinned
        .iter()
        .map(|(path, count)| ((*path).to_owned(), *count))
        .collect();
    assert_eq!(
        measured, expected,
        "the fake source's footprint outside rustdar-overlays moved - {VERDICT} \
         Every entry in this table is a test-side count term or a reserved id \
         spelling; a NEW file, or a file that grew, is the thing to look at",
    );
}

/// **The registration sites inside `rustdar-overlays`, pinned the same way.**
///
/// This is the number the acceptance criterion is really about: what one more
/// source costs the crate that owns it. `fake.rs` itself is excluded — a
/// source is allowed to be as large as it is.
#[test]
fn the_fakes_registration_footprint_inside_overlays_is_exactly_what_is_pinned() {
    for (path, expected) in FOOTPRINT_INSIDE {
        let src = read(path);
        assert!(
            !src.is_empty(),
            "{path} is empty, so its count below would be a vacuous zero",
        );
        assert_eq!(
            count_needles(&src),
            *expected,
            "{path} carries a different number of fake-source lines than the \
             {expected} pinned - {VERDICT}",
        );
    }
    // The one manifest line that declares the feature, and the two that
    // forward it. Read here rather than walked, because the walk is `.rs` only.
    assert_eq!(
        read("rustdar-overlays/Cargo.toml")
            .lines()
            .filter(|l| l.trim_start().starts_with("fake-source"))
            .count(),
        1,
        "rustdar-overlays declares the feature exactly once - {VERDICT}",
    );
    for (path, expected) in FORWARDING_MANIFESTS {
        assert_eq!(
            count_needles(&read(path)),
            *expected,
            "{path}'s forwarding-feature entry moved - {VERDICT} Amendment M1 \
             requires a forwarding FEATURE and not a dev-dependency",
        );
    }
}

/// **The acceptance suite is the only thing on the allowlist, and it is finite.**
///
/// The allowlist is what makes the two pins above readable, so it cannot be
/// allowed to grow quietly: a third file added to it would let a real arm hide
/// behind "campaign infrastructure".
#[test]
fn the_acceptance_allowlist_is_the_two_files_that_landed_at_e10_3() {
    assert_eq!(
        ACCEPTANCE_FILES.len(),
        2,
        "the acceptance allowlist grew. It is two files by design - the UI half \
         and this one - and anything else naming the fake is a per-source cost \
         that must appear in the footprint table instead: {VERDICT}",
    );
    for file in ACCEPTANCE_FILES {
        let src = read(file);
        assert!(
            count_needles(&src) > 0,
            "{file} is on the allowlist and names the fake nowhere, so the \
             allowlist entry is excusing a file that does not need excusing",
        );
    }
}
