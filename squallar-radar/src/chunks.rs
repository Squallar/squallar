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
                // The same drop the archive decode makes, for the same
                // reason: a chunk-fed volume lands in the same caches and is
                // read by the same readers. See `crate::moment_drop`.
                let (radial, skipped_cfp) = m
                    .into_radial_without_clutter_filter_power()
                    .map_err(|e| decode(e.into()))?;
                crate::moment_drop::dropped_cfp(skipped_cfp);
                out.radials.push(radial);
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

/// **Decoded volumes the real-time chunk feed is holding.**
///
/// A process-global level, read through by
/// `squallar_egui::heap_census`'s `chunk feed` family the way
/// `crate::render::parked_bytes` is: this crate cannot see that one, and a
/// publish from the frame thread's telemetry tick would have to walk a volume
/// to produce a figure.
///
/// **Maintained off the frame thread, where the bytes actually move.** A
/// round runs on a tokio worker (native) or outside the frame callback
/// (wasm), so this is an atomic rather than a field the frame thread reads —
/// and every seam that moves it is inside a round, except the two `Drop`s,
/// which run wherever the owner is finally released. That includes the
/// frame-paced discard queue: `squallar_worker::offload::discard_each` frees a
/// retired `SiteFeed` on a free lane at some later frame, so a level tied to
/// the retirement rather than the drop would fall before the bytes did.
static CHUNK_FEED_BYTES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// **Serialising the level against the harness's other threads.**
///
/// `CHUNK_FEED_BYTES` is process-wide and this crate's lib-test binary runs its
/// tests on several threads, so a test that brackets an operation with two
/// reads of the level is measuring *every* thread's assemblers, not its own. A
/// concurrent seal landing between the two reads ADDS to the level, so an
/// observed fall comes out smaller than the drop that caused it — which is how
/// `the_chunk_feed_prices_its_volumes_and_gives_them_back` reported "dropping
/// an assembler holding 4086832 B moved the level only 0 B" under a loaded
/// board, while passing alone and passing 3/3 package-scoped. A level sampled
/// across a window while other threads are live is a false zero for anything
/// living less than the sample.
///
/// **The lock is at the writer, not on the fixtures**, because
/// [`move_feed_level`] is the single writer of the counter — every seal, the
/// snapshot, the invalidation and both `Drop`s funnel through it — so no mover
/// can be added later that forgets to take it. A lock hung on the test
/// fixtures would be silently escaped by the next test that builds a
/// `VolumeAssembler` directly, and 46 sites in this crate's tests already do.
///
/// **Re-entrant for the exclusive holder, and that is load-bearing**: the
/// observing test holds exclusivity across its own `drop(assembler)`, and that
/// drop re-enters [`move_feed_level`] on the same thread. A plain `Mutex` taken
/// unconditionally would deadlock the test this exists to fix, and
/// `std::sync::ReentrantLock` is still unstable on this tree's pinned 1.97.1
/// (`E0658`, tracking issue 121440) — so the re-entrancy is a thread-local flag
/// the holder sets, checked before the lock is taken.
#[cfg(test)]
pub(crate) mod feed_level_serial {
    use std::cell::Cell;
    use std::sync::{Mutex, MutexGuard};

    static LOCK: Mutex<()> = Mutex::new(());

    thread_local! {
        /// Set only on the thread currently holding [`exclusive`].
        static HOLDING: Cell<bool> = const { Cell::new(false) };
    }

    /// Exclusive access for the calling thread until this is dropped. Other
    /// threads' level moves block; this thread's re-enter freely.
    pub(crate) struct Exclusive(#[allow(dead_code)] MutexGuard<'static, ()>);

    impl Drop for Exclusive {
        fn drop(&mut self) {
            // Cleared BEFORE the guard field drops (a `Drop` body runs ahead of
            // the fields), so no other thread can be admitted while this one
            // still reads itself as the holder.
            HOLDING.with(|h| h.set(false));
        }
    }

    /// Take the level for this thread. Poison is recovered rather than
    /// propagated: one panicking test must not cascade into every other.
    pub(crate) fn exclusive() -> Exclusive {
        let guard = LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        HOLDING.with(|h| h.set(true));
        Exclusive(guard)
    }

    /// Apply one level move, serialised — unless this thread is the exclusive
    /// holder, in which case it is already serialised and must not re-lock.
    pub(crate) fn with_move(apply: impl FnOnce()) {
        if HOLDING.with(Cell::get) {
            apply();
            return;
        }
        let _serial = LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        apply();
    }
}

/// The shipped arm: no lock, no thread-local, no cost. The `cfg` selects a
/// module, never a fork inside [`move_feed_level`]'s body.
#[cfg(not(test))]
pub(crate) mod feed_level_serial {
    #[inline]
    pub(crate) fn with_move(apply: impl FnOnce()) {
        apply();
    }
}

/// Move the level from `was` to `now`. One `Relaxed` read-modify-write of a
/// difference the caller already has, so an add here and a subtract elsewhere
/// cannot drift the way two separate stores could.
fn move_feed_level(was: u64, now: u64) {
    feed_level_serial::with_move(|| {
        CHUNK_FEED_BYTES.fetch_add(now.wrapping_sub(was), std::sync::atomic::Ordering::Relaxed);
    });
}

/// **Compressed chunk bytes the live assemblers are keeping**, this instant —
/// see [`VolumeAssembler::raw`]. Moved through [`feed_level_serial`] for the
/// reason `CHUNK_FEED_BYTES` is, and deliberately a SEPARATE total from it:
/// that family is decoded volumes and this is their compressed form, two
/// orders of magnitude apart and released by different rules, so a line that
/// added them could not say which had moved.
static CHUNK_RAW_BYTES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Archives handed to the loop cache at a roll, and the bytes they carried.
static CHUNK_ARCHIVES_KEPT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static CHUNK_ARCHIVE_BYTES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// **Whole volumes offered nothing**: their bytes were not all retained, or
/// their concatenation would not split back into LDM records. The
/// denominator's other half — `whole = kept + refused` — and the reading that
/// says whether a way back this feed offers is one. A non-zero refusal is not
/// a failure to act on, it is the guard doing its job; a non-zero KEPT that
/// the withdrawal then cannot decode would be, which is why the framing walk
/// is on the offering side.
static CHUNK_ARCHIVES_REFUSED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Volumes that closed whole — what the two above are read against.
static CHUNK_WHOLE_CLOSES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn move_raw_level(was: u64, now: u64) {
    feed_level_serial::with_move(|| {
        CHUNK_RAW_BYTES.fetch_add(now.wrapping_sub(was), std::sync::atomic::Ordering::Relaxed);
    });
}

fn note_archive_kept(bytes: u64) {
    CHUNK_ARCHIVES_KEPT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    CHUNK_ARCHIVE_BYTES.fetch_add(bytes, std::sync::atomic::Ordering::Relaxed);
}

fn note_archive_refused() {
    CHUNK_ARCHIVES_REFUSED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// **What the live assemblers are holding as their volumes' way back**, this
/// instant. A LEVEL, never added to [`feed_bytes`].
pub fn retained_chunk_bytes() -> u64 {
    CHUNK_RAW_BYTES.load(std::sync::atomic::Ordering::Relaxed)
}

/// **Whole volumes handed over with their compressed form, the bytes they
/// carried, and the ones refused for framing** — running totals for the life
/// of the process, so a line of their own and never added to a level.
pub fn chunk_archive_totals() -> (u64, u64, u64, u64) {
    (
        CHUNK_WHOLE_CLOSES.load(std::sync::atomic::Ordering::Relaxed),
        CHUNK_ARCHIVES_KEPT.load(std::sync::atomic::Ordering::Relaxed),
        CHUNK_ARCHIVE_BYTES.load(std::sync::atomic::Ordering::Relaxed),
        CHUNK_ARCHIVES_REFUSED.load(std::sync::atomic::Ordering::Relaxed),
    )
}

/// **Host bytes the chunk feed's decoded volumes are holding**, this instant.
///
/// Two terms per assembler and one per poller:
///
/// * the **staged sweeps** — cuts that have sealed since the last snapshot
///   was built, priced at the seal and waiting to be moved into the next one;
/// * the **built snapshot** — `VolumeAssembler::cached`, the `Scan` the feed
///   serves, which OWNS every sweep folded into it rather than copying one;
/// * the poller's **parked closed volumes**, each a whole `Scan`.
///
/// **The two assembler terms are disjoint, and together they are ONE
/// volume.** A sealed cut's sweep is in exactly one of them: staged until a
/// build moves it in, inside the built `Scan` afterwards. Until 2026-09-07
/// they were not disjoint — the build deep-cloned every sealed sweep and the
/// assembler went on owning the originals — so a live site held its volume
/// twice and this figure named both copies.
///
/// **A FLOOR, and it under-counts in one direction only.** The radials of a
/// cut still being received (`Cut::Open`) are not priced: they move into a
/// sealed sweep untouched when the cut completes, so pricing them would need
/// a subtraction at the seal to avoid double counting, and at most one cut
/// per assembler is open at a time — a sixteenth of a volume on a VCP 212.
/// Nothing here ever prices bytes that have gone.
///
/// **The one whole-volume under-count left is a rebuild a consumer forced,
/// and MEASURED it is every rebuild — but it is no longer a whole volume.**
/// Since `nexrad_model::data::GateBuffer`, the two generations a rebuild
/// leaves alive SHARE their gate buffers, so what is resident and outside this
/// figure is the older generation's containers, not its gates; and what this
/// figure and `still scans` now charge twice is the same shared gate bytes.
/// [`shared_overlap_bytes`] is the measured upper bound on that, and
/// `crate::scan_size`'s module note is where it is written down. `chunk_feed::SiteFeed::last_snapshot`
/// holds an `Arc` of whatever [`VolumeAssembler::snapshot`] last handed out,
/// to serve the frame thread while the poller is away on a round; the still
/// inventory holds the same `Arc` from the moment a round delivers. At rest
/// those are the same allocation the assembler holds, so they cost nothing
/// extra and `cached_bytes` prices them. A round that seals while they are
/// holding it is the case [`VolumeAssembler::snapshot`] cannot move out of:
/// it copies for that rebuild, and the old allocation is resident and not in
/// this figure. On a 420 s HEAVY6 leg that was 156 of 156 rebuilds — see
/// [`rebuild_totals`], which is the reading. It goes when they let go of it:
/// the frame thread's next `ChunkFeedManager::snapshot` refreshes the bridge,
/// and `still scans` prices the inventory's meanwhile.
/// **Unbounded for a live site the frame thread stops asking about**, as
/// before — but one volume rather than two, because the assembler no longer
/// keeps a copy of its own besides.
///
/// **An UPPER bound against the other radar families**, like every figure in
/// `radar_total`: once a round delivers, the same `Arc<Scan>` is installed in
/// the still inventory, and `still scans` prices it too. This says what
/// emptying the feed alone would free.
pub fn feed_bytes() -> usize {
    usize::try_from(CHUNK_FEED_BYTES.load(std::sync::atomic::Ordering::Relaxed))
        .unwrap_or(usize::MAX)
}

/// **Rebuilds that moved the previous volume, and rebuilds that copied it.**
///
/// [`VolumeAssembler::snapshot`] takes the previous `Scan` apart with
/// `Arc::try_unwrap`. When the assembler is the last owner the sweeps MOVE and
/// the rebuild allocates nothing; when somebody else is holding it the sweeps
/// are cloned — a whole decoded volume, tens of megabytes across tens of
/// thousands of gate buffers, on the poller's thread.
///
/// Six running totals, always on, `Relaxed`: they are read against themselves
/// on a telemetry tick, never against another thread's clock.
static REBUILD_MOVES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static REBUILD_MOVED_BYTES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static REBUILD_MOVED_BLOCKS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static REBUILD_COPIES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static REBUILD_COPIED_BYTES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static REBUILD_COPIED_BLOCKS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static REBUILD_SHARED_BYTES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static REBUILD_SHARED_BLOCKS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// **Gate bytes the census is charging twice, right now**, summed across live
/// assemblers — see the note in [`crate::scan_size`].
static SHARED_OVERLAP_BYTES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn move_shared_overlap(was: u64, now: u64) {
    feed_level_serial::with_move(|| {
        SHARED_OVERLAP_BYTES.fetch_add(now.wrapping_sub(was), std::sync::atomic::Ordering::Relaxed);
    });
}

/// **An upper bound on what the radar census families over-report because a
/// rebuild's two generations share their gate buffers.**
///
/// `nexrad_model::data::GateBuffer` makes a volume's clone a refcount bump
/// rather than a copy, so while the assembler's new generation and whatever
/// the bridge or the still inventory is still holding are both alive, the two
/// families charge the same gate bytes twice. This is the sum, over live
/// assemblers, of exactly the gate bytes each one's last rebuild shared.
///
/// **Its size, MEASURED** on a 420 s six-site HEAVY6 leg (2026-09-09, 153
/// copying rebuilds): a copy shared a median **32.3 MiB** and at most
/// **67.2 MiB**, and the sum across the six sites' last rebuilds — this
/// gauge's own value at the end of the leg — was **147.8 MiB**.
///
/// **An upper bound, not the instant truth**: it is set at the rebuild and
/// cleared at the next one or when the assembler is dropped, and nothing here
/// learns the moment the older generation is actually released — which is
/// usually within a frame or two, when the frame thread's next
/// `ChunkFeedManager::snapshot` refreshes the bridge. It is deliberately the
/// side that cannot understate the correction.
///
/// Zero before the first rebuild of a process, and zero for a site whose last
/// rebuild moved rather than copied.
pub fn shared_overlap_bytes() -> u64 {
    SHARED_OVERLAP_BYTES.load(std::sync::atomic::Ordering::Relaxed)
}

/// What [`rebuild_totals`] answers. Bytes AND blocks, because a decoded
/// volume's bytes are 95.9 % gate buffers: a figure in megabytes alone hides
/// that the same copy was tens of thousands of allocations.
///
/// **Read `copied_bytes` and `copied_blocks` as the price of the volume the
/// rebuild had in hand, not as what it spent.** They are what
/// `scan_size::scan_bytes_and_blocks` charged the previous generation, which
/// is what the copy cost until gate buffers became shareable and is still the
/// right denominator for the saving. `shared_bytes` and `shared_blocks` are
/// the part of that price the copy no longer pays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RebuildTotals {
    /// Rebuilds where `Arc::try_unwrap` succeeded and the sweeps moved.
    pub moves: u64,
    /// Bytes those moves did NOT copy — what the previous volume was priced at.
    pub moved_bytes: u64,
    /// Allocations those moves did not make.
    pub moved_blocks: u64,
    /// Rebuilds where another owner held the volume and the sweeps were cloned.
    pub copies: u64,
    /// Bytes those clones copied.
    pub copied_bytes: u64,
    /// Allocations those clones would have made before the gate buffers
    /// became shareable. See [`Self::shared_blocks`] for what they make now.
    pub copied_blocks: u64,
    /// **Of [`Self::copied_bytes`], the bytes the clones did NOT allocate**
    /// because `nexrad_model::data::GateBuffer` shares them. The difference,
    /// `copied_bytes - shared_bytes`, is the container term a clone still
    /// pays: the sweep vector, one radial vector per sweep, and the `Radial`
    /// structs' inline moments.
    pub shared_bytes: u64,
    /// **Of [`Self::copied_blocks`], the allocations the clones did NOT
    /// make** — two per non-empty moment, the gate `Vec` and its `Arc`.
    pub shared_blocks: u64,
}

/// File one rebuild against [`rebuild_totals`], and say so at `debug`.
///
/// `owners` is the strong count the rebuild saw, `previous` included, so 1 is
/// the sole-owner case the move arm needs and anything above it names how many
/// other holders there were. It is on the line because the count is the whole
/// diagnosis: which holder is second matters far less than whether removing
/// any one of them could have got the count to 1.
///
/// Off the frame thread wherever a rebuild is — `ChunkPoller::warm_snapshot`
/// runs inside a round — and one line per sealed cut per site, which is about
/// one every sixteen seconds on a VCP 212.
fn record_rebuild(
    site: &str,
    copied: bool,
    owners: usize,
    bytes: u64,
    blocks: u64,
    shared: (u64, u64),
) {
    use std::sync::atomic::Ordering::Relaxed;
    // Through the same serialiser the byte level uses, for the same reason: a
    // test that brackets one rebuild with two reads of a process-wide counter
    // is otherwise measuring every other thread's assemblers too. The shipped
    // arm of `feed_level_serial` is an inlined call of the closure.
    feed_level_serial::with_move(|| {
        if copied {
            REBUILD_COPIES.fetch_add(1, Relaxed);
            REBUILD_COPIED_BYTES.fetch_add(bytes, Relaxed);
            REBUILD_COPIED_BLOCKS.fetch_add(blocks, Relaxed);
            REBUILD_SHARED_BYTES.fetch_add(shared.0, Relaxed);
            REBUILD_SHARED_BLOCKS.fetch_add(shared.1, Relaxed);
        } else {
            REBUILD_MOVES.fetch_add(1, Relaxed);
            REBUILD_MOVED_BYTES.fetch_add(bytes, Relaxed);
            REBUILD_MOVED_BLOCKS.fetch_add(blocks, Relaxed);
        }
    });
    let totals = rebuild_totals();
    log::debug!(
        "{site}: volume rebuild {} with {owners} owner(s), {bytes} B in {blocks} blocks, \
         shared {} B in {} blocks; \
         totals copied {} rebuilds / {} B / {} blocks, shared {} B / {} blocks, \
         moved {} rebuilds / {} B / {} blocks",
        if copied { "COPIED" } else { "moved" },
        shared.0,
        shared.1,
        totals.copies,
        totals.copied_bytes,
        totals.copied_blocks,
        totals.shared_bytes,
        totals.shared_blocks,
        totals.moves,
        totals.moved_bytes,
        totals.moved_blocks,
    );
}

/// **Whether a rebuild moved the previous volume or had to copy it**, as
/// running totals over the life of the process.
///
/// A rebuild that finds itself the volume's last owner costs nothing; one that
/// finds a second owner clones the volume's containers and shares its gate
/// buffers. Both arms are counted, because a mechanism that never fires and
/// one that always fires are indistinguishable from the arm that fired alone.
///
/// **The reading that gate-buffer sharing landed on**, a 420 s six-site
/// HEAVY6 leg on the same seed as the 156-of-156 measurement above
/// (2026-09-09, 153 rebuilds with a previous volume, all of them copies):
///
/// ```text
/// COPIED                        : 153   4,396.4 MiB   5,405,092 blocks
/// of which SHARED, not allocated:       4,281.3 MiB   5,403,600 blocks
/// copy still allocated          :         115.1 MiB       1,492 blocks
/// mean shared per copy          :          28.0 MiB      35,318 blocks
/// mean still allocated per copy :           0.752 MiB         9.8 blocks
/// ```
///
/// **97.4 % of the bytes and 100.0 % of the blocks a rebuild used to allocate
/// are gone**, and what is left is a sweep vector plus one radial vector a
/// sweep — 9.8 allocations where the same volume's clone was ~17,700 on the
/// pre-sharing model. `moves` staying at zero is correct rather than missing:
/// see [`VolumeAssembler::snapshot`].
///
/// **The block figures are on the post-sharing model**, which charges two
/// blocks a non-empty moment rather than one (`crate::scan_size`'s
/// [`crate::scan_size::GATE_BUFFER_SHARE_BYTES`]), so `copied_blocks` here and
/// the 18,873-a-rebuild mean quoted before it have different denominators and
/// must not be subtracted from one another.
pub fn rebuild_totals() -> RebuildTotals {
    use std::sync::atomic::Ordering::Relaxed;
    RebuildTotals {
        moves: REBUILD_MOVES.load(Relaxed),
        moved_bytes: REBUILD_MOVED_BYTES.load(Relaxed),
        moved_blocks: REBUILD_MOVED_BLOCKS.load(Relaxed),
        copies: REBUILD_COPIES.load(Relaxed),
        copied_bytes: REBUILD_COPIED_BYTES.load(Relaxed),
        copied_blocks: REBUILD_COPIED_BLOCKS.load(Relaxed),
        shared_bytes: REBUILD_SHARED_BYTES.load(Relaxed),
        shared_blocks: REBUILD_SHARED_BLOCKS.load(Relaxed),
    }
}

/// **Move the feed level from a dependent crate's test.**
///
/// Behind `test-support` for the reason the feature exists: `#[cfg(test)]` is
/// crate-local, and `squallar_egui`'s census gate needs to move this level to
/// show that its `chunk feed` family really reads THIS function and has not
/// been quietly wired to a constant. A tamper replacing that read with `0u64`
/// survived every test in the tree until this existed.
#[cfg(feature = "test-support")]
pub fn force_feed_level(was: u64, now: u64) {
    move_feed_level(was, now);
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
    /// A full rotation, frozen. The sweep itself is moved on — into
    /// [`VolumeAssembler::staged`], and from there into the built snapshot —
    /// so what stays here is only what the assembler answers about the cut
    /// without reading its radials.
    Sealed {
        /// The median elevation angle over the sweep's radials, taken once at
        /// the seal because the sweep is about to move out of reach.
        /// `None` only for a sweep with no radials, which [`VolumeAssembler`]
        /// does not seal.
        angle: Option<f32>,
    },
    /// Terminated, or closed with the volume, short of its radial count.
    Abandoned { have: usize, expected: usize },
}

impl Cut {
    fn is_sealed(&self) -> bool {
        matches!(self, Self::Sealed { .. })
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

/// **One volume's retained chunks, concatenated into its Archive II form** —
/// or `None` where the result would not be one.
///
/// Free rather than a method so the property can be driven from real chunk
/// bytes without a whole volume behind them: what could break here is the
/// JOIN, and a start chunk followed by one more record is enough to show the
/// record framing survives it.
///
/// Two refusals, both cheap and both on the offering side:
///
/// * the buffer must begin `AR2`, which is the volume header a start chunk
///   carries and no other chunk does;
/// * it must split back into LDM records — [`volume::File::records`] reads
///   each record's control word and decompresses none, so this is a walk of
///   ~100 words against the 4-11 s a volume decode costs.
///
/// The alternative to refusing is worse than a missing archive: what takes
/// this is `App::release_unneeded_base_gates`, whose restore has no second
/// way back, so a way back that is not one is a merge base that never comes
/// home.
fn archive_from_chunks(
    pieces: std::collections::BTreeMap<u16, Vec<u8>>,
) -> Option<std::sync::Arc<Vec<u8>>> {
    let total: usize = pieces.values().map(Vec::len).sum();
    let mut out: Vec<u8> = Vec::with_capacity(total);
    // Consumed in sequence order, each piece dropped as it goes, so the peak
    // is the buffer plus one chunk rather than twice the compressed volume.
    for (_, piece) in pieces {
        out.extend_from_slice(&piece);
        drop(piece);
    }
    if out.get(..3) != Some(b"AR2".as_slice()) {
        note_archive_refused();
        return None;
    }
    let archive = std::sync::Arc::new(out);
    match volume::File::from_shared(std::sync::Arc::clone(&archive)).records() {
        Ok(records) if !records.is_empty() => {
            note_archive_kept(archive.len() as u64);
            Some(archive)
        }
        _ => {
            note_archive_refused();
            None
        }
    }
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
    /// The `Scan` the feed serves, and the **owner** of every sweep folded
    /// into it. Marked stale rather than dropped when a cut seals: dropping
    /// it would throw the volume away, not release a copy of it. See
    /// [`Self::snapshot`].
    cached: Option<std::sync::Arc<nexrad_model::data::Scan>>,
    /// Whether [`Self::cached`] is missing something learned since it was
    /// built — a sealed cut, the coverage pattern, the site.
    stale: bool,
    /// Sealed sweeps not yet folded into [`Self::cached`], by elevation
    /// number. A sealed cut's sweep is in exactly one of the two places, which
    /// is what makes the two byte terms below disjoint.
    staged: std::collections::BTreeMap<u8, nexrad_model::data::Sweep>,
    /// Host bytes [`Self::staged`] holds, summed at each seal and handed to
    /// [`Self::cached_bytes`] when a build folds the sweeps in. See
    /// [`feed_bytes`] for what is deliberately not in it.
    staged_bytes: u64,
    /// Host bytes [`Self::cached`] holds — the volume itself, once, not a copy
    /// of it. Zero exactly when no snapshot has been built.
    cached_bytes: u64,
    /// Allocations [`Self::cached`] holds, off the same walk that priced it —
    /// what a rebuild forced to clone the volume would allocate all over
    /// again if the gate buffers did not share.
    cached_blocks: u64,
    /// **Gate bytes this assembler's last rebuild SHARED with the generation
    /// it cloned from**, and therefore an upper bound on what the census
    /// families are charging twice for this site — see
    /// [`shared_overlap_bytes`]. Zero until a rebuild copies, and zero again
    /// after one moves.
    shared_overlap: u64,
    /// Every cut's declared Nyquist velocity, accumulated across the chunks as
    /// they arrive. See [`Self::declared_nyquist`].
    declared_nyquist: crate::nyquist::DeclaredNyquist,
    /// Where the radar said it was, off the first chunk that carried a Volume
    /// Data Block.
    reported_site: Option<nexrad_model::meta::Site>,
    /// **The compressed bytes of every chunk this volume accepted**, by
    /// sequence — the volume's own way back, kept because this feed is
    /// already holding them.
    ///
    /// A chunk-assembled volume has always been filed with `archive: None`,
    /// on the reasoning that it "was never one compressed object". It was:
    /// the start chunk is an Archive II header followed by LDM records, every
    /// later chunk is one more LDM record with its control word
    /// ([`decode_chunk`] is where both shapes are read), and the S3 object the
    /// bucket publishes minutes later is their concatenation. So the way back
    /// costs a retention rather than a download.
    ///
    /// **What it buys is the only lever that can withdraw a whole decoded
    /// volume.** `App::release_unneeded_base_gates` refuses a base it cannot
    /// decode back, and on a 420 s single-pane leg of 2026-09-10 that refusal
    /// was 298 of 511 considerations — every one of them with the identity
    /// index knowing the base's own address and no compressed half standing
    /// anywhere for it (`base way-back: unlearned 0, archive-gone 298,
    /// shadowed 0`). The archive drain does not close that gap on its own:
    /// the 60 s check is skipped while a feed serves the site, so over that
    /// whole leg two volumes reached the loop cache and one of them was this
    /// feed's, with nothing behind it.
    ///
    /// Kept by sequence and not appended in arrival order because a
    /// notification-driven fetch may land out of order, and an archive whose
    /// records are transposed is one that decodes to nothing.
    ///
    /// **Bounded by the volume it belongs to**: dropped with the assembler,
    /// and taken (not copied) by [`Self::take_archive`] at the roll. The
    /// compressed form is 1.0-16.1 MiB against a 33.7-82.7 MiB decoded
    /// volume.
    raw: std::collections::BTreeMap<u16, Vec<u8>>,
    /// Host bytes [`Self::raw`] holds, its own running term of the level —
    /// see [`retained_chunk_bytes`].
    raw_bytes: u64,
}

impl VolumeAssembler {
    pub fn new(site: impl Into<String>, volume: VolumeIndex) -> Self {
        Self {
            site: site.into(),
            volume,
            volume_time: None,
            ingested: Default::default(),
            cuts: Default::default(),
            staged: Default::default(),
            staged_bytes: 0,
            cached_bytes: 0,
            cached_blocks: 0,
            shared_overlap: 0,
            coverage_pattern: None,
            saw_start_chunk: false,
            saw_scan_end: false,
            chunk_map: None,
            selection: CutSelection::All,
            late_radials_dropped: 0,
            closed: false,
            cached: None,
            stale: false,
            declared_nyquist: crate::nyquist::DeclaredNyquist::empty(),
            reported_site: None,
            raw: Default::default(),
            raw_bytes: 0,
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
        let outcome = self.ingest_contents(id.sequence(), id.kind(), id.volume_time(), contents);
        // **Kept only for a chunk the assembler took**, so a duplicate, a
        // stale rotation's leftover and a chunk for another volume cost
        // nothing. This is the one seam that has the compressed bytes at all
        // — `ingest_contents` is reached by the equivalence tests with a
        // golden `Scan` re-sliced and no encoder, and an assembler driven
        // that way retains nothing and offers no archive, which
        // `take_archive` checks rather than assumes.
        if outcome.accepted {
            let was = self.raw_bytes;
            self.raw_bytes = self
                .raw_bytes
                .saturating_add(bytes.len() as u64)
                .saturating_add(crate::scan_size::ALLOCATOR_BLOCK_OVERHEAD as u64);
            move_raw_level(was, self.raw_bytes);
            self.raw.insert(id.sequence(), bytes.to_vec());
        }
        Ok(outcome)
    }

    /// **Take the volume's compressed form**, and release the retention
    /// either way.
    ///
    /// `Some` only for a volume that is whole ([`Self::is_whole_volume_complete`]),
    /// whose every accepted chunk's bytes are held, and whose concatenation
    /// splits back into LDM records — the framing walk, not a decode: it
    /// reads each record's control word and never decompresses one, so it
    /// costs a walk of ~100 records against the 4-11 s a volume decode
    /// costs. A buffer that fails it is not offered, because what would take
    /// it is a merge-base withdrawal whose restore has no second way back.
    ///
    /// Called at the roll, which runs on the poller and never on the frame
    /// thread. One concatenation of the compressed volume, with each piece
    /// dropped as it is consumed, so the peak is the buffer plus one chunk
    /// rather than twice the volume.
    pub(crate) fn take_archive(&mut self) -> Option<std::sync::Arc<Vec<u8>>> {
        let pieces = std::mem::take(&mut self.raw);
        move_raw_level(self.raw_bytes, 0);
        self.raw_bytes = 0;
        if !self.is_whole_volume_complete() {
            return None;
        }
        CHUNK_WHOLE_CLOSES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if pieces.len() != self.ingested.len() {
            // An assembler fed through `ingest_contents` — the test seam —
            // holds no bytes, and a partial retention would concatenate to a
            // hole.
            note_archive_refused();
            return None;
        }
        archive_from_chunks(pieces)
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
            self.invalidate();
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
            self.invalidate();
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
                Cut::Sealed { .. } | Cut::Abandoned { .. } => self.late_radials_dropped += 1,
            }
        }

        touched.sort_unstable();
        for elevation in touched {
            if self.try_seal(elevation) {
                outcome.sealed.push(elevation);
            }
        }
        if !outcome.sealed.is_empty() {
            self.invalidate();
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
        let sweep = nexrad_model::data::Sweep::new(elevation, radials.into_values().collect());
        // **Priced here, once, off the frame thread.** The radials moved out
        // of the open cut rather than being copied, so this is bytes crossing
        // from unpriced to priced and there is nothing to subtract. One walk
        // of this sweep's ~720 radials, ~16 times a volume, inside the round
        // that sealed it — never on a frame.
        let bytes = crate::scan_size::sweep_bytes(&sweep) as u64;
        move_feed_level(self.staged_bytes, self.staged_bytes + bytes);
        self.staged_bytes += bytes;
        // Taken before the sweep moves out of reach. `progress` used to
        // recompute this median over the cut's ~720 radials on every call, and
        // every round makes several.
        let angle = sweep.elevation_angle_degrees();
        self.staged.insert(elevation, sweep);
        self.cuts.insert(elevation, Cut::Sealed { angle });
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
        // **Nothing is invalidated here, and that is a change of 2026-09-07.**
        // A cut going from `Open` to `Abandoned` changes nothing the snapshot
        // carries — only `Cut::Sealed` ever reaches it — so a build made
        // before this call is still exactly right. It used to be dropped
        // anyway, which cost nothing back when the build was a copy. Under a
        // build that OWNS the volume it would cost a whole one: a volume whose
        // only open cut was never wanted still completes, so `roll` asks for
        // its snapshot, and that rebuild would run with the bridge still
        // holding the volume being rebuilt — the one branch that copies.
        self.closed = true;
        self.progress()
    }

    pub fn progress(&self) -> VolumeProgress {
        let mut sealed_elevations = Vec::new();
        let mut sealed_angles = Vec::new();
        let mut abandoned = Vec::new();
        for (elevation, cut) in &self.cuts {
            match cut {
                Cut::Sealed { angle } => {
                    sealed_elevations.push(*elevation);
                    sealed_angles.push(angle.unwrap_or(f32::NAN));
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
    /// **The sweeps MOVE into the `Scan`.** A sealed cut's sweep waits in
    /// [`Self::staged`] and is folded in here; a rebuild after a later seal
    /// takes the previous `Scan` apart with `Arc::try_unwrap` and moves its
    /// sweeps into the new one. One volume per live site, in one place —
    /// which place it is depends only on how much of it has been built.
    ///
    /// **The copy is not the exception — MEASURED, it is every rebuild —
    /// and it is now nearly free.** `try_unwrap` identifies the case where
    /// another owner still holds the volume being rebuilt, and on a 420 s
    /// HEAVY6 leg (six live sites, 2026-09-09) it failed on **156 of 156**
    /// rebuilds that had a previous volume: 4,744.9 MiB across 2,944,230
    /// allocator blocks, a mean 30.4 MiB and 18,873 blocks a rebuild. A second
    /// leg of the same arm read 154 of 154 and 4,406.7 MiB.
    ///
    /// That did not change and cannot be changed here — see the owner count
    /// below. What changed is what the clone costs.
    /// `nexrad_model::data::GateBuffer` made a moment's gate buffer an
    /// `Arc<Vec<u8>>`, and 95.9 % of a volume's bytes are those buffers at one
    /// block apiece, so `to_vec` below now copies the containers — the sweep
    /// vector, one radial vector per sweep, and the `Radial` structs with
    /// their inline moments — and bumps a refcount for every gate array
    /// instead of reallocating one. [`RebuildTotals::shared_bytes`] and
    /// [`RebuildTotals::shared_blocks`] are exactly that difference, filed on
    /// the copy arm, and the `debug` line beside them carries the per-rebuild
    /// figures and the running totals together so a reader of either can check
    /// itself against the other.
    ///
    /// **The move arm stays at zero, and that is the correct outcome rather
    /// than a missing one.** Sharing a gate buffer does not change who owns
    /// the `Arc<Scan>`, so `try_unwrap` still fails exactly as often; the
    /// saving is on the copy arm, in `shared_bytes` / `shared_blocks`, not in
    /// a migration from one arm to the other.
    ///
    /// **And the owner count says no single holder can be removed to fix it.**
    /// The strong count seen at the rebuild, this `Arc` included, was **3 on
    /// 135** of those 156 and 2 on the other 21 — of which 20 were volumes
    /// priced at zero bytes. So exactly ONE rebuild in 156 had a second owner
    /// that removing one holder could have made sole. The holders are the
    /// bridge (`chunk_feed::SiteFeed::last_snapshot`), the still inventory
    /// (`squallar-app`'s `install_still`, which clones this very `Arc` on
    /// every applied round) and a pane.
    ///
    /// **Confining the bridge to the away window cannot help, by
    /// construction.** Every copy was on `tokio-rt-worker` — inside a round,
    /// which is precisely the window the poller is away and the bridge MUST be
    /// held. The bridge is not a holder that overstays; it is a holder whose
    /// whole purpose overlaps the rebuild. Which is why the cut taken was
    /// making a `Sweep` clone cheap rather than moving ownership around.
    ///
    /// **The warm path stays free**, which is what the frame thread's several
    /// calls a frame depend on; a build happens on the poller's thread, inside
    /// the round that invalidated (`ChunkPoller::warm_snapshot`).
    pub fn snapshot(&mut self) -> std::sync::Arc<nexrad_model::data::Scan> {
        if let Some(cached) = &self.cached
            && !self.stale
        {
            return std::sync::Arc::clone(cached);
        }
        // Taken out of the field first: the assembler must not still be an
        // owner when `try_unwrap` asks whether anybody else is one.
        let mut sweeps: Vec<nexrad_model::data::Sweep> = match self.cached.take() {
            None => Vec::new(),
            Some(previous) => {
                let owners = std::sync::Arc::strong_count(&previous);
                let (bytes, blocks) = (self.cached_bytes, self.cached_blocks);
                match std::sync::Arc::try_unwrap(previous) {
                    Ok(scan) => {
                        record_rebuild(&self.site, false, owners, bytes, blocks, (0, 0));
                        move_shared_overlap(self.shared_overlap, 0);
                        self.shared_overlap = 0;
                        scan.into_sweeps()
                    }
                    Err(shared) => {
                        // The gate term of the volume being cloned: what the
                        // clone below would have allocated before
                        // `nexrad_model::data::GateBuffer`, and what it now
                        // bumps a refcount for instead. Priced from the
                        // volume in hand rather than inferred from `bytes`,
                        // because `bytes` includes the containers the clone
                        // does still allocate.
                        let (gate_bytes, gate_blocks) =
                            crate::scan_size::scan_gate_bytes_and_blocks(&shared);
                        let (gate_bytes, gate_blocks) = (gate_bytes as u64, gate_blocks as u64);
                        record_rebuild(
                            &self.site,
                            true,
                            owners,
                            bytes,
                            blocks,
                            (gate_bytes, gate_blocks),
                        );
                        move_shared_overlap(self.shared_overlap, gate_bytes);
                        self.shared_overlap = gate_bytes;
                        shared.sweeps().to_vec()
                    }
                }
            }
        };
        sweeps.extend(std::mem::take(&mut self.staged).into_values());
        // Both sources are already ascending and a cut seals once, so this
        // sorts a dozen-odd already-ordered runs and cannot see a duplicate.
        sweeps.sort_by_key(nexrad_model::data::Sweep::elevation_number);
        let vcp = self
            .coverage_pattern
            .clone()
            .unwrap_or_else(|| placeholder_coverage_pattern(0));
        let scan = std::sync::Arc::new(match self.reported_site.clone() {
            Some(site) => nexrad_model::data::Scan::with_site(site, vcp, sweeps),
            None => nexrad_model::data::Scan::new(vcp, sweeps),
        });
        // **One volume, priced once.** Going in this assembler held
        // `staged_bytes + cached_bytes`; coming out it holds this `Scan` — the
        // same sweeps, in one container, plus its own vector and metadata
        // blocks. The walk is one pass over the volume on the poller's thread,
        // about as often as a cut seals; the warm return above does no work at
        // all, which is what keeps the frame thread's calls free.
        let (now, blocks) = crate::scan_size::scan_bytes_and_blocks(&scan);
        let now = now as u64;
        move_feed_level(self.staged_bytes + self.cached_bytes, now);
        self.staged_bytes = 0;
        self.cached_bytes = now;
        self.cached_blocks = blocks as u64;
        self.stale = false;
        self.cached = Some(std::sync::Arc::clone(&scan));
        scan
    }

    /// Mark the built snapshot stale, so the next [`Self::snapshot`] rebuilds.
    ///
    /// **It frees nothing and the level does not move**, which is the whole
    /// difference from the `drop_cached` it replaced on 2026-09-07. The built
    /// `Scan` owns the volume's sweeps now, so dropping it here would throw
    /// the volume away rather than release a copy of it — and retracting the
    /// level here would be worse than merely wrong: it would price bytes that
    /// are still resident, the one direction this instrument may never fail
    /// in.
    ///
    /// Still spelled once rather than at each of the four sites, for the
    /// reason it always was: an invalidation that forgot its accounting is
    /// invisible.
    fn invalidate(&mut self) {
        self.stale = true;
    }

    /// Whether [`Self::snapshot`] would return without building.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub(crate) fn snapshot_is_warm(&self) -> bool {
        self.cached.is_some() && !self.stale
    }

    /// Whether a snapshot has been built and is missing something learned
    /// since — a sealed cut, the coverage pattern, the site.
    ///
    /// **Not the negation of [`Self::snapshot_is_warm`]**: a volume nothing
    /// has ever asked for is neither warm nor stale. It is cold, and
    /// [`ChunkPoller::warm_snapshot`] leaves it that way.
    fn snapshot_is_stale(&self) -> bool {
        self.cached.is_some() && self.stale
    }

    /// Every cut's declared Nyquist velocity — the number [`Self::snapshot`]'s
    /// `Scan` cannot carry, `Radial` having no field for it.
    pub fn declared_nyquist(&self) -> &crate::nyquist::DeclaredNyquist {
        &self.declared_nyquist
    }

    /// **The assembled snapshot's allocation and its price**, for a caller
    /// building a union across every holder of a decoded volume.
    ///
    /// The pointer and not the volume: the question is identity. The price is
    /// [`Self::cached_bytes`] as [`Self::snapshot`] last set it, so this walks
    /// nothing at all. `None` before any snapshot has been built.
    ///
    /// `chunk feed` is the one decoded-volume family the census publishes that
    /// no union could reach, and the census's own doc says so — the overlap
    /// against `still scans` "is not measured by any walk". This is that
    /// walk's near end. It is deliberately NOT a [`Self::snapshot`] call:
    /// that one rebuilds a stale assembler on the caller's thread, and a
    /// telemetry tick must never be the thing that pays for a volume rebuild.
    pub fn cached_allocation(&self) -> Option<(*const nexrad_model::data::Scan, u64)> {
        self.cached
            .as_ref()
            .map(|scan| (std::sync::Arc::as_ptr(scan), self.cached_bytes))
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
    /// **The volume's compressed form**, from the chunks it was built out of
    /// — see [`VolumeAssembler::take_archive`]. `None` for a volume that did
    /// not close whole, whose bytes were not all retained, or whose
    /// concatenation would not split back into records.
    pub archive: Option<std::sync::Arc<Vec<u8>>>,
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
    /// Host bytes [`Self::pending_closed`] holds. Maintained at the four
    /// push/pop seams rather than walked: the queue is unbounded, and a fold
    /// over it would be a walk of every parked volume's radials.
    pending_bytes: u64,
}

/// **Give every byte back, wherever this assembler is finally released.**
///
/// There is no single choke point to hang this on and there are six paths to
/// it: `roll` replacing `current`, `poll`'s rediscovery, a poller dropped
/// because its site went away mid-round, a cancelled round task, the whole
/// `App` tearing down, and — the one that makes a `Drop` mandatory rather
/// than tidy — `retain_live`'s retired feeds, which
/// `squallar_worker::offload::discard_each` frees on a free lane at some
/// later frame. A level retracted at the retirement instead would fall while
/// the bytes were still resident, which is the one direction an instrument
/// must never fail in.
impl Drop for VolumeAssembler {
    fn drop(&mut self) {
        move_feed_level(self.staged_bytes.saturating_add(self.cached_bytes), 0);
        move_shared_overlap(self.shared_overlap, 0);
        move_raw_level(self.raw_bytes, 0);
    }
}

/// As [`VolumeAssembler`]'s: the parked queue is freed with the poller, on
/// whichever thread finally holds it.
impl Drop for ChunkPoller {
    fn drop(&mut self) {
        move_feed_level(self.pending_bytes, 0);
    }
}

/// Host bytes a parked closed volume holds. `None` — a volume that closed
/// with no sealed cut at all — costs nothing.
fn closed_volume_bytes(closed: &ClosedVolume) -> u64 {
    closed
        .scan
        .as_deref()
        .map_or(0, |scan| crate::scan_size::scan_bytes(scan) as u64)
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
            pending_bytes: 0,
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
            pending_bytes: 0,
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

    /// **The assembler this poller is filling**, so a test in a sibling module
    /// can drive real chunks through [`VolumeAssembler::ingest_contents`]
    /// rather than reach for a hand-set field.
    ///
    /// `chunk_feed`'s bridge-copy suite needs a feed whose poller has actually
    /// rebuilt, and a poller that never ingested cannot produce one.
    #[cfg(test)]
    pub(crate) fn assembler_mut(&mut self) -> Option<&mut VolumeAssembler> {
        self.current.as_mut()
    }

    /// What the volume being assembled declared its cuts' Nyquist velocities to be.
    pub fn declared_nyquist(&self) -> Option<&crate::nyquist::DeclaredNyquist> {
        self.current.as_ref().map(VolumeAssembler::declared_nyquist)
    }

    /// **The decoded volume this poller's assembler holds**, as allocation and
    /// price. See [`VolumeAssembler::cached_allocation`]; no walk.
    pub fn cached_allocation(&self) -> Option<(*const nexrad_model::data::Scan, u64)> {
        self.current.as_ref()?.cached_allocation()
    }

    /// **What the parked queue is holding, as a count and its maintained
    /// byte level** — never as allocations, and that asymmetry is the whole
    /// point of this signature.
    ///
    /// [`Self::pending_closed`] is unbounded and its per-entry price is not
    /// stored; the only figure kept is the running
    /// [`Self::pending_bytes`] the four push/pop seams maintain. Pricing the
    /// entries individually — which is what a union would need to fold them
    /// in by pointer — means a [`crate::scan_size::scan_bytes`] walk of every
    /// parked volume's radials, and this is asked on a telemetry tick.
    ///
    /// So a union built from [`Self::cached_allocation`] does not contain
    /// these bytes, and a caller that prints the union must print this beside
    /// it rather than fold it in or drop it. The queue is drained one per
    /// round and is empty on an ordinary leg, so the count is normally 0 and
    /// says so itself when it is not — which is the reading that tells a
    /// reader whether the union it just read is missing anything.
    pub fn parked_volumes(&self) -> (usize, u64) {
        (self.pending_closed.len(), self.pending_bytes)
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
            // Unconditional, because it is also what releases the retention:
            // a volume that closed short offers nothing and keeps nothing.
            let archive = current.take_archive();
            ClosedVolume {
                progress,
                scan,
                declared_nyquist,
                archive,
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
            closed: self.take_pending(),
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

    /// Rebuild [`VolumeAssembler::snapshot`]'s build inside the round that
    /// invalidated it, so the frame thread never pays for one.
    ///
    /// **A seal is not the only thing that invalidates**, and until 2026-09-07
    /// this returned on `sealed_elevations` alone. The round the start chunk
    /// lands on learns the coverage pattern and the first chunk carrying a
    /// Volume Data Block learns the site; both mark the build stale and
    /// neither seals a cut, so both left a whole-volume rebuild to whoever
    /// asked next — the frame thread, through `ChunkFeedManager::snapshot`.
    /// The start chunk is not a rare case: it is one round in every volume,
    /// and the late-arrival path it also covers is the one where a pane is
    /// already drawing the volume being rebuilt.
    ///
    /// **Cold still stays cold.** A volume nothing has ever asked for is not
    /// built here, which is what keeps a seal-less round free.
    fn warm_snapshot(&mut self, outcome: &PollOutcome) {
        let Some(current) = self.current.as_mut() else {
            return;
        };
        if outcome.sealed_elevations.is_empty() && !current.snapshot_is_stale() {
            return;
        }
        let _ = current.snapshot();
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
            self.park_pending_front(closed);
        }
    }

    /// Take the oldest parked volume, giving its bytes back to
    /// [`feed_bytes`]. It leaves on an outcome, crosses to the frame thread
    /// and is either installed — where `still scans` prices it — or dropped.
    fn take_pending(&mut self) -> Option<ClosedVolume> {
        let closed = self.pending_closed.pop_front()?;
        let bytes = closed_volume_bytes(&closed);
        move_feed_level(self.pending_bytes, self.pending_bytes.saturating_sub(bytes));
        self.pending_bytes = self.pending_bytes.saturating_sub(bytes);
        Some(closed)
    }

    /// Park a volume at the front — it is older than everything queued.
    fn park_pending_front(&mut self, closed: ClosedVolume) {
        self.charge_pending(&closed);
        self.pending_closed.push_front(closed);
    }

    /// Park a volume behind the others.
    fn park_pending_back(&mut self, closed: ClosedVolume) {
        self.charge_pending(&closed);
        self.pending_closed.push_back(closed);
    }

    /// One walk per parked volume, at the park. The queue is drained one an
    /// outcome, so this runs about as often as a volume rolls.
    fn charge_pending(&mut self, closed: &ClosedVolume) {
        let bytes = closed_volume_bytes(closed);
        move_feed_level(self.pending_bytes, self.pending_bytes + bytes);
        self.pending_bytes += bytes;
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
            self.park_pending_back(closed);
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
            closed: self.take_pending(),
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
