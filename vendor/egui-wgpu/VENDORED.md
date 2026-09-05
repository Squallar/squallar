# egui-wgpu, vendored

This directory is a copy of a crates.io crate that this workspace maintains
locally. It is not our code. Everything in it is upstream's except what is
listed under [Local changes](#local-changes) below, and keeping that list short
and true is the whole point of the file — it is what makes the directory
reviewable, and what lets a later upstream release be adopted by re-applying a
delta somebody can still read.

It is the eighth such directory, after `vendor/nexrad-decode`,
`vendor/nexrad-data`, `vendor/bzip2-rs`, `vendor/walkers`, `vendor/nexrad-model`,
`vendor/pmtiles` and `vendor/mvt-reader`, and it follows their shape
deliberately.

**These changes are not going upstream.** That is this workspace's standing
stance, taken for `vendor/nexrad-decode` on 2026-08-12 and applied here on
2026-09-02. No pull request will be filed.

## Provenance

| Field | Value |
| --- | --- |
| Package | `egui-wgpu` |
| Version | `0.35.0` — the version this workspace already pinned (`=0.35.0`) |
| Source | crates.io, `sha256:d2e6cfac0725563555fa4f91e9f799b9d7c6c5dd831fca6abc8234afc64b7a34` |
| Unpacked from | `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/egui-wgpu-0.35.0` |
| Upstream repo | <https://github.com/emilk/egui/tree/main/crates/egui-wgpu> |
| Upstream commit | `6f15dc0e16b26edce1fc2a05212eaf7e749c1d05` (from the tarball's `.cargo_vcs_info.json`) |
| Authors | Nils Hasenbanck, embotech, Emil Ernerfeldt |
| License | MIT **OR** Apache-2.0, as declared in the manifest |

The checksum above is of the tarball in the local registry cache, taken before
extraction.

**No licence text ships with this crate**, and that is upstream's packaging
rather than an omission here: the manifest's `include` list names
`../../LICENSE-APACHE` and `../../LICENSE-MIT`, which are outside the package
root, so cargo packaged neither. Every other vendored directory here carries the
text it was shipped. This one carries the SPDX expression from the manifest and
nothing more; writing a copyright line nobody shipped would be worse than
saying so.

## Why this exists

Every frame's index and vertex arrays reach the GPU through
`Renderer::update_buffers`, which stages both through
`wgpu::Queue::write_buffer_with`. That hands back a mapping of a
`MAP_WRITE | COPY_SRC` buffer (`wgpu-core`'s `StagingBuffer::new`,
`resource.rs`), and `wgpu-hal`'s Vulkan backend maps that usage pair to
`gpu_allocator`'s `MemoryLocation::CpuToGpu` (`vulkan/device.rs`, `is_cpu_write`)
— on a discrete card, the host-visible BAR window. Every host store is a write
down the PCIe link and the frame thread stands and waits for the last one.

Measured on this box, RTX 3090 / Vulkan, 1 MiB to 30 MiB in 1 to 5000 chunks,
minimum of five runs
(`squallar-gpu/tests/geometry_staging_gpu.rs::what_each_staging_route_costs_per_byte`):

| bytes | chunks | `write_buffer_with` mapping | `MAP_READ` ring slot |
| --- | --- | --- | --- |
| 1 MiB | 1 | 2.15 GB/s | 26.9 GB/s |
| 1 MiB | 5000 | 1.90 GB/s | 7.71 GB/s |
| 8 MiB | 1 | 2.15 GB/s | 51.2 GB/s |
| 8 MiB | 5000 | 1.89 GB/s | 13.4 GB/s |
| 30 MiB | 1 | 2.15 GB/s | 23.8 GB/s |
| 30 MiB | 5000 | 2.07 GB/s | 17.9 GB/s |

The two columns are read differently. The mapping's is stable to the second
decimal across every run taken — 2.15 GB/s, whatever the size and whatever the
chunking — and *that* is what says the BAR route is paying for **bytes** and not
for memcpy calls. The ring's is not stable: it ranged 7.7 to 65.5 GB/s over
three runs on a box with other work on it, because a cached-RAM memcpy competes
for the cache and the memory controller. The table gives an order of magnitude,
not a number.

And what that is worth to the function itself, over a run of frames rather than
one staging
(`…::what_a_run_of_frames_costs_through_each_route`): 60 consecutive
`update_buffers` calls at 13.75 MB a frame, three runs at different box loads —

| | run 1 | run 2 | run 3 (quiet box) |
| --- | --- | --- | --- |
| BAR mapping | 7 800 µs/frame | 7 766 µs/frame | 7 657 µs/frame |
| ring | 1 816 µs/frame | 1 626 µs/frame | 945 µs/frame |
| staged / declined | 60 / 0 | 60 / 0 | 60 / 0 |

**A 4.2× cut at worst and 8.1× at best.** The spreads are the reading: the BAR
side moves 1.9% and the ring side 1.9×, and all of the ring's is the box —
a cached-RAM memcpy competes for cache and memory bandwidth, a BAR write is
gated by the link and does not. Plan with the 4.2×; a quiet machine gets the
8.1×.

`60 / 0` in every run is the part a per-byte table cannot say. A ring slot
returns through `map_async`, which resolves only once the copy reading it has
drained, and at a depth of two a run of frames could outpace it. It does not.

`squallar-gpu` already escaped this for **textures** in August 2026
(`squallar-gpu/src/staging_ring.rs`, whose module note carries the original
32 MiB reading). Geometry never did: same function, same thread, same frame,
an order of magnitude apart per byte.

### Ruled out before vendoring

* **Patch `wgpu-core` instead.** The BAR placement is decided in
  `StagingBuffer::new`, which hardcodes `BufferUses::MAP_WRITE | COPY_SRC` for
  *every* `write_buffer`, `write_texture` and `write_buffer_with` in the
  process. Vendoring wgpu-core would put a much larger, much hotter crate under
  local maintenance to change one line that every other caller also depends on.
* **Reduce the bytes rather than the cost per byte.** Worth checking, and it
  does not answer. Differencing the `frame prep geometry:` running totals in
  `.github/browser-rig/out-native/A.before.r1/app.log` (2026-09-02 04:35),
  the heaviest two-second window staged **32.6 MB and 928 000 vertices per
  `update_buffers` call** — after the basemap fills and strokes had already
  moved to the GPU. What is left is genuine frame content, and at the ring's
  rate 30 MB costs 1.4 ms rather than 14.6 ms: the byte volume stops being the
  problem before it stops being large.

  **That log no longer exists.** `out-native/` is gitignored and was removed
  from the shared checkout by another session while this work was in progress,
  so the two figures above are quoted from a reading that cannot now be
  re-taken. Nothing in this directory rests on them — they set
  `MAX_STAGED_GEOMETRY_BYTES` and `GEOMETRY_SLOT_GRANULARITY`, both of which
  degrade gracefully if they are wrong, and both of which report their own
  misses through `GeometryStagingTotals::declined`.
* **Make the vertex and index buffers themselves host-visible.**
  `VERTEX | MAP_READ` would put them in cached system RAM and remove the copy
  entirely, but then the GPU fetches every vertex attribute across PCIe out of
  snooped host memory at draw time. That trades a measurable frame-thread cost
  for an unmeasurable GPU one, and it is the exact footgun wgpu's
  `MAPPABLE_PRIMARY_BUFFERS` documentation warns about on a discrete card.
* **Fork `Renderer` into `squallar-gpu` instead of vendoring the crate.** The
  known-viable route, and the one that was scoped first: 1 174 lines of
  `renderer.rs` plus `egui.wgsl`. It is strictly worse than this. The delta
  needed is the same either way, but a fork also re-homes `Callback`,
  `CallbackTrait`, `CallbackResources` and `ScreenDescriptor`, which are
  `renderer.rs` types that `squallar-gpu/src/tile_mesh.rs`,
  `squallar-gpu/src/gpu_probe.rs`, `squallar-egui/src/volume_view.rs`,
  `squallar-volumetric` and four GPU test suites all name — and it leaves 1 174
  lines whose provenance nothing records. Vendoring copies more lines and
  changes fewer: every caller compiles untouched, and this file is the record.
* **A staging module private to this crate, duplicating the ring.** Rejected
  because the ring's heap claim is the load-bearing one, and a second copy of
  it is a second thing to keep true. The seam below lets `squallar-gpu` keep
  the one `staging_ring.rs` it already had and already probed.

## Local changes

### Changed — `Cargo.toml`, the default feature set

`default` is emptied. Upstream's is
`["fragile-send-sync-non-atomic-wasm", "macos-window-resize-jitter-fix",
"wgpu/default", "wgpu/webgl"]`, and this is the change with the widest blast
radius in the directory, so it is first:

**A vendored crate is a workspace member, and `cargo …--workspace` builds every
member as a root package with its own default features on, whatever its
dependents asked for.** The workspace pins this crate `default-features =
false` and every dependent names what it wants per target; membership silently
overrode all of that. Measured on the tree at the moment of vendoring:
`wgpu/default` and `wgpu/metal` arrived in every target's graph, and on wasm32
`fragile-send-sync-non-atomic-wasm` came on — whose `Send + Sync` grant is
conditional on `not(target_feature = "atomics")`, while this workspace's browser
build carries `+atomics`. Upstream's own `renderer_impl_send_sync` then compiled
and could not hold: **199 errors on CI's wasm row**, none of them in code
anybody here wrote.

Invisible to every dependent, which is what makes it safe: `squallar-gpu` and
`squallar-app` name `winit` and `macos-window-resize-jitter-fix` themselves, and
`squallar-gpu` names wgpu's backend features directly in its own manifest.

### Changed — `src/lib.rs`, one `cfg` on `wgpu_config_impl_send_sync`

The guard upstream put on the sibling assertion in `renderer.rs` and not on
this one. `WgpuConfiguration` holds `wgpu` handles, so the assertion cannot hold
on wasm32 without `fragile-send-sync-non-atomic-wasm` — 51 of the 199 errors
above survived emptying the default set, and they were all this line. Nobody
upstream sees it because nobody builds this crate's test target for wasm32.

### Changed — `Cargo.toml`, the lint tables

Upstream's `[lints]` — roughly two hundred clippy lints at `warn`, plus four
`[lints.rust]` groups — is replaced by the permissive pair every other vendored
directory here carries, for the reason spelled out in
`vendor/nexrad-decode/Cargo.toml`: this workspace's clippy job runs
`--fix` across every member and pushes the result, and a vendored crate under
that bot is an invitation to rewrite upstream source. The manifest's own comment
names the representative case — upstream asks for `clippy::empty_enum`, which
rustc has renamed, so the *unmodified* copy emits a warning that `-D warnings`
turns into a failed row on a line nobody here wrote.

One entry is not upstream's and not merely a silencing: `unfulfilled_lint_expectations
= "allow"`. Upstream's source carries `#[expect(clippy::unwrap_used)]` twice,
and `unwrap_used` is a restriction lint the table above no longer turns on, so
both expectations are now unfulfilled — which is itself a lint, and one
`-D warnings` fails on. Allowed rather than answered by editing upstream's
attributes away.

Nothing else in the manifest is touched: same version, same dependencies.

### Added — `src/renderer.rs`, the `GeometryStager` seam

Three items, all new, none replacing anything:

* `pub trait GeometryStager`, whose one method is handed the two destination
  buffers with the byte count each needs and a closure that fills a single
  contiguous region — indices at 0, vertices after them. Returning `false`
  means it declined and nothing was written.
* `pub type BoxedGeometryStager`, cfg-selected exactly the way upstream selects
  `CallbackResources`, so that a `Renderer` carrying a stager stays as
  `Send + Sync` as it was without one. Upstream's own
  `renderer_impl_send_sync` is what enforces that, and is what rejected the
  first spelling of this field.
* `Renderer::set_geometry_stager`, and the `geometry_stager: Option<..>` field
  it writes. `None` is the default and is upstream's behaviour exactly.

Plus two private helpers, `meshes` and `fill_slices`, which hold the walk and
the slice arithmetic that the two routes now share.

### Changed — `src/renderer.rs`, `update_buffers`

Upstream had two blocks, one per array, each of which sized its buffer, grew it
if needed, opened a `write_buffer_with` mapping and filled the slice list inside
the copy loop. The three concerns are separated here, because a stager claims
**one** region for both arrays and so has to know both sizes before either is
written, and because the slice arithmetic is identical whichever route the bytes
take:

1. both required sizes are computed,
2. both buffers are grown — same rule, same `at_least` doubling,
3. both slice lists are filled — same contents, same order, still under
   upstream's `index_count > 0` / `vertex_count > 0` guards, so a frame with no
   indices still leaves the index slice list exactly as upstream left it,
4. the stager is offered the frame, and
5. if there is no stager, or it declined, upstream's `write_buffer_with` path
   runs — including its two `profiling::scope!`s and both of its panic
   messages verbatim.

`Renderer::render` was untouched by this change; the two sections below are
what later changed in it.

## The pin

`egui-wgpu` publishes no test target (`autotests = false`) and its `src/` holds
two plain `#[test]` fns, both one-line `Send + Sync` marker assertions.
Measured: `cargo test -p egui-wgpu` selects **2 unit tests and 0 doctests**.
That is a thinner inherited pin than the other seven directories have, so the
real one is written outside:

* `squallar-gpu/tests/geometry_staging_gpu.rs` renders one 288-primitive,
  16 992-vertex scene twice on a real adapter — once with a stager, once
  without — and asserts the readbacks differ by **zero bytes**, that the ring
  moved exactly the bytes the primitive list has, and that the ring was
  **actually taken**. The third is what keeps the first two from passing
  vacuously.
* `squallar-gpu/tests/geometry_staging_gpu.rs::the_ring_and_the_queue_allocate_out_of_different_heaps`
  reads the physical device's memory types through `Device::as_hal` and
  replicates `gpu_allocator`'s own selection, so "these bytes no longer cross
  the BAR" is a checked property of the driver's heaps rather than an inference
  from the usage flags passed in.
* `squallar-gpu/src/egui_renderer/tests.rs::a_new_renderer_installs_the_geometry_stager`
  is the source-level pin on the one line neither of those covers: the install
  inside `EguiRenderer::new`, which needs a window and an event loop.

### Non-vacuity, measured 2026-09-02

Three tampers, each applied to the tree and reverted:

| Tamper | Result |
| --- | --- |
| `stage` returns `false` immediately — i.e. the pre-vendoring behaviour | RED: `staged 0 time(s)`, expected 1 |
| vertex base offset shifted by one `Vertex` | RED (out-of-bounds slice) |
| only the first three meshes' vertices written | RED: 516 660 of 1 048 576 readback bytes differ |

The third is why the scene gives every cell its own clip rect. On the first
draft the whole picture tessellated to **one** primitive, and that tamper passed.

### Changed — `src/renderer.rs`, what `Renderer::render` records

Two edits inside the primitive walk, plus `Clone, Copy, PartialEq, Eq` on the
private `ScissorRect` so one of them can compare. Nothing about which pixels are
drawn changes; both remove render-pass calls whose effect was already in force.

* **The state reset is deferred to the next primitive that draws with it.**
  Upstream re-establishes egui's viewport, pipeline and uniform bind group at
  the top of every iteration following a painted callback — including an
  iteration that is itself a callback, and one whose clip rect is about to be
  skipped. Neither can use any of the three. egui's pipeline is undrawable
  until the mesh arm binds the vertex buffer, the index buffer and a texture
  bind group; and the viewport is overwritten, unread, by the courtesy viewport
  the callback arm sets sixty lines further down. The `if needs_reset` block
  moves into the `Primitive::Mesh` arm.
* **A scissor the pass already holds is not recorded again.** `render` now
  remembers the rect it last set. A painted callback clears that memory, in the
  same statement that raises `needs_reset`, because a callback owns the pass
  while it paints and may set a scissor of its own.

**Why the second one is not already free, when the first is next to two calls
that are.** `wgpu-core` drops a `set_bind_group` or a `set_pipeline` whose
argument already holds (`StateChange::set_and_check_redundant`, `render.rs`) —
so a redundant *bind group* costs an FFI hop and stops. It has no such check for
`set_scissor_rect`, `set_index_buffer` or `set_vertex_buffer` at either the
recording or the execution layer: each is pushed onto the pass's
`Vec<ArcRenderCommand>` and replayed into the HAL encoder unconditionally. On
the GL backend that becomes a `glScissor` executed on the frame thread at
`queue.submit`, which is where 93% of the frame tail is.

### Measured, scene D, 2026-09-04

One 1920×1080 pane, KTLX, all overlays, the `ui-sweep` gesture script, run
through `.github/browser-rig/run_measure_native.sh` on **Xvfb :99** — a display
with no active mode, which makes every *timing* on that leg invalid and says
nothing about a *count*. Backend read out of the app's own log: **Vulkan, NVIDIA
GeForce RTX 3090 (DiscreteGpu)**, hardware, no fallback.

Scene D's frames are bimodal, and the two are not averaged here. A frame
carrying the basemap issues one egui callback per tile-mesh run; a frame without
it issues none. Solving the running mean against the two, both the primitive
count and the call count give the same mixture, 26% basemap frames:

| per frame | basemap frame | frame without |
| --- | --- | --- |
| primitives | 279 (109 mesh, **170 callback**) | 73 (73 mesh, 0 callback) |
| recorded calls, before | 1399 | 369 |
| — of which reset triples | 513 (171 resets) | 3 |
| — of which repeated scissors | 228 | 67 |

`squallar-gpu`'s `command_stream` census is the instrument, and it counts what
this walk records rather than what it costs: a count is deterministic where this
arm's 340 µs noise floor makes a timing of one frame's tail unmeasurable.

### Changed — `src/renderer.rs`, the buffers are bound once per reset

Upstream's mesh arm issued a `set_index_buffer` and a `set_vertex_buffer` for
**every** drawn mesh — the same two buffers, sliced at that mesh's own offset —
and then `draw_indexed(0..n, 0, 0..1)`. Neither `wgpu` layer drops a buffer
bind whose argument already holds (see the section above), so every pair
reached the HAL. On desktop GL 4.3+ a `set_vertex_buffer` is one
`glBindVertexBuffer`; on **ES 3.0, which is WebGL2**, there is no
`VERTEX_BUFFER_LAYOUT`, the offset lives in `glVertexAttribPointer`, and the
following draw's `prepare_draw` replays one `SetVertexAttribute` per vertex
attribute — three for egui's vertex, each a `glBindBuffer`,
`glEnableVertexAttribArray`, `glVertexAttrib{I}Pointer` and
`glVertexAttribDivisor` — plus the index bind's own `glBindBuffer`. Thirteen GL
calls a mesh, on the frame thread, at `queue.submit`.

Measured before the change by apitrace (2026-09-04, native app forced to the
ES 3.0 profile with `MESA_GLES_VERSION_OVERRIDE=3.0 WGPU_GLES_MINOR_VERSION=0`,
llvmpipe, counts only): the per-mesh rebinds were **396 of a non-basemap
frame's 750 GL calls (52.8%)** and 1 100 904 of the leg's 3 253 517 (33.8%).
Every one rebound the buffer that was already bound, at a new offset.

Three edits, all inside `renderer.rs`:

* **`update_buffers` rebases each mesh's indices by its vertex base** — the
  start of its vertex slice, in vertices — as it writes them, on both routes
  (`index_writes` is the one walk both call; `write_rebased_indices` does the
  element-wise, write-only store). The vertex bytes are untouched. A
  `GeometryStager` still receives the same region of the same size; only the
  index words in it changed value.
* **`render` binds both buffers, whole, inside the `needs_reset` block** —
  once when the walk opens and once after every painted callback, which may
  have bound buffers of its own — and draws each mesh as
  `draw_indexed(first_index..first_index + n, 0, 0..1)` with `first_index` the
  mesh's index-slice start in words. `base_vertex` stays zero: WebGL2 has no
  base-vertex draw (`DownlevelFlags::BASE_VERTEX` needs ES 3.2), which is why
  the rebase is in the bytes rather than in the draw.
* `render` no longer walks the vertex slice list at all; the skip arm advances
  only the index iterator.

**The index type is `u32` and stays `u32`** (`wgpu::IndexFormat::Uint32`,
upstream's choice). A rebased index is bounded by the pass's total vertex
count, which the heaviest frame measured put at 928 000 — three orders of
magnitude under `u32::MAX` and fourteen over `u16::MAX`, so there was never a
narrower type to protect. The vertex and index buffers are each **one
allocation for the whole pass**: `update_buffers` grows them before staging and
`render` reads the same two handles, so "once per reset" is exactly that — the
staging ring is a *source* the copy engine reads from, and no mesh's bytes are
ever drawn out of a different destination buffer within one pass. There is no
wrap to count.

Pinned by `squallar-gpu/tests/egui_bind_once_gpu.rs`: 288 meshes with
distinct non-zero vertex bases, drawn by `first_index` out of buffers bound
once, read back byte-identical to the same shapes tessellated as **one** mesh
(base zero, `first_index` zero — the draw upstream made, with the rebase done
by `epaint`'s own `Mesh::append`). Its sensitivity control stages the same
picture with one mesh's indices rebased one mesh too far, through the
`GeometryStager` seam, and asserts the readback **differs**. It needs no
adapter feature and runs on a software rasteriser. The census's
`the_vendored_walks_three_rules_are_the_ones_the_census_models` grew a fourth
rule: both binds inside the reset block, none after it in the mesh arm, and a
`first_index` draw.

#### Measured, scene D, 2026-09-05

One 1920×1080 pane, KTLX, all overlays, the `ui-sweep` gesture script, on this
lane's own Xvfb display (counts only — every *timing* on such a leg is invalid
and none is quoted). Backend read out of the app's own log on both legs:
**Vulkan, NVIDIA GeForce RTX 3090 (DiscreteGpu)**, hardware. Two legs, each 62
telemetry ticks over 6 gesture loops; each tick's `command stream: last` group
is one frame's walk, bucketed by whether that frame carried the basemap
(callbacks > 0) and never averaged across the two kinds. Modes over ticks.

The `before` leg ran the pre-change binary that was in the shared checkout. The
`same frames` column is what the pre-change census would have recorded for the
**after** leg's own frames — it charged two binds per drawn mesh, by
construction — so that column and the last are the like-for-like pair; the
first is the observation that the formula is what the old binary really did.

| per frame | before, observed | same frames, per-mesh formula | after, observed |
| --- | --- | --- | --- |
| basemap frame: n (ticks) | 18 | 19 | 19 |
| basemap frame: draws / resets | 56 / 46 | 59 / 46 | 59 / 46 |
| basemap frame: **buffer binds** | **112** | **118** | **92** |
| frame without: n (ticks) | 44 | 43 | 43 |
| frame without: draws / resets | 32 / 1 | 35 / 1 | 35 / 1 |
| frame without: **buffer binds** | **64** | **70** | **2** |

Every one of the 62 before ticks read exactly `2 × draws`; every one of the 62
after ticks read exactly `2 × resets`. On a frame without the basemap the pair
is gone — 2 binds whatever the mesh count, 70 → 2. On a frame carrying the
basemap the floor is its 46 resets: a callback owns the pass while it paints,
so the first mesh after each one must rebind, and the callback/mesh
interleaving — not the bind — is what is left to cut there (118 → 92). The two
legs' *callback* counts differ (170 against 45 per basemap frame) because the
before binary is from an earlier tree whose commit was not recorded —
`058bd921` (one paint callback per tile rather than per tile-mesh run) is the
change of that size, and is not this one; the reset count, which is what the
after figure is a function of, is 46 in both.

What that is worth on the GL backend is not in these counts, which are
`wgpu-core` commands: on ES 3.0 each removed pair was thirteen GL calls at
`queue.submit`, so the frame without the basemap sheds 68 × 13 = 884 GL calls
a frame against the 750 the apitrace leg counted for the whole of such a frame
under a different layout. The apitrace re-capture that would turn that
arithmetic into an observation was not taken.

## Removing this directory



Delete `vendor/egui-wgpu/`, the `[patch.crates-io]` entry, the workspace
`members` entry and the `[profile.dev.package.egui-wgpu]` override, then delete
`squallar-gpu/src/egui_renderer/geometry_staging.rs`, its `pub mod` line, the
`set_geometry_stager` call in `EguiRenderer::new`, the
`geometry_staging_totals` accessor and its two fields on the
`frame prep geometry:` line (and the matching capture groups in
`.github/browser-rig/drive.py`). Every frame's geometry goes back through the
BAR window.

Also delete `squallar-gpu/src/egui_renderer/command_stream.rs`, its `pub mod`
line, the census call in `EguiRenderer::draw`, the two accessors and
`squallar-app`'s `frame command stream:` line: the census models *this* walk,
and against upstream's it would report resets and scissors that are recorded
again.
