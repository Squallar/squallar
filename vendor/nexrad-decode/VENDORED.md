# nexrad-decode, vendored

This directory is a copy of a crates.io crate that this workspace maintains
locally. It is not our code. Everything in it is upstream's except what is
listed under [Local changes](#local-changes) below, and keeping that list short
and true is the whole point of the file — it is what makes the directory
reviewable, and what lets a later upstream release be adopted by re-applying a
delta somebody can still read.

**These changes are not going upstream.** That is a decision taken 2026-08-12,
not an oversight. The consequence is worth stating plainly: upstream will not
carry these fixes, so this directory is not a stopgap until a release ships —
it is where the fixed decoder lives, indefinitely. A later upstream release is
still worth adopting for everything else it brings, and re-applying this delta
onto it is the job the short list below exists to make possible.

## Provenance

| Field | Value |
| --- | --- |
| Package | `nexrad-decode` |
| Version | `1.0.0-rc.3` — the newest published version as of 2026-08-11 |
| Source | crates.io, `sha256:19a6928b9f7bb9a2ac7c7f4bcb04ad9104ef307d6c4dee251c36cd4b69a47bc1` |
| Unpacked from | `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/nexrad-decode-1.0.0-rc.3/` |
| Upstream repo | <https://github.com/danielway/nexrad>, subdirectory `nexrad-decode` |
| Upstream commit | `de6e16d1977ca118738d917c130bf13413da7626` (from the tarball's `.cargo_vcs_info.json`) |
| Author | Daniel Way `<contact@danieldway.com>` |
| License | MIT — full text in `LICENSE` next to this file |

The packaged tarball ships **no** license file, only `license = "MIT"` in the
manifest. `LICENSE` here was reconstructed from the upstream repository's own
`LICENSE`, copyright line included, because "MIT" in a metadata field is a
declaration and not the notice the license requires us to redistribute.

## Why this exists

The published nexrad-decode cannot read a TDWR volume.

`decode_messages` (`src/messages/mod.rs:40`) walks a record message by message.
For a Message 31 it hands the body to `digital_radar_data::Message::parse` and
then accepts wherever that parser happened to stop, instead of repositioning to
the end the message header declared. WSR-88D pads its Message 31 bodies to
nothing, so the two agree and the code is accidentally correct. TDWR pads every
radial to an 8-byte boundary, leaving 4–7 bytes of slack per message: the next
header is read out of that padding, `segmented()` flips true on garbage, the
decoder logs segment numbers in the tens of thousands, and the record never
resyncs.

Byte-verified on `s3://unidata-nexrad-level2/2026/08/10/TPIT/TPIT20260810_000139_V08`
(924,093 bytes, 49 records, VCP 90), decoded through `nexrad-data` both ways:

| | as published | with the framing fix |
| --- | --- | --- |
| messages | 780 | 5,894 |
| Type 31 radials | **51** | **5,760** |
| radials per radial record | 1 or 2 | 120, all 48 records |
| warnings | 330 | **0** |

Fifty-one radials out of 5,760 is why the site rendered as filled wedges rather
than a sweep.

There is no seam to work around it in — both of this workspace's ingest paths
reach the decoder through `nexrad-data`, which depends on nexrad-decode itself.
Hence a `[patch.crates-io]` entry in the root `Cargo.toml`, which redirects our
copy *and* nexrad-data's, so one patched crate serves both.

Vendoring rather than forking, because a git dependency puts this workspace's
build on somebody's branch continuing to exist. Vendoring rather than waiting,
because `1.0.0-rc.3` is the newest published version and upstream `main` still
carries the identical logic.

## Local changes

This is the complete list. `diff -rq` against the unpacked registry copy must
produce exactly these entries and nothing else.

### Removed — local contamination and packaging residue

| Path | Why |
| --- | --- |
| `default_*.profraw` (2 files, 530 KB) | Coverage output from an unrelated local run that happened to land in the registry checkout. Never part of the crate. |
| `.cargo-ok` | Cargo's own extraction marker. |
| `.cargo_vcs_info.json` | Recorded in the provenance table above instead. |
| `Cargo.toml.orig` | Upstream's pre-normalisation manifest, which refers to workspace inheritance and sibling `path` dependencies that do not exist here. |
| `Cargo.lock` | A packaged crate's lockfile is inert; this workspace's root `Cargo.lock` is the one that resolves anything. |

### Removed — targets that cannot compile or that nothing here runs

| Path | Why |
| --- | --- |
| `tests/volume_decode.rs`, `tests/version_compat.rs`, `tests/extract_fixtures.rs` + their `[[test]]` blocks | All three `include_bytes!("../../downloads/…")` or read `nexrad_data::volume` against files that exist only in upstream's checkout after a download step. They cannot compile here at all. |
| `benches/` (4 files, 420 KB) + the `[[bench]]` block + the `criterion` dev-dependency | Nothing in this workspace runs them, and `--all-targets` would compile criterion on every CI row including wasm32. |
| `examples/` + the `[[example]]` block + the `clap` dev-dependency | Same. |
| dev-dependency `env_logger`, dev-dependency `tokio` | Used only by the three deleted tests. |
| dev-dependency `nexrad-data` | Used only by the three deleted tests — and it pins **1.0.0-rc.4** while this workspace runs **=1.0.0-rc.7**. Pre-release versions do not unify, so keeping it would have put a second nexrad-data in `Cargo.lock`, built and linted on every row, to serve tests that are gone. |

`autoexamples`/`autobenches`/`autotests` are all `false` in the packaged
manifest, so each block had to be deleted together with its directory: a block
naming a missing path is a hard error, and a directory with no block is dead
weight.

### Kept deliberately

- `tests/malformed_input.rs` + its `[[test]]` block — the negative-input pin.
  It constructs the all-ones header whose sentinel size field is exactly what
  the framing fix has to survive.
- `tests/generate_synthetic_fixtures.rs` + its `[[test]]` block — std-only,
  `#[ignore]`-gated, compiles everywhere.
- All 15 in-source `snapshot_test.rs` modules, their 16 `.snap` files, the
  `insta` dev-dependency, and `tests/data/messages/` (15 fixtures, 112 KB).
  **These are the WSR-88D behaviour pin.** The bar for any change to this
  directory is that they stay byte-unchanged — the one exception being a
  snapshot that was itself recording a defect, which is the case for the two
  the multi-segment payload fix moves, each named and evidenced under
  *Changed — source*. No fixture byte has ever changed.

### Added

| Path | Why |
| --- | --- |
| `LICENSE` | See above — the tarball ships none. |
| `VENDORED.md` | This file. |
| `src/messages/framing_tests.rs` | The framing fix's own tests, and the multi-segment payload fix's, described in full under *Changed — source* below. An in-source module rather than a `tests/` target so it travels with the fix in any delta re-applied onto a later upstream release — which does mean it is a new file, shows up in `diff -rq`, and so belongs in this table and not only in that prose. |
| `src/messages/digital_radar_data/position_tests.rs` | The position scale fix's tests, under *Changed — source* below. Same arrangement and the same reason. Ships no fixture: the three Volume Data Blocks it reads are quoted as byte arrays in the file, each naming the archive object it was lifted from, so `tests/data/` is untouched. |

### Changed — `Cargo.toml`

Beyond the removals already listed: a vendoring note under the generated
header, and two lint tables.

The lint tables are not cosmetic. `.github/workflows/clippy.yaml` runs
`cargo clippy --all-targets --all-features --fix` across every workspace
member and **auto-commits the rewritten tree to `main`**, then gates on
`-D warnings`. Without the tables, a bot would eventually rewrite upstream
source here on its own schedule and this file would stop being true. With
them, the bot has nothing to fix and the gate has nothing to fail on.

Two mechanics worth knowing before editing them:

- Cargo turns the tables into `-A` flags and CI appends `-D warnings` after
  them. That does not undo them — `-D warnings` only escalates lints that are
  *at* warn level, and an explicitly allowed lint is at allow. The one form
  that would **not** have survived is `[lints.rust] warnings = "allow"`: same
  lint id as the CI flag, and there the last flag wins. That is why the rustc
  side names groups (`unused`, `future_incompatible`, …) instead.
- The tables cannot reach a `#![deny(…)]` written in the source, because a lint
  attribute outranks any command-line level. That is what forced the single
  source deviation below.

Verified against a real case: `manual_div_ceil` in
`tests/generate_synthetic_fixtures.rs` fires on the registry copy — with the
`[lints.clippy]` table it does not, including under `-- -D warnings`.

### Changed — source

The framing fix this directory exists for, the position scale fix that came out
of reading a framed TDWR volume, the multi-segment payload fix, one lint
scoping that changes no behaviour, and four findings
from an audit of the vendored copy — one allocation removed from the per-radial
path, one silent truncation fixed, and two hygiene changes. Every one of the
four is either a shrink of the delta or a pin on something the delta depends
on, and none of them moves a decoded byte; the three that touch decoding were
each verified over 35 volumes (27 WSR-88D, 8 TDWR) with a digest over every
message, every moment array and every radial.

#### The framing fix — `src/messages/mod.rs`

Three places decided where a message ends, and all three read the declared size
as if it were measured from the start of the message. It is measured from byte
12: `segment_size` counts halfwords from the end of the CTM prefix, so a
message occupies `12 + message_size_bytes()` bytes. Upstream states this itself
in `nexrad-data`'s `src/volume/record.rs:213-214`. A new `declared_end(offset,
header)` helper is now the single answer to the question, and all three call
it.

1. **Type 31, parsed successfully.** Previously the reader was left wherever
   the data block walk stopped. Now the declared end is used instead — but only
   when it lies within `MAX_TRAILING_PAD` (7) bytes of the walk. That range is
   the whole of the disagreement TDWR can legitimately produce: it pads each
   radial body out to an eight-byte boundary, which is 4-7 bytes, and WSR-88D
   pads none. Anything further out is a declaration no parse agrees with, and
   the framing stays with the parse, with one warning naming both positions.

   The bound is the point. Trusting the declaration unconditionally would have
   fixed TDWR and simultaneously introduced a failure the published code did
   not have — a corrupt `segment_size` desyncing a record whose messages are
   all parsing cleanly. Bounded, neither failure exists.

   The skip is clamped to the end of the input, so a final radial that parsed
   completely but whose pad was truncated is still kept.

2. **Unknown variable-length message.** Previously `advance(size - 28)`, which
   lands 12 bytes short because `size_of::<MessageHeader>()` includes the CTM
   prefix that `message_size_bytes()` already excludes. Now a skip to the
   declared end, and `Err(UnexpectedEof)` if that is past the end of the input.
   Nothing walks these messages, so there is no second opinion to bound the
   declaration against.

3. **Error recovery.** `try_skip_to(offset + size)` → `try_skip_to(declared_end
   (…))`; same 12 bytes, same correction. Landing short here put the reader
   inside the failed message's padding, so a single bad radial cost the rest of
   the record.

Every addition saturates. A 0xFFFF-sentinel header carries a 32-bit byte count
of up to `0xFFFF_FFFF`, which overflows a 32-bit `usize` — a panic on wasm32 in
dev and a silent wrap into mis-framing in release. `tests/malformed_input.rs`'s
`test_all_ones` constructs exactly that header, and rustdar-web is a shipped
wasm32 target. A saturated target is simply past the end of any input, which
`try_skip_to` rejects.

Two behaviour deltas worth stating:

- `Message::size` for a Type 31 is now the declared framed size including pad,
  where before it was the distance the parser walked. Identical on any message
  without pad, which is every WSR-88D message and every packaged fixture.
- A truncated unknown variable-length message at the end of the input now
  surfaces `UnexpectedEof` instead of over-advancing silently. `decode_messages`
  absorbs that as a `break` and keeps everything decoded so far, so no caller
  can newly lose a record.

New `src/messages/framing_tests.rs`, an in-source `#[cfg(test)]` module so it
travels with the fix when this delta is re-applied onto a later release. Eight tests here, and two more added
by the multi-segment payload fix below: the padded TDWR stream
stays framed; every pad width from 0 to 7 stays framed; a pad of 8 — one past
the bound — is not trusted; the unpadded stream keeps the exact offsets and
sizes it had; the packaged WSR-88D radial declares exactly its own length so
the skip is a no-op; a failed parse recovers to the next message; an unknown
variable-length message is skipped to its declared end; and a corrupt declared
size does not desync a clean parse. Three of the eight fail against the
published code.

The second and third of those are the ones that make "the bound is the point"
enforceable rather than merely asserted. Two one-line edits used to survive the
whole suite: closing the accepted range one byte early
(`walk..walk + MAX_TRAILING_PAD`), and widening `MAX_TRAILING_PAD` from 7 to
15. The first silently desyncs 13.3% of real TDWR radials — measured across
48,600 of them, the pad is 4 on 30,285, 5 on 1,440, 6 on 10,395 and **7 on
6,480**, so the inclusive endpoint is where a sixth of the site's data lives.
The second reintroduces exactly the corrupt-`segment_size` failure the bound
was chosen to prevent. Both now fail, each against a different test. Both
tests spell 7 out rather than reading `MAX_TRAILING_PAD`: a test phrased in
terms of the bound moves with the bound and pins nothing.

Odd pads needed a fixture that can express one. A body of message header plus
radial header is even and `segment_size` counts halfwords, so the original
`m31` builder can only produce an even pad, and asserts as much.
`m31_with_moment` adds one eight-bit reflectivity block with an odd gate count,
which is how real TDWR radials come by their odd pads.

#### The multi-segment payload is the whole declaration — `src/messages/mod.rs`

The same twelve bytes again, in the fourth place that read the declared size:
a fixed-length segment's payload was `message_size_bytes() -
size_of::<MessageHeader>()`, which takes the CTM prefix out a second time. The
reader position survives it — the frame is padded to 2432 either way — so
nothing desynced and nothing warned. What went was the last twelve bytes of
every segment's *content*.

The previous campaign left this alone and recorded the reason: fixing it
changes what WSR-88D messages decode to, which was exactly what the
byte-identical-snapshot bar existed to protect. What was never established was
whether anything was actually lost. It is, on every volume:

| | as vendored | fixed |
| --- | --- | --- |
| Message 15, azimuth segments | 1,790 of 1,800, last elevation **350** of 360 | **1,800**, every elevation 360 |
| Message 18, `site_name` | `None` — the bytes at that offset are not text | `"KDMX"`, `"KTLX"`, `"KATX"`, `"KLWX"`, `"KCRP"` |
| Message 13, five elevations (KCRP) | 588 bytes short, `UnexpectedEof`, **0** elevation segments | **5** |

Measured over 171 WSR-88D volumes across 11 sites (KHNX, KMSX, KLOT, KLWX,
KATX, KTLX, KTWX, KDMX, KFTG, KCRP and the holdout), decoding every fixed-
segment message both ways. The Message 15 result is 171 of 171, not a sample.
Message 18 is offset-addressed — `data[ICD_offset - 44]` — so the loss is not
at the tail but at *each* segment boundary, and every field past the first one
shifts; its own doc comment already said the message "spans approximately 9468
bytes", which is the fixed length, against the 9420 the code produced.

Five TDWR volumes (TPIT, TDAL, TJFK, TMCI 2026-08-10, TORD 2020-08-10) carry no
multi-segment message at all — only types 0, 2, 5 and 31 — so the network this
directory exists for is untouched by this, and so is everything this workspace
reads: it matches only Types 31, 1 and 5, and no Type 5 in the corpus is
multi-segment. Not one radial, moment or VCP moves.

**Clamped to the frame**, which is the second half of the fix. A segment is one
2432-byte frame and its body is 2404 bytes; a declaration past that describes
bytes the frame does not hold. All 855 real multi-segment frames measured
declare 2,416 and clamp to nothing — but this crate's own
`tests/generate_synthetic_fixtures.rs` measures `segment_size` from the start
of the frame rather than from the end of the prefix (line 61,
`(HEADER_SIZE + payload_size) / 2` with `HEADER_SIZE = 28`), so
`clutter_filter_bypass_map.bin` declares 2,432 against a 2,404-byte body.
Uncorrected, its payload runs twelve bytes into the next frame, splices that
frame's CTM prefix into the message content, and leaves the reader inside the
frame with the next header read out of the middle of a segment. The clamp is
also strictly safer than what was there: the old arithmetic over-ran the frame
for any declaration above 2,432, and the new one cannot over-run at all.

**Two snapshots move, and both were recording the defect.**

- `clutter_filter_map`: `azimuth_count: 350` → `360` on the fifth elevation.
  A clutter filter map has one azimuth segment per degree; 350 is not a map the
  RDA can emit. The first four elevations were already 360 because the loss
  lands 12 bytes per segment and only the last elevation runs out.
- `rda_adaptation_data`: `site_name: None` → `Some("KDMX")`. The fixture is
  KDMX's — the same snapshot's `site_latitude: 41.731` and
  `site_longitude: -93.723` are KDMX's coordinates. `None` was
  `std::str::from_utf8` refusing four shifted bytes, and the same field reads
  the correct four-letter id at all five sites checked.

Every other snapshot is byte-unchanged, including
`clutter_filter_bypass_map`'s, which is what the clamp is holding in place.

Two new tests in `src/messages/framing_tests.rs`, which already owns this
question for variable-length messages. `a_multi_segment_payload_is_everything_
its_header_declares` builds a two-elevation clutter filter map framed the way
the RDA frames one and fails against the unfixed decoder with `[360, 356]`;
`a_segment_declaring_more_than_its_frame_holds_is_read_to_the_frame` builds a
bypass map framed the way the fixture generator frames one and fails against
the fix with the clamp removed. Neither is phrased in terms of the constants it
pins.

#### The position is read at the scale it was written — `src/messages/digital_radar_data/volume_data_block.rs`

`latitude_raw()` and `longitude_raw()` handed back the `Real*4` in the block
unexamined. The field is **degrees** — ICD 2620002AA Table XVII-B, *Data Block
#1 (Volume Data)*, `Lat` at bytes 8-11 and `Long` at bytes 12-15, both "deg",
`0.0 to 90.0` and `-180.0 to +180.0`. No revision of that table states another
scale and every WSR-88D volume writes degrees.

TDWR Level II volumes written **before 2021-09-15** do not. They carry the
Level III radar position in those fields instead — ICD 2620001AD's Product
Description Block halfwords 11-12 and 13-14, an `INT*4` count of thousandths of
a degree — widened into the `Real*4` without being divided. `TORD` states
`41797.0, -87858.0` and means 41.797 °N, 87.858 °W. Read as declared it is not
a place on Earth, so a caller that range-checks refuses it and no TDWR volume
older than that date places its radar at all.

Measured, not inferred from one file. Seven TDWR sites on 2020-08-10 — `TORD`,
`TOKC`, `TDAL`, `TPIT`, `TMIA`, `TSJU`, `TPHX` — all thousandths; `KTLX` and
`KAMX` on the same day, degrees. The producer was corrected between
`TORD20210914_000151_V08` and `TORD20210915_000148_V08`, bisected over 21 TORD
volumes from 2020-08 to 2026-08. Every affected volume is still in
`s3://unidata-nexrad-level2`, so this is not history.

Checked against a source that is not another Level II volume: the same nine
radars' Level III `NCR` products of 2026-08-12, whose Product Description Block
carries the position in the documented thousandths field. Decoded through the
thousandths reading, every site lands within **0.3 m** of its Level III
position except `TSJU` (106 m) and `TPHX` (111 m), each of which is one
thousandth of a degree — the finest their field can express, and a
disagreement between producers rather than a decode error.

The reading is a property of the **pair**, in `states_thousandths`: the pair is
not already a position, both coordinates are exact integers, and a thousandth
of the pair is a position. Each condition earns its place. The first is what
makes every conforming volume bit-identical — a value inside the ICD's range is
never touched, and every WSR-88D value is inside it. The second is what
separates the encoding from corruption: an `INT*4` widened into an `f32` is an
exact integer, every count of thousandths a coordinate can produce is under
180,000 and so exactly representable, and a non-integer out of range is not
this encoding. The third is what makes the result checkable rather than
assumed. And deciding for the pair rather than per coordinate matters at the
equator: a radar within 0.09° of it states a thousandths latitude that is also
a legal degrees latitude, and two independent decisions could land it in a place
neither reading names.

Neither height field is affected — `TORD` states `site_height` 226 and
`tower_height` 226 either side of the change — and nothing else in the block is
touched.

**No snapshot moves.** The 16 `.snap` files record the `Debug` of the *raw*
zerocopy structs, not these accessors: `digital_radar_data`'s snapshot holds
`latitude: 41.7312` straight off `raw::VolumeDataBlock`, and that field is
still exactly the bytes. Verified by running the suite.

Nine tests in the new `src/messages/digital_radar_data/position_tests.rs`;
three fail against the unfixed accessors. They read three real Volume Data
Blocks quoted as byte arrays — `TORD20200810_203830_V08`,
`TORD20260812_000527_V08` and `KTLX20260811_000049_V06` — and pin, among the
rest, that both hemispheres divide alike (no US radar is south or east, so a
sign error would hide behind the archive), that the pair decides together, and
that a position no scale rescues is left out of range for the caller to refuse.

**One API consequence**, stated because it is a real loss: `raw` is
`pub(crate)`, so these two accessors were the only public route to these two
fields, and a caller can no longer see the number the block literally holds. No
new accessor was added for it — nothing in this workspace wants one, and the
smaller delta is worth more than a method with no caller.

#### The pointer table is walked, not collected — `src/messages/digital_radar_data/message.rs`

Message 31's parser read its data block pointer table into a `Vec` and then
consumed that `Vec` in the loop immediately below. The list never escaped the
function, and `pointers_raw` borrows the input rather than the reader, so the
loop can walk `chunks_exact` directly.

Collecting cost an allocation per radial and usually two: `Result<Vec<_>>`
collects through a shunt iterator whose `size_hint` lower bound is zero, so the
`Vec` could not be pre-sized from a count `chunks_exact` knows exactly — it
allocated at capacity 4 and grew 4 → 8 → 16. Measured over an 11,160-radial
volume: `decode_messages` allocator operations 40,287 → 14,007 (−65%),
instructions retired 30.65 M → 17.12 M (−44%), and 63.05 M → 49.51 M (−21.5%)
across `decode_messages` + `into_radial` together. That is 0.16–1.08% of a
whole volume decode — not user-visible on WSR-88D, and 4–7× more valuable on
TDWR, which is the site class this directory exists for.

Bit-identity is structural: `chunks_exact(4)` yields exactly-four-byte slices,
so the `try_into()` that could fail never could and its error arm was
unreachable.

**One API consequence.** That arm was the crate's only constructor of
`Error::Decoding(String)`. The clutter filter map guard below gives the variant
a constructor again, so the enum is not left with a dead arm — but if only the
pointer-table change survives a future re-application, note that `Error` is not
`#[non_exhaustive]`, so a downstream `match` naming `Decoding` still compiles
and simply cannot be reached.

#### The clutter map segment count is refused, not truncated — `src/messages/clutter_filter_map/message.rs`

`elevation_segment_count` is declared as a `u16`; `ElevationSegment` numbers
itself with a `u8`; the two were reconciled with `as u8`. A header declaring 256
segments therefore truncated to zero, the loop never ran, and the caller got
`Ok` on an empty map whose own `elevation_segment_count()` still reported 256.
Nothing in that value said the map had been lost.

Refused rather than saturated: the ICD allows 1 to 5 elevation segments
(2620002AA Table XIV), so a declaration past 255 is not a map this decoder can
act on, and clamping to 255 would mean acting on a number the file never
stated.

This is the one change here that alters what an input decodes to, and altering
it is the point — a 256-declaring header used to return `Ok`, and now returns
an error. What holds is the narrower claim: **no valid map is refused, and
nothing that ever decoded to a single elevation segment decodes differently.**
Only counts a `u8` could never hold are affected. The two real Message 15s in
the corpus checked here, from KABR and KCRP, each declare 5 segments and decode
5.

Nor can the new error cost a caller anything it had. `decode_messages` catches
a fixed-segment parse failure as a `warn!` and drops that one message; by then
the reader has already advanced past the frames, so no record desyncs and
everything else in it still decodes. This workspace matches only
`DigitalRadarData`, `DigitalRadarDataLegacy` and `VolumeCoveragePattern`, so it
never looks at a Message 15 at all.

Two in-source tests pin it: the refusal at 256, 257, 512 and 65,535, and 0, 1,
2, 5 and 255 still parsing to exactly that many segments.

#### A file's count no longer sizes an allocation on its own — `src/messages/rda_prf_data/message.rs`, `src/messages/clutter_filter_bypass_map/message.rs`

Three `Vec::with_capacity(n)` calls took `n` from a header and allocated before
anything had checked the input could back it. All three counts are
`u16`-bounded, so the worst case was about 2.1 MB and the loop underneath hit
EOF on its first read — but the crate already contains the right shape twice,
in `volume_coverage_pattern`'s elevation cuts and the clutter filter map's
range zones: `take_slice(count)`, which fails on EOF, and only then `.collect()`.

`rda_prf_data`'s PRF values are that shape exactly and now use it. The other
two cannot be: a waveform entry is variable-length, and a bypass map's range
bins are read across segment boundaries into an owned buffer, which is the one
thing `take_slice` will not do. Both are instead sized to what the reader still
holds divided by the smallest an entry can cost, so a declaration can shrink
the allocation but never grow it past what the input could satisfy.

Two caveats, because these are real differences and not only hygiene, and
because a note that claims to be the honest account has to carry the parts that
do not flatter the change. Both are stated at their sites in comments too.

`take_slice` is **narrower** than the `take_ref` loop it replaces, not only
differently placed. The loop reads what fits in the current segment and
continues in the next; `take_slice` moves to the next segment first and then
wants the whole run inside it, because that is what
`ref_from_prefix_with_elems` needs. So a PRF run that fits across the remaining
segments but not in the next one alone is now `UnexpectedEof` where the loop
would have read it in pieces. Unreachable in practice: no Message 32 in any
volume decoded here is multi-segment, the packaged fixture is a single segment
of 64 halfwords and its snapshot is unchanged, and this workspace never matches
the type.

And the bypass map's capacity is now `min(declared, remaining_total / 23042)`,
which is a floor and not the count: on a truncated map it comes out *below* the
true number of segments. That is a pre-size that can under-size, trading an
over-allocation on an unchecked number for the occasional realloc on a
once-per-volume metadata path. The right way round, but a trade. On a whole map
it is exact — while the multi-segment payload slice was twelve bytes short per
segment it was not, and with that fixed both KCRP's five-elevation map and the
packaged single-elevation fixture pre-size to the number they parse.

#### The lint scoping — `src/lib.rs`

In `src/lib.rs`'s crate-level lint attributes, upstream writes

```rust
#![deny(clippy::unwrap_used)]
#![deny(clippy::expect_used)]
```

which here becomes `#![cfg_attr(not(test), deny(…))]` for both.

Upstream's own test code breaks the rule 23 times: six `unwrap`s in
`segmented_slice_reader`'s unit tests, and 17 `expect`s across the 15 snapshot
modules (three in `digital_radar_data_legacy`, one in each of the other
fourteen). Upstream never notices, because it lints without
`--all-targets` and so never compiles `cfg(test)` code under clippy. This
workspace's CI does lint `--all-targets`, and the crate would not pass clippy
at all. Scoping the deny to non-test builds keeps it saying what it was written
to say — no `unwrap` in the library — and is the form to keep.

A comment at the site says the same thing, so nobody has to find this file
first.

Note what did **not** happen: the wasm32 contingency. `insta` 1.48.0 compiles
clean for `wasm32-unknown-unknown` under `--all-targets`, so the snapshot
modules are **not** cfg-gated and run on every target.

## Known upstream defects deliberately left alone

Recorded so that "we did not notice" is never the explanation, and so anyone
re-applying this delta onto a later upstream release knows what was examined
and deliberately left. Nobody is reporting these upstream, so expect them to
still be there.

- **`tests/generate_synthetic_fixtures.rs` writes a `segment_size` no RDA
  writes.** `build_multi_segment_frames` computes it as
  `(HEADER_SIZE + payload_size) / 2` with `HEADER_SIZE = 28` (line 61), so a
  full segment declares 2,432 bytes where the ICD — and all 855 real
  multi-segment frames measured here — declares 2,416. `build_single_segment_
  frame` writes `FRAME_SIZE / 2` for the same reason (line 44), which is inert
  because a single segment's payload is the frame body regardless.

  Not fixed, because fixing it without regenerating leaves the generator and
  the committed fixture disagreeing, and regenerating rewrites
  `clutter_filter_bypass_map.bin` — a packaged fixture, which is the one thing
  this directory does not touch. The symptom to watch for: a multi-segment
  fixture generated by this file over-declares by twelve bytes per segment, and
  only the frame clamp in `decode_messages` keeps it decodable. Correct is
  `(16 + payload_size) / 2`, together with regenerating the fixture; the
  decoded content does not change either way.

- **`SliceReader::advance` is unchecked** (`src/slice_reader.rs:35-37`). A
  runaway skip produces no warning at all; the "bytes remaining" diagnostic
  only fires on a clean break. This is why the TDWR failure was silent for as
  long as it was.

- **The 65535-sentinel path's `+12` reference point is an inference.** For the
  ordinary halfword case it is proven — by fixture, and by upstream stating it
  itself in `nexrad-data`'s `src/volume/record.rs:213-214`. For the 32-bit
  sentinel case no real message exists in any fixture to prove it; internal
  consistency favours it, and this note exists so that nobody later mistakes
  the inference for a citation.

  The framing fix above inherits the inference rather than introducing it —
  `declared_end` applies the same `+12` to both cases, because
  `message_size_bytes()` is the same function for both. The ICD says the
  sentinel repurposes the segment count and number fields as a 32-bit byte
  count; it does not say what that count is measured from.
  `an_unknown_variable_length_message_is_skipped_to_its_declared_end` pins the
  inference so that changing it is a deliberate act, and says so in its own
  comment. The one thing that would settle it is a real sentinel message, which
  no volume examined here contains.

## rustfmt

`cargo fmt --all -- --check` is clean over this directory as vendored, checked
with the toolchain `rust-toolchain.toml` pins — which is `stable`, i.e. a
**floating** version. rustfmt's output is version-dependent. A future stable
release that formats any of these ~15,500 source lines differently turns the
`fmt` CI job red on code nobody here wrote, and the fix would be a formatting
commit across upstream source that makes the next re-application noisy.

If that happens, the options in preference order are: rebase the vendored copy
onto a newer upstream release that has the fix (best — deletes this directory);
or accept the reformat as a single, separately-titled commit that touches
nothing else, and say so here.

There is no `rustfmt.toml` in this workspace, so this directory is formatted by
the same default rules as the rest of it.

## Two other bots that can reach this directory

- **Renovate** — handled. `.github/renovate.json` scans every Cargo manifest in
  the repository, and this directory contains one. Its `insta`, `chrono` and
  `zerocopy` declarations are upstream's, not ours to move: a bump merged here
  would rewrite the vendored crate and quietly make the *Local changes* list
  above untrue. `"ignorePaths"` now lists `vendor/**`, so no package file under
  it is scanned at all.

  Two mechanics behind the shape of that key. It is a top-level option — the
  schema has no `ignorePaths` inside `packageRules`, so it cannot be scoped to
  a rule. And it is an array, which config resolution replaces rather than
  merges, so writing it drops Renovate's own default of
  `["**/node_modules/**", "**/bower_components/**"]`; both are written back
  explicitly. That also means a value set by the shared
  `local>USA-RedDragon/renovate-configs` preset — which is not in this
  repository and cannot be read from here — is now overridden by this file. If
  the preset was ignoring paths of its own, they have to be restated here.

  Deliberately not done: pinning the vendored manifest's dependency versions,
  or moving it out of `[workspace.members]`. Both would trade a bot problem for
  a real one — the vendored tests must run in `cargo test --workspace`.

- **Coverage** — not handled, named so it is not a surprise.
  `cargo llvm-cov --all-features` in `test.yaml` now measures this crate too,
  and the badge and `coverage-baseline.tsv` it auto-commits move accordingly —
  roughly 15,500 lines of decoder joined the denominator, covered only to the
  extent upstream's own suite covers them. The number changing is expected and
  is not a regression in this workspace's code. Nothing gates on a threshold,
  so nothing fails.

## Not going upstream

**Decided 2026-08-12: none of this is being contributed back.** No pull request
will be filed, and the defects under *Known upstream defects* are not being
reported as issues.

This is recorded because the shape of the work here only makes sense against
it. The changes are committed as independent, separately-titled commits; the
tests live in an in-source module beside the code they pin; the trims are kept
apart from the behavioural fixes. That was all done to keep a contributable
diff, and it is worth keeping for a different reason — it is what makes the
delta re-appliable onto a later upstream release, and auditable in the
meantime. Read the structure that way rather than as a PR waiting to be sent.

## Removing this directory

Since these fixes are not being contributed, no upstream release will contain
them by way of this work. The directory therefore stays. The one case that
removes it is upstream fixing the same defects independently — worth checking
whenever the pin is bumped. If a published nexrad-decode ever does frame a TDWR
Message 31 and read a multi-segment payload correctly:

1. Delete `vendor/nexrad-decode/`.
2. Delete `"vendor/nexrad-decode"` from `[workspace.members]`, the
   `[patch.crates-io]` entry, and `[profile.dev.package.nexrad-decode]` in the
   root `Cargo.toml`.
3. Bump the `[workspace.dependencies]` pin to that version.
4. `cargo tree -i nexrad-decode` should show one registry node again.
