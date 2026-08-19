pub mod actions;
pub mod overlay_cache;
pub mod pane;
pub(crate) mod point_painter;
pub mod shell_api;
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

/// Where the UI layout's persistence key lives (`ui_config.rs`, mounted as
/// `ui::config`); surfaced here the way `Gui` is, so consumers spell it
/// `rustdar_egui::UI_CONFIG_KEY`.
pub use ui::config::UI_CONFIG_KEY;
pub use ui::{CurrentVolumeStamp, Gui, StormMotionOverride};
pub use ui_input::{normalize_touch_devices, normalize_wheel_units};
