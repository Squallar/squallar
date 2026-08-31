// LOCAL CHANGE: upstream selects between `OUT_DIR`'s freshly generated
// bindings and the committed ones with `#[cfg(feature = "protoc")]`. The
// `protoc` feature and the `build.rs` that produced the former are gone (see
// Cargo.toml and VENDORED.md), so the committed bindings are the only arm and
// the cfg would be an `unexpected_cfgs` warning on every build.
include!("generated/vector_tile.rs");
