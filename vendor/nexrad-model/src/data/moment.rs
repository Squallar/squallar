use crate::BinaryData;
use std::fmt::{self, Debug, Formatter};

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

#[cfg(feature = "uom")]
use uom::si::{f64::Length, length::kilometer};

/// Common interface for types that provide gate-level moment data.
///
/// Both [`MomentData`] and [`CFPMomentData`] implement this trait, providing
/// access to shared gate metadata, scale/offset encoding parameters, and raw
/// gate values. Only the decoded value semantics differ between them.
pub trait DataMoment {
    /// The number of gates in this data moment.
    fn gate_count(&self) -> u16;

    /// The range to the center of the first gate in kilometers.
    fn first_gate_range_km(&self) -> f64;

    /// The range to the center of the first gate.
    #[cfg(feature = "uom")]
    fn first_gate_range(&self) -> Length;

    /// The range between the centers of consecutive gates in kilometers.
    fn gate_interval_km(&self) -> f64;

    /// Number of bits per gate (8 or 16).
    fn data_word_size(&self) -> u8;

    /// The range between the centers of consecutive gates.
    #[cfg(feature = "uom")]
    fn gate_interval(&self) -> Length;

    /// The scale factor used to decode raw gate values into floating-point values.
    /// A value of `0.0` means raw values are used directly without scaling.
    fn scale(&self) -> f32;

    /// The offset used to decode raw gate values into floating-point values.
    /// The decoded value is `(raw - offset) / scale`.
    fn offset(&self) -> f32;

    /// The raw encoded gate values as bytes. For 8-bit moments, each byte is one gate.
    /// For 16-bit moments, each pair of bytes is a big-endian `u16` gate value.
    fn raw_values(&self) -> &[u8];

    /// Iterator over raw gate values as `u16`, handling both 8-bit and 16-bit word sizes.
    fn raw_gate_values(&self) -> impl Iterator<Item = u16> + '_;

    /// **LOCAL CHANGE.** The shared allocation holding this moment's gates —
    /// see [`GateBuffer`]. For a walk that must not charge one buffer twice
    /// when two clones of a volume are alive at once.
    fn gate_buffer(&self) -> &GateBuffer;

    /// **LOCAL CHANGE.** Gates present on this moment that carry the
    /// below-threshold sentinel and whose bytes were not stored. Zero unless the
    /// block came from `from_fixed_point_dropping_sentinel_tail`.
    ///
    /// [`Self::raw_gate_values`] already restores them, so a consumer reading
    /// gates needs nothing from this. It is for the two callers that index
    /// `raw_values()` directly and for the byte accounting.
    fn trailing_sentinel_gates(&self) -> u16;

    /// **LOCAL CHANGE.** Gates this moment can answer for: the stored words plus
    /// the restored sentinel tail. **This, not `raw_values().len()`, is the
    /// authority on how many gates there are** — `raw_values()` is the authority
    /// on how many are *stored*.
    fn gates_present(&self) -> usize {
        let step = if self.data_word_size() == 16 { 2 } else { 1 };
        self.raw_values().len() / step + usize::from(self.trailing_sentinel_gates())
    }
}

/// Implements [`DataMoment`] for a wrapper type that stores a `MomentDataBlock` as `self.inner`.
macro_rules! impl_data_moment {
    ($ty:ty) => {
        impl DataMoment for $ty {
            fn gate_count(&self) -> u16 {
                self.inner.gate_count()
            }
            fn first_gate_range_km(&self) -> f64 {
                self.inner.first_gate_range_km()
            }
            #[cfg(feature = "uom")]
            fn first_gate_range(&self) -> Length {
                self.inner.first_gate_range()
            }
            fn gate_interval_km(&self) -> f64 {
                self.inner.gate_interval_km()
            }
            fn data_word_size(&self) -> u8 {
                self.inner.data_word_size()
            }
            #[cfg(feature = "uom")]
            fn gate_interval(&self) -> Length {
                self.inner.gate_interval()
            }
            fn scale(&self) -> f32 {
                self.inner.scale()
            }
            fn offset(&self) -> f32 {
                self.inner.offset()
            }
            fn raw_values(&self) -> &[u8] {
                self.inner.raw_values()
            }
            fn raw_gate_values(&self) -> impl Iterator<Item = u16> + '_ {
                self.inner.raw_gate_values()
            }
            fn gate_buffer(&self) -> &GateBuffer {
                &self.inner.values.0
            }
            fn trailing_sentinel_gates(&self) -> u16 {
                self.inner.trailing_sentinel_gates()
            }
        }
    };
}

/// **LOCAL CHANGE.** Shared storage for one (ray, moment) gate buffer.
///
/// A decoded volume is **95.9 % per-(ray, moment) gate buffers**, one
/// allocator block apiece and ~32,400 of them on a VCP-212-shaped volume.
/// While `MomentDataBlock::values` was a `Vec<u8>`, cloning a `Sweep` — which
/// `squallar_radar::chunks::VolumeAssembler::snapshot` does on every rebuild
/// that is not the volume's last owner, MEASURED at 156 of 156 rebuilds on a
/// 420 s six-site leg — deep-copied every one of them: 30.4 MiB across 18,873
/// blocks per rebuild, on the poller's thread.
///
/// This makes that clone a refcount bump. The buffer is **immutable by
/// construction**: it is written exactly once, by
/// [`MomentDataBlock::from_fixed_point`], and there is no `&mut` path to the
/// bytes anywhere in this crate or out of it — `Radial` hands out
/// `Option<&MomentData>` and nothing else, and the only other writer,
/// [`MomentDataBlock::without_values`], replaces the whole field with
/// [`GateBuffer::empty`]. So sharing cannot change what any reader sees, and
/// `Arc` rather than `Rc` because a volume is built on a runtime worker and
/// read on the frame thread.
///
/// **Not `Arc<[u8]>`**: `Arc<[u8]>::from(Vec<u8>)` copies the bytes into a
/// fresh allocation, which would put a whole volume's memcpy back on the
/// decode path to save 24 bytes a buffer. `Arc<Vec<u8>>` moves the `Vec`'s
/// three words into the new block and the gate bytes never move.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct GateBuffer(std::sync::Arc<Vec<u8>>);

impl GateBuffer {
    /// The shared empty buffer.
    ///
    /// One allocation for the whole process rather than one per released
    /// moment: [`MomentDataBlock::without_values`] is called for every moment
    /// of every radial of a volume being reduced to a skeleton, and an
    /// `Arc::new(Vec::new())` apiece would be tens of thousands of blocks to
    /// represent nothing.
    pub fn empty() -> Self {
        static EMPTY: std::sync::OnceLock<std::sync::Arc<Vec<u8>>> = std::sync::OnceLock::new();
        Self(std::sync::Arc::clone(
            EMPTY.get_or_init(|| std::sync::Arc::new(Vec::new())),
        ))
    }

    /// The gate bytes.
    pub fn as_slice(&self) -> &[u8] {
        self.0.as_slice()
    }

    /// **The identity of the shared allocation**, for a walk that prices a set
    /// of volumes and must not charge one buffer twice because two generations
    /// of a rebuilt volume are alive at once.
    ///
    /// It is an address and it is only ever compared against another address
    /// read while both owners are held — see
    /// `squallar_radar::scan_size::GateBufferSet`, which holds the `Scan`s it
    /// is walking for exactly as long as it holds their ids.
    pub fn id(&self) -> usize {
        std::sync::Arc::as_ptr(&self.0) as usize
    }

    /// Whether these two moments' gates are the same allocation.
    pub fn shares_with(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0)
    }

    /// How many owners this buffer has, this one included.
    pub fn owners(&self) -> usize {
        std::sync::Arc::strong_count(&self.0)
    }
}

impl From<Vec<u8>> for GateBuffer {
    fn from(values: Vec<u8>) -> Self {
        Self(std::sync::Arc::new(values))
    }
}

impl AsRef<[u8]> for GateBuffer {
    fn as_ref(&self) -> &[u8] {
        self.0.as_slice()
    }
}

impl std::ops::Deref for GateBuffer {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        self.0.as_slice()
    }
}

impl Debug for GateBuffer {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("GateBuffer")
            .field("len", &self.0.len())
            .field("owners", &std::sync::Arc::strong_count(&self.0))
            .finish()
    }
}

#[cfg(feature = "serde")]
impl Serialize for GateBuffer {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // The same wire form the `Vec<u8>` this replaced had: `serde` has no
        // `Arc` impls without its own `rc` feature, and enabling that to
        // serialise a byte string would be a wire change for nothing.
        Serialize::serialize(self.0.as_ref(), serializer)
    }
}

#[cfg(feature = "serde")]
impl<'de> Deserialize<'de> for GateBuffer {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self(std::sync::Arc::new(Vec::<u8>::deserialize(
            deserializer,
        )?)))
    }
}

/// CFP status codes for clutter filter power moments.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum CFPStatus {
    /// Clutter filter not applied.
    FilterNotApplied,
    /// Point clutter filter applied.
    PointClutterFilterApplied,
    /// Dual-pol-only filter applied.
    DualPolOnlyFilterApplied,
    /// Reserved CFP status code.
    Reserved(u8),
}

/// Encoded moment data from a radial containing gate metadata and raw values.
///
/// This is an internal type providing gate metadata accessors (count, range, interval) shared
/// by both [`MomentData`] and [`CFPMomentData`]. Use those public wrapper types for decoded
/// gate values and access to gate metadata via the [`DataMoment`] trait.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub(crate) struct MomentDataBlock {
    gate_count: u16,
    first_gate_range: u16,
    gate_interval: u16,
    /// Bits per gate (8 or 16). Dual-pol moments often use 16-bit words.
    data_word_size: u8,
    scale: f32,
    offset: f32,
    values: BinaryData<GateBuffer>,
    /// **LOCAL CHANGE.** Gates that are present in the moment and carry the
    /// below-threshold sentinel, but whose bytes are not stored — dropped as a
    /// trailing run by [`MomentDataBlock::from_fixed_point_dropping_sentinel_tail`].
    ///
    /// Reads restore them: [`MomentDataBlock::raw_gate_values`] emits this many
    /// raw zeroes after the stored bytes, so `iter`, `values` and every
    /// consumer downstream of them see exactly the gates the decoder read.
    ///
    /// **Zero for every other constructor**, which is what keeps "declared
    /// more gates than it carries" (a malformed or synthetic block, whose
    /// unstored gates are *unknown*) distinct from "dropped a tail that was
    /// measured and was sentinel". Those are different facts and only the
    /// second may be restored as zeroes.
    trailing_sentinel_gates: u16,
}

impl MomentDataBlock {
    /// **LOCAL CHANGE.** The same block with its gate buffer released and
    /// every scalar kept: `gate_count`, `first_gate_range`, `gate_interval`,
    /// `data_word_size`, `scale` and `offset` all carry over unchanged, and
    /// only `values` is emptied.
    ///
    /// Added for `squallar_radar::skeleton`, which keeps a volume's structure
    /// resident while releasing the arrays — the gate buffers are 95.9 % of
    /// what a decoded volume costs the allocator. It is written here rather
    /// than rebuilt through the public accessors on purpose: `first_gate_range`
    /// and `gate_interval` are stored as fixed-point `u16` and read back as
    /// `f64` kilometres, so an accessor round trip would not reproduce the
    /// block exactly, and `gate_count` in particular is HASHED by
    /// `squallar_radar::sampler::ladder_fingerprint` — a skeleton that
    /// changed it would silently move a re-cut key.
    ///
    /// No representation change: this constructs the same struct the decoder
    /// does, with one field empty.
    pub(crate) fn without_values(&self) -> Self {
        Self {
            gate_count: self.gate_count,
            first_gate_range: self.first_gate_range,
            gate_interval: self.gate_interval,
            data_word_size: self.data_word_size,
            scale: self.scale,
            offset: self.offset,
            values: BinaryData::from(GateBuffer::empty()),
            // **Zero, deliberately.** A released block must go on yielding
            // NOTHING from `raw_gate_values`, exactly as it did before this
            // field existed: `skeleton`'s four silent readers are written
            // against that. Restoring `gate_count` zeroes here would turn a
            // gate-released volume into a fully below-threshold one, which is
            // the blank-picture-reported-as-success failure that module exists
            // to make unrepresentable.
            trailing_sentinel_gates: 0,
        }
    }

    /// Create new moment data block from fixed-point encoding.
    pub(crate) fn from_fixed_point(
        gate_count: u16,
        first_gate_range: u16,
        gate_interval: u16,
        data_word_size: u8,
        scale: f32,
        offset: f32,
        values: Vec<u8>,
    ) -> Self {
        Self::from_gate_buffer(
            gate_count,
            first_gate_range,
            gate_interval,
            data_word_size,
            scale,
            offset,
            GateBuffer::from(values),
        )
    }

    /// **LOCAL CHANGE.** The same block from a gate buffer that already
    /// exists, so a caller holding one adopts the allocation instead of
    /// copying the gates out of it.
    ///
    /// [`from_fixed_point`](Self::from_fixed_point) is this function with a
    /// `Vec<u8>` moved into a fresh [`GateBuffer`] first; it stays because
    /// every decoder call site is building the bytes and has no buffer to
    /// adopt. This spelling is for the round trip in
    /// `squallar_radar::render_input`, which takes a moment apart into scalars
    /// plus gates and puts it back together, and copied a whole volume's gates
    /// twice to do it.
    pub(crate) fn from_gate_buffer(
        gate_count: u16,
        first_gate_range: u16,
        gate_interval: u16,
        data_word_size: u8,
        scale: f32,
        offset: f32,
        values: GateBuffer,
    ) -> Self {
        debug_assert!(
            data_word_size != 16 || values.as_slice().len() % 2 == 0,
            "16-bit moment data must have an even number of bytes, got {}",
            values.as_slice().len()
        );

        Self {
            gate_count,
            first_gate_range,
            gate_interval,
            data_word_size,
            scale,
            offset,
            values: BinaryData::new(values),
            // A buffer handed over whole has no implied tail. The spelling that
            // carries one is `from_gate_buffer_with_sentinel_tail`, and it is a
            // separate function rather than a defaulted argument because
            // "declared more gates than it carries" and "dropped a measured
            // sentinel tail" are different facts about a block.
            trailing_sentinel_gates: 0,
        }
    }

    /// **LOCAL CHANGE.** [`Self::from_gate_buffer`] for a buffer whose trailing
    /// run of below-threshold gates was dropped rather than stored, carrying the
    /// count needed to restore them.
    ///
    /// **This is what makes the shared-buffer round trip in
    /// `squallar_radar::render_input` compose with the truncation.** That round
    /// trip takes a moment apart into scalars plus a `GateBuffer` and puts it
    /// back together; the buffer is adopted by refcount, so a truncated one
    /// arrives SHORT. Rebuilding it through plain [`Self::from_gate_buffer`]
    /// would set the tail to zero and the reconstructed moment would answer for
    /// `keep` gates instead of `gate_count` — silently, because the code plane
    /// pads to `gate_count()` with the same sentinel and the picture would look
    /// right while the velocity grid and the volumetric status plane saw gates
    /// that were not there.
    pub(crate) fn from_gate_buffer_with_sentinel_tail(
        gate_count: u16,
        first_gate_range: u16,
        gate_interval: u16,
        data_word_size: u8,
        scale: f32,
        offset: f32,
        values: GateBuffer,
        trailing_sentinel_gates: u16,
    ) -> Self {
        Self {
            trailing_sentinel_gates,
            ..Self::from_gate_buffer(
                gate_count,
                first_gate_range,
                gate_interval,
                data_word_size,
                scale,
                offset,
                values,
            )
        }
    }

    /// **LOCAL CHANGE.** [`Self::from_fixed_point`] with the ray's trailing run
    /// of below-threshold gates left unstored.
    ///
    /// # Why
    ///
    /// Gate buffers are 95.9 % of a decoded volume, and a radar ray is mostly
    /// nothing: past the last gate that detected anything, every remaining gate
    /// carries the below-threshold sentinel out to the end of the declared
    /// sweep. MEASURED over 100 archived volumes from 8 sites and 5 VCPs
    /// (`/home/reddragon/.cache/rd-t18-seam-corpus`), that trailing run is
    /// **59.8 % of gate bytes on the precipitation VCP and 73.7 % on clear
    /// air**, and no volume in the corpus was under 41.8 %.
    ///
    /// # Why this is lossless
    ///
    /// The drop is decided on, and restored at, the **raw** word level, before
    /// any decode. `raw_gate_values` re-emits the dropped gates as raw `0`,
    /// which is the byte the decoder would have read, so every consumer —
    /// `iter`, `values`, `field::decode_moment_into`, the velocity grid, the
    /// volumetric status plane — sees the identical sequence. This holds for a
    /// `scale == 0.0` moment too, where raw `0` is an ordinary `Value(0.0)`
    /// rather than a status code: padding is raw, so the decode that follows is
    /// unchanged either way.
    ///
    /// Only a run of raw `0` is dropped. Range-folded gates (raw `1`) are
    /// measurements and are stored; they are 0.06–0.48 % of gate bytes.
    ///
    /// # The caller's obligation
    ///
    /// `values` must be the moment's **complete** gate bytes, as read from the
    /// message — `gate_count` gates of `data_word_size`. This constructor
    /// converts "stored" into "stored plus a known sentinel tail", and that is
    /// only true if nothing was missing on the way in.
    pub(crate) fn from_fixed_point_dropping_sentinel_tail(
        gate_count: u16,
        first_gate_range: u16,
        gate_interval: u16,
        data_word_size: u8,
        scale: f32,
        offset: f32,
        values: Vec<u8>,
    ) -> Self {
        let step = if data_word_size == 16 { 2 } else { 1 };
        let whole = values.len() / step;
        // The last gate that is not an all-zero word. `rposition` over whole
        // words rather than over bytes: a 16-bit gate is sentinel only when
        // BOTH its bytes are zero, and a byte-wise scan would stop on the
        // high byte of a small non-zero value.
        let keep = values
            .chunks_exact(step)
            .rposition(|word| word.iter().any(|&b| b != 0))
            .map_or(0, |last| last + 1);
        let dropped = whole.saturating_sub(keep);
        // Only a tail this constructor can actually restore may be claimed.
        // If the caller handed fewer bytes than `gate_count` declares, the
        // gates beyond them are unknown, not sentinel, and stay that way.
        let Ok(trailing_sentinel_gates) = u16::try_from(dropped) else {
            return Self::from_fixed_point(
                gate_count,
                first_gate_range,
                gate_interval,
                data_word_size,
                scale,
                offset,
                values,
            );
        };
        let mut kept = values;
        kept.truncate(keep * step);
        // `truncate` alone keeps the original capacity, and capacity is what the
        // allocator charged for — the whole point here is to hand `GateBuffer`
        // a block the size of what it holds.
        kept.shrink_to_fit();
        // An all-sentinel ray keeps nothing, and routes to the ONE process-wide
        // empty buffer rather than to a fresh zero-length `Arc`, so
        // `scan_size::gate_bytes_and_blocks`'s `len == 0` arm (0 bytes, 0
        // blocks) tells the truth about it.
        let buffer = if kept.is_empty() {
            GateBuffer::empty()
        } else {
            GateBuffer::from(kept)
        };
        Self::from_gate_buffer_with_sentinel_tail(
            gate_count,
            first_gate_range,
            gate_interval,
            data_word_size,
            scale,
            offset,
            buffer,
            trailing_sentinel_gates,
        )
    }

    /// The number of gates in this data moment.
    fn gate_count(&self) -> u16 {
        self.gate_count
    }

    /// The range to the center of the first gate in kilometers.
    fn first_gate_range_km(&self) -> f64 {
        self.first_gate_range as f64 * 0.001
    }

    /// The range to the center of the first gate.
    #[cfg(feature = "uom")]
    fn first_gate_range(&self) -> Length {
        Length::new::<kilometer>(self.first_gate_range as f64 * 0.001)
    }

    /// The range between the centers of consecutive gates in kilometers.
    fn gate_interval_km(&self) -> f64 {
        self.gate_interval as f64 * 0.001
    }

    /// Number of bits per gate (8 or 16).
    fn data_word_size(&self) -> u8 {
        self.data_word_size
    }

    /// The range between the centers of consecutive gates.
    #[cfg(feature = "uom")]
    fn gate_interval(&self) -> Length {
        Length::new::<kilometer>(self.gate_interval as f64 * 0.001)
    }

    /// The scale factor used to decode raw gate values into floating-point values.
    /// A value of `0.0` means raw values are used directly without scaling.
    fn scale(&self) -> f32 {
        self.scale
    }

    /// The offset used to decode raw gate values into floating-point values.
    /// The decoded value is `(raw - offset) / scale`.
    fn offset(&self) -> f32 {
        self.offset
    }

    /// The raw encoded gate values as bytes. For 8-bit moments, each byte is one gate.
    /// For 16-bit moments, each pair of bytes is a big-endian `u16` gate value.
    fn raw_values(&self) -> &[u8] {
        self.values.0.as_slice()
    }

    /// Iterator over raw gate values as `u16`, handling both 8-bit and 16-bit word sizes.
    ///
    /// **LOCAL CHANGE.** Ends with [`Self::trailing_sentinel_gates`] raw zeroes,
    /// restoring a below-threshold tail that
    /// [`Self::from_fixed_point_dropping_sentinel_tail`] chose not to store. The
    /// count is zero for every other constructor, so this is the identity
    /// iterator everywhere else — including for a gate-released block, which
    /// goes on yielding nothing.
    fn raw_gate_values(&self) -> impl Iterator<Item = u16> + '_ {
        let is_16bit = self.data_word_size == 16;
        let step = if is_16bit { 2 } else { 1 };
        self.values
            .0
            .as_slice()
            .chunks_exact(step)
            .map(move |chunk| {
                if is_16bit {
                    u16::from_be_bytes([chunk[0], chunk[1]])
                } else {
                    chunk[0] as u16
                }
            })
            .chain(std::iter::repeat_n(
                0u16,
                usize::from(self.trailing_sentinel_gates),
            ))
    }

    /// **LOCAL CHANGE.** See the field.
    fn trailing_sentinel_gates(&self) -> u16 {
        self.trailing_sentinel_gates
    }
}

/// Moment data from a radial for a particular product where each value corresponds to a gate.
///
/// Gate metadata (count, range, interval) is available through the [`DataMoment`] trait —
/// see [`gate_count`](DataMoment::gate_count), [`first_gate_range_km`](DataMoment::first_gate_range_km),
/// and [`gate_interval_km`](DataMoment::gate_interval_km).
/// Use [`values`](MomentData::values) to decode gates with standard moment semantics
/// (below threshold, range folded, or numeric value).
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(transparent))]
pub struct MomentData {
    inner: MomentDataBlock,
}

impl MomentData {
    /// **LOCAL CHANGE.** This moment with its gate buffer released and every
    /// scalar kept — see [`MomentDataBlock::without_values`].
    pub fn without_values(&self) -> Self {
        Self {
            inner: self.inner.without_values(),
        }
    }

    /// **LOCAL CHANGE.** The same moment from a gate buffer that already
    /// exists — see [`MomentDataBlock::from_gate_buffer`]. The gates are
    /// adopted by reference count; nothing is copied.
    pub fn from_gate_buffer(
        gate_count: u16,
        first_gate_range: u16,
        gate_interval: u16,
        data_word_size: u8,
        scale: f32,
        offset: f32,
        values: GateBuffer,
    ) -> Self {
        Self {
            inner: MomentDataBlock::from_gate_buffer(
                gate_count,
                first_gate_range,
                gate_interval,
                data_word_size,
                scale,
                offset,
                values,
            ),
        }
    }

    /// **LOCAL CHANGE.** [`Self::from_gate_buffer`] for a buffer whose trailing
    /// below-threshold run was dropped — see
    /// [`MomentDataBlock::from_gate_buffer_with_sentinel_tail`]. Carrying the
    /// count is what lets a truncated moment survive a round trip that shares
    /// the buffer instead of copying it.
    pub fn from_gate_buffer_with_sentinel_tail(
        gate_count: u16,
        first_gate_range: u16,
        gate_interval: u16,
        data_word_size: u8,
        scale: f32,
        offset: f32,
        values: GateBuffer,
        trailing_sentinel_gates: u16,
    ) -> Self {
        Self {
            inner: MomentDataBlock::from_gate_buffer_with_sentinel_tail(
                gate_count,
                first_gate_range,
                gate_interval,
                data_word_size,
                scale,
                offset,
                values,
                trailing_sentinel_gates,
            ),
        }
    }

    /// Create new moment data from fixed-point encoding.
    pub fn from_fixed_point(
        gate_count: u16,
        first_gate_range: u16,
        gate_interval: u16,
        data_word_size: u8,
        scale: f32,
        offset: f32,
        values: Vec<u8>,
    ) -> Self {
        Self {
            inner: MomentDataBlock::from_fixed_point(
                gate_count,
                first_gate_range,
                gate_interval,
                data_word_size,
                scale,
                offset,
                values,
            ),
        }
    }

    /// **LOCAL CHANGE.** [`Self::from_fixed_point`] with the ray's trailing run of
    /// below-threshold gates left unstored — see
    /// [`MomentDataBlock::from_fixed_point_dropping_sentinel_tail`] for what it
    /// costs, what it saves and why it is lossless. `values` must be the
    /// moment's complete gate bytes.
    pub fn from_fixed_point_dropping_sentinel_tail(
        gate_count: u16,
        first_gate_range: u16,
        gate_interval: u16,
        data_word_size: u8,
        scale: f32,
        offset: f32,
        values: Vec<u8>,
    ) -> Self {
        Self {
            inner: MomentDataBlock::from_fixed_point_dropping_sentinel_tail(
                gate_count,
                first_gate_range,
                gate_interval,
                data_word_size,
                scale,
                offset,
                values,
            ),
        }
    }

    /// Iterator over decoded gate values without allocating.
    pub fn iter(&self) -> impl Iterator<Item = MomentValue> + '_ {
        let scale = self.inner.scale;
        let offset = self.inner.offset;

        self.inner.raw_gate_values().map(move |raw_value| {
            // scale == 0.0 is an exact comparison; the value comes from a binary format
            // where IEEE 754 zero is stored literally.
            if scale == 0.0 {
                return MomentValue::Value(raw_value as f32);
            }

            match raw_value {
                0 => MomentValue::BelowThreshold,
                1 => MomentValue::RangeFolded,
                _ => MomentValue::Value((raw_value as f32 - offset) / scale),
            }
        })
    }

    /// Decoded gate values collected into a `Vec`. Prefer [`iter`](Self::iter) when
    /// processing values sequentially to avoid allocation.
    pub fn values(&self) -> Vec<MomentValue> {
        self.iter().collect()
    }
}

impl_data_moment!(MomentData);

/// The data moment value for a product in a radial's gate. The value may be a floating-point number
/// or a special case such as "below threshold" or "range folded".
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum MomentValue {
    /// The data moment value for a gate.
    Value(f32),
    /// The value for this gate was below the signal threshold.
    BelowThreshold,
    /// The value for this gate exceeded the maximum unambiguous range.
    RangeFolded,
}

/// The validity status of a gate value in a field.
///
/// Used alongside `f32` values in field types ([`SweepField`](crate::data::SweepField),
/// [`CartesianField`](crate::data::CartesianField), [`VerticalField`](crate::data::VerticalField))
/// to indicate whether each gate contains a valid measurement or a special condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum GateStatus {
    /// The gate contains a valid floating-point value.
    Valid,
    /// The signal was below the detection threshold.
    BelowThreshold,
    /// The velocity was range-folded (aliased).
    RangeFolded,
    /// No data is available for this gate.
    NoData,
}

impl From<&MomentValue> for GateStatus {
    fn from(value: &MomentValue) -> Self {
        match value {
            MomentValue::Value(_) => GateStatus::Valid,
            MomentValue::BelowThreshold => GateStatus::BelowThreshold,
            MomentValue::RangeFolded => GateStatus::RangeFolded,
        }
    }
}

/// A decoded CFP gate value. Raw values 0–7 are status codes; values 8+ are numeric.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum CFPMomentValue {
    /// A CFP status code (raw values 0–7).
    Status(CFPStatus),
    /// A decoded floating-point CFP value (raw values 8+).
    Value(f32),
}

/// Clutter filter power (CFP) moment data.
///
/// Gate metadata (count, range, interval) is available through the [`DataMoment`] trait —
/// see [`gate_count`](DataMoment::gate_count), [`first_gate_range_km`](DataMoment::first_gate_range_km),
/// and [`gate_interval_km`](DataMoment::gate_interval_km).
/// Use [`values`](CFPMomentData::values) to decode gates with CFP-specific semantics.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(transparent))]
pub struct CFPMomentData {
    inner: MomentDataBlock,
}

impl CFPMomentData {
    /// **LOCAL CHANGE.** This moment with its gate buffer released and every
    /// scalar kept — see [`MomentDataBlock::without_values`].
    pub fn without_values(&self) -> Self {
        Self {
            inner: self.inner.without_values(),
        }
    }

    /// **LOCAL CHANGE.** The same moment from a gate buffer that already
    /// exists — see [`MomentDataBlock::from_gate_buffer`]. The gates are
    /// adopted by reference count; nothing is copied.
    pub fn from_gate_buffer(
        gate_count: u16,
        first_gate_range: u16,
        gate_interval: u16,
        data_word_size: u8,
        scale: f32,
        offset: f32,
        values: GateBuffer,
    ) -> Self {
        Self {
            inner: MomentDataBlock::from_gate_buffer(
                gate_count,
                first_gate_range,
                gate_interval,
                data_word_size,
                scale,
                offset,
                values,
            ),
        }
    }

    /// **LOCAL CHANGE.** [`Self::from_gate_buffer`] for a buffer whose trailing
    /// below-threshold run was dropped — see
    /// [`MomentDataBlock::from_gate_buffer_with_sentinel_tail`]. Carrying the
    /// count is what lets a truncated moment survive a round trip that shares
    /// the buffer instead of copying it.
    pub fn from_gate_buffer_with_sentinel_tail(
        gate_count: u16,
        first_gate_range: u16,
        gate_interval: u16,
        data_word_size: u8,
        scale: f32,
        offset: f32,
        values: GateBuffer,
        trailing_sentinel_gates: u16,
    ) -> Self {
        Self {
            inner: MomentDataBlock::from_gate_buffer_with_sentinel_tail(
                gate_count,
                first_gate_range,
                gate_interval,
                data_word_size,
                scale,
                offset,
                values,
                trailing_sentinel_gates,
            ),
        }
    }

    /// Create new CFP moment data from fixed-point encoding.
    pub fn from_fixed_point(
        gate_count: u16,
        first_gate_range: u16,
        gate_interval: u16,
        data_word_size: u8,
        scale: f32,
        offset: f32,
        values: Vec<u8>,
    ) -> Self {
        Self {
            inner: MomentDataBlock::from_fixed_point(
                gate_count,
                first_gate_range,
                gate_interval,
                data_word_size,
                scale,
                offset,
                values,
            ),
        }
    }

    /// Iterator over decoded CFP gate values without allocating.
    ///
    /// Raw values 0–7 are decoded as CFP status codes. Values 8+ are decoded as
    /// floating-point values using the moment's scale and offset.
    pub fn iter(&self) -> impl Iterator<Item = CFPMomentValue> + '_ {
        let scale = self.inner.scale;
        let offset = self.inner.offset;

        self.inner
            .raw_gate_values()
            .map(move |raw_value| match raw_value {
                0 => CFPMomentValue::Status(CFPStatus::FilterNotApplied),
                1 => CFPMomentValue::Status(CFPStatus::PointClutterFilterApplied),
                2 => CFPMomentValue::Status(CFPStatus::DualPolOnlyFilterApplied),
                3..=7 => CFPMomentValue::Status(CFPStatus::Reserved(raw_value as u8)),
                _ => {
                    if scale == 0.0 {
                        CFPMomentValue::Value(raw_value as f32)
                    } else {
                        CFPMomentValue::Value((raw_value as f32 - offset) / scale)
                    }
                }
            })
    }

    /// Decoded CFP gate values collected into a `Vec`. Prefer [`iter`](Self::iter) when
    /// processing values sequentially to avoid allocation.
    pub fn values(&self) -> Vec<CFPMomentValue> {
        self.iter().collect()
    }
}

impl_data_moment!(CFPMomentData);
