//! TEMPORARY measurement instrument — never committed.
//!
//! `cargo run --release -p rustdar-radar --example zz_reach_probe -- <vol>...`

use nexrad_model::data::DataMoment;
use rustdar_radar::types::RadarProduct;

fn scan_of(path: &std::path::Path) -> nexrad_model::data::Scan {
    let bytes = std::fs::read(path).unwrap();
    let name = path.file_name().unwrap().to_str().unwrap();
    let contents = rustdar_radar::chunks::decode_chunk(name, &bytes).unwrap();
    let cp = contents.coverage_pattern.unwrap();
    let sweeps = nexrad_model::data::Sweep::from_radials(contents.radials);
    match contents.site {
        Some(site) => nexrad_model::data::Scan::with_site(site, cp, sweeps),
        None => panic!("no site"),
    }
}

/// Slant reach of `product` on one sweep's radials, and the ground factor.
fn sweep_reach(radials: &[nexrad_model::data::Radial], product: RadarProduct) -> Option<(f64, f64)> {
    let mut slant = f64::NEG_INFINITY;
    for r in radials {
        let Some(m) = product.get_moment(r) else {
            continue;
        };
        slant =
            slant.max(m.first_gate_range_km() + f64::from(m.gate_count()) * m.gate_interval_km());
    }
    if slant == f64::NEG_INFINITY {
        return None;
    }
    let e = rustdar_radar::volumetric::sweep_elevation_deg(radials).unwrap_or(0.0);
    Some((slant, e))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    println!(
        "{:<8} {:<24} {:>6} {:>7} {:>9} {:>9} {:>8} {:>8} {:>8} {:>8}",
        "site",
        "product",
        "elev",
        "gates",
        "slant_km",
        "ground_km",
        "ext_old",
        "ext_new",
        "fill_old",
        "fill_new"
    );
    for a in &args {
        let path = std::path::PathBuf::from(a);
        let scan = scan_of(&path);
        let site = scan
            .site()
            .map(|s| String::from_utf8_lossy(s.identifier()).to_string())
            .unwrap_or_else(|| "????".to_owned());
        for product in [
            RadarProduct::Reflectivity,
            RadarProduct::Velocity,
            RadarProduct::SpectrumWidth,
            RadarProduct::CorrelationCoefficient,
            RadarProduct::DifferentialReflectivity,
        ] {
            // Lowest sweep carrying the moment — what a pane opens on.
            let mut best: Option<(f64, f64, u16)> = None;
            for sweep in scan.sweeps() {
                let radials = sweep.radials();
                let Some((slant, e)) = sweep_reach(radials, product) else {
                    continue;
                };
                let gates = radials
                    .iter()
                    .filter_map(|r| product.get_moment(r))
                    .map(|m| m.gate_count())
                    .max()
                    .unwrap_or(0);
                if best.is_none() || e < best.unwrap().1 {
                    best = Some((slant, e, gates));
                }
            }
            let Some((slant, e, gates)) = best else {
                continue;
            };
            let ground = slant * e.to_radians().cos();
            let ext_old = rustdar_radar::types::plan_view_extent_km(ground);
            let ext_new = ground;
            let fill = |ext: f64| {
                std::f64::consts::PI * ground * ground / (4.0 * ext * ext) * 100.0
            };
            println!(
                "{site:<8} {:<24} {e:>6.2} {gates:>7} {slant:>9.3} {ground:>9.3} {ext_old:>8.2} {ext_new:>8.2} {:>7.1}% {:>7.1}%",
                format!("{product:?}"),
                fill(ext_old),
                fill(ext_new),
            );
        }
        // Whole-volume reach, the 3D path's answer, for comparison.
        for product in [RadarProduct::Reflectivity, RadarProduct::Velocity] {
            println!(
                "{site:<8} {:<24} volume_reach_km = {:.3}",
                format!("{product:?}"),
                rustdar_radar::voxel::volume_reach_km(&scan, product),
            );
        }
    }
}
