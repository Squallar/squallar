//! **What a hit map holds on the heap**, so the census can stop reading a
//! literal zero for the live panes' hit maps.
//!
//! A hit map is two halves. The quarter-cell index is an `FxHashMap<u32,
//! Vec<u32>>` with one entry per cell the rasterizer drew into, and it is
//! always this map's own. The items those cells name are the layer's, and the
//! two shapes they arrive in are priced differently on purpose: a
//! [`HitItems::Rows`] list is a pointer vector the map owns, a
//! [`HitItems::Slab`] is a handle onto a block the layer already prices, and
//! both shipped hit-map layers take the slab.

use super::*;
use crate::glm::{GlmDataLevel, GlmFlash, GlmSatellite};
use crate::render::handlers::glm::GlmSlab;
use crate::render::handlers::reports::StormReportItem;
use crate::spc::reports::{StormReport, StormReportKind};

/// The texture the fixture cells are recorded for; its quarter grid is 64 x 64,
/// so every id below 4096 gets a cell of its own.
const W: u32 = 256;
const H: u32 = 256;

/// `k` storm reports as the layer holds them, one `Arc` apiece, as the
/// [`HitItems::Rows`] list a materialising layer would answer.
fn report_rows(k: usize) -> HitItems {
    (0..k)
        .map(|index| {
            Arc::new(StormReportItem {
                report: StormReport {
                    kind: StormReportKind::Hail,
                    time: "2015".into(),
                    valid: None,
                    magnitude: Some(100.0),
                    location: "NORMAN".into(),
                    county: "CLEVELAND".into(),
                    state: "OK".into(),
                    lat: 35.22,
                    lon: -97.44,
                    comments: String::new(),
                },
                index,
            }) as Arc<dyn OverlayItem>
        })
        .collect()
}

fn a_flash() -> GlmFlash {
    GlmFlash {
        lat: 35.22,
        lon: -97.44,
        energy: f32::NAN,
        area: f32::NAN,
        time: chrono::NaiveDate::from_ymd_opt(2026, 7, 24)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap(),
        satellite: GlmSatellite::GoesEast,
        level: GlmDataLevel::Flash,
    }
}

/// Cells with `k` distinct ids recorded, each in its own quarter-cell, so the
/// index grows with `k` rather than one cell's vector doing all the growing.
fn cells_for(k: usize) -> HitCells {
    let mut cells = HitCells::new(W, H);
    let per_row = (W / 4) as usize;
    for id in 0..k {
        let qx = (id % per_row) as f32;
        let qy = (id / per_row) as f32;
        cells.record(qx * 4.0 + 1.0, qy * 4.0 + 1.0, id as u32);
    }
    cells
}

/// **The gate.** A map holding `k` items reports more than an empty one, and
/// more again each time `k` grows — and the items half is at least the `k`
/// pointers the list holds.
#[test]
fn a_hit_map_with_k_items_reports_more_than_an_empty_one_and_grows_with_k() {
    let empty = HitMap::from_cells(HitCells::new(W, H), &report_rows(0));
    let mut last = empty.resident_bytes();
    for k in [1usize, 8, 64, 512] {
        let cells = cells_for(k);
        let cells_alone = cells.resident_bytes();
        let map = HitMap::from_cells(cells, &report_rows(k));
        let bytes = map.resident_bytes();
        assert!(
            bytes > last,
            "k={k}: {bytes} bytes is not above the {last} the smaller map reported"
        );
        assert!(
            bytes - cells_alone >= k * size_of::<Arc<dyn OverlayItem>>(),
            "k={k}: the items half is {} bytes, below the {} its {k} pointers occupy",
            bytes - cells_alone,
            k * size_of::<Arc<dyn OverlayItem>>()
        );
        last = bytes;
    }
}

/// A slab-backed map prices its cells and nothing else: the block the handle
/// reaches is the layer's own and is priced there. Both shipped hit-map layers
/// take this arm, so this is the figure the census reads for them.
#[test]
fn a_slab_backed_hit_map_prices_its_cells_alone() {
    const K: usize = 64;
    let slab: Arc<dyn squallar_source::hit::HitResolve> = Arc::new(GlmSlab {
        flashes: vec![a_flash(); K],
    });
    let cells = cells_for(K);
    let cells_alone = cells.resident_bytes();
    assert!(cells_alone > 0, "{K} recorded cells hold something");

    let map = HitMap::from_cells(cells, &HitItems::Slab(slab));
    assert_eq!(
        map.resident_bytes(),
        cells_alone,
        "a slab's block is the layer's own and priced there; adding it here \
         would put the same {K} flashes into two census families"
    );
}
