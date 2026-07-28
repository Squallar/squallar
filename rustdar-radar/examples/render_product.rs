//! Render one product tilt from a local Level II archive file to a PPM image.,
//!
//! ```sh
//! cargo run -p rustdar-radar --example render_product -- \
//!     KMKX_20260727_174935 42.96778 -88.55056 nrot out.ppm [elevation]
//! ```

use rustdar_radar::types::RadarProduct;

fn main() {
    let mut args = std::env::args().skip(1);
    let usage =
        "usage: render_product <archive2-file> <lat> <lon> <br|bv|nrot|kdp|...> <out.ppm> [elev]";
    let path = args.next().expect(usage);
    let lat: f64 = args.next().expect(usage).parse().unwrap();
    let lon: f64 = args.next().expect(usage).parse().unwrap();
    let product_code = args.next().expect(usage);
    let out = args.next().expect(usage);
    let elevation: f32 = args.next().map_or(0.5, |e| e.parse().unwrap());

    let data = std::fs::read(&path).expect("read archive file");
    let file = nexrad_data::volume::File::new(data);
    let scan = file.scan().expect("decode archive file");

    // KDP is derived from the Level II dual-pol moments rather than read off
    // a moment, so it renders through its own path — with the radial-header
    // parameters (initial system PhiDP, dBZ0, atmos) read from the raw file,
    // exactly as the RPG reads them.
    if product_code == "kdp" {
        let params = rustdar_radar::kdp::KdpParams::from_archive(&file);
        eprintln!("kdp params from archive: {params:?}");
        let (rgba, max_range_km, _values) =
            rustdar_radar::render::render_derived_kdp_to_image(&scan, elevation, lat, lon, &params)
                .expect("no dual-pol sweep at that elevation");
        write_ppm(&out, &rgba, max_range_km);
        return;
    }

    let product = match product_code.as_str() {
        "br" => RadarProduct::Reflectivity,
        "bv" => RadarProduct::Velocity,
        "sw" => RadarProduct::SpectrumWidth,
        "zdr" => RadarProduct::DifferentialReflectivity,
        "cc" => RadarProduct::CorrelationCoefficient,
        "phi" => RadarProduct::DifferentialPhase,
        "nrot" => RadarProduct::NormalizedRotation,
        "eti" => RadarProduct::EchoTopsInterpolated,
        "vild" => RadarProduct::VilDensity,
        other => panic!("unknown product {other}"),
    };

    // WIND_FILE ("height_km u v" per line, e.g. a parsed NVW product) feeds
    // NROT's wind-aided dealiaser, matching probe_nrot and the app path.
    let wind_levels: Option<Vec<(f64, f64, f64)>> = std::env::var("WIND_FILE").ok().map(|p| {
        std::fs::read_to_string(&p)
            .expect("read WIND_FILE")
            .lines()
            .filter_map(|l| {
                let mut it = l.split_whitespace();
                Some((
                    it.next()?.parse().ok()?,
                    it.next()?.parse().ok()?,
                    it.next()?.parse().ok()?,
                ))
            })
            .collect()
    });

    let (rgba, max_range_km, _values) = rustdar_radar::render::render_radar_to_image_with_winds(
        &scan,
        elevation,
        product,
        lat,
        lon,
        wind_levels.as_deref(),
    )
    .expect("no sweep at that elevation");
    write_ppm(&out, &rgba, max_range_km);
}

/// Composite onto near-black and write a binary PPM.
fn write_ppm(out: &str, rgba: &[u8], max_range_km: f64) {
    let side = (rgba.len() / 4).isqrt();
    eprintln!("rendered {side}x{side}, max range {max_range_km:.1} km");
    let mut ppm = format!("P6\n{side} {side}\n255\n").into_bytes();
    for px in rgba.chunks_exact(4) {
        let a = px[3] as u32;
        for &c in &px[..3] {
            ppm.push(((c as u32 * a + 20 * (255 - a)) / 255) as u8);
        }
    }
    std::fs::write(out, ppm).expect("write ppm");
    eprintln!("wrote {out}");
}
