//! The wire's pinned identity, as production data: the framing rows the
//! `offload::tests` digests assert live here, so the build token can read
//! them.
//!
//! [`wire_digest`] is the local-dev half of the page/worker build token
//! (`squallar_web::worker_protocol::build_token`); in CI the token carries
//! `GITHUB_SHA` instead. Two local builds diverge exactly when a re-pinned
//! row, the registry composition, or the envelope shape differs, and a
//! divergent pair respawns rather than exchanging bytes one of them
//! misreads. Nested payload layouts (`RenderInput`, the polar field, the
//! decoded volume) are pinned by `squallar-radar`'s own digest suites and do
//! not feed this one.

/// The 16 job-framing rows, exactly as
/// `offload::tests::the_job_framing_is_the_one_this_protocol_ships` asserts
/// them, in test order: `kind | framed-prefix length | FNV-1a-64 digest`.
/// Sixteen rows for **fifteen** kinds: `voxels` contributes two — the
/// picked-region and sourceless forms frame differently.
///
/// The literal list lives HERE, never regenerated from the encoder; the test
/// points at it. Re-pinning a row changes [`wire_digest`] and so the local
/// build token.
pub const WIRE_FRAMING_ROWS: &[&str] = &[
    "radar | 46 | 0x813f26d9407ac047",
    "level3 | 68 | 0xae8be80f6b6cf96e",
    "level3/vild | 74 | 0xad0639c99f05f4e7",
    "section | 88 | 0xe8e95369569f391f",
    "voxels | 103 | 0x7992be29197ec332",
    "voxels | 87 | 0xb6df319c59c9e0a5",
    "decode | 74 | 0xc06cc21eea05e948",
    // Re-pinned when the site layer split: row 7 was `overlay/sites | 134`,
    // whose input carried a zoom, a theme flag and, per station, a name and two
    // role bools. The markers, the names and the selected station's ring are
    // screen-space per-frame painting now, so none of that crosses the wire;
    // what is left is the network's coverage, and `CoverageSite` is a position
    // and nothing else. The row keeps index 7 deliberately — codes are assigned
    // by position, so moving it would renumber every row after it.
    "overlay/coverage | 101 | 0xe9757285d7ff6422",
    "overlay/alerts | 763 | 0x9a307969466bc79a",
    "overlay/outlooks | 560 | 0x01fc75ae56a219d4",
    "overlay/discussions | 267 | 0x6a6da1ea1f7fc09c",
    // Re-pinned at WB-2: `ReportsInput` gained `as_of` and per-row `valid`,
    // the depicted instant and each report's own — the storm-reports as-of
    // cull rides the wire.
    "overlay/reports | 200 | 0x8b8cebdb5011dbc0",
    "overlay/glm | 243 | 0xf628cffc30d313df",
    "overlay/model | 265 | 0x01f0be29a2ccb24f",
    // The height row, chained last so no code before it is renumbered. Its
    // whole payload is framing: the tile bodies are opaque PNGs this codec
    // frames and never interprets, the same ruling `framing_of` gives an
    // archive and a Level III object.
    // Row-length arithmetic, independent of the encoder: 1 code byte + 44
    // envelope + 81 prefix (6xf64 box + 2xu32 posts + u8 zoom + 5xu32 cover +
    // u32 count) + one tile of 12 header + 268 body = 406.
    // Re-pinned once, deliberately, before this row ever shipped: the fixture
    // box was a square, so a symmetric reorder of the six box terms in `encode`
    // and `decode` left these bytes identical and this row could not see it.
    // `x_km` and `y_km` now differ. The length is unchanged at 406 -- the field
    // widths did not move, only the values that make the swap visible.
    "terrain/heights | 406 | 0xc776d09fd53b08e0",
    "buildings/prisms | 239 | 0x607da6bb1409aaa8",
];

/// The 2 overlay-reply framing rows, exactly as
/// `offload::tests::the_overlay_reply_framing_is_the_one_this_protocol_ships`
/// asserts them, folded into the token beside the request rows.
pub const WIRE_REPLY_ROWS: &[&str] = &[
    "bare | 17 | 0x770d1b313226dd5f",
    "cells | 69 | 0x6637c90fa10e397a",
];

/// The 6 frame-reply framing rows, exactly as
/// `offload::tests::the_frame_reply_framing_is_the_one_this_registry_ships`
/// asserts them: the frame's head+tails wire form
/// (`squallar_radar::frame::RenderedFrame::write_head` + the nominated
/// `[polar, image]` tails) — head, polar tail and image tail for one fixture
/// with all three optional trios present and one with none. The per-tail
/// rows also pin the tail ORDER.
///
/// Row-length arithmetic (independent of the encoder): head/full
/// 8 + (1+8) + (1+1) + (1+1+4+4) = 29; head/bare 8+1+1+1 = 11; polar/full
/// 80 = 16 header + 3x8 + 2x8 + 6x4; polar/bare 40 = 16 header + 3x8
/// (the default field); image 8/4 = the fixture Vecs.
pub const WIRE_FRAME_REPLY_ROWS: &[&str] = &[
    "frame/full/head | 29 | 0x89dc568e54cf7abb",
    "frame/full/polar | 80 | 0x9f0c3f4e5dce8435",
    "frame/full/image | 8 | 0x0363b2a3926bce45",
    "frame/bare/head | 11 | 0xc813d3185b023723",
    "frame/bare/polar | 40 | 0x3f3ecf0cef9be2c0",
    "frame/bare/image | 4 | 0xbe7a5e775165785d",
];

/// The 2 height-reply framing rows, exactly as
/// `offload::tests::the_height_reply_framing_is_the_one_this_registry_ships`
/// asserts them: the `terrain/heights` reply's head+tail wire form, over a
/// **literal** [`squallar_elevation::HeightField`] rather than a resampled one,
/// so the digest is over stored bits and not over whatever libm answered.
///
/// **This list exists because review found the reply layout pinned nowhere.**
/// The request side had the framing row; the reply had only a length assertion,
/// so field order and endianness were free — samples written big-endian and read
/// big-endian survived the whole suite, as did a head writing `y_km` before
/// `x_km`. `every_codec_row_has_a_parity_test` cannot catch either by
/// construction: the direct arm and the via-wire arm run the same codec, so a
/// symmetric change is invisible to it. Every other reply family in the tree
/// already had such a list; this row was the exception.
///
/// Row-length arithmetic (independent of the encoder): head
/// 6x8 (box) + 2x4 (posts) = 56; samples 5 x 3 posts x 2 bytes = 30.
pub const WIRE_HEIGHT_REPLY_ROWS: &[&str] = &[
    "heights/head | 56 | 0xdb55a1bbf0c427c2",
    "heights/samples | 30 | 0xd62e344d457129b5",
];

/// The 4 buildings-reply framing rows, exactly as
/// `offload::tests::the_building_reply_framing_is_the_one_this_registry_ships`
/// asserts them: the `buildings/prisms` reply's head and three tails, over a
/// **literal** `squallar_buildings::prism::BuildingMesh` rather than an
/// extruded one, so the digest is over stored bits and not over whatever libm
/// and lyon answered.
///
/// **This row has a symmetry the height row does not**, which is why it is
/// pinned per tail rather than as a total: positions and normals are the same
/// length, so a build that swapped the two tails would move no length
/// anywhere and the parity test -- which runs the same codec on both arms --
/// could not see it either.
///
/// Row-length arithmetic (independent of the encoder): head 5x4 = 20;
/// positions and normals 3 vertices x 3 axes x 4 = 36 each; indices 3 x 4 =
/// 12.
pub const WIRE_BUILDING_REPLY_ROWS: &[&str] = &[
    "prisms/head | 20 | 0x3412020b8d2915c5",
    "prisms/positions | 36 | 0x2c6e36aef59d52d0",
    "prisms/normals | 36 | 0x1048b47ceb2b9eb8",
    "prisms/indices | 12 | 0x9ef40c127c771966",
];

/// The canonical envelope's layout as one literal sentence, folded into
/// [`wire_digest`] so a reshaped envelope changes the local build token.
/// The code byte ahead of the envelope is covered by the per-row index
/// fold; this names the 44 bytes after it, in wire order.
pub const CANONICAL_ENVELOPE_LAYOUT: &str = "w:u32 h:u32 bounds:4xf64 ceil:u32";

/// FNV-1a 64, continued from `hash`. A copy of the house hash rather than a
/// call to `squallar_radar::wire::layout_digest`, which is `#[cfg(test)]`.
fn fnv1a64(mut hash: u64, bytes: &[u8]) -> u64 {
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// The wire's identity as one number: FNV-1a 64 over, in order, each row of
/// [`WIRE_FRAMING_ROWS`] prefixed by the composed-registry index of the
/// row's kind, then [`CANONICAL_ENVELOPE_LAYOUT`], then every row of
/// [`WIRE_REPLY_ROWS`], then every row of [`WIRE_FRAME_REPLY_ROWS`], then
/// every row of [`WIRE_HEIGHT_REPLY_ROWS`], then every row of
/// [`WIRE_BUILDING_REPLY_ROWS`].
///
/// Panics on a pinned row whose label is not in the registry.
///
/// # The folds are not gated, and that is a live gap
///
/// **Recorded 2026-08-30, found by adversarial review of the buildings row and
/// pre-existing to it.** Every row list above is pinned to what the encoder
/// writes, by its own test. What is *not* pinned is this function's dependence
/// on them: replacing the whole body with a constant, or deleting any one of
/// the four `for` loops below, leaves the worker suite green. Nothing asserts
/// that a moved row moves the digest.
///
/// So the claim these rows are kept for — that a page and a worker of
/// different builds respawn rather than exchange bytes one of them misreads —
/// rests on a fold that no test would notice the removal of. The rows are
/// real; the wiring from rows to token is prose.
///
/// Not fixed here because it is not this unit's defect and a digest test is
/// its own piece of work: it needs to assert that perturbing each list changes
/// the answer, which means a way to perturb them that is not a source edit.
/// Left named rather than left silent.
pub fn wire_digest() -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for row in WIRE_FRAMING_ROWS {
        let label = row
            .split(" | ")
            .next()
            .expect("a pinned row is never empty");
        let index = crate::job_registry::job_codecs()
            .position(|codec| codec.label == label)
            .unwrap_or_else(|| {
                panic!("the pinned framing row {row:?} names no composed-registry kind")
            });
        hash = fnv1a64(hash, &[u8::try_from(index).expect("15 kinds fit a byte")]);
        hash = fnv1a64(hash, row.as_bytes());
    }
    hash = fnv1a64(hash, CANONICAL_ENVELOPE_LAYOUT.as_bytes());
    for row in WIRE_REPLY_ROWS {
        hash = fnv1a64(hash, row.as_bytes());
    }
    for row in WIRE_FRAME_REPLY_ROWS {
        hash = fnv1a64(hash, row.as_bytes());
    }
    for row in WIRE_HEIGHT_REPLY_ROWS {
        hash = fnv1a64(hash, row.as_bytes());
    }
    for row in WIRE_BUILDING_REPLY_ROWS {
        hash = fnv1a64(hash, row.as_bytes());
    }
    hash
}
