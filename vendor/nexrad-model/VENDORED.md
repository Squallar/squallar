# nexrad-model, vendored

This directory is a copy of a crates.io crate that this workspace maintains
locally. It is not our code. Everything in it is upstream's except what is
listed under [Local changes](#local-changes) below, and keeping that list short
and true is the whole point of the file — it is what makes the directory
reviewable, and what lets a later upstream release be adopted by re-applying a
delta somebody can still read.

**These changes are not going upstream.** Same decision as the other vendored
nexrad crates (2026-08-12): upstream will not carry this delta, so this
directory is where the decomposable scan model lives, indefinitely. A later
upstream release is still worth adopting for everything else it brings, and
re-applying this delta onto it is the job the short list below exists to make
possible.

## Provenance

| Field | Value |
| --- | --- |
| Package | `nexrad-model` |
| Version | `1.0.0-rc.2` — what this workspace already pinned as a registry dependency |
| Source | crates.io, `sha256:4e2e15c56d3f5869b78eef326ca33debd6d144a4a3348ff4829e8579c6d2a9ea` |
| Unpacked from | `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/nexrad-model-1.0.0-rc.2/` |
| Upstream repo | <https://github.com/danielway/nexrad>, subdirectory `nexrad-model` |
| Upstream commit | `3d9ef8d7d1ecaa24795c53300fbdd5825852f1aa` (from the tarball's `.cargo_vcs_info.json`) |
| Author | Daniel Way `<contact@danieldway.com>` |
| License | MIT — full text in `LICENSE` next to this file |

The packaged tarball ships **no** license file, only `license = "MIT"` in the
manifest. `LICENSE` here is the same reconstruction the other vendored nexrad
crates carry — the upstream repository's own `LICENSE`, copyright line
included — because "MIT" in a metadata field is a declaration and not the
notice the license requires us to redistribute.

## Why this exists

The published nexrad-model seals a `Scan`'s sweeps behind `&[Sweep]` borrows:
`Scan::new` takes `Vec<Sweep>` by value, and no method gives them back owned.

That seal is what made a decoded volume's teardown indivisible. A decoded
volume is a measured 48.88 MiB median / 74.63 MiB maximum of live heap across
thousands of per-radial buffers, over 208 real archive volumes decoded under a
counting global allocator, and on wasm every discarded volume is freed *on the
page thread*
by `squallar_worker::offload::drain_deferred_drops`, whose budget paces turns
but cannot split a payload: the drain's real spend per frame is its budget plus
one whole payload. With the volume indivisible, that overshoot is the whole
volume.

`Scan::into_sweeps` is the seam that fixes the shape: the eviction path hands
each sweep to the drop queue as its own payload, so one drain turn frees one
sweep rather than one volume.

## Local changes

This is the complete list. `diff -rq` against the unpacked registry copy must
produce exactly these entries and nothing else.

### Removed — packaging residue

| Path | Why |
| --- | --- |
| `default_*.profraw` | Coverage output from an unrelated local run that landed in the registry checkout. Never part of the crate. |
| `.cargo-ok` | Cargo's own extraction marker. |
| `.cargo_vcs_info.json` | Recorded in the provenance table above instead. |
| `Cargo.toml.orig` | Upstream's pre-normalisation manifest, which refers to workspace inheritance that does not exist here. |
| `Cargo.lock` | A packaged crate's lockfile is inert; this workspace's root `Cargo.lock` is the one that resolves anything. |

### Removed — targets that cannot compile here

| Path | Why |
| --- | --- |
| `tests/fixture_snapshots.rs` + its `[[test]]` block | `include_bytes!("../../tests/fixtures/…")` — five archive volumes that exist only in upstream's monorepo checkout. Cannot compile here at all. |
| `tests/scan_snapshot.rs` + its `[[test]]` block | `include_bytes!("../../downloads/…")` — a file that exists only after upstream's download step. Same. |
| `tests/snapshots/` | The insta snapshots of the two deleted tests; nothing else reads them. |
| dev-dependencies `insta`, `nexrad-data`, `nexrad-decode`, `hex`, `sha2` | Used only by the two deleted tests. (`hex` and `sha2` remain as the lib's own runtime dependencies, untouched.) |

`tests/model_types.rs` — upstream's pure in-crate unit suite, 20+ tests over
`Scan`/`Sweep`/`Radial`/`MomentData` construction and `Sweep::merge` — is kept
verbatim and runs as a workspace member. It is the behaviour pin on the model
the added method must leave untouched.

### Added

| Path | What |
| --- | --- |
| `src/data/scan.rs` | Two methods. `Scan::into_sweeps(self) -> Vec<Sweep>` — the only owned decomposition of a scan. It moves the field `Scan::new` took; no representation changes. `Scan::sweeps_capacity(&self) -> usize` — reads `Vec::capacity` on the same field, because `sweeps()` hands out a slice and a slice cannot report spare capacity. Both marked `LOCAL CHANGE` at their definitions. |
| `src/data/sweep.rs` | One method, `Sweep::radials_capacity(&self) -> usize` — `Vec::capacity` on the radial vector, for the same reason and marked the same way. Reading it is what lets `squallar_radar::scan_size::scan_bytes` charge what the allocator holds rather than what the slice's length implies; the spare is ~42 % of the length in real decoded volumes. |
| `Cargo.toml` | The `[lints]` tables every vendored crate here carries, so the clippy fix-bot cannot rewrite upstream source — see the comment above them and vendor/nexrad-decode/Cargo.toml for the mechanism. Also the two `[[test]]` blocks and five dev-dependencies of the deleted tests, removed. |
| `LICENSE`, `VENDORED.md` | This file and the license notice. |
