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
#[derive(Debug, Default)]
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
pub async fn download_chunk(sources: &crate::sources::DataSources, id: &ChunkId) -> Result<Vec<u8>> {
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

    let prefixes =
        crate::archive::collect_common_prefixes(&bucket, &prefix, DELIMITER, |url| {
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
                Ok(at.filter(|m| *m >= 0).map(|m| base + chrono::Duration::minutes(m)))
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
        let url = crate::archive::list_url_delimited(
            &sources.level2_chunks_bucket,
            "KTLX/",
            "/",
            None,
        )
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
        let chunks = list_chunks(&sources, "KTLX", volume).await.expect("listing");

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

    /// End to end: the start chunk decodes, and it is where the VCP comes from.
    #[ignore = "hits the live unidata-nexrad-level2-chunks S3 bucket"]
    #[tokio::test]
    async fn live_a_start_chunk_decodes_and_carries_the_coverage_pattern() {
        let sources = crate::sources::DataSources::production();
        crate::tls::init();
        let volume = latest_volume(&sources, "KTLX").await.expect("discovery");
        let chunks = list_chunks(&sources, "KTLX", volume).await.expect("listing");

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
