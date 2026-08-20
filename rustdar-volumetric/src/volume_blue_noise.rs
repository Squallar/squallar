//! The raymarch's stratification tile: 64 x 64 bytes of **blue noise**.

/// The tile's edge, in texels. A power of two so the shader's wrap is a mask
/// rather than a modulo — `textureLoad` takes signed coordinates and WGSL's
/// `%` follows the sign of its left operand, so a mask is the one that cannot
/// index backwards off the tile.
pub const BLUE_NOISE_EDGE: u32 = 64;

/// Texels in the tile.
const TEXELS: usize = (BLUE_NOISE_EDGE * BLUE_NOISE_EDGE) as usize;

/// The standard deviation of the void-and-cluster filter, in texels.
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
const TILE: &[u8; TEXELS] = include_bytes!("volume_blue_noise/tile.bin");

/// The blue noise tile, one byte per texel, row-major.
pub fn blue_noise_tile() -> &'static [u8] {
    TILE
}

/// What `tile.bin` has to contain: void-and-cluster's ranks, scaled to bytes.
#[cfg(test)]
fn generated_tile() -> Vec<u8> {
    void_and_cluster()
        .iter()
        .map(|&rank| (rank * 256 / TEXELS) as u8)
        .collect()
}

/// Every texel's rank in `0..TEXELS`, lowest where the point was placed first.
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
    // each time, numbering upwards.
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
