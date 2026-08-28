//! Mapbox Terrain-RGB v1.
//!
//! ```text
//! height = -10000 + (R*65536 + G*256 + B) * 0.1
//! ```
//!
//! so, inverting, with `v` the packed 24-bit integer:
//!
//! ```text
//! v = round((height + 10000) * 10)   clamped to 0 ..= 16777215
//! ```
//!
//! ---------------------------------------------------------------------------
//! WHY NOT THE OBVIOUS OPTIONS
//!
//! `gdal_translate -of MBTILES -co ELEVATION_TYPE=terrain-rgb` does NOT encode.
//! It writes `elevation_type=terrain-rgb` into the MBTiles metadata table and
//! nothing else. Measured on GDAL 3.13.3 with the N39 W106 probe, from both a
//! native EPSG:4326 Float32 DEM and a reprojected EPSG:3857 one: the tiles come
//! back 2-band grey+alpha Byte, 700 bytes each, values 0..255 — gdal_translate
//! rescaled Float32 into Byte on the way in and the option never got a chance
//! to act. Exit status 0. Feed the same driver an ALREADY-encoded 3-band RGB
//! and it passes it through correctly, which is what this module produces, so
//! the option is a LABEL for pre-encoded input, not an encoder.
//!
//! `rio rgbify` is the usual answer and is not taken: PyPI 0.4.0 dates from
//! April 2022 and the repository's recent traffic is dependency bots.
//!
//! An earlier design did this inside a GDAL VRT expression pixel function.
//! Those landed in GDAL 3.11.0, and are compiled only under `GDAL_USE_MUPARSER`
//! — so a version check passes on a GDAL that reports 3.13 and the expression
//! still fails at runtime. Packing here needs neither.
//! ---------------------------------------------------------------------------

use std::io::{Read, Write};

use crate::Res;

/// Metres per count of the blue channel.
pub const QUANTUM_M: f64 = 0.1;

/// The lowest height the encoding can carry.
pub const BASE_M: f64 = -10_000.0;

/// The largest packed value, `2^24 - 1`.
pub const MAX_PACKED: u32 = 16_777_215;

/// Pack one height, in metres, to its RGB triple.
///
/// `(h + 10000) * 10`, not `(h + 10000) / 0.1`. The two are the same function
/// in exact arithmetic and different ones in binary: 0.1 is not representable,
/// so the quotient carries a rounding the product does not. Measured over
/// 2,261,416 `f32` heights strided across −500 m to 9000 m, they disagree on
/// 98,304 of them — 4.35%. The product is the spelling the GDAL muparser
/// expression this replaced used, and it is what keeps the two encoders
/// byte-identical.
///
/// `round` is half-away-from-zero, which is also muparser's `rint`; numpy's
/// default is half-to-even and disagrees with it on an exactly-half-integer
/// packed value. Those arise whenever a height is an exact multiple of 0.25 but
/// not of 0.1 — 0.41% of the same sweep, and the two rules part company on half
/// of those. Both land exactly half a quantum from truth, so neither encoder is
/// more correct; what matters is that this one matches the one it replaced.
///
/// NaN packs as the encoding's floor rather than propagating: `NaN as u32` is
/// 0 by saturating cast anyway, and being explicit is what stops that being an
/// accident.
pub fn pack(height: f64) -> [u8; 3] {
    if height.is_nan() {
        return [0, 0, 0];
    }
    let v = ((height - BASE_M) * 10.0)
        .round()
        .clamp(0.0, f64::from(MAX_PACKED)) as u32;
    [(v >> 16) as u8, (v >> 8) as u8, v as u8]
}

/// Recover the height a triple carries.
pub fn unpack(rgb: [u8; 3]) -> f64 {
    let v = (u32::from(rgb[0]) << 16) | (u32::from(rgb[1]) << 8) | u32::from(rgb[2]);
    BASE_M + f64::from(v) * QUANTUM_M
}

/// Pack `count` little-endian `f32` heights from `src` into interleaved RGB on
/// `dst`.
///
/// Little-endian because that is what `gdalwarp -of ENVI` writes on every
/// platform this build runs on; the header it emits alongside says
/// `byte order = 0`, and [`crate::raster`] asserts the file's length rather
/// than trusting the count.
pub fn pack_stream<R: Read, W: Write>(src: &mut R, dst: &mut W, count: u64) -> Res<u64> {
    // 64 Ki pixels a pass: 256 KiB in, 192 KiB out, both inside L2.
    const BATCH: usize = 65_536;
    let mut inbuf = vec![0u8; BATCH * 4];
    let mut outbuf = vec![0u8; BATCH * 3];
    let mut done = 0u64;
    while done < count {
        let n = (count - done).min(BATCH as u64) as usize;
        src.read_exact(&mut inbuf[..n * 4])?;
        for i in 0..n {
            let h = f32::from_le_bytes([
                inbuf[i * 4],
                inbuf[i * 4 + 1],
                inbuf[i * 4 + 2],
                inbuf[i * 4 + 3],
            ]);
            outbuf[i * 3..i * 3 + 3].copy_from_slice(&pack(f64::from(h)));
        }
        dst.write_all(&outbuf[..n * 3])?;
        done += n as u64;
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The round trip is within half a quantum everywhere the encoding reaches.
    #[test]
    fn a_packed_height_survives_within_half_a_quantum() {
        let mut worst = 0.0f64;
        let mut h = BASE_M;
        while h <= 20_000.0 {
            let back = unpack(pack(h));
            worst = worst.max((back - h).abs());
            assert!(
                (back - h).abs() <= QUANTUM_M / 2.0 + 1e-9,
                "{h} m came back as {back} m"
            );
            h += 0.017;
        }
        // Non-triviality: the sweep must actually visit the error's worst case,
        // or "within half a quantum" would be satisfied by an encoder that
        // quantised far more coarsely and never got tested near the edge.
        assert!(
            worst > 0.049,
            "sweep never approached half a quantum: {worst}"
        );
    }

    /// The heights this DEM actually carries: the Dead Sea shore to Everest.
    #[test]
    fn real_elevations_round_trip() {
        for h in [
            -430.5, -100.0, -0.05, 0.0, 0.05, 1.0, 1609.34, 4401.2, 8848.86,
        ] {
            let back = unpack(pack(h));
            assert!((back - h).abs() <= 0.05 + 1e-9, "{h} -> {back}");
        }
    }

    /// The encoding's own boundaries, where a clamp that is off by one shows.
    #[test]
    fn the_encoding_saturates_rather_than_wrapping() {
        assert_eq!(pack(BASE_M), [0, 0, 0]);
        assert_eq!(pack(BASE_M - 1.0), [0, 0, 0]);
        assert_eq!(pack(f64::NEG_INFINITY), [0, 0, 0]);
        let top = BASE_M + f64::from(MAX_PACKED) * QUANTUM_M;
        assert_eq!(pack(top), [255, 255, 255]);
        assert_eq!(pack(top + 1.0), [255, 255, 255]);
        assert_eq!(pack(f64::INFINITY), [255, 255, 255]);
        assert_eq!(pack(f64::NAN), [0, 0, 0]);
    }

    /// Every channel boundary: one count of R is 6553.6 m, so a carry that is
    /// dropped between the digits is a catastrophic error rather than a soft
    /// one. This walks each carry.
    #[test]
    fn the_base_256_carries_are_exact() {
        for v in [
            0u32, 255, 256, 257, 65_535, 65_536, 65_537, 16_777_214, MAX_PACKED,
        ] {
            let h = BASE_M + f64::from(v) * QUANTUM_M;
            let rgb = pack(h);
            let got = (u32::from(rgb[0]) << 16) | (u32::from(rgb[1]) << 8) | u32::from(rgb[2]);
            assert_eq!(got, v, "packed {v} as {rgb:?}");
        }
    }

    /// Ties break away from zero, matching the muparser encoder this replaced.
    ///
    /// A height that is an exact multiple of 0.25 but not of 0.1 puts
    /// `(h + 10000) * 10` exactly on a half-integer. These are the 0.022% of
    /// pixels on which any half-to-even encoder — numpy's `round`, among others
    /// — produces a different byte, each exactly half a quantum from truth.
    #[test]
    fn half_integer_packed_values_round_away_from_zero() {
        // −1696.75 m -> 83032.5. Half-to-even gives 83032 (even); away gives
        // 83033. The packed value proves which rule ran.
        assert_eq!(packed(-1696.75), 83033);
        // −2591.75 m -> 74082.5. Same disagreement, opposite parity of input.
        assert_eq!(packed(-2591.75), 74083);
        // −737.25 m -> 92627.5, where BOTH rules give 92628. The control: it
        // shows the two assertions above turn on the tie rule and not on the
        // arithmetic that produced the half.
        assert_eq!(packed(-737.25), 92628);
    }

    /// The exact `f64` a height packs to, before it is split into bytes.
    fn packed(h: f64) -> u32 {
        let rgb = pack(h);
        (u32::from(rgb[0]) << 16) | (u32::from(rgb[1]) << 8) | u32::from(rgb[2])
    }

    /// Walk the `f32` grid across the heights this DEM actually carries.
    fn sweep(mut visit: impl FnMut(f32)) {
        // Stride 65536 ulps: ~34 k samples, enough to see a 4% effect and fast
        // enough for a unit test.
        let mut h = -500.0f32;
        while h < 0.0 {
            visit(h);
            h = f32::from_bits(h.to_bits().saturating_sub(65_536));
        }
        let mut h = 0.0f32;
        while h <= 9000.0 {
            visit(h);
            h = f32::from_bits(h.to_bits() + 65_536);
        }
    }

    /// The spelling of the scale is load-bearing, not a style choice: `/0.1`
    /// carries a rounding `*10` does not, and it lands on different bytes.
    #[test]
    fn multiplying_by_ten_is_not_dividing_by_a_tenth() {
        let (mut n, mut differ) = (0u32, 0u32);
        sweep(|h| {
            let d = f64::from(h);
            n += 1;
            if (d + 10_000.0) * 10.0 != (d + 10_000.0) / 0.1 {
                differ += 1;
            }
        });
        assert!(
            differ * 100 > n,
            "only {differ} of {n} heights distinguish the two spellings; below 1% \
             the measurement on `pack` is stale"
        );
    }

    /// Half-integer packed values are common enough that the tie rule decides
    /// real bytes, which is why `pack` must break ties the way muparser did.
    #[test]
    fn exact_half_integer_packed_values_are_reached() {
        let (mut n, mut halves) = (0u32, 0u32);
        sweep(|h| {
            n += 1;
            if ((f64::from(h) + 10_000.0) * 10.0).fract() == 0.5 {
                halves += 1;
            }
        });
        assert!(
            halves > 0,
            "no height in {n} reached a tie; the tie-rule assertions above would \
             then be testing nothing"
        );
    }

    #[test]
    fn the_stream_packs_every_pixel_in_order() {
        let heights: Vec<f32> = vec![-430.5, 0.0, 1609.34, 8848.86];
        let mut src: Vec<u8> = Vec::new();
        for h in &heights {
            src.extend_from_slice(&h.to_le_bytes());
        }
        let mut out = Vec::new();
        let n = pack_stream(&mut src.as_slice(), &mut out, heights.len() as u64).unwrap();
        assert_eq!(n, 4);
        assert_eq!(out.len(), 12);
        for (i, h) in heights.iter().enumerate() {
            let rgb = [out[i * 3], out[i * 3 + 1], out[i * 3 + 2]];
            assert_eq!(rgb, pack(f64::from(*h)));
        }
    }

    /// A truncated input is an error, not a short write that GDAL would later
    /// read as a black row.
    #[test]
    fn a_short_stream_fails() {
        let src = vec![0u8; 6];
        let mut out = Vec::new();
        assert!(pack_stream(&mut src.as_slice(), &mut out, 4).is_err());
    }
}
