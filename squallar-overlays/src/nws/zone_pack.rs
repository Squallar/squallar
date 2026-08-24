//! The NWS zone pack: every published zone boundary in one indexed file, so a
//! round of alerts is not 1,800 requests to `api.weather.gov`.
//!
//! This module is the format, both halves of it. `tools/nws-zone-pack` is the
//! offline converter: it reads the AWIPS shapefiles, unions the several
//! features one zone is spread across, simplifies with the app's own
//! `simplify_ring`, and calls [`write`] — *this* `write`, by path dependency,
//! so there is one encoder and the reader below cannot drift from it. The app
//! calls [`install`] and [`installed`].
//!
//! The blob layout is deliberately the one [`crate::render::jobs`]'
//! `encode_polygon`/`decode_polygon` already speaks — ring count, then per ring
//! a point count and its `(lat, lon)` pairs, exterior first, holes preserved by
//! never reordering anything — with a polygon count in front, because a zone
//! unioned out of several features is several disjoint parts. It is read
//! through [`squallar_source::wire::Reader`], the workspace's one bounds-checked
//! cursor, so every length that could be corrupt is refused through `bounded`
//! rather than trusted into an allocation.
//!
//! Layout:
//!
//! ```text
//! magic        b"NWSZPK"   6
//! version      u16         2
//! coding       u16         2   0 = f64, 1 = f32, 2 = zigzag-varint deltas
//! quantum_exp  u16         2   coding 2 only: units of 10^-quantum_exp degrees
//! _pad         u16         2
//! epsilon      f64         8   the simplification this pack was built at
//! zone_count   u32         4
//! index                    11 * zone_count   { key: [u8; 7], offset: u32 }
//! end_offset   u32         4   sentinel, so entry i's length is off[i+1]-off[i]
//! blobs                    the rest
//! ```
//!
//! The index is sorted by key and every entry is fixed width, so a lookup is a
//! binary search over a slice: no allocation, no hash map, and nothing decoded
//! but the one zone asked for. The key is a kind byte and six ASCII UGC bytes —
//! `(kind, ugc)`, **never** the bare UGC, because `FLC087` is four different
//! shapes and the same id is a different shape in the fire set than in the
//! public-forecast set. Keying on the id alone would draw a real, filled,
//! correctly coloured polygon in the wrong place.
//!
//! # Where the bytes come from
//!
//! Nothing here reads a file or a URL by itself; a host hands it bytes. That
//! keeps the `cfg` cascade out of the format and puts it where the platforms
//! actually differ — see `nws::zone_pack_source`.

use std::sync::{Arc, RwLock};

use squallar_geo::GeoPolygon;
use squallar_source::wire::Reader;

pub const MAGIC: &[u8; 6] = b"NWSZPK";
pub const VERSION: u16 = 1;
pub const HEADER_LEN: usize = 26;
pub const INDEX_ENTRY_LEN: usize = 11;

/// The file name every target agrees on, so the converter's output, the web
/// deploy's asset and the service worker's route cannot disagree about which
/// file they mean.
pub const PACK_FILE_NAME: &str = "zones.pack";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coding {
    F64,
    F32,
    Varint,
}

impl Coding {
    fn code(self) -> u16 {
        match self {
            Self::F64 => 0,
            Self::F32 => 1,
            Self::Varint => 2,
        }
    }

    fn from_code(code: u16) -> Option<Self> {
        match code {
            0 => Some(Self::F64),
            1 => Some(Self::F32),
            2 => Some(Self::Varint),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::F64 => "f64",
            Self::F32 => "f32",
            Self::Varint => "varint",
        }
    }
}

/// The app's three zone kinds — the three path segments `affectedZones` uses.
///
/// The six shapefiles do **not** map one-to-one onto these: `forecast` is
/// `z` ∪ `mz` ∪ `oz` ∪ `hz`, because a marine or offshore id arrives from the
/// alerts feed under `/zones/forecast/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    Forecast,
    County,
    Fire,
}

impl Kind {
    pub fn byte(self) -> u8 {
        match self {
            Self::Forecast => 0,
            Self::County => 1,
            Self::Fire => 2,
        }
    }

    pub fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            0 => Some(Self::Forecast),
            1 => Some(Self::County),
            2 => Some(Self::Fire),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Forecast => "forecast",
            Self::County => "county",
            Self::Fire => "fire",
        }
    }

    /// The kind segment of a zone URL — `/zones/county/TXC113` → `County`.
    ///
    /// `None` for anything else, which is what keeps an unrecognised zone type
    /// off the pack path and on the HTTP path, where it can still resolve.
    pub fn from_url_segment(segment: &str) -> Option<Self> {
        match segment {
            "forecast" => Some(Self::Forecast),
            "county" => Some(Self::County),
            "fire" => Some(Self::Fire),
            _ => None,
        }
    }
}

/// `(kind, ugc)` as the index spells it: the kind byte, then the id left-
/// aligned in six ASCII bytes, space-padded.
///
/// `None` rather than a truncation for anything that is not one to six ASCII
/// alphanumerics. Truncating a longer id would make it *collide* with a
/// different zone, and a colliding key is the one failure this format cannot
/// afford: it answers with real geometry belonging to somewhere else. Every id
/// in the published corpus is exactly six.
pub fn key(kind: Kind, ugc: &str) -> Option<[u8; 7]> {
    let bytes = ugc.as_bytes();
    if bytes.is_empty() || bytes.len() > 6 || !bytes.iter().all(u8::is_ascii_alphanumeric) {
        return None;
    }
    let mut key = [b' '; 7];
    key[0] = kind.byte();
    key[1..1 + bytes.len()].copy_from_slice(bytes);
    Some(key)
}

// ── varints ──────────────────────────────────────────────────────────────
//
// The read half is on `Reader` (`uvarint`, `zigzag`), where every other
// bounds-checked read lives. Only the write half is here: nothing else in the
// workspace writes one.

fn put_uvarint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push((value as u8) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn put_zigzag(out: &mut Vec<u8>, value: i64) {
    put_uvarint(out, ((value << 1) ^ (value >> 63)) as u64);
}

// ── blobs ────────────────────────────────────────────────────────────────

/// One zone's parts. `quantum` is `10^quantum_exp` and is read only by
/// [`Coding::Varint`].
fn encode_blob(out: &mut Vec<u8>, polygons: &[GeoPolygon], coding: Coding, quantum: f64) {
    match coding {
        Coding::F64 | Coding::F32 => {
            out.extend_from_slice(&(polygons.len() as u32).to_le_bytes());
            for polygon in polygons {
                out.extend_from_slice(&(polygon.len() as u32).to_le_bytes());
                for ring in polygon {
                    out.extend_from_slice(&(ring.len() as u32).to_le_bytes());
                    for &(lat, lon) in ring {
                        if coding == Coding::F64 {
                            out.extend_from_slice(&lat.to_le_bytes());
                            out.extend_from_slice(&lon.to_le_bytes());
                        } else {
                            out.extend_from_slice(&(lat as f32).to_le_bytes());
                            out.extend_from_slice(&(lon as f32).to_le_bytes());
                        }
                    }
                }
            }
        }
        Coding::Varint => {
            // One cursor for the whole zone rather than one per ring: the first
            // vertex of ring n+1 is near the last vertex of ring n, so carrying
            // the cursor across the boundary keeps that delta small too.
            let (mut cursor_lat, mut cursor_lon) = (0i64, 0i64);
            put_uvarint(out, polygons.len() as u64);
            for polygon in polygons {
                put_uvarint(out, polygon.len() as u64);
                for ring in polygon {
                    put_uvarint(out, ring.len() as u64);
                    for &(lat, lon) in ring {
                        let qlat = (lat * quantum).round() as i64;
                        let qlon = (lon * quantum).round() as i64;
                        put_zigzag(out, qlat - cursor_lat);
                        put_zigzag(out, qlon - cursor_lon);
                        cursor_lat = qlat;
                        cursor_lon = qlon;
                    }
                }
            }
        }
    }
}

fn decode_blob(reader: &mut Reader, coding: Coding, quantum: f64) -> Option<Vec<GeoPolygon>> {
    match coding {
        Coding::F64 | Coding::F32 => {
            let per_point = if coding == Coding::F64 { 16 } else { 8 };
            let polygon_count = reader.u32()?;
            let polygon_count = reader.bounded(polygon_count, 4)?;
            let mut polygons = Vec::with_capacity(polygon_count);
            for _ in 0..polygon_count {
                let ring_count = reader.u32()?;
                let ring_count = reader.bounded(ring_count, 4)?;
                let mut polygon = Vec::with_capacity(ring_count);
                for _ in 0..ring_count {
                    let point_count = reader.u32()?;
                    let point_count = reader.bounded(point_count, per_point)?;
                    let mut ring = Vec::with_capacity(point_count);
                    for _ in 0..point_count {
                        if coding == Coding::F64 {
                            ring.push((reader.f64()?, reader.f64()?));
                        } else {
                            ring.push((f64::from(reader.f32()?), f64::from(reader.f32()?)));
                        }
                    }
                    polygon.push(ring);
                }
                polygons.push(polygon);
            }
            Some(polygons)
        }
        Coding::Varint => {
            let (mut cursor_lat, mut cursor_lon) = (0i64, 0i64);
            let polygon_count = reader.uvarint()?.try_into().ok()?;
            // A varint list item is at least one byte, a varint point two —
            // one per ordinate.
            let polygon_count = reader.bounded(polygon_count, 1)?;
            let mut polygons = Vec::with_capacity(polygon_count);
            for _ in 0..polygon_count {
                let ring_count = reader.uvarint()?.try_into().ok()?;
                let ring_count = reader.bounded(ring_count, 1)?;
                let mut polygon = Vec::with_capacity(ring_count);
                for _ in 0..ring_count {
                    let point_count = reader.uvarint()?.try_into().ok()?;
                    let point_count = reader.bounded(point_count, 2)?;
                    let mut ring = Vec::with_capacity(point_count);
                    for _ in 0..point_count {
                        cursor_lat += reader.zigzag()?;
                        cursor_lon += reader.zigzag()?;
                        ring.push((cursor_lat as f64 / quantum, cursor_lon as f64 / quantum));
                    }
                    polygon.push(ring);
                }
                polygons.push(polygon);
            }
            Some(polygons)
        }
    }
}

// ── writing ──────────────────────────────────────────────────────────────

/// One entry as the converter assembles it: the `(kind, ugc)` key from [`key`],
/// and the disjoint parts that one zone's features unioned to.
pub type PackedZone = ([u8; 7], Vec<GeoPolygon>);

/// `zones` must be sorted by key; the index's binary search depends on it and
/// nothing else re-sorts.
pub fn write(zones: &[PackedZone], coding: Coding, quantum_exp: u16, epsilon: f64) -> Vec<u8> {
    let quantum = 10f64.powi(i32::from(quantum_exp));
    let mut blobs = Vec::new();
    let mut offsets = Vec::with_capacity(zones.len() + 1);
    for (_, polygons) in zones {
        offsets.push(blobs.len() as u32);
        encode_blob(&mut blobs, polygons, coding, quantum);
    }
    offsets.push(blobs.len() as u32);

    let mut out = Vec::with_capacity(HEADER_LEN + zones.len() * INDEX_ENTRY_LEN + 4 + blobs.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&coding.code().to_le_bytes());
    out.extend_from_slice(&quantum_exp.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&epsilon.to_le_bytes());
    out.extend_from_slice(&(zones.len() as u32).to_le_bytes());
    for (i, (key, _)) in zones.iter().enumerate() {
        out.extend_from_slice(key);
        out.extend_from_slice(&offsets[i].to_le_bytes());
    }
    out.extend_from_slice(&offsets[zones.len()].to_le_bytes());
    out.extend_from_slice(&blobs);
    out
}

// ── reading ──────────────────────────────────────────────────────────────

/// Why a candidate pack was refused. Every variant means the same thing to the
/// caller — resolve zones over HTTP, as before the pack existed — but they are
/// distinct so the log can say which.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackError {
    /// Not this format, or a version this build does not read.
    NotAPack,
    /// The header parsed and the file cannot hold the index it declares.
    Truncated,
    /// It opened, and it draws nothing.
    ///
    /// The failure this exists for is the silent one: a pack that decoded to
    /// empty geometry would read as a spectacular size win and leave the map
    /// blank, with the fallback tail quietly turning back into the fan-out the
    /// pack was built to remove.
    Undrawable,
}

impl std::fmt::Display for PackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAPack => f.write_str("not an NWSZPK pack of a version this build reads"),
            Self::Truncated => f.write_str("truncated: the index does not fit the file"),
            Self::Undrawable => f.write_str("opened, and decoded no drawable geometry"),
        }
    }
}

/// Zones [`ZonePack::open`] decodes to establish that the pack draws something.
///
/// Spread across the whole index, **first and last included**, so a file whose
/// tail is rubbish is caught too. 64 is a floor no empty encoder can clear and
/// costs well under a millisecond on the 11,651-zone pack.
const PROBE_SAMPLES: usize = 64;

/// A validated pack, owning its bytes and decoding one zone at a time.
///
/// The whole corpus is never resident as polygons: a lookup is a binary search
/// over the index slice and one blob decoded. 3.9 MB of bytes, not 900,000
/// `(f64, f64)` in a `HashMap`.
pub struct ZonePack {
    bytes: Vec<u8>,
    /// Where the fixed-width index starts, and where the blobs do. Both are
    /// established once, by [`ZonePack::open`]; a lookup re-derives nothing.
    index_at: usize,
    blobs_at: usize,
    zone_count: usize,
    coding: Coding,
    quantum: f64,
    epsilon: f64,
}

impl std::fmt::Debug for ZonePack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ZonePack")
            .field("zones", &self.zone_count)
            .field("bytes", &self.bytes.len())
            .field("coding", &self.coding.label())
            .field("epsilon", &self.epsilon)
            .finish()
    }
}

impl ZonePack {
    /// Parse the header, then **prove the pack draws** before accepting it.
    ///
    /// The proof is the point. Every other failure here is loud; a pack that
    /// decoded to empty geometry fails silently and looks like a triumph.
    pub fn open(bytes: Vec<u8>) -> Result<Self, PackError> {
        let mut reader = Reader::new(&bytes);
        if reader.take(6) != Some(MAGIC.as_slice()) || reader.u16() != Some(VERSION) {
            return Err(PackError::NotAPack);
        }
        let coding = reader
            .u16()
            .and_then(Coding::from_code)
            .ok_or(PackError::NotAPack)?;
        let quantum_exp = reader.u16().ok_or(PackError::NotAPack)?;
        let _pad = reader.u16().ok_or(PackError::NotAPack)?;
        let epsilon = reader.f64().ok_or(PackError::NotAPack)?;
        let zone_count = reader.u32().ok_or(PackError::NotAPack)?;
        // `bounded` refuses a count the file could not hold before any of it is
        // sliced, so a corrupt `zone_count` cannot reserve gigabytes.
        let zone_count = reader
            .bounded(zone_count, INDEX_ENTRY_LEN)
            .ok_or(PackError::Truncated)?;
        let index_at = HEADER_LEN;
        let index_len = zone_count * INDEX_ENTRY_LEN + 4;
        reader.take(index_len).ok_or(PackError::Truncated)?;
        let blobs_at = index_at + index_len;

        let pack = Self {
            index_at,
            blobs_at,
            zone_count,
            coding,
            quantum: 10f64.powi(i32::from(quantum_exp)),
            epsilon,
            bytes,
        };
        pack.prove_it_draws()?;
        Ok(pack)
    }

    /// The non-vacuity floor, run once at install: decode a sample spread over
    /// the whole index and insist every one of them is a shape with area.
    fn prove_it_draws(&self) -> Result<(), PackError> {
        if self.zone_count == 0 {
            return Err(PackError::Undrawable);
        }
        // Both ends included, deliberately. A fixed stride from zero leaves a
        // tail of up to `step - 1` zones unprobed — with 200 zones it stops at
        // 196 — and rubbish written at the end of a file is exactly as likely
        // as rubbish written at the start.
        let last = self.zone_count - 1;
        let mut probed = 0usize;
        let mut vertices = 0usize;
        let mut previous: Option<usize> = None;
        for sample in 0..PROBE_SAMPLES.min(self.zone_count) {
            let i = match self.zone_count {
                1 => 0,
                _ => sample * last / (PROBE_SAMPLES.min(self.zone_count) - 1),
            };
            if previous == Some(i) {
                continue;
            }
            previous = Some(i);
            let polygons = self.at(i).ok_or(PackError::Undrawable)?;
            // A polygon list, a ring list and a ring are three separate places
            // an encoder can produce a well-formed nothing. Reject all three: a
            // ring under three points bounds no area, so it paints nothing even
            // though it decoded without complaint.
            if polygons.is_empty() || polygons.iter().any(|polygon| polygon.is_empty()) {
                return Err(PackError::Undrawable);
            }
            for polygon in &polygons {
                if polygon[0].len() < 3 {
                    return Err(PackError::Undrawable);
                }
                vertices += polygon.iter().map(Vec::len).sum::<usize>();
            }
            probed += 1;
        }
        if probed == 0 || vertices == 0 {
            return Err(PackError::Undrawable);
        }
        Ok(())
    }

    fn entry_key(&self, i: usize) -> [u8; 7] {
        let at = self.index_at + i * INDEX_ENTRY_LEN;
        self.bytes[at..at + 7].try_into().expect("7 bytes")
    }

    fn entry_offset(&self, i: usize) -> u32 {
        // The sentinel sits where entry `zone_count`'s key would start, so its
        // offset is read from there directly.
        let at = self.index_at
            + if i == self.zone_count {
                self.zone_count * INDEX_ENTRY_LEN
            } else {
                i * INDEX_ENTRY_LEN + 7
            };
        u32::from_le_bytes(self.bytes[at..at + 4].try_into().expect("4 bytes"))
    }

    /// The `i`th zone's parts in index order. Public for the converter's
    /// verification pass, which walks the whole file.
    pub fn at(&self, i: usize) -> Option<Vec<GeoPolygon>> {
        if i >= self.zone_count {
            return None;
        }
        let start = self.blobs_at.checked_add(self.entry_offset(i) as usize)?;
        let end = self
            .blobs_at
            .checked_add(self.entry_offset(i + 1) as usize)?;
        let slice = self.bytes.get(start..end)?;
        decode_blob(&mut Reader::new(slice), self.coding, self.quantum)
    }

    pub fn key_at(&self, i: usize) -> Option<[u8; 7]> {
        (i < self.zone_count).then(|| self.entry_key(i))
    }

    /// The zone's parts, or `None` if this pack does not carry that
    /// `(kind, ugc)` — a routine answer, not a fault: the pack is one published
    /// edition and the alerts feed still names ids that edition retired.
    pub fn get(&self, kind: Kind, ugc: &str) -> Option<Vec<GeoPolygon>> {
        let key = key(kind, ugc)?;
        let mut lo = 0usize;
        let mut hi = self.zone_count;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            match self.entry_key(mid).cmp(&key) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => return self.at(mid),
            }
        }
        None
    }

    pub fn zone_count(&self) -> usize {
        self.zone_count
    }

    pub fn byte_len(&self) -> usize {
        self.bytes.len()
    }

    pub fn coding(&self) -> Coding {
        self.coding
    }

    /// The simplification tolerance the pack was built at. Worth comparing to
    /// [`crate::types::SIMPLIFY_EPSILON`]: a pack built coarser than the app's
    /// own tolerance draws visibly blockier zones than a fetched one.
    pub fn epsilon(&self) -> f64 {
        self.epsilon
    }
}

// ── the installed pack ───────────────────────────────────────────────────

/// The pack this process resolves zones against.
///
/// A lock rather than a `OnceLock` because web installs asynchronously, after
/// the first frames are already drawn. `RwLock` compiles and behaves on
/// `wasm32-unknown-unknown`, where it is single-threaded and never contended.
static INSTALLED: RwLock<Option<Arc<ZonePack>>> = RwLock::new(None);

/// The installed pack, or `None`.
///
/// `None` is not an error path: it is the behaviour that shipped before the
/// pack existed — every zone over HTTP — so a missing or rejected artifact
/// costs correctness nothing and costs only requests.
pub fn installed() -> Option<Arc<ZonePack>> {
    INSTALLED.read().ok().and_then(|slot| slot.clone())
}

/// Validate `bytes` and make them the installed pack, returning it so the
/// caller can log what it got without a second read of the lock.
pub fn install(bytes: Vec<u8>) -> Result<Arc<ZonePack>, PackError> {
    let pack = Arc::new(ZonePack::open(bytes)?);
    if let Ok(mut slot) = INSTALLED.write() {
        *slot = Some(Arc::clone(&pack));
    }
    Ok(pack)
}

#[cfg(test)]
mod tests;
