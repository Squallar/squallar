//! The pack format — which lives in the app, not here.
//!
//! This file used to carry a second copy of the encoder, the decoder and the
//! index. It no longer does: `rustdar_overlays::nws::zone_pack` is the format,
//! and the app reads what this tool writes through the very code that wrote it.
//! A tool with its own encoder is a tool whose output can drift from what the
//! app can read, and the drift would show up as a map with missing zones rather
//! than as a compile error.
//!
//! Nothing but the re-export belongs here. Anything the converter needs and the
//! app does not — a diagnostic, a size table — goes in `main.rs`.

pub use rustdar_overlays::nws::zone_pack::{
    Coding, HEADER_LEN, INDEX_ENTRY_LEN, Kind, PackedZone, ZonePack, key, write,
};
