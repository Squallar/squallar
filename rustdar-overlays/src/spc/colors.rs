/// Parse a hex color string like "#RRGGBB" into [r, g, b, a].
/// Returns a default grey on parse failure.
pub fn parse_hex_color(hex: &str, alpha: u8) -> [u8; 4] {
    let hex = hex.trim_start_matches('#');
    if hex.len() < 6 {
        return [128, 128, 128, alpha];
    }
    let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(128);
    let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(128);
    let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(128);
    [r, g, b, alpha]
}

use super::discussion::MdType;

/// Fill color for an SPC Mesoscale Discussion polygon based on topic.
pub fn md_fill_color(md_type: &MdType) -> [u8; 4] {
    match md_type {
        MdType::Convective => [255, 180, 50, 60],
        MdType::WinterWeather => [100, 180, 255, 60],
        MdType::Other => [180, 180, 180, 60],
    }
}

/// Stroke color for an SPC Mesoscale Discussion polygon based on topic.
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
}
