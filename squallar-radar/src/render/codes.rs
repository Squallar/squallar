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
//! asked for the polar surface. What is still dark is the *draw*: no build
//! installs a `RadarFanPainter`, so nothing paints through this table on a
//! shipped scene, and `squallar_device_profile`'s polar price stays unselected
//! with it — that crate scrapes for its own function's name as a literal, so
//! this sentence names it only by description. The fidelity question was
//! answerable before any pixel
//! moved for the same reason it still is: a 256-entry table is exact for some
//! products and cannot be for others, and `codes/tests.rs` settles which is
//! which by measurement rather than by assumption.
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
}

/// Whether an R8 code plane can carry a product's gates without dropping a
/// distinction the raster paints today.
///
/// Measured, not assumed — `an_r8_plane_is_exact_for_wire_bytes_and_lossy_for_computed_gradients`
/// derives every one of these from two independent quantities (how many values
/// the encoding can produce, and how many colours the palette resolves over
/// the extent its own legend declares) and fails if this table disagrees with
/// the measurement.
///
/// **`docs/radar-polar-design.md` §7 and §2.4 put six lossy products on R8
/// planes** — NROT and SRV in phase D, and KDP, VIL, EchoTops and VIL density
/// in the reduce table. That is a fidelity regression and this type is where
/// it stops: [`CodePlane::build`] refuses them, so the design's schedule
/// cannot be followed into the mistake by a later lane that reads the document
/// and not this table.
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
    /// Computed like the arm above, but over a **banded** palette narrow
    /// enough that 254 codes hold every colour it can show. Fidelity admits
    /// it; note that no quantiser is defined for these products anywhere, so
    /// admitting one is not the same as being ready to build it.
    ComputedPaletteFits,
}

impl R8Fidelity {
    /// Whether an R8 plane is lossless for this verdict on the **narrow** wire
    /// form — the question the palette measurement answers, with the wide-word
    /// case held out because that is a domain refusal and not a palette one.
    pub fn admits_eight_bit_wire(self) -> bool {
        self.admits(8)
    }

    /// Whether a plane may be built for this verdict at all, given the wire
    /// word size the sweep carried.
    fn admits(self, word_bits: u8) -> bool {
        match self {
            R8Fidelity::WireByteExact | R8Fidelity::ComputedPaletteFits => true,
            R8Fidelity::ExactOnEightBitWireOnly => word_bits == 8,
            R8Fidelity::WireWordTooWide | R8Fidelity::ComputedPaletteTooWide => false,
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
        // Banded scales of ten and twelve stops: an R8 plane holds them with
        // room to spare. They are not migrated for size reasons rather than
        // fidelity ones -- 360 x 230 cells is 82.8 KB and the polar win does
        // not pay for defining a quantiser -- but nothing here forbids it.
        RadarProduct::ProbabilityOfSevereHail | RadarProduct::MaxExpectedHailSize => {
            R8Fidelity::ComputedPaletteFits
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

/// Levels in a full chain over `radials x gates`, **counting level 0**.
///
/// The last level is the one whose both dimensions have reached 1 under
/// repeated ceil-halving. `squallar_device_profile::constants::full_mip_levels`
/// answers the same question on the pricing side; this is the producer's own,
/// and `the_price_weighs_the_plane_the_producer_actually_builds` holds them equal.
pub fn full_mip_levels(radials: usize, gates: usize) -> usize {
    let mut levels = 1;
    let (mut r, mut g) = (radials, gates);
    while r > 1 || g > 1 {
        r = r.div_ceil(2);
        g = g.div_ceil(2);
        levels += 1;
    }
    levels
}

/// **A sweep's gates as the codes the wire carried, plus a max-reducing mip
/// chain** — the polar representation's payload.
///
/// Radial-major, one byte per gate at level 0. Each further level ceil-halves
/// both dimensions and reduces the up-to-four cells beneath it by the
/// product's [`Reduce`], so a zoomed-out fragment reads the strongest echo in
/// its footprint instead of whichever radial happened to be written last. That
/// is a deliberate change from the raster, whose `RenderBuffers::claim` orders
/// by `write_key` and is therefore last-radial-wins.
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
    /// Levels 1..=L concatenated, each ceil-halved from the one before.
    mips: Vec<u8>,
    /// Where each level begins in [`CodePlane::mips`], indexed by
    /// `level - 1`; one entry per level above zero.
    mip_offsets: Vec<usize>,
    radials: usize,
    gates: usize,
    key: LutKey,
    reduce: Reduce,
    /// The wire word size the codes arrived at, as [`CodePlane::build`] was
    /// told — provenance, and the one build argument the plane cannot
    /// re-derive from what it stores. See [`CodePlane::word_bits`].
    word_bits: u8,
}

impl CodePlane {
    /// Build a plane from a sweep's raw codes.
    ///
    /// `word_bits` is the wire word size the moment was carried at — 8 or 16 —
    /// and it is a parameter rather than an assumption because differential
    /// reflectivity appears as both and only the narrow form is exact.
    ///
    /// Refuses, never truncates and never panics: a shape past the caps, a
    /// code buffer that does not match the shape, a product whose gates cannot
    /// survive eight bits, and a wide word for a product that is exact only at
    /// eight. Every refusal increments [`CodePlane::refusals`].
    pub fn build(
        radials: usize,
        gates: usize,
        codes: Vec<u8>,
        key: LutKey,
        word_bits: u8,
    ) -> Result<Self, PlaneRefusal> {
        Self::build_inner(radials, gates, codes, key, word_bits).inspect_err(|_| {
            REFUSALS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        })
    }

    fn build_inner(
        radials: usize,
        gates: usize,
        codes: Vec<u8>,
        key: LutKey,
        word_bits: u8,
    ) -> Result<Self, PlaneRefusal> {
        let fidelity = r8_fidelity(key.product);
        if !fidelity.admits(word_bits) {
            return Err(match fidelity {
                R8Fidelity::ExactOnEightBitWireOnly => PlaneRefusal::WideWireWord {
                    product: key.product,
                    word_bits,
                },
                _ => PlaneRefusal::NotRepresentable {
                    product: key.product,
                    fidelity,
                },
            });
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

        let reduce = Reduce::for_product(key.product);
        let mut plane = Self {
            codes,
            mips: Vec::new(),
            mip_offsets: Vec::new(),
            radials,
            gates,
            key,
            reduce,
            word_bits,
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
            r = r.div_ceil(2);
            g = g.div_ceil(2);
            self.mip_offsets.push(self.mips.len());
            for radial in 0..r {
                for gate in 0..g {
                    let mut cells = [None; 4];
                    let mut n = 0;
                    for dr in 0..2 {
                        for dg in 0..2 {
                            let (sr, sg) = (radial * 2 + dr, gate * 2 + dg);
                            if sr < pr && sg < pg {
                                cells[n] = Some(self.level_code(level - 1, pg, sr, sg));
                                n += 1;
                            }
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

    /// The declared reduce over up to four cells, with the sentinel rule.
    ///
    /// Codes 0 and 1 are status and not measurement, so they are excluded from
    /// the comparison and only survive when nothing under the cell measured
    /// anything. Excluding them from [`Reduce::MaxMagnitude`] is load-bearing:
    /// code 0 sits 129 away from a mid-scale zero and would otherwise beat
    /// every real velocity.
    fn reduce_cells(&self, cells: &[Option<u8>]) -> u8 {
        let zero_code = self.key.offset.round();
        let mut best: Option<u8> = None;
        let mut folded = false;
        for code in cells.iter().flatten().copied() {
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
                                let key = |c: u8| {
                                    (((f32::from(c) - zero_code).abs() * 1_000.0) as i64, c)
                                };
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
            r = r.div_ceil(2);
            g = g.div_ceil(2);
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
    pub fn key(&self) -> LutKey {
        self.key
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

    /// The wire word size this plane's codes arrived at — 8 or 16, whichever
    /// [`CodePlane::build`] was told.
    ///
    /// **Provenance, and not re-derivable from what the plane stores.**
    /// [`r8_fidelity`] answers which widths a product *admits*, which for
    /// differential reflectivity is a strictly wider set than the one width
    /// this sweep actually carried. So the wire carries the byte rather than
    /// inferring it: an inference would be a second opinion about
    /// admissibility on the decode side, and the two halves could then differ
    /// about which sweeps are representable.
    pub fn word_bits(&self) -> u8 {
        self.word_bits
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
/// The block is **exactly [`CodePlane::build`]'s arguments but the codes**, in
/// declaration order, so the encoder and the decoder cannot come to disagree
/// about what a plane is made of: `radials` `u32`, `gates` `u32`, the
/// product's `u16` wire code, `scale` `f32`, `offset` `f32`, `word_bits` `u8`.
impl CodePlane {
    /// Bytes [`CodePlane::write_wire_head`] writes. Fixed — every field is a
    /// scalar of stated width.
    pub const WIRE_HEAD_BYTES: usize = 4 + 4 + 2 + 4 + 4 + 1;

    /// The head block, little-endian.
    ///
    /// `radials` and `gates` are written as `u32` and cannot truncate:
    /// [`CodePlane::build`] refuses anything past [`MAX_POLAR_RADIALS`] and
    /// [`MAX_POLAR_GATES`], both far below `u32::MAX`, so a plane that exists
    /// fits by construction.
    pub fn write_wire_head(&self, out: &mut Vec<u8>) {
        let dim = |n: usize| {
            u32::try_from(n).expect("the shape caps bound both dimensions well below u32::MAX")
        };
        out.extend_from_slice(&dim(self.radials).to_le_bytes());
        out.extend_from_slice(&dim(self.gates).to_le_bytes());
        out.extend_from_slice(&self.key.product.wire_code().to_le_bytes());
        out.extend_from_slice(&self.key.scale.to_le_bytes());
        out.extend_from_slice(&self.key.offset.to_le_bytes());
        out.push(self.word_bits);
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
    /// sweep's level 0 is 1,319,040 B against the whole chain's 1,758,832 B
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
        let radials = r.u32()? as usize;
        let gates = r.u32()? as usize;
        let product = RadarProduct::from_wire_code(r.u16()?)?;
        let scale = r.f32()?;
        let offset = r.f32()?;
        let word_bits = r.u8()?;
        Self::build(
            radials,
            gates,
            codes,
            LutKey {
                product,
                scale,
                offset,
            },
            word_bits,
        )
        .ok()
    }
}

#[cfg(test)]
mod tests;
