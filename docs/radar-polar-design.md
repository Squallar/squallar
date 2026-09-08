# Radar polar data representation — implementation design

Status: design. Nothing here is landed. Every `file:line` was re-read on
`6b7e7ada` (local main at the time of writing) in the `design/radar-polar`
worktree. A figure marked **inferred** is arithmetic over cited constants and
not a reading from a running app; per the standing rule, the unchanged-tree
spread must be published before any delta is quoted against it.

---

## 1. Goal, and the two rulings that shape it

Today a radar sweep is rasterised to a square RGBA plan view on the worker and
uploaded as an `Rgba8Unorm` texture (`render.rs:1239` `render_with_projection`, `app_render.rs`
`ctx.load_texture(format!("radar_image_{counter}"), .., NEAREST)`). One native
loop frame costs `2048² × 4 = 16,777,216 B` on the GPU
(`constants::plan_view_frame_cost`, `budget.rs:970-979`); the desktop cap is
`DESKTOP_MAX_LOOP_FRAMES = 60` (`constants.rs:374`).

**Goal: loops of hundreds of frames.** The sweep already exists in polar form
on the wire — `bufs.polar.paint(from, ctx.azimuth_deg, ctx.az_half_spacing,
value)` is the *first* thing `MercatorProjection::render_gate` does
(`render.rs:95-96`), and `PolarField::to_bytes` is a nominated wire tail
(`polar.rs:253-276`, `jobs.rs:33`). What is missing is a *code* plane and a
draw path that projects it on the GPU.

Per-tilt R8 code plane, surveillance cut (720 radials × 1832 gates, **inferred**
from `chunks.rs:450-455` and `types/tests.rs:231-236`):

| form | bytes | vs a 2048 native loop frame | vs a 1024 wasm loop frame |
|---|---:|---:|---:|
| R8 codes, level 0 | 1,319,040 (1.258 MiB) | 0.0786× | 0.3145× |
| R8 + full max-mip chain | 1,758,832 (1.677 MiB) | **0.1048×** | **0.4193×** |
| today's `PolarField` f32 values | 5,276,160 (5.03 MiB) | 0.3145× | 1.258× |

`9.54×` more GPU-resident frames per byte on the native arm, `2.39×` on wasm
(**inferred**; the wasm loop frame is already only 4 MiB, so the headline is a
*native* figure and the two are never merged).

### Ruling (1) — the strongest echo wins at zoom-out

Today's arbitration is **not** a max over value. `RenderBuffers::claim` is
`self.cells[pixel_idx].fetch_max(cell, Relaxed)` over
`cell(key, value) = ((key as u64) << 32) | value.to_bits()`
(`render.rs:652-662`) with
`write_key(from) = (radial.min(0xFFFF) << 16) | (gate.min(0xFFFE) + 1)`
(`render.rs:795`). The doc says it plainly: *"The key takes the high bits so
`fetch_max` orders by it and not by the value riding along in the low ones."*
So today is **last-radial / outer-gate wins**.

The ruling replaces that with **max over the quantity**, implemented as a
max-reducing mip chain on the code plane (§2.3). This is a deliberate change of
appearance at every zoom where more than one gate lands in a pixel.

### Ruling (2) — the 3D pane's floor must keep showing radar

The floor works today only because the radar picture is a `Primitive::Mesh`.
`EguiRenderer::render_mirror` swaps **every** callback for an empty mesh
(`egui_renderer.rs:635-640`), because *"`render` ignores `Primitive::Callback`
but `update_buffers` does not, and would run every `prepare` twice"*
(`egui_renderer.rs:610-614`). A callback-based radar layer therefore needs a
second mirror path — §5. The tile-mesh basemap already has this hole and the
same path closes it.

### Constraints carried in from the frame-time session (binding)

* ONE paint callback per pane for all sweeps. The "sited **adjacent to the
  ground callbacks** so it rides inside a reset already paid" half of this
  constraint is **withdrawn** by the §10.6 ruling — that siting removes radar
  from the layer order, which is not a cost this design may spend. One callback
  per pane still binds; where it is issued is the layer walk's business, and the
  reset it costs is paid rather than dodged — see §4.1's Siting note.
* STATIC disk mesh; the Mercator projection is done in the **vertex shader**.
  No mesh rebuild on pan or zoom.
* Per frame: one uniform write and nothing else. No vertex or index writes on
  an interact frame. Frame-thread encode ≤ 50 µs per pane.
* The ~1.3 MB sweep upload and the max-mip build happen at upload time
  (`prepare` or the worker), once per loop step. **A loop step swaps a bind
  group.**
* Census label: the `&'static str` `"radar polar"`. Expected recorded calls for
  a one-tilt pane: **6** — pipeline, 2 bind groups, vertex buffer, index
  buffer, `draw_indexed`; **+2** per extra sweep. `>10` is a per-radial rebind
  defect.
* Interaction is realtime; heavy work never on the frame thread; a
  `cfg(target_arch = "wasm32")` may select a value, a dependency or a type
  alias and may never fork behaviour inside a function body; reopen is 1:1.

---

## 2. Data model

### 2.1 The code plane

A new type in `squallar-radar`, **beside** `PolarField` and not replacing it:

```rust
pub struct CodePlane {
    codes: Vec<u8>,          // radials * gates, radial-major, level 0
    mips: Vec<u8>,           // levels 1..=L, concatenated, each ceil-halved
    radials: usize,
    gates: usize,
    width: CodeWidth,        // R8 today; R16 is the phase-F widening
    key: LutKey,             // what decodes it, and what colours it
    reduce: Reduce,          // how a mip level was built
}
```

`PolarField` keeps its wire form byte-for-byte (`polar.rs:253-276`) and keeps
carrying the **geometry** — the per-radial `Wedge { azimuth_deg,
half_width_deg }` table, `first_gate_slant_km`, `gate_interval_slant_km`,
`elevation_deg` (`NaN` = `None`), `gates`, `reach_gates`. On a polar frame its
`values` vector is empty, which the decoder already accepts
(`polar.rs:307-309`: `n_values != 0 && n_values != radials*gates` is the only
refusal). **Digest suite 2 (`render/polar/tests.rs:302`,
`(112, 0x986a_92ef_b56e_c209)`) therefore does not move.**

Dims per family, all from the sweep in hand and never from the VCP (radials
disagree about reach, gate count and spacing — `render.rs` `compute_max_range`,
`compute_gate_interval_km`, `compute_gate_span`; `MIN_SEALED_RADIAL_PERCENT =
95` at `chunks.rs:384`):

| family | radials × gates | width | codes on the wire? |
|---|---|---|---|
| REF / RHO (surveillance) | 720 × 1832 | R8 | yes, raw + per-block `(scale, offset)` |
| VEL / SW (Doppler) | 720 × 1192 | R8 | yes |
| ZDR 8-bit | 720 × 1832 | R8 | yes, `(16, 128)` per `voxel.rs:756-757` |
| ZDR 16-bit, PHI | 720 × 1832 | R16 | yes, `(32, 418)` / `(2.8361, 2.0)` — **phase F** |
| Level III radial (N0K, EET, DVL, DPR) | run count × `num_range_bins` | R8 | yes, per-PDB LUT or xdr `(scale, offset)` |
| NROT / SRV (derived per-tilt) | velocity-grid shape | R8 | **no** — needs a quantiser, §10.2 |
| ETI / POSH / MEHS / VILD | 360 × 230 @ 1 km | R8 | **no** — no quantiser defined anywhere |
| HHC | 360 × 920 @ 0.25 km | R8 | categorical class codes 10..150 |

R8 costs, **inferred**: surveillance 1,319,040 B; Doppler 858,240 B; 360 × 230
= 82,800 B; 360 × 920 = 331,200 B.

### 2.2 The azimuth → row table, and the cap

Radials are non-uniform: `l2_wedge_half_widths_deg` (`render.rs`) takes the
declared spacing as a *floor* and extends each half-width to half the gap to
each neighbour when that gap ≤ `MAX_ADJACENT_GAP_STEPS = 1.5` × base, capped at
`MAX_WEDGE_DEG = 2.0`. Because the declared spacing is a floor, **wedges can
overlap** where the real gap is tighter than the declaration, and
`PolarGeometry::pick` resolves that by *"Radial-major, greatest wins —
`write_key`'s ordering"* (`polar.rs:95-102`).

Geometry cannot express "greatest wins" without a depth buffer, and egui's pass
has none (`egui_renderer.rs` `draw`: `depth_stencil_attachment: None`). So the
design **removes the overlap instead of ordering it**:

> `polar::draw_edges(&[Wedge]) -> Vec<(f32, f32)>` returns each radial's drawn
> `(lo, hi)` azimuth, trimming a wedge at the midpoint to a neighbour it would
> otherwise overlap. For non-overlapping input it is the identity, so `pick`'s
> answer is unchanged wherever `pick` was unambiguous; it differs only inside
> slivers where two radials both claimed the point.

That table is **derived host-side, at upload, from the wire wedges**. It is not
a wire field, so no digest moves. It is the single definition used by *both*
the fan geometry and the new hover pick (§4.4, §8 test 2).

**The cap and the hostile-payload guard.** `MAX_EXTENT_KM = 470.0`
(`types.rs:57`) bounds a mis-framed radial's *reach*; it does not bound
`radials × gates`. The WebGL2 guarantee is 2048 per axis
(`WEBGL2_MAX_TEXTURE_DIMENSION_2D = 2048`, `types.rs:20`; `device.rs:57-59`
pins `downlevel_webgl2_defaults().using_resolution(adapter)` on web, so only
resolution is lifted). Therefore:

```
MAX_POLAR_RADIALS = 1440   // twice the 720 the RDA can declare
                           // (render.rs: "Level II declares 0.5° or 1.0° and
                           //  nothing else, the RDA has no third resolution")
MAX_POLAR_GATES   = 2048   // the WebGL2 per-axis guarantee, verbatim
```

`gates_used = min(shape.gates, ceil(slant_for(MAX_EXTENT_KM) / interval),
MAX_POLAR_GATES)`. A payload past either cap is **refused and counted**, never
truncated silently and never panicking — a `CodePlane::refusals()` counter in
the crate's always-on ledger idiom. Test 6 in §8 drives a 60,000-gate radial,
the same shape `types.rs:47-56` already documents for the raster.

### 2.3 The LUT

One entry point exists for colour: `get_color_for_value(product, value) ->
(u8,u8,u8,u8)` (`palette.rs:43`), `(0,0,0,0)` for a non-finite value.

**Keying.** `LutKey = (FieldId, scale: f32, offset: f32)` — *not* product
alone. Scale and offset are per moment block and the `scale == 0.0` branch
(`moment.rs:262`: raw is the value, no sentinels) must be reproduced or a
zero-scale moment's legitimate 0 and 1 gates become sentinels.

**Size per family.**

| family | entries | bytes | exact? |
|---|---:|---:|---|
| REF, VEL, SW, RHO, ZDR-8, HHC, all five L3 | 256 | 1,024 | **yes** |
| PHI, ZDR-16 | 65,536 | 262,144 | yes but deferred (§7 phase F) |

Build: `entry[c] = get_color_for_value(product, (c − offset)/scale)` for
`c ≥ 2`; `entry[0] = (0,0,0,0)` (BelowThreshold is unpainted, `render.rs:809`);
`entry[1] = palette::RANGE_FOLDED`. The CPU evaluates the piecewise-linear
blends today, per value, so baking them into 256 entries loses nothing.

**Alpha.** The 2D palette is moving to return alpha 255 with per-layer opacity
applied at draw. The LUT therefore bakes **RGB at full alpha**, and the *only*
alpha-0 entries are the per-product transparency cutoffs, which live only
inside `get_color_for_value` (`xsect.rs:662-667` says so): REF `< 0`, SW `< 0`,
RHO `< 0`, EchoTops `< 5`, VIL `< 1`, VilDensity `< 0.5`, POSH `< 10`, MEHS
`< 0.25`, HHC `< 10`, PrecipRate `< 0.01`, NROT `|v| < NROT_FIRST_CLASS`. The
fragment emits **premultiplied** `(rgb·a·opacity, a·opacity)` into egui's own
blend (copied verbatim by `tile_mesh.rs:603`), so an alpha-0 entry
contributes exactly nothing and needs no `discard` and no branch.

**What this changes about "must not claim".** `render.rs:1617-1620` skips the
NROT claim when the colour's alpha is zero, *"or it would outrank a real return
from a lower radial"*. On the fan there **is** no lower radial: wedges are
non-overlapping by construction (§2.2), so a sub-threshold gate simply shows
the basemap through it. That is a change from today and it is the more correct
of the two — the gate that is actually there is the one drawn.

**HHC is never max-reduced.** Class codes 10..150 (`types.rs:864-881`) are
ordinally meaningless.

### 2.4 The max-mip chain

Built **on the CPU, off the frame thread, in bands** — wgpu has no mip
generator; the precedent is `volume_raymarch.rs` `downsampled_band` written
with `queue.write_texture(.. mip_level: 1 ..)`. Built once per sweep at upload,
never per frame.

```
chain(R, G) = Σ_{l=0}^{L} ceil(R/2^l) · ceil(G/2^l),  L = floor(log2 max(R,G))
```
720 × 1832: 1,319,040 + 329,760 + 82,440 + 20,610 + 5,175 + 1,334 + 348 + 90 +
24 + 8 + 2 + 1 = **1,758,832 B**, a factor of **1.33338** (**inferred**).
720 × 1192: **1,144,411 B**, factor 1.33344.

**What "max" means — the reduce operator, declared per family.** Each level's
cell reduces the ≤ 4 cells under it by:

```
data = { c in cells : c >= 2 }              // 0 = below threshold, 1 = range folded
if data non-empty      -> Reduce::MaxCode:      max(data)
                          Reduce::MaxMagnitude: argmax over |c − zero_code|, ties to the larger c
else if any cell == 1  -> 1
else                   -> 0
```

`zero_code = round(offset)` — the code that decodes to zero, available because
the LUT key already carries `(scale, offset)`.

| products | operator | why |
|---|---|---|
| REF, SW, ZDR, VIL, ETI, POSH, MEHS, VILD, KDP, PrecipRate | `MaxCode` | the code ascends with the quantity, so the strongest echo is the largest code |
| **VEL, SRV, NROT** | `MaxMagnitude` | codes ascend from −Nyquist to +Nyquist; the largest code is the fastest *away*, not the strongest. **The choice is the magnitude of the decoded quantity**, which is well defined because the code map is affine and monotone, and it is what "strongest" means for a signed field |
| HHC, PHI | `None` — one level only, LOD clamped to 0 | HHC is categorical; PHI wraps (`palette.rs:84` `rem_euclid(360)`) and a max over a wrapping angle is meaningless |
| RHO | `MaxCode` **provisionally** | see §10.3 — for a quality field the *low* value is the interesting one, and this wants a ruling |

Excluding codes 0 and 1 from the magnitude comparison is load-bearing: code 0
has `|0 − 129| = 129` and would otherwise beat every real velocity.

---

## 3. Wire — DECISION

**A third tail, `codes`, mutually exclusive with `image`.** Not a replacement
of `image`.

Reasons, in order of weight:

1. Both paths must coexist for the whole migration (§7): the products on the
   raster and the products on the fan share one `RenderedFrame` and one
   `LoopFrameImage::PlanView`. A tail that is sometimes RGBA and sometimes
   codes has no self-describing discriminant, so the head would need one
   anyway — at which point the third tail is free.
2. **It does not double the bytes.** Exactly one of `image` / `codes` is
   non-empty, asserted at encode. An empty tail costs its framing only; the
   existing `frame/bare/image | 4` row is already a 4-byte fixture
   (`wire_identity.rs:109-116`). This retires the cost objection in the scout's
   open question 8.
3. `PolarField` is untouched, so digest suite 2 does not move.

**Exactly which rows move.**

| row | today | after |
|---|---|---|
| `WIRE_FRAME_REPLY_ROWS` `frame/full/head` | `29 \| 0x89dc568e54cf7abb` | re-pinned; the head gains a 1-byte surface discriminant and the code plane's shape/key block |
| `frame/bare/head` | `11 \| 0xc813d3185b023723` | re-pinned, same reason |
| `frame/full/polar` | `80 \| 0x9f0c3f4e5dce8435` | **length and digest unchanged**; re-listed only because the row set grows |
| `frame/bare/polar` | `40 \| 0x3f3ecf0cef9be2c0` | unchanged |
| `frame/full/image` | `8 \| 0x0363b2a3926bce45` | unchanged |
| `frame/bare/image` | `4 \| 0xbe7a5e775165785d` | unchanged |
| `frame/full/codes` | — | **new** |
| `frame/bare/codes` | — | **new** |

Six rows become eight; two of the six are re-pinned. `jobs.rs:766-771`'s
`tails.len() == 2` becomes 3, and its message text moves with it. **The build
token moves**: `wire_digest()` is the local dev page/worker build token, which
is exactly what Tier-2's `doctored`-token respawn leg exercises
(`.github/browser-rig/run_tier2.sh`) — so that leg is the gate on this change,
not a casualty of it.

**`WIRE_FRAMING_ROWS` row 0 (`radar | 46 | 0x813f26d9407ac047`) does not move**:
it is the *request*, and `RenderInput` gains no field — raw codes and their
`(scale, offset)` already cross verbatim. Digest suite 3
(`render_input/tests.rs`, `(12, 260, 0xaa29_1c4f_2a6e_feb5)`) is untouched.

**Digest suites touched: none.** All ten (`render/tests.rs`,
`render/polar/tests.rs`, `render_input/tests.rs`, `volume_wire/tests.rs`,
`xsect/tests.rs`, `voxel/tests.rs`, `velocity/tests.rs`,
`volumetric/tests.rs`, `chunks/tests.rs`, `tests/geodesy_one_definition.rs`)
pass unedited. `wire_identity.rs` is `squallar-worker`'s registry and is not one
of them. **New suite:** `squallar-radar/src/render/codes/tests.rs`, digesting
`CodePlane::to_bytes()` in the shape suites 2 and 8 already use.

Note the standing ambiguity CLAUDE.md warns about: two different sets of ten
exist, and suite 1 asserts no pinned constant while suite 10 asserts none at
all. A report on this work must say *which* ten.

`RenderedFrame::straight_rasters_mut` (`frame.rs:135-140`) must **not** return
the code tail — codes are not premultipliable. It already returns only the
`RasterImage::Bytes` arm, so no edit is needed; the worker's blank-discard
(`offload.rs`) survives by construction because the no-data code stays byte 0.

---

## 4. Draw path

### 4.1 The callback

**Crate: `squallar-gpu`.** It may name `wgpu`, `egui`, `egui-wgpu`,
`squallar-egui`, `squallar-device-profile` and `web-time`, and nothing else
(`squallar-gpu/tests/charter.rs`, "normal" arm). It must not depend on
`squallar-radar`, and it never needs to: the payload reaching it is plain bytes
and plain numbers.

Structs, mirrored from `tile_mesh` as landed in `192708ca`:

| tile_mesh | radar fan |
|---|---|
| `squallar-gpu/src/tile_mesh.rs` `TileMeshStore` | `squallar-gpu/src/radar_fan.rs` `RadarFanStore` |
| `TileMeshCallback { .., slot: AtomicU32 }` | `RadarFanCallback { sweeps: Arc<[Arc<FanSweep>]>, view: FanView, pass_nr, slot: AtomicU32, mirror_slot: AtomicU32 }` |
| `TileMeshBridge` (`#[derive(Default)]`, holds nothing) | `RadarFanBridge` |
| `squallar-egui/src/tile_mesh.rs` `TileMeshPainter` / `GroundDraw` / `Placement` | `squallar-egui/src/radar_fan.rs` `RadarFanPainter` / `FanDraw` / `FanView` |
| `squallar-gpu/src/tile_mesh.wgsl` | `squallar-gpu/src/radar_fan.wgsl` |

`squallar-egui::radar_fan::FanSweep` holds the CPU side — code plane, mip
offsets, LUT bytes, drawn-edge table, and the scalars. It is keyed by `FieldId`
and names no `RadarProduct`, so `PRODUCT_IN_EGUI_MAX = 0`
(`arch_ratchets.rs:443`, asserted with `assert_eq!`) holds. Residency is
`tile_mesh`'s rule verbatim: a `Weak<FanSweep>` per resident GPU sweep, swept
once per pass under a `swept_pass: Option<u64>` guard — the GPU store has no
budget of its own.

**Resources.**

* group 0 — the per-frame uniform, `has_dynamic_offset: true`, written from a
  ring exactly as `TileMeshStore` does (`RING_SLOTS` with a
  `const _: () = assert!(PANES * SWEEPS_PER_PANE <= RING_SLOTS, ..)`
  build-failure proof, and one `queue.write_buffer` for the whole pass in
  `finish_prepare` — `tile_mesh.rs:136-142`: *"`queue.write_buffer` is not a memcpy"*, measured at
  4.25 of 8.55 percent of main-thread samples, Linux, scene D, 174.96 Hz).
* group 1 — per sweep: the R8 code texture (with its mip chain), the 256 × 1
  `Rgba8Unorm` LUT, and a per-sweep uniform holding the drawn-edge table plus
  the sweep scalars.

**The drawn-edge table lives in the per-sweep uniform**, as
`array<vec4<f32>, 360>` = 1440 sectors × (lo, hi) = 5,760 B, under the WebGL2
guaranteed 16 KiB uniform block. Deliberately *not* a texture: a vertex-stage
texture fetch is legal in ES 3.0 but is not exercised anywhere in this tree and
would be a second thing to prove on two web arms.

**Six recorded calls.** `paint` does exactly:

```
set_pipeline                    1
set_bind_group(0, .., &[slot])  1
set_bind_group(1, sweep)        1   ┐ per sweep
set_vertex_buffer(0, ..)        1   │ once — the mesh is shared
set_index_buffer(..)            1   │ once
draw_indexed(0..N, 0, 0..1)     1   ┘ per sweep
```
= 6 for one tilt, +2 per extra sweep (`set_bind_group(1)` + `draw_indexed`).
**No `set_viewport`.** `tile_mesh` overrides egui's courtesy viewport because
its geometry is placed in whole-screen points; the fan does not need to,
because the callback rect *is* the pane's map rect and the vertex shader emits
clip space inside it. That is what buys the seventh call back.

Because the uniform is written in `prepare` but the viewport is set by egui
before `paint`, `prepare` must mirror `PaintCallbackInfo::viewport_in_pixels`'s
rounding itself — the same discipline `tile_mesh.rs:682-685` applies to
`ScreenDescriptor::screen_size_in_points` ("this is its body, and it must stay
its body"). Pinned by a test against the vendored function.

**Siting. SUPERSEDED by the ruling on §10.6 (2026-09-08).** This section
proposed issuing the callback at the **tail of the ground-callback run**, from
the `issue_run_batch` neighbourhood in
`squallar-egui/src/ui_map_overlays.rs:596-628`, where — adjacent to the existing
callbacks — the fan would pay **zero** additional resets. It named the visible
consequence as "basemap strokes and labels draw over radar" and sent the
appearance to §10.6 for a ruling.

The ruling rejected the framing: **a radar is an overlay like any other
overlay.** The ground run happens before the pane's layer walk starts, so a fan
issued there occupies no position in the stack at all — it draws beneath every
other weather layer regardless of what the user wants, and stops answering to
the draw order the user is entitled to rearrange. Severing radar from the
ordering was never a cost the design was entitled to price; it was a defect
inside one arm of a question that should not have been asked.

**What is built instead:** the callback is issued at radar's own position in
the layer walk, from `draw_radar_fan` in `squallar-egui/src/ui_map_pane.rs`, the
same `ui.painter()` at the same point the raster's textured rectangle used.
Radar composites exactly where it always has and keeps responding to reordering.
`the_fan_draws_at_the_position_the_user_ordered_radar_into` pins it.

**The reset price is real, and it is not avoidable by siting.** A painted
callback sets `needs_reset` and the next *mesh* pays the reset triple plus two
buffer binds (`command_stream.rs:204-213`, matching
`vendor/egui-wgpu/src/renderer.rs:693-720`). "The next mesh" is the next one
anywhere in the pass, not the next layer above radar — the pane's deferred
notices, the colour scale, another pane and the app's chrome all qualify — so
budget **one reset per pane per frame** and treat a configuration that dodges it
as luck. What is withdrawn is not the cost but the comparison: the only siting
that avoids the reset is the one that removes radar from the layer order, so
there is nothing left to price this against, and the "50 %" figure has no second
arm.

It is also **not measurable today**: no build issues a fan callback (this
section's renderer does not exist and `radar_fan_bridge` returns `None`), so any
figure would be inferred. The two terms the cost is a function of are pinned
instead — one callback per pane, at radar's ordered position.

### 4.2 The static disk mesh — per-radial sectors, PICKED

Two candidates:

* **Per-gate quads** — 1,319,040 quads for one super-res tilt. Rejected: the
  mesh would be per-sweep, so a loop step would swap a vertex *and* an index
  buffer (+3 calls per extra sweep, not +2), and 1.3 M quads is a vertex load
  no arm was measured at.
* **Per-radial sectors — PICKED.** One canonical mesh of
  `SECTORS = MAX_POLAR_RADIALS = 1440` sectors × `RINGS = 16` range segments,
  built once at `RadarFanStore::new` and never rebuilt. Vertex attribute is one
  packed `u32` (`sector` 11 bits, `ring` 5 bits, `side` 1 bit): 1440 × 17 × 2 =
  48,960 vertices × 4 B = 195,840 B, and 1440 × 16 × 2 × 3 = 138,240 `u32`
  indices = 552,960 B. Both static, both **inferred**. A sweep with fewer
  radials leaves the surplus sectors' `(lo, hi)` equal, so they are degenerate
  and cost nothing.

`RINGS = 16` is a silhouette parameter only (§4.4 makes the *value* lookup
exact independently of it). At 460 km the outer arc of a 0.5° sector has chord
error `r(1 − cos 0.25°) = 4.4 m`, and a 29 km radial segment's departure from
the true Mercator curve is smaller still — both far under a 250 m gate
(**inferred**).

### 4.3 The vertex shader, in the continuous longitude frame

`walkers::Projector::project` is
`clip_rect.center() + (project_at_scale(pos, world_pixels) − map_center_projected)`
(`vendor/walkers/src/projector.rs:53-60`) and folds nothing;
`great_circle_destination` returns `site_lon + Δlon` **unnormalised** by
contract (`squallar-geo/src/lib.rs:100-116`). The shader must stay in that one
continuous frame: it adds a longitude *offset* to the site and never wraps.

Per vertex: `sector` selects `(lo, hi)` from the edge table and `side` picks
one; `ring` gives `t ∈ [0,1]` over `[first_gate_km, reach_km]` in **ground**
range. Then, mirroring `MercatorProjection::pixel_at` (`render.rs:74-84`) term
for term:

```
sin_lat = sin_lat0·cos_d + cos_lat0·sin_d·cos_az          // clamped to ±1
dlon    = atan2(sin_az·sin_d·cos_lat0, cos_d − sin_lat0·sin_lat)
```

**Precision.** `merc_y = mercator_y_from_sin_lat(sin_lat)` is `O(1)`, so
`merc_y − merc_y_site` in f32 loses ~1e-7 absolute — 4.3 px at zoom 20
(**inferred**). The shader therefore never forms that difference. It computes
`Δsin_lat = sin_lat0·(cos_d − 1) + cos_lat0·sin_d·cos_az` with
`cos_d − 1 = −2 sin²(d/2)` (cancellation-free) and takes
`Δmerc_y = atanh(Δsin_lat / (1 − sin_lat0·sin_lat))`, which is cancellation-free
end to end: ~1e-8, i.e. 0.43 px at zoom 20 and 0.007 px at zoom 14
(**inferred**). `Δlon` is already a difference. The site's own screen position
is computed on the CPU in `f64` and enters the uniform as a small f32.

```
screen_px = site_px + world_px · vec2(dlon/2π, −Δmerc_y/2π)
clip      = (screen_px / viewport_px) · vec2(2, −2) + vec2(-1, 1)
```

**The shader names no geodesy literal.** `EARTH_RADIUS_KM` and `RE_EFF_KM`
arrive as uniform f32 (as `1/R` products, precomputed). That is a design choice
with a gate behind it: `squallar-radar/tests/geodesy_one_definition.rs` scans
every `.rs` **and `.wgsl`** in the workspace (`:257`, `:269-270`) for literals
in the 6300–6400 band, and `radar_fan.wgsl` therefore needs no `ALLOWED` entry.

### 4.4 The fragment: the gate lookup, and hover agreement

The radial index arrives `@interpolate(flat)` from the vertex — exact, because
the geometry *is* the wedge.

The gate must be exact too, and `PolarGeometry::gate_at` is the definition:

```rust
let along_beam_km = match self.elevation_deg {
    Some(e) => crate::beam::slant_range_for_ground_km(ground_km, e),
    None => ground_km,
};
let g = ((along_beam_km - self.first_gate_slant_km) / self.gate_interval_slant_km + 0.5).floor();
```
(`polar.rs:106-115`.) `slant_range_for_ground_km` is the exact inverse of
`beam::ground_range_km`, the **spherical** 4/3 arc `RE_EFF_KM · asin(r cos e /
(RE_EFF + h))` with `h = √(r² + Rₑ² + 2 r Rₑ sin e) − Rₑ` (`beam.rs:39-54`). It
is emphatically **not** `sweep_ground_factor`'s `ground/slant` ratio, which the
renderer uses only for *sizing*.

So the fragment must recover its own true `ground_km`. It does, exactly:

> The vertex stage also outputs `(Δx_norm, Δy_norm)` — the fragment's position
> in **normalized Web Mercator relative to the site**. This interpolates
> *exactly* across a triangle, because `Projector::project` is affine from
> normalized-Mercator to screen. The fragment then inverts:
> `sin_lat = tanh(merc_y_site − 2π·Δy_norm)`, `dlon = 2π·Δx_norm`, and takes the
> haversine ground range on `EARTH_RADIUS_KM` — the same sphere
> `site_bearing_range_km` uses (`squallar-geo/src/lib.rs:49-68`).

Cost: one `tanh`, one `cos`, one `atan2`/`sqrt` pair per fragment. No
derivatives, no implicit LOD (a hard validator failure under non-uniform
control flow — `volume.wgsl:4-7`).

**Hover agreement is then two properties, each with a test (§8):**

* *azimuth*: the drawn sector and the picked radial both come from
  `polar::draw_edges`, one function, one table.
* *gate*: the fragment evaluates `gate_at`'s expression on the same
  `ground_km`, `first_gate_slant_km`, `gate_interval_slant_km` and
  `elevation_deg` the pick uses.

Hover reads the **host code plane** and decodes with the LUT key's
`(scale, offset)` — never a GPU readback, never the picture.

### 4.5 Sampling, LOD, and the loop step

**No samplers at all.** Every fetch is `textureLoad`: `texture_2d<u32>` (R8Uint)
for the code plane at an explicit level, `texture_2d<f32>` for the 256 × 1 LUT
at level 0. This is nearest at level 0 by construction, it dissolves the
"coverage channel" question (a single-channel plane sampled `Linear` would blend
air into data at an echo edge — `lib.rs:50-58`'s litigated 8-bit-index
argument), it sidesteps naga's `Error::ImageMultipleSamplers`, and it avoids
`textureNumLevels`, which is unreachable on WebGL2 (`volume.wgsl:13`).

**Mips sample by explicit level.** The per-frame uniform carries `km_per_px`;
the per-sweep uniform carries `gate_interval_km` and `mip_levels`:

```
lod = clamp(floor(log2(km_per_px / gate_interval_km)), 0, mip_levels - 1)
coord = vec2(radial >> lod, gate >> lod)
```
`mip_levels = 1` for `Reduce::None` clamps HHC and PHI to level 0 on their own,
with no branch and no cfg.

**A loop step swaps a bind group.** Group 1 is the only thing that changes: the
code texture, the LUT and the per-sweep uniform. Nothing is written, nothing is
uploaded, no mesh moves. Per frame the store writes **one** uniform buffer, in
`finish_prepare`, for the whole pass.

---

## 5. The 3D floor mirror — the second path

**File:** `squallar-gpu/src/egui_renderer/mirror/named_callbacks.rs` (new;
`mirror/` exists and holds only `tests.rs` today).
**Types:** `pub(crate) struct MirrorPhase;` — a zero-sized marker inserted into
`egui_wgpu::CallbackResources` for the duration of the mirror pass and removed
after — and `pub(crate) const MIRROR_SAFE: &[&str] = &["radar polar"];`.

Three changes, and each is small:

1. **A name on the callback.** `vendor/egui-wgpu` gains a local change (the
   crate already carries several, each marked `**Local change**` and recorded in
   `VENDORED.md`): `Callback::new_paint_callback_named(rect, cb, name:
   &'static str)` and `Callback::name() -> &'static str`, defaulting to `""`.
   **The same `&'static str` is the census label** — `"radar polar"` — so the
   mirror filter and the census cannot drift apart.
2. **The swap becomes selective.** `render_mirror`'s
   `matches!(primitive.primitive, Primitive::Callback(_))`
   (`egui_renderer.rs:635-640`) becomes "a callback whose name is not in
   `MIRROR_SAFE`". Everything else is unchanged, including the clip clamp and
   the restore loop.
3. **A named callback gets a second slot.** The double `prepare` is the whole
   reason for the original swap: `update_buffers` runs `prepare` once for the
   mirror slice and once for the frame slice, so a single `slot: AtomicU32`
   would leave the *main* pass painting with the *mirror's* placement — a wrong
   picture, silently, exactly the failure `tile_mesh.rs:98` guards its ring
   against. `RadarFanCallback` therefore holds `slot` **and** `mirror_slot`;
   `prepare` writes whichever the presence of `MirrorPhase` in
   `callback_resources` selects, and `paint` reads by the same test. Both
   pass-orderings stay legal because `update_buffers` runs every `prepare` of a
   pass before any `paint` of it (`vendor/egui-wgpu/src/renderer.rs:48`, `:110`).

The mirror pass's own `ScreenDescriptor` is already the mirror's
(`egui_renderer.rs:645-648`), so the mirror slot's projection is right for free.

This also closes the tile-mesh basemap's identical hole the day `"tile mesh"`
is added to `MIRROR_SAFE` — a one-line follow-on, deliberately **not** in this
work's scope, because it is a separate appearance change to gate.

`squallar_egui::floor_ledger::note_mirror_render()` (`egui_renderer.rs:627`)
stays where it is and keeps counting one per pass encoded.

---

## 6. Budget hooks

### 6.1 The formula

`squallar-device-profile/src/constants.rs:122-127` already names this swap
verbatim: *"It is here only because that file is being rewritten to a polar
representation — where a frame costs `radials × gates × width × sweeps` and a
mip factor, which no expression over a `side` can state at all. This function
is the seam that rewrite swaps: give it the polar shape's own inputs, and every
site below keeps reading a `FrameCost` and needs no edit."*

```rust
pub const fn polar_frame_cost(shape: PolarFrameShape) -> FrameCost {
    // shape: { radials, gates, width_bytes, sweeps, mip_levels }
    let base  = shape.radials * shape.gates * shape.width_bytes;
    let chain = chain_texels(shape.radials, shape.gates, shape.mip_levels) * shape.width_bytes;
    FrameCost {
        gpu:          shape.sweeps * chain,
        host_held:    if CODES_RETAINED { shape.sweeps * base } else { 0 },
        host_scratch: shape.sweeps * (chain - base),
    }
}
```

`chain_texels(R, G, L) = Σ_{l<L} ceil(R/2^l)·ceil(G/2^l)`, which is
`< (4/3)·R·G + 2(R+G) + 2L` and equals `1.33338 × R·G` at 720 × 1832
(**inferred**, exact sum in §2.4). `mip_levels = 1` for `Reduce::None`, which
makes `host_scratch` zero for HHC and PHI.

Not in the per-frame formula, because they are per **sweep-key** and shared
across a loop's frames: the LUT at `256 × 4 = 1,024 B` on the GPU per
`(FieldId, scale, offset)`, and the drawn-edge table at `radials × 2 × 4 =
5,760 B` at 720 radials, host and GPU.

Against the current arm-by-arm figures (all **inferred**; the arms are never
added):

| | today | polar, one surveillance tilt | ratio |
|---|---:|---:|---:|
| GPU / frame, native (2048) | 16,777,216 | 1,758,832 | 0.105× |
| GPU / frame, wasm (1024) | 4,194,304 | 1,758,832 | 0.419× |
| host_held / frame, native still | 33,554,432 | 1,319,040 | 0.039× |
| host_scratch / frame, native still | 33,554,432 | 439,792 | 0.013× |
| host_peak / frame, native still | 67,108,864 | 1,758,832 | 0.026× |
| GPU at `DESKTOP_MAX_LOOP_FRAMES = 60` | 1,006,632,960 (960.0 MiB) | 105,529,920 (100.6 MiB) | 0.105× |
| GPU at `WASM_MAX_LOOP_FRAMES = 14` | 58,720,256 (56.0 MiB) | 24,623,648 (23.5 MiB) | 0.419× |

### 6.2 Is a CPU copy of the polar data retained per frame? — YES for stills,
### and the loop default is unchanged

`CODES_RETAINED` is the successor to `values_wanted` (`jobs.rs:87-88`,
`loop_downloads.rs:789`, and the second strip at `app_fetch.rs:2446`). Its
default **preserves today's behaviour**: on for a still pane (a hover reads it: `values_wanted: true` at
`render_dispatch.rs:1781` and `:2198`), off for a loop frame.

What changes is that turning it on for loops is now affordable, and it is worth
recording because it buys a product property that does not exist today —
**hover on every loop frame, without the pinned `Arc<Scan>`**:

| | per frame | × 60 desktop | × 14 wasm |
|---|---:|---:|---:|
| today's f32 values (why loops strip them) | 5,276,160 | 316,569,600 (301.9 MiB) | 73,866,240 (70.4 MiB) |
| R8 codes | 1,319,040 | 79,142,400 (75.5 MiB) | 18,466,560 (17.6 MiB) |

(**inferred**.) A loop frame's host price today is the wedge table alone —
720 × 8 = **5,760 B**, not the "5.8 KiB" `hover.rs:82` says; that prose is loose
and should be re-derived from `PolarGeometry::resident_bytes`
(`polar.rs:172-174`) in the same land.

### 6.3 Does hover read from it? — YES, and only from it

`HoverSource::read` → `geometry().pick(azimuth_deg, ground_km)` → resident
values, else `SweepGates::at` off the pinned `Arc<Scan>` (`hover.rs:69-126`).
The shape is unchanged; the resident term becomes codes rather than f32, and the
pick becomes `pick_drawn` (§2.2) so the readout and the picture cannot
disagree. Hover **never** reads the picture today and must not start.

### 6.4 The change to `PaneNeed` and the loop pool

`PaneNeed::overlay_frame_bytes` (`fit.rs:482, :499, :506`) is the precedent for
a layer supplying its own measured per-frame cost. Mirror it:

* `PaneNeed` gains `plan_view_frame_bytes: usize`, `0` meaning "no measured
  figure — use the budget's ceiling".
* `fit.rs`'s `RenderView::PlanView => budgets.static_frame_bytes()` and the
  loop arm take the measured figure when it is non-zero.
* `Budgets::loop_frame_cost` / `static_frame_cost` keep returning
  `plan_view_frame_cost(side)` as the **ceiling** for a pane that has not
  reported yet, so `budget/tests.rs::check_invariants` re-derives over the whole
  scene table unchanged for every raster pane.
* The loop pool's `plan_view` model term takes the same measured figure.
  `loop_pool`'s pinned `arm.model.overlay != arm.model.plan_view` must keep
  holding, and the measured polar figure must not collide with
  `2880 × 1620 × 4 = 18,662,400`. 1,758,832 does not (**inferred**).

`render_cache_budget_bytes` = `entries × plan_view_frame_cost(long_range_image_side_px).host_held`
(`budget.rs:986-989`) becomes `entries × polar_frame_cost(..).host_held` for a
cache of polar entries — a **fall**, from 33,554,432 to 1,319,040 per entry.

---

## 7. Migration

**Phase A — per-tilt 8-bit wire moments: REF, VEL, SW, RHO, ZDR-8.** Exact
256-entry LUT, no quantiser needed, codes already on the wire. This is the
whole of the first landing.

**Phase B — the five Level III radial products actually fetched**: `N0K`,
`EET`, `DVL`, `DVL+EET` (VIL density), `DPR` (`product_spec.rs`). Exact LUT
keyed on the PDB thresholds or the xdr `(scale, offset)`; `l3_values.rs:125-152`
is already table-driven and `quantize_via_lut -> u8` shows L3 levels are 8-bit.
Required because all three frame rows share one `RenderedFrame`
(`frame_reply_codec!` for `RadarPlanJob`, `Level3Job`, `Level3PairJob`), so
without B a loop's `LoopFrameImage::PlanView` stops being one thing.

**Phase C — HHC.** R8 direct, categorical, `Reduce::None`, `mip_levels = 1`.

**Phase D — NROT and SRV**, after the `derive::codec` ruling (§10.2).

**Stays on the raster, and why:**

* **ETI, POSH, MEHS, VILD** — 360 × 230 = 82,800 cells. No quantiser exists
  anywhere for them, and at 82.8 KB the polar win is not worth defining one.
* **PHI and 16-bit ZDR** — need R16 codes and a 65,536-entry LUT or an in-shader
  affine decode plus the piecewise stops; PHI additionally wraps and must not
  blend across the 345°→0° seam. Phase F, unscheduled.
* **`render_derived_kdp_to_image` / `render_derived_srm_to_image`** — no
  non-test caller exists (§10.9). Neither is migrated.

**Coexistence behind one `RenderView`.** `RenderView::PlanView` is unchanged and
`LoopFrameKey { target, view, timestamp, section }` is unchanged. The
discriminant lives one level down, inside `RadarImageData`:

```rust
pub enum RadarSurface {
    Raster(egui::TextureHandle),
    Fan(std::sync::Arc<dyn std::any::Any + Send + Sync>),   // an Arc<FanSweep> set
}
```

`LoopFrameImage`'s four arms are **not** touched, so `LOOP_FRAME_ARMS_MAX = 8`
and `LOOP_FRAME_ARMS_NON_TEST_MAX = 2` (`arch_ratchets.rs:488`, `:502`) hold
unchanged. A loop's frames are all one product, so one surface kind per loop is
asserted at `LoopFrameStore` insert.

`App::spawn_overlay_render`'s `_` arm (`app_fetch.rs:1407-1421`) keeps refusing
radar by name, and radar stays out of the overlay ledger
(`overlay_cache/ledger.rs:11-20`: *"its raster comes off its own pipeline and is
refused by name at the overlay dispatch, so it is in none of these figures"*).
A polar path must not change that: `overlay rasters` and `texture uploads` have
different denominators and are never added.

---

## 8. Gates

### Must pass UNEDITED

* `cargo test -p squallar-radar` — all ten digest suites, per
  `native_row.py`'s `DEFAULT_GATE_SET` row 4 (*"a moved digest is a bug in the
  encoder, not a pin to re-record"*). §3 shows why none of them moves.
* Suite 1's 58 tests specifically: `render_radar_to_image`,
  `render_level3_radial_to_image(.., types::IMAGE_SIZE) -> SweepRender{image,
  values}`, `render_with_projection`, `RenderBuffers`, `into_output` all stay
  compiling and correct — including `level2_later_radial_wins_a_contested_pixel`
  and `level3_later_radial_wins_a_contested_pixel`, which pin the raster's
  tie-break *direction by name*. The fan's different arbitration is a property
  of the fan, not of the raster.
* The four pool suites: `tests/render_cell_pool.rs`, `render_output_pool.rs`,
  `render_output_slot.rs`, `render_pool_residency.rs`.
* `squallar-app/tests/arch_ratchets.rs` — spelled `--test arch_ratchets`, since
  `-p squallar-app arch_ratchets` selects **zero tests**. `PRODUCT_IN_EGUI_MAX`
  stays 0 (`assert_eq!`), `LOOP_FRAME_ARMS_MAX` 8, `SELF_GUI_MAX` 152,
  `SELF_GUI_NON_TEST_MAX` 147, `UI_SETTER_MAX` 0, `HUB_RECEIVER_MAX` 17.
  Ceilings may only fall, and re-spelling a counted reach through a local
  binding is forbidden by name.
* `app/gui_seam_ratchet_tests.rs`, `app_render/frame_build_order_tests.rs`
  (Apply < Advance < Dispatch < push_frame_inputs < gui.ui_phased — the fan's
  uniform write must sit inside that order),
  `app_render/restore_describes_its_image_tests.rs` (it scrapes for the literals
  `RadarTextureMeta {`, `product,`, `elevation,`, `nyquist_ms,`,
  `melting_layer_source,` and `let product = cached.product;`),
  `app_render/raster_telemetry_line_tests.rs`, `app_render/rig_js_tests.rs`
  (`"plan views may reach "` must keep being emitted).
* `squallar-radar/tests/doc_citations_resolve.rs` — workspace-wide; deleting a
  `fn` named in any comment reddens it from an unopened crate.
* `squallar-radar/tests/geodesy_one_definition.rs` — untouched, because
  `radar_fan.wgsl` names no literal in any band (§4.3). Its own prose digest
  `0x5385ddeb1814353b` at `:45` is **stale** — the live pin is
  `0x7718c8e4c1f550ef` (`volumetric/tests.rs:305`, `chunks/tests.rs` ×4) — and
  should be corrected in the lane that touches the file, as a live "prose is not
  evidence" instance sitting inside a gate.
* The loop pin-list roster:
  `cargo test -p squallar-app --lib -- --list | grep -E "loop_|frame_build_order"`.
  **The `--lib` is load-bearing.** What is pinned is the gap, not the count.
* `squallar-device-profile/src/budget/tests.rs::check_invariants`, per arm.
* The wasm gate, verbatim:
  `.github/scripts/wasm-threads.sh cargo check --workspace --all-targets --target wasm32-unknown-unknown`.
  Every word is load-bearing — the wrapper because rayon's `compile_error!`
  makes the plain spelling unable to pass, and `--all-targets` because without
  it only the libs compile.
* `.github/browser-rig/run_tier2.sh` on **both** browsers, Firefox governing.
  Its `doctored`-token respawn leg is the gate on §3's moved `wire_digest()`.
  None of its six overlay conjuncts sees radar, and none may start to.

### New tests

1. **`lut_reproduces_get_color_for_value`** (`squallar-radar`) — for every
   migrated `(FieldId, scale, offset)` family, all 256 entries byte-equal to
   `get_color_for_value(product, (c − offset)/scale)`, with `entry[0] ==
   (0,0,0,0)` and `entry[1] == palette::RANGE_FOLDED`, and the `scale == 0.0`
   branch covered. Must survive the pinned colour tests at `product.rs:365-537`
   and `palette.rs:713-741` (which `include_str!`s its own source and
   cross-checks `SCALE_COUNT = 17`).
2. **`drawn_gate_equals_picked_gate`** — a Rust mirror of the fragment's solve
   (`Δmerc → sin_lat → ground_km → slant_range_for_ground_km → gate`) fuzzed
   over positions inside the disc against `PolarGeometry::pick_drawn`, asserting
   equality of `(radial, gate)`, not "close". Both arms: a position that lands
   in a trimmed sliver, and one that does not.
3. **`the_fan_callback_records_six_calls`** — two independent arms, because a
   self-reported count answers a narrower question than the claim. (a) the
   store's always-on counter over a one-tilt pane reads
   `recorded/paints == 6`, and `8`, `10` for two and three sweeps; (b) a
   source scrape of `RadarFanCallback::paint`'s body counting `render_pass.`
   receivers, asserting the same arithmetic. `> 10` for one tilt is a
   per-radial rebind and fails by name.
4. **`a_loop_of_n_frames_costs_n_times_the_formula`** — the census/ledger
   measured bytes over an N-frame loop equal `N × polar_frame_cost(shape)`
   term by term (`gpu`, `host_held`, `host_scratch`), not within a tolerance.
5. **`the_mirror_keeps_the_named_callbacks`** — a mirror pass over a fixture
   carrying a `"radar polar"` callback and an unnamed one: readback proves the
   first drew and the second did not, plus `slot != mirror_slot` on the frame
   where both passes ran. The over-firing arm matters more than the
   under-firing one: a healthy unnamed callback must still be swapped.
6. **`a_hostile_polar_payload_is_refused`** — 60,000 gates and 4,000 radials
   are refused and counted, not truncated and not panicking.
7. **`max_mip_is_max`** — level `L`'s cell equals the declared reduce over its
   `2^L × 2^L` level-0 footprint, per operator, including the 0/1 sentinel rule
   and the `Reduce::None` products having exactly one level.
8. **`the_vertex_projection_matches_walkers_projector`** — the WGSL's Rust
   mirror within 0.1 px of `Projector::project` over the zoom ladder and over a
   site set spanning ±60° latitude, and past the antimeridian in the continuous
   frame.
9. **`radar_fan_gpu.rs`** (`squallar-gpu/tests/`) — the `tile_mesh_gpu.rs`
   pattern verbatim: two readbacks byte-compared, run on sRGB **and** non-sRGB,
   with the two arms asserted to *differ* as an interleaved control. Picked up
   by CI's `gpu` job automatically, since
   `gpu_job_covers_every_suite.rs` derives targets from every file with a
   column-0 `#[ignore]`.
10. **A tamper per gate.** Each of 1–9 must be shown red on a defect *and* green
    on a healthy input that resembles it. Touch every restored file — a
    preserved mtime makes cargo reuse the mutated build.

---

## 9. Work breakdown

Eight lanes. File sets are disjoint; no lane edits another's files.

| # | lane | files | depends on |
|---|---|---|---|
| 1 | **Code plane + LUT** (produce only, no wire) | `squallar-radar/src/render/codes.rs` (new), `render/codes/tests.rs` (new), `src/palette.rs`, `src/render.rs` (the one call site beside `bufs.polar.paint`) | — |
| 2 | **Drawn edges + hover** | `squallar-radar/src/render/polar.rs` (`draw_edges`, `pick_drawn`), `src/hover.rs`, `render/polar/tests.rs` (additive only — the digest at `:301` must not move) | — |
| 3 | **Budget seam** | `squallar-device-profile/src/constants.rs` (`polar_frame_cost`, `chain_texels`), `budget.rs`, `fit.rs`, `budget/tests.rs` | — |
| 4 | **GPU store + shader** | `squallar-gpu/src/radar_fan.rs` (new), `radar_fan.wgsl` (new), `src/lib.rs` (module + the const-assert row), `tests/radar_fan_gpu.rs` (new) | 1 (for the payload shape) |
| 5 | **Wire** | `squallar-radar/src/frame.rs`, `src/jobs.rs`, `squallar-worker/src/wire_identity.rs`, `squallar-worker/src/offload.rs` | 1 |
| 6 | **UI seam** | `squallar-egui/src/radar_fan.rs` (new), `ui_map_overlays.rs` (issue site), `ui_map_pane.rs` (`render_radar_overlay`), `pane.rs` (`RadarSurface`), `shell_api.rs` (`GuiEvent::RadarFanPainter`) | 4 |
| 7 | **App wiring** | `squallar-app/src/app.rs` (install order: store into `callback_resources_mut()` **first**, painter published after), `render_dispatch.rs`, `app_render.rs`, `app_fetch.rs` | 5, 6 |
| 8 | **Mirror** | `vendor/egui-wgpu/src/renderer.rs` + `VENDORED.md`, `squallar-gpu/src/egui_renderer.rs`, `egui_renderer/mirror/named_callbacks.rs` (new), `egui_renderer/mirror/tests.rs` | 4 |

Order: 1, 2, 3 in parallel → 4, 5 → 6, 8 → 7. Each lane is a day or less and
lands on local main on its own gate; nothing pushes.

Lane-7 is the only one that can turn the fan on, so 1–6 and 8 land dark and the
raster keeps drawing. That is deliberate: it keeps every intermediate board
green and makes the appearance change a single reviewable landing.

Standing hygiene: `fmt` and `clippy` are package-scoped, never `--all` and never
`--workspace` for a write; the landing gate itself is board-wide, because
`ui_glyphs`, `doc_citations`, `geodesy` and the ratchets all scan sibling
crates; `rm` the lane's target dir when it lands or bins.

---

## 10. Open questions, and what settles each

1. **How common is 16-bit ZDR in live Level II?** The decoder snapshot shows
   `(32, 418)` at 16 bits; `voxel.rs:756-757` assumes 8-bit `(16, 128)`. Both
   exist in the wild. *Settles it:* a census of `data_word_size()` per moment
   over the 208-volume corpus, plus the upper codes actually used by 16-bit PHI
   and ZDR — a small prefix would make a 2048-entry LUT exact and pull phase F
   forward.
2. **Do the 2D derived arms adopt `derive::codec`?** It has SRV `(2.0, 129.0)`,
   NROT `(25.3, 128.5)` and KDP `253/12.05` already, but `derive.rs:47-53`
   states the 2D plan view bypasses `derive` **deliberately** (the memo, the
   key, the wind fit). Adopting is a behaviour change; duplicating is a second
   definition. *Settles it:* a ruling, plus — if adopted — a digest-checked
   before/after on one volume.
3. **What is "strongest" for RHO and ZDR?** `MaxCode` is right for an echo
   field and questionable for a quality field, where the *low* correlation is
   the interesting one. *Settles it:* a user ruling, plus a two-arm screenshot
   at 3–4 zooms.
4. **Does `array<vec4<f32>, 360>` in a uniform, and an R8Uint texture with mips
   read by `textureLoad`, hold under `downlevel_webgl2_defaults`?** Both are
   inside the ES 3.0 guarantees on paper; neither is exercised in this tree.
   *Settles it:* a spike on both web arms — Firefox/llvmpipe and
   Chromium/SwiftShader — remembering that a leg records several adapters and
   the prominent one is often not the one in use: read `app_backend`, then that
   backend's adapter.
5. **What does the per-fragment inverse-Mercator cost on the software arms?**
   Roughly four transcendentals per covered fragment. *Settles it:* a
   measurement on llvmpipe and SwiftShader at a full-pane disc — measured, never
   scaled, and against the unchanged-tree spread first.
6. ~~**Is it acceptable that basemap strokes and labels draw over radar?**~~
   **SETTLED 2026-09-08, against the premise.** The question offered a choice
   between pinning radar on top as today and pinning it beneath the stack by
   drawing it earlier. The ruling: *"there's a misconception here. a radar is an
   overlay like any other overlay."* It takes its position in the ordinary
   layer order the user controls and keeps responding to reordering exactly as
   Global Satellite, Model Data, MRMS Mosaic and the two SPC layers do. There
   is no special slot at either end, so the ground-callback-tail siting was not
   a cheaper implementation of radar-as-overlay — it was not one at all, and the
   "50 % regression" priced the only legal implementation against an illegal
   one. §4.1's Siting paragraph is superseded. **The reset itself is not
   withdrawn**: budget one per pane per frame and keep it in the frame-time
   accounting. What is withdrawn is the idea that it was optional.
7. **Should `CODES_RETAINED` default on for loop frames?** It buys hover on
   every loop frame — which does not exist today — for 75.5 MiB desktop /
   17.6 MiB wasm (**inferred**), where the f32 equivalent was 301.9 MiB and
   unaffordable. The design ships it off, matching today. *Settles it:* a user
   ruling.
8. **Does anything *gate* on `texture uploads`?** A polar path drops radar's
   share of that figure sharply for a reason unrelated to work done.
   *Settles it:* a grep of every Tier-2 leg and the native row for a threshold
   on those figures.
9. **Do `render_derived_kdp_to_image` and `render_derived_srm_to_image`
   survive?** Neither has a non-test caller. *Settles it:* delete or keep — but
   deleting reddens `doc_citations_resolve.rs` if anything cites them.
10. **Nothing in this design was measured.** Every byte figure is arithmetic
    over constants read from the tree: real per-sweep radial counts, real upper
    cut gate counts, real `texture uploads` / `overlay rasters` totals on a
    Tier-2 leg, and whether radar's current frame-thread cost is upload or
    tessellation are all unknown. *Settles it:* publish the unchanged-tree
    spread before quoting any delta, and name the arm on every figure.
