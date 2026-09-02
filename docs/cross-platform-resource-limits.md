# Cross-platform resource limits: an architecture

Status: **Stages 2–3 landed; §8–§11 approved 2026-09-01, not yet landed.** The
resolver this document designed exists at `squallar-device-profile/src/budget.rs`:
`DeviceProfile` (`:85`) → `resolve` (`:639`) → one immutable `Budgets` (`:500`),
21 `Bracket` fields per bracket set (`BudgetLimits`, `:301`), stepped down by
`demote` (`:679`) in the §4.3 order, with web promotion on the adapter's reported
ceilings (`reported_promotion`, `:120`). This revision (2026-09-01) re-resolves
every citation at main `e2c1e664`, records the user's rulings of 2026-09-01
(§7.5), marks which of §7's open decisions they settle, and adds §8–§11 for the
campaign now approved — the plan file is
`~/.claude/plans/what-actually-exists-in-squishy-conway.md`.

Every figure below is read from the source at the cited symbol or computed from
cited constants; where I computed, I say so and show the arithmetic. A `:line`
was re-resolved at `e2c1e664` on 2026-09-01; a citation with a file and symbol
but no line was verified to exist in that file the same day. Figures that were
measured for the 2026-08-12 draft and whose source comment has since been
rewritten are marked *(f43c464f)* — that is the last revision of this document
on branch `plan/cross-platform-resource-limits`, which is where it lived until
now (it was never on main). Numbers in §8–§11 come from the plan file and say so.

Target of the plan: *"no-compromises when the hardware resources are available,
and only get reduced down to garbage-ass versions when we're left literally no
other option"* — expressed as an app-wide mechanism for deciding and spending
budgets across every target. Six rows, not five: Firefox and Chromium are
separate profiles on the same machine and Firefox governs (§0.1, §5.0). Ruling 5
(§7.5) sharpens the target: capacity is never *demoted*, and the same scene costs
the same bytes on every machine — what a big machine buys is *economy*, not a
bigger picture (§9).

---

## 0. Corrections to the brief (verify-first results)

Six claims in the brief needed correcting. They matter because two of them
change what the plan has to build. The "Evidence" column is where each fact
lives today; what became of each finding is in the last column.

| Claim | Verdict | Evidence (today) | Since |
| --- | --- | --- | --- |
| "Every resource budget is a hardcoded compile-time `cfg` cascade. Nothing measures the machine." | **False as stated.** One subsystem — the loop pool — already resolved a runtime budget from a device signal, clamped it into a compile-time floor/ceiling pair, learned from allocation failure, and persisted what it learned. | `squallar-app/src/loop_pool.rs:321` (`LoopPool::for_device`), `:352` (`back_off`), `:28` (`LoopPoolLimits`), `:24` (`LOOP_POOL_KEY`) | Generalised into `resolve` (Stage 2). The persisted half is **overruled** by ruling 6 — §7.5, §10. |
| `VOXEL_TEXTURE_BUDGET_BYTES` "Not a runtime check — nothing measures against it" | **The doc was stale.** It has a runtime consumer: the job decoder refuses a request whose cell count exceeds it. | doc at `squallar-radar/src/voxel.rs:557-558` (now reads "What one grid's index plane may occupy"); consumer at `squallar-radar/src/jobs.rs:523` | Stage 1 item 1 done: the doc was corrected. |
| `VOLUME_TEXTURE_BUDGET_BYTES` "Not a runtime check … exactly like `LOOP_POOL_FLOOR_BYTES`" | **Half stale.** The claim about itself is true (only `const _: () = assert!` at `squallar-volumetric/src/volume_raymarch.rs:2960` and the tests). The comparison to `LOOP_POOL_FLOOR_BYTES` is wrong — that one *is* measured against, at `LoopPoolLimits::for_target` (`squallar-app/src/loop_pool.rs:39-44`) and `squallar-app/src/app_render.rs:931-934`. | | Both are `Budgets` fields now (`volume_texture_bytes`, `loop_pool_floor_bytes`). |
| `APP_TEXTURE_BUDGET_BYTES` has no runtime consumer | **True, verified** at f43c464f: every reference was a doc comment or a test. | Today it is the *floor* of the `app_texture_ceiling_bytes` bracket (`squallar-device-profile/src/budget.rs:387,427,469-473`), and `demote` moves the resolved field (`:698`). Nothing at runtime yet *clamps* against it — the sum proof is still host-test only (`squallar-device-profile/src/constants/tests.rs:116`). | Runtime clamp-and-log is WO-9 (§9). |
| `MAX_LOOP_RENDER_BUDGET` is "the binding one", implying `MAX_LOOP_FRAMES` is not | **Both bind, on different resources.** `MAX_LOOP_RENDER_BUDGET` caps *textured* frames via `LoopFrameModel::render_budget`; `MAX_LOOP_FRAMES` caps frames *held* for 2D loops. | `LoopFrameModel::from_budgets` (`squallar-app/src/loop_pool.rs:159`), `LoopPool::plan` (`:362`); `Budgets::loop_frames_held` read at `squallar-app/src/app_render.rs:5368-5376` and `squallar-app/src/app.rs:536` | Both are `Budgets` fields (`loop_render_budget`, `loop_frames_held`). |
| Desktop 3D loop sits at ~504 MiB of 512 MiB (98.4%) | **Confirmed by my own arithmetic at f43c464f.** Grid+mips+LUT desktop = 8,388,608·4 + 1,048,576·4 + 1024 = 37,749,760 B. 13 loop frames + 1 live grid = 14 · 37,749,760 = 528,496,640 B = **504.0 MiB of 512 MiB = 98.44 %**. | *(f43c464f)* Historical: the desktop floor is now `DESKTOP_LOOP_POOL_FLOOR_BYTES` = 576 MiB (`squallar-device-profile/src/constants.rs:321`) and `DESKTOP_MAX_LOOP_VOLUME_FRAMES` no longer exists (next paragraph). The 3D frame count is computed by `LoopPool::plan`, not pinned. | Not re-derived here. |

Two further findings not in the brief, and what became of them:

- **`MAX_LOOP_VOLUME_FRAMES` (8/12/13) had no runtime consumer at all.** Only
  the tests read it; the real 3D loop frame count is computed at runtime by
  `LoopPool::plan` (`squallar-app/src/loop_pool.rs:362`). It was
  documentation-as-test, not a budget. **Deleted since** (Stage 1 item 2): no
  file in the workspace names it at `e2c1e664`.
- **A runtime budget already escaped the compile-time proof.** The volume
  store's bound is `loop_allocation().volume_reserve_bytes().max(…)`, and the
  `.max()` floors the store at the whole loop pool *floor* regardless of how
  much of the pool the 2D loops have already been promised. Today that reads
  `.max(self.budgets.volume_loop_bytes())` (`squallar-app/src/app_render.rs:931-934`,
  `Budgets::volume_loop_bytes` at `budget.rs:560`). At f43c464f the sum proof
  did not model the override; I walked the reachable pane configurations and
  could not construct an overrun (worst case found: 1 volume loop + 5 plan-view
  loops ⇒ ~2888 MiB against a 3072 MiB pool ceiling *(f43c464f)*), so it was a
  gap in the proof, not a live bug. **Closed since** (Stage 1 item 3):
  `the_whole_application_fits_its_gpu_ceiling`
  (`squallar-device-profile/src/constants/tests.rs:116-135`) now sums "a loop
  pool + a volume-store floor + panes × offscreen" — its own failure message
  names the volume-store floor term.

**The single most important consequence:** the mechanism the user asked for
already existed in miniature and was documented to a standard the rest of the
app should be held to. This plan was *generalise `LoopPool`*, not *invent
something*. That is what Stage 2 did.

### 0.1 "Web" is not one target — Firefox governs

Folded in after the first draft, and it changes the target matrix rather than
decorating it. Firefox is the first-class browser; where Firefox and Chromium
disagree, Firefox's answer is the one the app is held to.

This is the codebase's practice, not a new convention:

- `squallar-egui/src/ui_input.rs:41` — `PX_PER_WHEEL_LINE = 20.0`, paired with
  `POINTS_PER_ZOOM_LEVEL = 120.0` (`squallar-egui/src/ui_region.rs:133`). At
  f43c464f the comment beside them said why: *"Firefox reports six lines per
  notch and 6 × 20 lands on Chromium's 120."* The sentence has since been
  shortened out of the source; the two constants and their product survive. The
  calibration is **on Firefox** and Chromium is the one made to agree.
- The web backend choice. At f43c464f a `const _: () = assert!` failed the
  build if `wgpu`'s `webgpu` feature was re-enabled, *"because Firefox has no
  stable WebGPU."* That assertion is gone; what replaced it is a runtime
  decision that still centres Firefox: `create_instance`
  (`squallar-app/src/app.rs:95-97`) uses wgpu's
  `new_instance_with_webgpu_detection`, which drops `BROWSER_WEBGPU` when
  `requestAdapter()` returns null, and its doc comment records that "on
  Firefox/Linux — which governs here, and where WebGPU is still unshipped —
  that is every run". Every Firefox leg is therefore WebGL2 today, and Chromium
  may be WebGPU. §8 turns that difference into two different *capacity* arms.

**Why this is a stronger argument for the runtime profile than the 3090-vs-iGPU
one.** A `cfg(target_arch = "wasm32")` arm is *structurally* incapable of
distinguishing Firefox from Chromium: they are the same binary, served from the
same origin, running on the same machine, differing only in what the adapter
reports at runtime. If the two report different `max_texture_dimension_2d/3d`,
different `TextureFormatFeatures`, or different downlevel flags, then there is no
compile-time expression of the correct budget — not a coarse one, *none*. The
3090-vs-iGPU case is a cascade that is too coarse; the Firefox-vs-Chromium case
is a cascade that cannot express the question at all.

And it is already the sanctioned path: `adapter.limits()`, `adapter.features()`
and `AdapterInfo` are explicitly permitted under the parity rule, and the app
runs a full runtime capability probe on exactly this seam —
`squallar_volumetric::probe(adapter, limits)`
(`squallar-volumetric/src/lib.rs:123-135`), which checks limits
(`limits_shortfall`, `:191`) *and* `get_texture_format_features(VOLUME_TEXTURE_FORMAT)`
for `TEXTURE_BINDING` and `FILTERABLE` (`format_shortfall`, `:240`) and returns a
human-readable reason. That function is the existing, working answer to
"available here, absent there on the same hardware". Nothing new is needed for
capability; what was missing was the *budget* half, and what is still missing is
the *capacity* half (§8).

---

## 1. The complete inventory

### 1.1 `cfg`-gated numbers

Sweep method (f43c464f): every `#[cfg(...)]` attribute immediately preceding a
`const`/`static` in every non-vendor, non-target `.rs` file in the workspace,
plus a `target_os = "android"` / `"ios"` pass. Nineteen cascades gated a number.
The remaining `cfg(target_arch = "wasm32")` hits were `Send` bounds, a sequential
rayon shim (`squallar-radar/src/par.rs`, whose header now says "Nothing here
reads a `cfg`"), runtime-shape shims (`squallar-egui/src/tile_source.rs`), or
HTTP client construction (`squallar-source/src/tls.rs:33,60,80`) — no numbers.

**What changed since (Stage 2).** Every row below is now a `Bracket` field of
`BudgetLimits` (`squallar-device-profile/src/budget.rs:301`), one set per arm
(`WASM`, `MOBILE`, `DESKTOP`), resolved into the `Budgets` field named in the
table by `resolve` (`:639`). The `cfg` constants remain, as the bracket
*definitions*, exactly as §6 Stage 2 proposed. Consumers read `Budgets`; the
exceptions that still read a constant directly are named. Three rows were added
since f43c464f and are listed at the foot. Values are the arms at `e2c1e664`;
where a value moved since f43c464f the old one is in parentheses.

Legend for **Binds**: *runtime* = a non-test code path reads the resolved field
and changes behaviour; *const-assert* = only a `const _: () = assert!`;
*test-only* = only `#[test]` / `#[cfg(test)]` reads it.

All `constants.rs` paths below are `squallar-device-profile/src/constants.rs`.

| # | Constant | Cascade at | Arm values (wasm / mobile / desktop) | Arms named at | `Budgets` field | Binds |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | `LONG_RANGE_IMAGE_SIZE` | `constants.rs:16-21` | 2048 (= `WEBGL2_MAX_TEXTURE_DIMENSION_2D`) / 4096 / 4096 | `:11-14` | `long_range_image_side_px` | **runtime** — floors `raster_side_ceiling_px` (`budget.rs:670-673`), spent by `Budgets::raster_side_for_adapter` at `squallar-app/src/app_state.rs:90` |
| 2 | `LOOP_IMAGE_SIZE` | `constants.rs:83-88` | 1024 / 2048 / 2048 | `:79-81` | `loop_image_side_px` | **runtime** — `LoopFrameModel::from_budgets` (`squallar-app/src/loop_pool.rs:159`) |
| 3 | `MAX_CONCURRENT_RENDERS` | `constants.rs:159-164` | 1 / 3 / 6 | `:120,156,157` | `concurrent_renders` | **runtime** — `squallar-app/src/render_dispatch.rs:650` (held), read at `:671,1667,1753,1768,1817,1875`; also `FrameInputs::concurrent_renders` (`squallar-egui/src/shell_api.rs:89`) |
| 4 | `MAX_LOOP_RENDER_BUDGET` | `constants.rs:190-195` | 14 / 18 / 36 (was 8 / 12 / 30) | `:185-187` | `loop_render_budget` | **runtime** — `LoopFrameModel::from_budgets`, capped in `LoopPool::plan` (`loop_pool.rs:366`) |
| 5 | `MAX_CONCURRENT_LOOP_DOWNLOADS` | `constants.rs:198-201` | 8 / 4 / 8 (`mobile` only; **wasm takes `NON_MOBILE_`**) | `:203-204` | `concurrent_loop_downloads` | **runtime** — `squallar-app/src/app_render.rs:3250,3313` |
| 6 | `MAX_LOOP_FRAMES` | `constants.rs:208-213` | 14 / 20 / 60 (was 12 / 20 / 60) | `:215-217` | `loop_frames_held` | **runtime** — `squallar-app/src/app_render.rs:5368-5376`, `squallar-app/src/app.rs:536` |
| 7 | `LOOP_POOL_FLOOR_BYTES` | `constants.rs:312-317` | 56 / 288 / 576 MiB (was 48 / 256 / 512) | `:319-321` | `loop_pool_floor_bytes` | **runtime** — `LoopPoolLimits::from_budgets` (`loop_pool.rs:47`) |
| 8 | `LOOP_POOL_CEILING_BYTES` | `constants.rs:325-330` | 192 / 640 / 3072 MiB | `:332-334` | `loop_pool_ceiling_bytes` | **runtime** — same |
| 9 | `VOLUME_LOOP_TEXTURE_BUDGET_BYTES` | `constants.rs:359` (alias of #7) | 56 / 288 / 576 MiB | `:360-362` | `Budgets::volume_loop_bytes()` (`budget.rs:560`) | **runtime** — `squallar-app/src/app_render.rs:931-934` |
| 10 | `MAX_LOOP_VOLUME_FRAMES` | *deleted* | (was 8 / 12 / 13) | — | — | was **test-only**; retired (Stage 1) |
| 11 | `APP_TEXTURE_BUDGET_BYTES` | `constants.rs:373-378` | 288 / 1024 / 3840 MiB (was 256 / 768 / 3840); desktop ceiling `DESKTOP_APP_TEXTURE_CEILING_BYTES` 4032 MiB (`:386`) | `:380-382` | `app_texture_ceiling_bytes` | **test-only** still — the sum proof (`constants/tests.rs:116,138`) and `demote` (`budget.rs:698`); no runtime clamp (WO-9) |
| 12 | `MAX_RENDER_CACHE_ENTRIES` | `constants.rs:389-392` | 8 / 4 / 8 (`mobile` only) | `:394-395` | `render_cache_entries` | **runtime** — `RenderCache::new` at `squallar-app/src/render_dispatch.rs:646-647` |
| 13 | `VOLUME_GRID_CELLS` | `constants.rs:406-411` | [128,128,64] / [192,192,96] / [256,256,128] = 1,048,576 / 3,538,944 / 8,388,608 cells | `:399-401` | `grid_cells` | **runtime** — `squallar-app/src/app.rs:1268`, `loop_pool.rs:167`; `volume_grid_shape_of` (`constants.rs:420`). **Exception:** `limits_shortfall` still reads the constant (`squallar-volumetric/src/lib.rs:192`) |
| 14 | `VOLUME_TEXTURE_BUDGET_BYTES` | `constants.rs:442-447` | 6 / 20 / 48 MiB | `:449-451` | `volume_texture_bytes` | **const-assert** (`squallar-volumetric/src/volume_raymarch.rs:2960`) plus `demote` rung 3 |
| 15 | `VOLUME_OFFSCREEN_BUDGET_BYTES` | `constants.rs:466-470` | 5 / 5 / 20 MiB; desktop ceiling `DESKTOP_VOLUME_OFFSCREEN_CEILING_BYTES` (48 MiB per the bracket's comment, `budget.rs:453-462`) | `:457-459` | `offscreen_bytes` | **runtime** — `squallar-app/src/app.rs:1085` → `squallar-volumetric/src/volume_bridge.rs:548` |
| 16 | `VOLUME_MIRROR_BYTES_MAX` | `constants.rs:478-482` | 16 / 16 / 64 MiB (wasm arm = `MIRROR_MAX_SIDE`² · 4, `:485`; `MIRROR_MAX_SIDE` = 2048 at `:473`) | `:485-487` | `mirror_bytes` | **runtime** — `squallar-app/src/app_render.rs:2804` → `MirrorLimits::for_device` (`squallar-gpu/src/egui_renderer/mirror.rs:79`) |
| 17 | `quality::PLATFORM_CEILING` | `squallar-device-profile/src/quality.rs:235-239` | Half+Off / Half+Off / BEST | `:216,223,229` | `quality_ceiling` | **runtime** — `quality::select(class, self.budgets.quality_ceiling)` at `squallar-app/src/app.rs:1061` |
| 18 | `squallar_radar::types::IMAGE_SIZE` | `squallar-radar/src/types.rs:27,29` | 2048 / 2048 (wasm vs native; **identical**) | `:13,16` | `image_side_px` | **runtime** — `types.rs:84,111`, `squallar-radar/src/render.rs:1235,1299,1717,2226` read the constant |
| 19 | `squallar_radar::xsect::SECTION_WIDTH` | `squallar-radar/src/xsect.rs:47,50` | 1024 / 2048 | `:40,43` | `section_width_px` | **runtime**. **Exception:** `squallar-egui/src/ui_section_pane.rs:6,903` still reads the constant |
| 20 | `LOOP_SPAN_BUDGET_SECS` *(new)* | `constants.rs:176-181` | 45 min / 60 min / 120 min | `:167-169` | `loop_span_secs` | **runtime**. §9 names it a capacity presumption in disguise |
| 21 | `RASTER_SIDE_CEILING` *(new)* | arms only | 2048 / 4096 / 8192 | `:34,30,28` | `raster_side_ceiling_px` | **runtime** — `app_state.rs:90` |
| 22 | `MAX_PANES_DESKTOP` / `MAX_PANES_MOBILE` *(moved)* | `squallar-device-profile/src/budget.rs:294,297` | — / 4 / 6 | same | `max_panes` | **runtime** via `WidthClass` (`squallar-egui/src/ui_layout.rs:19,43-44`), which reads the constants directly |

### 1.2 Per-target numbers that are *not* `cfg`-gated but are still per-target

| Constant | At | Value | Selected by | Binds |
| --- | --- | --- | --- | --- |
| `WASM_SHAPE` / `MOBILE_SHAPE` / `DESKTOP_SHAPE` | `squallar-radar/src/voxel.rs:562,569,576` | 128²·64 / 192²·96 / 256²·128 | caller (`squallar-radar` cannot see `mobile`) | **runtime**, via `volume_grid_shape` |
| `VOXEL_TEXTURE_BUDGET_BYTES` | `squallar-radar/src/voxel.rs:558` | 8 MiB flat — one byte per cell of the largest index plane | — | **runtime** — `squallar-radar/src/jobs.rs:523` |
| `MIRROR_MAX_SIDE` | `squallar-device-profile/src/constants.rs:473` | 2048 | fallback; raised at runtime by `MirrorLimits::for_device` (`squallar-gpu/src/egui_renderer/mirror.rs:79`) | **runtime** |
| `WEBGL2_MAX_TEXTURE_DIMENSION_3D` | `constants.rs:439` | 256 | — | **runtime** — argument to `volume_grid_shape` (`:432`) |
| `WEBGL2_MAX_TEXTURE_DIMENSION_2D` | `squallar-radar/src/types.rs:20` | 2048 | — | **runtime** — `AdapterCeilings::WEBGL2_GUARANTEE` (`budget.rs:36`) |
| `DESKTOP_CLASS_REPORT` *(new)* | `budget.rs:44` | {16384, 8192} — "the componentwise least either desktop-class machine this project has **measured** a browser report on" | — | **runtime** — `reported_promotion` (`:120`) |
| `MIN_LOOP_FRAMES_PER_PANE` | `constants.rs:337` | 2 | — | **runtime** (the never-blank floor) |
| `LOOP_POOL_HYSTERESIS` / `LOOP_POOL_DWELL_FRAMES` | `constants.rs:345,349` | 1.25 / 15 | — | **runtime** — `LoopPoolState::observe` (`loop_pool.rs:421`) |
| `MAX_LOOP_VOLUME_BUILDS_PER_FRAME` / `MAX_LOOP_SECTION_CUTS_PER_FRAME` | `constants.rs:369,223` | 1 / 1 | — | **runtime** (frame-time pacing, not memory) |
| `VOLUME_OFFSCREEN_REFERENCE_PANE_PX` | `constants.rs:454` | [2560, 1440] | — | **runtime** |
| `TILE_CACHE_ENTRIES` / `PARSED_TILE_CACHE_ENTRIES` *(new here)* | `squallar-egui/src/tile_source.rs:182-195` — its **own** cascade on `any(target_os = "android", target_os = "ios")`, not `mobile` | 100 / 100 / 256 and 24 / 24 / 96 (plan, "What exists") | — | **runtime**; wholly outside `Budgets` — §11 |

### 1.3 Non-GPU pressure with no budget at all

| Thing | At | Size | Bounded by |
| --- | --- | --- | --- |
| `POOLED_CELLS` | `squallar-radar/src/render.rs:249` | `IMAGE_SIZE² · 8` = **33,554,432 B**, i.e. *above* glibc `DEFAULT_MMAP_THRESHOLD_MAX`; measured cliff at 33,554,393 B (0 minor faults at 33,554,392; 8,193 at 33,554,432, glibc 2.44) *(f43c464f — the source comment that carried this has since been rewritten around a capacity-slack rule, `render.rs:251-262`)* | depth **1** |
| `POOLED_IMAGE`, `POOLED_VALUES` | `render.rs:354,357` | 2048²·4 = 16,777,216 B each | depth **1** each |
| `POOLED_PLANES` | `squallar-radar/src/xsect.rs:504` | three planes of one section | depth **1** |
| wasm worker→main index plane | one byte per cell (`squallar-radar/src/voxel.rs:557`): 1 / 3.375 / 8 MiB by shape; crosses the worker wire | nothing. *(The `channels.rs:191-193` citation at f43c464f no longer resolves; the crossing was not re-traced in this revision.)* |
| wasm linear memory | `squallar-device-profile/src/constants.rs:130-146` | **1 GiB per module instance**, not 4 GiB: the module declares `maximum=16384 pages`, set by `--max-memory=1073741824` at `.github/scripts/wasm-threads.sh:132`. The source itself says "This doc previously said 'a hard 4 GiB'… it is **not this build's ceiling**". And there are **two** instances — §8. | nothing today; the watermark of §10 |

**The depth-1 pools are the sharpest unbudgeted item.** `MAX_CONCURRENT_RENDERS`
is 6 on desktop and every one of the 6 concurrent renders wants the same slot, so
5 of 6 miss. Whether that is worth fixing is a *measurement* question — the
source argues against a free list (`render.rs:384-390`: a misfit is dropped on
its own statement so the two buffers' peak is not the sum of both) — but it is a
resource decision currently made with no budget and no signal, and it is
downstream of a number (`concurrent_renders`) that is now runtime-resolved.

---

## 2. The proposed architecture (landed as Stage 2)

### 2.1 One idea, stated once

> Every per-target number becomes a **pure function of a `DeviceProfile`**, held
> inside a **compile-time `[floor, ceiling]` pair**, resolved **once** at device
> creation into an immutable **`Budgets`** struct that every subsystem reads
> instead of a `cfg` constant — and that can only ever step **down**, from
> observed failure, never up.

That is `LoopPool` generalised. Nothing here is novel; the novelty is that there
is one of them instead of one per subsystem, and that the ordering *between*
subsystems becomes expressible.

**Amended by ruling 5 (§7.5, §9):** "only ever step down" described the memo
era. Under the approved model the budgets a session runs at are `fit(scene,
limits, capacity)` — a pure function that moves *both ways* with the scene and
the session's capacity presumption — and nothing is remembered between sessions.

### 2.2 Where it lives

`squallar-device-profile/src/budget.rs`, in the crate whose `build.rs` emits
`mobile` (`squallar-device-profile/build.rs:21`; the rule is
`squallar-device-profile/src/mobile_cfg.rs`). The crate is `#![forbid(unsafe_code)]`
and depends on `squallar-radar` and `log` only; signals arrive as plain data.
Consumers in `squallar-radar`, `squallar-egui`, `squallar-volumetric` and
`squallar-gpu` take budgets as **arguments**, exactly as
`squallar_radar::voxel::shape_for_budget` does and for the reason
`constants.rs:396-398` gives ("named outside the `cfg` cascade so that all three
are reachable from any target's tests").

### 2.3 `DeviceProfile` — the input

As landed (`squallar-device-profile/src/budget.rs:85-107`):

```rust
pub struct DeviceProfile {
    pub platform: Platform,          // Native | Web — which APIs exist, from cfg, once, at the seam
    pub limits: BudgetLimits,        // the bracket set this binary was built with
    pub class: DeviceClass,          // driver's classification; Unknown on every browser
    pub adapter: AdapterCeilings,    // max_texture_dimension_2d / _3d, as reported
    pub vram_bytes: Option<u64>,     // declared; populated None; never read  ← WO-8
    pub system_ram_bytes: Option<u64>, // declared; None; never read           ← WO-2
    pub parallelism: usize,          // declared; 1; never read                 ← WO-2
    pub form_factor: Option<FormFactor>, // declared; None; never read          ← WO-2/11
    pub memo: Option<BudgetMemo>,    // what a previous session learned by failing — OVERRULED, ruling 6
}
```

`DeviceProfile::for_target` (`:139-155`) is the profile before an adapter has
been met: `AdapterCeilings::WEBGL2_GUARANTEE` (`:36`), `DeviceClass::Unknown`,
every optional socket empty. `promotion()` (`:111`) is `Promotion::for_class`
(`:64`: `Discrete → Ceiling`, `Integrated → Step`, `Virtual | Unknown | Software →
Floor`) unless the class is `Unknown`, in which case `reported_promotion` (`:120`)
compares the adapter's 2D/3D caps against `DESKTOP_CLASS_REPORT {16384, 8192}`
(`:44`) — `Ceiling` if both are met, else `Floor`. That is the whole of what
promotes a browser today; §8 and D3 of the plan make `Step` mean something.

`platform: Platform` rather than `cfg!()` read inline is the crux of testability
and is the shape `quality::select` (`squallar-device-profile/src/quality.rs:244`)
already used. It is **not** a behavioural split: the resolver is one function
that takes it as data, and every arm is reachable from one host test.

**`Platform` has exactly two variants and deliberately does not name a browser.**
There is no `Browser::{Firefox, Chromium}` in `DeviceProfile` and there must not
be: a browser-name switch is a user-agent sniff by another name, it is spoofable,
and it goes stale. Firefox and Chromium are distinguished the way the parity rule
sanctions — by **what they report**. Two browsers on one machine that report
different `max_texture_dimension_3d` produce two different `AdapterCeilings` and
therefore two different `Budgets`, with no browser-identity term anywhere. Firefox
"governing" is expressed not as a branch but as a **bracket**: the web
floor/ceiling pair is chosen so that Firefox's reported figures land inside it
comfortably, and a Chromium that reports more simply resolves higher within the
same bracket. §8 adds a second, larger difference that is still not a name: a
WebGPU adapter can be *probed* for capacity and a WebGL2 one cannot.

### 2.3.1 Capabilities are separate from budgets, and already solved

`DeviceProfile` carries capability answers alongside budget inputs, but they are
resolved by a different function and must not be conflated:

- **Capability** = *can this device do the thing at all?* Answered by
  `squallar_volumetric::probe(adapter, limits)`
  (`squallar-volumetric/src/lib.rs:123-135`), which returns
  `VolumeSupport::Unavailable(reason)` (`squallar-volumetric/src/volume_degrade.rs:7`)
  with a human-readable string. It checks four limits (`limits_shortfall`,
  `lib.rs:191-234`) and two format-feature flags — `TEXTURE_BINDING` and
  `FILTERABLE` on `VOLUME_TEXTURE_FORMAT` (`format_shortfall`, `:240-255`).
- **Budget** = *how much of the thing can it afford?* This document's subject,
  now split further into need, capacity and economy (§9).

Keep the split. It is what makes "available in Firefox, absent in Chromium on the
same box" a first-class outcome rather than a surprise: the capability probe
runs per-adapter, per-launch, and produces a reason the UI can show. The ladder
(§4.3) is indexed on the **resolved capability set**, never on `Platform` — a
rung that depends on a feature must ask the profile whether the feature is
present, not ask whether this is the web build.

The one place that could go wrong is `Rg16Float` filterability.
`squallar-volumetric/src/lib.rs:50-58` argues it from the ES 3.0 spec (Table 3.13)
and from the format's relative quantisation — an 8-bit format's error is
absolute, `|Δindex| ≤ 2q / Ḡ` with `q = 1/255`, "the whole palette one cell out
from an echo edge" — which is a good argument; but it is an argument, and the
probe checks it at runtime anyway. That is the right belt-and-braces.

`vram_bytes: Option<u64>` is deliberately an `Option`, and the `None` arm is not
a fallback bolted on — it is the *majority* arm. Every WebGL2 browser is `None`.
Every target where a reader turns out untrustworthy is `None` (the Software /
Virtual lie-guard, §8). The resolver produces a good answer from `class + adapter`
alone, and a capacity figure is a *promotion* signal on top, never a
prerequisite.

`DeviceClass` is reused, not re-invented — `squallar_gpu::device::device_class_of`
(`squallar-gpu/src/device.rs:105-108`) classifies `AdapterInfo::device_type`
exhaustively into `squallar_device_profile::quality::DeviceClass`
(`quality.rs:177`), and `loop_pool.rs` reuses it rather than inventing a parallel
enum.

### 2.4 `Budgets` — the output

One immutable struct (`squallar-device-profile/src/budget.rs:500`), resolved
once, threaded from `AppState`. As landed, the 21 resolved fields are:
`image_side_px`, `long_range_image_side_px`, `loop_image_side_px`,
`section_width_px`, `concurrent_renders`, `concurrent_loop_downloads`,
`loop_frames_held`, `loop_span_secs`, `loop_render_budget`,
`loop_pool_floor_bytes`, `loop_pool_ceiling_bytes`, `grid_cells`,
`volume_texture_bytes`, `offscreen_bytes`, `mirror_bytes`,
`render_cache_entries`, `quality_ceiling`, `max_panes`,
`app_texture_ceiling_bytes`, `raster_side_ceiling_px` — plus `name`,
`promotion` and `steps_back` for the record (`resolve`, `:639-676`).

**How a subsystem asks for its budget.** It does not ask. It is *handed* the
`Budgets` it needs at construction, the same way `RenderCache::new` is handed its
capacity (`squallar-app/src/render_dispatch.rs:646-647`) and `MirrorLimits` is
handed a texture dimension and a byte cap
(`squallar-gpu/src/egui_renderer/mirror.rs:79`). No global, no `OnceLock`, no
`thread_local`. A global would be untestable in the matrix, and the existing
code proves the argument-passing style scales.

### 2.5 `BudgetLimits` — the compile-time bracket that survives

For every field of `Budgets` there is a compile-time bracket per platform arm:
`Bracket { floor, step, ceiling }` (`budget.rs:161`) built by `pinned` (`:172`),
`new` (`:181`) or `stepped` (`:190`) and read by `at(Promotion)` (`:204`);
`CellBracket` (`:215`) and `QualityBracket` (`:262`) for the two non-scalar
fields. The arm values are named outside the cascade (the pattern at
`constants.rs:399-401`, `:319-321`, `:332-334`, `quality.rs:216-229`).

- **floor** = a *decision*: the worst device this build is willing to work on.
  Never crossed downward, whatever happens. This is what the wasm const-assert
  guards (`VOLUME_GRID_FLOOR_SHAPE`, `constants.rs:432`, asserted in the const
  block at `:507`).
- **ceiling** = a *guard against a lie*: the most this build will ever spend even
  if the device claims infinity. Protects against a spoofed `deviceMemory`, a
  misread heap budget, and a driver that reports a shared-memory iGPU's whole
  DRAM as VRAM.

At f43c464f this section ended: *"The user's 'no compromises' requirement is
expressed as: raise the desktop ceilings."* **Overruled** by rulings 3 and 5
(§7.5): where capacity is *measured*, no `*_CEILING_BYTES` constant binds. The
brackets become floors (the worst device), three presumed capacities (for the
arm that cannot measure), cost functions, and one shed order — §9. The
lie-guard the ceiling provided moves to the capacity readers (§8): a Software or
Virtual adapter's reading is `None`, and `NEED_FRACTION` bounds what any reading
may be spent on.

### 2.6 The three seams that already existed and were reused, not duplicated

1. `LoopPool::for_device(class, remembered, limits)` —
   `squallar-app/src/loop_pool.rs:321`, now `for_promotion` (`:330`) at the
   `Promotion` the whole set resolved at. Became `resolve`'s loop row. §9
   replaces its input: `LoopPool::new(min(Σ loop need, room), limits)`.
2. `LoopPool::back_off(limits)` — `loop_pool.rs:352`, driven from
   `App::back_off_budgets` (`squallar-app/src/app_render.rs:3829`) on a lost
   surface (`wgpu::CurrentSurfaceTexture::Lost`, `:2744`). Became the app-wide
   **demotion** path: `back_off_budgets` increments `BudgetMemo::steps_back` and
   re-resolves, and `demote` (`budget.rs:679`) walks that many rungs. §10
   replaces the counter with reclaim-then-re-fit.
3. `KvStore` (`squallar-kv/src/lib.rs:19`) key `LOOP_POOL_KEY` (`loop_pool.rs:24`)
   — its own entry, written synchronously, because a value learned by crashing
   the GPU must not be lost to the 3 s autosave timer. Became `BudgetMemo`
   under `BUDGET_MEMO_KEY = "budget_steps"` (`squallar-app/src/budget_memo.rs:7`).
   **Overruled** by ruling 6 (§7.5): nothing is learned across sessions. Both
   keys stop being read (a stale key is harmless; kv has no delete) — §10.

### 2.7 What the `mobile` cfg becomes

**Keep it, narrow its job.** `mobile` stops selecting budgets and selects only the
*floor/ceiling bracket*, which is legitimately a compile-time fact about which
binary is being built. The honest taxonomy — "the `cfg` tells you which **APIs
exist**, and the device class, discovered at runtime, tells you what the machine
**is**" — was stated at length in the `loop_pool.rs` module doc at f43c464f
(§8 records that survey) and survives in short form at
`squallar-app/src/loop_pool.rs:10-11`: "`mobile` is a cfg for native
Android/iOS: a browser on a phone is `wasm32`, not `mobile`."

Collapsing `mobile` entirely is tempting and I recommend **against** it:

- `Platform::{Native, Web}` genuinely differs in API surface (the WebGL2 limit
  negotiation in `squallar_gpu::device::device_limits`,
  `squallar-gpu/src/device.rs:59`, is not a budget, it is a capability).
- A native Android build and a native desktop build differ in what the *floor*
  may safely be even before any device is seen, because the floor is a promise
  about the worst shipped device, and those populations do not overlap.
- Deleting `mobile` would delete the middle arm from
  `the_grid_dimensions_match_the_shapes_squallar_radar_names`
  (`squallar-device-profile/src/constants/tests.rs:761`), which is the only
  thing binding this crate's copy of the shapes to `squallar-radar`'s
  (`constants.rs:396-398` explains why the duplication is forced). Losing that
  binding costs more than the cfg does.

What *did* go away: `mobile` no longer gates 19 numbers, it gates one of three
bracket sets, and every bracket is reachable from one host test because every
arm is named outside the cascade.

---

## 3. The compile-time-proof tension, and my answer

### 3.1 What is actually at risk

Reading them, the pinned tests split into three kinds, and only one is genuinely
threatened:

**(a) Floor proofs — survive untouched.** `VOLUME_GRID_FLOOR_SHAPE`'s const-assert
(`constants.rs:432`, the const block at `:507` with the grid-axis assertion at
`:564`) asserts that the shape a device *reporting exactly the WebGL2 guarantee*
receives fits that guarantee. It is already a statement about the floor, not
about the resolved value. A runtime budget does not touch it. *Landed as
stated.*

**(b) Cross-crate binding proofs — survive untouched.**
`the_grid_dimensions_match_the_shapes_squallar_radar_names`
(`constants/tests.rs:761`) and `every_named_shape_fits_the_texture_budget`
(`squallar-radar/src/voxel/tests.rs:399`) bind two crates' copies of the same
literal. They are about *duplication*, not about *devices*. *Landed as stated.*

**(c) Sum proofs — genuinely threatened.**
`the_whole_application_fits_its_gpu_ceiling` (`constants/tests.rs:116`),
`the_app_ceiling_is_not_slack_enough_to_hide_a_doubling` (`:138`),
`the_volume_grid_fits_the_target_texture_budget`
(`squallar-volumetric/src/volume_raymarch/tests.rs:1684`),
`the_volume_budget_is_not_slack_enough_to_hide_a_doubling` (`:1702`). These
assert a relation between numbers that were about to stop being numbers.

**But note what (c) already was.** `constants/tests.rs` had a function `arms()`
returning a **struct of budgets per device class**, and every sum proof was
`for arm in arms() { … }`. The suite was *already* a pure function of a profile
struct. That is the whole answer, and it is what landed: `arms()` is now
`profiles().map(resolve)` — `constants/tests.rs:8` (`profiles`), `:21` (`arms`
returning `[Budgets; 3]`), with sibling copies in
`squallar-app/src/budget_arms.rs:21,26` and
`squallar-volumetric/src/budget_arms.rs:21,26`.

### 3.2 The three candidate directions, costed

| Direction | Cost | Verdict | Status |
| --- | --- | --- | --- |
| **A. Keep the compile-time proof over the floor configuration only; add runtime assertions above it.** | Cheap. Loses nothing that (a)/(b) already guard. But a runtime assertion above the floor either panics in front of a user or logs and is ignored; neither is a *proof*. | Necessary, insufficient alone. | Floor proofs kept. |
| **B. Convert the ceiling to a runtime invariant checked at budget-resolution time.** | Moderate. Failure mode must be *clamp-and-log*, never panic. | Adopt, as the enforcement. | **Not landed.** WO-9 (§9): on the measured arm the invariant becomes `need ≤ NEED_FRACTION × cap`, clamped and logged. |
| **C. Make the resolver a pure function of `DeviceProfile` so the whole matrix is unit-testable without a GPU.** | Moderate, and mostly already paid. | Adopt, as the proof. | **Landed.** `synthetic_profiles` (`squallar-device-profile/src/budget/tests.rs:179`), `check_invariants` (`:218`), `the_resolver_reproduces_every_shipped_constant` (`:31`). |

**Recommendation: all three, layered, in this order.**

1. **Floor is a compile-time const-assert.** The relation stays `const`. It is
   checkable on every target including the wasm `cargo check` row, and it is the
   only proof that matters for "does this run at all on the worst device".

2. **The resolver is a pure function and the matrix is the proof.**
   `fn resolve(profile: &DeviceProfile) -> Budgets`, no `cfg!` inside, no
   globals. The test is a walk over `synthetic_profiles()` asserting the sum
   proof, the snugness proof, monotonicity against the floor and the bracket,
   and that every grid axis fits `adapter.max_texture_dimension_3d`
   (`check_invariants`). The Firefox and Chromium rows were to be **pinned to
   measured figures** once they landed; what landed is `DESKTOP_CLASS_REPORT
   {16384, 8192}` (`budget.rs:44`) as the least measured desktop-class report,
   with `a_desktop_class_browser_is_promoted_and_a_spec_floor_browser_is_not`
   (`budget/tests.rs:606`) as the pair test. The plan's D1 adds a form-factor
   axis (720 → 2160 rows) and asserts byte-identity with and without the new
   inputs.

3. **The invariant is also checked at runtime, and clamps.** *Not landed* —
   WO-9.

**What this costs, honestly.** The compile-time proof stops covering the *shipped
desktop configuration* as a compile-time fact and starts covering it as a
host-test fact. That is a real reduction in strength, and the mitigation is that
the host test covers the whole matrix instead of 3 rows. I judged that a net
gain; it is a trade and the user saw it as one (§7 item 7, accepted).

**One thing I said I would not do, and was overruled on.** At f43c464f: *"make
`APP_TEXTURE_BUDGET_BYTES` itself device-derived… The moment the ceiling is also
measured, the snugness test degenerates into a tautology."* Ruling 3 (§7.5)
overrules the conclusion; §9 answers the premise. The constant-based snugness
tests survive **unchanged as statements about the presumed arm**, where the
constants still are the capacity, so they still bite there. On the measured
arm the invariant is relational — `need ≤ NEED_FRACTION × cap`, `need + economy
≤ ECONOMY_FRACTION × cap` — and a term whose arithmetic silently doubles is
caught where it already is, by the byte tests beside the arithmetic, not by the
ceiling.

---

## 4. The cross-subsystem degradation ladder

### 4.1 The never-degrades list

These are floors, not rungs. Below any of them the app is broken, not degraded.

1. **Correctness of the picture.** No budget may change what a pixel *means* —
   no palette quantisation, no dropping a product, no substituting a coarser
   product. `VOLUME_TEXTURE_FORMAT` staying `Rg16Float`
   (`squallar-volumetric/src/lib.rs:59`) is in this class and the reason is
   measured, not aesthetic (`lib.rs:50-58`: an 8-bit format's absolute error is
   the whole palette one cell out from an echo edge). **The half-float channel
   is never a degradation rung.**
2. **`MIN_LOOP_FRAMES_PER_PANE = 2`** (`constants.rs:337`). A loop that is not a
   loop reads as a bug the user cannot undo by guessing.
3. **Interactivity.** Pan, zoom, product switch and pane add must stay responsive.
   A budget may never be spent by making the frame thread wait. Eviction under
   pressure is bounded per frame and payloads are freed through
   `squallar_worker::offload::discard` (`squallar-worker/src/offload.rs:25`) —
   §10.
4. **One live 3D grid beside a looping one.**
   `a_full_3d_loop_leaves_room_for_a_live_grid_beside_it`
   (`squallar-volumetric/src/volume_raymarch/tests.rs:1640`) guards this and
   `LoopPool::plan` subtracts one grid before dividing
   (`squallar-app/src/loop_pool.rs:377`). Without it the loop's frame 0 is
   evicted and rebuilt at ~89 ms of resample *(f43c464f)*, for ever.
5. **The 1:1 reopen rule.** At f43c464f: *"A resolved budget is remembered; a
   session must not re-probe and show a different loop length on every start."*
   **Amended by ruling 6.** The property stands; the mechanism changes from
   persistence to *determinism*: `fit` is pure, so the same scene against the
   same capacity yields the same budgets on every start
   (`a_reopen_fits_the_same_scene_to_the_same_budgets`, plan §Pins, replacing
   `a_backed_off_machine_reopens_where_it_left_off` at
   `squallar-app/src/app/chunk_feed_precedence_tests.rs:1290`).

### 4.2 The top rung — "no compromises"

Defined, so it can be aimed at:

- Grid at the largest square the adapter's `max_texture_dimension_3d` will hold
  at the resolved cell budget. `squallar_radar::voxel::shape_for_budget` spends
  cells this way and it is free: 512×512×32 and 256×256×128 are the same
  8,388,608 cells (arithmetic; the passage arguing it at f43c464f
  `constants.rs:1029-1035` was not re-found at `e2c1e664`).
- `VolumeQuality::BEST` (`squallar-device-profile/src/quality.rs:153`) — cloud
  reconstruction, native offscreen resolution.
- Every loop at `loop_render_budget` frames over the full `loop_span_secs`.
- Full raster side on every pane, no long-range step-down.
- All six panes.

**Amended by ruling 5.** The top rung is a statement about the *scene*, not
about the machine: a device whose capacity covers the scene's need runs at it,
whatever the card's size, and holds not one byte of picture more than the scene
costs. A bigger card buys economy (§9) — a longer tile history, a fuller render
cache — never a different picture for the same scene.

### 4.3 The ladder, ordered

The ordering principle: **degrade what the user is least likely to notice, and
degrade smoothly before degrading discretely.** Each rung names its knob.

| Rung | What gives | Knob | Why here |
| --- | --- | --- | --- |
| 0 | *nothing* — top rung above | — | — |
| 1 | **3D lighting model** | `GradientShading::On → Off` (`quality.rs:129`) | The app's first rung and the reasoning is measured: on a 3090 the cloud rung is 0.766 ms dense vs 0.263 ms for the flat march at 1440×900 (`quality.rs:8`). The cheapest large saving, and the one a user is least likely to name. `quality.rs:27`: "The ladder degrades lighting before resolution". |
| 2 | **3D offscreen resolution** | `ResolutionRung::Native → Half → Quarter` (`quality.rs:93,101`) | ~3.4× per step at ~85 % efficiency (`quality.rs:14-15`). Blurrier, still correct, still interactive. |
| 3 | **Loop *history*, 2D before 3D** | `loop_render_budget`, halved toward `MIN_LOOP_FRAMES_PER_PANE` one halving a step; `loop_span_secs` stays the demand *(knob as landed, WO-7 2026-09-02)* | A shorter loop is the least destructive thing in the app: nothing on screen gets worse, there is just less of it. The pool already divides smoothly and floors. **2D loop frames go before 3D grid resolution** — §4.4. |
| 4 | **Tile sharpness** *(replaces "overlay texture area")* | whole-zoom snap: `tile_zoom = zoom.floor()` for the overrunning source | Fewer, larger tiles cover the same glass; the picture is less crisp, never wrong, and the ancestor net that keeps the map from going blank is never traded — §11. Placed above raster resolution because a coarser radar raster is a *wrong-looking* picture and a softer basemap is a *softer* one. |
| 5 | **3D grid cell budget** | `grid_cells` down the named brackets | Now the picture itself gets coarser. This is the first rung the user will call "worse", so it is deliberately late. |
| 6 | **2D raster side** | `raster_side_ceiling_px` → the long-range floor | Has a runtime path: `squallar-app/src/app_state.rs:90-98` resolves and logs "plan views may reach N px". Late because it is the most visible. |
| 7 | **Concurrency** | `concurrent_renders → 1`, `render_cache_entries → 1` | Not a picture change; a *latency* change. Placed last among memory rungs because rung 7 makes the app feel slow, and slow-but-right beats fast-but-coarse for a radar viewer. |
| 8 | **Pane count** | `WidthClass`-driven cap, tightened | Structural. The user loses a view they explicitly asked for, so it must be nearly last, and it must never silently rewrite their saved layout — `the_config_clamp_is_wider_than_a_compact_screen_offers` (`squallar-egui/src/ui_layout.rs:372`) pins exactly this. |
| — | **Capability-gated rungs** | any rung whose knob needs a feature | **Not a rung position — a rule.** A rung that needs a format feature, a downlevel flag or a limit must ask `DeviceProfile`'s capability set for it, never ask whether this is the web build. When the capability is absent the rung is *skipped*. |
| **Floor** | 3D view retired entirely | `squallar_volumetric::degrade::VolumeSupport` | Already exists and latches after `MAX_SURFACE_LOSSES_WITH_VOLUME = 2` surface losses (`squallar-volumetric/src/volume_degrade.rs:31`). The "garbage-ass version" the user wants to reach only as a last resort: 2D radar, correct, interactive, no 3D. |

**What landed.** `demote` (`squallar-device-profile/src/budget.rs:679-723`) is
four rungs — shading; offscreen resolution and bytes (with the app ceiling that
moved with it); grid cells and volume texture bytes; raster side — applied as
"the *first rung that moves*, not the nth rung", so a machine already at a
rung's stop steps the next one. Pinned by
`the_ladder_surrenders_lighting_before_resolution_and_the_picture_last`
(`budget/tests.rs:768`) and
`no_number_of_back_offs_takes_a_machine_below_its_bracket_floor` (`:811`).

**What landed (WO-7, 2026-09-02).** `fit` (§9) walks this same ladder by
arithmetic instead of by a counter — one module-level six-rung table in
`budget.rs`, shared by `demote` and `fit` through `step_down` — with two rungs
inserted between resolution and grid: loop history (rung 3, one halving of
`loop_render_budget` a step, 2D before 3D) and tile sharpness (rung 4,
`Budgets::tile_whole_zoom`, consumed by WO-12). The two pins above kept every
property and were **re-argued in the body, not kept verbatim**: with the
inserted rungs the grid reaches its floor and the ladder its fixed point later
than the 3 and 4 steps the four-rung ladder pinned — by exactly the halvings
the bracket's render budget takes to reach the two-frame floor plus one tile
step, a rung with nowhere to go on a bracket costing no step — and the
resolution rung, like the history rung, is one coarsening a step, so it is two
steps on the desktop bracket (Native to Half to Quarter) and one on the other
two: desktop 9 (1+2+4+1+0+1, its grid is pinned), mobile 5 (0+1+3+1+0+0),
wasm32 7 (0+1+3+1+1+1). `deep` still has grid at floor and raster at the
long-range floor.

### 4.4 The direct competition the brief flags: 2D loop frames vs 3D grid resolution

They draw on the same pool. The ladder says **2D loop frames yield first**
(rung 3) and **3D grid resolution yields later** (rung 5). Justification:

- A 2D loop losing frames loses *history*, which the user can re-acquire by
  waiting. A grid losing cells loses *detail*, which they cannot.
- The pool's own division is already frame-count-based and already degrades
  smoothly with a floor; the grid's is a discrete step between three named
  shapes with no intermediate. Smooth-before-discrete.
- `LoopPool::plan` (`squallar-app/src/loop_pool.rs:362-390`) caps every loop
  kind, 3D included, at the same `render_budget` (`:366`) — at f43c464f its
  comment called this "so a 3D loop is not licensed to hold more history than
  the plan-view loop beside it". That is a *fairness* rule between loop kinds,
  not a degradation ordering, and it stays — but the degradation ordering above
  it lets the 2D side shrink first.

Concretely: when `fit` must cut, it halves `loop_render_budget` toward
`MIN_LOOP_FRAMES_PER_PANE` frames before it steps `grid_cells` down a bracket.

---

## 5. Per-target reality

Assembled from what the source establishes plus the web-signal survey recorded
in §8. **Everything marked *unverified* is unverified by me.** The last column
now points at the §8 capacity source for each row.

| Target | Backend | `AdapterInfo::device_type` | `adapter.limits()` | VRAM (today) | Threads | Capacity source (§8) |
| --- | --- | --- | --- | --- | --- | --- |
| **Linux** | Vulkan | Real (`Discrete`/`Integrated`) | Real | `VK_EXT_memory_budget` **is read by wgpu-hal 29.0.4** but only to *refuse* past `wgt::MemoryBudgetThresholds`; the figures are never returned (survey, §8). | Real | **Measured**: Vulkan `DEVICE_LOCAL` heap sum via `as_hal`; RAM from `/proc/meminfo`. |
| **Windows** | DX12/Vulkan | Real | Real | Same wgpu limitation. DXGI `QueryVideoMemoryInfo` is not exposed through wgpu — reached via `as_hal` in the shell. | Real | **Measured**: DXGI local `Budget`; RAM from `GlobalMemoryStatusEx`. |
| **macOS** | Metal | Real; Apple Silicon reports `IntegratedGpu` | Real, and generous (`max_texture_dimension_2d` 16384) | `MTLDevice.recommendedMaxWorkingSetSize` is not surfaced by wgpu — reached via `as_hal`. | Real | **Measured**: `recommendedMaxWorkingSetSize`, every class — the fix for "`Integrated` is a lie on Apple Silicon" (an M-series `Integrated` with 64 GiB unified memory takes `Step` = 2 × floor under today's rule, `loop_pool.rs:340`). |
| **iOS** | Metal | `IntegratedGpu` | Real | Same as macOS | Real | **Measured**, same reader. Same class as macOS by every signal, wildly different by memory — the sharpest unmeasured target (§7.4). |
| **Android** | GLES/Vulkan | Driver-dependent; often `IntegratedGpu`, sometimes `Other` | Real | None through wgpu | Real | **Measured**: Vulkan heaps where Vulkan; `system_ram / UNIFIED_MEMORY_GPU_DIVISOR (2)` for non-Apple integrated — heaps lie both ways on UMA. |
| **Web — Firefox** | WebGL2 (every leg today; `create_instance`'s doc, `squallar-app/src/app.rs:80-97`) | **`Other` → `Unknown`** (`squallar-gpu/src/device.rs:105-108`) | Real for resolution only — `squallar_gpu::device::device_limits` lifts *only* `max_texture_dimension_2d/3d` via `using_resolution` (`squallar-gpu/src/device.rs:59`) | None, ever | **1** for the app; a rayon pool from `hardwareConcurrency` in the worker (`squallar-web/src/rayon_pool.rs:48`) | **Presumed** (§8): no safe probe exists on WebGL2. **The governing web row.** |
| **Web — Chromium** | WebGPU when `requestAdapter()` answers, else WebGL2 | `Other` → `Unknown` | Same mechanism; **may report different figures on the same machine** | None through wgpu | same | **Probed** on WebGPU (§8): a throwaway device under an error scope measures the per-tab allowance. Presumed on WebGL2. |
| **Software / Virtual adapter** | any | `Cpu` / `VirtualGpu` → `Software` / `Virtual` | Real | whatever it says | Real | **`None` — the lie-guard.** `Promotion::for_class` already sends both to `Floor` (`budget.rs:64-70`). |

Six rows, not five, and the sixth is not a courtesy: Firefox and Chromium are the
same binary on the same silicon and are separated only by what they report — and
now also by whether a capacity probe is *possible*. Any design that treats "web"
as one profile will be wrong on one of them.

### 5.0 Firefox first: what changes, and what does not

**What does not change.** The mechanism. Firefox and Chromium both land in
`Platform::Web`, both get `DeviceClass::Unknown`, both go through the same
`resolve` — and the same `fit`. Nothing branches on browser identity — see §2.3,
the anti-sniffing rule.

**What changes — three concrete things.**

1. **The web bracket is set from Firefox's numbers, not from a spec floor and not
   from Chromium's.** `WASM_LOOP_POOL_FLOOR_BYTES` is 56 MiB
   (`constants.rs:319`), `WASM_VOLUME_GRID_CELLS` is 128×128×64 (`:399`), and
   `WASM_PLATFORM_CEILING` is Half+Off (`quality.rs:216`). None of those was
   derived from a Firefox measurement — they were derived from the WebGL2
   *guarantee* (`constants.rs:438-439`) and from a conservative reading of what
   a phone browser might report. Under §9 these are the web arm's **presumed
   capacity** and its floors; measuring Firefox re-cuts the presumption.
2. **Promotion by reported limits is the *primary* web lever, not a nicety.**
   *Landed:* `reported_promotion` (`budget.rs:120-131`) promotes a browser to
   `Ceiling` when it reports at least `DESKTOP_CLASS_REPORT {16384, 8192}`. If
   Firefox reports 16384 on a desktop GPU and 4096 on a phone, that single number
   separates the two cases that a `cfg` cannot — identically in both browsers.
   D3 of the plan adds form factor as a second conjunct (§8).
3. **Any rung that depends on a capability must query it, never assume it from
   `Platform`.** `format_shortfall` (`squallar-volumetric/src/lib.rs:240`) is the
   pattern and it already produces a per-adapter answer.

**The disagreement rule, stated so it can be tested.** If Firefox and Chromium
report different figures for the same axis on the same machine, the resolver does
not care — each resolves from its own report. What Firefox governs is the
**bracket**: `floor_web` is set so Firefox is comfortable inside it, and
`ceiling_web` is set so that nothing Chromium reports can push the app past what
Firefox would also survive on comparable hardware. Two profiles for one machine
must both satisfy every invariant in `check_invariants`, and the Firefox row
must never be the one that fails.

### 5.1 The target where nothing useful is exposed: the browser

This is not an edge case; it is the target the user is most likely to be on when
they say the app looks like garbage on a good machine. A workstation browser and
a phone browser are **indistinguishable** at compile time and nearly so at
runtime, and the app gave both the phone's answer until Stage 3.

What I would do, in order of confidence — with what became of each:

1. **Ship the floor, always.** Never guess high on the one target that answers
   exhaustion by destroying the rendering context. Correct as a *starting*
   point. *Kept:* the presumed arm (§8, §9) starts at the bracket and `fit`
   sheds from there.
2. **Promote on `max_texture_dimension_2d/3d`.** *Landed* (`reported_promotion`).
3. **`matchMedia('(pointer: coarse)')` + `(any-pointer: fine)` +
   `navigator.maxTouchPoints`** as a form-factor classifier, through
   `squallar-web` and the platform bridge into `DeviceProfile::form_factor`.
   *Approved:* plan D1, `squallar-web/src/form_factor.rs`, WO-2; spent in WO-11
   (§8).
4. **Never `navigator.deviceMemory` or `hardwareConcurrency` as a bound.**
   *Kept, sharpened:* `deviceMemory` is a hint that can only *lower* a
   presumption, never raise one (plan D1). §8 carries the survey's reasons.
5. **Never screen area as a classifier.** *Kept* — §8.
6. **The behavioural backstop is the real bound.** At f43c464f: *"`LoopPool::back_off`
   halving on a lost surface, persisted synchronously, is what actually makes web
   safe."* **Overruled** in its persisted half by ruling 6. The backstop is now
   §10: evict economy, lower *this session's* presumption, re-fit — and forget
   it at exit.

### 5.2 The case no signal can see

An installed iOS Home Screen PWA is the tightest thing this app ships and is
identical to a Safari tab under every signal above (§8 records why). At
f43c464f the memo was "the only answer", and it was to be its own `KvStore` key
written synchronously rather than a `UiConfig` field on the 3 s autosave timer.
**Overruled** by ruling 6. The in-session answer (§10) is what the PWA gets: the
first `memory_warning` or wasm watermark evicts economy and re-fits, within the
session. If a real device proves to hit the wall on every launch with the same
scene, a one-failure memory is the documented later add-on — off, and not in
this plan.

---

## 6. Staged implementation path — status

Ordered so the user could stop after any stage and be better off.

### Stage 1 — Make the shipped configuration honest — **done**

- The two stale doc claims found in §0: `voxel.rs`'s now reads "What one grid's
  index plane may occupy, bytes" (`squallar-radar/src/voxel.rs:557`); the
  volume-texture comparison to the loop floor is gone.
- `MAX_LOOP_VOLUME_FRAMES` deleted; no file names it.
- The §0 proof gap closed: `the_whole_application_fits_its_gpu_ceiling` sums the
  volume-store floor (`constants/tests.rs:116-135`).

### Stage 2 — Extract the resolver, change no behaviour — **landed**

- `squallar-device-profile/src/budget.rs` with `DeviceProfile`, `Budgets`,
  `BudgetLimits`, `resolve`; brackets populated from today's constants;
  `the_resolver_reproduces_every_shipped_constant` (`budget/tests.rs:31`) pins
  that `resolve` reproduces the three shipped profiles exactly.
- `arms()` became `profiles().map(resolve)`; `synthetic_profiles` +
  `check_invariants` are the matrix.
- `Budgets` threaded from `AppState` to the consumers named in §1.1; the `cfg`
  constants remain as the bracket definitions.

### Stage 3 — Turn on the signals that already exist — **landed, one part overruled**

- `DeviceClass` promotion for every bracketed field (`Promotion::for_class`).
- Web promotion on reported `max_texture_dimension_2d/3d`
  (`reported_promotion`) — the step that separates a desktop Firefox from a
  phone Firefox, using a number the app already reads and already lifts.
- `back_off` extended from the loop pool to the whole `Budgets`
  (`back_off_budgets`, `demote`), one step down the §4.3 ladder per latched
  failure, persisted under `budget_steps`. **The persistence is overruled**
  (ruling 6) and replaced by §10; the ladder itself survives as `fit`'s shed
  order. Pins that move are listed in the plan's §Pins.

### Stages 4–6 — superseded by §8–§11

Stage 4 ("spend the measurement") assumed the desktop *ceilings* would be
raised by hand once measured; ruling 3 makes measured capacity bind directly
(§9, WO-8/9/10). Stage 5 (non-GPU pressure: the depth-1 pools, the wasm index
plane) is answered by the wasm watermark (§10, WO-13) and remains otherwise
open. Stage 6 (aarch64) is §7.4 and the plan's U1/U2 user actions.

---

## 7. Open questions and unmeasured ground

**Measurements I need from others (stated as inputs, not re-derived).** Each is
phrased as the question my design will ask the number, so the measurement can be
checked against the need. Status as of 2026-09-01 in brackets.

1. **VRAM.** Whether a trustworthy figure is obtainable, on which backends, and
   what it means on unified memory. *Slots into:* `DeviceProfile::vram_bytes`.
   `None` is the **majority** case, so a negative answer costs only the promotion
   rung. [Answered by the plan's D1: not through wgpu 29.0.4, but through
   `as_hal` readers in the `squallar` shell — §8, WO-8. Unified memory: Metal's
   `recommendedMaxWorkingSetSize` on Apple; `RAM / 2` elsewhere.]
2. **2D radar-raster and overlay-texture resolution numbers.** *Slots into:*
   ladder rungs 4 and 6, and `Budgets::{raster_side_ceiling_px, section_width_px}`.
   [`DESKTOP_RASTER_SIDE_CEILING = 8192` landed (`constants.rs:28`), "the ceiling
   this build was measured to" per `app_state.rs:91-97`.]
3. **TDWR loop cadence.** *Slots into:* rung 3's cost model. [`PaneNeed::cadence_secs`
   in the plan's `Scene` (§9).]
4. **Firefox WebGL2 reported limits** — the five `limits_shortfall` reads, on a
   desktop discrete GPU and a desktop iGPU. *Slots into:* the web presumption.
   [Partly landed: `DESKTOP_CLASS_REPORT {16384, 8192}` is "the componentwise
   least either desktop-class machine this project has measured a browser report
   on". The Mac's 3D cap is **measured**: `MAX_3D_TEXTURE_SIZE` 2048 (WebGL2
   renderer string `Apple GPU`, `MAX_TEXTURE_SIZE` 16384; WebGPU
   `maxTextureDimension3D` also 2048), read 2026-09-02 by the browser rig's
   environment probe — not the app log — on the user's Mac mini M2 (10 GPU cores,
   8 GB unified), macOS 26.4.1, Safari 26.4. Firefox and Chrome on the Mac were
   not run; the caps come from the GPU and driver, not the browser, so the same
   2048 is presumed for them, unmeasured. 2048 < 8192 fails the 3D conjunct, so
   every Mac browser resolves to `Floor` regardless of `FormFactor::Desktop`
   (`a_mac_browser_resolves_on_its_own_3d_cap`).]
5. **The same five, from Chromium, on the same machine.** [Same status. If they
   are identical, one web bracket suffices and the pair test is cheap insurance.]
6. **`get_texture_format_features(Rg16Float)` from both browsers.** *Slots into:*
   the capability half (§2.3.1). [Open; the probe checks it at runtime anyway.]
7. **Device pixel ratio / DPI handling in both browsers.** *Slots into:* the
   offscreen and mirror budgets, sized in **physical** pixels
   (`VOLUME_OFFSCREEN_REFERENCE_PANE_PX = [2560, 1440]`, `constants.rs:454`). [Open.]

**Questions I cannot answer without hardware:**

### 7.4 aarch64

4. **aarch64 is three of five targets and is entirely unmeasured.** macOS, iOS
   and Android all report `IntegratedGpu` or `Other`, and today's rule gives
   `Integrated` exactly `Step` = `2 × floor` (`squallar-app/src/loop_pool.rs:340`;
   `Promotion::for_class`, `budget.rs:66`). On an M-series Mac with 64 GiB of
   unified memory that is the user's complaint, verbatim. **I would not ship a
   promotion rule for `Integrated` until someone has run the app on Apple
   Silicon and on at least one flagship and one bargain Android handset.** Until
   then the MOBILE bracket is wholly pinned — `BudgetLimits::MOBILE`
   (`squallar-device-profile/src/budget.rs:405-431`, whose comment cites this
   section) and
   `the_mobile_bracket_promotes_nothing_until_somebody_measures_aarch64`
   (`budget/tests.rs:736`). *Status:* the plan keeps the pinned fields pinned on
   both arms — fill-rate and wire-capped fields never scale on capacity — and
   makes the *measured* arm the way Apple Silicon stops being a lie
   (`recommendedMaxWorkingSetSize`, §8), without a promotion rule for
   `Integrated`. The one-time measurements only the user can run are the plan's
   U1 (Mac browsers — the Safari leg landed 2026-09-02, 3D cap 2048, §7 item 4)
   and U2 (a ≤ 4 GiB and a ≥ 12 GiB Android, 20 min each).
5. Does `MTLDevice.recommendedMaxWorkingSetSize` or DXGI `QueryVideoMemoryInfo`
   reach through wgpu 29.0.4 at all? [No — and the thin platform-layer reader is
   acceptable: ruling 2, §7.5. `Adapter::as_hal` is `unsafe fn`, so the readers
   live in the `squallar` shell, the only crate not `#![forbid(unsafe_code)]`
   (§8).]
6. Is an installed iOS PWA's memory limit different from a Safari tab's? No
   primary source says so, and the widely repeated ~200 MB figure appears in
   none (§8). Treat as unknown. [At f43c464f: "the memo is the answer".
   **Overruled**, ruling 6 — §10 is the answer.]

**Decisions I asked the user to make** — and their status after §7.5:

7. Accept the §3.1 trade: the shipped desktop configuration stops being a
   *compile-time* proof and becomes a *host-test-over-the-matrix* proof, while
   the floor stays compile-time. **Accepted as written** (landed, Stage 2).
8. Accept the §4.3 ladder ordering, in particular rung 3 before rung 5 (2D loop
   history yields before 3D grid resolution) and rung 7 (concurrency) placed
   below the picture rungs. **Accepted as written**; the plan extends it with
   loop span and tile sharpness between resolution and grid.
9. Accept keeping the `mobile` cfg as a *bracket selector* rather than deleting
   it (§2.7). **Accepted as written.**
10. Whether `APP_TEXTURE_BUDGET_BYTES` should stay a compile-time bracket. I
    recommended yes, because measuring both sides of the snugness test makes it
    a tautology. **Overruled** for measured capacity (rulings 3 and 5): on the
    measured arm no ceiling constant binds; the constant is the *presumed*
    capacity of the arm that cannot measure, where the snugness test still
    bites (§3.2, §9).
11. Accept the anti-sniffing rule (§2.3): **no `Browser::{Firefox, Chromium}`
    variant anywhere in `DeviceProfile`.** **Accepted as written.** §8's
    WebGPU-vs-WebGL2 distinction is not a name either: it is whether
    `requestAdapter()` answered, read from the adapter.
12. Whether a browser row is worth adding to the matrix at all if measurement 5
    comes back saying Firefox and Chromium report identical figures. **Accepted
    as written**: keep the pair test regardless.

Also decided here and not in the list above: **§2.6 item 3** (the persisted
back-off memo, `budget_steps` and `loop_pool`) is **overruled** by ruling 6.

### 7.5 Rulings on the record

The user's rulings of 2026-09-01, quoted from the approved plan's Context, in
order:

1. *"Tile caching is problematic in squallar and downright broken on web. … we
   should be as runtime-aware as possible. … The beefcakes shouldn't suffer so
   that phones can exist. but phones (even the shitty ones) need to work too."*
2. VRAM readers may live in the `squallar` shell with scoped `unsafe`.
3. No static pinned VRAM ceilings where capacity is measured.
4. Resource management is not something the user sees or manages.
5. *"I don't think we should demote capacity but can kick things out so we don't
   have needless resident memory. … a desktop shouldn't just use 30x more memory
   for the same data and resolution as a tablet just because it has more."*
6. No learning across sessions. Capacity is measured, probed or presumed at
   startup; pressure is answered within the session.

What each settles in this document: ruling 1 is the target restated and §11's
subject; ruling 2 is §8's placement of the readers; ruling 3 overrules item 10
and §2.5's "raise the desktop ceilings"; ruling 4 means no budget appears in the
UI — the `budget state:` telemetry line and the opt-in diagnostics row (§8) are
developer readouts; ruling 5 is §9 — need is a function of the scene, capacity
only limits, economy is what a big machine buys; ruling 6 overrules §2.6 item 3,
§4.1 item 5's mechanism, §5.1 item 6 and §5.2, and is §10.

---

## 8. Capacity: measured, probed, presumed

**Source.** This section carries forward the web-signal survey that lived in the
`loop_pool.rs` module doc at f43c464f (`git show
f43c464f:rustdar-frontend/src/loop_pool.rs`, lines 55–175) and has since been
deleted from the source; the substantive claims below are that survey's, marked
*(survey)*, plus the plan's D1 table, marked *(plan)*. Nothing in this section
is measured by this revision.

### 8.1 What can be asked, per backend

*(survey)* **wgpu 29.0.4 exposes no memory capacity on any backend.**
`Device::generate_allocator_report` reports what *we* have allocated, not what
the device has. `VK_EXT_memory_budget` **is** read by wgpu-hal — `heap_budget`
and `heap_usage` — but only to *refuse* an allocation past a percentage the
application sets through `wgt::MemoryBudgetThresholds`; the figures are never
handed back. `AdapterInfo::device_type` is the one real signal, and WebGL2
reports nothing at all: every browser is `DeviceClass::Unknown` whatever the
silicon is. The workspace is still on wgpu 29.0.4 at `e2c1e664` (`Cargo.lock`).

So capacity is read **beside** wgpu, not through it. Sources in order of trust
*(plan D1)*:

| platform | GPU capacity | host capacity | source |
| --- | --- | --- | --- |
| Linux / Windows / Android native, `Discrete` | Vulkan `DEVICE_LOCAL` heap sum, or DXGI local `Budget`, via `Adapter::as_hal` | `/proc/meminfo` `MemTotal` / `GlobalMemoryStatusEx` | **Measured** |
| native `Integrated` (non-Apple) | `system_ram / UNIFIED_MEMORY_GPU_DIVISOR (2)` — Vulkan heaps lie both ways on UMA | RAM | **Measured (RAM)** |
| macOS / iOS | Metal `recommendedMaxWorkingSetSize` — every class; the fix for "`Integrated` is a lie on Apple Silicon" | `NSProcessInfo.physicalMemory` | **Measured** |
| **web, WebGPU** (Chromium today; Firefox once shipped on Linux/Android) | **Probed** — §8.3 | 1 GiB link constant × 2 instances — §8.5 | **Probed** |
| **web, WebGL2** (every Firefox leg today) | **Presumed** — §8.4: the bracket's constant, refined by the adapter's 2D/3D caps and form factor | 1 GiB × 2 | **Presumed** |
| Software / Virtual adapter, any reading | `None` — the lie-guard | — | **Presumed** |

`Capacity { gpu_bytes: Option<u64>, host_bytes: Option<u64>, source:
CapacitySource }` with `CapacitySource::{Measured, Probed, Presumed}` *(plan D0)*
is what `fit` consumes (§9). A reading arrives as plain data through the
platform bridge (`squallar-app/src/platform.rs:284`, `PlatformBridge`), which
gains defaulted methods so every implementation compiles unchanged *(plan D1)*:
`host_signals() -> HostSignals`, `linear_memory() -> Option<LinearMemory>`,
`gpu_capacity(adapter, device) -> Option<(u64, GpuCapacitySource)>`. None of
these exists at `e2c1e664`.

### 8.2 Where the readers live, and why not in `squallar-gpu`

`Adapter::as_hal` is an `unsafe fn` *(plan)*. `squallar-gpu/src/lib.rs:2`,
`squallar-app/src/lib.rs:2`, `squallar-web/src/lib.rs:2` and
`squallar-device-profile/src/lib.rs:2` are all `#![forbid(unsafe_code)]`, which no
inner attribute can lift. The `squallar` shell (`squallar/src/lib.rs:4`,
`squallar/src/main.rs:4`) is `#![deny(unsafe_code)]`, which a scoped
`#[allow(unsafe_code)]` on one function can — and ruling 2 (§7.5) permits
exactly that. So the readers are **cfg-selected modules** in the shell,
`squallar/src/capacity/{mod,linux,apple,windows,vulkan,metal,dx12}.rs` *(plan
D1)*, and the `cfg` selects a *module*, which the workspace's boundary rule
allows ("a `cfg` selects a value, dependency, type alias or module — never a
fork in a body"). Zero new packages: `ash 0.38`, `objc2-metal 0.3.2`,
`objc2-foundation 0.3.2` and `windows 0.62.2` are already in `Cargo.lock`
(verified at `e2c1e664`). `squallar-gpu` gains a `device_local_total` helper
without `unsafe` *(plan WO-8)*.

The value crosses into `squallar-device-profile` as `Option<u64>` — the crate's
dependency rule (`squallar-radar` + `log` only) is untouched.

### 8.3 The WebGPU probe

**Landed** (`squallar-web/src/gpu_probe.rs`, its wasm-only `run` submodule,
`squallar_app::platform::ProbedCapacity`). On a page whose own backend is
WebGPU the app **probes**: a second `wgpu::Instance` on `navigator.gpu`, a
second adapter (the app's own is consumed by the one `requestDevice` that
already took it), a throwaway device, and textures allocated in **doubling
steps from 64 MiB** — each a square power-of-two `Rgba8Unorm` 2D array under
the device's `maxTextureDimension2D` and `maxTextureArrayLayers`, **every
layer cleared in its own render pass** so the memory is resident and not
merely reserved — inside a `pushErrorScope("out-of-memory")` per step, until
the scope pops a `GPUOutOfMemoryError` or `device.lost` fires. Then every
texture and the device are destroyed. Nothing is persisted (ruling 6).

**What it measures** is the browser's **per-tab WebGPU allowance**: the last
total the throwaway device held without refusal. Not the card — a tab is
allowed a share, decided by the browser, and no API states it. The figure
enters as `Capacity::probed`, never through `vram_bytes`: nothing a browser
*reports* is a measurement, and a probe is not a report. It takes the same
three-quarter allowance a measured figure does (§9.3).

**Bounds.** Three of the probe's own, each reported as `capped`: an **8 GiB**
byte ceiling (the last step is clamped so a device that never refuses reports
exactly 8 GiB — above that the figure would be the card, not an allowance); a
**2 s** wall budget, applied predictively (a step is taken to cost twice the
last, and one that would carry the total past 2 s is not taken); and a shape
no single texture can take on a narrow adapter. A capped figure is a floor on
the allowance. A probe that held nothing — the first allocation refused, no
second adapter, no device — reports no figure, and the presumption stands.

**When it runs.** After the first presented frame, never before: the app asks
the bridge on the 2 s telemetry tick (`PlatformBridge::probed_gpu_capacity`,
defaulted `None`; the web bridge starts the probe on the first ask and reads
a thread-local outcome cell on the later ones, the way the worker's heap
reading arrives, with no channel receiver spent). The app starts on the
presumption (§8.4), folds the figure **once** when it lands
(`App::adopt_probed_capacity`), re-fits the scene against it and re-sizes the
loop pool. `DeviceProfile::capacity` is untouched and stays pure: the figure
is the session's, and `capacity_with_probe` spends it on exactly one profile
— `Platform::Web` with `DeviceClass::Unknown`, which is every browser, since
WebGPU reports no device type. A measured capacity outranks it; a native
profile ignores it (`a_probe_on_a_native_profile_is_ignored`); a web adapter
classed software or virtual keeps its presumption, as the lie-guard already
rules for its readings.

**When it is skipped.** Whenever the app's own backend is not `BrowserWebGpu`
(`gpu_probe_applies_to`, pinned by `a_probe_on_a_webgl2_page_never_runs`). wgpu
binds one browser API when the instance is built (`ARCHITECTURE.md` §4), and
on a WebGL2 page the probe would measure an API that is not the one drawing;
WebGL2 itself has no clean failure to probe (§8.4). The web bridge logs
`gpu probe: skipped (backend Gl)` once — every Firefox/Linux leg today — and
the presumption stands.

**What it prints, and which line carries what.** Two kinds of line, for two
readers. **Once-only, for humans**: `gpu probe: 4032 MiB ok, failed at
8128 MiB, 7 steps, 812 ms, backend BrowserWebGpu` when the figure lands
(`failed at none` and a trailing `, capped` when the probe stopped at its own
bound), `gpu probe: skipped (backend Gl)` on a WebGL2 page, `gpu probe:
nothing held (...)` or `gpu probe: no second adapter (...)` when it ran and
held nothing. These carry the step count, the elapsed time and the refused
total, and **nothing may be read off them by a test or a rig row**: the
browser console keeps a bounded ring, frame telemetry evicts a once-only
line within seconds, and a scrape reading it as absent cannot tell "evicted"
from "never ran". **Re-said every 2 s, for the rig**: the `budget state:`
line (§8.6) carries the two facts a row reads — `cap N 2`, the figure in
force and that it is probed, and `probe <code>`, where the probe stands:

```text
gpu probe: 4032 MiB ok, failed at 8128 MiB, 7 steps, 812 ms, backend BrowserWebGpu
budget state: bracket wasm32, rung 0, ..., cap 4032 2, probe 4
budget state: bracket wasm32, rung 0, ..., cap 288 0, probe 1        (a WebGL2 page)
```

`probe` is `0` absent (native, or not yet asked), `1` skipped, `2` pending,
`3` empty, `4` found at the device's refusal, `5` found at the probe's own
bound. Integers only, ASCII only. The rig expects two rows, never merged:
Chromium under `--enable-unsafe-webgpu --enable-features=Vulkan
--use-angle=vulkan` shows `cap N 2, probe 4|5`; Firefox/Linux shows `cap 288
0, probe 1`. Neither row has been taken yet by this revision; the state
machine is host-tested in `squallar-web/src/gpu_probe.rs` and the fold in
`squallar-app/src/app/gpu_capacity_tests.rs`
(`a_probed_capacity_reaches_the_fit_on_a_web_profile_and_prints_cap_2`: six
two-hour loops that the 288 MiB presumption had to shorten fit at the class
rung under a probed 4032 MiB).

**Through wgpu, not `web-sys`.** The WebGPU backend surfaces
`GPUOutOfMemoryError` as `wgpu::Error::OutOfMemory` from a popped
`ErrorFilter::OutOfMemory` scope and device loss through
`set_device_lost_callback`, so `squallar-web` gained no `web-sys` feature and
no package — only `egui-wgpu`, already in its wasm32 graph. Two filters are
pushed per step and no more: the out-of-memory scope is the reading, a
validation scope drains the errors a refused texture goes on to produce so
they never reach the device's uncaptured path, and an `Internal` filter is
never pushed because wgpu 29.0.4's backend maps a popped `GPUInternalError`
to a panic. A lost device answers every later call as if it succeeded, so
the loss flag outranks a silent scope.

### 8.4 Why WebGL2 has no safe probe, and what "presumed" means

*(plan D1)* On WebGL2 **no clean failure exists**: drivers oversubscribe
silently, or the GPU-process reset loses every context in the tab, or the tab is
killed. There is nothing to catch. So every WebGL2 browser — every Firefox leg
today — runs on the **presumed** arm: the bracket's `APP_TEXTURE_BUDGET_BYTES`
constant *is* the presumed GPU capacity (288 / 1024 / 3840 MiB — that is what
those three numbers always were), refined by the adapter's reported 2D/3D caps
and by form factor *(plan D2, D3)*.

*(plan D3)* `reported_promotion` becomes: not desktop-class → `Floor`;
desktop-class **and** `form_factor == Desktop` **and** not `declared_small`
(`deviceMemory ≤ 2 GiB`) → `Ceiling`; otherwise `Step`. WASM promotable fields
become `stepped(floor, today's ceiling, today's ceiling)` — behaviour-preserving
by construction, creating the slot a probed or measured desktop-browser tier
fills later. Classifier failure modes are pinned as rows, unmeasured ones
`#[ignore]` with a reason: iPad + trackpad (16384/2048/Desktop → `Floor`, held
only by the 3D conjunct), Chromebook, phone in DeX, touch laptop + dGPU
(→ `Ceiling`), a Handheld reporting desktop-class caps (→ mobile tier, never
above), Mac in Safari/Firefox/Chrome (16384/2048/Desktop → `Floor`; the 2048
**measured** in Safari 26.4 on the user's M2 by the rig's environment probe,
2026-09-02, and presumed the same for Firefox and Chrome, unmeasured).

**The web signals, and what each is worth.** *(survey, carried forward; plan D1
where it sharpened the rule)*

- **`matchMedia('(pointer: coarse)')` with `(any-pointer: fine)`** is the only
  device-class signal stock Chromium, Safari and Firefox all implement
  unclamped, Baseline since 2018. A handheld is coarse-and-not-also-fine; a
  touchscreen laptop is both, which is exactly the case a naive `coarse` test
  gets wrong. `navigator.maxTouchPoints` is the tiebreak — WebKit hard-codes 5
  on iOS/iPadOS and 0 on macOS. *(plan D1)* Classifier, pure and host-tested in
  `squallar-web/src/form_factor.rs`: `Handheld = coarse && !any_fine`;
  `Desktop = any_fine`; neither → `maxTouchPoints > 0 ? Handheld : Desktop`;
  all fail → `None`. Today `matchMedia` is read for the colour scheme only
  (`squallar-web/src/bridge.rs:41`).
- **Screen area is not a class signal**, however tempting. Phones run a device
  pixel ratio of 3, so their physical pixel counts *exceed* a workstation's:
  1080p desktop 2.07 Mpx, Pixel 10 2.62, iPhone 16 Pro Max 3.79. Ranking by
  pixels puts a flagship phone above a laptop with an 8 GiB discrete GPU behind
  it. It is a good *rendering-cost* term (`PaneNeed::px`, §9) and a bad
  classifier.
- **`navigator.deviceMemory` refines only on Chromium**, and never bounds.
  WebKit filed a formal oppose position in April 2026, so it will never exist in
  Safari, and Chrome 147 recut the buckets to `{1,2,4,8}` on Android — a 16 GiB
  flagship and an 8 GiB midrange are the same value. *(plan D1)* It is a hint
  that can only **lower** a presumption (`declared_small`), never raise one.
  The rig read 32 on the desktop leg; unused today *(plan)*.
- **`hardwareConcurrency` is worse**: Safari's is a two-valued function of 4 or
  8, and returns a pseudorandom 1..63 to a tracker-classified script under the
  fingerprinting protection that is on by default from iOS 26. Today it sizes
  the worker's rayon pool only (`squallar-web/src/rayon_pool.rs:34-48`); under
  the plan it arrives as `HostSignals::parallelism` and moves no budget.
- **Every one of them is spoofable in a line of JavaScript, so none is a
  bound.** At f43c464f the conclusion was "the learn-from-failure path is the
  real backstop"; under ruling 6 the backstop is the in-session response of
  §10.

**The case no signal can see.** *(survey)* An **installed iOS Home Screen PWA**
is the tightest thing this application ships, and an iPhone in Safari and the
same iPhone in a Home Screen PWA are identical to every signal above. It really
is a different process: WebKit checks
`applicationBundleIsEqualTo("com.apple.webapp")`, and auxiliary processes are
namespaced to the host bundle, so a PWA gets its own WebContent, Networking and
GPU processes rather than sharing Safari's. The background lifecycle is harsher
than a desktop's by construction — background process assertions time out after
30 s on iOS and not at all on macOS, suspension follows at 20 s, all assertions
are dropped at 4 minutes, and `BoostedJetsam` is taken only under
`PLATFORM(MAC)`. WebKit's own statement in May 2026 is that nothing in iOS 26
changed memory accounting or budgets for WebKit, and jetsam kills are still being
filed against an iPhone 17 Pro. What is **not** established either way is
whether a PWA's memory limit differs from a Safari tab's — no primary source
says so, and the widely repeated ~200 MB figure appears in none. The figure that
is sourced is WebKit's own: ~1.5 GB for `WebContent` on most iPhones; the wasm
pool floor (then 48 MiB, now 56) was argued as a small fraction of that, "the
margin this arm is entitled to given that it cannot measure, cannot predict, and
cannot recover without taking every other tab with it". That is the case §10's
`memory_warning` and watermark exist for.

### 8.5 The wasm heap: two instances, one constant, no probe

*(plan, verified at `e2c1e664`)* **The wasm 1 GiB is two ceilings.** The page
and the rasterization worker are separate module instances, each linked with
`--max-memory=1073741824` (`.github/scripts/wasm-threads.sh:132`), and the module
header declares `maximum=16384 pages (1.000 GiB)`
(`squallar-device-profile/src/constants.rs:130-146`). The measured Tier-2
`firefox.huge` trap (MRMS decode) was the **worker's** *(plan)*.

**Why not probe it.** The ceiling is a link-time constant that the module
header states; "nothing has to be run to learn it" (`constants.rs:141-142`).
Growing the heap to find a wall the header already names would spend, on a
throwaway, the very memory the picture needs — and on the arm with the least of
it. So the heap is *read*, not probed: the page via
`memory().buffer().byte_length()` — today `wasm_bindgen::memory()` is reached
(`squallar-web/src/shared_loan.rs:233`) but `byte_length()` is never called
*(plan; confirmed by grep at `e2c1e664`)* — and the worker via a `mem` field on
the hello and every reply (`LinearMemory { page_bytes, worker_bytes }`, plan
D1). What is done with the reading is §10's watermark.

**The `--max-memory` evidence bar.** *(plan §Decisions)* The flag stays at 1 GiB
until a phone boots at 2 GiB — raising it is a measurement someone has to make on
a device, not a constant to edit.

### 8.6 What every target prints

One telemetry line on the 2 s tick, integers only, scraped by a `drive.py`
regex (`budget_state_re`) and by `native_row.py`'s own arm. **As the app
prints it** (`squallar-app/src/budget_telemetry.rs`, pinned by
`the_budget_state_line_reads_exactly_as_pinned` and held to the rig's regex by
`the_rig_reads_the_budget_line_the_app_actually_writes`):

```text
budget state: bracket desktop, rung 2, steps 0, pool 3456 MiB, ceiling 4032 MiB, vram 24822 MiB, ram 65536 MiB, declared 0 MiB, threads 32, form 2, linear 0/0 MiB, cap 24822 1, probe 0
```

The fields, in order, every one mandatory and every byte figure MiB by integer
division: `bracket` (the compile-time set), `rung` (the promotion resolved at:
0 floor, 1 step, 2 ceiling), `steps` (the ladder rungs `fit` shed), `pool` (the
**live** loop pool — what the loops need, capped by the room), `ceiling` (the
whole-application texture ceiling, the bracket's constant), `vram`, `ram`,
`declared` (three sources — measured VRAM, measured RAM, a browser's
`deviceMemory` — never one figure, `0` for unread since 0 is not a possible
measurement of any of them), `threads`, `form` (0 unknown, 1 handheld,
2 desktop), `linear` page/worker (two wasm instances, two ceilings), `cap`
(the **capacity in force this session** — measured where the readings amount
to a measurement, the bracket's presumption where they do not, held to what
pressure has taught the session) and its source (`0` presumed, `1` measured,
`2` probed), and `probe` (where the browser's WebGPU probe of §8.3 stands:
`0` absent — every native log, or not asked yet; `1` skipped — a WebGL2 page;
`2` pending; `3` empty — ran, held nothing; `4` found at the device's refusal;
`5` found at the probe's own bound, so the `cap` figure is a floor). `probe`
rides this level line rather than the probe's own once-only lines because
the browser console keeps a bounded ring and frame telemetry evicts a
once-only line within seconds; a scrape reading it as absent could not tell
"evicted" from "never ran". `cap` is not `vram`: an integrated part on a
64 GiB host prints `vram 0 … cap 32768 1`; llvmpipe reading 24 GiB prints
`vram 24576 … cap 3840 0`. A binary older than the last three groups matches
nothing and the rig reads `null`/`n/a`, never `cap 0`.

The earlier example in this section (`cap 24576 MiB measured, need 1380 MiB,
economy 512 MiB, … cause 0`) was the plan's sketch, not the app's line: `need`,
`economy` and `cause` are not printed here — need and the economy allowance
appear in the `Budgets:` prose lines when a fit is adopted, and the pressure
cause in the `pressure:` line — and the source is an integer, not a word.

The same sentence is one row in the opt-in diagnostics overlay
(`squallar-egui/src/ui_diagnostics.rs`). Ruling 4: this is a developer readout,
not a control. The proof that signals moved nothing before anything read them
(WO-2) has been re-argued at WO-9, when the measured arm went live: the 2160-row
matrix now asserts that on the **presumed** arm every fit is byte-identical
with and without the readings, and that on the **measured** arm (504 of the
rows) only the pool and its room differ, every other difference being a rung of
the one ladder
(`the_signals_move_nothing_on_the_presumed_arm_and_only_the_pool_and_room_where_measured`).

---

## 9. Need, capacity, economy, and fit

### 9.1 The three quantities

*(plan, Context)* Resident memory has three parts.

**Need** is what the scene on screen costs — tiles covering the glass, loop
frames for the span, the 3D grid in use, pane-sized offscreens, visible overlay
pictures. It is a function of what is shown and at what resolution, **never of
the machine**: the same scene costs the same bytes on a desktop and a tablet.

**Capacity** is what the device can hold — measured where an API exists, probed
where a clean probe exists, presumed otherwise (§8). It only ever *limits*.

**Economy** is what is resident beyond need — tiles panned away from, parsed
geometry kept for a restyle, the render cache. It is the one place more memory
legitimately means "keep more", and it is the first thing evicted under
pressure, at no cost beyond a later refetch.

When need alone exceeds capacity, the scene degrades in the fixed order of §4.3,
computed live from scene and capacity, and it comes back the moment the scene
shrinks. No rung counter, no persistence, no timer.

### 9.2 The one function

*(plan D0)* In `squallar-device-profile` — pure, host-testable, no `cfg` in any
body:

```rust
pub struct Scene { pub panes: Vec<PaneNeed>, pub tile_sources: Vec<TileNeed>, pub mirror_px: [u32; 2] }
pub struct PaneNeed { pub px: [u32; 2], pub view: RenderView, pub looping: bool, pub loop_span_secs: usize, pub cadence_secs: Option<u32>, pub overlay_frame_bytes: usize, pub volume_grids: usize, pub ground: GroundPass }
pub struct TileNeed { pub tiles_on_glass: usize, pub ancestor_net: usize, pub bytes_per_tile: usize }

pub struct Capacity { pub gpu_bytes: u64, pub host_bytes: Option<u64>, pub source: CapacitySource }
pub enum CapacitySource { Measured, Probed, Presumed }

/// The raymarch's own resident-grid arithmetic, handed in: this crate sits under it.
pub type GridBytes = fn([u32; 3]) -> Option<usize>;

/// The cost of a scene at a given Budgets: every term the tree already prices.
pub fn need(scene: &Scene, b: &Budgets, grid_bytes: GridBytes) -> Need   // { gpu_bytes, host_bytes }

/// The largest Budgets whose need fits `cap.allowance()`, shedding down the
/// existing `demote` ladder (extended) one rung at a time until it does. Pure.
pub fn fit(scene: &Scene, profile: &DeviceProfile, cap: &Capacity, grid_bytes: GridBytes) -> Budgets

pub const NEED_FRACTION: (u64, u64) = (3, 4);
pub const ECONOMY_FRACTION: (u64, u64) = (9, 10);
```

*(as landed, WO-7 2026-09-02 — `squallar-device-profile/src/{scene,fit}.rs`)*
Three small differences from the plan's sketch, each for a reason: `need` and
`fit` take the grid pricer as a function (`squallar_volumetric::raymarch::
resident_grid_bytes` on the app side) rather than re-deriving the raymarch's
tiling arithmetic in a crate that sits under it — the volumetric suite already
records that "the byte figure is this module's arithmetic and the resolver must
not call up into it"; `PaneNeed` carries `looping` and the pane's measured
`overlay_frame_bytes`, because an overlay loop's frame is the pane's own raster
planned by a crate this one cannot see and pricing it as a radar frame is the
4.6× under-price on wasm the pool's own tests refuse; and `fit` takes the
`DeviceProfile` (whose `limits` it reads) so that it starts from `resolve`.

**The allowance rule.** `Capacity::allowance()` is `NEED_FRACTION × gpu_bytes`
for a **measured or probed** figure — raw hardware, needing headroom for the
driver, the compositor and the picture in flight — and **the figure itself for
a presumed one**: the bracket's `APP_TEXTURE_BUDGET_BYTES` constant was argued
with its own headroom and today's sum proof already spends up to it, so the
fraction is not applied twice. `Capacity::presumed(limits)` is that constant at
the bracket's floor (288 / 1024 / 3840 MiB) whatever rung the class earned.

**Both arms are live** *(as landed, WO-9 2026-09-02)*. The one function the
application asks is `DeviceProfile::capacity()`
(`squallar-device-profile/src/budget.rs`), and it is a policy over the
profile's readings, matched on data and never on a `cfg`:

| platform, class | `gpu_capacity_bytes()` | arm |
| --- | --- | --- |
| any, `Software` or `Virtual` | `None` — the lie-guard: a reading does not un-rasterise a rasteriser (llvmpipe lists 93.9 GiB of system RAM device-local) | presumed |
| `Web`, any | `None` — nothing a browser reports about memory is a measurement, `deviceMemory` included | presumed (the probe, WO-10, will be the browser's own arm) |
| `Native`, `Discrete` | `vram_bytes` — the Vulkan heap sum, DXGI's budget, Metal's working set; a card whose reader answered nothing stays presumed, RAM is not VRAM there | measured where `Some` |
| `Native`, `Integrated` | `vram_bytes` (Metal answers for every class) else `system_ram_bytes / UNIFIED_MEMORY_GPU_DIVISOR` (2 — a guard, not a measurement; Metal's own working set is ~75 % of RAM) | measured where either reads |
| `Native`, `Unknown` at the desktop-class line | as `Integrated` — a 3090 over GL is `Other` to wgpu | measured where either reads |
| `Native`, `Unknown` below it | `None` | presumed |

`Some(bytes)` becomes `Capacity { gpu_bytes, host_bytes: system_ram_bytes,
source: Measured }`; `None` becomes `Capacity::presumed(limits)`. `Probed` is
constructible and priced but no profile produces it. The application's
`App::capacity()` is that answer held to the session's pressure presumption,
and every `fit`, `LoopPool::for_scene` and pressure re-fit runs against it —
so the moment `update_device_profile` adopts a discrete card's reading, the
scene is fitted to three quarters of it. Pinned cell by cell by
`a_reading_is_a_measurement_only_where_the_platform_and_class_make_it_one`;
the whole 2160-row matrix runs `check_invariants` on both arms
(`every_synthetic_profile_satisfies_every_invariant`, 504 measured rows).

What the measured arm changes, on the box's own RTX 3090 (24822 MiB read): six
two-hour loops beside their renders cost 4992 MiB against an 18616.5 MiB
allowance and fit at the class rung with every frame — the pool is the
3456 MiB they need, **past the desktop bracket's 3072 MiB loop-pool ceiling,
which is a presumption and binds only the presumed arm**
(`LoopPoolLimits::on`) — and 17080.5 MiB of room is left. A 4 GiB card allows
3072 MiB and the same scene sheds to 9 frames a pane: 8 × 259 s = thirty-four
minutes of the two hours asked for
(`a_measured_capacity_is_the_allowance_the_scene_is_fitted_to`,
`what_five_real_machines_get`). Nothing but the pool, the room and the rungs
shed moves: the fill-rate and wire-capped fields stay at the class rung on
both arms.

**Pressure lowers the capacity figure**, not the allowance, by one economy
fraction per event (`App::refit_under_pressure`): on the presumed arm the two
spellings agree, and on the measured arm lowering the allowance's figure and
then allowing three quarters of *that* would compound the step to 0.675.

**The economy allowance** is `Capacity::economy_allowance(need)` =
`ECONOMY_FRACTION × gpu_bytes − need`, saturating at zero, on every arm. It is
computed and printed in the `Budgets:` and `Loop pool:` lines; its first
consumer is the tile cache's budget (WO-6), which has not joined this
arithmetic yet.

**The runtime clamp.** `fit_holds(scene, budgets, limits, cap)` is the
invariant `fit` promises — need under the allowance or every rung at its stop
— stated once so the application can check the answer it adopts
(`App::fit_scene`). `fit` holds it by construction; a `false` is a defect in
the arithmetic, so a debug build stops on it and a release build logs it once
at warn and holds the loop pool at its floor from then on.

`need` sums terms the tree already prices: `loop_frame_bytes`,
`squallar_volumetric::raymarch::resident_grid_bytes` (read by
`LoopFrameModel::from_budgets`, `squallar-app/src/loop_pool.rs:167`),
`squallar_device_profile::quality::offscreen_bytes` (`quality.rs:268`), the tile
entry cost (§11), the mirror.

**`fit` is today's `demote`, driven by arithmetic instead of a counter:** start
at the rung the class earns, compute `need`, and while it exceeds the allowance
take the next rung. The ladder order is §4.3 made whole: 3D lighting → 3D
offscreen resolution → **loop span** (2D before 3D, floor
`MIN_LOOP_FRAMES_PER_PANE`) → **tile sharpness** (whole-zoom snap) → 3D grid →
raster side → 3D retired. Each rung is a picture the user can still trust; none
is a cap on what the machine may hold.

### 9.3 The only two fractions, and why

*(plan D0, §Decisions)* `NEED_FRACTION = 3/4`: need may occupy at most 75 % of
a **measured or probed** capacity — Metal's own working set is ~75 % of RAM on
M-series, which is the one vendor figure for how much of a unified memory a
renderer is meant to take. It is not applied to a presumed capacity (§9.2).
`ECONOMY_FRACTION = 9/10`: economy may fill to 90 %; the last 10 % is the
in-flight picture, the driver and the compositor.

These are the **only** fractions, they are *limits*, and they are identical on
every machine. There are no shares-of-capacity as *targets* — a rule like "use
half the card" would violate ruling 5 as surely as a bracket does, because it
makes the picture a function of the machine. What varies with the machine is
economy, and economy is `min(generous cap, ECONOMY_FRACTION × capacity − need)`,
split across the tile LRU history, the parsed-tile cache and the render cache.
It is re-evaluated whenever `need` changes and shrinks lazily — at most one
eviction per pump, payloads through `offload::discard`
(`squallar-worker/src/offload.rs:25`) — so the frame thread never pays for it.

### 9.4 The same scene costs the same bytes on every bracket

*(plan D0)* The test ruling 5 asked for:
`the_same_scene_costs_the_same_bytes_on_every_bracket` — one `Scene`, resolved
against `DESKTOP` and `MOBILE` at the same resolution constants, `need()`
byte-identical. Where brackets differ in resolution (`LOOP_IMAGE_SIZE` 2048 vs
1024 on web, `constants.rs:79-81`), the test states the difference as the
resolution term and nothing else.

*(plan D2)* What that means on real cards: a 3090 with two panes and a 2 h span
holds what that costs (~1.4 GiB) and not a byte more; the remaining 90 % is
economy, reclaimable. A 48 GiB card with the same scene holds the same 1.4 GiB.
Six panes at the full render budget (3456 MiB) fit any card above ~4.6 GiB; on a
4 GiB card `fit` shortens the span. *(These are the plan's figures, from the
cost terms the tree already prices; not re-derived here.)*

### 9.5 What the constants become

*(plan D2)* Today's per-bracket constants stop being 21 caps and become:
**floors** (the worst device this build works on), **presumed capacities** (three
numbers, `APP_TEXTURE_BUDGET_BYTES` per arm — §8.4), **cost functions**, and
**one shed order**. Staged: the first landings keep `Bracket::at(promotion)` as
the starting rung and let `fit` shed from there; collapsing the brackets is a
later WO once `need` prices every term.

- **Fill-rate fields stay on the class rung** (`offscreen_bytes`,
  `quality_ceiling`): memory says nothing about milliseconds, and
  `a_discrete_desktop_gpu_can_afford_a_4k_pane_at_native_resolution`
  (`squallar-device-profile/src/budget/tests.rs:676`) pins an Integrated GPU at
  the `Step` offscreen for exactly that reason (the bracket's comment,
  `budget.rs:453-462`: 4K native is ~4.9 ms of a 16.7 ms frame on a 3090; an
  integrated desktop GPU extrapolates to 12–23 ms at 1440×900). `fit` may
  *shed* them (rungs 1–2) but never raise them past the class rung.
- **Loop pool.** `LoopPool::for_scene(scene, budgets, cap, limits)` =
  `min(Σ loop need, room)` where `room = cap.allowance() − (need without loops)`
  — the allowance being the constant on the presumed arm (§9.2). One two-hour
  loop on the desktop bracket is 36 × 16 MiB = 576 MiB whatever the card; six
  are min(3456, 3840 − 6 × 256) = 2304 MiB at the class rung; `LoopPoolState::observe`
  (`squallar-app/src/loop_pool.rs:421`) with its 15-frame dwell and 1.25×
  hysteresis stays as the live re-planner — it divides a *pool size*, which is
  the one capacity-shaped thing already in the tree, and under this plan the
  size it divides is what the loops *need*, capped by what capacity leaves,
  and its dwell now keys on the pool and the frame model as well as the demand.
  `for_promotion`, `for_device` and `back_off` retired at WO-7; the `loop_pool`
  key is named and never read.
- **Span.** `LOOP_SPAN_BUDGET_SECS` is 2 h / 1 h / 45 min per bracket
  (`constants.rs:167-169`) — a capacity presumption in disguise. First landings
  keep it as the per-bracket *demand*; a later WO makes the demand 2 h
  everywhere and lets `fit` shorten it, so a phone that can hold two hours gets
  two hours. What `fit` shortens today is `loop_render_budget` (§4.3 rung 3);
  a pane's own lookback is converted at its cadence by
  `Budgets::frames_for_span_of` and held to both.
- **Buildings.** `squallar-buildings/src/budget.rs` `PrismCeilings.vram_bytes`
  (`:186-189`) is a 16 MiB constant and a third budget outside the system; it
  becomes `BudgetLimits.prism_geometry_bytes` pinned at 16 MiB on every arm
  and a `need` term for a pane drawing buildings, priced at the resolved
  `Budgets.prism_vram_bytes` -- the ceiling the job is fitted inside; the job's
  own fitted `budgeted_bytes()` is not reachable from squallar-device-profile,
  whose charter declares squallar-radar and nothing else. `squallar-worker`'s
  agreement test holds the resolved figure equal to the worker's default.

### 9.6 Snugness, on both arms

*(plan D2)* The constant-based tests —
`the_whole_application_fits_its_gpu_ceiling`,
`the_app_ceiling_is_not_slack_enough_to_hide_a_doubling`
(`constants/tests.rs:116,138`),
`the_span_budget_is_the_longest_the_ceiling_can_pay_for` (`:197`),
`the_documented_per_class_figures_are_what_the_arms_actually_say` (`:334`) —
are statements about the **presumed arm** and run unchanged. On the **measured
arm** the invariant is `need ≤ NEED_FRACTION × cap` and `need + economy ≤
ECONOMY_FRACTION × cap`, asserted by `check_invariants` over the matrix and
clamped-and-logged at runtime (WO-9). A term whose arithmetic silently doubles
is caught where it already is — the byte tests beside the arithmetic. This is
the answer to §3.2's tautology objection: the two sides of the snugness test
are still independent on the arm where the constant is the capacity, and on the
other arm the constant is not in the test at all.

---

## 10. Pressure: reclaim, then re-fit, within the session

### 10.1 What exists

The only demotion trigger at `e2c1e664` is a lost surface:
`wgpu::CurrentSurfaceTexture::Lost` (`squallar-app/src/app_render.rs:2744`) →
`App::back_off_budgets` (`:3829`), which halves the loop pool toward its floor,
increments `BudgetMemo::steps_back`, re-resolves, and writes both under
`LOOP_POOL_KEY` and `BUDGET_MEMO_KEY = "budget_steps"`
(`squallar-app/src/budget_memo.rs:7`). A non-volume `wgpu::Error::OutOfMemory`
is **re-panicked in debug and logged in release** —
`squallar_volumetric::install_error_latch` (`squallar-volumetric/src/lib.rs:274`)
and `disposition` (`:307`) know only "is this the volume's label or not". winit
0.30.13 (`Cargo.lock`) delivers `ApplicationHandler::memory_warning` on
Android/iOS; the app does not implement it. Running out of wasm linear memory
triggers nothing; the tab dies *(plan)*.

The reclaim machinery already exists and is what "kick things out" builds on:
`Pane::evict_textures_outside_render_set(budget)` (`squallar-egui/src/pane.rs:1552`),
`VolumeStore::enforce_budget` (`squallar-volumetric/src/volume_bridge.rs:328`),
`App::evict_unneeded_loop_scans` (`squallar-app/src/app.rs:1990`),
`Gui::release_held_rasters` (`squallar-egui/src/ui.rs:2331`).

### 10.2 Triggers

*(plan D4)*

```rust
pub enum Pressure { SurfaceLost, OutOfMemory, MemoryWarning, LinearMemory { used: u64, max: u64 } }
```

1. **Surface lost** — as today.
2. **`wgpu::Error::OutOfMemory` on any label** — `squallar_volumetric::disposition`
   gains a first arm matched on the enum, calling
   `squallar_gpu::pressure::note_out_of_memory()`; `App` polls once per frame.
3. **`memory_warning`** — `impl ApplicationHandler for App` gains it (Android
   `onLowMemory`, iOS `didReceiveMemoryWarning`; zero deps, no JNI).
4. **wasm watermark** on the 2 s tick: `max(page, worker)` (§8.5) against
   `WASM_LINEAR_MEMORY_MAX_BYTES = 1 << 30`, pinned to the link flag by a test:
   **warn at 75 %, act at 87 %**; re-fire needs `used ≥ last + 32 MiB`.

### 10.3 Response, in order, all in-session

*(plan D4)*

- **(a) Evict economy** to zero — tile LRU history down to the working set, the
  parsed cache, the render cache, held rasters — through the existing reclaim
  calls, bounded per frame. This is the whole response when need alone still
  fits, which is the common case ruling 5 anticipates: the picture does not
  change.
- **(b) Lower the session's capacity presumption** and **re-fit** the scene,
  which sheds rungs only if need alone no longer fits. *As landed (WO-7,
  2026-09-02):* the presumption comes down to `allowance × ECONOMY_FRACTION`,
  not to `resident_at_event × 0.9` — nothing in the tree measures what was
  resident (the profile's `vram_bytes` is capacity and the upload ledgers are
  running totals), and lowering to nine tenths of *need* can never be fitted by
  that same need, so it would shed a rung on every event including one whose
  whole cause was economy. The wall is therefore taken to be at most the
  allowance in force, less the economy the eviction just took; a second event
  lowers it again. `App::refit_under_pressure` carries the argument.
- **(c) Restore economy and rungs** as pressure clears, in the shape
  `LoopPoolState::observe` already has (dwell, then hysteresis).

### 10.4 Nothing persisted, and why

Ruling 6 (§7.5): capacity is measured, probed or presumed at startup; pressure
is answered within the session. The `budget_steps` and `loop_pool` kv keys stop
being read (a stale key is harmless; kv has no delete). No fingerprint: with no
memo there is nothing to key — which also retires the f43c464f-era hazard that a
back-off learned on one GPU followed the config to another. The reasons the
memo was designed for are answered elsewhere: the 1:1 reopen by determinism
(§4.1 item 5); the "value that must not be lost to the 3 s autosave" by there
being no value.

The transient-decode-spike case (MRMS at the wall) is answered upstream by the
grib peak-allocation item in the owned queue *(plan)*; the watermark makes it
survivable meanwhile.

**The documented later add-on.** If a real device proves to hit the WebGL2 wall
on every launch with the same scene, a one-failure memory is a small later
add-on. **Not in this plan** (ruling 6); it would need the device and the
repeated launch as evidence first.

### 10.5 Pins that move

*(plan §Pins; names as landed at WO-7)* `a_lost_surface_steps_the_budgets_down_a_rung_and_writes_it_at_once`
→ (WO-4) `a_lost_surface_evicts_economy_and_steps_down_and_writes_nothing` →
`a_lost_surface_evicts_economy_and_refits_and_writes_nothing`, joined by
`a_lost_surface_refits_a_scene_the_lowered_presumption_no_longer_holds` (a
scene that fits the presumption to the byte, then loses a surface: the loop
history is the first rung that pays);
`a_backed_off_machine_reopens_where_it_left_off` → (WO-4)
`a_reopen_starts_at_the_ladder_top_whatever_the_store_holds` →
`a_reopen_fits_the_same_scene_to_the_same_budgets`;
`the_ladder_position_stops_rising_once_every_rung_is_at_its_stop` →
`a_session_that_keeps_failing_settles_at_the_floor_and_never_writes` (nine
steps on the desktop bracket, twelve tenths off the presumption);
`an_out_of_memory_error_steps_the_ladder_once_per_frame_and_writes_nothing` →
`an_out_of_memory_error_refits_once_per_frame_and_writes_nothing`;
`a_memory_warning_evicts_economy_and_steps_down` →
`a_memory_warning_evicts_economy_and_refits`. The source-scrape order pin
`the_device_profile_is_folded_in_before_any_budget_is_spent` (`:1214-1237`:
`self.update_device_profile(` precedes `LoopPoolLimits::from_budgets` and
`self.budgets.quality_ceiling` in `install_volume_bridge`) is untouched.

---

## 11. The tile cache as the model in miniature

### 11.1 What exists, and why it is "downright broken on web"

The tile cache is wholly outside the budget system.
`squallar-egui/src/tile_source.rs` keeps a count-bounded `LruCache` per source on
its **own** cfg cascade (`:182-195`, on `any(target_os = "android", target_os =
"ios")` rather than `mobile`): `TILE_CACHE_ENTRIES` 100 / 100 / 256
(`WASM_TILE_CACHE_ENTRIES = 100`, `:213`), `PARSED_TILE_CACHE_ENTRIES` 24 / 24 / 96
*(plan)*. There is no byte accounting. Its own doc states the rule it breaks:
"An LRU below the working set is not a slower cache, it is a broken one"
(`:176`).

*(plan, "What exists")* **Need and economy share one LRU**, so a pending or
failed `None` marker takes a full slot and can be evicted mid-flight (a double
fetch), and history evicts tiles that are on the glass. A styled entry costs
**~1.03 MB, not the documented 652 KB** (`MEASURED_VECTOR_TILE_BYTES = 652_112`,
`:255`) — `slot_for` adds a flattened `TileMeshes` copy nobody priced.
`tiles/tests.rs` pins the tier table
(`the_resident_counts_are_the_ones_the_tier_table_quotes`,
`squallar-egui/src/tiles/tests.rs:895-903`: 1920×1080 → 54 whole / 84 worst;
1920×1200 → 54 / 96; 2560×1440 → 77 / 144; 3840×2160 → 160 / 299) and asserts
that the wasm arm **overruns at 2560×1440 as intended** (`:942-960`). The user's
own window (2878×1651, the rig's `huge` leg) has **104 tiles per source on the
glass at a whole zoom and 187 between zooms** (`tiles_resident_grid`,
`squallar-egui/src/tiles.rs:1076`: a 13×8 grid and a 17×11 grid — the plan's
tier-table row `(2878, 1651, 104, 187)`), and **110 and 193 resident once the
`WARM_ANCESTOR_STEPS` net is added** (`tiles.rs:1023`, `tiles_resident_with_warm_net`
`:1038`; +6 on each, by subtraction) — **against a cap of 100**. Below the
working set at every zoom on either denominator: glass alone, or glass plus
ancestor net. This is the leading hypothesis for ruling 1's "downright broken on web", and it
is **measured in WO-1 before any fix**. The GPU `TileMeshStore` has no capacity
of its own; identity is minted per put, so no counter tells a first upload from
a re-upload; the 3,070 uploads vs 2,848 evictions are **undiagnosed** *(plan)*.

### 11.2 The model, applied

*(plan D5)* **Need** = tiles on glass + ancestor net —
`tiles_resident_with_warm_net(actual_rect, bias, layers)`
(`squallar-egui/src/tiles.rs:1038`), per pass, no canvas constant. **Economy** =
the LRU history and the parsed-tile cache. **Capacity** = its share of the
host-cache allowance (§9.3). **Shed** = whole-zoom snapping.

- **WO-1, diagnosis, no behaviour change.** `CachedTile` gains
  `restyle_pending`; a new `squallar-egui/src/tile_source/cache_ledger.rs`
  (atomics, always on) keyed by `CacheRole { Base, Terrain }` counts requests,
  restyle asks, refetch-after-eviction, first / restyle / duplicate / orphan
  puts, pending and resident evictions and bytes, plus resident entries and
  bytes, overrun bytes, parsed bytes. A bounded "recently evicted"
  `LruCache<TileId, ()>` (4 × cap) per source distinguishes
  refetch-after-eviction from first sight. A new **sibling** telemetry line
  `tile cache (base): …` — never edit `ground tiles:`, `drive.py` matches it
  with a strict field list. A new Tier-2 leg `tilecache` (opt-in): dense city
  core at z14, control 1280×900 and repro 2878×1651. A host pin on the loopback
  `TileServer` at cap 100 over a 12×12 grid **lands red on purpose** and flips
  to `== 0` in WO-6.
- **WO-6, need and economy separated, in bytes.**
  `squallar-egui/src/tile_source/byte_lru.rs`: `ByteLru<K, V>` with `budget`,
  `resident`, `floor_entries` (the pass's working set — eviction never goes
  below it; the excess is `overrun_bytes`), every entry charged at least
  `MARKER_BYTES`. `CachedTile::bytes()` prices the raster or the styled shapes
  plus `meshes.bytes()`; `ParsedTile::heap_bytes()` prices the parsed cache.
  Shrink lazily, grow eagerly. Plumbing: `FrameInputs.tile_cache: TileCacheBudget`
  (`squallar-egui/src/shell_api.rs:73` — a new field costs zero `self.gui.`
  reaches) → `Gui::apply_frame_inputs` → `MapTileState::set_budget` →
  `HttpsTiles::set_budget`. The six count constants and `TERRAIN_TILE_CACHE_ENTRIES`
  are deleted. **Presumed-arm floors** (MiB, styled / parsed / terrain): WASM
  48 / 48 / 25, MOBILE 48 / 48 / 25, DESKTOP 160 / 192 / 64 — each argued from the
  measured entry costs and the canvases they must hold; on the measured arm the
  economy allowance comes from `fit`. Terrain rasters stay outside the GPU sum
  (WASM is at 278 of 288 MiB), with a named-omission test.
- **WO-5, conditional.** An in-flight set so an evicted pending marker never
  double-fetches — only if WO-1 shows `orphan` / `duplicate` > 0. Stable mesh
  identity across restyles is recommended against: a restyle changes vertex
  colours.
- **WO-12, the shed rung.** A bytes-based bias gate: after a 15-pass dwell of
  measured overrun, `tile_zoom = zoom.floor()` for that source (187 → 104 tiles on
  the glass of the user's canvas, plan) — sharpness, never input latency; hysteresis both
  ways. **The ancestor net is never traded**: it is what keeps the map from
  going blank while tiles arrive, and blank is a wrong picture where soft is
  not.
- **WO-14, page-thread cost.** Measure `tile phase (parse|style)` p50/p99 on
  Firefox, then Chromium; if `style` dominates, a resumable `StyledCursor` in
  `vendor/walkers/src/mvt.rs`. Worker offload of tile styling is the structural
  follow-on, its own WO.

### 11.3 Why this is the whole model in one cache

The tile cache has, in one struct, every quantity §9 names: a working set that
is a pure function of the glass (need), a history that is worth exactly what
memory is free (economy), a byte allowance that comes from capacity, and one
rung that trades sharpness for bytes without ever trading correctness (shed).
It also has every defect the rest of the app is being protected from: a count
where a byte is needed, need and economy in one LRU, a cap sized for a canvas
the user's window exceeds, and no instrument that can tell a first fetch from a
refetch. Fixing it first (β lane, WO-1/3/5/6/12/14) is where "downright broken
on web" is characterised with numbers, and where the user's 2878×1651 window
stops evicting the tiles on its own glass — the plan's stated value of stopping
after WO-6.
