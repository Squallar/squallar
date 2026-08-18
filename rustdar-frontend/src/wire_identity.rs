//! The wire's pinned identity, as production data: the framing rows the
//! `offload::tests` digests assert live here, so the build token can read
//! them.
//!
//! # What the digest is
//!
//! [`wire_digest`] is the local-dev half of the page/worker build token
//! (`rustdar_web::worker_protocol::build_token`). In CI the token carries
//! `GITHUB_SHA`, which distinguishes two deploys finer than any hand-kept
//! number ever did; locally there is no SHA, and this digest stands in. It
//! folds, in order, every job-tag `(name, byte)` pair and then every row of
//! [`WIRE_FRAMING_ROWS`] and [`WIRE_REPLY_ROWS`] — the same literals the
//! framing tests pin — so the two halves of one build are equal by
//! construction (same module, same constants), and two local builds diverge
//! exactly when a re-pinned row or tag differs between them. A divergent pair
//! refuses the handshake and respawns instead of exchanging bytes one of them
//! misreads.
//!
//! # What it deliberately does not cover (yet)
//!
//! The nested payload layouts — `RenderInput`, the polar field, the decoded
//! volume — are pinned by `rustdar-radar`'s own digest suites and do not feed
//! this one. A local worker stale in a way that moved none of these rows (a
//! rasterizer-only change, a nested-layout change) still reads as the same
//! build: a missed detection, accepted, and exactly the status quo before
//! this module existed — locally there is no service-worker deploy skew to
//! create such a pair, and production always has the SHA.
//!
//! # Interim form
//!
//! This digests the current `TAG_*` constants and the fixture rows. **M7b
//! re-bases it onto the composed `JobCodec` registry** once that registry
//! exists (M6/M7): the source of the folded identity moves, the mechanism
//! stays.

/// The 14 job-framing rows, exactly as
/// `offload::tests::the_job_framing_is_the_one_this_protocol_ships` asserts
/// them, in test order: `kind | framed-prefix length | FNV-1a-64 digest`.
///
/// The literal list lives HERE — spelled out, never regenerated from the
/// encoder — and the test points at it. A framing change goes red against
/// these rows, and re-pinning a row changes [`wire_digest`], which changes
/// the local build token: two local builds with different rows refuse each
/// other and respawn, and in CI the `GITHUB_SHA` does the same for every
/// change.
pub const WIRE_FRAMING_ROWS: &[&str] = &[
    "radar | 6 | 0x7d65cbf16a7b2ab7",
    "level3 | 28 | 0xff9d9dbb7736ea4e",
    "level3/vild | 34 | 0x4f9fb6a0901a3704",
    "section | 44 | 0x4fe286e53034800e",
    "voxels | 59 | 0x625ac8a3330ec11f",
    "voxels | 43 | 0xdbe9f7d560c79fd4",
    "decode | 30 | 0x2aad0634cde46e59",
    "overlay/sites | 131 | 0xf24942f4dd119e16",
    "overlay/alerts | 760 | 0xa89f5057b3b51a4a",
    "overlay/outlooks | 557 | 0x62486e4e14434bb0",
    "overlay/discussions | 264 | 0x3d28f1976f832a20",
    "overlay/reports | 127 | 0x533f15051c5cc607",
    "overlay/glm | 240 | 0xca1519d51136a8ef",
    "overlay/model | 262 | 0x0133cf4f818a1a23",
];

/// The 2 overlay-reply framing rows, exactly as
/// `offload::tests::the_overlay_reply_framing_is_the_one_this_protocol_ships`
/// asserts them: the reply direction's only framed layout, folded into the
/// token beside the request rows — same suite, same mechanism, strictly
/// better local detection.
pub const WIRE_REPLY_ROWS: &[&str] = &[
    "bare | 17 | 0x770d1b313226dd5f",
    "cells | 69 | 0x6637c90fa10e397a",
];

/// FNV-1a 64, continued from `hash`. A copy of the house hash rather than a
/// call to `rustdar_radar::wire::layout_digest`: that one is `#[cfg(test)]`,
/// so it does not exist outside its own crate, and copying a *hash* is
/// sanctioned — the thing that must not be duplicated is an encoder, because
/// a second encoder has to be kept in step; a second FNV-1a either agrees
/// with the first or is obviously broken. Private: the digest is the surface,
/// the hash is not.
fn fnv1a64(mut hash: u64, bytes: &[u8]) -> u64 {
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// The wire's identity as one number: FNV-1a 64 over, in order, each
/// `(name, byte)` of the job-tag table, then every row of
/// [`WIRE_FRAMING_ROWS`], then every row of [`WIRE_REPLY_ROWS`].
///
/// Equal between the two halves of one build by construction; divergent
/// between two builds exactly when a pinned tag or row differs. See the
/// module doc for what deliberately does not feed it.
pub fn wire_digest() -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for (name, byte) in crate::offload::wire_tags() {
        hash = fnv1a64(hash, name.as_bytes());
        hash = fnv1a64(hash, &[byte]);
    }
    for row in WIRE_FRAMING_ROWS.iter().chain(WIRE_REPLY_ROWS) {
        hash = fnv1a64(hash, row.as_bytes());
    }
    hash
}
