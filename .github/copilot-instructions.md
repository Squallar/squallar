# Copilot Instructions — Rustdar

## Project Overview

Rustdar is a cross-platform NEXRAD weather radar viewer built in Rust. It fetches real-time radar data from AWS, renders it onto a map, and runs on both desktop (Linux/macOS/Windows) and Android. The GUI uses **egui** with a **wgpu** rendering backend and **winit** for windowing.

Ensure that any architecture or major change to the code is reflected in this document, as it serves as the single source of truth for project structure and conventions.

## Workspace Architecture

Seven crates in a Cargo workspace (`resolver = "2"`, all edition 2024):

| Crate | Role |
|---|---|
| `rustdar-platform` | Main binary + lib (dual output: binary from `main.rs`, library `rustdar_platform_lib` with crate types `staticlib`/`cdylib`/`rlib` for Android). Owns the winit event loop, wgpu surface, egui renderer, and Tokio runtime. `app.rs` orchestrates lifecycle and has two `#[path]` submodules: `app_fetch.rs` (fetch dispatch/actions) and `app_render.rs` (redraw, channel polling, render orchestration). State module `app_state.rs` holds wgpu rendering resources. `render_dispatch.rs` manages per-pane render state and Level III data cache. `platform.rs` defines the `PlatformBridge` trait with desktop/Android impls. |
| `rustdar-egui` | Pure egui UI layer. Defines `Gui` (map + controls) and `GuiAction` enum. Uses the `walkers` crate for map tiles (CartoDB no-labels + labels-only overlay). No wgpu dependency. Large `Gui` impl is split across six `#[path]` submodules: `ui_popups.rs`, `ui_config.rs`, `ui_mobile.rs`, `ui_map_overlays.rs`, `ui_desktop.rs`, `ui_map.rs`. `ui_map.rs` has its own `#[path]` submodule `ui_map_pane.rs` for per-pane map content rendering (`PaneRenderCtx`, `render_pane_map_content()`). Public modules: `actions.rs` (GuiAction/RadarConfig), `geo.rs` (geometry), `hatch.rs` (CIG hatching), `layers.rs` (LayerManager/LayerKind), `overlay_cache.rs` (viewport-keyed polygon cache), `overlay_state.rs` (OverlayData/OverlayState<T>), `pane.rs` (PaneState/PaneLayout/RadarImageData), `tiles.rs` (CartoDB tile source + slippy-map math). |
| `rustdar-radar` | Radar data logic: fetching Level II scans from AWS and Level III products from TGFTP (`scan.rs`), rendering to 1800×1800 RGBA images via Web Mercator projection (`render.rs`), data-driven color palettes via `ColorScale` lookup tables (`palette.rs`), the static site database (`sites.rs`, 207 NEXRAD sites via `LazyLock<HashMap>`), and shared data types (`types.rs`: `RadarProduct` with 13 variants, `ScanInfo`, `ImageBounds`, constants). Uses `rayon` for parallel rendering with `AtomicU32` buffers. `render.rs` also contains product-specific value conversion helpers moved from `nexrad-level3`: `build_vil_lut()`, `decode_legacy_thresholds()`, `nexrad_float16()`. |
| `rustdar-overlays` | Weather overlay data: SPC convective outlooks (Days 1–8, GeoJSON), Mesoscale Discussions (RSS via `roxmltree` + `LAT...LON` polygon extraction), and NWS alerts (API + zone geometry resolution with disk caching). Pre-computes triangulations (`earcutr`) and geo-bounds at fetch time. Simplifies polygons with Ramer-Douglas-Peucker (ε=`SIMPLIFY_EPSILON` 0.005° in `types.rs`). Types: `OverlayFeature`, `GeoPolygon`, `HatchPattern`, `SpcOutlook`, `SpcDiscussion`, `NwsAlert`. Alert enums (`AlertSeverity`, `AlertUrgency`, `AlertCertainty`) use a `str_enum!` macro for boilerplate-free `FromStr` impls. Centralized alpha constants (`CIG_FILL_ALPHA`, `REGULAR_FILL_ALPHA`, `NWS_FILL_ALPHA`, `STROKE_ALPHA`) in `types.rs`. |
| `nexrad-level3` | NEXRAD Level III product decoder. Handles WMO header stripping, zlib/BZ2 decompression, Message Header + Product Description Block parsing, and symbology block decoding (radial packets: legacy 4-bit RLE packet 0xAF1F, digital packet 16, generic packet 28). PDB exposes `data_scale()`, `data_offset()`, `elevation_angle()` accessors. Product-specific LUT/threshold decoding lives in `rustdar-radar/src/render.rs`. Binary read helpers use a generic `read_bytes<const N>` core function. |
| `rustdar-android` | Thin Android entry point (`android_main`). Configures `cargo-apk`, initializes logging + rustls-platform-verifier (with custom `PathClassLoader` for DEX), registers back handler, starts GPS polling thread, then delegates to `rustdar-platform`. Requires DEX injection via `build-android.sh`. |
| `rustdar-android-theme` | JNI-based Android dark/light theme detection (reads `uiMode` from Resources Configuration). Desktop stub returns `false`. |

**Data flow:** `Gui::ui()` returns `Vec<GuiAction>` → `App::process_gui_actions()` dispatches via `handle_radar_action()` / `handle_overlay_action()` in `app_fetch.rs` → fetches spawn on `App::tokio_runtime` → results arrive via `std::sync::mpsc` channels in `ChannelHub` → radar rendering runs on `std::thread::spawn` (not Tokio) with rayon parallelism → image textures loaded into egui via `set_radar_image_for_pane()`.

## Key Types & Data Structures

### ChannelHub (`rustdar-platform/src/channels.rs`)
Six channel pairs for async communication: `scan` (Level II fetch results with `ScanResponse`), `render` (completed radar images with `RenderResponse`), `level3` (Level III product fetches), `outlook` (SPC outlooks), `alert` (NWS alerts), `discussion` (SPC Mesoscale Discussions). All use `std::sync::mpsc` with non-blocking `try_recv()` polling in the event loop.

### RenderDispatcher (`rustdar-platform/src/render_dispatch.rs`)
Manages `Vec<PaneRenderState>` (per-pane render tracking with `render_in_flight`, `last_rendered`, `cached_render`), a `level3_data: HashMap<(RadarProduct, String), Arc<Level3Message>>` cache, and generation counters (`fetch_generation`, `render_generation`). Key methods: `spawn_level2_render()`, `try_spawn_level3_render()` (both delegate to private `spawn_render()` helper), `reset_panes()`, `clear_last_rendered()`.

### PlatformBridge trait (`rustdar-platform/src/platform.rs`)
Abstraction with `DesktopPlatform` and `AndroidPlatform` impls. Methods: `poll_theme()`, `poll_location()`, `query_insets()`, `handle_back()`, `detect_dark_theme()`, `zone_cache_dir()`, `needs_process_exit()`. Desktop zone cache defaults to `XDG_CACHE_HOME/rustdar/zones` or `~/.cache/rustdar/zones`. Android requires explicit `std::process::exit(0)` after event loop exit.

### RadarProduct (`rustdar-radar/src/types.rs`)
13 variants: 6 Level II base moments (`Reflectivity`, `Velocity`, `SpectrumWidth`, `DifferentialPhase`, `CorrelationCoefficient`, `DifferentialReflectivity`) + 1 derived (`NormalizedRotation`, computed from velocity azimuthal shear) + 6 Level III products (`StormRelativeVelocity`, `SpecificDifferentialPhase`, `EchoTops`, `VerticallyIntegratedLiquid`, `HydrometeorClassification`, `PrecipitationRate`). Key methods: `code()`, `name()`, `sort_order()`, `is_level3()`, `tgftp_dirs()`, `get_moment()`, `format_value()`.

### Multi-Pane System (`rustdar-egui/src/pane.rs`)
`PaneState` holds per-pane: `selected_product`, `selected_elevation`, `radar_image: Option<RadarImageData>`, `layers: LayerManager`, `map_memory: MapMemory` (walkers viewport), overlay caches (`spc_overlay_caches`, `nws_overlay_cache`, `spc_md_overlay_cache`). `PaneLayout` defines grid arrangements: 1–6 panes desktop (`MAX_PANES_DESKTOP = 6`), 1–4 mobile (`MAX_PANES_MOBILE = 4`). Predefined layouts: 1→[1], 2→[2], 3→[2,1], 4→[2,2], 5→[3,2], 6→[3,3].

### LayerKind (`rustdar-egui/src/layers.rs`)
Layer toggles: `Radar`, `SpcCategorical`, `SpcTornado`, `SpcWind`, `SpcHail`, `SpcProbabilistic`, `SpcMesoscaleDiscussions`, `NwsWarnings`, `NwsWatches`, `NwsAdvisories`, `CityLabels`, `RadarSites`. Defaults: Radar/MDs/NWS alerts/CityLabels enabled; SPC outlooks/RadarSites disabled.

### Overlay Caching (`rustdar-egui/src/overlay_cache.rs`)
`OverlayLayerCache` stores projected polygons keyed by `ViewportKey` (zoom level + quantized screen origin + dimensions). Cache invalidates on any viewport change or `data_generation` bump. Triangulation indices are pre-computed at fetch time;  cache rebuilds only do O(n) vertex projection. `CachedPolygon` stores screen points, triangle indices, bounding rect, and hatch lines.

## Key Conventions

- **`#![warn(clippy::all)]` and `#![forbid(unsafe_code)]`** on `rustdar-platform` (lib.rs + main.rs) and `nexrad-level3`. Other crates don't set these explicitly. Only `rustdar-android-theme` uses `unsafe` (JNI). Keep it that way.
- **CI auto-applies clippy fixes** on PRs via `cargo clippy --fix`, then re-runs strict clippy. Always pass `cargo clippy --all-targets --all-features`.
- **Generation counters** (`fetch_generation`, `render_generation`) guard against stale async/threaded results. Increment the counter before spawning work; discard results with generation < current.
- **Platform-conditional compilation:** `#[cfg(target_os = "android")]` gates appear in `rustdar-platform` (theme detection, Android theme polling thread, mobile vs desktop UI) and dependency sections of Cargo.toml files. Android requires `openssl-sys` with `vendored` feature in both `rustdar-radar` and `rustdar-overlays`.
- **No web target.** wgpu limits use native adapter limits, not WebGL2.
- **`#[path]` submodule pattern:** Large files like `ui.rs` and `app.rs` are split using `#[path = "ui_xxx.rs"] mod xxx;` to avoid converting to directory modules. `ui.rs` has six submodules: `popups`, `config`, `mobile`, `map_overlays`, `desktop`, `map`. `ui_map.rs` has a nested submodule `pane_render` via `ui_map_pane.rs`. `app.rs` has two: `fetch` and `render`. Extracted methods use `impl super::Gui {}` (or `impl super::App {}`) blocks with `pub(super)` visibility. Free functions use `pub(super) fn`.
- **Workspace build profiles:** Dev builds strip symbols and optimize dependencies with `opt-level = "s"` (workspace code unoptimized for debuggability). Release uses `opt-level = "s"` + LTO.
- **Pinned external crate versions:** `nexrad-data =1.0.0-rc.5`, `nexrad-model =1.0.0-rc.2`, `nexrad-decode =1.0.0-rc.3` are pinned with `=` (exact version). Don't upgrade without testing.
- **Config persistence:** `Gui` saves/loads `ui.json` (JSON format via serde_json; fields: pane_count, viewport_sync, sync_layers, auto_poll, site) from `Gui::default_config_dir()` (`XDG_CONFIG_HOME/rustdar` or `~/.config/rustdar`). Loaded at `App::new()`, saved on exit.

## Build & Run

```bash
# Desktop (requires libasound2-dev on Ubuntu)
cargo build --workspace
cargo run -p rustdar-platform

# Android (requires cargo-apk, ANDROID_HOME, NDK, d8, zipalign, apksigner)
./build-android.sh            # builds APK via cargo-apk, injects DEX, signs
adb install -r <path-to-apk>
adb logcat -s rustdar         # view logs

# Lint (matches CI)
cargo clippy --all-targets --all-features
```

**Android build details:** `build-android.sh` does three steps: (1) `cargo apk build --no-default-features`, (2) compile Java helpers + extract rustls-platform-verifier AAR + d8 to produce `classes.dex`, (3) inject DEX into APK, zipalign, and sign with debug keystore. The DEX injection is needed because pure NativeActivity has no DEX class paths, but rustls-platform-verifier requires Kotlin certificate verification classes.

## Architecture Patterns to Follow

- **UI ↔ Platform boundary:** `rustdar-egui` must not depend on wgpu/winit. It communicates intent through `GuiAction` and receives state via setter methods on `Gui` (e.g., `set_scan_info()`, `set_radar_image_for_pane()`). `Gui::ui(&mut self, &egui::Context) -> Vec<GuiAction>` is the main entry point called each frame. `Gui` exposes `active_pane()` / `active_pane_mut()` for direct access to the current pane's `PaneState`. Android-only fields (`show_menu`, `double_tap_detector`) are grouped into a `MobileState` substruct gated with `#[cfg(target_os = "android")]`.
- **Async work:** Network I/O goes through the shared `App::tokio_runtime`. CPU-heavy radar rendering uses `std::thread::spawn` + rayon, not Tokio tasks. Background tasks send results via mpsc channels and call `notify_redraw()` (in `app.rs`) to wake the event loop.
- **Lazy rendering state:** `AppState` (wgpu device/queue/surface) is created lazily on first `handle_redraw()` after window creation, not in `resumed()`. This prevents ANRs during Android configuration changes (fold/unfold).
- **Surface loss recovery:** If `surface.get_current_texture()` returns `SurfaceError::Lost`, drop entire `AppState` but keep `cached_render` in `PaneRenderState` for instant restore without re-rendering. Next `handle_redraw()` recreates `AppState` with fresh surface.
- **Radar rendering:** Both Level II and Level III rendering share a common `render_with_projection()` pipeline that creates a `MercatorProjection` + `RenderBuffers`, delegates parallel radial rendering to a caller-provided closure, then produces output. Helper functions `find_sweep()`, `compute_azimuth_spacing()`, and `compute_max_range()` extract sweep selection and parameter computation. `AtomicU32` buffers are written in parallel by rayon with `Ordering::Relaxed`, then converted to `Vec<u8>` RGBA. Gate parameters (`first_gate_range`, `gate_interval`, `gate_count`) are read from each radial's moment data — they vary by VCP and moment type. Renders produce dual output: RGBA image + parallel `Vec<f32>` value data (for hover tooltips). NormalizedRotation (NROT) is a special case: computed from velocity azimuthal shear (not a raw moment), scaled by `NROT_SCALE = 250.0`, with gates < 5 km excluded.
- **Level III rendering:** `render_level3_message_to_image()` extracts radial packets from symbology, resolves value mapping (LUT for legacy 4-bit/VIL via `build_vil_lut()`/`decode_legacy_thresholds()`, scale/offset for digital products), and feeds into the shared `render_with_projection()` pipeline. SRV values are converted knots→m/s (×0.514444). The LUT/threshold helpers and `nexrad_float16()` live as free functions in `render.rs` (not in nexrad-level3).
- **Radar image data:** `RadarImageData` struct (in `pane.rs`) bundles texture handle, lat/lon, max range, and `value_data: Arc<Vec<f32>>`. Prefer this over raw tuples.
- **ImageBounds** (in `rustdar-radar/src/types.rs`) computes geographic bounds from a radar site lat/lon for the 1800×1800 image. Uses Web Mercator Y for vertical alignment with map tiles. `geo_to_pixel()` converts lat/lon → pixel coordinates for hover value lookup.
- **Elevation angle rounding:** `ScanInfo::from_scan()` rounds elevation angles to 1 decimal place to collapse SAILS/MRLE duplicate scans at nominally the same angle.
- **Scan loading:** `ScanInfo::from_scan()` (in `rustdar-radar/src/types.rs`) inspects a `nexrad_model::data::Scan` to discover available products and elevation angles via `discover_product_elevations()` helper. Level III products are added with empty elevation vectors (populated as L3 data arrives). This keeps radar domain logic in the `rustdar-radar` crate.
- **Color palettes** in `rustdar-radar/src/palette.rs` map radar product values → RGBA with constant `TRANSPARENCY = 180` alpha. Uses data-driven `ColorScale` lookup tables (sorted threshold → RGB pairs) with a shared `scale_color()` function, dispatched by `get_color_for_value()`. Bidirectional scales (velocity, SRV, NROT) use separate inbound/outbound tables. Alpha 0 = transparent (no data/below threshold). `format_value()` on `RadarProduct` formats hover display strings with units.
- **Geometry helpers** in `rustdar-egui/src/geo.rs` provide shared screen-space utilities (`point_in_polygon`, `aabb`, `dashed_line_shapes`, `clip_line_to_polygon`). Don't duplicate these in consumer modules.
- **Shared UI helpers:** `render_pane_selector()` in `ui.rs` draws pane count buttons and active-pane selector for both desktop and mobile. `render_layer_controls()` draws product/elevation/layer toggles. `show_detail_popup()` in `ui_popups.rs` handles popup window sizing and builder boilerplate for both alert and MD detail popups.
- **Theme detection:** Desktop uses `dark-light` crate + `WindowEvent::ThemeChanged`; Android uses JNI polling every 2 seconds in a background thread. Both feed `cached_dark_theme` in `App`. Only update egui visuals when theme actually changes (`applied_visuals_dark` guard in `EguiRenderer`).
- **Map tile strategy:** CartoDB no-labels base map + separate labels-only tile layer drawn on top of radar/overlays. This prevents radar imagery from obscuring city/street names. Light/dark variants are kept alive across theme toggles (`MapTileState` uses take/restore ownership pattern for per-pane rendering).
- **Overlay drawing:** `OverlayDrawContext` in `ui_map_overlays.rs` bundles shared drawing state (ui, projector, screen_rect, viewport key, click detection) and exposes three drawing methods (`draw_spc_overlays()`, `draw_spc_discussions()`, `draw_nws_alerts()`) plus a `clicked_polygon()` helper for hit-testing cached polygons. NWS alert iteration uses `iter_alert_features()` helper to flatten alert→feature→cached-geometry without manual index tracking. All share `OverlayLayerCache` for viewport-keyed geometry caching. Hatching uses a two-pass system: pass 1 triangulates polygons, pass 2 generates hatch lines with exclusion zones (lower-CIG hatches masked by higher-CIG fills). CIG1 = dotted 45° lines, CIG2 = solid 135° lines, CIG3 = cross-hatch.
- **Per-pane map rendering:** `ui_map_pane.rs` (submodule of `ui_map.rs`) contains `PaneRenderCtx` struct and `render_pane_map_content()` entry point. Splits rendering into Phase 1 (immutable `&Ui` — overlays, radar image, labels) and Phase 2 (mutable `&mut Ui` — radar sites via `render_radar_sites()`, user location via `render_user_location()`). This separation avoids borrow conflicts between `OverlayDrawContext` (holds `&Ui`) and allocation-based drawing.
- **Viewport sync:** When enabled, snapshots zoom+position before rendering, detects which pane changed, and propagates to all others. Layer sync copies active pane's layer states to all panes.
- **Auto-polling:** Radar scans poll every 60s (exponential backoff on error, capped at 300s, reset to 60s on success). NWS alerts and SPC MDs auto-refresh every 120s when their layers are enabled.

## Data Fetching Details

### Level II Scans (`rustdar-radar/src/scan.rs`)
- Fetches from AWS S3 via `nexrad-data` crate. Falls back to previous day if no files on requested date.
- Parses filenames to extract timestamps, picks closest match to requested time.
- `check_and_fetch_latest()` combines check + fetch into single S3 LIST to avoid duplicate requests.
- Timestamps flow: user picks local time → `local_to_utc()` converts for API calls.

### Level III Products (`rustdar-radar/src/scan.rs`)
- **TGFTP:** `get_tgftp_product()` fetches from `https://tgftp.nws.noaa.gov/SL.us008001/DF.of/DC.radar/DS.{dir}/SI.{site}/sn.last` — always returns latest product, no listing needed.
- Site codes convert 4-letter ICAO to 3-letter for Level III (e.g., `KTLX` → `TLX`). Non-CONUS sites keep full code.
- TGFTP directories per product: SRV→`["56rm0".."56rm3"]`, KDP→`["163k0"]`, EchoTops→`["135et"]`, VIL→`["134il"]`, HHC→`["177hh"]`, PrecipRate→`["176pr"]`.

### SPC Outlooks (`rustdar-overlays/src/spc/`)
- GeoJSON endpoints: Days 1–3 use short-range URLs (`day1otlk_cat.lyr.geojson`), Days 4–8 use extended-range.
- Parses `LABEL`/`LABEL2` for risk level, `fill`/`stroke` hex colors, `VALID`/`EXPIRE` timestamps.
- Simplifies polygons at fetch time via RDP (ε=0.005° ≈ 500m).

### SPC Mesoscale Discussions (`rustdar-overlays/src/spc/discussion.rs`)
- Fetched from RSS feed, parsed with `roxmltree`.
- Polygon coordinates extracted from `LAT...LON` blocks: 7–8 digit tokens where first 3–4 digits = lat×100, last 4 = lon×100.
- Classified by topic keywords: `Convective`, `WinterWeather`, `Other`.

### NWS Alerts (`rustdar-overlays/src/nws/`)
- API: `https://api.weather.gov/alerts/active?status=actual`
- Zone geometry resolution: deduplicates zone URLs, checks disk cache (1-year TTL), fetches misses concurrently (max 50) via `futures::stream::buffer_unordered()`. `fetch_zone_geometry()` delegates to `fetch_zone_json()` (HTTP) and `parse_zone_polygons()` (geometry extraction + simplification).
- Alert colors use a static `ALERT_COLORS` table of `AlertColorEntry` structs (keyword list + RGB) in `nws/colors.rs`. `alert_color()` does a linear scan matching all keywords against the lowercased event name.

## Key Constants

| Constant | Value | Location |
|---|---|---|
| `IMAGE_SIZE` | 1800 | `rustdar-radar/src/types.rs` |
| `MAX_RANGE_KM` | 230.0 | `rustdar-radar/src/types.rs` |
| `PIXELS_PER_KM` | ~3.91 | Derived: 1800 / (2 × 230) |
| `TRANSPARENCY` | 180 | `rustdar-radar/src/palette.rs` (alpha for all rendered data) |
| `NROT_SCALE` | 250.0 | `rustdar-radar/src/render.rs` |
| `NROT_MIN_RANGE_KM` | 5.0 | `rustdar-radar/src/render.rs` |
| `RENDER_WIDTH` | 1920 | `rustdar-platform/src/constants.rs` |
| `RENDER_HEIGHT` | 1080 | `rustdar-platform/src/constants.rs` |
| `MAX_PANES_DESKTOP` | 6 | `rustdar-egui/src/pane.rs` |
| `MAX_PANES_MOBILE` | 4 | `rustdar-egui/src/pane.rs` |
| `HATCH_SPACING` | 10 | `rustdar-egui/src/hatch.rs` |
| `SIMPLIFY_EPSILON` | 0.005 | `rustdar-overlays/src/types.rs` |
| `CIG_FILL_ALPHA` | 40 | `rustdar-overlays/src/types.rs` |
| `REGULAR_FILL_ALPHA` | 100 | `rustdar-overlays/src/types.rs` |
| `NWS_FILL_ALPHA` | 80 | `rustdar-overlays/src/types.rs` |
| `STROKE_ALPHA` | 255 | `rustdar-overlays/src/types.rs` |
| `DOUBLE_TAP_TIMEOUT_S` | 0.4 | `rustdar-egui/src/ui_mobile.rs` |
| `DOUBLE_TAP_DISTANCE_PX` | 50.0 | `rustdar-egui/src/ui_mobile.rs` |
| `TAP_DURATION_MAX_S` | 0.3 | `rustdar-egui/src/ui_mobile.rs` |
| `TAP_DISTANCE_MAX_PX` | 20.0 | `rustdar-egui/src/ui_mobile.rs` |
| `ZOOM_DRAG_SENSITIVITY` | 150.0 | `rustdar-egui/src/ui_mobile.rs` |

## Gotchas & Non-Obvious Behaviors

- **Deferred exit:** `GuiAction::Exit` during `handle_redraw()` sets `exit_requested = true` because the event loop reference isn't available. Actual exit happens on next `WindowEvent` dispatch. On Android, `std::process::exit(0)` is called explicitly after event loop exit.
- **Suspend/resume (Android):** `suspended()` drops `AppState` (wgpu resources) but preserves `cached_render` in each `PaneRenderState`. On resume, textures are restored from cache without re-rendering.
- **Window minimized:** `handle_redraw()` skips all rendering when the window is minimized (zero-size surface).
- **Redraw optimization:** Event loop uses `ControlFlow::Wait`. `notify_redraw()` helper (in `app.rs`) wraps the `if let Some(w) = window { w.request_redraw() }` pattern for background tasks holding `Option<WindowRef>`. Called only when background work is pending, overlays auto-poll is active, or user interacts. No spinning at high framerate when idle.
- **Polled channels:** Event loop uses `try_recv()` (non-blocking) on all channels. Background tasks call `notify_redraw()` after sending results.
- **Surface format preference:** `AppState` prefers `Bgra8Unorm` if available, else first available or `Rgba8UnormSrgb`. Surface dimensions are clamped to `max_texture_dimension_2d`.
- **Dual scale factors:** `pixels_per_point = window.scale_factor() * state.scale_factor`. The first is OS DPI scaling, the second is manual zoom (default 1.0).
- **Radar gate counts vary per radial:** Different radials in the same sweep can have different gate counts. Gate parameters are read per-radial from moment data.
- **Level III site code conversion:** 4-letter ICAO codes drop the first letter for Level III (`KTLX` → `TLX`). Non-CONUS sites (length ≠ 4) keep full code.
- **Hover value deduplication:** Only recomputed when cursor moves >0.5px from last position, preventing per-frame spam.
- **Android configuration changes:** The activity handles orientation/screen size changes in-app (via `config_changes` manifest attribute) rather than restarting, which would deadlock the native event loop on Z Fold devices.
- **Android back button:** Calls `moveTaskToBack(true)` to minimize (preserves recents thumbnail) rather than destroying the activity.
- **NWS zone caching:** Zone geometries are cached on disk with 1-year TTL. Cache key format: `"county_TXC113"`. Zone polygons are simplified on fetch.
- **Pre-computed triangulation:** `OverlayFeature` triangulates polygons at fetch time (in `rustdar-overlays`). Overlay cache reuses these indices unless vertex count changes during projection filtering.

## Adding a New Radar Product

1. Add variant to `RadarProduct` enum in `rustdar-radar/src/types.rs`
2. Implement `code()`, `name()`, `sort_order()`, `is_level3()`, `format_value()`, `get_moment()` arms, and add to `all()`
3. If Level III: implement `tgftp_dirs()` mapping
4. Update the sort key in `ScanInfo::from_scan()` (same file)
5. Add a `ColorScale` table in `palette.rs` and wire it in `get_color_for_value()`
6. The UI picks up new products automatically from `ScanInfo::available_products`

## Adding a New Overlay Type

1. Add fetch/parse module in `rustdar-overlays` (follow `spc/` or `nws/` patterns)
2. Define data types in `rustdar-overlays/src/types.rs` (use `OverlayFeature` for renderable polygons, pre-compute triangulations via `OverlayFeature::new()`)
3. Add `LayerKind` variant in `rustdar-egui/src/layers.rs`, set default state
4. Add `OverlayState<T>` field to `OverlayData` in `rustdar-egui/src/overlay_state.rs`
5. Add drawing function in `ui_map_overlays.rs` (use `OverlayLayerCache` for viewport caching)
6. Add `GuiAction` variant in `rustdar-egui/src/actions.rs`
7. Add channel pair to `ChannelHub` in `rustdar-platform/src/channels.rs`
8. Add fetch handler in `app_fetch.rs` and poll receiver in `app.rs`
