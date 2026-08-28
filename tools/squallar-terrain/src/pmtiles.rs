//! The one assertion every emitted archive passes.

use std::io::Read;
use std::path::Path;

use crate::Res;

/// A PMTiles v3 archive begins with the 7-byte magic "PMTiles" then a version
/// byte.
///
/// Asserted because tippecanoe and tile-join choose their output container from
/// the FILE EXTENSION: `-o foo.pmtiles.part` writes SQLite, and a later rename
/// to `.pmtiles` leaves a file that is the wrong format, the right name, a
/// plausible size, and a zero exit status. Every temporary this build uses
/// therefore keeps the `.pmtiles` suffix, and this is what proves it did.
pub fn assert_archive(path: &Path) -> Res<()> {
    let mut f = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut magic = [0u8; 7];
    f.read_exact(&mut magic)
        .map_err(|_| format!("{} is empty or truncated", path.display()))?;
    if &magic != b"PMTiles" {
        return Err(format!(
            "{} is not a PMTiles archive (magic {:?}). tippecanoe and tile-join infer \
             the container from the output file extension — check that every -o \
             argument ends in .pmtiles.",
            path.display(),
            String::from_utf8_lossy(&magic)
        )
        .into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("squallar-terrain-test-{name}"));
        std::fs::File::create(&p).unwrap().write_all(bytes).unwrap();
        p
    }

    #[test]
    fn a_real_header_passes() {
        let p = temp("pm-ok", b"PMTiles\x03rest of the archive");
        assert_archive(&p).unwrap();
        std::fs::remove_file(p).ok();
    }

    /// The exact failure this exists for: SQLite wearing a `.pmtiles` name.
    #[test]
    fn an_mbtiles_wearing_a_pmtiles_name_is_caught() {
        let p = temp("pm-sqlite.pmtiles", b"SQLite format 3\0and so on");
        let err = assert_archive(&p).unwrap_err().to_string();
        assert!(err.contains("not a PMTiles archive"), "{err}");
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn an_empty_file_is_caught() {
        let p = temp("pm-empty", b"");
        assert!(assert_archive(&p).is_err());
        std::fs::remove_file(p).ok();
    }
}
