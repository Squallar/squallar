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

/// The 17 job-framing rows, exactly as
/// `offload::tests::the_job_framing_is_the_one_this_protocol_ships` asserts
/// them, in test order: `kind | framed-prefix length | FNV-1a-64 digest`.
/// Seventeen rows for **sixteen** kinds: `voxels` contributes two — the
/// picked-region and sourceless forms frame differently.
///
/// The literal list lives HERE, never regenerated from the encoder; the test
/// points at it. Re-pinning a row changes [`wire_digest`] and so the local
/// build token.
pub const WIRE_FRAMING_ROWS: &[&str] = &[
    // Re-pinned when the plan-view request learned to state which surface the
    // caller can draw (`jobs::PlanSurface`). The row grew by that one byte and
    // by nothing else, and the arithmetic is checkable without the encoder:
    // the surface byte is written LAST in the framed prefix — behind
    // `values_wanted`, in front of the nested `RenderInput` that `framing_of`
    // strips — so the new digest is the old one folded with a single `0x00`,
    // which is `PlanSurface::Raster`'s code and what the fixture carries.
    // `0x813f26d9407ac047 * 0x100000001b3 mod 2^64 == 0x190f4a289094b8a5`,
    // exactly what the encoder now posts. Every other row was byte-identical
    // in this change; a move in one of those would have been a bug in it.
    "radar | 47 | 0x190f4a289094b8a5",
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
    "overlay/model | 266 | 0xcdf1f34fa989cc4b",
    // Added when METAR left the frame thread: the station geometry — wind
    // barbs, cloud cover, weather symbols — is a picture now, and this row is
    // the twelve observation fields the drawing reads. It lands at the end of
    // the overlays chunk, so every row after it is renumbered by one; those
    // three digests below moved for that reason and no other.
    "overlay/metar | 402 | 0x88f409803fc25408",
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
    "terrain/heights | 406 | 0x7d1d06a4e408f2e5",
    "buildings/prisms | 239 | 0xf8883249f73c1bb5",
    // The basemap row, chained last so no code before it is renumbered.
    // Its whole payload is framing too: the tile bodies are opaque MVT this
    // codec frames and never interprets, the same ruling `framing_of` gives an
    // archive, a Level III object and a terrain PNG.
    // Row-length arithmetic, independent of the encoder: 1 code byte + 44
    // envelope + 1 theme u8 + 4 disabled count + 2 names (4 + 8 "building",
    // 4 + 3 "poi") + 4 tile count + one tile of 13 header + 113 body = 199.
    "basemap/tiles | 199 | 0x5e52dbc4796f3a91",
];

/// The 3 overlay-reply framing rows, exactly as
/// `offload::tests::the_overlay_reply_framing_is_the_one_this_protocol_ships`
/// asserts them, folded into the token beside the request rows.
///
/// Re-pinned 2026-09-04 when a raster with no ink in it stopped putting its
/// pixels on the wire. The reply gained a **pixels tag** — one byte — so the
/// two painted rows each moved by one byte and both digests with them. The
/// third row is the form that change added: `blank` is a whole picture's
/// answer in 6 bytes, against the 17 the same 16-byte fixture costs painted.
/// At the sizes measured on the Tier-2 legs that is 8.26 MB (Chromium) and
/// 8.92 MB (Firefox) of pixels not sent, per blank reply — two targets, never
/// added.
///
/// **Re-pinned again 2026-09-08, and this one MOVED BYTES ON PURPOSE.** The
/// pixels block was hoisted in front of the cells block and given an explicit
/// `u32` length, so the picture now starts at a constant offset of five and the
/// browser transport can copy it straight into the `Vec<Color32>` its consumer
/// keeps instead of through a `Vec<u8>` of its own size — two full-size buffers
/// live at once, measured at 75.4 MiB for one picture, of which this removes
/// one. Both painted rows grew by exactly the four bytes of that length; the
/// blank row is unchanged at 6, because a blank already wrote a tag and a `u32`
/// and only their order moved. **Re-pinning here is the sanctioned response to
/// a deliberate framing change and nothing else**: the row feeds the local
/// build token, so two builds with different framings refuse each other and
/// respawn rather than exchanging a reply either would misread.
///
/// **Re-pinned a third time, later on 2026-09-08, and ONLY the `blank` row
/// moved** — `bare` and `cells` are byte-identical, which is the evidence the
/// change is confined to the arm it was meant for and did not disturb the
/// constant pixel offset the row above bought. A blank reply gained a
/// **reason byte** (`squallar_overlays::render::rasterize::BlankReason`), so it
/// costs 7 bytes rather than 6, and the byte sits *after* the length so
/// `OVERLAY_PIXEL_PREFIX_BYTES` is untouched.
///
/// The byte is not a saving and does not pretend to be one; it is what makes
/// the saving *readable*. A blank is a **clear** — the pane stops drawing the
/// layer — and until this byte existed the page counted how often that happened
/// and could say nothing about whether the layer still covered the view when it
/// did. The same figure was therefore readable only as waste avoided, never as
/// data disappearing from under the user, and on the web the page is the only
/// side that counts: the decision is taken in the worker and this reply is the
/// whole of what comes back. One byte against the 8.26-8.92 MB the same row
/// already declines to send.
/// **Every row grew one byte on 2026-09-10**, and it is the same byte in all
/// three: a **window tag** at the very end of the reply, `0` for a picture that
/// is the whole of what was dispatched and `1` followed by four `u32`s for one
/// cut down to a bounding box of its content
/// (`squallar_overlays::render::rasterize::PictureCrop`).
///
/// **At the end, after the cells block, and that placement is the compatibility
/// story.** `OVERLAY_PIXEL_PREFIX_BYTES` is the constant offset the browser
/// transport finds the picture at before it copies anything, and a field
/// written after both variable-length blocks cannot move it; the split decoder
/// reads the window from the same place because the cells block states its own
/// length. One byte, or seventeen on a row that cut a window, against the
/// 17,971,200 B picture a window is there to avoid allocating.
pub const WIRE_REPLY_ROWS: &[&str] = &[
    "bare | 23 | 0x5e1e7016a0bc593c",
    "cells | 75 | 0xd7db240157d871b3",
    "blank | 8 | 0x30fa538cf73d1169",
];

/// The 13 frame-reply framing rows, exactly as
/// `offload::tests::the_frame_reply_framing_is_the_one_this_registry_ships`
/// asserts them: the frame's head+tails wire form
/// (`squallar_radar::frame::RenderedFrame::write_head` + the nominated
/// `[polar, codes, image]` tails) — head and three tails for each of three
/// fixtures: `full`, with every optional trio present; `bare`, with none; and
/// `plane`, whose surface is a code plane rather than a raster. The per-tail
/// rows also pin the tail ORDER.
///
/// # What moved when the code tail arrived, and what did not
///
/// A polar frame carries its gates as **codes**, and they travel as their own
/// tail rather than appended to the head. `8b22ca6f2` is why: the overlay
/// reply appends its picture to the head and *"the page cannot find the
/// picture in it, because the pixels sit behind a block with no length of its
/// own"*. A tail has its own length by construction and starts at offset zero.
///
/// Two rows are **re-pinned**, and both for the same one-byte reason — the
/// head gained a surface discriminant, written last so every field already in
/// it keeps its offset:
///
/// | row | was | is |
/// |---|---|---|
/// | `frame/full/head` | `29 \| 0x89dc568e54cf7abb` | `30 \| 0x10e1ceda1c8d8bc1` |
/// | `frame/bare/head` | `11 \| 0xc813d3185b023723` | `12 \| 0xfbe6d562a4c3b079` |
///
/// **Every other pre-existing row was byte-identical in THAT change**, and
/// that was an assertion rather than an expectation: `frame/full/polar`,
/// `frame/bare/polar`, `frame/full/image` and `frame/bare/image` carried the
/// same lengths and the same digests they had carried before, because
/// `PolarField` and `RasterImage` were not touched by it. A move in one of
/// those would have been a bug in it, not a row to re-record. The three polar
/// rows have since moved for a different deliberate change — the next block
/// is what moved them and why; the image rows have not moved and still carry
/// what they always did.
///
/// Six rows are **new**: a code tail for each of the two raster fixtures
/// (empty, which is the mutual exclusion made visible — exactly one of
/// `codes` / `image` is ever non-empty), and four for the `plane` fixture.
///
/// **The `plane` fixture exists because neither of the other two reaches the
/// head's code block.** Both are rasters, so their surface byte is followed by
/// nothing; a build that reordered the plane's shape and key fields would move
/// no length and no digest anywhere, and the codec's own round-trip could not
/// see it either — the direct arm and the via-wire arm run the same encoder,
/// so a symmetric change is invisible to a parity test. That is the hole
/// [`WIRE_HEIGHT_REPLY_ROWS`] was written to close for the height reply, found
/// again here before it could ship.
///
/// # What moved when the polar tail learned to name its value form
///
/// **Re-pinned 2026-09-08, and the three polar rows MOVED BYTES ON PURPOSE.**
/// `PolarField` holds a still pane's gate numbers in either of two forms —
/// four bytes a gate, or one byte a gate indexing a table of the distinct
/// patterns the render painted — and the tail now leads with a **form byte**
/// saying which. Before it, the reply could only carry the wide form, so the
/// worker compacted a plane and then widened it at the door and the page held
/// four bytes a gate for numbers already narrowed. The two payloads are not
/// distinguishable from their own bytes, which is why a byte in front of them
/// is the whole change.
///
/// | row | was | is |
/// |---|---|---|
/// | `frame/full/polar` | `80 \| 0x9f0c3f4e5dce8435` | `81 \| 0xacf06f3c361ecebd` |
/// | `frame/bare/polar` | `40 \| 0x3f3ecf0cef9be2c0` | `41 \| 0x257bc9fd74b17416` |
/// | `frame/plane/polar` | `40 \| 0x3f3ecf0cef9be2c0` | `41 \| 0x257bc9fd74b17416` |
///
/// The head and every surface row are **unchanged**: the form byte leads the
/// tail it describes, and the head's surface discriminant is about `codes` vs
/// `image` and knows nothing about it. `frame/coded/polar` is the thirteenth
/// row and the form the change admits — the `full` fixture's own six numbers,
/// held once each and named by a byte apiece.
///
/// **The wide payload behind that byte is the payload it always was.**
/// `polar::tests::the_polar_wire_layout_is_the_one_this_protocol_ships` pins
/// those bytes and is unedited at `(112, 0x986a_92ef_b56e_c209)`; the tag went
/// in FRONT of the payload rather than into it, which is the whole reason a
/// second form could join a versionless encoding at all.
///
/// The `full` fixture's four counts were made **four different numbers** in
/// the same change. They were `2, 3, 3, 6`, and a tamper that swapped `gates`
/// with `reach_gates` inside the encoder left `frame/full/polar` byte-identical
/// — the same blindness to a symmetric reorder the `plane` fixture above was
/// added for, in the neighbouring block. `polar/full` moved for that as well
/// as for the form byte; the two default fields moved for the form byte alone.
///
/// # What moved when the plane block learned to name its decode form
///
/// **Re-pinned 2026-09-09, and exactly one row moved.** A `CodePlane` decodes
/// through either the wire's own affine pair or a table of the bit patterns
/// one sweep painted, and the block now leads with a **form byte** saying
/// which. The two decodes are not distinguishable from their own bytes, which
/// is the same reason the polar tail grew its form byte a day earlier.
///
/// | row | was | is |
/// |---|---|---|
/// | `frame/plane/head` | `31 \| 0xe21c3f042291c176` | `32 \| 0xa35db09a7c5eceea` |
///
/// **Accounted byte for byte, not re-recorded.** The old 31 bytes were
/// `0000000000c06c40 00 00 00 01` — 230.0 km, three absent optionals and
/// `SURFACE_CODES` — followed by the plane's 19:
/// `03000000 04000000 0100 00004000 00844208`, which is the fixture's
/// 3 radials, 4 gates, Reflectivity's wire code 1, 2.0, 66.0 and 8. FNV-1a
/// over that run is `0xe21c3f042291c176`, the pin it replaced, computed from
/// those literals and not from the encoder. The new 32 are that run with one
/// `0x00` inserted at **offset 12**, in front of the plane block and behind
/// the frame's own fields, and FNV-1a over *that* is `0xa35db09a7c5eceea`.
/// A prepended byte has no multiplicative shortcut the way an appended `0x00`
/// does (`new == old * FNV_prime`), so the account is the byte positions and
/// the recomputation rather than a one-line identity.
///
/// Every other row is **unchanged**, the three polar rows and the code tail
/// included: the form byte went into the head block, and the tail is still
/// level 0's codes and nothing else. `frame/table/head` is the fourteenth row
/// and the form the change admits — the same fixture plane, held as the two
/// numbers it actually paints and a byte apiece naming them.
///
/// Row-length arithmetic (independent of the encoder): head/full
/// 8 + (1+8) + (1+1) + (1+1+4+4) + 1 = 30; head/bare 8+1+1+1+1 = 12;
/// head/plane 8+1+1+1+1 + 20 = 32, the trailing 20 being
/// `CodePlane::WIRE_HEAD_AFFINE_BYTES` (1+4+4+2+4+4+1 — a form byte, the
/// shape, and the affine decode); head/table 8+1+1+1+1 + (1+4+4+2+4 + 2x4) =
/// 35, the table's own block being a count and its two entries; polar/full
/// 81 = 1 form + 16 header + 3x8 + 2x8 + 6x4; polar/bare, polar/plane and
/// polar/table 41 = 1 form + 16 header + 3x8 (the default field); polar/coded
/// 91 = 1 form + 16 header + 3x8 + 2x8 + 4 table count + 6x4 table + 6x1
/// codes; codes/plane and codes/table 12 = 3 radials x 4 gates, **level 0
/// alone** — the mip chain is a pure function of it and is rebuilt at decode
/// rather than sent; image and the empty tails = the fixture Vecs.
pub const WIRE_FRAME_REPLY_ROWS: &[&str] = &[
    "frame/full/head | 30 | 0x10e1ceda1c8d8bc1",
    "frame/full/polar | 81 | 0xacf06f3c361ecebd",
    "frame/full/codes | 0 | 0xcbf29ce484222325",
    "frame/full/image | 8 | 0x0363b2a3926bce45",
    "frame/bare/head | 12 | 0xfbe6d562a4c3b079",
    "frame/bare/polar | 41 | 0x257bc9fd74b17416",
    "frame/bare/codes | 0 | 0xcbf29ce484222325",
    "frame/bare/image | 4 | 0xbe7a5e775165785d",
    "frame/plane/head | 32 | 0xa35db09a7c5eceea",
    "frame/plane/polar | 41 | 0x257bc9fd74b17416",
    "frame/plane/codes | 12 | 0x6c2b85b62288e8a5",
    "frame/plane/image | 0 | 0xcbf29ce484222325",
    "frame/coded/polar | 91 | 0x8a7e554ed27594b3",
    "frame/table/head | 35 | 0xbc2c96ba6fb7830f",
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
/// # The folds are gated (closed 2026-09-04; the gap below was live from 2026-08-30)
///
/// The fold runs in [`digest_over`], over the lists it is handed, so
/// `digest_fold_tests` perturbs one list at a time and asserts the token moves.
/// Deleting any one loop, or replacing the body with a constant, reddens the
/// test for that list. What follows is the gap as it was recorded, kept so the
/// reason for the split is beside the split.
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
    digest_over(
        WIRE_FRAMING_ROWS,
        CANONICAL_ENVELOPE_LAYOUT,
        WIRE_REPLY_ROWS,
        WIRE_FRAME_REPLY_ROWS,
        WIRE_HEIGHT_REPLY_ROWS,
        WIRE_BUILDING_REPLY_ROWS,
    )
}

/// The fold itself, over the lists it is handed rather than the pinned
/// constants, so a test can perturb one list and assert the answer moves.
/// That is the gate the doc above recorded as missing: with the fold inlined
/// over the constants, the only way to perturb a list was a source edit, and
/// nothing could assert that each `for` loop below is load-bearing.
fn digest_over(
    framing: &[&str],
    envelope: &str,
    reply: &[&str],
    frame_reply: &[&str],
    height_reply: &[&str],
    building_reply: &[&str],
) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for row in framing {
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
    hash = fnv1a64(hash, envelope.as_bytes());
    for row in reply {
        hash = fnv1a64(hash, row.as_bytes());
    }
    for row in frame_reply {
        hash = fnv1a64(hash, row.as_bytes());
    }
    for row in height_reply {
        hash = fnv1a64(hash, row.as_bytes());
    }
    for row in building_reply {
        hash = fnv1a64(hash, row.as_bytes());
    }
    hash
}

#[cfg(test)]
mod digest_fold_tests {
    //! Each pinned list is load-bearing in the digest: perturb one, and the
    //! token moves. Deleting any one `for` loop in `digest_over`, or replacing
    //! its body with a constant, reddens the test for that list -- which is
    //! exactly what the worker suite could not say before these existed.
    use super::*;

    fn real() -> u64 {
        digest_over(
            WIRE_FRAMING_ROWS,
            CANONICAL_ENVELOPE_LAYOUT,
            WIRE_REPLY_ROWS,
            WIRE_FRAME_REPLY_ROWS,
            WIRE_HEIGHT_REPLY_ROWS,
            WIRE_BUILDING_REPLY_ROWS,
        )
    }

    /// A perturbation that is not a perturbation would make every test below
    /// vacuous, so each one goes through here and is checked to differ.
    fn perturbed(rows: &[&str]) -> Vec<String> {
        assert!(
            rows.len() >= 2,
            "a list of fewer than two rows cannot be reordered"
        );
        let mut out: Vec<String> = rows.iter().map(|r| r.to_string()).collect();
        out.swap(0, 1);
        assert_ne!(out[0], rows[0], "the perturbation must actually move a row");
        out
    }

    fn as_strs(v: &[String]) -> Vec<&str> {
        v.iter().map(String::as_str).collect()
    }

    #[test]
    fn the_pinned_digest_is_what_wire_digest_reports() {
        assert_eq!(real(), wire_digest());
    }

    /// Framing rows are reordered rather than rewritten: the fold looks each
    /// label up in the composed registry and panics on an unknown one, so the
    /// perturbation keeps every label valid and moves only their order --
    /// which the registry index prefix makes visible.
    #[test]
    fn a_moved_framing_row_moves_the_digest() {
        let p = perturbed(WIRE_FRAMING_ROWS);
        let moved = digest_over(
            &as_strs(&p),
            CANONICAL_ENVELOPE_LAYOUT,
            WIRE_REPLY_ROWS,
            WIRE_FRAME_REPLY_ROWS,
            WIRE_HEIGHT_REPLY_ROWS,
            WIRE_BUILDING_REPLY_ROWS,
        );
        assert_ne!(moved, real(), "the framing fold is not load-bearing");
    }

    #[test]
    fn a_moved_envelope_layout_moves_the_digest() {
        let moved = digest_over(
            WIRE_FRAMING_ROWS,
            "h:u32 w:u32 bounds:4xf64 ceil:u32",
            WIRE_REPLY_ROWS,
            WIRE_FRAME_REPLY_ROWS,
            WIRE_HEIGHT_REPLY_ROWS,
            WIRE_BUILDING_REPLY_ROWS,
        );
        assert_ne!(moved, real(), "the envelope fold is not load-bearing");
    }

    #[test]
    fn a_moved_reply_row_moves_the_digest() {
        let p = perturbed(WIRE_REPLY_ROWS);
        let moved = digest_over(
            WIRE_FRAMING_ROWS,
            CANONICAL_ENVELOPE_LAYOUT,
            &as_strs(&p),
            WIRE_FRAME_REPLY_ROWS,
            WIRE_HEIGHT_REPLY_ROWS,
            WIRE_BUILDING_REPLY_ROWS,
        );
        assert_ne!(moved, real(), "the reply fold is not load-bearing");
    }

    #[test]
    fn a_moved_frame_reply_row_moves_the_digest() {
        let p = perturbed(WIRE_FRAME_REPLY_ROWS);
        let moved = digest_over(
            WIRE_FRAMING_ROWS,
            CANONICAL_ENVELOPE_LAYOUT,
            WIRE_REPLY_ROWS,
            &as_strs(&p),
            WIRE_HEIGHT_REPLY_ROWS,
            WIRE_BUILDING_REPLY_ROWS,
        );
        assert_ne!(moved, real(), "the frame-reply fold is not load-bearing");
    }

    #[test]
    fn a_moved_height_reply_row_moves_the_digest() {
        let p = perturbed(WIRE_HEIGHT_REPLY_ROWS);
        let moved = digest_over(
            WIRE_FRAMING_ROWS,
            CANONICAL_ENVELOPE_LAYOUT,
            WIRE_REPLY_ROWS,
            WIRE_FRAME_REPLY_ROWS,
            &as_strs(&p),
            WIRE_BUILDING_REPLY_ROWS,
        );
        assert_ne!(moved, real(), "the height-reply fold is not load-bearing");
    }

    #[test]
    fn a_moved_building_reply_row_moves_the_digest() {
        let p = perturbed(WIRE_BUILDING_REPLY_ROWS);
        let moved = digest_over(
            WIRE_FRAMING_ROWS,
            CANONICAL_ENVELOPE_LAYOUT,
            WIRE_REPLY_ROWS,
            WIRE_FRAME_REPLY_ROWS,
            WIRE_HEIGHT_REPLY_ROWS,
            &as_strs(&p),
        );
        assert_ne!(moved, real(), "the building-reply fold is not load-bearing");
    }
}
