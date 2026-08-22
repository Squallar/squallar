//! The pack format: one file, an index in front, zone geometry behind it.
//!
//! The blob layout is deliberately the one `rustdar-overlays`'
//! `render::jobs::encode_polygon`/`decode_polygon` already speaks — ring count,
//! then per ring a point count and its `(lat, lon)` pairs, exterior first,
//! holes preserved by never reordering anything — with a polygon count added in
//! front so a zone can be several disjoint parts. The reader is built on
//! [`rustdar_source::wire::Reader`], the workspace's one bounds-checked cursor,
//! so every length that could be corrupt is refused through `bounded` rather
//! than trusted into an allocation.
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
//! binary search over a slice and needs no allocation and no hash map. The key
//! is a kind byte and six ASCII UGC bytes — `(type, ugc)`, never the bare UGC,
//! because `FLC087` is four different shapes and `OKZ001` is a different shape
//! in the fire set than in the public-forecast set.

use rustdar_geo::GeoPolygon;
use rustdar_source::wire::Reader;

pub const MAGIC: &[u8; 6] = b"NWSZPK";
pub const VERSION: u16 = 1;
pub const HEADER_LEN: usize = 26;
pub const INDEX_ENTRY_LEN: usize = 11;

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
    fn from_code(c: u16) -> Option<Self> {
        match c {
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

/// The app's three zone kinds. The six shapefiles do **not** map one-to-one
/// onto these: `forecast` is `z` ∪ `mz` ∪ `oz` ∪ `hz`, because a marine or
/// offshore id arrives from the alerts feed under `/zones/forecast/`.
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
    pub fn from_byte(b: u8) -> Option<Self> {
        match b {
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
}

pub fn key(kind: Kind, ugc: &str) -> [u8; 7] {
    let mut k = [b' '; 7];
    k[0] = kind.byte();
    for (i, b) in ugc.bytes().take(6).enumerate() {
        k[1 + i] = b;
    }
    k
}

// ── varints ──────────────────────────────────────────────────────────────

fn put_uvarint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn put_zigzag(out: &mut Vec<u8>, v: i64) {
    put_uvarint(out, ((v << 1) ^ (v >> 63)) as u64);
}

/// LEB128 over [`Reader::u8`], so it inherits the cursor's bounds checking and
/// its `None`-on-short-buffer policy. Capped at ten groups because a `u64`
/// cannot need more, and an unterminated run of `0x80`s would otherwise spin to
/// the end of the file.
///
/// Shipping this format would want these two on `Reader` itself rather than
/// here; see the report.
fn uvarint(r: &mut Reader) -> Option<u64> {
    let mut v: u64 = 0;
    for group in 0..10 {
        let b = r.u8()?;
        v |= u64::from(b & 0x7F) << (7 * group);
        if b & 0x80 == 0 {
            return Some(v);
        }
    }
    None
}

fn zigzag(r: &mut Reader) -> Option<i64> {
    let u = uvarint(r)?;
    Some(((u >> 1) as i64) ^ -((u & 1) as i64))
}

// ── blobs ────────────────────────────────────────────────────────────────

/// One zone's parts. `quantum` is `10^quantum_exp` and is read only by
/// [`Coding::Varint`].
fn encode_blob(out: &mut Vec<u8>, polys: &[GeoPolygon], coding: Coding, quantum: f64) {
    match coding {
        Coding::F64 | Coding::F32 => {
            out.extend_from_slice(&(polys.len() as u32).to_le_bytes());
            for poly in polys {
                out.extend_from_slice(&(poly.len() as u32).to_le_bytes());
                for ring in poly {
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
            let (mut clat, mut clon) = (0i64, 0i64);
            put_uvarint(out, polys.len() as u64);
            for poly in polys {
                put_uvarint(out, poly.len() as u64);
                for ring in poly {
                    put_uvarint(out, ring.len() as u64);
                    for &(lat, lon) in ring {
                        let qlat = (lat * quantum).round() as i64;
                        let qlon = (lon * quantum).round() as i64;
                        put_zigzag(out, qlat - clat);
                        put_zigzag(out, qlon - clon);
                        clat = qlat;
                        clon = qlon;
                    }
                }
            }
        }
    }
}

fn decode_blob(r: &mut Reader, coding: Coding, quantum: f64) -> Option<Vec<GeoPolygon>> {
    match coding {
        Coding::F64 | Coding::F32 => {
            let per_point = if coding == Coding::F64 { 16 } else { 8 };
            let n = r.u32()?;
            let n = r.bounded(n, 4)?;
            let mut polys = Vec::with_capacity(n);
            for _ in 0..n {
                let rc = r.u32()?;
                let rc = r.bounded(rc, 4)?;
                let mut poly = Vec::with_capacity(rc);
                for _ in 0..rc {
                    let pc = r.u32()?;
                    let pc = r.bounded(pc, per_point)?;
                    let mut ring = Vec::with_capacity(pc);
                    for _ in 0..pc {
                        if coding == Coding::F64 {
                            ring.push((r.f64()?, r.f64()?));
                        } else {
                            ring.push((r.f32()? as f64, r.f32()? as f64));
                        }
                    }
                    poly.push(ring);
                }
                polys.push(poly);
            }
            Some(polys)
        }
        Coding::Varint => {
            let (mut clat, mut clon) = (0i64, 0i64);
            let n = uvarint(r)? as u32;
            // A varint point is at least two bytes, one per ordinate.
            let n = r.bounded(n, 1)?;
            let mut polys = Vec::with_capacity(n);
            for _ in 0..n {
                let rc = uvarint(r)? as u32;
                let rc = r.bounded(rc, 1)?;
                let mut poly = Vec::with_capacity(rc);
                for _ in 0..rc {
                    let pc = uvarint(r)? as u32;
                    let pc = r.bounded(pc, 2)?;
                    let mut ring = Vec::with_capacity(pc);
                    for _ in 0..pc {
                        clat += zigzag(r)?;
                        clon += zigzag(r)?;
                        ring.push((clat as f64 / quantum, clon as f64 / quantum));
                    }
                    poly.push(ring);
                }
                polys.push(poly);
            }
            Some(polys)
        }
    }
}

// ── the file ─────────────────────────────────────────────────────────────

/// `zones` must be sorted by key; the index's binary search depends on it and
/// nothing else re-sorts.
pub fn write(
    zones: &[([u8; 7], Vec<GeoPolygon>)],
    coding: Coding,
    quantum_exp: u16,
    epsilon: f64,
) -> Vec<u8> {
    let quantum = 10f64.powi(i32::from(quantum_exp));
    let mut blobs = Vec::new();
    let mut offsets = Vec::with_capacity(zones.len() + 1);
    for (_, polys) in zones {
        offsets.push(blobs.len() as u32);
        encode_blob(&mut blobs, polys, coding, quantum);
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
    for (i, (k, _)) in zones.iter().enumerate() {
        out.extend_from_slice(k);
        out.extend_from_slice(&offsets[i].to_le_bytes());
    }
    out.extend_from_slice(&offsets[zones.len()].to_le_bytes());
    out.extend_from_slice(&blobs);
    out
}

pub struct Pack<'a> {
    index: &'a [u8],
    blobs: &'a [u8],
    pub zone_count: usize,
    pub coding: Coding,
    pub quantum: f64,
    pub epsilon: f64,
}

impl<'a> Pack<'a> {
    pub fn open(bytes: &'a [u8]) -> Option<Self> {
        let mut r = Reader::new(bytes);
        if r.take(6)? != MAGIC {
            return None;
        }
        if r.u16()? != VERSION {
            return None;
        }
        let coding = Coding::from_code(r.u16()?)?;
        let quantum_exp = r.u16()?;
        let _pad = r.u16()?;
        let epsilon = r.f64()?;
        let zone_count = r.u32()?;
        // The index is `zone_count` fixed-width entries plus the sentinel, and
        // `bounded` refuses a count the file could not hold before any of it is
        // sliced.
        let zone_count = r.bounded(zone_count, INDEX_ENTRY_LEN)?;
        let index_len = zone_count * INDEX_ENTRY_LEN + 4;
        let index = r.take(index_len)?;
        Some(Self {
            index,
            blobs: r.rest(),
            zone_count,
            coding,
            quantum: 10f64.powi(i32::from(quantum_exp)),
            epsilon,
        })
    }

    fn entry_key(&self, i: usize) -> [u8; 7] {
        let at = i * INDEX_ENTRY_LEN;
        self.index[at..at + 7].try_into().expect("7 bytes")
    }

    fn entry_offset(&self, i: usize) -> u32 {
        // The sentinel sits where entry `zone_count`'s key would start, so its
        // offset is read from there directly.
        let at = if i == self.zone_count {
            self.zone_count * INDEX_ENTRY_LEN
        } else {
            i * INDEX_ENTRY_LEN + 7
        };
        u32::from_le_bytes(self.index[at..at + 4].try_into().expect("4 bytes"))
    }

    pub fn key_at(&self, i: usize) -> [u8; 7] {
        self.entry_key(i)
    }

    pub fn get(&self, key: &[u8; 7]) -> Option<Vec<GeoPolygon>> {
        let mut lo = 0usize;
        let mut hi = self.zone_count;
        while lo < hi {
            let mid = (lo + hi) / 2;
            match self.entry_key(mid).cmp(key) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => return self.at(mid),
            }
        }
        None
    }

    pub fn at(&self, i: usize) -> Option<Vec<GeoPolygon>> {
        if i >= self.zone_count {
            return None;
        }
        let start = self.entry_offset(i) as usize;
        let end = self.entry_offset(i + 1) as usize;
        let slice = self.blobs.get(start..end)?;
        let mut r = Reader::new(slice);
        decode_blob(&mut r, self.coding, self.quantum)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Three zones, deliberately awkward: a zone with a hole, a zone in two
    /// disjoint parts (the shape a unioned multi-feature zone takes), and a
    /// zone whose UGC collides with the first one's under a *different* kind —
    /// which is the whole reason the key is `(kind, ugc)` and not the UGC.
    fn corpus() -> Vec<([u8; 7], Vec<GeoPolygon>)> {
        let square = |lat: f64, lon: f64, r: f64| -> Vec<(f64, f64)> {
            vec![
                (lat - r, lon - r),
                (lat - r, lon + r),
                (lat + r, lon + r),
                (lat + r, lon - r),
                (lat - r, lon - r),
            ]
        };
        let mut v = vec![
            (
                key(Kind::County, "FLC087"),
                vec![vec![square(24.7, -81.4, 0.3), square(24.7, -81.4, 0.1)]],
            ),
            (
                key(Kind::Forecast, "AMZ350"),
                vec![
                    vec![square(30.0, -88.0, 0.5)],
                    vec![square(31.0, -87.0, 0.25)],
                ],
            ),
            (
                key(Kind::Fire, "FLC087"),
                vec![vec![square(25.0, -80.0, 1.0)]],
            ),
        ];
        v.sort_by_key(|(k, _)| *k);
        v
    }

    #[test]
    fn every_coding_round_trips_holes_parts_and_ordering() {
        for (coding, qexp) in [(Coding::F64, 0), (Coding::F32, 0), (Coding::Varint, 5)] {
            let zones = corpus();
            let bytes = write(&zones, coding, qexp, 0.005);
            let p = Pack::open(&bytes).expect("the writer's own output must open");
            assert_eq!(p.zone_count, zones.len());
            let tol = match coding {
                Coding::F64 => 0.0,
                Coding::F32 => 1e-5,
                Coding::Varint => 1e-5,
            };
            let mut compared = 0;
            for (k, want) in &zones {
                let got = p.get(k).expect("every key is findable by binary search");
                assert_eq!(got.len(), want.len(), "{coding:?}: polygon count");
                for (gp, wp) in got.iter().zip(want) {
                    // Ring 0 is the exterior and the rest are holes; a codec
                    // that reordered rings would turn an island into a lake.
                    assert_eq!(gp.len(), wp.len(), "{coding:?}: ring count");
                    for (gr, wr) in gp.iter().zip(wp) {
                        assert_eq!(gr.len(), wr.len(), "{coding:?}: point count");
                        for (&(ga, go), &(wa, wo)) in gr.iter().zip(wr) {
                            assert!(
                                (ga - wa).abs() <= tol && (go - wo).abs() <= tol,
                                "{coding:?}: ({ga}, {go}) is not ({wa}, {wo})",
                            );
                            compared += 1;
                        }
                    }
                }
            }
            // The non-vacuity floor: a decoder that returned empty geometry
            // would satisfy every loop above without executing one of them.
            assert_eq!(compared, 25, "{coding:?}: the round-trip compared nothing");
        }
    }

    #[test]
    fn the_same_ugc_under_two_kinds_is_two_different_shapes() {
        let zones = corpus();
        let bytes = write(&zones, Coding::Varint, 5, 0.005);
        let p = Pack::open(&bytes).expect("open");
        let county = p.get(&key(Kind::County, "FLC087")).expect("county FLC087");
        let fire = p.get(&key(Kind::Fire, "FLC087")).expect("fire FLC087");
        assert_ne!(
            county[0][0][0], fire[0][0][0],
            "keying by the bare UGC would have collapsed these into one shape",
        );
        assert!(p.get(&key(Kind::Forecast, "FLC087")).is_none());
    }

    #[test]
    fn a_truncated_pack_is_refused_rather_than_panicking() {
        let bytes = write(&corpus(), Coding::Varint, 5, 0.005);
        assert!(
            Pack::open(&bytes).is_some(),
            "premise: the whole pack opens"
        );
        for cut in [0, 5, 10, 25, 30, 40, bytes.len() - 1] {
            // `open` may legitimately succeed on a truncated tail; what must
            // never happen is a panic, and what must not happen is a decode
            // claiming geometry the bytes do not contain.
            if let Some(p) = Pack::open(&bytes[..cut]) {
                for i in 0..p.zone_count {
                    let _ = p.at(i);
                }
            }
        }
    }

    #[test]
    fn a_corrupt_length_cannot_reserve_more_than_the_file_holds() {
        let mut bytes = write(&corpus(), Coding::F64, 0, 0.005);
        // Overwrite the first blob's polygon count with 4 billion.
        let blob_start = HEADER_LEN + 3 * INDEX_ENTRY_LEN + 4;
        bytes[blob_start..blob_start + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        let p = Pack::open(&bytes).expect("the header is still intact");
        assert!(
            p.at(0).is_none(),
            "a polygon count the blob cannot hold must be refused, not allocated",
        );
    }
}
