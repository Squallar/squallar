# Squallar

[![Build](https://github.com/Squallar/squallar/actions/workflows/build.yaml/badge.svg)](https://github.com/Squallar/squallar/actions/workflows/build.yaml) [![License](https://badgen.net/github/license/Squallar/squallar)](https://github.com/Squallar/squallar/blob/main/LICENSE) [![Latest release](https://img.shields.io/github/release/Squallar/squallar.svg)](https://github.com/Squallar/squallar/releases/) [![Coverage](.github/badges/coverage.svg)](https://github.com/Squallar/squallar/actions/workflows/test.yaml)

## System requirements

**x86-64 desktop builds require SSE4.1** — Intel Penryn (January 2008) and every
Nehalem and later, or AMD Bulldozer (October 2011) and later. This covers the
Linux, Windows and Intel-Mac binaries.

The exclusion worth naming, because it is the one that surprises people: **AMD
K10 — Phenom, Phenom II, Athlon II — has SSE4a, which is a different and
disjoint instruction set, not SSE4.1.** Those chips will not run these binaries;
they fault on the first `roundss`. Nothing else in the x86-64 line since 2011 is
affected.

The floor costs nothing real. Squallar renders through wgpu on Vulkan, Metal,
D3D12 or WebGPU, and a machine too old for SSE4.1 has no GPU that can drive that
either — the CPU requirement sits far below the requirement the app already had.
What it buys is that `f32::round()`, `floor()`, `ceil()` and `trunc()` — which a
UI frame calls hundreds of times laying out shapes and text — become one
instruction each instead of a call into a software implementation. Measured at
**7.4 % off the instruction count of a UI frame**. The flag and the full
measurement live in [`.cargo/config.toml`](.cargo/config.toml).

Apple Silicon Macs, aarch64 and the browser build are unaffected: they were never
paying that cost, and they carry no such flag.

Other floors, for completeness: **macOS 11**, **iOS 14** (both pinned as
deployment targets in `.cargo/config.toml`), **Android 9 / API 28**
(`packaging/android/app/build.gradle.kts`), and any browser with WebGPU or
WebGL2 plus cross-origin isolation for the web build.
