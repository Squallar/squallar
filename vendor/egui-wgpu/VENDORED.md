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

`Renderer::render` is untouched, and so is everything else in the file.

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

## Removing this directory

Delete `vendor/egui-wgpu/`, the `[patch.crates-io]` entry, the workspace
`members` entry and the `[profile.dev.package.egui-wgpu]` override, then delete
`squallar-gpu/src/egui_renderer/geometry_staging.rs`, its `pub mod` line, the
`set_geometry_stager` call in `EguiRenderer::new`, the
`geometry_staging_totals` accessor and its two fields on the
`frame prep geometry:` line (and the matching capture groups in
`.github/browser-rig/drive.py`). Every frame's geometry goes back through the
BAR window.
