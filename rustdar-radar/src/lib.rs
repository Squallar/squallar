pub mod archive;
pub(crate) mod azimuth;
pub mod beam;
pub mod catalogue;
pub mod chunks;
pub mod current;
pub mod derive;
pub(crate) mod dpprep;
pub mod eet;
pub mod hail;
pub mod hca;
pub mod hhc;
pub mod hover;
pub mod kdp;
pub(crate) mod l3_values;
pub mod level3;
pub mod nrot;
pub mod nyquist;
mod palette;
pub(crate) mod par;
pub mod render;
pub mod render_input;
pub mod sampler;
pub mod scan;
pub mod site_position;
pub mod sites;
pub mod sounding;
pub mod sources;
pub mod srm;
pub mod srv;
pub mod tls;
pub mod twin;
pub mod types;
pub mod velocity;
pub mod vil;
pub mod vild;
pub mod volume_wire;
pub mod volumetric;
pub mod voxel;
pub mod xsect;

pub use palette::{LegendScale, RANGE_FOLDED, get_color_for_value, get_legend_scale};

/// The one bounds-checked cursor over untrusted payload bytes, now defined in
/// `rustdar-source`. Crate-visible only: the frontend's duplicate `Reader`
/// stays deliberately separate until the M6/M7 unification, and this crate
/// must not leak `wire` publicly meanwhile.
pub(crate) use rustdar_source::wire;
