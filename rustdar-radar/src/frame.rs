//! The frame reply type — what a rasterizing job produces — beside the
//! renderer that fills it (WO-M7.1).
//!
//! [`RenderedFrame`] and its `From<SweepRender>` conversion moved here
//! **verbatim** from `rustdar_frontend::offload`, which re-exports the type
//! at the path it always published it at — the move is what lets the radar
//! codec rows ([`crate::jobs`]) name their own output type in their `run`
//! bodies. The two wire newtypes that ride beside it on the browser's reply
//! port (`MeltingLayerWire`/`StormMotionWire`) stay in the frontend until
//! the reply direction joins the codec table (WO-M7c), which is also when
//! this type gains a wire form of its own.

/// What a rasterizing job produces: the RGBA texture, the half-width it was
/// projected at, and the per-pixel value grid (`NAN` where no gate landed).
///
/// Named fields, as the renderer's own [`crate::render::SweepRender`]
/// has: the two buffers are the same shape to a message port, and transposing
/// them would swap a texture for a value grid somewhere with no type error to
/// catch it. A separate type and not that one because this is what crosses the
/// port.
///
/// The extent and the fold limit are metadata and stay metadata — they say
/// where the pixels *are* and what speed they wrap at, never how many of them
/// there are. How many there are is the buffer's own length, checked rather
/// than believed at each consumer (`constants::raster_side_from_rgba_len`);
/// nothing on this port describes its own shape, which is what keeps a
/// malformed payload from being believed. Adding a second `f64` beside the
/// extent does not weaken that: neither number can be read as a dimension,
/// and the guard that protects a pane from a blank texture reads the length
/// and only the length.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderedFrame {
    pub image: Vec<u8>,
    pub max_range_km: f64,
    /// The gates behind the pixels, at the resolution the radar measured them.
    ///
    /// **The `side²` `f32` raster grid is not here and does not leave the
    /// renderer.** It used to: `7362² × 4` = 206.75 MiB on desktop, and 16 MiB
    /// through the browser's `postMessage` — transferred, but still copied once
    /// into the worker's linear memory and once back out of the page's. This is
    /// the same numbers at the resolution they were measured at, about 5 MiB
    /// for the widest sweep the fleet flies, and it is what a hover reads. See
    /// [`crate::render::polar`].
    pub polar: crate::render::polar::PolarField,
    /// Where the rendered sweep's cut declared its velocity folds, m/s, or
    /// `None` for a raster with no one cut behind it — every Level III
    /// product and every volume product — and for a volume that declared
    /// nothing, which is every Message 1 volume.
    ///
    /// See [`crate::render::SweepRender::nyquist_ms`], which is where
    /// it comes from and which explains what it is a property of.
    pub nyquist_ms: Option<f64>,
    /// Where the melting layer this raster was classified against came from,
    /// or `None` for every raster that classified nothing — which is every
    /// product but the hybrid classification.
    ///
    /// See [`crate::hca::MeltingLayerSource`]. It rides beside
    /// `nyquist_ms` and for the same reason: it is a fact about *this* picture
    /// that the far end cannot recompute, and here it is the difference
    /// between a classification measured for this volume and one standing on a
    /// fleet constant that has been measured 3 km wrong.
    pub melting_layer_source: Option<crate::hca::MeltingLayerSource>,
    /// Where the storm motion vector this raster was shifted by came from, or
    /// `None` for every raster that shifted nothing — which is every product
    /// but storm-relative velocity.
    ///
    /// See [`crate::srv::SrvMotion`]. It rides beside
    /// `melting_layer_source` and for the same reason: it is a fact about
    /// *this* picture that the far end cannot recompute — the projection of
    /// this vector is already inside every gate value, and the two derived
    /// rungs are computed from a wind profile the page never sees.
    ///
    /// The whole vector rather than its provenance byte, because the legend
    /// draws the speed and direction and only apologises for nothing.
    pub storm_motion: Option<crate::srv::SrvMotion>,
}

impl From<crate::render::SweepRender> for RenderedFrame {
    /// The renderer's own answer, whole. One conversion for all three
    /// rasterizing arms, so a Level III frame and a Level II one cannot come to
    /// describe themselves differently.
    fn from(render: crate::render::SweepRender) -> Self {
        // **Where the raster value grid dies, on every path.** It is the
        // rasterizer's own instrument — its tests measure painted ranges and
        // ring bounds off it, and the colouring pass writes through it — and
        // nothing outside that crate has needed it since the readout started
        // reading gates. This is the one conversion all three rasterizing arms
        // come through, so putting it here is what makes "it never leaves the
        // renderer" a property of the type rather than of three call sites.
        //
        // Handed back rather than freed: the slot is waiting for it, and on
        // desktop this is a 206.75 MiB allocation glibc can never recycle. See
        // `crate::render::POOLED_VALUES`.
        crate::render::recycle_values(render.values);
        Self {
            image: render.image,
            max_range_km: render.max_range_km,
            polar: render.polar,
            nyquist_ms: render.nyquist_ms,
            melting_layer_source: render.melting_layer_source,
            storm_motion: render.storm_motion,
        }
    }
}

/// The reply half of the job boundary's erasure seam: a described frame
/// render answers this type through the codec rows in [`crate::jobs`] —
/// erased on the direct path, and on the wire too once WO-M7c gives the
/// frame reply its OUT codec.
impl rustdar_source::job::JobOut for RenderedFrame {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn into_any(self: Box<Self>) -> Box<dyn std::any::Any> {
        self
    }
}
