# Squallar — architecture

Squallar is a cross-platform NEXRAD weather radar viewer. It fetches Level II
and Level III volumes and a dozen weather overlays, rasterises them, and draws
them on a map on desktop (Linux/macOS/Windows), Android, iOS and in the browser
as a wasm32 PWA (WebGPU, falling back to WebGL2). The GUI is **egui**; the
renderer is **wgpu**; windowing is **winit**.

This file describes the **shape** of the workspace and the rules that hold it
in that shape. It is written from the tree, and every structural claim below
names the file or the test that enforces it. Feature-level and data-source
detail live in `features.md` and `data.md`; keep all three updated when
architecture or features change.

---

## 1. The crate graph

Cargo workspace, `resolver = "2"`, edition 2024, toolchain `stable`
(`rust-toolchain.toml`; edition 2024 needs 1.85+). Twenty-one members:
seventeen first-party `squallar-*` crates, the `nexrad-level3` decoder, and three
vendored crates.io crates.

Read the graph bottom-up. Nothing in a lower band may depend on a higher one.

**Band 0 — leaves, no first-party dependencies.**

| Crate | Role |
|---|---|
| `squallar-geo` | Geographic primitives: `GeoPoint`, `GeoBounds`, `PlacedRaster`, Web Mercator, `MERCATOR_LAT_LIMIT_DEG`. |
| `squallar-units` | Unit conversion and timezone formatting. `UserPreferences`, persisted in `ui.json`. Conversions happen at display boundaries only; internal data stays in original units. |
| `squallar-kv` | Small named blobs across sessions. `KvStore` is `load`, `store`, `store_now` and deliberately nothing more. |
| `squallar-nmea-serial` | NMEA parser and serial-port reader behind the `serial` feature (off on wasm and iOS). |
| `nexrad-level3` | Level III product decoder — WMO headers, zlib/BZ2, radial packets. Byte slices in, model types out; no network, no filesystem. |
| `squallar-netcdf` | NetCDF4-over-HDF5 reading and CF-convention unpacking (`_Unsigned`, `_FillValue`, `valid_range`, `scale_factor`, CF time units). Knows the format; knows nothing about satellites or lightning. Two shapes for a decoded variable — `Vec<Option<f64>>` for records, `Vec<f32>`/NaN for rasters — plus row-windowed reads. |

**Band 1 — the substrate.** `squallar-source` stands on `squallar-geo` and
`squallar-units` and nothing else. It holds contract and vocabulary only: the
`SourceHandler` trait and its `PaneRef`/`PaneMut` views, `LayerId` and the
`known::` const table, `FieldId`, `RenderMode`, `Surface`, `SourceEvent`,
`SourceLiveness`, `TimeAxis`, the `JobInput`/`JobOut`/`JobCodec` job vocabulary,
`VolumeCapable` and the volume types, the fetch-policy retry ladder, the wire
`Reader`/`Writer`, and the TLS provider selection.

**Contract and vocabulary — plus the values two bands both have to agree on.**
`REFLECTIVITY_SHARED_STOPS` (`product.rs`) is the first of those and the rule it
sets is narrow. Until 2026-08-23 a colour ramp was held to belong to the crate that
published the field, and reflectivity was drawn through three separate tables:
`squallar-radar`'s, MRMS's and HRRR's. The two overlay tables were pinned equal
to each other; the radar one was pinned to nothing, and it had drifted about one
5 dBZ band through the green-to-red region — the same storm read 45 dBZ red on a
tilt and orange on the mosaic beside it, in the same pane, with every gate
green. The stops moved here because the rule below already covers them: this is
a thing both data crates need, and the edge between them is cut.

**A ramp only one source publishes still lives with that source**, and *how* a
ramp is painted stays with the source too — `LegendScale::is_gradient` is a
per-layer decision, and the three dBZ layers genuinely disagree about it (a
radar tilt is a wash, a mosaic and a forecast composite are bands). What may
move down here is the shared *value*, not the presentation, and only once a
second band needs it.

**And what comes down is the agreement, which is allowed to be narrower than the
whole thing.** The dBZ ladders agree from 0 through 70 and part above it: radar
draws a hail band to 95 dBZ that MRMS and HRRR do not, because their grids do
not produce values up there and a bar advertising a range its own raster cannot
reach is worse than a divergence. So the substrate holds a shared core, a
per-layer tail, and `REFLECTIVITY_DIVERGENCE_DBZ` naming the one value with two
colours — not one table every layer slices. A layer names the ladder it draws
(`REFLECTIVITY_RADAR_STOPS`, `REFLECTIVITY_OVERLAY_STOPS`); nothing outside
`product.rs` builds a bar from the core alone. **A value moved down here must be
one the bands really do agree on, and where they stop agreeing is part of what
gets written down.** `REFLECTIVITY_ALPHA` is a second such value — one opacity
for the same quantity on every layer that draws it, scoped to that field and not
to either crate's own translucency constant.

**Band 2 — the data crates.** `squallar-radar` and `squallar-overlays` each stand
on the substrate. **They do not know about each other**: the
overlays→radar edge is cut, and anything both sides need lives in
`squallar-source` instead.

**Band 3 and up — engine, renderer and shell.** `squallar-device-profile`
(budgets and constants) sits above `squallar-radar`; `squallar-worker` (the job
funnel, the pool, the wire) above the two data crates; `squallar-egui` (pure UI)
above the data crates and the device profile; `squallar-gpu` (wgpu renderer,
upload path, mirror, staging ring) and `squallar-volumetric` (the 3D stack) above
that; `squallar-location` — the location facade, standing only on
`squallar-geo`, `squallar-kv` and `squallar-nmea-serial`, and wrapping every OS's
location quirks behind one Rust surface (`linux`, `windows`, `apple`,
`android`, `web`, plus the NMEA serial provider) — off to one side of them;
`squallar-app` (the portable application: winit handler, fetch and render
dispatch, app state) above all of them; and the two entry crates on top —
`squallar` (desktop/Android/iOS binary and `squallar_native` lib) and
`squallar-web` (browser).

**The direction is enforced, not merely intended.** Ten crates carry a
`tests/charter.rs` that reads `cargo metadata --no-deps --format-version 1` and
asserts against **declared** dependencies, so no feature selection can mask what
they see. Each charter has a `the_dependency_ceiling_holds` test with an
explicit allow-list plus a falsifiability floor (an empty parse cannot pass it),
and most add a direction test:

| Charter | Direction test |
|---|---|
| `squallar-source/tests/charter.rs` | `the_overlays_to_radar_edge_stays_cut` |
| `squallar-geo/tests/charter.rs` | `the_floor_sits_under_the_substrate` |
| `squallar-device-profile/tests/charter.rs` | `the_floor_sits_under_the_app_side` |
| `squallar-gpu/tests/charter.rs` | `the_boundary_sits_under_the_app` |
| `squallar-volumetric/tests/charter.rs` | `the_stack_sits_under_the_app` |
| `squallar-worker/tests/charter.rs` | `the_engine_sits_above_the_vocabulary` |
| `squallar-kv/tests/charter.rs` | `the_contract_is_three_methods_and_nothing_more` |
| `squallar-location/tests/charter.rs` | `the_feature_fences_map_the_arms`, `the_facade_stands_on_the_provider_and_not_the_reverse` |
| `squallar-nmea-serial/tests/charter.rs` | ceiling only |
| `squallar-netcdf/tests/charter.rs` | `the_crate_is_a_band_zero_leaf`, `the_format_layer_sits_under_the_data_crate` |

**One cycle looks like a cycle and is not.** `squallar-volumetric` declares a
normal dependency on `squallar-gpu`; `squallar-gpu` declares `squallar-volumetric`
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

These two sentences are carried verbatim out of the repository instructions this
file replaced (`.github/copilot-instructions.md`, deleted once this file and
`CLAUDE.md` took over). Both were re-verified against the tree at the time of
writing.

> **UI ↔ Platform boundary:** `squallar-egui` must not depend on wgpu/winit.
> Communicates via `GuiAction` (out) and setter methods (in). Entry point:
> `Gui::ui(&mut self, &egui::Context) -> Vec<GuiAction>`.

Still true of the dependency half — `squallar-egui`'s manifest names neither
wgpu nor winit, and `squallar-gpu` exists precisely so the renderer can sit
*above* the UI crate rather than inside it. The "setter methods (in)" half has
since been replaced: the in-direction is now the typed seam of §3, and the
setter surface is ratcheted at 0 in `ui.rs`.

> **No portable code** [in `squallar`] **— that lives in `squallar-app`, which
> this crate depends on (never the other way round).**

Still true: `squallar` declares `squallar-app`, and `squallar-app` declares no
dependency on `squallar` of any kind.

---

## 3. Seam inventory

A **seam** is a place where one layer speaks to another through a named type
rather than through field access. Each has an owning crate and a test that
pins it.

### 3.1 `FrameInputs` — App → Gui, snapshot-shaped

* **Owner**: `squallar-egui/src/shell_api.rs`.
* **Shape**: one frame's facts, composed by the App from state it already owns,
  applied by `Gui::apply_frame_inputs` once per frame immediately before
  `Gui::ui`. Insets, exit support, loop frame budget, location permission and
  fix, heading, catalogue pending, the opaque `liveness` slice, floor tile zoom
  bias.
* **Contract test (Gui half)**: `squallar-egui/src/shell_api/tests.rs` —
  a sentinel-expression walk asserting every field surfaces through the `Gui`'s
  own read side **and persists** across frames with no re-application, so a
  missed compose is a stale value rather than a reverted one.
* **Contract test (App half)**: `squallar-app/src/app/chunk_feed_precedence_tests.rs`,
  `the_chunk_feeds_status_reaches_the_seam_that_publishes_it`. It exists because
  the Gui-half test alone was green while the App computed a status and dropped
  it on the floor.

### 3.2 `GuiEvent` — App → Gui, event-shaped

* **Owner**: `squallar-egui/src/shell_api.rs`; applied by `Gui::apply` at the
  call site's existing control-flow position, so drain timing does not move.
* **Nine variants**, each named after the behaviour it replaced: scan info for a
  site / for a pane, merge-semantics chunk scan info, fetching, error, radar
  config, per-pane live/historic, and the `VolumePainter` install.

### 3.3 `GuiAction` — Gui → App

* **Owner**: `squallar-egui/src/actions.rs`. `Gui::ui` returns `Vec<GuiAction>`;
  `App::process_gui_actions` dispatches them.
* **Contract test**: `squallar-app/src/app/gui_action_replay_tests.rs`,
  `a_scripted_action_batch_lands_through_the_seam` — a scripted batch driven
  through both directions of the seam.

### 3.4 `SourceHandler` — the layer contract

* **Owner**: `squallar-source/src/handler.rs`. Fifty-eight methods; most
  defaulted. `RadarSource` (`squallar-radar/src/source.rs`) overrides
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
* **Composition**: `squallar-egui/src/sources.rs::all()` chains
  `squallar_overlays::render::handlers::sources()` with
  `squallar_radar::source::sources()`. That is the only composition.

#### 3.4.1 When a fact becomes a contract method, and when it stays an id check

This rule was enforced consistently for a year before it was written down. It
lived in six scattered comments, and the cost of that was a design round in
which the rule had to be reconstructed from them before a question could be
answered. Cite this section instead of re-deriving it.

**A fact becomes a contract method** when it is a *declaration about the layer's
own data* that the shell would otherwise re-spell. The re-spelling is the harm:
it creates a second authority to keep in step. `loop_start_frame`
(`squallar-app/src/app_render.rs`) states it plainly — *"Whether stamps later
than the wall clock are expected is the layer's own answer… A `match` on the
layer id here would be a second authority to keep in step."* The same sentence
appears at `arm_layer_loop`, `rail_regions` (`ui_timeline.rs`),
`loop_span_secs_for` (`gui/sync.rs`) and `comes_in_stamped_frames` (`pane.rs`).
`time_axis`, `frame_horizon`, `min_loop_frames`, `latest_at` and `residency_for`
are all this shape.

**A `match` on a concrete layer id stays** when the thing it names is one of:

* **(a) pane state the shell owns and the handler cannot see.** `scan_info` is a
  public field on `PaneState`, written by five different `Gui` events, and two
  panes on one site may legitimately hold different volumes. It is not reachable
  through `PaneRef`. `layer_removal_refusal` (`pane.rs`) names the radar id for
  the same reason: *"the radar slot is not just another layer here: its `config`
  is where this pane keeps its own site, product, elevation and live-chunk
  switch."*
* **(b) storage that still lives above the handler.** `RadarSource`'s
  `frames_resident`, `retain_frames` and `apply_frame` are **written-out honest
  empties with a named work item (WO-M12d)**, never silent defaults — and a
  conformance walk over them must *name radar explicitly* rather than let an
  empty answer read as agreement. The residency and render-dispatch forks in
  `app_render.rs`, and `App::evict_unneeded_loop_scans`, are the same case.

**Worked example — `window_end`, proposed and declined 2026-08-23.**
`arm_layer_loop` ends radar's backward window at the displayed volume's
timestamp rather than at the wall clock, and no contract method expresses that.
Adding `FrameSource::window_end` was rejected on four grounds, recorded here so
it is not re-proposed without new facts:

1. **It would delete no id check.** The surviving arm yields *two* values — the
   site, which is the timeline's geometry anchor, and the timestamp. A
   `window_end` takes the timestamp only; the arm stays for the site. The
   `None if layer == RADAR => return None` arm is a **geometry** refusal (without
   a scan the timeline gets a placeholder anchor, `radar_layer::site` answers
   `""`, and the loop sits in `FetchingScanList` for ever drawing nothing), so it
   stays too. Fourteen implementors would change and nothing would be removed.
2. **Radar could not answer honestly** — criterion (a) above.
3. **The conformance law would be vacuous.** Three of the four framed layers
   answer `now`; the one that differs cannot answer at all. That is exactly what
   `time_contract_tests.rs`'s floors exist to reject.
4. `frame_horizon` **is** the forward half of this idea, and the asymmetry is
   real — but an asymmetry is not itself a defect.

What would change the ruling: a **second** layer whose window ends at something
other than `now`, or `scan_info` becoming reachable through `PaneRef`. Note the
genuinely unexpressed fact next door — GMGSI's newest frame is structurally ~40
minutes behind the wall clock (measured 34–42) and MRMS's ~2 minutes — which is a
declaration about the layer's own data and would make a **non-vacuous** method.
That is a different question from this one.

### 3.5 `JobCodec` / the job funnel

* **Owner**: `squallar-source/src/job.rs` (`JobInput`, `JobOut`, `DescribedJob`,
  `DescribedOut`, `JobGeometry`, `JobCost`, `JobCodec`, `JobSpec`,
  `JobOutCodec`); funnel in `squallar-worker/src/offload.rs`; composition in
  `squallar-worker/src/job_registry.rs`.
* **The pin**: `squallar-app/tests/arch_ratchets.rs::offload_names_zero_source_crate_types`
  — `offload.rs` names **zero** `squallar_overlays::` or `squallar_radar::` paths,
  in either direction, prose included, with a presence control on
  `job_registry.rs` so a rotted needle cannot leave the zero green over
  anything.

### 3.6 `KvStore` — configuration persistence

* **Owner**: `squallar-kv/src/lib.rs`. String keys, string blobs, never
  load-bearing: a backend that cannot tell "absent" from "unreadable" answers
  `None` for both and the caller falls back to defaults.
* Keys are logical names, not paths — a filesystem backend maps a key onto a
  filename itself, the web build lands in `localStorage`, tests hold everything
  in memory. **The key strings are on-disk compatibility**: a changed string
  silently orphans every config an existing install has saved. Each constant
  lives beside its owner.
* **Pin**: `squallar-kv/tests/charter.rs::the_contract_is_three_methods_and_nothing_more`.

### 3.7 `VolumePainter` → `VolumeCapable`

* **`VolumePainter`** (`squallar-egui/src/volume_view.rs`) is what the UI is
  handed so a 3D pane can draw; it is installed through
  `GuiEvent::VolumePainter` and can be taken away again.
* **`VolumeCapable`** (`squallar-source/src/volume.rs`) is the layer-side half: a
  handler that can build a volume answers `SourceHandler::volume()` with one,
  and shapes its own job through the required, undefaulted `volume_job`.
* **Pin**: `squallar-app/tests/arch_ratchets.rs::the_radar_geometry_type_is_defined_in_radar_and_not_in_egui`
  keeps the radar-shaped half of this out of the presentation crate.

### 3.8 `AsyncTileSource` — map tiles

* **Owner**: `squallar-egui/src/tile_source.rs`. A blanket-implemented marker
  splitting the `Send + Sync` host bound from the single-threaded wasm one, so
  the same tile machinery compiles on both.
* The base map is the self-hosted PMTiles vector archive
  (`squallar_egui::tiles::BASEMAP_ARCHIVE_URL`), rasterised client-side against
  the committed styles; labels come out of the same tile as the ground. There
  is no raster tile provider — an unreachable archive draws nothing and says so
  in the credit corner.

---

## 4. Binding runtime rules

**The frame thread does no heavy work.** Long-running, CPU-bound work goes
through `squallar_worker::offload::offload_job`, which posts to a worker if this
thread has a sink and otherwise runs it here. "It runs rarely" is not an
exception. Freeing large payloads goes through `offload::discard` from the frame
thread, because the deferred queue is thread-local.

**Interaction is realtime; data may lag.** Layers and volumes may take time to
arrive, and minimising that is worth work — but map movement, controls and UI
stay realtime. Never trade interaction latency for data latency.

**Reopen is 1:1.** UI state persists so that reopening the app is visually
identical to closing it. "That's session posture" is not a reason not to
persist. Units and formatting go through `squallar-units`.

**Every option is expressed.** Every option any model offers must be reachable
and *drawn* through the real chrome on every width class. Enforced by
`squallar-egui/src/parity_walk.rs` —
`every_option_is_reachable_on_a_compact_screen`,
`..._on_a_medium_screen`, `..._on_an_expanded_screen` — which drives the real
input harness and records an item as reachable only when its centre lands
inside the screen rect. A control may be **disabled with a stated reason**; it
may not be absent. The walk derives its layer and field inventories from the
live registry, so a registered layer is walked by construction:

```bash
cargo test -p squallar-egui parity_walk                    # 3/3, fifteen layers
```

**The web target is measured, never inferred.** A scaled or extrapolated figure
is not acceptable evidence about the browser. And be precise about what a green
browser gate proves:

> **No gate has ever executed a frame of the web build.** There is no
> `wasm-bindgen-test` in this workspace; the CI wasm rows are build and check.
> Two gaps were measured at WO-M13a and both still hold: the stock Tier-2 scene
> **enables no texture overlay**, so those legs prove boot, render and the
> worker wire and nothing about the overlay arrival path; and `squallar-web`
> initialises `console_log` at `Level::Info`, so a `debug!` line can **never**
> appear in a browser. **A green Tier-2 is evidence about the scene Tier-2
> runs.** A change whose subject that scene does not exercise must say so and
> measure it another way.

**"Web" is two targets.** Firefox is first-class and governs over Chrome.
Measure both separately and never merge the figures.

**WebGPU, falling back to WebGL2.** `squallar_app::app` asks for
`Backends::BROWSER_WEBGPU | GL` on wasm32; `squallar-gpu/Cargo.toml` compiles
both browser backends, and neither is optional. This is about **adapter
coverage, not throughput** — WebGPU recovers nothing on the one measured
platform loss (WebGL2 has no `MAPPABLE_PRIMARY_BUFFERS`, so uploads run at
2.1 GB/s against 24.7 native, and that mechanism is native-only in wgpu by
construction). What it reaches is a browser with **no WebGL2 to give**.

Measured 2026-08-22 with `run_gpu_arm.sh`, one box, one browser, one
configuration — Chromium 151 under `--use-angle=vulkan --enable-unsafe-webgpu`,
which has no WebGL context at all:

| build | selected backend | canvas | distinct px | rig errors |
| --- | --- | --- | --- | --- |
| before (`Backends::GL`) | none — no adapter found | 300×150, unconfigured | 1 | 2 |
| after | `BrowserWebGpu` | 1248×714 | 2020 | 0 |

Everything else measured selects `Gl` and is unchanged: Firefox/Linux on a real
driver (32768 px), Firefox on Xvfb/llvmpipe (16384), Chromium headless on
SwiftShader (8192), Chromium headed on the RTX 3090 (32768). Do not quote a cap
without its arm and its renderer.

> **Asking for both is not choosing between them.** wgpu binds a browser API
> when the *instance* is built, not when an adapter is found: `Instance::new`
> takes the WebGPU context whenever `navigator.gpu` merely *exists*, and a
> browser that exposes that object can still answer `requestAdapter()` with
> null. And the choice is permanent once a surface exists — `create_surface` on
> the WebGPU backend calls `canvas.getContext("webgpu")`, and a canvas that has
> answered one `getContext` never answers another with a different id. So the
> instance is built by wgpu's **detecting** constructor, which issues the
> adapter request first and drops `BROWSER_WEBGPU` from the mask when it comes
> back empty. `App::create_instance` carries the mechanism; that is also why
> `App::new` builds no instance at all — the decision needs an `await`.

**Limits are requested, not accepted.** A WebGPU device gets
`max_texture_dimension_2d` 8192 unless it asks for more, which is *below*
Firefox's WebGL2 32768. `squallar_gpu::device::device_limits` starts from the
WebGL2 downlevel floor and lifts the resolution to the adapter's own report on
both browser APIs, so enabling WebGPU cannot lower the ceiling.

Firefox governs here and ships stable WebGPU on Windows (141, 2025-07-22) and
macOS (145 on Apple Silicon / macOS 26+, 147 on all versions); **Linux and
Android are still unshipped**, with Mozilla expecting Linux during 2026 (W3C
gpuweb Implementation Status). On the platform this is developed and gated on
the feature therefore does nothing at all: Firefox/Linux exposes no
`navigator.gpu` for the detection to find, and every Tier-2 leg selects `Gl`.
Keeping that true is the fallback's whole job, and the rig checks it by reading
the backend the app logs at startup rather than by assuming. With the pref
forced on (`--ff-pref dom.webgpu.enabled=true`) the same build selects
`BrowserWebGpu` and reaches 32767 px 2D textures against WebGL2's 32768 — a
one-texel difference, and the evidence that the second path runs rather than
merely compiles. It does double the paths under test:
`squallar-gpu/Cargo.toml` is the one feature-chooser for the whole graph, and
`squallar-gpu/src/lib.rs` fails the build if either browser backend goes missing.
Read those comments before touching wgpu features anywhere.

**Generation counters guard against stale results.** `RenderDispatch`
(`squallar-app/src/render_dispatch.rs`) keeps a per-site `fetch_generations` map
and one `render_generation`; take the next generation before spawning
(`next_fetch_generation`), and discard a result whose generation is below the
current one (`is_fetch_stale`, `is_render_stale`).

---

## 5. Adding a source

A source is a layer. Adding one is **one handler file plus three registration
lines**, and nothing outside `squallar-overlays` (or `squallar-radar`, for a
radar-shaped source).

1. **The handler file** — `squallar-overlays/src/render/handlers/<name>.rs`,
   implementing `SourceHandler`. Follow `outlook.rs` or `alert.rs` for a
   polygon/texture layer, `metar.rs` for a per-frame point layer,
   `location.rs`/`colorscale.rs` for a per-frame direct layer.
2. **`squallar-source/src/id.rs`** — a `known::` const.
3. **`squallar-source/src/id.rs`** — the matching `LAYER_ID_LEDGER` entry.
4. **`squallar-overlays/src/render/handlers/mod.rs`** — one row in `sources()`,
   which is the only place these registrations are named.

Then bump the two hand-kept second spellings in `squallar-egui/src/sources.rs`:
`REGISTERED_LAYER_COUNT` and, if the source registers fields,
`REGISTERED_FIELD_COUNT`. **Never derive either from `all()`, `sources()` or
`LAYER_ID_LEDGER::len()`** — a floor computed from the thing it floors compares
the registry against itself and cannot fail.

**Nothing proves this checklist by construction.** Until 2026-08-22 a
test-only `fake-source` layer carried an acceptance suite
(`squallar-app/tests/fake_source_acceptance.rs` and the UI half in
`squallar-egui`) whose footprint pins asserted, file by file, that a new source
cost no edit outside `squallar-overlays`. The layer and the suite were deleted
together and **no gate replaced them**. The checklist above is now design
intent held by review.

What is left is evidence rather than a gate, and it is weaker than the deleted
suite claimed. Three real sources landed in August 2026 — SPC Fire Weather
Outlooks (`23df4d92`), the MRMS national mosaic (`4cf3dd7f`) and the GMGSI
global mosaic (`93e8606d`). Each kept all of its *behaviour* inside
`squallar-overlays`, added no channel, and — because both gridded layers ride
`GriddedInput` — added no codec row. That is real evidence the seams hold.

**Be precise about what the fake ever proved.** A texture layer that renders
through the job funnel needs one arm outside its own crate: the described-kinds
match in `App::spawn_overlay_render` (`squallar-app/src/app_fetch.rs`), which
names every such layer by `known::` const. All three real sources above added a
line to it. The fake escaped that arm only because it fell into the match's
fallback branch — a build that registered it logged
`spawn_overlay_render reached the dispatch with a layer it cannot rasterize`
and drew nothing through the funnel; its draw test wrote a texture straight
into the pane's overlay cache instead. So "zero edits outside its crate" held
for the fake and has never held for a real texture source. Treat a *new kind*
of arm as the signal that a seam is incomplete; the registration tax and the
`spawn_overlay_render` row are the known, expected cost.

Radar registers itself: `squallar_radar::source::sources()`, chained by
`squallar_egui::sources::all()`. A new source **never adds a channel** — the
`ChannelHub`'s generic seams (`overlay_render`, `voxel`) already carry it.

---

## 6. The ratchet index

Architecture is held by counted ceilings. **A ceiling only ever falls.** Lower
the pin in the land that earns it; **never raise one without a written plan
amendment**. Never hide a needle instead of shedding the coupling — the
`let gui = &mut self.gui;` re-spelling is **forbidden by name**, because it
makes the walker read zero while the coupling is identical.

**The rule**: a ceiling equals the count it measures, and the land that sheds
an occurrence lowers the constant with it. A value below is therefore never a
standing promise — it is the last measurement that satisfied the rule. Values
here were re-measured at **WO-ARREARS, 2026-08-21, base `178ab361`**, each by
its own instrument (the crate-wide walk in `arch_ratchets.rs`; the
whitespace-collapsed per-file scrape in `gui_seam_ratchet_tests.rs` — they are
different instruments and one's number is never the other's).

### 6.1 `squallar-app/tests/arch_ratchets.rs` — 10 tests

| Test | Constant | Ceiling | What it counts |
|---|---|---|---|
| `the_app_pokes_gui_coupling_never_grows` | `SELF_GUI_MAX` | 181 | `self.gui.` anywhere in `squallar-app` |
| | `SELF_GUI_NON_TEST_MAX` | 176 | the same, outside test-named paths |
| | — | 0 | `self.gui.set_` anywhere in `squallar-app` — the target zero, held as a test rather than as a grep |
| `the_config_swap_stays_deleted` | — | 0 | `load_pane_configs` / `save_pane_configs` / `loaded_configs`, with `serialize_pane_state` as the presence control |
| `the_gui_setter_surface_never_grows` | `UI_SETTER_MAX` | 0 | `pub fn set_` in `squallar-egui/src/ui.rs` |
| | `GUI_IMPL_SETTER_MAX` | 1 | the same needle over **every** inherent `impl Gui` block in `squallar-egui` |
| `the_product_enum_never_spreads_further_into_egui` | `PRODUCT_IN_EGUI_MAX` | 0 | `RadarProduct` anywhere under `squallar-egui`, **comments included** |
| `the_channel_hub_never_grows_past_eighteen_receiver_pairs` | `HUB_RECEIVER_MAX` | 17 | `_receiver: Receiver<` fields on `ChannelHub` |
| `offload_names_zero_source_crate_types` | — | 0 | `squallar_overlays::` / `squallar_radar::` in `squallar-worker/src/offload.rs` |
| `the_radar_geometry_type_is_defined_in_radar_and_not_in_egui` | — | 1 / 0 | `struct LoopGeometry` in radar / in egui |
| `the_loop_frame_arms_stay_radars_own_vocabulary` | `LOOP_FRAME_ARMS_MAX` | 8 | the loop frame's closed arms and their two cache aliases |
| | `LOOP_FRAME_ARMS_NON_TEST_MAX` | 2 | the same, outside tests |
| `the_ingest_phase_has_exactly_one_production_caller` | `INGEST_CALLERS_NON_TEST` | 1 | `self.poll_data_channels(` outside test paths — the frame pump's `Ingest` phase, whose once-per-frame property WO-M13b measured and registered as unpinned |
| `the_site_keyed_volume_stores_have_one_owner` | — | 0 / 0 | the two site-keyed decoded-volume store names as fields of `pub struct App`, with the third such store (deliberately left on the `App`) and the `volumes` field as two presence controls read from the same extracted block |

Run it by target name — a filter matching zero tests is a failed run, not a
pass:

```bash
cargo test -p squallar-app --test arch_ratchets   # 10/10
```

### 6.2 `squallar-app/src/app/gui_seam_ratchet_tests.rs` — the per-file coupling ceilings

| Test | File | Ceiling |
|---|---|---|
| `no_production_file_pushes_through_a_gui_setter` | `app.rs`, `app_fetch.rs`, `app_render.rs`, `app_chunks.rs` | 0 `self.gui.set_` each |
| `the_gui_coupling_only_ever_shrinks` | `app.rs` | 37 |
| | `app_fetch.rs` | 42 |
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

**The standing rule is that a ceiling equals the count it measures**, so under
a permanent ceiling the correct response to needing a new reach is **shed
first, then land** — not "ask whether the ceiling should move".

**That is a rule, not a property the tree keeps on its own.** A land that sheds
an occurrence and does not lower the pin leaves **arrears**: unearned slack a
later land can spend with nothing going red. It has happened twice on the
record — WO-E9c left `PRODUCT_IN_EGUI_MAX` 34 above its haystack, and
`1e94ce59` left three ceilings above theirs between WO-E10.4 and WO-ARREARS.
So the honest form of the claim is dated: **as re-measured at WO-ARREARS
(2026-08-21, base `178ab361`) every ceiling in §6.1 and §6.2 sits on its
measured value.** Checking that is a `git diff` of the constants against a
fresh measurement, not a sentence to trust.

The two honest sheds are:

* **loop-state addressing** — the vocabulary by which the app reaches loop state
  held on the `Gui`; and
* **the all-panes-versus-visible-panes distinction**, which has already produced
  one bug.

If neither is reachable inside a change's charter, **stop and report**. Do not
raise, and do not re-spell.

**The tree currently contains the forbidden re-spelling twice**, and the
ceilings above are the number the walk sees, not the coupling the crate has.
`App::poll_overlay_fetch_results` (`app.rs`) hides 4 reaches behind one
`let gui = &mut self.gui;` and `App::poll_overlay_render_results`
(`app_render.rs`) hides 1. WO-ARREARS **compile-proved neither is
borrow-forced** — a direct reach builds with no diagnostic in either case — so
they are evasion, not borrow-splitting. Shedding them makes the crate-wide walk
read **186** against the 181 it reads today — five above the ceiling, and above
both values it has carried since WO-E9e (185, then 184). (It would have fitted
under the 188 the ceiling carried earlier in the campaign; that headroom was
spent down deliberately and is not available to reclaim.) The shed therefore
needs a land that can also shed the difference, and it was **refused rather
than paid for with a raise**. The full record lives on `SELF_GUI_MAX`'s own doc. A third such
binding has no standing: these two are documented because they were found and
measured, not because the construct is allowed.

### 6.4 Other standing pins

* **Dependency ceilings** — the nine `tests/charter.rs` files of §1.
* **Cited-test resolution** — `squallar-radar/tests/doc_citations_resolve.rs`
  scans **every `//`, `///` and `//!` comment in the workspace** and requires
  any backticked name whose final `::` segment is snake_case with **five or
  more** underscore-separated segments to resolve to a real `fn` or `mod`, and
  requires a citation to an `#[ignore]`d test to say that it is ignored.
  **Do not name a test in a comment unless it exists.** Genuine non-defects go
  in that file's `ALLOWED` table *with the reason*.
* **One geodesy definition** — `squallar-radar/tests/geodesy_one_definition.rs`.
* **No release-artifact row asks for a test-only feature** —
  `squallar/src/release_artifact_features.rs` parses `.github/workflows/build.yaml`'s
  build matrix and, for every row carrying an `artifact:` key, asserts that the
  feature **set** that row's cargo command requests contains no test-only
  feature. `--all-features` fails it because it expands to every feature the
  member manifests declare — the property, not the spelling. It carries two
  presence controls (the four desktop rows are found by name with a `cmd:`
  each; every needle names a feature some manifest declares). Its module docs
  record the one residual it deliberately does **not** assert: `--all-targets`
  unifies `squallar-radar/test-support` into the shipped lib unit, measured, and
  unfolding the artifact build from the coverage build is not a change a gate
  should force.
* **On-screen strings are ASCII unless the glyph is registered** —
  `squallar-egui/src/ui_glyphs.rs` holds the one inventory of icon and text
  glyphs, verified against the fonts egui actually bundles, and
  `ui_string_literals_use_only_registered_glyphs` scans every string literal in
  `squallar-egui`, `squallar-overlays`, `squallar-app` and `squallar-volumetric`
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
| `squallar-device-profile` | 70 | | `squallar-overlays` | 21 |
| `squallar-radar` | 44 | | `squallar-gpu` | 18 |
| `squallar-app` | 41 | | `squallar-location` | 17 |
| `squallar-egui` | 39 | | `squallar-source` | 17 |
| `squallar-worker` | 26 | | `squallar-web` | 14 |
| | | | `squallar-volumetric` | 12 |
| | | | `squallar-geo`, `squallar-kv`, `squallar-nmea-serial` | 1 each |

**Total 322 across 14 crates.** The plan's older baseline (`165/54/40/30/25,
Σ314`) is **not comparable** and should not be read as a rise of 8: it was taken
over five pre-reshape crates, and the Phase-3R/3F dissolution re-partitioned that
same code across fourteen. Comparing them would be a figure without its
denominator.

---

## 7. Before landing

Carried verbatim from the repository instructions this file replaced
(`.github/copilot-instructions.md`, since deleted) and re-verified against
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
- **The wasm32 row goes before clippy**, as it does in CI: it sits several steps ahead of `Run Clippy` in the same job, so a break there aborts the job and hides every lint finding behind it. It is also the only build that type-checks `squallar-web` — the crate's deps are under `[target.'cfg(target_arch = "wasm32")'.dependencies]`, so every other target compiles it as an empty shell and reports success.
- **The other two gates are not cheap**, and CI is the right place for them: the host build (`cargo build --workspace --all-targets --all-features`) and the Android arm (`cargo ndk -t arm64-v8a -P <minSdk> check -p squallar --lib`). Test runs `cargo llvm-cov --no-report --all-features` in its own workflow.
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
