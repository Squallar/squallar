# walkers, vendored

This directory is a copy of a crates.io crate that this workspace maintains
locally. It is not our code. Everything in it is upstream's except what is
listed under [Local changes](#local-changes) below, and keeping that list short
and true is the whole point of the file — it is what makes the directory
reviewable, and what lets a later upstream release be adopted by re-applying a
delta somebody can still read.

It is the fourth such directory, after `vendor/nexrad-decode/VENDORED.md`,
`vendor/nexrad-data/VENDORED.md` and `vendor/bzip2-rs/VENDORED.md`, and it
follows their shape deliberately.

**These changes are not going upstream.** That is this workspace's standing
stance, taken for `vendor/nexrad-decode` on 2026-08-12 and applied here on
2026-08-26. No pull request will be filed. The consequence is worth stating
plainly: upstream will not carry these changes, so this directory is not a
stopgap until a release ships — it is where this workspace's map widget lives,
indefinitely. A later upstream release is still worth adopting for everything
else it brings, and re-applying this delta onto it is the job the short list
below exists to make possible.

## Provenance

| Field | Value |
| --- | --- |
| Package | `walkers` |
| Version | `0.56.0` — the newest published version as of 2026-08-26 |
| Source | crates.io, `sha256:936a0d5741eb3dcd0fc2da5ec819447147f0c296492a54f20318e76f3b2f37bc` |
| Unpacked from | `~/.cargo/registry/cache/index.crates.io-1949cf8c6b5b557f/walkers-0.56.0.crate` |
| Upstream repo | <https://github.com/podusowski/walkers>, subdirectory `walkers` (`path_in_vcs`) |
| Upstream commit | `b587a5664e9a2b601b0b2ae971e598bdaf50c50c` (from the tarball's `.cargo_vcs_info.json`) |
| Author | Piotr Podusowski |
| License | MIT — full text in `LICENSE` next to this file |

The checksum above was verified against the local registry cache before
extraction, and the tarball was extracted from that cache rather than fetched.

The packaged tarball ships **no** license file, only `license = "MIT"` in the
manifest. `LICENSE` here was reconstructed from the upstream repository's own
root `LICENSE` at the pinned commit, copyright line included, because "MIT" in
a metadata field is a declaration and not the notice the license requires us to
redistribute.

## Why this exists

Unlike the three sibling directories, this one does not exist for a single
named defect. It exists because this workspace draws every basemap through
`walkers::Map` and needs to keep changing the widget — style parsing, layer
fidelity, `serde` on `Circle`, visibility — none of which has a seam to work
around from outside the crate.

The first thing it buys, and the one the two commits that created this directory
are about, is a graph trim. walkers reaches the network through its own
`HttpTiles` / `io` / `pmtiles` stack. **Nothing in this workspace calls it.**
Both `Map::new` sites (`squallar-egui/src/ui_map.rs:238` and `:1236`) pass
`None` for tiles, and `squallar-egui/src/tile_source.rs` is this workspace's own
full reimplementation of the `Tiles` trait — its module docs already name
`walkers::HttpTiles` as "one path that bypassed" the shared HTTP client. The
only occurrences of `HttpTiles` outside this directory are those three doc
comments.

That unused stack pulls a **second** `reqwest`: walkers depends on `reqwest
"0.12"` while this workspace pins `=0.13.4`. See
[The dependency graph](#the-dependency-graph) for what that costs and what
deleting it recovers.

Vendored rather than forked because a git dependency would put this workspace's
build on somebody's branch surviving; vendored rather than pinned-and-waited
because 0.56.0 is the newest published version.

## Local changes

This is the complete list. `diff -rq` against the unpacked tarball must produce
exactly these entries and nothing else.

### Removed — packaging residue

| Path | Why |
| --- | --- |
| `.cargo_vcs_info.json` | Recorded in the provenance table above instead. |
| `Cargo.toml.orig` | Upstream's pre-normalisation manifest, which refers to workspace inheritance and a sibling `path` dev-dependency (`hypermocker`) that does not exist here. |
| `Cargo.lock` | A packaged crate's lockfile is inert; this workspace's root `Cargo.lock` is the one that resolves anything. |

There was no `.cargo-ok` and no `default_*.profraw` to remove; the cache
tarball is clean.

### Removed — the test module that cannot compile

`src/http_tiles.rs`'s `#[cfg(test)] mod tests` (lines 170–449 as published, 9
`#[tokio::test]` fns) is deleted, and this one is **not optional**. It is built
on `hypermocker`, which upstream declares as
`hypermocker = { path = "../hypermocker" }` — a path-only dev-dependency, which
Cargo strips when it packages the crate. It is therefore absent from the
published `Cargo.toml`, absent from the tarball, and unobtainable: the module
cannot compile anywhere outside upstream's own checkout, including in upstream's
own published crate. Making walkers a workspace member is what makes that
visible, because a member's test targets are actually built.

Deleting it is a straight loss of 9 tests and worth naming as one. It is a
smaller loss than it reads: those tests drive `HttpTiles`, which the second
commit deletes outright as unused.

### Removed — dev-dependencies

| Dependency | Why |
| --- | --- |
| `eframe` | Upstream's, for a `demo/` the tarball does not contain. `autoexamples` is `false` and there is no `examples/` directory, so nothing here can use it. As a non-member dependency walkers' dev-deps were never resolved at all; as a member they are, so keeping it would have materialised eframe and its winit/glow tree in `Cargo.lock` to serve nothing. |
| `env_logger` | Used only by the `http_tiles.rs` test module deleted above. |

`approx` stays — `src/mercator.rs`, `src/position.rs` and `src/projector.rs`
all use it, and those tests do run here.

### Removed — the HTTP stack

`src/http_tiles.rs`, `src/io/` (5 files) and `src/pmtiles.rs` are deleted
outright, together with `EguiTileFactory` in `src/tiles.rs` — the one item
outside those files that implemented an `io` trait — and their `mod`/`pub use`
lines in `src/lib.rs`.

The claim that nothing here uses them was checked, not assumed. Both `Map::new`
call sites in this workspace (`squallar-egui/src/ui_map.rs:238` and `:1236`)
pass `None` for tiles; `squallar-egui/src/tile_source.rs` is a full local
reimplementation of the `Tiles` trait, written precisely because `HttpTiles`
"bypassed" the shared HTTP client; and `grep -rn HttpTiles` over the workspace
outside this directory returns three hits, all of them doc comments in that
file.

The public API this removes is `HttpTiles`, `Stats`, `HttpOptions`,
`HeaderValue`, `MaxParallelDownloads` and (under its feature) `PmTiles`. The
`pmtiles` feature is deleted with the module it gated. `LocalTiles` is
untouched — it never used `io`.

Ten dependencies leave with it: `reqwest`, `reqwest-middleware`,
`http-cache-reqwest`, `tokio`, `futures`, `bytes`, `getrandom`,
`wasm-bindgen-futures`, the optional `pmtiles`, and `egui_extras`. All four
`[target.'cfg(…)']` tables go with them, so what remains is
platform-independent. See [The dependency graph](#the-dependency-graph).

`egui_extras` is on that list for the next reason rather than this one.

### Removed — the bundled assets

All of `assets/` (7 files, 456 KB), and it splits into three groups:

| Files | Why |
| --- | --- |
| `mapbox-logo-black.svg`, `mapbox-logo-white.svg` | Third-party **trademarked** assets. They were reached only by two `egui::include_image!` calls in `src/sources/mapbox.rs`'s `Attribution`, now `None`. `egui_extras`, whose `svg` feature existed to rasterise them, is unused once they are gone. |
| `protomaps-{dark,dark-vis,light}.json`, `openfreemap-bright.json` | 449 KB of third-party basemap style JSON, compiled into every binary by `include_str!`. This workspace ships its own styles. |
| `blank-255-tile.png` | Referenced by nothing. It was test-fixture data for the `hypermocker` module already deleted in the previous commit. |

### Removed — the four bundled-style constructors

`Style::protomaps_dark()`, `protomaps_dark_vis()`, `protomaps_light()` and
`openfreemap_bright()` in `src/style.rs` go with the JSON they `include_str!`.

They are worth naming separately from the bytes they carried, because they are
also **the reason style loading is infallible today**. Each is
`serde_json::from_str(…).expect("failed to parse style JSON")` returning a bare
`Self`, so the only constructors `Style` had could not report a parse failure —
they could only panic. Deleting them is the prerequisite for the fallible
parsing that lands next; that work is not in either of these commits.

### Kept deliberately

- **All but one of the 38 in-source `#[test]` fns.** They are the behaviour pin on
  the code the later patches change, and the bar for any change to this
  directory is that they stay green. See
  [What the pin actually selects](#what-the-pin-actually-selects) for the part
  of that sentence that is smaller than it looks.

  **Exactly one was deleted**, and only by the second commit:
  `style::tests::test_style_parsing`, which called two of the four bundled-style
  constructors removed above and had nothing left to call. Measured:
  `cargo test -p walkers --all-features` reports 37, where the tarball has 38.
  Nothing in `src/expression.rs` was touched — all 22 of its tests are still
  there and still pass.

  The 9 deleted in the first commit are a disjoint set, and the attribute is
  what separates them: the 38 are `#[test]`; the 9 were `#[tokio::test]`, all in
  `http_tiles.rs`, all `hypermocker`-driven.

  **Two were added by the third commit**, both in a fresh `style::tests` — the
  module the deletion above had emptied. They are ours, not upstream's, and they
  are the pin on the two `style.rs` changes that commit makes. Measured:
  `cargo test -p walkers --all-features` reports **39**.
- `README.md` — required, not decoration: `src/lib.rs` opens with
  `#![doc = include_str!("../README.md")]`, so deleting it is a compile error.

### Added

| Path | Why |
| --- | --- |
| `LICENSE` | See above — the tarball ships none. |
| `VENDORED.md` | This file. |

### Changed — `Cargo.toml`

Beyond the dev-dependency removals already listed: a vendoring note under the
generated header, and two lint tables.

`package.version` stays **exactly** `0.56.0`, with no `+local` metadata. A
`[patch]` is only accepted for a version satisfying the requirement it
replaces, and the root `[workspace.dependencies]` pin is `walkers = "=0.56.0"`.

The lint tables are not cosmetic. `.github/workflows/clippy.yaml` runs
`cargo clippy --all-targets --all-features --fix` across every workspace member
and **auto-commits the rewritten tree to `main`**, then gates on `-D warnings`.
Without the tables, a bot would eventually rewrite upstream source here on its
own schedule and this file would stop being true. With them, the bot has
nothing to fix and the gate has nothing to fail on. The manifest itself carries
the two mechanics worth knowing before editing them — why the rustc side names
groups rather than `warnings`, and what a source-level `#![deny(…)]` outranks.

One difference from `vendor/nexrad-decode` worth stating because its absence
could look like an omission: **no lint scoping was needed in source.**
`src/lib.rs`'s `#![deny(clippy::unwrap_used, rustdoc::broken_intra_doc_links)]`
is upstream's and outranks the tables. It stays.

**Correction, measured on the third commit: it is *not* "already clean on this
tree", which is what this paragraph used to claim.** `cargo clippy -p walkers
--all-targets` exits **101 with 6 `clippy::unwrap_used` errors**, and
`--all-features` gives the same 6. They are all in upstream's own
`#[cfg(test)]` code — `src/projector.rs:100` and `:141`, `src/zoom.rs:66`,
`:67`, `:72`, `:80` — reached because `--all-targets` builds the test targets,
which a plain `cargo clippy` does not. The `[lints.clippy] all = "allow"` table
cannot suppress them: a source-level `deny` outranks a command-line allow, which
is the very mechanic the paragraph above describes.

This is **pre-existing and not caused by the third commit**. Verified by
stashing that commit's changes and running the same command on `a152805c`: 6
errors, byte-identical list, exit 101 both times. It is recorded here rather
than fixed because fixing it means editing upstream test bodies — a much larger
delta than anything else in this file, for lint compliance rather than
behaviour.

What it means for CI is worth stating plainly, because it is the reason this
matters at all: `.github/workflows/clippy.yaml` runs `--all-targets
--all-features` over every member and gates on it, so that job is red on this
directory independently of any patch here. Whoever owns that gate has to
choose — scope the lint in source (the `vendor/nexrad-decode` remedy this
paragraph says was not needed), or exclude the member. Nothing in the third
commit changes the count in either direction.

### Changed — source

Nine files. Six of them arrived in three groups — still six there, because the
third group touches only `src/lib.rs` and `src/style.rs`, which the first two
had already changed. The other three arrive with the fifth and sixth commits
described at the end of this section: `src/expression.rs`, then
`src/mercator.rs` and `src/local_tiles.rs` (the sixth also touches `src/tiles.rs`
and `src/lib.rs`, which were already on the list). Nothing in the first three
groups changes how a tile that this workspace actually renders is drawn.

The first commit changed two files, and both changes were forced by the
packaging rather than chosen:

1. **`src/http_tiles.rs`** — the `hypermocker` test module deleted, as above.
   Nothing outside `#[cfg(test)]` is touched; the file is upstream's through
   line 168 and ends there.
2. **`README.md`, one word** — the quick-start fence at line 34 is now
   ```` ```rust,ignore ```` rather than ```` ```rust ````. Because `lib.rs`
   includes the README as crate docs, that fence is a doctest, and it opens
   `use eframe::{App, Frame};`. With `eframe` removed it does not compile;
   with `eframe` kept it would compile at the cost of the whole eframe tree in
   the lockfile, to typecheck a snippet that constructs a windowed application.
   `ignore` and not `no_run`, deliberately: `no_run` still compiles the snippet
   and so still needs the dependency.

   It is also the change that makes this fence survive the second commit, which
   deletes the `HttpTiles` the snippet is built around. The snippet still names
   `HttpTiles` and is now wrong as documentation; it is left that way
   deliberately, so that the delta stays a deletion list rather than a rewrite
   of upstream's prose.

Then, in the second commit, four files lose the code that reached the deleted
modules and assets and nothing else:

- **`src/lib.rs`** — the `mod` and `pub use` lines for `http_tiles`, `io` and
  `pmtiles`.
- **`src/tiles.rs`** — `use crate::io::TileFactory;` and the `EguiTileFactory`
  struct and its two impls. Everything else in the file, including all of
  `Tile`, `TileId`, `TilePiece` and the `Tiles` trait this workspace
  implements, is upstream's.
- **`src/style.rs`** — the four bundled-style constructors and the one test
  that called them.
- **`src/sources/mapbox.rs`** — two `Attribution` fields go from
  `Some(egui::include_image!(…))` to `None`.

The third commit is the first that is chosen rather than forced, and it is the
first that adds code here instead of removing it. Three changes, two files:

1. **`src/style.rs`, one attribute — the `Circle` serde defect.**
   `Layer::Circle` was the only variant carrying a `source_layer` field without
   the `#[serde(rename_all = "kebab-case")]` its `Fill`, `Line` and `Symbol`
   siblings all have. The enum-level `rename_all` renames *variants*, not
   fields, so `Circle` alone expected a literal `source_layer` where MapLibre
   writes `source-layer`.

   The blast radius is larger than one variant, and that is the reason to fix
   it rather than route around it: `source_layer` is `String` and not
   `Option<String>`, so the miss is a hard `missing field` error, not a
   dropped value — and `Style` is a single `Vec<Layer>` of an internally-tagged
   enum, so serde fails the *whole* style. One `circle` layer anywhere in a
   MapLibre style takes the entire parse down with it.

   Verified before it was changed, not after. Against upstream's code the new
   test fails with
   ``Error("missing field `source_layer`", line: 5, column: 13)`` on a style
   whose other layer is a perfectly good `background`.

   The fix is the missing attribute and nothing else — `Circle` now matches its
   three siblings exactly, `source_layer` deliberately stays a non-optional
   `String` as theirs are. `style::tests::circle_layer_with_kebab_case_source_layer_parses`
   pins it.

2. **`src/style.rs`, `Style::from_json`.** A `pub fn from_json(&str) ->
   Result<Self, serde_json::Error>`, added because the *Removed — the four
   bundled-style constructors* entry above left `Style` with no way to be built
   from JSON at all. Those four were also the crate's only `.expect("failed to
   parse style JSON")` sites, so this is the fallible replacement that entry
   anticipated: this workspace loads its own styles and must be able to report a
   bad one rather than abort. Kept to one function on purpose — it is a thin
   wrapper over `serde_json::from_str`, not a new style-loading API.
   `style::tests::from_json_reports_a_parse_failure_instead_of_panicking` pins
   that it returns `Err` rather than panicking.

3. **`src/lib.rs`, visibility — the one structural change.** `mod style`, `mod
   text` and `mod mvt` become `pub mod`, and `Layout`, `Text`, `OccupiedAreas`,
   `render` and `transformed` join the existing re-exports.

   This is the change the other two are in service of. Until it, the only way
   into the vector pipeline from outside was `Tile::new`, which hands back an
   opaque `Tile` and keeps style loading, text layout and label collision
   sealed inside this directory — so every later fidelity fix to any of them
   would have had to land *here*, growing the delta this file exists to keep
   short. With `mvt::render` reachable, a dependent crate can hold the returned
   `Vec<ShapeOrText>` itself and do that work in its own tree; for this
   workspace that means `squallar-egui`.

   The `#[cfg(not(feature = "mvt"))]` dummy `mod style` is made `pub` too, so
   that `walkers::style::Style` resolves under either feature selection rather
   than only one. Nothing in this change alters behaviour: no function body
   moved and no item's contents changed, only which of them a dependent crate
   can name.

### Changed — source, fifth commit: three defects in the expression evaluator

`Style::from_json` (above) is what makes these live. Before it, a `Style` could
only be `Default`, so no attacker-shaped or merely sloppy expression ever
reached the evaluator; now this workspace hands it JSON it did not author.

All three are reachable from style JSON alone, and none of them is caught by
`deny(clippy::unwrap_used)` — two are slice indexing, which that lint does not
see at all.

1. **`interpolate` with an odd-length stop list panicked.** Stops were paired
   with `.chunks(2).map(|chunk| (chunk[0].clone(), chunk[1].clone()))`; a
   trailing unpaired stop gives a one-element final chunk and `chunk[1]`
   panics. Now each chunk goes through the existing `two_elements` helper, so
   the odd list is `Error::TwoElementsExpected`.

   Verified against the unmodified file:
   `index out of bounds: the len is 1 but the index is 1`, at
   `src/expression.rs:195`.

2. **`interpolate` with no stops at all panicked.** When no surrounding stop
   pair is found the code clamped with `stops[0]` and `stops[stops.len() - 1]`.
   `stops` can legitimately be empty: it comes from `first_and_rest`, whose
   name promises two but whose body is `split_first`, guaranteeing one. So
   `["interpolate", ["linear"], 5]` — a valid-looking expression with the
   interpolation type and the input and nothing else — indexed an empty vector.
   Now the clamp goes through `first()`/`last()` and an empty list is
   `Error::InterpolateStopNotFound`, the same error the no-match arm already
   returned.

   Verified against the unmodified file:
   `index out of bounds: the len is 0 but the index is 0`, at
   `src/expression.rs:218`.

3. **`!=` resolved only its left operand.** `==` calls
   `property_or_expression` on both sides; `!=` called it on the left and
   compared against the raw right-hand `Value`. So `["!=", "class", ["get",
   "x"]]` compared a resolved property against the literal JSON array
   `["get", "x"]` — never equal, so `!=` was unconditionally true whenever its
   right side was an expression. `!=` is now the exact mirror of `==`.

   Verified against the unmodified file: the new test read `Bool(true)` where
   `Bool(false)` is correct — a silently wrong answer, which is why this one
   had no chance of being noticed as a crash.

Pinned by `expression::tests::test_interpolate_with_an_odd_stop_list_is_an_error`,
`…::test_interpolate_with_no_stops_is_an_error` and
`…::test_not_eq_operator_evaluates_both_sides`. Each was written first and run
against the unmodified file; the failure text above is what it actually printed.
The 22 upstream expression tests are unchanged and still pass, so the evaluator's
existing behaviour is pinned across the change.

### Changed — source, sixth commit: the tile grid past zoom 31, and three exports

Two changes, related only because the same three helpers are involved in both.

**A public-API panic.** `mercator::total_tiles` was `2u32.pow(zoom as u32)` over
a `u8` zoom, so every zoom from 32 up overflowed — panic in debug, wrap in
release. That is reachable from outside the crate, not just internally:
`TileId::east` and `TileId::south` are `pub` and both compute
`total_tiles(self.zoom) - 1`, and `TileId` is a `pub` struct with a `pub zoom:
u8` field, so any caller can construct one. `total_tiles` now returns
`Option<u32>` — `None` above zoom 31, the last the `u32` grid holds — and the
three callers thread it through: `east` and `south` return `None`, `valid()`
returns false.

Verified against the unmodified file, on probes shaped to compile against the
old signatures. `total_tiles(32)`, `TileId { zoom: 32, .. }.east()`, `.south()`
and `.valid()` each panicked with `attempt to multiply with overflow` (raised
inside `core`'s `pow`); `interpolate_from_lower_zoom(TileId { zoom: 2, .. }, 3)`
panicked with `assertion failed: tile_id.zoom >= available_zoom` at
`src/tiles.rs:316`.

Pinned by `mercator::tests::total_tiles_has_no_answer_past_the_u32_grid`
(`Some` for every zoom 0..=31, `None` for every zoom 32..=255),
`tiles::tests::tile_id_past_the_u32_grid_is_not_valid` and
`tiles::tests::interpolating_from_a_deeper_zoom_is_none`. Upstream's
`tiles::tests::tile_id_cannot_go_beyond_limits` is untouched and still passes,
so the ordinary edge-of-grid behaviour is pinned across the change.

**Three helpers become public.** `mercator::total_tiles`, `TileId::valid` and
`tiles::interpolate_from_lower_zoom` were `pub(crate)` or `pub(crate)`-by-module,
and `squallar-egui/src/tile_source.rs` reimplements all three verbatim — its own
doc comments say so, naming the visibility as the reason. That is exactly the
kind of duplicate this directory is supposed to make unnecessary. All three are
now `pub`; `tiles` is a private module, so `interpolate_from_lower_zoom` is
re-exported from `src/lib.rs` alongside `Tile`, `TileId`, `TilePiece` and
`Tiles`. `mercator` is already `pub mod`.

`interpolate_from_lower_zoom` also stops asserting. It `assert!`ed
`tile_id.zoom >= available_zoom` and then subtracted; it now returns
`Option<(TileId, Rect)>`, `None` for a deeper ancestor (`checked_sub`) or a
ratio that does not fit a `u32` (`checked_pow`). `src/local_tiles.rs` is the one
in-crate caller and takes the `Option` with a `?` inside a `find_map` closure
that already returns `Option<TilePiece>` — a zoom candidate with no ancestor is
skipped, which is what the loop already did with every other miss.

Deleting the `squallar-egui` copies is deliberately **not** part of this commit;
this one is additive to `vendor/walkers` and changes nothing outside it.

`diff -rq` against the unpacked tarball, in full, is exactly:

```text
Files <tarball>/Cargo.toml   and vendor/walkers/Cargo.toml   differ
Files <tarball>/README.md    and vendor/walkers/README.md    differ
Files <tarball>/src/expression.rs and …/src/expression.rs differ
Files <tarball>/src/lib.rs   and vendor/walkers/src/lib.rs   differ
Files <tarball>/src/local_tiles.rs and …/src/local_tiles.rs differ
Files <tarball>/src/mercator.rs and …/src/mercator.rs differ
Files <tarball>/src/sources/mapbox.rs and …/src/sources/mapbox.rs differ
Files <tarball>/src/style.rs and vendor/walkers/src/style.rs differ
Files <tarball>/src/tiles.rs and vendor/walkers/src/tiles.rs differ
Only in <tarball>: assets
Only in <tarball>: Cargo.lock
Only in <tarball>: Cargo.toml.orig
Only in <tarball>: .cargo_vcs_info.json
Only in <tarball>/src: http_tiles.rs
Only in <tarball>/src: io
Only in <tarball>/src: pmtiles.rs
Only in vendor/walkers: LICENSE
Only in vendor/walkers: VENDORED.md
```

Every other file under `src/` — `center`, `map`, `memory`, `mvt`, `options`,
`plugin`, `position`, `projector`, `text`, `zoom`, and the rest of `sources/` —
is byte-for-byte upstream's.

## What the pin actually selects

"Upstream's 38 inline tests are the behaviour pin" is the reason this crate is
a workspace member rather than an `exclude`, and it is true with a caveat large
enough that stating the number alone would mislead.

**23 of the 38 do not compile under this workspace's feature selection.** The
22 in `src/expression.rs` and the 1 in `src/style.rs` live in modules gated
`#[cfg(feature = "mvt")]` in `src/lib.rs`, and nothing here enables `mvt` —
`walkers = { workspace = true }` in `squallar/Cargo.toml` and
`squallar-egui/Cargo.toml` names no features, and `default = []`.

Measured, `cargo test --workspace`, exit 0 at every point:

| | count |
| --- | --- |
| walkers unit tests selected | **15** — mercator 2, position 4, projector 4, tiles 2, zoom 3 |
| walkers doctests selected | 1 (`src/map.rs`) + 1 ignored (the README fence above) |
| workspace total, before vendoring | 4,521 passed |
| workspace total, after commit 1 | 4,537 passed |
| workspace total, after commit 2 | 4,537 passed |

The +16 is exactly those 15 plus the one doctest. Commit 2 moves the total by
**nothing**: the single `#[test]` it deletes is the mvt-gated one in
`style.rs`, which was never in a default-feature run to begin with. The other 23
arrive the day `mvt` is enabled, which is the same day the style and expression
patches land — so the pin is real for the work it is meant to guard, and the
guard is simply not armed yet. Do not quote "38" as a figure this workspace
measures today.

`--all-features` is the other half of that sentence: it *does* enable `mvt`, and
`cargo llvm-cov --all-features` in `.github/workflows/test.yaml` and
`cargo clippy --all-targets --all-features` in `clippy.yaml` both pass it. Those
rows compile the mvt modules and run the inline tests in them.
`cargo test -p walkers --all-features` reports **37 passed**, against the
tarball's 38 — the one absent is `style::tests::test_style_parsing`. The 15
above is the figure for a default-feature run, and the two are not
interchangeable.

### A correction to the paragraph above, from the third commit

The sentence "the other 23 arrive the day `mvt` is enabled" was **wrong by one
when it was written**, and the error is worth keeping visible rather than
silently patching, because the figure was quoted forward.

There were 23 mvt-gated tests *in the tarball*. But the second commit deleted
`style::tests::test_style_parsing` — which was one of those 23 — leaving
`src/style.rs` with no `mod tests` at all. So the number waiting on `mvt` was
already **22**, all of them in `src/expression.rs`, from the moment that commit
landed. The `37 = 15 + 22` in the paragraph above is self-consistent with that
and is the arithmetic that gives it away; only the prose said 23.

Measured on this tree, `cargo test -p walkers --features mvt` before the
`style.rs` changes: `1 passed; 1 failed; 37 filtered out` — the 37 filtered are
the whole pre-existing suite, and 37 − 15 = 22.

After the third commit the count waiting on `mvt` is **24**: those 22 plus the
two new `style::tests` fns, which are mvt-gated for the same reason — `mod
style` is behind the feature.

### The guard is now armed — fourth commit

Nothing above this line waits any more. `squallar-egui` enables `mvt`
unconditionally, so a plain `cargo test --workspace` selects all 24:

| | count |
| --- | --- |
| walkers unit tests selected, before | 15 |
| walkers unit tests selected, now | **39** — the 15, plus 22 in `expression.rs`, plus 2 in `style.rs` |
| workspace total, after commit 2 | 4,537 passed, 91 suites, exit 0 |
| workspace total, after commit 4 | **4,561 passed, 91 suites, exit 0** |

The +24 is exactly the 22 plus the 2, and **all 22 expression tests pass
unmodified** — the evaluator needed nothing. That matters more than the count:
those 22 are the behaviour pin on the expression evaluator, and any later
fidelity work on `expression.rs` or `style.rs` now has a real gate under it
rather than a dormant one. Suite count is unchanged at 91; no new target
appeared, an existing one grew.

The sentence "Do not quote 38 as a figure this workspace measures today" still
holds, for a new reason: the figure this workspace measures today is **39**, and
it is not upstream's 38 — it is 37 of upstream's plus 2 of ours.

## rustfmt

`cargo fmt -p walkers -- --check` is clean over this directory as vendored,
checked with the toolchain `rust-toolchain.toml` pins — which is `stable`, i.e.
a **floating** version. rustfmt's output is version-dependent, and
`vendor/nexrad-decode/VENDORED.md`'s note on what to do when a future stable
release disagrees applies here unchanged: rebase onto a newer upstream release
that has the fix (best — it shrinks this directory), or accept the reformat as a
single, separately-titled commit that touches nothing else, and say so here.

Package-scoped, and that is not a stylistic preference: this workspace forbids
`cargo fmt --all` outright, because a workspace-wide format pulls another
worktree's in-flight files into the tree running it.

There is no `rustfmt.toml` in this workspace and upstream's tarball ships none,
so this directory is formatted by the same default rules as the rest of it, and
happens to be clean under them already.

## Two other bots that can reach this directory

- **Renovate** — already handled, and not by these commits.
  `.github/renovate.json` lists `vendor/**` in `"ignorePaths"` because of the
  first sibling directory, and that glob covers this one too. It matters here
  for the same reason it mattered there: this manifest's `egui`, `image`,
  `lru` and `thiserror` declarations are upstream's, not ours to move, and a
  bump merged here would rewrite the vendored crate and quietly make the
  *Local changes* list above untrue. Nothing to do; recorded so the next person
  does not go looking.
- **Coverage** — not handled, named so it is not a surprise.
  `cargo llvm-cov --all-features` in `test.yaml` now measures this crate too,
  and the badge and `.github/coverage-baseline.tsv` it auto-commits move
  accordingly — 3,464 lines of widget joined the denominator (4,333 as
  vendored, before the HTTP stack came out), covered only to the extent
  upstream's own inline tests cover them. Note that the `--all-features` there
  means the mvt modules are in that denominator *and* their 22 remaining tests
  are in the numerator, unlike a default-feature run. The number changing is
  expected and is not a regression in this workspace's code. Nothing gates on a
  threshold, so nothing fails.

## The dependency graph

Two counts, and they have **different denominators and are never added**.
`[[package]]` blocks in `Cargo.lock` is everything the resolver locked, whether
or not a feature selects it. Distinct packages in `cargo tree --workspace` is
what a default-feature host build actually compiles. Measured at all three
points:

| | `Cargo.lock` blocks | `cargo tree --workspace` |
| --- | --- | --- |
| before vendoring (`bf7afc62`) | 664 | 468 |
| after commit 1 (vendored as published) | 712 | 469 |
| after commit 2 (HTTP stack deleted) | **649** | **420** |
| after commit 3 (the patch set) | 649 | 420 |
| after commit 4 (`mvt` enabled) | **649** | **443** |
| net vs. before vendoring | **−15** | **−25** |

The first row pair is the one that surprises. Commit 1 adds 48 lockfile blocks
while adding **one** compiled package. That is not the mvt feature being
switched on — it is that Cargo resolves a *workspace member's* optional and dev
dependencies into the lockfile whether or not they are activated, where for a
plain registry dependency it drops the ones no feature selects. So `geo`,
`lyon_path`, `lyon_tessellation`, `mvt-reader`, `color`, `pmtiles`, `serde_json`
and `approx` join the walkers entry and drag their own trees in. Only `approx`,
a dev-dependency, is compiled; the rest are locked, not built. The
`--all-features` CI rows are where the rest of them do get compiled.

Commit 2 then takes 63 blocks and 49 compiled packages back out, and the net
across both commits is a graph that is 48 packages *smaller* than before walkers
was vendored at all. That reduction is the concrete thing this pair of commits
buys, ahead of any patch.

### Turning `mvt` on, and why the lockfile does not move

The fourth commit sets `walkers = { workspace = true, features = ["mvt"] }` in
`squallar-egui/Cargo.toml`. The natural expectation is that `geo`, `lyon_path`,
`lyon_tessellation` and `mvt-reader` now "enter the lockfile". **They do not,
and `Cargo.lock` does not change at all** — 649 blocks before and after, a
zero-line `git diff`.

That is the first row-pair's surprise collected: those packages were locked the
moment walkers became a *member*, because Cargo resolves a member's optional
dependencies whether or not a feature selects them. Enabling the feature
activates them for compilation; it locks nothing new. **Lockfile blocks are the
wrong instrument for this change** — the one that moves is the compiled set.

`cargo tree --workspace` goes **420 → 443, +23 packages, none removed.** Of the
seven optional dependencies `mvt` activates, only four are actually new work:

| | |
| --- | --- |
| new, named by the feature | `geo`, `lyon_path`, `lyon_tessellation`, `mvt-reader` |
| already compiled | `color` (via `epaint` → `peniko`), `serde_json` and `serde` (workspace pins) |

and the other 19 are transitives those four drag in: `anyhow`, `float_next_after`
(**twice**, `1.0.0` and `2.0.0`), `geographiclib-rs`, `heapless`, `i_float`,
`i_key_sort`, `i_overlay`, `i_shape`, `i_tree`, `lyon_geom`, `prost`,
`prost-derive`, `rand`, `rand_core`, `rand_pcg`, `robust`, `rstar`, `sif-itree`.

Two of those are worth flagging rather than listing. `float_next_after` arrives
at two incompatible majors at once, so the graph compiles both. And `prost`
means `mvt-reader` brings a protobuf runtime — expected for a format that *is*
protobuf, but it is the largest single item in the 23 and the one most likely to
matter to a target that has to build it.

Which is the open question this row does not answer: **whether these 23 build on
`wasm32-unknown-unknown`, Android and iOS is not measured here.** Nothing in the
list is obviously host-only — there are no `[target.…]` tables left in this
manifest and none of the four named crates declares one — but "no obvious
blocker" is not a measurement, and this workspace's rule is to measure the real
platform rather than infer. Treat the figure above as host-only until somebody
runs the other three.

The pin the other three vendored directories use holds here too:

```console
$ cargo tree -i walkers
walkers v0.56.0 (…/vendor/walkers)
└── squallar-egui v0.1.0 (…/squallar-egui)
    ├── squallar v0.1.0 (…/squallar)
    ├── squallar-app v0.1.0 (…/squallar-app)
    …
```

Exactly one node, and it names the path. In `Cargo.lock` the walkers entry has
lost its `source` and `checksum` lines, which is the visible form of the
`[patch.crates-io]` redirect.

### The second reqwest, which is gone

`walkers` depended on `reqwest "0.12"` while this workspace pins
`reqwest =0.13.4`. Those do not unify, so the graph carried both — verified
before this work at `Cargo.lock:4019` (`reqwest 0.12.28`) and `:4057`
(`reqwest 0.13.4`).

`reqwest 0.12.28` was reached from exactly three packages, and all three were
walkers': `walkers` itself, `http-cache-reqwest` and `reqwest-middleware`.
Deleting the HTTP stack removes all three, and with them `quinn` and the rest of
0.12's HTTP/3 tail.

After commit 2, `grep '^name = "reqwest"' -A1 Cargo.lock` reports **one**
version, `0.13.4`. `http-cache-reqwest`, `reqwest-middleware`, `pmtiles`,
`quinn` and `egui_extras` are absent from the lockfile entirely.

### The second webpki-roots, which is not

**A correction worth carrying, because it was believed before it was checked.**
The graph also held two `webpki-roots` — `0.26.11` at `Cargo.lock:6064` and
`1.0.9` at `:6073` — and it is natural to read that as the same problem and
expect this work to fix it. It is not, and it does not.

`reqwest 0.12.28` depended on `webpki-roots 1.0.9`, the same one everything else
uses. The **only** dependant of `webpki-roots 0.26.11` in the whole lockfile is
`tungstenite 0.24.0`:

```console
$ cargo tree -i webpki-roots@0.26.11
webpki-roots v0.26.11
└── tungstenite v0.24.0
    └── ewebsock v0.8.0
        └── squallar-radar v0.1.0 (…/squallar-radar)
```

`ewebsock` is a direct `[workspace.dependencies]` pin of this workspace's own
(`=0.8.0`, `features = ["tls"]`), nothing to do with this directory.
`webpki-roots 0.26.11` is still in the lockfile after commit 2 and will stay
there. No work in this directory can remove it; the lever is `ewebsock`.

## Removing this directory

Since these changes are not being contributed, no upstream release will contain
them by way of this work. The directory therefore stays. The one case that
removes it is upstream independently arriving at a walkers that needs none of
the patches this workspace carries — worth checking whenever the pin is bumped.
If that ever happens:

1. Delete `vendor/walkers/`.
2. Delete `"vendor/walkers"` from `[workspace.members]`, the
   `[patch.crates-io]` entry, and `[profile.dev.package.walkers]` in the root
   `Cargo.toml`.
3. Bump the `[workspace.dependencies]` pin to that version.
4. `cargo tree -i walkers` should show one registry node again.

## Not going upstream

**Decided 2026-08-26: none of this is being contributed back.** No pull request
will be filed, and nothing here is being reported as an issue.

This is recorded because the shape of the work only makes sense against it. The
changes are committed as independent, separately-titled commits; the trims are
kept apart from anything behavioural; the deletions are each justified against
a grep rather than an assumption. That was all done to keep a contributable
diff, and it is worth keeping for a different reason — it is what makes the
delta re-appliable onto a later upstream release, and auditable in the meantime.
Read the structure that way rather than as a PR waiting to be sent.

One of the changes here could not go upstream even in principle, and it is
worth separating from the ones that are merely not being sent: the
`http_tiles.rs` test module is deleted because `hypermocker` is unpublishable
as declared, which is upstream's own packaging problem and not a patch this
directory could carry back.
