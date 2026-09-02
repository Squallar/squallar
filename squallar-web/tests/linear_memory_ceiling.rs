//! The wasm heap ceiling the budget system judges readings against is the one
//! the module is linked with.
//!
//! `WASM_LINEAR_MEMORY_MAX_BYTES` is a build constant: a shared memory has to
//! declare its maximum at link time, and `.github/scripts/wasm-threads.sh` is
//! where this build declares it. The two are one figure written in two
//! languages in two directories, so this reads the script at compile time and
//! holds them equal — a moved or deleted script is a build failure here, not a
//! skipped test.
#![cfg(not(target_arch = "wasm32"))]

use squallar_device_profile::constants::WASM_LINEAR_MEMORY_MAX_BYTES;

const SCRIPT: &str = include_str!("../../.github/scripts/wasm-threads.sh");

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
