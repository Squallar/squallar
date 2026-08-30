//! Copernicus GLO-30 tile names, and the pinned list of which ones exist.

use std::collections::HashSet;

use crate::Res;
use crate::grid::{LonLatBox, TileRange, tile_range};
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

/// Drop every super-cell whose tile extent misses `bbox` at `zoom`.
///
/// THE LEVER `ONLY_SUPERCELL` COULD NOT BE. That one is
/// `name.contains(filter)` against `sc_z11_000320_000640`, and a region is a
/// two-dimensional block range: CONUS at z11 with `SUPERCELL=64` is columns
/// 5-10 by rows 10-13, nine populated blocks whose names agree on neither
/// field. No substring selects them, and asking for a region with no lever at
/// all builds the globe — 1024 z11 blocks against nine.
///
/// The clip is per-BLOCK, not per-tile: a super-cell that overlaps the box at
/// all is built whole, so the region actually produced is `bbox` rounded
/// outward to super-cell boundaries. Cutting a block in half would break the
/// invariant the whole raster pass rests on — every super-cell is the same
/// pixel count everywhere on the globe.
pub fn clip_to_bbox(cells: &mut Vec<SuperCell>, zoom: u8, bbox: LonLatBox) {
    let want = bbox.tile_range(zoom);
    cells.retain(|c| c.range.intersects(want));
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

    /// CONUS, the box the terrain-RGB z11-z12 archive is actually built with.
    const CONUS: LonLatBox = LonLatBox {
        w: -125.0,
        s: 24.0,
        e: -66.0,
        n: 50.0,
    };

    /// Five CONUS degree cells spread to the corners, plus four that must not
    /// survive: Alberta (north of the box), France, Japan and Sydney.
    fn a_global_scattering() -> TileList {
        list_of(&[
            (39, -106),
            (24, -98),
            (48, -123),
            (25, -81),
            (48, -68),
            (55, -115),
            (48, 7),
            (35, 139),
            (-33, 151),
        ])
    }

    /// The retained set is a two-DIMENSIONAL block range, and it is spelled out
    /// rather than re-derived: block columns 5-10 by rows 10-13 at z11 with
    /// `SUPERCELL=64`, which is `tx0` in {320, 384, 448, 512, 576, 640} against
    /// `ty0` in {640, 704, 768, 832}.
    ///
    /// Alberta at 55N is the interesting drop. Its northern block (row 9) goes,
    /// but its southern one (row 10) is the SAME block Washington already
    /// contributes -- a super-cell 64 tiles tall spans a lot of latitude, and
    /// the clip is per-block by construction. Asserting the whole set is what
    /// catches that; asserting "Alberta is gone" would be false.
    #[test]
    fn a_conus_bbox_keeps_the_conus_block_range_and_drops_the_rest() {
        let mut cells = supercells(&a_global_scattering(), 11, 64);
        let before: Vec<String> = cells.iter().map(|c| c.name.clone()).collect();
        clip_to_bbox(&mut cells, 11, CONUS);
        let after: Vec<&str> = cells.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            after,
            [
                "sc_z11_000320_000640",
                "sc_z11_000320_000704",
                "sc_z11_000384_000768",
                "sc_z11_000448_000832",
                "sc_z11_000512_000832",
                "sc_z11_000576_000640",
                "sc_z11_000576_000704",
                "sc_z11_000640_000640",
                "sc_z11_000640_000704",
            ],
            "unclipped set was {before:?}"
        );
        // Every dropped block, named, so a clip that widens is caught too.
        for gone in [
            "sc_z11_000320_000576", // Alberta, block row 9
            "sc_z11_001024_000640", // France
            "sc_z11_001024_000704",
            "sc_z11_001792_000768", // Japan
            "sc_z11_001856_001216", // Sydney
        ] {
            assert!(
                before.iter().any(|n| n == gone),
                "{gone} was never enumerated"
            );
            assert!(!after.contains(&gone), "{gone} survived the clip");
        }
    }

    /// WHY THE BOX EXISTS. `ONLY_SUPERCELL` is `name.contains(filter)`, and no
    /// filter of that shape selects the CONUS block range: exhaustively, every
    /// substring of a retained name either misses one of the nine or catches
    /// one of the five that must go.
    ///
    /// Enumerating substrings of ONE retained name is complete, not a sample --
    /// a filter that matches all nine must be a substring of each of them.
    #[test]
    fn no_substring_filter_can_express_the_conus_block_range() {
        let list = a_global_scattering();
        let mut want = supercells(&list, 11, 64);
        clip_to_bbox(&mut want, 11, CONUS);
        let want: Vec<String> = want.into_iter().map(|c| c.name).collect();
        assert_eq!(want.len(), 9);

        let first = want[0].clone();
        for lo in 0..first.len() {
            for hi in (lo + 1)..=first.len() {
                let Some(filter) = first.get(lo..hi) else {
                    continue;
                };
                let mut got = supercells(&list, 11, 64);
                got.retain(|c| c.name.contains(filter));
                let got: Vec<String> = got.into_iter().map(|c| c.name).collect();
                assert_ne!(
                    got, want,
                    "ONLY_SUPERCELL={filter:?} would have expressed the region \
                     and RASTER_BBOX would be unnecessary"
                );
            }
        }
    }

    /// The unfiltered z11 globe, for scale: the second archive command without
    /// a region lever does not build a region, it builds this.
    #[test]
    fn a_missing_bbox_builds_every_land_block_on_the_globe() {
        let mut cells = supercells(&a_global_scattering(), 11, 64);
        assert_eq!(cells.len(), 14);
        clip_to_bbox(&mut cells, 11, CONUS);
        assert_eq!(cells.len(), 9);
    }

    /// A region's EAST border, where a super-cell overlaps the box by exactly
    /// ONE tile column. This is the half of a clip nobody looks at: an
    /// exclusive comparison drops the border blocks and the terrain just ends
    /// short of the coast, in the direction the operator is least likely to
    /// pan.
    ///
    /// `SUPERCELL=8` rather than 64 to reach the case: CONUS's east edge is
    /// tile column 648 at z11, which is a multiple of 8 and not of 64, so the
    /// block starting at 648 touches the box on exactly its first column.
    #[test]
    fn the_clip_keeps_a_block_the_box_touches_by_one_tile() {
        let want = CONUS.tile_range(11);
        assert_eq!(
            (want.tx0, want.ty0, want.tx1, want.ty1),
            (312, 694, 648, 883)
        );
        // One degree cell straddling the east edge, at 39N 67W.
        let mut cells = supercells(&list_of(&[(39, -67)]), 11, 8);
        assert_eq!(cells.len(), 4);
        clip_to_bbox(&mut cells, 11, CONUS);
        let names: Vec<&str> = cells.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "sc_z11_000640_000768",
                "sc_z11_000640_000776",
                // tile columns 648-655: the box reaches 648 and stops.
                "sc_z11_000648_000768",
                "sc_z11_000648_000776",
            ]
        );
    }

    /// Two cells inside one block produce one super-cell, not two.
    #[test]
    fn supercells_are_deduplicated() {
        let l = list_of(&[(39, -106), (39, -105), (40, -106)]);
        let sc = supercells(&l, 8, 64);
        assert_eq!(sc.len(), 1, "{sc:?}");
    }
}
