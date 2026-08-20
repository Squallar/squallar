//! The wire's pinned identity, as production data: the framing rows the
//! `offload::tests` digests assert live here, so the build token can read
//! them.
//!
//! [`wire_digest`] is the local-dev half of the page/worker build token
//! (`rustdar_web::worker_protocol::build_token`); in CI the token carries
//! `GITHUB_SHA` instead. Two local builds diverge exactly when a re-pinned
//! row, the registry composition, or the envelope shape differs, and a
//! divergent pair respawns rather than exchanging bytes one of them
//! misreads. Nested payload layouts (`RenderInput`, the polar field, the
//! decoded volume) are pinned by `rustdar-radar`'s own digest suites and do
//! not feed this one.

/// The 14 job-framing rows, exactly as
/// `offload::tests::the_job_framing_is_the_one_this_protocol_ships` asserts
/// them, in test order: `kind | framed-prefix length | FNV-1a-64 digest`.
/// (`voxels` contributes two rows — the picked-region and sourceless forms
/// frame differently.)
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
    "overlay/sites | 134 | 0x41633a0073cefd5e",
    "overlay/alerts | 763 | 0x9a307969466bc79a",
    "overlay/outlooks | 560 | 0x01fc75ae56a219d4",
    "overlay/discussions | 267 | 0x6a6da1ea1f7fc09c",
    "overlay/reports | 130 | 0x8baff87efe9934bf",
    "overlay/glm | 243 | 0xf628cffc30d313df",
    "overlay/model | 265 | 0x01f0be29a2ccb24f",
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
/// (`rustdar_radar::frame::RenderedFrame::write_head` + the nominated
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

/// The canonical envelope's layout as one literal sentence, folded into
/// [`wire_digest`] so a reshaped envelope changes the local build token.
/// The code byte ahead of the envelope is covered by the per-row index
/// fold; this names the 44 bytes after it, in wire order.
pub const CANONICAL_ENVELOPE_LAYOUT: &str = "w:u32 h:u32 bounds:4xf64 ceil:u32";

/// FNV-1a 64, continued from `hash`. A copy of the house hash rather than a
/// call to `rustdar_radar::wire::layout_digest`, which is `#[cfg(test)]`.
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
/// [`WIRE_REPLY_ROWS`], then every row of [`WIRE_FRAME_REPLY_ROWS`].
///
/// Panics on a pinned row whose label is not in the registry.
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
        hash = fnv1a64(hash, &[u8::try_from(index).expect("13 rows fit a byte")]);
        hash = fnv1a64(hash, row.as_bytes());
    }
    hash = fnv1a64(hash, CANONICAL_ENVELOPE_LAYOUT.as_bytes());
    for row in WIRE_REPLY_ROWS {
        hash = fnv1a64(hash, row.as_bytes());
    }
    for row in WIRE_FRAME_REPLY_ROWS {
        hash = fnv1a64(hash, row.as_bytes());
    }
    hash
}
