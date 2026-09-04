//! The census's arithmetic and its one line. Host tests: nothing here needs a
//! browser, and the line is the same line the wasm hook writes.

use super::*;

/// A census with one distinct figure per family, so a field swapped for its
/// neighbour in `census()` or in the line fails rather than reading the same.
fn distinct() -> Census {
    Census {
        loop_scan_bytes: 1,
        loop_l3_bytes: 2,
        still_scan_bytes: 4,
        derive_memo_bytes: 8,
        loop_frame_scan_bytes: 16,
        render_cache_bytes: 32,
        overlay_picture_bytes: 64,
        overlay_grid_bytes: 128,
        loop_frame_bytes: 256,
        upload_pending_bytes: 512,
        tile_body_bytes: 1024,
        tile_parsed_bytes: 2048,
        tile_cache_bytes: 4096,
        loan_outstanding_bytes: 8192,
        volume_store_bytes: 16384,
        job_in_flight_bytes: 32768,
        tile_mesh_bytes: 65536,
    }
}

/// The resident total is every family that lives on this heap and **not** the
/// tile meshes, which are the GPU's. Powers of two make the omission
/// unambiguous: a total that included them could only be this one bug.
#[test]
fn the_resident_total_leaves_the_gpu_family_out() {
    let c = distinct();
    let every_family = (1 << 17) - 1;
    assert_eq!(c.resident_total(), every_family - 65536);
    assert_eq!(c.radar_total(), 1 + 2 + 4 + 8 + 16);
}

/// The residual is the reading less the families, and `None` — not zero, and
/// not a wrapped `u64` — where the families price above the reading.
#[test]
fn the_residual_is_against_a_real_reading_and_refuses_to_wrap() {
    let c = distinct();
    let total = c.resident_total();
    assert_eq!(c.residual(total + 1_000), Some(1_000));
    assert_eq!(c.residual(total), Some(0));
    assert_eq!(
        c.residual(total - 1),
        None,
        "a shortfall wrapped instead of refusing"
    );
}

/// Every family is a level: set replaces, it does not accumulate. The whole
/// instrument is wrong if a second publish of the same figure doubles it.
#[test]
fn a_level_is_set_and_not_added() {
    reset();
    set_loop_scan_bytes(500);
    set_loop_scan_bytes(500);
    assert_eq!(census().loop_scan_bytes, 500);
    set_loop_scan_bytes(0);
    assert_eq!(
        census().loop_scan_bytes,
        0,
        "a level could not go back down"
    );
    reset();
}

/// The line names every family, its own total, the reading it is against and
/// the residual — and the GPU family is on it but marked out of the sum.
#[test]
fn the_line_names_every_family_and_its_denominator() {
    let c = distinct();
    let said = line(&c, Some(c.resident_total() + 900_000_000), "page");
    for field in [
        "loop scans 1 B",
        "loop l3 2 B",
        "still scans 4 B",
        "derive memo 8 B",
        "loop frame scans 16 B",
        "render cache 32 B",
        "overlay pictures 64 B",
        "overlay grids 128 B",
        "loop frames 256 B",
        "upload pending 512 B",
        "tile bodies 1024 B",
        "tile parsed 2048 B",
        "tile cache 4096 B",
        "loans out 8192 B",
        "volume store 16384 B",
        "jobs in flight 32768 B",
    ] {
        assert!(said.contains(field), "{field} missing from {said}");
    }
    assert!(said.starts_with("heap census (page): "), "{said}");
    assert!(said.contains("residual 900000000 B"), "{said}");
    assert!(
        said.contains("tile meshes 65536 B (GPU, not in the total)"),
        "the GPU family must be on the line and marked out of the sum: {said}"
    );
}

/// An unread heap says so instead of printing a residual against nothing.
/// The hook's own case: `memory_bytes` is an `Option` because the cast can
/// fail, and a census that invented a denominator there would be the exact
/// defect this whole lane is chasing.
#[test]
fn an_unread_heap_prints_no_residual() {
    let said = line(&distinct(), None, "page");
    assert!(said.contains("unread linear, residual unknown"), "{said}");
    assert!(!said.contains("residual 0"), "{said}");
}

/// The line fits the fixed buffer the allocation-error hook writes it into,
/// at the widest figures a `u64` can hold. The hook cannot allocate, so a
/// line that outgrew its buffer would be silently cut at the very moment it
/// is the only evidence there is.
#[test]
fn the_widest_line_fits_the_hooks_buffer() {
    let widest = Census {
        loop_scan_bytes: u64::MAX,
        loop_l3_bytes: u64::MAX,
        still_scan_bytes: u64::MAX,
        derive_memo_bytes: u64::MAX,
        loop_frame_scan_bytes: u64::MAX,
        render_cache_bytes: u64::MAX,
        overlay_picture_bytes: u64::MAX,
        overlay_grid_bytes: u64::MAX,
        loop_frame_bytes: u64::MAX,
        upload_pending_bytes: u64::MAX,
        tile_body_bytes: u64::MAX,
        tile_parsed_bytes: u64::MAX,
        tile_cache_bytes: u64::MAX,
        tile_mesh_bytes: u64::MAX,
        volume_store_bytes: u64::MAX,
        loan_outstanding_bytes: u64::MAX,
        job_in_flight_bytes: u64::MAX,
    };
    let said = line(&widest, Some(u64::MAX), "rasterization worker");
    assert!(
        said.len() <= CENSUS_LINE_CAPACITY,
        "the widest census line is {} bytes, past the hook's {CENSUS_LINE_CAPACITY}",
        said.len()
    );
}
