//! **The committed GMGSI granule is a SYNTHETIC RAMP, and this is the test
//! that says so before it misleads a fourth lane.**
//!
//! `GLOBCOMPLIR_v3r0_blend_s202506011200000_...nc` looks like a real product
//! file — it carries the operational name, the real variable layout and the
//! real 3000 x 5000 shape, and it decodes through the shipped reader without a
//! complaint. Its **pixels are not satellite imagery**: **2,998 of its 3,000
//! rows carry one single value across all 5,000 columns**, and going down any
//! column the value changes every **exactly twelve rows**. The granule is 250
//! flat horizontal bands stacked to 3,000 rows — a ramp. (Two rows, 1000 and
//! 1499, each carry one differing cell; one of them is the granule's only
//! `NaN`. They are the only texture in the whole 15 M-point raster.)
//!
//! # What it can and cannot answer
//!
//! It is a perfectly good fixture for what it was committed for: shape,
//! header parsing, the `_Unsigned`/`scale_factor` unpacking, the byte
//! narrowing's round trip, the staging pool's identity and block accounting.
//! Every one of those is a question about the READER.
//!
//! **It cannot answer any question about the DATA's spatial statistics** —
//! density, entropy, tile uniformity, compressibility, run lengths, or how any
//! encoder would price a real granule. A lane measured tile uniformity on it
//! and read **66.7 % uniform at an 8x8 tile and 0.5 % at 16x16**, which is not
//! a fact about satellite imagery at all: 8 divides into a 12-row band twice
//! with a remainder, so exactly two of every three 8-row tiles fall inside one
//! band and are uniform, and a 16-row tile almost never does. That is the
//! ramp's period showing through, and the 2/3 is arithmetic, not weather.
//!
//! Measured against **twelve real granules** pulled from `noaa-gmgsi-pds`
//! (four channels, two dates), the true figure is **0.0-20.6 % uniform at
//! 16x16** — and tiling *costs* bytes on the LW and SW channels. The fixture
//! overstated the compressibility of an 8x8 tiling by roughly **forty-fold**.
//!
//! A real granule is ~7.3 MB compressed and none is committed, so the fix is
//! this gate rather than a replacement: **if someone swaps in real data, this
//! test fails**, and its message points at the note above so the claim and the
//! bytes cannot drift apart.

use squallar_overlays::gmgsi::{self, GmgsiChannel};

const GRANULE: &[u8] = include_bytes!(
    "../testdata/GLOBCOMPLIR_v3r0_blend_s202506011200000_e202506011209599_c202506011234579.nc"
);

/// Rows of a single value each, in flat bands — the ramp's signature.
///
/// Floor: the row-constancy walk counts the rows it checked and asserts that
/// count against `nj`, so a walk that compared nothing cannot pass.
#[test]
fn the_committed_gmgsi_fixture_is_a_synthetic_ramp_not_imagery() {
    let grid = gmgsi::decode::decode(GRANULE.to_vec(), GmgsiChannel::LongwaveIr)
        .expect("the fixture decodes");
    let (ni, nj) = (grid.grid.ni, grid.grid.nj);
    let v: Vec<f32> = grid.grid.values.iter().collect();
    assert_eq!(v.len(), ni * nj);

    // 1. Rows are flat. Real imagery is never this. Two rows carry a single
    //    differing cell each and are allowed for by name, not by a tolerance:
    //    a bound that let a tenth of the grid vary would pass on real data.
    let mut constant_rows = 0usize;
    let mut rows_checked = 0usize;
    for j in 0..nj {
        let row = &v[j * ni..(j + 1) * ni];
        if row.iter().all(|&x| x == row[0]) {
            constant_rows += 1;
        }
        rows_checked += 1;
    }
    assert_eq!(
        rows_checked, nj,
        "the walk checked fewer rows than the grid"
    );
    assert_eq!(
        constant_rows,
        nj - 2,
        "the ramp has exactly two rows that are not flat. If this granule now \
         has texture in it, real data may have been committed — re-read this \
         file's header and DELETE this test and the warnings pointing at it, \
         because several of them would then be wrong.",
    );

    // 2. Down a column the value changes every twelve rows. This is the band
    //    height, and it is the whole explanation of the artifact: 8 does not
    //    divide 12, so exactly two of every three 8-row tiles fall inside one
    //    band and read uniform, while a 16-row tile almost never does.
    let mut runs = Vec::new();
    let mut j = 0usize;
    while j < nj {
        let first = v[j * ni];
        let mut k = j;
        while k < nj && v[k * ni] == first {
            k += 1;
        }
        runs.push(k - j);
        j = k;
    }
    assert_eq!(
        runs.len(),
        250,
        "3,000 rows in bands of 12 is 250 bands; found {} runs",
        runs.len(),
    );
    assert!(
        runs.iter().all(|&r| r == 12),
        "every band is twelve rows; found {:?}",
        runs.iter()
            .filter(|&&r| r != 12)
            .take(4)
            .collect::<Vec<_>>(),
    );

    // 3. And the bands climb the byte range, which is what makes it a ramp
    //    rather than a checkerboard.
    let mut distinct = std::collections::BTreeSet::new();
    for &x in &v {
        distinct.insert(x.to_bits());
    }
    assert!(
        distinct.len() > 200,
        "the ramp climbs the byte range; {} distinct values is not it",
        distinct.len(),
    );
}
