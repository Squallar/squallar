//! Copernicus GLO-30 tile names, and the pinned list of which ones exist.

use std::collections::HashSet;

use crate::Res;
use crate::grid::{TileRange, tile_range};
use crate::md5;

/// GLO-30 is `_COG_10_` — one tenth of an arc-second times ten, i.e. 1". The
/// 90 m product spells `_COG_30_` and does not belong in this archive, so the
/// prefix is matched in full rather than skipped over.
pub const NAME_PREFIX: &str = "Copernicus_DSM_COG_10_";

/// The south-west corner of one 1x1 degree DEM cell, in whole degrees.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct Cell {
    pub lat: i32,
    pub lon: i32,
}

/// `Cell { lat: 39, lon: -106 }` -> `Copernicus_DSM_COG_10_N39_00_W106_00_DEM`.
pub fn tile_name(c: Cell) -> String {
    format!(
        "{NAME_PREFIX}{}{:02}_00_{}{:03}_00_DEM",
        if c.lat >= 0 { 'N' } else { 'S' },
        c.lat.abs(),
        if c.lon >= 0 { 'E' } else { 'W' },
        c.lon.abs(),
    )
}

/// The inverse, rejecting anything that is not exactly that shape.
///
/// Latitude is two digits and longitude three, both zero-padded. The padding is
/// what made both earlier parsers wrong in different ways, so the digits are
/// read one ASCII byte at a time and the field widths are checked.
pub fn parse_tile_name(name: &str) -> Option<Cell> {
    let body = name.strip_prefix(NAME_PREFIX)?;
    let body = body.strip_suffix("_DEM")?;
    let body = body.strip_suffix("_00")?;
    let (body, lon_field) = body.rsplit_once('_')?;
    let lat_field = body.strip_suffix("_00")?;
    let lat = signed_field(lat_field, b'N', b'S', 2)?;
    let lon = signed_field(lon_field, b'E', b'W', 3)?;
    if !(-90..=90).contains(&lat) || !(-180..=180).contains(&lon) {
        return None;
    }
    Some(Cell { lat, lon })
}

/// `N08` -> 8, `W006` -> -6.
///
/// The accumulate-per-digit loop is deliberate. Handing `08` to a general
/// number reader is what broke this twice: bash reads a leading zero as octal,
/// so `$((08))` is a syntax error rather than 8, and `10#` had to be spelled on
/// every use.
fn signed_field(field: &str, positive: u8, negative: u8, digits: usize) -> Option<i32> {
    let bytes = field.as_bytes();
    if bytes.len() != digits + 1 {
        return None;
    }
    let sign = match bytes[0] {
        b if b == positive => 1,
        b if b == negative => -1,
        _ => return None,
    };
    let mut value = 0i32;
    for byte in &bytes[1..] {
        if !byte.is_ascii_digit() {
            return None;
        }
        value = value * 10 + i32::from(byte - b'0');
    }
    Some(sign * value)
}

/// The set of cells GLO-30 Public actually publishes.
#[derive(Debug)]
pub struct TileList {
    cells: HashSet<Cell>,
}

/// What `verify` compares against.
pub struct Pin {
    pub md5: &'static str,
    pub count: usize,
    pub bytes: usize,
    pub release: &'static str,
}

impl TileList {
    /// Parse the bucket's `tileList.txt`.
    ///
    /// The file is CRLF-terminated — all 26450 lines, verified 2026-08-27 — so
    /// every line is trimmed on both ends. The shell had to keep an
    /// LF-normalised copy beside the raw one because a raw line pasted into a
    /// URL carries a trailing `\r` and 404s; holding the parsed cells instead
    /// of the raw text removes that hazard rather than working around it.
    ///
    /// A line that is not a GLO-30 name is an error, not a skip: the whole
    /// point of the pin is to notice when the published set changes shape.
    pub fn parse(raw: &[u8]) -> Res<Self> {
        let text = std::str::from_utf8(raw)?;
        let mut cells = HashSet::new();
        let mut rejected = Vec::new();
        for (n, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match parse_tile_name(line) {
                Some(c) => {
                    cells.insert(c);
                }
                None if rejected.len() < 5 => rejected.push(format!("line {}: {line}", n + 1)),
                None => {}
            }
        }
        if !rejected.is_empty() {
            return Err(format!(
                "tileList.txt holds names this build cannot parse:\n     {}\n     \
                 GLO-30 names are `{NAME_PREFIX}<N|S>dd_00_<E|W>ddd_00_DEM`.",
                rejected.join("\n     ")
            )
            .into());
        }
        Ok(Self { cells })
    }

    /// Check the raw bytes against the pin, then parse them.
    ///
    /// The md5 hashes the CRLF bytes, because that is what the bucket serves.
    pub fn verify_and_parse(raw: &[u8], pin: &Pin) -> Res<Self> {
        let got = md5::hex(raw);
        if got != pin.md5 {
            return Err(format!(
                "tileList.txt moved.\n     \
                 pinned md5 {} ({} tiles, {} bytes)\n     \
                 actual md5 {got} ({} bytes)\n     \
                 GLO-30 Public gained or lost tiles. The elevation values \
                 themselves do not change; re-pin TILELIST in src/config.rs, \
                 note the date, and rebuild.",
                pin.md5,
                pin.count,
                pin.bytes,
                raw.len(),
            )
            .into());
        }
        let list = Self::parse(raw)?;
        if list.len() != pin.count {
            return Err(format!(
                "tileList.txt md5 matches the pin but holds {} distinct cells, not {}. \
                 The file has duplicate lines.",
                list.len(),
                pin.count
            )
            .into());
        }
        Ok(list)
    }

    pub fn contains(&self, c: Cell) -> bool {
        self.cells.contains(&c)
    }

    pub fn len(&self) -> usize {
        self.cells.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    /// Every cell, in a fixed order so a rerun enumerates the same work.
    pub fn sorted(&self) -> Vec<Cell> {
        let mut v: Vec<Cell> = self.cells.iter().copied().collect();
        v.sort_unstable();
        v
    }
}

/// One degree-aligned chunk of the contour pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chunk {
    pub w: i32,
    pub s: i32,
    pub e: i32,
    pub n: i32,
    pub name: String,
}

/// The populated `deg`-degree chunks, sorted.
pub fn chunks(list: &TileList, deg: i32) -> Vec<Chunk> {
    let mut corners: Vec<(i32, i32)> = list
        .sorted()
        .into_iter()
        // `div_euclid`, not `/`: truncation toward zero would put lon −106 and
        // lon 106 in cells of different widths.
        .map(|c| (c.lon.div_euclid(deg) * deg, c.lat.div_euclid(deg) * deg))
        .collect();
    corners.sort_unstable();
    corners.dedup();
    corners
        .into_iter()
        .map(|(w, s)| Chunk {
            w,
            s,
            e: w + deg,
            n: s + deg,
            name: format!(
                "chunk_{}{:03}_{}{:02}",
                if w >= 0 { 'E' } else { 'W' },
                w.abs(),
                if s >= 0 { 'N' } else { 'S' },
                s.abs()
            ),
        })
        .collect()
}

/// One block of the raster pass, `side` tiles on an edge at `zoom`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SuperCell {
    pub range: TileRange,
    pub name: String,
}

/// The populated `side`x`side`-tile super-cells at one zoom, sorted.
///
/// A super-cell is the same pixel count everywhere on the globe — 64 tiles a
/// side is 16384x16384 px, 1.07 GB as Float32 — which is the whole reason the
/// raster pass counts tiles where the contour pass counts degrees.
pub fn supercells(list: &TileList, zoom: u8, side: u32) -> Vec<SuperCell> {
    let last = 2u32.saturating_pow(u32::from(zoom)).saturating_sub(1);
    let mut blocks: Vec<(u32, u32)> = Vec::new();
    for c in list.sorted() {
        let r = tile_range(
            zoom,
            f64::from(c.lon),
            f64::from(c.lat),
            f64::from(c.lon + 1),
            f64::from(c.lat + 1),
        );
        for bx in (r.tx0 / side)..=(r.tx1 / side) {
            for by in (r.ty0 / side)..=(r.ty1 / side) {
                blocks.push((bx, by));
            }
        }
    }
    blocks.sort_unstable();
    blocks.dedup();
    blocks
        .into_iter()
        .map(|(bx, by)| {
            let tx0 = bx * side;
            let ty0 = by * side;
            SuperCell {
                range: TileRange {
                    tx0,
                    ty0,
                    tx1: (tx0 + side - 1).min(last),
                    ty1: (ty0 + side - 1).min(last),
                },
                name: format!("sc_z{zoom}_{tx0:06}_{ty0:06}"),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The octal trap, in both fields at once. `N08`/`W006` are the names that
    /// made `$((08))` a bash syntax error.
    #[test]
    fn zero_padded_fields_are_decimal_not_octal() {
        let c = parse_tile_name("Copernicus_DSM_COG_10_N08_00_W006_00_DEM").unwrap();
        assert_eq!(c, Cell { lat: 8, lon: -6 });
        let c = parse_tile_name("Copernicus_DSM_COG_10_N09_00_E009_00_DEM").unwrap();
        assert_eq!(c, Cell { lat: 9, lon: 9 });
        // 08 and 09 are the two that are not valid octal at all; 07 is, and
        // would have parsed to 7 by luck. It must still be 7 here.
        let c = parse_tile_name("Copernicus_DSM_COG_10_N07_00_W007_00_DEM").unwrap();
        assert_eq!(c, Cell { lat: 7, lon: -7 });
    }

    #[test]
    fn both_hemispheres_carry_their_sign() {
        for (name, want) in [
            (
                "Copernicus_DSM_COG_10_N39_00_W106_00_DEM",
                Cell { lat: 39, lon: -106 },
            ),
            (
                "Copernicus_DSM_COG_10_S34_00_E018_00_DEM",
                Cell { lat: -34, lon: 18 },
            ),
            (
                "Copernicus_DSM_COG_10_S01_00_W080_00_DEM",
                Cell { lat: -1, lon: -80 },
            ),
            (
                "Copernicus_DSM_COG_10_N00_00_E000_00_DEM",
                Cell { lat: 0, lon: 0 },
            ),
            (
                "Copernicus_DSM_COG_10_S90_00_W180_00_DEM",
                Cell {
                    lat: -90,
                    lon: -180,
                },
            ),
            (
                "Copernicus_DSM_COG_10_N89_00_E179_00_DEM",
                Cell { lat: 89, lon: 179 },
            ),
        ] {
            assert_eq!(parse_tile_name(name), Some(want), "{name}");
            assert_eq!(tile_name(want), name, "round trip");
        }
    }

    /// A malformed name must be REJECTED, not silently mis-parsed. The awk read
    /// fixed character offsets counted from the end of the string, so any name
    /// of the right length parsed to something.
    #[test]
    fn malformed_names_are_rejected() {
        for bad in [
            "",
            "Copernicus_DSM_COG_10_N39_00_W106_00",
            "Copernicus_DSM_COG_10_N39_00_W106_00_DEM_",
            // Wrong hemisphere letters.
            "Copernicus_DSM_COG_10_X39_00_W106_00_DEM",
            "Copernicus_DSM_COG_10_N39_00_N106_00_DEM",
            // Wrong field widths: latitude is two digits, longitude three.
            "Copernicus_DSM_COG_10_N9_00_W106_00_DEM",
            "Copernicus_DSM_COG_10_N039_00_W106_00_DEM",
            "Copernicus_DSM_COG_10_N39_00_W16_00_DEM",
            "Copernicus_DSM_COG_10_N39_00_W1060_00_DEM",
            // Non-digits inside the digit fields.
            "Copernicus_DSM_COG_10_N3a_00_W106_00_DEM",
            "Copernicus_DSM_COG_10_N39_00_W10+_00_DEM",
            // The minute fields are always 00 in GLO-30.
            "Copernicus_DSM_COG_10_N39_30_W106_00_DEM",
            "Copernicus_DSM_COG_10_N39_00_W106_30_DEM",
            // The 90 m product, which does not belong in this archive.
            "Copernicus_DSM_COG_30_N39_00_W106_00_DEM",
            // Out of range.
            "Copernicus_DSM_COG_10_N99_00_W106_00_DEM",
            "Copernicus_DSM_COG_10_N39_00_W900_00_DEM",
            // No prefix at all.
            "N39_00_W106_00_DEM",
        ] {
            assert_eq!(parse_tile_name(bad), None, "{bad:?} must be rejected");
        }
    }

    fn list_of(cells: &[(i32, i32)]) -> TileList {
        let raw: String = cells
            .iter()
            .map(|&(lat, lon)| format!("{}\r\n", tile_name(Cell { lat, lon })))
            .collect();
        TileList::parse(raw.as_bytes()).unwrap()
    }

    /// CRLF is what the bucket serves, and trimming it is structural rather
    /// than a normalised second copy of the file.
    #[test]
    fn crlf_lines_parse() {
        let l = list_of(&[(39, -106), (-34, 18)]);
        assert_eq!(l.len(), 2);
        assert!(l.contains(Cell { lat: 39, lon: -106 }));
    }

    #[test]
    fn an_unparseable_line_fails_the_list_rather_than_being_skipped() {
        let raw = "Copernicus_DSM_COG_10_N39_00_W106_00_DEM\r\nnot-a-tile\r\n";
        let err = TileList::parse(raw.as_bytes()).unwrap_err().to_string();
        assert!(err.contains("not-a-tile"), "{err}");
    }

    /// Chunk corners floor toward −infinity, so a west-hemisphere cell lands in
    /// a chunk of the same width as an east-hemisphere one.
    #[test]
    fn chunk_corners_floor_toward_minus_infinity() {
        let l = list_of(&[(39, -106), (39, -102), (2, 3)]);
        let c = chunks(&l, 5);
        let names: Vec<&str> = c.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            ["W110_N35", "W105_N35", "E000_N00"].map(|s| format!("chunk_{s}"))
        );
        assert_eq!(c[0].w, -110);
        assert_eq!(c[0].e, -105);
    }

    /// The property that makes tile-counted chunking necessary: the same degree
    /// cell is far taller in pixels at high latitude than at the equator.
    #[test]
    fn a_degree_cell_spans_far_more_tile_rows_at_high_latitude() {
        let rows = |lat: i32| {
            let r = tile_range(12, 0.0, f64::from(lat), 1.0, f64::from(lat + 1));
            r.ty1 - r.ty0 + 1
        };
        assert!(rows(80) > 5 * rows(0), "{} vs {}", rows(80), rows(0));
    }

    /// A super-cell is the same tile count everywhere, high latitude included.
    #[test]
    fn every_supercell_is_the_same_size_regardless_of_latitude() {
        let l = list_of(&[(0, 0), (80, 20), (-60, -70)]);
        for sc in supercells(&l, 12, 64) {
            assert_eq!(sc.range.tx1 - sc.range.tx0 + 1, 64, "{}", sc.name);
            assert_eq!(sc.range.ty1 - sc.range.ty0 + 1, 64, "{}", sc.name);
            assert_eq!(sc.range.tx0 % 64, 0);
            assert_eq!(sc.range.ty0 % 64, 0);
        }
    }

    /// Two cells inside one block produce one super-cell, not two.
    #[test]
    fn supercells_are_deduplicated() {
        let l = list_of(&[(39, -106), (39, -105), (40, -106)]);
        let sc = supercells(&l, 8, 64);
        assert_eq!(sc.len(), 1, "{sc:?}");
    }
}
