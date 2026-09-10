//! **A chunked `f32` variable read a band of chunks at a time**, so the whole
//! variable never exists as one buffer.
//!
//! # The constraint this works around
//!
//! `hdf5_pure` 0.44 offers two reads: the whole dataset
//! ([`hdf5_pure::Dataset::read_raw`]) and a window along the **first**
//! dimension ([`hdf5_pure::Dataset::read_raw_rows`]). GMGSI's mosaic is
//! `data(time, yc, xc)` with `time = 1`, so its first dimension has one row and
//! the windowed form degenerates to the whole read — which is why
//! [`crate::h5::Granule::read_unpacked_f32_to`] materialised 60,000,000 B of
//! assembled storage bytes for a variable it then walked one element at a time.
//!
//! What the crate *does* expose is the storage itself: the chunk shape, the
//! filter pipeline, every chunk's address and stored length, and the file
//! bytes. That is enough to do the assembly here, at the granularity the
//! storage is actually in — and the storage is chunked far finer than the
//! variable: GMGSI's `data` is 16 chunks of `1 x 793 x 1322` (4,193,384 B
//! each) making up a 3000 x 5000 raster.
//!
//! # Why a *band* and not a chunk
//!
//! [`crate::cf::UnpackedSink`] takes values in row-major order, and one chunk
//! holds a rectangle. Reconstructing row `j` of the raster needs every chunk in
//! the band of chunk-rows that contains `j` — four of them here — so a band is
//! the smallest unit this can hold and still deliver values in order. Four
//! chunks is 16,773,536 B against the 60,000,000 B whole: the buffers are
//! allocated once and reused for every band, so that figure is the peak, not
//! the total.
//!
//! **Nothing here is a smaller spelling of the same block.** The four band
//! buffers are the only allocation the read makes, they are real blocks the
//! allocator is asked for, and their sum is the honest cost.
//!
//! # Why the values are the same values
//!
//! The stored bytes go through the same two reversals `hdf5_pure` applies, in
//! the same order — inflate (`flate2`, the same decoder and the same version it
//! uses) and unshuffle — and then through the same [`crate::cf::Packing`] the
//! whole-variable path uses. The unshuffle is not materialised: shuffling moves
//! byte `k` of element `i` to plane `k`, so an element is *gathered* from four
//! planes at read time rather than transposed into a second buffer. That is a
//! different place to do the same permutation, not a different permutation.
//! Pinned bit-for-bit against `read_raw` by
//! [`crate::cf::tests::a_band_streamed_read_is_the_whole_read_bit_for_bit`].
//!
//! Anything this cannot reproduce exactly — a filter it does not implement, a
//! chunk that was never written (which decodes to the fill value, not to
//! stored bytes), a layout that is not effectively 2-D, a band that would not
//! actually be smaller than the whole — is refused at plan time and the caller
//! takes the whole read unchanged. There is no partial arm.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::cf::{Packing, UnpackedSink};

/// HDF5 filter id for deflate (`H5Z_FILTER_DEFLATE`).
const FILTER_DEFLATE: u16 = 1;
/// HDF5 filter id for the byte shuffle (`H5Z_FILTER_SHUFFLE`).
const FILTER_SHUFFLE: u16 = 2;

/// The storage width this reads. Only a standard 4-byte IEEE float reaches
/// here; the caller has already established that.
const WIDTH: usize = 4;

static READS: AtomicU64 = AtomicU64::new(0);
static ELEMENTS: AtomicU64 = AtomicU64::new(0);
static BYTES_AVOIDED: AtomicU64 = AtomicU64::new(0);

static PICKED_READS: AtomicU64 = AtomicU64::new(0);
static PICKED_ELEMENTS: AtomicU64 = AtomicU64::new(0);
static PICKED_BYTES_AVOIDED: AtomicU64 = AtomicU64::new(0);

/// Output bytes one inflate step of a picked read may produce at a time.
///
/// The whole point of the picked read is that the chunk never exists, so this
/// is the buffer that stands in for it: the inflate is driven a window at a
/// time and the window is thrown away as soon as the bytes this read wants
/// have been taken out of it. 64 KiB because it is large enough that the
/// per-call overhead is nothing beside the inflating and small enough to be
/// noise beside a granule.
const WINDOW: usize = 64 * 1024;

/// What the band-streamed reads have done, process-wide and always on.
///
/// **A cut that never executes reads identical to one that works**, so this is
/// not diagnostics: it is the evidence that the path fired. `reads` is zero on
/// a build where every variable was refused at plan time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BandStreamTotals {
    /// Variables read band by band rather than whole.
    pub reads: u64,
    /// Elements those reads delivered.
    pub elements: u64,
    /// Whole-variable storage bytes that were never allocated: for each read,
    /// the whole variable's byte length less the band buffers it held.
    pub bytes_avoided: u64,
}

/// [`BandStreamTotals`] as they stand.
pub fn band_stream_totals() -> BandStreamTotals {
    BandStreamTotals {
        reads: READS.load(Ordering::Relaxed),
        elements: ELEMENTS.load(Ordering::Relaxed),
        bytes_avoided: BYTES_AVOIDED.load(Ordering::Relaxed),
    }
}

/// How a chunk's stored bytes turn back into elements.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Reversal {
    /// Stored as elements already.
    Plain,
    /// Byte-shuffled: element `i`'s byte `k` sits at `k * n + i`.
    Shuffled,
    /// Deflated, then read as elements.
    Deflated,
    /// Deflated over shuffled — the pipeline every real granule uses.
    DeflatedShuffled,
}

impl Reversal {
    /// The reversal for a pipeline, or `None` for one this cannot reverse.
    ///
    /// Filters are listed in application (forward) order, so a written
    /// `shuffle` then `deflate` reverses as inflate then unshuffle.
    ///
    /// The shuffle's own `client_data` — which carries an element size — is
    /// deliberately not read: `hdf5_pure` unshuffles at the *datatype's* width
    /// and this has to agree with it or the two reads differ, so the width
    /// comes from the same place its does. Here that is [`WIDTH`], which the
    /// caller has already established is this variable's.
    fn of(filters: &[hdf5_pure::Filter]) -> Option<Self> {
        let ids: Vec<u16> = filters.iter().map(|f| f.id).collect();
        match ids.as_slice() {
            [] => Some(Self::Plain),
            [FILTER_SHUFFLE] => Some(Self::Shuffled),
            [FILTER_DEFLATE] => Some(Self::Deflated),
            [FILTER_SHUFFLE, FILTER_DEFLATE] => Some(Self::DeflatedShuffled),
            _ => None,
        }
    }

    /// The reversal left after `mask` says which filters were skipped for one
    /// chunk. Bit `i` set means forward filter `i` did not run on this chunk.
    fn masked(self, mask: u32) -> Self {
        let skipped = |i: u32| mask >> i & 1 == 1;
        match self {
            Self::Plain => Self::Plain,
            Self::Shuffled => {
                if skipped(0) {
                    Self::Plain
                } else {
                    Self::Shuffled
                }
            }
            Self::Deflated => {
                if skipped(0) {
                    Self::Plain
                } else {
                    Self::Deflated
                }
            }
            Self::DeflatedShuffled => match (skipped(0), skipped(1)) {
                (false, false) => Self::DeflatedShuffled,
                (true, false) => Self::Deflated,
                (false, true) => Self::Shuffled,
                (true, true) => Self::Plain,
            },
        }
    }

    fn inflates(self) -> bool {
        matches!(self, Self::Deflated | Self::DeflatedShuffled)
    }

    fn gathers(self) -> bool {
        matches!(self, Self::Shuffled | Self::DeflatedShuffled)
    }
}

/// One chunk's stored extent in the file.
struct Stored {
    start: usize,
    len: usize,
    mask: u32,
}

/// **A plan to read one variable band by band** — built once, then walked.
///
/// Every refusal is decided here, before a byte is read, so the walk itself has
/// no fallback arm to get wrong.
pub(crate) struct BandPlan {
    /// Rows of the effective 2-D raster.
    nj: usize,
    /// Columns of the effective 2-D raster.
    ni: usize,
    /// Chunk rows.
    cj: usize,
    /// Chunk columns.
    ci: usize,
    /// Chunks across one band: `ni.div_ceil(ci)`.
    across: usize,
    /// Bands: `nj.div_ceil(cj)`.
    bands: usize,
    /// Elements in one whole chunk, padding included: `cj * ci`.
    chunk_elems: usize,
    /// The reversal the pipeline asks for, before any chunk's mask.
    reversal: Reversal,
    /// Every chunk's stored extent, in `band * across + column` order.
    stored: Vec<Stored>,
}

impl BandPlan {
    /// A plan for `ds`, or `None` when the whole read is the right read.
    ///
    /// `shape` is the caller's already-read dimensions, so this does not
    /// re-read them.
    pub(crate) fn build(ds: &hdf5_pure::Dataset, shape: &[u64]) -> Option<Self> {
        let plan = Self::layout(ds, shape)?;
        // A band that is not materially smaller than the variable buys
        // nothing, and a single-chunk variable — GMGSI's `lat` and `lon` are
        // one 60,000,000 B chunk each — is exactly that case. The whole read
        // is simpler and costs the same, so take it. [`PickPlan`] is what
        // reads such a variable for a caller that wants a handful of its
        // elements rather than all of them.
        let whole_bytes = plan.nj.checked_mul(plan.ni)?.checked_mul(WIDTH)?;
        if plan.band_bytes().saturating_mul(2) > whole_bytes {
            return None;
        }
        Some(plan)
    }

    /// The storage as this reads it — shape, chunking, reversal and every
    /// chunk's extent — with no judgement about which read is the right one.
    ///
    /// Shared with [`PickPlan`], which refuses on different grounds: a band
    /// read wants chunks materially smaller than the variable, and a picked
    /// read wants the variable's storage reproducible at all.
    fn layout(ds: &hdf5_pure::Dataset, shape: &[u64]) -> Option<Self> {
        let chunk_shape = ds.chunk_shape().ok()??;
        if chunk_shape.len() != shape.len() || shape.len() < 2 {
            return None;
        }
        // Effectively 2-D: every dimension but the last two is a single
        // element stored one to a chunk. `data(time=1, yc, xc)` is the case
        // this exists for; a real 3-D variable is refused rather than
        // half-handled.
        let lead = shape.len() - 2;
        if shape[..lead].iter().any(|&d| d != 1) || chunk_shape[..lead].iter().any(|&c| c != 1) {
            return None;
        }
        let dim = |v: &[u64], i: usize| usize::try_from(v[i]).ok().filter(|&n| n != 0);
        let nj = dim(shape, lead)?;
        let ni = dim(shape, lead + 1)?;
        let cj = dim(&chunk_shape, lead)?;
        let ci = dim(&chunk_shape, lead + 1)?;

        let reversal = Reversal::of(&ds.filter_pipeline())?;
        let across = ni.div_ceil(ci);
        let bands = nj.div_ceil(cj);
        let chunk_elems = cj.checked_mul(ci)?;
        // Overflow-checked here so neither plan has to: a band is `across`
        // chunks of this and a picked read inflates one of them.
        chunk_elems.checked_mul(WIDTH)?.checked_mul(across)?;

        // Only chunks that were written are enumerated. A variable with an
        // unwritten chunk decodes it to the fill value, which this does not
        // carry, so it is refused whole.
        let chunks = ds.chunks().ok()?;
        if chunks.len() != bands.checked_mul(across)? {
            return None;
        }
        let mut stored: Vec<Option<Stored>> = (0..chunks.len()).map(|_| None).collect();
        for chunk in chunks {
            if chunk.offset.len() != shape.len() || chunk.offset[..lead].iter().any(|&o| o != 0) {
                return None;
            }
            let (oj, oi) = (
                usize::try_from(chunk.offset[lead]).ok()?,
                usize::try_from(chunk.offset[lead + 1]).ok()?,
            );
            if !oj.is_multiple_of(cj) || !oi.is_multiple_of(ci) {
                return None;
            }
            let (bj, bi) = (oj / cj, oi / ci);
            if bj >= bands || bi >= across {
                return None;
            }
            let slot = stored.get_mut(bj * across + bi)?;
            if slot.is_some() {
                return None;
            }
            *slot = Some(Stored {
                start: usize::try_from(chunk.address).ok()?,
                len: usize::try_from(chunk.storage_size).ok()?,
                mask: chunk.filter_mask,
            });
        }
        let stored: Option<Vec<Stored>> = stored.into_iter().collect();

        Some(BandPlan {
            nj,
            ni,
            cj,
            ci,
            across,
            bands,
            chunk_elems,
            reversal,
            stored: stored?,
        })
    }

    /// Elements the plan will deliver — checked against the caller's count
    /// before anything is read.
    pub(crate) fn elements(&self) -> Option<usize> {
        self.nj.checked_mul(self.ni)
    }

    /// Bytes one band holds at once.
    fn band_bytes(&self) -> usize {
        self.chunk_elems * WIDTH * self.across
    }

    /// **Walk the variable, pushing every value into `out` in row-major
    /// order.**
    ///
    /// `file` is the whole file's bytes, which is what the chunk addresses
    /// index. `little` is the storage byte order the caller resolved.
    pub(crate) fn stream_to(
        &self,
        file: &[u8],
        packing: &Packing,
        little: bool,
        name: &str,
        out: &mut impl UnpackedSink,
    ) -> Result<usize, String> {
        let count = self
            .elements()
            .ok_or_else(|| format!("Variable {name} is too large for this platform"))?;
        out.reserve(count)
            .map_err(|e| format!("Variable {name}: {e}"))?;

        let chunk_bytes = self.chunk_elems * WIDTH;
        let mut slots: Vec<Vec<u8>> = Vec::new();
        slots
            .try_reserve_exact(self.across)
            .map_err(|_| format!("Variable {name}: cannot hold {} chunks", self.across))?;
        for _ in 0..self.across {
            let mut slot: Vec<u8> = Vec::new();
            slot.try_reserve_exact(chunk_bytes).map_err(|_| {
                format!("Variable {name}: cannot hold a {chunk_bytes} B chunk of it")
            })?;
            slots.push(slot);
        }
        let mut inflater = flate2::Decompress::new(true);
        // One per band buffer, refilled per band: a chunk's own `filter_mask`
        // can skip a filter the pipeline lists, so which reversal a slot needs
        // is a property of the chunk in it and not of the variable.
        let mut reversals: Vec<Reversal> = Vec::new();
        reversals
            .try_reserve_exact(self.across)
            .map_err(|_| format!("Variable {name}: cannot describe {} chunks", self.across))?;

        for band in 0..self.bands {
            reversals.clear();
            for (column, slot) in slots.iter_mut().enumerate() {
                let stored = &self.stored[band * self.across + column];
                let bytes = file
                    .get(stored.start..stored.start.saturating_add(stored.len))
                    .ok_or_else(|| {
                        format!("Variable {name}: a chunk is stored past the end of the file")
                    })?;
                let reversal = self.reversal.masked(stored.mask);
                slot.clear();
                if reversal.inflates() {
                    inflate_into(&mut inflater, bytes, slot, chunk_bytes)
                        .map_err(|e| format!("Variable {name}: {e}"))?;
                } else {
                    if bytes.len() != chunk_bytes {
                        return Err(format!(
                            "Variable {name}: an unfiltered chunk stores {} B, not {chunk_bytes} B",
                            bytes.len()
                        ));
                    }
                    slot.extend_from_slice(bytes);
                }
                reversals.push(reversal);
            }

            // Row-major across the band: row `j` of the raster is that row of
            // each chunk in turn, left to right, and only as far as the
            // dataset goes — the rest of the last chunk in a band is padding
            // stored at full chunk size, which is not a value.
            let rows = self.cj.min(self.nj - band * self.cj);
            for row in 0..rows {
                for (column, slot) in slots.iter().enumerate() {
                    let first = column * self.ci;
                    let cols = self.ci.min(self.ni - first);
                    let base = row * self.ci;
                    let n = self.chunk_elems;
                    let mut emit = |word: [u8; WIDTH]| {
                        let stored = if little {
                            f32::from_le_bytes(word)
                        } else {
                            f32::from_be_bytes(word)
                        };
                        out.push(
                            packing
                                .apply(f64::from(stored))
                                .map_or(f32::NAN, |v| v as f32),
                        );
                    };
                    if reversals[column].gathers() {
                        // The unshuffle, done as a gather. Byte `k` of every
                        // element sits in plane `k`, so the run of elements
                        // this row wants is the same run of each plane —
                        // sliced once, so the walk over them carries no
                        // per-element bounds check and no second buffer.
                        let plane = |k: usize| &slot[k * n + base..k * n + base + cols];
                        let (p0, p1, p2, p3) = (plane(0), plane(1), plane(2), plane(3));
                        for c in 0..cols {
                            emit([p0[c], p1[c], p2[c], p3[c]]);
                        }
                    } else {
                        for word in slot[base * WIDTH..(base + cols) * WIDTH].chunks_exact(WIDTH) {
                            emit(word.try_into().expect("chunks_exact yields four bytes"));
                        }
                    }
                }
            }
        }

        let whole = count as u64 * WIDTH as u64;
        let avoided = whole.saturating_sub(self.band_bytes() as u64);
        READS.fetch_add(1, Ordering::Relaxed);
        ELEMENTS.fetch_add(count as u64, Ordering::Relaxed);
        BYTES_AVOIDED.fetch_add(avoided, Ordering::Relaxed);
        log::debug!(
            "netcdf band-streamed read: {name} {}x{} in {} bands of {} chunks, {} B held rather \
             than {whole} B; totals {:?}",
            self.nj,
            self.ni,
            self.bands,
            self.across,
            self.band_bytes(),
            band_stream_totals(),
        );
        Ok(count)
    }
}

/// What the picked reads have done, process-wide and always on.
///
/// The same evidence [`BandStreamTotals`] carries, for the same reason: a cut
/// that never fires reads exactly like one that works, and `reads` is zero on
/// a build where every variable was refused at plan time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PickedReadTotals {
    /// Variables read element by picked element rather than whole.
    pub reads: u64,
    /// Elements those reads delivered — the picks, not the variable.
    pub elements: u64,
    /// Whole-variable storage bytes that were never allocated: for each read,
    /// the whole variable's byte length less the window and the gathered
    /// bytes it held.
    pub bytes_avoided: u64,
}

/// [`PickedReadTotals`] as they stand.
pub fn picked_read_totals() -> PickedReadTotals {
    PickedReadTotals {
        reads: PICKED_READS.load(Ordering::Relaxed),
        elements: PICKED_ELEMENTS.load(Ordering::Relaxed),
        bytes_avoided: PICKED_BYTES_AVOIDED.load(Ordering::Relaxed),
    }
}

/// **A plan to read a named handful of a chunked variable's elements** without
/// the variable, or even one of its chunks, ever existing as a buffer.
///
/// # The read this exists for
///
/// GMGSI's `lat` and `lon` are 2-D coordinate arrays stored as **one**
/// 3000 x 5000 chunk each, and a decode wants a few thousand of those
/// 15,000,000 elements: one column of `lat`, one row of `lon`, and the
/// scattered pairs that prove the arrays repeat. [`BandPlan`] cannot help —
/// there is one chunk, so a band is the variable — and reading a window
/// through `hdf5_pure` inflates and unshuffles the whole chunk into its cache:
/// two 60,000,000 B blocks per coordinate variable, four per cold decode,
/// for 64 KB of axis.
///
/// # Why a window is enough
///
/// Inflating is sequential and so is the shuffle's layout — element `i`'s byte
/// `k` sits at `k * n + i`, which for a fixed `k` ascends with `i` — so the
/// bytes a picked read wants can be *taken as they go past*, in one pass, in
/// ascending output order, with nothing behind the window retained. What is
/// held is [`WINDOW`] plus four bytes per pick.
///
/// **The inflating itself is not avoided and is not claimed to be.** The chunk
/// is one deflate stream and its last byte depends on its first, so every byte
/// is still produced; what is removed is the buffer it was produced into, the
/// second buffer it was unshuffled into, and the unpacking of the 15,000,000
/// values nobody asked for.
///
/// # Why the values are the same values
///
/// The same decoder, the same reversals and the same [`Packing`] as
/// [`BandPlan::stream_to`] and [`crate::h5::Granule::read_unpacked_f32_to`],
/// applied to the same stored bytes — the unshuffle is a gather here too, one
/// plane byte at a time instead of one row at a time. Pinned bit-for-bit
/// against the whole read by
/// [`crate::cf::tests::a_picked_read_is_the_whole_read_bit_for_bit`].
pub(crate) struct PickPlan(BandPlan);

impl PickPlan {
    /// A plan for `ds`, or `None` for storage this cannot reproduce exactly.
    ///
    /// Every refusal [`BandPlan::layout`] makes, and none of the ones about
    /// size: a single chunk is the case this is *for*.
    pub(crate) fn build(ds: &hdf5_pure::Dataset, shape: &[u64]) -> Option<Self> {
        Some(Self(BandPlan::layout(ds, shape)?))
    }

    /// Elements the variable holds, so the caller can bound its picks.
    pub(crate) fn elements(&self) -> Option<usize> {
        self.0.elements()
    }

    /// **The elements at `picks`, appended to `out` in `picks` order.**
    ///
    /// `picks` are row-major element indices, strictly ascending and inside
    /// the variable — a caller that cannot say that gets an error rather than
    /// a wrong element. `file` is the whole file's bytes, which is what the
    /// chunk addresses index; `little` is the storage byte order the caller
    /// resolved.
    ///
    /// A chunk no pick falls in is never touched, let alone inflated.
    pub(crate) fn read_picks(
        &self,
        file: &[u8],
        packing: &Packing,
        little: bool,
        name: &str,
        picks: &[usize],
        out: &mut Vec<f32>,
    ) -> Result<(), String> {
        let plan = &self.0;
        let count = plan
            .elements()
            .ok_or_else(|| format!("Variable {name} is too large for this platform"))?;
        let mut previous: Option<usize> = None;
        for &pick in picks {
            if pick >= count {
                return Err(format!(
                    "Variable {name}: element {pick} was asked for and it holds {count}"
                ));
            }
            if previous.is_some_and(|q| pick <= q) {
                return Err(format!(
                    "Variable {name}: picks must ascend, and {pick} does not follow {}",
                    previous.unwrap_or_default()
                ));
            }
            previous = Some(pick);
        }
        if picks.is_empty() {
            return Ok(());
        }

        let chunk_bytes = plan.chunk_elems * WIDTH;
        // The gathered bytes, in `picks` order, four to a pick. Written to by
        // absolute position because a chunk carries a subsequence of the
        // picks, not a run of them.
        let mut words: Vec<u8> = Vec::new();
        let want = picks.len() * WIDTH;
        words
            .try_reserve_exact(want)
            .map_err(|_| format!("Variable {name}: cannot hold {} picks", picks.len()))?;
        words.resize(want, 0);
        let mut window: Vec<u8> = Vec::new();
        window
            .try_reserve_exact(WINDOW)
            .map_err(|_| format!("Variable {name}: cannot hold a {WINDOW} B window of it"))?;
        window.resize(WINDOW, 0);
        let mut inflater = flate2::Decompress::new(true);
        // Which picks the chunk in hand carries: `(pick, element in chunk)`.
        let mut here: Vec<(usize, usize)> = Vec::new();
        let mut taken = 0usize;

        for band in 0..plan.bands {
            let (j0, j1) = (band * plan.cj, ((band + 1) * plan.cj).min(plan.nj));
            for column in 0..plan.across {
                let (i0, i1) = (column * plan.ci, ((column + 1) * plan.ci).min(plan.ni));
                here.clear();
                for (slot, &pick) in picks.iter().enumerate() {
                    let (j, i) = (pick / plan.ni, pick % plan.ni);
                    if j >= j0 && j < j1 && i >= i0 && i < i1 {
                        here.push((slot, (j - j0) * plan.ci + (i - i0)));
                    }
                }
                if here.is_empty() {
                    continue;
                }
                let stored = &plan.stored[band * plan.across + column];
                let bytes = file
                    .get(stored.start..stored.start.saturating_add(stored.len))
                    .ok_or_else(|| {
                        format!("Variable {name}: a chunk is stored past the end of the file")
                    })?;
                let reversal = plan.reversal.masked(stored.mask);
                // Every byte this chunk owes, as `(offset in the chunk's
                // inflated bytes, offset in `words`)`, ascending in the first
                // — shuffled, that is plane by plane; plain, it is the four
                // bytes of each element in turn.
                let n = plan.chunk_elems;
                let owed = here.len() * WIDTH;
                let byte = |t: usize| -> (usize, usize) {
                    if reversal.gathers() {
                        let (k, which) = (t / here.len(), t % here.len());
                        (k * n + here[which].1, here[which].0 * WIDTH + k)
                    } else {
                        let (which, k) = (t / WIDTH, t % WIDTH);
                        (here[which].1 * WIDTH + k, here[which].0 * WIDTH + k)
                    }
                };

                if reversal.inflates() {
                    inflater.reset(true);
                    let (mut produced, mut cursor) = (0usize, 0usize);
                    loop {
                        let consumed = usize::try_from(inflater.total_in()).unwrap_or(usize::MAX);
                        let input = bytes.get(consumed..).unwrap_or(&[]);
                        let before_in = inflater.total_in();
                        let before_out = inflater.total_out();
                        let status = inflater
                            .decompress(input, &mut window, flate2::FlushDecompress::None)
                            .map_err(|e| {
                                format!("Variable {name}: a chunk did not inflate: {e}")
                            })?;
                        let got = usize::try_from(inflater.total_out() - before_out)
                            .unwrap_or(usize::MAX);
                        if produced.saturating_add(got) > chunk_bytes {
                            return Err(format!(
                                "Variable {name}: a chunk inflated past its {chunk_bytes} B chunk \
                                 size"
                            ));
                        }
                        while cursor < owed {
                            let (at, into) = byte(cursor);
                            if at >= produced + got {
                                break;
                            }
                            let offset = at.checked_sub(produced).ok_or_else(|| {
                                format!("Variable {name}: a chunk's bytes arrived out of order")
                            })?;
                            words[into] = window[offset];
                            cursor += 1;
                        }
                        produced += got;
                        if status == flate2::Status::StreamEnd {
                            break;
                        }
                        if got == 0 && inflater.total_in() == before_in {
                            return Err(format!(
                                "Variable {name}: a chunk's stream ended before the chunk was \
                                 complete"
                            ));
                        }
                    }
                    if produced != chunk_bytes {
                        return Err(format!(
                            "Variable {name}: a chunk inflated to {produced} B, not the \
                             {chunk_bytes} B its shape declares"
                        ));
                    }
                    if cursor != owed {
                        return Err(format!(
                            "Variable {name}: a chunk owed {owed} B and gave {cursor}"
                        ));
                    }
                } else {
                    if bytes.len() != chunk_bytes {
                        return Err(format!(
                            "Variable {name}: an unfiltered chunk stores {} B, not {chunk_bytes} B",
                            bytes.len()
                        ));
                    }
                    for t in 0..owed {
                        let (at, into) = byte(t);
                        words[into] = *bytes.get(at).ok_or_else(|| {
                            format!("Variable {name}: a chunk is shorter than its shape declares")
                        })?;
                    }
                }
                taken += here.len();
            }
        }
        if taken != picks.len() {
            return Err(format!(
                "Variable {name}: {taken} of {} picks fell inside a chunk",
                picks.len()
            ));
        }

        out.try_reserve(picks.len())
            .map_err(|_| format!("Variable {name}: cannot hold {} values", picks.len()))?;
        for word in words.chunks_exact(WIDTH) {
            let word: [u8; WIDTH] = word.try_into().expect("chunks_exact yields four bytes");
            let stored = if little {
                f32::from_le_bytes(word)
            } else {
                f32::from_be_bytes(word)
            };
            out.push(
                packing
                    .apply(f64::from(stored))
                    .map_or(f32::NAN, |v| v as f32),
            );
        }

        let whole = count as u64 * WIDTH as u64;
        let held = (WINDOW + words.len()) as u64;
        PICKED_READS.fetch_add(1, Ordering::Relaxed);
        PICKED_ELEMENTS.fetch_add(picks.len() as u64, Ordering::Relaxed);
        PICKED_BYTES_AVOIDED.fetch_add(whole.saturating_sub(held), Ordering::Relaxed);
        log::debug!(
            "netcdf picked read: {name} {}x{}, {} of {count} elements, {held} B held rather than \
             {whole} B; totals {:?}",
            plan.nj,
            plan.ni,
            picks.len(),
            picked_read_totals(),
        );
        Ok(())
    }
}

/// Inflate one chunk into `out`, which must have `expected` bytes of capacity
/// and be empty.
///
/// The loop is `hdf5_pure`'s: grow only when the buffer is genuinely full,
/// refuse output past the chunk's declared size rather than truncating to it,
/// and call a stream that consumes nothing and writes nothing truncated.
/// Resetting on acquisition rather than on release is what makes a chunk that
/// failed halfway leave nothing behind for the next one.
fn inflate_into(
    inflater: &mut flate2::Decompress,
    src: &[u8],
    out: &mut Vec<u8>,
    expected: usize,
) -> Result<(), String> {
    use flate2::{FlushDecompress, Status};

    inflater.reset(true);
    loop {
        let consumed = usize::try_from(inflater.total_in()).unwrap_or(usize::MAX);
        let input = src.get(consumed..).unwrap_or(&[]);
        let before_in = inflater.total_in();
        let before_out = out.len();
        let status = inflater
            .decompress_vec(input, out, FlushDecompress::None)
            .map_err(|e| format!("a chunk did not inflate: {e}"))?;
        if status == Status::StreamEnd {
            break;
        }
        if out.len() == out.capacity() {
            if out.len() >= expected {
                return Err(format!("a chunk inflated past its {expected} B chunk size"));
            }
            out.reserve(expected - out.len());
        } else if inflater.total_in() == before_in && out.len() == before_out {
            return Err("a chunk's stream ended before the chunk was complete".to_string());
        }
    }
    if out.len() != expected {
        return Err(format!(
            "a chunk inflated to {} B, not the {expected} B its shape declares",
            out.len()
        ));
    }
    Ok(())
}
