//! Anonymous S3 access to the NEXRAD Level II *real-time* chunk bucket.
//!
//! The archive bucket [`crate::archive`] reads publishes a volume only once the
//! radar has finished every elevation cut, so the 0.5° tilt — collected in the
//! first ~30 seconds of a 4-6 minute volume, and the tilt that matters during
//! severe weather — reaches a display five to seven minutes late. The same data
//! is published here in ~55 pieces as it is collected, each landing within
//! seconds of the radar producing it.
//!
//! # Why this reimplements `nexrad_data::aws::realtime`
//!
//! Same reason [`crate::archive`] reimplements `nexrad_data::aws::archive`: that
//! tree is behind the crate's `aws` feature, which turns on `reqwest/rustls`,
//! which resolves to `__rustls-aws-lc-rs` and drags `aws-lc-sys` in beside the
//! *ring* stack [`crate::tls`] installs. `ChunkIdentifier`, `VolumeIndex`,
//! `ElevationChunkMapper` and `assemble_volume` are all on the wrong side of
//! that flag.
//!
//! Only the naming, the HTTP and the message walk are rebuilt here.
//! `nexrad_data::volume` — which is *not* feature-gated — still does every byte
//! of the decompression and decoding, exactly as the archive path does.
//!
//! # Layout
//!
//! Keys are `{site}/{volume}/{YYYYMMDD}-{HHMMSS}-{NNN}-{S|I|E}`, e.g.
//! `KTLX/42/20260728-181234-007-I`. The volume index rotates through 1..=999 and
//! is **not** zero-padded, so `KTLX/9/` and `KTLX/10/` are siblings and a
//! listing of the site prefix comes back in string order rather than numeric.
//! The name's leading timestamp is the volume's start time and is identical on
//! every chunk of a volume; the three digits are a sequence within it.

use std::future::Future;

use nexrad_data::volume;
use nexrad_decode::messages::MessageContents;
use nexrad_model::data::{
    ChannelConfiguration, ElevationCut, PulseWidth, Radial, VolumeCoveragePattern, WaveformType,
};

use crate::archive::ArchiveError;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Failures reaching or interpreting the real-time chunk bucket.
///
/// Shaped like [`crate::level3::Level3Error`]: the bucket's own failures arrive
/// transparently from [`ArchiveError`], and only what is specific to chunks gets
/// a variant here. Consumers render these with `{:?}`, so the variant and field
/// names are what a user sees — they are kept short and legible for that reason.
#[derive(Debug, thiserror::Error)]
pub enum ChunkError {
    #[error(transparent)]
    Bucket(#[from] ArchiveError),

    /// The site has no volume directories at all. An ordinary outcome for a
    /// radar that is down or has never published chunks, not a failure to reach
    /// the bucket — the distinction [`ArchiveError::NotFound`] draws for the
    /// archive.
    #[error("no real-time volumes for {site}")]
    NoVolumes { site: String },

    /// Neither an Archive II volume header nor a bzip2 LDM record.
    #[error(
        "chunk {name:?} is {len} bytes beginning {head:02x?}, which is neither \
         an Archive II volume header nor a bzip2 LDM record"
    )]
    UnrecognizedChunk {
        name: String,
        len: usize,
        head: Vec<u8>,
    },

    /// A start chunk too short to hold the volume header it claims.
    ///
    /// Its own variant because `volume::File::records` slices past the header
    /// without checking, so reaching it with a short buffer panics inside the
    /// dependency rather than returning an error.
    #[error("chunk {name:?} claims an Archive II header but is only {len} bytes")]
    ShortStartChunk { name: String, len: usize },

    #[error("decode error in chunk {name:?}: {source}")]
    Decode {
        name: String,
        #[source]
        source: nexrad_data::result::Error,
    },
}

pub type Result<T> = std::result::Result<T, ChunkError>;

// ---------------------------------------------------------------------------
// Naming
// ---------------------------------------------------------------------------

/// A volume's index in the rotating real-time bucket, 1..=999.
///
/// **Not zero-padded in bucket keys**, so it is a number interpolated into a
/// key, never a formatted field, and a delimited listing of a site returns its
/// directories in string order (1, 10, 100, …, 2, 20, …) rather than numeric.
/// Anything that searches over them has to parse and sort first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VolumeIndex(u16);

impl VolumeIndex {
    /// `None` outside 1..=999. A checked constructor rather than upstream's
    /// `debug_assert!`, which silently yields `VolumeIndex(1000)` in release.
    pub fn new(index: u16) -> Option<Self> {
        (1..=999).contains(&index).then_some(Self(index))
    }

    pub fn get(self) -> u16 {
        self.0
    }

    /// The next index in the rotation. Wraps 999 -> 1, never 0.
    pub fn next(self) -> Self {
        if self.0 == 999 {
            Self(1)
        } else {
            Self(self.0 + 1)
        }
    }

    /// This volume's key prefix, **including the trailing slash**.
    ///
    /// The slash is load-bearing: without it `KTLX/9` also matches every key
    /// under `KTLX/90/` and `KTLX/99/`, so a listing would mix three volumes.
    pub fn prefix(self, site: &str) -> String {
        format!("{site}/{}/", self.0)
    }
}

/// Where a chunk sits in its volume, from the trailing character of its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ChunkKind {
    /// Sequence 1. Carries the Archive II volume header and the metadata
    /// messages, including message 5 — the volume coverage pattern.
    Start,
    Intermediate,
    /// The last chunk of the volume.
    End,
}

impl ChunkKind {
    fn from_suffix(c: char) -> Option<Self> {
        match c {
            'S' => Some(Self::Start),
            'I' => Some(Self::Intermediate),
            'E' => Some(Self::End),
            _ => None,
        }
    }
}

/// One real-time chunk's identity, parsed from its object name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ChunkId {
    site: String,
    volume: VolumeIndex,
    /// The volume's start time, identical on every chunk of a volume.
    ///
    /// The only monotone field a chunk carries: the volume *index* rotates, so
    /// 999 precedes 1 in time and follows it in number.
    volume_time: chrono::NaiveDateTime,
    sequence: u16,
    kind: ChunkKind,
    name: String,
}

impl ChunkId {
    /// Parse a bare object name. `site` and `volume` come from the key path,
    /// which the name does not repeat.
    ///
    /// `None` rather than an error for a name that does not fit, matching
    /// [`crate::archive`]'s `key_to_identifier`: an unexpected key in a listing
    /// is dropped rather than failing the whole listing. Every slice goes
    /// through `.get()` — names come from bucket keys, and a short or non-ASCII
    /// one must not panic.
    pub fn parse(site: &str, volume: VolumeIndex, name: &str) -> Option<Self> {
        // "YYYYMMDD-HHMMSS-NNN-T" is exactly 21 bytes; anything shorter cannot
        // carry a sequence and a type.
        if name.len() < 21 {
            return None;
        }
        let volume_time =
            chrono::NaiveDateTime::parse_from_str(name.get(..15)?, "%Y%m%d-%H%M%S").ok()?;
        let bytes = name.as_bytes();
        if bytes.get(15) != Some(&b'-') || bytes.get(19) != Some(&b'-') {
            return None;
        }
        let sequence = name.get(16..19)?.parse::<u16>().ok()?;
        let kind = ChunkKind::from_suffix(name.chars().next_back()?)?;
        Some(Self {
            site: site.to_string(),
            volume,
            volume_time,
            sequence,
            kind,
            name: name.to_string(),
        })
    }

    /// Parse a full bucket key, `{site}/{volume}/{name}`.
    pub fn from_key(key: &str) -> Option<Self> {
        let mut parts = key.split('/');
        let site = parts.next()?;
        let volume = VolumeIndex::new(parts.next()?.parse::<u16>().ok()?)?;
        let name = parts.next()?;
        if parts.next().is_some() {
            return None;
        }
        Self::parse(site, volume, name)
    }

    pub fn key(&self) -> String {
        format!("{}{}", self.volume.prefix(&self.site), self.name)
    }

    pub fn site(&self) -> &str {
        &self.site
    }
    pub fn volume(&self) -> VolumeIndex {
        self.volume
    }
    pub fn volume_time(&self) -> chrono::NaiveDateTime {
        self.volume_time
    }
    pub fn sequence(&self) -> u16 {
        self.sequence
    }
    pub fn kind(&self) -> ChunkKind {
        self.kind
    }
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// Ordered by site, then volume start time, then sequence.
///
/// The volume *index* is deliberately absent. It rotates, so index 999 sorts
/// before index 1 by time and after it by number, and no total order can hold
/// both; the start time in the name says which volume is newer without it.
impl Ord for ChunkId {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.site
            .cmp(&other.site)
            .then(self.volume_time.cmp(&other.volume_time))
            .then(self.sequence.cmp(&other.sequence))
            .then(self.kind.cmp(&other.kind))
            .then(self.name.cmp(&other.name))
    }
}

impl PartialOrd for ChunkId {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

// ---------------------------------------------------------------------------
// Decoding
// ---------------------------------------------------------------------------

/// What one chunk carried.
#[derive(Debug, Default, Clone)]
pub struct ChunkContents {
    /// In the order the messages appeared, which is the order the radar
    /// collected them.
    pub radials: Vec<Radial>,
    /// Present on the start chunk, which is the only one carrying message 5.
    pub coverage_pattern: Option<VolumeCoveragePattern>,
    /// Each cut's declared Nyquist velocity, read off Message 31's Radial Data
    /// Block as the radials go past.
    ///
    /// Read here rather than recovered later because here is the last place it
    /// exists: `into_radial` builds a `nexrad_model::data::Radial`, which has
    /// no field for it, so a chunk that has been turned into radials has
    /// forgotten what it declared. The velocity fold guard in
    /// [`crate::sampler`] is the consumer — see [`crate::nyquist`].
    ///
    /// Empty for a Message 1 chunk: the legacy message declares no Nyquist
    /// velocity at all, and that is an absence, not a decode failure.
    pub declared_nyquist: crate::nyquist::DeclaredNyquist,
    /// Where the radar says it is, off the first Message 31's Volume Data
    /// Block — the same four fields [`crate::scan`]'s archive walk reads, and
    /// the same "first radial wins".
    ///
    /// Read here for the same reason the Nyquist velocity is: here is the last
    /// place it exists. `into_radial` builds a `nexrad_model::data::Radial`,
    /// which has no site on it, so a chunk that has been turned into radials
    /// has forgotten where it was collected — and this is a position the radar
    /// itself states, which is the one source [`crate::sites`] ranks above
    /// every other.
    ///
    /// The identifier comes off the Message 31 header rather than off a volume
    /// header, because an intermediate chunk has no volume header at all.
    ///
    /// `None` for a Message 1 chunk, which carries no Volume Data Block, and
    /// for a start chunk that carries only message 5.
    pub site: Option<nexrad_model::meta::Site>,
}

/// Decode one chunk's bytes.
///
/// **Dispatch is on content, never on the name's `S`/`I`/`E` letter.** A start
/// chunk is an Archive II volume header followed by a compressed LDM record; an
/// intermediate or end chunk is a bare LDM record with no header at all. Routing
/// by filename would hand a headerless record to the volume-header path, which
/// slices 24 bytes off the front and reads a length from the middle of the
/// payload — a silent mis-decode rather than an error. The letter is metadata
/// about position in the volume; the magic bytes are what say how to read it.
///
/// Deliberately not `Record::radials()`, which drops
/// `MessageContents::VolumeCoveragePattern` on the floor — the one thing the
/// start chunk exists to deliver. One `messages()` walk yields both.
pub fn decode_chunk(name: &str, bytes: &[u8]) -> Result<ChunkContents> {
    let mut out = ChunkContents::default();

    if bytes.get(..3) == Some(b"AR2".as_slice()) {
        // `volume::File::records` slices past the header without checking its
        // length, so a truncated start chunk panics inside the dependency.
        if bytes.len() <= std::mem::size_of::<volume::Header>() {
            return Err(ChunkError::ShortStartChunk {
                name: name.to_string(),
                len: bytes.len(),
            });
        }
        let file = volume::File::new(bytes.to_vec());
        let records = file.records().map_err(|source| ChunkError::Decode {
            name: name.to_string(),
            source,
        })?;
        for record in records {
            ingest_record(name, record, &mut out)?;
        }
    } else if bytes.get(4..6) == Some(b"BZ".as_slice()) {
        ingest_record(name, volume::Record::new(bytes.to_vec()), &mut out)?;
    } else {
        return Err(ChunkError::UnrecognizedChunk {
            name: name.to_string(),
            len: bytes.len(),
            head: bytes.iter().take(8).copied().collect(),
        });
    }

    Ok(out)
}

fn ingest_record(name: &str, record: volume::Record<'_>, out: &mut ChunkContents) -> Result<()> {
    let decode = |source| ChunkError::Decode {
        name: name.to_string(),
        source,
    };
    let record = if record.compressed() {
        record.decompress().map_err(decode)?
    } else {
        record
    };
    for message in record.messages().map_err(decode)? {
        match message.into_contents() {
            MessageContents::DigitalRadarData(m) => {
                // First radial of the chunk wins, matching `crate::scan`'s
                // archive walk. The position is restated on every radial and
                // does not move within a volume, so any of them would do; the
                // first is the one the archive path picks.
                if out.site.is_none()
                    && let Some(volume) = m.volume_data_block()
                {
                    out.site = Some(nexrad_model::meta::Site::new(
                        *m.header().radar_identifier_raw(),
                        volume.inner().latitude_raw(),
                        volume.inner().longitude_raw(),
                        volume.inner().site_height_raw(),
                        volume.inner().tower_height_raw(),
                    ));
                }
                // Before `into_radial`, which is where the number is lost: the
                // model type has no field for it. First radial of a cut wins,
                // which `declare` enforces; within a sweep the PRF is constant.
                out.declared_nyquist.declare_from_message(&m);
                out.radials
                    .push(m.into_radial().map_err(|e| decode(e.into()))?);
            }
            MessageContents::DigitalRadarDataLegacy(m) => {
                out.radials
                    .push(m.into_radial().map_err(|e| decode(e.into()))?);
            }
            // First one wins, matching `volume::File::scan`, which keeps the
            // first message 5 it sees and ignores any repeat.
            MessageContents::VolumeCoveragePattern(m) if out.coverage_pattern.is_none() => {
                out.coverage_pattern = Some(coverage_pattern_from(&m));
            }
            _ => {}
        }
    }
    Ok(())
}

/// Translate a decoded message 5 into the model's coverage pattern.
///
/// Transcribed from `nexrad_data::aws::realtime::assemble_volume`, which is
/// behind the `aws` feature. Reconstructing it in full rather than keeping only
/// the pattern number costs one mechanical `map` and makes a chunk-assembled
/// `Scan` indistinguishable from an archive-decoded one — which matters the day
/// something reads `coverage_pattern().elevation_cuts()`.
///
/// `pub(crate)` for [`crate::scan`], which decodes an archive volume on a walk
/// of its own and needs the same translation. One copy rather than two is the
/// whole point: an archive volume and a chunk-assembled one must not differ on
/// a field because two transcriptions of message 5 drifted apart.
pub(crate) fn coverage_pattern_from(
    msg: &nexrad_decode::messages::volume_coverage_pattern::Message<'_>,
) -> VolumeCoveragePattern {
    use nexrad_decode::messages::volume_coverage_pattern as vcp;

    let header = msg.header();
    let pulse_width = match header.pulse_width() {
        vcp::PulseWidth::Short => PulseWidth::Short,
        vcp::PulseWidth::Long => PulseWidth::Long,
        vcp::PulseWidth::Unknown => PulseWidth::Unknown,
    };

    let elevation_cuts = msg
        .elevations()
        .iter()
        .map(|elev| {
            let channel_config = match elev.channel_configuration() {
                vcp::ChannelConfiguration::ConstantPhase => ChannelConfiguration::ConstantPhase,
                vcp::ChannelConfiguration::RandomPhase => ChannelConfiguration::RandomPhase,
                vcp::ChannelConfiguration::SZ2Phase => ChannelConfiguration::SZ2Phase,
                vcp::ChannelConfiguration::UnknownPhase => ChannelConfiguration::Unknown,
            };
            let waveform = match elev.waveform_type() {
                vcp::WaveformType::CS => WaveformType::CS,
                vcp::WaveformType::CDW => WaveformType::CDW,
                vcp::WaveformType::CDWO => WaveformType::CDWO,
                vcp::WaveformType::B => WaveformType::B,
                vcp::WaveformType::SPP => WaveformType::SPP,
                vcp::WaveformType::Unknown => WaveformType::Unknown,
            };
            ElevationCut::new(
                elev.elevation_angle(),
                channel_config,
                waveform,
                elev.azimuth_rate(),
                elev.super_resolution_half_degree_azimuth(),
                elev.super_resolution_quarter_km_reflectivity(),
                elev.super_resolution_doppler_to_300km(),
                elev.super_resolution_dual_pol_to_300km(),
                elev.surveillance_prf_number(),
                elev.surveillance_prf_pulse_count_radial(),
                elev.reflectivity_threshold(),
                elev.velocity_threshold(),
                elev.spectrum_width_threshold(),
                elev.differential_reflectivity_threshold(),
                elev.differential_phase_threshold(),
                elev.correlation_coefficient_threshold(),
                elev.is_sails_cut(),
                elev.sails_sequence_number(),
                elev.is_mrle_cut(),
                elev.mrle_sequence_number(),
                elev.is_mpda_cut(),
                elev.is_base_tilt_cut(),
            )
        })
        .collect();

    VolumeCoveragePattern::new(
        header.pattern_number(),
        header.version(),
        header.doppler_velocity_resolution(),
        pulse_width,
        header.is_sails_vcp(),
        header.number_of_sails_cuts(),
        header.is_mrle_vcp(),
        header.number_of_mrle_cuts(),
        header.is_mpda_vcp(),
        header.is_base_tilt_vcp(),
        header.number_of_base_tilts(),
        header.vcp_sequencing_sequence_active(),
        header.vcp_sequencing_truncated(),
        elevation_cuts,
    )
}

/// Stands in until a start chunk arrives.
///
/// Only `pattern_number` is meaningful. That is enough because
/// [`crate::render::render_radar_to_image`] reads `scan.sweeps()` and nothing
/// else, and the single reader of the pattern anywhere in this workspace is
/// [`crate::types::ScanInfo::from_scan`], which takes the number for the "VCP n"
/// chrome. A volume joined mid-flight therefore renders identically and only
/// mislabels itself until the next volume's start chunk lands.
pub(crate) fn placeholder_coverage_pattern(pattern_number: u16) -> VolumeCoveragePattern {
    VolumeCoveragePattern::new(
        pattern_number,
        0,
        0.5,
        PulseWidth::Unknown,
        false,
        0,
        false,
        0,
        false,
        false,
        0,
        false,
        false,
        Vec::new(),
    )
}

// ---------------------------------------------------------------------------
// Assembly
// ---------------------------------------------------------------------------

/// How complete a cut must be before it is rendered, as a percentage of the
/// radials its azimuth spacing implies.
///
/// The `ElevationEnd` status says the RDA finished the cut, not that every chunk
/// carrying it arrived, so the count is checked too. 95% is the gap between "a
/// few radials dropped" and "a whole chunk missing": chunks hold 120 radials, so
/// one lost chunk leaves 600 of 720 (83%) or 240 of 360 (67%), both well under,
/// while a stray drop or two still seals.
const MIN_SEALED_RADIAL_PERCENT: usize = 95;

/// Radials one chunk carries, which is what makes a chunk sequence map onto an
/// elevation cut without decoding anything.
///
/// Measured against the live bucket: an intermediate chunk of a KTLX volume
/// decodes to exactly 120 radials, and the cuts of a VCP-35 volume sealed on
/// sequences 7, 13, 19, 25, 31, 37, then 40, 43, 46, 49, 52, 55 — six chunks for
/// each super-resolution cut and three for each standard one, against a start
/// chunk at sequence 1 that carries no radials at all.
const RADIALS_PER_CHUNK: usize = 120;

/// Which cuts a caller wants assembled.
///
/// Downloading every chunk to render one tilt is most of the traffic wasted: a
/// 0.5° pane needs 13 of a 55-chunk volume. But several products integrate the
/// whole volume — the set [`crate::types::RadarProduct::reads_whole_volume`]
/// names, which is the one place it is written down — and each walks only the
/// tilts *present*: `compute_echo_tops` clamps every column to the topmost one,
/// so feeding it a selective volume produces a plausible, low, wrong answer with
/// no error to notice.
///
/// So the selection is the caller's to make, and [`All`](Self::All) is the
/// answer whenever anything on screen needs the volume.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum CutSelection {
    /// Every cut. What a volume-wide product requires, and the default: nothing
    /// may be skipped until a caller has said what it can do without.
    #[default]
    All,
    /// Only cuts within [`ELEVATION_MATCH`] of one of these angles.
    ///
    /// The start chunk is always taken regardless: it carries the coverage
    /// pattern, without which nothing can be mapped at all.
    Tilts(Vec<f32>),
}

/// How near a cut's planned angle must be to a wanted one to count as it.
///
/// Deliberately wider than `render::ELEVATION_WINDOW`, which these two used to
/// share. The quantities are not the same: `find_sweep` compares a request
/// against a sweep the radar has already flown, and can be exact about it, but
/// this compares a request against the VCP's *planned* angle for a cut that has
/// not been downloaded yet — which is the whole point, since the decision is
/// what to skip. The errors here are also asymmetric. Taking a cut that was not
/// wanted costs one download; skipping one that was costs a cut that never
/// completes and a volume that never closes, so the slack stays.
const ELEVATION_MATCH: f32 = 0.3;

impl CutSelection {
    fn wants_angle(&self, angle: f32) -> bool {
        match self {
            Self::All => true,
            Self::Tilts(wanted) => wanted.iter().any(|w| (w - angle).abs() <= ELEVATION_MATCH),
        }
    }

    fn is_all(&self) -> bool {
        matches!(self, Self::All)
    }
}

/// Which elevation cut each chunk sequence belongs to, derived from the volume
/// coverage pattern.
///
/// This is what lets a chunk be skipped *before* it is downloaded. The radials
/// inside it say which cut it belongs to, but only once it has been fetched and
/// decompressed — which is the cost being avoided.
///
/// Reimplements upstream's `ElevationChunkMapper`, which is behind the `aws`
/// feature. Purely arithmetic over a decoded message 5.
#[derive(Debug, Clone)]
pub struct ElevationChunkMap {
    /// One entry per cut, in VCP order: its 1-based elevation number, its
    /// planned angle, and the sequence range it occupies.
    cuts: Vec<(u8, f32, std::ops::RangeInclusive<u16>)>,
}

impl ElevationChunkMap {
    /// A cut's planned angle, with a negative base tilt read as negative.
    ///
    /// A VCP carries the angle as an unsigned field, so a site that points its
    /// lowest cut *below* the horizon declares it as a number just under 360:
    /// KMSX's is 359.82°, and its radials duly report −0.124°. Left as it comes,
    /// the comparison in [`CutSelection::wants_angle`] is against a wanted angle
    /// near −0.2°, which is 360° away — so under [`CutSelection::Tilts`] that cut
    /// would never be wanted, never downloaded, and the lowest tilt of a
    /// mountain-top site would stay empty however long you waited for it.
    ///
    /// Mountain-top sites only. [`crate::sampler`] reaches the same rule for its
    /// own elevation keys (`if key > 180.0 { key -= 360.0 }`) — arrived at
    /// independently rather than copied from here, since neither existed when
    /// the other was written, which is some evidence it is the right reading of
    /// the field rather than a local patch.
    fn planned_angle_degrees(cut: &nexrad_model::data::ElevationCut) -> f32 {
        let angle = cut.elevation_angle_degrees();
        // Halfway round is far past any tilt a radar flies, so nothing legitimate
        // is caught by this.
        if angle > 180.0 {
            (angle - 360.0) as f32
        } else {
            angle as f32
        }
    }

    /// Build from the coverage pattern the start chunk carried.
    ///
    /// `None` when the pattern lists no cuts — a placeholder, or a message 5
    /// this build could not read — in which case nothing can be skipped safely
    /// and the caller must take everything.
    pub fn from_coverage_pattern(vcp: &VolumeCoveragePattern) -> Option<Self> {
        let planned = vcp.elevation_cuts();
        if planned.is_empty() {
            return None;
        }
        // Sequence 1 is the start chunk: metadata only, no radials.
        let mut next = 2u16;
        let cuts = planned
            .iter()
            .enumerate()
            .map(|(i, cut)| {
                let radials = if cut.super_resolution_half_degree_azimuth() {
                    720
                } else {
                    360
                };
                let chunks = (radials / RADIALS_PER_CHUNK) as u16;
                let range = next..=(next + chunks - 1);
                next += chunks;
                ((i + 1) as u8, Self::planned_angle_degrees(cut), range)
            })
            .collect();
        Some(Self { cuts })
    }

    /// The cut a sequence belongs to: its elevation number and planned angle.
    /// `None` for the start chunk, or a sequence past the end of the pattern.
    pub fn cut_for(&self, sequence: u16) -> Option<(u8, f32)> {
        self.cuts
            .iter()
            .find(|(_, _, range)| range.contains(&sequence))
            .map(|(elevation, angle, _)| (*elevation, *angle))
    }

    /// Whether this sequence is worth downloading under `selection`.
    ///
    /// The start chunk and anything the map cannot place are always wanted: the
    /// first carries the coverage pattern, and skipping the second would be
    /// guessing. Being wrong here costs a download; being wrong the other way
    /// costs a cut that never completes.
    pub fn wants(&self, sequence: u16, selection: &CutSelection) -> bool {
        match self.cut_for(sequence) {
            None => true,
            Some((_, angle)) => selection.wants_angle(angle),
        }
    }

    /// Elevation numbers a selection asks for, which is what "complete" means
    /// once cuts are being skipped.
    pub fn wanted_elevations(&self, selection: &CutSelection) -> Vec<u8> {
        self.cuts
            .iter()
            .filter(|(_, angle, _)| selection.wants_angle(*angle))
            .map(|(elevation, _, _)| *elevation)
            .collect()
    }

    /// How many cuts the pattern plans.
    pub fn cut_count(&self) -> usize {
        self.cuts.len()
    }
}

/// One elevation cut being accumulated.
enum Cut {
    /// Still receiving.
    ///
    /// Keyed by `azimuth_number` — the RDA's 1..720 index within the sweep — so
    /// re-ingesting a chunk is idempotent by construction rather than by a rule
    /// every insertion point has to remember, and iteration is already in
    /// collection order, which is what `Sweep::merge` sorts to and what the
    /// rasterizer's wedge painter expects.
    Open {
        radials: std::collections::BTreeMap<u16, Radial>,
        /// An `ElevationEnd` or `ScanEnd` radial has arrived.
        terminated: bool,
        /// Radials a full rotation implies, from the first radial's azimuth
        /// spacing: 720 at 0.5°, 360 at 1.0°.
        ///
        /// A rotation is the assumption, and it is the *only* thing at ingest
        /// that separates a cut the RDA swept over less than the circle from a
        /// cut whose chunks did not all arrive. Nothing on the wire announces
        /// an intended sweep extent, and the two look alike: an `ElevationEnd`
        /// radial says the RDA finished, not that everything it sent got here,
        /// so a cut that lost its first or a middle chunk terminates with a
        /// contiguous-looking 240 of 360 — and sealing that would put a grid
        /// with a 120° hole in its middle in front of `nrot`, whose
        /// `azimuth::Rows` measures where a grid ends and not where it is
        /// sparse. (Losing the *last* chunk is already caught for free: the
        /// terminating radial is in it.) So a short cut is abandoned, and the
        /// count is what abandons it.
        ///
        /// The assumption costs nothing on the data this reads. Every cut of
        /// every TDWR volume measured out of the real-time bucket — TATL,
        /// TDFW, THOU, TJFK, TMCO, TPHX and TPIT, three volumes each, across
        /// both scan strategies the network flies (16 cuts and 23) — is 360
        /// radials of declared 1.0°, numbered 1..360 with no gap, so
        /// `360 / spacing` is their radial count exactly, as it is for every
        /// WSR-88D VCP cut. The one short cut in the sample was a volume still
        /// arriving.
        ///
        /// If a sector cut ever does turn up, the count is not the number to
        /// relax. `azimuth_number` is the anchor that tells the two cases
        /// apart: the RDA numbers a cut's radials from 1, so a swept sector
        /// runs 1..N dense and a cut with a chunk missing does not.
        expected: Option<usize>,
    },
    /// A full rotation, frozen. Radials are *moved* out of the map rather than
    /// copied — a sweep is megabytes of gate bytes.
    Sealed(nexrad_model::data::Sweep),
    /// Terminated, or closed with the volume, short of its radial count.
    ///
    /// Kept as a diagnostic and **never** placed in a snapshot, because a cut
    /// that lost a chunk out of its middle is a grid with a hole in the middle:
    /// radials 1..120 and 241..360 land as 240 consecutive rows, and rows 119
    /// and 120 of that grid are 121° apart with nothing between them.
    /// `nrot::llsd_nrot` differentiates across that seam like any other pair of
    /// adjacent rows — `azimuth::Rows` measures where a grid *ends*, which is
    /// what keeps a sector's two edges honest, and knows nothing about where
    /// one is sparse. It bails only at zero radials, so nothing downstream
    /// would catch this.
    ///
    /// The two things that no longer compound it: the scale, since
    /// `azimuth::Rows` measures the grid's own step instead of taking
    /// `360 / rows` (a half cut is differentiated over the arc it covers rather
    /// than over twice it, and `render::derived_grid_wedge_deg` paints it at
    /// that same spacing), and the outer edges, which now read ND rather than
    /// each other's velocities.
    Abandoned { have: usize, expected: usize },
}

impl Cut {
    fn is_sealed(&self) -> bool {
        matches!(self, Self::Sealed(_))
    }
}

/// What one `ingest` call changed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IngestOutcome {
    /// `false` when this sequence had already been ingested and nothing changed.
    pub accepted: bool,
    /// Elevation numbers whose cut completed on this chunk, ascending.
    ///
    /// **The render trigger.** Also the test the caller uses to decide whether a
    /// snapshot is worth building at all — see [`VolumeAssembler::snapshot`].
    pub sealed: Vec<u8>,
    /// This chunk carried the coverage pattern.
    pub learned_coverage_pattern: bool,
    /// Every cut the volume plans is sealed and the volume has ended.
    pub volume_complete: bool,
}

/// A cut that ended short of a full rotation, and by how much.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AbandonedCut {
    pub elevation: u8,
    pub have: usize,
    /// What the cut's azimuth spacing implied. `0` when no radial ever arrived
    /// for it, so the spacing was never learned.
    pub expected: usize,
}

/// A volume's assembly state, for the caller's gating and logging.
#[derive(Debug, Clone, PartialEq)]
pub struct VolumeProgress {
    pub volume: VolumeIndex,
    pub volume_time: Option<chrono::NaiveDateTime>,
    /// Elevation numbers with a complete sweep in the snapshot, ascending.
    pub sealed_elevations: Vec<u8>,
    /// Their angles, parallel to `sealed_elevations`. From
    /// `Sweep::elevation_angle_degrees`, which is the median over the sweep's
    /// radials and so is not thrown by a transitional first or last one.
    pub sealed_angles: Vec<f32>,
    /// Cuts that ended short. A volume holding one never completes, so this is
    /// the first thing to look at when one does not.
    pub abandoned: Vec<AbandonedCut>,
    pub saw_scan_end: bool,
    /// Every cut **the selection asked for** sealed, and the volume ended. See
    /// [`VolumeAssembler::is_volume_complete`].
    ///
    /// The gate for the cuts on screen, and **not** the gate for a product that
    /// integrates the volume: under a narrow [`CutSelection`] this is true with
    /// most of the pattern never downloaded. Use [`Self::whole_volume_complete`]
    /// for that, and see its doc for what goes wrong otherwise.
    pub volume_complete: bool,
    /// The volume is **whole**: every cut it carries sealed, contiguous from 1.
    /// See [`VolumeAssembler::is_whole_volume_complete`].
    ///
    /// Gate for every product [`crate::types::RadarProduct::reads_whole_volume`]
    /// names, and the only safe gate for a volume that will outlive the selection
    /// that produced it.
    ///
    /// `volumetric::compute_echo_tops` walks only the tilts present and clamps
    /// each column to the topmost *available* tilt's centre height, so a volume
    /// missing cuts yields a plausible, low, wrong number in kft — no error, no
    /// NaN. Every product in that set fails the same invisible way, which is why
    /// the gate is one flag rather than a per-product judgement here.
    pub whole_volume_complete: bool,
    pub chunks_ingested: usize,
    /// Radials that arrived for an already-sealed cut. Expected to stay zero;
    /// a nonzero count means elevation numbers repeat within a volume, which the
    /// `BTreeMap<u8, _>` accumulator cannot represent.
    pub late_radials_dropped: usize,
}

/// Accumulates one volume's chunks into complete sweeps.
///
/// Keyed by elevation number rather than fed to `Sweep::from_radials`, which
/// groups by *consecutive runs* of equal elevation number: out-of-order or
/// backfilled chunks make it emit several sweeps all claiming one elevation.
/// That breaks `render::find_sweep`, which searches `.rev()` assuming a later
/// sweep is the newer cut, and `volumetric::VolumeCube`'s newest-wins dedup,
/// where a fragment would displace a complete earlier sweep. `Sweep::merge` is
/// no better — it requires equal elevation numbers, re-sorts by azimuth, and
/// does not dedup, so merging a chunk twice doubles the sweep.
///
/// The scheme is safe because elevation numbers are sequential and unique within
/// a volume, SAILS and MRLE inserts included — each repeat of a low tilt takes
/// its own number. `archive::tests::live_volume_elevation_numbers_are_contiguous_and_terminated`
/// pins that against a real VCP-212 volume — `#[ignore]`d, since it fetches
/// one, so the default test row does not check it.
pub struct VolumeAssembler {
    site: String,
    volume: VolumeIndex,
    /// Learned from the first accepted chunk; every later one must match.
    volume_time: Option<chrono::NaiveDateTime>,
    ingested: std::collections::BTreeSet<u16>,
    cuts: std::collections::BTreeMap<u8, Cut>,
    coverage_pattern: Option<VolumeCoveragePattern>,
    saw_start_chunk: bool,
    saw_scan_end: bool,
    /// Built from the coverage pattern the moment it arrives; `None` until the
    /// start chunk lands, which is why nothing can be skipped before then.
    chunk_map: Option<ElevationChunkMap>,
    /// What the caller asked for. Only ever narrows what is *fetched*; anything
    /// that arrives anyway is still assembled.
    selection: CutSelection,
    late_radials_dropped: usize,
    closed: bool,
    /// Invalidated whenever a cut seals. See [`Self::snapshot`].
    cached: Option<std::sync::Arc<nexrad_model::data::Scan>>,
    /// Every cut's declared Nyquist velocity, accumulated across the chunks as
    /// they arrive. See [`Self::declared_nyquist`].
    declared_nyquist: crate::nyquist::DeclaredNyquist,
    /// Where the radar said it was, off the first chunk that carried a Volume
    /// Data Block. Goes onto [`Self::snapshot`]'s `Scan`, which is what lets a
    /// chunk-fed pane place itself from its own data — see
    /// [`ChunkContents::site`].
    reported_site: Option<nexrad_model::meta::Site>,
}

impl VolumeAssembler {
    pub fn new(site: impl Into<String>, volume: VolumeIndex) -> Self {
        Self {
            site: site.into(),
            volume,
            volume_time: None,
            ingested: Default::default(),
            cuts: Default::default(),
            coverage_pattern: None,
            saw_start_chunk: false,
            saw_scan_end: false,
            chunk_map: None,
            selection: CutSelection::All,
            late_radials_dropped: 0,
            closed: false,
            cached: None,
            declared_nyquist: crate::nyquist::DeclaredNyquist::empty(),
            reported_site: None,
        }
    }

    pub fn site(&self) -> &str {
        &self.site
    }
    pub fn volume(&self) -> VolumeIndex {
        self.volume
    }
    pub fn volume_time(&self) -> Option<chrono::NaiveDateTime> {
        self.volume_time
    }

    /// Feed one chunk's bytes.
    pub fn ingest(&mut self, id: &ChunkId, bytes: &[u8]) -> Result<IngestOutcome> {
        if id.volume() != self.volume || self.ingested.contains(&id.sequence()) {
            return Ok(IngestOutcome::default());
        }
        let contents = decode_chunk(id.name(), bytes)?;
        Ok(self.ingest_contents(id.sequence(), id.kind(), id.volume_time(), contents))
    }

    /// The decode-free half, and the seam the equivalence test drives: a golden
    /// `Scan` re-sliced into chunks needs no encoder to reach this.
    pub(crate) fn ingest_contents(
        &mut self,
        sequence: u16,
        kind: ChunkKind,
        volume_time: chrono::NaiveDateTime,
        contents: ChunkContents,
    ) -> IngestOutcome {
        // A leftover from the previous pass through this rotating index carries
        // elevation numbers that would collide with the volume being assembled,
        // so it is refused rather than merged. Checked here rather than in
        // `ingest` so it is reachable without encoding a chunk.
        if self.volume_time.is_some_and(|known| known != volume_time) {
            return IngestOutcome::default();
        }
        if self.closed || !self.ingested.insert(sequence) {
            return IngestOutcome::default();
        }
        self.volume_time = Some(volume_time);
        if kind == ChunkKind::Start {
            self.saw_start_chunk = true;
        }

        let mut outcome = IngestOutcome {
            accepted: true,
            ..Default::default()
        };
        if let Some(vcp) = contents.coverage_pattern
            // Learning it again is not learning it: a repeat of the same table
            // changes nothing to rebuild the chunk map for and nothing for a
            // reader to redraw. Guarded like the reported site below rather than
            // left to the fact that a sequence is ingested once, so the cost of
            // this arm is bounded by the code and not by an argument about the
            // caller.
            && self.coverage_pattern.as_ref() != Some(&vcp)
        {
            let vcp = self.coverage_pattern.insert(vcp);
            self.chunk_map = ElevationChunkMap::from_coverage_pattern(vcp);
            outcome.learned_coverage_pattern = true;
            // Same reasoning as the reported site below, and the same one line.
            // A snapshot handed out before this carries
            // `placeholder_coverage_pattern`, whose cut table is empty, and a
            // `Scan` that cannot key its own sweeps must not go on being served
            // once the real pattern is known.
            //
            // Reachable, not theoretical: the start chunk is the only carrier of
            // message 5, and a listed-then-missing key is ordinary enough that
            // `fetch_disposition` calls it `Skip` — so a round can seal a cut
            // with the pattern still absent. `current::resolve` drops an overlay
            // whose pattern has no cuts, by design, which left the live volume
            // discarded until the *next* seal happened to rebuild the cache.
            self.cached = None;
        }

        // Accumulated whatever else this chunk turns out to be. A cut arrives
        // across several chunks and its declaration is repeated on every
        // radial, so the first chunk to mention a cut is the one that names it
        // and the rest are no-ops; a chunk refused above never reaches here,
        // so a stale volume's numbers cannot leak into this one's table.
        for (elevation_number, ms) in contents.declared_nyquist.iter() {
            self.declared_nyquist.declare(elevation_number, ms);
        }

        // First chunk that states one wins, as the archive walk's fold does.
        // The snapshot already handed out does not carry it, so it is dropped:
        // the position is on the volume the caller is about to ask for again,
        // and a `Scan` that says where its radar is must not go on being served
        // from a cache built before anything knew.
        if self.reported_site.is_none()
            && let Some(site) = contents.site
        {
            self.reported_site = Some(site);
            self.cached = None;
        }

        let mut touched: Vec<u8> = Vec::new();
        for radial in contents.radials {
            let elevation = radial.elevation_number();
            let status = radial.radial_status();
            let terminates = matches!(
                status,
                nexrad_model::data::RadialStatus::ElevationEnd
                    | nexrad_model::data::RadialStatus::ScanEnd
            );
            if matches!(status, nexrad_model::data::RadialStatus::ScanEnd) {
                self.saw_scan_end = true;
            }

            let cut = self.cuts.entry(elevation).or_insert_with(|| Cut::Open {
                radials: Default::default(),
                terminated: false,
                expected: None,
            });
            match cut {
                Cut::Open {
                    radials,
                    terminated,
                    expected,
                } => {
                    if expected.is_none() {
                        let spacing = radial.azimuth_spacing_degrees();
                        if spacing > 0.0 {
                            *expected = Some((360.0 / spacing).round() as usize);
                        }
                    }
                    *terminated |= terminates;
                    // First write wins, so re-ingesting a chunk is byte-stable.
                    radials.entry(radial.azimuth_number()).or_insert(radial);
                    if !touched.contains(&elevation) {
                        touched.push(elevation);
                    }
                }
                // Never reopened: a sealed cut may already be inside a `Scan`
                // some render is holding, and mutating it would change an image
                // mid-flight.
                Cut::Sealed(_) | Cut::Abandoned { .. } => self.late_radials_dropped += 1,
            }
        }

        touched.sort_unstable();
        for elevation in touched {
            if self.try_seal(elevation) {
                outcome.sealed.push(elevation);
            }
        }
        if !outcome.sealed.is_empty() {
            self.cached = None;
        }
        outcome.volume_complete = self.is_volume_complete();
        outcome
    }

    /// Seal a cut if it is terminated and complete enough. Returns whether it
    /// sealed on this call.
    fn try_seal(&mut self, elevation: u8) -> bool {
        let Some(Cut::Open {
            radials,
            terminated,
            expected,
        }) = self.cuts.get_mut(&elevation)
        else {
            return false;
        };
        let Some(expected) = *expected else {
            return false;
        };
        if !*terminated || radials.len() * 100 < expected * MIN_SEALED_RADIAL_PERCENT {
            return false;
        }
        let radials = std::mem::take(radials);
        self.cuts.insert(
            elevation,
            Cut::Sealed(nexrad_model::data::Sweep::new(
                elevation,
                radials.into_values().collect(),
            )),
        );
        true
    }

    /// Narrow what this volume will fetch.
    ///
    /// Applied to *downloads*, never to assembly: a chunk that arrives anyway —
    /// because it was already in flight, or because the selection just widened —
    /// is still ingested.
    pub fn set_selection(&mut self, selection: CutSelection) {
        self.selection = selection;
    }

    pub fn selection(&self) -> &CutSelection {
        &self.selection
    }

    /// Whether this chunk is worth downloading.
    ///
    /// Always true until the start chunk has been seen: without the coverage
    /// pattern there is no map, and guessing would drop cuts the caller wanted.
    pub fn wants_chunk(&self, sequence: u16) -> bool {
        if self.selection.is_all() {
            return true;
        }
        match &self.chunk_map {
            None => true,
            Some(map) => map.wants(sequence, &self.selection),
        }
    }

    /// Whether every cut **the selection asked for** is sealed and the volume has
    /// ended.
    ///
    /// This is the gate for the cuts on screen, and it is deliberately *not* the
    /// gate for a product that integrates the volume. Under a narrow
    /// [`CutSelection`] it is true with most of the pattern never downloaded — a
    /// single 0.5° pane completes a volume of one cut — so a caller holding this
    /// flag knows only "the cuts I asked for all arrived". The question a volume
    /// integral has to ask is [`Self::is_whole_volume_complete`].
    pub fn is_volume_complete(&self) -> bool {
        if !self.saw_start_chunk || !self.saw_scan_end || self.cuts.is_empty() {
            return false;
        }
        match (&self.selection, &self.chunk_map) {
            // Everything was asked for, so the two questions coincide.
            (CutSelection::All, _) | (_, None) => self.every_cut_sealed_contiguously(),
            // Cuts were deliberately skipped, so contiguity is meaningless and
            // "complete" means every cut that was *asked for*. Note what this
            // does not license: a volume completed this way must never reach a
            // product that integrates the volume. That is what
            // `is_whole_volume_complete` is for — the distinction used to be left
            // to callers, and a volume completed this way reached the loop cache,
            // where a later product change read it as a short whole volume.
            (selection, Some(map)) => {
                let wanted = map.wanted_elevations(selection);
                !wanted.is_empty()
                    && wanted
                        .iter()
                        .all(|elevation| self.is_elevation_sealed(*elevation))
            }
        }
    }

    /// Whether this volume is **whole**: every cut it carries sealed, their
    /// numbers contiguous from 1, and the volume ended.
    ///
    /// The question every product [`crate::types::RadarProduct::reads_whole_volume`]
    /// names has to ask, and the only one safe to ask of a volume that will outlive
    /// the selection that produced it — a cached loop frame, say, which is read by
    /// whatever product the pane is showing *later*.
    ///
    /// Asks the data, not the intent: a narrow selection whose skipped cuts turned
    /// up anyway answers `true`, because it really is whole. Nothing about
    /// [`CutSelection`] appears here.
    ///
    /// The contiguity clause is not decoration. A volume *joined mid-flight* has no
    /// entry at all for the cuts that finished before the first chunk arrived, so
    /// without it this would report whole a volume whose lowest tilts are simply
    /// absent — and `compute_echo_tops` would integrate tilts 8..23 and report
    /// every column's top far too low.
    pub fn is_whole_volume_complete(&self) -> bool {
        if !self.saw_start_chunk || !self.saw_scan_end || self.cuts.is_empty() {
            return false;
        }
        self.every_cut_sealed_contiguously()
    }

    /// Written once so the two predicates above cannot drift apart.
    fn every_cut_sealed_contiguously(&self) -> bool {
        self.cuts.values().all(Cut::is_sealed)
            && self
                .cuts
                .keys()
                .copied()
                .eq(1..=self.cuts.keys().copied().max().unwrap_or(0))
    }

    /// Whether this sequence has already been taken.
    ///
    /// A poller re-lists the whole volume directory each tick, so this is what
    /// stops it re-downloading every chunk it already has.
    pub fn has_ingested(&self, sequence: u16) -> bool {
        self.ingested.contains(&sequence)
    }

    pub fn is_elevation_sealed(&self, elevation: u8) -> bool {
        self.cuts.get(&elevation).is_some_and(Cut::is_sealed)
    }

    /// Resolve every still-open cut and stop accepting chunks.
    ///
    /// Open cuts are resolved *here* rather than when a higher elevation number
    /// appears: out-of-order arrival is the premise of this module, so "a later
    /// cut has started" is not evidence an earlier one finished.
    pub fn close(&mut self) -> VolumeProgress {
        let short: Vec<(u8, usize, usize)> = self
            .cuts
            .iter()
            .filter_map(|(elevation, cut)| match cut {
                Cut::Open {
                    radials, expected, ..
                } => Some((*elevation, radials.len(), expected.unwrap_or(0))),
                _ => None,
            })
            .collect();
        for (elevation, have, expected) in &short {
            log::debug!(
                "{} volume {}: elevation {elevation} closed with {have}/{expected} radials",
                self.site,
                self.volume.get()
            );
            self.cuts.insert(
                *elevation,
                Cut::Abandoned {
                    have: *have,
                    expected: *expected,
                },
            );
        }
        if !short.is_empty() {
            self.cached = None;
        }
        self.closed = true;
        self.progress()
    }

    pub fn progress(&self) -> VolumeProgress {
        let mut sealed_elevations = Vec::new();
        let mut sealed_angles = Vec::new();
        let mut abandoned = Vec::new();
        for (elevation, cut) in &self.cuts {
            match cut {
                Cut::Sealed(sweep) => {
                    sealed_elevations.push(*elevation);
                    sealed_angles.push(sweep.elevation_angle_degrees().unwrap_or(f32::NAN));
                }
                Cut::Abandoned { have, expected } => abandoned.push(AbandonedCut {
                    elevation: *elevation,
                    have: *have,
                    expected: *expected,
                }),
                Cut::Open { .. } => {}
            }
        }
        VolumeProgress {
            volume: self.volume,
            volume_time: self.volume_time,
            sealed_elevations,
            sealed_angles,
            abandoned,
            saw_scan_end: self.saw_scan_end,
            volume_complete: self.is_volume_complete(),
            whole_volume_complete: self.is_whole_volume_complete(),
            chunks_ingested: self.ingested.len(),
            late_radials_dropped: self.late_radials_dropped,
        }
    }

    /// The volume so far, as a `Scan` carrying **only complete sweeps**, in
    /// ascending elevation-number order.
    ///
    /// *Only complete sweeps*: a partial cut is never in the `Scan`, so
    /// `render::find_sweep` — and `render_input::RenderInput::extract`, which
    /// calls it — cannot reach one, and NROT cannot be rendered from one. That
    /// is a property of the data handed over rather than a rule every caller has
    /// to remember.
    ///
    /// *Ascending elevation number* is what preserves the "a later sweep is the
    /// newer cut" invariant `find_sweep`'s `.rev()` and `VolumeCube`'s
    /// newest-wins dedup both stand on: a SAILS revisit of a low tilt takes a
    /// higher elevation number than the original, so it sorts after it.
    ///
    /// **Cached, and the cache matters.** `Sweep: Clone` is a deep copy of every
    /// gate byte — `nexrad_model::BinaryData` wraps a `Vec<u8>`, not an
    /// `Arc<[u8]>` — and `Scan::new` takes owned sweeps, so each call copies
    /// every sealed sweep so far. The same `Arc` comes back until a cut seals;
    /// build one per *rendered* completion, not one per poll, and use
    /// [`IngestOutcome::sealed`] to tell whether the seal was even for a tilt
    /// anything is showing. [`ChunkPoller`] refills this cache at the end of
    /// any round that sealed — see `ChunkPoller::warm_snapshot` — so the first
    /// frame-thread call after a seal is already the `Arc` clone.
    ///
    /// **Carries the site** whenever a chunk stated one, which every Message 31
    /// chunk does. That is what makes a chunk-fed pane place itself the way an
    /// archive-fed one does: [`crate::types::ScanInfo::from_scan`] reads
    /// `Scan::site()` first of the three sources it ranks, so a live feed for a
    /// radar this install has never opened lands on that radar's own position
    /// instead of on whatever the catalogue could say about it. A Message 1
    /// volume states none and the `Scan` then carries none.
    pub fn snapshot(&mut self) -> std::sync::Arc<nexrad_model::data::Scan> {
        if let Some(cached) = &self.cached {
            return std::sync::Arc::clone(cached);
        }
        let sweeps: Vec<nexrad_model::data::Sweep> = self
            .cuts
            .values()
            .filter_map(|cut| match cut {
                Cut::Sealed(sweep) => Some(sweep.clone()),
                _ => None,
            })
            .collect();
        let vcp = self
            .coverage_pattern
            .clone()
            .unwrap_or_else(|| placeholder_coverage_pattern(0));
        let scan = std::sync::Arc::new(match self.reported_site.clone() {
            Some(site) => nexrad_model::data::Scan::with_site(site, vcp, sweeps),
            None => nexrad_model::data::Scan::new(vcp, sweeps),
        });
        self.cached = Some(std::sync::Arc::clone(&scan));
        scan
    }

    /// Whether [`Self::snapshot`] would return without building. The question
    /// the warming tests have to ask *before* calling `snapshot()`, which
    /// would itself warm a cold cache and erase the difference being tested.
    #[cfg(test)]
    pub(crate) fn snapshot_is_warm(&self) -> bool {
        self.cached.is_some()
    }

    /// Every cut's declared Nyquist velocity, as far as the chunks so far have
    /// said — the number [`Self::snapshot`]'s `Scan` cannot carry, because
    /// `nexrad_model::data::Radial` has no field for it.
    ///
    /// Covers cuts that are still open as well as sealed ones: the declaration
    /// arrives on the cut's first radial, long before its last. That is
    /// harmless — a reader looks the table up by the elevation number of a
    /// sweep it already holds, so an entry for a cut no snapshot carries is
    /// never consulted — and it means a cut is declared from the moment it
    /// seals rather than one chunk later.
    ///
    /// Empty for a volume assembled entirely from Message 1, which declares no
    /// Nyquist velocity; readers then estimate. See [`crate::nyquist`].
    pub fn declared_nyquist(&self) -> &crate::nyquist::DeclaredNyquist {
        &self.declared_nyquist
    }
}

// ---------------------------------------------------------------------------
// Polling
// ---------------------------------------------------------------------------

/// Base delay between rounds.
///
/// Measured against KTLX: the *latency* of this feed is bound by this number,
/// not by the bucket — cuts became renderable a median 4 s after their last
/// radial was collected, with a 5 s interval. The only cadence-dependent cost is
/// the listing, ~5 kB a round (~3.5 MB/hour), because the chunk downloads
/// themselves happen once regardless of how often the directory is checked.
pub const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

/// Delay after a round that found nothing new.
///
/// Backing off on *quiet* rather than on error, because no new chunk is the
/// ordinary state between cuts and across the gap between volumes; an empty
/// round is not a failure and must not be counted as one.
pub const QUIET_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);

/// Ceiling on the failure backoff.
pub const MAX_BACKOFF: std::time::Duration = std::time::Duration::from_secs(120);

/// How stale the current volume may get before discovery is re-run rather than
/// the next index probed. Three volume periods; past that the app was probably
/// backgrounded and stepping one index per round would take many rounds to catch
/// up.
const VOLUME_STALE: chrono::TimeDelta = chrono::TimeDelta::minutes(15);

/// What a round should do, decided from state alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PollPlan {
    /// Nothing known, or what is known is too old to walk forward from.
    Discover,
    /// List the current volume and fetch what is new.
    Fill { volume: VolumeIndex },
    /// The current volume ended; see whether the next has started.
    ProbeNext {
        current: VolumeIndex,
        next: VolumeIndex,
    },
}

/// Whether a per-chunk fetch failure ends the round.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FetchDisposition {
    Skip,
    Abort,
}

/// A listed-then-missing key is ordinary: S3 is eventually consistent, and the
/// rotation can retire a key between the listing and the GET. Anything else ends
/// the round — the chunks already ingested stay ingested, so the next round
/// resumes rather than restarts.
pub(crate) fn fetch_disposition(e: &ArchiveError) -> FetchDisposition {
    match e {
        ArchiveError::NotFound(_) => FetchDisposition::Skip,
        _ => FetchDisposition::Abort,
    }
}

/// A volume that ended, and what it ended as.
///
/// **The scan travels with the report.** [`ChunkPoller::roll`] is the only thing
/// that closes a volume, and it closes one by replacing the assembler
/// [`ChunkPoller::snapshot`] reads — so a caller told "that volume completed" can
/// no longer reach the volume in question. Handing back only its
/// [`VolumeProgress`] made every consequence gated on
/// [`VolumeProgress::volume_complete`] unreachable: by the time the report
/// arrived, the only readable volume was the *new* one, zero cuts in.
///
/// Both fields describe the same instant, which is the point: `scan` carries
/// exactly the cuts `progress.sealed_elevations` names, so a whole-volume product
/// checking `volume_complete` and then reading `scan` cannot be looking at two
/// different volumes.
#[derive(Clone)]
pub struct ClosedVolume {
    pub progress: VolumeProgress,
    /// The volume as it stood when it closed — complete sweeps only, so a cut
    /// that ended short is absent rather than present and partial.
    ///
    /// `Some` **iff** `progress.volume_complete`. Building it costs a deep copy of
    /// every sealed `Sweep` whenever [`VolumeAssembler::snapshot`]'s cache is cold,
    /// and the cache is cold *exactly* when the volume did not complete —
    /// [`VolumeAssembler::close`] clears it only when it had to abandon an open
    /// cut. So building one unconditionally paid the full copy in precisely the
    /// case every consumer throws it away.
    pub scan: Option<std::sync::Arc<nexrad_model::data::Scan>>,
    /// What the closed volume's cuts declared their Nyquist velocities to be —
    /// the number [`Self::scan`] cannot carry, because
    /// `nexrad_model::data::Radial` has no field for it.
    ///
    /// Always present, unlike [`Self::scan`]: it is a handful of `f64`s, so
    /// there is nothing to save by withholding it, and a consumer that adopts
    /// the closed volume as its merge base needs the pair or its sections
    /// guard on estimated fold limits. Empty for a Message 1 volume, which
    /// declares no Nyquist velocity at all.
    pub declared_nyquist: crate::nyquist::DeclaredNyquist,
}

/// Summarised rather than derived, and not because of the gate bytes: those live
/// in `BinaryData`, which has its own summarising `Debug`. The cost is that the
/// summary is a **sha256 over every gate byte of every moment of every radial**,
/// on top of ~20 MB of output text. This type is reachable from `PollOutcome`'s
/// derived `Debug`, so one `log::debug!("{outcome:?}")` would pay all of it.
impl std::fmt::Debug for ClosedVolume {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClosedVolume")
            .field("progress", &self.progress)
            .field("scan_sweeps", &self.scan.as_ref().map(|s| s.sweeps().len()))
            .finish()
    }
}

/// What one round changed.
///
/// An outcome carrying a [`Self::closed`] describes **two** volumes: the one
/// that closed, there, and the one now being assembled, in every other field.
/// They are deliberately not merged — a caller reading a whole volume wants the
/// first and a caller reading one tilt wants the second.
///
/// **`closed` is not necessarily this round's roll.** A round that rolls and
/// then fails parks its closed volume and a later round delivers it, so
/// `closed.is_some()` does not imply `rolled_to.is_some()`, nor that
/// `closed.progress.volume` is the volume before the one now being assembled.
/// Read everything about the closed volume from the [`ClosedVolume`] itself,
/// never by pairing it with the rest of this struct — see
/// `ChunkPoller::park_for_next_round`.
#[derive(Debug, Clone, Default)]
pub struct PollOutcome {
    pub ingested: usize,
    /// A chunk this round carried the coverage pattern, so the volume being
    /// assembled can key its own sweeps from now on and could not before.
    ///
    /// **A render trigger in its own right**, and the only one on the round it
    /// fires: the start chunk carries no radials, so nothing seals, and the
    /// snapshot every reader held until now carried
    /// `placeholder_coverage_pattern` — which `crate::current::resolve` refuses,
    /// having no cut table to key by. Images already drawn from that refusal
    /// stay drawn until something invalidates them, and
    /// [`Self::sealed_elevations`] is empty here.
    pub learned_coverage_pattern: bool,
    /// Elevation numbers whose cut completed this round, ascending. **The render
    /// trigger**, and the test for whether a snapshot is worth building.
    pub sealed_elevations: Vec<u8>,
    /// Their angles, parallel to `sealed_elevations`.
    pub sealed_angles: Vec<f32>,
    /// The volume rolled this round; `snapshot` now describes a new volume.
    pub rolled_to: Option<VolumeIndex>,
    /// A volume that closed, **with the scan it closed as**. Where
    /// `volume_complete` for a finished volume is reported, and the only way to
    /// reach that volume: the roll that produced it replaced the assembler
    /// `snapshot` reads. See [`ClosedVolume`].
    ///
    /// Usually this round's roll, but not always — a roll whose round then
    /// failed is delivered by a later one. See this struct's own doc before
    /// pairing this with any other field.
    pub closed: Option<ClosedVolume>,
    pub progress: Option<VolumeProgress>,
    /// Keys that would not parse or bytes that would not decode. Skipped, not
    /// fatal.
    pub skipped: usize,
}

/// One site's real-time feed.
///
/// **Pull-driven: nothing here sleeps, loops or schedules.** [`Self::poll`] does
/// one round and returns; the caller decides when to call again. That is a wasm
/// requirement, not a preference — this crate builds for `wasm32`, where reqwest
/// is the browser's `fetch()`, there is no `tokio::time`, and a self-scheduling
/// task could not be cancelled by the UI. The frontend already drives every
/// other fetch this way.
pub struct ChunkPoller {
    site: String,
    current: Option<VolumeAssembler>,
    consecutive_failures: u32,
    last_round_was_quiet: bool,
    /// Carried on the poller rather than the assembler so it survives a volume
    /// roll — the caller sets it from what is on screen, which does not change
    /// just because the radar started a new volume.
    selection: CutSelection,
    /// Volumes that closed in rounds that then failed, oldest first, waiting for
    /// outcomes that reach the caller. See [`Self::park_for_next_round`].
    pending_closed: std::collections::VecDeque<ClosedVolume>,
}

impl ChunkPoller {
    pub fn new(site: impl Into<String>) -> Self {
        Self {
            site: site.into(),
            current: None,
            consecutive_failures: 0,
            last_round_was_quiet: false,
            selection: CutSelection::All,
            pending_closed: std::collections::VecDeque::new(),
        }
    }

    /// Resume from a known index, skipping discovery.
    pub fn resume(site: impl Into<String>, volume: VolumeIndex) -> Self {
        let site = site.into();
        Self {
            current: Some(VolumeAssembler::new(site.clone(), volume)),
            site,
            consecutive_failures: 0,
            last_round_was_quiet: false,
            selection: CutSelection::All,
            pending_closed: std::collections::VecDeque::new(),
        }
    }

    /// Narrow what this feed downloads to the cuts a caller actually renders.
    ///
    /// The traffic this saves is the bulk of the feed: a 0.5° pane needs 13 of a
    /// 55-chunk volume, so ~76% of the bytes — and ~76% of the bzip2 work, which
    /// on wasm happens on the main thread.
    ///
    /// **The caller owns the safety of this.** `compute_echo_tops` clamps every
    /// column to the topmost tilt present and a wind-profile fit averages
    /// whatever velocity tilts it is handed, so either would read a selective
    /// volume as a complete short one — silently. Pass [`CutSelection::All`]
    /// whenever anything on screen is a product
    /// [`crate::types::RadarProduct::reads_whole_volume`] names.
    pub fn set_selection(&mut self, selection: CutSelection) {
        if self.selection == selection {
            return;
        }
        log::debug!("{}: cut selection -> {selection:?}", self.site);
        self.selection = selection.clone();
        if let Some(current) = self.current.as_mut() {
            // Applied to the volume in flight too, so widening the selection
            // backfills within this volume rather than waiting for the next one:
            // the skipped chunks are still in the bucket, and the next listing
            // will select them.
            current.set_selection(selection);
        }
    }

    pub fn selection(&self) -> &CutSelection {
        &self.selection
    }

    pub fn site(&self) -> &str {
        &self.site
    }

    pub fn volume(&self) -> Option<VolumeIndex> {
        self.current.as_ref().map(VolumeAssembler::volume)
    }

    pub fn progress(&self) -> Option<VolumeProgress> {
        self.current.as_ref().map(VolumeAssembler::progress)
    }

    /// The volume so far, complete sweeps only. `None` before the first chunk.
    pub fn snapshot(&mut self) -> Option<std::sync::Arc<nexrad_model::data::Scan>> {
        self.current.as_mut().map(VolumeAssembler::snapshot)
    }

    /// What the volume being assembled declared its cuts' Nyquist velocities
    /// to be — the number [`Self::snapshot`]'s `Scan` cannot carry. `None`
    /// before the first chunk, and empty for a Message 1 volume.
    ///
    /// See [`VolumeAssembler::declared_nyquist`]; a caller pairs the two into
    /// a [`crate::nyquist::Volume`] so the velocity fold guard reads the
    /// declaration rather than estimating.
    pub fn declared_nyquist(&self) -> Option<&crate::nyquist::DeclaredNyquist> {
        self.current.as_ref().map(VolumeAssembler::declared_nyquist)
    }

    /// Advisory delay before the next [`Self::poll`]. Advisory because this crate
    /// has no timer on wasm — the caller owns the clock.
    pub fn suggested_interval(&self) -> std::time::Duration {
        if self.consecutive_failures > 0 {
            let shift = self.consecutive_failures.min(6);
            return (POLL_INTERVAL * (1 << shift)).min(MAX_BACKOFF);
        }
        if self.last_round_was_quiet {
            QUIET_INTERVAL
        } else {
            POLL_INTERVAL
        }
    }

    /// What the next round should do. `now` is a parameter so the staleness rule
    /// is testable.
    pub(crate) fn plan(&self, now: chrono::NaiveDateTime) -> PollPlan {
        let Some(current) = &self.current else {
            return PollPlan::Discover;
        };
        // A volume this old means the walk-forward would take many rounds to
        // catch up; one discovery is cheaper.
        if current
            .volume_time()
            .is_some_and(|t| now.signed_duration_since(t) > VOLUME_STALE)
        {
            return PollPlan::Discover;
        }
        if current.progress().saw_scan_end {
            return PollPlan::ProbeNext {
                current: current.volume(),
                next: current.volume().next(),
            };
        }
        PollPlan::Fill {
            volume: current.volume(),
        }
    }

    /// The chunks in `listed` this volume still wants, ascending.
    pub(crate) fn select(&self, listed: &[ChunkId]) -> Vec<ChunkId> {
        let Some(current) = &self.current else {
            return Vec::new();
        };
        let volume_time = current.volume_time();
        listed
            .iter()
            .filter(|id| {
                id.volume() == current.volume()
                    && !current.has_ingested(id.sequence())
                    && current.wants_chunk(id.sequence())
                    // A directory the rotation has not yet cleared can still
                    // hold the previous pass's chunks alongside the new ones.
                    && volume_time.is_none_or(|known| id.volume_time() == known)
            })
            .cloned()
            .collect()
    }

    /// Close the current volume and begin the next.
    ///
    /// Returns the closed volume *and its scan*, not just its progress. The
    /// assembler [`Self::snapshot`] reads is replaced on the next line, so
    /// anything this does not hand back is unreachable the instant the caller
    /// learns the volume finished — see [`ClosedVolume`].
    ///
    /// # What the snapshot costs, and why it is free on the path that wants it
    ///
    /// Only built when the volume completed, because that is exactly the case
    /// [`VolumeAssembler::snapshot`]'s cache is *warm* in: `close` clears the cache
    /// only when it had to abandon an open cut, which is the definition of not
    /// completing. Built unconditionally, the deep copy of every sealed `Sweep`
    /// landed precisely on the volumes every consumer discards.
    ///
    /// Warm is not automatic, and the reason is a cross-layer one worth naming: a
    /// **seal** also clears that cache, so what makes it warm at roll time is that
    /// something rebuilt it in the round the volume's last cut sealed. That is now
    /// [`Self::warm_snapshot`]'s job — every sealing round ends by refilling the
    /// cache — so it no longer depends on the caller having asked for a snapshot,
    /// which is a guarantee this path used to rest on and no longer has to. It held
    /// either way: [`Self::plan`] runs before any ingestion, so it cannot see
    /// `saw_scan_end` until a *later* round, and the final seal and the roll are
    /// therefore never the same round.
    pub(crate) fn roll(&mut self, to: VolumeIndex) -> Option<ClosedVolume> {
        let closed = self.current.as_mut().map(|current| {
            // `close`, not `progress`: it is what resolves a still-open cut to
            // `Abandoned`, so `progress.abandoned` names exactly the cuts the scan
            // below is missing. What keeps a short cut *out* of that scan is
            // `snapshot`'s own sealed-only filter, so these two lines commute —
            // this is the readable order, not a load-bearing one.
            let progress = current.close();
            let declared_nyquist = current.declared_nyquist().clone();
            let scan = progress.volume_complete.then(|| current.snapshot());
            ClosedVolume {
                progress,
                scan,
                declared_nyquist,
            }
        });
        let mut next = VolumeAssembler::new(self.site.clone(), to);
        next.set_selection(self.selection.clone());
        self.current = Some(next);
        closed
    }

    /// Fetch one chunk the caller already knows the key of, and ingest it.
    ///
    /// The push-notification path. A notification names the object outright, so
    /// none of [`Self::poll`]'s reconnaissance is needed: no listing to find what
    /// is new, no discovery search to find the volume, and no probe to notice a
    /// rollover — the chunk's own name carries the volume's start time, which is
    /// what makes one directory distinguishable from the same index one rotation
    /// ago.
    ///
    /// Rolls the volume when the named chunk belongs to a *newer* one, and
    /// ignores it when it belongs to an older one: an index the rotation has not
    /// yet reused still holds the previous pass, and a late or replayed message
    /// must not drag the assembler backwards.
    ///
    /// Deliberately additive rather than a replacement. [`Self::poll`] keeps
    /// running underneath as the gap-filler for anything a dropped socket or a
    /// missed message left behind, and it is the whole path when no notifier is
    /// reachable.
    pub async fn fetch_notified(
        &mut self,
        sources: &crate::sources::DataSources,
        id: &ChunkId,
    ) -> Result<PollOutcome> {
        // Before the first `.await`, for the same reason as in `poll`.
        let _ = crate::archive::shared_client();

        // As in `poll`: a volume closed by a round that then failed leaves on a
        // later outcome, whichever way that one leaves. Oldest first.
        let mut outcome = PollOutcome {
            closed: self.pending_closed.pop_front(),
            ..Default::default()
        };
        if id.site() != self.site {
            return Ok(outcome);
        }

        if self.is_stale_notification(id) {
            return Ok(outcome);
        }
        if self.should_roll_to(id) {
            let closed = self.roll(id.volume());
            self.deliver_or_queue(&mut outcome, closed);
            outcome.rolled_to = Some(id.volume());
        } else if self.current.is_none() {
            let mut next = VolumeAssembler::new(self.site.clone(), id.volume());
            next.set_selection(self.selection.clone());
            self.current = Some(next);
            outcome.rolled_to = Some(id.volume());
        }

        let Some(current) = self.current.as_mut() else {
            return Ok(outcome);
        };
        if current.has_ingested(id.sequence()) || !current.wants_chunk(id.sequence()) {
            return Ok(outcome);
        }

        let bytes = match download_chunk(sources, id).await {
            Ok(bytes) => bytes,
            // A notification can beat the object into the bucket, and S3 is
            // eventually consistent besides. The periodic poll picks it up.
            Err(ChunkError::Bucket(e)) if fetch_disposition(&e) == FetchDisposition::Skip => {
                outcome.skipped += 1;
                return Ok(outcome);
            }
            Err(e) => {
                self.consecutive_failures += 1;
                self.park_for_next_round(&mut outcome);
                return Err(e);
            }
        };

        let Some(current) = self.current.as_mut() else {
            return Ok(outcome);
        };
        match current.ingest(id, &bytes) {
            Ok(o) if o.accepted => {
                outcome.ingested += 1;
                outcome.sealed_elevations.extend(o.sealed);
                outcome.learned_coverage_pattern |= o.learned_coverage_pattern;
            }
            Ok(_) => {}
            Err(e) => {
                log::warn!("{}: chunk {} did not decode: {e:?}", self.site, id.name());
                outcome.skipped += 1;
            }
        }

        self.consecutive_failures = 0;
        self.fill_sealed_angles(&mut outcome);
        self.warm_snapshot(&outcome);
        Ok(outcome)
    }

    /// Whether a notified chunk belongs to a volume newer than the one being
    /// assembled, and so should close it and start there.
    ///
    /// Compared on the volume's *start time*, taken from the chunk's own name,
    /// not on the index: the index rotates, so 999 precedes 1 in time and follows
    /// it in number. This is what replaces `PollPlan::ProbeNext` on the
    /// notification path.
    pub(crate) fn should_roll_to(&self, id: &ChunkId) -> bool {
        self.current
            .as_ref()
            .and_then(VolumeAssembler::volume_time)
            .is_some_and(|known| id.volume_time() > known)
    }

    /// Whether a notified chunk belongs to a volume *older* than the one being
    /// assembled — a replayed message, or a directory the rotation has not yet
    /// cleared.
    pub(crate) fn is_stale_notification(&self, id: &ChunkId) -> bool {
        self.current
            .as_ref()
            .and_then(VolumeAssembler::volume_time)
            .is_some_and(|known| id.volume_time() < known)
    }

    /// Resolve each sealed elevation number to the angle a pane selects on.
    fn fill_sealed_angles(&self, outcome: &mut PollOutcome) {
        let progress = self.progress();
        if let Some(progress) = &progress {
            outcome.sealed_angles = outcome
                .sealed_elevations
                .iter()
                .map(|e| {
                    progress
                        .sealed_elevations
                        .iter()
                        .position(|s| s == e)
                        .map(|i| progress.sealed_angles[i])
                        .unwrap_or(f32::NAN)
                })
                .collect();
        }
        outcome.progress = progress;
    }

    /// Rebuild [`VolumeAssembler::snapshot`]'s cache inside the round that
    /// sealed, so the frame thread does not pay the copy.
    ///
    /// A seal is exactly what clears that cache, and the first `snapshot()`
    /// after the round returns is the frame thread's — the frontend reads one
    /// when it applies the outcome and one per frame when it publishes base
    /// volumes. Left cold, what lands on the paint path is a deep copy of
    /// every sweep sealed so far, once per sealed cut, on whatever cadence the
    /// coverage pattern sets. Built here, that copy runs inside the async
    /// round and the frame thread's call is an `Arc` clone. The same move
    /// [`Self::roll`] documents for a completed volume, extended to every
    /// sealing round. The size and the cadence are both the volume's, and
    /// neither has been measured on this path.
    ///
    /// Keyed on [`PollOutcome::sealed_elevations`] rather than run every
    /// round, because a seal is the invalidation the frame thread would
    /// otherwise pay for. It is not the only one — the first chunk to report
    /// the site or the coverage pattern clears the cache too, and so does
    /// [`VolumeAssembler::close`] abandoning a short cut — so this warms the
    /// common case rather than guaranteeing a warm cache. Warming on every
    /// round instead would build volumes for rounds nothing will render.
    ///
    /// On wasm the copy still runs on the page thread — the round is a
    /// `spawn_local` future — so there this moves it out of the paint
    /// callback, not off the thread. What would remove the copy rather than
    /// move it is gate bytes that can be shared instead of cloned:
    /// `nexrad_model::BinaryData` wraps a `Vec<u8>` rather than an
    /// `Arc<[u8]>`, which is what makes `Sweep: Clone` a byte copy.
    fn warm_snapshot(&mut self, outcome: &PollOutcome) {
        if outcome.sealed_elevations.is_empty() {
            return;
        }
        if let Some(current) = self.current.as_mut() {
            let _ = current.snapshot();
        }
    }

    /// Hold a closed volume back when the round it closed in ends in an error,
    /// so a later outcome carries it.
    ///
    /// **What this guarantees: no `ClosedVolume` this poller has built is ever
    /// dropped *by the poller*.** [`Self::roll`] is the only thing that closes a
    /// volume, and it closes one by *replacing* the assembler
    /// [`Self::snapshot`] reads — so the report is the only way back to that
    /// volume, and an `Err` return drops the outcome carrying it. The volume
    /// then never appears anywhere: no error names it, the next round describes
    /// the volume after it, and a volume that finished assembling goes missing
    /// while the caller sees what looks like an ordinary transient fetch
    /// failure.
    ///
    /// Both round paths roll *before* they fetch — `poll` rolls on the probe
    /// listing and then lists the new directory, `fetch_notified` rolls on the
    /// chunk's own name and then downloads it — so in both, a failure lands
    /// after the assembler has already moved. One remedy covers both, because
    /// both build their report in a `PollOutcome` and both leave through an
    /// `Err` that discards it.
    ///
    /// # How far the guarantee actually reaches
    ///
    /// Only to the poller's own edge, and the distance is worth stating plainly:
    /// this survives **one or two consecutive failures**, which is the common
    /// case and the whole point, not an arbitrary number of them. A parked
    /// volume still dies with the poller or the round, at four places the poller
    /// cannot see — [`crate::chunk_feed`] rebuilding a retired feed
    /// after `RETRY_AFTER`, `retain_live` dropping a site, `finish_round`
    /// finding no feed to return the poller to, and `app_chunks` discarding a
    /// round whose fetch generation went stale. Feed retirement at three
    /// consecutive errors is what bounds the queue below, and it is also what
    /// ends the guarantee.
    ///
    /// # Why a queue and not a slot
    ///
    /// A slot has to overwrite, and overwriting is the very bug this exists to
    /// stop. An earlier version argued a slot could never be occupied at roll
    /// time, on the grounds that rolling again needs the current assembler to
    /// have a volume time, that a fresh one has none until a chunk is ingested,
    /// and that a round which ingests returns `Ok` and drains on the way. That
    /// last clause is false: `poll`'s mid-round failure path ingests a chunk and
    /// *then* leaves through `Err`, re-parking as it goes, so one round reaches
    /// "parked volume, assembler with a volume time" — from which the next
    /// notification rolls. Two volumes then want the slot, and the older is the
    /// one that would have been thrown away.
    ///
    /// So the order is explicit instead: oldest out first, and nothing is ever
    /// overwritten. Growth needs a round that both rolls and fails, every one of
    /// which counts toward the retirement that ends the feed at three.
    fn park_for_next_round(&mut self, outcome: &mut PollOutcome) {
        if let Some(closed) = outcome.closed.take() {
            log::debug!(
                "{}: volume {} closed in a round that failed; holding its report \
                 for a later one ({} now waiting)",
                self.site,
                closed.progress.volume.get(),
                self.pending_closed.len() + 1
            );
            // Front, not back: this one was drained before the ones already
            // queued were added, so it is the oldest of them.
            self.pending_closed.push_front(closed);
        }
    }

    /// Give a freshly closed volume to this round if it is not already carrying
    /// one, and queue it behind the others otherwise.
    ///
    /// The one place a `ClosedVolume` may be assigned, so that "never
    /// overwritten" is a property of the code rather than of an argument about
    /// which states are reachable. See [`Self::park_for_next_round`].
    fn deliver_or_queue(&mut self, outcome: &mut PollOutcome, closed: Option<ClosedVolume>) {
        let Some(closed) = closed else {
            return;
        };
        if outcome.closed.is_none() {
            outcome.closed = Some(closed);
        } else {
            self.pending_closed.push_back(closed);
        }
    }

    /// One round: no sleeping, no looping, no self-scheduling.
    pub async fn poll(&mut self, sources: &crate::sources::DataSources) -> Result<PollOutcome> {
        // Before the first `.await`, so merely polling this future installs the
        // crypto provider — `crate::tls` has a probe that depends on it.
        let _ = crate::archive::shared_client();

        let now = chrono::Utc::now().naive_utc();
        // A volume closed by an earlier round that then failed rides out on this
        // one, whichever way it leaves. Oldest first. See `park_for_next_round`.
        let mut outcome = PollOutcome {
            closed: self.pending_closed.pop_front(),
            ..Default::default()
        };

        let volume = match self.plan(now) {
            PollPlan::Discover => {
                let volume = match latest_volume(sources, &self.site).await {
                    Ok(volume) => volume,
                    Err(e) => {
                        self.consecutive_failures += 1;
                        self.park_for_next_round(&mut outcome);
                        return Err(e);
                    }
                };
                let mut started = VolumeAssembler::new(self.site.clone(), volume);
                started.set_selection(self.selection.clone());
                self.current = Some(started);
                outcome.rolled_to = Some(volume);
                volume
            }
            PollPlan::Fill { volume } => volume,
            PollPlan::ProbeNext { current, next } => {
                // Roll only when the next directory holds a volume that started
                // *later* than this one. An index the rotation has not yet
                // reused still holds the previous pass, whose start time is
                // older — that is not a new volume.
                let listed = match list_chunks(sources, &self.site, next).await {
                    Ok(listed) => listed,
                    Err(e) => {
                        self.consecutive_failures += 1;
                        self.park_for_next_round(&mut outcome);
                        return Err(e);
                    }
                };
                let current_time = self.current.as_ref().and_then(VolumeAssembler::volume_time);
                let started = listed
                    .first()
                    .is_some_and(|c| current_time.is_none_or(|t| c.volume_time() > t));
                if started {
                    let closed = self.roll(next);
                    self.deliver_or_queue(&mut outcome, closed);
                    outcome.rolled_to = Some(next);
                    next
                } else {
                    self.consecutive_failures = 0;
                    self.last_round_was_quiet = true;
                    outcome.progress = self.progress();
                    let _ = current;
                    return Ok(outcome);
                }
            }
        };

        let listed = match list_chunks(sources, &self.site, volume).await {
            Ok(listed) => listed,
            Err(e) => {
                self.consecutive_failures += 1;
                self.park_for_next_round(&mut outcome);
                return Err(e);
            }
        };

        // Held rather than returned from inside the loop: a chunk earlier in
        // the round may already have sealed a cut, and a seal is what clears
        // the snapshot cache. Returning straight out left that rebuild to
        // whoever asked next — the frame thread — so the round leaves by one
        // exit that warms it, and a flaky network keeps the stall the happy
        // path no longer has.
        let mut failure: Option<ChunkError> = None;
        for id in self.select(&listed) {
            let bytes = match download_chunk(sources, &id).await {
                Ok(bytes) => bytes,
                Err(ChunkError::Bucket(e)) => match fetch_disposition(&e) {
                    FetchDisposition::Skip => {
                        outcome.skipped += 1;
                        continue;
                    }
                    FetchDisposition::Abort => {
                        self.consecutive_failures += 1;
                        failure = Some(ChunkError::Bucket(e));
                        break;
                    }
                },
                Err(e) => {
                    self.consecutive_failures += 1;
                    failure = Some(e);
                    break;
                }
            };
            let Some(current) = self.current.as_mut() else {
                break;
            };
            match current.ingest(&id, &bytes) {
                Ok(o) if o.accepted => {
                    outcome.ingested += 1;
                    outcome.sealed_elevations.extend(o.sealed);
                    outcome.learned_coverage_pattern |= o.learned_coverage_pattern;
                }
                Ok(_) => {}
                // A chunk that will not decode is skipped, not fatal: the volume
                // is still worth what already arrived, and the next round will
                // not retry it.
                Err(e) => {
                    log::warn!("{}: chunk {} did not decode: {e:?}", self.site, id.name());
                    outcome.skipped += 1;
                }
            }
        }

        if let Some(e) = failure {
            // Only the warm, not `fill_sealed_angles`: the outcome is dropped
            // with the `Err`, so its angles and progress are unreachable, while
            // the cache the seals invalidated is state on the assembler and
            // outlives the round. `consecutive_failures` was already counted at
            // the break.
            //
            // **Known loss, not yet repaired: `sealed_elevations`.** A cut that
            // sealed earlier in this round is dropped here with the outcome, and
            // it does not come back. The frontend invalidates panes through
            // `render_dispatch::reset_panes_for_tilts`, which matches a pane's
            // elevation against *this round's* angles within
            // `ELEVATION_TOLERANCE` — so a lost 2.4° seal is not repaired by the
            // next round sealing 0.5°. That pane waits for another seal at its
            // own tilt, a volume period away, or for the volume to close. It is
            // left alone deliberately: parking these the way the closed volume
            // above is parked would have `fill_sealed_angles` resolve them
            // against whatever volume is current at delivery, which is the
            // cross-volume attribution the frontend's `apply_chunk_outcome`
            // documents at length — a wrong angle rather than a late one.
            self.warm_snapshot(&outcome);
            self.park_for_next_round(&mut outcome);
            return Err(e);
        }

        self.consecutive_failures = 0;
        self.last_round_was_quiet = outcome.ingested == 0;
        self.fill_sealed_angles(&mut outcome);
        self.warm_snapshot(&outcome);
        Ok(outcome)
    }
}

// ---------------------------------------------------------------------------
// Bucket access
// ---------------------------------------------------------------------------

/// The delimiter that turns a site listing into a directory listing.
const DELIMITER: &str = "/";

/// Every chunk in one volume's directory, ascending.
///
/// Unparseable names are dropped rather than failing the listing, matching
/// [`crate::archive`]'s handling of an unexpected key: a bucket is allowed to
/// contain something this code has never heard of.
pub async fn list_chunks(
    sources: &crate::sources::DataSources,
    site: &str,
    volume: VolumeIndex,
) -> Result<Vec<ChunkId>> {
    let client = crate::archive::shared_client();
    let bucket = sources.s3_bucket_url(&sources.level2_chunks_bucket);
    let prefix = volume.prefix(site);

    let keys =
        crate::archive::collect_keys(&bucket, &prefix, None, |url| get_text(client, url)).await?;

    let mut ids: Vec<ChunkId> = keys
        .iter()
        .filter_map(|key| {
            let name = key.rsplit('/').next()?;
            ChunkId::parse(site, volume, name)
        })
        .collect();
    ids.sort();
    Ok(ids)
}

/// Fetch one chunk's bytes.
pub async fn download_chunk(
    sources: &crate::sources::DataSources,
    id: &ChunkId,
) -> Result<Vec<u8>> {
    let client = crate::archive::shared_client();
    let url = sources.s3_object_url(&sources.level2_chunks_bucket, &id.key());
    Ok(crate::archive::get_bytes(client, url).await?)
}

/// Which volume directories a site currently has, ascending **numerically**.
///
/// S3 returns `CommonPrefixes` in UTF-8 order — `1, 10, 100, …, 2, 20, …` —
/// because the index is not zero-padded. Every caller wants rotation order, and
/// the sort is what turns the result into something a search can reason about.
pub async fn list_volume_indices(
    sources: &crate::sources::DataSources,
    site: &str,
) -> Result<Vec<VolumeIndex>> {
    let client = crate::archive::shared_client();
    let bucket = sources.s3_bucket_url(&sources.level2_chunks_bucket);
    let prefix = format!("{site}/");

    let prefixes = crate::archive::collect_common_prefixes(&bucket, &prefix, DELIMITER, |url| {
        get_text(client, url)
    })
    .await?;

    Ok(parse_volume_indices(&prefixes))
}

/// Pull the indices out of `{site}/{n}/` directory prefixes, sorted numerically.
///
/// Split from the request so the ordering — the thing S3 gets "wrong" from this
/// module's point of view — is testable without a socket.
pub(crate) fn parse_volume_indices(prefixes: &[String]) -> Vec<VolumeIndex> {
    let mut indices: Vec<VolumeIndex> = prefixes
        .iter()
        .filter_map(|p| {
            let mut parts = p.trim_end_matches('/').rsplit('/');
            VolumeIndex::new(parts.next()?.parse::<u16>().ok()?)
        })
        .collect();
    indices.sort();
    indices.dedup();
    indices
}

/// The newest volume start time in a directory, or `None` if it holds nothing
/// this code can read.
///
/// Takes the **maximum** name rather than the first. A listing comes back in
/// UTF-8 order, so the first key is the *oldest* name in the directory — which,
/// if the bucket does not empty a directory when the rotation reuses its index,
/// is a leftover from ~3.5 days ago. Ordering discovery on that would make a
/// freshly-written volume look stale and skip it.
async fn volume_time(
    sources: &crate::sources::DataSources,
    site: &str,
    volume: VolumeIndex,
) -> Result<Option<chrono::NaiveDateTime>> {
    Ok(list_chunks(sources, site, volume)
        .await?
        .iter()
        .map(ChunkId::volume_time)
        .max())
}

/// The volume a site is currently writing.
///
/// Two phases. One delimited listing establishes which directories exist, which
/// is what makes the second phase an ordinary search rather than upstream's
/// blind probe of all 999 indices. Then a pivot search over those directories'
/// start times: they ascend with the rotation except across the single wrap
/// point, so the newest is the element before the minimum.
///
/// Approximate is good enough, and that is deliberate. A discovery that lands on
/// the previous volume costs one extra roll — the poller advances as soon as it
/// sees a newer directory — so this trades exactness for a bounded number of
/// requests rather than the other way round.
pub async fn latest_volume(
    sources: &crate::sources::DataSources,
    site: &str,
) -> Result<VolumeIndex> {
    let indices = list_volume_indices(sources, site).await?;
    if indices.is_empty() {
        return Err(ChunkError::NoVolumes {
            site: site.to_string(),
        });
    }
    let mut probes = 0usize;
    let found = newest_by_rotation(&indices, |v| {
        probes += 1;
        volume_time(sources, site, v)
    })
    .await?;
    log::debug!(
        "chunk discovery for {site}: {} directories, {probes} probes -> {found:?}",
        indices.len()
    );
    found.ok_or_else(|| ChunkError::NoVolumes {
        site: site.to_string(),
    })
}

/// Pivot search for the newest entry of a rotated-ascending ladder.
///
/// `probe` is asked for an entry's sort key and may answer `None` for a
/// directory holding nothing readable — a real state for one being written right
/// now. A `None` is treated as "older than anything", which keeps the search
/// converging; the cost of guessing wrong is one extra roll, not a wrong volume
/// forever.
///
/// Separated from the I/O so the ordering cases — not rotated, rotated in the
/// middle, rotated at either end, a hole — are testable against canned data,
/// the way [`crate::archive::collect_keys`] separates paging from fetching.
pub(crate) async fn newest_by_rotation<F, Fut>(
    indices: &[VolumeIndex],
    mut probe: F,
) -> Result<Option<VolumeIndex>>
where
    F: FnMut(VolumeIndex) -> Fut,
    Fut: Future<Output = Result<Option<chrono::NaiveDateTime>>>,
{
    let n = indices.len();
    match n {
        0 => return Ok(None),
        1 => return Ok(Some(indices[0])),
        _ => {}
    }

    // `None` sorts below every real time, so a hole never wins a comparison.
    let first = probe(indices[0]).await?;
    let last = probe(indices[n - 1]).await?;
    if first <= last {
        // Not rotated within the window we can see: the ladder ascends, so the
        // last directory is the newest.
        return Ok(Some(indices[n - 1]));
    }

    // Rotated: everything before the wrap is newer than `last` and everything
    // from the wrap on is not, so the first entry that is not newer than `last`
    // is the oldest — and the newest is the one before it.
    //
    // Compared against the fixed `last` rather than against a moving `hi`, which
    // would cost a second probe per step for the same answer.
    let (mut lo, mut hi) = (0usize, n - 1);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if probe(indices[mid]).await? > last {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    Ok(Some(indices[(lo + n - 1) % n]))
}

/// `archive::get_text` with the chunk module's error type.
async fn get_text(
    client: &reqwest::Client,
    url: String,
) -> std::result::Result<String, ArchiveError> {
    crate::archive::get_text(client, url).await
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
