pub mod archive;
pub(crate) mod azimuth;
pub mod beam;
pub mod catalogue;
pub mod chunk_feed;
pub mod chunk_notify;
pub mod chunks;
pub mod current;
pub mod derive;
pub(crate) mod dpprep;
pub mod eet;
pub mod fields;
pub mod frame;
pub mod hail;
pub mod hca;
pub mod hhc;
pub mod hover;
pub mod jobs;
pub mod kdp;
pub(crate) mod l3_values;
pub mod level3;
pub mod loop_downloads;
pub mod loop_geometry;
pub mod nrot;
pub mod nyquist;
mod palette;
pub(crate) mod par;
pub(crate) mod product_spec;
pub mod render;
pub mod render_input;
pub mod sampler;
pub mod scan;
pub mod site_position;
pub mod sites;
pub mod sounding;
/// The radar layer's `SourceHandler` registration.
pub mod source;
/// Network origins, defined in `squallar-source` and re-exported here.
pub use squallar_source::origins as sources;
pub mod srm;
pub mod srv;
pub use squallar_source::tls;
/// The fresh-process pins tying `scan`/`archive`/`chunks` to `tls::init`.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tls_pins;
pub mod twin;
pub mod types;
pub mod velocity;
pub mod vil;
pub mod vild;
pub mod volume_wire;
pub mod volumetric;
pub mod voxel;
pub mod xsect;

pub use palette::{
    LegendScale, LegendScaleRef, RANGE_FOLDED, get_color_for_value, get_legend_scale,
    get_legend_scale_ref,
};

/// Bounds-checked cursor over untrusted payload bytes, from `squallar-source`;
/// crate-visible only — `wire` must not leak publicly.
pub(crate) use squallar_source::wire;
