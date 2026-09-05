//! The gridded raster's colour registry: which fields
//! [`rasterize_gridded`](super::rasterize::rasterize_gridded) can paint, and how.
//!
//! The raster itself knows only a [`FieldId`]. This module is the one place
//! that turns that identity into a colour, so a new gridded source registers a
//! row here rather than adding an arm to the rasterizer, the codec or the wire.
//!
//! **A code this build does not register is refused, never defaulted.** That is
//! the same posture the model codec has always had — it decoded a parameter
//! only if the code named itself back — carried across to field identity: an
//! unresolved id means a newer build's field, and painting it through some
//! other field's scale would be a silent misread.

use std::sync::LazyLock;

use squallar_source::product::{FieldId, LegendScale};

/// A whole decoded grid in hand, with **no source's own enum in it**.
///
/// [`GriddedInput::Resident`] carries this by `Arc`, so a source that holds its
/// grid whole and windows at encode — the posture MRMS and HRRR both take —
/// describes a job for the cost of a refcount. Everything
/// [`rasterize_gridded`] needs is here and nothing else is: the raster resolves
/// `field` through [`field_paint`] and refuses what that does not answer, which
/// is why a second gridded source needs no arm in the rasterizer, the codec or
/// the wire.
///
/// [`GriddedInput::Resident`]: crate::render::rasterize::GriddedInput::Resident
/// [`rasterize_gridded`]: crate::render::rasterize::rasterize_gridded
#[derive(Debug, Clone, PartialEq)]
pub struct ResidentGrid {
    /// The field being drawn, as its registering source's own `ProductSpec`
    /// spells it — never a string parsed back from somewhere.
    pub field: FieldId,
    /// Points along a parallel, and along a meridian. `values` is row-major in
    /// these: point `(i, j)` is `values[j * ni + i]`.
    pub ni: usize,
    pub nj: usize,
    pub coords: crate::hrrr::GridCoords,
    pub values: GridValues,
}

/// **How a grid's values are stored — `f32`, or the source's own narrower
/// code.**
///
/// A grid point costs four bytes only when the source really carries four.
/// MRMS does not: its GRIB2 simple packing states a 16-bit code and the value
/// is `(ref_val + code * 2^exp) * 10^-dec`, so an `f32` per point is a
/// *widening of the source's own width* and storing the code back is a
/// **repacking, not a quantisation**
/// (`mrms::decode::tests::every_mosaic_value_is_a_sixteen_bit_code_and_three_scalars`
/// pins that bit for bit over both shipped products and all 24.5 M points).
///
/// **GMGSI's width is a fact about its VALUES, not about its storage.** It is
/// `float` on disk with no `scale_factor`, so there is no declared width to
/// read an arm off — and every value of it is an integer on the unit lattice
/// in `0..=255`, which its own `long_name` says in as many words ("0-255
/// Brightness Temperature"). The `f32` array is therefore a fourfold widening
/// of a byte source exactly as MRMS's was of a 16-bit one, and [`Self::Bytes`]
/// stores it back. The claim is not made about the product: `gmgsi::decode`
/// proves it **per granule**, value by value, on the way past, and a granule
/// that fails is decoded wide. HRRR takes neither arm.
#[derive(Debug, Clone, PartialEq)]
pub enum GridValues {
    /// One `f32` a point — what a source whose values really are floats holds.
    F32(Vec<f32>),
    /// One 16-bit code a point, plus the affine it is read back through.
    Scaled(ScaledU16),
    /// One byte a point, read back as the byte's own value.
    Bytes(ByteCodes),
}

/// The element [`ScaledU16`] stores one of a point.
pub type ScaledCode = u16;
/// The element [`ByteCodes`] stores one of a point.
pub type ByteCode = u8;

/// **Which store an arm is** — the question [`SampleKind::bytes_per_sample`]
/// prices, asked once so the answer is not spelled three times.
///
/// Three types name a storage arm: [`GridValues`] owns one, [`ValuesRef`]
/// borrows one, and `render::jobs::WireValues` is the wire's statement of one.
/// Each carried its **own copy of the widths**, and nothing held the copies
/// equal.
///
/// **A disagreement here is a misread, not an over-charge.** A lend is cut in
/// the store's width by `GriddedJob::resident_payload` and read back in the
/// wire tag's width by `GriddedJob::decode_resident`, so two copies that stop
/// agreeing describe one band at two strides. Measured on this tree with the
/// borrowed copy's scaled arm alone moved to four: the crate compiled and all
/// 992 tests passed, because no test drove those two arithmetics against each
/// other. An interior band is then refused — and a **vertically centred** one,
/// the shape where the lend's range clamp trims an over-long request to exactly
/// the length the far end demands, is *accepted* with every sample read from
/// the wrong rows: 224 of 224 wrong, nothing refused, nothing logged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleKind {
    F32,
    ScaledU16,
    Bytes,
}

impl SampleKind {
    /// **Bytes one stored sample occupies** — the multiplier every byte figure
    /// and every wire length in the tree is built from.
    ///
    /// Read off each arm's **own element** rather than spelled here: a literal
    /// `size_of::<u16>()` would be the same defect one turn later, going on
    /// reading two after the store it describes had moved. That is precisely
    /// how `GLOBAL_GRID_BYTES` came to price four bytes a point for a store
    /// that had narrowed to one while its own `== N` pin stayed green.
    #[inline]
    pub const fn bytes_per_sample(self) -> usize {
        match self {
            Self::F32 => size_of::<f32>(),
            Self::ScaledU16 => ScaledU16::ELEMENT_BYTES,
            Self::Bytes => ByteCodes::ELEMENT_BYTES,
        }
    }
}

// The three widths, pinned APART, so a build failure names WHICH store moved
// rather than only that some total did. A widening is allowed to land; what it
// may not do is land silently, and this is the line that makes a human re-read
// the budgets and the prose around it.
const _: () = assert!(SampleKind::F32.bytes_per_sample() == 4);
const _: () = assert!(SampleKind::ScaledU16.bytes_per_sample() == 2);
const _: () = assert!(SampleKind::Bytes.bytes_per_sample() == 1);

/// **A byte a point, and the points that carry no reading.**
///
/// No affine, deliberately. `value = f32::from(code)` is the whole rule, which
/// is what makes the round trip exact by inspection rather than by argument:
/// `f32::from` is lossless for every `u8`, so a value that entered as a code
/// leaves as the same bit pattern. A source needing a scale over bytes extends
/// this then, with its own proof; inventing the operands now would put a
/// multiply on the sampling path for nobody.
///
/// **Missing is a side list, not a code.** GMGSI declares
/// `_FillValue = -9999` and all 256 codes are used by real data somewhere in
/// the corpus, so no code may be spent as a sentinel. The absent points ride
/// beside the codes instead, as their own indices — sorted, bounded by
/// [`MAX_ABSENT_POINTS`], and empty on every granule the product has actually
/// published (24 real granules, four channels, three dates: `n_fill = 0` on
/// every one; the committed fixture plants exactly one).
///
/// Why indices and not a bit per point. A bitmask is 1,875,000 B against a
/// 15,000,000 B mosaic and covers any number of absent points, but it is a
/// *per-point* fact and the window cut on the wire is strided, so every job
/// would have to repack a window's worth of bits **on the frame thread** where
/// `JobRequest::to_bytes` runs. A bounded index set rides the head instead —
/// the shape [`ScaledU16::nan_codes`] already established — and costs the
/// encoder at most [`MAX_ABSENT_POINTS`] comparisons, so the zero-copy
/// resident lend survives untouched. What that buys is paid for on the other
/// side: a granule with more absent points than the bound takes
/// [`GridValues::F32`] whole, at four times the bytes, priced honestly by
/// [`GridValues::resident_bytes`] and visible in the memory census as the 60 MB
/// it is.
#[derive(Debug, Clone, PartialEq)]
pub struct ByteCodes {
    codes: Vec<ByteCode>,
    absent: Vec<u32>,
}

/// How many absent points the byte arm will carry before it declines.
///
/// A `binary_search` of this many `u32` sits on the sampling path, so it is
/// bounded rather than trusted — the reasoning [`MAX_NAN_CODES`] gives, at a
/// bound six comparisons deep instead of a walk. The observed populations are
/// zero (every real granule) and one (the fixture's plant); a granule with a
/// genuine outage gap has thousands and is not a granule whose missing points
/// are a small reserved set, so it takes [`GridValues::F32`] rather than making
/// every sample pay for it.
pub const MAX_ABSENT_POINTS: usize = 64;

impl ByteCodes {
    /// **Bytes one stored code occupies**, off the field's own element — the
    /// width [`SampleKind::Bytes`] prices this arm at.
    pub const ELEMENT_BYTES: usize = size_of::<ByteCode>();

    /// The byte arm, or `None` when `absent` is not a set this can carry:
    /// longer than [`MAX_ABSENT_POINTS`], not strictly ascending, or naming a
    /// point past the codes.
    ///
    /// Strictly ascending is checked rather than sorted-for: it is what
    /// [`Self::value`]'s `binary_search` needs, and an unsorted list read off a
    /// wire would otherwise answer "present" for a point that is missing —
    /// silently, on some samples and not others.
    pub fn new(codes: Vec<u8>, absent: Vec<u32>) -> Option<Self> {
        if absent.len() > MAX_ABSENT_POINTS {
            return None;
        }
        if !absent.windows(2).all(|w| w[0] < w[1]) {
            return None;
        }
        if absent.last().is_some_and(|&k| k as usize >= codes.len()) {
            return None;
        }
        Some(Self { codes, absent })
    }

    /// The codes, unwidened — what the transport lends and the wire writes.
    #[inline]
    pub fn codes(&self) -> &[u8] {
        &self.codes
    }

    /// The points this grid holds no reading for, as indices into
    /// [`Self::codes`], strictly ascending.
    #[inline]
    pub fn absent(&self) -> &[u32] {
        &self.absent
    }

    /// Give the codes back — the door the staging pool's `give` is behind.
    pub fn into_codes(self) -> Vec<u8> {
        self.codes
    }

    /// The value at a flat index, or `None` past the end.
    ///
    /// `f32::from` widens exactly; the whole cost of the narrow store at a
    /// sample is that call plus a `binary_search` of an **empty** slice on
    /// every granule the product publishes.
    #[inline]
    pub fn get(&self, index: usize) -> Option<f32> {
        let code = *self.codes.get(index)?;
        if self.absent.binary_search(&(index as u32)).is_ok() {
            return Some(f32::NAN);
        }
        Some(f32::from(code))
    }
}

/// **GRIB2 simple packing, kept packed.**
///
/// `value = (ref_val + code * two_pow) * dig_factor`, evaluated in exactly that
/// order and with exactly these operands.
///
/// **`two_pow` and `dig_factor` are stored, not `exp` and `dec`.** They are
/// `2^exp` and `10^-dec` as the decoder computed them, and keeping the operands
/// rather than the exponents is what makes every reader's arithmetic
/// bit-identical *by construction* — including a reader on the far side of the
/// wire, which is a different `powi` on a different target. Recomputing them
/// would put the losslessness claim at the mercy of two implementations
/// agreeing in the last ULP.
#[derive(Debug, Clone, PartialEq)]
pub struct ScaledU16 {
    pub codes: Vec<ScaledCode>,
    pub ref_val: f32,
    /// `2^exp` — section 5's binary scale, pre-raised.
    pub two_pow: f32,
    /// `10^-dec` — section 5's decimal scale, pre-raised and negated.
    pub dig_factor: f32,
    /// **Every code that reads back as `NaN`**, found by an exhaustive scan of
    /// `0..=65535` at decode rather than by re-testing a tolerance per sample.
    ///
    /// The sentinel rule (`mrms::decode::reading`) is a pure function of the
    /// code, so the scan is exact by construction and costs one pass of 65 536
    /// once a granule. Sorted, and short: the shipped products reserve one code
    /// (rate) or two (composite). A packing whose tolerance swallowed more than
    /// [`MAX_NAN_CODES`] is refused the narrow arm entirely — see
    /// [`ScaledU16::new`].
    pub nan_codes: Vec<u16>,
}

/// How many reserved codes the narrow arm will carry before it declines.
///
/// A linear scan of this many `u16` sits on the sampling path, so it is bounded
/// rather than trusted. Both shipped MRMS products need one or two; a packing
/// that needed more than this is not one whose sentinels are a small reserved
/// set, and it takes [`GridValues::F32`] instead of quietly making every sample
/// walk a long list.
pub const MAX_NAN_CODES: usize = 8;

impl ScaledU16 {
    /// **Bytes one stored code occupies**, off the field's own element — the
    /// width [`SampleKind::ScaledU16`] prices this arm at.
    pub const ELEMENT_BYTES: usize = size_of::<ScaledCode>();

    /// The narrow arm, or `None` when this packing does not belong in it.
    ///
    /// `nan_codes` is discovered here rather than supplied: the caller states
    /// the *rule* (`is_nan`, which for MRMS is `reading` against the product's
    /// reserved set) and this walks every code the width can hold. A caller
    /// that passed a list would be stating the same fact twice, and the two
    /// could disagree.
    pub fn new(
        codes: Vec<u16>,
        ref_val: f32,
        two_pow: f32,
        dig_factor: f32,
        is_nan: impl Fn(f32) -> bool,
    ) -> Option<Self> {
        let nan_codes = Self::nan_codes_for(ref_val, two_pow, dig_factor, is_nan)?;
        Some(Self {
            codes,
            ref_val,
            two_pow,
            dig_factor,
            nan_codes,
        })
    }

    /// The reserved-code set for a packing, **before** its values are decoded.
    ///
    /// Split out because it is a function of the packing alone: a decoder can
    /// ask whether this packing may take the narrow arm at all, and only then
    /// spend a mosaic's worth of decoding into a `Vec<u16>` it would otherwise
    /// have to widen again. `None` is "not this arm" — see [`MAX_NAN_CODES`].
    pub fn nan_codes_for(
        ref_val: f32,
        two_pow: f32,
        dig_factor: f32,
        is_nan: impl Fn(f32) -> bool,
    ) -> Option<Vec<u16>> {
        let mut nan_codes = Vec::new();
        for code in 0..=u16::MAX {
            if is_nan((ref_val + f32::from(code) * two_pow) * dig_factor) {
                if nan_codes.len() == MAX_NAN_CODES {
                    return None;
                }
                nan_codes.push(code);
            }
        }
        Some(nan_codes)
    }

    /// One code read back as the value it stands for.
    ///
    /// The arithmetic is `mrms::decode::decode_png_into`'s, operand for operand
    /// and in the same order, which is what makes this exact rather than close.
    #[inline]
    pub fn value(&self, code: u16) -> f32 {
        if self.nan_codes.contains(&code) {
            return f32::NAN;
        }
        (self.ref_val + f32::from(code) * self.two_pow) * self.dig_factor
    }

    #[inline]
    pub fn get(&self, index: usize) -> Option<f32> {
        self.codes.get(index).map(|&code| self.value(code))
    }
}

impl GridValues {
    #[inline]
    pub fn len(&self) -> usize {
        match self {
            Self::F32(v) => v.len(),
            Self::Scaled(s) => s.codes.len(),
            Self::Bytes(b) => b.codes.len(),
        }
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The value at a flat index, or `None` past the end.
    #[inline]
    pub fn get(&self, index: usize) -> Option<f32> {
        match self {
            Self::F32(v) => v.get(index).copied(),
            Self::Scaled(s) => s.get(index),
            Self::Bytes(b) => b.get(index),
        }
    }

    /// Which store this holds — the arm [`SampleKind`] prices.
    #[inline]
    pub fn kind(&self) -> SampleKind {
        match self {
            Self::F32(_) => SampleKind::F32,
            Self::Scaled(_) => SampleKind::ScaledU16,
            Self::Bytes(_) => SampleKind::Bytes,
        }
    }

    /// **Bytes one point occupies** — the multiplier every byte figure on this
    /// grid is built from, and the one the wire's length check derives from.
    ///
    /// Through [`SampleKind`], never restated here: this and the borrowed and
    /// wire spellings must agree, and the only way two of them cannot disagree
    /// is for there to be one of them.
    #[inline]
    pub fn bytes_per_sample(&self) -> usize {
        self.kind().bytes_per_sample()
    }

    /// **What this grid costs resident.** Every byte budget in the tree is
    /// spent against this figure, so it is derived from the stored width rather
    /// than restated: a store that narrowed while this did not would let a
    /// cache hold twice the granules while believing it was at budget.
    #[inline]
    pub fn resident_bytes(&self) -> usize {
        // **Every block, not only the samples.** The byte arm's absent set is
        // a second allocation beside the codes, and a figure that priced the
        // codes alone would let a cache hold more than it believes it does —
        // the same misreading a store that narrowed while this did not would
        // produce. It is at most `MAX_ABSENT_POINTS * 4` = 256 B.
        self.len() * self.bytes_per_sample()
            + match self {
                Self::Bytes(b) => size_of_val(b.absent.as_slice()),
                Self::F32(_) | Self::Scaled(_) => 0,
            }
    }

    /// Every value in order — for the passes that read a whole grid once and
    /// keep no slice.
    ///
    /// **A concrete iterator, deliberately not `Box<dyn Iterator>`.** The
    /// callers walk 24.5 M points (the summary on the fetch path) and 33 × 24.5 M
    /// (the 3D stack's `push`), and a boxed iterator makes every one of those a
    /// virtual call that cannot inline. Being off the frame thread is not a
    /// licence for that: it is a path a user waits on, and "it runs rarely" has
    /// never been an exception in this tree.
    ///
    /// The per-point cost that is left is one **predictable** branch inside
    /// [`GridValuesIter::next`]. Where even that is worth removing, match once
    /// outside the loop instead — [`Self::summarize`] is what that looks like.
    pub fn iter(&self) -> GridValuesIter<'_> {
        match self {
            Self::F32(v) => GridValuesIter::F32(v.iter()),
            Self::Scaled(s) => GridValuesIter::Scaled {
                scaled: s,
                codes: s.codes.iter(),
            },
            // By index, not by code: on this arm a value is a function of
            // *where* it sits — an absent point carries a code like any other
            // and is missing all the same — so a walk over the codes alone
            // could not tell the two apart.
            Self::Bytes(b) => GridValuesIter::Bytes { bytes: b, next: 0 },
        }
    }

    /// [`crate::hrrr::summarize_values_iter`] over this grid, **matching the
    /// storage arm once** rather than once a point.
    ///
    /// The hot one: it is the whole-grid pass `parse_grib2` runs on the fetch
    /// path, 24.5 M points at CONUS. Dispatching here and handing each arm its
    /// own concrete iterator monomorphises the summary twice over two
    /// zero-cost walks, so the inner loop has no storage branch in it at all.
    ///
    /// **The summary body is still the one in `hrrr`**, called twice rather
    /// than written twice: a second copy of "count the painted, track the
    /// range" is exactly the shape that lets two answers drift apart, and this
    /// figure feeds the blank notice a user reads.
    pub fn summarize(&self, paints: impl Fn(f32) -> bool) -> (usize, Option<(f32, f32)>) {
        match self {
            Self::F32(v) => crate::hrrr::summarize_values_iter(v.iter().copied(), paints),
            Self::Scaled(s) => {
                crate::hrrr::summarize_values_iter(s.codes.iter().map(|&c| s.value(c)), paints)
            }
            Self::Bytes(b) => crate::hrrr::summarize_values_iter(
                (0..b.codes.len()).map(|k| b.get(k).unwrap_or(f32::NAN)),
                paints,
            ),
        }
    }

    /// **Widen the whole grid into an `f32` vector.**
    ///
    /// A mosaic's worth of allocation — 98,000,000 B at CONUS — which is the
    /// entire cost the narrow store exists to avoid. **Never on a render, wire
    /// or fetch path**: those read one value at a time through [`Self::get`] or
    /// stream through [`Self::iter`]. It is here for suites that want to
    /// compare a whole grid, and for a consumer that genuinely needs a
    /// contiguous `f32` slice.
    pub fn to_f32(&self) -> Vec<f32> {
        self.iter().collect()
    }

    /// **The whole store as its own bytes** — what the transport lends,
    /// unwidened. `f32` and `u16` are both `Pod`, alignment falls to 1, and
    /// the length is exact, so this is total and copy-free.
    #[inline]
    pub fn stored_bytes(&self) -> &[u8] {
        match self {
            Self::F32(v) => bytemuck::cast_slice(v),
            Self::Scaled(s) => bytemuck::cast_slice(&s.codes),
            // Already bytes. The absent set is **not** here and must not be:
            // this is what the transport lends by range, and a set of grid
            // indices is not a range of samples. It rides the head instead,
            // cut to the window — see `jobs::WireValues`.
            Self::Bytes(b) => &b.codes,
        }
    }

    #[inline]
    pub fn view(&self) -> ValuesRef<'_> {
        match self {
            Self::F32(v) => ValuesRef::F32(v),
            Self::Scaled(s) => ValuesRef::Scaled(s),
            Self::Bytes(b) => ValuesRef::Bytes(b),
        }
    }
}

/// A borrowed [`GridValues`], plus the `f32`-only shape HRRR's own grid is in.
///
/// The raster and the encoders read through this so neither has an arm per
/// source: what varies is the storage width, not who is asking.
#[derive(Debug, Clone, Copy)]
pub enum ValuesRef<'a> {
    F32(&'a [f32]),
    Scaled(&'a ScaledU16),
    Bytes(&'a ByteCodes),
}

impl<'a> ValuesRef<'a> {
    #[inline]
    pub fn len(self) -> usize {
        match self {
            Self::F32(v) => v.len(),
            Self::Scaled(s) => s.codes.len(),
            Self::Bytes(b) => b.codes.len(),
        }
    }

    #[inline]
    pub fn is_empty(self) -> bool {
        self.len() == 0
    }

    #[inline]
    pub fn get(self, index: usize) -> Option<f32> {
        match self {
            Self::F32(v) => v.get(index).copied(),
            Self::Scaled(s) => s.get(index),
            Self::Bytes(b) => b.get(index),
        }
    }

    /// Which store this borrows — the arm [`SampleKind`] prices.
    #[inline]
    pub fn kind(self) -> SampleKind {
        match self {
            Self::F32(_) => SampleKind::F32,
            Self::Scaled(_) => SampleKind::ScaledU16,
            Self::Bytes(_) => SampleKind::Bytes,
        }
    }

    /// **Bytes one borrowed point occupies.** The width the lend's byte range
    /// is cut in — see [`SampleKind`] for what a copy of this that stopped
    /// agreeing with the wire's costs.
    #[inline]
    pub fn bytes_per_sample(self) -> usize {
        self.kind().bytes_per_sample()
    }

    /// The stored bytes of `range`, in the storage's own width — what the
    /// transport lends and what the raw encoder writes, with **no expansion
    /// anywhere**. `None` for a range past the end, which the far end then
    /// refuses as a length mismatch rather than drawing a short band.
    pub fn sample_bytes(self, range: std::ops::Range<usize>) -> Option<&'a [u8]> {
        match self {
            Self::F32(v) => v.get(range).map(bytemuck::cast_slice),
            Self::Scaled(s) => s.codes.get(range).map(bytemuck::cast_slice),
            Self::Bytes(b) => b.codes.get(range),
        }
    }
}

/// The alpha every gridded overlay paints at. HRRR's eleven ramps all use it,
/// and a raster drawn under the radar layer wants to be seen through.
const ALPHA: u8 = 160;

/// How one gridded field is painted.
///
/// The colour and the visibility test are stored side by side because the two
/// are asked at very different rates: the colour once per drawn cell, the
/// visibility once per *grid point* on the fetch path — see
/// [`crate::hrrr::summarize_values`]. A field whose ramp is a plain walk over
/// its own [`LegendScale`] gets both from [`FieldPaint::over_scale`]; one whose
/// ramp is not — every HRRR parameter, for the two reasons in
/// [`register_model_fields`] — supplies its own pair.
pub struct FieldPaint {
    /// The field this paints, borrowed from the registering source's own
    /// `ProductSpec`, so a decoder can hand back the registry's spelling rather
    /// than one it parsed.
    pub id: &'static FieldId,
    /// The colour bar consumers read. **Not necessarily the ramp**: see
    /// [`register_model_fields`].
    pub scale: &'static LegendScale,
    color: Box<dyn Fn(f32) -> [u8; 4] + Send + Sync>,
    visible: Box<dyn Fn(f32) -> bool + Send + Sync>,
}

impl FieldPaint {
    /// The default: paint through the field's own scale with [`color_for`], and
    /// call a value visible exactly when that scale's first stop admits it.
    pub fn over_scale(id: &'static FieldId, scale: &'static LegendScale) -> Self {
        FieldPaint {
            id,
            scale,
            color: Box::new(move |v| color_for(scale, v)),
            visible: Box::new(move |v| paints_over_scale(scale, v)),
        }
    }

    pub fn color_for_value(&self, value: f32) -> [u8; 4] {
        (self.color)(value)
    }

    /// Whether `value` paints anything, answered without building a colour.
    pub fn paints(&self, value: f32) -> bool {
        (self.visible)(value)
    }
}

impl std::fmt::Debug for FieldPaint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FieldPaint")
            .field("id", &self.id)
            .field("stops", &self.scale.thresholds.len())
            .finish_non_exhaustive()
    }
}

/// The generic ramp over a colour bar: transparent below the first stop,
/// interpolated between stops when `is_gradient` and flat-banded when not, and
/// clamped to the last stop's colour above it.
///
/// The NaN guard is load-bearing for the same reason the model's own ramps have
/// one: NaN fails every comparison, so an unguarded missing point would fall
/// through to the top of the scale — see `rasterize/model_nan_tests.rs`.
///
/// `value` must be in the same units the scale's stops are stated in. That is
/// not a free property: the model's scales are stated in *display* units for
/// six of its sixteen parameters while its grids carry raw GRIB2 values, which
/// is one of the two reasons those fields do not use this function.
pub fn color_for(scale: &LegendScale, value: f32) -> [u8; 4] {
    if !value.is_finite() {
        return [0, 0, 0, 0];
    }
    let stops = &scale.thresholds;
    let (Some(&(first_value, _)), Some(&(last_value, last_color))) = (stops.first(), stops.last())
    else {
        return [0, 0, 0, 0];
    };
    if value < first_value {
        return [0, 0, 0, 0];
    }
    if value >= last_value {
        return [last_color[0], last_color[1], last_color[2], ALPHA];
    }
    // `stops` is ascending (`hrrr::fields::tests` and the radar palettes both
    // pin that), so the bracket is a partition point. `k + 1` is in range
    // because `value < last_value` was answered above.
    let k = stops.partition_point(|&(v, _)| v <= value) - 1;
    let (lo_value, lo_color) = stops[k];
    let (hi_value, hi_color) = stops[k + 1];
    if !scale.is_gradient {
        return [lo_color[0], lo_color[1], lo_color[2], ALPHA];
    }
    let t = if hi_value > lo_value {
        (value - lo_value) / (hi_value - lo_value)
    } else {
        0.0
    };
    let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t) as u8;
    [
        mix(lo_color[0], hi_color[0]),
        mix(lo_color[1], hi_color[1]),
        mix(lo_color[2], hi_color[2]),
        ALPHA,
    ]
}

/// [`color_for`]'s visibility test, without building the colour: everything from
/// the first stop up.
pub fn paints_over_scale(scale: &LegendScale, value: f32) -> bool {
    value.is_finite() && scale.thresholds.first().is_some_and(|&(v, _)| value >= v)
}

/// Every gridded field this build can paint, in registration order.
///
/// One `extend` per gridded source. The order is the wire's tie-break for
/// nothing at all — lookup is by `FieldId` — but it is the order the catalogue
/// lists groups in.
static PAINTS: LazyLock<Vec<FieldPaint>> = LazyLock::new(|| {
    let mut paints = register_model_fields();
    paints.extend(register_mrms_fields());
    paints.extend(register_gmgsi_fields());
    paints
});

/// GMGSI's four channels, each through [`FieldPaint::over_scale`].
///
/// The two conditions that function states both hold, and neither is a
/// coincidence of the ramp being grey:
///
/// * the scales are stated in the units the grid carries — 0-255 counts, with
///   no conversion between the value and the bar, because a count is
///   `Quantity::Unitless` and converts to itself;
/// * the ramps fade out below their first stop and clamp above their last,
///   which is exactly [`color_for`]'s posture. The first stop sits at count 0,
///   so the only value that comes out transparent is a `_FillValue` the CF
///   layer already turned into a `NaN`.
fn register_gmgsi_fields() -> Vec<FieldPaint> {
    crate::gmgsi::GmgsiChannel::all()
        .iter()
        .map(|&c| {
            let spec = crate::gmgsi::fields::spec(c);
            FieldPaint::over_scale(&spec.id, spec.scale)
        })
        .collect()
}

/// MRMS's products, each through [`FieldPaint::over_scale`].
///
/// **This is the case that function was written for**, and the two conditions
/// it states both hold here where they do not hold for the model's sixteen:
///
/// * the scales are stated in the units the grid carries — dBZ and mm/h, with
///   no `convert_for_display` between the value and the bar (pinned by
///   `mrms::fields::tests::no_product_converts_for_display`);
/// * the ramp fades out below its first stop and clamps above its last, which
///   is exactly [`color_for`]'s posture.
///
/// It is also why MRMS does not reach for `squallar-radar`'s reflectivity
/// palette: the overlays→radar edge is cut, and `mrms::fields` registers its own
/// bar rather than crossing it.
fn register_mrms_fields() -> Vec<FieldPaint> {
    crate::mrms::MrmsProduct::all()
        .iter()
        .map(|&p| {
            let spec = crate::mrms::fields::spec(p);
            FieldPaint::over_scale(&spec.id, spec.scale)
        })
        .collect()
}

/// The model's sixteen, each keeping its **own** ramp rather than taking
/// [`color_for`] over its registered scale.
///
/// Two properties of those scales make the generic ramp a different picture,
/// and both are the scale's business rather than the ramp's:
///
/// * six parameters state their stops in **display** units (kt, °F, in, mi)
///   while the grid carries raw GRIB2 values, so the generic ramp would compare
///   metres against miles;
/// * the ramps have three different postures outside their stops — CIN, lifted
///   index and visibility are transparent *above* their last stop, temperature
///   is transparent nowhere, and the rest are transparent below their first —
///   and a `LegendScale` states no posture at all.
///
/// Neither is a defect to repair here: the scale is what the *legend* draws, in
/// the units the legend prints. A gridded source whose scale is in its values'
/// own units and whose ramp fades out below its first stop registers with
/// [`FieldPaint::over_scale`] and needs none of this.
fn register_model_fields() -> Vec<FieldPaint> {
    crate::hrrr::ModelParameter::all()
        .iter()
        .map(|&p| {
            let spec = crate::hrrr::fields::spec(p);
            FieldPaint {
                id: &spec.id,
                scale: spec.scale,
                color: Box::new(move |v| p.color_for_value(v)),
                visible: Box::new(move |v| p.paints(v)),
            }
        })
        .collect()
}

/// How `id` is painted, or `None` for a field this build does not register.
pub fn field_paint(id: &FieldId) -> Option<&'static FieldPaint> {
    paint_for_code(id.as_str())
}

/// [`field_paint`] from the bare spelling — the form a decoder has in hand
/// before it is willing to build a `FieldId` it might not honour.
pub fn paint_for_code(code: &str) -> Option<&'static FieldPaint> {
    PAINTS.iter().find(|paint| paint.id.as_str() == code)
}

/// The colour bar `id` is drawn through, or `None` for a field this build does
/// not register.
pub fn field_scale(id: &FieldId) -> Option<&'static LegendScale> {
    field_paint(id).map(|paint| paint.scale)
}

#[cfg(test)]
mod tests;

/// [`GridValues::iter`]'s iterator — one arm per storage width, so both
/// monomorphise.
///
/// `ExactSizeIterator` because the callers zip it against a sized destination
/// (the 3D stack's level slice) and collect it into a pre-sized vector; a
/// length the iterator cannot state is one those callers would have to ask the
/// grid for separately, and a second statement of a length can disagree.
pub enum GridValuesIter<'a> {
    F32(std::slice::Iter<'a, f32>),
    Scaled {
        scaled: &'a ScaledU16,
        codes: std::slice::Iter<'a, u16>,
    },
    Bytes {
        bytes: &'a ByteCodes,
        next: usize,
    },
}

impl Iterator for GridValuesIter<'_> {
    type Item = f32;

    #[inline]
    fn next(&mut self) -> Option<f32> {
        match self {
            Self::F32(values) => values.next().copied(),
            Self::Scaled { scaled, codes } => codes.next().map(|&code| scaled.value(code)),
            Self::Bytes { bytes, next } => {
                let value = bytes.get(*next)?;
                *next += 1;
                Some(value)
            }
        }
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        match self {
            Self::F32(values) => values.size_hint(),
            Self::Scaled { codes, .. } => codes.size_hint(),
            Self::Bytes { bytes, next } => {
                let left = bytes.codes.len() - next.min(&bytes.codes.len());
                (left, Some(left))
            }
        }
    }
}

impl ExactSizeIterator for GridValuesIter<'_> {}
