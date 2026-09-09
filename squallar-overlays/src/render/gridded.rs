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
    /// The same 16-bit codes, **tiled**, with a tile that carries one code
    /// carrying it in its index entry and no samples at all. See [`TiledU16`]
    /// for the corpus the shape was chosen against.
    Tiled(TiledU16),
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
    /// The tiled 16-bit store. Its width is one ARENA sample: the store's byte
    /// total is not `points * width` — see [`TiledU16::resident_bytes`] — but
    /// every length on the wire is still a count of samples times this.
    TiledU16,
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
            Self::TiledU16 => TiledU16::ELEMENT_BYTES,
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
const _: () = assert!(SampleKind::TiledU16.bytes_per_sample() == 2);

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

/// **What the absent set costs at its bound**, in bytes — the second
/// allocation [`GridValues::resident_bytes`] counts beside the codes, and the
/// term every byte-arm grid budget has to carry if it is to describe a real
/// granule rather than a point count.
///
/// [`super::handlers::gmgsi::GLOBAL_GRID_BYTES`] was `GRID_POINTS * 1` and so
/// under-stated every real granule by this set: the committed fixture reads
/// 15,000,004 B against a budget of 15,000,000, and `GRID_CACHE_BYTES >= 4 *
/// GLOBAL_GRID_BYTES` was therefore a claim about a constant rather than about
/// four granules the cache actually holds. Under-stating is the direction that
/// silently overruns — `insert` runs out of unpinned victims, takes its
/// `break` arm and holds the entries anyway while the constant says otherwise —
/// and it is the direction an admission door must never take.
///
/// `size_of::<u32>()` restated here would be the same defect one level down, so
/// `the_door_charges_exactly_what_a_handler_at_its_ceiling_holds` pins this against a
/// grid built at the bound rather than against the literal.
pub const MAX_ABSENT_BYTES: usize = MAX_ABSENT_POINTS * size_of::<u32>();

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

/// **Tile side, in points, on both axes.**
///
/// Chosen against the corpus rather than by taste: 28 granules of both shipped
/// products spanning 2021-10-05 to 2026-09-08, priced at 8, 16 and 32 with this
/// same fixed-slot layout. 16 holds the lowest bytes on 24 of the 28 and the
/// lowest mean; 8 is 7 % better on the single densest granule and 1.5 MB worse
/// in index alone on every one; 32 is 20-30 % worse throughout. See
/// [`TiledU16`].
pub const TILE: usize = 16;

/// Points one stored tile holds — the fixed slot width every offset in
/// [`TiledU16`] is a multiple of, and what makes a tile-row band of the arena a
/// contiguous byte range.
pub const TILE_CELLS: usize = TILE * TILE;

/// The index entry's discriminant bit: set means the low 16 bits are the tile's
/// one code, clear means they are its slot in the arena.
///
/// The top bit rather than a side vector because the entry is read on the
/// sampling path: a slot number needs 25 bits at the largest grid this store
/// will hold (24.5 M points is 95,704 tiles) and a code needs 16, so one `u32`
/// carries either with room to spare and the test is a single `and`.
const TILE_UNIFORM: u32 = 1 << 31;

/// **GRIB2 simple packing, kept packed AND kept sparse.**
///
/// The same `(ref_val + code * two_pow) * dig_factor` [`ScaledU16`] evaluates,
/// over the same operands in the same order — this store changes *where a code
/// lives*, never what it decodes to.
///
/// # What it stores
///
/// The grid is cut into [`TILE`]x[`TILE`] tiles, row-major. A tile whose points
/// all carry one code keeps that code in its index entry and **no arena space
/// at all**; any other tile takes one fixed [`TILE_CELLS`]-sample slot. Edge
/// tiles take a whole slot too, padded, so every offset is `slot * TILE_CELLS`
/// and no second table is needed to find one.
///
/// # Why tiles and not the three other shapes
///
/// Measured over 28 granules — both shipped products, 8 dates from 2021-10-05
/// to 2026-09-08, chosen off the bucket's own object sizes so the densest and
/// the emptiest granules of each day are in the set. Drawable points run from
/// **0.83 % to 8.02 %** of the mosaic across it, so a form is judged at its
/// worst granule, not its typical one:
///
/// | form | worst granule | emptiest granule |
/// |---|---|---|
/// | the flat `ScaledU16` | 49,000,000 B | 49,000,000 B |
/// | index + code per drawable point | **97,459,950 B** | 2,735,064 B |
/// | present-bitmap + packed codes | **35,740,558 B** | 4,165,596 B |
/// | run-length along rows | 8,648,472 B | 694,300 B |
/// | **this** | **8,942,280 B** | 2,349,256 B |
///
/// The two sparse forms **lose to the flat store** on the precipitation rate
/// and it is not close: rate `0.0` is a *reading*, not a sentinel, so 66 % of
/// that mosaic's points are ones a sparse form must carry. That is the whole
/// case against choosing on "4 % of points are drawable" — the drawable
/// fraction is not the fraction a representation has to hold.
///
/// Row RLE prices within 3 % of this at the worst granule and beats it on the
/// empty ones. It is not taken for two reasons that are both about the *other*
/// consumers: a point lookup becomes a search of its row's runs (618 runs a row
/// at the worst granule measured, so ~10 dependent loads) where this is two,
/// and it is the raster's per-cell reader as well as hover's; and its worst
/// case is unbounded above the flat store — a checkerboard row costs 4 B a
/// point — where this one cannot exceed the flat store by more than its index,
/// **0.78 %**, whatever arrives.
#[derive(Debug, Clone, PartialEq)]
pub struct TiledU16 {
    /// Points along a parallel — the FULL grid's width. A band cut for the wire
    /// narrows rows, never columns.
    ni: usize,
    /// Rows this store covers, starting at [`Self::origin_j`].
    nj: usize,
    /// The grid row [`Self::nj`] starts at. Zero for a whole grid; a multiple of
    /// [`TILE`] for a band.
    origin_j: usize,
    tiles_i: usize,
    tiles_j: usize,
    /// One entry per tile, row-major over tiles. See [`TILE_UNIFORM`].
    index: Vec<u32>,
    /// The stored tiles, [`TILE_CELLS`] samples each, in tile-scan order — so
    /// the tiles of rows `tj0..tj1` are one unbroken run, which is what lets a
    /// band be LENT rather than copied.
    arena: Vec<ScaledCode>,
    /// Slots stored before each tile row, `tiles_j + 1` entries. The prefix sum
    /// that turns a row band into that run.
    row_slot: Vec<u32>,
    pub ref_val: f32,
    /// `2^exp` — section 5's binary scale, pre-raised.
    pub two_pow: f32,
    /// `10^-dec` — section 5's decimal scale, pre-raised and negated.
    pub dig_factor: f32,
    /// Every code that reads back as `NaN`. [`ScaledU16::nan_codes`] carries the
    /// reasoning; this store holds the same list because it decodes codes the
    /// same way.
    pub nan_codes: Vec<u16>,
}

impl TiledU16 {
    /// **Bytes one stored sample occupies** — the arena's own element, the
    /// width [`SampleKind::TiledU16`] prices and every wire length is built
    /// from.
    pub const ELEMENT_BYTES: usize = size_of::<ScaledCode>();

    /// Tile the whole plane `codes`, which must be `ni * nj` row-major samples.
    ///
    /// `None` for a plane whose length is not the shape beside it — the same
    /// refusal `parse_grib2_raw_in` already makes, restated here because this
    /// is the one place the two are read against each other.
    ///
    /// **A plane is no longer how a shipped mosaic is built.** [`TileBands`]
    /// is, one [`TILE`]-row band at a time out of the PNG decoder's own row
    /// walk, and this is that same builder fed from a plane a caller already
    /// holds. One tiler, not two spellings of it that could come to disagree
    /// about a granule — which is what makes `mrms::decode`'s streamed store
    /// and this one the same bytes by construction rather than by two suites
    /// agreeing.
    pub fn from_plane(
        codes: &[ScaledCode],
        ni: usize,
        nj: usize,
        ref_val: f32,
        two_pow: f32,
        dig_factor: f32,
        nan_codes: Vec<u16>,
    ) -> Option<Self> {
        if ni == 0 || nj == 0 || codes.len() != ni.checked_mul(nj)? {
            return None;
        }
        let mut bands =
            TileBands::new(ni, nj, Vec::new(), ref_val, two_pow, dig_factor, nan_codes)?;
        for row in codes.chunks_exact(ni) {
            bands.fill_row(|dst| dst.copy_from_slice(row))?;
        }
        bands.finish().map(|(tiled, _spent_band)| tiled)
    }

    /// A band's store, built at the far end of the wire from the index rows the
    /// head carried and the arena run the payload lent.
    ///
    /// Every refusal here is a head this build cannot honour rather than a
    /// panic: the length has to be the slots the index names, every non-uniform
    /// entry has to point inside them, and the shape has to be the one the
    /// tile counts describe. A head that fails any of them leaves the pane its
    /// last texture — the posture `ByteCodes::new` already takes.
    #[allow(clippy::too_many_arguments)]
    pub fn from_band(
        ni: usize,
        nj: usize,
        origin_j: usize,
        index: Vec<u32>,
        arena: Vec<ScaledCode>,
        ref_val: f32,
        two_pow: f32,
        dig_factor: f32,
        nan_codes: Vec<u16>,
    ) -> Option<Self> {
        if ni == 0 || nj == 0 {
            return None;
        }
        let tiles_i = ni.div_ceil(TILE);
        let tiles_j = nj.div_ceil(TILE);
        if index.len() != tiles_i.checked_mul(tiles_j)? {
            return None;
        }
        if !arena.len().is_multiple_of(TILE_CELLS) {
            return None;
        }
        let slots = arena.len() / TILE_CELLS;
        let mut stored = 0usize;
        let mut row_slot = Vec::with_capacity(tiles_j + 1);
        for tj in 0..tiles_j {
            row_slot.push(u32::try_from(stored).ok()?);
            for entry in &index[tj * tiles_i..(tj + 1) * tiles_i] {
                if entry & TILE_UNIFORM != 0 {
                    // A uniform entry's payload is a 16-bit code, so nothing
                    // above the low half may be set beside the flag: an entry
                    // that carries more is not one this build wrote.
                    if entry & !(TILE_UNIFORM | 0xffff) != 0 {
                        return None;
                    }
                    continue;
                }
                // Slots are handed out in tile-scan order, so the run is
                // dense and checked as such: an entry naming any slot but the
                // next one describes an arena laid out differently from the
                // one this reads, and a lenient read would sample another
                // tile's rows with nothing to say so.
                if *entry as usize != stored {
                    return None;
                }
                stored += 1;
            }
        }
        row_slot.push(u32::try_from(stored).ok()?);
        if stored != slots {
            return None;
        }
        Some(Self {
            ni,
            nj,
            origin_j,
            tiles_i,
            tiles_j,
            index,
            arena,
            row_slot,
            ref_val,
            two_pow,
            dig_factor,
            nan_codes,
        })
    }

    /// Points along a parallel — the full grid's width.
    #[inline]
    pub fn ni(&self) -> usize {
        self.ni
    }

    /// Rows this store covers.
    #[inline]
    pub fn nj(&self) -> usize {
        self.nj
    }

    /// The grid row this store's first row is.
    #[inline]
    pub fn origin_j(&self) -> usize {
        self.origin_j
    }

    /// The tile index, row-major over tiles.
    #[inline]
    pub fn index(&self) -> &[u32] {
        &self.index
    }

    /// The stored tiles, unwidened — what the transport lends and the wire
    /// writes.
    #[inline]
    pub fn arena(&self) -> &[ScaledCode] {
        &self.arena
    }

    /// **The stored code at `(i, j)` in this store's own row space**, or `None`
    /// outside it.
    ///
    /// Two dependent loads and no branch on a search: the tile's entry, then —
    /// only when the tile is not uniform — the sample. `TILE` is a power of two,
    /// so every division here is a shift.
    #[inline]
    pub fn code_at(&self, i: usize, j: usize) -> Option<ScaledCode> {
        if i >= self.ni || j >= self.nj {
            return None;
        }
        let entry = *self.index.get((j / TILE) * self.tiles_i + i / TILE)?;
        if entry & TILE_UNIFORM != 0 {
            return Some(entry as ScaledCode);
        }
        self.arena
            .get(entry as usize * TILE_CELLS + (j % TILE) * TILE + i % TILE)
            .copied()
    }

    /// One code read back as the value it stands for — [`ScaledU16::value`]'s
    /// body over this store's own operands.
    #[inline]
    pub fn value(&self, code: ScaledCode) -> f32 {
        if self.nan_codes.contains(&code) {
            return f32::NAN;
        }
        (self.ref_val + f32::from(code) * self.two_pow) * self.dig_factor
    }

    /// The value at a flat index in this store's own `ni * nj` space.
    #[inline]
    pub fn get(&self, index: usize) -> Option<f32> {
        if index >= self.len() {
            return None;
        }
        self.code_at(index % self.ni, index / self.ni)
            .map(|c| self.value(c))
    }

    /// The value at a point in the **whole grid's** coordinates, or `None` for
    /// one this store does not cover.
    ///
    /// What a band answers through: a cut store keeps the grid's own numbering
    /// rather than rebasing it, so the raster asks the same question of a whole
    /// grid and of a band.
    #[inline]
    pub fn get_grid(&self, i: usize, j: usize) -> Option<f32> {
        self.code_at(i, j.checked_sub(self.origin_j)?)
            .map(|c| self.value(c))
    }

    /// Points this store covers.
    #[inline]
    pub fn len(&self) -> usize {
        self.ni * self.nj
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// **What this store costs resident**, every block of it — the arena, the
    /// index and the prefix sum. Never `len() * ELEMENT_BYTES`: that is the
    /// figure this store exists to be smaller than.
    #[inline]
    pub fn resident_bytes(&self) -> usize {
        size_of_val(self.arena.as_slice())
            + size_of_val(self.index.as_slice())
            + size_of_val(self.row_slot.as_slice())
            + size_of_val(self.nan_codes.as_slice())
    }

    /// **Whether any point of this store holds `code`.**
    ///
    /// Exact rather than approximate over the padding: a padded cell is filled
    /// with a code its own tile already carries, so a hit inside one is a hit
    /// at a real point of that tile.
    pub fn contains_code(&self, code: ScaledCode) -> bool {
        self.arena.contains(&code)
            || self
                .index
                .iter()
                .any(|&e| e & TILE_UNIFORM != 0 && e as ScaledCode == code)
    }

    /// **The tile rows spanning grid rows `j0..j1`**, as the half-open tile-row
    /// interval and the arena's own sample range for it.
    ///
    /// The range is contiguous because slots are handed out in tile-scan order;
    /// that is the property the zero-copy lend rests on, and
    /// [`Self::from_band`] refuses an arena laid out any other way.
    pub fn band_for(&self, j0: usize, j1: usize) -> Option<(usize, usize, std::ops::Range<usize>)> {
        if j1 <= j0 {
            return None;
        }
        let tj0 = j0.checked_sub(self.origin_j)? / TILE;
        let tj1 = j1
            .saturating_sub(self.origin_j)
            .div_ceil(TILE)
            .min(self.tiles_j);
        if tj1 <= tj0 {
            return None;
        }
        let slot0 = *self.row_slot.get(tj0)? as usize;
        let slot1 = *self.row_slot.get(tj1)? as usize;
        Some((tj0, tj1, slot0 * TILE_CELLS..slot1 * TILE_CELLS))
    }

    /// The index entries of tile rows `tj0..tj1`, with every slot rebased to
    /// the band's own arena — the head's half of what [`Self::from_band`] reads
    /// back.
    pub fn band_index(&self, tj0: usize, tj1: usize) -> Vec<u32> {
        let base = self.row_slot.get(tj0).copied().unwrap_or(0);
        self.index[tj0 * self.tiles_i..tj1 * self.tiles_i]
            .iter()
            .map(|&entry| {
                if entry & TILE_UNIFORM != 0 {
                    entry
                } else {
                    entry - base
                }
            })
            .collect()
    }
}

/// **A [`TiledU16`] built one [`TILE`]-row band at a time, so no whole plane
/// ever exists.**
///
/// The store this yields is byte-for-byte the one [`TiledU16::from_plane`] used
/// to build — that function *is* this builder now — and the difference is
/// entirely in what is live while it is being built. MRMS decodes a
/// 7000 x 3500 mosaic; the plane the tiler read was **49,000,000 B**, parked
/// between granules by `mrms::staging` and, once the store itself had gone
/// tiled, **81 % of what one looping pane held**. What is live here instead is
/// `TILE` rows of it — **224,000 B at that width** — plus the tiles already
/// kept.
///
/// # A tile cannot be decided before its sixteenth row
///
/// That is the whole shape of this type. Uniformity is a property of the
/// tile's 256 points, so a band is *accumulated* and only flushed when it is
/// [`TILE`] rows tall; nothing about a tile is written until the row that
/// completes it arrives. The band buffer is the smallest thing that can hold
/// that decision, and it is one allocation reused across all 219 bands rather
/// than one per band.
///
/// # The last band is short, and that is not a special case
///
/// 3500 rows is 218 whole bands and a **12-row remainder**, and 7000 columns is
/// 437 whole tile columns and an **8-column remainder**. Both are handled the
/// same way [`TiledU16::from_plane`] always handled the column remainder: the
/// tile's own extent is `rows` by `i1 - i0`, uniformity is tested over exactly
/// that, and a kept tile still takes a whole fixed [`TILE_CELLS`] slot with the
/// unreached cells padded with a code the tile already carries. **Fixed slots
/// are load-bearing** — every offset is `slot * TILE_CELLS` and the zero-copy
/// wire lend is built on that arithmetic — so a short band costs the same slot
/// a tall one does and no second table is needed to say which is which.
/// [`Self::finish`] flushes whatever partial band is standing, so the remainder
/// needs no caller to know it is there.
///
/// # Why the arena is the one thing that grows
///
/// `index` and `row_slot` are `tiles_i * tiles_j` and `tiles_j + 1` — known at
/// [`Self::new`] and reserved exactly there. The arena is not: how many tiles
/// are kept is what the *data* says, and the only way to know it before reading
/// the data would be to decode section 7 twice, which at ~235 ms a granule is a
/// worse trade than any allocation on this path.
///
/// So it is reserved **one band's worst case at a time**, exactly —
/// `try_reserve_exact`, fallible for the reason `mrms::decode`'s header gives
/// about a target where an allocation failure aborts without unwinding. Exactly
/// rather than amortised, and that is a measured choice: `try_reserve`'s
/// doubling took the decode's peak live bytes to **15,033,896 B** at the worst
/// granule for an arena of 8,943,164, because the last doubling is most of the
/// block. Asking for what the band can need holds the capacity within one
/// band's worst case of the length — **9,464,328 B peak at that same granule**,
/// a third off — and the decode-time difference between the two policies was
/// inside the run-to-run spread. Reserving before the band rather than before
/// each tile is what keeps an allocation out of the middle of one either way.
///
/// [`Self::finish`] then shrinks the arena to its exact length, because
/// `TiledU16::resident_bytes` prices the slice and a tail the allocator is
/// holding would be a block the census cannot see.
pub struct TileBands {
    ni: usize,
    nj: usize,
    tiles_i: usize,
    tiles_j: usize,
    /// Grid rows accepted so far — the count [`Self::finish`] checks against
    /// `nj`, because a store built from fewer rows than the grid declares is a
    /// mosaic with a hole in it and not a smaller mosaic.
    rows: usize,
    /// The band under construction: up to [`TILE`] whole rows of `ni` samples,
    /// row-major. Handed in and handed back so a caller that pools it — the
    /// shipped decode does — keeps the one allocation across granules.
    band: Vec<ScaledCode>,
    /// [`Self::band`]'s length when it is full, in samples — `min(TILE, nj) *
    /// ni`, which is its capacity. Held rather than recomputed so the compare
    /// in [`Self::fill_row`] cannot be the one place `TILE * ni` overflows a
    /// grid [`Self::new`] already accepted.
    band_points: usize,
    index: Vec<u32>,
    row_slot: Vec<u32>,
    arena: Vec<ScaledCode>,
    slots: usize,
    ref_val: f32,
    two_pow: f32,
    dig_factor: f32,
    nan_codes: Vec<u16>,
}

impl TileBands {
    /// A builder for an `ni` x `nj` grid, taking `band` as its row buffer.
    ///
    /// `band`'s contents are discarded; what it is taken for is its
    /// **allocation**, so a caller holding a spent buffer of the right shape
    /// pays nothing here. `None` for a shape this target cannot address or an
    /// allocation this build cannot serve — never a panic, for the reason
    /// `mrms::decode`'s header gives about a target where nothing unwinds.
    pub fn new(
        ni: usize,
        nj: usize,
        mut band: Vec<ScaledCode>,
        ref_val: f32,
        two_pow: f32,
        dig_factor: f32,
        nan_codes: Vec<u16>,
    ) -> Option<Self> {
        if ni == 0 || nj == 0 {
            return None;
        }
        let tiles_i = ni.div_ceil(TILE);
        let tiles_j = nj.div_ceil(TILE);
        band.clear();
        // `min(TILE, nj)` rather than `TILE`: a grid shorter than one band
        // never fills one, and a test grid should not be handed a buffer sized
        // for a mosaic.
        let band_points = TILE.min(nj).checked_mul(ni)?;
        band.try_reserve_exact(band_points.saturating_sub(band.capacity()))
            .ok()?;
        debug_assert!(band.capacity() >= band_points);
        let mut index: Vec<u32> = Vec::new();
        index
            .try_reserve_exact(tiles_i.checked_mul(tiles_j)?)
            .ok()?;
        let mut row_slot: Vec<u32> = Vec::new();
        row_slot.try_reserve_exact(tiles_j.checked_add(1)?).ok()?;
        let mut arena: Vec<ScaledCode> = Vec::new();
        // One band's worst case up front, so the first band cannot realloc
        // either. Every later band asks for the same again.
        arena
            .try_reserve_exact(tiles_i.checked_mul(TILE_CELLS)?)
            .ok()?;
        Some(Self {
            ni,
            nj,
            tiles_i,
            tiles_j,
            rows: 0,
            band,
            band_points,
            index,
            row_slot,
            arena,
            slots: 0,
            ref_val,
            two_pow,
            dig_factor,
            nan_codes,
        })
    }

    /// **Take the next grid row**, written in place by `f` into the band's own
    /// storage.
    ///
    /// A slice rather than a `&[ScaledCode]` argument so the caller's decode
    /// writes its codes straight into the band and no row is copied twice: the
    /// PNG walk unpacks a 14,000 B row of big-endian samples directly into
    /// this.
    ///
    /// `None` for a row past the `nj` this builder was told about, or for a
    /// band whose flush could not be served — both of which leave the builder
    /// unusable, and neither of which is a store to finish.
    pub fn fill_row(&mut self, f: impl FnOnce(&mut [ScaledCode])) -> Option<()> {
        if self.rows >= self.nj {
            return None;
        }
        let at = self.band.len();
        // Never allocates: `new` reserved `min(TILE, nj) * ni` and a band is
        // flushed and cleared the moment it reaches that.
        self.band.resize(at + self.ni, 0);
        f(&mut self.band[at..]);
        self.rows += 1;
        if self.band.len() == self.band_points {
            self.flush()?;
        }
        Some(())
    }

    /// Tile the standing band and clear it. A no-op on an empty one, which is
    /// what makes [`Self::finish`] able to call it unconditionally.
    fn flush(&mut self) -> Option<()> {
        let rows = self.band.len() / self.ni;
        if rows == 0 {
            return Some(());
        }
        self.row_slot.push(u32::try_from(self.slots).ok()?);
        // This band's worst case, before a tile is written: every tile kept.
        // Fallible, exact, and asked once a band rather than once a tile — so
        // no allocation can land in the middle of a band, and the capacity
        // never runs more than one band ahead of the length.
        self.arena
            .try_reserve_exact(self.tiles_i.checked_mul(TILE_CELLS)?)
            .ok()?;
        let ni = self.ni;
        for ti in 0..self.tiles_i {
            let (i0, i1) = (ti * TILE, ((ti + 1) * TILE).min(ni));
            let first = self.band[i0];
            let uniform = (0..rows).all(|r| {
                self.band[r * ni + i0..r * ni + i1]
                    .iter()
                    .all(|&c| c == first)
            });
            if uniform {
                self.index.push(TILE_UNIFORM | u32::from(first));
                continue;
            }
            self.index.push(u32::try_from(self.slots).ok()?);
            self.slots += 1;
            // The padding is the tile's first code rather than zero: a padded
            // cell is never read back — `code_at` bounds-checks against the
            // grid — and writing a code that is already in the tile keeps the
            // slot's own byte range free of a value the source never
            // published, which is what a wire reader would otherwise see in a
            // hexdump and have to be told to ignore.
            let slot = self.arena.len();
            self.arena.resize(slot + TILE_CELLS, first);
            for r in 0..rows {
                let src = &self.band[r * ni + i0..r * ni + i1];
                let dst = slot + r * TILE;
                self.arena[dst..dst + src.len()].copy_from_slice(src);
            }
        }
        self.band.clear();
        Some(())
    }

    /// The finished store, and the band buffer back for the next granule.
    ///
    /// `None` for a builder that was fed fewer rows than its grid declares —
    /// **refused, not padded**: a mosaic short of its last bands would
    /// otherwise reach the raster as sentinel, which is a picture and not an
    /// error.
    pub fn finish(mut self) -> Option<(TiledU16, Vec<ScaledCode>)> {
        // Whatever partial band is standing — 12 rows at the CONUS shape.
        self.flush()?;
        if self.rows != self.nj {
            return None;
        }
        self.row_slot.push(u32::try_from(self.slots).ok()?);
        if self.index.len() != self.tiles_i.checked_mul(self.tiles_j)?
            || self.row_slot.len() != self.tiles_j + 1
            || self.arena.len() != self.slots.checked_mul(TILE_CELLS)?
        {
            return None;
        }
        // The arena is the one block that grew, so it is the one that can be
        // holding more than it filled — and `TiledU16::resident_bytes` prices
        // the slice, so an unshrunk tail would be a block the census cannot
        // see. Shrinking a `Vec` splits its chunk rather than asking for
        // another, on dlmalloc and on the host allocators alike, so this is
        // the one place a fallible reserve buys nothing.
        self.arena.shrink_to_fit();
        Some((
            TiledU16 {
                ni: self.ni,
                nj: self.nj,
                origin_j: 0,
                tiles_i: self.tiles_i,
                tiles_j: self.tiles_j,
                index: self.index,
                arena: self.arena,
                row_slot: self.row_slot,
                ref_val: self.ref_val,
                two_pow: self.two_pow,
                dig_factor: self.dig_factor,
                nan_codes: self.nan_codes,
            },
            self.band,
        ))
    }
}

impl GridValues {
    #[inline]
    pub fn len(&self) -> usize {
        match self {
            Self::F32(v) => v.len(),
            Self::Scaled(s) => s.codes.len(),
            Self::Bytes(b) => b.codes.len(),
            Self::Tiled(t) => t.len(),
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
            Self::Tiled(t) => t.get(index),
        }
    }

    /// Which store this holds — the arm [`SampleKind`] prices.
    #[inline]
    pub fn kind(&self) -> SampleKind {
        match self {
            Self::F32(_) => SampleKind::F32,
            Self::Scaled(_) => SampleKind::ScaledU16,
            Self::Bytes(_) => SampleKind::Bytes,
            Self::Tiled(_) => SampleKind::TiledU16,
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
        match self {
            // **Not `points * width`.** A tiled store's whole reason to exist
            // is that the two are no longer the same number, and a census that
            // priced it by the point count would report the flat figure for a
            // store holding a fifth of it.
            Self::Tiled(t) => t.resident_bytes(),
            Self::Bytes(b) => {
                self.len() * self.bytes_per_sample() + size_of_val(b.absent.as_slice())
            }
            Self::F32(_) | Self::Scaled(_) => self.len() * self.bytes_per_sample(),
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
            // By index for the same reason the byte arm is: on this store a
            // value is a function of WHERE it sits, because a uniform tile
            // holds one code standing for up to 256 points.
            Self::Tiled(t) => GridValuesIter::Tiled {
                tiled: t,
                i: 0,
                j: 0,
            },
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
            // **By row and column, never by flat index.** `get` splits a flat
            // index with a division and a remainder by a runtime `ni`, and
            // this walks 24.5 M points on the fetch path — the pass
            // `parse_grib2` runs before a granule is handed over. Walking the
            // two coordinates it already has costs neither.
            Self::Tiled(t) => crate::hrrr::summarize_values_iter(
                (0..t.nj()).flat_map(|j| {
                    (0..t.ni()).map(move |i| t.code_at(i, j).map_or(f32::NAN, |c| t.value(c)))
                }),
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
            // The ARENA, which is not a plane: it is meaningless without the
            // index beside it, and the index rides the head. Nothing may read
            // this as `points * width` bytes of samples — `sample_bytes`
            // refuses the flat cut for exactly that reason.
            Self::Tiled(t) => bytemuck::cast_slice(t.arena()),
        }
    }

    #[inline]
    pub fn view(&self) -> ValuesRef<'_> {
        match self {
            Self::F32(v) => ValuesRef::F32(v),
            Self::Scaled(s) => ValuesRef::Scaled(s),
            Self::Bytes(b) => ValuesRef::Bytes(b),
            Self::Tiled(t) => ValuesRef::Tiled(t),
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
    Tiled(&'a TiledU16),
}

impl<'a> ValuesRef<'a> {
    #[inline]
    pub fn len(self) -> usize {
        match self {
            Self::F32(v) => v.len(),
            Self::Scaled(s) => s.codes.len(),
            Self::Bytes(b) => b.codes.len(),
            Self::Tiled(t) => t.len(),
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
            Self::Tiled(t) => t.get(index),
        }
    }

    /// **The value at grid point `(i, j)` of a store held WHOLE**, given the
    /// row stride.
    ///
    /// The flat stores multiply back out to their own flat index; the tiled one
    /// is addressed by tile and offset, and asking it for a flat index would
    /// make it divide the product straight back apart — two divisions by a
    /// runtime stride on the raster's per-cell reader. Measured over the
    /// 28-granule corpus, a row-major window sweep: 2.24 ns a point flat and
    /// 2.61 ns tiled through this door.
    #[inline]
    pub fn grid_value(self, i: usize, j: usize, stride: usize) -> Option<f32> {
        match self {
            Self::Tiled(t) => t.get_grid(i, j),
            Self::F32(_) | Self::Scaled(_) | Self::Bytes(_) => self.get(j * stride + i),
        }
    }

    /// **The value at a point in the WHOLE GRID's coordinates**, given the
    /// window this store was cut to.
    ///
    /// The flat stores are cut column by column, so their own index space is
    /// the window's; the tiled store is cut in whole tile rows and keeps the
    /// grid's numbering. Asking each store where a grid point lives — rather
    /// than computing one flat index for all of them — is what lets a tiled
    /// band be lent uncut while the raster still iterates only the window the
    /// head named.
    #[inline]
    pub fn window_value(
        self,
        i: usize,
        j: usize,
        win: &crate::render::rasterize::IndexWindow,
    ) -> Option<f32> {
        match self {
            Self::Tiled(t) => t.get_grid(i, j),
            Self::F32(_) | Self::Scaled(_) | Self::Bytes(_) => {
                self.get((j - win.j0) * (win.i1 - win.i0) + (i - win.i0))
            }
        }
    }

    /// Which store this borrows — the arm [`SampleKind`] prices.
    #[inline]
    pub fn kind(self) -> SampleKind {
        match self {
            Self::F32(_) => SampleKind::F32,
            Self::Scaled(_) => SampleKind::ScaledU16,
            Self::Bytes(_) => SampleKind::Bytes,
            Self::Tiled(_) => SampleKind::TiledU16,
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
            // **Refused, never approximated.** A flat run of points is not a
            // run of this store's bytes: the arena holds tiles, and a uniform
            // tile holds no samples at all. Every caller of this treats `None`
            // as "write nothing", and the tiled arm's own wire path is taken
            // before any of them is reached.
            Self::Tiled(_) => None,
        }
    }
}

/// Every painted cell's alpha. Opacity is a property of the layer, not of the
/// texel: a raster is painted opaque and the pane's opacity slider dims it at
/// paint time, starting from [`DEFAULT_PLAN_ALPHA`].
const OPAQUE: u8 = 255;

/// **The plan-view default opacity of every gridded overlay** -- the MRMS
/// mosaic, the satellite mosaic and every HRRR field -- as the alpha byte
/// their texels carried before opacity became a layer property. Each of those
/// handlers answers `SourceHandler::default_opacity` with [`DEFAULT_OPACITY`],
/// so a fresh slot reproduces yesterday's look and the user's slider moves
/// from there. `squallar_source::product::REFLECTIVITY_ALPHA` was chosen to
/// equal it (that constant records why), and
/// `a_tilt_and_a_mosaic_paint_the_same_dbz_at_the_same_opacity` holds the two
/// equal.
pub const DEFAULT_PLAN_ALPHA: u8 = 160;

/// The `0..=1` factor `SourceHandler::default_opacity` answers for every
/// gridded layer.
///
/// **A whole percent, deliberately**, not `DEFAULT_PLAN_ALPHA / 255`
/// (0.627451). The slider shows an integer percent, so that value would
/// display 63 % while painting something a user could not return to by hand.
/// It costs one unit of 255 in painted alpha -- `0.63 * 255` rounds to 161
/// where these texels used to carry 160 -- which is below perception and
/// below the ramps' own quantization. Equal to
/// `squallar_source::product::REFLECTIVITY_DEFAULT_OPACITY` for the same
/// reason the byte above equals `REFLECTIVITY_ALPHA`.
pub const DEFAULT_OPACITY: f32 = squallar_source::product::REFLECTIVITY_DEFAULT_OPACITY;

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
        return [last_color[0], last_color[1], last_color[2], OPAQUE];
    }
    // `stops` is ascending (`hrrr::fields::tests` and the radar palettes both
    // pin that), so the bracket is a partition point. `k + 1` is in range
    // because `value < last_value` was answered above.
    let k = stops.partition_point(|&(v, _)| v <= value) - 1;
    let (lo_value, lo_color) = stops[k];
    let (hi_value, hi_color) = stops[k + 1];
    if !scale.is_gradient {
        return [lo_color[0], lo_color[1], lo_color[2], OPAQUE];
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
        OPAQUE,
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
    Tiled {
        tiled: &'a TiledU16,
        /// The next point, as a **coordinate pair** rather than a flat index:
        /// this store is addressed by tile and offset, and a flat index would
        /// be divided straight back apart once a point over a 24.5 M-point
        /// walk.
        i: usize,
        j: usize,
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
            Self::Tiled { tiled, i, j } => {
                let value = tiled.code_at(*i, *j).map(|c| tiled.value(c))?;
                *i += 1;
                if *i == tiled.ni() {
                    *i = 0;
                    *j += 1;
                }
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
            Self::Tiled { tiled, i, j } => {
                let done = (*j * tiled.ni() + *i).min(tiled.len());
                let left = tiled.len() - done;
                (left, Some(left))
            }
        }
    }
}

impl ExactSizeIterator for GridValuesIter<'_> {}
