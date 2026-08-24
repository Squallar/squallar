//! Anonymous S3 access to the NEXRAD Level II *real-time* chunk bucket.

use std::future::Future;

use nexrad_data::volume;
use nexrad_decode::messages::MessageContents;
use nexrad_model::data::{
    ChannelConfiguration, ElevationCut, PulseWidth, Radial, VolumeCoveragePattern, WaveformType,
};

use crate::archive::ArchiveError;

/// Failures reaching or interpreting the real-time chunk bucket.
#[derive(Debug, thiserror::Error)]
pub enum ChunkError {
    #[error(transparent)]
    Bucket(#[from] ArchiveError),

    /// The site has no volume directories at all — an ordinary outcome for a
    /// radar that is down, not a failure to reach the bucket.
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

/// A volume's index in the rotating real-time bucket, 1..=999.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VolumeIndex(u16);

impl VolumeIndex {
    /// `None` outside 1..=999.
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
    pub fn prefix(self, site: &str) -> String {
        format!("{site}/{}/", self.0)
    }
}

/// Where a chunk sits in its volume, from the trailing character of its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ChunkKind {
    /// Sequence 1.
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
    volume_time: chrono::NaiveDateTime,
    sequence: u16,
    kind: ChunkKind,
    name: String,
}

impl ChunkId {
    /// Parse a bare object name.
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
    pub declared_nyquist: crate::nyquist::DeclaredNyquist,
    /// Where the radar says it is, off the first Message 31's Volume Data Block.
    pub site: Option<nexrad_model::meta::Site>,
}

/// Decode one chunk's bytes.
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
                // archive walk.
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
                // Before `into_radial`, which is where the number is lost.
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

/// How complete a cut must be before it is rendered, as a percentage of the
/// radials its azimuth spacing implies.
const MIN_SEALED_RADIAL_PERCENT: usize = 95;

/// Radials one chunk carries, which is what makes a chunk sequence map onto an
/// elevation cut without decoding anything.
const RADIALS_PER_CHUNK: usize = 120;

/// Which cuts a caller wants assembled.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum CutSelection {
    /// Every cut — the default.
    #[default]
    All,
    /// Only cuts within [`ELEVATION_MATCH`] of one of these angles.
    Tilts(Vec<f32>),
}

/// How near a cut's planned angle must be to a wanted one to count as it.
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
#[derive(Debug, Clone)]
pub struct ElevationChunkMap {
    /// One entry per cut, in VCP order: its 1-based elevation number, its
    /// planned angle, and the sequence range it occupies.
    cuts: Vec<(u8, f32, std::ops::RangeInclusive<u16>)>,
}

impl ElevationChunkMap {
    /// A cut's planned angle, with a negative base tilt read as negative.
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
    pub fn cut_for(&self, sequence: u16) -> Option<(u8, f32)> {
        self.cuts
            .iter()
            .find(|(_, _, range)| range.contains(&sequence))
            .map(|(elevation, angle, _)| (*elevation, *angle))
    }

    /// Whether this sequence is worth downloading under `selection`.
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
    Open {
        radials: std::collections::BTreeMap<u16, Radial>,
        /// An `ElevationEnd` or `ScanEnd` radial has arrived.
        terminated: bool,
        /// Radials a full rotation implies, from the first radial's azimuth
        /// spacing: 720 at 0.5°, 360 at 1.0°.
        expected: Option<usize>,
    },
    /// A full rotation, frozen. Radials are moved out of the map, not copied.
    Sealed(nexrad_model::data::Sweep),
    /// Terminated, or closed with the volume, short of its radial count.
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
    /// What the cut's azimuth spacing implied.
    pub expected: usize,
}

/// A volume's assembly state, for the caller's gating and logging.
#[derive(Debug, Clone, PartialEq)]
pub struct VolumeProgress {
    pub volume: VolumeIndex,
    pub volume_time: Option<chrono::NaiveDateTime>,
    /// Elevation numbers with a complete sweep in the snapshot, ascending.
    pub sealed_elevations: Vec<u8>,
    /// Their angles, parallel to `sealed_elevations`, from
    /// `Sweep::elevation_angle_degrees` (a median over the sweep's radials).
    pub sealed_angles: Vec<f32>,
    /// Cuts that ended short. A volume holding one never completes.
    pub abandoned: Vec<AbandonedCut>,
    pub saw_scan_end: bool,
    /// Every cut **the selection asked for** sealed, and the volume ended.
    pub volume_complete: bool,
    /// The volume is **whole**: every cut it carries sealed, contiguous from 1.
    pub whole_volume_complete: bool,
    pub chunks_ingested: usize,
    /// Radials that arrived for an already-sealed cut.
    pub late_radials_dropped: usize,
}

/// Accumulates one volume's chunks into complete sweeps.
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
    /// What the caller asked for.
    selection: CutSelection,
    late_radials_dropped: usize,
    closed: bool,
    /// Invalidated whenever a cut seals. See [`Self::snapshot`].
    cached: Option<std::sync::Arc<nexrad_model::data::Scan>>,
    /// Every cut's declared Nyquist velocity, accumulated across the chunks as
    /// they arrive. See [`Self::declared_nyquist`].
    declared_nyquist: crate::nyquist::DeclaredNyquist,
    /// Where the radar said it was, off the first chunk that carried a Volume
    /// Data Block.
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
        // elevation numbers that would collide with the volume being assembled.
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
            // A repeat of the same table changes nothing to rebuild for.
            && self.coverage_pattern.as_ref() != Some(&vcp)
        {
            let vcp = self.coverage_pattern.insert(vcp);
            self.chunk_map = ElevationChunkMap::from_coverage_pattern(vcp);
            outcome.learned_coverage_pattern = true;
            // A snapshot handed out before this carries
            // `placeholder_coverage_pattern`, whose cut table is empty; a `Scan`
            // that cannot key its own sweeps must not go on being served once
            // the real pattern is known.
            self.cached = None;
        }

        // The first chunk to mention a cut is the one that names it; the rest
        // are no-ops.
        for (elevation_number, ms) in contents.declared_nyquist.iter() {
            self.declared_nyquist.declare(elevation_number, ms);
        }

        // First chunk that states one wins, as the archive walk's fold does.
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
                // some render is holding.
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

    /// Seal a cut if it is terminated and complete enough.
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
    pub fn set_selection(&mut self, selection: CutSelection) {
        self.selection = selection;
    }

    pub fn selection(&self) -> &CutSelection {
        &self.selection
    }

    /// Whether this chunk is worth downloading.
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
    pub fn is_volume_complete(&self) -> bool {
        if !self.saw_start_chunk || !self.saw_scan_end || self.cuts.is_empty() {
            return false;
        }
        match (&self.selection, &self.chunk_map) {
            // Everything was asked for, so the two questions coincide.
            (CutSelection::All, _) | (_, None) => self.every_cut_sealed_contiguously(),
            // Cuts were deliberately skipped, so contiguity is meaningless and
            // "complete" means every cut that was asked for.
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
    pub fn has_ingested(&self, sequence: u16) -> bool {
        self.ingested.contains(&sequence)
    }

    pub fn is_elevation_sealed(&self, elevation: u8) -> bool {
        self.cuts.get(&elevation).is_some_and(Cut::is_sealed)
    }

    /// Resolve every still-open cut and stop accepting chunks.
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

    /// Whether [`Self::snapshot`] would return without building.
    #[cfg(test)]
    pub(crate) fn snapshot_is_warm(&self) -> bool {
        self.cached.is_some()
    }

    /// Every cut's declared Nyquist velocity — the number [`Self::snapshot`]'s
    /// `Scan` cannot carry, `Radial` having no field for it.
    pub fn declared_nyquist(&self) -> &crate::nyquist::DeclaredNyquist {
        &self.declared_nyquist
    }
}

/// Base delay between rounds.
pub const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

/// Delay after a round that found nothing new.
pub const QUIET_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);

/// Ceiling on the failure backoff.
pub const MAX_BACKOFF: std::time::Duration = std::time::Duration::from_secs(120);

/// How stale the current volume may get before discovery is re-run rather than
/// the next index probed — three volume periods.
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

/// A listed-then-missing key is ordinary: S3 is eventually consistent, and the rotation
/// can retire a key between the listing and the GET.
pub(crate) fn fetch_disposition(e: &ArchiveError) -> FetchDisposition {
    match e {
        ArchiveError::NotFound(_) => FetchDisposition::Skip,
        _ => FetchDisposition::Abort,
    }
}

/// A volume that ended, and what it ended as.
#[derive(Clone)]
pub struct ClosedVolume {
    pub progress: VolumeProgress,
    /// The volume as it stood when it closed — complete sweeps only, so a cut
    /// that ended short is absent rather than present and partial.
    pub scan: Option<std::sync::Arc<nexrad_model::data::Scan>>,
    /// What the closed volume's cuts declared their Nyquist velocities to be.
    pub declared_nyquist: crate::nyquist::DeclaredNyquist,
}

/// Summarised rather than derived: a derived `Debug` is a sha256 over every
/// gate byte of every moment of every radial, plus ~20 MB of output text, and
/// this type is reachable from `PollOutcome`'s derived `Debug`.
impl std::fmt::Debug for ClosedVolume {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClosedVolume")
            .field("progress", &self.progress)
            .field("scan_sweeps", &self.scan.as_ref().map(|s| s.sweeps().len()))
            .finish()
    }
}

/// What one round changed.
#[derive(Debug, Clone, Default)]
pub struct PollOutcome {
    pub ingested: usize,
    /// A chunk this round carried the coverage pattern, so the volume being
    /// assembled can key its own sweeps from now on and could not before.
    pub learned_coverage_pattern: bool,
    /// Elevation numbers whose cut completed this round, ascending.
    pub sealed_elevations: Vec<u8>,
    /// Their angles, parallel to `sealed_elevations`.
    pub sealed_angles: Vec<f32>,
    /// The volume rolled this round; `snapshot` now describes a new volume.
    pub rolled_to: Option<VolumeIndex>,
    /// A volume that closed, with the scan it closed as — the only way to reach
    /// that volume, since the roll replaced the assembler `snapshot` reads.
    pub closed: Option<ClosedVolume>,
    pub progress: Option<VolumeProgress>,
    /// Keys that would not parse or bytes that would not decode.
    pub skipped: usize,
}

/// One site's real-time feed.
pub struct ChunkPoller {
    site: String,
    current: Option<VolumeAssembler>,
    consecutive_failures: u32,
    last_round_was_quiet: bool,
    /// Carried on the poller rather than the assembler so it survives a volume
    /// roll.
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
    pub fn set_selection(&mut self, selection: CutSelection) {
        if self.selection == selection {
            return;
        }
        log::debug!("{}: cut selection -> {selection:?}", self.site);
        self.selection = selection.clone();
        if let Some(current) = self.current.as_mut() {
            // Applied to the volume in flight too, so widening the selection
            // backfills within this volume rather than waiting for the next one.
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

    /// What the volume being assembled declared its cuts' Nyquist velocities to be.
    pub fn declared_nyquist(&self) -> Option<&crate::nyquist::DeclaredNyquist> {
        self.current.as_ref().map(VolumeAssembler::declared_nyquist)
    }

    /// Advisory delay before the next [`Self::poll`].
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

    /// What the next round should do.
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
    pub(crate) fn roll(&mut self, to: VolumeIndex) -> Option<ClosedVolume> {
        let closed = self.current.as_mut().map(|current| {
            // `close`, not `progress`: it is what resolves a still-open cut to
            // `Abandoned`, so `progress.abandoned` names exactly the cuts the
            // scan below is missing.
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
    pub(crate) fn should_roll_to(&self, id: &ChunkId) -> bool {
        self.current
            .as_ref()
            .and_then(VolumeAssembler::volume_time)
            .is_some_and(|known| id.volume_time() > known)
    }

    /// Whether a notified chunk belongs to a volume older than the one being
    /// assembled.
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
                // Roll only when the next directory holds a volume that started *later*
                // than this one.
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

        // Held rather than returned from inside the loop: a seal clears the
        // snapshot cache, and the round must leave by the one exit that warms
        // it rather than leaving the rebuild to the frame thread.
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
            // with the `Err`, while the cache the seals invalidated outlives
            // the round.
            //
            // Known loss: a cut that sealed earlier in this round is dropped
            // here with the outcome and does not come back, so that pane waits
            // for another seal at its own tilt or for the volume to close.
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

/// The delimiter that turns a site listing into a directory listing.
const DELIMITER: &str = "/";

/// Every chunk in one volume's directory, ascending.
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
