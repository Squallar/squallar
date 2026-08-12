# bzip2-rs, vendored

This directory is a copy of a crate that this workspace maintains locally. It
is not our code. Everything in it is upstream's except what is listed under
[Local changes](#local-changes) below, and keeping that list short and true is
the whole point of the file — it is what makes the directory reviewable, and
what makes the eventual upstream pull request a diff somebody can read.

It is the third such directory, after `vendor/nexrad-decode/VENDORED.md` and
`vendor/nexrad-data/VENDORED.md`, and it follows their shape deliberately. It
also differs from both in a way that is worth stating in the first paragraph:
**the local delta to the source is nothing at all.** `src/` and `tests/` are
byte-for-byte the upstream tree at the pinned commit. This is a pinned
snapshot, not a fork — the only file edited is `Cargo.toml`, where the change
is three removals and two lint tables, and everything else is a whole file or
directory dropped for not belonging in a dependency.

## Provenance

| Field | Value |
| --- | --- |
| Package | `bzip2-rs` |
| Version | `0.1.2` — upstream's own declared version at the pinned commit, and **not** the 0.1.2 the registry has. See [Why not the published release](#why-not-the-published-release). |
| Source | git, <https://github.com/paolobarbolini/bzip2-rs> |
| Pinned commit | `2be7684b444987d6cc7b0b226bda7581be739e3c` ("fix: nightly build"), **2024-12-28** — and still the tip of upstream `main` on 2026-08-12 |
| Author | Paolo Barbolini `<paolo@paolo565.org>` |
| License | MIT **OR** Apache-2.0 — both texts ship with the crate and are in `LICENSE-MIT` and `LICENSE-APACHE` next to this file |

Unlike the two sibling directories, nothing had to be reconstructed: upstream
ships both license files, so this is redistribution of the notices as given.

**Upstream is quiet.** The pinned commit is the tip of `main` and is nineteen
months old; the last release is from 2021. That is a reason the snapshot is
pinned by hash rather than tracked, and it is the reason the *Upstream* section
at the bottom asks for a release rather than offering a patch. It is not a
reason against the change — the decoder is finished work with a fuzzing
harness and a differential test against libbzip2 behind it — but somebody
reading this file in a year should know they are not waiting for anything.

## Why this exists

`Record::decompress` in `vendor/nexrad-data` is where a Level II volume decode
spends **98–99% of the instructions it retires at all** — measured, below —
and it was reaching libbzip2 through the `bzip2` crate, whose backend is
`libbz2-rs-sys`, a c2rust translation of the 1996 C. `bzip2-rs` is the same
algorithm written as Rust, and on this workspace's own data it retires **a
third fewer instructions for byte-identical output**.

Measured on this machine, 32 cores, release profile with `lto = true`,
`opt-level = 3`, `strip = true`, one instrument source built twice and run
interleaved with the 1-minute load average under 20. Instructions retired via
`perf stat -e instructions:u`, differenced across 2 and 6 repetitions so
process setup, the file reads, the gunzipping of legacy volumes and the record
splitting all cancel:

| | before (`bzip2` 0.6.1 → `libbz2-rs-sys` 0.2.5) | after (`bzip2-rs` `2be7684`) | |
| --- | --- | --- | --- |
| decompress pass, 8 volumes / 668 records / 421 MB out | 44,809,807,445 | 29,975,059,883 | **−33.10%** |
| end-to-end decode, dense volume (KFTG, 16.9 MB) | 7,883,148,609 | 5,313,317,394 | **−32.60%** |
| end-to-end decode, light volume (KDMX, 1.4 MB) | 1,041,455,806 | 865,078,476 | **−16.94%** |
| **control** — full decode of 4 volumes with no compressed records | 19,261,333 | 19,261,451 | **+0.0006%** |

The share those first two rows imply is worth stating on its own, because it is
larger than the number this workspace had been quoting. Decompression alone,
for the dense volume, is 7,806,188,828 of the 7,883,148,609 its whole
end-to-end decode retires — **99.0%** before, 98.5% after. `rustdar-radar`'s
`scan::decoded` used to say 92%; that figure came from `perf record` sample
shares, which are cycles and not instructions, and decompression retires its
instructions at a lower IPC than the Message 31 parse does. Both numbers are
true of different quantities and the comment there now says which is which.

Each row is the mean of two independent measurements taken in alternation; the
two agree to within 315 instructions out of 44.8 billion on the decompress row,
which is the precision the percentages are quoted at.

The control is the load-bearing row and it earned its place in the sibling
directory before it earned it here: it is a full decode of the four legacy CTM
volumes, which contain **no compressed records at all** — every code path
except decompression — and it does not move. Anyone repeating this should keep
it and disbelieve any run in which it moves.

Wall clock, seven runs each, interleaved before/after, same corpora:

| | before, median | after, median | | before, best | after, best | spread (max−min) |
| --- | --- | --- | --- | --- | --- | --- |
| decompress pass | 4337.4 ms | 3477.0 ms | **−19.8%** | 4327.7 | 3430.7 | 70.3 / 71.6 ms |
| e2e dense (parallel, 32 threads) | 56.8 ms | 47.2 ms | **−16.9%** | 52.6 | 44.2 | 7.5 / 5.6 ms |
| e2e light (parallel, 32 threads) | 13.2 ms | 11.3 ms | **−14.3%** | 12.6 | 10.6 | 2.0 / 3.3 ms |

**Wall clock moves less than instructions do, and that is the honest figure to
quote for anything a user feels.** −33% of instructions buys −20% of seconds on
the same pass, so `bzip2-rs` retires its smaller instruction count at a lower
IPC than the translated C does — unsurprising for a decoder whose remaining
cost is a cache-hostile inverse Burrows-Wheeler walk and an unpredictable
Huffman loop. The instruction count is the load-insensitive number and is what
the table above leads with; the second table is what the pipeline actually
feels.

The light volume is the other honest row. Its end-to-end gain is half the dense
one's, because a 1.4 MB volume spends proportionally less of its decode inside
decompression — the smaller the volume, the more of the decode is Message 31
parsing and radial construction that this change does not touch.

### The other reason, which is not a number

`bzip2-rs` is `#![forbid(unsafe_code)]`. `libbz2-rs-sys` is a machine
translation of a 1996 C decompressor that this workspace runs on every byte it
downloads from an S3 bucket, on the parallel path, and in a browser. Replacing
it with a Rust implementation that cannot express an out-of-bounds write is
worth having independently of the instruction count, and it is why the
alternative that measured *faster* was rejected. The research pass that
surveyed the field reports `lbzip2` panicking on 25 of 6,000 mutated records at
an out-of-bounds index — **their** figure, not re-measured here, recorded so
the rejected candidate is not revisited without knowing why. `bzip2-rs` panics
on **none** of the 6,000 mutations run here; see
[Malformed input](#malformed-input) below.

## Why not the published release

The registry has a `bzip2-rs 0.1.2` and this directory declares `0.1.2`, and
those are not the same code. Upstream has not published since, so the version
in `Cargo.toml` is simply upstream's own, left untouched rather than invented.
It is recorded here so nobody reads the manifest and concludes the registry
copy would do.

The pinned commit is **57 commits** past `359904e "cargo: release v0.1.2"`.
`diff -rq` against the crates.io tarball of 0.1.2:

- 0.1.2 has no `src/decoder/parallel/`, no `src/threadpool.rs` and no
  `src/decoder/state.rs`; it keeps the block decoder at `src/block/` rather
  than `src/decoder/block/`.
- Nine of the files present in both differ, 560 changed lines against a 1,535
  line crate — the crate roughly doubled, to 2,375 lines.
- Among the changes are ones that are not optional for a decoder run on
  network input: `9bcf62c` "block: fix panic when calling `ArrayVec::set_len`
  with a too high of a number" and `7f9f60e` "move_to_front: `decode_small`
  takes n < 6 not n <= 6". `tests/decode_reader.rs::trees_overflow` is the
  regression test for the first of those and it is kept here.

So depending on the registry version would mean shipping a decoder with known
panics on malformed input, which is precisely the property this change was
chosen for. That is the whole justification for pinning a commit instead.

### And why vendored rather than a git dependency

The same reason as the two sibling directories: a `git = "…"` dependency puts
this workspace's build on somebody else's branch continuing to exist and
continuing to have that commit. A vendored copy is a build that still works
when the repository is renamed, force-pushed or deleted, and its content is
reviewable in the same diff as the change that uses it.

## Why there is no `[patch.crates-io]` entry

This is the one structural difference from `vendor/nexrad-decode` and
`vendor/nexrad-data`, and it is worth a section because the absence looks like
an oversight.

Both siblings are patched because something *else* in the graph resolves the
same name — `nexrad-data` depends on `nexrad-decode`, and four crates here
depend on `nexrad-data` — so a plain path dependency would leave a second,
unpatched registry copy in the build serving everything that reached it by
version. `[patch]` redirects the source, so one crate serves every path.

Nothing else in this graph resolves `bzip2-rs`. `vendor/nexrad-data` is its
only dependent, and it names it by path. There is no second copy to unify with,
so there is nothing for a patch entry to do — and writing one anyway would
claim, in the lockfile, that this snapshot *is* the registry's 0.1.2, which the
section above says it is not.

The check that must keep answering the same way:

```console
$ cargo tree --workspace --all-features -i bzip2-rs
bzip2-rs v0.1.2 (…/vendor/bzip2-rs)
└── nexrad-data v1.0.0-rc.7 (…/vendor/nexrad-data)
```

One node, and it names a path.

## The `bzip2` dependency that stays, and where it went

`bzip2-rs` is a **decoder**. It has no encoder, and upstream does not intend to
add one.

`vendor/nexrad-data/src/volume/record.rs`'s `decompress_bound_tests` build
their fixtures by *compressing*: a record at exactly the 16 MiB ceiling, one
byte past it, a 256 MiB bomb that is under 4 KB on the wire. So `bzip2` stays,
moved from `[dependencies]` to `[dev-dependencies]` and pinned `=0.6.1` to
match `nexrad-level3`.

That it reaches nothing shipped was **checked rather than assumed**, because
"dev-dependencies do not ship" is the kind of claim that is true until a
`[build-dependencies]` or a feature unification makes it false:

```console
$ cargo tree -p nexrad-data -e normal -i bzip2
warning: nothing to print.

$ cargo tree -p nexrad-data -e normal,dev -i bzip2
bzip2 v0.6.1
[dev-dependencies]
└── nexrad-data v1.0.0-rc.7 (…/vendor/nexrad-data)
```

(A `warning:` and exit 0, not an error — `cargo tree -i` errors only when the
package is absent from the lockfile entirely, and `bzip2` is still in it. The
distinction matters if anyone scripts this check: assert on the *output*, not
on the exit status.)

`rustdar-radar/tests/bzip2_backend.rs` pins the arrangement, because losing it
is invisible: moving that one line back up to `[dependencies]` compiles, passes
every other test, and quietly puts `libbz2-rs-sys` back into the wasm bundle
and back onto the decode path.

## The `bzip2-sys` unification hazard

Recorded here because it is the reason the guard test above exists, and because
this change *narrows* the hazard without closing it.

`bzip2` picks its backend with a feature, not with a dependency:

```rust
// bzip2-0.6.1/src/lib.rs:59
#[cfg(feature = "bzip2-sys")]      // C libbzip2
#[cfg(not(feature = "bzip2-sys"))] // libbz2-rs-sys
```

Under `resolver = "2"` features unify across everything resolving the same
package for the same target, so one crate anywhere in the graph asking for
`bzip2/bzip2-sys` switches **every** user of `bzip2` to the C library at once —
silently on desktop, and fatally on `wasm32-unknown-unknown`, which cannot link
a C archive.

Before this change, that would have switched the Level II decode path. It no
longer can: `bzip2-rs` has no backend feature and cannot participate. What is
still exposed is `nexrad-level3`, which keeps `bzip2 = "=0.6.1"` — see the next
section. `rustdar-radar/tests/bzip2_backend.rs` fails loudly if `bzip2-sys` or
`libbz2-sys` ever appears in `Cargo.lock`, which is the exact fingerprint of
the feature having been enabled, and it fails on a developer machine before the
wasm CI row gets to.

## What was deliberately not changed

### `nexrad-level3` still uses `bzip2`

`nexrad-level3/src/decode/mod.rs:146` runs `bzip2::read::BzDecoder` over the
BZ2 tail of a compressed Level III product, and it is left alone.

The swap is two lines and would remove `bzip2` and `libbz2-rs-sys` from the
shipping graph entirely — a real prize, since it would make the hazard above
structurally impossible rather than merely guarded. It is not taken here for
one reason: **there is no Level III corpus on this machine to prove
bit-identity against.** Level III products are fetched live from the NWS, the
Level II corpus proves nothing about them, and this change's whole standard of
evidence is 8.7 GB of byte-identical output. A decoder swap with no corpus
behind it is not the same change as the one this directory is carrying, however
similar the diff looks.

It is the obvious next commit, and it needs a Level III corpus first, not more
reasoning.

### The whole-stream CRC is not verified

`src/decoder/block/mod.rs:186` reads:

```rust
let _crc = reader.read_u32().ok_or_else(|| BlockError::new("whole stream crc truncated"))?;
// TODO: check whole stream crc
```

libbzip2 checks both the per-block CRC and the combined stream CRC; `bzip2-rs`
checks **every block's** CRC (`self.expected_crc == crc`, one block above) and
reads the stream CRC without comparing it.

This cannot cost correctness, and the reason is worth stating rather than
assuming: the stream CRC is a **fold over the per-block CRCs**, so it is
redundant with checks that all ran before any byte reached the caller. Skipping
it can only cost a *rejection* — a stream whose blocks are individually intact
but whose stored stream CRC disagrees — never buy a wrong answer. See
[The safety argument](#the-safety-argument-which-does-not-depend-on-enumerating-anything).

It is still a real difference from what the workspace ran before, it is written
down rather than left for someone to find, and it is the obvious upstream
contribution from here.

### `DecoderReader` re-copies the compressed bytes

`DecoderReader::read` pulls from its inner reader 1024 bytes at a time into a
stack buffer and appends them to a `VecDeque`. The input at the call site is
already a `&[u8]` in memory, so this is a copy of every compressed byte that a
`Decoder` driven directly would not make, plus up to `blockSize100k × 100k` —
900 KB for the `BZh9` streams NEXRAD uses — of compressed input buffered before
the first block decodes.

Left alone, deliberately: the whole win above was measured *through*
`DecoderReader`, so it is already paid for, and the alternative is a hand-rolled
`Decoder` loop in `record.rs` that would have to re-derive the ceiling logic.
Named here so it is not rediscovered as a surprise, and because it is the next
thing to try if this function is ever revisited.

### The `nightly` cfg arms stay in the source

Removing the `nightly` *feature* from `Cargo.toml` (see below) leaves eighteen
lines across four files naming `feature = "nightly"` — thirteen positive arms
and five `not(...)` ones — that can no longer select either way. They
are deliberately not deleted: leaving them is what keeps `src/` byte-identical
to upstream, which is the claim this whole file rests on. `unexpected_cfgs =
"allow"` in the lint table is what stops cargo's check-cfg from warning about
them.

## Local changes

This is the complete list. `diff -rq --exclude=.git` against a fresh checkout
of the pinned commit must produce exactly these entries and nothing else — and
as of the vendoring commit it produces exactly these eight lines:

```console
Only in <upstream>: benches
Only in <upstream>: Cargo.lock
Files <upstream>/Cargo.toml and vendor/bzip2-rs/Cargo.toml differ
Only in <upstream>: fuzz
Only in <upstream>: .github
Only in <upstream>: .gitignore
Only in <upstream>: rustfmt.toml
Only in vendor/bzip2-rs: VENDORED.md
```

**Nothing under `src/` or `tests/` appears in that list.** That is the claim
this whole file rests on, and it is a `diff` and not an assurance.

### Removed — targets that cannot compile or that nothing here runs

| Path | Why |
| --- | --- |
| `benches/decoder_reader.rs` + the `[[bench]]` block + dev-dependency `criterion` | Nothing in this workspace runs them, and `--all-targets` would compile criterion on every CI row including wasm32. Same call as both sibling directories. |
| dev-dependency `bzip2 = ">= 0.4.1, <0.6"` | Used only by that bench, and it would have put a **second** bzip2 in this workspace's lockfile: the range excludes the `=0.6.1` everything else here pins, so the two cannot unify. A benchmark's baseline is not worth a duplicated compression library on every CI row. |
| `fuzz/` (4 targets + its own `Cargo.toml`) | A separate cargo project requiring `cargo-fuzz` and a nightly toolchain; `rust-toolchain.toml` pins stable and nothing here invokes it. Its existence is evidence about upstream's malformed-input discipline and is cited under [Malformed input](#malformed-input) rather than carried. |
| `.github/workflows/ci.yml`, `.gitignore` | Upstream's CI and upstream's checkout layout. |
| `Cargo.lock` | A dependency's lockfile is inert here; this workspace's root `Cargo.lock` is the one that resolves anything. |

### Removed — `rustfmt.toml`

Upstream's sets `edition = "2018"` (which cargo passes anyway, from the
manifest) and three **nightly-only** options — `format_code_in_doc_comments`,
`group_imports = "StdExternalCrate"`, `imports_granularity = "Module"`.

On the stable toolchain this repository pins, rustfmt cannot honour any of the
three. It does not fail; it prints

```text
Warning: can't set `group_imports = StdExternalCrate`, unstable features are only available in nightly channel.
```

nine times per `cargo fmt --all` — three options × three targets — on every
developer machine and every CI fmt row, for a file that changes nothing.

Deleted rather than kept, and `cargo fmt --all --check` is clean and silent
over this directory either way (checked both ways before deciding). Keeping it
would also have been a latent trap: if this workspace ever moved to a nightly
rustfmt, this one directory would silently start formatting by different rules
from the rest of the tree.

### Removed — the `nightly` feature

Upstream declares

```toml
nightly = ["crc32fast/nightly"]
```

and `src/lib.rs` answers it with

```rust
#![cfg_attr(feature = "nightly", feature(read_buf, core_io_borrowed_buf))]
```

`#[feature]` off the nightly channel is **E0554, a hard error**, and
`rust-toolchain.toml` pins `stable`. A *declared* feature is reachable from
`--all-features`, which this workspace's CI passes to `cargo clippy
--all-targets --all-features -- -D warnings` and to `cargo test --workspace
--all-features`. Leaving it declared would mean a workspace that cannot be
linted at all the moment this crate became a member.

Removing the declaration is the smallest fix that works and the only one that
keeps `src/` untouched — see *The `nightly` cfg arms stay* above. `crc32fast`
loses nothing it could have used on stable.

One consequence, stated because it is a test that stops running:
`tests/decode_reader.rs::bad_num_selectors` carries `#[cfg(feature =
"nightly")]` and therefore never compiles here. It did not compile here before
either — the feature was never enabled — so nothing regressed, but the file now
contains a test that can no longer be switched on without editing the manifest.

### Added — the two lint tables

Same mechanism and same reason as both sibling directories: `clippy.yaml` runs
`cargo clippy --all-targets --all-features --fix` over every workspace member
and **auto-commits the rewritten tree to `main`**, then gates on `-D warnings`.
Without the tables a bot would eventually rewrite upstream source here on its
own schedule and the list above would silently stop being true.

The limit is the same too — a lint attribute in source outranks any
command-line level, so the tables cannot reach `src/lib.rs`'s own
`#![deny(trivial_casts, trivial_numeric_casts, rust_2018_idioms,
clippy::cast_lossless, clippy::doc_markdown, missing_docs,
rustdoc::broken_intra_doc_links)]`.

**Here that costs nothing**, which is the one place this directory got off
lighter than its siblings. Both of those needed a `#![cfg_attr(not(test),
deny(…))]` source edit, because upstream's own test code broke upstream's own
`unwrap_used`/`expect_used` denies. `bzip2-rs` denies nothing its tests break,
so `cargo clippy -p bzip2-rs --all-targets --all-features -- -D warnings` is
clean as vendored, with **no source edit at all**.

### Added — `VENDORED.md`

This file.

## Bit-identity

The bar, and it is met exactly.

An FNV-1a 64 digest over every decompressed LDM record — each record's length
as a little-endian `u64` followed by its bytes, files walked in sorted order —
computed by one instrument source built twice, differing only in which decoder
`Record::decompress` calls:

| | |
| --- | --- |
| corpus | `~/.cache/rustdar-nyq-corpus/{storm,vols,holdout}` + `/home/reddragon/projects/nexrad/downloads/` |
| files read | **176** — 158 archive volumes plus 18 downloads |
| files **contributing** to the digest | **172**, WSR-88D and TDWR, including the `TORD20200810_*` TDWR pair. The other four are the gzip-wrapped legacy CTM volumes back to 1991: they hold **zero** compressed records, so they add nothing here. They are corpus breadth for the *control* row, where having no decompression at all is the point, and they are not decompression coverage. |
| compressed records | **10,063** |
| decompressed bytes | **8,700,758,040** |
| errors | **0** on both sides |
| digest, `bzip2` 0.6.1 | `0xe85000caa3a58fef` |
| digest, `bzip2-rs` `2be7684` | `0xe85000caa3a58fef` |

The record and byte totals are the same 10,063 / 8,700,758,040 an independent
research pass reached over the same corpus, which is a second opinion on the
corpus rather than on the decoder.

## Malformed input

The property that decided this, and the one that disqualified the faster
alternative.

6,000 mutated records per side, generated from 512 real compressed records by a
seeded xorshift so the corpus is reproducible, in four equal families:
truncation to a random length, one to eight single-bit flips, a splice of two
records at a random point, and a valid stream with 1–64 random bytes appended.
The last family is meant to *succeed* — it is where two decoders can
legitimately disagree about where a stream ends — and all 1,500 of its cases
decompress identically on both sides. The four-byte LDM prefix and the `BZ`
marker are restored after every mutation, so each case reaches `decompress` as
something the caller has already accepted. Every call runs inside
`catch_unwind` under `panic = "unwind"`, so a panic is counted rather than
fatal, and every accepted result is digested per case so "same decision" and
"same bytes" are checked separately.

| | `bzip2` 0.6.1 | `bzip2-rs` `2be7684` |
| --- | --- | --- |
| decompressed anyway (`Ok`) | 1,500 | 1,501 |
| refused (`Err`) | 4,499 | 4,498 |
| — of which `RecordTooLarge` | 1 | 1 |
| **panics** | **0** | **0** |

Also run against a **dev**-profile build, because `bzip2-rs` carries
`debug_assert!`s that a release build does not evaluate — `skip_bits < 8`, and
two on the shape of the input `VecDeque`'s slices in `Decoder::read`. Same
6,000 mutations, byte-for-byte the same outcome as the release build, **0
panics**, so none of those assertions is reachable from a malformed record.

### The safety argument, which does not depend on enumerating anything

State this first, because everything below is an illustration of it and not a
substitute for it.

**`bzip2-rs` verifies every block's CRC32 before returning that block's bytes.**
`decoder/block/mod.rs`, at the point the block is exhausted:

```rust
let crc = self.hasher.finalize();
return if self.expected_crc == crc { Ok(0) } else { Err(BlockError::new("bad crc")) };
```

The hasher is fed every byte handed to the caller, and `expected_crc` is the
32 bits the compressor stored in the block header. So:

> **accepts ⟹ every block's CRC32 verified ⟹ the bytes cannot be silently
> corrupt, short of a CRC32 collision.**

That is the whole safety property, and it holds for inputs nobody has tried.
It is also why the unchecked whole-stream CRC noted above cannot mask corrupt
data: the stream CRC is a **fold over the per-block CRCs**, so it is redundant
with checks that all ran. Not checking it can cost a *rejection*; it cannot buy
a wrong answer.

What follows is measurement confirming that, plus the shape of the
disagreements, which is worth knowing even though it is not what the argument
rests on.

### The disagreements, measured

The two decoders disagree only about **which corrupt records to refuse**, never
about what a record means.

Across the 6,000 mutations here and an independent replication that ran 7,500
random mutations **plus exhaustive single-bit sweeps of entire records from four
sites** — KFTG 18,368 bit positions, TORD 2,088, KDMX 18,680, KLWX 32,768, some
79,400 malformed inputs in all:

| | |
| --- | --- |
| both accept, **different bytes** | **0**, in every run |
| panics, either decoder | **0** |

That zero is the number to read. Everything else is a difference of strictness.

**It runs in both directions.** The account here originally gave only one, which
was an artefact of a 6,000-case random sample:

- `bzip2-rs` accepts where `libbzip2` refuses — the case dissected below.
- `libbzip2` accepts, with correct bytes, where **`bzip2-rs` refuses**: 22 bit
  positions on TORD, 16 on KDMX, 10 on KLWX, clustered contiguously (around
  bytes 67–77 on TORD, the selector/MTF table). Strictly safe — an error rather
  than wrong data — and it means the change can also turn a record that used to
  decode into a reported bad record. On a corrupted input either way.

**It is position-dependent, not a rate.** "About 1 in 6,000 single-bit
corruptions" appeared in an earlier draft of this file and was wrong in kind: it
described the corpus, not the decoder. Divergence is concentrated in the block
header. Swept exhaustively on KDMX, **300 of 18,680 bit positions diverge —
1.6%**, roughly a hundred times the sampled figure, and the remaining 98.4% of
positions never diverge at all.

**One class is deterministic and pinnable.** A flip in any of the trailing four
payload bytes gives a missed rejection **4 times out of 4 byte positions**,
100% reproducible. So the mechanism *is* identifiable in general, even though
the specific instance dissected below is a different one.

### The instance that was dissected

Kept because it is worked all the way through, and because it shows what
"missed rejection" means concretely.

One bit-flipped record: `bzip2` refuses it, `bzip2-rs` accepts it and returns
1,194,720 bytes. **Those bytes are byte-identical to the correct decompression
of the unmutated record**, established three ways with the same SHA-256:
`bzip2-rs` on the mutant, `bzip2-rs` on the seed, and system `bzip2 1.0.8` on
the seed.

- Exactly one byte of the 258,896-byte stream differs from the seed, at offset
  1642, one bit (`0x8A` → `0xCA`).
- The stream is a single block; `bzip2 -tvvvv` on the seed prints one block and
  matching combined CRCs, and on the mutant prints
  `[1: huff+mtf data integrity (CRC) error in data`.
- `bzip2-rs`'s block CRC check *passed* — necessarily, since the decoded bytes
  are the original ones.

So the two implementations resolve this particular corruption differently:
libbzip2 decodes it to something else and its block CRC fails, `bzip2-rs`
decodes it to the original and its block CRC succeeds. This one is **not** the
trailing-four-bytes class above, and its exact field was not pinned down; that
is stated rather than guessed.

### What it means for this workspace

A record corrupted in transit may be accepted where it used to be rejected, or
rejected where it used to be accepted, depending on where the corruption lands.
Neither direction can produce wrong data, by the CRC argument at the top. Over
10,063 real records the two decoders produce identical output, so reaching any
of this needs corruption, and the archive path is S3-over-TLS.

Upstream runs four cargo-fuzz targets over exactly this surface
(`decompress`, `decompress_parallel`, and two that differential-test against
libbzip2). Not vendored — see *Local changes* — but named here because it is
why this result is unsurprising rather than lucky.

## Peak RSS

`bzip2-rs` allocates the same `blockSize100k × 100000 × 4` per record that
libbzip2 does, and makes roughly twice as many allocation calls doing it, so
the memory question is **not** answered by the instruction count and had to be
measured separately. It was, and it comes out the right way.

Peak RSS (`VmHWM`), parallel decompress of the 8-volume storm corpus, means of
five runs each, interleaved:

| threads | before | after | |
| --- | --- | --- | --- |
| 1 | 51.1 MB | 50.7 MB | −0.8% |
| 4 | 67.1 MB | 65.2 MB | −2.8% |
| **32** | **216.4 MB** | **190.3 MB** | **−12.1%** |

The shape is the one `vendor/nexrad-data/VENDORED.md` established for the
starting-capacity change and for the same reason: at one worker the peak is the
decoded output and the decompressor is noise, and only as the pool grows does
per-worker live decompressor state become the peak. More allocation *calls*
turn out not to mean more live bytes — `bzip2-rs` holds less at once.

RSS figures carry a few percent of spread between runs, unlike the instruction
counts, which reproduce to within 315 out of 44.8 billion. The −12.1% row is
well outside that spread; the −0.8% row is not, and should be read as "does not
move".

## rustfmt

`cargo fmt --all --check` is clean over this directory as vendored, checked
with the toolchain `rust-toolchain.toml` pins — which is `stable`, i.e. a
**floating** version. rustfmt's output is version-dependent, and
`vendor/nexrad-decode/VENDORED.md`'s note on what to do when a future stable
release disagrees applies here unchanged: rebase onto a newer upstream release
that has the fix (best — it deletes this directory), or accept the reformat as
a single, separately-titled commit that touches nothing else, and say so here.

Note that this directory is now formatted by the **workspace's** default rules
rather than upstream's, because upstream's `rustfmt.toml` was deleted (see
above). It happens to be a no-op today — the tree as upstream wrote it is
already clean under the default rules — but a future upstream commit formatted
with `imports_granularity = "Module"` could arrive needing a reformat here.

## Two other bots that can reach this directory

- **Renovate** — already handled, and not by this commit. `.github/renovate.json`
  lists `vendor/**` in `"ignorePaths"` because of the first sibling directory,
  and that glob covers this one too. Nothing to do; recorded so that the next
  person does not go looking.
- **Coverage** — not handled, named so it is not a surprise. `cargo llvm-cov
  --all-features` now measures this crate as well — about 2,375 lines of
  decoder joining the denominator, covered to the extent upstream's own five
  test targets cover them — and the badge and `coverage-baseline.tsv` it
  auto-commits move accordingly. Nothing gates on a threshold, so nothing fails.

## The dependency graph

**Exactly two new packages**, both pure Rust. `cargo tree --workspace
--prefix none`, package names and versions, sorted and de-duplicated, against
a pristine `main` checkout:

| flags | main | branch | difference |
| --- | --- | --- | --- |
| default | 459 | 461 | `+ bzip2-rs`, `+ tinyvec`. **Nothing else.** |
| `--all-features` | 468 | 470 | the same two |

| package | why | C? |
| --- | --- | --- |
| `bzip2-rs` | this directory | `#![forbid(unsafe_code)]` |
| `tinyvec` | the decoder's on-stack Huffman and selector arrays | `#![forbid(unsafe_code)]` |

`crc32fast` (the per-block CRC) and `rayon-core` (only under this crate's
`rayon` feature, which nothing here enables) are **not** new — both were
already resolved on `main`, `crc32fast` through flate2 and `rayon-core` through
`nexrad-data/parallel`, so they do not appear in the difference.

**`Cargo.lock` gains one `[[package]]` block, not two**, and the two counts are
both right. `tinyvec 1.12.0` was already *locked* — `quinn-proto`, in reqwest's
HTTP/3 stack, depends on it — but it was not resolved into the host build tree,
which is what `cargo tree` reports. So the lockfile diff is a single new block
for `bzip2-rs`, and the build gains two crates. Stated because a reviewer
comparing the two numbers would otherwise have to work this out.

### No new C toolchain

This matters because a review in this campaign blocked a change for silently
pulling in `aws-lc-sys` — CMake, C and assembly — onto the macOS and Windows
release rows. Run whole-workspace and `--all-features` on both sides:

```console
$ cargo tree --workspace --all-features -i aws-lc-sys
error: package ID specification `aws-lc-sys` did not match any packages   # main and branch
$ cargo tree --workspace --all-features -i bzip2-sys
error: package ID specification `bzip2-sys` did not match any packages    # main and branch
```

`cc` **is** in the graph, and was before this change — `ring`'s build script,
via rustls, which `rustdar-radar/Cargo.toml` already names as "the workspace's
one C dependency". Identical package (`cc v1.4.0`) and identical path on both
sides, so it is not a difference. Stated rather than omitted, because an
earlier draft of this section claimed `cc` did not resolve at all, checked
that claim, and found it false.

### What is still on the wasm bundle, and what it costs

`cargo tree -p rustdar-web -e normal --target wasm32-unknown-unknown` now
resolves **both** decoders:

| | |
| --- | --- |
| `bzip2-rs` | `nexrad-data` → `Record::decompress`, the Level II path |
| `bzip2` + `libbz2-rs-sys` | `nexrad-level3` → `decompress_after_pdb`, the Level III path |

That is the price of leaving `nexrad-level3` alone, and it is a real one: the
web bundle carries two bzip2 decoders where it used to carry one. Measured,
`cargo build --release -p rustdar-web --target wasm32-unknown-unknown`, before
`wasm-opt`:

| | bytes |
| --- | --- |
| branch point | 14,453,841 |
| this branch | 14,479,445 |
| | **+25,604 (+0.18%)** |

Small, and smaller than a standalone `bzip2-rs` module (~86 KB) because most of
what the decoder needs is already linked. But it is a cost and not a saving,
and it should not be there. Converting `nexrad-level3` — two lines — removes
`bzip2` and `libbz2-rs-sys` from the graph outright, which would turn that
`+25,604` into a net reduction *and* make the `bzip2-sys` hazard structurally
impossible instead of merely guarded. It waits on a Level III corpus; see
*What was deliberately not changed* above.

## Removing this directory

The snapshot is meant to be temporary. When a published `bzip2-rs` contains the
pinned commit's decoder:

1. Replace `[dependencies.bzip2-rs] path = "../bzip2-rs"` in
   `vendor/nexrad-data/Cargo.toml` with `bzip2-rs = "0.1.3"` (or whatever the
   version is). That is the upstream-suitable form and the one the eventual
   nexrad-data pull request should carry.
2. Delete `vendor/bzip2-rs/`.
3. Delete `"vendor/bzip2-rs"` from `[workspace.members]` and
   `[profile.dev.package.bzip2-rs]` in the root `Cargo.toml`.
4. Relax `rustdar-radar/tests/bzip2_backend.rs`'s manifest assertions to match —
   the lockfile assertion in the same file is about `bzip2-sys` and stays
   regardless.
5. `cargo tree --workspace -i bzip2-rs` should show one registry node.

## Upstream

> **TODO — nothing filed.** Two things belong upstream and neither is a change
> to this directory's source, because there is none:
>
> - **A release.** The single most useful outcome is a `0.1.3` cutting the 64
>   commits since 0.1.2 — including two panic fixes — which deletes this
>   directory outright. That is an issue, not a patch.
> - **The whole-stream CRC**, `src/decoder/block/mod.rs:186`. It is upstream's
>   own `TODO` and the evidence for why it matters is in *What was deliberately
>   not changed* above.
>
> Separately, `vendor/nexrad-data`'s move onto this crate is a change to *that*
> directory and belongs in *its* pull request; `vendor/nexrad-data/VENDORED.md`
> carries it.
