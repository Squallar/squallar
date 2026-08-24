/// `"#RRGGBB"` or `"RRGGBB"`. Falls back to grey rather than failing: this
/// runs on network data inside the outlook fetch task, where a panic is
/// swallowed and the overlay wedges in "fetching".
pub fn parse_hex_color(hex: &str, alpha: u8) -> [u8; 4] {
    let hex = hex.trim_start_matches('#');
    if hex.len() < 6 {
        return [128, 128, 128, alpha];
    }
    // `get`, not `[..]`: the ranges are byte offsets, and indexing panics when
    // one lands inside a multi-byte character ("€€" is six bytes, so it passes
    // the length gate above).
    let r = hex
        .get(0..2)
        .and_then(|s| u8::from_str_radix(s, 16).ok())
        .unwrap_or(128);
    let g = hex
        .get(2..4)
        .and_then(|s| u8::from_str_radix(s, 16).ok())
        .unwrap_or(128);
    let b = hex
        .get(4..6)
        .and_then(|s| u8::from_str_radix(s, 16).ok())
        .unwrap_or(128);
    [r, g, b, alpha]
}

use super::discussion::MdType;

pub fn md_fill_color(md_type: &MdType) -> [u8; 4] {
    match md_type {
        MdType::Convective => [255, 180, 50, 60],
        MdType::WinterWeather => [100, 180, 255, 60],
        MdType::Other => [180, 180, 180, 60],
    }
}

pub fn md_stroke_color(md_type: &MdType) -> [u8; 4] {
    match md_type {
        MdType::Convective => [255, 180, 50, 200],
        MdType::WinterWeather => [100, 180, 255, 200],
        MdType::Other => [180, 180, 180, 200],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_hex_color() {
        assert_eq!(parse_hex_color("#FF0000", 255), [255, 0, 0, 255]);
        assert_eq!(parse_hex_color("#00FF00", 128), [0, 255, 0, 128]);
        assert_eq!(parse_hex_color("0000FF", 200), [0, 0, 255, 200]);
    }

    #[test]
    fn test_parse_hex_color_invalid() {
        assert_eq!(parse_hex_color("bad", 128), [128, 128, 128, 128]);
    }

    #[test]
    fn a_multi_byte_fill_string_falls_back_to_grey_instead_of_panicking() {
        assert_eq!(parse_hex_color("€€", 128), [128, 128, 128, 128]);
        assert_eq!(parse_hex_color("#€€", 64), [128, 128, 128, 64]);
        assert_eq!(parse_hex_color("ff€€", 255), [255, 128, 128, 255]);
    }
}
