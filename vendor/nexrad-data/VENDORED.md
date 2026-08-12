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

`Record::decompress` (`src/volume/record.rs`) is four lines long, is the
dominant cost of the entire decode path, and has no upper bound on what it will
allocate.

```rust
let mut decompressed_data = Vec::new();
BzDecoder::new(data).read_to_end(&mut decompressed_data)?;
```

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

Nine of the eleven packaged test targets are gone, and the reason is the same
one the sibling directory ran into: upstream's tests read files that exist only
in upstream's checkout, and the packaged tarball does not contain them.

| Path | Why |
| --- | --- |
| `tests/volume_header.rs`, `tests/volume_records.rs`, `tests/volume_scan.rs`, `tests/aws_realtime_polling.rs` | All four `include_bytes!("../../downloads/KDMX20220305_232324_V06")`, a file produced by a download step in upstream's repo. Not in the tarball; they cannot compile here at all. |
| `tests/fixture_integration.rs` | `include_bytes!("../../tests/fixtures/…")` × 8, likewise upstream-checkout-only. |
| `tests/live_decode.rs`, `tests/aws_realtime_network.rs`, `tests/aws_archive.rs` | Download live data from the AWS archive bucket at test time. A workspace test suite that reaches the network is a test suite that fails when the network does. |
| `benches/scan.rs` + the `[[bench]]` block + the `criterion` dev-dependency | Nothing in this workspace runs them, and `--all-targets` would compile criterion on every CI row including wasm32. |
| `examples/` (3 files) + the three `[[example]]` blocks + the `clap` dev-dependency | Same. |
| dev-dependency `env_logger`, dev-dependency `tokio` | Used only by the deleted tests. |

`autoexamples`/`autobenches`/`autotests` are all `false` in the packaged
manifest, so each block had to be deleted together with its file: a block
naming a missing path is a hard error, and a file with no block is dead weight.

Two of the three *kept* targets each had one test reaching for the same missing
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

One difference worth naming: unlike nexrad-decode, this crate's `src/lib.rs`
carries no `#![deny(…)]` of its own. A lint attribute in source outranks any
command-line level, which is what forced a source edit in the sibling
directory; here the tables reach everything and **no source edit was needed to
make clippy pass**.

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

**Where 16 MiB comes from.** Two independent legs, and the number has to
satisfy both — it must be out of reach of real data, and low enough to be worth
having.

*Real data.* 693 compressed records across 12 volumes: 5 WSR-88D sites spanning
2017–2023 and 7 TDWR sites, chosen because the two formats build records
differently.

| | largest record | ceiling is |
| --- | --- | --- |
| WSR-88D | 1,416,480 B | **11.8×** larger |
| TDWR | 325,888 B | **51×** larger |

*The format's own reach.* Archive II groups radials into LDM records of at most
120 messages, and a message's declared size is a `u16` count of halfwords
measured from byte 12 — at most 65,535 halfwords, so 131,070 bytes plus the
12-byte CTM prefix. 120 × 131,082 = **15,729,840 bytes**, which is under
16,777,216. The ceiling therefore sits above the largest record the structure
can express, not merely above the largest one this corpus happened to hold.
That arithmetic is an assertion in
`decompress_bound_tests::the_ceiling_is_above_every_record_the_archive_ii_structure_can_express`,
so lowering the constant past the format's reach fails a test rather than
passing review.

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

+0.0005%. Allocation counts, byte totals and peak live bytes are *identical* —
`diff` on the per-volume allocation table reports no change at all — and so are
the decoded-moment digest and the raw decompressed-bytes digest over all 14
volumes.

New `#[cfg(test)] mod decompress_bound_tests` in `src/volume/record.rs`, an
in-source module so it travels with the fix in the upstream diff. Five tests: a
record of exactly `MAX` bytes decompresses; a record one byte over is
`RecordTooLarge`; a 256 MiB bomb that is under 4 KB on the wire is refused; an
ordinary 2432-byte record round-trips byte-identically; and the Archive II
arithmetic above.

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
| `Vec::new()` | 1,372 | 7.57× | 23.73× | 243–251 MB |
| 128 KiB | 280 | 7.41× | 22.97× | 230–236 MB |
| **256 KiB** | **189** | **7.26×** | **22.77×** | **230–240 MB** |
| 512 KiB | 98 | 6.94× | 24.23× | 243–245 MB |

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
| instructions per pass | 26,064,467,974 | **26,054,460,125** (−0.038%) |
| peak RSS @32 threads | 243–251 MB | **230–240 MB** |

Peak RSS at 1 and 4 threads does not move (101 MB, 113 MB) and is not expected
to: at low thread counts the peak is the decoded output, and only at 32 is it
the transient decompression buffers that this changes. The RSS figures are
given as ranges because they carry about 4% run-to-run spread, unlike the
instruction counts, which reproduce to within 1,500 out of 52 billion.

The decoded-moment digest and the raw decompressed-bytes digest are unchanged
over all 14 volumes.

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

**What to do instead.** The fix belongs in `bzip2`: a `Decompress::reset` that
keeps the `tt` allocation when the new stream's `blockSize100k` matches the old
one — which for NEXRAD it always does, every record being `BZh9`. `flate2`
already has exactly this shape of API (`Decompress::reset`). That is an
upstream issue against `bzip2`, and this section is the evidence for it.

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

## One resolution artifact

Adding this member added five packages to `Cargo.lock` that were not there
before: `aws-lc-rs`, `aws-lc-sys`, `cmake`, `dunce`, `fs_extra`. They arrive
through the optional `reqwest` dependency's `rustls` feature and are **not
built** — feature unification with this workspace's own reqwest pin
(`rustls-no-provider` + `ring`) means the aws-lc provider is never selected,
which was confirmed by watching a `--all-features` build of this package: it
compiles `rustls-webpki`, `tokio-rustls`, `hyper-rustls` and
`rustls-platform-verifier`, and no `aws-lc-sys`. Lockfile entries for optional
dependencies that nothing enables are ordinary; this note exists so the five
new lines in `Cargo.lock` are not mistaken for a new C toolchain requirement.

## Upstream pull request

> **TODO — not yet filed.** When it is, put the URL here and note which of the
> changes above it carries. The source changes are intended for upstream; the
> trims and the `default` line are not (they are local packaging).
