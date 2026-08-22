//! dBase III `.dbf` attribute reading, character fields only.
//!
//! Six columns are wanted out of these files (`STATE`, `ZONE`, `FIPS`, `ID`,
//! `id`, `NAME`) and all six are `C` fields, so the numeric and date types are
//! deliberately not implemented: a `LON`/`LAT` centroid this tool never reads
//! would be a decoder to get wrong for nothing.

use std::collections::HashMap;

use rustdar_source::wire::Reader;

#[derive(Debug)]
pub struct Dbf {
    pub fields: Vec<String>,
    /// One map per record, field name to trimmed value. A field that is all
    /// spaces in the file becomes an empty string, not a missing key — the
    /// distinction matters for the high-seas `id` column, which is present and
    /// blank rather than absent.
    pub records: Vec<HashMap<String, String>>,
}

#[derive(Debug)]
pub enum DbfError {
    TooShort,
    NoTerminator,
    BadGeometry(String),
}

impl std::fmt::Display for DbfError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooShort => f.write_str("file is shorter than its own header claims"),
            Self::NoTerminator => f.write_str("field descriptor array has no 0x0D terminator"),
            Self::BadGeometry(s) => write!(f, "{s}"),
        }
    }
}

pub fn read(bytes: &[u8]) -> Result<Dbf, DbfError> {
    if bytes.len() < 32 {
        return Err(DbfError::TooShort);
    }
    let mut h = Reader::new(&bytes[..32]);
    h.take(4).ok_or(DbfError::TooShort)?; // version byte + YY MM DD
    let record_count = h.u32().ok_or(DbfError::TooShort)? as usize;
    let header_len = h.u16().ok_or(DbfError::TooShort)? as usize;
    let record_len = h.u16().ok_or(DbfError::TooShort)? as usize;

    if header_len < 33 || header_len > bytes.len() {
        return Err(DbfError::TooShort);
    }

    // Descriptors run from byte 32 to the 0x0D terminator, 32 bytes each.
    let mut names = Vec::new();
    let mut widths = Vec::new();
    let mut at = 32usize;
    loop {
        if at >= header_len {
            return Err(DbfError::NoTerminator);
        }
        if bytes[at] == 0x0D {
            break;
        }
        if at + 32 > bytes.len() {
            return Err(DbfError::TooShort);
        }
        let name_bytes = &bytes[at..at + 11];
        let name_end = name_bytes.iter().position(|&b| b == 0).unwrap_or(11);
        names.push(String::from_utf8_lossy(&name_bytes[..name_end]).into_owned());
        widths.push(bytes[at + 16] as usize);
        at += 32;
    }

    let stated: usize = 1 + widths.iter().sum::<usize>();
    // A record length that disagrees with the descriptors means the columns are
    // being sliced at the wrong offsets, and every value after the first would
    // be silently shifted. Refuse rather than emit plausible garbage.
    if stated != record_len {
        return Err(DbfError::BadGeometry(format!(
            "record length is {record_len} but the {} field widths sum to {stated}",
            widths.len(),
        )));
    }

    let mut records = Vec::with_capacity(record_count);
    for i in 0..record_count {
        let start = header_len + i * record_len;
        let end = start + record_len;
        if end > bytes.len() {
            return Err(DbfError::TooShort);
        }
        // Byte 0 is the deletion flag: 0x2A marks a row `DELETE`d but not yet
        // packed out. Keeping it would put a retired zone in the pack.
        if bytes[start] == 0x2A {
            records.push(HashMap::new());
            continue;
        }
        let mut row = HashMap::with_capacity(names.len());
        let mut at = start + 1;
        for (name, &w) in names.iter().zip(widths.iter()) {
            let raw = &bytes[at..at + w];
            let value = String::from_utf8_lossy(raw).trim().to_string();
            row.insert(name.clone(), value);
            at += w;
        }
        records.push(row);
    }

    Ok(Dbf {
        fields: names,
        records,
    })
}
