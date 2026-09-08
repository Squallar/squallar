//! The page really takes the one-copy route for a picture.
//!
//! `worker_port` is `cfg(target_arch = "wasm32")`, so no host test can call it.
//! What a host test CAN do is read it as text — the shape
//! `transport_line_shape.rs` and `linear_memory_ceiling.rs` already use.
//!
//! **Why this needs its own pin at all.** The codec-side gates in
//! `squallar_worker::offload::tests` prove the picture sits at a constant
//! offset and that the split decode agrees with the whole one. Every one of
//! them stays green if the page simply stops asking for the split: the two
//! buffers come back, the pictures are still correct, and nothing in the tree
//! says a word. That is the failure this file exists for — a property covered
//! on one route and silently abandoned on the other.
#![cfg(not(target_arch = "wasm32"))]

const PORT: &str = include_str!("../src/worker_port.rs");

/// The body of `fn <name>(` up to the line that closes it at the same
/// indentation, so an assertion about one function cannot be satisfied by
/// another.
fn body_of(name: &str) -> &'static str {
    let at = PORT
        .find(name)
        .unwrap_or_else(|| panic!("worker_port.rs no longer defines `{name}`"));
    let rest = &PORT[at..];
    let end = rest
        .find("\n    }\n")
        .unwrap_or_else(|| panic!("`{name}` is unterminated"));
    &rest[..end]
}

/// **The page asks whether the reply is a picture, and routes one when it is.**
#[test]
fn the_page_routes_a_raster_reply_through_the_split_delivery() {
    for call in [
        "offload::reply_is_raster(",
        "offload::deliver_encoded_reply_split(",
        "pull.split(&out)",
    ] {
        assert!(
            PORT.contains(call),
            "worker_port.rs no longer contains {call:?}; the page has stopped \
             taking the one-copy route and every codec-side gate stays green",
        );
    }
}

/// **The picture is copied into pixels, never through a `Vec<u8>` of its own
/// size.** `to_vec` is the two-buffer spelling and belongs only to `whole`.
#[test]
fn the_split_pull_never_materializes_the_picture_as_bytes() {
    let split = body_of("fn split(");
    assert!(
        !split.contains("to_vec()"),
        "`Pull::split` calls `to_vec()`, which allocates a `Vec<u8>` the size \
         of the picture — the buffer this whole path exists to not allocate: \
         {split}",
    );
    for spelling in ["RasterBuf::transparent(", "copy_to("] {
        assert!(
            split.contains(spelling),
            "`Pull::split` no longer contains {spelling:?}, so it is not \
             copying the worker's bytes into an aligned pixel destination",
        );
    }
    // The control: the whole-head route is the one that may say `to_vec`, so a
    // tree where NEITHER says it is a tree where this pin stopped meaning
    // anything.
    assert!(
        body_of("fn whole(").contains("to_vec()"),
        "neither pull route calls `to_vec()`; this pin is asserting the \
         absence of a spelling the file no longer has anywhere",
    );
}
