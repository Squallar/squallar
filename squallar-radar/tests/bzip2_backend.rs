//! No C libbzip2 in this workspace, and no `bzip2` on the Level II decode path,
//! enforced by reading the lockfile and the manifests rather than by trusting a
//! comment.
#![cfg(not(target_arch = "wasm32"))]

use std::path::PathBuf;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("squallar-radar sits one level under the workspace root")
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
