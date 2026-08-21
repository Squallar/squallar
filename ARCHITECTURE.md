# Rustdar — architecture

Rustdar is a cross-platform NEXRAD weather radar viewer. It fetches Level II
and Level III volumes and a dozen weather overlays, rasterises them, and draws
them on a map on desktop (Linux/macOS/Windows), Android, iOS and in the browser
as a wasm32 + WebGL2 PWA. The GUI is **egui**; the renderer is **wgpu**;
windowing is **winit**.

This file describes the **shape** of the workspace and the rules that hold it
in that shape. It is written from the tree, and every structural claim below
names the file or the test that enforces it. Feature-level and data-source
detail live in `features.md` and `data.md`; keep all three updated when
architecture or features change.

---

## 1. The crate graph

Cargo workspace, `resolver = "2"`, edition 2024, toolchain `stable`
(`rust-toolchain.toml`; edition 2024 needs 1.85+). Twenty members: sixteen
first-party `rustdar-*` crates, the `nexrad-level3` decoder, and three vendored
crates.io crates.

Read the graph bottom-up. Nothing in a lower band may depend on a higher one.

**Band 0 — leaves, no first-party dependencies.**

| Crate | Role |
|---|---|
| `rustdar-geo` | Geographic primitives: `GeoPoint`, `GeoBounds`, `PlacedRaster`, Web Mercator, `MERCATOR_LAT_LIMIT_DEG`. |
| `rustdar-units` | Unit conversion and timezone formatting. `UserPreferences`, persisted in `ui.json`. Conversions happen at display boundaries only; internal data stays in original units. |
| `rustdar-kv` | Small named blobs across sessions. `KvStore` is `load`, `store`, `store_now` and deliberately nothing more. |
| `rustdar-nmea-serial` | NMEA parser and serial-port reader behind the `serial` feature (off on wasm and iOS). |
| `nexrad-level3` | Level III product decoder — WMO headers, zlib/BZ2, radial packets. Byte slices in, model types out; no network, no filesystem. |

**Band 1 — the substrate.** `rustdar-source` stands on `rustdar-geo` and
`rustdar-units` and nothing else. It holds contract and vocabulary only: the
`SourceHandler` trait and its `PaneRef`/`PaneMut` views, `LayerId` and the
`known::` const table, `FieldId`, `RenderMode`, `Surface`, `SourceEvent`,
`SourceLiveness`, `TimeAxis`, the `JobInput`/`JobOut`/`JobCodec` job vocabulary,
`VolumeCapable` and the volume types, the fetch-policy retry ladder, the wire
`Reader`/`Writer`, and the TLS provider selection.

**Band 2 — the data crates.** `rustdar-radar` and `rustdar-overlays` each stand
on the substrate. **They do not know about each other**: the
overlays→radar edge is cut, and anything both sides need lives in
`rustdar-source` instead.

**Band 3 and up — engine, renderer and shell.** `rustdar-device-profile`
(budgets and constants) sits above `rustdar-radar`; `rustdar-worker` (the job
funnel, the pool, the wire) above the two data crates; `rustdar-egui` (pure UI)
above the data crates and the device profile; `rustdar-gpu` (wgpu renderer,
upload path, mirror, staging ring) and `rustdar-volumetric` (the 3D stack) above
that; `rustdar-location` — the location facade, standing only on
`rustdar-geo`, `rustdar-kv` and `rustdar-nmea-serial`, and wrapping every OS's
location quirks behind one Rust surface (`linux`, `windows`, `apple`,
`android`, `web`, plus the NMEA serial provider) — off to one side of them;
`rustdar-app` (the portable application: winit handler, fetch and render
dispatch, app state) above all of them; and the two entry crates on top —
`rustdar` (desktop/Android/iOS binary and `rustdar_native` lib) and
`rustdar-web` (browser).

**The direction is enforced, not merely intended.** Nine crates carry a
`tests/charter.rs` that reads `cargo metadata --no-deps --format-version 1` and
asserts against **declared** dependencies, so no feature selection can mask what
they see. Each charter has a `the_dependency_ceiling_holds` test with an
explicit allow-list plus a falsifiability floor (an empty parse cannot pass it),
and most add a direction test:

| Charter | Direction test |
|---|---|
| `rustdar-source/tests/charter.rs` | `the_overlays_to_radar_edge_stays_cut` |
| `rustdar-geo/tests/charter.rs` | `the_floor_sits_under_the_substrate` |
| `rustdar-device-profile/tests/charter.rs` | `the_floor_sits_under_the_app_side` |
| `rustdar-gpu/tests/charter.rs` | `the_boundary_sits_under_the_app` |
| `rustdar-volumetric/tests/charter.rs` | `the_stack_sits_under_the_app` |
| `rustdar-worker/tests/charter.rs` | `the_engine_sits_above_the_vocabulary` |
| `rustdar-kv/tests/charter.rs` | `the_contract_is_three_methods_and_nothing_more` |
| `rustdar-location/tests/charter.rs` | `the_feature_fences_map_the_arms`, `the_facade_stands_on_the_provider_and_not_the_reverse` |
| `rustdar-nmea-serial/tests/charter.rs` | ceiling only |

**One cycle looks like a cycle and is not.** `rustdar-volumetric` declares a
normal dependency on `rustdar-gpu`; `rustdar-gpu` declares `rustdar-volumetric`
as a **dev**-dependency only, for its own test targets. Cargo's normal-dep graph
is acyclic. Read dependency questions off `cargo metadata`, per kind, the way
the charters do.

**Vendored members.** `vendor/nexrad-decode`, `vendor/nexrad-data` and
`vendor/bzip2-rs` are workspace members rather than `exclude`d, deliberately:
their own upstream test targets are the behaviour pin that our decode patches
must leave untouched, and they only run if `cargo test --workspace` selects
them. The first two are patched over the registry copies by `[patch.crates-io]`
in the root `Cargo.toml`; `bzip2-rs` is not patched — nothing else in the graph
resolves that name, so `vendor/nexrad-data` depends on it by path. Each carries
a `VENDORED.md` saying what was changed and why.

**Version pins.** Every external dependency is pinned exactly (`=x.y.z`) in
`[workspace.dependencies]` in the root `Cargo.toml`. That section is the source
of truth; don't restate versions elsewhere and don't upgrade without testing.

---

## 2. The two boundary rules

These two sentences are carried verbatim out of `.github/copilot-instructions.md`,
which this file replaces. Both were re-verified against the tree at the time of
writing.

> **UI ↔ Platform boundary:** `rustdar-egui` must not depend on wgpu/winit.
> Communicates via `GuiAction` (out) and setter methods (in). Entry point:
> `Gui::ui(&mut self, &egui::Context) -> Vec<GuiAction>`.

Still true of the dependency half — `rustdar-egui`'s manifest names neither
wgpu nor winit, and `rustdar-gpu` exists precisely so the renderer can sit
*above* the UI crate rather than inside it. The "setter methods (in)" half has
since been replaced: the in-direction is now the typed seam of §3, and the
setter surface is ratcheted at 0 in `ui.rs`.

> **No portable code** [in `rustdar`] **— that lives in `rustdar-app`, which
> this crate depends on (never the other way round).**

Still true: `rustdar` declares `rustdar-app`, and `rustdar-app` declares no
dependency on `rustdar` of any kind.

---

## 3. Seam inventory

A **seam** is a place where one layer speaks to another through a named type
rather than through field access. Each has an owning crate and a test that
pins it.

### 3.1 `FrameInputs` — App → Gui, snapshot-shaped

* **Owner**: `rustdar-egui/src/shell_api.rs`.
* **Shape**: one frame's facts, composed by the App from state it already owns,
  applied by `Gui::apply_frame_inputs` once per frame immediately before
  `Gui::ui`. Insets, exit support, loop frame budget, location permission and
  fix, heading, catalogue pending, the opaque `liveness` slice, floor tile zoom
  bias.
* **Contract test (Gui half)**: `rustdar-egui/src/shell_api/tests.rs` —
  a sentinel-expression walk asserting every field surfaces through the `Gui`'s
  own read side **and persists** across frames with no re-application, so a
  missed compose is a stale value rather than a reverted one.
* **Contract test (App half)**: `rustdar-app/src/app/chunk_feed_precedence_tests.rs`,
  `the_chunk_feeds_status_reaches_the_seam_that_publishes_it`. It exists because
  the Gui-half test alone was green while the App computed a status and dropped
  it on the floor.

### 3.2 `GuiEvent` — App → Gui, event-shaped

* **Owner**: `rustdar-egui/src/shell_api.rs`; applied by `Gui::apply` at the
  call site's existing control-flow position, so drain timing does not move.
* **Nine variants**, each named after the behaviour it replaced: scan info for a
  site / for a pane, merge-semantics chunk scan info, fetching, error, radar
  config, per-pane live/historic, and the `VolumePainter` install.

### 3.3 `GuiAction` — Gui → App

* **Owner**: `rustdar-egui/src/actions.rs`. `Gui::ui` returns `Vec<GuiAction>`;
  `App::process_gui_actions` dispatches them.
* **Contract test**: `rustdar-app/src/app/gui_action_replay_tests.rs`,
  `a_scripted_action_batch_lands_through_the_seam` — a scripted batch driven
  through both directions of the seam.

### 3.4 `SourceHandler` — the layer contract

* **Owner**: `rustdar-source/src/handler.rs`. Fifty-eight methods; most
  defaulted. `RadarSource` (`rustdar-radar/src/source.rs`) overrides
  thirty-four of them.
* **The scope line, as ruled**:

  > at campaign end `RadarSource` implements every `SourceHandler` surface
  > EXCEPT `create_fetch_tasks`; radar's per-pane multi-stage ingest stays
  > bespoke; **Level III loop supply and the decoded-volume cache behind
  > `frames_resident`/`retain_frames` are named parts of that bespoke half**;
  > the per-pane fetch seam is post-campaign.

  Measured beside it, so the sentence is not read wider than it is:
  `create_fetch_tasks` is one of the twenty-four surfaces `RadarSource` leaves
  at their trait default. It is the one that *matters*, because it is the
  overlay fetch door; radar arrives instead through `create_frame_list_task`,
  `list_frames`, `fetch_frame`, `apply_frame_listing` and its own bespoke
  multi-stage ingest.
* **Composition**: `rustdar-egui/src/sources.rs::all()` chains
  `rustdar_overlays::render::handlers::sources()` with
  `rustdar_radar::source::sources()`. That is the only composition.

### 3.5 `JobCodec` / the job funnel

* **Owner**: `rustdar-source/src/job.rs` (`JobInput`, `JobOut`, `DescribedJob`,
  `DescribedOut`, `JobGeometry`, `JobCost`, `JobCodec`, `JobSpec`,
  `JobOutCodec`); funnel in `rustdar-worker/src/offload.rs`; composition in
  `rustdar-worker/src/job_registry.rs`.
* **The pin**: `rustdar-app/tests/arch_ratchets.rs::offload_names_zero_source_crate_types`
  — `offload.rs` names **zero** `rustdar_overlays::` or `rustdar_radar::` paths,
  in either direction, prose included, with a presence control on
  `job_registry.rs` so a rotted needle cannot leave the zero green over
  anything.

### 3.6 `KvStore` — configuration persistence

* **Owner**: `rustdar-kv/src/lib.rs`. String keys, string blobs, never
  load-bearing: a backend that cannot tell "absent" from "unreadable" answers
  `None` for both and the caller falls back to defaults.
* Keys are logical names, not paths — a filesystem backend maps a key onto a
  filename itself, the web build lands in `localStorage`, tests hold everything
  in memory. **The key strings are on-disk compatibility**: a changed string
  silently orphans every config an existing install has saved. Each constant
  lives beside its owner.
* **Pin**: `rustdar-kv/tests/charter.rs::the_contract_is_three_methods_and_nothing_more`.

### 3.7 `VolumePainter` → `VolumeCapable`

* **`VolumePainter`** (`rustdar-egui/src/volume_view.rs`) is what the UI is
  handed so a 3D pane can draw; it is installed through
  `GuiEvent::VolumePainter` and can be taken away again.
* **`VolumeCapable`** (`rustdar-source/src/volume.rs`) is the layer-side half: a
  handler that can build a volume answers `SourceHandler::volume()` with one,
  and shapes its own job through the required, undefaulted `volume_job`.
* **Pin**: `rustdar-app/tests/arch_ratchets.rs::the_radar_geometry_type_is_defined_in_radar_and_not_in_egui`
  keeps the radar-shaped half of this out of the presentation crate.

### 3.8 `AsyncTileSource` — map tiles

* **Owner**: `rustdar-egui/src/tile_source.rs`. A blanket-implemented marker
  splitting the `Send + Sync` host bound from the single-threaded wasm one, so
  the same tile machinery compiles on both.
* CartoDB no-labels base plus a labels-only overlay drawn *above* radar and
  overlays, so text is not obscured.

---

## 4. Binding runtime rules

**The frame thread does no heavy work.** Long-running, CPU-bound work goes
through `rustdar_worker::offload::offload_job`, which posts to a worker if this
thread has a sink and otherwise runs it here. "It runs rarely" is not an
exception. Freeing large payloads goes through `offload::discard` from the frame
thread, because the deferred queue is thread-local.

**Interaction is realtime; data may lag.** Layers and volumes may take time to
arrive, and minimising that is worth work — but map movement, controls and UI
stay realtime. Never trade interaction latency for data latency.

**Reopen is 1:1.** UI state persists so that reopening the app is visually
identical to closing it. "That's session posture" is not a reason not to
persist. Units and formatting go through `rustdar-units`.

**Every option is expressed.** Every option any model offers must be reachable
and *drawn* through the real chrome on every width class. Enforced by
`rustdar-egui/src/parity_walk.rs` —
`every_option_is_reachable_on_a_compact_screen`,
`..._on_a_medium_screen`, `..._on_an_expanded_screen` — which drives the real
input harness and records an item as reachable only when its centre lands
inside the screen rect. A control may be **disabled with a stated reason**; it
may not be absent. Run the walk in both feature arms:

```bash
cargo test -p rustdar-egui parity_walk                    # 3/3, twelve layers
cargo test -p rustdar-egui --features fake-source parity_walk   # 3/3, thirteen
```

**The web target is measured, never inferred.** A scaled or extrapolated figure
is not acceptable evidence about the browser. And be precise about what a green
browser gate proves:

> **No gate has ever executed a frame of the web build.** There is no
> `wasm-bindgen-test` in this workspace; the CI wasm rows are build and check.
> Two gaps were measured at WO-M13a and both still hold: the stock Tier-2 scene
> **enables no texture overlay**, so those legs prove boot, render and the
> worker wire and nothing about the overlay arrival path; and `rustdar-web`
> initialises `console_log` at `Level::Info`, so a `debug!` line can **never**
> appear in a browser. **A green Tier-2 is evidence about the scene Tier-2
> runs.** A change whose subject that scene does not exercise must say so and
> measure it another way.

**"Web" is two targets.** Firefox is first-class and governs over Chrome.
Measure both separately and never merge the figures.

**WebGL2, never WebGPU.** `rustdar_app::app` pins `Backends::GL` on wasm32 and
the `webgpu` wgpu feature is deliberately absent on every target — Firefox has
no stable WebGPU, so compiling it would only add an untested second rendering
path. `rustdar-gpu/Cargo.toml` is the one feature-chooser for the whole graph;
read its comments before touching wgpu features anywhere.

**Generation counters guard against stale results.** `RenderDispatch`
(`rustdar-app/src/render_dispatch.rs`) keeps a per-site `fetch_generations` map
and one `render_generation`; take the next generation before spawning
(`next_fetch_generation`), and discard a result whose generation is below the
current one (`is_fetch_stale`, `is_render_stale`).

---

## 5. Adding a source

A source is a layer. Adding one is **one handler file plus three registration
lines**, and nothing outside `rustdar-overlays` (or `rustdar-radar`, for a
radar-shaped source).

1. **The handler file** — `rustdar-overlays/src/render/handlers/<name>.rs`,
   implementing `SourceHandler`. Follow `outlook.rs` or `alert.rs` for a
   polygon/texture layer, `metar.rs` for a per-frame point layer,
   `location.rs`/`colorscale.rs` for a per-frame direct layer.
2. **`rustdar-source/src/id.rs`** — a `known::` const.
3. **`rustdar-source/src/id.rs`** — the matching `LAYER_ID_LEDGER` entry.
4. **`rustdar-overlays/src/render/handlers/mod.rs`** — one row in `sources()`,
   which is the only place these registrations are named.

Then bump the two hand-kept second spellings in `rustdar-egui/src/sources.rs`:
`REGISTERED_LAYER_COUNT` and, if the source registers fields,
`REGISTERED_FIELD_COUNT`. **Never derive either from `all()`, `sources()` or
`LAYER_ID_LEDGER::len()`** — a floor computed from the thing it floors compares
the registry against itself and cannot fail.

**The executable form of this checklist is the fake source.**
`rustdar-overlays/src/render/handlers/fake.rs`, behind the `fake-source`
feature (forwarded by `rustdar-egui/fake-source` and `rustdar-app/fake-source`,
and enabled by nothing that ships), is a thirteenth layer that exists only so a
test build can watch a source arrive through the seams. Every count-pin
downstream is written as `12 + cfg!(feature = "fake-source") as usize`, and
`sources()` adds exactly one appended registration line under that cfg. If a new
source needs an arm anywhere the fake source does not have one, the seam is
incomplete — fix the seam, not the call site.

Radar registers itself: `rustdar_radar::source::sources()`, chained by
`rustdar_egui::sources::all()`. A new source **never adds a channel** — the
`ChannelHub`'s generic seams (`overlay_render`, `voxel`) already carry it.

---

## 6. The ratchet index

Architecture is held by counted ceilings. **A ceiling only ever falls.** Lower
the pin in the land that earns it; **never raise one without a written plan
amendment**. Never hide a needle instead of shedding the coupling — the
`let gui = &mut self.gui;` re-spelling is **forbidden by name**, because it
makes the walker read zero while the coupling is identical.

Values below are **final**, re-measured at WO-E10.4 (campaign close). Every one
of them equals the count it measures, so **none has headroom** — which is what a
ceiling is for once the migration that spent them down is over.

### 6.1 `rustdar-app/tests/arch_ratchets.rs` — 9 tests

| Test | Constant | Ceiling | What it counts |
|---|---|---|---|
| `the_app_pokes_gui_coupling_never_grows` | `SELF_GUI_MAX` | 184 | `self.gui.` anywhere in `rustdar-app` |
| | `SELF_GUI_NON_TEST_MAX` | 179 | the same, outside test-named paths |
| | — | 0 | `self.gui.set_` anywhere in `rustdar-app` — the target zero, held as a test rather than as a grep |
| `the_config_swap_stays_deleted` | — | 0 | `load_pane_configs` / `save_pane_configs` / `loaded_configs`, with `serialize_pane_state` as the presence control |
| `the_gui_setter_surface_never_grows` | `UI_SETTER_MAX` | 0 | `pub fn set_` in `rustdar-egui/src/ui.rs` |
| | `GUI_IMPL_SETTER_MAX` | 1 | the same needle over **every** inherent `impl Gui` block in `rustdar-egui` |
| `the_product_enum_never_spreads_further_into_egui` | `PRODUCT_IN_EGUI_MAX` | 0 | `RadarProduct` anywhere under `rustdar-egui`, **comments included** |
| `the_channel_hub_never_grows_past_eighteen_receiver_pairs` | `HUB_RECEIVER_MAX` | 17 | `_receiver: Receiver<` fields on `ChannelHub` |
| `offload_names_zero_source_crate_types` | — | 0 | `rustdar_overlays::` / `rustdar_radar::` in `rustdar-worker/src/offload.rs` |
| `the_radar_geometry_type_is_defined_in_radar_and_not_in_egui` | — | 1 / 0 | `struct LoopGeometry` in radar / in egui |
| `the_loop_frame_arms_stay_radars_own_vocabulary` | `LOOP_FRAME_ARMS_MAX` | 8 | the loop frame's closed arms and their two cache aliases |
| | `LOOP_FRAME_ARMS_NON_TEST_MAX` | 2 | the same, outside tests |
| `the_ingest_phase_has_exactly_one_production_caller` | `INGEST_CALLERS_NON_TEST` | 1 | `self.poll_data_channels(` outside test paths — the frame pump's `Ingest` phase, whose once-per-frame property WO-M13b measured and registered as unpinned |

Run it by target name — a filter matching zero tests is a failed run, not a
pass:

```bash
cargo test -p rustdar-app --test arch_ratchets   # 9/9
```

### 6.2 `rustdar-app/src/app/gui_seam_ratchet_tests.rs` — the per-file coupling ceilings

| Test | File | Ceiling |
|---|---|---|
| `no_production_file_pushes_through_a_gui_setter` | `app.rs`, `app_fetch.rs`, `app_render.rs`, `app_chunks.rs` | 0 `self.gui.set_` each |
| `the_gui_coupling_only_ever_shrinks` | `app.rs` | 37 |
| | `app_fetch.rs` | 45 |
| | `app_render.rs` | 108 |
| | `app_chunks.rs` | 13 |

Both scrapes are **whitespace-collapsed**, so a call wrapped across lines counts
exactly like one that is not, and a comment containing the needle counts too.
**Both** tests carry the same presence control (`self.gui.apply` must still be
found in `app.rs`) so neither can pass by reading nothing —
`the_gui_coupling_only_ever_shrinks` gained it at WO-E10.4, when its ceilings
came down to their measured values and four empty strings would otherwise have
satisfied all four.

### 6.3 These ceilings are permanent

**The `self.gui.` ceilings are a standing contract, not migration scaffolding.**
They are not deleted at any milestone. The contract they state is: **the app
layer does not grow its reach into the UI layer, and an attempt to is a build
failure, not a review comment.** They may only ever **fall**.

**Since WO-E10.4 every one of them sits exactly on its measured value**, so none
has headroom: under a permanent ceiling the correct response to needing a new
reach is **shed first, then land** — not "ask whether the ceiling should move".
(Before that land they each carried one spare slot, and "three ratchets at zero
headroom" was a paraphrase that was never true; the ceilings are the record, not
the paraphrase.) The two honest sheds are:

* **loop-state addressing** — the vocabulary by which the app reaches loop state
  held on the `Gui`; and
* **the all-panes-versus-visible-panes distinction**, which has already produced
  one bug.

If neither is reachable inside a change's charter, **stop and report**. Do not
raise, and do not re-spell.

### 6.4 Other standing pins

* **Dependency ceilings** — the nine `tests/charter.rs` files of §1.
* **Cited-test resolution** — `rustdar-radar/tests/doc_citations_resolve.rs`
  scans **every `//`, `///` and `//!` comment in the workspace** and requires
  any backticked name whose final `::` segment is snake_case with **five or
  more** underscore-separated segments to resolve to a real `fn` or `mod`, and
  requires a citation to an `#[ignore]`d test to say that it is ignored.
  **Do not name a test in a comment unless it exists.** Genuine non-defects go
  in that file's `ALLOWED` table *with the reason*.
* **One geodesy definition** — `rustdar-radar/tests/geodesy_one_definition.rs`.
* **No release-artifact row asks for a test-only feature** —
  `rustdar/src/release_artifact_features.rs` parses `.github/workflows/build.yaml`'s
  build matrix and, for every row carrying an `artifact:` key, asserts that the
  feature **set** that row's cargo command requests contains no test-only
  feature. `--all-features` fails it because it expands to every feature the
  member manifests declare — the property, not the spelling. It carries two
  presence controls (the four desktop rows are found by name with a `cmd:`
  each; every needle names a feature some manifest declares). Its module docs
  record the one residual it deliberately does **not** assert: `--all-targets`
  unifies `rustdar-radar/test-support` into the shipped lib unit, measured, and
  unfolding the artifact build from the coverage build is not a change a gate
  should force.
* **On-screen strings are ASCII unless the glyph is registered** —
  `rustdar-egui/src/ui_glyphs.rs` holds the one inventory of icon and text
  glyphs, verified against the fonts egui actually bundles, and
  `ui_string_literals_use_only_registered_glyphs` scans every string literal in
  `rustdar-egui`, `rustdar-overlays`, `rustdar-app` and `rustdar-volumetric`
  (non-test sources) for unregistered non-ASCII characters. Check it whenever a
  user-visible string changes: either spell it in ASCII or register the glyph.

### 6.5 Recorded measurements — information, never assertions

**`cfg(target_arch = "wasm32")` lines per crate.** A count-based ceiling on
these was **rejected by user ruling** ("ratchets for things like this seem super
sketchy and inappropriate for the rust ecosystem"), and nothing in the tree
asserts these numbers. They are recorded because the *shape* of the number is
worth watching by eye, not because a number is a contract.

The rule that does bind is qualitative and lives here and in review: **a `cfg`
may select a value, a dependency or a type alias. It may never fork behaviour
inside a function body.** Two arms of one function that do different things are
two untested programs; a `cfg` that picks a constant or an implementation is one
program with a platform-appropriate part.

Measured at WO-E10.4 (matching lines, per crate, `target`/`pkg` excluded):

| crate | lines | | crate | lines |
|---|---:|---|---|---:|
| `rustdar-device-profile` | 70 | | `rustdar-overlays` | 21 |
| `rustdar-radar` | 44 | | `rustdar-gpu` | 18 |
| `rustdar-app` | 41 | | `rustdar-location` | 17 |
| `rustdar-egui` | 39 | | `rustdar-source` | 17 |
| `rustdar-worker` | 26 | | `rustdar-web` | 14 |
| | | | `rustdar-volumetric` | 12 |
| | | | `rustdar-geo`, `rustdar-kv`, `rustdar-nmea-serial` | 1 each |

**Total 322 across 14 crates.** The plan's older baseline (`165/54/40/30/25,
Σ314`) is **not comparable** and should not be read as a rise of 8: it was taken
over five pre-reshape crates, and the Phase-3R/3F dissolution re-partitioned that
same code across fourteen. Comparing them would be a figure without its
denominator.

---

## 7. Before landing

Carried verbatim from `.github/copilot-instructions.md` and re-verified against
`.github/workflows/` at the time of writing. This is the one section ported
forward, because it is the one that was checked.

The Clippy workflow alone holds five gates, and no single command matches CI.
Three are cheap enough to run before every land, listed in the order CI reaches
them:

```bash
cargo fmt --all -- --check                                             # Format job
cargo check --workspace --all-targets --target wasm32-unknown-unknown  # Clippy job, wasm32 row
cargo clippy --all-targets --all-features -- -D warnings               # Clippy job, gating run
```

- **`-D warnings` is the gate.** A bare `cargo clippy --all-targets --all-features` is the autofix step that runs *before* it and fails on nothing, so it is not the CI-matching command.
- **The wasm32 row goes before clippy**, as it does in CI: it sits several steps ahead of `Run Clippy` in the same job, so a break there aborts the job and hides every lint finding behind it. It is also the only build that type-checks `rustdar-web` — the crate's deps are under `[target.'cfg(target_arch = "wasm32")'.dependencies]`, so every other target compiles it as an empty shell and reports success.
- **The other two gates are not cheap**, and CI is the right place for them: the host build (`cargo build --workspace --all-targets --all-features`) and the Android arm (`cargo ndk -t arm64-v8a -P <minSdk> check -p rustdar --lib`). Test runs `cargo llvm-cov --no-report --all-features` in its own workflow.
- **Check `--all`, fix `-p`.** Any formatter or lint-fixer that *writes* stays package-scoped (`cargo fmt -p <package>`); only checks go workspace-wide. An `--all` write reformats files the branch does not own, and a package-scoped *check* hides breakage elsewhere.

Two optional hooks in `.githooks/` run a subset of this locally. They are inert
until turned on, per clone:

```bash
git config core.hooksPath .githooks
```

`pre-commit` rustfmt-checks the staged `.rs` files only and runs no cargo;
`pre-push` runs the wasm32 row once per push. Both are escapable with
`--no-verify` for work in progress. The path is relative on purpose: git
resolves `core.hooksPath` against the root of the working tree the hook runs in,
so this one setting covers every worktree and each finds its own copy. The hooks
are tracked files, so they travel with the branch rather than living in a
`.git/hooks` nobody else can see.

**Re-verification note.** All of the above still matches
`.github/workflows/clippy.yaml`: the `fmt` job runs `cargo fmt --all -- --check`;
the `clippy` job runs the autofix (`--fix || true`), then the host build, then
the wasm32 check, then the Android `cargo ndk` row — which reads `minSdk` out of
`packaging/android/app/build.gradle.kts` rather than hard-coding it — and only
then `Run Clippy` with `-D warnings`. `.github/workflows/test.yaml` runs
`cargo llvm-cov --no-report --all-features`, plus a separate GPU job against a
software rasteriser. The two hooks exist and do what the paragraph says.

---

## 8. The documentation rule

**A measured claim in a comment carries the commit or campaign it was measured
under.** `main@ebe0ad3b`, `measured on main@62289151`, "measured at
main@ebe0ad3b, 2026-08-12" — the form is not important, the attribution is.
**Unattributed performance prose is a defect**, not a stylistic preference,
because there is no way to tell a figure that is still true from one that
described a mechanism which has since been deleted.

Three corollaries this tree has paid for:

* **Prose is not evidence.** A bare positive claim sitting beside caveated
  neighbours is a tell, not a reassurance. When a comment asserts a property,
  say what carries it: a test name, a type, a measurement.
* **State what a check can and cannot fail on.** A guard that cannot fail is
  worse than no guard, because it reads green. Every absence pin needs a
  presence control on the same walk; every zero needs an existence control on
  the haystack.
* **Say whether a claim is checked or hypothesised.** Both are useful; conflating
  them is not.

Keep this file, `features.md` and `data.md` updated when architecture or
features change.
