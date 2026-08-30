//! Terrain-RGB decode and the resample onto a volume box's post grid.
//!
//! **Its own crate because both must run inside the offload worker**, which
//! links neither egui, wgpu nor winit. Everything here is bytes in, numbers
//! out; nothing reaches a GPU, a window or the network. `tests/charter.rs`
//! holds that as a ceiling on the declared dependencies *and* on the resolved
//! graph, so it is a gate rather than a sentence.
//!
//! Three pieces:
//!
//! * [`trgb`] — the Mapbox Terrain-RGB constants and `unpack`, re-spelled from
//!   `tools/squallar-terrain` (a separate workspace, so it cannot be `use`d)
//!   and pinned to it three ways.
//! * [`height`] — [`HeightField`] and its `u16` encoding, 2 bytes per post.
//! * [`resample`] — tiles to one contiguous pixel plane, then the forward
//!   projection per post.
//!
//! The box floor's 1°×1° minimum-elevation grid is **not** here even though the
//! same builder pass emits it: it lives in `squallar_geo::min_elevation`,
//! because `squallar-radar` will read it and this crate is **planned** to stand
//! above `squallar-radar` through `squallar-device-profile`. Today this crate
//! declares neither — `tests/charter.rs` asserts the smaller set — so the cycle
//! is prospective rather than present, and the grid is placed for where the
//! graph is going. That module's docs carry the argument in full.

pub mod height;
pub mod resample;
pub mod trgb;

pub use height::{
    HEIGHT_BASE_M, HEIGHT_CEILING_M, HEIGHT_QUANTUM_M, HeightField, decode_height_m,
    encode_height_m,
};
pub use resample::{ElevationError, TileCover, TilePlane, cover_for, post_center_km, post_geo};
