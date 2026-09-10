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
        debug_assert!(
            data_word_size != 16 || values.len() % 2 == 0,
            "16-bit moment data must have an even number of bytes, got {}",
            values.len()
        );

        Self {
            gate_count,
            first_gate_range,
            gate_interval,
            data_word_size,
            scale,
            offset,
            values: BinaryData::new(GateBuffer::from(values)),
        }
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
