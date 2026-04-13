# Copilot Instructions — Rustdar

## Project Overview

Rustdar is a cross-platform NEXRAD weather radar viewer built in Rust. It fetches real-time radar data from AWS, renders it onto a map, and runs on both desktop (Linux/macOS/Windows) and Android. The GUI uses **egui** with a **wgpu** rendering backend and **winit** for windowing.

Keep this document, `features.md`, and `data.md` updated when architecture or features change.

## Workspace Architecture

Cargo workspace (`resolver = "2"`, edition 2024) with seven crates:

| Crate | Role |
|---|---|
| `rustdar-platform` | Binary + lib. Owns winit event loop, wgpu surface, egui renderer, Tokio runtime. `app.rs` orchestrates lifecycle with `#[path]` submodules `app_fetch.rs` and `app_render.rs`. `PlatformBridge` trait abstracts desktop/Android differences. |
| `rustdar-egui` | Pure egui UI layer — no wgpu dependency. Defines `Gui` + `GuiAction` enum. Uses `walkers` crate for CartoDB map tiles. Split via `#[path]` submodules (`ui_popups.rs`, `ui_config.rs`, `ui_mobile.rs`, `ui_map_overlays.rs`, `ui_desktop.rs`, `ui_map.rs`, `ui_settings.rs`). |
| `rustdar-units` | Leaf crate for unit conversion and timezone formatting. `UserPreferences` persisted in `ui.json`. Conversions happen at display boundaries only — internal data stays in original units. |
| `rustdar-radar` | Radar data: AWS Level II fetching, TGFTP Level III fetching, 2048×2048 RGBA rendering via Web Mercator, `ColorScale` palettes, 207 NEXRAD sites, `RadarProduct` enum (13 variants). |
| `rustdar-overlays` | Weather overlay data + render-agnostic logic. SPC outlooks, Mesoscale Discussions, NWS alerts, HRRR model data, METAR observations, storm reports. `OverlayHandler` trait + `OverlayRegistry` for type-erased overlay management. Rasterized to textures via tiny-skia. |
| `nexrad-level3` | Level III product decoder (WMO headers, zlib/BZ2, radial packets). Product-specific LUT/threshold decoding lives in `rustdar-radar`. |
| `rustdar-android` / `rustdar-android-theme` | Android entry point + JNI theme detection. Desktop stubs provided. |

## Data Flow

`Gui::ui()` → `Vec<GuiAction>` → `App::process_gui_actions()` dispatches fetches on Tokio runtime → results via `std::sync::mpsc` channels (`ChannelHub`) → radar rendering on `std::thread::spawn` with rayon → textures stored in per-pane `OverlayTextureCache` → drawn via `painter.image()` each frame.

Overlay fetching uses `OverlayRegistry::create_fetch_tasks()` → handler-specific async tasks → `OverlayFetchResult` → `apply_fetch_result()`. Overlay rendering: `GuiAction::RenderOverlay` → background thread with tiny-skia → `OverlayRenderResponse` → texture upload → `draw_overlay_texture()`.

## Key Conventions

- **`#![warn(clippy::all)]` and `#![forbid(unsafe_code)]`** on `rustdar-platform` and `nexrad-level3`. Only `rustdar-android-theme` uses `unsafe` (JNI).
- **CI:** `cargo clippy --fix` auto-applied, then strict clippy re-run. Always pass `cargo clippy --all-targets --all-features`.
- **Generation counters** (`fetch_generation`, `render_generation`) guard against stale results. Increment before spawning; discard results with generation < current.
- **`#[path]` submodule pattern:** Large files split via `#[path = "ui_xxx.rs"] mod xxx;`. Extracted methods use `impl super::Gui {}` with `pub(super)` visibility.
- **Pinned crate versions:** `nexrad-data =1.0.0-rc.5`, `nexrad-model =1.0.0-rc.2`, `nexrad-decode =1.0.0-rc.3`. Don't upgrade without testing.
- **Config:** `ui.json` saved/loaded from `XDG_CONFIG_HOME/rustdar` or `~/.config/rustdar`. Uses `#[serde(default)]` for backward compatibility.
- **No web target.** Native adapter limits only.
- **Android:** `#[cfg(target_os = "android")]` gates in `rustdar-platform` and Cargo.toml deps. Requires `openssl-sys` with `vendored` feature.

## Build & Run

```bash
cargo build --workspace                      # Desktop (needs libasound2-dev on Ubuntu)
cargo run -p rustdar-platform
cargo clippy --all-targets --all-features    # Lint (matches CI)

./build-android.sh                           # Android APK (needs cargo-apk, NDK, d8)
```

## Architecture Patterns

- **UI ↔ Platform boundary:** `rustdar-egui` must not depend on wgpu/winit. Communicates via `GuiAction` (out) and setter methods (in). Entry point: `Gui::ui(&mut self, &egui::Context) -> Vec<GuiAction>`.
- **Async work:** Network I/O on Tokio. CPU-heavy rendering on `std::thread::spawn` + rayon, not Tokio. Background tasks send results via mpsc channels and call `notify_redraw()`.
- **Overlay rendering:** All overlays (including radar) are rasterized to RGBA textures and drawn as geo-positioned images. Per-frame cost is one `painter.image()` per overlay type. `OverlayHandler` trait encapsulates fetch, render, and interaction per overlay type.
- **Map tiles:** CartoDB no-labels base + labels-only overlay on top of radar/overlays, so text isn't obscured.
- **Geometry helpers:** Framework-agnostic algorithms in `rustdar-overlays/src/render/geo.rs`, egui-specific bridges in `rustdar-egui/src/geo.rs`. Don't duplicate.
- **Radar rendering:** Level II and III share `render_with_projection()`. Produces dual output: RGBA image + `Vec<f32>` value data for hover tooltips. Gate parameters vary per radial.
- **Auto-polling:** Radar every 60s (only when `viewing_live`; historic results cached for instant live return). Overlays on their own intervals regardless of live/historic mode.

## Gotchas

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
3. If Level III: add `tgftp_dirs()` mapping
4. Add `ColorScale` in `palette.rs`; wire in `get_color_for_value()`
5. UI auto-discovers new products via `ScanInfo::available_products`

## Adding a New Overlay Type

1. Add fetch/parse module in `rustdar-overlays` (follow `spc/` or `nws/` patterns)
2. Define data types in `types.rs` (use `OverlayFeature` for polygons with pre-computed triangulations)
3. Add `LayerKind` variant in `layers.rs`
4. Add `OverlayKind` variant in `overlay_state.rs`; add to `all()`, `default_draw_order()`, and `texture_overlays()` if applicable
5. Add rasterize function in `rasterize.rs` (follow `rasterize_nws_alerts()` pattern)
6. Create handler in `handlers/` implementing `OverlayHandler` trait (follow existing handlers)
7. Register in `handlers/mod.rs` `create_handlers()`
