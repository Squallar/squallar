//! TEMPORARY measurement harness - NOT FOR COMMIT ON `main`.
//!
//! Measures the record drop rate over a directory tree of real GLM L2 LCFA
//! granules. Run with:
//!
//! ```text
//! GLM_CASES=/path/to/glm_cases cargo test -p rustdar-overlays \
//!     measure_tmp -- --ignored --nocapture
//! ```
//!
//! # Two independent readings of the same quantity
//!
//! The drop counts come from **the shipping parser**, via `parse_glm_netcdf`
//! and the `RecordDrops` it now returns: the question is what rustdar throws
//! away, so a reimplementation would answer a different one and could agree
//! with the parser by sharing its bug.
//!
//! The **controls** come from reading the columns directly, and exist because a
//! zero is only evidence if the counter could have been non-zero:
//!
//! * `off_narrow_band` - the coordinate predicate re-run against an impossible
//!   band. **Asserted**: it fires on every record in the corpus, so if it ever
//!   came back zero the loop is not running and the headline zero is vacuous.
//!
//! * `fill_marked` - `None`s reaching `values` from `_FillValue` on the energy
//!   and area columns. **Reported, deliberately not asserted.** It was an assert
//!   and it was wrong: measured over the corpus, `_FillValue` fires 3 times in
//!   1584507 records (all in one case) and **0 times** in the 76003-record
//!   holdout, because real GLM LCFA data essentially never marks a value
//!   missing. Asserting it turns legitimately clean data into a failed run -
//!   a gate that cannot pass, which is the same defect as one that cannot fail.
//!   What proves the fill branch is live is
//!   `a_dropped_record_reaches_the_caller_and_says_which_kind`, which declares
//!   a `_FillValue` on a synthetic granule and pins which bucket the drop lands
//!   in. A corpus cannot be relied on to contain a condition it is not obliged
//!   to have; a fixture can.
//!
//! `considered` is cross-checked against the direct read, so the two paths must
//! also agree about the denominator.

use super::fetch::{VarSource, normalize_longitude, parse_glm_netcdf};
use super::h5::Granule;
use crate::glm::{GlmDataLevel, GlmSatellite};

const LEVELS: [GlmDataLevel; 3] = [
    GlmDataLevel::Flash,
    GlmDataLevel::Group,
    GlmDataLevel::Event,
];

/// Direct-read column names per level, for the controls only: the two that
/// place a record, and the *descriptive* columns whose `_FillValue = -1s` is
/// the corpus's own demonstration that `cf::unpack` marks anything missing.
///
/// Both `_energy` and `_area` are watched, not just energy: measured over the
/// primary corpus energy alone fills 3 times in 1584507 records, which is
/// non-zero but far too thin to rest a control on - and on the held-out case it
/// is zero, which failed the assert and is exactly what a control is for.
/// Events have no `_area`, hence the `Option`.
const CONTROL_VARS: [(&str, &str, &str, Option<&str>); 3] = [
    ("flash_lat", "flash_lon", "flash_energy", Some("flash_area")),
    ("group_lat", "group_lon", "group_energy", Some("group_area")),
    ("event_lat", "event_lon", "event_energy", None),
];

#[derive(Default, Clone, Copy)]
struct Counts {
    granules: usize,
    /// From the shipping parser.
    considered: usize,
    fill_values: usize,
    off_globe: usize,
    level_failures: usize,
    /// From the direct read.
    ctl_considered: usize,
    ctl_fill_marked: usize,
    ctl_off_narrow_band: usize,
}

impl Counts {
    fn add(&mut self, o: Counts) {
        self.granules += o.granules;
        self.considered += o.considered;
        self.fill_values += o.fill_values;
        self.off_globe += o.off_globe;
        self.level_failures += o.level_failures;
        self.ctl_considered += o.ctl_considered;
        self.ctl_fill_marked += o.ctl_fill_marked;
        self.ctl_off_narrow_band += o.ctl_off_narrow_band;
    }
}

fn measure_granule(bytes: &[u8]) -> Result<Counts, String> {
    let parsed = parse_glm_netcdf(bytes, GlmSatellite::GoesEast, &LEVELS)?;
    let mut c = Counts {
        granules: 1,
        considered: parsed.drops.considered,
        fill_values: parsed.drops.fill_values,
        off_globe: parsed.drops.off_globe,
        level_failures: parsed.level_failures.len(),
        ..Counts::default()
    };

    // Controls, read straight off the file rather than through the parser.
    let g = Granule::open(bytes)?;
    for (lat_name, lon_name, energy_name, area_name) in CONTROL_VARS {
        let Some(lats) = VarSource::read_unpacked(&g, lat_name)? else {
            continue;
        };
        let Some(lons) = VarSource::read_unpacked(&g, lon_name)? else {
            continue;
        };
        let Some(energies) = VarSource::read_unpacked(&g, energy_name)? else {
            continue;
        };
        // Only levels the parser actually examined belong in the cross-check.
        if lons.values.len() != lats.values.len() {
            continue;
        }
        let areas = match area_name {
            Some(n) => VarSource::read_unpacked(&g, n)?,
            None => None,
        };
        c.ctl_considered += lats.values.len();
        for i in 0..lats.values.len() {
            if energies.values.get(i).copied().flatten().is_none() {
                c.ctl_fill_marked += 1;
            }
            if areas
                .as_ref()
                .is_some_and(|a| a.values.get(i).copied().flatten().is_none())
            {
                c.ctl_fill_marked += 1;
            }
            let (Some(lat), Some(lon)) = (lats.values[i], lons.values[i]) else {
                continue;
            };
            let _ = normalize_longitude(lon);
            if !(-1.0..=1.0).contains(&lat) {
                c.ctl_off_narrow_band += 1;
            }
        }
    }
    Ok(c)
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
    println!(
        "site,granules,records,fill_values,off_globe,level_failures,\
         CTL_records,CTL_fill_marked,CTL_off_narrow_band"
    );
    for site in &sites {
        let mut files: Vec<_> = std::fs::read_dir(site)
            .expect("read site")
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "nc"))
            .collect();
        files.sort();
        let mut per_site = Counts::default();
        for f in &files {
            let bytes = std::fs::read(f).expect("read granule");
            match measure_granule(&bytes) {
                Ok(c) => per_site.add(c),
                Err(e) => println!("# PARSE FAIL {}: {e}", f.display()),
            }
        }
        grand.add(per_site);
        println!(
            "{},{},{},{},{},{},{},{},{}",
            site.file_name().unwrap().to_string_lossy(),
            per_site.granules,
            per_site.considered,
            per_site.fill_values,
            per_site.off_globe,
            per_site.level_failures,
            per_site.ctl_considered,
            per_site.ctl_fill_marked,
            per_site.ctl_off_narrow_band,
        );
    }

    let dropped = grand.fill_values + grand.off_globe;
    println!(
        "# TOTAL granules={} records={} fill_values={} off_globe={} dropped={} \
         rate={:.3e} level_failures={}",
        grand.granules,
        grand.considered,
        grand.fill_values,
        grand.off_globe,
        dropped,
        dropped as f64 / grand.considered.max(1) as f64,
        grand.level_failures,
    );
    println!(
        "# CONTROLS records={} fill_marked={} off_narrow_band={}",
        grand.ctl_considered, grand.ctl_fill_marked, grand.ctl_off_narrow_band,
    );

    assert!(grand.granules > 0, "GLM_CASES held no granules");
    assert_eq!(
        grand.considered, grand.ctl_considered,
        "the shipping parser and the direct read disagree about how many records \
         exist, so they are not measuring the same thing",
    );
    if grand.ctl_fill_marked == 0 {
        println!(
            "# NOTE fill_marked=0: this corpus never marks a value missing, so it \
             cannot vouch for the fill-value branch. See the module header - the \
             synthetic fixture in `fetch::tests` is what pins that branch."
        );
    }
    assert!(
        grand.ctl_off_narrow_band > 0,
        "CONTROL FAILED: the coordinate predicate never fired even against an \
         impossible band, so the drop loop is not running and the zero above is \
         vacuous",
    );
}
