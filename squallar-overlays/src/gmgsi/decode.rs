//! A GMGSI granule into the gridded substrate.
//!
//! Reads through [`squallar_netcdf`] — the NetCDF4 and CF-convention layer,
//! which knows nothing about satellites — and produces a
//! [`ResidentGrid`] on [`GridCoords::Separable`]. Nothing here re-implements a
//! CF rule; `_FillValue` reaches the raster as a NaN because
//! [`squallar_netcdf::cf`] marked it missing, not because this file compared
//! against `-9999`.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use squallar_geo::GeoBounds;
use squallar_netcdf::StoredFingerprint;
use squallar_source::product::FieldId;

use super::GmgsiChannel;
use crate::hrrr::GridCoords;
use crate::render::gridded::ResidentGrid;

/// A decoded granule: the raster, plus the instant it covers.
#[derive(Debug, Clone, PartialEq)]
pub struct GmgsiGrid {
    pub channel: GmgsiChannel,
    pub grid: ResidentGrid,
    pub bounds: GeoBounds,
    /// `time_coverage_start`, the granule's own stamp.
    pub valid_time: chrono::NaiveDateTime,
}

/// How far apart two coordinates may sit before the grid is refused as
/// non-separable, in degrees.
///
/// The reference granule's deviation is **exactly** zero, so this is not a
/// tolerance the real product needs — it is the width of the claim being
/// checked. A grid that fails it is one whose latitude genuinely varies along a
/// row, which a per-axis representation cannot describe at all.
const SEPARABLE_EPS: f64 = 1e-6;

/// Rows and columns sampled per axis when checking separability.
///
/// A full check is 15,000,000 comparisons per coordinate; this is 10,000. The
/// strides are coprime with the grid's dimensions so the probes do not all land
/// on the same few columns.
const SEPARABLE_PROBE_STRIDE: usize = 97;

/// Decode a granule, **taking its bytes**.
///
/// By value on purpose: the reader needs an owned buffer, a GMGSI body is
/// 7.5 MB, and taking it here means `Granule::from_vec` can adopt the
/// allocation instead of `Granule::open` copying it.
///
/// The raster is decoded into [`super::staging`]'s retained slot; see
/// [`decode_in`] for the shape of the read and what it costs.
pub fn decode(bytes: Vec<u8>, channel: GmgsiChannel) -> Result<GmgsiGrid, String> {
    decode_in(bytes, channel, super::staging::global(), axis_cache())
}

/// [`decode`] against an explicit staging pool and axis cache rather than
/// the process-wide ones.
///
/// **Public so a suite can drive the real decoder over state it owns.** The
/// counters both turn on are process-global on the shipped path, and a
/// filtered run in this workspace is explicitly not self-contained. Every
/// shipped caller goes through [`decode`]; nothing chooses either at runtime.
///
/// # What one decode costs, and in what order
///
/// The two coordinate variables come first and the slot is taken **after**
/// them. `lat` and `lon` are each stored as one 3000 x 5000 chunk, so a read
/// of either used to cost `hdf5_pure` two chunk-sized blocks — it inflates the
/// chunk and unshuffles it into its cache — whatever window was asked for.
/// Reading them first kept a 60 MB slot out of the decode's hand at that
/// moment (measured: 240 MB against 130 MB, one cold decode of the committed
/// granule), and the order is kept now that the blocks are gone.
///
/// In the steady state the coordinate variables are not read at all: every
/// granule of the product stores the same two arrays, and [`AxisCache`]
/// proves it granule by granule from their stored bytes before handing back
/// the axes it derived last time. When they do have to be read — the first
/// granule, or a granule whose stored geometry differs, which the product did
/// change between 2025 and 2026 — the read asks for the elements the axis and
/// its separability probe are actually made of, 4,581 for `lat` and 6,560 for
/// `lon` of 15,000,000 each, through
/// [`read_picked_f32`](squallar_netcdf::Granule::read_picked_f32): it takes
/// their bytes out of the inflating chunk as they go past and holds 64 KiB
/// rather than the chunk. Nothing about the values changes: they are the whole
/// read's, bit for bit. The blocks that read cost, per cold decode:
///
/// | | grid-sized blocks |
/// |---|---|
/// | a window per row block, chunk cache off (`833bad45`) | 88 |
/// | one handle per variable, its chunk cache holding the chunk | 4 |
/// | picked reads (2026-09-10) | **0** |
///
/// Then the raster: `data` lands straight in the slot's buffer through
/// [`read_unpacked_f32_to`](squallar_netcdf::Granule::read_unpacked_f32_to) and
/// [`Narrowing`], one value at a time and at the width the values are — so no
/// fresh raster-sized block is taken past the first granule, and no wide array
/// exists at any point to be narrowed from.
///
/// That read used to leave **one** transient 60,000,000 B block behind it —
/// the stored bytes `hdf5_pure` assembles for `data`, because its public API
/// in 0.44 reads a dataset whole or by first-dimension row window and `data`
/// is `(time=1, yc, xc)`, whose first dimension is one row.
/// [`squallar_netcdf::bandstream`] walks the storage instead: `data` is 16
/// chunks of `1 x 793 x 1322`, and a band of four is what row-major delivery
/// needs held at once. So a steady-state granule's largest transient is
/// 16,773,536 B and **nothing a decode asks for is grid-sized any more**.
/// `tests/gmgsi_staging_blocks.rs` counts the blocks over a mosaic bar — zero
/// — and `tests/gmgsi_band_stream.rs` counts what replaced them.
pub fn decode_in(
    bytes: Vec<u8>,
    channel: GmgsiChannel,
    pool: &super::staging::StagingPool,
    axes: &AxisCache,
) -> Result<GmgsiGrid, String> {
    let granule = squallar_netcdf::Granule::from_vec(bytes)?;

    let shape = granule
        .shape("data")?
        .ok_or_else(|| "GMGSI granule has no `data` variable".to_string())?;
    // `(time, yc, xc)` with a single time step. A granule that ever carried
    // more than one would need a frame axis, not a silently-dropped dimension.
    let (nj, ni) = match shape.as_slice() {
        [1, nj, ni] => (*nj as usize, *ni as usize),
        [nj, ni] => (*nj as usize, *ni as usize),
        other => {
            return Err(format!(
                "GMGSI `data` has shape {other:?}; expected (time=1, yc, xc)"
            ));
        }
    };
    if ni == 0 || nj == 0 {
        return Err(format!("GMGSI `data` is empty at {nj} x {ni}"));
    }
    let points = ni
        .checked_mul(nj)
        .ok_or_else(|| format!("GMGSI `data` at {nj} x {ni} overflows this platform"))?;

    // **Reserve the decode's high-water mark, at the header.** The netCDF
    // header has given `shape("data")` and not one chunk has been inflated.
    //
    // The figure is `points × 5`, and the 5 is the mechanism rather than a
    // margin: `Narrowing` holds the values as one byte a point and widens to
    // `f32` only when a value will not fit a code, and **during that widen
    // both vectors are live** — the byte codes it is reading from and the
    // four-byte floats it is writing to. Reserving the widened arm alone
    // (`× 4`) would under-declare by the buffer being read out of, on exactly
    // the granules that take the expensive path.
    //
    // Guarded: the axis reads, the whole-variable read and the narrowing's own
    // fallible widen all leave by `?`.
    let reserved = squallar_source::reserve::global().take(points.saturating_mul(5) as u64);

    let lat_axis = axes.axis(&granule, "lat", nj, ni, Axis::Row)?;
    let lon_axis = axes.axis(&granule, "lon", nj, ni, Axis::Column)?;
    let bounds = bounds_of(&lat_axis, &lon_axis);
    let valid_time = granule
        .global_str("time_coverage_start")
        .and_then(|s| parse_coverage_start(&s))
        .ok_or_else(|| "GMGSI granule has no readable `time_coverage_start`".to_string())?;

    // Read straight into the raster form, narrowing on the way past. A
    // missing point is NaN, which `render::gridded::color_for` already paints
    // as fully transparent through its non-finite guard; encoding it as any
    // in-domain number would paint it as that number — and on the byte arm
    // every in-domain number is a real reading, which is why the absent points
    // are a list beside the codes and not a code.
    let mut values = Narrowing::new(pool.take(points)?, points);
    if let Err(e) = read_whole(&granule, "data", nj, ni, &mut values) {
        // A refused granule must not cost the slot its buffer.
        pool.give(values.into_codes());
        return Err(e);
    }
    let (values, spare) = values.into_values()?;
    // The truth: what the grid actually came out as, whichever arm it took.
    reserved.settle(values.resident_bytes() as u64);
    if let Some(spare) = spare {
        // The byte buffer the wide arm no longer needs, back to the slot
        // rather than dropped: a granule this build cannot narrow must not
        // also cost the next granule its retained block.
        pool.give(spare);
    }

    Ok(GmgsiGrid {
        channel,
        grid: ResidentGrid {
            field: FieldId::from_static(channel.as_str()),
            ni,
            nj,
            coords: GridCoords::separable(lat_axis, lon_axis),
            values,
        },
        bounds,
        valid_time,
    })
}

/// **The mosaic as it is stored, decided value by value while it arrives.**
///
/// GMGSI is `float` on disk and its values are not floats: every one is an
/// integer on the unit lattice in `0..=255` — measured over 24 real granules
/// (20 study, 4 holdout) on all four channels and three dates, `n_fill = 0`
/// and bit-exact on every one, and the variable's own `long_name` is "0-255
/// Brightness Temperature". So the `f32` array is a fourfold widening of a
/// byte source, and this narrows it back.
///
/// **The claim is proved per granule, not assumed from the corpus.** Every
/// value passes [`code_of`], which asks the only question that matters — does
/// this bit pattern survive a round trip through a byte — and a granule
/// carrying one value that does not, or more absent points than
/// [`MAX_ABSENT_POINTS`], becomes [`GridValues::F32`] whole, from the value
/// that failed onward, in the same single pass. **Nothing is ever quantised
/// and nothing is ever re-read**: the widening walks the codes already taken,
/// which cost 15,000,000 B rather than the 60,000,000 B a read-wide-then-narrow
/// design would have had to allocate before it could ask the question.
///
/// **And the layer below no longer defeats that.** Until 2026-09-09 the claim
/// was true of this type and false of a decode: `read_unpacked_f32_to` fed the
/// values in one at a time but got them out of a 60,000,000 B buffer
/// `hdf5_pure` assembled for the whole variable first, so the block the
/// narrowing exists to remove was allocated anyway, one frame lower.
/// [`squallar_netcdf::bandstream`] reads `data` at the granularity it is
/// stored in, and what a steady-state decode holds is now 15,000,000 B of
/// codes beside one 16,773,536 B band.
struct Narrowing {
    /// The codes so far. After a widening it is the emptied buffer waiting to
    /// go back to the staging pool, so the slot keeps its block either way.
    codes: Vec<u8>,
    /// Indices into `codes` the file marked missing, strictly ascending
    /// because they are pushed in the order they arrive.
    absent: Vec<u32>,
    /// `Some` from the first value that is not a byte code, or from the absent
    /// point past the bound.
    wide: Option<Vec<f32>>,
    /// The element count [`squallar_netcdf::UnpackedSink::reserve`] stated.
    count: usize,
    /// A reservation this could not make. `push` has no error channel, so the
    /// failure is carried to the end of the read rather than aborting inside
    /// the allocator — the reason every reserve in this tree is fallible.
    failed: Option<String>,
}

/// One stored value as the byte that stands for it, or `None` for a value no
/// byte stands for.
///
/// **The round trip decides, never the cast.** `v as u8` is total and
/// saturating — NaN gives 0, -1.0 gives 0, 300.0 gives 255 — so the cast alone
/// would call all three a code; comparing bit patterns after widening back is
/// what makes this exact. `-0.0` is refused for the same reason and by the
/// same line: it is numerically zero but not zero's bit pattern.
#[inline]
fn code_of(v: f32) -> Option<u8> {
    let code = v as u8;
    (f32::from(code).to_bits() == v.to_bits()).then_some(code)
}

impl Narrowing {
    fn new(codes: Vec<u8>, count: usize) -> Self {
        Self {
            codes,
            absent: Vec::new(),
            wide: None,
            count,
            failed: None,
        }
    }

    /// Values stored so far, whichever arm holds them.
    fn len(&self) -> usize {
        match &self.wide {
            Some(values) => values.len(),
            None => self.codes.len(),
        }
    }

    /// Start again from empty.
    ///
    /// One of the three `clear`s a staged buffer passes through — the pool's
    /// on `give` and on `take` are the other two — so "the decode starts from
    /// empty" does not rest on any one of them having run.
    fn clear(&mut self) {
        self.codes.clear();
        self.absent.clear();
        self.wide = None;
        self.failed = None;
    }

    /// Take the wide arm, carrying every code already stored into it.
    ///
    /// The absent points become `NaN` there, which is what they read as on the
    /// byte arm too — one meaning, two representations.
    fn widen(&mut self) {
        let mut values: Vec<f32> = Vec::new();
        if values.try_reserve_exact(self.count).is_err() {
            self.failed = Some(format!(
                "GMGSI `data`: cannot widen {} values to f32 in this build's memory",
                self.count
            ));
            return;
        }
        let mut absent = self.absent.iter().copied().peekable();
        for (k, &code) in self.codes.iter().enumerate() {
            if absent.peek() == Some(&(k as u32)) {
                absent.next();
                values.push(f32::NAN);
            } else {
                values.push(f32::from(code));
            }
        }
        self.codes.clear();
        self.absent.clear();
        self.wide = Some(values);
    }

    /// The store, plus the byte buffer the wide arm left over for the pool.
    fn into_values(self) -> Result<(crate::render::gridded::GridValues, Option<Vec<u8>>), String> {
        use crate::render::gridded::{ByteCodes, GridValues};
        if let Some(e) = self.failed {
            return Err(e);
        }
        match self.wide {
            Some(values) => Ok((GridValues::F32(values), Some(self.codes))),
            None => {
                let absent = self.absent.len();
                let codes = ByteCodes::new(self.codes, self.absent).ok_or_else(|| {
                    format!("GMGSI `data` produced {absent} absent points this store cannot carry")
                })?;
                Ok((GridValues::Bytes(codes), None))
            }
        }
    }

    /// The byte buffer, whatever arm this ended on — the refusal path's way of
    /// returning the slot's block.
    fn into_codes(self) -> Vec<u8> {
        self.codes
    }
}

impl squallar_netcdf::UnpackedSink for Narrowing {
    fn reserve(&mut self, count: usize) -> Result<(), String> {
        // The buffer is the staging slot's and is normally already exactly
        // this long; the reserve is then a no-op that stays fallible for the
        // granule whose shape the slot was not holding.
        //
        // **`try_reserve_exact`, as [`Self::widen`] and the whole MRMS path
        // use.** `try_reserve` is the amortised path: the granule whose count
        // the slot is *not* holding — the one case where this reserve does
        // anything at all — would take `max(2 * capacity, count)` and park a
        // block up to twice the mosaic. On wasm32 that block is permanent,
        // dlmalloc growing linear memory through `memory.grow` and never
        // returning it.
        self.count = count;
        self.codes
            .try_reserve_exact(count)
            .map_err(|_| format!("cannot hold {count} values"))
    }

    #[inline]
    fn push(&mut self, value: f32) {
        if let Some(values) = &mut self.wide {
            values.push(value);
            return;
        }
        if self.failed.is_some() {
            return;
        }
        // A missing point carries a code like any other — there is none to
        // spare, all 256 being real readings somewhere in the product — so
        // what records it is its index.
        if value.is_nan() {
            if self.absent.len() == crate::render::gridded::MAX_ABSENT_POINTS {
                self.widen();
                self.push(value);
                return;
            }
            self.absent.push(self.codes.len() as u32);
            self.codes.push(0);
            return;
        }
        match code_of(value) {
            Some(code) => self.codes.push(code),
            None => {
                self.widen();
                self.push(value);
            }
        }
    }
}

/// Read a whole `nj x ni` variable into `into`, which is emptied first and
/// holds exactly `nj * ni` values afterwards or the read is refused.
///
/// `into` decides the width it stores them at; see [`Narrowing`].
///
/// The count is checked against the shape the caller already established
/// rather than trusted: a variable that declares one shape and delivers
/// another would otherwise be a raster indexed off the end, silently.
fn read_whole(
    granule: &squallar_netcdf::Granule,
    name: &str,
    nj: usize,
    ni: usize,
    into: &mut Narrowing,
) -> Result<(), String> {
    into.clear();
    let read = granule
        .read_unpacked_f32_to(name, into)?
        .ok_or_else(|| format!("GMGSI granule has no `{name}` variable"))?;
    if read != nj * ni || into.len() != nj * ni {
        return Err(format!(
            "GMGSI `{name}` declares {nj} x {ni} but {read} values were read"
        ));
    }
    Ok(())
}

/// **The two axes the last granule's coordinate arrays collapsed to, keyed
/// by the stored bytes that produced them.**
///
/// GMGSI stores `lat` and `lon` as full 2-D arrays on every granule, and the
/// arrays are the same on every granule of the product — the mosaic's grid
/// does not move hour to hour. Reading them is the expensive half of a
/// decode: two 60 MB chunks inflated and unshuffled, 120 MB of transient
/// beside the slot, for two axes that total 64 KB.
///
/// This is not a decision to trust the geometry by shape. The key is a
/// [`StoredFingerprint`] — the variable's stored bytes, chunk by chunk, with
/// its type, chunking, filters and CF attributes — and decoding is a pure
/// function of exactly those, so an equal fingerprint *is* an equal array.
/// A granule whose stored arrays differ in one byte misses and is read and
/// verified as the first one was. ~446 KB of stored bytes are compared per
/// decode — ~74 KB for `lat` and ~373 KB for `lon`; nothing is inflated.
///
/// One entry per variable, `try_lock` only, for the reasons the staging pool
/// gives: the contenders are a live fetch and a frame fetch, contention is
/// rare, and a contended cache simply reads, which is what every decode did
/// before this existed. Injectable for the reason the pool is.
pub struct AxisCache {
    lat: Mutex<Option<(StoredFingerprint, Vec<f64>)>>,
    lon: Mutex<Option<(StoredFingerprint, Vec<f64>)>>,
    /// Axes handed back without a read. Always on, like the pool's totals.
    hits: AtomicUsize,
    /// Axes read and verified off the granule — a first granule, a changed
    /// geometry, a variable that could not be fingerprinted, or a contended
    /// lock.
    misses: AtomicUsize,
}

/// Running totals off [`AxisCache`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AxisCacheTotals {
    pub hits: usize,
    pub misses: usize,
}

impl AxisCache {
    pub const fn new() -> Self {
        Self {
            lat: Mutex::new(None),
            lon: Mutex::new(None),
            hits: AtomicUsize::new(0),
            misses: AtomicUsize::new(0),
        }
    }

    pub fn totals(&self) -> AxisCacheTotals {
        AxisCacheTotals {
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
        }
    }

    fn slot(&self, axis: &Axis) -> &Mutex<Option<(StoredFingerprint, Vec<f64>)>> {
        match axis {
            Axis::Row => &self.lat,
            Axis::Column => &self.lon,
        }
    }

    /// The axis `name` collapses to: remembered if this granule stores the
    /// variable byte for byte as the remembered one did, read and verified
    /// otherwise.
    fn axis(
        &self,
        granule: &squallar_netcdf::Granule,
        name: &str,
        nj: usize,
        ni: usize,
        axis: Axis,
    ) -> Result<Vec<f64>, String> {
        let fingerprint = granule.stored_fingerprint(name)?;
        let slot = self.slot(&axis);
        if let Some(fingerprint) = &fingerprint
            && let Ok(remembered) = slot.try_lock()
            && let Some((known, out)) = remembered.as_ref()
            && known == fingerprint
        {
            // The shape is part of the fingerprint, so an equal one is an
            // axis of the length this granule declares.
            self.hits.fetch_add(1, Ordering::Relaxed);
            return Ok(out.clone());
        }
        self.misses.fetch_add(1, Ordering::Relaxed);
        let out = axis_from_2d(granule, name, nj, ni, axis)?;
        if let Some(fingerprint) = fingerprint
            && let Ok(mut remembered) = slot.try_lock()
        {
            *remembered = Some((fingerprint, out.clone()));
        }
        Ok(out)
    }
}

impl Default for AxisCache {
    fn default() -> Self {
        Self::new()
    }
}

/// The process-wide axis cache — what every shipped decode uses. One for the
/// application, because every pane's granules share one grid.
static AXES: AxisCache = AxisCache::new();

/// See [`AXES`].
pub fn axis_cache() -> &'static AxisCache {
    &AXES
}

enum Axis {
    /// Varies down the rows, constant along each one.
    Row,
    /// Varies along the columns, constant down each one.
    Column,
}

/// Collapse a 2-D `(yc, xc)` coordinate variable to the axis it repeats,
/// refusing the collapse if the variable does not in fact repeat.
///
/// The refusal is the point. A separable representation of a non-separable
/// grid does not misplace one point — it misplaces the whole raster along one
/// dimension, and it does so silently, because every method still answers.
///
/// **What is read is exactly what is compared**: the axis itself and the probe
/// pairs — 4,581 elements for `lat` and 6,560 for `lon`, of a coordinate
/// array's 15,000,000 — taken out of the inflating chunk as it goes past by
/// [`read_picked_f32`](squallar_netcdf::Granule::read_picked_f32). The values
/// are the whole read's, bit for bit — this is a different place to gather the
/// same stored bytes, not a different reading of them, and nothing here
/// interpolates, rounds or infers a coordinate. It cannot: the mosaic's
/// latitude axis is uniform in *Mercator y*, not in degrees — its row spacing
/// runs 0.0214° at the top edge to 0.0720° at the equator — so no affine
/// function of the row index describes it and every row's own stored value is
/// needed. Measured over six real granules on four channels and four dates
/// plus the committed fixture, 2026-09-10: a straight line through the
/// latitude axis is out by up to 10.8°.
fn axis_from_2d(
    granule: &squallar_netcdf::Granule,
    name: &str,
    nj: usize,
    ni: usize,
    axis: Axis,
) -> Result<Vec<f64>, String> {
    let shape = granule
        .shape(name)?
        .ok_or_else(|| format!("GMGSI granule has no `{name}` variable"))?;
    if shape != [nj as u64, ni as u64] {
        return Err(format!(
            "GMGSI `{name}` has shape {shape:?}; expected ({nj}, {ni}) to match `data`",
        ));
    }
    let picks = picks_of(&axis, nj, ni);
    let values = granule
        .read_picked_f32(name, &picks)?
        .ok_or_else(|| format!("GMGSI granule has no `{name}` variable"))?;
    if values.len() != picks.len() {
        return Err(format!(
            "GMGSI `{name}`: {} elements were asked for and {} were read",
            picks.len(),
            values.len()
        ));
    }
    // `picks` ascends, so this is the position of `(j, i)` in what was read.
    let at = |j: usize, i: usize| -> Result<f64, String> {
        let k = picks
            .binary_search(&(j * ni + i))
            .map_err(|_| format!("GMGSI `{name}`: ({j}, {i}) is not among the elements read"))?;
        present(values[k], name, j, i)
    };

    let mut out: Vec<f64> = Vec::with_capacity(match axis {
        Axis::Row => nj,
        Axis::Column => ni,
    });
    match axis {
        // Column 0 of every row.
        Axis::Row => {
            for j in 0..nj {
                out.push(at(j, 0)?);
            }
        }
        // Row 0, and nothing else.
        Axis::Column => {
            for i in 0..ni {
                out.push(at(0, i)?);
            }
        }
    }

    // The probe, in the order each axis has always compared it: the axis's own
    // entry against the same entry of every probe row or column the stride
    // lands on.
    match axis {
        Axis::Row => {
            for k in (0..nj).step_by(SEPARABLE_PROBE_STRIDE) {
                for s in (0..ni).step_by(SEPARABLE_PROBE_STRIDE) {
                    let v = at(k, s)?;
                    if (v - out[k]).abs() > SEPARABLE_EPS {
                        return Err(not_separable(name, k, v, out[k]));
                    }
                }
            }
        }
        Axis::Column => {
            for k in (0..ni).step_by(SEPARABLE_PROBE_STRIDE) {
                for s in (0..nj).step_by(SEPARABLE_PROBE_STRIDE) {
                    let v = at(s, k)?;
                    if (v - out[k]).abs() > SEPARABLE_EPS {
                        return Err(not_separable(name, k, v, out[k]));
                    }
                }
            }
        }
    }
    Ok(out)
}

/// **Every element one axis read needs**, as ascending row-major indices.
///
/// The axis itself — one column, or one row — plus the probe lattice, which is
/// the same lattice for both axes and overlaps the axis at the entries whose
/// index the stride divides. Sorted and deduplicated because that is what a
/// picked read is defined over, and because a probe that read a second copy of
/// an entry could not disagree with the first.
fn picks_of(axis: &Axis, nj: usize, ni: usize) -> Vec<usize> {
    let mut picks: Vec<usize> = Vec::with_capacity(
        match axis {
            Axis::Row => nj,
            Axis::Column => ni,
        } + nj.div_ceil(SEPARABLE_PROBE_STRIDE) * ni.div_ceil(SEPARABLE_PROBE_STRIDE),
    );
    match axis {
        Axis::Row => picks.extend((0..nj).map(|j| j * ni)),
        Axis::Column => picks.extend(0..ni),
    }
    for k in (0..nj).step_by(SEPARABLE_PROBE_STRIDE) {
        for s in (0..ni).step_by(SEPARABLE_PROBE_STRIDE) {
            picks.push(k * ni + s);
        }
    }
    picks.sort_unstable();
    picks.dedup();
    picks
}

/// A coordinate the file marked missing is `NaN` in the raster form, and an
/// axis cannot carry one. `f64::from` is exact, so the axis holds the stored
/// value bit for bit.
fn present(v: f32, name: &str, j: usize, i: usize) -> Result<f64, String> {
    if v.is_nan() {
        return Err(format!("GMGSI `{name}` is missing at ({j}, {i})"));
    }
    Ok(f64::from(v))
}

fn not_separable(name: &str, k: usize, v: f64, on_axis: f64) -> String {
    format!(
        "GMGSI `{name}` is not separable: entry {k} reads {v} off-axis \
         against {on_axis} on it"
    )
}

/// The envelope the two axes span.
fn bounds_of(lat_axis: &[f64], lon_axis: &[f64]) -> GeoBounds {
    let fold = |axis: &[f64]| {
        axis.iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |a, &b| {
                (a.0.min(b), a.1.max(b))
            })
    };
    let (min_lat, max_lat) = fold(lat_axis);
    let (min_lon, max_lon) = fold(lon_axis);
    GeoBounds {
        min_lat,
        max_lat,
        min_lon,
        max_lon,
    }
}

/// `time_coverage_start` is ISO 8601 with a `Z`, e.g. `2025-06-01T12:00:00Z`.
/// The retired legacy granule wrote the same field without the `Z`, so both are
/// accepted rather than one being the parse and the other a failure.
fn parse_coverage_start(s: &str) -> Option<chrono::NaiveDateTime> {
    let trimmed = s.trim().trim_end_matches('Z');
    chrono::NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%dT%H:%M:%S").ok()
}

#[cfg(test)]
mod tests;
