//! ESRI shapefile (`.shp`) reading, Polygon records only.
//!
//! Hand-written rather than pulled from crates.io. The reasoning is in the
//! report and in `main.rs`'s header, but the short version: the polygon record
//! is a length-prefixed array of `f64` pairs behind two big-endian integers,
//! the workspace already owns the bounds-checked cursor this needs
//! ([`squallar_source::wire::Reader`]), and the result is checked feature-for-
//! feature and vertex-for-vertex against GDAL's own reader, which is a
//! genuinely independent implementation in another language.
//!
//! Byte order in this format is mixed and that is not a typo: the file header's
//! length and every *record* header are **big**-endian, while the geometry
//! itself is little-endian. `Reader` is little-endian by policy, so the four
//! big-endian fields are spelled out by hand.

use squallar_source::wire::Reader;

/// A shapefile's own record count comes from the `.shx` index, whose size is
/// `100 + 8 * records`. That is an independent count of the same thing the
/// `.shp` record loop produces, and the two disagreeing means one of them is
/// wrong — which is the only reason this tool reads `.shx` at all.
pub fn shx_record_count(shx: &[u8]) -> Option<usize> {
    (shx.len() >= 100 && (shx.len() - 100).is_multiple_of(8)).then(|| (shx.len() - 100) / 8)
}

/// One `.shp` record: the rings exactly as stored, in `(x, y)` = `(lon, lat)`
/// file order, still closed (first vertex repeated last).
///
/// No hole/exterior distinction here — that is a *ring orientation* question,
/// and it belongs one layer up in [`crate::rings`] where the convention can be
/// stated once.
#[derive(Debug, Default)]
pub struct ShpRecord {
    pub rings: Vec<Vec<(f64, f64)>>,
}

#[derive(Debug)]
pub enum ShpError {
    TooShort,
    BadMagic(i32),
    UnsupportedShapeType(i32),
    TruncatedRecord(usize),
}

impl std::fmt::Display for ShpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooShort => f.write_str("file is shorter than a 100-byte header"),
            Self::BadMagic(code) => write!(f, "file code is {code}, not 9994"),
            Self::UnsupportedShapeType(t) => write!(f, "shape type {t} is not Polygon (5)"),
            Self::TruncatedRecord(n) => write!(f, "record {n} runs past the end of the file"),
        }
    }
}

fn be_i32(r: &mut Reader) -> Option<i32> {
    Some(i32::from_be_bytes(r.take(4)?.try_into().ok()?))
}

/// Every Polygon record in `bytes`, in file order.
///
/// A Null shape (type 0) is a legal record with no geometry; it comes back as
/// a record with zero rings rather than being skipped, so record *n* here is
/// still DBF row *n*.
pub fn read_polygons(bytes: &[u8]) -> Result<Vec<ShpRecord>, ShpError> {
    if bytes.len() < 100 {
        return Err(ShpError::TooShort);
    }
    let mut head = Reader::new(&bytes[..100]);
    let code = be_i32(&mut head).ok_or(ShpError::TooShort)?;
    if code != 9994 {
        return Err(ShpError::BadMagic(code));
    }
    // Bytes 4..24 are unused, 24..28 the big-endian file length in 16-bit
    // words, 28..32 the version, 32..36 the shape type.
    head.take(20).ok_or(ShpError::TooShort)?;
    let file_len_words = be_i32(&mut head).ok_or(ShpError::TooShort)?;
    let _version = head.u32().ok_or(ShpError::TooShort)?;
    let shape_type = head.u32().ok_or(ShpError::TooShort)? as i32;
    if shape_type != 5 {
        return Err(ShpError::UnsupportedShapeType(shape_type));
    }

    // The header's declared length is authoritative over the file's actual
    // size: a shapefile that was truncated in transit still parses record after
    // record until it runs out, and would report a short but "clean" corpus.
    let declared_end = (file_len_words as usize).saturating_mul(2);
    let end = declared_end.min(bytes.len());

    let mut records = Vec::new();
    let mut at = 100usize;
    while at + 8 <= end {
        let mut rh = Reader::new(&bytes[at..at + 8]);
        let _record_number = be_i32(&mut rh).ok_or(ShpError::TruncatedRecord(records.len()))?;
        let content_words = be_i32(&mut rh).ok_or(ShpError::TruncatedRecord(records.len()))?;
        let content_len = (content_words.max(0) as usize) * 2;
        let body_start = at + 8;
        let body_end = body_start
            .checked_add(content_len)
            .ok_or(ShpError::TruncatedRecord(records.len()))?;
        if body_end > end {
            return Err(ShpError::TruncatedRecord(records.len()));
        }
        records.push(read_one(&bytes[body_start..body_end], records.len())?);
        at = body_end;
    }
    Ok(records)
}

fn read_one(body: &[u8], index: usize) -> Result<ShpRecord, ShpError> {
    let mut r = Reader::new(body);
    let shape_type = r.u32().ok_or(ShpError::TruncatedRecord(index))? as i32;
    match shape_type {
        0 => return Ok(ShpRecord::default()),
        5 => {}
        other => return Err(ShpError::UnsupportedShapeType(other)),
    }
    // The record's own bounding box: four f64, and this tool recomputes bounds
    // from the vertices rather than trusting it, so it is skipped.
    r.take(32).ok_or(ShpError::TruncatedRecord(index))?;

    let num_parts = r.u32().ok_or(ShpError::TruncatedRecord(index))?;
    let num_points = r.u32().ok_or(ShpError::TruncatedRecord(index))?;
    // `bounded` is the reason a corrupt count cannot reserve gigabytes here:
    // it refuses a count the remaining bytes could not possibly hold.
    let num_parts = r
        .bounded(num_parts, 4)
        .ok_or(ShpError::TruncatedRecord(index))?;
    let mut starts = Vec::with_capacity(num_parts);
    for _ in 0..num_parts {
        starts.push(r.u32().ok_or(ShpError::TruncatedRecord(index))? as usize);
    }
    let num_points = r
        .bounded(num_points, 16)
        .ok_or(ShpError::TruncatedRecord(index))?;
    let mut points = Vec::with_capacity(num_points);
    for _ in 0..num_points {
        let x = r.f64().ok_or(ShpError::TruncatedRecord(index))?;
        let y = r.f64().ok_or(ShpError::TruncatedRecord(index))?;
        points.push((x, y));
    }

    let mut rings = Vec::with_capacity(num_parts);
    for (i, &start) in starts.iter().enumerate() {
        let stop = starts.get(i + 1).copied().unwrap_or(points.len());
        if start > stop || stop > points.len() {
            return Err(ShpError::TruncatedRecord(index));
        }
        rings.push(points[start..stop].to_vec());
    }
    Ok(ShpRecord { rings })
}
