pub mod actions;
/// The PMTiles v3 basemap archive reader. Read by
/// [`tile_source::HttpsTiles::from_archive_url`], which is THE base map
/// source on every target since the raster (CartoDB) path was deleted.
pub mod basemap_archive;
/// What the Downloaded areas screen needs off the frame thread: the detail
/// vocabulary, the generation fact, and the worker that asks the store whether
/// an area is still all there.
pub(crate) mod basemap_areas;
/// The BasemapTiles layer: the handler that makes the base map a Layers-panel
/// citizen, and the per-source-layer toggle table.
/// `tests/committed_styles_parse.rs` — an integration binary, which never sees
/// `cfg(test)` items — pins the toggle table against the committed styles.
/// The offline-area download engine: enumerates a bbox's tiles, fetches their
/// byte ranges, and writes ~16 MB standalone `.pmtiles` segments through a
/// per-target store. Headless — the selection UI and the read-back
/// composition are later steps.
pub mod basemap_download;
pub mod basemap_layer;
/// The archive tile path's always-on counters: vector, raster and sniffed
/// bodies decoded. A basemap that decodes nothing reads zero here and passes
/// every other gate, which is exactly how one shipped.
pub mod basemap_ledger;
/// The two committed basemap styles, compiled in. The `include_str!` pair is
/// 241 KB of JSON, carried by every build because every build renders the
/// vector basemap from it.
pub mod basemap_style;
pub(crate) mod field_facts;
/// The 3D floor path's always-on counters: strips painted, mirrors rendered.
pub mod floor_ledger;
/// The scripted-input player: deterministic gesture scripts injected into the
/// renderer's raw input, and the click registry the UiSweep script drives real
/// widgets through. Dormant unless armed by the `gesture_script` config key or
/// the `SQUALLAR_GESTURE_SCRIPT` environment variable.
pub mod gesture_player;
/// The one copy of the integration-faithful event sequences, emitted by both
/// the headless input harness and the gesture player.
pub(crate) mod input_fidelity;
pub(crate) mod legend_ramp;
pub mod overlay_cache;
pub mod pane;
/// The exact-size PMTiles v3 directory reader: what a set of tiles costs to
/// download, to the byte. Beside [`basemap_archive`], never in its render
/// path — the stock crate keeps serving tiles; this answers the one question
/// its API declines to.
pub mod pmt_index;
pub(crate) mod point_painter;
/// The radar layer's own glue: what the presentation holds for radar that no
/// other layer has.
pub mod radar_layer;
pub mod shell_api;
/// A radar site's marker: sized in points, on glass, every frame.
pub(crate) mod site_marker;
/// The app's layer set, composed from the source crates that own the data.
pub mod sources;
pub(crate) mod terrain;
/// A vector tile's tessellated fills, flattened once and drawn from the GPU.
pub mod tile_mesh;
pub mod tile_source;
pub mod tiles;
mod ui;
/// The download arm of the region drag: the box it picks, the exact live size
/// figure measured off the frame thread, and the quota arithmetic the
/// selection UI spends them on. The detail vocabulary itself lives in
/// [`basemap_areas`], which the manage screen reads it back through.
pub(crate) mod ui_download_area;
pub(crate) mod ui_input;
pub(crate) mod ui_layout;
pub(crate) mod ui_region;
pub(crate) mod ui_section_edit;
pub mod volume_alpha;
pub mod volume_iso;
pub mod volume_view;

#[cfg(test)]
mod input_harness;

#[cfg(test)]
mod parity_walk;

/// Layer-stack curation through the chrome: the trash can, the catalog's add,
/// and the removal that survives a reopen.
#[cfg(test)]
mod layer_curation_tests;

/// The chrome's glyph inventory and the coverage tests over egui's bundled fonts.
#[cfg(test)]
mod ui_glyphs;

pub use radar_layer::CurrentVolumeStamp;
pub use ui::config::{UI_CONFIG_BACKUP_KEY, UI_CONFIG_KEY, back_up_pre_slot_config};
pub use ui::map::pane_render::overlay_cache_token;
pub use ui::{Gui, MirrorSources, StormMotionOverride};
pub use ui_input::{normalize_touch_devices, normalize_wheel_units};
