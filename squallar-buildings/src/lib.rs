//! Building footprints out of vector-tile bytes, extruded into prisms.
//!
//! **Its own crate because all of it runs inside the offload worker**, which
//! links neither egui, wgpu nor winit. Bytes in, vertices out; nothing here
//! reaches a GPU, a window or the network. `tests/charter.rs` holds that as a
//! ceiling on the declared dependencies *and* as a walk of the resolved graph,
//! so it is a gate rather than a sentence.
//!
//! Four pieces:
//!
//! * [`tile`] — a tile address and the one projection from a feature's
//!   extent-unit coordinates into the volume box's kilometres.
//! * [`footprint`] — the `building` source layer's features, read straight out
//!   of MVT bytes with `mvt-reader`, and what their properties mean.
//! * [`prism`] — lyon over the footprint, walls between
//!   `render_min_height` and `render_height`, and the mesh that comes out.
//! * [`budget`] — the vertex and index ceilings, fitted down a rung ladder
//!   against runtime figures, and the shed that keeps the tallest buildings
//!   when the whole set does not fit.
//! * [`jobs`] — the one codec row that runs all of the above off the frame
//!   thread, composed last by `squallar_worker::job_registry`.
//!
//! # Why prisms and not a contribution to the height field
//!
//! The height field would have cost nothing new: rasterise `render_height`
//! into the same posts as a maximum and inherit occlusion, drape and lighting
//! unchanged. **It cannot work, and the reason is arithmetic rather than
//! taste.** A height field represents a surface that is single-valued at the
//! field's resolution. Buildings are separated by streets 10-20 m wide, and at
//! `squallar_elevation::plan`'s finest realistic posts over a dollied ~24 km
//! patch the spacing is ~47 m. A 10-20 m street needs 5-10 m posts by Nyquist
//! and a 20-40 m footprint is not resolvable at 47 m either, so a downtown
//! block collapses into one lump and the streets between the buildings fill
//! in. Distinct masses with gaps between them are the whole of what makes a
//! city read as a city, and that is exactly what the field destroys.
//!
//! # Why a CPU mesh is allowed here and not for the ground
//!
//! The ground is a GPU-displaced procedural grid with no vertex or index
//! buffers at all, and that is a rule this crate deliberately breaks. The rule
//! exists because a height-field mesh is a couple of hundred thousand vertices
//! *per box change* and regular enough to be derived from
//! `@builtin(vertex_index)` alone. Buildings are irregular polygons whose
//! count is bounded by what is inside the visible footprint rather than by the
//! box, so there is no topology to derive. What the rule really protects is
//! the frame thread, and [`jobs`] is how that protection is kept: the parse,
//! the projection, the tessellation and the shed all run on the worker, and
//! the frame thread's whole cost is the buffer write.
//!
//! # `sl:building` does not govern this, and the reason is a test
//!
//! **Decided here, built in the unit that draws.** The `building` source layer
//! already has a control — `sl:building` in
//! `squallar_egui::basemap_layer::SOURCE_LAYER_TOGGLES`, shipped **off**,
//! with `the_source_layer_toggle_roster_matches_what_the_styles_reference`
//! pinning that switching everything on does not silently re-enable it. The
//! question this track had to answer is whether 3D buildings ride that switch
//! or carry one of their own. **They carry their own.**
//!
//! The decisive argument is a property of the tree rather than a preference.
//! That roster test pins the toggle table in **both** directions: every
//! `sl:`-prefixed control must name a source layer that some committed style
//! layer references, and the control id must be recoverable to the layer name.
//! Prisms are not a style layer — nothing in `www/styles/dark.json` or
//! `light.json` draws them, and they never pass through a `Style` at all — so
//! a `sl:`-prefixed control for them would either fail that direction or need
//! a carve-out exempting it, and a carve-out that says "this toggle governs
//! something the styles do not draw" is a worse edit than a control that is
//! honestly not a source-layer toggle.
//!
//! Two supporting reasons, and one cost that is being accepted rather than
//! argued away:
//!
//! * **The costs are not comparable.** `sl:building` adds some flat fills to
//!   tiles the map is already tessellating. Turning prisms on adds an MVT
//!   re-parse, a tessellation pass, a fetch of building tiles at a zoom the 2D
//!   map may not be at, and up to [`budget::DEFAULT_PRISM_VRAM_BYTES`] of
//!   geometry. Binding the expensive one to the cheap one makes it arrive
//!   unannounced.
//! * **They are not in the same pane.** `sl:building` governs what a 2D map
//!   pane draws; prisms exist only in a 3D pane, and only read as buildings at
//!   a dollied camera. A user with no 3D pane open would be paying for a
//!   switch that shows them nothing.
//! * **The cost accepted**: there are now two controls a user could call
//!   "buildings". The mitigation is placement — the 3D control belongs with
//!   the 3D pane's own controls and not in the basemap source-layer list — and
//!   it is a real discoverability cost, not a solved one.
//!
//! Nothing here reads either control: this crate is handed a tile set and a
//! budget. The unit that draws is where the control lands, and it persists
//! like every other piece of UI state.

pub mod budget;
pub mod footprint;
pub mod jobs;
pub mod prism;
pub mod tile;

pub use budget::{
    DEFAULT_PRISM_VRAM_BYTES, FINEST_VERTEX_CEILING, INDICES_PER_VERTEX_CEILING,
    MEASURED_VERTICES_PER_BUILDING, MIN_VERTEX_CEILING, PRISM_INDEX_BYTES, PRISM_VERTEX_BYTES,
    PrismBudget, PrismCeilings, PrismLimit, PrismRung,
};
pub use footprint::{
    BuildingFootprint, BuildingsError, HIDE_3D, RENDER_HEIGHT, RENDER_MIN_HEIGHT, SOURCE_LAYER,
    read_footprints,
};
pub use jobs::{BuildingMeshJob, BuildingTile};
pub use prism::{BuildingMesh, extrude};
pub use tile::{BoxFrame, MAX_TILE_ZOOM, TileId};
