pub mod actions;
pub mod config_store;
pub mod overlay_cache;
pub mod pane;
pub(crate) mod point_painter;
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

/// The chrome's glyph inventory and the coverage tests over egui's bundled
/// fonts — test-only, because the inventory exists to be asserted against.
#[cfg(test)]
mod ui_glyphs;

pub const DEFAULT_NOTIFIER_ENDPOINT: &str = "wss://nexrad-aws-notifier.mcswain.dev";

pub use ui::{ChunkFeedStatus, CurrentVolumeStamp, Gui, StormMotionOverride, TiltFreshness};
pub use ui_input::{normalize_touch_devices, normalize_wheel_units};
