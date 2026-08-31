# pmtiles, vendored

This directory is a copy of a crates.io crate that this workspace maintains
locally. It is not our code. Everything in it is upstream's except what is
listed under [Local changes](#local-changes) below, and keeping that list short
and true is the whole point of the file — it is what makes the directory
reviewable, and what lets a later upstream release be adopted by re-applying a
delta somebody can still read.

It is the sixth such directory, after `vendor/nexrad-decode/VENDORED.md`,
`vendor/nexrad-data/VENDORED.md`, `vendor/bzip2-rs/VENDORED.md`,
`vendor/walkers/VENDORED.md` and `vendor/nexrad-model/VENDORED.md`, and it
follows their shape deliberately.

It differs from all five in one way worth stating in the first paragraph: **it
exists for a defect that had already shipped and was already hurting users.**
The other five are pins, trims, or a standing need to change somebody else's
widget. This one is a repair.

## Provenance

| Field | Value |
| --- | --- |
| Package | `pmtiles` |
| Version | `0.23.0` — the version this workspace already pinned |
| Source | crates.io, `sha256:579153334aed2e066da3e2509b03c686b9d6e737ee2c3635f5baa90a57a762e1` |
| Unpacked from | `~/.cargo/registry/cache/index.crates.io-1949cf8c6b5b557f/pmtiles-0.23.0.crate` |
| Upstream repo | <https://github.com/stadiamaps/pmtiles-rs> (`path_in_vcs` is the repo root) |
| Upstream commit | `f3a3fa1c043a12840b74750d14a1927d2eb3a2c4` (from the tarball's `.cargo_vcs_info.json`) |
| Authors | Luke Seelenbinder, Yuri Astrakhan |
| License | MIT **OR** Apache-2.0 — both texts ship with the crate and are in `LICENSE-MIT` and `LICENSE-APACHE` next to this file |

The checksum was verified against this workspace's own `Cargo.lock` entry for
`pmtiles 0.23.0` before extraction, and the tarball was extracted from the
local registry cache rather than fetched.

## Why this exists

`pmtiles-0.23.0` declares its backend seam with 32-bit offsets:

```rust
// src/async_reader.rs, as published
fn read(&self, offset: usize, length: usize) -> impl Future<...> + Send;
// in find_entry_rec
let offset = (self.header.leaf_offset + entry.offset) as _;   // -> usize
```

**On wasm32 `usize` is 32 bits.** Every archive offset above 4 GiB was
therefore truncated mod 2^32 — silently, with no panic and no error, just bytes
read from the wrong part of the file.

The published basemap archive `omt-20260828.pmtiles` is **83.8 GB** with
`leaf_offset = 83,785,884,629`. `83785884629 mod 2^32 = 2,181,506,005`, which
lands in part **004** at local offset **181,506,005**. The shipped build asked
for exactly `part004 bytes=181506005-181540533`; the correct address is
`part167 bytes=285884629-285919157`. The wrong bytes begin `55 29 c9 a4`, which
is not a gzip member; the right ones begin `1f 8b 08 00` and inflate to a valid
leaf directory. The browser console showed 48 × `Invalid gzip header`.

**The failure was total, not partial.** The archive's root directory is 2917
entries, *all* of them leaf pointers and none of them direct tiles, so every
single tile lookup takes the leaf hop. No basemap tile was reachable in a
browser, ever, for as long as the self-hosted basemap shipped.

Native was never affected — `usize` is 64 bits there — which is why every test
and every CI job in this repository stayed green throughout.

### Ruled out before vendoring

The service worker (Firefox with `dom.serviceWorkers.enabled=false` produced
identical errors), CORS/COEP (`crossOriginIsolated=true`, ranged fetches
returned 206), and the two `UNPUBLISHED-GENERATION` entries in `ARCHIVE_URLS`
(both consumers are keep-sets; `install` is empty).

### Why not a version bump

**`pmtiles 0.24.0` has the same `offset: usize` signature.** Upgrading does not
fix it.

### Upstream's own view

Upstream knows. The first two lines of `src/async_reader.rs` as published are:

```rust
// FIXME: This seems like a bug - there are lots of u64 to usize conversions in this file,
//        so any file larger than 4GB, or an untrusted file with bad data may crash.
#![expect(clippy::cast_possible_truncation)]
```

Two things about that are worth recording. First, it understates the
consequence: there is no crash on the path that matters, only wrong bytes.
Second, the `#![expect(clippy::cast_possible_truncation)]` on the line
underneath is a module-wide suppression of exactly the lint that would have
pointed at every one of these casts.

**No upstream issue or pull request for this was found** (searched 2026-08-31).
The FIXME is the only acknowledgement, so there is nothing to cite but the
comment itself.

## The rule this copy applies

> An **offset** is `u64`. It is a coordinate in a file, and a file is not
> bounded by the address space of the process reading it.
>
> A **length** stays `usize`. It is the size of a buffer in this process, and
> it is bounded by exactly that.

That split is the entire design of the change, and it is why the seam ends up
matching `squallar-egui`'s own `RangeSource::read_range(offset: u64, length:
usize)` exactly — a signature that crate has had since it was written. The
narrowing was never ours; the `as u64` at our call site was widening a value
that had already lost its top bits.

## The offset-cast audit

Every `as usize`, `as _` and `usize`-typed offset on the read path was found
and judged. This is the whole list, not a sample.

| Site | As published | Judgement |
| --- | --- | --- |
| `async_reader.rs` `AsyncBackend::read` | `offset: usize` | **Fixed → `u64`.** The seam. |
| `async_reader.rs` `AsyncBackend::read_exact` | `offset: usize` | **Fixed → `u64`.** |
| `async_reader.rs` `read_directory` | `offset: usize` | **Fixed → `u64`.** |
| `async_reader.rs` `find_entry_rec` | `(leaf_offset + entry.offset) as _` | **Fixed → `u64 + u64`.** This single line is the outage: every tile takes it. |
| `async_reader.rs` `entries()` stream | `(leaf_offset + entry.offset) as _` | **Fixed → `u64 + u64`.** Same defect, second copy; only reachable with `iter-async`, which this copy now enables by default so it is compiled and tested. |
| `async_reader.rs` `get_tile` | `(data_offset + entry.offset) as _` | **Fixed → `u64 + u64`.** A second, independent truncation nobody had named: the tile body of an 83.8 GB archive is also above 4 GiB. Fixing only the leaf hop would have moved the failure, not removed it. |
| `async_reader.rs` `get_metadata` | `metadata_offset as _` | **Fixed → `u64`.** |
| `async_reader.rs` `get_metadata` | `metadata_length as _` | **Fixed → checked `usize::try_from`.** A length, so `usize` is right; the conversion is now fallible instead of truncating. |
| `async_reader.rs` `try_from_cached_source` | `(root_offset as usize) - HEADER_SIZE` | **Fixed → checked `usize::try_from`.** `usize` is genuinely correct here — it indexes a buffer already read into memory — but the cast could still truncate, and on wasm32 that turned a clean error into a wrong slice. |
| `async_reader.rs` `try_from_cached_source` | `root_length as _` | **Fixed → checked `usize::try_from`.** Same shape. |
| `cache.rs` `DirectoryCache::get_dir_entry_or_insert` | `offset: usize` | **Fixed → `u64`.** |
| `cache.rs` `HashMapCache` | `HashMap<usize, Directory>` | **Fixed → `HashMap<u64, _>`.** Not cosmetic, and not previously noticed: **the directory cache is keyed by archive offset.** On a 32-bit target two leaf directories exactly 4 GiB apart collided on one key and served each other's entries. `squallar-egui` uses `HashMapCache` and its module doc calls the cache "mandatory", so this was live. |
| `cache.rs` `MokaCache` | `Cache<usize, Directory>` | **Fixed → `Cache<u64, _>`** for consistency. Feature deleted (see below), so not compiled here. |
| `backends/http.rs` `read` | `offset: usize`, `offset + length - 1` | **Fixed → `u64`,** with the Range-header arithmetic done in `u64` so it cannot wrap. |
| `async_reader.rs` `get_tile` / `find_entry_rec` | `entry.length as _` | **Left alone.** `DirEntry::length` is `u32`; converting it to `usize` is a widening on every target this workspace builds. |
| `writer/mod.rs` | `leaves_bytes: usize`, `out.writer_bytes() as u64` | **Left alone, and this is a real 32-bit latent bug — recorded, not fixed.** The writer accumulates section sizes in `usize`, so writing an archive larger than 4 GiB *from* a 32-bit process would produce a corrupt header. It is out of scope because it is not the shipped defect and nothing here writes such a file: `basemap_download`'s segments are ~16 MB and are built in a `Cursor<Vec<u8>>`, which a 32-bit process cannot grow past 4 GiB anyway. Whoever needs a 32-bit writer of huge archives has to revisit this. |

## Local changes

### Removed — packaging residue

| Path | Why |
| --- | --- |
| `.cargo_vcs_info.json` | Recorded in the provenance table above instead. |
| `Cargo.toml.orig` | Upstream's pre-normalisation manifest. |
| `Cargo.lock`, `Cargo.lock.msrv` | A packaged crate's lockfile is inert; the root `Cargo.lock` is the one that resolves anything. |
| `.cargo-ok` | Registry extraction marker. |
| `.github/`, `.gitignore`, `.editorconfig`, `.pre-commit-config.yaml`, `clippy.toml`, `codecov.yml`, `deny.toml`, `justfile`, `release-plz.toml` | Upstream's project plumbing. None of it runs here, and `clippy.toml` in particular would apply upstream's lint configuration to a directory this workspace now gates. |

`README.md`, `CHANGELOG.md` and both licence files are kept.

### Removed — four of the five backends

`src/backends/aws_s3.rs` (`aws-sdk-s3`), `src/backends/s3.rs` (`rust-s3`),
`src/backends/object_store.rs` (`object_store`) and `src/backends/mmap.rs`
(`fmmap`), together with their features, their optional dependencies, their
`PmtError` variants and their `lib.rs` re-exports.

Nothing in this workspace constructs any of them, none of them compiles on
wasm32, and — the part that actually forced the decision — each carried an
optional dependency tree that a `[patch]`ed **workspace member** would have had
to resolve. See [The `--all-features` problem](#the---all-features-problem).

`src/backends/http.rs` **stays**: `squallar-egui` enables `http-async` on the
native target. Note that nothing constructs `HttpBackend` either — the reader
reaches every archive through `squallar-egui`'s own `RangeBackend` on both
targets — so this file is compiled but unused. It is kept rather than deleted
because deleting it would mean editing `squallar-egui`'s manifest, which is a
separate decision from repairing an offset.

### Removed — the test module that needs a network and a mock server

`src/backends/http.rs`'s `#[cfg(test)] mod tests` (4 `#[tokio::test]` fns).
One of them (`basic_http_test`) fetches
`https://protomaps.github.io/PMTiles/...` over the live network, which a gate
must not depend on. The other three are built on `mockito`, a dev-dependency
this copy does not carry. Deleting them is a straight loss of 4 tests and worth
naming as one; they test a backend nothing here constructs.

`src/async_reader.rs`'s two `#[cfg(feature = "object-store")]` test fns
(`get_tiles_object_store`, `test_data_version_source_modified`) went with their
backend.

### Removed — dev-dependencies

`fmmap` (backend deleted), `mockito` and `url` and `reqwest` (only used by the
deleted http test module).

`rstest`, `tempfile`, `tokio` and `flate2` stay — upstream's reader and writer
suites use them and those suites do run here.

### Added

| Path | Why |
| --- | --- |
| `src/backends/slice.rs` | `SliceBackend`, replacing `MmapBackend`. See below. |
| `src/wide_offset_tests.rs` | The pin on the offset width. See [The pin](#the-pin). |
| `VENDORED.md` | This file. |

### Changed — `MmapBackend` becomes `SliceBackend`

This is the largest *mechanical* part of the delta and the one most likely to
look gratuitous, so here is the reasoning.

As published, **all 25 of upstream's reader and writer round-trip test cases
are behind `#[cfg(feature = "mmap-async-tokio")]`** (17 `fn`s; the difference
is `rstest` case expansion) — that is, behind `fmmap`. They
are also the only tests in this tree that drive a leaf-directory hop end to end
against a real archive, which is precisely the code this directory exists to
change. Leaving them gated would have meant vendoring a crate to fix a
behaviour and then not running the tests for that behaviour.

Arming them upstream's way costs `fmmap`, which is `memmap2` plus a
`parse-display`/`regex` tree, for a test helper. Everything those 24 tests
actually do with the backend is "open a small fixture and read bytes out of
it", which `std::fs::read` does with no dependency at all.

So `src/backends/mmap.rs` is replaced by `src/backends/slice.rs`: the same two
inherent constructors (`new_with_path`, `new_with_cached_path`), the same
`try_from(path)` shape, backed by `Bytes` instead of a mapping. The delta in
the test modules is then a rename — `MmapBackend::try_from` →
`SliceBackend::try_from`, 20 call sites — plus dropping the feature gate.

`SliceBackend` is also the honest statement of the reader's contract ("bytes at
a `u64` offset") and is the identity case of it, which is why the pin below can
be written in ten lines.

### Changed — `Cargo.toml`

The manifest carries its own reasoning inline; the short version is in
[The `--all-features` problem](#the---all-features-problem) below. Beyond the
feature and dependency removals already listed:

* `default` is `["__async", "iter-async", "write"]`, replacing upstream's
  `__all_non_conflicting` (a list of twelve). A workspace member has to be able
  to build its own test targets, and that is what those three are for.
  `iter-async` earns its place twice: upstream's writer tests use `entries()`,
  and `entries()` contains one of the two copies of the truncating leaf hop.
* **Dev-dependencies are target-gated** to `cfg(not(target_arch = "wasm32"))`.
  This is not house style and is load-bearing: this workspace's wasm gate is
  `cargo check --workspace --all-targets --target wasm32-unknown-unknown`, and
  `--all-targets` builds test targets, so a member's dev-dependencies have to
  compile on wasm32 or the gate goes red. `tempfile` wants a filesystem. The
  three test modules that use them carry a matching
  `#[cfg(not(target_arch = "wasm32"))]`; they open fixture files off disk, so
  there was never anything for them to do there.
* `package.version` stays **exactly** `0.23.0`, with no `+local` metadata. A
  `[patch]` is only accepted for a version satisfying the requirement it
  replaces, and the root `[workspace.dependencies]` pin is `pmtiles = "=0.23.0"`.
* Two lint tables replace upstream's, for the same reason
  `vendor/walkers/Cargo.toml` replaces its own: `.github/workflows/clippy.yaml`
  runs `cargo clippy --all-targets --all-features --fix` across every member
  and **auto-commits the rewritten tree to `main`**, then gates on
  `-D warnings`. Without the tables a bot would eventually rewrite upstream
  source here on its own schedule and this file would stop being true.

### Changed — `src/lib.rs`

Upstream includes `README.md` as the crate docs whenever default features are
on. Three of the README's four examples use backends this copy deletes
(`new_with_path` was mmap, then `pmtiles::reqwest`, then `pmtiles::aws_sdk_s3`),
so including it would be three doctests that cannot compile. The fallback title
upstream already had for the no-default case is used unconditionally.

### Changed — source, the offset widening

`src/async_reader.rs`, `src/cache.rs`, `src/backends/http.rs`. Every site is in
[the audit table](#the-offset-cast-audit) above. Each edit carries a `VENDORED:`
comment naming what it was, so the delta is greppable:

```text
grep -rn 'VENDORED' vendor/pmtiles/src/
```

Upstream's `// FIXME` and its `#![expect(clippy::cast_possible_truncation)]` at
the top of `async_reader.rs` are replaced by a note stating the rule and
pointing here.

## The `--all-features` problem

This is the one consequence of vendoring that was not obvious and that drove
most of the manifest delta, so it is written out.

`--all-features` reaches a **workspace member's** features, not a
*dependency's*. Today `pmtiles` is a dependency with `default-features = false`,
so its optional dependencies are pruned from `Cargo.lock` entirely — measured
before this change: no `aws-sdk-s3`, no `rust-s3`, no `object_store`, no
`fmmap`, no `moka`, no `tilejson`, no `brotli`, no `zstd`.

The moment it became a member, `.github/workflows/clippy.yaml`'s
`cargo clippy --all-targets --all-features` became a job that turns **all** of
them on. Three were unaffordable at that price:

* **`zstd`** is `zstd-sys`, which compiles C in every CI job that ever runs
  clippy.
* **`reqwest-default`** is `reqwest?/default`, which would have switched the one
  shared `reqwest` in this workspace to native-tls behind everybody's back —
  the workspace pin is deliberately `rustls-no-provider`.
* **`mmap-async-tokio`** is `fmmap`, already discussed.

`moka`, `tilejson` and `brotli` were removed for the smaller version of the
same reason.

**The source those features gated is untouched.** `#[cfg(feature = "moka")]`
and its three siblings are still in `src/`, now permanently false, which is what
`unexpected_cfgs = "allow"` in `[lints.rust]` is for. That was a deliberate
choice over deleting ~120 lines across six files: re-adding a feature line to
the manifest is how you turn any of them back on, and the diff against upstream
stays a manifest diff rather than a source diff.

## The pin

`src/wide_offset_tests.rs`, one test:
`a_leaf_directory_above_four_gibibytes_is_not_truncated`.

**No fixture can be 4 GiB.** So the archive is real but *relocated*:
`fixtures/leaf.pmtiles` (4 KB, real leaf directories) is read into memory and
its 127-byte header rewritten so `leaf_offset` claims the exact value the
published archive carries, `83_785_884_629`. The backend under it translates
offsets back and records every one it was asked for. The reader does real work
on a real archive while genuinely asking for a byte 83.7 GB in.

### Why it lives here and not beside its caller

Because of what it can be *run* on, and this is the part that matters.

`usize` is 64 bits on the machine that runs `cargo test`, so a host-only
version of this test passes whether or not the bug is present. That is exactly
the shape of check that let the defect ship.

This crate — unlike `squallar-egui`, which drags in wgpu and winit — builds and
runs on **`i686-unknown-linux-gnu`**, a real 32-bit target where `usize` is 32
bits and `as usize` really does truncate.

### The control, measured 2026-08-31

Upstream's narrowing restored in `find_entry_rec` (the single line
`let offset = (self.header.leaf_offset + entry.offset) as usize as u64;`), the
**same source** run on both targets:

```text
cargo test -p pmtiles --lib wide_offset                                    ok
cargo test -p pmtiles --lib wide_offset --target i686-unknown-linux-gnu    FAILED
    the reader never asked for the leaf directory's true address
    (83785884629); it asked for [0, 2181506005]
```

That split *is* the defect: identical code, green on the 64-bit builder that
gates every CI job, red on a 32-bit target. `2181506005` there is not a number
the test computed — it is what the reader asked the backend for.

With the fix in place both targets are green (41 unit tests each).

There is a second, complementary pin next to the caller —
`squallar-egui/src/basemap_archive/four_gib_offset_tests.rs` — which adds a
compile-time proof that the seam cannot narrow (`u64` and `usize` are distinct
types in Rust regardless of width, so the unpatched signature fails to compile
on *any* host) and drives the same relocation through the production
`RangeBackend` + `HashMapCache` path.

## What the pin actually selects

`cargo test -p pmtiles` runs **41 unit tests and 7 doctests**. 27 of the 41 are
native-only by `#[cfg(not(target_arch = "wasm32"))]`.

40 of the 41 are upstream's, and the ones that matter here are the ones that
take a leaf hop: `test_tile_existence::case_2` (an existing leaf tile),
`test_martin_675`, `test_leaf_tile_compressed`, `test_entries`, and the writer's
`with_leaves` round trip. They pass **unedited** apart from the
`MmapBackend`→`SliceBackend` rename. That is the point of making this directory
a member: the behaviour the offset change could have broken is pinned by the
people who wrote the crate, not by us.

The 41st is `wide_offset_tests`, described above.

## End-to-end against the live archive, on a 32-bit target

The pin above uses a relocated 4 KB fixture. This is the other half: the **real
83.8 GB archive**, over the network, through this reader, built for
`i686-unknown-linux-gnu` — a target where `usize` really is 32 bits.

Measured 2026-08-31 with a temporary harness (a backend issuing range requests
through `curl`, so no 32-bit TLS stack is needed). The harness was deleted
after the run and is deliberately not in this directory; what follows is its
output.

The live archive's header, fetched with `curl -r 0-126`, confirms the numbers
the diagnosis named:

```text
root_offset                     127
root_length                  13,965
metadata_offset      83,785,883,110
metadata_length               1,519
leaf_offset          83,785,884,629
leaf_length              96,600,519
data_offset                  16,384
data_length          83,785,866,726
```

**With this copy of the crate**, asking for one real basemap tile (z6/14/25):

```text
usize is 32 bits on this target
leaf_offset = 83785884629
--- ranges actually requested ---
  part000 bytes=0-16383
  part167 bytes=285884629-285919157
  part000 bytes=43054152-43128100
--- tile TileCoord { z: 6, x: 14, y: 25 } -> Some(110147) bytes ---
```

`part167 bytes=285884629-285919157` is exactly the range the diagnosis
identified as correct, and 110,147 bytes of MVT came back.

**With upstream's narrowing restored**, same target, same archive, same tile:

```text
usize is 32 bits on this target
leaf_offset = 83785884629
panicked: the tile read succeeds: Reading(Custom { kind: InvalidData,
    error: "Invalid gzip header" })
```

That is the shipped symptom verbatim — the same `Invalid gzip header` the
browser console showed 48 times.

Confirming the two addresses independently, straight from the origin:

```text
curl -r 285884629-285919157 …part167   206, 34529 bytes, 1f8b 0800 0000 0000
curl -r 181506005-181540533 …part004   206, 34529 bytes, 5529 c9a4 33d3 986f
```

### What this evidence does not cover

It is not a browser. `i686-unknown-linux-gnu` and `wasm32-unknown-unknown` are
different targets that happen to share a pointer width, and the property being
exercised is exactly that shared width — but nothing here executed the web
build. The browser-side gate is the Tier-2 rig, not this file.

Two things the header above settle that were not previously stated:

* `metadata_offset` is **83,785,883,110** — also above 4 GiB. `get_metadata`
  was broken on wasm32 too, which is why its cast is in the audit table.
* `data_offset` is only 16,384, but `data_length` is **83.7 GB**, so
  `data_offset + entry.offset` exceeds 4 GiB for the overwhelming majority of
  tiles. Fixing the leaf hop alone would have moved the failure rather than
  removed it — the `get_tile` row in the audit table is not hypothetical.

## rustfmt

`cargo fmt -p pmtiles -- --check` is clean over this directory, checked with the
toolchain `rust-toolchain.toml` pins. Only the lines this delta adds ever needed
formatting — upstream's source was already clean under the same default rules,
and no reformat of upstream code is part of this delta. There is no
`rustfmt.toml` in this workspace and upstream's tarball ships none (upstream's
`.editorconfig` and `clippy.toml` were removed as project plumbing).

Package-scoped, and that is not a stylistic preference: this workspace forbids
`cargo fmt --all` outright, because a workspace-wide format pulls another
worktree's in-flight files into the tree running it.

rustfmt's output is version-dependent and `rust-toolchain.toml` pins a floating
`stable`. When a future stable release disagrees, the options are the ones
`vendor/nexrad-decode/VENDORED.md` records: rebase onto a newer upstream release
that has the fix (best — it shrinks this directory), or accept the reformat as a
single, separately-titled commit that touches nothing else, and say so here.

## The dependency graph

`cargo tree -i pmtiles` prints exactly one node and it names the path:

```text
pmtiles v0.23.0 (/…/vendor/pmtiles)
└── squallar-egui v0.1.0 (/…/squallar-egui)
```

Visible in `Cargo.lock` as the `pmtiles` entry losing its `source` and
`checksum` lines.

**Seven packages entered the lockfile**, all pure Rust and all small:
`async-stream` and `async-stream-impl` (from `iter-async` in the new `default`),
and `rstest`, `rstest_macros`, `relative-path`, `glob` and `futures-timer`
(dev-dependencies, now resolved because a member's dev-deps are). Nothing
heavyweight arrived: no `zstd-sys`, no `aws-sdk-s3`, no `moka`, no `fmmap`.

## Removing this directory

Delete `vendor/pmtiles/`, the `"vendor/pmtiles"` member entry, the
`pmtiles = { path = "vendor/pmtiles" }` line in `[patch.crates-io]`, and the
`[profile.dev.package.pmtiles]` override — all four are in the root
`Cargo.toml` and each is commented with a pointer to the others.

**Do not do this until upstream's `AsyncBackend::read` takes a `u64` offset.**
`squallar-egui`'s `impl AsyncBackend for RangeBackend` declares `offset: u64`,
so reverting to a registry copy that still says `usize` is a compile error, not
a silent regression — which is deliberate, and is the cheapest guard available.

## Not going upstream

**These changes are not going upstream.** That is this workspace's standing
stance, taken for `vendor/nexrad-decode` on 2026-08-12 and applied here on
2026-08-31. No pull request will be filed.

That is worth a caveat this file should not hide: unlike the other five
directories, the core of this delta is a **plain bug fix that everyone using
this crate on a 32-bit target needs**, and upstream has an open FIXME saying so.
A reader who disagrees with the standing stance has a stronger case here than
anywhere else in `vendor/`. The stance is recorded, not defended.

The practical consequence is unchanged: upstream will not carry this, so this
directory is where this workspace's PMTiles reader lives, indefinitely. A later
upstream release is still worth adopting for everything else it brings, and
re-applying this delta onto it is the job the list above exists to make
possible. Start with `grep -rn 'VENDORED' vendor/pmtiles/src/`.
