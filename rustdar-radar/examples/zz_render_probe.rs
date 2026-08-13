//! TEMPORARY measurement instrument — never committed.
//!
//! `cargo run --release -p rustdar-radar --example zz_render_probe -- <vol>...`

use nexrad_model::data::DataMoment;
use rustdar_radar::types::RadarProduct;

fn scan_of(path: &std::path::Path) -> nexrad_model::data::Scan {
    let bytes = std::fs::read(path).unwrap();
    let name = path.file_name().unwrap().to_str().unwrap();
    let contents = rustdar_radar::chunks::decode_chunk(name, &bytes).unwrap();
    let cp = contents.coverage_pattern.unwrap();
    let sweeps = nexrad_model::data::Sweep::from_radials(contents.radials);
    nexrad_model::data::Scan::with_site(contents.site.unwrap(), cp, sweeps)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    println!(
        "{:<6} {:<14} {:>7} {:>6} {:>8} {:>10} {:>9} {:>7} {:>9} {:>7}",
        "site", "product", "ext_km", "side", "px_per_km", "painted", "fill%", "ms", "gates", "gate_px"
    );
    for a in &args {
        let path = std::path::PathBuf::from(a);
        let scan = scan_of(&path);
        let site = scan
            .site()
            .map(|s| String::from_utf8_lossy(s.identifier()).to_string())
            .unwrap_or_else(|| "????".to_owned());
        let (lat, lon) = scan
            .site()
            .map(|s| (f64::from(s.latitude()), f64::from(s.longitude())))
            .unwrap();
        // Lowest elevation carrying each product.
        for product in [RadarProduct::Velocity, RadarProduct::Reflectivity] {
            let mut lowest: Option<(f32, f64, u16)> = None;
            for sweep in scan.sweeps() {
                let radials = sweep.radials();
                let Some(m) = radials.iter().find_map(|r| product.get_moment(r)) else {
                    continue;
                };
                let e = rustdar_radar::volumetric::sweep_elevation_deg(radials).unwrap_or(99.0);
                if lowest.is_none() || e < lowest.unwrap().1 {
                    lowest = Some((e as f32, e, m.gate_interval_km() as u16 * 0));
                }
                let _ = m;
            }
            let Some((elev, _, _)) = lowest else { continue };
            let gate_km = scan
                .sweeps()
                .iter()
                .flat_map(|s| s.radials())
                .find_map(|r| product.get_moment(r).map(|m| m.gate_interval_km()))
                .unwrap_or(0.25);
            let gates = scan
                .sweeps()
                .iter()
                .flat_map(|s| s.radials())
                .filter_map(|r| product.get_moment(r).map(|m| m.gate_count()))
                .max()
                .unwrap_or(0);

            let mut best_ms = f64::INFINITY;
            let mut out = None;
            for _ in 0..3 {
                let t = std::time::Instant::now();
                let r = rustdar_radar::render::render_radar_to_image(&scan, elev, product, lat, lon);
                let ms = t.elapsed().as_secs_f64() * 1000.0;
                best_ms = best_ms.min(ms);
                out = r;
            }
            let Some(r) = out else { continue };
            let side = (r.values.len() as f64).sqrt() as usize;
            let painted = r.values.iter().filter(|v| !v.is_nan()).count();
            let px_per_km = side as f64 / (2.0 * r.max_range_km);
            println!(
                "{site:<6} {:<14} {ext:>7.2} {side:>6} {px_per_km:>9.4} {painted:>10} {:>8.2}% {best_ms:>7.1} {gates:>9} {:>7.3}",
                format!("{product:?}"),
                painted as f64 / (side * side) as f64 * 100.0,
                gate_km * px_per_km,
                ext = r.max_range_km,
            );
            println!(
                "       image {:.2} MiB, values {:.2} MiB, useful bytes {:.2} MiB",
                r.image.len() as f64 / 1048576.0,
                r.values.len() as f64 * 4.0 / 1048576.0,
                painted as f64 * 8.0 / 1048576.0,
            );
        }
    }
}
