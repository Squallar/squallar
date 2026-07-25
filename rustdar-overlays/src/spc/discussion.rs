use crate::types::{GeoPolygon, GeoPolygonRing, HatchPattern, OverlayFeature};
use super::colors::{md_fill_color, md_stroke_color};

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
    /// Pre-built overlay feature for generic click detection and rendering.
    pub feature: OverlayFeature,
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
/// Every whitespace-separated token is exactly 8 digits; see
/// [`parse_coord_token`] for the encoding. Tokens that are not well formed are
/// skipped rather than aborting the parse.
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
        let has_coords = trimmed.split_whitespace().any(is_coord_token);
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

/// Longitude fields below this many hundredths of a degree had their leading
/// `1` stripped by the NWS encoding (see [`parse_coord_token`]).
///
/// A field of `6000` is 60.00°W and stands unshifted; `5999` would be 159.99°W
/// once restored, which is outside the product domain and gets rejected by the
/// range check. The threshold sits in that dead band: the easternmost point in
/// the MD archive is 66.83°W (Caribou, ME) and NWS's own GIS service declares
/// `xmax = -65.2554`, so no real product reaches 60°W.
const LON_DROPPED_HUNDREDS_THRESHOLD: u32 = 6000;

/// Is this token a well-formed `LAT...LON` coordinate token?
///
/// SPC coordinate tokens are fixed-width: exactly 8 ASCII digits.
fn is_coord_token(token: &str) -> bool {
    token.len() == 8 && token.bytes().all(|b| b.is_ascii_digit())
}

/// Parse a single coordinate token like `"35179718"` into `(lat, lon)`.
///
/// The token format is specified by NWS Instruction 10-517 (Aug 1, 2022),
/// §6.3.4/§6.3.5, Figures 5-6, which define the `LAT...LON` field as `AAaaBBbb`:
///
/// > `AAaa`=Latitude north in degrees to two decimal places (without decimal
/// > point), `BBbb`=Longitude west in degrees to two decimal places (without
/// > decimal point and **without leading 1 west of 100 degrees west**).
///
/// <https://www.weather.gov/media/directives/010_pdfs/pd01005017curr.pdf>
///
/// So `35179718` is 35.17°N, 97.18°W, and 120.34°W is encoded as `2034` with
/// its leading `1` dropped. Restoring that hundreds digit is what the shift
/// below does. Without it every point west of 100°W decodes to a spot in the
/// Atlantic and is discarded by the range check, which silently deletes or
/// deforms the polygon of any discussion covering the western third of the
/// country.
///
/// This runs on text fetched from the network, so it never panics: any token
/// that is not exactly 8 ASCII digits, or that decodes outside the SPC product
/// domain, yields `None` and is skipped by the caller. Per the directive the
/// field is fixed-width and zero-padded, so there is no 7-digit form (104.50°W
/// is `0450`, never `450`); a 7-digit token is malformed input.
///
/// One known consequence of decoding an intentionally lossy format: a corrupt
/// token can now land on a plausible CONUS point instead of being rejected
/// (`"35000100"` used to fail the range check at 1.00°W, and now reads as
/// 35.00°N, 101.00°W). That is inherent to the encoding — the alternative is
/// discarding a third of all discussions.
fn parse_coord_token(token: &str) -> Option<(f64, f64)> {
    if !is_coord_token(token) {
        return None;
    }

    let lat_hundredths: u32 = token[..4].parse().ok()?;
    let mut lon_hundredths: u32 = token[4..].parse().ok()?;

    // Restore the leading "1" the directive drops west of 100 degrees west.
    if lon_hundredths < LON_DROPPED_HUNDREDS_THRESHOLD {
        lon_hundredths += 10_000;
    }

    let lat = f64::from(lat_hundredths) / 100.0;
    let lon = -(f64::from(lon_hundredths) / 100.0); // Western Hemisphere

    // Basic sanity checks for CONUS coordinates
    if !(15.0..=60.0).contains(&lat) || !(-140.0..=-50.0).contains(&lon) {
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
    let doc = roxmltree::Document::parse(xml)
        .map_err(|e| format!("RSS parse error: {e}"))?;
    let mut discussions = Vec::new();

    for item in doc.descendants().filter(|n| n.has_tag_name("item")) {
        let child_text = |tag: &str| -> String {
            item.children()
                .find(|n| n.has_tag_name(tag))
                .and_then(|n| n.text())
                .unwrap_or_default()
                .trim()
                .to_string()
        };
        let title = child_text("title");
        let link = child_text("link");
        let description = child_text("description");

        // The description is often HTML-encoded; decode basic entities
        let text = strip_html_tags(&decode_html_entities(&description));

        let number = extract_md_number(&title).unwrap_or(0);
        let md_type = classify_md_type(&text);
        let concerning = extract_concerning(&text);
        let polygon = parse_lat_lon_polygon(&text)
            .map(|ring| vec![ring])
            .unwrap_or_default();

        if number == 0 && polygon.is_empty() {
            continue; // Skip items we can't meaningfully display
        }

        let feature = OverlayFeature::new(
            vec![polygon.clone()],
            md_fill_color(&md_type),
            md_stroke_color(&md_type),
            format!("MD {number}"),
            String::new(),
            HatchPattern::None,
        );

        discussions.push(SpcDiscussion {
            number,
            title,
            text,
            link,
            md_type,
            polygon,
            feature,
            concerning,
        });
    }

    Ok(discussions)
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

/// Strip HTML/XML markup from text, keeping only readable content.
///
/// Handles CDATA wrappers, `<br>` → newline, and removes all other tags.
fn strip_html_tags(s: &str) -> String {
    // Remove CDATA wrappers
    let s = s.replace("<![CDATA[", "").replace("]]>", "");
    // Convert <br> variants to newlines
    let s = s.replace("<br />", "\n").replace("<br/>", "\n").replace("<br>", "\n");
    // Strip all remaining HTML tags
    let mut result = String::with_capacity(s.len());
    let mut in_tag = false;
    for ch in s.chars() {
        if ch == '<' {
            in_tag = true;
        } else if ch == '>' {
            in_tag = false;
        } else if !in_tag {
            result.push(ch);
        }
    }
    // Collapse runs of 3+ blank lines into 2
    while result.contains("\n\n\n") {
        result = result.replace("\n\n\n", "\n\n");
    }
    result.trim().to_string()
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
    fn test_parse_coord_token_rejects_7digit() {
        // There is no 7-digit form: NWSI 10-517 specifies a fixed-width field
        // that zero-pads the longitude (104.50°W is "0450", not "450"), and no
        // 7-digit token appears anywhere in the MD archive. A 7-digit token is
        // malformed input and must be skipped, not guessed at.
        assert_eq!(parse_coord_token("9519755"), None);
        assert_eq!(parse_coord_token("3517450"), None);
    }

    #[test]
    fn test_parse_coord_token_west_of_100() {
        // NWS drops the leading "1" from longitudes >= 100°W, so "2034" is
        // 120.34°W. Real example: MD 1 of 2024, "the Sierra Nevada...from west
        // of Tahoe into areas southeast of Yosemite".
        let (lat, lon) = parse_coord_token("39282034").unwrap();
        assert!((lat - 39.28).abs() < 0.001);
        assert!((lon - (-120.34)).abs() < 0.001);

        // Real example: MD 112 of 2024, "Northern Arizona".
        let (lat, lon) = parse_coord_token("34611154").unwrap();
        assert!((lat - 34.61).abs() < 0.001);
        assert!((lon - (-111.54)).abs() < 0.001);

        // Zero-padded form for 100-109.99°W.
        let (lat, lon) = parse_coord_token("35060450").unwrap();
        assert!((lat - 35.06).abs() < 0.001);
        assert!((lon - (-104.50)).abs() < 0.001);
    }

    #[test]
    fn test_parse_coord_token_lon_shift_boundary() {
        // The exact edges of the dropped-"1" shift, derived from NWSI 10-517:
        // the field carries longitude west without its leading 1 west of 100
        // degrees west, so "0000" is 100.00°W (10000 with the 1 removed) while
        // "9999" is a literal 99.99°W.
        assert_eq!(parse_coord_token("35000000"), Some((35.00, -100.00)));
        assert_eq!(parse_coord_token("35009999"), Some((35.00, -99.99)));

        // First field that is NOT shifted. 60.00°W is far east of any real
        // product (easternmost archived point is 66.83°W) but is inside the
        // range check, so it must survive unshifted rather than become 160°W.
        assert_eq!(parse_coord_token("35006000"), Some((35.00, -60.00)));

        // One below the threshold: shifted to 159.99°W, then rejected as
        // outside the product domain. Nothing in the archive lands here.
        assert_eq!(parse_coord_token("35005999"), None);
    }

    #[test]
    fn test_parse_coord_token_malformed_never_panics() {
        // Malformed tokens return None instead of panicking or slicing at a
        // non-char boundary. A panic here would run inside a background fetch
        // task, where it is swallowed by the runtime and leaves the overlay
        // stuck in its "fetching" state.
        for token in [
            "",
            "3",
            "351797",       // too short
            "351797180",    // too long
            "3517971a",     // non-digit
            "-5179718",     // sign
            "35 179718",    // space
            "351797\u{e9}", // multi-byte char landing on the 8-byte boundary
            "\u{e9}\u{e9}\u{e9}\u{e9}",
            "99999999", // decodes out of domain
            // Rejected on latitude (0.0 fails the 15..=60 gate) before the
            // longitude is looked at — see the boundary test above for the
            // lon field "0000" path.
            "00000000",
        ] {
            assert_eq!(parse_coord_token(token), None, "token {token:?}");
        }
    }

    #[test]
    fn test_parse_lat_lon_polygon_western_md() {
        // Tokens from the LAT...LON block of SPC MD 1 of 2024 (Sierra Nevada),
        // in product order; the line wrapping and ATTN line are simplified.
        let text = "ATTN...WFO...REV...STO...\n\
                    \n\
                    LAT...LON   39282034 38832002 38131950 37361936 37671975\n\
                    38162027 38772070 39082082 39282034\n\
                    \n\
                    Trailing prose.";
        let ring = parse_lat_lon_polygon(text).unwrap();
        // All nine points must survive; none may be dropped as "out of range".
        assert_eq!(ring.len(), 9);
        for &(lat, lon) in &ring {
            assert!((37.0..40.0).contains(&lat), "lat {lat}");
            assert!((-121.0..-119.0).contains(&lon), "lon {lon}");
        }
        assert_eq!(ring.first(), ring.last());
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
    fn test_parse_md_rss_plaintext_plus_cdata_description() {
        // Hardcoded XML — no network. It mirrors the structure of spcmdrss.xml:
        // <description> holds a plain-text summary and a CDATA section with the
        // product body as sibling nodes, so this is what proves roxmltree merges
        // the two and the polygon is reachable at all. Coordinates are from
        // MD 1 of 2024 (Sierra Nevada), i.e. west of 100W.
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<rss version="2.0"><channel>
  <item>
    <link>https://www.spc.noaa.gov/products/md/md0001.html</link>
    <title>SPC MD 0001</title>
    <description>MD 0001 CONCERNING HEAVY SNOW... FOR THE SIERRA NEVADA
    <![CDATA[<br /><a href="https://www.spc.noaa.gov/products/md/md0001.html"><img src="x.png" /></a><pre>

Mesoscale Discussion 0001
NWS Storm Prediction Center Norman OK

Areas affected...the Sierra Nevada

Concerning...Heavy snow

ATTN...WFO...REV...STO...

LAT...LON   39282034 38832002 38131950 37361936 37671975
            38162027 38772070 39082082 39282034

</pre>
<a href="https://www.spc.noaa.gov/products/md/md0001.html">Read more</a>
]]>
    </description>
  </item>
</channel></rss>"#;

        let discussions = parse_md_rss(xml).unwrap();
        assert_eq!(discussions.len(), 1);
        let md = &discussions[0];
        assert_eq!(md.number, 1);
        let ring = md.polygon.first().expect("polygon should be parsed from CDATA body");
        assert_eq!(ring.len(), 9);
        for &(_, lon) in ring {
            assert!((-121.0..-119.0).contains(&lon), "lon {lon}");
        }
    }

    #[test]
    fn test_decode_html_entities() {
        assert_eq!(decode_html_entities("a &amp; b"), "a & b");
        assert_eq!(decode_html_entities("&lt;br&gt;"), "<br>");
    }
}
