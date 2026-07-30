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
fn coverage_pattern_from(
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
                ((i + 1) as u8, cut.elevation_angle_degrees() as f32, range)
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
        expected: Option<usize>,
    },
    /// A full rotation, frozen. Radials are *moved* out of the map rather than
    /// copied — a sweep is megabytes of gate bytes.
    Sealed(nexrad_model::data::Sweep),
    /// Terminated, or closed with the volume, short of its radial count.
    ///
    /// Kept as a diagnostic and **never** placed in a snapshot.
    /// `render_nrot_to_image` computes `avg_spacing_deg = 360.0 / num_radials`
    /// and wraps its azimuthal neighbour lookups with `.rem_euclid(num_radials)`,
    /// so a half-received cut both halves the computed shear and stitches the
    /// last received radial to the first — manufacturing a rotation signature
    /// out of a gap. It bails only at zero radials, so nothing downstream would
    /// catch this.
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
/// pins that against a real VCP-212 volume.
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
        if let Some(vcp) = contents.coverage_pattern {
            let vcp = self.coverage_pattern.insert(vcp);
            self.chunk_map = ElevationChunkMap::from_coverage_pattern(vcp);
            outcome.learned_coverage_pattern = true;
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
    /// anything is showing.
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
        let scan = std::sync::Arc::new(nexrad_model::data::Scan::new(vcp, sweeps));
        self.cached = Some(std::sync::Arc::clone(&scan));
        scan
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
/// A round that rolls describes **two** volumes: the one that closed, in
/// [`Self::closed`], and the one now being assembled, in every other field. They
/// are deliberately not merged — a caller reading a whole volume wants the first
/// and a caller reading one tilt wants the second.
#[derive(Debug, Clone, Default)]
pub struct PollOutcome {
    pub ingested: usize,
    /// Elevation numbers whose cut completed this round, ascending. **The render
    /// trigger**, and the test for whether a snapshot is worth building.
    pub sealed_elevations: Vec<u8>,
    /// Their angles, parallel to `sealed_elevations`.
    pub sealed_angles: Vec<f32>,
    /// The volume rolled this round; `snapshot` now describes a new volume.
    pub rolled_to: Option<VolumeIndex>,
    /// The volume that just closed, **with the scan it closed as**. Where
    /// `volume_complete` for a finished volume is reported, and the only way to
    /// reach that volume: the roll that produced this replaced the assembler
    /// `snapshot` reads. See [`ClosedVolume`].
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
}

impl ChunkPoller {
    pub fn new(site: impl Into<String>) -> Self {
        Self {
            site: site.into(),
            current: None,
            consecutive_failures: 0,
            last_round_was_quiet: false,
            selection: CutSelection::All,
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
    /// **seal** also clears that cache, so the cache is warm at roll time only
    /// because the caller asked for a snapshot in the round the volume's last cut
    /// sealed. That does hold — [`Self::plan`] runs before any ingestion, so it
    /// cannot see `saw_scan_end` until a *later* round, and the final seal and the
    /// roll are therefore never the same round. A caller that stops reading
    /// snapshots would pay one cold copy per volume here instead.
    pub(crate) fn roll(&mut self, to: VolumeIndex) -> Option<ClosedVolume> {
        let closed = self.current.as_mut().map(|current| {
            // `close`, not `progress`: it is what resolves a still-open cut to
            // `Abandoned`, so `progress.abandoned` names exactly the cuts the scan
            // below is missing. What keeps a short cut *out* of that scan is
            // `snapshot`'s own sealed-only filter, so these two lines commute —
            // this is the readable order, not a load-bearing one.
            let progress = current.close();
            let scan = progress.volume_complete.then(|| current.snapshot());
            ClosedVolume { progress, scan }
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

        let mut outcome = PollOutcome::default();
        if id.site() != self.site {
            return Ok(outcome);
        }

        if self.is_stale_notification(id) {
            return Ok(outcome);
        }
        if self.should_roll_to(id) {
            outcome.closed = self.roll(id.volume());
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
            }
            Ok(_) => {}
            Err(e) => {
                log::warn!("{}: chunk {} did not decode: {e:?}", self.site, id.name());
                outcome.skipped += 1;
            }
        }

        self.consecutive_failures = 0;
        self.fill_sealed_angles(&mut outcome);
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

    /// One round: no sleeping, no looping, no self-scheduling.
    pub async fn poll(&mut self, sources: &crate::sources::DataSources) -> Result<PollOutcome> {
        // Before the first `.await`, so merely polling this future installs the
        // crypto provider — `crate::tls` has a probe that depends on it.
        let _ = crate::archive::shared_client();

        let now = chrono::Utc::now().naive_utc();
        let mut outcome = PollOutcome::default();

        let volume = match self.plan(now) {
            PollPlan::Discover => {
                let volume = match latest_volume(sources, &self.site).await {
                    Ok(volume) => volume,
                    Err(e) => {
                        self.consecutive_failures += 1;
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
                        return Err(e);
                    }
                };
                let current_time = self.current.as_ref().and_then(VolumeAssembler::volume_time);
                let started = listed
                    .first()
                    .is_some_and(|c| current_time.is_none_or(|t| c.volume_time() > t));
                if started {
                    outcome.closed = self.roll(next);
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
                return Err(e);
            }
        };

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
                        return Err(ChunkError::Bucket(e));
                    }
                },
                Err(e) => {
                    self.consecutive_failures += 1;
                    return Err(e);
                }
            };
            let Some(current) = self.current.as_mut() else {
                break;
            };
            match current.ingest(&id, &bytes) {
                Ok(o) if o.accepted => {
                    outcome.ingested += 1;
                    outcome.sealed_elevations.extend(o.sealed);
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

        self.consecutive_failures = 0;
        self.last_round_was_quiet = outcome.ingested == 0;
        self.fill_sealed_angles(&mut outcome);
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
    let bucket = sources.level2_chunks_bucket.clone();
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
    let url = crate::sources::DataSources::s3_object_url(&sources.level2_chunks_bucket, &id.key());
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
    let bucket = sources.level2_chunks_bucket.clone();
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
mod tests {
    use super::*;

    fn vol(n: u16) -> VolumeIndex {
        VolumeIndex::new(n).expect("valid index")
    }

    // -- VolumeIndex --------------------------------------------------------

    /// The rotation is 1..=999 inclusive, and 0 is not a volume.
    #[test]
    fn volume_indices_outside_the_rotation_are_refused() {
        assert!(VolumeIndex::new(0).is_none());
        assert!(VolumeIndex::new(1000).is_none());
        assert_eq!(vol(1).get(), 1);
        assert_eq!(vol(999).get(), 999);
    }

    /// Wraps to 1, not to 0 — which is not a volume — and not to 1000.
    #[test]
    fn the_volume_after_999_is_1() {
        assert_eq!(vol(999).next(), vol(1));
        assert_eq!(vol(1).next(), vol(2));
    }

    /// The trailing slash is the whole point: without it `KTLX/9` is a prefix of
    /// every key under `KTLX/90/` and `KTLX/99/`, so one listing would return
    /// three volumes' chunks and the assembler would refuse most of them.
    #[test]
    fn a_volume_prefix_cannot_match_a_longer_index() {
        let prefix = vol(9).prefix("KTLX");
        assert_eq!(prefix, "KTLX/9/");
        assert!("KTLX/9/20260728-181234-001-S".starts_with(&prefix));
        assert!(!"KTLX/90/20260728-181234-001-S".starts_with(&prefix));
        assert!(!"KTLX/99/20260728-181234-001-S".starts_with(&prefix));
    }

    /// Indices are interpolated, never zero-padded — the bucket's own scheme.
    #[test]
    fn volume_prefixes_are_not_zero_padded() {
        assert_eq!(vol(1).prefix("KTLX"), "KTLX/1/");
        assert_eq!(vol(10).prefix("KTLX"), "KTLX/10/");
        assert_eq!(vol(100).prefix("KTLX"), "KTLX/100/");
    }

    // -- ChunkId ------------------------------------------------------------

    /// Fails on an off-by-one in either slice: the sequence is `name[16..19]`
    /// and the type is the last character.
    #[test]
    fn a_chunk_name_splits_into_time_sequence_and_kind() {
        let id = ChunkId::parse("KTLX", vol(42), "20260728-181234-007-I").expect("parses");
        assert_eq!(
            id.volume_time(),
            chrono::NaiveDate::from_ymd_opt(2026, 7, 28)
                .unwrap()
                .and_hms_opt(18, 12, 34)
                .unwrap()
        );
        assert_eq!(id.sequence(), 7);
        assert_eq!(id.kind(), ChunkKind::Intermediate);
        assert_eq!(id.site(), "KTLX");
        assert_eq!(id.volume(), vol(42));
    }

    /// The sequence is zero-padded in the name but a number here, so 001 and 055
    /// order correctly rather than lexicographically.
    #[test]
    fn a_zero_padded_sequence_reads_as_a_number() {
        let first = ChunkId::parse("KTLX", vol(1), "20260728-181234-001-S").expect("parses");
        let last = ChunkId::parse("KTLX", vol(1), "20260728-181234-055-E").expect("parses");
        assert_eq!(first.sequence(), 1);
        assert_eq!(last.sequence(), 55);
        assert_eq!(first.kind(), ChunkKind::Start);
        assert_eq!(last.kind(), ChunkKind::End);
        assert!(first < last);
    }

    /// Names come from bucket keys, so nothing here may panic on one that does
    /// not fit — it is dropped from the listing instead.
    #[test]
    fn a_name_that_does_not_fit_is_refused_rather_than_panicking() {
        for bad in [
            "",
            "2026",
            "20260728-181234-001-",       // 20 bytes: no type character
            "20260728-181234-001-X",      // unknown type
            "20260728_181234-001-S",      // wrong separator
            "20260728-181234-001-S-more", // trailing junk still parses the head
            "20260728-181234-abc-S",      // non-numeric sequence
            "not-a-timestamp-001-S",
            "2026072é-181234-001-S", // non-ASCII inside the first 20 bytes
        ] {
            assert!(
                ChunkId::parse("KTLX", vol(1), bad).is_none(),
                "{bad:?} should not parse"
            );
        }
    }

    /// A chunk of an older volume sorts before a chunk of a newer one whatever
    /// their sequences say. Fails for an `Ord` that derives over the fields in
    /// declaration order, which would compare the rotating index first.
    #[test]
    fn chunks_order_by_volume_time_before_sequence() {
        let old_last = ChunkId::parse("KTLX", vol(999), "20260728-180000-055-E").expect("parses");
        let new_first = ChunkId::parse("KTLX", vol(1), "20260728-181234-001-S").expect("parses");
        assert!(
            old_last < new_first,
            "the rotation makes index 999 older than index 1; ordering on the \
             index would invert them"
        );
    }

    #[test]
    fn a_key_round_trips() {
        let key = "KTLX/42/20260728-181234-007-I";
        let id = ChunkId::from_key(key).expect("parses");
        assert_eq!(id.key(), key);
    }

    /// A key whose shape is wrong is dropped, not fatal.
    #[test]
    fn a_key_with_the_wrong_shape_is_refused() {
        for bad in [
            "KTLX/20260728-181234-001-S",          // no volume segment
            "KTLX/0/20260728-181234-001-S",        // index outside the rotation
            "KTLX/1000/20260728-181234-001-S",     // index outside the rotation
            "KTLX/abc/20260728-181234-001-S",      // non-numeric index
            "KTLX/42/extra/20260728-181234-001-S", // too many segments
        ] {
            assert!(ChunkId::from_key(bad).is_none(), "{bad:?} should not parse");
        }
    }

    // -- discovery ----------------------------------------------------------

    /// S3 hands directories back in UTF-8 order because the index is not
    /// zero-padded, so `10` arrives between `1` and `2`. Everything downstream
    /// treats the list as rotation order, which it only is once sorted.
    #[test]
    fn volume_directories_are_sorted_numerically_not_lexicographically() {
        let listed = ["KTLX/1/", "KTLX/10/", "KTLX/100/", "KTLX/2/", "KTLX/20/"]
            .map(String::from)
            .to_vec();
        let parsed = parse_volume_indices(&listed);
        assert_eq!(
            parsed.iter().map(|v| v.get()).collect::<Vec<_>>(),
            vec![1, 2, 10, 20, 100],
            "left in S3's order, the pivot search is searching an array that is \
             not a rotation of anything"
        );
    }

    /// A directory naming something outside the rotation is dropped, not fatal.
    #[test]
    fn unparseable_volume_directories_are_dropped() {
        let listed = ["KTLX/1/", "KTLX/0/", "KTLX/1000/", "KTLX/abc/", "KTLX/"]
            .map(String::from)
            .to_vec();
        assert_eq!(
            parse_volume_indices(&listed)
                .iter()
                .map(|v| v.get())
                .collect::<Vec<_>>(),
            vec![1]
        );
    }

    /// Drive `newest_by_rotation` over a canned ladder, counting probes.
    ///
    /// The future is polled by hand rather than through an executor — the probe
    /// is immediate, so it never yields — mirroring `archive::tests::paginate`.
    fn search(times: &[(u16, i64)]) -> (Option<VolumeIndex>, usize) {
        let base = chrono::NaiveDate::from_ymd_opt(2026, 7, 28)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap();
        let indices: Vec<VolumeIndex> = times.iter().map(|(i, _)| vol(*i)).collect();
        let table: std::collections::HashMap<u16, i64> = times.iter().copied().collect();
        let probes = std::cell::Cell::new(0usize);

        let fut = newest_by_rotation(&indices, |v| {
            probes.set(probes.get() + 1);
            let at = table.get(&v.get()).copied();
            async move {
                // A negative minute stands for a directory holding nothing
                // readable.
                Ok(at
                    .filter(|m| *m >= 0)
                    .map(|m| base + chrono::Duration::minutes(m)))
            }
        });
        let mut fut = Box::pin(fut);
        let waker = std::task::Waker::noop();
        let mut cx = std::task::Context::from_waker(waker);
        let out = match fut.as_mut().poll(&mut cx) {
            std::task::Poll::Ready(v) => v.expect("the canned probe never fails"),
            std::task::Poll::Pending => panic!("the fixture probe never yields"),
        };
        (out, probes.get())
    }

    /// Before the rotation has wrapped, the ladder simply ascends and the last
    /// directory is the newest.
    #[test]
    fn an_unwrapped_ladder_ends_at_its_newest_volume() {
        let times: Vec<(u16, i64)> = (1..=20).map(|i| (i, i as i64)).collect();
        let (found, probes) = search(&times);
        assert_eq!(found, Some(vol(20)));
        assert_eq!(probes, 2, "an unwrapped ladder needs only its two ends");
    }

    /// The case a plain maximum-by-index gets wrong. The write head is at 12, so
    /// 13..20 still hold the previous pass and are ~3.5 days older than 1..12.
    #[test]
    fn a_wrapped_ladder_finds_the_write_head() {
        let mut times: Vec<(u16, i64)> = (1..=12).map(|i| (i, 1000 + i as i64)).collect();
        times.extend((13..=20).map(|i| (i, i as i64 - 13)));
        times.sort();

        let (found, probes) = search(&times);
        assert_eq!(
            found,
            Some(vol(12)),
            "the newest volume is the one before the wrap, not the highest index"
        );
        assert!(probes <= 8, "probe count grew to {probes}");
    }

    /// The wrap sitting at the very start: only index 1 belongs to the new pass.
    #[test]
    fn a_ladder_that_just_wrapped_finds_the_first_volume() {
        let mut times: Vec<(u16, i64)> = vec![(1, 1000)];
        times.extend((2..=20).map(|i| (i, i as i64)));
        let (found, _) = search(&times);
        assert_eq!(found, Some(vol(1)));
    }

    /// One directory is the answer without any search at all.
    #[test]
    fn a_single_directory_is_the_answer() {
        let (found, probes) = search(&[(7, 5)]);
        assert_eq!(found, Some(vol(7)));
        assert_eq!(probes, 0);
    }

    #[test]
    fn no_directories_means_no_volume() {
        let (found, _) = search(&[]);
        assert_eq!(found, None);
    }

    /// A directory being written right now can list nothing readable. Treated as
    /// older than everything, so the search still converges — the cost of
    /// guessing wrong here is one extra roll, not a wrong volume forever.
    #[test]
    fn a_directory_with_nothing_readable_does_not_derail_the_search() {
        let mut times: Vec<(u16, i64)> = (1..=12).map(|i| (i, 1000 + i as i64)).collect();
        times.push((13, -1)); // unreadable
        times.extend((14..=20).map(|i| (i, i as i64 - 13)));
        times.sort();
        let (found, _) = search(&times);
        assert_eq!(found, Some(vol(12)));
    }

    // -- decode -------------------------------------------------------------

    /// Neither magic sequence is present, so the bytes are named rather than
    /// guessed at.
    #[test]
    fn bytes_that_are_neither_shape_are_refused() {
        for bad in [
            b"hello world!!".as_slice(),
            b"".as_slice(),
            b"AR".as_slice(),
        ] {
            let err = decode_chunk("x", bad).expect_err("should not decode");
            assert!(
                matches!(err, ChunkError::UnrecognizedChunk { .. }),
                "got {err:?}"
            );
        }
    }

    /// `volume::File::records` slices `&data[24..]` with no length check, so
    /// without the guard this panics inside `nexrad-data` rather than returning.
    #[test]
    fn a_start_chunk_too_short_for_its_header_does_not_panic() {
        let err = decode_chunk("short", b"AR2V0006.").expect_err("should not decode");
        assert!(
            matches!(err, ChunkError::ShortStartChunk { .. }),
            "got {err:?}"
        );
    }

    /// The magic bytes choose the decoder, not the name's `S`/`I`/`E` letter.
    ///
    /// `ShortStartChunk` is the discriminator: it can only be reached from the
    /// volume-header route. So a short `AR2` buffer *named as an intermediate*
    /// must still produce it, and a bare LDM record *named as a start* must not
    /// — even though it is also shorter than a volume header, which is exactly
    /// what routing by filename would trip over.
    ///
    /// The mis-decode this prevents is silent, not loud: an intermediate chunk
    /// has no volume header, so the header route would slice 24 bytes off the
    /// front of a real record and read a length from the middle of its payload.
    #[test]
    fn the_magic_bytes_choose_the_decoder_not_the_name() {
        let short_headered = b"AR2V0006.".to_vec();
        assert!(
            short_headered.len() < std::mem::size_of::<volume::Header>(),
            "the fixture has to be shorter than a header for this to discriminate"
        );
        let err = decode_chunk("20260728-181234-007-I", &short_headered).expect_err("too short");
        assert!(
            matches!(err, ChunkError::ShortStartChunk { .. }),
            "an AR2 prefix must take the volume-header route whatever the name's \
             type letter says, got {err:?}"
        );

        // A size-prefixed BZ record, also shorter than a volume header.
        let mut record = vec![0u8, 0, 0, 8];
        record.extend_from_slice(b"BZh9garbage");
        assert!(record.len() < std::mem::size_of::<volume::Header>());
        let outcome = decode_chunk("20260728-181234-001-S", &record);
        assert!(
            !matches!(outcome, Err(ChunkError::ShortStartChunk { .. })),
            "a BZ record was routed through the volume-header path because its \
             name said `S`; a real intermediate chunk would be silently \
             mis-sliced, got {outcome:?}"
        );
    }

    // -- assembly -----------------------------------------------------------

    use nexrad_model::data::{Radial, RadialStatus};

    const VOLUME_TIME: &str = "20260728-181234";

    fn volume_time() -> chrono::NaiveDateTime {
        chrono::NaiveDateTime::parse_from_str(VOLUME_TIME, "%Y%m%d-%H%M%S").expect("fixture time")
    }

    /// Copy a radial with a different status. `Radial` has no setters, so every
    /// field is read back off the original; if one is ever added and missed
    /// here, the digest test below is what notices.
    fn with_status(r: &Radial, status: RadialStatus) -> Radial {
        Radial::new(
            r.collection_timestamp(),
            r.azimuth_number(),
            r.azimuth_angle_degrees(),
            r.azimuth_spacing_degrees(),
            status,
            r.elevation_number(),
            r.elevation_angle_degrees(),
            r.reflectivity().cloned(),
            r.velocity().cloned(),
            r.spectrum_width().cloned(),
            r.differential_reflectivity().cloned(),
            r.differential_phase().cloned(),
            r.correlation_coefficient().cloned(),
            r.clutter_filter_power().cloned(),
        )
    }

    /// The same radial reassigned to another elevation cut, so one sweep's radials
    /// can stand in for a cut the golden scan does not have.
    fn renumber(r: &Radial, elevation: u8) -> Radial {
        Radial::new(
            r.collection_timestamp(),
            r.azimuth_number(),
            r.azimuth_angle_degrees(),
            r.azimuth_spacing_degrees(),
            r.radial_status(),
            elevation,
            r.elevation_angle_degrees(),
            r.reflectivity().cloned(),
            r.velocity().cloned(),
            r.spectrum_width().cloned(),
            r.differential_reflectivity().cloned(),
            r.differential_phase().cloned(),
            r.correlation_coefficient().cloned(),
            r.clutter_filter_power().cloned(),
        )
    }

    /// `volumetric::tests::golden_scan` re-sliced into the chunks the bucket
    /// actually publishes.
    ///
    /// 120 radials per chunk is the real size — confirmed against the live
    /// bucket by `live_a_start_chunk_decodes_and_carries_the_coverage_pattern`,
    /// which decodes an intermediate chunk to exactly 120 — so a super-resolution
    /// cut is 6 chunks and a standard one 3. The slicing is the bucket's, not an
    /// arbitrary split.
    ///
    /// Two edits the fixture needs. Sequence 1 is a synthetic start chunk
    /// carrying the VCP and no radials, which is what a real one looks like. And
    /// each sweep's last radial is rebuilt with `ElevationEnd` — `ScanEnd` on the
    /// final sweep — because the generator marks every radial
    /// `IntermediateRadialData`, so without this nothing would ever seal.
    fn golden_chunks() -> Vec<(u16, ChunkKind, ChunkContents)> {
        let scan = crate::volumetric::tests::golden_scan();
        let sweeps = scan.sweeps();
        let mut out: Vec<(u16, ChunkKind, ChunkContents)> = vec![(
            1,
            ChunkKind::Start,
            ChunkContents {
                radials: Vec::new(),
                coverage_pattern: Some(crate::volumetric::tests::vcp()),
            },
        )];

        let mut sequence = 2u16;
        for (si, sweep) in sweeps.iter().enumerate() {
            let last_sweep = si + 1 == sweeps.len();
            let radials = sweep.radials();
            let terminator = if last_sweep {
                RadialStatus::ScanEnd
            } else {
                RadialStatus::ElevationEnd
            };
            let rebuilt: Vec<Radial> = radials
                .iter()
                .enumerate()
                .map(|(i, r)| {
                    if i + 1 == radials.len() {
                        with_status(r, terminator)
                    } else {
                        r.clone()
                    }
                })
                .collect();
            for group in rebuilt.chunks(120) {
                out.push((
                    sequence,
                    ChunkKind::Intermediate,
                    ChunkContents {
                        radials: group.to_vec(),
                        coverage_pattern: None,
                    },
                ));
                sequence += 1;
            }
        }
        if let Some(last) = out.last_mut() {
            last.1 = ChunkKind::End;
        }
        out
    }

    /// Feed chunks to a fresh assembler in the order given.
    fn assemble(chunks: Vec<(u16, ChunkKind, ChunkContents)>) -> VolumeAssembler {
        let mut a = VolumeAssembler::new("KTLX", vol(42));
        for (sequence, kind, contents) in chunks {
            a.ingest_contents(sequence, kind, volume_time(), contents);
        }
        a
    }

    fn digest(a: &mut VolumeAssembler) -> u64 {
        let scan = a.snapshot();
        crate::volumetric::tests::fnv1a64(&crate::volumetric::compute_echo_tops(&scan))
    }

    /// **The claim this module rests on**: assembling a volume from chunks
    /// produces the same `Scan` that decoding the whole volume does.
    ///
    /// Driven through `compute_echo_tops` and pinned on
    /// `volumetric::tests::golden_echo_tops_grid_is_pinned`'s own digest, so it
    /// exercises gridding, newest-wins dedup, beam heights and interpolation and
    /// demands bit-identity — not merely "some sweeps arrived". The NaN
    /// assertion additionally pins that ascending elevation-number order
    /// reproduces newest-wins, which is the invariant `find_sweep`'s `.rev()`
    /// and `VolumeCube` both stand on.
    #[test]
    fn the_assembled_golden_volume_reproduces_the_pinned_digest() {
        let mut a = assemble(golden_chunks());
        let progress = a.progress();
        assert!(
            progress.volume_complete,
            "the volume did not complete: {progress:?}"
        );
        assert_eq!(progress.late_radials_dropped, 0);

        let scan = a.snapshot();
        assert_eq!(
            scan.sweeps().len(),
            crate::volumetric::tests::golden_scan().sweeps().len()
        );
        let grid = crate::volumetric::compute_echo_tops(&scan);
        assert_eq!(
            crate::volumetric::tests::fnv1a64(&grid),
            0x4559ce366731e030,
            "assembling from chunks does not reproduce the volume the archive \
             path decodes"
        );
        let defined: usize = grid.values.iter().flatten().filter(|v| !v.is_nan()).count();
        assert_eq!(defined, 4680);
        assert!(
            grid.values[300][120].is_nan(),
            "the SAILS repeat no longer displaces the first 0.5° sweep, so the \
             emitted sweep order is not newest-last"
        );
    }

    /// Out-of-order delivery is the premise of the whole module. Fails for
    /// `Sweep::from_radials`, which groups by consecutive runs and would emit a
    /// sweep per fragment.
    #[test]
    fn a_shuffled_chunk_order_assembles_the_same_volume() {
        let mut chunks = golden_chunks();
        // A fixed permutation rather than a random one: no `rand` dependency,
        // and a failure reproduces.
        let mid = chunks.len() / 2;
        let mut shuffled: Vec<_> = Vec::with_capacity(chunks.len());
        let back = chunks.split_off(mid);
        let mut front = chunks.into_iter();
        let mut back = back.into_iter();
        loop {
            match (back.next(), front.next()) {
                (None, None) => break,
                (b, f) => shuffled.extend(b.into_iter().chain(f)),
            }
        }
        assert_eq!(shuffled.len(), golden_chunks().len());

        let mut a = assemble(shuffled);
        assert!(a.progress().volume_complete);
        assert_eq!(digest(&mut a), 0x4559ce366731e030);
    }

    /// The extreme case of the same property.
    #[test]
    fn a_reversed_chunk_order_assembles_the_same_volume() {
        let mut chunks = golden_chunks();
        chunks.reverse();
        let mut a = assemble(chunks);
        assert!(a.progress().volume_complete);
        assert_eq!(digest(&mut a), 0x4559ce366731e030);
    }

    /// A re-listed volume re-delivers chunks already seen. Fails for
    /// `Sweep::merge`, which extends without deduping and would double every
    /// sweep.
    #[test]
    fn ingesting_every_chunk_twice_changes_nothing() {
        let mut doubled = golden_chunks();
        doubled.extend(golden_chunks());
        let mut a = assemble(doubled);
        assert_eq!(
            a.progress().chunks_ingested,
            golden_chunks().len(),
            "a repeat was counted as new work"
        );
        assert_eq!(digest(&mut a), 0x4559ce366731e030);
    }

    /// The safety property, stated directly: a cut short of its radial count is
    /// never in the emitted `Scan`, so `find_sweep` and `RenderInput::extract`
    /// cannot reach one and NROT cannot be rendered from one.
    #[test]
    fn a_partial_cut_never_reaches_the_snapshot() {
        // Drop the chunk carrying the 0.5° cut's terminator.
        let mut chunks = golden_chunks();
        let dropped = chunks
            .iter()
            .position(|(_, _, c)| {
                c.radials.iter().any(|r| {
                    r.elevation_number() == 1 && r.radial_status() == RadialStatus::ElevationEnd
                })
            })
            .expect("the 0.5° cut has a terminator chunk");
        chunks.remove(dropped);

        let mut a = assemble(chunks);
        let progress = a.progress();
        assert!(
            !progress.sealed_elevations.contains(&1),
            "an unterminated cut sealed anyway: {progress:?}"
        );
        assert!(!progress.volume_complete);

        let scan = a.snapshot();
        assert!(
            scan.sweeps().iter().all(|s| s.elevation_number() != 1),
            "the partial 0.5° cut reached the Scan"
        );
    }

    /// The terminator alone is not enough: it says the RDA finished the cut, not
    /// that every chunk of it arrived. Fails for a seal rule that trusts the
    /// status by itself.
    #[test]
    fn a_terminator_without_the_radial_count_does_not_seal() {
        let mut chunks = golden_chunks();
        // Keep the 0.5° cut's terminator chunk, drop an interior one.
        let interior = chunks
            .iter()
            .position(|(_, _, c)| {
                c.radials.first().is_some_and(|r| r.elevation_number() == 1)
                    && c.radials
                        .iter()
                        .all(|r| r.radial_status() != RadialStatus::ElevationEnd)
            })
            .expect("the 0.5° cut spans several chunks");
        chunks.remove(interior);

        let a = assemble(chunks);
        let progress = a.progress();
        assert!(
            !progress.sealed_elevations.contains(&1),
            "a cut missing 120 of its 720 radials sealed on the terminator alone"
        );
    }

    /// The final cut of a volume carries `ScanEnd`, not `ElevationEnd`. Reading
    /// only `ElevationEnd` leaves the topmost cut open forever, so
    /// `volume_complete` never fires and everything gated on it — echo tops, the
    /// Level III refetch, the loop append — is dead.
    ///
    /// **This is only half of that claim, and the half it is not covers a bug that
    /// shipped.** Everything here stops at the assembler: the flag is read off
    /// `VolumeAssembler::progress` in the same breath as `snapshot`, so of course
    /// the two agree. What the frontend actually reads is
    /// `PollOutcome::closed`, produced one layer up by [`ChunkPoller::roll`] —
    /// which for a long time reported the completion and dropped the volume in the
    /// same statement, so the flag fired and everything gated on it was dead
    /// anyway. `a_roll_hands_back_the_volume_it_closed` is the missing layer, and
    /// any future claim of the form "`volume_complete` reaches a reader" belongs
    /// there rather than here.
    #[test]
    fn the_final_cut_seals_on_scan_end() {
        let mut a = assemble(golden_chunks());
        let top = crate::volumetric::tests::golden_scan().sweeps().len() as u8;
        assert!(
            a.is_elevation_sealed(top),
            "the last cut never sealed, so the volume can never complete"
        );
        assert!(a.progress().saw_scan_end);
        assert!(a.progress().volume_complete);
        let _ = a.snapshot();
    }

    /// **A roll must hand back the volume whose completion it reports.**
    ///
    /// [`ChunkPoller::roll`] is the only producer of [`PollOutcome::closed`], and
    /// both callers set that field in the same statement that replaces the
    /// assembler [`ChunkPoller::snapshot`] reads. Whatever the roll does not hand
    /// back is gone: the caller learns a volume completed at the exact instant it
    /// can no longer read it, and everything gated on `volume_complete` — echo
    /// tops, the Level III refetch, the loop append — is dead on a healthy feed.
    ///
    /// Asserted against the *live* snapshot as well, because that is the read the
    /// frontend used to make and the number it got back. Reverting `roll` to
    /// return only [`VolumeProgress`] cannot pass this.
    #[test]
    fn a_roll_hands_back_the_volume_it_closed() {
        let mut poller = ChunkPoller::new("KTLX");
        poller.current = Some(assemble(golden_chunks()));
        let expected = crate::volumetric::tests::golden_scan().sweeps().len();

        let closed = poller.roll(vol(43)).expect("a volume was open");

        assert!(
            closed.progress.volume_complete,
            "the fixture must close complete, or this proves nothing: {:?}",
            closed.progress
        );
        let scan = closed
            .scan
            .as_ref()
            .expect("a completed volume hands back its scan");
        assert_eq!(
            scan.sweeps().len(),
            expected,
            "the roll reported a completed volume and did not hand it over"
        );
        // And the scan really is the volume the progress describes, not some
        // other one: same cuts, in the same order.
        let handed: Vec<u8> = scan.sweeps().iter().map(|s| s.elevation_number()).collect();
        assert_eq!(
            handed, closed.progress.sealed_elevations,
            "the scan and the progress in one `ClosedVolume` describe different \
             volumes"
        );
        assert_eq!(
            poller.snapshot().map(|s| s.sweeps().len()),
            Some(0),
            "the poller is on the new volume now — which is why the closed one \
             has to travel out with the outcome rather than be read back off it"
        );
    }

    /// A volume that closed short reports the cuts it lost and hands back **no
    /// scan**.
    ///
    /// No scan because nothing reads one: every consumer gates on
    /// `volume_complete` first, and building it would deep-copy every sealed
    /// `Sweep` — `close` clears [`VolumeAssembler::snapshot`]'s cache exactly when
    /// it abandoned a cut, so the cold rebuild landed precisely on the volumes that
    /// get discarded.
    ///
    /// This deliberately gives up a guarantee an earlier version of this test
    /// asserted — that a partial cut is absent from the scan a short close hands
    /// over — because there is now no such scan to inspect. Nothing consumed it. The
    /// underlying property is unchanged and still pinned where it belongs, on
    /// `snapshot` itself, by `a_partial_cut_never_reaches_the_snapshot`.
    ///
    /// The mutation this kills is `roll` reading `progress()` instead of `close()`:
    /// the still-`Open` cut would be missing from `progress.abandoned`, so a caller
    /// asking "what is this volume missing?" would be told "nothing". Verified by
    /// mutation, along with the discovery that the *order* of `close` and `snapshot`
    /// is not observable and so is not claimed to be.
    #[test]
    fn a_roll_leaves_an_abandoned_cut_out_of_the_scan_it_hands_back() {
        // Drop the chunk carrying the 0.5° cut's terminator, so that cut is still
        // open when the volume closes.
        let mut chunks = golden_chunks();
        let dropped = chunks
            .iter()
            .position(|(_, _, c)| {
                c.radials.iter().any(|r| {
                    r.elevation_number() == 1 && r.radial_status() == RadialStatus::ElevationEnd
                })
            })
            .expect("the 0.5° cut has a terminator chunk");
        chunks.remove(dropped);

        let mut poller = ChunkPoller::new("KTLX");
        poller.current = Some(assemble(chunks));
        let closed = poller.roll(vol(43)).expect("a volume was open");

        assert!(
            closed.progress.abandoned.iter().any(|a| a.elevation == 1),
            "precondition: the 0.5° cut must have closed short: {:?}",
            closed.progress
        );
        assert!(
            !closed.progress.volume_complete,
            "a volume with an abandoned cut reported complete"
        );
        assert!(
            closed.scan.is_none(),
            "a volume that closed short built a snapshot anyway — a deep copy of \
             every sealed sweep, on the one path where `close` had just cleared \
             the cache, for a volume every consumer discards"
        );
    }

    /// **`volume_complete` is not "whole", and `roll` reports both.**
    ///
    /// Under a narrow [`CutSelection`] a volume completes on the cuts it was asked
    /// for — one, for a single 0.5° pane, which is the *default* layout. That is the
    /// right answer for the tilts on screen and the wrong one for anything that
    /// integrates the column, and the two used to be one flag, leaving the
    /// distinction to callers. A frontend that cached such a volume for its loops
    /// then handed it to echo tops the moment the pane changed product.
    #[test]
    fn a_narrow_selection_completes_without_the_volume_being_whole() {
        let narrow = CutSelection::Tilts(vec![0.5]);
        let mut poller = ChunkPoller::new("KTLX");
        poller.set_selection(narrow.clone());
        poller.current = Some(narrow_volume(&narrow));

        let closed = poller.roll(vol(43)).expect("a volume was open");

        assert!(
            closed.progress.volume_complete,
            "precondition: the cuts the selection asked for must all have sealed, \
             or the distinction below is not the one under test: {:?}",
            closed.progress
        );
        assert!(
            !closed.progress.whole_volume_complete,
            "a volume holding only the 0.5° cuts reported itself whole, so every \
             product that integrates the column may read it: {:?}",
            closed.progress
        );
        assert_eq!(
            closed.scan.as_ref().map(|s| s.sweeps().len()),
            Some(2),
            "precondition: the narrowed volume must really be short — two 0.5° \
             cuts out of a four-cut pattern"
        );
    }

    /// A volume assembled the way a 0.5°-only feed assembles one: the start chunk,
    /// both halves of the 0.5° split cut, and the volume's terminator.
    ///
    /// The terminator is the faithful part. It carries the *final* cut's radials,
    /// and it arrives even under a narrow selection because its sequence runs past
    /// the planned ranges the coverage pattern implies — SAILS and MRLE inserts are
    /// not in message 5, so real sequence numbers overrun the map, and
    /// `ElevationChunkMap::wants` deliberately answers `true` for anything it cannot
    /// place. That is what lets a narrow volume ever see `ScanEnd` and so ever
    /// report complete, and it is also why the top cut is present but partial.
    fn narrow_volume(selection: &CutSelection) -> VolumeAssembler {
        let vcp = vcp_with(&[(0.5, true), (0.5, true), (4.0, false), (6.0, false)]);
        let map = ElevationChunkMap::from_coverage_pattern(&vcp).expect("cuts");
        assert_eq!(
            map.wanted_elevations(selection),
            vec![1, 2],
            "precondition: the selection must want both halves of the split cut"
        );

        let mut a = VolumeAssembler::new("KTLX", vol(42));
        a.set_selection(selection.clone());
        a.ingest_contents(
            1,
            ChunkKind::Start,
            volume_time(),
            ChunkContents {
                radials: Vec::new(),
                coverage_pattern: Some(vcp),
            },
        );

        let scan = crate::volumetric::tests::golden_scan();
        // Both 0.5° cuts, each ending on `ElevationEnd`, at the sequences the map
        // places them at — checked against `wants_chunk`, so the fixture cannot
        // quietly stop being what a narrow feed downloads.
        let mut sequence = 2u16;
        for elevation in [1u8, 2u8] {
            let sweep = &scan.sweeps()[0];
            let radials: Vec<Radial> = sweep
                .radials()
                .iter()
                .enumerate()
                .map(|(i, r)| {
                    let r = renumber(r, elevation);
                    if i + 1 == sweep.radials().len() {
                        with_status(&r, RadialStatus::ElevationEnd)
                    } else {
                        r
                    }
                })
                .collect();
            for group in radials.chunks(120) {
                assert!(
                    a.wants_chunk(sequence),
                    "chunk {sequence} of the 0.5° cut was not wanted"
                );
                a.ingest_contents(
                    sequence,
                    ChunkKind::Intermediate,
                    volume_time(),
                    ChunkContents {
                        radials: group.to_vec(),
                        coverage_pattern: None,
                    },
                );
                sequence += 1;
            }
        }

        // The cuts in between are never fetched.
        for skipped in sequence..sequence + 3 {
            assert!(
                !a.wants_chunk(skipped),
                "chunk {skipped} belongs to a cut the selection skipped and was \
                 wanted anyway"
            );
        }

        // The terminator, past the map's ranges and so wanted regardless.
        let top = scan.sweeps().last().expect("the golden scan has sweeps");
        let tail: Vec<Radial> = top.radials()[..20]
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let r = renumber(r, 4);
                if i == 19 {
                    with_status(&r, RadialStatus::ScanEnd)
                } else {
                    r
                }
            })
            .collect();
        let terminator = 900u16;
        assert!(
            a.wants_chunk(terminator),
            "a sequence the map cannot place must be wanted, or a narrow volume \
             never sees ScanEnd and never completes at all"
        );
        a.ingest_contents(
            terminator,
            ChunkKind::End,
            volume_time(),
            ChunkContents {
                radials: tail,
                coverage_pattern: None,
            },
        );
        a
    }

    /// The counterweight: with everything asked for, the two flags agree. Without
    /// this the assertion above could pass on a flag that is simply always false.
    #[test]
    fn a_whole_volume_reports_both_flags() {
        let mut poller = ChunkPoller::new("KTLX");
        poller.current = Some(assemble(golden_chunks()));
        let closed = poller.roll(vol(43)).expect("a volume was open");
        assert!(closed.progress.volume_complete);
        assert!(
            closed.progress.whole_volume_complete,
            "a volume with every cut sealed contiguously from 1 did not report \
             whole, so nothing would ever reach a volume integral: {:?}",
            closed.progress
        );
    }

    /// A volume joined mid-flight is complete for a narrow selection that happens
    /// to want only cuts it caught, and is still not whole.
    ///
    /// The contiguity clause is what separates them. Without it the missing lower
    /// cuts would be invisible and `compute_echo_tops` would integrate tilts 3..23
    /// and report every column's top far too low.
    #[test]
    fn a_volume_joined_mid_flight_is_never_whole() {
        let chunks: Vec<_> = golden_chunks()
            .into_iter()
            .filter(|(_, _, c)| c.radials.first().is_none_or(|r| r.elevation_number() >= 3))
            .collect();
        let a = assemble(chunks);
        assert!(a.progress().saw_scan_end, "the fixture must reach the end");
        assert!(
            !a.is_whole_volume_complete(),
            "a volume missing its lowest cuts reported whole: {:?}",
            a.progress()
        );
    }

    /// One closing round describes **two** volumes, which is why the closed one
    /// cannot be represented by the live snapshot.
    ///
    /// This is the shape `poll` produces whenever the probe round finds the new
    /// directory already filling — after an error backoff of up to
    /// [`MAX_BACKOFF`], several cuts' worth. A consumer that trusted
    /// `closed.volume_complete` and then read the snapshot got a one-cut volume:
    /// no error, no NaN, a plausible wrong answer from every product
    /// [`crate::types::RadarProduct::reads_whole_volume`] names.
    #[test]
    fn a_closing_round_can_also_seal_a_cut_of_the_new_volume() {
        let mut poller = ChunkPoller::new("KTLX");
        poller.current = Some(assemble(golden_chunks()));
        let closed = poller.roll(vol(43)).expect("a volume was open");
        assert!(closed.progress.volume_complete);

        // What `poll` does next in the same round: list the new volume's
        // directory and ingest whatever is already in it.
        let current = poller.current.as_mut().expect("the roll started one");
        let mut sealed: Vec<u8> = Vec::new();
        for (sequence, kind, contents) in golden_chunks() {
            let outcome = current.ingest_contents(sequence, kind, volume_time(), contents);
            sealed.extend(outcome.sealed);
            if !sealed.is_empty() {
                break;
            }
        }
        assert_eq!(sealed, vec![1], "the new volume's first cut did not seal");
        assert_eq!(
            poller.snapshot().map(|s| s.sweeps().len()),
            Some(1),
            "the live snapshot on a closing round is the new volume, one cut in"
        );
        assert_eq!(
            closed.scan.as_ref().map(|s| s.sweeps().len()),
            Some(crate::volumetric::tests::golden_scan().sweeps().len()),
            "and the closed volume is unaffected by what the new one ingests"
        );
    }

    /// Reported per chunk, and only for the cut that finished on it — the
    /// frontend uses this to decide which panes to invalidate and whether a
    /// snapshot is worth building at all.
    #[test]
    fn a_cut_is_reported_sealed_on_the_chunk_that_finishes_it() {
        let mut a = VolumeAssembler::new("KTLX", vol(42));
        let mut seals: Vec<(u16, Vec<u8>)> = Vec::new();
        for (sequence, kind, contents) in golden_chunks() {
            let outcome = a.ingest_contents(sequence, kind, volume_time(), contents);
            if !outcome.sealed.is_empty() {
                seals.push((sequence, outcome.sealed));
            }
        }
        let sealed_order: Vec<u8> = seals.iter().flat_map(|(_, e)| e.clone()).collect();
        assert_eq!(
            sealed_order,
            (1..=crate::volumetric::tests::golden_scan().sweeps().len() as u8).collect::<Vec<_>>(),
            "cuts must seal once each, in acquisition order: {seals:?}"
        );
    }

    /// A volume joined mid-flight has no entry for the cuts that finished before
    /// the first chunk arrived. Without the contiguity clause it would report
    /// complete, and `compute_echo_tops` would integrate only the upper tilts
    /// and report every column's top far too low.
    #[test]
    fn a_volume_joined_mid_flight_never_reports_complete() {
        let chunks: Vec<_> = golden_chunks()
            .into_iter()
            .filter(|(_, _, c)| c.radials.first().is_none_or(|r| r.elevation_number() >= 3))
            .collect();
        let a = assemble(chunks);
        let progress = a.progress();
        assert!(
            progress.saw_scan_end,
            "the fixture must still reach the end of the volume"
        );
        assert!(
            !progress.volume_complete,
            "a volume missing its lowest cuts reported complete: {progress:?}"
        );
    }

    /// Radials for a cut that already sealed are dropped, not merged: the sealed
    /// sweep may already be inside a `Scan` a render is holding.
    #[test]
    fn late_radials_for_a_sealed_cut_are_dropped() {
        let mut a = assemble(golden_chunks());
        let before = a.snapshot();
        let replay = golden_chunks();
        // Re-deliver one cut's chunks under fresh sequence numbers so the
        // idempotence check does not short-circuit them.
        let mut next_sequence = 900u16;
        for (_, kind, contents) in replay {
            if contents
                .radials
                .first()
                .is_some_and(|r| r.elevation_number() == 1)
            {
                a.ingest_contents(next_sequence, kind, volume_time(), contents);
                next_sequence += 1;
            }
        }
        let after = a.snapshot();
        assert!(
            std::sync::Arc::ptr_eq(&before, &after),
            "a sealed cut was reopened, so a Scan already handed out changed"
        );
        assert!(a.progress().late_radials_dropped > 0);
    }

    /// The cache is what keeps a poll cheap: `Sweep: Clone` deep-copies every
    /// gate byte, so rebuilding per poll would be hundreds of megabytes of
    /// memcpy across a volume.
    #[test]
    fn the_snapshot_is_shared_until_a_cut_seals() {
        let mut a = VolumeAssembler::new("KTLX", vol(42));
        let chunks = golden_chunks();
        // Up to but not including the chunk that seals the first cut.
        let seal_at = chunks
            .iter()
            .position(|(_, _, c)| {
                c.radials
                    .iter()
                    .any(|r| r.radial_status() == RadialStatus::ElevationEnd)
            })
            .expect("some chunk seals a cut");
        for (sequence, kind, contents) in chunks.iter().take(seal_at).cloned() {
            a.ingest_contents(sequence, kind, volume_time(), contents);
        }
        let first = a.snapshot();
        let second = a.snapshot();
        assert!(
            std::sync::Arc::ptr_eq(&first, &second),
            "two snapshots with no seal between them rebuilt the volume"
        );

        let (sequence, kind, contents) = chunks[seal_at].clone();
        a.ingest_contents(sequence, kind, volume_time(), contents);
        let third = a.snapshot();
        assert!(
            !std::sync::Arc::ptr_eq(&first, &third),
            "a seal did not invalidate the cached snapshot"
        );
    }

    /// A leftover from the previous pass through this rotating index carries
    /// elevation numbers that would collide with the volume being assembled, so
    /// it must be refused outright rather than merged.
    #[test]
    fn a_chunk_from_another_volume_is_refused() {
        let mut a = VolumeAssembler::new("KTLX", vol(42));
        let chunks = golden_chunks();
        let (sequence, kind, contents) = chunks[1].clone();
        assert!(
            a.ingest_contents(sequence, kind, volume_time(), contents)
                .accepted
        );
        let sealed_before = a.progress().sealed_elevations;

        let stale = volume_time() - chrono::Duration::days(3);
        for (sequence, kind, contents) in chunks.into_iter().skip(2) {
            let outcome = a.ingest_contents(sequence, kind, stale, contents);
            assert!(
                !outcome.accepted,
                "a chunk from volume {stale} was merged into volume {}",
                volume_time()
            );
        }
        assert_eq!(a.progress().sealed_elevations, sealed_before);
        assert_eq!(a.progress().chunks_ingested, 1);
    }

    // -- poller -------------------------------------------------------------

    /// Seed a poller's assembler with one chunk so it has a volume time.
    fn primed(volume: VolumeIndex) -> ChunkPoller {
        let mut p = ChunkPoller::resume("KTLX", volume);
        let (sequence, kind, contents) = golden_chunks()[1].clone();
        p.current
            .as_mut()
            .expect("resume seeds an assembler")
            .ingest_contents(sequence, kind, volume_time(), contents);
        p
    }

    #[test]
    fn a_cold_poller_plans_discovery() {
        let p = ChunkPoller::new("KTLX");
        assert_eq!(p.plan(volume_time()), PollPlan::Discover);
    }

    /// While a volume is still filling, keep listing it; once it has ended, look
    /// at the next index instead.
    #[test]
    fn a_filling_volume_is_listed_and_a_finished_one_probes_the_next() {
        let mut p = primed(vol(42));
        assert_eq!(p.plan(volume_time()), PollPlan::Fill { volume: vol(42) });

        for (sequence, kind, contents) in golden_chunks().into_iter().skip(2) {
            p.current
                .as_mut()
                .unwrap()
                .ingest_contents(sequence, kind, volume_time(), contents);
        }
        assert_eq!(
            p.plan(volume_time()),
            PollPlan::ProbeNext {
                current: vol(42),
                next: vol(43)
            }
        );
    }

    /// Rollover reaches the plan, not just `VolumeIndex::next`.
    #[test]
    fn the_volume_probed_after_999_is_1() {
        let mut p = primed(vol(999));
        for (sequence, kind, contents) in golden_chunks().into_iter().skip(2) {
            p.current
                .as_mut()
                .unwrap()
                .ingest_contents(sequence, kind, volume_time(), contents);
        }
        assert_eq!(
            p.plan(volume_time()),
            PollPlan::ProbeNext {
                current: vol(999),
                next: vol(1)
            }
        );
    }

    /// A backgrounded app comes back to a volume long gone. Stepping one index
    /// per round would take many rounds to catch up, so discovery is re-run.
    #[test]
    fn a_stale_volume_replans_discovery() {
        let p = primed(vol(42));
        let much_later = volume_time() + chrono::Duration::minutes(30);
        assert_eq!(p.plan(much_later), PollPlan::Discover);
        assert_eq!(
            p.plan(volume_time() + chrono::Duration::minutes(2)),
            PollPlan::Fill { volume: vol(42) },
            "a volume two minutes old is simply the current one"
        );
    }

    /// A re-listed directory returns everything every round; only what is new
    /// gets fetched.
    #[test]
    fn an_already_ingested_chunk_is_not_selected_again() {
        let p = primed(vol(42));
        let listed: Vec<ChunkId> = (1..=3)
            .map(|seq| {
                ChunkId::parse("KTLX", vol(42), &format!("{VOLUME_TIME}-{seq:03}-I"))
                    .expect("parses")
            })
            .collect();

        let selected = p.select(&listed);
        assert_eq!(
            selected.iter().map(ChunkId::sequence).collect::<Vec<_>>(),
            vec![1, 3],
            "sequence 2 was already ingested by `primed`"
        );
    }

    /// A directory the rotation has not yet cleared holds the previous pass's
    /// chunks beside the new ones. Merging them would collide elevation numbers.
    #[test]
    fn a_leftover_from_the_previous_pass_is_not_selected() {
        let p = primed(vol(42));
        let stale = (volume_time() - chrono::Duration::days(3))
            .format("%Y%m%d-%H%M%S")
            .to_string();
        let listed = vec![
            ChunkId::parse("KTLX", vol(42), &format!("{VOLUME_TIME}-005-I")).expect("parses"),
            ChunkId::parse("KTLX", vol(42), &format!("{stale}-006-I")).expect("parses"),
        ];
        assert_eq!(
            p.select(&listed)
                .iter()
                .map(ChunkId::sequence)
                .collect::<Vec<_>>(),
            vec![5],
        );
    }

    /// A listed-then-missing key is ordinary — S3 is eventually consistent and
    /// the rotation retires keys — so it skips. Anything else ends the round,
    /// which is what stops a 503 being read as an empty volume.
    #[test]
    fn a_missing_chunk_is_skipped_and_a_server_error_is_not() {
        assert_eq!(
            fetch_disposition(&ArchiveError::NotFound("k".into())),
            FetchDisposition::Skip
        );
        assert_eq!(
            fetch_disposition(&ArchiveError::Status {
                status: reqwest::StatusCode::SERVICE_UNAVAILABLE,
                url: "u".into(),
                body: None,
            }),
            FetchDisposition::Abort
        );
        assert_eq!(
            fetch_disposition(&ArchiveError::MalformedListing("x".into())),
            FetchDisposition::Abort
        );
    }

    /// Backing off on quiet, not on error: an empty round is the ordinary state
    /// between cuts and across the gap between volumes, and treating it as a
    /// failure would retreat from a feed that is working perfectly.
    #[test]
    fn the_interval_backs_off_on_failure_and_on_quiet_but_not_on_progress() {
        let mut p = ChunkPoller::new("KTLX");
        assert_eq!(p.suggested_interval(), POLL_INTERVAL);

        p.last_round_was_quiet = true;
        assert_eq!(p.suggested_interval(), QUIET_INTERVAL);

        p.last_round_was_quiet = false;
        p.consecutive_failures = 1;
        assert_eq!(p.suggested_interval(), POLL_INTERVAL * 2);
        p.consecutive_failures = 3;
        assert_eq!(p.suggested_interval(), POLL_INTERVAL * 8);
        p.consecutive_failures = 99;
        assert_eq!(
            p.suggested_interval(),
            MAX_BACKOFF,
            "the backoff has to stop somewhere"
        );

        p.consecutive_failures = 0;
        assert_eq!(p.suggested_interval(), POLL_INTERVAL);
    }

    /// A notification for a newer volume rolls the assembler without any probe —
    /// the chunk's own name carries the volume start time, which is what
    /// distinguishes a new volume from the previous pass through the same
    /// rotating index.
    #[test]
    fn a_notified_chunk_from_a_newer_volume_rolls_the_assembler() {
        let p = primed(vol(42));
        let later = (volume_time() + chrono::Duration::minutes(6))
            .format("%Y%m%d-%H%M%S")
            .to_string();
        let next = ChunkId::parse("KTLX", vol(43), &format!("{later}-001-S")).expect("parses");

        // `plan` would have said `Fill` — the current volume has not ended — so
        // this roll comes from the notification alone.
        assert_eq!(p.plan(volume_time()), PollPlan::Fill { volume: vol(42) });
        assert!(
            p.should_roll_to(&next),
            "a chunk from a later-started volume must roll the assembler"
        );
    }

    /// And one from an older volume is refused: an index the rotation has not
    /// reused still holds the previous pass, and a replayed message must not drag
    /// the assembler backwards.
    #[test]
    fn a_notified_chunk_from_an_older_volume_is_ignored() {
        let p = primed(vol(42));
        let earlier = (volume_time() - chrono::Duration::days(3))
            .format("%Y%m%d-%H%M%S")
            .to_string();
        let stale = ChunkId::parse("KTLX", vol(41), &format!("{earlier}-001-S")).expect("parses");
        assert!(!p.should_roll_to(&stale));
        assert!(p.is_stale_notification(&stale));
    }

    /// A chunk of the volume already being assembled is neither a roll nor stale.
    #[test]
    fn a_notified_chunk_of_the_current_volume_is_just_ingested() {
        let p = primed(vol(42));
        let same =
            ChunkId::parse("KTLX", vol(42), &format!("{VOLUME_TIME}-009-I")).expect("parses");
        assert!(!p.should_roll_to(&same));
        assert!(!p.is_stale_notification(&same));
    }

    // -- selective download --------------------------------------------------

    /// A VCP whose first `super_res` cuts are half-degree and the rest standard,
    /// which is the shape every real pattern has.
    fn vcp_with(cuts: &[(f32, bool)]) -> VolumeCoveragePattern {
        let elevation_cuts = cuts
            .iter()
            .map(|(angle, super_res)| {
                ElevationCut::new(
                    *angle as f64,
                    ChannelConfiguration::ConstantPhase,
                    WaveformType::CS,
                    0.0,
                    *super_res,
                    false,
                    false,
                    false,
                    0,
                    0,
                    0.0,
                    0.0,
                    0.0,
                    0.0,
                    0.0,
                    0.0,
                    false,
                    0,
                    false,
                    0,
                    false,
                    false,
                )
            })
            .collect();
        VolumeCoveragePattern::new(
            35,
            0,
            0.5,
            PulseWidth::Short,
            false,
            0,
            false,
            0,
            false,
            false,
            0,
            false,
            false,
            elevation_cuts,
        )
    }

    /// The mapping this rests on, against the volume shape measured on the live
    /// bucket: KTLX VCP 35, twelve cuts, six chunks each for the six
    /// super-resolution ones and three for the six standard ones, sealing on
    /// sequences 7, 13, 19, 25, 31, 37, 40, 43, 46, 49, 52, 55.
    #[test]
    fn the_chunk_map_reproduces_a_measured_volume() {
        let mut cuts: Vec<(f32, bool)> = (0..6).map(|i| (0.5 + i as f32 * 0.4, true)).collect();
        cuts.extend((0..6).map(|i| (1.8 + i as f32 * 0.9, false)));
        let vcp = vcp_with(&cuts);
        let map = ElevationChunkMap::from_coverage_pattern(&vcp).expect("a pattern with cuts");
        assert_eq!(map.cut_count(), 12);

        // The start chunk belongs to no cut.
        assert_eq!(map.cut_for(1), None);
        // Each cut ends on the sequence the live volume sealed it on.
        for (elevation, last) in [
            (1u8, 7u16),
            (2, 13),
            (3, 19),
            (4, 25),
            (5, 31),
            (6, 37),
            (7, 40),
            (8, 43),
            (9, 46),
            (10, 49),
            (11, 52),
            (12, 55),
        ] {
            assert_eq!(
                map.cut_for(last).map(|(e, _)| e),
                Some(elevation),
                "sequence {last} should be the end of cut {elevation}"
            );
            assert_ne!(
                map.cut_for(last + 1).map(|(e, _)| e),
                Some(elevation),
                "cut {elevation} ran past sequence {last}"
            );
        }
    }

    /// The traffic claim, stated as a number: a pane on the lowest tilt needs
    /// thirteen chunks of fifty-five.
    #[test]
    fn one_tilt_wants_a_fraction_of_the_volume() {
        let mut cuts: Vec<(f32, bool)> = (0..2).map(|_| (0.5, true)).collect();
        cuts.extend((0..4).map(|i| (1.3 + i as f32 * 0.4, true)));
        cuts.extend((0..6).map(|i| (1.8 + i as f32 * 0.9, false)));
        let vcp = vcp_with(&cuts);
        let map = ElevationChunkMap::from_coverage_pattern(&vcp).expect("cuts");

        let low = CutSelection::Tilts(vec![0.5]);
        let wanted = (1..=55).filter(|s| map.wants(*s, &low)).count();
        assert_eq!(
            wanted, 13,
            "a 0.5° pane should want the start chunk and both halves of the \
             split cut, and nothing else"
        );
        assert_eq!(
            (1..=55)
                .filter(|s| map.wants(*s, &CutSelection::All))
                .count(),
            55,
            "asking for everything must still take everything"
        );
    }

    /// The start chunk is never skipped: it carries the coverage pattern, and
    /// without it there is no map to skip anything by.
    #[test]
    fn the_start_chunk_is_always_wanted() {
        let vcp = vcp_with(&[(0.5, true), (4.0, false)]);
        let map = ElevationChunkMap::from_coverage_pattern(&vcp).expect("cuts");
        assert!(map.wants(1, &CutSelection::Tilts(vec![90.0])));
        // And so is a sequence past the end of the pattern — skipping something
        // unplaceable would be guessing.
        assert!(map.wants(999, &CutSelection::Tilts(vec![90.0])));
    }

    /// A placeholder pattern lists no cuts, so nothing can be mapped and nothing
    /// may be skipped.
    #[test]
    fn a_pattern_with_no_cuts_yields_no_map() {
        assert!(
            ElevationChunkMap::from_coverage_pattern(&placeholder_coverage_pattern(212)).is_none()
        );
    }

    /// Before the start chunk lands there is no map, so every chunk is wanted
    /// whatever the selection says.
    #[test]
    fn nothing_is_skipped_until_the_coverage_pattern_arrives() {
        let mut a = VolumeAssembler::new("KTLX", vol(42));
        a.set_selection(CutSelection::Tilts(vec![0.5]));
        for sequence in 1..=55 {
            assert!(
                a.wants_chunk(sequence),
                "chunk {sequence} was skipped with no coverage pattern to judge by"
            );
        }
    }

    /// The split cut's other half is wanted too: both halves sit at the same
    /// angle, and a pane switching from reflectivity to velocity must not have
    /// to wait for the next volume.
    #[test]
    fn both_halves_of_a_split_cut_are_wanted_together() {
        let vcp = vcp_with(&[(0.5, true), (0.5, true), (4.0, false)]);
        let map = ElevationChunkMap::from_coverage_pattern(&vcp).expect("cuts");
        let low = CutSelection::Tilts(vec![0.5]);
        assert_eq!(map.wanted_elevations(&low), vec![1, 2]);
    }

    /// With cuts deliberately skipped, "complete" means every cut that was asked
    /// for — contiguity is meaningless once there are holes by design.
    #[test]
    fn a_selective_volume_completes_on_the_cuts_it_asked_for() {
        let vcp = vcp_with(&[(0.5, true), (4.0, false), (6.0, false)]);
        let mut a = VolumeAssembler::new("KTLX", vol(42));
        a.set_selection(CutSelection::Tilts(vec![0.5]));

        // Start chunk carrying the pattern, then the 0.5° cut, then the volume
        // ends — cuts 2 and 3 never arrive because they were never fetched.
        a.ingest_contents(
            1,
            ChunkKind::Start,
            volume_time(),
            ChunkContents {
                radials: Vec::new(),
                coverage_pattern: Some(vcp),
            },
        );
        assert!(!a.is_volume_complete(), "nothing has been assembled yet");

        let scan = crate::volumetric::tests::golden_scan();
        let sweep = &scan.sweeps()[0];
        let radials: Vec<Radial> = sweep
            .radials()
            .iter()
            .enumerate()
            .map(|(i, r)| {
                if i + 1 == sweep.radials().len() {
                    with_status(r, RadialStatus::ScanEnd)
                } else {
                    r.clone()
                }
            })
            .collect();
        for (n, group) in radials.chunks(120).enumerate() {
            a.ingest_contents(
                2 + n as u16,
                ChunkKind::Intermediate,
                volume_time(),
                ChunkContents {
                    radials: group.to_vec(),
                    coverage_pattern: None,
                },
            );
        }

        assert!(
            a.is_volume_complete(),
            "the only cut asked for is sealed and the volume ended, but it did \
             not report complete: {:?}",
            a.progress()
        );
    }

    /// The pairing that keeps a selective volume away from `compute_echo_tops`:
    /// under `All`, a volume with a hole is not complete.
    ///
    /// Contiguity is what carries this rather than the planned cut list, and
    /// deliberately so — SAILS and MRLE cuts are inserted at runtime, so the
    /// number of sweeps a volume produces need not match the number its message 5
    /// lists, and requiring the planned set could leave a volume that will never
    /// complete. `ScanEnd` marks the true final cut, so seeing it with 1..=n
    /// sealed and no gaps genuinely is the whole volume.
    #[test]
    fn a_volume_with_a_hole_is_incomplete_when_everything_was_asked_for() {
        let scan = crate::volumetric::tests::golden_scan();
        let mut a = VolumeAssembler::new("KTLX", vol(42));
        a.set_selection(CutSelection::All);
        a.ingest_contents(
            1,
            ChunkKind::Start,
            volume_time(),
            ChunkContents {
                radials: Vec::new(),
                coverage_pattern: Some(crate::volumetric::tests::vcp()),
            },
        );

        // Every cut but the second, with the last radial of the last one marking
        // the end of the volume — the shape a lost chunk leaves behind.
        let sweeps = scan.sweeps();
        let mut sequence = 2u16;
        for (si, sweep) in sweeps.iter().enumerate() {
            if si == 1 {
                continue;
            }
            let last_sweep = si + 1 == sweeps.len();
            let radials: Vec<Radial> = sweep
                .radials()
                .iter()
                .enumerate()
                .map(|(i, r)| {
                    if i + 1 == sweep.radials().len() {
                        with_status(
                            r,
                            if last_sweep {
                                RadialStatus::ScanEnd
                            } else {
                                RadialStatus::ElevationEnd
                            },
                        )
                    } else {
                        r.clone()
                    }
                })
                .collect();
            for group in radials.chunks(120) {
                a.ingest_contents(
                    sequence,
                    ChunkKind::Intermediate,
                    volume_time(),
                    ChunkContents {
                        radials: group.to_vec(),
                        coverage_pattern: None,
                    },
                );
                sequence += 1;
            }
        }

        assert!(a.progress().saw_scan_end, "the fixture must reach the end");
        assert!(
            !a.is_volume_complete(),
            "a volume missing cut 2 reported complete, so `compute_echo_tops` \
             would integrate a volume with a hole in it: {:?}",
            a.progress()
        );
    }

    /// Widening the selection mid-volume backfills within it rather than waiting
    /// for the next one — the skipped chunks are still in the bucket.
    #[test]
    fn widening_the_selection_makes_the_skipped_chunks_wanted_again() {
        let vcp = vcp_with(&[(0.5, true), (4.0, false)]);
        let mut a = VolumeAssembler::new("KTLX", vol(42));
        a.ingest_contents(
            1,
            ChunkKind::Start,
            volume_time(),
            ChunkContents {
                radials: Vec::new(),
                coverage_pattern: Some(vcp),
            },
        );

        a.set_selection(CutSelection::Tilts(vec![0.5]));
        assert!(!a.wants_chunk(8), "the 4.0° cut should be skipped");
        a.set_selection(CutSelection::Tilts(vec![0.5, 4.0]));
        assert!(
            a.wants_chunk(8),
            "a pane switching to 4.0° must not wait for the next volume"
        );
    }

    // -- live ---------------------------------------------------------------
    //
    // Run with:
    //   cargo test -p rustdar-radar --lib -- --ignored --nocapture chunks::tests::live_

    /// The claim this whole module rests on, mirroring
    /// `archive::tests::live_listing_needs_no_credentials`. If the chunk bucket
    /// ever required signing, every other live test here would fail with a
    /// confusing decode error while this one names the cause.
    #[ignore = "hits the live unidata-nexrad-level2-chunks S3 bucket"]
    #[tokio::test]
    async fn live_chunk_bucket_allows_anonymous_listing() {
        let sources = crate::sources::DataSources::production();
        crate::tls::init();
        let url =
            crate::archive::list_url_delimited(&sources.level2_chunks_bucket, "KTLX/", "/", None)
                .expect("url");
        let response = crate::archive::shared_client()
            .get(&url)
            .send()
            .await
            .expect("request should reach S3");
        println!("anonymous delimited LIST -> {}", response.status());
        assert!(
            response.status().is_success(),
            "anonymous listing was refused; this module would need SigV4"
        );
    }

    /// Discovery lands on a volume the radar is actually writing.
    ///
    /// Catches the bucket dropping `CommonPrefixes`, the search regressing to a
    /// linear scan, the name format changing, and — the reason the ladder is
    /// sorted numerically — a regression to lexicographic order, which would
    /// pick a volume hours or days stale.
    #[ignore = "hits the live unidata-nexrad-level2-chunks S3 bucket"]
    #[tokio::test]
    async fn live_discovery_finds_a_current_volume() {
        let sources = crate::sources::DataSources::production();
        crate::tls::init();

        let indices = list_volume_indices(&sources, "KTLX")
            .await
            .expect("listing volume directories");
        println!("KTLX has {} volume directories", indices.len());
        assert!(
            !indices.is_empty(),
            "no volume directories; the delimiter or prefix is wrong"
        );

        let volume = latest_volume(&sources, "KTLX").await.expect("discovery");
        let chunks = list_chunks(&sources, "KTLX", volume)
            .await
            .expect("listing the current volume");
        let newest = chunks.last().expect("a current volume holds chunks");
        let age = chrono::Utc::now().naive_utc() - newest.volume_time();
        println!(
            "volume {} started {} ({} min ago), {} chunks, newest {}",
            volume.get(),
            newest.volume_time(),
            age.num_minutes(),
            chunks.len(),
            newest.name(),
        );
        assert!(
            age < chrono::Duration::minutes(30),
            "discovery picked a volume {} minutes old; it is not the write head",
            age.num_minutes()
        );
    }

    /// A volume's directory holds a start chunk at sequence 1 and its chunks
    /// come back in sequence order.
    #[ignore = "hits the live unidata-nexrad-level2-chunks S3 bucket"]
    #[tokio::test]
    async fn live_a_volume_starts_at_sequence_one_and_is_ordered() {
        let sources = crate::sources::DataSources::production();
        crate::tls::init();
        let volume = latest_volume(&sources, "KTLX").await.expect("discovery");
        let chunks = list_chunks(&sources, "KTLX", volume)
            .await
            .expect("listing");

        assert!(!chunks.is_empty(), "the current volume listed no chunks");
        assert!(
            chunks.len() <= 200,
            "a volume should hold roughly 55 chunks, got {}",
            chunks.len()
        );
        let sequences: Vec<u16> = chunks.iter().map(ChunkId::sequence).collect();
        let mut sorted = sequences.clone();
        sorted.sort_unstable();
        assert_eq!(sequences, sorted, "list_chunks returned them out of order");

        let first = &chunks[0];
        println!(
            "first chunk: {} (seq {}, {:?})",
            first.name(),
            first.sequence(),
            first.kind()
        );
        assert_eq!(first.sequence(), 1);
        assert_eq!(
            first.kind(),
            ChunkKind::Start,
            "sequence 1 must be the start chunk — it is the only carrier of the \
             coverage pattern"
        );
    }

    /// The poller end to end: discover, fill, seal.
    ///
    /// Two rounds a few seconds apart, which is the cadence the frontend will
    /// drive. Prints the age of each cut's freshest radial at the moment it
    /// became renderable — **the measurement the whole feature is for**, against
    /// the archive path's five to seven minutes.
    #[ignore = "hits the live unidata-nexrad-level2-chunks S3 bucket"]
    #[tokio::test]
    async fn live_a_few_poll_rounds_assemble_and_seal() {
        let sources = crate::sources::DataSources::production();
        crate::tls::init();
        let mut poller = ChunkPoller::new("KTLX");

        // Polled to a deadline rather than a fixed round count: a cut is 3 or 6
        // chunks and the radar takes ~30-40 s over one, so a handful of rounds
        // can legitimately seal nothing.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(150);
        let mut total_sealed = 0usize;
        let mut round = 0;
        while std::time::Instant::now() < deadline && total_sealed == 0 {
            round += 1;
            let outcome = poller.poll(&sources).await.expect("a poll round");
            let progress = outcome.progress.clone().expect("a volume is being tracked");
            println!(
                "round {round}: volume {} | ingested {} | sealed {:?} | {} cuts so far | complete={} | next in {:?}",
                poller.volume().expect("a volume").get(),
                outcome.ingested,
                outcome.sealed_elevations,
                progress.sealed_elevations.len(),
                progress.volume_complete,
                poller.suggested_interval(),
            );
            for (elevation, angle) in outcome
                .sealed_elevations
                .iter()
                .zip(outcome.sealed_angles.iter())
            {
                let scan = poller.snapshot().expect("a snapshot after a seal");
                let age = scan
                    .sweeps()
                    .iter()
                    .find(|s| s.elevation_number() == *elevation)
                    .and_then(|s| s.radials().iter().map(|r| r.collection_timestamp()).max())
                    .and_then(chrono::DateTime::from_timestamp_millis)
                    .map(|t| (chrono::Utc::now() - t).num_seconds());
                println!(
                    "   elev {elevation} ({angle:.2}°) renderable, data age {}s",
                    age.unwrap_or(-1)
                );
            }
            total_sealed += outcome.sealed_elevations.len();
            if total_sealed == 0 {
                tokio::time::sleep(poller.suggested_interval()).await;
            }
        }

        let progress = poller.progress().expect("a volume");
        assert!(
            total_sealed > 0,
            "{round} rounds over 150s produced no complete cut; the feed is \
             ingesting {} chunks but nothing ever seals",
            progress.chunks_ingested
        );
        assert_eq!(
            progress.late_radials_dropped, 0,
            "radials arrived for a cut that had already sealed, which means \
             elevation numbers repeat within a volume"
        );
        // Every sweep handed out is a full rotation.
        if let Some(scan) = poller.snapshot() {
            for sweep in scan.sweeps() {
                let spacing = sweep
                    .radials()
                    .first()
                    .map(|r| r.azimuth_spacing_degrees())
                    .unwrap_or(1.0);
                let expected = (360.0 / spacing).round() as usize;
                assert!(
                    sweep.radials().len() * 100 >= expected * MIN_SEALED_RADIAL_PERCENT,
                    "elevation {} reached the snapshot with {}/{} radials",
                    sweep.elevation_number(),
                    sweep.radials().len(),
                    expected
                );
            }
        }
    }

    /// End to end: the start chunk decodes, and it is where the VCP comes from.
    #[ignore = "hits the live unidata-nexrad-level2-chunks S3 bucket"]
    #[tokio::test]
    async fn live_a_start_chunk_decodes_and_carries_the_coverage_pattern() {
        let sources = crate::sources::DataSources::production();
        crate::tls::init();
        let volume = latest_volume(&sources, "KTLX").await.expect("discovery");
        let chunks = list_chunks(&sources, "KTLX", volume)
            .await
            .expect("listing");

        let start = chunks
            .iter()
            .find(|c| c.kind() == ChunkKind::Start)
            .expect("a start chunk");
        let bytes = download_chunk(&sources, start).await.expect("download");
        println!("{} is {} bytes", start.name(), bytes.len());
        let contents = decode_chunk(start.name(), &bytes).expect("decode");
        let vcp = contents
            .coverage_pattern
            .expect("the start chunk carries message 5");
        println!(
            "VCP {} with {} planned cuts, {} radials in the start chunk",
            vcp.pattern_number().number(),
            vcp.elevation_cuts().len(),
            contents.radials.len(),
        );
        assert!(
            !vcp.elevation_cuts().is_empty(),
            "the reconstructed VCP has no elevation cuts"
        );

        // And an intermediate chunk decodes to radials without any header.
        if let Some(mid) = chunks.iter().find(|c| c.kind() == ChunkKind::Intermediate) {
            let bytes = download_chunk(&sources, mid).await.expect("download");
            let contents = decode_chunk(mid.name(), &bytes).expect("decode");
            println!(
                "{} is {} bytes -> {} radials",
                mid.name(),
                bytes.len(),
                contents.radials.len()
            );
            assert!(
                !contents.radials.is_empty(),
                "an intermediate chunk decoded to no radials"
            );
        }
    }
}
