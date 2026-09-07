# mvt-reader, vendored

This directory is a copy of a crates.io crate that this workspace maintains
locally. It is not our code. Everything in it is upstream's except what is
listed under [Local changes](#local-changes) below, and keeping that list short
and true is the whole point of the file — it is what makes the directory
reviewable, and what lets a later upstream release be adopted by re-applying a
delta somebody can still read.

It is the seventh such directory, after `vendor/nexrad-decode`,
`vendor/nexrad-data`, `vendor/bzip2-rs`, `vendor/walkers`, `vendor/nexrad-model`
and `vendor/pmtiles`, and it follows their shape deliberately.

**These changes are not going upstream.** That is this workspace's standing
stance, taken for `vendor/nexrad-decode` on 2026-08-12 and applied here on
2026-08-31. No pull request will be filed.

## What is different about this one

**Every other vendored directory here is a member because upstream's own test
target is the behaviour pin on the code we patch. This one has no such pin to
inherit.** The published tarball carries no test target at all: `autotests =
false`, `src/lib.rs` has no `#[cfg(test)]` module, and the `mvt-fixtures/` and
`vector-tile-spec/` submodules the repository tests against are outside the
manifest's `include` list. `cargo test -p mvt-reader` on the unmodified copy
selects **8 doctests and zero unit tests** — the doctests are the crate's usage
examples, all of them `let data = vec![/* Vector tile data */]`, and not one of
them reaches `parse_geometry`.

So the pin is written here instead, and it is named in the workspace
`Cargo.toml`'s member note so nobody looks for an inherited one:
`src/peak_allocation_tests.rs` measures the decode's real peak with a counting
global allocator and asserts it is linear in the feature rather than quadratic.
Its non-vacuity is measured, not asserted — see its module doc.

## Provenance

| Field | Value |
| --- | --- |
| Package | `mvt-reader` |
| Version | `2.4.0` — the version this workspace already resolved through `walkers` and `squallar-buildings` |
| Source | crates.io, `sha256:b21d02c206b092ea347b673caf17f15fcc7eb45fff0668adfc484306ee6f2a1a` |
| Unpacked from | `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/mvt-reader-2.4.0` |
| Upstream repo | <https://github.com/codeart1st/mvt-reader> |
| Upstream commit | `b0d05680fd87e5bc1544681a6efec8a0a74421d2` (from the tarball's `.cargo_vcs_info.json`, which records `"dirty": true`) |
| Author | Paul Lange |
| License | MIT — full text in `LICENSE` next to this file |

The checksum above is of the tarball in the local registry cache, taken before
extraction.

## Why this copy exists

`parse_geometry` decodes one feature's MVT command array into a geometry. A
polygon feature is a sequence of closed rings, and every ring of a feature is
held **at once**, in a `linestrings: Vec<LineString<T>>`, until the geometry is
assembled at the end. Upstream started each of those rings with

```rust
coordinates = Vec::with_capacity(geometry_data.len());
```

— `geometry_data` being the **whole feature's** command-integer array, not the
ring's share of it. The comment beside the first such reservation reads "worst
case capacity to prevent reallocation. not needed to be exact", which is a
sound thing to do **once**; done once per ring, with every ring alive, one
feature's decode peaks at `rings x commands` where `commands` is all it could
ever fill.

That is quadratic in a single feature, and it is not a theoretical shape. A
planet basemap's low-zoom `landcover` and `water` layers are exactly it: a
handful of features, each a multipolygon of hundreds of small rings. On
`wasm32-unknown-unknown` the reservation is an **infallible** `Vec` allocation
against a module memory whose maximum this workspace links at 1 GiB
(`.github/scripts/wasm-threads.sh`, `-Clink-arg=--max-memory=1073741824`), so
the failure is `handle_alloc_error` → `abort()` → an `unreachable` trap.

**Nothing unwinds through a wasm trap.** Measured in Firefox on 2026-08-31 at a
2878x1651 canvas, the trap landed inside `walkers::mvt::parse` while the module
held 332 MB of its 1 GiB, from a **172 KB** tile. `winit`'s web event loop had
`Shared::runner` mutably borrowed at the time — the trap is thrown out of the
`requestAnimationFrame` callback the frame was running in — and that `RefCell`
borrow is never released, so every later event panics `RefCell already borrowed`
at `winit-0.30.13/src/platform_impl/web/event_loop/runner.rs:599`. The frame
loop stops for good while `requestAnimationFrame` keeps firing at 17.06 ms and
the canvas holds its last painted frame: the page looks alive and is dead. That
is the user-reported "zooming out quite a bit can freeze it up".

`walkers::mvt::shrink_geometry` (in `vendor/walkers`) gives the slack back after
the fact and is still worth having — the first ring keeps upstream's whole-array
reservation, and the parsed tile is cached rather than transient. It could never
help with the **peak**, which is what the module dies on.

## Local changes

Seven, and nothing else. Two of them (5 and 6) are this workspace's CI applying
itself to a new member rather than decisions — they are listed because a reader
diffing against the registry copy will hit them first and should be able to
stop reading.

1. **`src/lib.rs`, `parse_geometry`: the two per-ring re-reservations are
   `Vec::new()`.** The first reservation, before the loop, is upstream's
   unchanged. This is the whole of the behaviour delta and it is three lines.

2. **`src/peak_allocation_tests.rs` is new, and `src/lib.rs` declares it under
   `#[cfg(test)]`.** The pin described above. It installs a counting
   `#[global_allocator]`, which is why it is a module of this crate rather than
   an integration test: it has to be inside the binary whose allocations it
   measures, and it is thread-local so a parallel test cannot be read as this
   one's.

3. **The `wasm`, `protoc` and `protoc-generated` features are gone**, with the
   optional dependencies they gated (`wasm-bindgen`, `serde-wasm-bindgen`,
   `js-sys`, `geojson`, `serde`, `serde_json`, `prost-build`), the `build.rs`
   that only ran under two of them, the 208-line `pub mod wasm` that only
   compiled under the third, and the `#[cfg(feature = "protoc")]` fork in
   `src/vector_tile.rs`. Also `crate-type` loses its `cdylib`, which existed
   for the `wasm` feature's JS package.

   **This is the lockfile, not tidiness.** Cargo locks a workspace *member's*
   optional dependencies whether or not anything enables them, so keeping the
   declarations added twelve packages to `Cargo.lock` — measured, on the first
   attempt — including a second major of `prost` (0.14, beside the 0.13 this
   crate actually decodes with) dragged in by `prost-build`. Nothing in this
   workspace enables any of the three features, and the committed protobuf
   bindings in `src/generated/` are what the decode uses either way.

4. **`[dev-dependencies]` are gone** (`serde`, `serde_json`, and a second
   `prost` entry with default features). They served the repository's test
   suite, which the tarball does not carry — see the section above. Nothing was
   dropped that could have run.

5. **`cargo fmt` has been applied**, which reindents every file from upstream's
   two spaces to rustfmt's four and rewraps some attributes. It is not a
   choice: `.github/workflows/clippy.yaml`'s `fmt` job runs `cargo fmt --all`
   and **commits the result back**, so the alternative is not an unformatted
   copy, it is a `chore: Apply rustfmt` commit landing on this directory the
   first time CI sees it. `vendor/walkers` is in the same state for the same
   reason. Diff this directory against the registry copy with `diff -w` or
   `git diff -w` and the whitespace disappears.

6. **Two `clippy::manual_unwrap_or` sites in `parse_geometry` are rewritten**
   (the `cursor[0]`/`cursor[1]` overflow clips, `match ... { Some(r) => r, None
   => i32::MAX }` becoming `.unwrap_or(i32::MAX)`). Same value, same clip; the
   same workflow gates on `cargo clippy --all-targets --all-features -D
   warnings`, and a warning here is a red board rather than a warning.

7. **`src/interned.rs` is new, and `Reader::get_interned_layer_as` beside
   `get_features_as` reads through it.** The same decode with the layer's key
   and value tables **kept** instead of expanded into a `HashMap` per feature.
   `intern_tags` is `parse_tags`'s twin: the same wire validation — pairs, and
   both indices inside their tables, the same `InvalidTags` — appending the
   `(key index, value index)` pair to a per-layer arena instead of cloning a
   key `String` and a value into a map.

   **This is additive and nothing was rerouted.** `get_features_as` is
   untouched, so `squallar-buildings`' footprint parse and this crate's own
   `peak_allocation_tests` still run through exactly the code they did.
   `parse_geometry` is not touched either — change 1 is still the whole of the
   behaviour delta in it.

   **Why the caller could not do this itself.** A `Layer` message's `keys` and
   `values` are behind `Reader`'s private `tile` field and the private
   `vector_tile` module, so the tables were unreachable from outside this
   crate: a dependent could only take the expanded bags and re-intern them,
   which pays the whole cost first and then throws it away. `vendor/walkers`'
   `mvt::parse` is the caller, and its `VENDORED.md` carries the measurement —
   on the committed Monaco z14 fixture the expanded bags were **1,447,103 B of
   a parsed tile's 2,092,002**, against **166 keys and 1,848 values** in the
   tables they came from.

   One thing this deliberately does **not** do: drop a repeated tag key. A tag
   list naming one key twice is malformed, and `get_features_as` resolves it
   through `HashMap::insert`, so the last write wins. Choosing that here would
   decide the question for every caller off a wire detail; the arena keeps both
   pairs in order and the caller that wants the map's answer reads its window
   backwards. `vendor/walkers`' `LayerProperties::pair` does, and says so.

The `include`, `keywords` and `repository` keys are left as upstream wrote them
even though this copy is never published, so the manifest still reads as a diff
against the registry version.

## Re-applying this onto a later upstream release

Change 1 is the one that matters and it is three lines in one function; find
`parse_geometry` and check whether the per-ring `coordinates = ...` assignments
still reserve `geometry_data.len()`. Change 7 is the largest by line count and
the easiest to re-apply: one new file plus one method and one free function,
none of which upstream has anything at the same place. If upstream has fixed it, this directory
can go: delete the member entry, the `[patch.crates-io]` line and the
`[profile.dev.package.mvt-reader]` override in the root `Cargo.toml`, and move
`src/peak_allocation_tests.rs` somewhere it can still run against the registry
copy — the pin outliving the patch is the point of having measured it.
