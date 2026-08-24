//! Offline NWS zone-geometry pipeline and, mainly, a measuring instrument.
//!
//! Reads the six AWIPS shapefile datasets the NWS publishes at
//! `weather.gov/source/gis/Shapefiles/`, keys every zone by `(kind, UGC)`,
//! collects the several features one zone is spread across into one
//! multi-polygon, simplifies with the *app's own* `simplify_ring`, and writes
//! one indexed pack. Then it measures the result at a ladder of tolerances and
//! encodings and prints the table that is the actual deliverable.
//!
//! It never touches `api.weather.gov`, and it never touches the network at all:
//! the zips are an input on disk.
//!
//! Usage:
//!   nws-zone-pack <unpacked-shapefile-dir> [<output.pack>]
//!
//! `<unpacked-shapefile-dir>` holds one subdirectory per dataset, each with the
//! `.shp`/`.shx`/`.dbf` triple as the zip ships them.
//!
//! # Where the output goes
//!
//! `<output.pack>` is what the app consumes, under the one name
//! `squallar_overlays::nws::zone_pack::PACK_FILE_NAME` declares:
//!
//! - **Web**: next to `index.html` in the deploy. `sw.js` routes it by that
//!   name into a cache of its own, outside the all-or-nothing shell install.
//! - **Native, iOS, Android**: beside the zone cache directory —
//!   `~/.cache/squallar/zones.pack` for a cache at `~/.cache/squallar/zones`.
//!
//! Nowhere is also fine. A pack that is absent, stale or unreadable is a
//! supported state: `zone_pack::installed()` answers `None` and every zone
//! resolves over HTTP exactly as it did before the pack existed. The only thing
//! at stake is the request count.

mod area;
mod dbf;
mod pack;
mod rings;
mod shp;

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use pack::{Coding, Kind};
use squallar_geo::GeoPolygon;
use squallar_overlays::render::geo::simplify_ring;
use squallar_overlays::types::SIMPLIFY_EPSILON;

/// The corpus at one fidelity: every zone, keyed and sorted, ready to encode.
type Zones = Vec<pack::PackedZone>;

/// How a dataset's UGC is spelled. None of the six carries a ready-made
/// `(kind, ugc)`; three carry the pieces and three carry an id column, one of
/// which is empty in every row.
enum Ugc {
    /// `STATE` + `Z` + `ZONE` — public forecast and fire weather both.
    StateZone,
    /// `STATE` + `C` + the last three of `FIPS`. The counties file has no zone
    /// column at all.
    StateFips,
    /// A named column that already holds the UGC.
    Column(&'static str),
}

struct Dataset {
    dir: &'static str,
    kind: Kind,
    ugc: Ugc,
    /// The count published on weather.gov, checked against both the `.shx`
    /// index and the `.shp` record loop.
    published_records: usize,
}

const DATASETS: &[Dataset] = &[
    Dataset {
        dir: "z_16ap26",
        kind: Kind::Forecast,
        ugc: Ugc::StateZone,
        published_records: 4157,
    },
    Dataset {
        dir: "fz16ap26",
        kind: Kind::Fire,
        ugc: Ugc::StateZone,
        published_records: 3683,
    },
    Dataset {
        dir: "c_16ap26",
        kind: Kind::County,
        ugc: Ugc::StateFips,
        published_records: 3352,
    },
    Dataset {
        dir: "mz16ap26",
        kind: Kind::Forecast,
        ugc: Ugc::Column("ID"),
        published_records: 569,
    },
    Dataset {
        dir: "oz16ap26",
        kind: Kind::Forecast,
        ugc: Ugc::Column("ID"),
        published_records: 130,
    },
    Dataset {
        dir: "hz17fe26",
        kind: Kind::Forecast,
        ugc: Ugc::Column("id"),
        published_records: 5,
    },
];

/// The tolerance ladder. `SIMPLIFY_EPSILON` is the app's own and is not written
/// as a literal here; the rest are multiples of it, so the table's rows stay
/// comparable if the app ever moves its own value.
const EPSILONS: &[f64] = &[
    SIMPLIFY_EPSILON,
    SIMPLIFY_EPSILON * 2.0,
    SIMPLIFY_EPSILON * 4.0,
    SIMPLIFY_EPSILON * 10.0,
    SIMPLIFY_EPSILON * 20.0,
];

fn main() {
    // RDP is recursive and a county ring can run tens of thousands of points,
    // so the work happens on a thread with room for it rather than on main's
    // 8 MiB.
    let args: Vec<String> = std::env::args().collect();
    let handle = std::thread::Builder::new()
        .stack_size(512 * 1024 * 1024)
        .spawn(move || run(&args))
        .expect("spawn worker");
    if handle.join().is_err() {
        std::process::exit(1);
    }
}

fn run(args: &[String]) {
    let Some(root) = args.get(1) else {
        eprintln!("usage: nws-zone-pack <unpacked-shapefile-dir> [<output.pack>]");
        std::process::exit(2);
    };
    let root = PathBuf::from(root);

    // The independent-oracle mode. `--wkt <dataset> <fid> <dir>` prints one
    // record's rings in the same WKT that `ogrinfo -al -geom=YES -fid N`
    // prints, at full precision, so the two can be diffed. GDAL's shapefile
    // reader is a separate implementation, in another language, sharing no code
    // and no author with this one; agreeing with it is the only control here
    // that could catch a wrong f64 rather than a wrong count.
    if root.to_str() == Some("--wkt") {
        let dir = args
            .get(2)
            .map(String::as_str)
            .unwrap_or_else(|| die("--wkt <dataset> <fid> <dir>"));
        let fid: usize = args
            .get(3)
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| die("--wkt <dataset> <fid> <dir>"));
        let base = PathBuf::from(args.get(4).map(String::as_str).unwrap_or("."))
            .join(dir)
            .join(dir);
        let records = shp::read_polygons(&read_file(&base.with_extension("shp")))
            .unwrap_or_else(|e| die(&format!("{dir}: {e}")));
        let rec = records.get(fid).unwrap_or_else(|| die("no such fid"));
        let parts: Vec<String> = rec
            .rings
            .iter()
            .map(|r| {
                let pts: Vec<String> = r.iter().map(|&(x, y)| format!("{x} {y}")).collect();
                format!("({})", pts.join(","))
            })
            .collect();
        println!("POLYGON ({})", parts.join(","));
        return;
    }

    // `--compare-cache <cache-dir> <pack>`: the pack against the app's real
    // on-disk zone cache, zone for zone. The cache was fetched from
    // `api.weather.gov` and simplified by the same `simplify_ring`, so a zone
    // present in both is the same shape rendered from two different origins —
    // which is the only way to find out whether swapping the origin changes
    // what the map draws. Reads the cache off disk; fetches nothing.
    if root.to_str() == Some("--compare-cache") {
        compare_cache(
            Path::new(
                args.get(2)
                    .map(String::as_str)
                    .unwrap_or_else(|| die("--compare-cache <cache-dir> <pack>")),
            ),
            Path::new(
                args.get(3)
                    .map(String::as_str)
                    .unwrap_or_else(|| die("--compare-cache <cache-dir> <pack>")),
            ),
        );
        return;
    }

    // `--wkt-pairs <cache-dir> <pack> <every-nth>`: the same zone pairs
    // `--compare-cache` measures, emitted as WKT next to this program's own
    // area figures, one tab-separated row per zone. It is the control for the
    // area sweep: GDAL/GEOS can recompute every row's intersection and union
    // from the identical geometry, and a sweep that agrees with an unrelated
    // implementation on thousands of real coastlines is not merely
    // self-consistent. Reads the cache off disk; fetches nothing.
    if root.to_str() == Some("--wkt-pairs") {
        wkt_pairs(
            Path::new(
                args.get(2)
                    .map(String::as_str)
                    .unwrap_or_else(|| die("--wkt-pairs <cache-dir> <pack> <every-nth>")),
            ),
            Path::new(
                args.get(3)
                    .map(String::as_str)
                    .unwrap_or_else(|| die("--wkt-pairs <cache-dir> <pack> <every-nth>")),
            ),
            args.get(4).and_then(|s| s.parse().ok()).unwrap_or(1),
        );
        return;
    }

    let out_path = args.get(2).map(PathBuf::from);

    // ── stage 1: parse ───────────────────────────────────────────────────
    println!("== parse ==");
    let mut raw: BTreeMap<[u8; 7], Vec<GeoPolygon>> = BTreeMap::new();
    let mut features_per_key: BTreeMap<[u8; 7], usize> = BTreeMap::new();
    let mut ring_stats = rings::RingStats::default();
    let mut bad_ugc: Vec<(String, String)> = Vec::new();
    let mut null_shapes = 0usize;
    let mut total_raw_vertices = 0usize;
    let mut source_of: BTreeMap<[u8; 7], &'static str> = BTreeMap::new();

    for ds in DATASETS {
        let base = root.join(ds.dir).join(ds.dir);
        let shp_bytes = read_file(&base.with_extension("shp"));
        let shx_bytes = read_file(&base.with_extension("shx"));
        let dbf_bytes = read_file(&base.with_extension("dbf"));

        let shx_count = shp::shx_record_count(&shx_bytes).unwrap_or_else(|| {
            die(&format!(
                "{}: .shx is not a whole number of entries",
                ds.dir
            ))
        });
        let records =
            shp::read_polygons(&shp_bytes).unwrap_or_else(|e| die(&format!("{}: {e}", ds.dir)));
        let attrs = dbf::read(&dbf_bytes).unwrap_or_else(|e| die(&format!("{}.dbf: {e}", ds.dir)));

        // Three independent counts of the same quantity. They agreeing is the
        // control that the record loop did not stop early or run on.
        assert_eq!(
            (shx_count, records.len(), attrs.records.len()),
            (
                ds.published_records,
                ds.published_records,
                ds.published_records
            ),
            "{}: .shx says {shx_count}, the .shp loop says {}, the .dbf says {}, \
             weather.gov publishes {}",
            ds.dir,
            records.len(),
            attrs.records.len(),
            ds.published_records,
        );

        // And an exact algebraic control on the geometry itself. Every Polygon
        // record is 8 bytes of record header, 44 of type and bounding box, 8
        // of the two counts, 4 per part and 16 per vertex, so the file's own
        // length pins the totals this parser extracted. Nothing here can be
        // silently short.
        let parts: usize = records.iter().map(|r| r.rings.len()).sum();
        let verts: usize = records
            .iter()
            .map(|r| r.rings.iter().map(Vec::len).sum::<usize>())
            .sum();
        let predicted = 100 + 52 * records.len() + 4 * parts + 16 * verts;
        assert_eq!(
            predicted,
            shp_bytes.len(),
            "{}: {} records / {parts} rings / {verts} vertices imply a {predicted}-byte \
             .shp, but the file is {} bytes",
            ds.dir,
            records.len(),
            shp_bytes.len(),
        );
        total_raw_vertices += verts;

        let mut unioned_here = 0usize;
        for (i, rec) in records.iter().enumerate() {
            if rec.rings.is_empty() {
                null_shapes += 1;
                continue;
            }
            let row = &attrs.records[i];
            let Some(ugc) = ugc_for(ds, row) else {
                bad_ugc.push((ds.dir.to_string(), describe_row(row)));
                continue;
            };
            if !well_formed(&ugc) {
                bad_ugc.push((ds.dir.to_string(), ugc.clone()));
                continue;
            }
            let Some(k) = pack::key(ds.kind, &ugc) else {
                bad_ugc.push((ds.dir.to_string(), ugc.clone()));
                continue;
            };
            let polys = rings::to_polygons(&rec.rings, &mut ring_stats);
            let entry = raw.entry(k).or_default();
            if !entry.is_empty() {
                unioned_here += 1;
            }
            entry.extend(polys);
            *features_per_key.entry(k).or_insert(0) += 1;
            source_of.entry(k).or_insert(ds.dir);
        }
        println!(
            "  {:<10} {:>5} records  {:>7} rings  {:>9} vertices  {:>4} extra features merged",
            ds.dir,
            records.len(),
            parts,
            verts,
            unioned_here,
        );
        // The columns, printed because the UGC spelling differs per file and
        // this is the evidence for which rule each one got.
        println!("             columns: {}", attrs.fields.join(" "));
    }

    println!(
        "\n  {} zone keys from {} records; {} raw vertices",
        raw.len(),
        DATASETS.iter().map(|d| d.published_records).sum::<usize>(),
        total_raw_vertices,
    );
    for kind in [Kind::Forecast, Kind::County, Kind::Fire] {
        let n = raw.keys().filter(|k| k[0] == kind.byte()).count();
        println!("    {:<9} {n}", kind.label());
    }

    // ── stage 2: anomalies ───────────────────────────────────────────────
    println!("\n== anomalies ==");
    let multi: Vec<([u8; 7], usize)> = features_per_key
        .iter()
        .filter(|(_, n)| **n > 1)
        .map(|(k, &n)| (*k, n))
        .collect();
    println!(
        "  zones spread over more than one shapefile feature: {} (max {} features)",
        multi.len(),
        multi.iter().map(|(_, n)| *n).max().unwrap_or(0),
    );
    let mut worst: Vec<(usize, String)> = multi.iter().map(|(k, n)| (*n, key_str(k))).collect();
    worst.sort_unstable_by_key(|(n, _)| std::cmp::Reverse(*n));
    for (n, k) in worst.iter().take(6) {
        println!("    {k} : {n} features");
    }
    println!("  null-geometry records: {null_shapes}");
    println!(
        "  rings under 4 points (cannot bound anything): {}",
        ring_stats.degenerate_rings
    );
    println!(
        "  records whose every ring wound as a hole: {}",
        ring_stats.all_holes_records
    );
    println!(
        "  holes contained by no exterior ring: {}",
        ring_stats.orphan_holes
    );
    println!("  rows with no usable UGC: {}", bad_ugc.len());
    let mut by_ds: BTreeMap<&str, usize> = BTreeMap::new();
    for (d, _) in &bad_ugc {
        *by_ds.entry(d.as_str()).or_insert(0) += 1;
    }
    for (d, n) in &by_ds {
        println!(
            "    {d}: {n}  e.g. {:?}",
            bad_ugc.iter().find(|(x, _)| x == d).map(|(_, v)| v)
        );
    }
    let malformed = raw.keys().filter(|k| !well_formed(&key_ugc(k))).count();
    println!(
        "  keys not matching ^[A-Z]{{2}}[CZ][0-9]{{3}}$: {malformed} (of {})",
        raw.len(),
    );

    // ── stage 3: the ladder ──────────────────────────────────────────────
    let raw_zones: Zones = raw.iter().map(|(k, v)| (*k, v.clone())).collect();
    let raw_v = count_vertices(&raw_zones);
    let raw_area = total_area(&raw_zones);
    println!("\n== fidelity ladder ==");
    println!(
        "  {:<9} {:>5} {:>11} {:>7} {:>7} {:>9} {:>7}",
        "epsilon", "~m", "vertices", "% left", "rings", "area kept", "empty",
    );
    println!(
        "  {:<9} {:>5} {:>11} {:>7} {:>7} {:>9} {:>7}",
        "none",
        "-",
        raw_v,
        "100.0%",
        count_rings(&raw_zones),
        "100.0000%",
        0,
    );

    let mut rungs: Vec<(f64, Zones)> = Vec::new();
    for &eps in EPSILONS {
        let zones = simplify_all(&raw_zones, eps);
        let v = count_vertices(&zones);
        let empty = zones.iter().filter(|(_, p)| p.is_empty()).count();
        println!(
            "  {:<9.4} {:>5.0} {:>11} {:>6.2}% {:>7} {:>8.4}% {:>7}",
            eps,
            eps * 111_194.9,
            v,
            100.0 * v as f64 / raw_v as f64,
            count_rings(&zones),
            100.0 * total_area(&zones) / raw_area,
            empty,
        );
        rungs.push((eps, zones));
    }

    // Non-vacuity: the ladder must actually be a ladder. A pipeline that
    // emitted nothing would print a beautifully small table.
    assert!(
        raw_v > 1_000_000,
        "premise: the raw corpus is millions of vertices, not {raw_v}"
    );
    let mut previous = raw_v;
    for (eps, zones) in &rungs {
        let v = count_vertices(zones);
        assert!(
            v < previous,
            "epsilon {eps} did not remove a single vertex ({v} vs {previous}); the \
             simplification control is vacuous",
        );
        assert!(v > 0, "epsilon {eps} deleted the entire corpus");
        previous = v;
    }

    // ── stage 4: sizes ───────────────────────────────────────────────────
    println!("\n== size table ==   (gzip level 6; index is header + 11 B/zone + sentinel)");
    println!(
        "  {:<9} {:<8} {:>12} {:>12} {:>9} {:>9}",
        "epsilon", "coding", "bytes", "gzipped", "index B", "index gz",
    );
    let variants: &[(Coding, u16)] = &[
        (Coding::F64, 0),
        (Coding::F32, 0),
        (Coding::Varint, 5),
        (Coding::Varint, 6),
    ];
    let mut best: Option<(f64, Coding, u16, usize)> = None;
    for (label, eps, zones) in std::iter::once(("none".to_string(), f64::NAN, &raw_zones))
        .chain(rungs.iter().map(|(e, z)| (format!("{e:.4}"), *e, z)))
    {
        for &(coding, qexp) in variants {
            let bytes = pack::write(zones, coding, qexp, if eps.is_nan() { 0.0 } else { eps });
            let gz = gzip(&bytes);
            let index_len = pack::HEADER_LEN + zones.len() * pack::INDEX_ENTRY_LEN + 4;
            let index_gz = gzip(&bytes[..index_len]).len();
            let name = if coding == Coding::Varint {
                format!("{}1e-{qexp}", coding.label())
            } else {
                coding.label().to_string()
            };
            println!(
                "  {:<9} {:<8} {:>12} {:>12} {:>9} {:>9}",
                label,
                name,
                bytes.len(),
                gz.len(),
                index_len,
                index_gz,
            );

            // ── the round-trip control, on every single cell of the table ──
            let p = pack::ZonePack::open(bytes.clone()).unwrap_or_else(|why| {
                die(&format!(
                    "{label}/{name}: the writer produced a pack the reader rejects ({why})"
                ))
            });
            assert_eq!(
                p.zone_count(),
                zones.len(),
                "{label}/{name}: zone count changed"
            );
            let tol = match coding {
                Coding::F64 => 0.0,
                // f32 has ~7 significant decimal digits; a longitude near 180
                // keeps about 5 decimal places, so ~1e-4 deg is the honest
                // ceiling and anything beyond it is a bug, not rounding.
                Coding::F32 => 2e-4,
                Coding::Varint => 1.0 / 10f64.powi(i32::from(qexp)),
            };
            let mut worst_dev = 0.0f64;
            let mut checked = 0usize;
            for (i, (k, want)) in zones.iter().enumerate() {
                assert_eq!(
                    p.key_at(i).as_ref(),
                    Some(k),
                    "{label}/{name}: key {i} moved"
                );
                // Through the binary search, not through `at(i)`: the index is
                // what a lookup goes via, and a mis-sorted index would still
                // answer correctly by position.
                let kind = Kind::from_byte(k[0])
                    .unwrap_or_else(|| die(&format!("{label}/{name}: key {i} has no kind")));
                let got = p.get(kind, &key_ugc(k)).unwrap_or_else(|| {
                    die(&format!(
                        "{label}/{name}: {} not found by binary search",
                        key_str(k)
                    ))
                });
                assert_eq!(
                    got.len(),
                    want.len(),
                    "{label}/{name}: {} polygon count",
                    key_str(k)
                );
                for (gp, wp) in got.iter().zip(want) {
                    assert_eq!(
                        gp.len(),
                        wp.len(),
                        "{label}/{name}: {} ring count",
                        key_str(k)
                    );
                    for (gr, wr) in gp.iter().zip(wp) {
                        assert_eq!(
                            gr.len(),
                            wr.len(),
                            "{label}/{name}: {} point count",
                            key_str(k)
                        );
                        for (&(ga, go), &(wa, wo)) in gr.iter().zip(wr) {
                            worst_dev = worst_dev.max((ga - wa).abs()).max((go - wo).abs());
                            checked += 1;
                        }
                    }
                }
            }
            assert!(
                checked == count_vertices(zones) && checked > 0,
                "{label}/{name}: the round-trip compared {checked} vertices, not {}",
                count_vertices(zones),
            );
            assert!(
                worst_dev <= tol,
                "{label}/{name}: round-trip moved a vertex by {worst_dev} deg, over the \
                 {tol} deg this coding may cost",
            );
            if coding == Coding::Varint && qexp == 5 && eps == SIMPLIFY_EPSILON {
                best = Some((eps, coding, qexp, gz.len()));
            }
            if !eps.is_nan() && (eps - SIMPLIFY_EPSILON).abs() < 1e-12 && coding == Coding::F64 {
                println!(
                    "      round-trip: {checked} vertices compared, worst deviation {worst_dev:e} deg",
                );
            }
        }
    }

    // ── stage 4b: the public-forecast-only subset ────────────────────────
    //
    // It gets its own rows because the published MazamaSpatialUtils ladder
    // (23 MB full; 1.7 MB, 902 KB, 614 KB at 5%, 2%, 1% of vertices) is for the
    // **public forecast zones alone** — one of the six files here. Reading it
    // against the all-six table above would be comparing 4,157 records with
    // 11,896, which is exactly the kind of silently-mixed denominator that
    // makes a size claim useless.
    println!("\n== public forecast zones only (z_16ap26), for the Mazama comparison ==");
    println!(
        "  {:<9} {:>9} {:>7} {:>12} {:>12}",
        "epsilon", "vertices", "% left", "varint1e-5", "gzipped",
    );
    let z_only = |zones: &[([u8; 7], Vec<GeoPolygon>)]| -> Zones {
        zones
            .iter()
            .filter(|(k, _)| source_of.get(k) == Some(&"z_16ap26"))
            .cloned()
            .collect()
    };
    let z_raw = z_only(&raw_zones);
    let z_raw_v = count_vertices(&z_raw);
    let z_raw_bytes = pack::write(&z_raw, Coding::Varint, 5, 0.0);
    println!(
        "  {:<9} {:>9} {:>7} {:>12} {:>12}",
        "none",
        z_raw_v,
        "100.0%",
        z_raw_bytes.len(),
        gzip(&z_raw_bytes).len(),
    );
    for (eps, zones) in &rungs {
        let z = z_only(zones);
        let v = count_vertices(&z);
        let b = pack::write(&z, Coding::Varint, 5, *eps);
        println!(
            "  {:<9.4} {:>9} {:>6.2}% {:>12} {:>12}",
            eps,
            v,
            100.0 * v as f64 / z_raw_v as f64,
            b.len(),
            gzip(&b).len(),
        );
    }
    println!("  ({} of the {} zone keys)", z_raw.len(), raw_zones.len());

    // ── stage 5: write the recommended pack ──────────────────────────────
    if let (Some(path), Some((eps, coding, qexp, _))) = (out_path, best) {
        let zones = &rungs
            .iter()
            .find(|(e, _)| (*e - eps).abs() < 1e-12)
            .expect("the recommended rung was measured")
            .1;
        let bytes = pack::write(zones, coding, qexp, eps);
        std::fs::write(&path, &bytes).unwrap_or_else(|e| die(&format!("write {path:?}: {e}")));
        println!(
            "\nwrote {} ({} bytes, {} gzipped) at epsilon {eps}, {}1e-{qexp}",
            path.display(),
            bytes.len(),
            gzip(&bytes).len(),
            coding.label(),
        );
    }
}

// ── the cache comparison ─────────────────────────────────────────────────

fn bbox(polys: &[GeoPolygon]) -> Option<(f64, f64, f64, f64)> {
    let (mut s, mut w, mut n, mut e) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    let mut any = false;
    for &(lat, lon) in polys.iter().flatten().flatten() {
        s = s.min(lat);
        n = n.max(lat);
        w = w.min(lon);
        e = e.max(lon);
        any = true;
    }
    any.then_some((s, w, n, e))
}

/// Intersection over union of two boxes. Zero when they do not overlap at all,
/// one when they are identical.
fn bbox_iou(a: (f64, f64, f64, f64), b: (f64, f64, f64, f64)) -> f64 {
    let s = a.0.max(b.0);
    let w = a.1.max(b.1);
    let n = a.2.min(b.2);
    let e = a.3.min(b.3);
    if n <= s || e <= w {
        return 0.0;
    }
    let inter = (n - s) * (e - w);
    let area = |x: (f64, f64, f64, f64)| ((x.2 - x.0) * (x.3 - x.1)).max(0.0);
    let union = area(a) + area(b) - inter;
    if union <= 0.0 { 0.0 } else { inter / union }
}

fn compare_cache(cache_dir: &Path, pack_path: &Path) {
    let bytes = read_file(pack_path);
    let p = pack::ZonePack::open(bytes).unwrap_or_else(|why| die(&format!("pack: {why}")));
    println!("== pack vs the app's on-disk zone cache ==");
    println!(
        "  pack: {} zones, coding {}, built at epsilon {} ({}the app's own)",
        p.zone_count(),
        p.coding().label(),
        p.epsilon(),
        if (p.epsilon() - SIMPLIFY_EPSILON).abs() < 1e-12 {
            ""
        } else {
            "NOT "
        },
    );

    let Matched {
        pairs,
        seen,
        unreadable,
        mut missing,
        per_kind,
    } = read_pairs(cache_dir, &p);

    let cache_v: usize = pairs
        .iter()
        .map(|q| q.cache.iter().flatten().map(Vec::len).sum::<usize>())
        .sum();
    let pack_v: usize = pairs
        .iter()
        .map(|q| q.pack.iter().flatten().map(Vec::len).sum::<usize>())
        .sum();
    let mut ious: Vec<f64> = Vec::new();
    let mut named_ious: Vec<(f64, String)> = Vec::new();
    for q in &pairs {
        if let (Some(a), Some(b)) = (bbox(&q.cache), bbox(&q.pack)) {
            let iou = bbox_iou(a, b);
            ious.push(iou);
            named_ious.push((iou, q.name.clone()));
        }
    }

    ious.sort_by(|a, b| a.partial_cmp(b).expect("no NaN in an IoU"));
    let pct = |q: f64| {
        ious.get(((ious.len() as f64 - 1.0) * q) as usize)
            .copied()
            .unwrap_or(0.0)
    };
    println!("  cache entries read: {seen} ({unreadable} unreadable)");
    for (k, (n, hit)) in &per_kind {
        println!("    {k:<9} {hit} of {n} present in the pack");
    }
    missing.sort();
    println!("  cache ids the pack does not carry: {}", missing.len());
    println!("    {}", missing.join(" "));
    println!(
        "  vertices over the matched zones: cache {cache_v}, pack {pack_v} ({:.2}x)",
        pack_v as f64 / cache_v.max(1) as f64,
    );
    println!(
        "  bounding-box IoU over {} matched zones: min {:.4}, p1 {:.4}, p50 {:.4}, mean {:.4}",
        ious.len(),
        ious.first().copied().unwrap_or(0.0),
        pct(0.01),
        pct(0.50),
        ious.iter().sum::<f64>() / ious.len().max(1) as f64,
    );
    println!(
        "  zones under 0.90 IoU: {}   under 0.50: {}",
        ious.iter().filter(|v| **v < 0.90).count(),
        ious.iter().filter(|v| **v < 0.50).count(),
    );
    named_ious.sort_by(|a, b| a.0.partial_cmp(&b.0).expect("no NaN in an IoU"));
    println!("  the worst-agreeing zones by box:");
    for (iou, name) in named_ious.iter().take(10) {
        println!("    {iou:.4}  {name}");
    }

    report_area_iou(&pairs, &named_ious);
}

/// One zone seen from both origins, held whole so the area sweep can have it.
struct Pair {
    name: String,
    cache: Vec<GeoPolygon>,
    pack: Vec<GeoPolygon>,
}

/// Everything one pass over the cache directory establishes.
struct Matched {
    /// Zones the cache and the pack both carry, geometry and all.
    pairs: Vec<Pair>,
    /// Cache files that named a zone kind this comparison understands.
    seen: usize,
    unreadable: usize,
    /// Cache ids with no pack entry — counted, and excluded from every figure
    /// downstream, because there is no second shape to compare them against.
    missing: Vec<String>,
    per_kind: BTreeMap<&'static str, (usize, usize)>,
}

/// Every zone the cache and the pack both carry, read once. The geometry is
/// kept rather than reduced on the way past: the area sweep needs it a second
/// time and re-reading seven thousand JSON files would be the slow half of this
/// program.
fn read_pairs(cache_dir: &Path, p: &pack::ZonePack) -> Matched {
    let entries =
        std::fs::read_dir(cache_dir).unwrap_or_else(|e| die(&format!("{cache_dir:?}: {e}")));
    let mut out = Matched {
        pairs: Vec::new(),
        seen: 0,
        unreadable: 0,
        missing: Vec::new(),
        per_kind: BTreeMap::new(),
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(stem) = name.strip_suffix(".json") else {
            continue;
        };
        // `county_TXC113` -> (county, TXC113), the cache's own file naming.
        let Some((kind_s, ugc)) = stem.split_once('_') else {
            continue;
        };
        let kind = match kind_s {
            "forecast" => Kind::Forecast,
            "county" => Kind::County,
            "fire" => Kind::Fire,
            _ => continue,
        };
        out.seen += 1;
        let Ok(text) = std::fs::read_to_string(entry.path()) else {
            out.unreadable += 1;
            continue;
        };
        let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
            out.unreadable += 1;
            continue;
        };
        let Some(polys) = json
            .get("polygons")
            .and_then(|v| serde_json::from_value::<Vec<GeoPolygon>>(v.clone()).ok())
        else {
            out.unreadable += 1;
            continue;
        };

        let e = out.per_kind.entry(kind.label()).or_insert((0, 0));
        e.0 += 1;
        match p.get(kind, ugc) {
            None => out.missing.push(format!("{kind_s}/{ugc}")),
            Some(mine) => {
                e.1 += 1;
                out.pairs.push(Pair {
                    name: format!("{kind_s}/{ugc}"),
                    cache: polys,
                    pack: mine,
                });
            }
        }
    }
    // `read_dir` order is the filesystem's, which is neither stable nor sorted;
    // an `--wkt-pairs` sample of "every nth" would otherwise mean a different
    // set of zones on a different machine.
    out.pairs.sort_by(|x, y| x.name.cmp(&y.name));
    out
}

/// One tab-separated row per zone: `name`, this program's own `area(cache)`,
/// `area(pack)` and `area(∩)` in square degrees, then the two geometries as
/// WKT in the very plane those three numbers were measured in.
///
/// Nothing here interprets anything. The row exists so a second, unrelated
/// geometry library can recompute columns 2-4 from columns 5-6 and disagree.
fn wkt_pairs(cache_dir: &Path, pack_path: &Path, every_nth: usize) {
    let bytes = read_file(pack_path);
    let p = pack::ZonePack::open(bytes).unwrap_or_else(|why| die(&format!("pack: {why}")));
    let m = read_pairs(cache_dir, &p);
    let step = every_nth.max(1);
    for pair in m.pairs.iter().step_by(step) {
        let a = area::areas(&pair.cache, &pair.pack);
        let (wa, wb, wrapped) = area::wkt_pair(&pair.cache, &pair.pack);
        println!(
            "{}\t{:.17e}\t{:.17e}\t{:.17e}\t{}\t{wa}\t{wb}",
            pair.name,
            a.a,
            a.b,
            a.inter,
            if wrapped { "lifted" } else { "plain" },
        );
    }
}

/// The area half of the comparison: same population, same quantiles, so the two
/// blocks can be read against each other line for line.
fn report_area_iou(pairs: &[Pair], box_ious: &[(f64, String)]) {
    let by_box: BTreeMap<&str, f64> = box_ious
        .iter()
        .map(|(v, name)| (name.as_str(), *v))
        .collect();
    let mut rows: Vec<(f64, &Pair, area::Areas)> = Vec::new();
    let mut degenerate: Vec<&str> = Vec::new();
    let mut wrapped: Vec<&str> = Vec::new();
    for pair in pairs {
        let m = area::areas(&pair.cache, &pair.pack);
        if m.wrapped {
            wrapped.push(&pair.name);
        }
        match m.iou() {
            Some(iou) => rows.push((iou, pair, m)),
            None => degenerate.push(&pair.name),
        }
    }
    rows.sort_by(|x, y| x.0.partial_cmp(&y.0).expect("no NaN in an IoU"));

    let n = rows.len();
    let pct = |q: f64| {
        rows.get(((n as f64 - 1.0) * q) as usize)
            .map(|r| r.0)
            .unwrap_or(0.0)
    };
    println!(
        "  polygon-area IoU over {n} matched zones: min {:.4}, p1 {:.4}, p50 {:.4}, mean {:.4}",
        rows.first().map(|r| r.0).unwrap_or(0.0),
        pct(0.01),
        pct(0.50),
        rows.iter().map(|r| r.0).sum::<f64>() / n.max(1) as f64,
    );
    println!(
        "  zones under 0.90 area IoU: {}   under 0.50: {}",
        rows.iter().filter(|r| r.0 < 0.90).count(),
        rows.iter().filter(|r| r.0 < 0.50).count(),
    );
    println!(
        "  zones whose area could not be measured (both operands enclose nothing): {} {}",
        degenerate.len(),
        degenerate.join(" "),
    );
    println!(
        "  zones the antimeridian lift fired on: {} {}",
        wrapped.len(),
        wrapped.join(" "),
    );

    // The whole-corpus symmetric difference, which is the figure that answers
    // "how much of the map changes if the origin is swapped" without letting a
    // per-zone ratio on a tiny zone dominate it.
    let cache_area: f64 = rows.iter().map(|r| r.2.a).sum();
    let pack_area: f64 = rows.iter().map(|r| r.2.b).sum();
    let inter: f64 = rows.iter().map(|r| r.2.inter).sum();
    println!(
        "  summed over those {n} zones (square degrees): cache {cache_area:.4}, pack \
         {pack_area:.4}, intersection {inter:.4}",
    );
    println!(
        "    cache-only {:.4} ({:.4}% of cache), pack-only {:.4} ({:.4}% of pack)",
        cache_area - inter,
        100.0 * (cache_area - inter) / cache_area.max(f64::MIN_POSITIVE),
        pack_area - inter,
        100.0 * (pack_area - inter) / pack_area.max(f64::MIN_POSITIVE),
    );

    // Whole polygons one origin carries that the other does not touch at all —
    // islands, cays, marsh fragments — separated from the same landmass drawn
    // to a different fidelity, over the whole matched population rather than
    // over the handful of zones printed below.
    let mut cache_orphan_zones = 0usize;
    let mut cache_orphan_polys = 0usize;
    let mut cache_orphan_area = 0.0f64;
    let mut pack_orphan_zones = 0usize;
    let mut pack_orphan_polys = 0usize;
    let mut pack_orphan_area = 0.0f64;
    for (_, pair, _) in &rows {
        let (cn, ca) = orphans(&pair.cache, &pair.pack);
        let (pn, pa) = orphans(&pair.pack, &pair.cache);
        if cn > 0 {
            cache_orphan_zones += 1;
        }
        if pn > 0 {
            pack_orphan_zones += 1;
        }
        cache_orphan_polys += cn;
        cache_orphan_area += ca;
        pack_orphan_polys += pn;
        pack_orphan_area += pa;
    }
    println!(
        "  whole polygons one origin has and the other does not touch, over those {n} zones:\n    \
         cache-only {cache_orphan_polys} polygon(s) across {cache_orphan_zones} zone(s), \
         {cache_orphan_area:.4} sq deg\n    pack-only  {pack_orphan_polys} polygon(s) across \
         {pack_orphan_zones} zone(s), {pack_orphan_area:.4} sq deg",
    );

    // The cross-tab is the point of the exercise: every zone either instrument
    // called a disagreement, with what the other one said about it. A zone the
    // box condemns and the area clears is a zone whose *box* moved — an
    // offshore cay, a spit — while its drawn pixels did not.
    let mut flagged: Vec<(&str, f64, f64)> = rows
        .iter()
        .filter_map(|(iou, pair, _)| {
            let b = by_box.get(pair.name.as_str()).copied()?;
            (b < 0.90 || *iou < 0.90).then_some((pair.name.as_str(), b, *iou))
        })
        .collect();
    flagged.sort_by(|x, y| x.1.partial_cmp(&y.1).expect("no NaN in an IoU"));
    println!(
        "  every zone either instrument put under 0.90, of the {n} matched: {} in all",
        flagged.len(),
    );
    println!("      box     area    zone              verdict");
    for (name, b, a) in &flagged {
        let verdict = match (b < &0.90, a < &0.90) {
            (true, false) => "box only - the box moved, the drawn area did not",
            (true, true) => "both",
            (false, true) => "area only - the box hid a real difference",
            (false, false) => unreachable!("filtered above"),
        };
        println!("    {b:.4}  {a:.4}  {name:<16}  {verdict}");
    }

    println!("  the worst-agreeing zones by area, and where the difference sits:");
    for (iou, pair, m) in rows.iter().take(20) {
        let lat = bbox(&pair.cache).map(|b| (b.0 + b.2) / 2.0).unwrap_or(0.0);
        let km2 = |v: f64| area::sq_deg_to_sq_km(v, lat);
        // A polygon of one origin that the other origin does not cover *at all*
        // is an island one side carries and the other drops; a polygon that
        // overlaps is the same landmass drawn to a different fidelity. The two
        // are different findings and must not be summed together.
        let (c_lost, c_lost_area) = orphans(&pair.cache, &pair.pack);
        let (p_lost, p_lost_area) = orphans(&pair.pack, &pair.cache);
        println!(
            "    {iou:.4}  {:<16} cache {:.1} km2 in {} poly, pack {:.1} km2 in {} poly; \
             cache-only {:.1} km2, pack-only {:.1} km2",
            pair.name,
            km2(m.a),
            pair.cache.len(),
            km2(m.b),
            pair.pack.len(),
            km2(m.a_only()),
            km2(m.b_only()),
        );
        println!(
            "            {c_lost} cache polygon(s) the pack does not touch ({:.2} km2); \
             {p_lost} pack polygon(s) the cache does not touch ({:.2} km2)",
            km2(c_lost_area),
            km2(p_lost_area),
        );
    }
}

/// Polygons of `mine` that `theirs` does not overlap by a single square degree,
/// and their total area. Whole landmasses one origin has and the other lacks —
/// a different finding from the same landmass drawn at a different fidelity,
/// and never to be summed with it.
///
/// The boxes prefilter: a polygon whose box misses every box on the other side
/// cannot overlap anything there, and saying so costs a comparison rather than
/// a sweep. Only the survivors are swept, and only against the polygons whose
/// boxes they actually meet.
///
/// The prefilter reads raw longitude, which the antimeridian defeats: a polygon
/// at 179 and one at -179 are neighbours whose boxes do not meet, and dropping
/// the second would invent an orphan. So on a pair that straddles it, the
/// prefilter is switched off and every polygon is swept. That is slower on
/// three zones and wrong on none.
fn orphans(mine: &[GeoPolygon], theirs: &[GeoPolygon]) -> (usize, f64) {
    let straddles = bbox(mine)
        .zip(bbox(theirs))
        .is_some_and(|(a, b)| a.3.max(b.3) - a.1.min(b.1) > 180.0);
    let boxes: Vec<Option<(f64, f64, f64, f64)>> = theirs
        .iter()
        .map(|p| bbox(std::slice::from_ref(p)))
        .collect();
    let overlaps = |a: (f64, f64, f64, f64), b: (f64, f64, f64, f64)| {
        a.0 <= b.2 && b.0 <= a.2 && a.1 <= b.3 && b.1 <= a.3
    };
    let mut count = 0;
    let mut lost = 0.0;
    for poly in mine {
        let Some(mine_box) = bbox(std::slice::from_ref(poly)) else {
            continue;
        };
        let near: Vec<GeoPolygon> = theirs
            .iter()
            .zip(&boxes)
            .filter(|(_, b)| straddles || b.is_some_and(|b| overlaps(mine_box, b)))
            .map(|(p, _)| p.clone())
            .collect();
        let m = area::areas(std::slice::from_ref(poly), &near);
        if m.inter == 0.0 && m.a > 0.0 {
            count += 1;
            lost += m.a;
        }
    }
    (count, lost)
}

// ── helpers ──────────────────────────────────────────────────────────────

fn simplify_all(zones: &[([u8; 7], Vec<GeoPolygon>)], eps: f64) -> Zones {
    // Exactly what `nws::zones::parse_zone_polygons` does to every zone the app
    // fetches: simplify each ring, drop a ring that came back under three
    // points, drop a polygon that lost every ring. Same function, same order,
    // same filters.
    zones
        .iter()
        .map(|(k, polys)| {
            let out: Vec<GeoPolygon> = polys
                .iter()
                .map(|poly| {
                    poly.iter()
                        .map(|ring| simplify_ring(ring, eps))
                        .filter(|r| r.len() >= 3)
                        .collect::<GeoPolygon>()
                })
                .filter(|p: &GeoPolygon| !p.is_empty())
                .collect();
            (*k, out)
        })
        .collect()
}

fn count_vertices(zones: &[([u8; 7], Vec<GeoPolygon>)]) -> usize {
    zones
        .iter()
        .map(|(_, p)| p.iter().flatten().map(Vec::len).sum::<usize>())
        .sum()
}

fn count_rings(zones: &[([u8; 7], Vec<GeoPolygon>)]) -> usize {
    zones
        .iter()
        .map(|(_, p)| p.iter().map(Vec::len).sum::<usize>())
        .sum()
}

/// Net area in square degrees: exterior minus its holes, summed. A fidelity
/// figure, not a geodetic one — the point is the ratio before and after, and
/// both sides are on the same flat approximation.
fn total_area(zones: &[([u8; 7], Vec<GeoPolygon>)]) -> f64 {
    zones
        .iter()
        .flat_map(|(_, p)| p.iter())
        .map(|poly| {
            let mut a = 0.0;
            for (i, ring) in poly.iter().enumerate() {
                let r = rings::signed_area(ring).abs();
                if i == 0 { a += r } else { a -= r }
            }
            a
        })
        .sum()
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(bytes).expect("gzip a Vec cannot fail");
    e.finish().expect("gzip a Vec cannot fail")
}

fn ugc_for(ds: &Dataset, row: &std::collections::HashMap<String, String>) -> Option<String> {
    let get = |k: &str| row.get(k).map(String::as_str).filter(|s| !s.is_empty());
    match ds.ugc {
        Ugc::StateZone => Some(format!("{}Z{}", get("STATE")?, get("ZONE")?)),
        Ugc::StateFips => {
            let fips = get("FIPS")?;
            let state = get("STATE")?;
            (fips.len() == 5).then(|| format!("{state}C{}", &fips[2..]))
        }
        Ugc::Column(c) => get(c).map(str::to_string),
    }
}

/// Constraint 4 from the brief, checked rather than assumed.
fn well_formed(ugc: &str) -> bool {
    let b = ugc.as_bytes();
    b.len() == 6
        && b[0].is_ascii_uppercase()
        && b[1].is_ascii_uppercase()
        && (b[2] == b'C' || b[2] == b'Z')
        && b[3..6].iter().all(u8::is_ascii_digit)
}

fn key_ugc(k: &[u8; 7]) -> String {
    String::from_utf8_lossy(&k[1..]).trim().to_string()
}

fn key_str(k: &[u8; 7]) -> String {
    let kind = Kind::from_byte(k[0]).map(Kind::label).unwrap_or("?");
    format!("{kind}/{}", key_ugc(k))
}

fn describe_row(row: &std::collections::HashMap<String, String>) -> String {
    let mut v: Vec<_> = row.iter().map(|(k, x)| format!("{k}={x:?}")).collect();
    v.sort();
    v.join(" ")
}

fn read_file(p: &Path) -> Vec<u8> {
    std::fs::read(p).unwrap_or_else(|e| die(&format!("read {p:?}: {e}")))
}

fn die(msg: &str) -> ! {
    eprintln!("nws-zone-pack: {msg}");
    std::process::exit(1)
}
