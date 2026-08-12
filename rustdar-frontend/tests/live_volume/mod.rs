//! The Level II volume the live instruments in this directory read, and the
//! radar they place from it.
//!
//! Three `#[ignore]`d instruments take a volume path from `VOL` and need the
//! position of the radar that collected it, because `build_voxels` expresses
//! its whole grid as kilometres from the site: `floor_alignment.rs`,
//! `volume_real_mask.rs` and `volume_march_cost.rs`. They carried a copy of
//! both steps each. An integration test is its own crate, so sharing them
//! means a module each file declares rather than an import — the same
//! arrangement `gpu_harness` is in, for the same reason.
//!
//! # The site comes out of the volume, not out of a list
//!
//! `rustdar-radar` carries no list of the network — see
//! [`SiteTable`](rustdar_radar::sites::SiteTable) — so a test binary that has
//! placed nothing knows no radars at all, and
//! [`get_radar_site`](rustdar_radar::sites::get_radar_site) answers `None` for
//! every identifier. Looking the site up by name therefore only ever worked
//! for the radars somebody had remembered to write into a fixture, and
//! `KDMX20250314_175512_V06` — the volume `floor_alignment.rs`'s standing
//! measurements were taken on — was not one of them.
//!
//! It never needed to be. Every Message 31 volume **states its own position**
//! in each radial's Volume Data Block, and an instrument that is about to
//! measure a volume is holding one. So [`scan_from_archive`] keeps the block
//! the decode reads and [`site_of`] resolves the process's site table from it,
//! which is the same step `App` takes with a volume it has just decoded. The
//! instruments work on any site's volume as a result, including a radar
//! commissioned after this checkout, and there is no list to maintain.
//!
//! A volume that states no position gets a panic that says *that* — see
//! [`site_of`] — rather than a `None` from a lookup three frames further in.

use nexrad_model::data::Scan;
use nexrad_model::meta::Site;

/// Decode a whole Level II archive file into a `Scan` carrying its own site.
///
/// **Not** `nexrad_data::volume::File::scan`, which is what
/// `rustdar-radar/examples/render_product.rs` uses: `nexrad-data` is
/// deliberately not a dependency of `rustdar-frontend` (its manifest says so in
/// as many words), so the only route from bytes to a `Scan` this crate's
/// dependency set offers is `rustdar_radar::chunks::decode_chunk` — which
/// dispatches on the `AR2` magic and walks exactly the same records — plus
/// `nexrad_model`'s own `Sweep::from_radials`. That pair is what `File::scan`
/// does internally, including the `Site` block: `ChunkContents::site` reads the
/// same four fields off the same Message 31, so the `Scan` handed back here
/// answers `site()` exactly as an archive-decoded one does.
pub fn scan_from_archive(path: &std::path::Path) -> Scan {
    let bytes =
        std::fs::read(path).unwrap_or_else(|e| panic!("reading VOL {}: {e}", path.display()));
    assert!(
        !bytes.starts_with(&[0x1f, 0x8b]),
        "{} is gzipped. Level II reaches this crate through nexrad-data's \
         bzip2-per-record framing and nothing in rustdar-frontend's dependency \
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
///
/// Resolves the process-wide site table as it goes, so anything else the
/// instrument calls afterwards — `eet::radar_height_ft_near`, the render paths'
/// MSL datum — finds the same row rather than an empty table. That is
/// `App::new`'s own step, taken with the one fix this process has.
///
/// `SITE` overrides the *identifier*, for a volume whose header holds something
/// other than the ICAO it is filed under. Nothing overrides the *position*: the
/// volume is the only thing in the room that measured it.
///
/// # Panics
///
/// With the reason, in the two cases where this cannot answer:
///
/// * the volume states no position at all — an `AR2V0001` archive is Message 1
///   throughout and carries no Volume Data Block — and
/// * the position it states is not a place a radar is, which
///   [`SitePosition::from_volume`](rustdar_radar::site_position::SitePosition::from_volume)
///   decides and which in practice means a zero-filled block.
///
/// Both say so in as many words, because "no radar is placed" is a real state
/// of this process and a lookup returning `None` several calls later is not a
/// description of it.
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
///
/// The header before the file name because it is the radar's own answer and
/// the file name is a convention; the file name is still there because a
/// re-encoded or hand-clipped volume can carry a blank identifier while its
/// name is intact.
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
