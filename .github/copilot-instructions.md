# Copilot Instructions — Rustdar

## Project Overview

Rustdar is a cross-platform NEXRAD weather radar viewer built in Rust. It fetches real-time radar data from public S3 buckets, renders it onto a map, and runs on desktop (Linux/macOS/Windows), Android, and in the browser as a PWA (wasm32 + WebGL2). The GUI uses **egui** with a **wgpu** rendering backend and **winit** for windowing.

Keep this document, `features.md`, and `data.md` updated when architecture or features change.

## Workspace Architecture

Cargo workspace (`resolver = "2"`, edition 2024) with ten crates:

| Crate | Role |
|---|---|
| `rustdar-frontend` | The portable half of the application: winit application handler, wgpu/egui renderer, fetch and render dispatch, app state. `app.rs` orchestrates lifecycle with `#[path]` submodules `app_fetch.rs` and `app_render.rs`. Defines the `PlatformBridge` trait that per-OS crates implement. |
| `rustdar-platform` | Binary + lib. Desktop, Android and iOS entry points: the event loop bootstrap (`run.rs`) and the concrete `PlatformBridge` implementations. No portable code — that lives in `rustdar-frontend`, which this crate depends on (never the other way round). |
| `rustdar-web` | Browser target (wasm32 + WebGL2): the entry point, the browser `PlatformBridge`, and the PWA shell (`index.html`, `sw.js`, `manifest.webmanifest`, icons). Deployed to GitHub Pages by `build.yaml` on every push to `main`. |
| `rustdar-android` | Android entry point (`cdylib`, `android_main`). Owns every JNI bridge — insets, compass, location, back handling, theme detection — and injects the callback-shaped ones into the `PlatformBridge` that `rustdar-platform` constructs. |
| `rustdar-egui` | Pure egui UI layer — no wgpu dependency. Defines `Gui` + `GuiAction` enum. Uses `walkers` crate for CartoDB map tiles. Split via `#[path]` submodules off `ui.rs` (`ui_popups.rs`, `ui_config.rs`, `ui_map_overlays.rs`, `ui_chrome.rs`, `ui_menu.rs`, `ui_map.rs`, `ui_settings.rs`; `ui_map_pane.rs` hangs off `ui_map.rs`) plus plain modules `ui_input.rs` and `ui_layout.rs`. |
| `rustdar-units` | Leaf crate for unit conversion and timezone formatting. `UserPreferences` persisted in `ui.json`. Conversions happen at display boundaries only — internal data stays in original units. |
| `rustdar-radar` | Radar data: Level II from the `unidata-nexrad-level2` S3 bucket, Level III from the `unidata-nexrad-level3` S3 bucket, storm-relative velocity derivation (`srm.rs`), RGBA rendering via Web Mercator, palettes, NEXRAD site list, `RadarProduct` enum. |
| `rustdar-overlays` | Weather overlay data + render-agnostic logic. SPC outlooks, Mesoscale Discussions, NWS alerts, HRRR model data, GLM lightning, METAR observations, storm reports. `OverlayHandler` trait + `OverlayRegistry` for type-erased overlay management. Rasterized to textures via tiny-skia. |
| `rustdar-gps` | GPS fix and config types; NMEA parser and serial-port reader behind the `serial` feature (off on wasm and iOS). |
| `nexrad-level3` | Level III product decoder (WMO headers, zlib/BZ2, radial packets). Byte slices in, model types out — no network, no filesystem. Product-specific LUT/threshold decoding lives in `rustdar-radar`. |

## Data Flow

`Gui::ui()` → `Vec<GuiAction>` → `App::process_gui_actions()` dispatches fetches on Tokio runtime → results via `std::sync::mpsc` channels (`ChannelHub`) → radar rendering on `std::thread::spawn` with rayon → textures stored in per-pane `OverlayTextureCache` → drawn via `painter.image()` each frame.

Overlay fetching uses `OverlayRegistry::create_fetch_tasks()` → handler-specific async tasks → `OverlayFetchResult` → `apply_fetch_result()`. Overlay rendering: `GuiAction::RenderOverlay` → background thread with tiny-skia → `OverlayRenderResponse` → texture upload → `draw_overlay_texture()`.

## Key Conventions

- **Lints:** `rustdar-frontend` and `rustdar-web` are `#![warn(clippy::all)]` + `#![forbid(unsafe_code)]`. `rustdar-platform` is `#![deny(unsafe_code)]`, not `forbid` — the iOS entry symbol carries a scoped `allow`, which a `forbid` could not be overridden by. `nexrad-level3` is `#![forbid(unsafe_code)]` + `#![deny(clippy::unwrap_used)]` + `#![deny(clippy::expect_used)]`. `rustdar-android` is the only crate that uses `unsafe` freely (JNI) — which is *why* Android capabilities reach the shared code as injected `fn` pointers rather than direct calls.
- **CI:** `cargo clippy --fix` auto-applied, then strict clippy re-run. Always pass `cargo clippy --all-targets --all-features`.
- **Generation counters** (`fetch_generation`, `render_generation`) guard against stale results. Increment before spawning; discard results with generation < current.
- **`#[path]` submodule pattern:** Large files split via `#[path = "ui_xxx.rs"] mod xxx;`. Extracted methods use `impl super::Gui {}` with `pub(super)` visibility.
- **Pinned crate versions:** every external dependency is pinned exactly (`=x.y.z`) in `[workspace.dependencies]` in the root `Cargo.toml` — that section is the source of truth for versions; don't restate them elsewhere, and don't upgrade without testing.
- **Config:** `ui.json` saved/loaded from `XDG_CONFIG_HOME/rustdar` or `~/.config/rustdar`. Uses `#[serde(default)]` for backward compatibility.
- **Web target is WebGL2, never WebGPU.** `rustdar_frontend::app` pins `Backends::GL` on wasm32, and the `webgpu` wgpu feature is deliberately absent on every target — Firefox has no stable WebGPU, so compiling it would only add an untested second rendering path. See the wgpu feature comments in `rustdar-frontend/Cargo.toml` before touching wgpu features anywhere.
- **Android:** `#[cfg(target_os = "android")]` gates in `rustdar-platform` and Cargo.toml deps. TLS is `rustls` + `rustls-platform-verifier` over the OS trust store; there is no OpenSSL and no bundled root store.

## Build & Run

```bash
cargo build --workspace                      # Desktop (needs libudev-dev on Ubuntu)
cargo run -p rustdar-platform
cargo clippy --all-targets --all-features    # Lint (matches CI)

cd rustdar-android/android && ./gradlew assembleRelease   # Android APK (needs SDK + NDK)
cd rustdar-android/android && ./gradlew bundleRelease     # Android .aab
```

Android is built by Gradle + cargo-ndk, not cargo-apk. The Gradle project lives in
`rustdar-android/android/`; it stages `librustdar_android.so` into `jniLibs` for
`arm64-v8a` and `x86_64`, and compiles `res/xml/network_security_config.xml` into
the resource table so the manifest can reference it.

## Architecture Patterns

- **UI ↔ Platform boundary:** `rustdar-egui` must not depend on wgpu/winit. Communicates via `GuiAction` (out) and setter methods (in). Entry point: `Gui::ui(&mut self, &egui::Context) -> Vec<GuiAction>`.
- **Async work:** Network I/O on Tokio. CPU-heavy rendering on `std::thread::spawn` + rayon, not Tokio. Background tasks send results via mpsc channels and call `notify_redraw()`.
- **Overlay rendering:** All overlays (including radar) are rasterized to RGBA textures and drawn as geo-positioned images. Per-frame cost is one `painter.image()` per overlay type. `OverlayHandler` trait encapsulates fetch, render, and interaction per overlay type.
  - **Exception — vertical cross-sections.** `rustdar-radar/src/xsect.rs` rasterizes to RGBA like everything else, but its axes are along-line distance × height MSL, not latitude × longitude, so it has no geographic bounds to place it by and is *not* a map overlay. It is also the one raster that is not square (`SECTION_WIDTH` × `SECTION_HEIGHT` = `IMAGE_SIZE` × `IMAGE_SIZE / 2`). The section pane is not wired up yet; when it is, it draws into its own pane rather than through `OverlayKind`.
- **Map tiles:** CartoDB no-labels base + labels-only overlay on top of radar/overlays, so text isn't obscured.
- **Geometry helpers:** Framework-agnostic algorithms in `rustdar-overlays/src/render/geo.rs`; `rustdar-egui` imports them (e.g. `overlay_cache.rs` uses `render::geo`) rather than keeping its own copies. Don't duplicate.
- **Radar rendering:** Level II and III share `render_with_projection()`. Produces dual output: RGBA image + `Vec<f32>` value data for hover tooltips. Gate parameters vary per radial.
- **Auto-polling:** Radar every 60s (only when `viewing_live`; historic results cached for instant live return). Overlays on their own intervals regardless of live/historic mode.

## Gotchas

- **Per-pane overlay config swapping:** `OverlayHandler` state is global (one instance per `OverlayKind`), but each pane has its own config snapshot (`PaneState::overlay_configs`). Code that reads handler config must call `overlays.load_pane_configs()` first to swap the correct pane's settings into the handler. **Every call site** that touches handler config — `controls()`, `apply_control()`, `prepare_rasterize()`, `create_fetch_tasks()`, `clickable_items()`, `hover_value_at()`, `per_frame_points()`, `has_data()`, `data_generation()` — must be preceded by a config load for the relevant pane. After mutation (e.g. `apply_control`), call `save_pane_configs()` / `save_enabled_map()` to persist changes back to the pane.
- **Handler `data_generation` must reflect config changes:** When a handler's config field affects rendering output (e.g. `selected_param`, `time_window_secs`, `satellite`), changing that field in `apply_control()` must bump `state.data_generation` (use `wrapping_add(1)`). Otherwise the per-pane `OverlayTextureCache` won't detect that a re-render is needed when configs differ between panes.
- **Global data + per-pane config handlers:** If a handler fetches data globally but renders differently based on config (e.g. model data `selected_param`, GLM `time_window_secs`), either cache results per-config-key (like `ModelDataHandler::cached_grids`) or ensure `prepare_rasterize` captures the config at closure-build time. A single global data slot is insufficient when two panes need different views of the same overlay type.
- **Deferred exit:** `GuiAction::Exit` sets a flag; actual exit on next `WindowEvent`. Android needs explicit `std::process::exit(0)`.
- **Surface loss:** Drop `AppState` but keep `cached_render` in `PaneRenderState`. Next redraw recreates fresh surface.
- **Lazy `AppState`:** Created on first `handle_redraw()`, not in `resumed()`, to prevent Android ANRs on fold/unfold.
- **Level III site codes:** 4-letter ICAO drops first letter (e.g., `KTLX` → `TLX`). Non-CONUS keeps full code.
- **Mercator lat clamping:** Bounds clamped to ±85.05° to avoid NaN from `tan(π/2)`.
- **Zoom quantization:** `(zoom * 32.0).round() as i32` for rerender triggers. Finer = excessive rerenders; coarser = missed changes.
- **`notify_redraw()`** wraps `request_redraw()` in `catch_unwind` to suppress `EventLoopClosed` panics from background threads.
- **Android config changes** handled in-app (not activity restart) to avoid native event loop deadlocks on foldables.

## Adding a New Radar Product

1. Add variant to `RadarProduct` in `rustdar-radar/src/types.rs`
2. Implement `code()`, `name()`, `sort_order()`, `is_level3()`, `format_value()`, `get_moment()` arms; add to `all()`
3. If Level III: add a `level3_products()` arm — the AWIPS product IDs that key the `unidata-nexrad-level3` bucket
4. Add `ColorScale` in `palette.rs`; wire in `get_color_for_value()`
5. UI auto-discovers new products via `ScanInfo::available_products`

## Adding a New Overlay Type

1. Add fetch/parse module in `rustdar-overlays` (follow `spc/` or `nws/` patterns)
2. Define data types in `types.rs` (use `OverlayFeature` for polygons with pre-computed geo bounds)
3. Add `LayerKind` variant in `layers.rs`
4. Add `OverlayKind` variant in `overlay_state.rs`; add to `all()`, `default_draw_order()`, and `texture_overlays()` if applicable
5. Add rasterize function in `rasterize.rs` (follow `rasterize_nws_alerts()` pattern)
6. Create handler in `handlers/` implementing `OverlayHandler` trait (follow existing handlers)
7. Register in `handlers/mod.rs` `create_handlers()`
8. **Per-pane correctness:** If the handler has config beyond `enabled` (e.g. selected parameter, day, time window), ensure:
   - `apply_control()` bumps `state.data_generation` when any render-affecting config changes
   - `serialize_state()` / `deserialize_state()` round-trip all config fields
   - `prepare_rasterize()` captures config into the closure (don't rely on `&self` at render time)
   - If different configs need different fetched data, cache per config key (see `ModelDataHandler::cached_grids`)
