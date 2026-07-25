pub mod archive;
pub mod level3;
mod palette;
pub mod render;
pub mod scan;
pub mod sites;
pub mod sources;
pub mod tls;
pub mod types;

pub use palette::{LegendScale, get_color_for_value, get_legend_scale};
