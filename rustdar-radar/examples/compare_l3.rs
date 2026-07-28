//! Compare a Level III product file against rustdar's own derivation of the
//! same field — the [`rustdar_radar::twin::compare`] toolkit driven from two
//! local files, no network.
//!
//! ```text
//! cargo run -p rustdar-radar --example compare_l3 -- \
//!     <archive2-file> <l3-file> <eet|dvl|kdp|hhc|dpr> [elev]
//! ```
//!
//! `eet` compares against the shipped interpolated echo tops. The other
//! products have no local derivation yet (they arrive in later work
//! packages); for those the derived grid is all-NaN, so the tally degenerates
//! to the Level III product's own footprint — presence counts and the
//! histogram — which is still the comparator exercising every code path it
//! will use once the derivations exist.

use rustdar_radar::twin::compare::{self, ProductKind, Tally, ValueCodec};
use rustdar_radar::volumetric;

fn usage() -> ! {
    eprintln!(
        "usage: compare_l3 <archive2-file> <l3-file> <eet|dvl|kdp|hhc|dpr> [elev]\n\
         \n\
         <archive2-file>  a NEXRAD Level II archive volume (e.g. KOAX20260727_120345_V06)\n\
         <l3-file>        a Level III product object (e.g. OAX_EET_2026_07_27_12_03_45)\n\
         [elev]           expected PDB elevation number, checked when given"
    );
    std::process::exit(2);
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (l2_path, l3_path, product) = match (args.get(1), args.get(2), args.get(3)) {
        (Some(a), Some(b), Some(c)) => (a, b, c.as_str()),
        _ => usage(),
    };
    if !matches!(product, "eet" | "dvl" | "kdp" | "hhc" | "dpr") {
        usage();
    }
    let expected_elevation: Option<u16> = args.get(4).map(|s| match s.parse() {
        Ok(e) => e,
        Err(_) => usage(),
    });

    // ── The Level II side ────────────────────────────────────────────────
    let l2_bytes = std::fs::read(l2_path).expect("read the archive2 file");
    let scan = nexrad_data::volume::File::new(l2_bytes)
        .scan()
        .expect("decode the archive2 volume");
    println!(
        "L2: {} sweeps, VCP {:?}",
        scan.sweeps().len(),
        scan.coverage_pattern_number(),
    );

    // ── The Level III side ───────────────────────────────────────────────
    let l3_bytes = std::fs::read(l3_path).expect("read the level3 file");
    let msg = nexrad_level3::decode::decode_product(&l3_bytes).expect("decode the level3 product");
    println!(
        "L3: product {} (message {}), elevation_number {}, VCP {}, volume start {}",
        msg.pdb.product_code,
        msg.header.message_code,
        msg.pdb.elevation_number,
        msg.pdb.vcp,
        compare::volume_scan_started(&msg.pdb)
            .map(|t| t.to_string())
            .unwrap_or_else(|| "unreadable".to_string()),
    );
    if let Some(want) = expected_elevation {
        if msg.pdb.elevation_number == want {
            println!("L3: elevation_number matches the requested {want}");
        } else {
            println!(
                "L3: WARNING — elevation_number {} is not the requested {want}",
                msg.pdb.elevation_number,
            );
        }
    }

    let codec = ValueCodec::for_message(&msg).expect("the product carries a radial packet");
    print_l3_histogram(&msg, &codec);

    // ── The derived side ─────────────────────────────────────────────────
    let derived: Vec<Vec<f32>> = match product {
        "eet" => {
            println!("derived: interpolated echo tops from the Level II volume");
            volumetric::compute_echo_tops(&scan).values
        }
        _ => {
            println!(
                "derived: no local {product} derivation yet — comparing against an \
                 all-NaN grid, so every defined L3 cell reads as a presence \
                 disagreement",
            );
            vec![vec![f32::NAN; volumetric::RANGE_BINS]; 360]
        }
    };

    let kind = if product == "hhc" {
        ProductKind::Class
    } else {
        ProductKind::Numeric
    };
    let tally =
        compare::tally_against_l3(&derived, &msg, kind).expect("the product carries radials");
    print_tally(&tally, kind);
}

/// The Level III product's own footprint: how many gates are defined and
/// where their physical values sit — a sanity readout before any comparison.
fn print_l3_histogram(msg: &nexrad_level3::model::Level3Message, codec: &ValueCodec) {
    let Some(packet) = rustdar_radar::srm::radial_packet(msg) else {
        return;
    };
    let mut total = 0usize;
    let mut defined = 0usize;
    let mut min = f32::INFINITY;
    let mut max = f32::NEG_INFINITY;
    let mut sum = 0f64;
    let mut levels = std::collections::BTreeMap::<u16, usize>::new();
    for run in &packet.radials {
        for &gate in &run.gate_values {
            total += 1;
            let v = codec.decode(gate);
            if v.is_finite() {
                defined += 1;
                min = min.min(v);
                max = max.max(v);
                sum += f64::from(v);
                *levels.entry(gate).or_insert(0) += 1;
            }
        }
    }
    println!(
        "L3 histogram: {defined} of {total} gates defined across {} levels",
        levels.len(),
    );
    if defined > 0 {
        println!(
            "L3 physical: min {min:.2}, max {max:.2}, mean {:.2}",
            sum / defined as f64,
        );
        let mut top: Vec<(usize, u16)> = levels.iter().map(|(&l, &n)| (n, l)).collect();
        top.sort_by(|a, b| b.cmp(a));
        let head: Vec<String> = top
            .iter()
            .take(8)
            .map(|(n, l)| format!("level {l} ({:.1}) ×{n}", codec.decode(*l)))
            .collect();
        println!("L3 busiest levels: {}", head.join(", "));
    }
}

fn print_tally(t: &Tally, kind: ProductKind) {
    println!("tally over the 360° × 230 km domain:");
    println!("  derived defined:         {}", t.derived_defined);
    println!("  L3 defined:              {}", t.l3_defined);
    println!("  compared (both defined): {}", t.compared);
    println!(
        "  exact:                   {} ({:.2}%)",
        t.exact,
        t.exact_pct()
    );
    println!(
        "  within ±1 level:         {} ({:.2}%)",
        t.within_one,
        t.within_one_pct(),
    );
    println!(
        "  within ±2 levels:        {} ({:.2}%)",
        t.within_two,
        t.within_two_pct(),
    );
    println!(
        "  presence disagreements:  {} ({:.2}% of the union)",
        t.presence_disagreements,
        t.presence_disagreement_pct(),
    );
    if kind == ProductKind::Class && !t.confusion.is_empty() {
        println!("  confusion (derived level → L3 level, top 20):");
        let mut cells: Vec<(usize, (u8, u8))> = t.confusion.iter().map(|(&k, &n)| (n, k)).collect();
        cells.sort_by(|a, b| b.cmp(a));
        for (n, (d, l)) in cells.into_iter().take(20) {
            println!("    {d:>3} → {l:>3}: {n}");
        }
    }
}
