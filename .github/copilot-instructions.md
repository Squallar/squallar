# Copilot Instructions — Rustdar

## Project Overview

Rustdar is a cross-platform NEXRAD weather radar viewer built in Rust. It fetches real-time radar data from AWS, renders it onto a map, and runs on both desktop (Linux/macOS/Windows) and Android. The GUI uses **egui** with a **wgpu** rendering backend and **winit** for windowing.

## Workspace Architecture

Seven crates in a Cargo workspace (`resolver = "2"`, edition 2024):

| Crate | Role |
|---|---|
| `rustdar-platform` | Main binary + lib. Owns the winit event loop, wgpu surface, egui renderer, and Tokio runtime. Entry points: `src/main.rs` (desktop), consumed by `rustdar-android` on Android. Fetch dispatch lives in `app_fetch.rs` (a `#[path]` submodule of `app.rs`). |
| `rustdar-egui` | Pure egui UI layer. Defines `Gui` (map + controls) and `GuiAction` enum. Uses the `walkers` crate for map tiles (CartoDB). No wgpu dependency. Large `Gui` impl is split across `#[path]` submodules: `ui_popups.rs`, `ui_config.rs`, `ui_mobile.rs`, `ui_map_overlays.rs`. Shared geometry helpers live in `geo.rs`, tile source in `tiles.rs`, per-pane state in `pane.rs`. |
| `rustdar-radar` | Radar data logic: fetching scans from AWS (`scan.rs`), rendering to 1800×1800 RGBA images (`render.rs`), color palettes (`palette.rs`), the static site database (`sites.rs`, 207 NEXRAD sites), and shared data types (`types.rs`: `RadarProduct`, `ScanInfo`, `ImageBounds`, constants). Uses `rayon` for parallel rendering. |
| `rustdar-overlays` | Weather overlay data: SPC convective outlooks, Mesoscale Discussions, and NWS alerts. Fetching, parsing, and coloring polygons. |
| `nexrad-level3` | NEXRAD Level III product decoder (radial/raster symbology). |
| `rustdar-android` | Thin Android entry point (`android_main`). Configures `cargo-apk`, initializes logging, then delegates to `rustdar-platform`. |
| `rustdar-android-theme` | JNI-based Android dark/light theme detection. Desktop stub returns `false`. |

**Data flow:** `Gui` emits `GuiAction` variants → `App::handle_gui_action()` (in `app_fetch.rs`) dispatches fetches on a shared `tokio::runtime::Runtime` → results arrive via `std::sync::mpsc` channels → radar rendering runs on `std::thread::spawn` (not Tokio) → image textures are loaded into egui.

## Key Conventions

- **`#![warn(clippy::all)]` and `#![forbid(unsafe_code)]`** on platform crates. Only `rustdar-android-theme` uses `unsafe` (JNI). Keep it that way.
- **CI auto-applies clippy fixes** on PRs via `cargo clippy --fix`, then re-runs strict clippy. Always pass `cargo clippy --all-targets --all-features`.
- **Generation counters** (`fetch_generation`, `render_generation`) guard against stale async/threaded results. Increment the counter before spawning work; discard results with generation < current.
- **Platform-conditional compilation:** `#[cfg(target_os = "android")]` gates appear in `rustdar-platform` (theme detection, Android theme polling thread, mobile vs desktop UI) and dependency sections of Cargo.toml files. Android requires `openssl-sys` with `vendored` feature.
- **No web target.** wgpu limits use native adapter limits, not WebGL2.
- **`#[path]` submodule pattern:** Large files like `ui.rs` and `app.rs` are split using `#[path = "ui_xxx.rs"] mod xxx;` to avoid converting to directory modules. Extracted methods use `impl super::Gui {}` blocks with `pub(super)` visibility. Free functions use `pub(super) fn`.

## Build & Run

```bash
# Desktop (requires libasound2-dev on Ubuntu)
cargo build --workspace
cargo run -p rustdar-platform

# Android (requires cargo-apk, ANDROID_HOME, NDK)
./build-android.sh            # builds APK via cargo-apk
adb install <path-to-apk>
adb logcat -s rustdar         # view logs

# Lint (matches CI)
cargo clippy --all-targets --all-features
```

## Architecture Patterns to Follow

- **UI ↔ Platform boundary:** `rustdar-egui` must not depend on wgpu/winit. It communicates intent through `GuiAction` and receives state via setter methods on `Gui` (e.g., `set_scan_info()`, `set_radar_image()`).
- **Async work:** Network I/O goes through the shared `App::tokio_runtime`. CPU-heavy radar rendering uses `std::thread::spawn` + rayon, not Tokio tasks.
- **Radar rendering:** `render_radar_to_image()` uses atomic buffers (`AtomicU32`) written in parallel by rayon, then converted to `Vec<u8>` RGBA. Gate parameters are inferred via heuristics in `infer_gate_params()` because `nexrad-model` doesn't expose them.
- **Radar image data:** `RadarImageData` struct (in `pane.rs`) bundles texture handle, lat/lon, max range, and value data. Prefer this over raw tuples.
- **Scan loading:** `ScanInfo::from_scan()` (in `rustdar-radar/src/types.rs`) inspects a `nexrad_model::data::Scan` to discover available products and elevation angles. This keeps radar domain logic in the `rustdar-radar` crate.
- **Color palettes** in `rustdar-radar/src/palette.rs` map radar product values → RGBA. Each `RadarProduct` variant has a dedicated color function.
- **Geometry helpers** in `rustdar-egui/src/geo.rs` provide shared screen-space utilities (`point_in_polygon`, `aabb`, `dashed_line_shapes`, `clip_line_to_polygon`). Don't duplicate these in consumer modules.
- **Theme detection:** Desktop uses `dark-light` crate; Android uses JNI polling in a background thread. Both feed `cached_dark_theme` in `App`. Only update egui visuals when theme actually changes (`applied_visuals_dark` guard).

## Adding a New Radar Product

1. Add variant to `RadarProduct` enum in `rustdar-radar/src/types.rs`
2. Implement `code()`, `name()`, `get_moment()` arms, and add to `all()`
3. Update the sort key in `ScanInfo::from_scan()` (same file)
4. Add color function in `palette.rs` and wire it in `get_color_for_value()`
5. The UI picks up new products automatically from `ScanInfo::available_products`
