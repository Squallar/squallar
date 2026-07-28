//! Render one product tilt from a local Level II archive file to a PPM image.,
//!
//! ```sh
//! cargo run -p rustdar-radar --example render_product -- \
//!     KMKX_20260727_174935 42.96778 -88.55056 nrot out.ppm [elevation]
//! ```

use rustdar_radar::types::RadarProduct;

fn main() {
    let mut args = std::env::args().skip(1);
    let usage = "usage: render_product <archive2-file> <lat> <lon> <br|bv|nrot> <out.ppm> [elev]";
    let path = args.next().expect(usage);
    let lat: f64 = args.next().expect(usage).parse().unwrap();
    let lon: f64 = args.next().expect(usage).parse().unwrap();
    let product = match args.next().expect(usage).as_str() {
        "br" => RadarProduct::Reflectivity,
        "bv" => RadarProduct::Velocity,
        "sw" => RadarProduct::SpectrumWidth,
        "zdr" => RadarProduct::DifferentialReflectivity,
        "cc" => RadarProduct::CorrelationCoefficient,
        "phi" => RadarProduct::DifferentialPhase,
        "nrot" => RadarProduct::NormalizedRotation,
        other => panic!("unknown product {other}"),
    };
    let out = args.next().expect(usage);
    let elevation: f32 = args.next().map_or(0.5, |e| e.parse().unwrap());

    let data = std::fs::read(&path).expect("read archive file");
    let file = nexrad_data::volume::File::new(data);
    let scan = file.scan().expect("decode archive file");

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

    if std::env::var("SWEEP_DEBUG").is_ok() {
        for (si, sw) in scan.sweeps().iter().enumerate() {
            if let Some(r) = sw.radials().first() {
                eprintln!(
                    "sweep {si}: elev {:.2} moment {:?}",
                    r.elevation_angle_degrees(),
                    product.get_moment(r).is_some()
                );
            }
        }
    }
    let (rgba, max_range_km, _values) = rustdar_radar::render::render_radar_to_image_with_winds(
        &scan,
        elevation,
        product,
        lat,
        lon,
        wind_levels.as_deref(),
    )
    .expect("no sweep at that elevation");

    let side = (rgba.len() / 4).isqrt();
    eprintln!("rendered {side}x{side}, max range {max_range_km:.1} km");

    // Composite onto near-black and write a binary PPM.
    let mut ppm = format!("P6\n{side} {side}\n255\n").into_bytes();
    for px in rgba.chunks_exact(4) {
        let a = px[3] as u32;
        for &c in &px[..3] {
            ppm.push(((c as u32 * a + 20 * (255 - a)) / 255) as u8);
        }
    }
    std::fs::write(&out, ppm).expect("write ppm");
    eprintln!("wrote {out}");
}

// (debug helper appended during product audit; remove before commit)
