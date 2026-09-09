//! The colour table a polar code plane is painted through.
//!
//! `docs/radar-polar-design.md` §2.3. A polar frame stores a sweep's gates as
//! the **codes the wire carried**, one byte each, and resolves colour on the
//! GPU through a 256-entry lookup table instead of on the CPU through
//! [`crate::palette::get_color_for_value`] per gate. The plane is the data; this
//! table is the palette, baked.
//!
//! **A renderer emits planes now** — [`crate::render::render_sweep_plane`],
//! through [`crate::render::plane`], for a Level II plan view whose caller
//! asked for the polar surface. The fidelity question is answered by
//! measurement rather than by assumption, and `codes/tests.rs` settles it.
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
//! or a 16-bit moment — a 256-entry table is a **quantiser**, and nothing in
//! the encoding bounds what it would have to quantise.
//! `an_r8_plane_is_exact_for_wire_bytes_and_lossy_for_computed_gradients`
//! derives the verdict from that and finds the split real in both directions:
//! an R8 plane carries every 8-bit wire moment exactly and drops distinctions
//! on every computed field.
//!
//! **A narrow palette is not a second way in.** A plane is read twice — the
//! table colours it and `Lut::value_of` answers a hover off it — so the count
//! that has to fit is the count of distinct **values** and never the count of
//! distinct colours. Two computed products rode a banded scale of ten and
//! twelve stops into the admitted set on the second count until 2026-09-09,
//! and on 124 real archive volumes across 15 sites they paint up to 352 and
//! 6,167 numbers. See [`R8Fidelity::ComputedValuesTooWide`].
//!
//! # The count is a sweep's, not a product's
//!
//! Everything above is about [`PlaneDecode::Affine`], where the codes are the
//! wire's own words and a product's verdict is the same for every sweep of it.
//! [`PlaneDecode::Table`] is the other half: its codes index the numbers **one
//! sweep actually painted**, so the question "do these fit a byte?" is asked
//! of the plane in hand. A computed field refused wholesale by
//! [`r8_fidelity`] still takes a plane on the sweeps whose realised values
//! number 254 or fewer, and stays a raster on the rest. Neither form ever
//! rounds: the table holds the painted bit patterns themselves.

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

/// **The first code a [`PlaneDecode::Table`] entry may take.**
///
/// The two sentinels keep the meanings they have in the affine form —
/// [`BELOW_THRESHOLD_CODE`] paints nothing and [`RANGE_FOLDED_CODE`] paints
/// [`palette::RANGE_FOLDED`] — so [`Reduce`]'s exclusion of them, the mip
/// chain's fallback and the raster's own "no gate here" all read the same in
/// both forms. What is left is [`PAINTABLE_CODES`], which is therefore both
/// the ceiling on a table and the bar a sweep is measured against.
pub const FIRST_TABLE_CODE: u8 = (LUT_ENTRIES - PAINTABLE_CODES) as u8;

/// **What one plane's codes mean**, in the two forms a code plane decodes
/// through.
///
/// The split is a fidelity one and not a stylistic one. A wire moment's 256
/// reachable values *are* `(c - offset) / scale` over every byte, so naming
/// that pair names all of them in nine bytes and the affine arm is exact by
/// construction. A field computed as `f32` per gate has no such rule: what it
/// paints is whatever the arithmetic produced, and the only exact description
/// of that is the list of patterns themselves.
///
/// **The table arm is admitted per sweep and never per product.**
/// [`r8_fidelity`] refuses a computed field wholesale because *some* of its
/// sweeps paint tens of thousands of distinct numbers, and a particular sweep
/// that paints two hundred is refused with them. A table is measured off the
/// plane in hand instead — `PolarField::compact_values`'s own rule, in the
/// form the picture can be drawn from — so a sweep that fits takes the plane
/// losslessly and a sweep that does not stays a raster and says so through
/// [`PlaneRefusal::TableWidth`].
#[derive(Clone, Debug, PartialEq)]
pub enum PlaneDecode {
    /// The codes are the wire's own words and [`LutKey`] says what each means.
    Affine {
        key: LutKey,
        /// The wire word size the codes arrived at — 8 or 16.
        ///
        /// **Provenance, and not re-derivable from what the plane stores.**
        /// [`r8_fidelity`] answers which widths a product *admits*, which for
        /// differential reflectivity is a strictly wider set than the one
        /// width a given sweep carried. An inference would be a second
        /// opinion about admissibility on the decode side.
        word_bits: u8,
    },
    /// The codes index the distinct bit patterns this sweep actually painted.
    ///
    /// **An indexing and never a rounding.** `values` holds the painted
    /// numbers themselves, so a read-back through [`CodePlane::value_table`]
    /// returns the pattern the raster's own grid held; what bounds the form is
    /// the *count* of them, and a sweep with more than [`PAINTABLE_CODES`] is
    /// refused rather than quantised onto the ones that fit.
    Table {
        product: RadarProduct,
        /// **Strictly ascending under [`f32::total_cmp`] and every entry
        /// finite**, indexed by `code - FIRST_TABLE_CODE`.
        ///
        /// Ascending because [`Reduce::MaxCode`] is defined as "the strongest
        /// echo is the largest code", which is a statement about the numbers
        /// and is false for a table in first-seen order. Finite because a NaN
        /// entry would be a third spelling of "nothing here" beside the two
        /// sentinels, and `PolarField::at` answers `None` for all three
        /// without being able to say which.
        values: Vec<f32>,
    },
}

impl PlaneDecode {
    /// The product this plane paints, whichever form says so.
    pub fn product(&self) -> RadarProduct {
        match self {
            Self::Affine { key, .. } => key.product,
            Self::Table { product, .. } => *product,
        }
    }

    /// The wire word size an affine plane's codes arrived at, or `None` for a
    /// table plane, whose codes arrived at no wire width at all.
    pub fn word_bits(&self) -> Option<u8> {
        match self {
            Self::Affine { word_bits, .. } => Some(*word_bits),
            Self::Table { .. } => None,
        }
    }

    /// **What every code a byte can address decodes to as a number** — the
    /// [`LUT_ENTRIES`]-long table a coded `PolarField` indexes.
    ///
    /// Whole rather than only the codes a sweep happens to carry, so it is a
    /// function of the decode alone and two planes decoded the same way cannot
    /// come to hold different tables. A code past a table plane's entries
    /// takes the unpainted marker, which is what [`CodePlane::build`] has
    /// already refused a plane for carrying.
    pub fn value_table(&self) -> Vec<f32> {
        match self {
            Self::Affine { key, .. } => Lut::value_table(*key),
            Self::Table { values, .. } => {
                let mut out = vec![crate::render::polar::UNPAINTED; LUT_ENTRIES];
                out[usize::from(RANGE_FOLDED_CODE)] = crate::render::RANGE_FOLDED_SENTINEL;
                let first = usize::from(FIRST_TABLE_CODE);
                out[first..first + values.len()].copy_from_slice(values);
                out
            }
        }
    }

    /// The magnitude [`Reduce::MaxMagnitude`] ranks `code` by, scaled to an
    /// integer so the ordering is total and the reduction associative.
    ///
    /// Read in each form's own space, and the two are the same ordering said
    /// twice: an affine map is monotone in `|c − offset|` whatever its scale's
    /// sign, and a table is ascending in the numbers themselves.
    ///
    /// The affine arm measures from the **rounded** offset — the code at which
    /// the decode reaches zero — because the quantity being ranked is a
    /// distance in code space and the zero it is measured from is a code.
    fn magnitude_of(&self, code: u8) -> i64 {
        let value = match self {
            Self::Affine { key, .. } => f32::from(code) - key.offset.round(),
            Self::Table { values, .. } => usize::from(code)
                .checked_sub(usize::from(FIRST_TABLE_CODE))
                .and_then(|i| values.get(i).copied())
                .unwrap_or(0.0),
        };
        (value.abs() * 1_000.0) as i64
    }
}

/// The first index at which `values` is not a strictly ascending run of finite
/// numbers, or `None` where it is one.
///
/// [`f32::total_cmp`] and not `<`: the ordering has to separate `-0.0` from
/// `0.0`, because the table's entries are the **bit patterns** a render
/// painted and the wire has to bring them back byte for byte. A `<` that
/// called those two equal would admit a table with a duplicate in it, and the
/// duplicate would be one gate's measurement answering under another gate's
/// code.
fn first_disordered(values: &[f32]) -> Option<usize> {
    for (i, value) in values.iter().enumerate() {
        if !value.is_finite() {
            return Some(i);
        }
        if let Some(next) = values.get(i + 1)
            && value.total_cmp(next) != std::cmp::Ordering::Less
        {
            return Some(i);
        }
    }
    None
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

    /// Bake the table for a whole [`PlaneDecode`] — the one entry point a
    /// consumer of a plane uses, so neither form is reached by naming it.
    ///
    /// The affine arm is [`Self::build`] unchanged. The table arm colours the
    /// numbers the sweep painted, and it reads them through
    /// [`PlaneDecode::value_table`] rather than off `values` directly, so the
    /// colour a code shows and the number a hover reads back come out of one
    /// array — including at the two sentinels, whose colours are the same in
    /// both forms because their codes are.
    pub fn of(decode: &PlaneDecode) -> Self {
        match decode {
            PlaneDecode::Affine { key, .. } => Self::build(*key),
            PlaneDecode::Table { product, .. } => {
                let values = decode.value_table();
                let mut entries = [(0, 0, 0, 0); LUT_ENTRIES];
                for (entry, value) in entries.iter_mut().zip(values) {
                    *entry = match value.to_bits() {
                        bits if bits == crate::render::polar::UNPAINTED.to_bits() => (0, 0, 0, 0),
                        bits if bits == crate::render::RANGE_FOLDED_SENTINEL.to_bits() => {
                            palette::RANGE_FOLDED
                        }
                        _ => get_color_for_value(*product, value),
                    };
                }
                Self { entries }
            }
        }
    }

    /// One entry, by code.
    pub fn entry(&self, code: u8) -> (u8, u8, u8, u8) {
        self.entries[usize::from(code)]
    }

    /// **What one code decodes to as a number**, or
    /// [`polar::UNPAINTED`](crate::render::polar::UNPAINTED) where the raster
    /// paints nothing at that code.
    ///
    /// [`Self::colour_of`]'s counterpart, in the same match on the same value
    /// and for the same reason: a code plane's numbers are its codes, and a
    /// readout over one has to answer the number the raster's own value grid
    /// held. The decode is `crate::render`'s `moment_value_at` followed by
    /// `painted_moment_value` — the exact pair the fill loop runs per gate —
    /// so a read-back through this is an **indexing** of the same arithmetic
    /// and never a rounding of it.
    ///
    /// The two things the raster paints that are both NaN stay distinct here:
    /// a below-threshold gate takes the unpainted marker and a range-folded
    /// one takes `RANGE_FOLDED_SENTINEL`. `PolarField::at` answers `None` for
    /// both, so nothing that goes through it can tell them apart — which is
    /// why they are separated on the bits at the one place that holds them.
    pub fn value_of(key: LutKey, code: u8) -> f32 {
        use nexrad_model::data::MomentValue;
        let decoded = if key.scale == 0.0 {
            MomentValue::Value(f32::from(code))
        } else {
            match code {
                BELOW_THRESHOLD_CODE => MomentValue::BelowThreshold,
                RANGE_FOLDED_CODE => MomentValue::RangeFolded,
                _ => MomentValue::Value((f32::from(code) - key.offset) / key.scale),
            }
        };
        crate::render::painted_moment_value(decoded).unwrap_or(crate::render::polar::UNPAINTED)
    }

    /// [`Self::value_of`] over every code a byte can address — the table a
    /// coded [`crate::render::polar::PolarField`] indexes.
    ///
    /// Whole rather than only the codes a sweep happens to carry, so the
    /// table is a function of the key alone and two planes decoded under one
    /// key cannot come to hold different tables.
    pub fn value_table(key: LutKey) -> Vec<f32> {
        (0..LUT_ENTRIES)
            .map(|code| Self::value_of(key, code as u8))
            .collect()
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

/// **The most radials a code plane may declare**, past which the payload is
/// refused rather than truncated.
///
/// Twice the 720 the RDA can declare: Level II states 0.5° or 1.0° azimuth
/// resolution and has no third, so this is slack by construction rather than
/// an observation of the widest sweep seen. `squallar_device_profile`'s
/// `MAX_POLAR_RADIALS` is this constant — the bound belongs beside the code
/// that enforces it, and the pricing crate reads it from here so the refusal
/// and the price cannot disagree about what is admissible.
pub const MAX_POLAR_RADIALS: usize = 2 * 720;

/// **The most gates a code plane may declare**, past which the payload is
/// refused.
///
/// The WebGL2 per-axis guarantee verbatim: the plane is a texture, and
/// `squallar_gpu` pins `downlevel_webgl2_defaults().using_resolution(adapter)`
/// on the web, which lifts resolution and nothing else. A real surveillance
/// cut declares 1832 gates, so this bound sits in the upper half of what the
/// data really produces and is live rather than decorative.
pub const MAX_POLAR_GATES: usize = crate::types::WEBGL2_MAX_TEXTURE_DIMENSION_2D;

/// Payloads refused by [`CodePlane::build`] since the process started.
///
/// Always on, like the rest of this crate's ledgers: a refusal is the only
/// evidence that a hostile or malformed sweep reached the encoder, and a
/// counter that exists only under `cfg(test)` cannot report one from a running
/// app.
static REFUSALS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Serialises the tests that read [`CodePlane::refusals`] as a **delta**.
///
/// The ledger is process-global and always on, and this crate's unit tests are
/// one binary, so two suites that both refuse payloads run in parallel threads
/// against one counter and a delta of "exactly six" becomes a race between
/// whichever tests happen to be in flight.
///
/// **Observed, not hypothesised.** With three such tests in the binary the
/// filtered run `--lib -- --test-threads 16 refused` failed 4 times in 60,
/// reporting `left: 8, right: 6` — two refusals from a neighbour landing
/// between one test's `before` and its assertion. With only the two this crate
/// had before the wire's arrival it still failed 1 time in 60, so the race
/// predates that test rather than being caused by it. Every test that refuses
/// a payload, or that reads the counter, takes this lock.
///
/// Poisoning is recovered from rather than propagated: a panic in one of these
/// tests is that test's own failure, and turning it into a second failure in
/// every other one hides which assertion actually broke.
#[cfg(test)]
pub(crate) static REFUSAL_LEDGER: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Take [`REFUSAL_LEDGER`] for the rest of the caller's scope.
#[cfg(test)]
pub(crate) fn hold_refusal_ledger() -> std::sync::MutexGuard<'static, ()> {
    REFUSAL_LEDGER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Why a payload could not become a code plane.
///
/// Every arm is a refusal and none is a truncation: a plane that silently
/// dropped radials or gates would paint a sweep that was never measured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaneRefusal {
    /// Zero radials, zero gates, or past [`MAX_POLAR_RADIALS`] /
    /// [`MAX_POLAR_GATES`].
    Shape { radials: usize, gates: usize },
    /// The code buffer is not exactly `radials * gates` bytes.
    CodeCount { got: usize, want: usize },
    /// The product's gates cannot survive eight bits. See [`R8Fidelity`].
    NotRepresentable {
        product: RadarProduct,
        fidelity: R8Fidelity,
    },
    /// The product is exact only on the 8-bit wire form and this sweep carried
    /// the wide one.
    WideWireWord {
        product: RadarProduct,
        word_bits: u8,
    },
    /// **This sweep painted more distinct numbers than a byte can name**, or
    /// none at all — the per-sweep half of the fidelity door, and the arm that
    /// makes a [`PlaneDecode::Table`] plane lossless rather than merely small.
    ///
    /// Refused and never truncated: dropping the entries past the ceiling
    /// would map every gate holding one onto a number nobody measured.
    TableWidth { entries: usize },
    /// A table that is not a strictly ascending run of finite numbers, at the
    /// first index where it stops being one.
    ///
    /// [`Reduce::MaxCode`] means "the largest code is the strongest echo", so
    /// a table in any other order paints the wrong cell at every zoom above
    /// the closest, and a non-finite entry is a third spelling of "nothing
    /// here" beside the two sentinels.
    TableNotAscending { at: usize },
    /// A code naming an entry the table does not hold.
    ///
    /// One sweep's codes read against another sweep's table answer a plausible
    /// number for a gate nobody measured, which is why this is a refusal on
    /// both sides of the wire rather than a clamp on either.
    CodeOutsideTable { code: u8, entries: usize },
}

/// Whether an R8 code plane can carry a product's gates without dropping a
/// distinction the raster paints today.
///
/// Measured, not assumed — `an_r8_plane_is_exact_for_wire_bytes_and_lossy_for_computed_gradients`
/// derives every one of these from how many **values** the encoding can
/// produce, and fails if this table disagrees with it. The palette walk that
/// runs beside it is a second, independent measurement of a *different*
/// quantity — how many colours the product resolves over the extent its own
/// legend declares — and it corroborates the computed products' verdict
/// without deciding it, because a plane answers a hover as well as painting a
/// picture. `the_palette_bounds_the_colours_and_a_plane_has_to_hold_the_values`
/// holds the two apart at the products where they disagree.
///
/// **`docs/radar-polar-design.md` §7 and §2.4 put six lossy products on R8
/// planes** — NROT and SRV in phase D, and KDP, VIL, EchoTops and VIL density
/// in the reduce table. That is a fidelity regression and this type is where
/// it stops: [`CodePlane::build`] refuses them, so the design's schedule
/// cannot be followed into the mistake by a later lane that reads the document
/// and not this table.
///
/// **This table is about the affine form alone**, and a refusal here is not
/// the last word on a product. [`PlaneDecode::Table`] carries a sweep on the
/// numbers it actually painted, and that door is opened per sweep by
/// [`CodePlane::build_values`] rather than by any row of this table — so a
/// product refused here still takes a plane on the sweeps whose realised
/// values number [`PAINTABLE_CODES`] or fewer, losslessly, and stays a raster
/// on the rest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum R8Fidelity {
    /// The wire carries this moment's codes at eight bits, so a gate can hold
    /// [`PAINTABLE_CODES`] distinct values and no more. An R8 plane stores it
    /// **as measured**, and the palette's width is irrelevant — reflectivity
    /// resolves thousands of distinct colours across its extent and a real
    /// reflectivity sweep still shows at most 254 of them, raster or plane.
    WireByteExact,
    /// Eight bits in most volumes and sixteen in some. Exact on the narrow
    /// form only, so [`CodePlane::build`] requires the caller to state the
    /// word size the sweep actually carried.
    ExactOnEightBitWireOnly,
    /// Sixteen-bit wire codes: 65,534 reachable values against 254 the plane
    /// can index. Excluded by its domain, whatever the palette does.
    WireWordTooWide,
    /// No wire code at all — the field is computed as `f32` per gate, so an R8
    /// plane is a quantiser rather than a store, and the palette resolves more
    /// distinct colours over the product's own extent than the plane has
    /// paintable codes.
    ComputedPaletteTooWide,
    /// Computed like the arm above, over a **banded** palette narrow enough
    /// that 254 codes hold every colour it can show — and refused anyway,
    /// because a plane has to answer the *numbers* and not only the picture.
    ///
    /// A code plane is read twice: [`Lut::entry`] colours it and
    /// [`Lut::value_of`] answers a hover off it, and
    /// `crate::render::polar::PolarField::at` returns the number the raster's
    /// own grid held. So what a byte has to address is the count of distinct
    /// **values** a render paints, and a banded palette bounds the count of
    /// distinct **colours** — a different quantity, and for these two an
    /// enormously smaller one.
    ///
    /// **Measured on 124 real archive volumes across 15 sites**, as the
    /// distinct `f32` bit patterns each render's own `PolarField` holds over
    /// every gate it painted:
    ///
    /// | product | distinct values, max | volumes past 254 |
    /// |---|---|---|
    /// | probability of severe hail | 352 | 3 of 124 |
    /// | maximum expected hail size | 6,167 | 21 of 124 |
    ///
    /// against 10 and 12 distinct colours respectively on the same renders.
    /// Restricting the count to gates the palette actually inks does not
    /// rescue either — 295 and 637 at the top, past 254 on 2 and 9 volumes.
    /// The first volume over the line reads exactly 255, so the bound is live
    /// and not decorative.
    ComputedValuesTooWide,
}

impl R8Fidelity {
    /// Whether an R8 plane is lossless for this verdict on the **narrow** wire
    /// form, with the wide-word case held out because that is a domain refusal
    /// rather than a question about the values.
    pub fn admits_eight_bit_wire(self) -> bool {
        self.admits(8)
    }

    /// Whether a plane may be built for this verdict at all, given the wire
    /// word size the sweep carried.
    ///
    /// **Only an encoding admits.** Every arm that says yes says it because
    /// the wire bounds one gate at [`PAINTABLE_CODES`] distinct values; no arm
    /// says it because a palette is narrow, and
    /// `the_palette_bounds_the_colours_and_a_plane_has_to_hold_the_values`
    /// exhibits the two products where those answers differ.
    fn admits(self, word_bits: u8) -> bool {
        match self {
            R8Fidelity::WireByteExact => true,
            R8Fidelity::ExactOnEightBitWireOnly => word_bits == 8,
            R8Fidelity::WireWordTooWide
            | R8Fidelity::ComputedPaletteTooWide
            | R8Fidelity::ComputedValuesTooWide => false,
        }
    }
}

/// The measured verdict for `product`. See [`R8Fidelity`].
pub fn r8_fidelity(product: RadarProduct) -> R8Fidelity {
    match product {
        RadarProduct::Reflectivity
        | RadarProduct::Velocity
        | RadarProduct::SpectrumWidth
        | RadarProduct::CorrelationCoefficient
        | RadarProduct::HydrometeorClassification => R8Fidelity::WireByteExact,
        RadarProduct::DifferentialReflectivity => R8Fidelity::ExactOnEightBitWireOnly,
        RadarProduct::DifferentialPhase => R8Fidelity::WireWordTooWide,
        RadarProduct::StormRelativeVelocity
        | RadarProduct::SpecificDifferentialPhase
        | RadarProduct::NormalizedRotation
        | RadarProduct::EchoTops
        | RadarProduct::EchoTopsInterpolated
        | RadarProduct::VerticallyIntegratedLiquid
        | RadarProduct::VilDensity
        | RadarProduct::PrecipitationRate => R8Fidelity::ComputedPaletteTooWide,
        // Banded scales of ten and twelve stops, and the plane still cannot
        // carry them: what a byte has to address is the numbers a hover reads
        // back, not the colours the picture shows. See
        // `R8Fidelity::ComputedValuesTooWide` for the counts.
        RadarProduct::ProbabilityOfSevereHail | RadarProduct::MaxExpectedHailSize => {
            R8Fidelity::ComputedValuesTooWide
        }
    }
}

/// How a mip level reduces the cells beneath it.
///
/// **Declared per product, because "strongest" is not one operation.** Design
/// §2.4.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reduce {
    /// The code ascends with the quantity, so the strongest echo is the
    /// largest code.
    MaxCode,
    /// Codes ascend from −Nyquist to +Nyquist, so the largest code is the
    /// fastest *away* rather than the strongest. The choice is the magnitude
    /// of the decoded quantity, which is well defined because the code map is
    /// affine and monotone.
    MaxMagnitude,
    /// No reduction: the plane is one level and the shader clamps to it.
    /// Hydrometeor class codes are categorical and ordinally meaningless; a
    /// maximum over them would name a class nobody measured.
    None,
}

impl Reduce {
    /// The operator `product` reduces by.
    pub fn for_product(product: RadarProduct) -> Self {
        match product {
            RadarProduct::HydrometeorClassification | RadarProduct::DifferentialPhase => {
                Reduce::None
            }
            RadarProduct::Velocity
            | RadarProduct::StormRelativeVelocity
            | RadarProduct::NormalizedRotation => Reduce::MaxMagnitude,
            _ => Reduce::MaxCode,
        }
    }
}

/// **One level down: `max(1, n >> 1)`** — the halving a GPU mip chain is
/// defined by, and therefore the only halving this chain may use.
///
/// A code plane is uploaded as a texture's own mip chain, and WebGPU fixes
/// both halves of that arithmetic: a level's extent is
/// `max(1, extent >> level)` and the levels a texture may declare are
/// `floor(log2(max(w, h))) + 1`. A chain halved any other way is not a mip
/// chain — it is a set of buffers whose shapes the texture will refuse, which
/// is what a ceil-halved chain was until 2026-09-08.
pub const fn half_level(n: usize) -> usize {
    if n > 1 { n >> 1 } else { 1 }
}

/// Levels in a full chain over `radials x gates`, **counting level 0**.
///
/// The last level is the one whose both dimensions have reached 1 under
/// repeated [`half_level`], which is `floor(log2(max(radials, gates))) + 1` —
/// wgpu's `Extent3d::max_mips` verbatim, because the chain IS the texture's
/// mip chain. `squallar_device_profile::constants::full_mip_levels` answers
/// the same question on the pricing side; this is the producer's own, and
/// `the_price_weighs_the_plane_the_producer_actually_builds` holds them equal.
pub fn full_mip_levels(radials: usize, gates: usize) -> usize {
    let mut levels = 1;
    let (mut r, mut g) = (radials, gates);
    while r > 1 || g > 1 {
        r = half_level(r);
        g = half_level(g);
        levels += 1;
    }
    levels
}

/// **A sweep's gates as the codes the wire carried, plus a max-reducing mip
/// chain** — the polar representation's payload.
///
/// Radial-major, one byte per gate at level 0. Each further level halves both
/// dimensions by [`half_level`] — the texture mip chain's own arithmetic, and
/// not a shape of this crate's choosing — and reduces the cells beneath it by
/// the product's [`Reduce`], so a zoomed-out fragment reads the strongest echo
/// in its footprint instead of whichever radial happened to be written last.
/// That is a deliberate change from the raster, whose `RenderBuffers::claim`
/// orders by `write_key` and is therefore last-radial-wins.
///
/// **A renderer emits these now** and nothing draws them yet, which are two
/// different statements: [`crate::render::render_sweep_plane`] builds a plane
/// where a caller asks for one, and no build installs a painter for it. The
/// budget seam that would price one stays dark until both are true —
/// `squallar_device_profile`'s own darkness gate is what holds that, and it
/// matches the price function's name as a literal, so this sentence names it
/// only by description.
#[derive(Clone, Debug, PartialEq)]
pub struct CodePlane {
    /// Level 0, radial-major, `radials * gates` bytes.
    codes: Vec<u8>,
    /// Levels 1..=L concatenated, each [`half_level`] of the one before.
    mips: Vec<u8>,
    /// Where each level begins in [`CodePlane::mips`], indexed by
    /// `level - 1`; one entry per level above zero.
    mip_offsets: Vec<usize>,
    radials: usize,
    gates: usize,
    /// What the codes mean — the wire's affine pair, or the table of patterns
    /// this sweep painted. See [`PlaneDecode`].
    decode: PlaneDecode,
    reduce: Reduce,
}

impl CodePlane {
    /// Build a plane whose codes are the wire's own words.
    ///
    /// `word_bits` is the wire word size the moment was carried at — 8 or 16 —
    /// and it is a parameter rather than an assumption because differential
    /// reflectivity appears as both and only the narrow form is exact.
    pub fn build(
        radials: usize,
        gates: usize,
        codes: Vec<u8>,
        key: LutKey,
        word_bits: u8,
    ) -> Result<Self, PlaneRefusal> {
        Self::build_decoded(
            radials,
            gates,
            codes,
            PlaneDecode::Affine { key, word_bits },
        )
    }

    /// **Build a plane whose codes index the numbers this sweep painted** —
    /// the per-sweep form.
    ///
    /// `values` is the distinct bit patterns, strictly ascending; code
    /// `FIRST_TABLE_CODE + i` names `values[i]` and the two sentinels keep
    /// their meanings. Refused where the table is empty, wider than
    /// [`PAINTABLE_CODES`], out of order, or does not hold every code the
    /// buffer carries.
    pub fn build_values(
        radials: usize,
        gates: usize,
        codes: Vec<u8>,
        product: RadarProduct,
        values: Vec<f32>,
    ) -> Result<Self, PlaneRefusal> {
        Self::build_decoded(
            radials,
            gates,
            codes,
            PlaneDecode::Table { product, values },
        )
    }

    /// Build a plane from a sweep's codes and what they decode through.
    ///
    /// Refuses, never truncates and never panics: a shape past the caps, a
    /// code buffer that does not match the shape, a product whose gates cannot
    /// survive eight bits on the wire form, a wide word for a product exact
    /// only at eight, and — for a table — a set of numbers a byte cannot name.
    /// Every refusal increments [`CodePlane::refusals`].
    pub fn build_decoded(
        radials: usize,
        gates: usize,
        codes: Vec<u8>,
        decode: PlaneDecode,
    ) -> Result<Self, PlaneRefusal> {
        Self::build_inner(radials, gates, codes, decode).inspect_err(|_| {
            REFUSALS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        })
    }

    fn build_inner(
        radials: usize,
        gates: usize,
        codes: Vec<u8>,
        decode: PlaneDecode,
    ) -> Result<Self, PlaneRefusal> {
        // **The fidelity door, and it is asked of the decode rather than of
        // the product.** An affine plane is exact for the products whose wire
        // words are one byte, which is a table of products; a table plane is
        // exact for the sweep whose numbers it holds, which is a property of
        // the plane in hand and of no product at all.
        match &decode {
            PlaneDecode::Affine { key, word_bits } => {
                let fidelity = r8_fidelity(key.product);
                if !fidelity.admits(*word_bits) {
                    return Err(match fidelity {
                        R8Fidelity::ExactOnEightBitWireOnly => PlaneRefusal::WideWireWord {
                            product: key.product,
                            word_bits: *word_bits,
                        },
                        _ => PlaneRefusal::NotRepresentable {
                            product: key.product,
                            fidelity,
                        },
                    });
                }
            }
            PlaneDecode::Table { values, .. } => {
                if values.is_empty() || values.len() > PAINTABLE_CODES {
                    return Err(PlaneRefusal::TableWidth {
                        entries: values.len(),
                    });
                }
                if let Some(at) = first_disordered(values) {
                    return Err(PlaneRefusal::TableNotAscending { at });
                }
            }
        }
        if radials == 0 || gates == 0 || radials > MAX_POLAR_RADIALS || gates > MAX_POLAR_GATES {
            return Err(PlaneRefusal::Shape { radials, gates });
        }
        let want = radials * gates;
        if codes.len() != want {
            return Err(PlaneRefusal::CodeCount {
                got: codes.len(),
                want,
            });
        }
        // Behind the length check on purpose: this is the one refusal whose
        // cost is a walk of the whole buffer, and a payload that is the wrong
        // size is already refused by a comparison of two numbers.
        if let PlaneDecode::Table { values, .. } = &decode {
            let ceiling = usize::from(FIRST_TABLE_CODE) + values.len();
            if let Some(&code) = codes.iter().find(|&&code| usize::from(code) >= ceiling) {
                return Err(PlaneRefusal::CodeOutsideTable {
                    code,
                    entries: values.len(),
                });
            }
        }

        let reduce = Reduce::for_product(decode.product());
        let mut plane = Self {
            codes,
            mips: Vec::new(),
            mip_offsets: Vec::new(),
            radials,
            gates,
            decode,
            reduce,
        };
        plane.build_chain();
        Ok(plane)
    }

    /// Reduce level by level, each from the one before.
    ///
    /// Hierarchical reduction equals reducing a level-0 footprint whole,
    /// because every operator here is a maximum over a total order with the
    /// same sentinel fallback, and a maximum is associative.
    /// `max_mip_is_max` asserts that against a direct walk of the footprint
    /// rather than trusting the argument.
    fn build_chain(&mut self) {
        if self.reduce == Reduce::None {
            return;
        }
        let levels = full_mip_levels(self.radials, self.gates);
        let (mut r, mut g) = (self.radials, self.gates);
        for level in 1..levels {
            let (pr, pg) = (r, g);
            r = half_level(pr);
            g = half_level(pg);
            self.mip_offsets.push(self.mips.len());
            for radial in 0..r {
                // **The last cell of an odd axis takes the remainder**, so the
                // footprints still PARTITION the level above: `half_level`
                // rounds down, and a cell that stopped at two would leave the
                // parent's odd last row or column in no footprint at all — a
                // gate the sweep measured that no zoomed-out fragment can
                // read. Up to three either way, so up to nine cells.
                let r_end = if radial + 1 == r { pr } else { radial * 2 + 2 };
                for gate in 0..g {
                    let g_end = if gate + 1 == g { pg } else { gate * 2 + 2 };
                    let mut cells = [0u8; 9];
                    let mut n = 0;
                    for sr in radial * 2..r_end {
                        for sg in gate * 2..g_end {
                            cells[n] = self.level_code(level - 1, pg, sr, sg);
                            n += 1;
                        }
                    }
                    let reduced = self.reduce_cells(&cells[..n]);
                    self.mips.push(reduced);
                }
            }
        }
    }

    /// One code out of an already-written level. `stride` is that level's gate
    /// count, passed in because the caller is mid-write and the offsets vector
    /// does not yet describe the level being produced.
    fn level_code(&self, level: usize, stride: usize, radial: usize, gate: usize) -> u8 {
        let index = radial * stride + gate;
        if level == 0 {
            self.codes[index]
        } else {
            self.mips[self.mip_offsets[level - 1] + index]
        }
    }

    /// The declared reduce over a cell's footprint, with the sentinel rule.
    ///
    /// Codes 0 and 1 are status and not measurement, so they are excluded from
    /// the comparison and only survive when nothing under the cell measured
    /// anything. Excluding them from [`Reduce::MaxMagnitude`] is load-bearing:
    /// code 0 sits 129 away from a mid-scale zero and would otherwise beat
    /// every real velocity.
    fn reduce_cells(&self, cells: &[u8]) -> u8 {
        let mut best: Option<u8> = None;
        let mut folded = false;
        for code in cells.iter().copied() {
            match code {
                BELOW_THRESHOLD_CODE => {}
                RANGE_FOLDED_CODE => folded = true,
                _ => {
                    let better = match best {
                        None => true,
                        Some(current) => match self.reduce {
                            Reduce::MaxCode => code > current,
                            // Ties to the larger code, so the key is a total
                            // order and the reduction is associative.
                            Reduce::MaxMagnitude => {
                                let key = |c: u8| (self.decode.magnitude_of(c), c);
                                key(code) > key(current)
                            }
                            Reduce::None => false,
                        },
                    };
                    if better {
                        best = Some(code);
                    }
                }
            }
        }
        best.unwrap_or(if folded {
            RANGE_FOLDED_CODE
        } else {
            BELOW_THRESHOLD_CODE
        })
    }

    /// Levels in this plane, counting level 0. `1` for [`Reduce::None`].
    pub fn levels(&self) -> usize {
        self.mip_offsets.len() + 1
    }

    /// A level's bytes and its dimensions, or `None` past the chain.
    pub fn level(&self, level: usize) -> Option<(&[u8], usize, usize)> {
        let (mut r, mut g) = (self.radials, self.gates);
        for _ in 0..level {
            r = half_level(r);
            g = half_level(g);
        }
        if level == 0 {
            return Some((&self.codes, r, g));
        }
        let start = *self.mip_offsets.get(level - 1)?;
        Some((&self.mips[start..start + r * g], r, g))
    }

    /// One code, by level and position.
    pub fn code_at(&self, level: usize, radial: usize, gate: usize) -> Option<u8> {
        let (bytes, r, g) = self.level(level)?;
        (radial < r && gate < g)
            .then(|| bytes[radial * g + gate])?
            .into()
    }

    /// What decodes and colours this plane.
    pub fn decode(&self) -> &PlaneDecode {
        &self.decode
    }

    /// The product this plane paints.
    pub fn product(&self) -> RadarProduct {
        self.decode.product()
    }

    /// What each of the 256 codes decodes to as a number — the table a coded
    /// `PolarField` beside this plane indexes.
    pub fn value_table(&self) -> Vec<f32> {
        self.decode.value_table()
    }

    /// The baked colour table this plane is painted through.
    pub fn lut(&self) -> Lut {
        Lut::of(&self.decode)
    }

    /// The operator its chain was reduced by.
    pub fn reduce(&self) -> Reduce {
        self.reduce
    }

    /// Level 0's shape.
    pub fn shape(&self) -> (usize, usize) {
        (self.radials, self.gates)
    }

    /// **Bytes this plane's buffers hold**, level 0 plus the whole chain.
    ///
    /// The heap the plane really occupies, read off the vectors rather than
    /// recomputed from the shape, so it is a measurement of the object and not
    /// a second spelling of the price.
    pub fn resident_bytes(&self) -> usize {
        self.codes.len() + self.mips.len()
    }

    /// The wire word size this plane's codes arrived at — 8 or 16 — or `None`
    /// for a table plane, whose codes were assigned here and arrived at no
    /// wire width at all. See [`PlaneDecode::Affine::word_bits`].
    pub fn word_bits(&self) -> Option<u8> {
        self.decode.word_bits()
    }

    /// Payloads refused since the process started.
    pub fn refusals() -> u64 {
        REFUSALS.load(std::sync::atomic::Ordering::Relaxed)
    }
}

// ── The wire form ────────────────────────────────────────────────────────────

/// **A plane crosses the worker wire as a head block plus a flat tail**, and
/// this is the head block's width.
///
/// The split is not a stylistic one. `8b22ca6f2` records what the alternative
/// costs: the overlay reply appends its picture to the head, behind a
/// variable-length block that states no byte length of its own, and *"the page
/// cannot find the picture in it"* — a reader that has not walked the whole
/// preceding list does not know where the pixels start, and the offset it does
/// reach is not 4-aligned. A tail has its own length by construction and
/// starts at offset zero, so the codes are addressable without walking
/// anything.
///
/// The block is **exactly [`CodePlane::build_decoded`]'s arguments but the
/// codes**, in declaration order, so the encoder and the decoder cannot come
/// to disagree about what a plane is made of: a form byte, `radials` `u32`,
/// `gates` `u32`, the product's `u16` wire code, then that form's own decode —
/// `scale` `f32`, `offset` `f32`, `word_bits` `u8` for the affine form, or a
/// `u32` count and that many `f32`s for the table.
///
/// **The form byte leads the block**, [`crate::render::polar::PolarWireForm`]'s
/// pattern to the letter: the two decodes are not distinguishable from their
/// own bytes, the tag is written in the same match arm as the payload it
/// describes, and a code this build does not write is declined rather than
/// assumed. It leads rather than trails because the shape fields are shared
/// and the *decode* is what the reader has to know the form of before it
/// interprets a byte of it.
impl CodePlane {
    /// An affine plane — `scale`, `offset` and a wire word size.
    pub const WIRE_FORM_AFFINE: u8 = 0;

    /// A table plane — the numbers the sweep painted, ascending.
    pub const WIRE_FORM_TABLE: u8 = 1;

    /// The form byte and the shape both forms open with.
    const WIRE_SHAPE_BYTES: usize = 1 + 4 + 4 + 2;

    /// Bytes an affine plane's head takes. Fixed — every field is a scalar of
    /// stated width.
    pub const WIRE_HEAD_AFFINE_BYTES: usize = Self::WIRE_SHAPE_BYTES + 4 + 4 + 1;

    /// **The widest head this block can be**: the shape, a table's count, and
    /// a full table. What a reply reserves, since a table's length is the one
    /// thing here that is not fixed.
    pub const WIRE_HEAD_MAX_BYTES: usize = Self::WIRE_SHAPE_BYTES + 4 + PAINTABLE_CODES * 4;

    /// The shape, little-endian — what both forms carry, written once so the
    /// two cannot come to describe one plane differently.
    ///
    /// `radials` and `gates` are written as `u32` and cannot truncate:
    /// [`CodePlane::build_decoded`] refuses anything past
    /// [`MAX_POLAR_RADIALS`] and [`MAX_POLAR_GATES`], both far below
    /// `u32::MAX`, so a plane that exists fits by construction.
    fn write_wire_shape(&self, out: &mut Vec<u8>) {
        let dim = |n: usize| {
            u32::try_from(n).expect("the shape caps bound both dimensions well below u32::MAX")
        };
        out.extend_from_slice(&dim(self.radials).to_le_bytes());
        out.extend_from_slice(&dim(self.gates).to_le_bytes());
        out.extend_from_slice(&self.decode.product().wire_code().to_le_bytes());
    }

    /// The head block, little-endian: the form byte, the shape, and that
    /// form's decode — the tag and the payload written in the same arm, so
    /// there is no arrangement of this function in which a head says one form
    /// and carries the other.
    pub fn write_wire_head(&self, out: &mut Vec<u8>) {
        match &self.decode {
            PlaneDecode::Affine { key, word_bits } => {
                out.push(Self::WIRE_FORM_AFFINE);
                self.write_wire_shape(out);
                out.extend_from_slice(&key.scale.to_le_bytes());
                out.extend_from_slice(&key.offset.to_le_bytes());
                out.push(*word_bits);
            }
            PlaneDecode::Table { values, .. } => {
                out.push(Self::WIRE_FORM_TABLE);
                self.write_wire_shape(out);
                out.extend_from_slice(
                    &u32::try_from(values.len())
                        .expect("a table is capped at PAINTABLE_CODES entries")
                        .to_le_bytes(),
                );
                for value in values {
                    out.extend_from_slice(&value.to_le_bytes());
                }
            }
        }
    }

    /// **Level 0's codes and nothing else** — the flat tail.
    ///
    /// The mip chain does **not** cross. It is a pure function of level 0 and
    /// the product's [`Reduce`], so sending it would be sending a derivation
    /// the receiver can make and would hand a hostile payload a chain that
    /// disagrees with the codes beneath it — a picture nobody measured, at
    /// every zoom but the closest. `docs/radar-polar-design.md` §2.4 builds
    /// the chain at upload for the same reason it is not a wire field.
    ///
    /// The saving is real and on the campaign's own axis: a surveillance
    /// sweep's level 0 is 1,319,040 B against the whole chain's 1,758,630 B
    /// (`a_surveillance_sweeps_plane_costs_what_the_chain_sums_to`), so the
    /// transient the receiving side allocates is 25% smaller than the object
    /// it builds.
    pub fn to_bytes(&self) -> Vec<u8> {
        self.codes.clone()
    }

    /// [`CodePlane::to_bytes`] by move — the encoder's spelling, so a reply
    /// does not hold two copies of a megabyte-plus buffer at once. It cannot
    /// disagree with the borrowing form: both hand back the same field.
    pub fn into_codes(self) -> Vec<u8> {
        self.codes
    }

    /// The inverse of [`write_wire_head`](Self::write_wire_head) plus
    /// [`to_bytes`](Self::to_bytes): read the block off `r` and rebuild.
    ///
    /// **Rebuilt through [`CodePlane::build`], not assembled field by field.**
    /// That is what keeps the wire from acquiring a second opinion about
    /// admissibility: the caps, the code-count check and the eight lossy
    /// products' refusal are the producer's own, run again on the decode side
    /// because they are the *same* function, and every refusal here lands in
    /// [`CodePlane::refusals`] rather than in a counter of the wire's own.
    ///
    /// The one refusal that is **not** counted there is a product wire code
    /// this build does not know: there is no [`LutKey`] to build with, so no
    /// plane is ever attempted. It is an ordinary `None`, and the reply is
    /// treated as a failed job.
    pub fn from_wire(r: &mut squallar_source::wire::Reader<'_>, codes: Vec<u8>) -> Option<Self> {
        let form = r.u8()?;
        let radials = r.u32()? as usize;
        let gates = r.u32()? as usize;
        let product = RadarProduct::from_wire_code(r.u16()?)?;
        let decode = match form {
            Self::WIRE_FORM_AFFINE => {
                let scale = r.f32()?;
                let offset = r.f32()?;
                let word_bits = r.u8()?;
                PlaneDecode::Affine {
                    key: LutKey {
                        product,
                        scale,
                        offset,
                    },
                    word_bits,
                }
            }
            Self::WIRE_FORM_TABLE => {
                let entries = r.u32()? as usize;
                // In front of the reservation and not behind it: a head
                // declaring four billion entries costs a comparison here and a
                // four-billion-element `Vec` one line later. `build_decoded`
                // refuses the same width again, and this is not that check —
                // it is the one that keeps the refusal reachable.
                if entries > PAINTABLE_CODES {
                    return None;
                }
                let mut values = Vec::with_capacity(entries);
                for _ in 0..entries {
                    values.push(r.f32()?);
                }
                PlaneDecode::Table { product, values }
            }
            _ => return None,
        };
        Self::build_decoded(radials, gates, codes, decode).ok()
    }
}

#[cfg(test)]
mod tests;
