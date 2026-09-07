//! Rasterise the app icon from `packaging/icon/squallar.svg` into `OUT_DIR`.
//!
//! The SVG is the only drawing of the icon in the repository. Every raster of
//! it — the web set, the iOS asset catalog, the macOS `.icns`, the Android
//! mipmaps, the Linux hicolor set — is generated during packaging by the
//! `squallar-icon` binary and none of them is committed. This build script and
//! that binary share one implementation, in the `squallar-icon` library, so a
//! change to how the icon rasterises cannot reach one and miss the other.
//!
//! The window icon cannot wait for packaging, because winit wants the pixels at
//! runtime and the usual spelling for that is `include_bytes!` of a file that
//! has to exist when the crate compiles. So this renders it here, with `resvg`,
//! which is pure Rust: no `rsvg-convert`, no ImageMagick, and a plain
//! `cargo run` on a machine with neither still gets an icon.
//!
//! What lands in `OUT_DIR`:
//!
//!   `window_icon.rgba`  256x256 premultiplied-free RGBA8, the layout
//!                       `winit::window::Icon::from_rgba` takes. Raw rather
//!                       than PNG so the runtime needs no decoder.
//!   `squallar.ico`      the Windows executable resource, written straight
//!                       into the file format because an ICO is a header and
//!                       a directory in front of PNG payloads.
//!
//! Re-run only when the SVG changes; `cargo:rerun-if-changed` below is what
//! keeps this off every incremental build.

use std::path::{Path, PathBuf};

use squallar_icon::{ICO_SIZES, Icon, ico};

/// The window icon's edge, in pixels. 256 is the largest any of the three
/// desktop window managers asks for, and they downscale from it themselves.
const WINDOW_ICON_PX: u32 = 256;

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let svg = manifest.join("../packaging/icon/squallar.svg");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());

    println!("cargo:rerun-if-changed={}", svg.display());
    println!("cargo:rerun-if-changed=build.rs");

    let data = std::fs::read(&svg).unwrap_or_else(|e| panic!("reading {}: {e}", svg.display()));
    let icon = Icon::parse(&data).unwrap_or_else(|e| panic!("{e}"));

    write(&out.join("window_icon.rgba"), &icon.rgba(WINDOW_ICON_PX));

    let images: Vec<(u32, Vec<u8>)> = ICO_SIZES.iter().map(|&px| (px, icon.png(px))).collect();
    write(&out.join("squallar.ico"), &ico(&images));
}

fn write(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).unwrap_or_else(|e| panic!("writing {}: {e}", path.display()));
}
