//! The section-plane slot parks a set of planes only when no plane holds
//! capacity beyond a section.
//!
//! **What this pins is retention, not correctness.** `CrossSection::from_parts`
//! is the door every section that did not come out of the renderer arrives by —
//! a worker's reply, a cache, a test — and it weighs each plane's `len`. A
//! plane assembled by growth rather than by an exact reserve is a legal section
//! by that measure while carrying capacity its length does not account for, and
//! `SectionPlanes::fit` never shrinks: parked, that overhang is held for the
//! life of the process.
//!
//! **Why its own test binary, and why one `#[test]`.** The slot and its byte
//! level are process-wide. `section_plane_pool.rs` drives the same slot through
//! the renderer, and a second `#[test]` beside it — here or there — would let
//! the harness interleave the two on separate threads and make every reading
//! below some other arm's. A separate integration target is a separate process,
//! so the premise of an empty slot at a level of zero is this file's own.
//!
//! Every expectation is computed from the planes that moved, never from a
//! constant, and plane by plane: a bound over the set's summed bytes is one a
//! single doubled plane sits under.

use squallar_radar::sampler::SampleStatus;
use squallar_radar::xsect::{
    CrossSection, SECTION_HEIGHT, SECTION_WIDTH, SectionAxes, pooled_bytes,
};

/// Axes that pass `from_parts`: finite throughout, with an empty tilt ladder
/// so the two clock vectors are empty too.
fn axes() -> SectionAxes {
    SectionAxes {
        length_km: 100.0,
        base_km_msl: 0.4,
        top_km_msl: 20.4,
        near_ground_range_km: 0.1,
        far_ground_range_km: 100.0,
        coverage_ground_range_km: 80.0,
        cone_of_silence_km: 0.0,
        tilt_count: 0,
        widest_tilt_gap_deg: 0.0,
        top_tilt_deg: 0.0,
        top_declared_cut_deg: 0.0,
    }
}

/// A byte plane `need` long holding `capacity`.
fn bytes(need: usize, capacity: usize) -> Vec<u8> {
    let mut v = Vec::new();
    v.reserve_exact(capacity);
    v.resize(need, 0u8);
    v
}

/// A section whose three planes are each exactly a section long and hold the
/// named overhang, in elements, beyond it.
///
/// The image plane's overhang is in bytes, the other two in pixels, because
/// each plane is weighed against its own need and the three needs are three
/// widths.
fn section(image_over: usize, value_over: usize, status_over: usize) -> CrossSection {
    let pixels = SECTION_WIDTH * SECTION_HEIGHT;

    let mut values: Vec<f32> = Vec::new();
    values.reserve_exact(pixels + value_over);
    values.resize(pixels, f32::NAN);

    CrossSection::from_parts(
        bytes(pixels * 4, pixels * 4 + image_over),
        values,
        {
            let mut status = bytes(pixels, pixels + status_over);
            status.fill(SampleStatus::NoCoverage.wire_code());
            status
        },
        axes(),
        Vec::new(),
        Vec::new(),
    )
    .expect("planes of a section's length, all NoCoverage over NaN, are a section")
}

#[test]
fn a_plane_holding_more_than_a_section_is_declined_rather_than_parked() {
    let pixels = SECTION_WIDTH * SECTION_HEIGHT;
    let section_bytes = pixels * 4 + pixels * std::mem::size_of::<f32>() + pixels;

    assert_eq!(
        pooled_bytes(),
        0,
        "premise: nothing in this process has parked planes, yet the slot \
         reports {} B",
        pooled_bytes()
    );

    // One arm per plane, each starting from an empty slot, so a park would be
    // visible and cannot be attributed to the slot being full.
    for (label, image_over, value_over, status_over) in [
        ("image", pixels * 4, 0, 0),
        ("value", 0, pixels, 0),
        ("status", 0, 0, pixels),
    ] {
        let fat = section(image_over, value_over, status_over);
        assert_eq!(
            fat.image().len(),
            pixels * 4,
            "premise: the {label} arm's section is a section by length, which \
             is all `from_parts` weighs",
        );
        drop(fat);
        assert_eq!(
            pooled_bytes(),
            0,
            "a set whose {label} plane holds an extra section's worth was \
             parked rather than declined, and the slot now reports {} B against \
             a section's {section_bytes} B",
            pooled_bytes(),
        );
    }

    // The same door with nothing to decline. Without this the arms above pass
    // on a slot that is simply shut.
    let exact = section(0, 0, 0);
    drop(exact);
    assert_eq!(
        pooled_bytes(),
        section_bytes,
        "an exactly-sized set went through the same door and the slot reports \
         {} B rather than its {section_bytes} B",
        pooled_bytes(),
    );
}
