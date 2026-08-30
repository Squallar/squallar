//! Terrain-RGB decode and the resample onto a volume box's post grid.
//!
//! **Its own crate because both must run inside the offload worker**, which
//! links neither egui, wgpu nor winit. Everything here is bytes in, numbers
//! out; nothing reaches a GPU, a window or the network. `tests/charter.rs`
//! holds that as a ceiling on the declared dependencies *and* on the resolved
//! graph, so it is a gate rather than a sentence.
//!
//! Four pieces:
//!
//! * [`trgb`] — the Mapbox Terrain-RGB constants and `unpack`, re-spelled from
//!   `tools/squallar-terrain` (a separate workspace, so it cannot be `use`d)
//!   and pinned to it three ways.
//! * [`height`] — [`HeightField`] and its `u16` encoding, 2 bytes per post.
//! * [`resample`] — tiles to one contiguous pixel plane, then the forward
//!   projection per post.
//! * [`jobs`] — the one codec row that runs the two inside the offload worker,
//!   composed last by `squallar_worker::job_registry`.
//! * [`plan`] — which posts over which footprint, fitted down a rung ladder
//!   until the texture bytes, the tile count and the adapter's own
//!   `max_texture_dimension_2d` all fit. The camera's LOD, and the half of the
//!   height path that must not run on the frame thread.
//!
//! The box floor's 1°×1° minimum-elevation grid is **not** here even though the
//! same builder pass emits it: it lives in `squallar_geo::min_elevation`,
//! because `squallar-radar` will read it and this crate is **planned** to stand
//! above `squallar-radar` through `squallar-device-profile`. This crate does
//! not declare `squallar-device-profile` — `tests/charter.rs` asserts the exact
//! set, which is `image`, `squallar-geo` and `squallar-source` — so the cycle is
//! prospective rather than present, and the grid is placed for where the graph
//! is going. That module's docs carry the argument in full.

pub mod height;
pub mod jobs;
pub mod plan;
pub mod resample;
pub mod trgb;

pub use height::{
    HEIGHT_BASE_M, HEIGHT_CEILING_M, HEIGHT_QUANTUM_M, HeightField, decode_height_m,
    encode_height_m,
};
pub use jobs::{HeightTile, TerrainHeightJob};
pub use plan::{
    CameraFootprint, FitRequest, Footprint, HeightCeilings, HeightPlan, HeightPlanner, PlanLimit,
    PostRung,
};
pub use resample::{ElevationError, TileCover, TilePlane, cover_for, post_center_km, post_geo};
