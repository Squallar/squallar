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

Nothing yet. `diff -rq` against the unpacked registry copy reports `src/` as
byte-identical, and this commit is the pure vendoring so that the diff which
follows is reviewable on its own.

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
