//! The Level II volume the live instruments in this directory read, and the
//! radar they place from it.
//!
//! The site comes out of the volume, not out of a list: `rustdar-radar` carries
//! no network table, so `get_radar_site` answers `None` for every identifier in
//! a test binary. Every Message 31 volume states its own position in each
//! radial's Volume Data Block, so [`scan_from_archive`] keeps that block and
//! [`site_of`] resolves the process's site table from it — the same step `App`
//! takes with a volume it has just decoded. A volume that states no position
//! panics saying so.

use nexrad_model::data::Scan;
use nexrad_model::meta::Site;

/// Decode a whole Level II archive file into a `Scan` carrying its own site.
pub fn scan_from_archive(path: &std::path::Path) -> Scan {
    let bytes =
        std::fs::read(path).unwrap_or_else(|e| panic!("reading VOL {}: {e}", path.display()));
    assert!(
        !bytes.starts_with(&[0x1f, 0x8b]),
        "{} is gzipped. Level II reaches this crate through nexrad-data's \
         bzip2-per-record framing and nothing in rustdar-app's dependency \
         set can gunzip a whole file; run `gunzip` on it first.",
        path.display(),
    );
    let name = path
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or("volume");
    let contents = rustdar_radar::chunks::decode_chunk(name, &bytes)
        .unwrap_or_else(|e| panic!("decoding {}: {e}", path.display()));
    let coverage_pattern = contents.coverage_pattern.unwrap_or_else(|| {
        panic!(
            "{} carries no message 5, so there is no tilt ladder and \
             VolumeSampler would refuse it",
            path.display(),
        )
    });
    let sweeps = nexrad_model::data::Sweep::from_radials(contents.radials);
    assert!(
        !sweeps.is_empty(),
        "{} decoded to no sweeps",
        path.display()
    );
    match contents.site {
        Some(site) => Scan::with_site(site, coverage_pattern, sweeps),
        None => Scan::new(coverage_pattern, sweeps),
    }
}

/// The radar's ICAO and position, learned from the volume in hand.
pub fn site_of(scan: &Scan, path: &std::path::Path) -> (String, f64, f64) {
    use rustdar_radar::site_position::SitePosition;
    use rustdar_radar::sites::SiteFix;

    let stated = scan.site().unwrap_or_else(|| {
        panic!(
            "{} states no position. Its radials carry no Volume Data Block, \
             which is what a pre-2010 AR2V0001 archive looks like — it is \
             Message 1 throughout — and nothing else in this process knows \
             where any radar is. Use a Message 31 volume.",
            path.display(),
        )
    });
    let position = SitePosition::from_volume(stated).unwrap_or_else(|| {
        panic!(
            "{} states ({}, {}), which is not a position a radar is at; its \
             Volume Data Block is zeroed or corrupt.",
            path.display(),
            stated.latitude(),
            stated.longitude(),
        )
    });

    let name = icao_of(stated, path);
    let row = rustdar_radar::sites::resolve([(name.as_str(), SiteFix::Learned(position))])
        .get(&name)
        .expect("the row the resolve above just placed");
    (name, row.lat, row.lon)
}

/// The identifier to file this volume's position under: `SITE`, else what the
/// radar wrote in its Message 31 headers, else the file name's first four
/// characters.
fn icao_of(stated: &Site, path: &std::path::Path) -> String {
    if let Ok(name) = std::env::var("SITE") {
        return name;
    }
    let identifier = stated.identifier_string();
    let identifier = identifier.trim();
    if identifier.len() == 4 && identifier.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return identifier.to_ascii_uppercase();
    }
    path.file_name()
        .and_then(std::ffi::OsStr::to_str)
        // `is_char_boundary`, not `len() >= 4`: the slice below is by bytes.
        .filter(|name| name.is_char_boundary(4))
        .map(|name| name[..4].to_ascii_uppercase())
        .unwrap_or_else(|| {
            panic!(
                "{} names no radar: its Volume Data Block holds {identifier:?} \
                 and its file name is no help either. Set SITE.",
                path.display(),
            )
        })
}
