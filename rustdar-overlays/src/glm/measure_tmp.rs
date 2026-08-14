//! TEMPORARY measurement harness - NOT FOR COMMIT.
//!
//! Counts `missing` / `off_globe` record drops over a directory tree of real
//! GLM L2 LCFA granules, using exactly the predicates `parse_level_records`
//! applies. Run with:
//!
//! ```text
//! GLM_CASES=/path/to/glm_cases cargo test -p rustdar-overlays \
//!     measure_tmp -- --ignored --nocapture
//! ```

use super::fetch::{VarSource, normalize_longitude};
use super::h5::Granule;
use crate::glm::GlmDataLevel;

struct LevelSpec {
    name: &'static str,
    lat: &'static str,
    lon: &'static str,
    time_offset: &'static str,
    energy: &'static str,
}

const LEVELS: [LevelSpec; 3] = [
    LevelSpec {
        name: "flash",
        lat: "flash_lat",
        lon: "flash_lon",
        time_offset: "flash_time_offset_of_first_event",
        energy: "flash_energy",
    },
    LevelSpec {
        name: "group",
        lat: "group_lat",
        lon: "group_lon",
        time_offset: "group_time_offset",
        energy: "group_energy",
    },
    LevelSpec {
        name: "event",
        lat: "event_lat",
        lon: "event_lon",
        time_offset: "event_time_offset",
        energy: "event_energy",
    },
];

#[derive(Default, Clone, Copy)]
struct Counts {
    total: usize,
    missing: usize,
    off_globe: usize,
    levels: usize,
    levels_all_dropped: usize,
    levels_any_dropped: usize,
    /// NON-TRIVIALITY CONTROL 1. `None`s in the *energy* column, which the
    /// product fills with `_FillValue = -1s`. A non-zero count proves the
    /// `_FillValue` -> `None` machinery in `cf::unpack` fires on *this* data,
    /// so a zero on lat/lon/time is a property of the data and not a reader
    /// that never marks anything missing.
    fill_in_energy: usize,
    /// NON-TRIVIALITY CONTROL 2. The `off_globe` branch re-run against a
    /// deliberately impossible band (lat within +/-1 deg). A large count proves
    /// the drop branch is reachable and reported, so a zero on the real
    /// predicate is a measurement rather than dead code.
    off_narrow_band: usize,
}

impl Counts {
    fn add(&mut self, o: Counts) {
        self.total += o.total;
        self.missing += o.missing;
        self.off_globe += o.off_globe;
        self.levels += o.levels;
        self.levels_all_dropped += o.levels_all_dropped;
        self.levels_any_dropped += o.levels_any_dropped;
        self.fill_in_energy += o.fill_in_energy;
        self.off_narrow_band += o.off_narrow_band;
    }
}

fn measure_granule(bytes: &[u8]) -> Result<Vec<(String, Counts)>, String> {
    let g = Granule::open(bytes)?;
    let mut out = Vec::new();
    for spec in &LEVELS {
        let lats = match VarSource::read_unpacked(&g, spec.lat)? {
            Some(v) => v,
            None => continue,
        };
        let lons = VarSource::read_unpacked(&g, spec.lon)?.ok_or("no lon")?;
        let times = VarSource::read_unpacked(&g, spec.time_offset)?.ok_or("no time")?;
        // Read as the parser does: `*_energy` is a *required* column, so a level
        // without one fails outright and its records are a level failure, never
        // a record drop. Excluding it here keeps the denominator honest.
        let energies = VarSource::read_unpacked(&g, spec.energy)?.ok_or("no energy")?;
        let count = lats.values.len();
        // The parser's length check: a short column fails the level, so those
        // records never reach the drop predicates either.
        if lons.values.len() != count || times.values.len() != count {
            return Err(format!("{}: column length mismatch", spec.name));
        }
        let mut c = Counts {
            total: count,
            levels: 1,
            ..Counts::default()
        };
        for i in 0..count {
            if energies.values.get(i).copied().flatten().is_none() {
                c.fill_in_energy += 1;
            }
            let (Some(lat), Some(lon), Some(_off)) =
                (lats.values[i], lons.values[i], times.values[i])
            else {
                c.missing += 1;
                continue;
            };
            let lon = normalize_longitude(lon);
            if !(-1.0..=1.0).contains(&lat) {
                c.off_narrow_band += 1;
            }
            if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
                c.off_globe += 1;
            }
        }
        let dropped = c.missing + c.off_globe;
        if dropped > 0 {
            c.levels_any_dropped = 1;
        }
        if count > 0 && dropped == count {
            c.levels_all_dropped = 1;
        }
        out.push((spec.name.to_string(), c));
    }
    let _ = GlmDataLevel::Flash;
    Ok(out)
}

#[test]
#[ignore]
fn measure_record_drops_over_real_granules() {
    let root = std::env::var("GLM_CASES").expect("set GLM_CASES");
    let mut sites: Vec<_> = std::fs::read_dir(&root)
        .expect("read root")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.path())
        .collect();
    sites.sort();

    let mut grand = Counts::default();
    let mut grand_files = 0usize;
    println!(
        "site,granules,level,records,missing,off_globe,levels,levels_any_dropped,\
         CTL_fill_in_energy,CTL_off_narrow_band"
    );
    for site in &sites {
        let mut files: Vec<_> = std::fs::read_dir(site)
            .expect("read site")
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "nc"))
            .collect();
        files.sort();
        let mut per_level: std::collections::BTreeMap<String, Counts> = Default::default();
        for f in &files {
            let bytes = std::fs::read(f).expect("read granule");
            match measure_granule(&bytes) {
                Ok(levels) => {
                    for (name, c) in levels {
                        per_level.entry(name).or_default().add(c);
                    }
                }
                Err(e) => println!("# PARSE FAIL {}: {e}", f.display()),
            }
        }
        grand_files += files.len();
        for (name, c) in &per_level {
            grand.add(*c);
            println!(
                "{},{},{},{},{},{},{},{},{},{}",
                site.file_name().unwrap().to_string_lossy(),
                files.len(),
                name,
                c.total,
                c.missing,
                c.off_globe,
                c.levels,
                c.levels_any_dropped,
                c.fill_in_energy,
                c.off_narrow_band,
            );
        }
    }
    println!(
        "# TOTAL granules={grand_files} records={} missing={} off_globe={} \
         levels={} levels_any_dropped={} levels_all_dropped={}",
        grand.total, grand.missing, grand.off_globe, grand.levels, grand.levels_any_dropped,
        grand.levels_all_dropped,
    );
    println!(
        "# CONTROLS fill_in_energy={} off_narrow_band={} (both must be > 0, or the \
         zero above is vacuous)",
        grand.fill_in_energy, grand.off_narrow_band,
    );
    assert!(
        grand.fill_in_energy > 0,
        "CONTROL 1 FAILED: no _FillValue reached `values` as None anywhere in the \
         corpus, so a zero on lat/lon/time proves nothing about the data",
    );
    assert!(
        grand.off_narrow_band > 0,
        "CONTROL 2 FAILED: the coordinate-drop branch never fired even against an \
         impossible band, so it is dead code and the zero above is vacuous",
    );
}
