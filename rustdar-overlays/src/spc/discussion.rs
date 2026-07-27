use super::colors::{md_fill_color, md_stroke_color};
use crate::types::{GeoPolygon, GeoPolygonRing, HatchPattern, OverlayFeature};

/// Not an SPC field: derived from keywords in the discussion text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MdType {
    Convective,
    WinterWeather,
    /// Fire weather, heavy rain, anything else.
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

#[derive(Debug, Clone)]
pub struct SpcDiscussion {
    pub number: u32,
    /// RSS `<title>`, e.g. "Mesoscale Discussion #0153".
    pub title: String,
    pub text: String,
    pub link: String,
    pub md_type: MdType,
    pub polygon: GeoPolygon,
    /// Pre-built for generic click detection and rendering.
    pub feature: OverlayFeature,
    pub concerning: Option<String>,
}

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

/// The `LAT...LON` block spans multiple lines:
/// ```text
///    LAT...LON   35179718 34889754 34449768 34209745 34189682
///    34249641 34639588 35149575 35449600 35459680 35179718
/// ```
/// Malformed tokens are skipped, not fatal. See [`parse_coord_token`].
pub fn parse_lat_lon_polygon(text: &str) -> Option<GeoPolygonRing> {
    let marker_pos = text.find("LAT...LON")?;
    let after_marker = &text[marker_pos + "LAT...LON".len()..];

    let mut coords: Vec<(f64, f64)> = Vec::new();

    for line in after_marker.lines() {
        let trimmed = line.trim();
        let has_coords = trimmed.split_whitespace().any(is_coord_token);
        if !has_coords && !trimmed.is_empty() {
            break;
        }
        if trimmed.is_empty() {
            // One blank line may precede the block; the second ends it.
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

    if coords.first() != coords.last() {
        let first = coords[0];
        coords.push(first);
    }

    Some(coords)
}

/// Longitude fields below this had their leading `1` stripped (see
/// [`parse_coord_token`]). The threshold sits in a dead band: `6000` is 60.00°W
/// unshifted, `5999` restores to 159.99°W and fails the range check. No real
/// product reaches 60°W — easternmost archived MD point is 66.83°W (Caribou,
/// ME), and NWS's GIS service declares `xmax = -65.2554`.
const LON_DROPPED_HUNDREDS_THRESHOLD: u32 = 6000;

/// SPC coordinate tokens are fixed-width: exactly 8 ASCII digits.
fn is_coord_token(token: &str) -> bool {
    token.len() == 8 && token.bytes().all(|b| b.is_ascii_digit())
}

/// `"35179718"` → 35.17°N, 97.18°W. Format is NWSI 10-517 (Aug 1 2022)
/// §6.3.4/§6.3.5 Figures 5-6, field `AAaaBBbb`:
///
/// > `AAaa`=Latitude north in degrees to two decimal places (without decimal
/// > point), `BBbb`=Longitude west in degrees to two decimal places (without
/// > decimal point and **without leading 1 west of 100 degrees west**).
///
/// <https://www.weather.gov/media/directives/010_pdfs/pd01005017curr.pdf>
///
/// So 120.34°W is `2034`. Without restoring that hundreds digit every point
/// west of 100°W decodes into the Atlantic and is dropped by the range check,
/// deforming the polygon of any discussion over the western third of the US.
///
/// The field is zero-padded, so there is no 7-digit form (104.50°W is `0450`,
/// never `450`); a 7-digit token is malformed input. Runs on network text, so
/// it returns `None` rather than panicking.
///
/// The encoding is lossy: a corrupt token can land on a plausible CONUS point
/// rather than being rejected (`"35000100"` reads as 35.00°N, 101.00°W).
fn parse_coord_token(token: &str) -> Option<(f64, f64)> {
    if !is_coord_token(token) {
        return None;
    }

    let lat_hundredths: u32 = token[..4].parse().ok()?;
    let mut lon_hundredths: u32 = token[4..].parse().ok()?;

    // Restore the leading "1" the directive drops west of 100°W.
    if lon_hundredths < LON_DROPPED_HUNDREDS_THRESHOLD {
        lon_hundredths += 10_000;
    }

    let lat = f64::from(lat_hundredths) / 100.0;
    let lon = -(f64::from(lon_hundredths) / 100.0); // Field is degrees *west*.

    if !(15.0..=60.0).contains(&lat) || !(-140.0..=-50.0).contains(&lon) {
        return None;
    }

    Some((lat, lon))
}

/// The feed uses both `CONCERNING...` and `Concerning...`.
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

/// From "Mesoscale Discussion #0153" and similar.
fn extract_md_number(title: &str) -> Option<u32> {
    if let Some(hash_pos) = title.find('#') {
        let after = &title[hash_pos + 1..];
        let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        return digits.parse().ok();
    }
    // Fallback: the trailing digit run.
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

/// `spcmdrss.xml` items carry `<title>`, `<link>` and a `<description>` holding
/// the whole product body, `LAT...LON` block included.
pub fn parse_md_rss(xml: &str) -> Result<Vec<SpcDiscussion>, String> {
    let doc = roxmltree::Document::parse(xml).map_err(|e| format!("RSS parse error: {e}"))?;
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

        // The description is usually HTML-encoded.
        let text = strip_html_tags(&decode_html_entities(&description));

        let number = extract_md_number(&title).unwrap_or(0);
        let md_type = classify_md_type(&text);
        let concerning = extract_concerning(&text);
        let polygon = parse_lat_lon_polygon(&text)
            .map(|ring| vec![ring])
            .unwrap_or_default();

        if number == 0 && polygon.is_empty() {
            continue; // Nothing displayable.
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

fn decode_html_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&#39;", "'")
}

/// Unwraps CDATA, maps `<br>` to a newline, drops all other tags.
fn strip_html_tags(s: &str) -> String {
    let s = s.replace("<![CDATA[", "").replace("]]>", "");
    let s = s
        .replace("<br />", "\n")
        .replace("<br/>", "\n")
        .replace("<br>", "\n");
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
        let (lat, lon) = parse_coord_token("35179718").unwrap();
        assert!((lat - 35.17).abs() < 0.001);
        assert!((lon - (-97.18)).abs() < 0.001);
    }

    #[test]
    fn test_parse_coord_token_rejects_7digit() {
        // NWSI 10-517 zero-pads the longitude field (104.50°W is "0450"), and
        // no 7-digit token appears in the MD archive. Skip, do not guess.
        assert_eq!(parse_coord_token("9519755"), None);
        assert_eq!(parse_coord_token("3517450"), None);
    }

    #[test]
    fn test_parse_coord_token_west_of_100() {
        // "2034" is 120.34°W. Real token, MD 1 of 2024 (Sierra Nevada).
        let (lat, lon) = parse_coord_token("39282034").unwrap();
        assert!((lat - 39.28).abs() < 0.001);
        assert!((lon - (-120.34)).abs() < 0.001);

        // Real token, MD 112 of 2024 (northern Arizona).
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
        // Edges of the dropped-"1" shift per NWSI 10-517: "0000" is 100.00°W,
        // "9999" is a literal 99.99°W.
        assert_eq!(parse_coord_token("35000000"), Some((35.00, -100.00)));
        assert_eq!(parse_coord_token("35009999"), Some((35.00, -99.99)));

        // First unshifted field: must stay 60.00°W, not become 160°W.
        assert_eq!(parse_coord_token("35006000"), Some((35.00, -60.00)));

        // One below the threshold: shifts to 159.99°W, then fails the range
        // check. Nothing in the archive lands here.
        assert_eq!(parse_coord_token("35005999"), None);
    }

    #[test]
    fn test_parse_coord_token_malformed_never_panics() {
        // A panic here runs inside a background fetch task, is swallowed by the
        // runtime, and leaves the overlay stuck in its "fetching" state.
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
            // Fails on latitude (0.0, outside 15..=60) before the lon is read.
            "00000000",
        ] {
            assert_eq!(parse_coord_token(token), None, "token {token:?}");
        }
    }

    #[test]
    fn test_parse_lat_lon_polygon_western_md() {
        // Real LAT...LON block, SPC MD 1 of 2024 (Sierra Nevada), in product
        // order. Fails if points west of 100°W are dropped as out of range.
        let text = "ATTN...WFO...REV...STO...\n\
                    \n\
                    LAT...LON   39282034 38832002 38131950 37361936 37671975\n\
                    38162027 38772070 39082082 39282034\n\
                    \n\
                    Trailing prose.";
        let ring = parse_lat_lon_polygon(text).unwrap();
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
        assert_eq!(ring.first(), ring.last());
    }

    #[test]
    fn test_classify_md_type_convective() {
        assert_eq!(
            classify_md_type("SEVERE THUNDERSTORM watch likely"),
            MdType::Convective
        );
        assert_eq!(
            classify_md_type("isolated tornado possible"),
            MdType::Convective
        );
    }

    #[test]
    fn test_classify_md_type_winter() {
        assert_eq!(
            classify_md_type("heavy snow expected"),
            MdType::WinterWeather
        );
        assert_eq!(
            classify_md_type("freezing rain advisory"),
            MdType::WinterWeather
        );
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
        // spcmdrss.xml's real shape: <description> holds a plain-text summary
        // and a CDATA product body as *sibling* nodes. Fails if roxmltree does
        // not merge the two, which puts the polygon out of reach entirely.
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
        let ring = md
            .polygon
            .first()
            .expect("polygon should be parsed from CDATA body");
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
