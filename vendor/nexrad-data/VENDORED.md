# nexrad-data, vendored

This directory is a copy of a crates.io crate that this workspace maintains
locally. It is not our code. Everything in it is upstream's except what is
listed under [Local changes](#local-changes) below, and keeping that list short
and true is the whole point of the file — it is what makes the directory
reviewable, and what makes the eventual upstream pull request a diff somebody
can read.

It is the second such directory. `vendor/nexrad-decode/VENDORED.md` is the
first, and this one follows its shape deliberately; where the two interact,
this file says so rather than repeating it.

## Provenance

| Field | Value |
| --- | --- |
| Package | `nexrad-data` |
| Version | `1.0.0-rc.7` — the newest published version as of 2026-08-11, and the version this workspace already pinned |
| Source | crates.io |
| Unpacked from | `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/nexrad-data-1.0.0-rc.7/` |
| Upstream repo | <https://github.com/danielway/nexrad>, subdirectory `nexrad-data` |
| Author | Daniel Way `<contact@danieldway.com>` |
| License | MIT — full text in `LICENSE` next to this file |

The packaged tarball ships **no** license file, only `license = "MIT"` in the
manifest. `LICENSE` here is the same reconstruction the sibling directory uses,
copyright line included, because "MIT" in a metadata field is a declaration and
not the notice the license requires us to redistribute.

## Why this exists

`Record::decompress` (`src/volume/record.rs`) is four lines long and has no
upper bound on what it will allocate.

```rust
let mut decompressed_data = Vec::new();
BzDecoder::new(data).read_to_end(&mut decompressed_data)?;
```

**That is the reason, and it is sufficient on its own.** This function is also
the dominant cost of the entire decode path, and the fork improves that
measurably — but by 0.039% of instructions, which would not justify a fork by
itself. The ceiling would. Read the two sections below in that order and in
that proportion.

### The bound

`read_to_end` on a `BzDecoder` reads until the stream ends. Every byte reaching
it was downloaded over the network from an S3 bucket, and a bzip2 stream's
expansion ratio is not bounded by anything the caller can see — the most
expansive *real* record in the corpus below is 1,363:1. A corrupt or hostile
record is therefore an out-of-memory abort with nothing in between, on a path
that runs on every volume open, every timeline scrub and every frame of a loop
download, and on the web on the browser's main thread. This is the change the
directory exists for; the rest is why it pays for itself.

### The cost

Measured on this machine over 12 volumes (5 WSR-88D, 7 TDWR), 693 compressed
records, release profile with `lto = true`:

| | allocations | bytes allocated | output | amplification |
| --- | --- | --- | --- | --- |
| KFTG20230622 | 456 + 1,372 reallocs | 333.9 MB + 237.0 MB realloc | 75.4 MB | **7.6×** |
| KCRP20170826 | 411 + 1,198 reallocs | 300.9 MB + 141.0 MB realloc | 52.9 MB | **8.4×** |
| TBOS20260810 (TDWR) | 246 + 624 reallocs | 179.8 MB + 21.5 MB realloc | 8.5 MB | **23.7×** |

Two mechanisms, both per record:

- **One `BzDecoder` per record.** libbzip2 allocates a `tt` array of
  `blockSize100k × 100000 × 4` bytes when it reads the stream header, which for
  the `BZh9` streams NEXRAD uses is **3,600,000 bytes**. 333.9 MB ÷ 91 records
  = 3,669,257 bytes per record: the `tt` array and about 69 KB of everything
  else. The workspace resolves `bzip2` 0.6.1, whose backend is `libbz2-rs-sys`
  with the `rust-allocator` feature, so this is a plain Rust allocation and a
  counting `GlobalAlloc` sees every one of them.
- **`Vec::new()` grown by `read_to_end`.** Starting from nothing and doubling
  to ~1.4 MB is ~15 reallocations per record, and the bytes copied are the
  `realloc` column above — 237 MB of `memcpy` on one KFTG volume.

For scale: 26.06 G instructions retired for the decompression of those 12
volumes against 40.9 M for a full decode of a corpus with no compressed records
in it at all.

It also explains a peak-RSS behaviour that had previously been attributed to
the decoder. Peak RSS for a full parallel decode of this corpus:

| threads | peak RSS |
| --- | --- |
| 1 | 101 MB |
| 4 | 114 MB |
| 32 | 246 MB |

The growth is per-worker live decompressor state, not decoded output — 5.77 MB
of it live at once in a single-threaded decompress pass, times the pool.

## Removing this directory

The patch is meant to be temporary. When a published nexrad-data contains the
changes:

1. Delete `vendor/nexrad-data/`.
2. Delete `"vendor/nexrad-data"` from `[workspace.members]`, the
   `[patch.crates-io]` entry, and `[profile.dev.package.nexrad-data]` in the
   root `Cargo.toml`.
3. Bump the `[workspace.dependencies]` pin to that version.
4. `cargo tree -i nexrad-data` should show one registry node again.

## Local changes

This is the complete list. `diff -rq` against the unpacked registry copy must
produce exactly these entries and nothing else.

### Removed — local contamination and packaging residue

| Path | Why |
| --- | --- |
| `default_*.profraw` (2 files) | Coverage output from an unrelated local run that happened to land in the registry checkout. Never part of the crate. |
| `.cargo-ok` | Cargo's own extraction marker. |
| `.cargo_vcs_info.json` | Recorded in the provenance table above instead. |
| `.gitignore` | Upstream's, for upstream's checkout layout. |
| `Cargo.toml.orig` | Upstream's pre-normalisation manifest, which refers to workspace inheritance and sibling `path` dependencies that do not exist here. |
| `Cargo.lock` | A packaged crate's lockfile is inert; this workspace's root `Cargo.lock` is the one that resolves anything. |

### Removed — targets that cannot compile or that nothing here runs

**Seven of the eleven** packaged test targets are gone and **four stay**. The
reason for most of them is the same one the sibling directory ran into:
upstream's tests read files that exist only in upstream's checkout, and the
packaged tarball does not contain them.

| Path | Why |
| --- | --- |
| `tests/volume_header.rs`, `tests/volume_records.rs`, `tests/volume_scan.rs`, `tests/aws_realtime_polling.rs` | All four `include_bytes!("../../downloads/KDMX20220305_232324_V06")`, a file produced by a download step in upstream's repo. Not in the tarball; they cannot compile here at all. |
| `tests/fixture_integration.rs` | `include_bytes!("../../tests/fixtures/…")` × 8, likewise upstream-checkout-only. |
| `tests/live_decode.rs`, `tests/aws_realtime_network.rs` | Download live data from the AWS archive bucket at test time. A workspace test suite that reaches the network is a test suite that fails when the network does. |
| `benches/scan.rs` + the `[[bench]]` block + the `criterion` dev-dependency | Nothing in this workspace runs them, and `--all-targets` would compile criterion on every CI row including wasm32. |
| `examples/` (3 files) + the three `[[example]]` blocks + the `clap` dev-dependency | Same. |
| dev-dependency `env_logger`, dev-dependency `tokio` | Used only by the deleted tests. |

`autoexamples`/`autobenches`/`autotests` are all `false` in the packaged
manifest, so each block had to be deleted together with its file: a block
naming a missing path is a hard error, and a file with no block is dead weight.

Two of the four *kept* targets each had one test reaching for the same missing
`downloads/KDMX20220305_232324_V06`, so keeping the file meant dealing with the
test:

| Test | What happened |
| --- | --- |
| `error_handling.rs::test_compressed_data_decode_error` | **Kept, fixture replaced.** It asserts that `messages()` refuses a compressed record, and that refusal rests on `compressed()` alone — the `BZ` marker at bytes 4..6. A ten-byte synthetic record exercises the same two lines, so the assertion survives intact. The constant is upstream's own: `tests/aws_realtime_types.rs` spells the identical `MINIMAL_BZ_RECORD`. |
| `aws_realtime_types.rs::test_chunk_identifier_next_chunk_intermediate` | **Removed.** It builds an `ElevationChunkMapper` from a real message 5 with real elevation cuts, which cannot be synthesised the way a `BZ` marker can. Removed alone rather than with its file, so the other thirty-odd tests in it still run. |

A comment at each site says the same thing, so neither looks like a silent
edit to somebody reading the test file.

### Kept deliberately

- `tests/malformed_records.rs` + its `[[test]]` block — **the negative-input
  pin on the function this directory exists to change.** Twelve tests over
  `split_compressed_records` and `Record::decompress` with truncated, empty,
  zero-sized and corrupt input. Any change to `decompress` has to leave every
  one of them passing.
- `tests/error_handling.rs` + its `[[test]]` block — the error-surface pin,
  including `decompress` on uncompressed data.
- `tests/aws_realtime_types.rs` + its `[[test]]` block — std-only, and behind
  `#![cfg(feature = "aws")]`, so it compiles to nothing unless something asks
  for `aws` and runs for real under `--all-features`.
- `tests/aws_archive.rs` + its `[[test]]` block — **eleven** plain, synchronous,
  fixture-free, network-free `Identifier` parsing tests, on exactly the same
  terms as `aws_realtime_types.rs` above.

  This file was dropped at first, and the justification given for it — "downloads
  live data from the AWS archive bucket at test time" — was true of only 8 of
  its 19 tests, and those 8 carry upstream's own `#[ignore = "requires AWS
  access"]`, so they never ran anywhere. The real obstacle was narrower: their
  `#[tokio::test]` attribute is what pulled the `tokio` dev-dependency this
  directory drops. So the eight went and the file stayed, which is the same
  surgical call already made one test at a time in `aws_realtime_types.rs`. A
  comment where they stood says so.
- `src/aws/` in full, and the `aws` and `aws-polling` features. Nothing in this
  workspace calls them — `rustdar-radar/src/archive.rs` and `src/chunks.rs`
  reimplement that surface and say so — but deleting a quarter of a crate to
  save a feature nobody enables would make the upstream diff unreadable for no
  gain. Only the `default` line moves; see below.

### Added

| Path | Why |
| --- | --- |
| `LICENSE` | See above — the tarball ships none. |
| `VENDORED.md` | This file. |

### Changed — `Cargo.toml`

Beyond the removals already listed: a vendoring note under the generated
header, one `default` line, and two lint tables.

#### The `default` line

Upstream's is `default = ["aws", "nexrad-model"]`; here it is
`default = ["nexrad-model"]`.

Cargo enables a *selected workspace member's* own default features regardless
of how its dependents declare it, and this workspace's dependents all declare
`default-features = false`. So membership alone would switch on four features
that were off before. Measured both ways with
`cargo tree -e features -i nexrad-data`:

| | resolved features |
| --- | --- |
| upstream `default` | aws, default, nexrad-model, once_cell, parallel, rayon, reqwest, xml |
| this line | default, nexrad-model, parallel, rayon |

Not because the wide set fails to build — that was checked, and
`cargo check -p nexrad-data --features aws --target wasm32-unknown-unknown`
compiles clean in 21 s, because reqwest 0.13 has a wasm backend and elides its
rustls features there. It is dropped because vendoring is supposed to change
nothing, and this one line is what keeps the resolved feature set identical to
what it was when nexrad-data was a plain registry dependency.

#### The lint tables

Same mechanism, same reason, as the sibling directory: `clippy.yaml` runs
`cargo clippy --all-targets --all-features --fix` over every workspace member
and **auto-commits the rewritten tree to `main`**, then gates on `-D warnings`.
Without the tables a bot would eventually rewrite upstream source here on its
own schedule and the *Local changes* list above would silently stop being true.

And the same limit: a lint attribute written in source outranks any
command-line level, so the tables cannot reach `src/lib.rs`'s own
`#![deny(…)]`. That forced one source edit, exactly as it did in the sibling
directory — see *The lint scoping* below.

### Changed — source

#### The ceiling — `src/volume/record.rs`, `src/result.rs`

`Record::decompress` grew an upper bound on what it will produce.

```rust
pub const MAX_DECOMPRESSED_RECORD_BYTES: usize = 16 * 1024 * 1024;
```

A bzip2 stream does not declare its decompressed size, so `read_to_end` on a
`BzDecoder` is unbounded by construction — the only way to find out how big a
record is, is to finish expanding it. Every record this crate decompresses came
off a network, and the most expansive record in the corpus below expands
1,363:1, so "the compressed record is small" implies nothing at all about the
memory the decompression will take. Without a ceiling, a corrupt or hostile
record is an out-of-memory abort, and on the parallel path it is one per worker
at once.

**Where 16 MiB comes from.** Measured headroom over real data, and nothing
else. It was originally justified on two legs; the second one was false and has
been withdrawn.

*Real data.* 10,063 compressed records across 176 volumes, WSR-88D and TDWR,
chosen because the two formats build records differently. Zero rejections.

| | largest record | ceiling is |
| --- | --- | --- |
| any site | 1,424,736 B | **11.8×** larger |
| TDWR | 325,888 B | **51×** larger |

*The withdrawn leg, kept visible so it is not reconstructed.* The claim was
that Archive II packs at most 120 messages per record, so 120 × 131,082 — the
largest a `u16` halfword count plus the 12-byte CTM prefix can express —
= 15,729,840 < 16,777,216 would put the ceiling above anything the format could
say. The arithmetic is right and the premise is not, twice:

| | messages per record, measured |
| --- | --- |
| WSR-88D | **78–127** |
| TDWR | **120–134** |

"120" is the *radial* count; the metadata messages share the record. At 134 the
same arithmetic gives 17,564,988, which is **above** the ceiling. And 131,082
is unreachable anyway — the largest real message measured is **12,160 bytes**
on WSR-88D and 2,432 on TDWR, so a 134-message record of real messages is about
1.6 MB.

So the format does not bound record size, and this ceiling does not claim it
does. The test that asserted the structural leg has been replaced by
`the_ceiling_keeps_real_headroom_over_the_largest_record_measured`, which pins
the thing that is actually true: at least 10× headroom over the largest record
ever measured, with that figure named as a constant so raising it means
measuring again. A second test decompresses a record of exactly that size and
requires it to be accepted.

*Worth having.* A bomb costs 16 MiB per worker and ends in an `Err`, instead of
costing the machine's memory and ending in an abort. At 32 threads that is a
512 MB worst case.

**Mechanics.** `.take(MAX + 1)` before `read_to_end`, then a length check. One
byte past the ceiling, so a record of exactly `MAX` bytes is still data and
anything beyond it is detected without being expanded any further. `Take`
bounds only how much is read per iteration and how much is kept — it does not
reserve its limit, which was checked against `default_read_to_end` in std
rather than assumed — so an ordinary record pays nothing.

The failure is `Error::RecordTooLarge { limit, compressed }`, a new variant, and
not a panic: one bad record in a volume of 50–130 should be a decode that
reports a bad record. No decompressed size is reported because the record is
abandoned at the ceiling rather than expanded to find out how big it really is,
which is the entire point.

**What it cost.** Nothing measurable, which is the other thing that had to be
true. Same 12 volumes, release + `lto`, instructions retired differenced across
2 and 6 repetitions so process setup, file reads and record splitting cancel:

| | instructions per pass | per record |
| --- | --- | --- |
| before | 26,064,467,974 | — |
| after | 26,064,595,110 | **+183** |

+0.0005%, measured on its own before the capacity change landed. Allocation
counts, byte totals and peak live bytes are *identical* —
`diff` on the per-volume allocation table reports no change at all — and so are
the decoded-moment digest and the raw decompressed-bytes digest over all 14
volumes.

New `#[cfg(test)] mod decompress_bound_tests` in `src/volume/record.rs`, an
in-source module so it travels with the fix in the upstream diff. Six tests: a
record of exactly `MAX` bytes decompresses; a record one byte over is
`RecordTooLarge`; a 256 MiB bomb that is under 4 KB on the wire is refused; an
ordinary 2432-byte record round-trips byte-identically; the ceiling keeps 10×
headroom over the largest record ever measured; and a record of exactly that
measured size is accepted.

**The hole this does not close.** `File::decompress`
(`src/volume/file.rs:46`) has the identical unbounded `read_to_end`, on a
`GzDecoder`, for the gzip-wrapped volume files that pre-~2016 archives use —
same network provenance, and a path rustdar takes for legacy `.gz` volumes.
DEFLATE reaches roughly 1032:1, so 16 MB of download expands to ~16 GB. It is
one `.take()` away from being fixed and it is deliberately not fixed here:
its bound is a different number needing its own corpus (whole volumes, not
records), and this directory's changes are scoped to the record path. Named
rather than left silent so the safety claim above is not read as complete. It
is the obvious next commit.

#### The starting capacity — `src/volume/record.rs`

`Vec::new()` became `Vec::with_capacity(INITIAL_DECOMPRESSED_CAPACITY)`, 256 KiB.

`read_to_end` doubles from nothing, so a 1.4 MB record was reached through
about fifteen reallocations, each copying everything accumulated so far — 237 MB
of `memcpy` to produce one KFTG volume's 75 MB of output.

There is nothing exact to size from: a bzip2 stream carries no
decompressed-size hint, and the four-byte LDM prefix is the *compressed*
length. Nor anything approximate — expansion ratios inside a single volume run
from 2.2:1 to 1,363:1, so a multiple of the compressed size is not a
prediction. What is left is a fixed floor chosen from the distribution of real
records: smallest 89,760, p25 191,520, median 245,280, p75 737,760, largest
1,416,480. 256 KiB is the median rounded up to a power of two.

It is also the measured optimum and not just the reasoned one. Three
candidates, same corpus, same method:

| capacity | KFTG reallocs | KFTG amplification | TDWR amplification | peak RSS @32 |
| --- | --- | --- | --- | --- |
| `Vec::new()` | 1,372 | 7.57× | 23.73× | 247.9 MB |
| 128 KiB | 280 | 7.41× | 22.97× | 233.1 MB |
| **256 KiB** | **189** | **7.26×** | **22.77×** | **234.8 MB** |
| 512 KiB | 98 | 6.94× | 24.23× | 244.2 MB |

512 KiB is the instructive row. It keeps improving the WSR-88D volumes, whose
records are large, while pushing TDWR **worse than doing nothing at all** —
every one of the 364 TDWR records is under 326 KB, so a 512 KiB floor
over-allocates all of them — and it hands the entire RSS saving back. Tuning
this on WSR-88D alone would have chosen it.

What 256 KiB buys, over the whole corpus:

| | before | after |
| --- | --- | --- |
| reallocations, KFTG | 1,372 | **189** (−86%) |
| reallocations, TBOS (TDWR) | 624 | **5** (−99.2%) |
| bytes recopied, KFTG | 237.0 MB | **189.3 MB** (−20%) |
| bytes recopied, TBOS | 21.5 MB | **0.53 MB** (−97.5%) |
| instructions per pass | 26,064,568,217 | **26,054,469,940** (−0.039%) |
| peak RSS @32 threads | 247.9 MB | **234.8 MB** (−5.3%) |

Peak RSS at 1 thread does not move (101 MB) and is not expected to: with one
worker the peak is the decoded output, and only as the pool grows is it the
transient decompression buffers that this changes. RSS figures are means of
five runs and carry a few percent of spread, unlike the instruction counts,
which reproduce to within 2,000 out of 52 billion.

The decoded-moment digest and the raw decompressed-bytes digest are unchanged
over all 14 volumes. Full method and the control in
[Measured, end to end](#measured-end-to-end).

#### The lint scoping — `src/lib.rs`

Upstream writes

```rust
#![deny(clippy::unwrap_used)]
#![deny(clippy::expect_used)]
```

which here becomes `#![cfg_attr(not(test), deny(…))]` for both. Identical edit,
identical reason, as `vendor/nexrad-decode`.

Upstream's own test code breaks the rule: five `unwrap`s in
`src/aws/realtime/retry_policy.rs`'s tests, plus the `expect`s in the
decompression-bound tests added above. Upstream never notices, because it lints
without `--all-targets` and so never compiles `cfg(test)` code under clippy.
This workspace's CI does lint `--all-targets`, and the crate would not pass
clippy at all — `cargo clippy --all-targets --all-features -- -D warnings`
failed with nine errors and `could not compile nexrad-data (lib test)` before
this change.

`Cargo.toml`'s `[lints]` tables cannot fix it, because a lint attribute in
source outranks any command-line level. This is therefore the one place a
source edit was unavoidable. Scoping the deny to non-test builds keeps it
saying what it was written to say — no `unwrap` in the library — and is the
form to offer upstream. A comment at the site says the same thing, so nobody
has to find this file first.

## Measured, end to end

Both changes together, against the vendored crate at the vendoring commit —
which `diff -rq` proved byte-identical to the crates.io tarball, so this is
also the comparison against what the workspace shipped before.

Method: two binaries built from **one** instrument source, differing only in
this directory, run back to back and interleaved base/final/base/final on an
otherwise-shared machine with the 1-minute load average below 20. Instructions
retired via `perf stat -e instructions:u`, differenced across 2 and 6
repetitions so process setup, the file reads and the record splitting all
cancel. Release profile, `lto = true`, `codegen-units = 1`, 32 cores. Buffers
are consumed through a volatile sink so no `malloc`/`free` pair can be elided.

| | before | after | |
| --- | --- | --- | --- |
| instructions, decompress pass | 26,064,568,217 | 26,054,469,940 | **−0.039%** |
| **control** (full decode, no compressed records) | 40,951,141 | 40,966,083 | **+0.036%** |
| reallocations, KFTG | 1,372 | 189 | −86.2% |
| reallocations, TBOS (TDWR) | 624 | 5 | −99.2% |
| reallocations, TSLC (TDWR) | 916 | 6 | −99.3% |
| bytes recopied, KFTG | 237.0 MB | 189.3 MB | −20.1% |
| bytes recopied, TBOS | 21.5 MB | 0.53 MB | −97.5% |
| amplification, KFTG | 7.57× | 7.26× | −4.1% |
| amplification, TBOS | 23.73× | 22.77× | −4.0% |
| amplification, TSLC | 17.85× | 16.74× | −6.2% |
| peak RSS, 1 thread | 101.0 MB | 101.1 MB | +0.1% |
| peak RSS, 4 threads | 116.0 MB | 113.6 MB | −2.0% |
| peak RSS, 32 threads | 247.9 MB | 234.8 MB | **−5.3%** |

The control is the load-bearing row. It is a full decode of the two legacy CTM
volumes, which contain no compressed records at all — every code path except
decompression — and it does not move.

It earned its place. An earlier version of this comparison showed the control
moving **+3.3%**, which would have made the whole table meaningless. The cause
was not the library: the instrument had gained an extra atomic load inside its
global allocator between the two runs, so `base` and `final` had been measured
with different instruments. Rebuilding both from one source removed it. Anyone
repeating this should keep the control and disbelieve any run in which it moves.

Bit-identity, over all 14 volumes — 5 WSR-88D, 7 TDWR, 2 legacy CTM:

- an FNV-1a digest over every decoded moment's raw gate values, gate count,
  first-gate range, gate interval, word size, scale and offset, plus each
  radial's timestamp, azimuth number, azimuth angle, azimuth spacing, elevation
  number, elevation angle and status: **identical**.
- an FNV-1a digest over the raw decompressed bytes of every record, length
  included: **identical**.

## The change that is not here: reusing the decompressor

Worth writing down, because it is the largest remaining win in this function by
a wide margin, it was attempted, and it is blocked by a dependency rather than
by a judgement call. Somebody will have this idea again.

**The prize.** Allocation sizes during a decompress pass, bucketed exactly by a
counting `GlobalAlloc` — not read off the source:

| size | count | total | per record |
| --- | --- | --- | --- |
| **3,600,000** | 91 | **327,600,000** | **1.00** |
| 262,144 | 91 | 23,855,104 | 1.00 |
| 60,952 | 91 | 5,546,632 | 1.00 |
| 8,192 | 91 | 745,472 | 1.00 |
| 80 | 91 | 7,280 | 1.00 |

That is one KFTG volume, 91 records. Exactly one 3,600,000-byte allocation per
record, and it is **91.6%** of every byte the decompression allocates. The TDWR
volumes give the identical shape and the identical 91.6%.

It is libbzip2's `tt` array, `blockSize100k × 100000 × 4` bytes for the `BZh9`
streams NEXRAD uses, allocated when the stream header is read. Eliminating it
by holding one decompressor per worker instead of one per record would take
KFTG's amplification from 7.26× to about **2.9×**, TDWR's from 22.77× to about
**2.0×**, and per-worker live bytes from 5.77 MB to about 2.17 MB — which is
most of the peak-RSS growth with core count.

**Why it is not here.** `bzip2` 0.6.1 does not expose any way to reuse a
decompressor, and 0.6.1 is the newest published version — there is no later one
to move to. Checked, not assumed:

- `bzip2::Decompress` has exactly `new(small: bool)`, `decompress`,
  `decompress_uninit`, `decompress_vec`, `total_in`, `total_out`. `new` is the
  only constructor.
- `grep -rn "reset\|reinit" bzip2-0.6.1/src/` returns nothing. There is no
  `reset` on `Decompress`, on `read::BzDecoder`, on `bufread::BzDecoder`, or on
  `MultiBzDecoder`.
- `MultiBzDecoder` is not a way in. It is `BzDecoder::new(r).multi(true)`, and
  `multi` is private — it decodes *concatenated* streams inside one reader, and
  LDM records are neither concatenated nor wanted in one output buffer.
- `impl Drop for Stream<DirDecompress>` calls `BZ2_bzDecompressEnd`, whose
  implementation in `libbz2-rs-sys` is `s.tt.dealloc(&allocator)`. So the 3.6 MB
  is freed with the decompressor by construction, and even a `reset` written as
  End-then-Init would give it straight back.

The two ways to have it anyway are forking `bzip2` as well, or calling
`libbz2-rs-sys` directly through `unsafe` and a `-sys` dependency this crate
does not have. Both are a larger and worse change than the one they would
enable, in a directory whose value depends on its diff staying readable.

**What to do instead.** Two routes, and the second needs no fork at all.

1. *Upstream.* The fix belongs in `bzip2`: a `Decompress::reset` that keeps the
   `tt` allocation when the new stream's `blockSize100k` matches the old one —
   which for NEXRAD it always does, every record being `BZh9`. `flate2` already
   has exactly this shape of API (`Decompress::reset`). That is an upstream
   issue against `bzip2`, and this section is the evidence for it.

2. *A caching global allocator, in rustdar rather than here.* The 3.6 MB block
   is freed and requested again in the identical size, per record, per worker.
   That is the access pattern a large-block-caching allocator — mimalloc,
   jemalloc — exists to serve: the free returns the block to a thread-local
   cache and the next request takes it back without touching the kernel. It
   recovers most of the same win with **no change to this directory and no
   fork of `bzip2`**, because it never needs the decompressor to be reused —
   only the memory. It is a rustdar-level decision with effects far beyond this
   function, so it is named here rather than made here, and it should be
   measured the way everything else in this file was.

## One more thing measured and left alone

The `8,192`-byte row in the table above is a `BufReader`, one per record, from
`bzip2::read::BzDecoder::new` — which is `bufread::BzDecoder::new(BufReader::new(r))`.
The input here is already a `&[u8]`, which implements `BufRead`, so
`bufread::BzDecoder` would drop both the allocation and the copy of every
compressed byte through it.

Left alone deliberately: it is 0.2% of the bytes allocated and roughly 0.015%
of the instructions, which is below the noise this directory's changes are
being judged against, and it is a fourth change in a diff that is meant to
carry three. Recorded here so it is not rediscovered as a surprise.

## rustfmt

`cargo fmt --all -- --check` is clean over this directory as vendored, checked
with the toolchain `rust-toolchain.toml` pins — which is `stable`, i.e. a
**floating** version. rustfmt's output is version-dependent, and the sibling
directory's note on what to do when a future stable release disagrees applies
here unchanged.

## Two other bots that can reach this directory

- **Renovate** — already handled, and not by this commit. `.github/renovate.json`
  lists `vendor/**` in `"ignorePaths"` because of the sibling directory, and
  that glob covers this one too. Nothing to do; recorded so that the next
  person does not go looking.
- **Coverage** — not handled, named so it is not a surprise. `cargo llvm-cov
  --all-features` now measures this crate as well, and the badge and
  `coverage-baseline.tsv` it auto-commits move accordingly. Nothing gates on a
  threshold, so nothing fails.

## The dependency graph, checked under the flags CI actually uses

An earlier version of this file claimed in this spot that `aws-lc-sys` and
friends "are **not built**" and that the note existed "so the five new lines in
`Cargo.lock` are not mistaken for a new C toolchain requirement." That was
wrong, and it is worth keeping the correction visible, because the mistake was
methodological rather than arithmetical: the check had been run on **one
package** (`cargo check -p nexrad-data --all-features`) and on the wasm32 row,
where reqwest elides its rustls features entirely. Neither is what CI runs.

Run whole-workspace, the branch **did** drag in a CMake + C + assembly build,
reachable only because the crate is now a workspace member and therefore
in range of `--all-features`. That reaches seven CI invocations, including
`clippy.yaml`'s `--all-features --fix`, which auto-commits, and four release
rows in `build.yaml` — Linux, aarch64-apple-darwin, x86_64-apple-darwin and
x86_64-pc-windows-gnullvm. The chain was
`reqwest/rustls` → `reqwest/__rustls-aws-lc-rs` → `rustls/aws-lc-rs` →
`aws-lc-rs` → `aws-lc-sys`.

Fixed by asking for `rustls-no-provider` instead of `rustls` in this
directory's manifest — the same feature the root `Cargo.toml` already pins, so
unification leaves one reqwest with `ring` behind it. The reasoning and the
reason it is *not* an upstream-suitable change are at the dependency itself.

The check that must not regress, and its answer now:

```console
$ cargo tree --workspace --all-features -i aws-lc-sys
error: package ID specification `aws-lc-sys` did not match any packages
```

Same answer on `main`. `aws-lc-sys`, `aws-lc-rs`, `cmake`, `jobserver`,
`dunce` and `fs_extra` are absent from both sides, under both flag sets.

What the vendoring *does* change in the resolved graph, whole-workspace,
`cargo tree --prefix none | sort -u`:

| flags | main | branch | difference |
| --- | --- | --- | --- |
| default | 608 | 609 | `nexrad-data` registry → path. **Nothing else.** |
| `--all-features` | 620 | 622 | the same, plus `signal-hook-registry` |

`signal-hook-registry` arrives because `--all-features` turns on this member's
`aws-polling`, which asks for `tokio/full`. It is a consequence of the crate
being a member with an optional async feature, not of anything this fork
changed, and it builds everywhere.

The lesson, written down because it is the one that generalises: **"vendoring
changes nothing" has to be checked under every flag combination CI uses**, not
only the default one, and whole-workspace rather than per-package.

## Upstream pull request

> **TODO — not yet filed.** When it is, put the URL here and note which of the
> changes above it carries. The source changes are intended for upstream; the
> trims and the `default` line are not (they are local packaging).
