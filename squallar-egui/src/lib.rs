pub mod actions;
/// The PMTiles v3 basemap archive reader.
///
/// Behind `basemap-vector` and, as of this commit, called by nothing but its
/// own tests — the draw seam is a later step. The feature gate is where the
/// dependency lives, so the module is where the code that names it lives.
#[cfg(feature = "basemap-vector")]
pub mod basemap_archive;
pub(crate) mod field_facts;
pub(crate) mod legend_ramp;
pub mod overlay_cache;
pub mod pane;
pub(crate) mod point_painter;
/// The radar layer's own glue: what the presentation holds for radar that no
/// other layer has.
pub mod radar_layer;
pub mod shell_api;
/// The app's layer set, composed from the source crates that own the data.
pub mod sources;
pub mod tile_source;
pub mod tiles;
mod ui;
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
pub use ui::{Gui, StormMotionOverride};
pub use ui_input::{normalize_touch_devices, normalize_wheel_units};
