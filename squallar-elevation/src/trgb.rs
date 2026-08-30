//! Mapbox Terrain-RGB v1, decode side only.
//!
//! ```text
//! height = -10000 + (R*65536 + G*256 + B) * 0.1
//! ```
//!
//! **Re-spelled, not shared.** `tools/squallar-terrain` is a separate Cargo
//! workspace by design (its own `[workspace]` key; the root manifest has no
//! `exclude` and must not gain one), so it cannot be `use`d from here. Three
//! things hold the two copies together, all in `tests/resample_oracle.rs`:
//! `the_constants_match_the_builders_source_text` reads the builder's
//! `trgb.rs` off disk and compares the text, `the_base_two_hundred_and_fifty_six_carries_are_exact`
//! walks the nine channel-boundary values, and
//! `the_committed_real_tile_decodes_to_its_recorded_heights` decodes a tile the
//! builder actually produced.
//!
//! **[`unpack`] only — never a `pack`.** Nothing in the app encodes a height,
//! so a `pack` here would be a second definition with no caller to notice it
//! drifting from the builder's. The builder's own `pack` carries a tie rule and
//! a multiply-don't-divide spelling that were measured; a copy of it here would
//! silently stop matching them.

/// Metres per count of the blue channel.
pub const QUANTUM_M: f64 = 0.1;

/// The lowest height the encoding can carry.
pub const BASE_M: f64 = -10_000.0;

/// The largest packed value, `2^24 - 1`.
pub const MAX_PACKED: u32 = 16_777_215;

/// Recover the height a triple carries.
pub fn unpack(rgb: [u8; 3]) -> f64 {
    let v = (u32::from(rgb[0]) << 16) | (u32::from(rgb[1]) << 8) | u32::from(rgb[2]);
    BASE_M + f64::from(v) * QUANTUM_M
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_encodings_two_ends_decode_to_the_range_it_advertises() {
        assert_eq!(unpack([0, 0, 0]), BASE_M);
        assert_eq!(
            unpack([255, 255, 255]),
            BASE_M + f64::from(MAX_PACKED) * QUANTUM_M
        );
    }
}
