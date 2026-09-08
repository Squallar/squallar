//! The colour table a polar code plane is painted through.
//!
//! `docs/radar-polar-design.md` §2.3. A polar frame stores a sweep's gates as
//! the **codes the wire carried**, one byte each, and resolves colour on the
//! GPU through a 256-entry lookup table instead of on the CPU through
//! [`crate::palette::get_color_for_value`] per gate. The plane is the data; this
//! table is the palette, baked.
//!
//! **This module produces a table and nothing draws through it yet.** No
//! renderer emits a code plane, so nothing here is on a paint path — it is the
//! first landable piece of the polar representation and it is deliberately
//! inert. Building it early is what makes the *fidelity* question answerable
//! before any pixel moves: a 256-entry table is exact for some products and
//! cannot be for others, and `codes/tests.rs` settles which is which by
//! measurement rather than by assumption.
//!
//! # Why a bake loses nothing where it is exact
//!
//! [`crate::palette::get_color_for_value`] is a pure function of
//! `(product, value)`, and on a wire-coded moment `value` is a function of
//! `(raw, scale, offset)` alone — [`crate::render`]'s `moment_value_at` is the one
//! definition and this module mirrors it term for term, `scale == 0.0` branch
//! included. So for a moment whose codes are eight bits wide there are exactly
//! 256 reachable values, the table has exactly 256 entries, and the bake is a
//! re-indexing rather than a resampling. `the_lut_reproduces_get_color_for_value`
//! asserts that as byte equality over every entry, not as a tolerance.
//!
//! Where the codes are **not** the wire's — a derived field computed as `f32`,
//! or a 16-bit moment — a 256-entry table is a **quantiser**, and whether that
//! is lossless is a property of the product's own palette rather than of the
//! representation. `an_r8_plane_is_exact_for_wire_bytes_and_lossy_for_computed_gradients`
//! measures both halves per product and derives the verdict, and it finds the
//! split real in both directions: an R8 plane carries every 8-bit wire moment
//! exactly and drops distinctions on every computed field with a gradient
//! palette.

use crate::palette::{self, get_color_for_value};
use crate::types::RadarProduct;

/// Codes in an R8 plane, and entries in the table that colours it.
///
/// One entry per code the plane can address: the table is exact only while
/// that holds, which is the invariant `squallar_device_profile`'s
/// `POLAR_LUT_ENTRIES` restates on the pricing side.
pub const LUT_ENTRIES: usize = 256;

/// The code for a gate below the moment's SNR threshold — **unpainted**, not
/// black. `crate::render`'s `moment_value_at` maps raw 0 to
/// `MomentValue::BelowThreshold` and the raster leaves such a pixel unclaimed.
pub const BELOW_THRESHOLD_CODE: u8 = 0;

/// The code for a **range-folded** gate, painted [`palette::RANGE_FOLDED`].
pub const RANGE_FOLDED_CODE: u8 = 1;

/// Codes that carry a measurement rather than a status: everything but the two
/// sentinels above. The ceiling on how many distinct values an R8 plane can
/// hold, and the bar
/// `an_r8_plane_is_exact_for_wire_bytes_and_lossy_for_computed_gradients`
/// measures each product against.
pub const PAINTABLE_CODES: usize = LUT_ENTRIES - 2;

/// What decodes a code plane, and therefore what colours it.
///
/// **Not the product alone.** `scale` and `offset` come from the moment block
/// the sweep was carried in and two sweeps of one product can disagree about
/// them, so a table cached under the product alone would paint the second
/// sweep with the first sweep's arithmetic. Design §2.3.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LutKey {
    pub product: RadarProduct,
    /// The moment block's scale. `0.0` is not "no scaling" — it is the
    /// format's own "the raw words *are* the values" arm, and it suppresses
    /// the two sentinels because in that encoding 0 and 1 are ordinary
    /// numbers. See [`Lut::build`].
    pub scale: f32,
    pub offset: f32,
}

impl LutKey {
    /// The key for a product whose codes are its own, with no affine decode:
    /// a categorical plane (hydrometeor class) or a plane a quantiser already
    /// wrote in code space.
    pub fn identity(product: RadarProduct) -> Self {
        Self {
            product,
            scale: 1.0,
            offset: 0.0,
        }
    }
}

/// A baked 256-entry RGBA colour table: [`crate::palette::get_color_for_value`]
/// evaluated once per code instead of once per gate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lut {
    entries: [(u8, u8, u8, u8); LUT_ENTRIES],
}

impl Lut {
    /// Bake the table for `key`.
    ///
    /// The decode mirrors [`crate::render`]'s `moment_value_at` exactly, because
    /// a second spelling of it is a second authority on what a code means:
    ///
    /// * `scale == 0.0` — the raw word *is* the value, for every code
    ///   including 0 and 1. The comparison is against literal zero on purpose;
    ///   that is the format's own encoding and not a tolerance.
    /// * otherwise raw 0 is below-threshold (unpainted), raw 1 is range-folded,
    ///   and raw `c` decodes to `(c - offset) / scale`.
    ///
    /// A non-finite decode paints nothing: `get_color_for_value` answers
    /// `(0, 0, 0, 0)` for it, so an infinite or NaN `scale` degrades to a fully
    /// unpainted table rather than to a panic or to garbage.
    pub fn build(key: LutKey) -> Self {
        let mut entries = [(0, 0, 0, 0); LUT_ENTRIES];
        for (code, entry) in entries.iter_mut().enumerate() {
            *entry = Self::colour_of(key, code as u8);
        }
        Self { entries }
    }

    /// One code's colour, without building the table. [`Lut::build`] is this
    /// over every code, and the two can never disagree because that is how the
    /// table is filled.
    pub fn colour_of(key: LutKey, code: u8) -> (u8, u8, u8, u8) {
        if key.scale == 0.0 {
            return get_color_for_value(key.product, f32::from(code));
        }
        match code {
            BELOW_THRESHOLD_CODE => (0, 0, 0, 0),
            RANGE_FOLDED_CODE => palette::RANGE_FOLDED,
            _ => get_color_for_value(key.product, (f32::from(code) - key.offset) / key.scale),
        }
    }

    /// One entry, by code.
    pub fn entry(&self, code: u8) -> (u8, u8, u8, u8) {
        self.entries[usize::from(code)]
    }

    /// The table as the bytes a `256 x 1` `Rgba8Unorm` texture takes.
    ///
    /// **Straight alpha, not premultiplied.** The fragment stage multiplies by
    /// the layer's opacity and premultiplies there, the way
    /// `squallar_gpu`'s tile-mesh path already does into egui's own blend
    /// state; premultiplying here would apply the factor twice. It is also why
    /// a code plane must never reach `RenderedFrame::straight_rasters_mut` —
    /// codes are indices and cannot be premultiplied at all.
    pub fn to_rgba_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(LUT_ENTRIES * 4);
        for &(r, g, b, a) in &self.entries {
            out.extend_from_slice(&[r, g, b, a]);
        }
        out
    }
}

/// **Distinct painted colours `product`'s palette produces over the extent its
/// own legend declares**, sampled at `samples` evenly spaced values.
///
/// The figure a 256-entry table has to be measured against: a palette that
/// shows more distinct colours than an R8 plane has paintable codes
/// ([`PAINTABLE_CODES`]) cannot ride one without losing some of them.
///
/// **This is a floor, and callers must show it converged.** A sampled count of
/// distinct outputs can only ever miss colours that live between two samples,
/// never invent one, so it under-reports on a scale fine enough to hide a band
/// between adjacent probes. The extent comes from
/// [`crate::palette::get_legend_scale_ref`] — the range the product's own
/// legend advertises — rather than from a literal chosen here, so a palette
/// that grows a band grows this domain with it.
///
/// Unpainted answers (alpha 0) are excluded: below a product's transparency
/// cutoff there is no colour, and counting "nothing" as a colour would make
/// every product's figure one larger for a reason unrelated to its palette.
pub fn distinct_palette_colours(product: RadarProduct, samples: usize) -> usize {
    let scale = palette::get_legend_scale_ref(product);
    let (lo, hi) = (scale.min_value, scale.max_value);
    let mut seen = std::collections::BTreeSet::new();
    for i in 0..samples {
        let t = i as f64 / (samples.max(2) - 1) as f64;
        let value = (f64::from(lo) + t * (f64::from(hi) - f64::from(lo))) as f32;
        let rgba = get_color_for_value(product, value);
        if rgba.3 != 0 {
            seen.insert(rgba);
        }
    }
    seen.len()
}

#[cfg(test)]
mod tests;
