//! What the tile has to be, rather than what it happens to contain — and,
//! once, that what ships is what the generator says.

use super::*;

/// **The shipped bytes are void-and-cluster's, and nothing else's.**
#[test]
fn the_shipped_bytes_are_the_ones_void_and_cluster_produces() {
    let generated = generated_tile();
    assert_eq!(
        generated.len(),
        TEXELS,
        "void-and-cluster produced {} bytes, not the {TEXELS} a {BLUE_NOISE_EDGE}-square tile is",
        generated.len(),
    );
    if generated.as_slice() == blue_noise_tile() {
        return;
    }
    let differing = generated
        .iter()
        .zip(blue_noise_tile())
        .filter(|(a, b)| a != b)
        .count();
    let first = generated
        .iter()
        .zip(blue_noise_tile())
        .position(|(a, b)| a != b)
        .expect("the slices differ, so some index does");
    panic!(
        "tile.bin is not what void_and_cluster() produces: {differing} of {TEXELS} bytes differ, \
         first at texel {first} ({} shipped against {} generated). Either the generator changed \
         and tile.bin was not rebaked, or this host's libm is a fifth implementation — see the \
         module doc before deciding which.",
        blue_noise_tile()[first],
        generated[first],
    );
}

/// The tile as `f64` in 0..1, which is what the shader's `textureLoad` sees
/// after the hardware's unorm decode.
fn normalised() -> Vec<f64> {
    blue_noise_tile()
        .iter()
        .map(|&byte| f64::from(byte) / 255.0)
        .collect()
}

/// Interleaved gradient noise over the same tile, for the tests that have to
/// show their threshold rejects the hash this module replaced.
fn ign_tile() -> Vec<f64> {
    let edge = BLUE_NOISE_EDGE as usize;
    let mut out = Vec::with_capacity(TEXELS);
    for y in 0..edge {
        for x in 0..edge {
            let inner = (x as f64 + 0.5) * 0.06711056 + (y as f64 + 0.5) * 0.00583715;
            out.push((52.9829189 * inner.fract()).fract());
        }
    }
    out
}

/// Power per frequency bin, by a separable DFT — 64-point transforms along the
/// rows and then the columns, which is ~500k operations rather than the 16.7M
/// the direct double sum would take in a debug build.
fn power_spectrum(values: &[f64]) -> Vec<f64> {
    let n = BLUE_NOISE_EDGE as usize;
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    let centred: Vec<f64> = values.iter().map(|v| v - mean).collect();
    let tau = std::f64::consts::TAU;

    // Rows first: `rows[y][kx]`, complex.
    let mut rows = vec![(0.0f64, 0.0f64); n * n];
    for y in 0..n {
        for kx in 0..n {
            let (mut re, mut im) = (0.0, 0.0);
            for x in 0..n {
                let angle = -tau * (kx * x) as f64 / n as f64;
                let sample = centred[y * n + x];
                re += sample * angle.cos();
                im += sample * angle.sin();
            }
            rows[y * n + kx] = (re, im);
        }
    }
    // Then the columns of that.
    let mut power = vec![0.0f64; n * n];
    for kx in 0..n {
        for ky in 0..n {
            let (mut re, mut im) = (0.0, 0.0);
            for y in 0..n {
                let angle = -tau * (ky * y) as f64 / n as f64;
                let (sre, sim) = rows[y * n + kx];
                re += sre * angle.cos() - sim * angle.sin();
                im += sre * angle.sin() + sim * angle.cos();
            }
            power[ky * n + kx] = re * re + im * im;
        }
    }
    power
}

/// Signed frequency of bin `k`, in cycles per texel.
fn frequency(k: usize) -> f64 {
    let n = BLUE_NOISE_EDGE as usize;
    let signed = if k <= n / 2 {
        k as i64
    } else {
        k as i64 - n as i64
    };
    signed as f64 / n as f64
}

/// The share of the tile's energy below `cut` cycles per texel — the band a
/// viewer reads as blotches, and the band the half-resolution rungs' 2x
/// `Linear` upscale keeps while attenuating everything above it.
fn low_band_percent(values: &[f64], cut: f64) -> f64 {
    let n = BLUE_NOISE_EDGE as usize;
    let power = power_spectrum(values);
    let mut low = 0.0;
    let mut total = 0.0;
    for ky in 0..n {
        for kx in 0..n {
            if kx == 0 && ky == 0 {
                continue;
            }
            let bin = power[ky * n + kx];
            total += bin;
            if frequency(kx).hypot(frequency(ky)) < cut {
                low += bin;
            }
        }
    }
    100.0 * low / total
}

/// The largest frequency bin over the median one. A hash with a periodic
/// structure — a lattice, a grating, anything that repeats — concentrates its
/// energy into a few bins and sends this enormous; noise leaves it small.
fn peak_over_median(values: &[f64]) -> f64 {
    let power = power_spectrum(values);
    let mut bins: Vec<f64> = power
        .iter()
        .enumerate()
        .filter(|(at, _)| *at != 0)
        .map(|(_, &p)| p)
        .collect();
    bins.sort_by(|a, b| a.partial_cmp(b).expect("no NaN in a power spectrum"));
    let median = bins[bins.len() / 2];
    let peak = *bins.last().expect("a non-empty spectrum");
    peak / median
}

/// Pearson correlation between each texel and its neighbour one step along
/// `(dx, dy)`, wrapped.
fn neighbour_correlation(values: &[f64], dx: i32, dy: i32) -> f64 {
    let n = BLUE_NOISE_EDGE as i32;
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    let (mut covariance, mut variance) = (0.0, 0.0);
    for y in 0..n {
        for x in 0..n {
            let here = values[(y * n + x) as usize] - mean;
            let ox = (x + dx).rem_euclid(n);
            let oy = (y + dy).rem_euclid(n);
            let there = values[(oy * n + ox) as usize] - mean;
            covariance += here * there;
            variance += here * here;
        }
    }
    covariance / variance
}

#[test]
fn the_tile_is_a_uniform_distribution() {
    // Sixteen texels per byte value, exactly — the tile is a permutation of the
    // ranks, so the offsets it hands the march are uniform over the step.
    let mut histogram = [0usize; 256];
    for &byte in blue_noise_tile() {
        histogram[byte as usize] += 1;
    }
    let expected = TEXELS / 256;
    for (value, &count) in histogram.iter().enumerate() {
        assert_eq!(
            count, expected,
            "byte {value} appears {count} times, not {expected}: the tile is not a permutation \
             of the ranks and the jitter is no longer uniform over the step",
        );
    }
}

#[test]
fn neighbouring_texels_are_decorrelated() {
    // The property the jitter exists for.
    let tile = normalised();
    for (dx, dy) in [(1, 0), (0, 1), (1, 1)] {
        let correlation = neighbour_correlation(&tile, dx, dy);
        assert!(
            correlation < -0.05,
            "neighbour correlation along ({dx},{dy}) is {correlation:.3}, not negative: the \
             jitter has stopped decorrelating adjacent pixels and the screen-locked banding \
             is back",
        );
    }
    // The mistake this rejects, made concrete.
    let n = BLUE_NOISE_EDGE as usize;
    let wave: Vec<f64> = (0..TEXELS)
        .map(|at| (std::f64::consts::TAU * (at % n) as f64 / n as f64).sin())
        .collect();
    assert!(
        neighbour_correlation(&wave, 1, 0) > 0.99,
        "the correlation measure itself is broken: one wave across the tile must correlate at \
         nearly 1, and it is the shape a screen-locked jitter would take",
    );
}

#[test]
fn no_frequency_bin_dominates_the_tile() {
    // The test that rejects a return to interleaved gradient noise, or to any
    // other lattice: a periodic hash puts its energy in a handful of bins and
    // the eye reads those as a weave.
    let measured = peak_over_median(&normalised());
    assert!(
        measured < 200.0,
        "the tile's largest frequency bin is {measured:.0}x its median: some periodic structure \
         has entered the hash, and a periodic hash draws a visible weave over every smooth \
         gradient in the volume",
    );
    let lattice = peak_over_median(&ign_tile());
    assert!(
        lattice > 1000.0,
        "interleaved gradient noise measured {lattice:.0}x here, so this threshold no longer \
         demonstrably rejects the hash it was written to reject",
    );
}

#[test]
fn the_tiles_energy_sits_above_the_visible_band() {
    // What makes it *blue*, and the reason a plain white hash is not an
    // acceptable substitute: the eye is most sensitive at low spatial
    // frequency, and the half-resolution rungs blit through a 2x `Linear`
    // upscale that keeps the low band and attenuates the high one.
    let measured = low_band_percent(&normalised(), 0.15);
    assert!(
        measured < 1.0,
        "{measured:.2}% of the tile's energy is below 0.15 cycles per texel; blue noise puts \
         almost none there, and whatever is in this tile will read as mottling once the half \
         resolution rungs upscale it",
    );
}

#[test]
fn the_tile_wraps_without_a_seam() {
    // The shader repeats this tile across the whole frame, so the join has to
    // be as unremarkable as the interior — void-and-cluster's filter is wrapped
    // for exactly this.
    let tile = normalised();
    let n = BLUE_NOISE_EDGE as usize;
    let mut across = 0.0;
    let mut interior = 0.0;
    for y in 0..n {
        across += (tile[y * n + (n - 1)] - tile[y * n]).abs();
        interior += (tile[y * n + n / 2] - tile[y * n + n / 2 + 1]).abs();
    }
    across /= n as f64;
    interior /= n as f64;
    assert!(
        (across - interior).abs() < 0.12,
        "the step across the wrap averages {across:.3} against {interior:.3} inside the tile: \
         the tile is not toroidal and repeating it will draw a seam every {n} pixels",
    );
}
