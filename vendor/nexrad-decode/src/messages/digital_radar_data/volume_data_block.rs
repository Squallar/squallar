use super::raw;
use super::{ProcessingStatus, VolumeCoveragePattern};
use std::borrow::Cow;

#[cfg(feature = "uom")]
use uom::si::f64::{Angle, Information, Length, Power};

/// Thousandths of a degree in one degree.
///
/// **Not** the unit this block is written in. See
/// [`VolumeDataBlock::latitude_raw`] for the volumes that write it anyway.
const THOUSANDTHS_PER_DEGREE: f32 = 1000.0;

/// Whether `lat`/`lon` name a point on Earth, in degrees.
///
/// The ICD's own ranges for the two fields, except that latitude is taken
/// symmetric: 2620002AA Table XVII-B states `0.0 to 90.0`, which is the
/// hemisphere the WSR-88D network happens to occupy and not a property of the
/// encoding.
fn is_on_earth(lat: f32, lon: f32) -> bool {
    (-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon)
}

/// Whether `lat`/`lon` read as a count of thousandths of a degree rather than
/// as the degrees the block declares.
///
/// The **numeric** half of the reading; [`VolumeDataBlock::states_thousandths`]
/// is the whole of it and holds the argument for why a numeric half is not
/// enough on its own.
///
/// Three conditions, and all three are needed:
///
/// * the pair is **not already a position**, which is what keeps every
///   conforming volume bit-identical: a value inside the ICD's range is
///   returned untouched, and every WSR-88D value is inside it;
/// * both are **exact integers**, because the encoding this recognises is an
///   `INT*4` widened into the `Real*4`, and every count of thousandths a
///   coordinate can produce (at most 180,000) is exactly representable in an
///   `f32`. A non-integer out-of-range float is not this encoding, it is
///   corruption, and corruption is for the caller to refuse;
/// * a thousandth of the pair **is** a position, so the reading is one the
///   result can be checked against rather than an assumption about the source.
fn pair_reads_as_thousandths(lat: f32, lon: f32) -> bool {
    !is_on_earth(lat, lon)
        && lat.fract() == 0.0
        && lon.fract() == 0.0
        && is_on_earth(lat / THOUSANDTHS_PER_DEGREE, lon / THOUSANDTHS_PER_DEGREE)
}

/// Internal representation of the volume data block, supporting both legacy and modern formats.
///
/// The format expanded at Build 20.0 (ICD 2620002U, July 2021).
#[derive(Clone, PartialEq, Debug)]
enum VolumeDataBlockInner<'a> {
    /// Legacy format (Build 10.0–19.0, 40 bytes, lrtup = 44).
    Legacy(Cow<'a, raw::VolumeDataBlockLegacy>),
    /// Modern format (Build 20.0+, 48 bytes, lrtup = 52).
    Modern(Cow<'a, raw::VolumeDataBlock>),
}

/// A volume data moment block.
///
/// This type provides access to volume metadata from digital radar data messages.
/// It supports both the legacy 40-byte format (Build 19.0 and earlier) and the
/// modern 48-byte format (Build 20.0 and later).
///
/// Fields that were added in Build 20.0 (`zdr_bias_estimate_weighted_mean` and `spare`)
/// return `Option` types that are `None` for legacy data.
#[derive(Clone, PartialEq, Debug)]
pub struct VolumeDataBlock<'a> {
    inner: VolumeDataBlockInner<'a>,
}

impl<'a> VolumeDataBlock<'a> {
    /// Create a new VolumeDataBlock wrapper from a raw VolumeDataBlock reference (modern format).
    pub(crate) fn new(inner: &'a raw::VolumeDataBlock) -> Self {
        Self {
            inner: VolumeDataBlockInner::Modern(Cow::Borrowed(inner)),
        }
    }

    /// Create a new VolumeDataBlock wrapper from a raw VolumeDataBlockLegacy reference.
    pub(crate) fn new_legacy(inner: &'a raw::VolumeDataBlockLegacy) -> Self {
        Self {
            inner: VolumeDataBlockInner::Legacy(Cow::Borrowed(inner)),
        }
    }

    /// Returns true if this is a legacy format block (Build 19.0 and earlier).
    pub fn is_legacy(&self) -> bool {
        matches!(self.inner, VolumeDataBlockInner::Legacy(..))
    }

    /// Convert this volume data block to an owned version with `'static` lifetime.
    pub fn into_owned(self) -> VolumeDataBlock<'static> {
        match self.inner {
            VolumeDataBlockInner::Legacy(inner) => VolumeDataBlock {
                inner: VolumeDataBlockInner::Legacy(Cow::Owned(inner.into_owned())),
            },
            VolumeDataBlockInner::Modern(inner) => VolumeDataBlock {
                inner: VolumeDataBlockInner::Modern(Cow::Owned(inner.into_owned())),
            },
        }
    }

    /// Size of data block in bytes (raw value).
    pub fn lrtup_raw(&self) -> u16 {
        match &self.inner {
            VolumeDataBlockInner::Legacy(inner) => inner.lrtup.get(),
            VolumeDataBlockInner::Modern(inner) => inner.lrtup.get(),
        }
    }

    /// Major version number.
    pub fn major_version_number(&self) -> u8 {
        match &self.inner {
            VolumeDataBlockInner::Legacy(inner) => inner.major_version_number,
            VolumeDataBlockInner::Modern(inner) => inner.major_version_number,
        }
    }

    /// Minor version number.
    pub fn minor_version_number(&self) -> u8 {
        match &self.inner {
            VolumeDataBlockInner::Legacy(inner) => inner.minor_version_number,
            VolumeDataBlockInner::Modern(inner) => inner.minor_version_number,
        }
    }

    /// Latitude of radar in degrees (raw value).
    ///
    /// # One producer writes thousandths of a degree here
    ///
    /// The field is `Real*4` **degrees** — ICD 2620002AA (Build 24.0)
    /// Table XVII-B, *Data Block #1 (Volume Data)*, `Lat` at bytes 8-11 and
    /// `Long` at bytes 12-15, both "deg". No revision of that table states any
    /// other scale, and every WSR-88D volume writes degrees.
    ///
    /// Older TDWR Level II volumes do not. They carry the **Level III** radar
    /// position instead: ICD 2620001AD's Product Description Block halfwords
    /// 11-12 and 13-14, an `INT*4` count of thousandths of a degree, widened
    /// into the `Real*4` without being divided. `TORD` reads
    /// `41797.0, -87858.0` where it means 41.797 °N, 87.858 °W, and the same
    /// holds at `TOKC`, `TDAL`, `TPIT`, `TATL`, `TMIA`, `TSJU` and `TPHX`.
    /// Neither height field is affected — `TORD` states 226 m on both sides of
    /// the change.
    ///
    /// # The correction was a rollout, not a date
    ///
    /// It is tempting to key this to 2021-09-15, because that is where a bisect
    /// over `TORD` lands: `TORD20210914_000151_V08` is the last thousandths
    /// volume and `TORD20210915_000148_V08` the first in degrees. **That date
    /// is `TORD`'s and no one else's.** Read one volume per site per sample
    /// date out of `s3://unidata-nexrad-level2`:
    ///
    /// ```text
    /// site   last seen in thousandths   first seen in degrees
    /// TATL   2021-06-15                 2021-09-14
    /// TDAL   2021-06-15                 2021-09-14
    /// TOKC   2021-06-15                 2021-09-14
    /// TPHX   2021-06-15                 2021-09-14
    /// TPIT   2021-06-15                 2021-09-14
    /// TORD   2021-09-14                 2021-09-15
    /// TMIA   2021-09-16                 2021-09-20
    /// TSJU   2020-08-10                 2022-08-10
    /// ```
    ///
    /// Five sites were already corrected on the day `TORD` was not, and `TMIA`
    /// was still writing thousandths five days *after* the date `TORD` suggests
    /// — so a rule that read "before 2021-09-15" would mis-read `TMIA`'s
    /// 2021-09-16 volume in one direction and nothing in the other. The
    /// timestamp is not the flag, and neither is anything else structural: see
    /// [`states_thousandths`](Self::states_thousandths) for the field-by-field
    /// comparison that says so.
    ///
    /// So this returns degrees for both, converting only a block that
    /// [`states_thousandths`](Self::states_thousandths) recognises. That
    /// predicate cannot fire on a value the ICD's own range admits, which is
    /// what makes every conforming volume byte-for-byte what it was; it cannot
    /// fire on a block no terminal radar would have written; and it does not
    /// rescue a pair that is merely wrong, which stays out of range for the
    /// caller to refuse.
    pub fn latitude_raw(&self) -> f32 {
        self.position_degrees().0
    }

    /// Longitude of radar in degrees (raw value).
    ///
    /// See [`latitude_raw`](Self::latitude_raw) for the one producer that
    /// writes thousandths of a degree in these two fields, and for what is done
    /// about it.
    pub fn longitude_raw(&self) -> f32 {
        self.position_degrees().1
    }

    /// The pair exactly as the block states it, before any scale is read into
    /// it.
    fn stated_position(&self) -> (f32, f32) {
        match &self.inner {
            VolumeDataBlockInner::Legacy(inner) => (inner.latitude.get(), inner.longitude.get()),
            VolumeDataBlockInner::Modern(inner) => (inner.latitude.get(), inner.longitude.get()),
        }
    }

    /// Whether this block is one the thousandths-writing producer wrote.
    ///
    /// # Why this is not only a test on the numbers
    ///
    /// [`pair_reads_as_thousandths`] asks whether a pair *could* be
    /// thousandths, and a pair that could be is not a pair that is. `(100.0,
    /// -100.0)` is not a position, both halves are integers, and a thousandth
    /// of it — `(0.1, -0.1)` — is a position in the Gulf of Guinea. Two bytes
    /// of damage in a coordinate therefore used to *become* a coordinate, and
    /// one that outranks the site catalogue and is then persisted. So the
    /// numeric reading has to be accompanied by something corrupt coordinates
    /// cannot forge: a fact about the block that says *which producer wrote
    /// it*.
    ///
    /// # What the archive offers, and what it does not
    ///
    /// **There is no structural signal for the era.** A pre-correction TDWR
    /// volume and a post-correction one from the same radar are byte-identical
    /// outside the two coordinates — checked field by field over the 7 terminal
    /// radars that have volumes on both 2021-09-14 and 2021-09-15, and over 93
    /// TDWR volumes from 9 sites between 2020 and 2026. The volume header, both
    /// metadata messages, the Message 31 data header, the pointer table, and
    /// the ELV and RAD blocks all compare equal; only the timestamps, the
    /// weather, and `Lat`/`Long` differ. See
    /// [`latitude_raw`](Self::latitude_raw) for the rollout that rules the date
    /// out as well.
    ///
    /// What the archive does offer is a signal for the **instrument**, and
    /// since the terminal producer is the only one that ever wrote thousandths,
    /// that is the fact worth requiring:
    ///
    /// * **the 40-byte block** ([`is_legacy`](Self::is_legacy)). Every volume
    ///   ever observed in thousandths is one. A WSR-88D's block expanded to 48
    ///   bytes at Build 20.0 and a TDWR's never did, so requiring the short
    ///   block costs a terminal radar nothing and puts the entire current
    ///   WSR-88D fleet permanently out of reach of this branch.
    /// * **one height stated twice**. A TDWR volume reports `tower_height`
    ///   byte-identical to `site_height` and no WSR-88D volume does — the
    ///   correspondence this crate's consumers already tell the two instruments
    ///   apart by, exact over the 205 volumes that built the site table and
    ///   over the 170 read for this. The nearest misses were checked rather
    ///   than assumed: the 6 WSR-88D whose published elevation equals a
    ///   standard tower height are `KJAX` 19/29, `KTLH` 19/34, `PAKC` 19/24,
    ///   `KCRP` 14/29 and `KLTX` 20/24, none of them equal.
    ///
    /// # What this still cannot do
    ///
    /// Narrow the door; not shut it. Damage confined to the two coordinates of
    /// a genuine TDWR block still satisfies every condition here, and so would
    /// a producer that wrote *hundredths* — `(4180, -8786)` is integral, out of
    /// range, and a thousandth of it is a place. Neither is a question this
    /// block can answer about itself, and both are refused a rung up, where a
    /// position can be held against a source outside the volume. See
    /// `rustdar_radar::types::ScanInfo::from_scan`.
    fn states_thousandths(&self) -> bool {
        let (lat, lon) = self.stated_position();
        self.is_legacy()
            && i32::from(self.site_height_raw()) == i32::from(self.tower_height_raw())
            && pair_reads_as_thousandths(lat, lon)
    }

    /// The stated pair in degrees, whichever of the two scales it is in.
    ///
    /// One function for both coordinates because the scale is a property of the
    /// pair: a latitude alone cannot always be told apart — a radar within
    /// 0.09° of the equator states a thousandths latitude that is itself a
    /// legal degrees latitude — and a decision taken twice could disagree with
    /// itself and land the radar in a place neither reading names.
    fn position_degrees(&self) -> (f32, f32) {
        let (lat, lon) = self.stated_position();
        if self.states_thousandths() {
            (lat / THOUSANDTHS_PER_DEGREE, lon / THOUSANDTHS_PER_DEGREE)
        } else {
            (lat, lon)
        }
    }

    /// Height of site base above sea level in meters (raw value).
    pub fn site_height_raw(&self) -> i16 {
        match &self.inner {
            VolumeDataBlockInner::Legacy(inner) => inner.site_height.get(),
            VolumeDataBlockInner::Modern(inner) => inner.site_height.get(),
        }
    }

    /// Height of radar tower above ground in meters (raw value).
    pub fn tower_height_raw(&self) -> u16 {
        match &self.inner {
            VolumeDataBlockInner::Legacy(inner) => inner.tower_height.get(),
            VolumeDataBlockInner::Modern(inner) => inner.tower_height.get(),
        }
    }

    /// Reflectivity scaling factor without correction by ground noise scaling factors given in
    /// adaptation data message in dB.
    pub fn calibration_constant(&self) -> f32 {
        match &self.inner {
            VolumeDataBlockInner::Legacy(inner) => inner.calibration_constant.get(),
            VolumeDataBlockInner::Modern(inner) => inner.calibration_constant.get(),
        }
    }

    /// Transmitter power for horizontal channel in kW (raw value).
    pub fn horizontal_shv_tx_power_raw(&self) -> f32 {
        match &self.inner {
            VolumeDataBlockInner::Legacy(inner) => inner.horizontal_shv_tx_power.get(),
            VolumeDataBlockInner::Modern(inner) => inner.horizontal_shv_tx_power.get(),
        }
    }

    /// Transmitter power for vertical channel in kW (raw value).
    pub fn vertical_shv_tx_power_raw(&self) -> f32 {
        match &self.inner {
            VolumeDataBlockInner::Legacy(inner) => inner.vertical_shv_tx_power.get(),
            VolumeDataBlockInner::Modern(inner) => inner.vertical_shv_tx_power.get(),
        }
    }

    /// Calibration of system ZDR in dB.
    pub fn system_differential_reflectivity(&self) -> f32 {
        match &self.inner {
            VolumeDataBlockInner::Legacy(inner) => inner.system_differential_reflectivity.get(),
            VolumeDataBlockInner::Modern(inner) => inner.system_differential_reflectivity.get(),
        }
    }

    /// Initial DP for the system in degrees (raw value).
    pub fn initial_system_differential_phase_raw(&self) -> f32 {
        match &self.inner {
            VolumeDataBlockInner::Legacy(inner) => inner.initial_system_differential_phase.get(),
            VolumeDataBlockInner::Modern(inner) => inner.initial_system_differential_phase.get(),
        }
    }

    /// Identifies the volume coverage pattern in use (raw value).
    pub fn volume_coverage_pattern_number(&self) -> u16 {
        match &self.inner {
            VolumeDataBlockInner::Legacy(inner) => inner.volume_coverage_pattern_number.get(),
            VolumeDataBlockInner::Modern(inner) => inner.volume_coverage_pattern_number.get(),
        }
    }

    /// Processing option flags (raw value).
    pub fn processing_status_raw(&self) -> u16 {
        match &self.inner {
            VolumeDataBlockInner::Legacy(inner) => inner.processing_status.get(),
            VolumeDataBlockInner::Modern(inner) => inner.processing_status.get(),
        }
    }

    /// RPG weighted mean ZDR bias estimate in dB.
    ///
    /// Returns `None` for legacy data (Build 19.0 and earlier) as this field
    /// was added in Build 20.0.
    pub fn zdr_bias_estimate_weighted_mean(&self) -> Option<u16> {
        match &self.inner {
            VolumeDataBlockInner::Legacy(_) => None,
            VolumeDataBlockInner::Modern(inner) => {
                Some(inner.zdr_bias_estimate_weighted_mean.get())
            }
        }
    }

    /// Spare bytes.
    ///
    /// Returns `None` for legacy data (Build 19.0 and earlier) as this field
    /// was added in Build 20.0.
    pub fn spare(&self) -> Option<&[u8; 6]> {
        match &self.inner {
            VolumeDataBlockInner::Legacy(_) => None,
            VolumeDataBlockInner::Modern(inner) => Some(&inner.spare.0),
        }
    }

    /// Size of data block.
    #[cfg(feature = "uom")]
    pub fn lrtup(&self) -> Information {
        Information::new::<uom::si::information::byte>(self.lrtup_raw() as f64)
    }

    /// Latitude of radar.
    #[cfg(feature = "uom")]
    pub fn latitude(&self) -> Angle {
        Angle::new::<uom::si::angle::degree>(self.latitude_raw() as f64)
    }

    /// Longitude of radar.
    #[cfg(feature = "uom")]
    pub fn longitude(&self) -> Angle {
        Angle::new::<uom::si::angle::degree>(self.longitude_raw() as f64)
    }

    /// Height of site base above sea level.
    #[cfg(feature = "uom")]
    pub fn site_height(&self) -> Length {
        Length::new::<uom::si::length::meter>(self.site_height_raw() as f64)
    }

    /// Height of radar tower above ground.
    #[cfg(feature = "uom")]
    pub fn tower_height(&self) -> Length {
        Length::new::<uom::si::length::meter>(self.tower_height_raw() as f64)
    }

    /// Transmitter power for horizontal channel.
    #[cfg(feature = "uom")]
    pub fn horizontal_shv_tx_power(&self) -> Power {
        Power::new::<uom::si::power::kilowatt>(self.horizontal_shv_tx_power_raw() as f64)
    }

    /// Transmitter power for vertical channel.
    #[cfg(feature = "uom")]
    pub fn vertical_shv_tx_power(&self) -> Power {
        Power::new::<uom::si::power::kilowatt>(self.vertical_shv_tx_power_raw() as f64)
    }

    /// Initial DP for the system.
    #[cfg(feature = "uom")]
    pub fn initial_system_differential_phase(&self) -> Angle {
        Angle::new::<uom::si::angle::degree>(self.initial_system_differential_phase_raw() as f64)
    }

    /// Identifies the volume coverage pattern in use.
    pub fn volume_coverage_pattern(&self) -> VolumeCoveragePattern {
        let volume_coverage_pattern = self.volume_coverage_pattern_number();
        match volume_coverage_pattern {
            12 => VolumeCoveragePattern::VCP12,
            31 => VolumeCoveragePattern::VCP31,
            35 => VolumeCoveragePattern::VCP35,
            112 => VolumeCoveragePattern::VCP112,
            212 => VolumeCoveragePattern::VCP212,
            215 => VolumeCoveragePattern::VCP215,
            other => VolumeCoveragePattern::Unknown(other),
        }
    }

    /// Processing option flags.
    pub fn processing_status(&self) -> ProcessingStatus {
        let processing_status = self.processing_status_raw();
        match processing_status {
            0 => ProcessingStatus::RxRNoise,
            1 => ProcessingStatus::CBT,
            _ => ProcessingStatus::Other(processing_status),
        }
    }
}
