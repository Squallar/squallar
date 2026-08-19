//! One planet for horizontal geodesy, enforced by scanning the workspace.
//!
//! `rustdar_geo::EARTH_RADIUS_KM` and the
//! `rustdar_geo::KM_PER_DEGREE_LAT` derived from it are the *only*
//! sphere anything in this workspace may convert between degrees and ground
//! kilometres on. That was not true: `render_gate` placed gates on 6371 while
//! `ImageBounds`, the plan-view range ring, `volume.wgsl`'s floor and the
//! region-drag preview all framed them with a literal `111.32` (a 6378.1 km
//! equatorial sphere) and `volume_view` reduced the pane affine with a literal
//! `111.319_49`. Three planets, 0.11 % apart, biased one way — so echoes sat
//! consistently outside the geography drawn under them and the error grew with
//! range.
//!
//! # Why a scan and not a comment
//!
//! Every one of those sites already carried a comment saying which convention
//! it followed and why. Comments are not guards: each duplicate was added by
//! someone who read the neighbouring comment, believed it, and copied the
//! number anyway. The single-source arrangement removes the *reason* to write
//! a fourth — `KM_PER_DEGREE_LAT` is an expression over `EARTH_RADIUS_KM`, so
//! there is nothing to keep in sync — and this scan removes the *ability*.
//!
//! # What it looks at
//!
//! Every `.rs` and `.wgsl` file in the workspace, with comments and string and
//! char literals blanked out first, so a number quoted in prose or in an
//! assertion message is inert and only a number the compiler sees can fail
//! this. Any positive numeric literal landing in one of [`BANDS`] must be at a
//! site named in [`ALLOWED`], each with the reason it is not the shared
//! constant. There is no wildcard.
//!
//! **Negative literals are skipped**, which is what keeps CONUS longitudes
//! (`-111.19833`, `-110.63028`) out: a conversion factor is never negative and
//! a longitude in this hemisphere always is.
//!
//! # What it deliberately does not look at
//!
//! *Metres.* `rustdar-overlays`'s Lambert projection is parameterised by the
//! ellipsoid in the GRIB message (`LambertConformalConic::new(a, b, ..)`) and
//! holds no radius constant at all; the metre-scale figures in its tests are
//! PROJ reference fixtures for a datum that is genuinely not this one
//! (6 371 229 m for HRRR's sphere, Clarke 1866 for Snyder's worked example).
//! Those are a different projection's inputs, not a second opinion about this
//! one, so the scan stays in kilometres — the unit the radar crate's geodesy
//! is written in.
//!
//! *Refraction.* `beam::RE_EFF_KM` and the `1.21 · Re` Level III models are in
//! [`ALLOWED`] rather than excluded, because they are exactly the sites a
//! careless unification would "fix". Their entries say why they must not be.
//!
//! # The Web Mercator latitude limit rides along
//!
//! `rustdar_geo::MERCATOR_LAT_LIMIT_DEG` — 85.051128779806°, the latitude whose
//! projected `y` is exactly `π` — went the same way and for the same reason.
//! Three modules each spelled it `85.05`: the tile helpers, `overlay_cache`
//! and `rustdar-overlays`'s `render::rasterize`. Two were repaired one at a
//! time, and the third was *missed by the commit that repaired the second* —
//! which is the whole argument for a scan. That truncation is 0.0011287798°
//! short, 125.51 m of meridian, and in the rasterizer's case it desynchronised
//! the Y-range a texture was drawn for from the Y-range it was placed
//! between.
//!
//! Its band is deliberately narrow — see [`BANDS`] — and it is the one band
//! test files are exempt from, see [`is_test_file`].
#![cfg(not(target_arch = "wasm32"))]

use std::path::{Path, PathBuf};

/// Value ranges a literal must not land in without an entry in [`ALLOWED`].
///
/// The degree band stops at 111.9 rather than at 112 so it spans exactly the
/// real spread of a degree of latitude — 110.57 km at the equator to 111.69 at
/// the poles — and no further; `112.0` in the wild is a pixel or a colour
/// channel, never a geodesy figure.
///
/// The Mercator band starts at 85.04 rather than at 85.0 for the same kind of
/// reason, measured the same way. The limit is 85.051128779806°, so every
/// spelling of it anybody actually writes — `85.05`, `85.0511`,
/// `85.05112878` — is at or above 85.05, while a figure rounded as far as
/// `85.0` is 5.7 km short and is nobody's statement of a projection limit.
/// Bare `85.0` *is* four things in this workspace, none of them a latitude:
/// `palette.rs`'s 85 dBZ colour stop, `nrot.rs`'s rot-divisor table row and
/// its assertion, and a green channel in `rasterize.rs`. Starting the band
/// above them is what keeps this guard from crying wolf at a colour table.
const BANDS: &[(&str, f64, f64)] = &[
    ("earth radius, km", 6300.0, 6400.0),
    ("km per degree", 110.5, 111.9),
    ("mercator latitude limit", 85.04, 85.06),
];

/// Only integers are exempt in the degree band: kilometres per degree is a
/// fractional quantity and every real spelling of it has a decimal point,
/// while bare `111` is a slice bound and bare `112` is a tower height.
const DEGREE_BAND: &str = "km per degree";

/// The band [`is_test_file`] exempts, and the only one with an exemption.
const MERCATOR_BAND: &str = "mercator latitude limit";

/// Every site in the workspace permitted to name one of [`BANDS`]' numbers,
/// with the reason it is not `EARTH_RADIUS_KM` or `KM_PER_DEGREE_LAT`.
///
/// Matched as `(path suffix, substring of the stripped line)`. The substring
/// is what makes an entry a licence for *one statement* rather than for a
/// file: moving the constant is fine, quietly adding a second one beside it is
/// not.
const ALLOWED: &[(&str, &str, &str)] = &[
    // ── The definition ──────────────────────────────────────────────────
    (
        "rustdar-geo/src/lib.rs",
        "pub const EARTH_RADIUS_KM: f64 = 6371.0;",
        "THE definition, in the workspace's geodesy floor. Everything \
         horizontal is this sphere or an expression over it.",
    ),
    (
        "rustdar-geo/src/lib.rs",
        "pub const MERCATOR_LAT_LIMIT_DEG: f64 = 85.051_128_779_806_6;",
        "THE definition of the Web Mercator latitude limit, for the same \
         reason and after the same three-copy history. Every other crate \
         reaches it by re-export rather than restating it, so this is the \
         only literal spelling in non-test code.",
    ),
    // ── Atmospheric refraction: a different physical quantity ───────────
    (
        "rustdar-radar/src/beam.rs",
        "pub const RE_EFF_KM: f64 = 6371.0 * 4.0 / 3.0;",
        "The 4/3 effective earth radius: a standard-atmosphere refraction \
         model, not the planet. Derived from the same figure by coincidence \
         of physics. Written as the expression and never as 8494.667 because \
         `volumetric::tests::golden_echo_tops_grid_is_pinned` is bit-exact on \
         it (digest 0x5385ddeb1814353b, asserted there and four more times in \
         `chunks`). Not `EARTH_RADIUS_KM * 4.0 / 3.0` for the same reason: \
         `beam::tests::the_shared_effective_earth_radius_is_bit_identical_to_\
         both_deleted_copies` pins this exact association.",
    ),
    (
        "rustdar-radar/src/hca.rs",
        "const ML_RE_KM: f64 = 6371.0;",
        "`melting_layer.c`'s beam-height model, used only as `ML_IR · \
         ML_RE_KM` with ML_IR = 1.21. A Level III refraction model, not a \
         geodesy constant.",
    ),
    (
        "rustdar-radar/src/vil.rs",
        "const RE_KM: f64 = 6371.0;",
        "`a313t1.ftn`'s own `RE`, used only in the depth table's 4/3 \
         curvature term. Faithful to the source the twin is checked against.",
    ),
    (
        "rustdar-radar/src/dpprep.rs",
        "range_km * range_km / (2.0 * 1.21 * 6371.0)",
        "The B5 spec's `1.21 · Re` beam height, matching `RPGCS_height`. \
         Deliberately not the crate's 4/3 model — see the constant's doc.",
    ),
    (
        "rustdar-radar/src/eet.rs",
        "100.5 * 100.5 / (2.0 * 6371.0 * 4.0 / 3.0)",
        "A test computing the 4/3 height at 100.5 km precisely so it can be \
         shown to differ from `RPG_HEIGHT_QUADRATIC_PER_KM`'s 1.21 model, \
         which `eet` uses to match its Level III twin bit-for-bit.",
    ),
    // ── Tests that pin the refraction spellings ─────────────────────────
    (
        "rustdar-radar/src/beam/tests.rs",
        "let volumetric_spelling: f64 = 6371.0 * 4.0 / 3.0;",
        "Pins that de-duplicating `volumetric`'s copy of RE_EFF_KM was not a \
         numeric change. The literal is the deleted code, quoted.",
    ),
    (
        "rustdar-radar/src/beam/tests.rs",
        "let nrot_spelling: f64 = 4.0 / 3.0 * 6371.0;",
        "As above, for `nrot`'s copy, whose different association order \
         rounds to the same bits.",
    ),
    (
        "rustdar-radar/src/beam/tests.rs",
        "const RE_EFF_KM: f64 = 6371.0 * 4.0 / 3.0;",
        "A local re-implementation of the pre-`beam` height formula, kept so \
         the shared one can be shown to reproduce it.",
    ),
    (
        "rustdar-radar/src/beam/tests.rs",
        "assert_eq!(EARTH_RADIUS_KM, 6371.0);",
        "Asserts the definition's value, which is the one place a literal \
         6371 in an assertion is the point.",
    ),
    // ── The one hand-written copy outside Rust ──────────────────────────
    (
        "rustdar-volumetric/src/volume.wgsl",
        "const KM_PER_DEGREE_LAT: f32 = 111.194927;",
        "WGSL cannot read a Rust constant. This is the only copy of the \
         figure anywhere, and `rustdar-volumetric`'s \
         `the_shaders_km_per_degree_is_the_radar_crates_own` compares it to \
         `KM_PER_DEGREE_LAT` on f32 bits, so it cannot drift.",
    ),
];

/// Blank out comments and string/char literals, preserving byte offsets and
/// line breaks so a hit still reports its own line.
///
/// Handles nested `/* */`, raw strings with any hash count, and escapes. Good
/// enough for both Rust and WGSL, whose comment and string syntax Rust's is a
/// superset of.
fn strip_comments_and_strings(src: &str) -> String {
    let bytes: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    let mut block_depth = 0usize;
    let blank = |out: &mut String, run: &[char]| {
        for c in run {
            out.push(if *c == '\n' { '\n' } else { ' ' });
        }
    };
    while i < bytes.len() {
        let rest = &bytes[i..];
        if block_depth > 0 {
            if rest.starts_with(&['/', '*']) {
                block_depth += 1;
                blank(&mut out, &rest[..2]);
                i += 2;
            } else if rest.starts_with(&['*', '/']) {
                block_depth -= 1;
                blank(&mut out, &rest[..2]);
                i += 2;
            } else {
                blank(&mut out, &rest[..1]);
                i += 1;
            }
            continue;
        }
        if rest.starts_with(&['/', '/']) {
            let end = rest.iter().position(|c| *c == '\n').unwrap_or(rest.len());
            blank(&mut out, &rest[..end]);
            i += end;
            continue;
        }
        if rest.starts_with(&['/', '*']) {
            block_depth = 1;
            blank(&mut out, &rest[..2]);
            i += 2;
            continue;
        }
        // Raw string: `r`, any number of `#`, then `"`.
        if rest[0] == 'r' {
            let hashes = rest[1..].iter().take_while(|c| **c == '#').count();
            if rest.get(1 + hashes) == Some(&'"') {
                let mut j = i + hashes + 2;
                loop {
                    if j >= bytes.len() {
                        break;
                    }
                    if bytes[j] == '"'
                        && bytes[j + 1..]
                            .iter()
                            .take(hashes)
                            .filter(|c| **c == '#')
                            .count()
                            == hashes
                    {
                        j += 1 + hashes;
                        break;
                    }
                    j += 1;
                }
                let end = j.min(bytes.len());
                blank(&mut out, &bytes[i..end]);
                i = end;
                continue;
            }
        }
        if rest[0] == '"' {
            let mut j = i + 1;
            while j < bytes.len() {
                match bytes[j] {
                    '\\' => j += 2,
                    '"' => {
                        j += 1;
                        break;
                    }
                    _ => j += 1,
                }
            }
            let end = j.min(bytes.len());
            blank(&mut out, &bytes[i..end]);
            i = end;
            continue;
        }
        // Char literal, but not a lifetime: `'a'` or `'\n'`.
        if rest[0] == '\'' {
            let len = if rest.get(1) == Some(&'\\') {
                rest.iter()
                    .take(6)
                    .position(|c| *c == '\'')
                    .filter(|p| *p > 1)
            } else if rest.get(2) == Some(&'\'') {
                Some(2)
            } else {
                None
            };
            if let Some(close) = len {
                blank(&mut out, &rest[..=close]);
                i += close + 1;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

/// Every `.rs` and `.wgsl` file under the workspace root, skipping build
/// output, nested checkouts and this file.
///
/// `.claude` is skipped because agent worktrees live under it — full copies of
/// the repository at whatever commit they forked from. Scanning them reports
/// every duplicate this guard ever removed, from trees that are not this one:
/// 1566 findings on a developer box with worktrees present, and none in CI,
/// where a fresh clone has no `.claude/worktrees`. A guard that fires only off
/// CI is one people learn to ignore.
///
/// The self-exclusion is not a loophole: [`BANDS`] has to *state* the numbers
/// it is looking for, so the scanner is the one file that cannot pass its own
/// scan. `comments_and_string_literals_are_inert` and
/// `a_fresh_duplicate_would_be_caught` are what cover this file instead — they
/// exercise the stripper and the band test directly, which is stronger than
/// scanning it would be.
fn sources(dir: &Path, into: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
    for entry in entries {
        let path = entry.expect("a readable directory entry").path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if path.is_dir() {
            if !matches!(name, "target" | ".git" | ".claude" | "node_modules") {
                sources(&path, into);
            }
        } else if matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("rs" | "wgsl")
        ) && !path.ends_with(file!())
        {
            into.push(path);
        }
    }
}

/// Whether a workspace-relative path is a test file by name.
///
/// Only [`MERCATOR_BAND`] consults this, and only because the projection's own
/// tests have to be able to *quote* latitudes at the limit to check what
/// happens there. `tiles/tests.rs` carries fourteen of them — the limit to
/// every digit, one ulp either side of it, and `85.05` itself, whose whole job
/// is to be shown measurably short. Those are reference vectors checked
/// against `mercantile`, deliberately written as literals so the table cannot
/// validate the constant against itself, and making each one ask [`ALLOWED`]
/// for permission would turn the guard into a form to fill in.
///
/// The two geodesy bands get no such exemption, because their duplicates
/// *were* in test code as often as not and every one of them is in [`ALLOWED`]
/// by name.
///
/// This is by filename, so a `#[cfg(test)] mod tests` inside an ordinary
/// source file is still scanned — `nrot.rs`'s inline tests are, and should be.
fn is_test_file(relative: &str) -> bool {
    let name = relative.rsplit('/').next().unwrap_or(relative);
    relative.contains("/tests/") || name == "tests.rs" || name.ends_with("_tests.rs")
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("rustdar-radar sits one level under the workspace root")
        .to_path_buf()
}

/// One offending literal, with everything needed to judge or allow it.
struct Hit {
    path: String,
    line_no: usize,
    line: String,
    literal: String,
    band: &'static str,
}

fn scan() -> Vec<Hit> {
    let root = workspace_root();
    let mut files = Vec::new();
    sources(&root, &mut files);
    files.sort();

    let mut hits = Vec::new();
    for path in files {
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        let code = strip_comments_and_strings(&raw);
        let relative = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");

        for (line_no, line) in code.lines().enumerate() {
            for (start, literal) in numeric_literals(line) {
                let _ = start;
                let cleaned = literal.trim_end_matches('.').replace('_', "");
                let Ok(value) = cleaned.parse::<f64>() else {
                    continue;
                };
                for (band, low, high) in BANDS {
                    if value < *low || value > *high {
                        continue;
                    }
                    if *band == DEGREE_BAND && !cleaned.contains('.') {
                        continue;
                    }
                    if *band == MERCATOR_BAND && is_test_file(&relative) {
                        continue;
                    }
                    hits.push(Hit {
                        path: relative.clone(),
                        line_no: line_no + 1,
                        line: line.trim().to_string(),
                        literal: literal.to_string(),
                        band,
                    });
                }
            }
        }
    }
    hits
}

/// Positive numeric literals in a line of stripped code, as `(offset, text)`.
///
/// A run of digits, underscores and dots that starts where an identifier,
/// another number or a field access cannot be continuing, and whose nearest
/// non-space predecessor is not a minus sign.
fn numeric_literals(line: &str) -> Vec<(usize, &str)> {
    let bytes = line.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if !bytes[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let before = i.checked_sub(1).map(|p| bytes[p]);
        if before.is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.') {
            // Mid-identifier, mid-number or a tuple field: not a fresh literal.
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            continue;
        }
        let start = i;
        while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'_' || bytes[i] == b'.')
        {
            i += 1;
        }
        let negated = line[..start]
            .trim_end()
            .chars()
            .next_back()
            .is_some_and(|c| c == '-');
        if !negated {
            out.push((start, &line[start..i]));
        }
        // Skip any type suffix so `6371.0f64` is one literal, not two.
        while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
            i += 1;
        }
    }
    out
}

/// No file in the workspace names an earth radius, a kilometres-per-degree
/// figure or the Web Mercator latitude limit in code, unless [`ALLOWED`] says
/// why.
///
/// The failure message is the whole point: it prints the offending line and
/// tells the author the two things they can do about it, so the guard reads as
/// guidance rather than as an obstacle.
#[test]
fn horizontal_geodesy_has_exactly_one_definition() {
    let mut unexplained = Vec::new();
    for hit in scan() {
        let allowed = ALLOWED.iter().any(|(path, needle, _)| {
            hit.path.ends_with(path)
                && hit
                    .line
                    .replace(char::is_whitespace, "")
                    .contains(&needle.replace(char::is_whitespace, ""))
        });
        if !allowed {
            unexplained.push(format!(
                "  {}:{}  [{}]  {}\n      {}",
                hit.path, hit.line_no, hit.band, hit.literal, hit.line
            ));
        }
    }

    assert!(
        unexplained.is_empty(),
        "{} site(s) name an earth radius, a kilometres-per-degree figure or \
         the Web Mercator latitude limit without saying why:\n\n{}\n\nIf this \
         is horizontal geodesy — turning degrees into ground kilometres or \
         back — use `rustdar_geo::KM_PER_DEGREE_LAT` (or \
         `EARTH_RADIUS_KM` for a great circle) instead; that is the whole \
         reason it exists, and a second spelling puts the data and the map \
         under it on different planets. If it is the latitude Web Mercator \
         ends at, use `rustdar_geo::MERCATOR_LAT_LIMIT_DEG`: `85.05` \
         is 125.51 m short of it, and a clamp at one figure feeding a texture \
         placed at the other is the defect this band exists for. If it is \
         something else — a refraction model, a Level III twin's own \
         constant, a projection's parameter — add it to `ALLOWED` in {}, with \
         the reason. There is no wildcard on purpose.",
        unexplained.len(),
        unexplained.join("\n"),
        file!(),
    );
}

/// Every [`ALLOWED`] entry still matches something, so the list cannot rot
/// into a set of licences for code that no longer exists.
///
/// Without this, deleting an exception leaves a stale entry that would quietly
/// re-permit the same literal if it ever came back.
#[test]
fn no_allowance_outlives_the_code_it_excuses() {
    let hits = scan();
    let stale: Vec<_> = ALLOWED
        .iter()
        .filter(|(path, needle, _)| {
            !hits.iter().any(|hit| {
                hit.path.ends_with(path)
                    && hit
                        .line
                        .replace(char::is_whitespace, "")
                        .contains(&needle.replace(char::is_whitespace, ""))
            })
        })
        .map(|(path, needle, _)| format!("  {path}  —  {needle}"))
        .collect();

    assert!(
        stale.is_empty(),
        "{} allowance(s) in {} match nothing any more; delete them rather \
         than leaving a licence lying about:\n{}",
        stale.len(),
        file!(),
        stale.join("\n"),
    );
}

/// The scanner is blind to prose, which is what lets the modules above explain
/// the seam they closed without tripping the guard that closed it.
#[test]
fn comments_and_string_literals_are_inert() {
    let decoys = r###"
// const KM_PER_DEGREE_LAT: f64 = 111.32;
/* 6371.0 and /* nested */ 6378.1 */
/// A degree is 111.32 km on a 6378 km sphere.
fn f() -> &'static str { "6371.0 / 111.32" }
fn g() -> &'static str { r#"111.32"# }
"###;
    let stripped = strip_comments_and_strings(decoys);
    assert!(
        !stripped.contains("111.32") && !stripped.contains("6371") && !stripped.contains("6378"),
        "the stripper left a decoy behind:\n{stripped}",
    );
    assert_eq!(
        stripped.lines().count(),
        decoys.lines().count(),
        "stripping moved the line numbers, so a hit would report the wrong line",
    );
}

/// And it is not blind to code, which is the half that would fail silently:
/// a stripper that ate everything would pass the whole workspace.
#[test]
fn a_fresh_duplicate_would_be_caught() {
    let offenders = [
        "const KM_PER_DEGREE: f64 = 111.32;",
        "let d = x / 111.319_49;",
        "const R: f64 = 6371.0;",
        "let r = 6378.137f64;",
        // The three spellings the Mercator limit was actually found in, plus
        // the rounding a fourth would most likely arrive as.
        "const MAX_MERCATOR_LAT: f64 = 85.05;",
        "let lat = lat.clamp(-85.05112878, 85.05112878);",
        "min_lat: 85.0511_f64,",
    ];
    for line in offenders {
        let stripped = strip_comments_and_strings(line);
        let found = numeric_literals(&stripped).iter().any(|(_, literal)| {
            let cleaned = literal.trim_end_matches('.').replace('_', "");
            cleaned.parse::<f64>().is_ok_and(|v| {
                BANDS.iter().any(|(band, low, high)| {
                    v >= *low && v <= *high && (*band != DEGREE_BAND || cleaned.contains('.'))
                })
            })
        });
        assert!(
            found,
            "a duplicate spelled `{line}` would slip past the scan"
        );
    }

    // And the exclusions really do exclude: a CONUS longitude and a tower
    // height are not conversion factors, and a bare `85.0` is a dBZ stop, a
    // table row or a colour channel — every one of those is real code in this
    // workspace, quoted from `palette.rs`, `nrot.rs` and `rasterize.rs`.
    for benign in [
        "min_lon: -111.1424,",
        "tower_ft: 112,",
        "&alphas[111..],",
        "(85.0, (255, 140, 0)),",
        "(85.0, 8.23),",
        "((255.0 - f * 55.0) as u8, (135.0 - f * 85.0) as u8, 0)",
    ] {
        let stripped = strip_comments_and_strings(benign);
        let flagged = numeric_literals(&stripped).iter().any(|(_, literal)| {
            let cleaned = literal.trim_end_matches('.').replace('_', "");
            cleaned.parse::<f64>().is_ok_and(|v| {
                BANDS.iter().any(|(band, low, high)| {
                    v >= *low && v <= *high && (*band != DEGREE_BAND || cleaned.contains('.'))
                })
            })
        });
        assert!(!flagged, "`{benign}` is not a geodesy constant");
    }
}

/// The one exemption in this file recognises test files and nothing else.
///
/// It is spelled by filename, so it is exactly as wide as its name suggests —
/// which is worth pinning, because a looser rule (any path containing `test`)
/// would quietly exempt real source and the guard would still pass.
#[test]
fn only_files_named_as_tests_are_exempt() {
    for path in [
        "rustdar-egui/src/tiles/tests.rs",
        "rustdar-egui/src/overlay_cache/texture_budget_tests.rs",
        "rustdar-radar/tests/geodesy_one_definition.rs",
    ] {
        assert!(is_test_file(path), "`{path}` is a test file");
    }
    for path in [
        "rustdar-overlays/src/render/rasterize.rs",
        "rustdar-egui/src/tiles.rs",
        "rustdar-radar/src/types.rs",
        // Not a test file: `tests` is a path component of neither, and the
        // exemption must not spread by substring.
        "rustdar-radar/src/latest.rs",
        "rustdar-app/src/app_fetch.rs",
    ] {
        assert!(!is_test_file(path), "`{path}` is source, not a test");
    }
}

/// The exemption applies to the Mercator band only — the geodesy bands still
/// scan test files, which is where several of their duplicates lived.
#[test]
fn the_exemption_does_not_reach_the_geodesy_bands() {
    let scanned_bands: Vec<_> = BANDS
        .iter()
        .filter(|(band, ..)| *band != MERCATOR_BAND)
        .map(|(band, ..)| *band)
        .collect();
    assert_eq!(
        scanned_bands,
        vec!["earth radius, km", DEGREE_BAND],
        "a band was added or renamed without deciding whether test files see it",
    );
    // The geodesy allowances that live in test files are still matched, which
    // can only be true if test files are still scanned for those bands.
    let hits = scan();
    assert!(
        hits.iter()
            .any(|h| h.path.ends_with("rustdar-radar/src/beam/tests.rs")),
        "test files stopped being scanned for the geodesy bands",
    );
}
