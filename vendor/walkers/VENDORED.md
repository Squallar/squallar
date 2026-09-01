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

The seventh commit removes `[dependencies.geo]` **and** `dep:geo` from the `mvt`
feature list. Both are required: deleting the table alone is a resolver error,
and deleting the feature entry alone leaves `geo` activated with nothing
importing it. `[dev-dependencies.approx]` stays — `mercator.rs`, `position.rs`
and `projector.rs` use it and are untouched.

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

Ten files. Six of them arrived in three groups — still six there, because the
third group touches only `src/lib.rs` and `src/style.rs`, which the first two
had already changed. The other four arrive with the fifth, sixth and seventh
commits described at the end of this section: `src/expression.rs`, then
`src/mercator.rs` and `src/local_tiles.rs` (the sixth also touches `src/tiles.rs`
and `src/lib.rs`, which were already on the list), then `src/text.rs`. Nothing
in the first three groups changes how a tile that this workspace actually
renders is drawn.

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

   **Half wrong, and corrected by the thirteenth commit.** The objection above
   is sound — an array on the right *is* an expression and must be evaluated —
   but "the exact mirror of `==`" was the wrong remedy, because `==` was
   itself wrong. `property_or_expression` on the right turns any filter
   *value* that collides with a property key into that property's value. It
   made twelve of this workspace's own style layers draw nothing. Only the
   first operand of a comparison is a property key; see
   [the thirteenth commit](#changed--source-thirteenth-commit-only-the-first-operand-of-a-comparison-is-a-property-key).

Pinned by `expression::tests::test_interpolate_with_an_odd_stop_list_is_an_error`
and `…::test_interpolate_with_no_stops_is_an_error`. The third pin,
`…::test_not_eq_operator_evaluates_both_sides`, asserted the wrong semantics and
was inverted by the thirteenth commit into
`…::test_not_eq_resolves_only_its_left_operand`. Each was written first and run
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

### Changed — source, seventh commit: label collision without `geo`

One file and one dependency. `src/text.rs` was the crate's **only** `geo::`
importer.

`OrientedRect` held a `geo::Polygon<f32>` built from a five-point `LineString`
and a `vec![]` of interiors, allocated once per label, and `intersects`
answered through `geo`'s polygon `Intersects` behind a bounding-box reject. It
now holds `[Pos2; 4]` and a plain `egui::Rect`, and answers with a
separating-axis test over the four candidate edge normals — two per rect, since
a rectangle's opposite edges are parallel. No allocation, no dependency.

The bounding-box reject stays exactly where it was, and the axis test is
deliberately **non-strict**: touching projections are not a separation. That is
what makes it agree with the reject rather than merely defer to it, so an
axis-aligned pair — which is every label but a line label, the only rotated
ones — gets the answer the bounding box already gave, touching cases included.

**Checked against the code it replaces, not against a belief about it.** Every
expected value in the new `text::tests` was read off the `geo` predicate on
those exact inputs, by running it, before it was deleted — including the two
rotated pairs whose bounding boxes overlap while the rects are apart, where
`geo` reports `bbox=true poly=false` and so does the axis test. All four axis
slots were then shown to be load-bearing by duplicating each one over its
neighbour in turn: each of the four turns a test red, and an AABB-only
regressor turns exactly the rotated cases red and nothing else.

Seven tests, `text::tests::*`. They are the first this directory has ever had
over label collision — upstream ships no `mod tests` in `text.rs` at all, which
is why the old predicate had to be run rather than trusted.

`geo-types` is **not** affected and stays. It is a separate, non-optional crate
that `src/mvt.rs` and `src/position.rs` genuinely use, and the name similarity
is the whole trap.

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
Files <tarball>/src/text.rs  and vendor/walkers/src/text.rs  differ
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
`plugin`, `position`, `projector`, `zoom`, and the rest of `sources/` — is
byte-for-byte upstream's.

That last sentence was true when the sixth commit was written and is not true
now: the subsection below takes `projector.rs` and `mercator.rs`, and the one
after it takes `map.rs` and `center.rs`.

### Changed — source, eighth commit: `Projector` stops recomputing what it holds

A pure refactor. No behaviour changes and no public signature changes — the
evidence for both is below.

A `Projector` is built once per frame from a snapshot of `MapMemory`, every one
of its methods takes `&self`, and all its fields are private and never mutated.
So the map's centre and the zoom are fixed for its whole life. It was resolving
both per call anyway:

- `unproject` recomputed the projected map centre from scratch on every call,
  even though the constructor had already computed and stored exactly that.
  `Center::position` → `detached` → `adjusted_position` → `AdjustedPosition::position`
  is `unproject(project(..))`, so each call cost a `powf`, a `tan`, an `asinh`,
  a `sinh` and an `atan`, plus a `project` on top of the result, before doing
  any of the work it was asked for. It now reads the stored field.
- `project` called `mercator::project`, which computes `total_pixels(zoom)` =
  `2f64.powf(zoom) * 256` **per point**. The projector now holds that scale as
  `world_pixels` and passes it to a new `mercator::project_at_scale`. Callers
  project many points per frame — `overlay_cache`, `ui_map`, `ui_map_pane` and
  `tiles` all do — and every one of them paid a `powf` per point.
- `calculate_meters_per_pixel` recomputed the same `total_pixels(zoom)` per
  call; it now takes the scale instead of the zoom.
- `mercator::unproject` carried its own copy of the
  `2f64.powf(zoom) * (TILE_SIZE as f64)` expression rather than calling
  `total_pixels`. It is now `unproject_at_scale(pixels, total_pixels(zoom))`,
  and `Projector::unproject` calls `unproject_at_scale` with its cached scale.

`mercator::project` and `mercator::unproject` keep their signatures and now
delegate to the `_at_scale` pair, so no caller outside the projector changed.

**A type bug fixed on the way through.** The field was declared
`map_center_projected_position: Position` but assigned the result of `project`,
which is `Pixels`. Both are aliases of `geo_types::Point`, so it compiled; it
was a projected pixel coordinate labelled as a lat/lon, and it made
`unproject`'s own comment ("Despite being in pixel space…") read as false. The
field is now `Pixels`.

**Two fields are gone.** With the centre and the scale resolved at construction,
nothing read `memory: MapMemory` or `my_position: Position` any more, and
`deny(warnings)` does not permit dead fields. Both are private, so this is not
an API change; `Projector::new` still takes `&MapMemory` and `my_position` and
still has the signature every one of its 11 call sites uses. Dropping the
`MapMemory` clone the constructor was taking is a small extra saving per frame.

**Evidence that nothing moved.** A throwaway harness dumped the raw IEEE-754
bits of `mercator::project`, `total_pixels`, `Projector::project`,
`Projector::unproject` (at four viewport points) and
`Projector::scale_pixel_per_meter` over a grid of 39 zooms × 9 latitudes × 9
longitudes, with the map detached away from `my_position` so the cached and
recomputed centres are distinguishable. 25272 lines, run against the tree
before and after the refactor:
`ee5230b024fd6958598c88479d06cbc210c649ee8a6874c9a8bfc613a14f8dbe` both times.
Not close — identical.

That harness is not in the tree. What is, standing in its place:

- `mercator::tests::a_precomputed_scale_projects_to_the_same_bits` and
  `…_unprojects_to_the_same_bits` spell out the pre-refactor bodies literally
  and `assert_eq!` on `f64::to_bits`, over 39 zooms × 49 positions.
- `projector::tests::the_cached_projection_state_is_bit_identical_to_recomputing_it`
  does the same for the cached centre, the cached scale, and the outputs of all
  three public methods, against the expressions each of them used to evaluate
  for itself.
- `projector::tests::unproject_is_inverse_of_project_when_the_map_is_detached`
  builds a projector whose `my_position` and whose `MapMemory` centre are on
  different continents, which is the only arrangement in which the cached and
  the recomputed centre could differ. **It is a guard, not a negative control:**
  the two expressions are identical today, so it passes before and after the
  change. It is there to keep them identical.

The two bit tests are not vacuous. Rewriting `total_pixels` as
`2f64.powi(zoom as i32) * (TILE_SIZE as f64)` — the plausible "optimisation" —
reds both of them and nothing else in the crate. Caching `my_position` in place
of `center_mode.position(my_position)` reds
`the_cached_projection_state_is_bit_identical_to_recomputing_it`; note that it
does **not** red the detached round-trip guard, because a consistently wrong
centre still inverts, which is exactly why that one is labelled a guard.

`projector::tests::test_equator_zoom_0` and `…_19` are mechanically re-pointed
from `calculate_meters_per_pixel(0.0, 0.)` to
`calculate_meters_per_pixel(0.0, total_pixels(0.))` to match the changed
parameter. Their asserted values are untouched.

### Changed — source, ninth commit: the frame stops paying for what it does not need

Three changes to `map.rs`, one supporting addition to `center.rs`, one to
`projector.rs`. Also a pure refactor.

**A short-circuit in the wrong order.** `handle_gestures` ended with
`if ui.ui_contains_pointer() && panning_enabled`. `ui_contains_pointer` is a
`Context::rect_contains_pointer` that ends in `Areas::layer_id_at` — an
O(layers) reverse scan of the area order taken under egui's memory lock
(`egui-0.35.0/src/memory/mod.rs:1224`). `panning_enabled` is two field reads.
The operands are swapped so the cheap one decides first. Both are
side-effect-free — the whole path from `Ui::ui_contains_pointer` down is `&self`
and takes the *read* lock (`Context::memory`, not `memory_mut`) — so the frame's
outcome is unchanged by construction; this is the one change in the commit with
no test of its own, and that is why.

The saving is not hypothetical: both `walkers::Map` construction sites in this
workspace (`squallar-egui/src/ui_map.rs:238` and `:1236`) call `.panning(false)`
on the next line, so `panning_enabled` is `false` for every pane on every frame
and that scan was being paid to reach a branch that can never be taken.

**`Center::is_detached`.** `map.rs` tested `self.memory.detached().is_some()`.
`detached` clones an `AdjustedPosition` and resolves its position through
`unproject(project(..))`, and the result was dropped unread — the very next line
called `self.position()`, which resolves it again. `Center::is_detached` is
`!matches!(self, Center::MyPosition)` and answers the question without building
anything. Note the old spelling was only wasteful when the map *was* detached:
against `Center::MyPosition`, `adjusted_position` returns `None` and the `map`
never runs.

**The frame's centre, resolved once.** `Map::show` computes `map_center` to draw
the tile layers with, and a dozen lines later handed `Projector::new` the
ingredients to compute the same thing again — another round trip. `Projector`
gains a `pub(crate) with_map_center` that takes the centre; `Projector::new`
keeps its signature and delegates to it. Nothing between the two points touches
`self.memory`.

**Tests.** `map.rs` and `center.rs` held **zero tests between them** before this
commit — the crate's suite is all in `mercator`, `position`, `projector`,
`tiles`, `zoom`, `style` and `expression` and touches none of this code. So
"the existing tests stayed green" would have been evidence of nothing here, and
each claim gets its own:

- `center::tests::is_detached_agrees_with_detached_on_every_variant` — the new
  `mod tests` in `center.rs`. All five variants, built through a helper whose
  companion `variant_name` is an exhaustive `match`, so adding a variant to
  `Center` stops the file compiling rather than quietly leaving the new one
  uncovered. It also asserts the answer is not constant (one `false`, four
  `true`), because agreement on a constant is agreement about nothing.
  **Negative control run:** inverting the `matches!` to
  `matches!(self, Center::MyPosition)` reds it —
  `assertion 'left == right' failed: MyPosition / left: false / right: true` —
  and reds `is_detached_does_not_resolve_the_position` alongside it. Nothing
  else in the crate moves.
- `projector::tests::a_hoisted_map_centre_builds_the_same_projector` — `new` and
  `with_map_center` build bit-identical projectors across four zooms, attached
  and detached, and a deliberately wrong centre must build a different one.
- `map::tests::the_maps_centre_is_the_one_the_projector_would_have_resolved` and
  `a_detached_map_is_not_centred_on_my_position` — `map.rs`'s first tests, pinning
  the premise the hoist rests on.

The hoist is additionally gated outside this crate, which is worth recording
because nothing in `walkers` exercises `Map::show`: handing
`with_map_center` `self.my_position` instead of `map_center` reds five
`squallar-egui` tests (`input_harness::tests::a_click_outside_the_pane_does_not_reach_a_site_icon_straddling_its_edge`,
`a_consumed_click_while_faded_unfades`,
`a_consumed_map_click_reports_itself_and_a_bare_one_does_not`,
`a_dialog_over_a_site_icon_suppresses_its_hover_readout`,
`a_qualifying_tap_fades_the_chrome_and_the_second_restores_it`) out of 1118.

### Changed — source, tenth commit: a drag whose release is never seen

`Center::Moving` had no termination condition. `update_movement`'s `Moving` arm
re-shifts the map by the *stored* `direction` and returns `true`
unconditionally, and `map.rs` turns that `true` into `ui.request_repaint()`.
The only exit was `handle_gestures`' middle arm, which needs
`response.drag_stopped()` — an edge egui offers to one widget on one frame. A
pane hidden mid-drag, a section or tab switch, a pan suppressed mid-drag by
`drag_pan_buttons(empty())`, or a pointer released off-canvas on the web all
lose it, and the map then pans at constant velocity **and repaints at full
rate, forever**. `Center::animating` deliberately excludes `Moving` and
`center_mode` is `pub(crate)`, so a caller could neither see the stuck state
nor clear it.

**Classification split from application.** `Center::classify` reads a
`Response` into a new `pub(crate) enum Gesture`
(`Dragging(Vec2)` / `Released` / `Vanished` / `Idle`); `Center::apply` drives
the state machine from one. `handle_gestures` is now the composition of the
two, so the whole state machine is reachable without an `egui::Response`.

The ordering inside `classify` is load-bearing and is the old `if`/`else if`
chain unchanged: `dragged_by` first, then `drag_stopped`, and only then the
state. `drag_stopped()` is button-agnostic while `dragged_by` is not, so a drag
on a non-panning button that ends still takes the `Released` arm exactly as it
did before. `Gesture::Vanished` is what is left — and it is reachable only from
`Moving`, which is the gate `Gesture::quiet` holds.

**`Vanished` settles to `Exact`, not `Inertia`.** `direction` is the last
`drag_delta` anyone observed and in this case nobody knows how old it is; a
pane hidden for a minute would come back and lurch. `drag_stopped` takes an
explicit `Coast::{Yes,No}` so the two endings are separately pinnable rather
than distinguished by a comment. The `PulledToMyPosition` branch is unchanged
and taken by both endings — it is the accidental-small-drag recovery, not a
coast, and it terminates.

**Observability.** `MapMemory::dragging()` — a pan drag is in progress,
awaiting its release. `MapMemory::settle()` — end any gesture or animation,
leave the map where it is; idempotent. `animating()` is deliberately
**untouched**: it is upstream's documented predicate with an explicitly stated
exclusion, and redefining it would silently change meaning for every reader.

**Tests.** Six in `center.rs`, four of them new here:

- `a_drag_that_is_never_released_stops_moving`. **Negative control, run against
  the unmodified tree** with the state built directly as `Center::Moving`
  (which is what `Gesture::Dragging` produces) and `update_movement` looped:
  `update_movement still demanded a repaint on frame Some(599) of 600; the
  centre has travelled -0.12874603271484375 deg of longitude with no input at
  all`. After the change it stops after 1 frame and does not move again over
  the next 600.
- `a_drag_that_is_released_still_coasts`. **Positive control**, green before
  and after — before the change, written against the private `drag_stopped`:
  `coasted 0.0027626240288967097 deg over 59 frames`. It is what stops the
  test above from being satisfied by deleting inertia.
- `a_vanished_drag_does_not_coast` — pins the `Exact`-not-`Inertia` choice, and
  that `Gesture::quiet` really does answer `Vanished` in that state.
- `settle_clears_every_state` — all five variants through `every_variant()`:
  after `settle()`, `update_movement` is `false`, neither `dragging` nor
  `animating`, `position()` unchanged, and the same again on a second call.
- `dragging_is_true_for_moving_alone` — the predicate is not a constant.

The one thing `walkers` cannot test is `Map::show`, so the whole path is gated
outside this crate by
`squallar_egui::ui::map::tests::a_drag_whose_release_is_never_seen_stops_the_pane`:
press, drag 60 px, turn the pane 3D, release the button while its map is not
drawn, ten frames, turn it back, 120 frames at 1/60. **Measured on the
unmodified tree, same sequence: 632.8 deg of longitude — 7,200 tile-pixels —
of drift, and `repaint_delay` 0 ns.** After the change: zero drift and
`Duration::MAX`.

That test uses the 3D pane as the way to hide the map because the hiding is
total: `Gui::draw_floor_strip` builds its `Map` on an **owned copy**
(`FloorStripCtx::map_memory`, and `floor_frame_for` hands it either
`pane_memory.clone()` or a fresh viewport), so nothing on a `Volume` frame can
see or clear the pane's own `center_mode`.

**Not fixed here, and a different bug.** `InputHarness::cursor_left` is not a
reproduction of this: egui keeps `primary_down` latched across `PointerGone`,
so `dragged()` stays true, `classify` answers `Dragging(Vec2::ZERO)` and the
map holds still while repainting at full rate. That is a latched-pointer defect
above walkers, not a lost release edge.

### Changed — source, eleventh commit: inertia is measured in seconds, not frames

`Center::Inertia` carried `amount`, a **per-frame** shift in points, and decayed
it by `lp_factor = INERTIA_TAU / (delta_time + INERTIA_TAU)` — a factor computed
from a *duration*. The shift ran once per frame while the decay ran per second,
so the two disagreed about what a second was and the total distance a flick
travelled depended on the display's refresh rate. `PulledToMyPosition` had the
same defect in its purest form: `AdjustedPosition::half_offset` halved the
offset once per frame and consulted `delta_time` not at all.

**Measured, on `41bf44c2`, coasting a fixed release velocity at 30/60/120/240 Hz
and reading the total travel off `AdjustedPosition::offset_length`:**

| release velocity | 30 Hz | 60 Hz | 120 Hz | 240 Hz | spread |
| --- | --- | --- | --- | --- | --- |
| 500 pt/s | 116.05 pt | 107.11 | 101.73 | 97.26 | **1.193x** |
| 1000 pt/s | 232.67 pt | 215.38 | 205.90 | 199.28 | **1.168x** |
| 2000 pt/s | 466.05 pt | 432.08 | 414.23 | 403.48 | **1.155x** |

After: **1.0004x, 1.0001x, 1.0000x**.

**16–19%, not the ~2x a per-frame reading suggests.** Comparing `13·amount₀` at
60 Hz with `25·amount₀` at 120 Hz holds *points per frame* fixed, but a real
flick holds *points per second* fixed and hands a 120 Hz display half the
per-frame delta it hands a 60 Hz one. That cancels most of the error — the
surviving `v·(dt + τ)` is the whole of the 16–19%. The `PulledToMyPosition`
defect was the larger one by far: 9 halvings to get a 256-point offset under a
point at **every** rate, so 0.300 s at 30 Hz against 0.0375 s at 240 Hz — an
**8x** spread.

**The fix is exact integration, not a smaller Euler step.** `Inertia` now holds
`velocity: Vec2` in points per second, and each frame shifts by

```text
  ∫₀^dt v·e^(-t/τ) dt = v·τ·(1 - e^(-dt/τ))
```

which is exactly `τ·(vₙ - vₙ₊₁)`, so the sum telescopes to `τ·(v_start - v_end)`
for *any* sequence of steps whatsoever. That is why the residual spread is 1e-4
and not merely small: nothing about the total refers to the frame rate. A plain
`velocity * dt` step would have left 8% at 60 Hz and 58% at 5 Hz.
`PulledToMyPosition` gets the same treatment, with `PULL_TAU = 1/(60·ln 2)`
chosen so a 60 Hz frame still halves the offset exactly as before;
`half_offset` becomes `AdjustedPosition::scale_offset(f64)`.

**The clamp is load-bearing.** `animation_dt` bounds `delta_time` into
`MIN_ANIMATION_DT = 1/1000 ..= MAX_ANIMATION_DT = 1/10`. The lower bound is the
termination condition: egui hands the first frame `stable_dt == 0.0`, which
under the old spelling made `lp_factor` exactly `1.0` and the coast immortal.
The upper bound stops a 250 ms hitch spending the entire remaining coast in one
step; it costs no distance, because the telescoping identity above holds for a
clamped step too. `f32::clamp` propagates a `NaN` rather than bounding it, so a
`NaN` frame time is caught separately and given the shortest step.

**The stop threshold is a position error, and is now stated as one.** The coast
stops when `velocity * INERTIA_TAU` — everything it still has to travel — falls
under `INERTIA_STOP_POINTS = 1.0`. That truncates the trajectory by up to one
point of unspent coast. It is not a saving; it is invisible only because nothing
draws the un-truncated curve beside it. Stating it in remaining *distance* is
what makes the truncation the same size at every rate — the per-frame spelling
it replaced cut 1.2 points at 60 Hz and 4.9 at 240 Hz.

**Feel at 60 Hz.** The old sum was `v·dt/(1 - lp) = v·(dt + τ)` = 0.2167·v,
against 0.2·v now: **7.7% shorter** by derivation, **7.6%** measured
(215.38 → 199.03 points at 1000 pt/s). All of it is the old Euler step
overshooting.

**Frames per flick, measured — and there is no percentage to quote.** At
1000 pt/s, before → after: 30 Hz **39 → 33**, 60 Hz **65 → 65**, 120 Hz
**110 → 129**, 240 Hz **182 → 256**. The count fell at 30 Hz, held at 60 and
*rose* at the high rates, because the coast is now a length of time (1.07–1.10 s
at every rate) rather than a count of frames. Two conflicting figures — 36% and
26% — have been quoted for this; both are unmeasured, and both are quoting one
row of that table under a different stop criterion.

**Plumbing.** `Gesture::Released` becomes `Released(Vec2)` and `Coast::Yes`
becomes `Coast::Yes(Vec2)`, both carrying points per second (the tenth commit's
description of `Gesture` above was accurate when written). The velocity comes
from `ui.input(|i| i.pointer.velocity())`, which is already smoothed and already
in the right units; egui documents it as possibly zero when the frame rate is
bad, so `Center::release_velocity` falls back to the drag's own stored per-frame
`direction` over `animation_dt(delta_time)` — the same quantity measured worse,
and the only source the old coast ever had. That is why `Map::show` now reads
`delta_time` **above** `handle_gestures` rather than below it, and passes it
down in a new `center::InputFrame`.

**Tests.** Nine new in `center.rs`, plus two existing ones re-pointed
(`a_drag_that_is_released_still_coasts` now names a velocity;
`every_variant`'s `Inertia` gains one). The first three are the acceptance
criteria:

- `a_flick_coasts_the_same_distance_at_every_frame_rate` — **(A)**, the gate,
  with the 1.193x/1.168x/1.155x negative control above recorded in its doc.
- `the_coast_travels_the_whole_of_what_the_velocity_is_worth` — the
  non-triviality floor for (A), which a coast that went nowhere would otherwise
  satisfy: travel must be `v·τ` less at most the one point the stop threshold
  can account for.
- `sixty_hertz_still_feels_like_it_did` — **(B)**, with the `v·(dt + τ)`
  derivation written into it and pinned to 0.923 ± 0.01, not merely to the 15%
  band.
- `a_coast_lasts_the_same_time_rather_than_the_same_frames` — **(C)**, which
  asserts the duration is flat and that the frame counts genuinely differ, so it
  cannot be read as a frames-saved claim.
- `a_frame_with_no_elapsed_time_still_ends_the_coast` — `0.0`, negative, `NaN`
  and both infinities all terminate.
- `a_hitch_does_not_teleport_the_coast` — one 250 ms frame spends under 75% of
  the coast, and the remainder still arrives to within a point.
- `the_pull_home_takes_the_same_time_at_every_frame_rate` — the 8x control
  above; compared in frames-worth rather than seconds, because at 30 Hz one
  frame *is* 0.033 s and a tighter tolerance would be measuring the
  quantisation. Also pins the 60 Hz halving directly (256 → 128).
- `a_release_with_no_pointer_velocity_falls_back_to_the_drag` — both arms of
  the fallback.

### Changed — source, twelfth commit: an opt-out from the wheel-zoom frame-time multiplier

`Map::zoom_delta` multiplies `smooth_scroll_delta.y` by `stable_dt` (clamped to
`predicted_dt * 0.5 ..= predicted_dt * 2.0`) before dividing by 4. On an app
whose frame time is a constant that is a smoothing; on this one, whose frames
are 4 ms idle and hundreds of ms while a layer rasterises, it means a wheel
notch zooms a different distance depending on how busy the frame it landed on
was.

**The option is not "remove the multiplier".** The term is
`y * frame_time / 4.0`, so deleting `frame_time` makes one notch `y/2` zoom
levels instead of `y/120` — a **60x** zoom, pinned by
`map::tests::dropping_the_frame_time_entirely_would_be_a_sixty_times_zoom`.
What is wanted is a *nominal* frame time substituted for the measured one, and
that is what `Options::wheel_zoom_scales_with_frame_time` (default `true`,
upstream's behaviour) and `Map::wheel_zoom_scales_with_frame_time` do, against
`NOMINAL_FRAME_TIME = 1.0 / 60.0`.

The arithmetic moves into a free `wheel_zoom_delta(scroll_y, frame_scale)`, so
it is testable without an `egui::Context`; the `if` selects the **value** of
`frame_scale` and both arms then run the same expression.

**Four tests in `map::tests`**: the 60x trap above; that a nominal frame time
holds the notch at 1.0 levels across 240 Hz → a 0.2895 s web frame while the
measured multiplier spreads it by more than 60x over the same frames (the
control, so the first assertion is not comparing against a constant); that the
default is upstream's behaviour on both `Options` and a built `Map`; and that
the gesture stays linear and signed in how far the wheel turned.

**What this deletes above walkers.** `squallar-egui`'s `ui_region.rs` carried
`steady_wheel`, `wheel_rate_correction`, a `Restore` drop guard and a
thread-local re-entrancy flag — ~60 lines that cancelled the multiplier by
**mutating `egui::InputState` around the widget**, at two `input_mut` write
locks per pane per frame. All of it goes; the plan view calls the builder
method at its `Map::new` site instead. `zoom_step`, `POINTS_PER_ZOOM_LEVEL` and
`ZOOM_SPEED` stay — they serve the 3D camera, not walkers.

**The gate did not move and was not weakened.**
`squallar_egui::ui_region::tests::a_notch_moves_the_plan_view_the_same_distance_at_every_frame_rate`
drives the real `walkers::Map` through `InputHarness` at 240/120/60/30/10 Hz
and a 3.5 Hz web p50, asserting `|levels - 1.0| < 0.02` on every row and a
`widest / tightest < 1.02` spread. It is green with `steady_wheel` deleted.
**Non-vacuity control**: with `.wheel_zoom_scales_with_frame_time(false)`
removed and nothing else changed, it fails — `[240Hz 0.4830, 120Hz 0.4934,
60Hz 1.0000, 30Hz 2.0000, 10Hz 2.0000, 3.5Hz 2.0000]`, a **4.14x** spread. So
the option really is carrying what `steady_wheel` carried.

**A related defect, folded in.** `Gui::draw_floor_strip`'s `Map` — the second
`Map::new` site — was never wrapped in `steady_wheel`, so it zoomed at
walkers' rate while the plan view zoomed at the corrected one. It now takes
`.zoom_gesture(false)` rather than the opt-out, because its `MapMemory` is an
**owned copy** (`FloorStripCtx::map_memory`, documented `owned` at its
declaration, moved out of the struct and dropped at the end of the function —
verified, and the same fact the tenth commit's note above relies on). A zoom
gesture there writes to a throwaway, so the strip should be a pure projector.
**UNMEASURED**: the mechanism is in the code and the ownership is confirmed,
but nobody has watched the two panes disagree.

### Changed — source, thirteenth commit: an affine tile rect on `Projector`

Four files: `src/projector.rs`, `src/tiles.rs`, `src/map.rs`, `src/mercator.rs`.

**`Projector::tile_rect(TileId) -> Rect`, new and public.** A tile's corners are
rational fractions of the world bitmap — tile `x` at zoom `z` begins exactly
`x / 2^z` across it — so placing one is two multiplies and an add against the
projected centre the projector already caches. It was being done through
geography instead, in both of this workspace's tile paths.

```rust
let side = self.world_pixels / 2f64.powi(i32::from(tile_id.zoom));
let offset = tile_id.project(side) - self.map_center_projected_position;
Rect::from_min_size(
    (self.clip_rect.center().to_vec2() + offset.to_vec2()).to_pos2(),
    Vec2::splat(side as f32),
)
```

**No `tile_size` parameter, deliberately.** `mercator::tile_id` has already
folded a larger source tile into the zoom, reducing it by
`log2(source_tile_size / 256)`, so a 512 px source arrives as a tile one zoom
shallower and `256 · 2^(map_zoom − tile_zoom)` reproduces its doubled side by
itself. A parameter would double-count it, by exactly 2x, in the one case
nothing else here covers — `map::tests::a_larger_source_tile_arrives_as_a_shallower_zoom_and_a_doubled_side`
is what covers it.

**`flood_fill_tiles` no longer places tiles itself**, and this is a behaviour
change as well as a simplification. It used to offset every tile from
`painter.clip_rect().center()`. `egui::Painter::with_clip_rect` **intersects**
rather than sets (egui 0.35, `painter.rs:73`), so a parent `Ui` narrower than
the map leaves the painter clipped tighter than the widget it belongs to — and
under that narrowing the tiles slid away from the markers and overlays the
plugins drew through the projector, by half the difference of the two centres.
Placement now follows the projector's viewport, culling still follows the
painter's true clip, and `draw_tiles` asserts the containment that actually
holds between them rather than an equality that does not.
`map::tests::a_narrowed_parent_clip_culls_more_tiles_without_moving_them` pins
it.

`Map::show` builds its projector from the allocated `rect` rather than
`response.rect` so the tiles and the plugins place against the same viewport.
`allocate_exact_size` returns `align_size_within_rect(desired, response.rect)`
for a `desired` that is that rect's own size, so the two are the same rect;
that is egui's arithmetic and not ours, so it is a `debug_assert_eq!` rather
than an assumption.

**A `u8` underflow fixed on the way through.** `mercator::tile_id` reduced the
zoom for a large source tile with a plain `-=`, so a 512 px source with the map
zoomed all the way out evaluated `0u8 - 1` — a debug-build panic, reachable
from `draw_tiles`, found by the test above rather than by reading. It is
`saturating_sub` now, and zoom 0 is the right answer there: the source's own
zoom-0 tile is the whole world, which is what the one tile of the zoom-0 grid
is. `mercator::tests::a_large_source_tile_at_the_top_of_the_grid_does_not_underflow`,
with a control at zoom 4 showing the reduction still bites away from the edge.

`tiles::rect` is deleted with its only caller.

**This path has no runtime coverage in this workspace.** Both `Map::new` sites
in `squallar-egui::ui_map` pass `None` for tiles and neither adds a layer, so
nothing in the application reaches `draw_tiles` — a green board says nothing
about it. The three new `map.rs` tests are the only things that execute it, and
they are the first to do so: they drive a real `Map::show` with a tile source
that answers every id, and read the meshes back off `Context::end_pass`.

**Evidence that the arithmetic is the geographic answer.** The consumer that is
actually reachable is `squallar-egui`'s own `draw_tile_layer`, and
`squallar_egui::tiles::tests::the_affine_tile_rect_agrees_with_the_geographic_round_trip`
measures `tile_rect` against the `tile_to_lat`/`tile_to_lon` →
`geo_corner_rect` round trip it replaces: **1,242,989 tiles over 9,344
viewports, worst corner disagreement 0.000244 points** — one `f32` ulp, against
a tile that is 181 points across at its smallest.

**Two negative controls, both red.** Flipping the exponent's sign fails by
1.0e9 points; using 512 as the tile-size base fails by 6.7e7. And a third,
which is why the sweep is shaped as it is: with the sign flipped **and** the
sweep restricted to whole zooms at `tile_zoom == round(zoom)`, 29,106 tiles
over 896 viewports pass at a worst disagreement of **exactly 0**. A parity test
that never leaves that row cannot fail for the mistake this arithmetic invites.

### Changed — source, fourteenth commit: only the first operand of a comparison is a property key

This one **corrects the fifth commit above**, which got `!=` half right and
half wrong. Read item 3 of that section first; the correction note under it
points here.

In MapLibre's legacy filter syntax only the **first** operand of a comparison
names a property. Everything after it is the value being compared against.
`property_or_expression` implements the first half of that — a `String` that
happens to be a key in `properties` resolves to that property's value — and it
is correct on the left operand and wrong on every other one.

`==` resolved **both** operands through it (upstream's own code), and the fifth
commit made `!=` match, so both did. The consequence is silent: a filter whose
*value* collides with a property key on the same feature stops comparing a
property to a value and starts comparing two properties to each other.

**Measured blast radius in this workspace's own styles.** OpenMapTiles'
`transportation` layer carries a `service` field, and twelve committed layers
filter on `["==", "class", "service"]` — `{tunnel,road,bridge}_service_{case,fill}`
across `www/styles/dark.json` and `www/styles/light.json`. On a driveway
(`class = "service"`, `service = "driveway"`) the evaluator compared
`"service" == "driveway"` and returned false, so **all twelve drew nothing**,
with no error and no fallback.

`service` is the **only** such collision: of the 214 `==`/`!=` filters in the
two styles with a bare-string right operand, it is the one whose value matches
an OpenMapTiles field name. So this change alters the outcome of exactly those
twelve layers and nothing else those styles express.

**The fix is not "stop resolving the right operand at all."** That was
upstream's original `!=` and it is what the fifth commit correctly objected
to: an *array* on the right is an expression and must be evaluated, or
`["!=", "class", ["get", "x"]]` compares a resolved property against the
literal JSON array `["get", "x"]` and is unconditionally true. Both halves are
needed, and they are different functions:

| operand | resolver | a bare `String` | an `Array` |
| --- | --- | --- | --- |
| left | `property_or_expression` | property key | expression |
| right | `evaluate` | string literal | expression |

That is also exactly what the `in` arm already did with its list items, so
`==`/`!=` now read the same way `in` does.

Pinned by `expression::tests::test_not_eq_resolves_only_its_left_operand`,
which replaces the fifth commit's `test_not_eq_operator_evaluates_both_sides`
— that test asserted the wrong semantics, so it was inverted rather than kept
— and by
`expression::tests::test_eq_filter_when_the_value_collides_with_a_property_key`,
which drives a `Filter` over the real `class`/`service` shape. Both were
written first and run against the unfixed evaluator. The line numbers below are
that pre-fix file's and are not where the tests sit now; the assertion text is
the part to match on:

```text
---- expression::tests::test_eq_filter_when_the_value_collides_with_a_property_key stdout ----
thread '…' panicked at vendor/walkers/src/expression.rs:881:9:
assertion failed: filter.matches(&driveway_context)

---- expression::tests::test_not_eq_resolves_only_its_left_operand stdout ----
thread '…' panicked at vendor/walkers/src/expression.rs:836:9:
assertion `left == right` failed
  left: Bool(true)
 right: Bool(false)
```

The new test carries a comment naming `db99c617` and saying why the obvious
symmetry with the right-hand side is wrong, because that symmetry is what
produced the defect the first time.

**The 22 upstream expression tests did not move.** That is load-bearing here:
`test_eq_operator` asserts `["==", "Polska", ["get", "name"]]` is true, which
is only reachable if the right operand is evaluated as an expression. Upstream's
own pin is what rules out the simpler "leave the right operand raw" fix, and it
is why the table above has two different resolvers rather than one.

**Audited, not fixed — the four ordered comparisons never resolve their right
operand at all.** `<`, `>`, `<=` and `>=` pass the raw `&Value` to `lt`/`lte`,
whose `_ => false` arm swallows the type mismatch. So
`[">=", ["get", "a"], ["get", "b"]]` is silently false. This is the *same*
defect the fifth commit found in `!=`, still present in four arms; it is the
opposite shape from the one fixed here (under-resolving, not over-resolving),
it changes behaviour for array right-hand sides, and it wants its own test, so
it is left for its own commit. Neither committed style has an array on the
right of an ordered comparison (checked: 0 of 318), so nothing draws wrong
today. **Fixed by the fifteenth commit below.**

**Audited, clean.** `has`, `!has` and `get` take their argument as a key via
`single_string` and never resolve it, which is right. `in` resolves its first
operand and `evaluate`s the list items, which is right. `any`, `all`, `case`,
`coalesce`, `!`, `match`, `format` and `interpolate` evaluate sub-expressions
and touch no property keys. After this commit `property_or_expression` is
called in exactly seven places and every one of them is a first-or-left
operand.

**Also audited, and absent rather than wrong:** the legacy `none` and `!in`
operators are not implemented at all and fall to
`Error::InvalidExpression`. Neither appears in either committed style, so
this is latent; adding an operator is not the shape of this commit.
**Implemented by the sixteenth commit below.**

### Changed — source, fifteenth commit: the ordered comparisons resolve their right operand

The other half of the fourteenth commit, and the **opposite** shape of defect.
That one over-resolved the right operand of `==`/`!=`; these four
under-resolve it and never resolve it at all.

`<`, `>`, `<=` and `>=` resolved their left operand through
`property_or_expression` and then handed `lt`/`lte` the **raw `&Value`** for
the right. `lt`/`lte` match on `(Number, Number)` and `(String, String)` and
have a `_ => false` arm, so a right operand that is a JSON array — i.e. an
expression nobody evaluated — does not error. It compares unequal and the whole
filter is **silently false**, whatever the operands actually say.
`[">=", ["get", "a"], ["get", "b"]]` never draws.

The fix is one line per arm: the right operand goes through `evaluate`, the
same resolver `==`, `!=` and `in`'s list items already use. The two-resolver
table from the fourteenth commit now covers all six comparisons rather than
two.

**This changes behaviour for array right operands and nothing else.**
`evaluate` returns `primitive.clone()` for anything that is not an array, so
for a number, a string, a bool or null it is the identity and the comparison is
bit-for-bit what it was. The one further consequence is that a right operand
which is an array but *not* a valid expression now returns
`Error::InvalidExpression` instead of comparing false — which is what `==`,
`!=` and `in` already do with the same input, so the six arms agree rather than
four of them failing quietly.

**Measured blast radius in this workspace's own styles — zero.** Counted
directly over `www/styles/dark.json` and `www/styles/light.json`, walking every
nested array:

| | dark | light | both |
| --- | --- | --- | --- |
| ordered comparisons (`<`, `>`, `<=`, `>=`) | 159 | 159 | **318** |
| …with an **array** right operand | 0 | 0 | **0** |
| …with a bare-string right operand | 0 | 0 | **0** |
| …with an array *left* operand (e.g. `[">=", ["zoom"], 9]`) | 148 | 148 | 296 |

Every one of the 318 is three elements long, and every right operand is a
number. So this commit changes the outcome of **no layer either committed style
expresses**; it is a latent defect fixed before it was reachable, not a
rendering change. The 296 array *left* operands are the reason the left side
was resolved and the right side's omission went unnoticed.

Pinned by `expression::tests::test_ordered_comparisons_resolve_their_right_operand`,
written first and run against the unfixed evaluator:

```text
---- expression::tests::test_ordered_comparisons_resolve_their_right_operand stdout ----
thread '…' panicked at vendor/walkers/src/expression.rs:930:9:
assertion `left == right` failed
  left: Bool(false)
 right: Bool(true)
```

The test has two blocks and **only the first is that negative control.** Each
of the four arms gets an array-right-operand assertion that the unfixed code
cannot satisfy, plus the opposite direction so that "resolve the right operand"
cannot be mistaken for "make every ordered comparison true". The second block
asserts that a bare string on the right stays a **literal** — `class` is
`"minor"` and a `minor` property holds `"trunk"`, so resolving the right
operand as a property key would invert both assertions. That block passes
before *and* after this commit, by construction: it is the regression guard
against reintroducing the fourteenth commit's defect in four more arms, not
evidence of this one.

**The 22 upstream expression tests did not move.** None of them exercises the
`<`, `>`, `<=` or `>=` arms at all — the only upstream reads of `lt`/`lte` are
through `interpolate`, which calls them directly on already-evaluated stops and
is untouched here.

Also corrected: the comment above the `==` arm pointed at "VENDORED.md,
thirteenth commit" for the ordered-comparison defect. The thirteenth commit is
the affine tile rect on `Projector`; the comparison work is the fourteenth.

### Changed — source, sixteenth commit: the legacy `none` and `!in` operators

Both fell through to `Error::InvalidExpression`. `style::Filter::matches`
turns an `Err` into `false`, so a layer filtered on either drew **nothing** —
and these two are the worst pair for that failure mode, because both are
*negative* filters, true of most features. Failing them closed suppresses a
layer broadly rather than narrowly, with one `warn!` nobody reads.

**Why implement rather than document the gap.** The obvious argument against
is that neither appears in either committed style, so this is an unused spec
corner. That argument does not survive looking at *why* they are absent:

- `tools/basemap-style` **already rewrote them away**, in `rewrite_filter` —
  `["!in", k, …]` became `["!", ["in", k, …]]` and `["none", …]` became
  `["!", ["any", …]]`. Its doc comment and `DECISIONS.md` § "Legacy filter
  operators" both named this evaluator's fallback arm as the reason. They are
  absent from the committed styles by construction, not by luck.
- And they were **present in the real input**: `DECISIONS.md` recorded "one
  `!in` per theme; no `none`". So `!in` is a live construct in the upstream
  CARTO styles that something has to handle. The only question is where.
- Handling it in the converter puts a correctness requirement of the evaluator
  in a **different workspace**. `tools/basemap-style` was its own workspace
  root, so root `cargo test --workspace` walked straight past its manifest, and
  **nothing in CI ran the converter** — the workflow that briefly did was
  deleted as a recurring gate on a one-time job. (For one day, 2026-08-28, the
  root workspace did compile that crate's `src/lib.rs`, via a `#[path]` include
  in `squallar-egui/tests/committed_styles_parse.rs`. The crate was deleted the
  same day: `convert` and its four tests went to git history, and the checker
  half moved to `squallar-egui/tests/style_gate/mod.rs`.) Any style that reaches
  `Context::evaluate` without going through that converter — a hand-written
  one, a future source, a style fetched rather than generated — loses a whole
  layer silently.
- The cost of closing it is four lines of dispatch and two extracted helpers,
  and the semantics are not a judgement call: the legacy filter spec defines
  `none` as the negation of `any` and `!in` as the negation of `in`, both of
  which were already implemented and already tested.

So the gap is closed here, at the layer the symptom lives in. `in` and `any`
move into `is_in` and `any_of` so that `!in` and `none` are literally the
negation of the same code and cannot drift from it; neither existing arm
changes behaviour.

**The converter's rewrite was deliberately left in place**, in another crate
and another workspace; it went with that crate on 2026-08-28. Nothing here
depended on it either way, and the committed styles still carry the modern
spelling, which `no_legacy_stops_tokens_or_not_in_filters_survive` holds them
to.

Pinned by `expression::tests::test_none_and_not_in_operators`, written first
and run against the unfixed evaluator:

```text
---- expression::tests::test_none_and_not_in_operators stdout ----
thread '…' panicked at vendor/walkers/src/expression.rs:1009:18:
called `Result::unwrap()` on an `Err` value: InvalidExpression(Array
[String("none"), Array [String("=="), String("class"), String("path")],
Array [String("=="), String("rank"), Number(9)]])
```

Each operator is asserted against the one it negates, on identical arguments
and in both directions, so the pair pins the *complement* rather than a value
that a constant would also satisfy. The `any` and `in` halves of those pairs
pass before and after — they are the reference, not the control. The test ends
by driving a `Filter` over each, because "the evaluator returns a bool" is one
layer below the symptom, which is a layer that does not draw.

Inherited and not changed: `property_or_expression` returns the key string
itself when the named property is missing, so `["!in", "absent", "x"]` compares
`"absent"` against `"x"`. That is pre-existing behaviour shared with `==` and
`in`, and changing it belongs to its own commit with its own measurement.

### Changed — source, seventeenth commit: the property bag stops being rebuilt per feature

`get_layer_features` built a whole `HashMap<String, serde_json::Value>` for
every feature it touched, by `mvt_properties_to_json_properties`, purely so
that `expression::Context` had something to hold. That is one `HashMap`
allocation plus one `String` clone per string-valued property, per feature,
**per style layer that names the source layer** — paid before the filter has
decided whether the feature draws at all, and for a bag that most expressions
read one key out of or none.

`Context` now holds an `expression::Properties`, which is either the `Json`
map a caller supplied or the `Mvt` map exactly as `mvt-reader` produced it,
**moved** into the context rather than converted into a new one.
`mvt_value_to_json_value` runs on the way *out* of a lookup instead of on the
way in, and is `pub(crate)` for that.

**The public shape does not change.** `Properties` and
`Context::with_properties` are both `pub(crate)`; `mod expression` is private
and `lib.rs` re-exports only `Context`. `Context::new` keeps its exact
signature — `(String, HashMap<String, serde_json::Value>, u8)` — which is what
`squallar-egui/tests/committed_styles_parse.rs` calls and what the 22 inline
expression tests call. None of them were edited. (That caller was
`tools/basemap-style/tests/converted_styles.rs` until 2026-08-28, when the
committed-style gate moved into the workspace and that crate was deleted; the
call site is unchanged.)

#### A second cost, found on the way, and larger than the first

```rust
Value::String(key) if self.properties.contains_key(key) => Ok(self
    .properties
    .get(key)
    .ok_or(Error::PropertyMissing(key.clone(), self.properties.clone()))?
    .clone()),
```

`ok_or` takes its argument **by value**, so that `Error::PropertyMissing` was
constructed on every *successful* property read — a `key.clone()` and a deep
clone of the entire property map — to serve an error the `contains_key` guard
immediately above it had already made unreachable. It is now one `get` whose
`None` falls through to the literal arm, exactly as the failed guard used to.
`Error::PropertyMissing` had no other constructor and is removed with it.

#### Measured, not reasoned

A counting `GlobalAlloc` behind a thread-local counter, wrapped around one
`render` of `mvt::tests::fixture` after three warm-up rounds. The instrument
is **not committed** — the figures are:

| tree | allocations per `render` |
| --- | --- |
| before | 489 |
| the `ok_or` fix alone | 431 |
| both (this commit) | 391 |

Deterministic to the unit across repeated runs. **98 allocations removed,
20.0% of the round** — 58 from the eagerly-constructed error, 40 from the
per-feature map.

The denominator is one `render` of that fixture: 7 features in 3 source
layers, visited **18 times** between them, because the style's 8 layers name
`roads` four times, `landuse` twice and `places` once. The 40 is exactly what
the source predicts for those 18 visits — one `HashMap` for each of the 14
visits to a feature that has properties, plus 26 `String` clones for the
string-valued ones. The four visits to the property-less `roads/r3` cost
nothing, which confirms that collecting an empty iterator into a `HashMap`
does not allocate.

This path is not on the app's frame path today — no vector tile source is
wired — so the figure is a property of `render`, not a frame-time claim.

#### Not done here

`Context` still takes `geometry_type: String` and `get_layer_features` still
spells it `geometry_type_to_str(…).to_string()`, so 18 more `String`s are
allocated per render of this fixture from a function that returns
`&'static str`. Narrowing that field changes `Context::new`'s public
signature and so reaches its callers — which since 2026-08-28 are all inside
this workspace, `squallar-egui/tests/committed_styles_parse.rs` among them, so
the change is at least gated where it lands. It belongs to its own commit.

#### Evidence

`mvt::tests` is new in the commit before this one and passes **unedited**
across the change — it was written and landed against the tree as it stood,
which is what makes it a before/after and not a rationalisation. It encodes a
vector tile by hand (`mvt-reader` only decodes, its protobuf module is
private, and no `.pbf` is checked in anywhere in this workspace) and compares
the `Debug` rendering of all twelve resulting shapes, every mesh vertex,
stroke and label, against the recording taken from the old code.

Two tampers, each checked to have actually applied rather than silently
matching nothing:

- Lazy conversion broken — `Properties::Mvt::get` returns `Null` for a key it
  holds. 12 shapes become 3.

  ```text
  ---- mvt::tests::rendering_the_fixture_reproduces_the_recorded_shapes_exactly stdout ----
  thread '…' panicked at vendor/walkers/src/mvt.rs:899:9:
  assertion `left == right` failed: the fixture draws a background, two fills,
  four strokes and five labels
    left: 3
   right: 12
  ```

- The fourteenth commit's defect reintroduced — `==` resolves its right
  operand through `property_or_expression`. Four tests red: both new ones and
  the two upstream pins, `test_eq_filter_when_the_value_collides_with_a_property_key`
  and `test_not_eq_resolves_only_its_left_operand`. The fixture loses
  **exactly one** shape, `roads/r1`'s stroke, which is what it carries
  `kind = "primary"` alongside `primary = "yes"` to catch:

  ```text
  ---- mvt::tests::rendering_the_fixture_reproduces_the_recorded_shapes_exactly stdout ----
  thread '…' panicked at vendor/walkers/src/mvt.rs:899:9:
  assertion `left == right` failed: the fixture draws a background, two fills,
  four strokes and five labels
    left: 11
   right: 12
  ```

The `--features mvt` test count goes 85 -> 87, against **48** without it.
(85 is measured on `ec75a18d`; the figure of 83 circulating today is stale.)
`--features mvt` is not optional when running this crate's tests: without it `expression`,
`mvt`, `style` and `text` do not compile in, and a filter naming any of them
selects zero tests and still exits `0`.

### Changed — source, eighteenth commit: `line-width` arrives at the width the style asked for

`src/mvt.rs`, `render_line`. One factor, `4.0` -> `16.0`, now the named constant
`LINE_WIDTH_TO_EXTENT`.

A style's `line-width` is in screen points. `render` emits shapes in MVT extent
units, and `transformed` later scales the whole tile by
`rect.width() / ONLY_SUPPORTED_EXTENT`. So the pre-multiplier that makes a
styled width land on screen at that width is `ONLY_SUPPORTED_EXTENT /
rect.width()`, where `rect.width()` is the side the consumer draws a tile at.

| consumer | tile side, points | correct factor | upstream's 4.0 draws |
| --- | ---: | ---: | --- |
| squallar (`TILE_SIDE_POINTS`, `squallar-egui/src/tiles.rs`) | 256 | **16** | a quarter width |
| `TileSource::tile_size` default (`src/sources/mod.rs`) | 256 | 16 | a quarter width |
| `OpenFreeMap` (`src/sources/openfreemap.rs`) | 512 | 8 | half width |

**`4.0` is `4096/1024`, and this crate ships no 1024-point source** — so the
constant matched nothing upstream either. It is a defect, not a preference we
are overriding.

The `else` branch — `2.0` when a layer sets no `line-width` — is **left as
upstream wrote it**, and it is inconsistent with the line above it: MapLibre's
default is 1, so it should be `1.0 * LINE_WIDTH_TO_EXTENT`. Nothing in this
workspace reaches it. All 56 `line` layers of both committed styles
(`www/styles/{dark,light}.json`) set `line-width`, counted 2026-08-28, and all
56 set it to the scalar `8`. Changing a branch no committed style exercises
would be an unverifiable edit inside a vendored file.

`mvt::tests::rendering_the_fixture_reproduces_the_recorded_shapes_exactly` moves
with the factor, and this is the one class of golden edit that is legitimate:
the pin is on *what expressions evaluate to*, and the factor is downstream of
the evaluation. Three of its four stroke widths scale by 4 — `16.0 -> 64.0` and
`8.0 -> 32.0` twice. The fourth, `2.0`, does **not** move, which is what shows
the `else` branch was left alone.

**Superseded by the nineteenth commit below**, which removes the pre-multiplier
rather than correcting it. The entry is kept because the reasoning above is
still how the number got to 16, and because the table's third row — 8 for
`OpenFreeMap`'s 512-point tiles — is the evidence that no single constant could
have been right.

### Changed — source, nineteenth commit: `line-width` stops being a constant that is right at one tile side

`src/mvt.rs`. `LINE_WIDTH_TO_EXTENT` is **deleted**; `render_line` writes the
styled width through unchanged, and the placement no longer scales stroke
widths.

The eighteenth commit above fixed the constant and left the shape of the defect
in place: `transformed` scales by `rect.width() / ONLY_SUPPORTED_EXTENT`, so
*any* pre-multiplier baked at render time is correct at exactly one
`rect.width()`. Measured against the committed styles, whose 56 `line` layers
all ask for width `8`:

| tile drawn at | 16.0 delivered | now |
| --- | ---: | ---: |
| 128 pt (`tile_zoom_bias = 1`, a 3D pane's floor strip) | 4.00 | 8.00 |
| 181 pt (the half step) | 5.66 | 8.00 |
| 256 pt (whole zoom, bias 0) | 8.00 | 8.00 |
| 362 pt (the other half step) | 11.31 | 8.00 |
| 512 pt | 16.00 | 8.00 |

The rule, and it is MapLibre's: `line-width` and `text-size` are in **screen
points**, the geometry beside them is in MVT extent units, and a style's own
zoom stops are what scale a road with the map. `Text` already worked this way
upstream — `ShapeOrText::transform` scaled `position` and left `font_size`
alone — so the change makes strokes agree with labels rather than inventing a
convention.

`ShapeOrText::transform(&mut self)` is replaced by
`ShapeOrText::placed(&self, TSTransform) -> ShapeOrText`, and `mvt::placement`
is added so a consumer can place shape by shape. Three reasons, one of them not
about line width at all:

1. Only `placed` can leave a stroke width alone; `Shape::transform` scales it.
2. The in-place spelling forced the caller to own a copy first, and for a
   `Shape::Mesh` — an `Arc<Mesh>` the tile cache is still referencing — the
   mutation then went through `Arc::make_mut` and copied the mesh **again**.
   Measured on `squallar-egui/testdata/monaco.pmtiles`' z14 city tile, release
   build: `transformed` 135.2 us before, 24.8 us after.
3. `transformed` is kept, in terms of `placed`, so `Tile::draw` and the inline
   tests are unchanged.

`mvt::tests::rendering_the_fixture_reproduces_the_recorded_shapes_exactly` moves
again, in the same legitimate class: three of its four stroke widths lose the
factor of 16 — `64.0 -> 4.0` and `32.0 -> 2.0` twice — and the fourth, `2.0`,
does not move because it is the untouched `else` branch. That branch's *unit*
moved with the change: it is now 2 screen points rather than 2 extent units.
Nothing in this workspace reaches it, for the reason recorded above.

`a_styled_line_width_survives_every_tile_side` is added. It exists because the
blind spot was uniform: every pre-existing test placed on a 256-point rect or
did not place at all, and 256 is exactly the side at which `4096/256` is right.

### Changed — source, nineteenth commit: a tile's fills are one mesh, and the tessellator's slack is given back

`src/mvt.rs`, `tessellate_polygon` and a new `coalesce_adjacent_meshes` pass at
the end of `render`. Both are about what a *cached* tile costs, and both were
found by measuring the committed Monaco fixture's z14 city tile.

`VertexBuffers::new()` is `with_capacity(512, 1024)`
(`lyon_tessellation-1.0.20/src/geometry_builder.rs:269`), so every polygon's
mesh carried 10,240 + 4,096 bytes of allocation whatever it needed. The tile has
2,257 of them holding eight vertices each: **1,155,584 vertex slots and
2,311,168 index slots for 18,018 and 40,812** — 32.35 MB of capacity for 0.52 MB
of content. `shrink_to_fit` on both, after tessellation, because the output count
is not a function of any input the caller has.

`coalesce_adjacent_meshes` folds each **run** of neighbouring `Shape::Mesh`es
into one with `Mesh::append_ref`. Order-preserving is the whole safety argument:
only adjacent meshes fold, so a line layer between two fill layers breaks the
run and every shape still draws where the style put it. Every mesh carries
`TextureId::default()`, which is what makes them appendable; colour is
per-vertex, so meshes of different colours fold correctly.

| | before | after |
| --- | ---: | ---: |
| shapes | 2,993 | 738 |
| meshes | 2,257 | 2 |
| resident heap | 32,767,648 B | 646,264 B |
| consumer's per-frame placement | 135.2 us | 24.8 us |
| `Tile::from_mvt` | 23.72 ms | 24.80 ms |

The last row is the cost: coalescing and shrinking add ~1.1 ms (4.4%) to
tessellation, which runs off the frame thread, to take 50.7x off what the cache
holds and 5.5x off what the frame thread pays.

`rendering_the_fixture_reproduces_the_recorded_shapes_exactly` moves for this
too — 12 shapes to 11, its two adjacent fills becoming one mesh with rebased
indices — and the non-triviality floor beside it moves with it.

### Changed — source, nineteenth commit: `Tile::Vector` is shared, not owned

`src/tiles.rs`. `Vector(Vec<ShapeOrText>)` becomes
`Vector(std::sync::Arc<Vec<ShapeOrText>>)`.

`Tiles::at` hands back a `TilePiece` **by value**, once per visible grid cell
per frame. The raster arm costs nothing to hand over because a `TextureHandle`
is a refcount; the vector arm deep-copied every shape in the tile. Measured on
the same z14 tile: **22.9 us per tile per frame, against a viewport that holds
up to 84 tiles**. An `Arc` clone measures 0.01 us.

### Changed — source, twentieth commit: labels wrap, rivers stop repeating, and the halo is real

Four files: `src/mvt.rs`, `src/style.rs`, `src/text.rs`, `src/tiles.rs`. All
four are already on the changed list above. **`src/mvt.rs` is not**, and its
absence there is a pre-existing gap in that list rather than something this
commit introduces: the seventeenth, eighteenth and nineteenth commits all
changed it. The `diff -rq` block above is from the seventh commit and has not
been re-run since; treat it as that commit's snapshot. What is true now is that
`src/mvt.rs` differs too.

Four defects, reported from a live map.

**1. A warn per style layer per tile, for ordinary data.**
`get_layer_features` warned "Source layer '…' not found. Skipping." whenever a
tile did not carry a source layer the style names. A style names 94 source
layers and no tile carries all of them, so this fired constantly and said
nothing anyone could act on — this workspace's standing rule is that a notice
the reader cannot act on is a defect in the code. **Measured over a 45-tile
Oklahoma viewport at zooms 6, 7 and 8: 903 warn lines, which was 100% of the
warn output `mvt::render` produced; `transportation` alone 245. After: 0.**

It is `trace!` and not deleted, because "why is this one layer not drawing on
this one tile" is a real question with no other instrument. The case that is
genuinely broken — a style naming a source layer no tile anywhere carries — is
caught before shipping by `squallar-egui/tests/committed_styles_parse.rs`, which
fails the build.

**2. `text-max-width` was implemented by nothing.** No wrapping code existed
anywhere in the crate. The committed styles set the property on 16 layers and
every one was ignored, so a 77-character tribal-nation name drew as a single
run **414.7 points wide**, across the user's whole viewport. `Layout` gains
`text_max_width` and `text_line_height`; `Text` gains `max_width_ems` and
`line_height_ems`, and the ems become points in `Text::galley`, which is the
first place the font is known. At the `text-max-width: 8` its layer asks for,
that name is 91.8 x 70 points.

The default when a style is silent is MapLibre's **10 ems**, applied in
`symbol_wrapping` rather than left to the text layer: "the style said nothing"
and "the style said do not wrap" are different instructions.

**Wrapping is a point-placement rule, and getting that wrong was caught before
it shipped.** MapLibre shapes a symbol with
`placement === 'point' ? text-max-width * ONE_EM : 0`, so a line-placed label is
never wrapped — it is laid out along the line and has no column to break into.
The first version of this change applied the 10-em default to both arms, which
would have stacked `waterway_label`'s "North Canadian River" into short rows
lying across the river instead of along it: a *worse* result than the
unwrapped run it replaced, on the one layer whose placement was reported as
already legible. `mvt::tests::a_line_label_is_never_wrapped` pins the
distinction, with a point label under the same absurd 2-em cap as the control,
and the golden recording carries both values for the same reason.

**3. A river was named once per OSM way.** `render_symbol` emitted one label per
`LineString`, anchored at the midpoint of whichever single *segment* was
longest, at that segment's slope.

*A correction to how this was first described to me, worth recording because it
changes what the fix is for:* the old anchor was **not** off the line. The
midpoint of a straight segment lies on that segment. The real defect is that
"the longest segment" is decided by how the tile was generalised, so it moves
between zooms — which is the reported symptom of labels that "jump around and
rotate differently at different zoom levels", and it is a *stability* defect
rather than a *correctness* one. `anchor_along` replaces it with the point half
way along by arc length and the tangent over a window centred there.
`mvt::tests::simplifying_a_river_barely_moves_its_label` is the pin, with the
rule it replaced spelled out beside it as the control.

Deduplication is **not** here, because a river crosses tiles and this crate
renders one. It is in the consumer's label phase, keyed on name and a minimum
screen distance, so a river spanning the pane is still readable at both ends —
which is what MapLibre's `symbol-spacing` buys. **Measured on the same
viewport, z8: "Beaver River" 6 labels to 2, "Cimarron River" 4 to 2, "Washita
River" 4 to 2, "Verdigris River" and "North Canadian River" 3 each to 1. At z6
and z7 every river repeat is gone.**

**4. The halo was a background rectangle.** `Text::background_color` carried the
style's `text-halo-color` at half alpha and the consumer put it in
`egui::TextFormat::background`, which fills the galley's bounding box — a
translucent slab behind the label, not an outline around the letters. The field
is now `halo_color`, `Paint` gains `text_halo_width`, and `Text::shape` draws
the glyphs eight times on a circle beneath themselves.

`text-halo-width` is read rather than assumed because it is not constant: 28
symbol layers in each committed style ask for `1` and `watername_ocean` asks
for `0`. MapLibre's default is 0, so a style naming a halo colour and no width
gets no halo, here as there.

**The `MultiPoint` arm never had a halo at all** — it passed
`Color32::TRANSPARENT` unconditionally, so every place name on the map drew
unhaloed however loudly the style asked. Both arms read the same three paint
properties now, through `symbol_colors`.

**What it costs the frame, measured rather than waved at.** Eight offsets is
nine draws where there was one. Tessellating 96 labels — what a z8 pane
actually puts on glass — release build, best of 20: **10.5 us without the halo,
94.0 us with it**. So +83.5 us per pane per frame, 0.5% of a 60 Hz budget. All
nine draws share one `Arc<Galley>`, so the text is shaped once and only the
tessellation repeats. An SDF or a blurred mask is what MapLibre does and would
be cheaper as well as better; it needs a glyph atlas this crate does not build.

**`Text::galley` and `Text::shape` are new public methods, and they exist to
stop a second copy of this logic appearing.** `src/tiles.rs`'s own `draw_text`
and the consumer's label phase both lay a label out; putting the wrapping and
the halo on `Text` means the two cannot drift. `draw_text` shrank to four lines
as a result.

One consequence a caller must know: `galley` lays out with
`halign: Align::Center`, so rows are centred on the anchor as MapLibre's default
`text-justify` has them, and the galley is measured about its own centre line —
`galley.rect.min.x` is negative. `Text::shape` takes the block's top-left corner
and undoes that. A caller that passes a galley origin straight to `TextShape`
will draw half a label-width to the left.

**Tests: seven new in `mvt::tests`, and `rendering_the_fixture_…` re-recorded
once.** Every difference in that recording was attributed before it was
accepted, and they are listed in the constant's own doc: the rename, two new
fields at MapLibre's defaults, `line_height_ems: None`, and the two `Back Alley`
halo colours losing the `gamma_multiply(0.5)` that softened the fake box. **No
label position moved** — the fixture's roads are straight, and the arc-length
anchor agrees with the old chord midpoint on straight geometry, which is what
makes that recording a pin on `anchor_along` being a no-op where it should be.

**Negative control, run.** Reverting `anchor_along` to the longest-segment rule
and leaving everything else alone turns four of the new tests red
(`a_line_label_sits_half_way_along_the_line`,
`a_line_label_takes_the_tangent_at_its_own_anchor`,
`simplifying_a_river_barely_moves_its_label`, `a_degenerate_line_has_no_anchor`)
plus the golden, and nothing else in the crate.
`a_westward_line_label_is_still_upright` stays green under both, and is labelled
a guard rather than a control for that reason: `slope().atan()` was upright too,
and the test is there to keep the `atan2` fold from losing the property.

**Test count: `cargo test -p walkers --features mvt` reports 95**, from 88
before. A spelling gotcha worth carrying: plain `cargo test -p walkers` selects
**48** and cannot see any of this — `mvt::tests` is gated, and the golden test
is in it.

### Changed — source, twenty-first commit: the parse is split from the styling

Two files: `src/mvt.rs` and `src/expression.rs`. The consumer motive lives in
`squallar-egui`: `HttpsTiles` caches the parsed tile per `TileId`, so a theme
flip or a map-detail toggle re-styles from cache with zero fetches and zero
re-parses, where it used to rebuild the source and refetch the viewport.

`render(bytes, style, zoom)` used to decode and style in one pass — and
re-decode per style layer: `get_layer_features` called
`reader.get_features(layer_index)` once **per style layer**, so the committed
styles' 95 layers over 16 source layers decoded the same protobuf features
several times per tile. It is now exactly `styled(&parse(bytes)?, style, zoom)`:

- **`parse(bytes) -> ParsedTile`** — the zoom- and style-independent half:
  every source layer's features, geometry in extent units, property bags as
  mvt-reader hands them over, each behind an `Arc`. Each source layer decodes
  **once**. `ParsedTile::heap_bytes()` reports resident cost at capacity, for
  consumers sizing a cache.
- **`styled(&ParsedTile, &Style, zoom) -> Vec<ShapeOrText>`** — filter
  evaluation, paint/layout expressions, tessellation. Infallible: everything
  fallible happened in `parse`.
- `Properties::Mvt` (`src/expression.rs`) holds `Arc<HashMap<..>>` instead of
  the map by value, so one parse serves every styling without copying a bag.
  Lookup behaviour is untouched.

**`parse` also shrinks every ring's coordinate vector**, and the number is the
reason: mvt-reader 2.4.0 grows each ring with
`Vec::with_capacity(geometry_data.len())` — the whole feature's command count,
per ring (`mvt-reader-2.4.0/src/lib.rs:361,379,414`). Measured on the Monaco
fixture's z14 city tile, geometry capacity was **28,128,896 bytes for 317,736
of content**; the transient `render` path never noticed because the value was
dropped per tile, but a *cached* parse would have been 29.9 MB per entry
instead of 2.09 MB. Same trade `tessellate_polygon` already makes.

**The golden did not move, and no recording was touched**: `render` routes
through the split, so `rendering_the_fixture_reproduces_the_recorded_shapes_exactly`
now pins parse-then-style against the fused recording. Two new tests:
`one_parse_styled_under_two_styles_matches_the_fused_path_for_both` (equality
with `render` per style, plus the two stylings differing from each other as
the non-vacuity floor) and `a_parsed_tiles_heap_grows_with_its_content`.

One behavioural edge, stated rather than hidden: `parse` decodes **every**
layer the tile carries, so a layer whose features will not decode now fails
the whole tile even when no style layer names it, where the fused path only
reached it on a style's request. A tile like that is a broken tile; failing
it loudly is the honest arm. `Error::LayerNotFound` and
`Error::UnsupportedLayerExtent` are no longer constructed (the lookup falls to
the same `trace!` skip both cases always fell to); the variants stay on the
public enum.

New public surface: `ParsedTile` (opaque; `heap_bytes()`), `parse`, `styled`.

**Test count: `cargo test -p walkers --features mvt` reports 98**, from 95.
Plain `cargo test -p walkers` still selects 48 — the flag gotcha above stands.

### Changed — source, twenty-second commit: a style layer's own zoom range gates it

Two files: `src/style.rs` and `src/mvt.rs`.

`Layer` had no `minzoom` or `maxzoom` field, so serde dropped both at parse and
`styled` visited every style layer at every zoom. The waste is not
tessellation — a layer whose range excludes the zoom draws nothing either
way — it is the **scan**: for each such layer, a `Context` built over every
feature of its source layer and a filter evaluated against it, for a layer that
cannot produce a shape.

Measured on this workspace's committed `dark` style (95 layers) over Monaco's
z14 8529/5974 tile (2,913 features across 14 source layers), before the change:
**36,921 feature scans at every zoom from 0 to 16** — the same number at zoom 0,
where 14 of the 95 layers are live by their own declared ranges, as at zoom 16,
where 78 are. After:

| zoom | scans before | scans after | live layers |
| --- | --- | --- | --- |
| 0 | 36,921 | 347 | 14 / 95 |
| 5 | 36,921 | 4,295 | 26 / 95 |
| 8 | 36,921 | 6,301 | 31 / 95 |
| 10 | 36,921 | 9,543 | 37 / 95 |
| 12 | 36,921 | 18,024 | 55 / 95 |
| 14 | 36,921 | 23,835 | 65 / 95 |
| 16 | 36,921 | 36,680 | 78 / 95 |

`ZoomRange` is a two-field struct `#[serde(flatten)]`ed into the five drawing
variants, so the bounds and their asymmetry are spelled once.
`minzoom` **inclusive**, `maxzoom` **exclusive** — the specification's wording,
not a choice: "at zoom levels less than the minzoom, the layer will be hidden"
against "at zoom levels equal to or greater than the maxzoom, the layer will be
hidden". Both are `Option<f32>`, because the specification allows fractional
bounds while the zoom asked about is the integer tile zoom. `raster` and
`fill-extrusion` stay unit variants and `visible_at` answers `true` for them, so
this is never the reason one of them is skipped.

**Nothing this workspace draws moved, and that was measured rather than
reasoned.** Both committed themes over that tile, at zooms 0, 5, 8, 10, 12, 14
and 16 — fourteen renderings — produce **byte-identical** `Debug` shape lists
before and after. They can, because the committed styles already fold each
layer's zoom range into its `filter` as `[">=", ["zoom"], min]` /
`["<", ["zoom"], max]`, so an out-of-range layer already drew nothing; all this
change does there is stop paying to discover that per feature. That the
fourteen renderings match is also the check that `ZoomRange`'s bounds agree with
the fold's on all 87 ranged layers.

**Where it is not free: a style that does *not* fold.** That is now the better
style to write, and the same measurement says how much better — with every zoom
clause stripped from every filter, `styled` at zoom 14 is **5.40 ms** against
**7.08 ms** for the folded style on unmodified code, output still byte-identical
at all seven zooms. Removing the fold from `www/styles/{dark,light}.json` is a
separate change to those documents and is not in this commit.

New public surface: `ZoomRange`, `Layer::visible_at`.

Two new tests, both shown red against unmodified `59f08766` with only the
test-only additions applied (`git diff --stat`: one file, 164 insertions, **0
deletions**, so no behaviour was in the demonstration):
`a_layer_outside_its_zoom_range_is_never_scanned` read 18 scans where 9 is
correct and drew the out-of-range strokes, and
`the_zoom_range_honours_minzoom_inclusively_and_maxzoom_exclusively` drew them
at zoom 4 of a `[5, 7)` range. Both assert equalities — scans *and* picture
against the same style with the layers deleted — because a one-sided "fewer
scans" is satisfied by drawing nothing at all.

A `#[cfg(test)]` `mvt::scans` counter arrives with them: a thread-local bumped
once per feature considered by one style layer. Thread-local so a filtered run
measuring one walk is not perturbed by a sibling thread rendering. It compiles
to nothing outside `cfg(test)`.

**Test count: `cargo test -p walkers --features mvt --lib` reports 100**, from
98.

## What the pin actually selects

"Upstream's 38 inline tests are the behaviour pin" is the reason this crate is
a workspace member rather than an `exclude`, and it is true with a caveat large
enough that stating the number alone would mislead.

**23 of the 38 do not compile under this workspace's feature selection.** The
22 in `src/expression.rs` and the 1 in `src/style.rs` live in modules gated
`#[cfg(feature = "mvt")]` in `src/lib.rs`, and nothing here enables `mvt` —
`walkers = { workspace = true }` in `squallar/Cargo.toml` and
`squallar-egui/Cargo.toml` names no features, and `default = []`.

> **Stale as of the fifteenth commit — this paragraph is no longer true.**
> `squallar-egui/Cargo.toml` now reads
> `walkers = { workspace = true, features = ["mvt"] }`, and cargo unifies that
> across the workspace build, so `cargo test --workspace` **does** select the
> `mvt`-gated tests. Verified by listing rather than inferred: `cargo test
> --workspace -- --list` names all 27 `expression::tests::*` rows present at
> that commit, out of 4,715 selected. The counts in the table below are from
> before that feature was requested and have not been re-measured; read them as
> that commit's figures, not as today's. Anything reading this to decide
> whether a new expression test is gated: it is.
>
> Note the denominators differ. `cargo test -p walkers --lib` alone still
> selects 48 tests and **zero** expression tests, because `default = []` and
> `-p` does not pull `squallar-egui`'s feature request in. Working on this file
> directly, the spelling is `cargo test -p walkers --lib --features mvt`
> (84 tests), and a filter must use the full path
> `expression::tests::<name>` — a bare test name with `--exact` selects
> nothing and exits **0**.

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

**The 39 above went stale twice, and the table was not updated either time.**
Measured with `cargo test -p walkers --all-features`, exit 0 at each point: the
fifth and sixth commits take it to **45**, and the seventh takes it to **52**.
So the row reading "now | 39" means "after commit 4", not "now" — treat every
figure in that table as stamped with the commit that produced it.

Stamped again: on `ec75a18d`, `cargo test -p walkers --lib --features mvt --
--list` reports **85**, and the seventeenth commit's fixture takes it to
**87**. A figure of 83 was in circulation on 2026-08-27 and does not match this
tree.

The seventh's **+7** is `text::tests`, and they are the first tests this
directory has ever had over label collision: upstream ships no `mod tests` in
`text.rs` at all. That absence is why replacing its predicate had to be checked
by *running* the old one, rather than by watching a suite stay green.

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

### Taking `geo` back out — seventh commit

Two counts again, and again **different denominators that are never added**.
Both are distinct `name version` rows from `cargo tree --prefix none`, `(*)`
back-references stripped, sorted unique; both measured against `6a2b507a`:

| | before | after | delta |
| --- | --- | --- | --- |
| `cargo tree -p walkers --all-features` | 110 | 92 | **−18** |
| `cargo tree -p squallar-web --target wasm32-unknown-unknown` | 279 | 263 | **−16** |

Nothing is added in either. The second row is the one that matters — it is the
shipped wasm bundle, and it is also the first entry in this section measured on
a target other than the host, which answers for `geo` the question the row above
leaves open for the whole 23.

The 16 that leave the wasm bundle: `geo` itself, then `approx`,
`float_next_after`, `geographiclib-rs`, `heapless`, `i_float`, `i_key_sort`,
`i_overlay`, `i_shape`, `i_tree`, `rand`, `rand_core`, `rand_pcg`, `robust`,
`rstar`, `sif-itree`.

**The rows differ by 18 − 16 = 3 − 1, and neither number is an error.**
`approx` is in the 16 but not the 18: it is walkers' own dev-dependency, so it
survives in walkers' tree and is still compiled for its tests. `byteorder`,
`hash32` and `stable_deref_trait` are in the 18 but not the 16: they leave
*walkers'* tree behind `heapless` and `rstar`, and stay in the bundle because
other members reach them independently. This is the reason the two rows are
never added.

`Cargo.lock` loses 13 `[[package]]` blocks — `geo`, `float_next_after 2.0.0`,
`geographiclib-rs`, the five `i_*`, the three `rand*`, `robust`, `sif-itree`.

Thirteen and not 16, and the missing three are the section above's point made
again. `approx` stays because it is still walkers' dev-dependency, and it is
still *compiled*. `rstar 0.12.2` and `heapless 0.8.0` stay **locked but no
longer compiled**: `geo-types` declares all five `rstar` majors as optional
deps behind its `rstar_*` features, so Cargo locks every one of them whether or
not a feature selects any, and `rstar` in turn locks `heapless`. Checked, not
assumed — after this commit `cargo tree -i rstar@0.12.2` and
`cargo tree -i heapless@0.8.0` print `nothing to print` at
`--workspace --all-features --target all`, and fail to match a package at all
without those flags. `cargo tree -i geo` does not match a package under any of
those spellings: it is gone from the workspace, not merely deactivated.

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
