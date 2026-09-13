//! The wasm heap ceiling the module is LINKED with, and the runtime ladder
//! each instance walks underneath it.
//!
//! # What changed, twice
//!
//! This file first held one equality: `WASM_LINEAR_MEMORY_MAX_BYTES` is the
//! `--max-memory` in `.github/scripts/wasm-threads.sh`. Then it learned that
//! the link flag is only what the module's memory import DECLARES, and a
//! supplied `WebAssembly.Memory` matches that import at any maximum **at or
//! below** it (54 cells plus negative controls across Firefox and Chromium,
//! 2026-09-03), so `heap.js` picked a figure per device underneath a 1 GiB
//! flag: 1 GiB for a desktop's page and worker, 512/256 MiB for a handheld's.
//!
//! **Both figures were guesses, and the desktop one was measured wrong.**
//! Chromium 152 and Firefox 155 on the rig's desktop construct a 4 GiB shared
//! memory and touch all of it, while Chromium's six-pane scene died at its
//! 1 GiB wall with eleven refused allocations. iOS WebKit, at the other end,
//! refuses large shared maxima at construction. So the flag is now wasm32's
//! architectural maximum -- 65,536 pages, which places no wall -- and each
//! instance finds its own at runtime by constructing down `heap.js`'s
//! `LADDER_BYTES` until the engine accepts a rung.
//!
//! **What an instance constructs is a reservation, and nothing is sized from
//! it.** An iPhone 13 Pro constructs 4 GiB and iOS kills the tab near 2.3 GiB,
//! so the budgets keep the per-device POLICY figures heap.js chose before --
//! 1024/1024 MiB for a desktop, 512/256 for a handheld -- which now travel to
//! the app beside the reservation and never select it.
//!
//! What is pinned here, from text: the flag is 65,536 pages and equal to the
//! linked constant; the ladder starts at the flag and descends strictly in
//! whole pages; the desktop policy is the budget constant and the handheld
//! pair keeps its shape; no policy figure reaches what is constructed; and the
//! page and worker bootstraps start their walks where the design says. What the walk DOES is exercised
//! under an engine in `tests/heap.test.mjs`, and what a browser really
//! constructed is read off every rig leg by `drive.py`'s
//! `linear_memory_ladder_verdict`.
#![cfg(not(target_arch = "wasm32"))]

use squallar_device_profile::constants::{
    DECLARED_RAM_HANDHELD_BYTES, WASM_LINKED_MAX_BYTES, WASM_POLICY_HEAP_BYTES,
};

const SCRIPT: &str = include_str!("../../.github/scripts/wasm-threads.sh");
const HEAP_JS: &str = include_str!("../heap.js");
const INDEX_HTML: &str = include_str!("../index.html");
const WORKER_JS: &str = include_str!("../worker.js");

/// wasm's page size, stated independently of `heap.js` so the two can disagree.
const WASM_PAGE: u64 = 65_536;

/// Every `--max-memory=<digits>` in `text`, in order, as bytes.
fn max_memory_flags(text: &str) -> Vec<u64> {
    const FLAG: &str = "--max-memory=";
    text.match_indices(FLAG)
        .map(|(at, _)| {
            let digits: String = text[at + FLAG.len()..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            digits
                .parse()
                .unwrap_or_else(|_| panic!("`{FLAG}` at byte {at} is not followed by a byte count"))
        })
        .collect()
}

/// A chain of integer literals joined by `*`, multiplied, or `None` for
/// anything else.
fn product(expr: &str) -> Option<u64> {
    expr.split('*')
        .map(|term| term.trim().parse::<u64>().ok())
        .try_fold(1u64, |acc, term| acc.checked_mul(term?))
}

/// The value of `export const <name> = <arithmetic>;` in `heap.js`, evaluated.
///
/// The right-hand sides there are written the way byte counts are written —
/// `512 * 1024 * 1024` — so this multiplies a chain of integer literals. A
/// name that is absent, or whose value is spelled any other way, panics: the
/// point of the pin is that a figure moved in one language reddens the other,
/// and a parser that quietly returned `None` would let it through.
fn js_const(name: &str) -> u64 {
    let needle = format!("export const {name} = ");
    let at = HEAP_JS
        .find(&needle)
        .unwrap_or_else(|| panic!("heap.js no longer exports `{name}`"));
    let rest = &HEAP_JS[at + needle.len()..];
    let end = rest
        .find(';')
        .unwrap_or_else(|| panic!("`{name}` in heap.js is not terminated by `;`"));
    let expr = &rest[..end];
    product(expr).unwrap_or_else(|| {
        panic!("`{name}` in heap.js is `{expr}`, not a product of integer literals")
    })
}

/// The elements of `export const <name> = [ ... ];` in `src`, each evaluated
/// as a product of integer literals. Panics on an absent list or an element
/// spelled any other way, for `js_const`'s reason.
fn js_const_list(src: &str, name: &str) -> Vec<u64> {
    let needle = format!("export const {name} = [");
    let at = src
        .find(&needle)
        .unwrap_or_else(|| panic!("heap.js no longer exports `{name}` as a list"));
    let rest = &src[at + needle.len()..];
    let end = rest
        .find("];")
        .unwrap_or_else(|| panic!("`{name}` in heap.js is not terminated by `];`"));
    rest[..end]
        .split(',')
        .map(str::trim)
        .filter(|element| !element.is_empty())
        .map(|element| {
            product(element).unwrap_or_else(|| {
                panic!("an element of `{name}` is `{element}`, not a product of integer literals")
            })
        })
        .collect()
}

/// The literal string assigned to `export const <name> = "..."` in `heap.js`.
fn js_string_const(name: &str) -> String {
    let needle = format!("export const {name} = \"");
    let at = HEAP_JS
        .find(&needle)
        .unwrap_or_else(|| panic!("heap.js no longer exports `{name}`"));
    let rest = &HEAP_JS[at + needle.len()..];
    rest[..rest.find('"').expect("unterminated string in heap.js")].to_string()
}

/// Every reason `rungs` is not a ladder an instance can walk under a module
/// linked at `flag`, one sentence a defect.
fn ladder_defects(rungs: &[u64], flag: u64, page: u64) -> Vec<String> {
    let mut defects = Vec::new();
    if rungs.len() < 2 {
        defects.push(format!(
            "a ladder of {} rung(s) is not a ladder",
            rungs.len()
        ));
    }
    if rungs.first() != Some(&flag) {
        defects.push(format!(
            "the top rung is {:?}, not the link flag {flag}: a device whose engine \
             accepts the flag would lose memory to this mechanism",
            rungs.first()
        ));
    }
    for &rung in rungs {
        if rung == 0 || rung % page != 0 {
            defects.push(format!(
                "{rung} B is not a whole, non-zero number of wasm pages"
            ));
        }
        if rung > flag {
            defects.push(format!(
                "{rung} B is above the link flag {flag}: the engine refuses a supplied \
                 memory above the declared maximum, so this rung can never link"
            ));
        }
    }
    for pair in rungs.windows(2) {
        if pair[0] <= pair[1] {
            defects.push(format!(
                "{} B then {} B: the ladder does not strictly descend",
                pair[0], pair[1]
            ));
        }
    }
    defects
}

/// **The link flag is stated exactly once, it is 65,536 pages, and it states
/// the constant.**
#[test]
fn the_linear_memory_ceiling_is_the_link_flag_and_the_link_flag_is_65536_pages() {
    let flags = max_memory_flags(SCRIPT);
    assert_eq!(
        flags.len(),
        1,
        "the presence control: `--max-memory=<bytes>` should appear exactly once \
         in wasm-threads.sh, so that one occurrence is the build's ceiling; \
         found {flags:?}",
    );
    assert_eq!(
        flags[0],
        65_536 * WASM_PAGE,
        "the link flag is not wasm32's architectural 65,536 pages: a smaller flag \
         is a compile-time wall again, and the ladder's top rungs could never link",
    );
    assert_eq!(
        flags[0], WASM_LINKED_MAX_BYTES,
        "the linker's ceiling and the budget system's constant have parted: the \
         constant follows the flag, never the other way round",
    );
}

/// The extractor really can disagree: a doctored script with another figure
/// yields that figure, one with the flag twice yields both, and the prose
/// mention of the flag without `=` is not counted.
#[test]
fn the_flag_extractor_reads_what_is_written() {
    assert_eq!(max_memory_flags("x -Clink-arg=--max-memory=42 y"), vec![42]);
    assert_eq!(
        max_memory_flags("--max-memory=1 then --max-memory=2\n"),
        vec![1, 2],
    );
    assert_eq!(
        max_memory_flags("#   --max-memory      required by wasm-ld"),
        Vec::<u64>::new(),
    );
}

/// **The ladder starts at the link flag and descends strictly, in whole
/// pages**, and `heap.js`'s page size is wasm's.
///
/// The top rung is the flag because a device that can have it should: that is
/// the whole reason the flag moved. Whole pages because a maximum is declared
/// in pages. Strictly descending because `constructMemory` walks it in order
/// and `initWithHeap` retries "one rung lower" as "the first rung below this
/// one" -- a repeated or ascending rung would re-try a refusal or skip a rung.
/// The 1 GiB and 512 MiB rungs are named because
/// `squallar_device_profile::linear_memory`'s pins are written against them.
#[test]
fn the_ladder_starts_at_the_link_flag_and_descends_in_whole_pages() {
    assert_eq!(js_const("PAGE_BYTES"), WASM_PAGE);
    let rungs = js_const_list(HEAP_JS, "LADDER_BYTES");
    assert_eq!(
        ladder_defects(&rungs, WASM_LINKED_MAX_BYTES, WASM_PAGE),
        Vec::<String>::new(),
        "LADDER_BYTES = {rungs:?}",
    );
    assert_eq!(
        rungs.last(),
        Some(&(128 << 20)),
        "the bottom rung moved: an engine that refuses it takes the glue's fallback"
    );
    for named in [1u64 << 30, 512 << 20] {
        assert!(
            rungs.contains(&named),
            "{named} B is no longer a rung; linear_memory.rs's pins are written against it"
        );
    }
}

/// The ladder checks can disagree: each defect class, planted, is named.
#[test]
fn the_ladder_checks_read_what_is_written() {
    const GIB: u64 = 1 << 30;
    let flag = 4 * GIB;
    assert!(ladder_defects(&[flag, 2 * GIB, GIB], flag, WASM_PAGE).is_empty());
    let named = |rungs: &[u64], word: &str| {
        let defects = ladder_defects(rungs, flag, WASM_PAGE);
        assert!(
            defects.iter().any(|d| d.contains(word)),
            "{rungs:?} was not named for `{word}` -- {defects:?}"
        );
    };
    named(&[2 * GIB, GIB], "not the link flag");
    named(&[flag, GIB, 2 * GIB], "does not strictly descend");
    named(&[flag, GIB, GIB], "does not strictly descend");
    named(&[flag, GIB + 1], "whole");
    named(&[8 * GIB, flag], "above the link flag");
    named(&[flag], "is not a ladder");

    let list = "export const LADDER_BYTES = [\n  4 * 1024,\n  1024,\n];";
    assert_eq!(js_const_list(list, "LADDER_BYTES"), vec![4096, 1024]);
    assert!(
        std::panic::catch_unwind(|| js_const_list(
            "export const LADDER_BYTES = [a];",
            "LADDER_BYTES"
        ))
        .is_err(),
        "a rung spelled as a name read as something instead of failing",
    );
}

/// **A desktop's budget policy is the budget constant, and no policy figure is
/// above it or off a page boundary.**
///
/// The policy is what every budget, watermark and admission door judges a heap
/// against, and it is exactly the figure they read before the reservation split
/// from it: `POLICY_DESKTOP_BYTES` is `WASM_POLICY_HEAP_BYTES`, which is the
/// wasm bracket's host presumption.
#[test]
fn the_desktop_policy_is_the_budget_constant_and_no_policy_is_above_it() {
    assert_eq!(
        js_const("POLICY_DESKTOP_BYTES"),
        WASM_POLICY_HEAP_BYTES,
        "heap.js's desktop policy is not the figure the budgets presume",
    );
    assert_eq!(
        squallar_device_profile::budget::BudgetLimits::WASM.presumed_host_bytes,
        Some(WASM_POLICY_HEAP_BYTES),
        "the wasm bracket presumes something other than a desktop's policy",
    );
    for name in [
        "POLICY_DESKTOP_BYTES",
        "POLICY_HANDHELD_PAGE_BYTES",
        "POLICY_HANDHELD_WORKER_BYTES",
    ] {
        let bytes = js_const(name);
        assert!(
            bytes <= WASM_POLICY_HEAP_BYTES,
            "heap.js's {name} ({bytes} B) is above a desktop's policy: a policy that \
             large is a reservation standing in for a budget",
        );
        assert_eq!(
            bytes % WASM_PAGE,
            0,
            "{name} is not a whole number of wasm pages"
        );
    }
}

/// **The two handheld policies are different figures, and the page's clears
/// its floor.** The page holds the caches -- the wasm bracket's own floor is
/// 128 MiB of tile host ceiling plus 56 MiB of loop pool -- and the worker holds
/// only the jobs in flight. Pinning `worker < page` keeps a later edit from
/// collapsing the pair into one number; pinning the page well above the floor
/// sum keeps it from being set so low that the watermark acts on every tick
/// with nothing to shed.
#[test]
fn the_handheld_page_and_worker_policies_differ_and_the_page_clears_its_floor() {
    let page = js_const("POLICY_HANDHELD_PAGE_BYTES");
    let worker = js_const("POLICY_HANDHELD_WORKER_BYTES");
    assert!(
        worker < page,
        "the handheld worker policy ({worker} B) is not below the page's ({page} B)",
    );
    assert!(
        page < WASM_POLICY_HEAP_BYTES,
        "the handheld page policy is the desktop's: nothing was chosen",
    );
    let floor = squallar_device_profile::constants::WASM_TILE_HOST_CEILING_BYTES[0] as u64
        + squallar_device_profile::constants::WASM_LOOP_POOL_FLOOR_BYTES as u64;
    assert!(
        page > floor * 2,
        "the handheld page policy ({page} B) leaves less than the wasm bracket's own \
         cache floor ({floor} B) again in working room",
    );
}

/// The `deviceMemory` bucket that lowers a desktop to the handheld pair is the
/// same figure the promotion ladder already uses, written once in Rust. It may
/// only LOWER a policy, and it is absent in Firefox and WebKit.
#[test]
fn the_declared_memory_bucket_is_the_one_the_promotion_ladder_uses() {
    assert_eq!(
        js_const("DECLARED_HANDHELD_BYTES"),
        DECLARED_RAM_HANDHELD_BYTES
    );
}

/// Every reason a policy figure could reach what is constructed, one sentence
/// a defect. Read from the three bootstraps' text: the bodies of
/// `constructMemory` and `initWithHeap`, the worker's bootstrap (which
/// constructs from the page's rung and chooses nothing), and the page's one
/// construction call. Names that meant "what gets constructed" before the split
/// must be gone, so a reader written against them fails instead of reading a
/// policy.
fn policy_reaches_construction(heap: &str, index: &str, worker: &str) -> Vec<String> {
    const POLICY_WORDS: [&str; 7] = [
        "POLICY_",
        "policyHeapBytes",
        "choosePolicyHeapBytes",
        "classifyFormFactor",
        "pageSignals",
        "matchMedia",
        "DECLARED_HANDHELD_BYTES",
    ];
    let mut defects = Vec::new();
    for (name, open) in [
        ("constructMemory", "export function constructMemory("),
        ("initWithHeap", "export async function initWithHeap("),
    ] {
        let Some(at) = heap.find(open) else {
            defects.push(format!("heap.js no longer has `{open}`"));
            continue;
        };
        let body = &heap[at..];
        let body = &body[..body.find("\n}\n").map_or(body.len(), |end| end + 3)];
        for word in POLICY_WORDS {
            if body.contains(word) {
                defects.push(format!(
                    "heap.js `{name}` reads `{word}` -- a policy figure reaches what is constructed"
                ));
            }
        }
    }
    for word in POLICY_WORDS {
        if worker.contains(word) {
            defects.push(format!(
                "worker.js reads `{word}` -- the worker constructs from the page's rung and chooses nothing"
            ));
        }
    }
    let calls: Vec<&str> = index
        .match_indices("initWithHeap(")
        .map(|(at, _)| index[at..].split(')').next().unwrap_or(""))
        .collect();
    if calls != ["initWithHeap(init, LADDER_BYTES[0]"] {
        defects.push(format!(
            "index.html constructs with {calls:?} -- not the top rung alone"
        ));
    }
    // As calls and exports, not bare words: heap.js's comment records the
    // rename by spelling the old names, and a gate that tripped on its own
    // history would be deleted rather than kept.
    for gone in [
        "heapMaxBytes(",
        "chooseHeapMaxBytes(",
        "DESKTOP_PAGE_BYTES",
        "export const HANDHELD_PAGE_BYTES",
        "export const HANDHELD_WORKER_BYTES",
    ] {
        for (file, text) in [
            ("heap.js", heap),
            ("index.html", index),
            ("worker.js", worker),
        ] {
            if text.contains(gone) {
                defects.push(format!(
                    "{file} still spells `{gone}` -- a name that meant what gets constructed"
                ));
            }
        }
    }
    defects
}

/// **No policy figure selects what is constructed**, and a doctored page or
/// heap.js that let one is caught.
#[test]
fn no_policy_figure_selects_what_is_constructed() {
    assert_eq!(
        policy_reaches_construction(HEAP_JS, INDEX_HTML, WORKER_JS),
        Vec::<String>::new()
    );
    let doctored = INDEX_HTML.replacen(
        "initWithHeap(init, LADDER_BYTES[0])",
        "initWithHeap(init, policy.page)",
        1,
    );
    assert_ne!(doctored, INDEX_HTML, "the tamper did not apply");
    assert!(
        policy_reaches_construction(HEAP_JS, &doctored, WORKER_JS)
            .iter()
            .any(|d| d.contains("index.html constructs")),
        "a page constructing at its policy figure went unnoticed",
    );
    let doctored = HEAP_JS.replacen(
        "const top = startBytes > 0 ? startBytes : LADDER_BYTES[0];",
        "const top = POLICY_DESKTOP_BYTES;",
        1,
    );
    assert_ne!(doctored, HEAP_JS, "the tamper did not apply");
    assert!(
        policy_reaches_construction(&doctored, INDEX_HTML, WORKER_JS)
            .iter()
            .any(|d| d.contains("`initWithHeap` reads `POLICY_`")),
        "an initWithHeap walking from a policy figure went unnoticed",
    );
    assert_eq!(
        HEAP_JS.matches("new WebAssembly.Memory(").count(),
        1,
        "heap.js constructs a memory somewhere other than `constructMemory`",
    );
}

/// **Where each walk starts.** The page walks from the top rung and hands its
/// answer to `start` as its reservation -- which is also the worker's starting
/// rung -- with the two policy figures beside it; the worker walks from that
/// rung, and from the top only when its `name` carries none. Neither bootstrap
/// states a byte figure of its own.
#[test]
fn the_page_walks_from_the_top_and_the_worker_from_the_pages_answer() {
    for needle in [
        "const pageReserved = await initWithHeap(init, LADDER_BYTES[0]);",
        "start(pageReserved, policy.page, policy.worker);",
    ] {
        assert!(
            INDEX_HTML.contains(needle),
            "index.html no longer carries `{needle}`"
        );
    }
    assert!(
        WORKER_JS.contains("heap.heapFromName(self.name) ?? heap.LADDER_BYTES[0]"),
        "worker.js no longer starts its ladder from the page's rung"
    );
    for (file, text) in [("index.html", INDEX_HTML), ("worker.js", WORKER_JS)] {
        assert!(
            !text.contains("1024 * 1024"),
            "{file} states a byte figure of its own; the rungs belong to heap.js"
        );
    }
}

/// **One formatter for each line the rig reads off an instance's boot.** The
/// ladder line is printed once, on success; the fallback sentence is printed
/// once, when no rung was used. `drive.py` needles both.
#[test]
fn the_ladder_line_and_the_fallback_sentence_each_have_one_formatter() {
    for (needle, count) in [
        ("\"squallar: linear memory ladder: constructed \"", 1),
        ("console.info(ladderLine(rung.bytes, refused));", 1),
        ("\"squallar: could not instantiate with a \"", 1),
    ] {
        assert_eq!(
            HEAP_JS.matches(needle).count(),
            count,
            "heap.js carries `{needle}` a different number of times"
        );
    }
}

/// The `Worker` name the page starts the rasterization worker under is spelled
/// the same in both languages. It is the whole channel the worker's starting
/// rung travels on, and a drift in either half is a worker that silently walks
/// from the top on every device.
#[test]
fn the_worker_name_prefix_is_spelled_the_same_in_both_languages() {
    let rust = include_str!("../src/worker_port.rs");
    let prefix = js_string_const("WORKER_NAME_PREFIX");
    assert!(
        rust.contains(&format!("const WORKER_NAME_PREFIX: &str = \"{prefix}\";")),
        "heap.js starts workers under `{prefix}` and worker_port.rs does not \
         name them that",
    );
}

/// **`heap.js` states no `initial` figure; it reads one off the module.**
///
/// The file held `INITIAL_PAGES = 65`, a hand copy of the generated glue's
/// own default, and the module came to declare 66: every page and worker
/// then instantiated through the `LinkError` fallback, at the declared
/// bound instead of the chosen one, with one `warn` nobody read. A
/// figure read off the module cannot drift from it, so what is pinned is
/// that the constant is gone and the reading is what `initWithHeap` uses:
/// the only `initial:` in the file is the parsed one, and the parse is over
/// a clone of the same `Response` the glue then streams -- one download.
///
/// The reading itself is exercised in `tests/heap.test.mjs`, under
/// `sw_behaviour.rs`, over a module built byte by byte.
#[test]
fn the_initial_pages_are_read_off_the_module_and_stated_nowhere() {
    assert!(
        !HEAP_JS.contains("export const INITIAL_PAGES"),
        "heap.js states the module's memory minimum again; it drifted the \
         last time and every instance took the fallback",
    );
    let initials: Vec<&str> = HEAP_JS
        .match_indices("initial:")
        .map(|(at, _)| {
            HEAP_JS[at + "initial:".len()..]
                .split(',')
                .next()
                .expect("an `initial:` is followed by a value")
                .trim()
        })
        .collect();
    assert_eq!(
        initials,
        vec!["pages"],
        "every `initial:` in heap.js must be the parsed minimum (`pages`), \
         never a literal; found {initials:?}",
    );
    for needle in [
        "export function readDeclaredMinimum(",
        "export async function declaredMinimumPages(",
        "const pages = await declaredMinimumPages(response.clone());",
        "module_or_path: response,",
    ] {
        assert!(
            HEAP_JS.contains(needle),
            "heap.js no longer carries `{needle}`: the reading, or the single \
             fetch it shares with the glue, is gone",
        );
    }
}

/// The module `heap.js` fetches to read its minimum from is the module the
/// glue instantiates and the one `sw.js` precaches -- one spelling, held
/// against the shell list, so a renamed bundle cannot leave the reading
/// fetching a 404 while the glue streams the real file.
#[test]
fn the_module_path_heap_js_reads_is_the_one_the_shell_precaches() {
    let path = js_string_const("MODULE_PATH");
    let relative = path
        .strip_prefix("./")
        .unwrap_or_else(|| panic!("MODULE_PATH {path:?} is not spelled relative to heap.js"));
    let sw = include_str!("../sw.js");
    assert!(
        sw.contains(&format!("\"{relative}\"")),
        "heap.js reads {relative:?} and sw.js does not precache it",
    );
    assert!(
        relative.ends_with("_bg.wasm"),
        "{relative:?} is not a wasm-pack module"
    );
}

/// The parser can disagree. A name that is absent panics, and a product is
/// really multiplied rather than read as its first term.
#[test]
fn the_js_constant_reader_reads_what_is_written() {
    assert_eq!(js_const("PAGE_BYTES"), 65536);
    assert_eq!(js_const("HEADER_READ_CAP_BYTES"), 4 * 1024 * 1024);
    assert!(
        std::panic::catch_unwind(|| js_const("NO_SUCH_CONSTANT_IN_HEAP_JS")).is_err(),
        "an absent constant read as something instead of failing",
    );
}
