//! The JavaScript embedded in the browser rig's driver, checked by a JS engine.
//!
//! `drive.py` carries sixteen blocks of JavaScript as Python string constants
//! and ships them over the WebDriver wire to be executed in the page. **Until
//! this module, nothing in the tree had ever looked at that JavaScript.** The
//! line pins next door (`frame_telemetry_line_tests`,
//! `raster_telemetry_line_tests`) compare regex *text* against the Rust
//! formatters; `ast.parse` and `py_compile` parse the *Python*, not the string
//! it carries. Every one of those is structurally incapable of seeing a defect
//! in the JavaScript, and on 2026-09-04 one landed:
//! `frame_worst_all.push({ t: t, … })` in `WORKER_SIGNAL_PROBE`, where the
//! surrounding loop stamps entries `C[i].t` and the `var t = C[i].t` the idiom
//! was borrowed from belongs to a different loop in a different probe. In the
//! browser that is `ReferenceError: t is not defined`, thrown on the scrape
//! function's *first* execution — so the rig collected **no** signals, not
//! merely the new one, and two browser legs were spent before it was noticed.
//! It is repaired (`a9336df0`) and stays pinned: the tamper below puts it back
//! and requires the execution gate to catch it, so the class cannot go
//! unguarded again once the memory of it fades.
//!
//! **What this module covers, and what it does not.** `drive.py` holds two
//! different artefacts and they need different gates. This module covers the
//! **embedded JavaScript strings** — the text executed in the page. The
//! driver's **Python** — its selectors, its window arithmetic, everything
//! outside those string literals — is covered by `drive.py --selftest`, which
//! `run_tier2.sh` invokes. Neither gate says anything about the other's
//! artefact, and a reader who takes one as covering both is the failure mode
//! both exist to prevent: on 2026-09-04 a Python-side selector defect and this
//! JavaScript-side one were live at the same time, in the same file, and every
//! gate in the chain was green through both.
//!
//! **These gates are a cheap early filter, not the gate.** CI's rig job runs
//! `.github/browser-rig/run_tier2.sh` on both browsers and that really does
//! execute these scrapes, in the real engines, against a real page. It remains
//! the true pre-landing gate; what this module buys is that a `cargo test`
//! catches the same class in under a second, on a machine with no browser,
//! before anyone spends a leg on it. Nothing here replaces a browser leg, and
//! a green here is not evidence that a leg would pass.
//!
//! Neither engine used here is an engine the rig actually drives: the legs run
//! SpiderMonkey (Firefox, which governs) and V8 (Chrome). Node and Deno are
//! both V8, so a Firefox-only parse or scope difference is outside what these
//! gates can see.
//!
//! **Why the extraction is done here rather than through `drive.py`'s own
//! `--selftest`.** That entry point takes the thing under test as a parameter
//! (`selftest_export_window`, and the windowed worst-frame selector fixture
//! joining it, are both written to accept the function under test so the same
//! pins can be aimed at an older driver). It is the right seam for what it
//! holds — and what it holds is **Python callables**, pinned with Python
//! dictionaries. No JavaScript engine appears in it. Routing this module's
//! checks through it would buy no
//! fidelity, because the extraction below was checked against the strongest
//! available oracle and matched it exactly (2026-09-04: all sixteen constants
//! read both by this parser and by importing `drive.py` as a module — every
//! pair byte-identical). And it would cost the property that makes
//! [`the_worst_frame_scrape_reads_back_the_line_the_app_formats`] worth having:
//! that stub line is [`super::frame_worst_line`]'s own output, and a Rust
//! formatter is not reachable from inside `drive.py`, so the line would have to
//! become a hand-copied literal — the drift this module exists to avoid.

use std::process::Command;

/// The rig driver, read at compile time so a moved or deleted file is a build
/// failure rather than a skipped test — `frame_telemetry_line_tests`' reason.
const DRIVE_PY: &str = include_str!("../../../.github/browser-rig/drive.py");

/// How many JavaScript blocks `drive.py` is known to carry.
///
/// A floor, not a pin: new probes land and the number climbs. It exists so
/// that an extraction which quietly matches *nothing* — the shape a rename of
/// the constants would take — fails loudly instead of passing over an empty
/// list. A gate that cannot fail is worse than one that did not run.
const KNOWN_BLOCK_FLOOR: usize = 16;

/// How many of those blocks scrape the page-side console ring.
const KNOWN_SCRAPE_FLOOR: usize = 6;

/// A JS engine to run the checks under, or `None` on a machine with neither.
///
/// **Absence skips, it never reddens.** A box without a JS engine cannot
/// answer the question these gates ask, and a red there would say something
/// false about the driver. Node is preferred and Deno is the fallback; both
/// are V8, so the answer does not change between them.
fn js_engine() -> Option<(&'static str, &'static [&'static str])> {
    for (bin, args) in [
        ("node", &[][..] as &'static [&'static str]),
        ("deno", &["run", "--quiet"][..]),
    ] {
        if Command::new(bin)
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
        {
            return Some((bin, args));
        }
    }
    None
}

/// The message printed when no engine is present, so a skip can never be read
/// back as a pass.
const NO_ENGINE: &str = "SKIPPED: neither `node` nor `deno` is on PATH, so the JavaScript in \
                         drive.py was not checked here. This is a skip and NOT a pass: nothing \
                         about the driver has been verified.";

/// One embedded block: the Python constant's name, and the JavaScript the
/// browser is actually handed.
struct Block {
    name: String,
    src: String,
}

/// Every `NAME = """…"""` / `NAME = r"""…"""` block in `driver`, with any
/// `%`-formatting resolved so the string is what the browser receives rather
/// than the literal as typed.
///
/// **Proven byte-identical to the real module constants** (2026-09-04): the
/// sixteen constants were read both this way and by importing `drive.py` as a
/// Python module, and every pair compared equal. It stays honest because no
/// non-raw block contains a backslash — Python's escape processing is
/// therefore a no-op on all of them — and the one `%`-formatted block is
/// resolved below rather than guessed at.
///
/// Takes the driver text as an argument rather than reading [`DRIVE_PY`]
/// directly so that [`each_gate_rejects_a_driver_it_should_reject`] can feed
/// it deliberately broken copies. A gate proven only to pass is half verified.
fn embedded_blocks(driver: &str) -> Vec<Block> {
    let ints = |name: &str| -> Option<u64> {
        driver.lines().find_map(|l| {
            let (lhs, rhs) = l.split_once(" = ")?;
            (lhs == name).then(|| rhs.replace('_', "").parse().ok())?
        })
    };
    let mut out = Vec::new();
    let mut lines = driver.lines();
    while let Some(line) = lines.next() {
        let Some(name) = line
            .strip_suffix(" = \"\"\"")
            .or_else(|| line.strip_suffix(" = r\"\"\""))
        else {
            continue;
        };
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        {
            continue;
        }
        let mut body = String::new();
        let mut close = "";
        for l in lines.by_ref() {
            if l.starts_with("\"\"\"") {
                close = l;
                break;
            }
            body.push_str(l);
            body.push('\n');
        }
        // `NAME = """…""" % (A, B)`: substitute the named integer constants
        // positionally, as Python does. Only `%d` is supported, and a `%d`
        // left standing afterwards is a hard failure rather than a block
        // checked in a shape the browser never sees.
        if let Some(args) = close
            .strip_prefix("\"\"\" % (")
            .and_then(|r| r.split_once(')'))
            .map(|(a, _)| a)
        {
            for arg in args.split(',').map(str::trim).filter(|a| !a.is_empty()) {
                let v = ints(arg).unwrap_or_else(|| {
                    panic!(
                        "drive.py formats {name} with {arg}, which is not a \
                         module-level integer this extractor can resolve"
                    )
                });
                body = body.replacen("%d", &v.to_string(), 1);
            }
        }
        assert!(
            !body.contains("%d"),
            "{name} still carries a `%d` after formatting was resolved, so the \
             JavaScript checked here is not the JavaScript the browser is sent",
        );
        out.push(Block {
            name: name.to_string(),
            src: body,
        });
    }
    out
}

/// A string that the JavaScript regex `pattern` matches.
///
/// The group spellings are the ones `drive.py` actually uses. A spelling this
/// does not know would silently produce a line the probe cannot read, and the
/// family would then look empty for a reason that is this function's fault
/// rather than the driver's — so the emptiness rule below is stated over keys
/// the driver hands back, and a new group spelling shows up there as a red
/// that names the key.
fn sample_matching(pattern: &str) -> String {
    const SPELLINGS: [(&str, &str); 6] = [
        (r"(\d+|none|over)", "7"),
        (r"(\d+)", "7"),
        (r"([0-9,]+)", "1,2,3"),
        (r"([a-z0-9-]+)", "x"),
        (r"([A-Za-z0-9_-]+)", "x"),
        (r"([a-z0-9]+)", "x"),
    ];
    let mut out = String::new();
    let mut rest = pattern;
    while let Some((at, spelling, value)) = SPELLINGS
        .iter()
        .filter_map(|(s, v)| rest.find(s).map(|at| (at, *s, *v)))
        .min_by_key(|(at, _, _)| *at)
    {
        out.push_str(&rest[..at]);
        out.push_str(value);
        rest = &rest[at + spelling.len()..];
    }
    out.push_str(rest);
    out.replace(r"\(", "(")
        .replace(r"\)", ")")
        .replace(r"\/", "/")
        // The loose skew-detector patterns carry an unparenthesised `\d+`, and
        // `ground_stroke_draws_re` a `.*`.
        .replace(r"\d+", "7")
        .replace(".*", "filler")
}

/// One console line per family the probe scrapes, built from the probe's own
/// declarations: a sample for every `var …_re = /…/;`, and a line carrying the
/// literal of every `indexOf("…")`.
///
/// **What this cannot see.** These lines are derived from the *rig's* patterns,
/// so they cannot detect drift between what the app writes and what the rig
/// reads. That property is held next door — `frame_telemetry_line_tests` and
/// `raster_telemetry_line_tests` compare the rig's patterns to the Rust
/// formatters, sentence by sentence — and this module composes with them
/// rather than restating them. The one line fed from a Rust formatter here is
/// `frame worst:`, in
/// [`the_worst_frame_scrape_reads_back_the_line_the_app_formats`], because that
/// is the line today's defect was in.
fn stub_lines(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in src.lines() {
        if let Some(pat) = line
            .strip_prefix("var ")
            .and_then(|rest| rest.split_once(" = /"))
            .and_then(|(_, pat)| pat.strip_suffix("/;"))
        {
            out.push(sample_matching(pat));
        }
        let mut rest = line;
        while let Some(at) = rest.find("indexOf(\"") {
            rest = &rest[at + "indexOf(\"".len()..];
            let Some((lit, tail)) = rest.split_once("\")") else {
                break;
            };
            out.push(format!("stub prefix {lit} stub suffix"));
            rest = tail;
        }
    }
    out
}

/// Run `js` under the available engine and hand back its stdout.
fn run_js(engine: (&str, &[&str]), js: &str, tag: &str) -> String {
    let path = std::env::temp_dir().join(format!(
        "squallar-rig-js-{tag}-{}-{:?}.js",
        std::process::id(),
        std::thread::current().id(),
    ));
    std::fs::write(&path, js).expect("the temp directory is not writable");
    let out = Command::new(engine.0)
        .args(engine.1)
        .arg(&path)
        .output()
        .expect("the JS engine reported present by `--version` would not run");
    let _ = std::fs::remove_file(&path);
    assert!(
        out.status.success(),
        "the JS harness itself failed to run under {} — that is an instrument \
         failure, not a finding about drive.py:\n{}",
        engine.0,
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8(out.stdout).expect("the harness printed non-UTF-8")
}

/// Every block of `driver` that does not compile, as `NAME: <error>`.
///
/// Each block is a *function body* — they open no function and `return` at top
/// level, because WebDriver's execute-script wraps them — so `new Function`
/// is the right spelling: it compiles the body exactly as the wire does, and
/// it compiles **without executing**. (`node --check` is the wrong tool and
/// would reject every block: a top-level `return` is a syntax error in a
/// script.)
fn parse_failures(engine: (&str, &[&str]), driver: &str) -> Vec<String> {
    let blocks = embedded_blocks(driver);
    let payload = serde_json::to_string(
        &blocks
            .iter()
            .map(|b| serde_json::json!({ "name": b.name, "src": b.src }))
            .collect::<Vec<_>>(),
    )
    .expect("the blocks are plain strings");
    let out = run_js(
        engine,
        &format!(
            r#"const BLOCKS = {payload};
const bad = [];
for (const b of BLOCKS) {{
  try {{ new Function(b.src); }}
  catch (e) {{ bad.push(b.name + ": " + String(e)); }}
}}
console.log(JSON.stringify({{ checked: BLOCKS.length, bad: bad }}));
"#
        ),
        "parse",
    );
    let report: serde_json::Value = serde_json::from_str(&out).expect("the harness printed JSON");
    assert_eq!(
        report["checked"].as_u64(),
        Some(blocks.len() as u64),
        "the harness did not see every block it was handed",
    );
    report["bad"]
        .as_array()
        .expect("`bad` is a list")
        .iter()
        .map(|v| v.as_str().unwrap_or_default().to_string())
        .collect()
}

/// Keys a console scrape may legitimately hand back empty under this stub,
/// each with the reason it is empty — everything else must be filled.
///
/// The burden is deliberately inverted: a **new** family added to a probe is
/// required to be non-empty and reddens the gate until it is either scraped
/// properly or claimed here. That is the discipline
/// `every_frame_line_family_the_app_writes_has_a_named_rig_probe` uses, for
/// the same reason — a per-family allowlist that grows by default is how a
/// family goes unscraped for weeks without anything noticing.
const MAY_BE_EMPTY: &[(&str, &str, &str)] = &[
    (
        "WORKER_SIGNAL_PROBE",
        "console_ring_evicted",
        "a LEVEL off the page-side ring, and the stub ring never evicted",
    ),
    (
        "FRAME_LINE_PROBE",
        "gpu_unavailable",
        "the absence sentence, scanned verbatim rather than by a pattern, so \
         the stub declares no line for it",
    ),
];

/// Every way `driver`'s console scrapes fail when executed against a stub ring.
///
/// Two failures are reported, and they are different findings:
///
/// * **a scrape that throws.** An unbound identifier, a null dereference, a
///   method that does not exist on what the page actually stores: invisible to
///   a parse, and fatal in the browser *for the whole scrape* rather than for
///   one field, which is what made the last one expensive.
/// * **a hand-back key that comes back empty** after a line matching its
///   family was fed. Not-throwing alone would pass a scrape that ran to
///   completion and quietly gathered nothing — the same silent partial success
///   as a throw, minus the error.
///
/// Emptiness is judged at the top level of the hand-back only. A nested field
/// may be legitimately null on purpose — `ground.stroke_draws` is null against
/// a bundle built before 2026-09-01 and the driver documents that as a
/// reading — so a recursive rule would redden on a deliberate design.
fn exec_failures(engine: (&str, &[&str]), driver: &str) -> Vec<String> {
    let blocks = embedded_blocks(driver);
    let scrapes: Vec<&Block> = blocks
        .iter()
        .filter(|b| b.src.contains("__rig_console"))
        .collect();
    assert!(
        scrapes.len() >= KNOWN_SCRAPE_FLOOR,
        "only {} console scrapes were found and at least {KNOWN_SCRAPE_FLOOR} \
         are known to be there; the extraction has stopped matching",
        scrapes.len(),
    );
    // A line every probe is fed regardless of what it declares, so that a probe
    // scraping by neither regex nor `indexOf` (RIG_ERRORS_PROBE walks the ring
    // for an export budget; CONSOLE_MATCH_PROBE tests a caller's pattern) still
    // faces a ring with entries in it.
    const BASELINE: &str = "rasterization worker attached, rayon: 8 threads";
    let payload = serde_json::to_string(
        &scrapes
            .iter()
            .map(|b| {
                let mut lines = stub_lines(&b.src);
                lines.push(BASELINE.to_string());
                serde_json::json!({ "name": b.name, "src": b.src, "lines": lines })
            })
            .collect::<Vec<_>>(),
    )
    .expect("the scrapes are plain strings");
    let out = run_js(
        engine,
        &format!(
            r#"const SCRAPES = {payload};
// `CONSOLE_MATCH_PROBE` compiles `arguments[0]` into a RegExp: it is the
// generic wait-for-a-console-line primitive and every caller passes a pattern.
const ARGS = {{ CONSOLE_MATCH_PROBE: ["rasterization worker attached"] }};
const report = [];
for (const s of SCRAPES) {{
  const C = s.lines.map((m, i) => ({{ t: 1000 + i, lvl: "INFO", msg: m }}));
  // The page-side shape `serve.py`'s PAGE_PRELUDE installs.
  globalThis.window = {{
    __rig_console: C,
    __rig_errors: [{{ t: 1, msg: "stub error" }}],
    __rig: {{ t0: 1, seeded: {{ "squallar.ui": "{{}}" }} }},
    __rig_marks: C,
  }};
  try {{
    const got = new Function(s.src)(...(ARGS[s.name] || []));
    const empty = [];
    for (const k of Object.keys(got || {{}})) {{
      const v = got[k];
      if (v === null || v === undefined || v === false
          || (Array.isArray(v) && v.length === 0)
          || (typeof v === "object" && !Array.isArray(v)
              && Object.keys(v).length === 0)) empty.push(k);
    }}
    report.push({{ name: s.name, threw: null, fed: C.length,
                  keys: Object.keys(got || {{}}).length, empty: empty }});
  }} catch (e) {{
    report.push({{ name: s.name, threw: String((e && e.stack) || e),
                  fed: C.length }});
  }}
}}
console.log(JSON.stringify(report));
"#
        ),
        "exec",
    );
    let report: Vec<serde_json::Value> =
        serde_json::from_str(&out).expect("the harness printed JSON");
    assert_eq!(
        report.len(),
        scrapes.len(),
        "the harness did not run every scrape it was handed",
    );
    let mut failures = Vec::new();
    for row in &report {
        let name = row["name"].as_str().unwrap_or_default();
        if let Some(threw) = row["threw"].as_str() {
            failures.push(format!(
                "{name} THREW on execution against {} stub console entries — in \
                 the browser this loses the WHOLE scrape, not one field: {}",
                row["fed"],
                threw.lines().next().unwrap_or_default(),
            ));
            continue;
        }
        assert!(
            row["keys"].as_u64().unwrap_or(0) > 0,
            "{name} handed back no keys at all",
        );
        for key in row["empty"].as_array().into_iter().flatten() {
            let key = key.as_str().unwrap_or_default();
            if MAY_BE_EMPTY
                .iter()
                .any(|(probe, k, _)| *probe == name && *k == key)
            {
                continue;
            }
            failures.push(format!(
                "{name}.{key} came back empty after a line matching its family \
                 was fed — the scrape ran and collected nothing for it, which \
                 is the silent half of this failure mode",
            ));
        }
    }
    failures
}

/// **Every embedded JavaScript block in `drive.py` parses.**
///
/// The honest name is *parses*, and the claim stops there.
///
/// **This gate would NOT have caught the 2026-09-04 regression, and must never
/// be quoted as though it would.** `frame_worst_all.push({ t: t, … })` is
/// perfectly valid JavaScript; `t` being unbound is a *scope* error that only
/// execution reveals, and it is
/// [`every_console_scrape_executes_and_hands_back_every_family_it_was_fed`]
/// that catches it. What this catches is the class a parse can catch: a typo,
/// an unbalanced brace or paren, an unterminated string or regex literal, a
/// stray keyword. That is a real class — it is how a probe breaks under an
/// ordinary edit — and it is all this claims.
#[test]
fn every_embedded_javascript_block_parses() {
    let blocks = embedded_blocks(DRIVE_PY);
    assert!(
        blocks.len() >= KNOWN_BLOCK_FLOOR,
        "only {} JavaScript blocks were extracted from drive.py and at least \
         {KNOWN_BLOCK_FLOOR} are known to be there; the extraction has stopped \
         matching and this gate is checking almost nothing",
        blocks.len(),
    );
    let Some(engine) = js_engine() else {
        eprintln!("{NO_ENGINE}");
        return;
    };
    let bad = parse_failures(engine, DRIVE_PY);
    assert!(
        bad.is_empty(),
        "JavaScript in drive.py does not parse (engine: {}):\n  {}",
        engine.0,
        bad.join("\n  "),
    );
}

/// **Every console scrape in `drive.py` executes without throwing, and hands
/// back something for every family it was fed.**
///
/// This is the gate that catches the 2026-09-04 class, and the second half of
/// it is the one that catches the *expensive* shape: a scrape that runs and
/// quietly collects a subset. See [`exec_failures`] for what each half means
/// and [`MAY_BE_EMPTY`] for the keys allowed to come back empty.
///
/// **Not a substitute for a browser leg.** `run_tier2.sh` executes these
/// scrapes in Firefox and Chrome against a real page; this executes them in
/// V8 against a synthetic ring. It is the cheap filter that runs first.
#[test]
fn every_console_scrape_executes_and_hands_back_every_family_it_was_fed() {
    let Some(engine) = js_engine() else {
        eprintln!("{NO_ENGINE}");
        return;
    };
    let failures = exec_failures(engine, DRIVE_PY);
    assert!(
        failures.is_empty(),
        "the rig's console scrapes do not survive execution (engine: {}):\n  {}",
        engine.0,
        failures.join("\n  "),
    );
}

/// **The worst-frame scrape reads back the line the app formats, timestamp
/// included.**
///
/// The stub line here is not derived from the rig's own pattern — it is
/// [`super::frame_worst_line`]'s output, so it cannot drift from what the app
/// writes — and the assertion reaches into the entry the scrape pushed rather
/// than stopping at "the list is non-empty".
///
/// `t` is checked by name and on purpose. The 2026-09-04 defect was `t: t`
/// against a loop that stamps `C[i].t`, and while an *unbound* `t` throws (and
/// so is caught one gate up), a `t` that resolved to something wrong — a stale
/// binding, a literal — would not throw, would leave the family non-empty, and
/// would pass every other check in this file while every worst-frame reading
/// carried the wrong stamp.
#[test]
fn the_worst_frame_scrape_reads_back_the_line_the_app_formats() {
    let blocks = embedded_blocks(DRIVE_PY);
    // `FRAME_LINE_PROBE` and not `WORKER_SIGNAL_PROBE`: `60a889f0` moved the
    // worst-frame scrape into the probe the frame watcher POLLS, because the
    // one it lived in runs once and so no windowed worst frame was ever
    // selected. This test kept naming the old probe and read an empty family
    // out of it, which is the exact reading a broken scrape gives — the
    // instrument's own breakage printed as a result.
    let probe = blocks
        .iter()
        .find(|b| b.name == "FRAME_LINE_PROBE")
        .expect("drive.py no longer declares FRAME_LINE_PROBE");
    assert!(
        probe.src.contains("frame_worst_all"),
        "the worst-frame scrape has moved out of FRAME_LINE_PROBE again; \
         point this test at the probe the frame watcher polls, never at one \
         that runs once",
    );
    let Some(engine) = js_engine() else {
        eprintln!("{NO_ENGINE}");
        return;
    };
    let worst = crate::frame_ledger::WorstFrame {
        service: 13_455,
        segments: [64, 55, 9_514, 2_829, 700, 293],
        interact: true,
    };
    let boot = crate::frame_ledger::WorstFrame {
        service: 22_628,
        segments: [100, 90, 300, 21_000, 800, 338],
        interact: false,
    };
    let payload = serde_json::to_string(&serde_json::json!({
        "src": probe.src,
        "lines": [
            super::frame_worst_line(Some(worst), Some(boot)),
            super::frame_worst_line(None, Some(boot)),
        ],
    }))
    .expect("the probe and the lines are plain strings");
    let out = run_js(
        engine,
        &format!(
            r#"const P = {payload};
const C = P.lines.map((m, i) => ({{ t: 4200 + i, lvl: "INFO", msg: m }}));
globalThis.window = {{ __rig_console: C, __rig_errors: [], __rig: {{ t0: 1 }} }};
let got, threw = null;
try {{ got = new Function(P.src)(); }} catch (e) {{ threw = String((e && e.stack) || e); }}
console.log(JSON.stringify({{ threw: threw,
  worst: threw ? null : (got.frame_worst_all || []) }}));
"#
        ),
        "worst",
    );
    let report: serde_json::Value = serde_json::from_str(&out).expect("the harness printed JSON");
    assert!(
        report["threw"].is_null(),
        "the worker-signal scrape threw on the very line the app formats \
         (engine: {}): {}",
        engine.0,
        report["threw"].as_str().unwrap_or_default(),
    );
    let got = report["worst"].as_array().expect("`worst` is a list");
    assert_eq!(
        got.len(),
        2,
        "the scrape read {} worst-frame entries out of the two the app wrote; \
         `frame_worst_all` is what the p99 instrument is read from",
        got.len(),
    );
    // The presented-frame spelling: every segment, plus the boot anatomy, plus
    // the console entry's own stamp.
    assert_eq!(got[0]["t"].as_u64(), Some(4200), "the wrong console stamp");
    assert_eq!(got[0]["service"].as_u64(), Some(13_455));
    assert_eq!(got[0]["family"].as_str(), Some("interact"));
    assert_eq!(got[0]["since_boot"].as_u64(), Some(22_628));
    assert_eq!(got[0]["ui"].as_u64(), Some(9_514));
    assert_eq!(got[0]["boot_family"].as_str(), Some("idle"));
    assert_eq!(got[0]["boot_prepare"].as_u64(), Some(21_000));
    // The absence spelling: a period in which nothing presented still carries
    // the since-boot maximum and its stamp.
    assert_eq!(got[1]["t"].as_u64(), Some(4201), "the wrong console stamp");
    assert!(got[1]["service"].is_null());
    assert_eq!(got[1]["since_boot"].as_u64(), Some(22_628));
    assert_eq!(got[1]["boot_prepare"].as_u64(), Some(21_000));
}

/// **Each gate above reddens on a driver that deserves it, and passes on one
/// that does not.**
///
/// A gate proven only to *fire* is half verified, and here the other half is
/// the expensive one: these run on every rig edit, so a checker that
/// over-fires would block every change to `drive.py`. Both directions are
/// pinned, in memory, against copies of the real driver — no file on disk is
/// touched, so this stays honest on a tree another lane is editing.
///
/// Each mutation asserts it actually changed the text before it is used.
/// An unmatched pattern would otherwise make a tamper a silent no-op, and the
/// gate would then be "verified" by a copy identical to the original.
///
/// Red arm 2 reintroduces the exact 2026-09-04 defect rather than describing
/// it, so the regression that motivated this module stays pinned after the
/// driver was repaired (`a9336df0`). It asserts **both** halves of what made
/// that bug expensive: the parse checker must stay SILENT on it — it is valid
/// JavaScript — and the execution checker must catch it. A parse gate that
/// started reporting it would mean this module's prose understates itself;
/// an execution gate that stopped would mean the class is unguarded again.
#[test]
fn each_gate_rejects_a_driver_it_should_reject() {
    let Some(engine) = js_engine() else {
        eprintln!("{NO_ENGINE}");
        return;
    };

    // GREEN ARM. Both checkers pass on the driver as it stands. This is the
    // half that keeps the gates usable: they run on every rig edit, so a
    // checker that over-fires would block every change to drive.py.
    assert!(
        parse_failures(engine, DRIVE_PY).is_empty(),
        "the parse checker fires on a driver that parses",
    );
    let clean = exec_failures(engine, DRIVE_PY);
    assert!(
        clean.is_empty(),
        "the execution checker fires on a driver whose scrapes are sound: {clean:?}",
    );

    // RED ARM 1 — an unbalanced brace. The class a parse really does catch.
    let braced = DRIVE_PY.replace(
        "var backend = null, ceiling = null;",
        "var backend = null, ceiling = null; if (true) {",
    );
    assert_ne!(braced, DRIVE_PY, "the brace tamper matched nothing");
    let bad = parse_failures(engine, &braced);
    assert!(
        bad.iter().any(|f| f.starts_with("APP_BACKEND_PROBE")),
        "an unbalanced brace did not redden the parse checker: {bad:?}",
    );

    // RED ARM 2 — the 2026-09-04 defect's CLASS, put back: an identifier a
    // scrape reads that nothing in its scope binds.
    //
    // Not the original tamper any more, and that is the repair rather than a
    // weakening. The defect was `frame_worst_all.push({ t: t, …)` in a probe
    // whose loop bound no `t`; `a9336df0` moved that push into the loop that
    // DOES bind one, so re-spelling it `t: t` now names a live binding and
    // throws nothing. The tamper was a silent no-op and the gate read green
    // while proving nothing. Unbinding the loop's own `t` reproduces the
    // class at the same site.
    let unbound = DRIVE_PY.replace("  var t = C[i].t;", "  var t_unused = C[i].t;");
    assert_ne!(
        unbound, DRIVE_PY,
        "the unbound-identifier tamper matched nothing"
    );
    assert!(
        parse_failures(engine, &unbound).is_empty(),
        "the unbound-identifier defect is valid JavaScript and must NOT be \
         reported by the parse checker; if it is, the parse gate's prose is \
         understating what it catches",
    );
    let bad = exec_failures(engine, &unbound);
    assert!(
        bad.iter()
            .any(|f| f.contains("FRAME_LINE_PROBE") && f.contains("THREW")),
        "an unbound identifier in a scrape did not redden the execution \
         checker: {bad:?}",
    );

    // RED ARM 3 — a scrape that runs to completion and hands back nothing for
    // one family. No throw, so only assertion (b) can see it.
    let nulled = DRIVE_PY.replace(
        "return { backend: backend, raster_ceiling: ceiling,",
        "return { backend: null, raster_ceiling: ceiling,",
    );
    assert_ne!(
        nulled, DRIVE_PY,
        "the null-hand-back tamper matched nothing"
    );
    let bad = exec_failures(engine, &nulled);
    assert!(
        bad.iter().all(|f| !f.contains("THREW")),
        "the null-hand-back tamper was supposed to run cleanly and collect \
         nothing, not throw: {bad:?}",
    );
    assert!(
        bad.iter().any(|f| f.contains("APP_BACKEND_PROBE.backend")),
        "a family that silently collected nothing did not redden the \
         execution checker: {bad:?}",
    );
}

/// **Every JavaScript constant the driver executes is one this module checks.**
///
/// The gates above walk what [`embedded_blocks`] finds. If a new probe were
/// declared in a shape that extraction does not match, it would be executed in
/// the browser and checked by nothing here, and every gate in this file would
/// stay green while covering less — the exact shape of a check that quietly
/// stops checking. So the names passed to `session.execute(…)` and
/// `session.execute_async(…)` are enumerated from the driver's own call sites
/// and required to be present.
#[test]
fn every_javascript_constant_the_driver_executes_is_extracted() {
    let found: Vec<String> = embedded_blocks(DRIVE_PY)
        .iter()
        .map(|b| b.name.clone())
        .collect();
    let mut executed: Vec<String> = Vec::new();
    for call in ["session.execute(", "session.execute_async(", ".execute("] {
        let mut rest = DRIVE_PY;
        while let Some(at) = rest.find(call) {
            rest = &rest[at + call.len()..];
            let name: String = rest
                .chars()
                .take_while(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == '_')
                .collect();
            // A call site whose argument is not an ALL-CAPS identifier is an
            // inline literal (`return Date.now();`) or a variable holding one
            // of these constants; neither names a block to extract.
            if name.len() > 2 && !executed.contains(&name) {
                executed.push(name);
            }
        }
    }
    assert!(
        executed.len() >= 10,
        "only {} executed JavaScript constants were found at drive.py's call \
         sites; this check has stopped matching and proves nothing",
        executed.len(),
    );
    let missing: Vec<&String> = executed.iter().filter(|n| !found.contains(n)).collect();
    assert!(
        missing.is_empty(),
        "drive.py executes {missing:?}, which this module's extraction does \
         not find — those blocks reach the browser unchecked",
    );
}
