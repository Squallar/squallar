//! MBTiles is SQLite, so the raster analogue of `tile-join` is an ATTACH and an
//! INSERT. `sqlite3` does it; nothing here needs a SQLite binding.

use std::path::Path;

use crate::Res;
use crate::run::{capture, cmd};

fn sql(db: &Path, statement: &str) -> Res<String> {
    capture(cmd("sqlite3", &[db.to_string_lossy().as_ref(), statement]))
}

/// Append `src`'s tiles to `dst`, creating `dst` if it does not exist.
///
/// Super-cells abut rather than overlap; `OR REPLACE` only makes a resumed cell
/// idempotent.
pub fn merge(src: &Path, dst: &Path) -> Res<()> {
    if !dst.exists() {
        std::fs::copy(src, dst)?;
        return Ok(());
    }
    sql(
        dst,
        &format!(
            "ATTACH DATABASE '{}' AS s;
             INSERT OR REPLACE INTO tiles (zoom_level, tile_column, tile_row, tile_data)
               SELECT zoom_level, tile_column, tile_row, tile_data FROM s.tiles;
             DETACH DATABASE s;",
            src.display()
        ),
    )?;
    Ok(())
}

/// The distinct zoom levels an archive holds, ascending.
pub fn zoom_levels(db: &Path) -> Res<Vec<u8>> {
    let out = sql(
        db,
        "SELECT DISTINCT zoom_level FROM tiles ORDER BY zoom_level;",
    )?;
    out.split_whitespace()
        .map(|s| s.parse::<u8>().map_err(|e| format!("{s}: {e}").into()))
        .collect()
}

pub fn tile_count(db: &Path) -> Res<u64> {
    Ok(sql(db, "SELECT count(*) FROM tiles;")?.trim().parse()?)
}

/// Write the metadata rows go-pmtiles reads.
///
/// go-pmtiles decides the archive's `tile_type` from the row named `format`, so
/// that row must read png/jpg/webp or the conversion produces an archive a
/// viewer cannot interpret.
pub fn set_metadata(db: &Path, rows: &[(&str, String)]) -> Res<()> {
    let values: Vec<String> = rows
        .iter()
        .map(|(k, v)| format!("('{}', '{}')", escape(k), escape(v)))
        .collect();
    sql(
        db,
        &format!(
            "INSERT OR REPLACE INTO metadata (name, value) VALUES {};",
            values.join(", ")
        ),
    )?;
    Ok(())
}

/// SQL string literals escape a quote by doubling it. The attribution notice
/// carries no quote today, and this is what keeps that from being load-bearing.
fn escape(s: &str) -> String {
    s.replace('\'', "''")
}

#[cfg(test)]
mod tests {
    use super::escape;

    #[test]
    fn a_quote_in_a_metadata_value_is_doubled() {
        assert_eq!(escape("d'Azur"), "d''Azur");
        assert_eq!(escape("plain"), "plain");
    }
}
