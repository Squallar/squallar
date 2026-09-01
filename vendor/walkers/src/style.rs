use color::Rgba8;
use egui::Color32;
use log::warn;
use serde::Deserialize;
pub use serde_json::{Value, json};
use thiserror::Error;

use crate::expression::Context;

/// Style for rendering vector maps.
///
/// It is based on MapLibre's style specification, but only a small subset is supported.
/// Most notably, Walkers only read `layers` section of the style and applies it to the
/// [`crate::Tiles`] it is used with. In spite that, it should be possible to deserialize most
/// of the MapLibre's styles using `serde`, as unknown JSON/YAML fields are simply ignored.
///
/// <https://maplibre.org/maplibre-style-spec/>
#[derive(Deserialize, Default)]
pub struct Style {
    pub layers: Vec<Layer>,
}

impl Style {
    /// Parse a style from MapLibre style JSON.
    ///
    /// This is the only constructor that reads JSON. The four bundled-style
    /// constructors it replaces each ended in `.expect("failed to parse style
    /// JSON")`, so a malformed style aborted the process; this reports the
    /// failure instead.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }
}

#[derive(Deserialize, Debug)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Layer {
    Background {
        #[serde(flatten)]
        zoom_range: ZoomRange,
        paint: Paint,
    },
    #[serde(rename_all = "kebab-case")]
    Fill {
        source_layer: String,
        #[serde(flatten)]
        zoom_range: ZoomRange,
        filter: Option<Filter>,
        paint: Paint,
    },
    #[serde(rename_all = "kebab-case")]
    Line {
        source_layer: String,
        #[serde(flatten)]
        zoom_range: ZoomRange,
        filter: Option<Filter>,
        paint: Paint,
    },
    #[serde(rename_all = "kebab-case")]
    Symbol {
        source_layer: String,
        #[serde(flatten)]
        zoom_range: ZoomRange,
        filter: Option<Filter>,
        layout: Layout,
        paint: Option<Paint>,
    },
    #[serde(rename_all = "kebab-case")]
    Circle {
        source_layer: String,
        #[serde(flatten)]
        zoom_range: ZoomRange,
        filter: Option<Filter>,
    },
    Raster,
    FillExtrusion,
}

impl Layer {
    /// Whether this layer draws at `zoom`, by its own declared zoom range.
    ///
    /// A layer type that carries no range — `raster`, `fill-extrusion`, which
    /// this renderer does not draw at all — is visible at every zoom, so that
    /// the answer here never becomes the reason one of them is skipped.
    pub fn visible_at(&self, zoom: u8) -> bool {
        match self {
            Layer::Background { zoom_range, .. }
            | Layer::Fill { zoom_range, .. }
            | Layer::Line { zoom_range, .. }
            | Layer::Symbol { zoom_range, .. }
            | Layer::Circle { zoom_range, .. } => zoom_range.contains(zoom),
            Layer::Raster | Layer::FillExtrusion => true,
        }
    }
}

/// A style layer's `minzoom`/`maxzoom`, with MapLibre's asymmetric bounds.
///
/// <https://maplibre.org/maplibre-style-spec/layers/>
///
/// **`minzoom` is inclusive and `maxzoom` is exclusive**, which is the
/// specification's wording and not a choice made here: "at zoom levels less
/// than the minzoom, the layer will be hidden" against "at zoom levels equal
/// to or greater than the maxzoom, the layer will be hidden". Getting that
/// asymmetry backwards would draw one whole zoom level's worth of layers that
/// the style excluded, or hide one it included, and neither shows up as
/// anything but a wrong-looking map.
///
/// The bounds are `f32` because the specification allows fractional values,
/// while the zoom this is asked about is the integer tile zoom. A
/// `"minzoom": 13.5` therefore first admits the layer at tile zoom 14, which
/// is the same answer MapLibre gives a tile renderer.
#[derive(Deserialize, Debug, Default)]
pub struct ZoomRange {
    pub minzoom: Option<f32>,
    pub maxzoom: Option<f32>,
}

impl ZoomRange {
    /// Whether `zoom` falls inside this range. An absent bound does not
    /// constrain; an absent range admits every zoom.
    pub fn contains(&self, zoom: u8) -> bool {
        let zoom = f32::from(zoom);
        !self.minzoom.is_some_and(|minzoom| zoom < minzoom)
            && !self.maxzoom.is_some_and(|maxzoom| zoom >= maxzoom)
    }
}

#[derive(Deserialize, Default, Debug)]
#[serde(rename_all = "kebab-case")]
pub struct Paint {
    pub background_color: Option<Color>,
    pub fill_color: Option<Color>,
    /// <https://maplibre.org/maplibre-style-spec/layers/>#fill-opacity
    pub fill_opacity: Option<Float>,
    pub line_width: Option<Float>,
    /// <https://maplibre.org/maplibre-style-spec/layers/>#line-color
    pub line_color: Option<Color>,
    /// <https://maplibre.org/maplibre-style-spec/layers/>#line-opacity
    pub line_opacity: Option<Float>,
    /// <https://maplibre.org/maplibre-style-spec/layers/>#text-color
    pub text_color: Option<Color>,
    /// <https://maplibre.org/maplibre-style-spec/layers/>#text-halo-color
    pub text_halo_color: Option<Color>,
    /// <https://maplibre.org/maplibre-style-spec/layers/>#text-halo-width
    ///
    /// In screen points, like `line-width` beside it. Read because it is not a
    /// constant across the committed styles: 28 symbol layers ask for `1` and
    /// `watername_ocean` asks for `0`, so a renderer that assumed a width would
    /// draw a halo the style explicitly declined.
    pub text_halo_width: Option<Float>,
}

#[derive(Debug, Error)]
enum StyleError {
    #[error(transparent)]
    Expression(#[from] crate::expression::Error),
    #[error("invalid type")]
    InvalidType,
    #[error(transparent)]
    Parsing(#[from] color::ParseError),
}

#[derive(Deserialize, Debug)]
pub struct Color(pub Value);

impl Color {
    pub fn evaluate(&self, context: &Context) -> Color32 {
        match self.try_evaluate(context) {
            Ok(color) => color,
            Err(err) => {
                warn!("{err}");
                Color32::MAGENTA
            }
        }
    }

    fn try_evaluate(&self, context: &Context) -> Result<Color32, StyleError> {
        match context.evaluate(&self.0)? {
            Value::String(color) => {
                let color: color::AlphaColor<color::Srgb> = color.parse()?;
                let Rgba8 { r, g, b, a } = color.to_rgba8();
                Ok(Color32::from_rgba_premultiplied(r, g, b, a))
            }
            _ => Err(StyleError::InvalidType),
        }
    }
}

#[derive(Deserialize, Debug)]
pub struct Float(pub Value);

impl Float {
    pub fn evaluate(&self, context: &Context) -> f32 {
        match self.try_evaluate(context) {
            Ok(value) => value,
            Err(err) => {
                warn!("{err}");
                0.5
            }
        }
    }

    fn try_evaluate(&self, context: &Context) -> Result<f32, StyleError> {
        match context.evaluate(&self.0)? {
            Value::Number(num) => Ok(num.as_f64().ok_or(StyleError::InvalidType)? as f32),
            _ => Err(StyleError::InvalidType),
        }
    }
}

#[derive(Deserialize, Debug)]
pub struct Filter(pub Value);

impl Filter {
    /// Match this filter against feature properties.
    pub fn matches(&self, context: &Context) -> bool {
        match context.evaluate(&self.0) {
            Ok(Value::Bool(b)) => b,
            other => {
                warn!("Expected filter to evaluate to boolean, got: {other:?}");
                false
            }
        }
    }
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "kebab-case")]
pub struct Layout {
    text_field: Option<Value>,
    pub text_size: Option<Float>,
    /// <https://maplibre.org/maplibre-style-spec/layers/>#text-max-width
    ///
    /// The width a symbol label wraps at, **in ems of its own `text-size`**.
    /// MapLibre's default is 10; `None` here means the style said nothing and
    /// the renderer applies that default.
    pub text_max_width: Option<Float>,
    /// <https://maplibre.org/maplibre-style-spec/layers/>#text-line-height
    ///
    /// Also in ems. MapLibre's default is 1.2, which is close enough to egui's
    /// own row height that `None` is left to the text layer rather than
    /// substituted here.
    pub text_line_height: Option<Float>,
}

impl Layout {
    pub fn text(&self, context: &Context) -> Option<String> {
        self.text_field
            .as_ref()
            .and_then(|value| match context.evaluate(value) {
                Ok(Value::String(s)) => Some(s),
                _ => None,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// MapLibre emits `source-layer`; every other variant renames it, so a
    /// `circle` layer used to fail -- and, because `layers` is one `Vec<Layer>`
    /// of an internally-tagged enum, took the whole style parse down with it.
    #[test]
    fn circle_layer_with_kebab_case_source_layer_parses() {
        let json = r#"{
            "layers": [
                { "type": "background", "paint": {} },
                { "type": "circle", "source-layer": "poi", "filter": ["==", "k", "v"] }
            ]
        }"#;

        let style = Style::from_json(json).expect("style with a circle layer parses");
        assert_eq!(style.layers.len(), 2);

        let Some(Layer::Circle { source_layer, .. }) = style.layers.get(1) else {
            panic!("expected the second layer to be a Circle");
        };
        assert_eq!(source_layer, "poi");
    }

    #[test]
    fn from_json_reports_a_parse_failure_instead_of_panicking() {
        assert!(Style::from_json("{ \"layers\": [ { \"type\": \"nope\" } ] }").is_err());
    }
}
