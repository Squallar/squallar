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

The first thing it buys, and the one this commit and the next one are about, is
a graph trim. walkers reaches the network through its own
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
smaller loss than it reads: those tests drive `HttpTiles`, which the next commit
deletes outright as unused.

### Removed — dev-dependencies

| Dependency | Why |
| --- | --- |
| `eframe` | Upstream's, for a `demo/` the tarball does not contain. `autoexamples` is `false` and there is no `examples/` directory, so nothing here can use it. As a non-member dependency walkers' dev-deps were never resolved at all; as a member they are, so keeping it would have materialised eframe and its winit/glow tree in `Cargo.lock` to serve nothing. |
| `env_logger` | Used only by the `http_tiles.rs` test module deleted above. |

`approx` stays — `src/mercator.rs`, `src/position.rs` and `src/projector.rs`
all use it, and those tests do run here.

### Kept deliberately

- `assets/` — as published, in this commit. The next commit deletes all of it
  but `blank-255-tile.png`, for reasons that are its own; keeping it here means
  this commit is a vendoring and nothing else.
- **All 38 in-source `#[test]` fns**, untouched. They are the behaviour pin on
  the code the later patches change, and the bar for any change to this
  directory is that they stay green. See
  [What the pin actually selects](#what-the-pin-actually-selects) for the part
  of that sentence that is smaller than it looks.

  The 38 and the 9 deleted above are disjoint sets, and the two attributes are
  what separates them: the 38 are `#[test]`, spread over seven modules; the 9
  were `#[tokio::test]`, all in `http_tiles.rs`, all `hypermocker`-driven. No
  `#[test]` fn was removed by this commit.
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
is upstream's, outranks the tables, and is already clean on this tree. It stays.

### Changed — source

Two changes, neither of which moves a rendered pixel.

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

   It is also the change that makes this fence survive the next commit, which
   deletes the `HttpTiles` the snippet is built around.

No file under `src/` other than `http_tiles.rs` differs from the tarball by a
byte.

## What the pin actually selects

"Upstream's 38 inline tests are the behaviour pin" is the reason this crate is
a workspace member rather than an `exclude`, and it is true with a caveat large
enough that stating the number alone would mislead.

**23 of the 38 do not compile under this workspace's feature selection.** The
22 in `src/expression.rs` and the 1 in `src/style.rs` live in modules gated
`#[cfg(feature = "mvt")]` in `src/lib.rs`, and nothing here enables `mvt` —
`walkers = { workspace = true }` in `squallar/Cargo.toml` and
`squallar-egui/Cargo.toml` names no features, and `default = []`.

Measured on this commit, `cargo test --workspace`:

| | count |
| --- | --- |
| walkers unit tests selected | **15** — mercator 2, position 4, projector 4, tiles 2, zoom 3 |
| walkers doctests selected | 1 (`src/map.rs`) + 1 ignored (the README fence above) |
| workspace total, before this commit | 4,521 passed |
| workspace total, after this commit | 4,537 passed |

The +16 is exactly those 15 plus the one doctest. The other 23 arrive the day
`mvt` is enabled, which is the same day the style and expression patches land —
so the pin is real for the work it is meant to guard, and the guard is simply
not armed yet. Do not quote "38" as a figure this workspace measures today.

`--all-features` is the other half of that sentence: it *does* enable `mvt`, and
`cargo llvm-cov --all-features` in `.github/workflows/test.yaml` and
`cargo clippy --all-targets --all-features` in `clippy.yaml` both pass it. Those
rows compile the mvt modules and run all 38. The 15 above is the figure for a
default-feature run, and the two are not interchangeable.

## The dependency graph

Making walkers a member has a cost in the lockfile that is worth stating up
front because it is larger than a reader would guess and it is **not** a cost in
the build.

`Cargo.lock` gains 48 `[[package]]` blocks on this commit. That is not the mvt
feature being switched on — it is that Cargo resolves a *workspace member's*
optional and dev dependencies into the lockfile whether or not they are
activated, where for a plain registry dependency it drops the ones no feature
selects. So `geo`, `lyon_path`, `lyon_tessellation`, `mvt-reader`, `color`,
`pmtiles`, `serde_json` and `approx` join the walkers entry's dependency list
and drag their own trees in with them.

Nothing in that set is *built* by a default-feature `cargo build --workspace`:
`cargo tree` still does not resolve them, because features still gate them.
They are locked, not compiled. What it does mean is that a `--all-features` row
now compiles them, and that the lockfile diff for this commit is long.

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

### The two reqwests

`walkers` depends on `reqwest "0.12"`; this workspace pins `reqwest =0.13.4`.
Those do not unify, so the graph carries both — verified on the commit before
this one at `Cargo.lock:4019` (`reqwest 0.12.28`) and `:4057`
(`reqwest 0.13.4`).

`reqwest 0.12.28` is reached from exactly three places, and all three are
walkers': `walkers` itself, `http-cache-reqwest` (walkers' non-wasm
dependency) and `reqwest-middleware` (walkers' dependency). Deleting the HTTP
stack in the next commit removes all three. That is recorded in the next
commit's section of this file rather than asserted here.

**A correction worth carrying**, because it was believed before it was checked:
the graph also holds two `webpki-roots` (`0.26.11` at `Cargo.lock:6064` and
`1.0.9` at `:6073`), and the older one is **not** walkers'. `reqwest 0.12.28`
depends on `webpki-roots 1.0.9`, the same one everything else uses. The single
dependant of `webpki-roots 0.26.11` in the whole lockfile is `tungstenite
0.24.0`, which is reached from `ewebsock 0.8.0` — a direct
`[workspace.dependencies]` pin of this workspace's own, nothing to do with this
directory. Removing walkers' HTTP stack will not remove it, and no work in this
directory can.

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

- **Renovate** — already handled, and not by this commit.
  `.github/renovate.json` lists `vendor/**` in `"ignorePaths"` because of the
  first sibling directory, and that glob covers this one too. It matters here
  for the same reason it mattered there: this manifest's `egui`, `image`,
  `lru`, `reqwest` and `thiserror` declarations are upstream's, not ours to
  move, and a bump merged here would rewrite the vendored crate and quietly
  make the *Local changes* list above untrue. Nothing to do; recorded so the
  next person does not go looking.
- **Coverage** — not handled, named so it is not a surprise.
  `cargo llvm-cov --all-features` in `test.yaml` now measures this crate too,
  and the badge and `.github/coverage-baseline.tsv` it auto-commits move
  accordingly — roughly 4,300 lines of widget joined the denominator, covered
  only to the extent upstream's own inline tests cover them. Note that the
  `--all-features` there means the mvt modules are in that denominator *and*
  their 23 tests are in the numerator, unlike a default-feature run. The number
  changing is expected and is not a regression in this workspace's code.
  Nothing gates on a threshold, so nothing fails.

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
