# nexrad-decode, vendored

This directory is a copy of a crates.io crate that this workspace maintains
locally. It is not our code. Everything in it is upstream's except what is
listed under [Local changes](#local-changes) below, and keeping that list short
and true is the whole point of the file — it is what makes the directory
reviewable, and what makes the eventual upstream pull request a diff somebody
can read.

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

Byte-verified on `s3://unidata-nexrad-level2/2026/08/10/TPIT/TPIT20260810_000139_V08`:
as published, 782 garbage-mixed "messages" and 328 warnings; with the framing
corrected, 48/48 records × 120/120 radials, zero warnings, zero bytes remaining.

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
  directory is that they stay byte-unchanged.

### Added

| Path | Why |
| --- | --- |
| `LICENSE` | See above — the tarball ships none. |
| `VENDORED.md` | This file. |

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

**Exactly one source deviation exists, and it changes no behaviour.**

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
to say — no `unwrap` in the library — and is the form to offer upstream.

A comment at the site says the same thing, so nobody has to find this file
first.

Note what did **not** happen: the wasm32 contingency. `insta` 1.48.0 compiles
clean for `wasm32-unknown-unknown` under `--all-targets`, so the snapshot
modules are **not** cfg-gated and run on every target.

## Known upstream defects deliberately left alone

Recorded so that "we did not notice" is never the explanation, and so the
upstream PR can raise them without having to rediscover them.

- **The multi-segment payload slice is 12 bytes short.**
  `src/messages/mod.rs:85-96` computes `payload_size` as
  `message_size_bytes() - size_of::<MessageHeader>()`, but `segment_size`
  counts halfwords from byte 12 — it already excludes the 12-byte CTM prefix,
  so subtracting the full 16-byte header again undercounts by 12. The reader
  position self-corrects, because the frame padding is skipped to
  `SEGMENT_FRAME_SIZE` regardless; what can truncate is the *content* of a
  multi-segment payload, e.g. a Message 15 clutter filter map. Not fixed here
  on purpose: changing it changes what WSR-88D clutter-map messages decode to,
  which is precisely what the byte-identical-snapshot bar exists to protect.
  Raise it upstream, where the fixture coverage to validate it can be built.

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

## rustfmt

`cargo fmt --all -- --check` is clean over this directory as vendored, checked
with the toolchain `rust-toolchain.toml` pins — which is `stable`, i.e. a
**floating** version. rustfmt's output is version-dependent. A future stable
release that formats any of these ~15,500 source lines differently turns the
`fmt` CI job red on code nobody here wrote, and the fix would be a formatting
commit across upstream source that makes the next upstream diff noisy.

If that happens, the options in preference order are: rebase the vendored copy
onto a newer upstream release that has the fix (best — deletes this directory);
or accept the reformat as a single, separately-titled commit that touches
nothing else, and say so here.

There is no `rustfmt.toml` in this workspace, so this directory is formatted by
the same default rules as the rest of it.

## Two other bots that can reach this directory

Neither is handled here, both are named so they are not a surprise.

- **Renovate** (`.github/renovate.json`) scans every Cargo manifest in the
  repository. It may open pull requests bumping the versions *inside*
  `vendor/nexrad-decode/Cargo.toml` — `insta`, `chrono`, `zerocopy`, and the
  rest — which are upstream's declarations and are not ours to move. Whether it
  actually does depends on the shared `local>USA-RedDragon/renovate-configs`
  preset, which is not in this repository, so this was not pre-emptively
  configured against. **Close such a PR and add `"ignorePaths": ["vendor/**"]`
  rather than merging it**, unless the bump is being taken deliberately and
  recorded above.

- **Coverage.** `cargo llvm-cov --all-features` in `test.yaml` now measures this
  crate too, and the badge and `coverage-baseline.tsv` it auto-commits move
  accordingly — roughly 15,500 lines of decoder joined the denominator, covered
  only to the extent upstream's own suite covers them. The number changing is
  expected and is not a regression in this workspace's code. Nothing gates on a
  threshold, so nothing fails.

## Upstream pull request

> **TODO — not yet filed.** When it is, put the URL here and note which of the
> changes above it carries. The framing fix and the `cfg_attr` lint scoping are
> both intended for upstream; the trims are not (they are local packaging).
> The three defects under *Known upstream defects* belong in the PR description
> or in follow-up issues.

## Removing this directory

The patch is meant to be temporary. When a published nexrad-decode contains the
fix:

1. Delete `vendor/nexrad-decode/`.
2. Delete `"vendor/nexrad-decode"` from `[workspace.members]`, the
   `[patch.crates-io]` entry, and `[profile.dev.package.nexrad-decode]` in the
   root `Cargo.toml`.
3. Bump the `[workspace.dependencies]` pin to that version.
4. `cargo tree -i nexrad-decode` should show one registry node again.
