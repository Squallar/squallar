//! The wasm heap ceilings — the one the module is LINKED with, and the ones a
//! device is actually given underneath it.
//!
//! # What changed, and why the pin below is not the pin it used to be
//!
//! This file used to hold one equality: `WASM_LINEAR_MEMORY_MAX_BYTES` is the
//! `--max-memory` in `.github/scripts/wasm-threads.sh`, "a build constant" that
//! "no browser and no device moves". The equality still holds and is still
//! checked. **What is no longer true is the reading of it.** The link flag is
//! what the module's memory import DECLARES, and a supplied
//! `WebAssembly.Memory` matches that import at any maximum **at or below** it:
//! 54 cells plus negative controls across Firefox and Chromium (2026-09-03)
//! instantiate for `supplied <= declared` and raise `LinkError: imported
//! Memory with incompatible maximum size` for `supplied > declared`. So the
//! flag is a validation bound with room underneath, and `squallar-web/heap.js`
//! chooses per device inside that room before the module is instantiated.
//!
//! The flag is therefore pinned here as a **ceiling on every per-device
//! figure** rather than as the single value the application judges against.
//! Raising it is still a measurement someone has to make on a device, and it
//! is a strictly harder one than lowering: the reservation results behind this
//! are all x86-64 Linux desktop, and a 32-bit browser process cannot make a
//! 4 GiB `PROT_NONE` reservation and uses bounds-checked memory instead.
//! **Shrinking needs no such leg; growing does.**
//!
//! # Why the figures are pinned across a language boundary
//!
//! The choice has to run before any Rust does — the maximum of a `shared`
//! memory is fixed at construction, and `WebAssembly.Memory.prototype.type()`
//! exists in neither engine, so nothing inside the module can read it back.
//! That puts the policy in JavaScript, in another directory, in a file no
//! Rust test can execute. What CAN be held is that the two files state the
//! same numbers, which is what this does: it reads `heap.js` at compile time
//! and holds every figure in it against the Rust side. A moved or deleted
//! file is a build failure here, not a skipped test.
#![cfg(not(target_arch = "wasm32"))]

use squallar_device_profile::constants::{
    DECLARED_RAM_HANDHELD_BYTES, WASM_LINEAR_MEMORY_MAX_BYTES,
};

const SCRIPT: &str = include_str!("../../.github/scripts/wasm-threads.sh");
const HEAP_JS: &str = include_str!("../heap.js");

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
    expr.split('*')
        .map(|term| {
            term.trim().parse::<u64>().unwrap_or_else(|_| {
                panic!("`{name}` in heap.js is `{expr}`, not a product of integer literals")
            })
        })
        .product()
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

/// The link flag is stated exactly once, and it states the constant.
#[test]
fn the_linear_memory_ceiling_is_the_link_flag() {
    let flags = max_memory_flags(SCRIPT);
    assert_eq!(
        flags.len(),
        1,
        "the presence control: `--max-memory=<bytes>` should appear exactly once \
         in wasm-threads.sh, so that one occurrence is the build's ceiling; \
         found {flags:?}",
    );
    assert_eq!(
        flags[0], WASM_LINEAR_MEMORY_MAX_BYTES,
        "the linker's ceiling and the budget system's constant have parted: \
         raising `--max-memory` is a measurement someone has to make on a \
         device, and the constant follows the flag, never the other way round",
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

/// **Every per-device figure sits at or below the link flag, and the desktop
/// figure IS the link flag.**
///
/// The first half is the import rule: a supplied maximum above the module's
/// declared maximum is a `LinkError` and the page does not boot. The second
/// half is the product rule — a desktop gets the full declared bound, because
/// the whole point of choosing per device is that the small end gets a small
/// wall while the large end loses nothing.
#[test]
fn no_per_device_ceiling_is_above_the_link_flag() {
    let desktop = js_const("DESKTOP_PAGE_BYTES");
    assert_eq!(
        desktop, WASM_LINEAR_MEMORY_MAX_BYTES,
        "heap.js's desktop ceiling is not the bound the module is linked with: \
         a desktop is supposed to lose nothing to this mechanism",
    );
    for name in ["HANDHELD_PAGE_BYTES", "HANDHELD_WORKER_BYTES"] {
        let bytes = js_const(name);
        assert!(
            bytes <= WASM_LINEAR_MEMORY_MAX_BYTES,
            "heap.js's {name} ({bytes} B) is above the module's declared \
             maximum ({WASM_LINEAR_MEMORY_MAX_BYTES} B). The engine refuses a \
             supplied memory above the declared one outright -- this is a page \
             that does not boot, not a page that runs large",
        );
        assert_eq!(
            bytes % js_const("PAGE_BYTES"),
            0,
            "{name} is not a whole number of wasm pages, and a maximum is \
             declared in pages",
        );
    }
}

/// **The two instances are chosen separately, and on a handheld they differ.**
///
/// The page holds the caches — the wasm bracket's own floor is 128 MiB of
/// tile host ceiling plus 56 MiB of loop pool — and the worker holds only the
/// jobs in flight. Pinning `worker < page` is what keeps a later edit from
/// collapsing the pair back into one number and calling it a simplification;
/// pinning the page well above the floor sum is what keeps it from being set
/// so low that the watermark sits permanently in `Act` with nothing to shed.
#[test]
fn the_handheld_page_and_worker_are_different_figures_and_the_page_clears_its_floor() {
    let page = js_const("HANDHELD_PAGE_BYTES");
    let worker = js_const("HANDHELD_WORKER_BYTES");
    assert!(
        worker < page,
        "the handheld worker ceiling ({worker} B) is not below the page's \
         ({page} B): the page holds the tile caches and the loop pool, the \
         worker holds the jobs in flight",
    );
    assert!(
        page < WASM_LINEAR_MEMORY_MAX_BYTES,
        "the handheld page ceiling is the desktop's: nothing was chosen",
    );
    let floor = squallar_device_profile::constants::WASM_TILE_HOST_CEILING_BYTES[0] as u64
        + squallar_device_profile::constants::WASM_LOOP_POOL_FLOOR_BYTES as u64;
    assert!(
        page > floor * 2,
        "the handheld page ceiling ({page} B) leaves less than the wasm \
         bracket's own cache floor ({floor} B) again in working room; the \
         watermark would act on every tick with nothing to shed",
    );
}

/// The `deviceMemory` bucket that lowers a desktop to the handheld pair is the
/// same figure the promotion ladder already uses, written once in Rust.
///
/// It is a hint that may only LOWER — there is no arm anywhere that reads a
/// large `deviceMemory` and grows a ceiling — and it is absent in Firefox,
/// which governs, so the pointer rule has to carry the decision on its own.
#[test]
fn the_declared_memory_bucket_is_the_one_the_promotion_ladder_uses() {
    assert_eq!(
        js_const("DECLARED_HANDHELD_BYTES"),
        DECLARED_RAM_HANDHELD_BYTES,
    );
}

/// The `Worker` name the page starts the rasterization worker under is spelled
/// the same in both languages. It is the whole channel the worker's ceiling
/// travels on, and a drift in either half is a worker that silently falls back
/// to the declared bound on every device.
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

/// The parser can disagree. A name that is absent panics, and a product is
/// really multiplied rather than read as its first term.
#[test]
fn the_js_constant_reader_reads_what_is_written() {
    assert_eq!(js_const("PAGE_BYTES"), 65536);
    assert_eq!(js_const("HANDHELD_WORKER_BYTES"), 256 * 1024 * 1024);
    assert!(
        std::panic::catch_unwind(|| js_const("NO_SUCH_CONSTANT_IN_HEAP_JS")).is_err(),
        "an absent constant read as something instead of failing",
    );
}
