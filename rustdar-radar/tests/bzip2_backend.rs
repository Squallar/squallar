//! No C libbzip2 in this workspace, and no `bzip2` on the Level II decode path,
//! enforced by reading the lockfile and the manifests rather than by trusting a
//! comment.
//!
//! # The hazard
//!
//! The `bzip2` crate picks its backend with a **feature**, not with a
//! dependency:
//!
//! ```text
//! bzip2-0.6.1/src/lib.rs:59   #[cfg(feature = "bzip2-sys")]      -> C libbzip2
//! bzip2-0.6.1/src/lib.rs:61   #[cfg(not(feature = "bzip2-sys"))] -> libbz2-rs-sys
//! ```
//!
//! Under `resolver = "2"` features are unified across everything that resolves
//! the same package for the same target. So *one* crate anywhere in this
//! workspace — or anywhere in the transitive graph, or one `--features`
//! flag on a command line — asking for `bzip2/bzip2-sys` switches **every**
//! user of `bzip2` to the C library at once. `nexrad-level3` is the remaining
//! user, and it is compiled for `wasm32-unknown-unknown` on the web row, where
//! `bzip2-sys` builds a C archive and cannot link.
//!
//! On desktop that switch is silent: the build succeeds, the binary grows a C
//! toolchain requirement, and nothing says so. That is what this file is for.
//!
//! # Why a lockfile scan
//!
//! `bzip2-sys` is the fingerprint and it cannot be faked: the feature is
//! `bzip2-sys = ["dep:bzip2-sys"]`, so enabling it is exactly what puts the
//! package in `Cargo.lock`, and the package is in the lockfile *only* if
//! something enabled it. A `cargo tree` shell-out would be a slower way to
//! learn the same thing and would need a cargo on `PATH` inside a test.
//!
//! The scan is not a substitute for the wasm CI row, which would also fail —
//! it is the row that fails *first*, on every developer machine, naming the
//! cause.
#![cfg(not(target_arch = "wasm32"))]

use std::path::PathBuf;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("rustdar-radar sits one level under the workspace root")
        .to_path_buf()
}

#[test]
fn no_c_libbzip2_resolves_anywhere_in_the_workspace() {
    let lock = std::fs::read_to_string(workspace_root().join("Cargo.lock"))
        .expect("the workspace lockfile is next to the root manifest");

    let offenders: Vec<&str> = lock
        .lines()
        .map(str::trim)
        .filter(|l| *l == r#"name = "bzip2-sys""# || *l == r#"name = "libbz2-sys""#)
        .collect();

    assert!(
        offenders.is_empty(),
        "Cargo.lock resolves a C libbzip2 ({offenders:?}).\n\
         Something has enabled the `bzip2/bzip2-sys` feature. Because cargo \
         unifies features across the whole graph, that switches *every* user of \
         `bzip2` in this workspace from the pure-Rust `libbz2-rs-sys` backend to \
         a C archive -- silently on desktop, and fatally on the \
         wasm32-unknown-unknown row, which cannot link one.\n\
         Find it with `cargo tree --workspace --all-features -i bzip2-sys` and \
         remove the feature rather than removing this test."
    );
}

#[test]
fn the_level_two_decode_path_does_not_reach_bzip2_at_all() {
    // `vendor/nexrad-data`'s `Record::decompress` is 98-99% of the instructions
    // a Level II volume decode retires -- counted, not sampled; see
    // `crate::scan::decoded` -- and runs `bzip2-rs`, a decode-only pure-Rust
    // crate that has no backend feature and so cannot participate in the
    // unification above. `bzip2` is
    // kept only as a **dev**-dependency, for the `BzEncoder` that the ceiling
    // tests build their fixtures with -- dev-dependencies are resolved for test
    // and bench targets alone, so they reach no shipped binary.
    //
    // This pins the arrangement, because the failure mode of losing it is
    // invisible: moving that line back up to `[dependencies]` compiles, passes
    // every other test, and quietly puts libbz2-rs-sys back into the wasm
    // bundle and back on the decode path.
    let manifest = std::fs::read_to_string(
        workspace_root()
            .join("vendor")
            .join("nexrad-data")
            .join("Cargo.toml"),
    )
    .expect("the vendored nexrad-data manifest");

    assert!(
        manifest.contains("[dependencies.bzip2-rs]"),
        "vendor/nexrad-data must decompress LDM records with bzip2-rs"
    );
    assert!(
        !manifest.contains("[dependencies.bzip2]"),
        "vendor/nexrad-data has `bzip2` back as a normal dependency. It belongs \
         under [dev-dependencies]: the decode path runs bzip2-rs, and the only \
         thing that still needs `bzip2` is the BzEncoder the tests compress \
         their fixtures with. See vendor/bzip2-rs/VENDORED.md."
    );
    assert!(
        manifest.contains("[dev-dependencies.bzip2]"),
        "vendor/nexrad-data's ceiling tests compress their own fixtures and need \
         `bzip2` as a dev-dependency; bzip2-rs is decode-only"
    );
}
