//! The raymarch's stratification tile: 64 x 64 bytes of **blue noise**.
//!
//! # What this replaces, and why the replacement is not "any other hash"
//!
//! The march offsets its sample comb by a per-pixel fraction of a step. That
//! offset has to be *decorrelated between neighbouring pixels* or the comb is
//! phase-locked to the eye and every iso-`t` shell draws a contour that stays
//! put in screen space while the volume slides beneath it — the "slithering"
//! the 2026-08-09 recording shows. Any hash at all buys that.
//!
//! What the hash is *also* doing, and what picking one badly costs, is decide
//! the **shape of the residual**. The offset's quantisation error lands on the
//! screen as a pattern with the hash's own spatial spectrum, scaled by the
//! per-step opacity quantum. So the hash is not an implementation detail of
//! the jitter; it is the artefact the user sees when the quantum is not small.
//!
//! This tile is here because the two obvious hashes are both wrong, measured
//! rather than argued (2026-08-11, `harness/hatch-diagnosis`; energy below
//! 0.15 cyc/px, the band the eye reads as blotchiness, and the ratio of the
//! largest frequency bin to the median, which is what "a visible pattern"
//! means):
//!
//! | hash | low-band, 1:1 | low-band, after a 2x upscale | peak / median |
//! |---|---|---|---|
//! | interleaved gradient noise | 0.49% | 12.18% | **13975** |
//! | a well-distributed white hash | **7.43%** | **51.82%** | 9.4 |
//! | this tile | 0.00% | 2.45% | 24.5 |
//!
//! * **Interleaved gradient noise**, which the march used until this module,
//!   is a rank-1 lattice: 81.6% of its energy sits in 0.1% of its frequency
//!   bins, as a grating of period 1.86 px at -35 degrees. It is built to be
//!   *filtered* — it is low-discrepancy over a 3x3 neighbourhood, which is why
//!   its low band is clean — and this renderer shows it raw. Shown raw it is a
//!   fine diagonal weave over any region whose gradient is smooth, which is
//!   the artefact reported against the 3D view on 2026-08-11.
//! * **A white hash** deletes the grating and is the worse trade: it has no
//!   coherent peak, but fifteen times the low-frequency energy, which is
//!   mottling in exactly the band the eye is most sensitive to. The half
//!   resolution rungs make that concrete — they blit through a `Linear` 2x
//!   upscale, which is a low-pass, so white noise arrives with *half its
//!   energy* in the visible band.
//!
//! Blue noise is the one that is good at both: no coherent peak to read as
//! structure, and its energy pushed up against Nyquist where both the eye and
//! that upscale attenuate it.
//!
//! # Why it is shipped as bytes, and generated only in the test
//!
//! The tile is an `include_bytes!` of `volume_blue_noise/tile.bin`, and
//! `void_and_cluster` — the generator those 4096 bytes came out of — is still
//! here, under `cfg(test)`, where
//! `the_shipped_bytes_are_the_ones_void_and_cluster_produces` runs it and
//! asserts the two agree byte for byte.
//!
//! It used to be computed at startup, and this section used to say
//! void-and-cluster "runs in well under a millisecond once per
//! `VolumePipelines`". **That was wrong by a factor of ten natively and thirty
//! in a browser.** What hid it is that the `OnceLock` was filled from the
//! *first* [`crate::volume::raymarch::VolumePipelines::upload_volume_at`],
//! which runs on the frame thread — so the cost was paid once, inside the frame
//! that first shows a volume, where a one-off lands on no steady-state figure
//! at all.
//!
//! Measured, `opt-level = 3` and `lto = true`, one fresh process per reading,
//! best of seven on an otherwise idle Ryzen 9 7950X: **9.90 ms**, +276 KiB RSS,
//! 24 minor faults. The same file compiled to wasm32 and run in V8, one fresh
//! instance per reading: **31.4 ms** on the first call — which is what a
//! browser tab actually pays, because a function called once never leaves the
//! baseline (Liftoff) tier. `extreme` is an unconditional linear scan of all
//! 4096 texels and the three phases below make 4420 of those calls.
//!
//! The property tests were the whole reason this was ever computed at runtime —
//! "a blob can only be pinned against itself" — and they are untouched. They
//! still run over what actually ships, which is now the blob; what replaces the
//! runtime generation is one more test rather than one fewer, and a blob
//! checked against its own generator is not a blob nobody can check.
//!
//! # One measured caveat, for whoever regenerates it
//!
//! `gaussian_kernel` calls `f32::exp`, and **`exp` is not bit-identical across
//! libm implementations.** Compiling this file verbatim for four targets and
//! comparing all 169 kernel weights: x86-64 glibc, aarch64 glibc and
//! wasm32-wasip1's wasi-libc agree exactly, and wasm32-unknown-unknown's Rust
//! `libm` — the one the shipped web build uses — differs by **one ULP on 12 of
//! the 169**, the `exp(-8/4.5)` and `exp(-29/4.5)` rings. All four produce
//! these same 4096 bytes, which was checked rather than assumed. But `extreme`
//! settles ties on the lower index and 164 of its 4420 decisions are decided by
//! under 64 ULP, so this construction is not *robust* to a kernel that drifts;
//! it is *measured equal* on every target that ships.
//!
//! That is an argument for the blob rather than against it: bytes make every
//! device march the same tile by construction, which computing it at runtime
//! only happened to do. If the regeneration test ever fails on a host whose
//! libm is a fifth implementation, the shipped tile is not thereby wrong — the
//! generator has been handed different weights — and the answer is to record
//! that here, not to re-bake against whichever machine ran the suite.

/// The tile's edge, in texels. A power of two so the shader's wrap is a mask
/// rather than a modulo — `textureLoad` takes signed coordinates and WGSL's
/// `%` follows the sign of its left operand, so a mask is the one that cannot
/// index backwards off the tile.
pub const BLUE_NOISE_EDGE: u32 = 64;

/// Texels in the tile.
const TEXELS: usize = (BLUE_NOISE_EDGE * BLUE_NOISE_EDGE) as usize;

/// The standard deviation of the void-and-cluster filter, in texels.
///
/// Ulichney's own figure. It sets the scale the algorithm calls "clustered":
/// much smaller and the filter cannot see far enough to break up a cluster,
/// much larger and it smooths every candidate site to the same value and the
/// choice of where to place the next point stops meaning anything.
#[cfg(test)]
const FILTER_SIGMA: f32 = 1.5;

/// Half-width of the filter's support. At 4 sigma the Gaussian is under 4e-4
/// of its peak, so the truncation is far below the differences being ranked.
#[cfg(test)]
const FILTER_RADIUS: i32 = 6;

/// The fraction of the tile the initial binary pattern starts filled at.
/// Ulichney's 10%: enough points that the relaxation has something to relax,
/// few enough that the voids are large.
#[cfg(test)]
const INITIAL_FILL: usize = TEXELS / 10;

/// The tile as it ships: one byte per texel, row-major, `TEXELS` of them.
///
/// The array type is the length check. `include_bytes!` yields a
/// `&'static [u8; N]` with `N` taken from the file, so a truncated or padded
/// `tile.bin` is a compile error naming both lengths rather than a texture
/// upload that reads past its rows.
const TILE: &[u8; TEXELS] = include_bytes!("volume_blue_noise/tile.bin");

/// The blue noise tile, one byte per texel, row-major.
///
/// The same 4096 bytes on every platform and every run — now by construction
/// rather than by a determinism argument, which is what the module doc's
/// caveat on `exp` is about. That matters because the jitter is meant to be
/// *static*; see the shader's note on why an animated jitter is the same
/// artefact at one remove.
///
/// Free at run time. The texture it fills is still created per grid upload —
/// 4 KiB beside a grid of megabytes — and this is now a pointer into `.rodata`
/// rather than the ~16.8 M-step scan that used to sit in the first frame to
/// show a volume.
///
/// Each byte is its texel's rank scaled to `0..256`: 4096 ranks over 256 levels
/// is sixteen texels a level. The *ordering* is what carries the blue spectrum,
/// and the offset it feeds is a fraction of one march step, which eight bits
/// resolves far past the eight-bit colour it ends up in.
pub fn blue_noise_tile() -> &'static [u8] {
    TILE
}

/// What `tile.bin` has to contain: void-and-cluster's ranks, scaled to bytes.
///
/// This is the body [`blue_noise_tile`] used to run at startup, moved here
/// intact so the test regenerates *exactly* what the tile was before it was
/// baked, conversion included, rather than a restatement of it.
#[cfg(test)]
fn generated_tile() -> Vec<u8> {
    void_and_cluster()
        .iter()
        .map(|&rank| (rank * 256 / TEXELS) as u8)
        .collect()
}

/// Every texel's rank in `0..TEXELS`, lowest where the point was placed first.
///
/// The three phases are Ulichney's. What they share is one primitive: find the
/// texel whose neighbourhood is most crowded (the "tightest cluster") or least
/// crowded (the "largest void"), against a Gaussian-filtered copy of the
/// current point set that is maintained *incrementally* — placing or lifting a
/// point adds or subtracts one kernel, which is what keeps a run of this down
/// to the ~10 ms the module doc quotes rather than the minutes the naive
/// re-filter would take.
#[cfg(test)]
fn void_and_cluster() -> Vec<usize> {
    let kernel = gaussian_kernel();
    let mut placed = vec![false; TEXELS];
    let mut energy = vec![0.0f32; TEXELS];

    // A fixed-seed LCG rather than `rand`: this wants to be reproducible and
    // dependency-free far more than it wants to be statistically excellent,
    // and every trace of the seed is relaxed away by the loop below.
    let mut state: u32 = 0x9E37_79B9;
    let mut next = || {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (state >> 8) as usize
    };
    let mut count = 0;
    while count < INITIAL_FILL {
        let at = next() % TEXELS;
        if !placed[at] {
            place(&mut placed, &mut energy, &kernel, at, true);
            count += 1;
        }
    }

    // Phase 0: relax the random start until moving the tightest cluster into
    // the largest void puts it back where it came from — the fixed point that
    // says the pattern is as homogeneous as this operation can make it.
    loop {
        let tightest = extreme(&energy, &placed, true, true);
        place(&mut placed, &mut energy, &kernel, tightest, false);
        let largest = extreme(&energy, &placed, false, false);
        if largest == tightest {
            place(&mut placed, &mut energy, &kernel, tightest, true);
            break;
        }
        place(&mut placed, &mut energy, &kernel, largest, true);
    }

    let initial = placed.clone();
    let initial_energy = energy.clone();
    let mut ranks = vec![0usize; TEXELS];

    // Phase 1: lift the initial points out one at a time, tightest cluster
    // first, numbering them downwards. The last point left is rank 0.
    for rank in (0..INITIAL_FILL).rev() {
        let tightest = extreme(&energy, &placed, true, true);
        place(&mut placed, &mut energy, &kernel, tightest, false);
        ranks[tightest] = rank;
    }

    // Phases 2 and 3: from the initial pattern again, fill the largest void
    // each time, numbering upwards. One loop rather than Ulichney's two
    // because the split only exists to swap the sense of the filter at the
    // halfway point, and at this tile size the unsplit form measures the same
    // spectrum — which `tests` checks rather than assumes.
    placed = initial;
    energy = initial_energy;
    for (rank, _) in (INITIAL_FILL..TEXELS).enumerate() {
        let largest = extreme(&energy, &placed, false, false);
        place(&mut placed, &mut energy, &kernel, largest, true);
        ranks[largest] = INITIAL_FILL + rank;
    }
    ranks
}

/// The index of the most or least crowded texel among those that are (or are
/// not) already placed.
///
/// `want_placed` picks which set is searched; `maximum` picks which end. Ties
/// go to the lower index, which is what makes the whole construction
/// reproducible.
#[cfg(test)]
fn extreme(energy: &[f32], placed: &[bool], want_placed: bool, maximum: bool) -> usize {
    let mut best = usize::MAX;
    let mut best_energy = if maximum {
        f32::NEG_INFINITY
    } else {
        f32::INFINITY
    };
    for (at, &value) in energy.iter().enumerate() {
        if placed[at] != want_placed {
            continue;
        }
        if (maximum && value > best_energy) || (!maximum && value < best_energy) {
            best_energy = value;
            best = at;
        }
    }
    debug_assert!(
        best != usize::MAX,
        "no candidate texel in the requested set"
    );
    best
}

/// Add or remove a point, and the kernel it contributes to its neighbourhood.
#[cfg(test)]
fn place(placed: &mut [bool], energy: &mut [f32], kernel: &[f32], at: usize, on: bool) {
    placed[at] = on;
    let edge = BLUE_NOISE_EDGE as i32;
    let (cx, cy) = ((at as i32) % edge, (at as i32) / edge);
    let span = 2 * FILTER_RADIUS + 1;
    for dy in -FILTER_RADIUS..=FILTER_RADIUS {
        for dx in -FILTER_RADIUS..=FILTER_RADIUS {
            // Wrapped, so the tile is toroidal and therefore seamless when the
            // shader repeats it across the frame.
            let x = (cx + dx).rem_euclid(edge);
            let y = (cy + dy).rem_euclid(edge);
            let weight = kernel[((dy + FILTER_RADIUS) * span + (dx + FILTER_RADIUS)) as usize];
            let target = &mut energy[(y * edge + x) as usize];
            *target += if on { weight } else { -weight };
        }
    }
}

/// The truncated Gaussian, row-major over `(2 * FILTER_RADIUS + 1)^2`.
///
/// The one place a platform's libm reaches this construction — see the module
/// doc's caveat, which measured the four implementations that matter.
#[cfg(test)]
fn gaussian_kernel() -> Vec<f32> {
    let span = 2 * FILTER_RADIUS + 1;
    let mut kernel = Vec::with_capacity((span * span) as usize);
    for dy in -FILTER_RADIUS..=FILTER_RADIUS {
        for dx in -FILTER_RADIUS..=FILTER_RADIUS {
            let squared = (dx * dx + dy * dy) as f32;
            kernel.push((-squared / (2.0 * FILTER_SIGMA * FILTER_SIGMA)).exp());
        }
    }
    kernel
}

#[cfg(test)]
#[path = "volume_blue_noise/tests.rs"]
mod tests;
