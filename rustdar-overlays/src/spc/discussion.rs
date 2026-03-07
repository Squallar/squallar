use crate::types::{GeoPolygon, GeoPolygonRing};

/// Broad classification of a Mesoscale Discussion topic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MdType {
    /// Severe thunderstorms, tornadoes, convective hazards.
    Convective,
    /// Snow, ice, freezing rain, winter weather.
    WinterWeather,
    /// Anything else (e.g. fire weather, heavy rain).
    Other,
}

impl std::fmt::Display for MdType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MdType::Convective => write!(f, "Convective"),
            MdType::WinterWeather => write!(f, "Winter Weather"),
            MdType::Other => write!(f, "Other"),
        }
    }
}

/// A parsed SPC Mesoscale Discussion.
#[derive(Debug, Clone)]
pub struct SpcDiscussion {
    /// MD number (e.g. 153).
    pub number: u32,
    /// Title from the RSS feed (e.g. "Mesoscale Discussion #0153").
    pub title: String,
    /// Full discussion text body.
    pub text: String,
    /// URL to the SPC discussion page.
    pub link: String,
    /// Topic classification derived from the text.
    pub md_type: MdType,
    /// Geographic polygon enclosing the discussion area.
    pub polygon: GeoPolygon,
    /// The "CONCERNING..." line, if present.
    pub concerning: Option<String>,
}

/// Classify an MD by scanning the discussion text for topic keywords.
pub fn classify_md_type(text: &str) -> MdType {
    let upper = text.to_uppercase();
    if upper.contains("SEVERE THUNDERSTORM")
        || upper.contains("TORNADO")
        || upper.contains("CONVECTIVE")
        || upper.contains("SUPERCELL")
        || upper.contains("SEVERE WEATHER")
        || upper.contains("HAIL")
        || upper.contains("WIND DAMAGE")
    {
        MdType::Convective
    } else if upper.contains("WINTER")
        || upper.contains("SNOW")
        || upper.contains("ICE")
        || upper.contains("FREEZING")
        || upper.contains("BLIZZARD")
        || upper.contains("SLEET")
    {
        MdType::WinterWeather
    } else {
        MdType::Other
    }
}

/// Parse the `LAT...LON` polygon block from a Mesoscale Discussion text.
///
/// The SPC embeds polygon coordinates in a specific format:
/// ```text
///    LAT...LON   35179718 34889754 34449768 34209745 34189682
///    34249641 34639588 35149575 35449600 35459680 35179718
/// ```
///
/// Each whitespace-separated token is 7-8 digits encoding lat×100 and lon×100.
/// The latitude portion is the first 4 digits (or 3 if the token is 7 digits),
/// the longitude portion is the remaining 4 digits. Longitude is negated
/// (Western Hemisphere implied).
///
/// Returns `None` if no valid polygon could be extracted.
pub fn parse_lat_lon_polygon(text: &str) -> Option<GeoPolygonRing> {
    // Find the LAT...LON marker
    let marker_pos = text.find("LAT...LON")?;
    let after_marker = &text[marker_pos + "LAT...LON".len()..];

    // Collect all numeric tokens after the marker until we hit a non-numeric line
    let mut coords: Vec<(f64, f64)> = Vec::new();

    for line in after_marker.lines() {
        let trimmed = line.trim();
        // Stop at empty lines or lines that look like prose/headers
        // (contain alphabetic chars that aren't part of coordinate data)
        let has_coords = trimmed.split_whitespace().any(|tok| {
            tok.len() >= 7 && tok.len() <= 8 && tok.chars().all(|c| c.is_ascii_digit())
        });
        if !has_coords && !trimmed.is_empty() {
            break;
        }
        if trimmed.is_empty() {
            // Allow one empty line within the block, but stop at the second
            if coords.is_empty() {
                continue;
            }
            break;
        }

        for token in trimmed.split_whitespace() {
            if let Some(pair) = parse_coord_token(token) {
                coords.push(pair);
            }
        }
    }

    if coords.len() < 3 {
        return None;
    }

    // Close the ring if not already closed
    if coords.first() != coords.last() {
        let first = coords[0];
        coords.push(first);
    }

    Some(coords)
}

/// Parse a single coordinate token like "35179718" into (lat, lon).
///
/// Format: first 4 digits = lat × 100, last 4 digits = lon × 100.
/// For 7-digit tokens: first 3 digits = lat × 100, last 4 digits = lon × 100.
/// Longitude is negated (Western Hemisphere).
fn parse_coord_token(token: &str) -> Option<(f64, f64)> {
    if !token.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let len = token.len();
    if len < 7 || len > 8 {
        return None;
    }

    let split = len - 4; // lat portion is everything except last 4 digits
    let lat_raw: f64 = token[..split].parse::<f64>().ok()?;
    let lon_raw: f64 = token[split..].parse::<f64>().ok()?;

    let lat = lat_raw / 100.0;
    let lon = -(lon_raw / 100.0); // Western Hemisphere

    // Basic sanity checks for CONUS coordinates
    if lat < 15.0 || lat > 60.0 || lon > -50.0 || lon < -140.0 {
        return None;
    }

    Some((lat, lon))
}

/// Extract the "CONCERNING..." line from the discussion text.
fn extract_concerning(text: &str) -> Option<String> {
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("CONCERNING...") {
            return Some(rest.trim().to_string());
        }
        if let Some(rest) = trimmed.strip_prefix("Concerning...") {
            return Some(rest.trim().to_string());
        }
    }
    None
}

/// Extract the MD number from the title string.
/// Expects format like "Mesoscale Discussion #0153" or similar.
fn extract_md_number(title: &str) -> Option<u32> {
    // Look for # followed by digits
    if let Some(hash_pos) = title.find('#') {
        let after = &title[hash_pos + 1..];
        let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        return digits.parse().ok();
    }
    // Fallback: look for any multi-digit number
    let mut num_str = String::new();
    for ch in title.chars().rev() {
        if ch.is_ascii_digit() {
            num_str.insert(0, ch);
        } else if !num_str.is_empty() {
            break;
        }
    }
    if num_str.is_empty() {
        None
    } else {
        num_str.parse().ok()
    }
}

/// Parse the SPC Mesoscale Discussion RSS XML feed into discussion structs.
///
/// The RSS feed at `https://www.spc.noaa.gov/products/spcmdrss.xml` returns
/// items with `<title>`, `<link>`, and `<description>` containing the full text
/// including `LAT...LON` polygon coordinates.
pub fn parse_md_rss(xml: &str) -> Result<Vec<SpcDiscussion>, String> {
    // Simple XML parsing without a full XML library.
    // The RSS format is straightforward: <item> elements with child <title>,
    // <link>, and <description> elements.
    let mut discussions = Vec::new();

    let mut remaining = xml;
    while let Some(item_start) = remaining.find("<item>") {
        let after_item = &remaining[item_start + 6..];
        let Some(item_end) = after_item.find("</item>") else {
            break;
        };
        let item_body = &after_item[..item_end];
        remaining = &after_item[item_end + 7..];

        let title = extract_xml_text(item_body, "title").unwrap_or_default();
        let link = extract_xml_text(item_body, "link").unwrap_or_default();
        let description = extract_xml_text(item_body, "description").unwrap_or_default();

        // The description is often HTML-encoded; decode basic entities
        let text = decode_html_entities(&description);

        let number = extract_md_number(&title).unwrap_or(0);
        let md_type = classify_md_type(&text);
        let concerning = extract_concerning(&text);
        let polygon = parse_lat_lon_polygon(&text)
            .map(|ring| vec![ring])
            .unwrap_or_default();

        if number == 0 && polygon.is_empty() {
            continue; // Skip items we can't meaningfully display
        }

        discussions.push(SpcDiscussion {
            number,
            title,
            text,
            link,
            md_type,
            polygon,
            concerning,
        });
    }

    Ok(discussions)
}

/// Extract text content from a simple XML element like `<tag>content</tag>`.
fn extract_xml_text<'a>(xml: &'a str, tag: &str) -> Option<String> {
    let open = format!("<{}", tag);
    let close = format!("</{}>", tag);

    let start = xml.find(&open)?;
    let after_open = &xml[start + open.len()..];

    // Skip past any attributes and the closing >
    let content_start = after_open.find('>')? + 1;
    let content = &after_open[content_start..];

    let end = content.find(&close)?;
    Some(content[..end].trim().to_string())
}

/// Decode common HTML entities found in RSS CDATA/description fields.
fn decode_html_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&#39;", "'")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_coord_token_8digit() {
        // 35179718 → lat 35.17, lon -97.18
        let (lat, lon) = parse_coord_token("35179718").unwrap();
        assert!((lat - 35.17).abs() < 0.001);
        assert!((lon - (-97.18)).abs() < 0.001);
    }

    #[test]
    fn test_parse_coord_token_7digit() {
        // 9519755 → lat 9.51 (3-digit), lon -97.55
        let (lat, lon) = parse_coord_token("9519755").unwrap();
        assert!((lat - 9.51).abs() < 0.001);
        assert!((lon - (-97.55)).abs() < 0.001);
    }

    #[test]
    fn test_parse_lat_lon_polygon() {
        let text = "Some preamble text\n\
                     LAT...LON   35179718 34899754 34449768\n\
                     34209745 34189682 35179718\n\
                     \n\
                     Some trailing text";
        let ring = parse_lat_lon_polygon(text).unwrap();
        assert!(ring.len() >= 3);
        // First and last should be the same (closed ring)
        assert_eq!(ring.first(), ring.last());
    }

    #[test]
    fn test_classify_md_type_convective() {
        assert_eq!(classify_md_type("SEVERE THUNDERSTORM watch likely"), MdType::Convective);
        assert_eq!(classify_md_type("isolated tornado possible"), MdType::Convective);
    }

    #[test]
    fn test_classify_md_type_winter() {
        assert_eq!(classify_md_type("heavy snow expected"), MdType::WinterWeather);
        assert_eq!(classify_md_type("freezing rain advisory"), MdType::WinterWeather);
    }

    #[test]
    fn test_classify_md_type_other() {
        assert_eq!(classify_md_type("fire weather concerns"), MdType::Other);
    }

    #[test]
    fn test_extract_md_number() {
        assert_eq!(extract_md_number("Mesoscale Discussion #0153"), Some(153));
        assert_eq!(extract_md_number("Mesoscale Discussion #42"), Some(42));
    }

    #[test]
    fn test_parse_md_rss_basic() {
        let xml = r#"<?xml version="1.0"?>
<rss><channel>
<item>
<title>Mesoscale Discussion #0100</title>
<link>https://www.spc.noaa.gov/products/md/md0100.html</link>
<description>
ATTN...WFO...OUN...TSA...

CONCERNING...SEVERE THUNDERSTORM WATCH 123...

Some discussion text here.

LAT...LON   35179718 34899754 34449768 34209745 35179718

</description>
</item>
</channel></rss>"#;

        let discussions = parse_md_rss(xml).unwrap();
        assert_eq!(discussions.len(), 1);
        assert_eq!(discussions[0].number, 100);
        assert_eq!(discussions[0].md_type, MdType::Convective);
        assert!(discussions[0].concerning.is_some());
        assert!(!discussions[0].polygon.is_empty());
    }

    #[test]
    fn test_decode_html_entities() {
        assert_eq!(decode_html_entities("a &amp; b"), "a & b");
        assert_eq!(decode_html_entities("&lt;br&gt;"), "<br>");
    }
}
