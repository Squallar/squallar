//! Convert a CARTO MapLibre style into one this workspace owns and renders.
//!
//! Two halves, deliberately separate:
//!
//! * [`convert`] rewrites an upstream style document into ours and returns a
//!   [`Report`] of everything it did.
//! * [`check`] re-reads a finished style and judges it, knowing nothing about
//!   how it was produced.
//!
//! The split is the point. A checker that shares the converter's beliefs
//! cannot catch the converter being wrong, so [`check`] re-derives every
//! conclusion from the output document alone -- it never sees the input, the
//! rename table's hit count, or the converter's own `Report`.
//!
//! [`check`] is still fed deliberately broken documents and required to reject
//! each one, but that suite is no longer here: it is
//! `squallar-egui/tests/committed_styles_parse.rs`, which compiles this file in
//! with `#[path]` so there is exactly one `check`. `tests/converted_styles.rs`
//! retains only the four tests that call [`convert`].

use serde_json::{Map, Value, json};
use std::collections::BTreeSet;
use std::fmt;

/// The sixteen source layers an OpenMapTiles vector tile can carry.
///
/// Measured off 42 real tiles in `/home/reddragon/.cache/omt-corpus/` and
/// cross-checked against the `vector_layers` array the live TileJSON at
/// <https://tiles.openfreemap.org/planet> serves; the two agree exactly.
/// A `source-layer` outside this set names nothing and draws nothing.
pub const OMT_SOURCE_LAYERS: [&str; 16] = [
    "aerodrome_label",
    "aeroway",
    "boundary",
    "building",
    "housenumber",
    "landcover",
    "landuse",
    "mountain_peak",
    "park",
    "place",
    "poi",
    "transportation",
    "transportation_name",
    "water",
    "water_name",
    "waterway",
];

/// Mapbox Streets source layers with an OpenMapTiles counterpart.
///
/// Applied defensively. Against the CARTO inputs this tool actually ran on it
/// fired zero times, because those styles already target OpenMapTiles -- see
/// DECISIONS.md, "The renames were already done upstream". The table stays so
/// that pointing this converter at an older CARTO revision, or at any other
/// Mapbox-Streets-era style, is a supported thing to do rather than a silent
/// map with no roads on it.
pub const SOURCE_LAYER_RENAMES: [(&str, &str); 6] = [
    ("road", "transportation"),
    ("road_label", "transportation_name"),
    ("admin", "boundary"),
    ("poi_label", "poi"),
    ("place_label", "place"),
    ("airport_label", "aerodrome_label"),
];

/// Mapbox Streets source layers with no OpenMapTiles counterpart at all.
///
/// Not renames. A layer drawing from one of these cannot be satisfied from the
/// OpenMapTiles schema, so the converter refuses rather than dropping it
/// quietly; the caller has to decide what the map does without it and record
/// the decision. See DECISIONS.md, "Layers OpenMapTiles cannot satisfy".
pub const ABSENT_FROM_OMT: [&str; 4] = [
    "landuse_overlay",
    "motorway_junction",
    "natural_label",
    "structure",
];

/// Source layers dropped on purpose, each with the reason.
///
/// Empty since 2026-08-28. It held one entry, `housenumber`, because CARTO
/// painted its `text-color` and `text-halo-color` `transparent` in both Dark
/// Matter and Positron, so the layer cost a full symbol pass over the densest
/// layer in the tile to render nothing. That was CARTO's minimal-backdrop
/// aesthetic, inherited rather than chosen, and these styles have been owned
/// source since 2026-08-27: `www/styles/*.json` now carry their own
/// `housenumber` layer with a visible colour, recorded in each style's
/// `metadata."squallar:restored-layers"`.
///
/// The array stays, rather than the concept being deleted, because the drop
/// mechanism is still the thing that would account for a future decision not
/// to carry some upstream layer. Nothing is dropped today.
pub const DELIBERATE_DROPS: [(&str, &str); 0] = [];

/// The key each output layer carries its render phase under.
///
/// `metadata` is where the MapLibre style specification puts arbitrary
/// application data, and `walkers::style::Layer` has no field for it, so serde
/// ignores it on the way in. That is exactly the arrangement wanted: the tag
/// travels with the layer it describes, in the committed source, and costs the
/// current renderer nothing.
pub const PHASE_KEY: &str = "squallar:phase";

/// Which pass of the two-phase renderer a layer belongs to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    /// Shapes. Accumulated first, in style order.
    Ground,
    /// Text. Runs after every ground layer, so one `walkers::text::OccupiedAreas`
    /// per pane can arbitrate collisions across the whole map at once.
    Label,
}

impl Phase {
    /// The string written into the style document.
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Ground => "ground",
            Phase::Label => "label",
        }
    }

    /// The phase a MapLibre layer type belongs to.
    ///
    /// `symbol` is the only type that places text, and every one of the 27
    /// symbol layers in both CARTO inputs carries a `text-field`; none is
    /// icon-only. So the split is exactly "symbol or not", with no per-layer
    /// judgement to get wrong.
    pub fn of_layer_type(layer_type: &str) -> Phase {
        if layer_type == "symbol" {
            Phase::Label
        } else {
            Phase::Ground
        }
    }
}

/// Everything the converter did, for the report the operator reads.
#[derive(Default, Debug)]
pub struct Report {
    pub input_layers: usize,
    pub output_layers: usize,
    /// `(layer id, source-layer, reason)`.
    pub dropped: Vec<(String, String, String)>,
    /// `(layer id, from, to)`.
    pub renamed: Vec<(String, String, String)>,
    pub ground_layers: usize,
    pub label_layers: usize,
    pub stop_sets_to_expressions: usize,
    pub stop_sets_collapsed_to_scalars: usize,
    pub text_fields_rewritten: usize,
    pub not_in_filters_rewritten: usize,
    pub zoom_ranges_folded: usize,
}

/// A style this converter refuses to convert.
#[derive(Debug)]
pub enum Error {
    NotAnObject,
    NoLayers,
    LayerNotAnObject(usize),
    MissingLayerType(String),
    /// A layer draws from a source layer OpenMapTiles has no counterpart for.
    UnsatisfiableSourceLayer {
        layer: String,
        source_layer: String,
    },
    /// A layer draws from a name that survived renaming and is still not one of
    /// the sixteen.
    UnknownSourceLayer {
        layer: String,
        source_layer: String,
    },
    /// A `text-field` this converter will not guess at.
    UnhandledTextField {
        layer: String,
        found: String,
    },
    /// A legacy stop set whose shape the converter does not recognise.
    UnhandledStopSet {
        layer: String,
        found: String,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotAnObject => write!(f, "the style document is not a JSON object"),
            Error::NoLayers => write!(f, "the style document has no `layers` array"),
            Error::LayerNotAnObject(i) => write!(f, "layer at index {i} is not a JSON object"),
            Error::MissingLayerType(id) => write!(f, "layer `{id}` has no `type`"),
            Error::UnsatisfiableSourceLayer {
                layer,
                source_layer,
            } => write!(
                f,
                "layer `{layer}` draws from `{source_layer}`, which OpenMapTiles does not carry \
                 in any form. This is not a rename. Decide what the map does without it and \
                 record the decision in DECISIONS.md, then drop the layer explicitly."
            ),
            Error::UnknownSourceLayer {
                layer,
                source_layer,
            } => write!(
                f,
                "layer `{layer}` draws from `{source_layer}`, which is not one of the sixteen \
                 OpenMapTiles source layers. If it is a Mapbox Streets name, add it to \
                 SOURCE_LAYER_RENAMES; a name that reaches the output unmapped draws nothing, \
                 silently."
            ),
            Error::UnhandledTextField { layer, found } => write!(
                f,
                "layer `{layer}` has a `text-field` this converter will not guess at: {found}"
            ),
            Error::UnhandledStopSet { layer, found } => write!(
                f,
                "layer `{layer}` has a legacy stop set of an unrecognised shape: {found}"
            ),
        }
    }
}

impl std::error::Error for Error {}

/// Convert one CARTO style document into one this workspace owns.
///
/// `theme_name` is written into the output's `name`; `provenance` is recorded
/// verbatim in the output's top-level `metadata` so the committed file says
/// where it came from without anyone having to find this tool.
pub fn convert(
    input: &Value,
    theme_name: &str,
    provenance: &Map<String, Value>,
) -> Result<(Value, Report), Error> {
    let object = input.as_object().ok_or(Error::NotAnObject)?;
    let layers = object
        .get("layers")
        .and_then(Value::as_array)
        .ok_or(Error::NoLayers)?;

    let mut report = Report {
        input_layers: layers.len(),
        ..Report::default()
    };
    let mut output_layers = Vec::with_capacity(layers.len());

    for (index, layer) in layers.iter().enumerate() {
        let layer = layer
            .as_object()
            .ok_or(Error::LayerNotAnObject(index))?
            .clone();
        if let Some(converted) = convert_layer(layer, index, &mut report)? {
            output_layers.push(Value::Object(converted));
        }
    }

    report.output_layers = output_layers.len();

    let mut metadata = provenance.clone();
    metadata.insert(
        "squallar:upstream-layers".into(),
        json!(report.input_layers),
    );
    // Every drop, named in the committed file itself. The verification suite
    // reads the expected layer count back off this rather than being told it,
    // so "93 in, 92 out" is a claim the document makes and the checker tests.
    metadata.insert(
        "squallar:dropped-layers".into(),
        Value::Array(
            report
                .dropped
                .iter()
                .map(|(id, source_layer, reason)| {
                    json!({ "id": id, "source-layer": source_layer, "reason": reason })
                })
                .collect(),
        ),
    );

    let mut style = Map::new();
    style.insert("version".into(), json!(8));
    style.insert("name".into(), json!(theme_name));
    style.insert("metadata".into(), Value::Object(metadata));
    // Not read by `walkers::style::Style`, which deserialises `layers` and
    // nothing else -- the app's tile URL comes from
    // `walkers::sources::OpenFreeMap` instead. Emitted anyway so the file is a
    // valid MapLibre style document that other tooling can open.
    style.insert(
        "sources".into(),
        json!({
            "openmaptiles": {
                "type": "vector",
                "url": "https://tiles.openfreemap.org/planet"
            }
        }),
    );
    style.insert("layers".into(), Value::Array(output_layers));

    Ok((Value::Object(style), report))
}

/// Convert one layer, or return `Ok(None)` if it is deliberately dropped.
fn convert_layer(
    mut layer: Map<String, Value>,
    index: usize,
    report: &mut Report,
) -> Result<Option<Map<String, Value>>, Error> {
    let id = layer
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("<no id>")
        .to_owned();
    let layer_type = layer
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::MissingLayerType(id.clone()))?
        .to_owned();

    // 1. Rename Mapbox Streets source layers to their OpenMapTiles counterparts.
    if let Some(source_layer) = layer.get("source-layer").and_then(Value::as_str) {
        let source_layer = source_layer.to_owned();
        let renamed = SOURCE_LAYER_RENAMES
            .iter()
            .find(|(from, _)| *from == source_layer)
            .map(|(_, to)| (*to).to_owned());
        if let Some(to) = renamed {
            report
                .renamed
                .push((id.clone(), source_layer.clone(), to.clone()));
            layer.insert("source-layer".into(), json!(to));
        }
    }

    // 2. Drop what we drop on purpose, and refuse what we cannot satisfy.
    let source_layer = layer
        .get("source-layer")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    if !source_layer.is_empty() {
        if let Some((_, reason)) = DELIBERATE_DROPS.iter().find(|(sl, _)| *sl == source_layer) {
            report
                .dropped
                .push((id, source_layer, (*reason).to_owned()));
            return Ok(None);
        }
        if ABSENT_FROM_OMT.contains(&source_layer.as_str()) {
            return Err(Error::UnsatisfiableSourceLayer {
                layer: id,
                source_layer,
            });
        }
        if !OMT_SOURCE_LAYERS.contains(&source_layer.as_str()) {
            return Err(Error::UnknownSourceLayer {
                layer: id,
                source_layer,
            });
        }
    }

    // 3. Point the layer at the one source the output declares.
    if layer.contains_key("source") {
        layer.insert("source".into(), json!("openmaptiles"));
    }

    // 4. Normalise the filter, then fold the layer's zoom range into it.
    if let Some(filter) = layer.remove("filter") {
        let filter = rewrite_filter(&filter, report);
        layer.insert("filter".into(), filter);
    }
    fold_zoom_range(&mut layer, report);

    // 5. Normalise paint and layout values.
    for section in ["paint", "layout"] {
        if let Some(Value::Object(mut values)) = layer.remove(section) {
            for (property, value) in values.iter_mut() {
                let rewritten = if property == "text-field" {
                    report.text_fields_rewritten += 1;
                    rewrite_text_field(value, &id)?
                } else {
                    rewrite_stop_sets(value, property, &id, report)?
                };
                *value = rewritten;
            }
            layer.insert(section.into(), Value::Object(values));
        }
    }

    // 6. Tag the render phase.
    let phase = Phase::of_layer_type(&layer_type);
    match phase {
        Phase::Ground => report.ground_layers += 1,
        Phase::Label => report.label_layers += 1,
    }
    let mut metadata = match layer.remove("metadata") {
        Some(Value::Object(existing)) => existing,
        _ => Map::new(),
    };
    metadata.insert(PHASE_KEY.into(), json!(phase.as_str()));
    metadata.insert("squallar:source-index".into(), json!(index));
    layer.insert("metadata".into(), Value::Object(metadata));

    Ok(Some(layer))
}

/// Fold `minzoom` / `maxzoom` into the layer's filter.
///
/// **This is the step the work order said was unnecessary, and it is not.**
/// `walkers::mvt::render` iterates `style.layers` and consults each layer's
/// `filter` and nothing else; `walkers::style::Layer` has no `minzoom` or
/// `maxzoom` field, so serde drops both on the way in. Without this fold every
/// layer draws at every zoom -- 49 road layers and 15 place-label layers at
/// z0. See DECISIONS.md, "Zoom ranges have to be folded into filters".
///
/// The MapLibre range is `minzoom <= z < maxzoom`, which is what the emitted
/// comparisons say. `minzoom` and `maxzoom` are left on the layer as well:
/// they are correct, they are what the specification wants, and if walkers
/// ever honours them the fold merely re-asserts the same range.
fn fold_zoom_range(layer: &mut Map<String, Value>, report: &mut Report) {
    let minzoom = layer.get("minzoom").and_then(Value::as_f64);
    let maxzoom = layer.get("maxzoom").and_then(Value::as_f64);
    if minzoom.is_none() && maxzoom.is_none() {
        return;
    }

    // Zoom levels are integers in every input, and `walkers::expression`
    // answers `["zoom"]` with an integer. Emitting `9` rather than `9.0` keeps
    // the committed source readable for the humans who now own it.
    let level = |z: f64| -> Value {
        if z.fract() == 0.0 && z >= 0.0 {
            json!(z as u64)
        } else {
            json!(z)
        }
    };

    let mut clauses = Vec::new();
    if let Some(min) = minzoom {
        clauses.push(json!([">=", ["zoom"], level(min)]));
    }
    if let Some(max) = maxzoom {
        clauses.push(json!(["<", ["zoom"], level(max)]));
    }

    match layer.remove("filter") {
        // Flatten an existing `all` rather than nesting one inside another.
        Some(Value::Array(existing)) if existing.first().and_then(Value::as_str) == Some("all") => {
            clauses.extend(existing.into_iter().skip(1));
        }
        Some(other) => clauses.push(other),
        None => {}
    }

    clauses.insert(0, json!("all"));
    layer.insert("filter".into(), Value::Array(clauses));
    report.zoom_ranges_folded += 1;
}

/// Normalise legacy filter operators to one modern spelling.
///
/// `["!in", k, a, b]` becomes `["!", ["in", k, a, b]]` and `["none", a, b]`
/// becomes `["!", ["any", a, b]]`. One `!in` per theme in the CARTO input; no
/// `none`.
///
/// THE ORIGINAL REASON HERE IS DEAD, AND THE REWRITE IS KEPT ANYWAY. It used to
/// say both forms reached `walkers::expression::Context::evaluate`'s fallback
/// arm, which errored, which `walkers::style::Filter::matches` turned into
/// `false` -- so the layer drew nothing while logging a warning nobody reads.
/// That was true until walkers implemented both operators on 2026-08-27; a
/// style carrying the legacy spelling now evaluates correctly, and this
/// function is no longer compensating for anything.
///
/// It stays because normalisation was always the better justification: the
/// output is committed source that a human edits by hand, and one spelling per
/// predicate is worth more than preserving whichever the input happened to use.
/// That is the same reason legacy `{"stops": [...]}` is rewritten to a modern
/// expression here rather than passed through.
fn rewrite_filter(filter: &Value, report: &mut Report) -> Value {
    let Value::Array(items) = filter else {
        return filter.clone();
    };
    let Some(operator) = items.first().and_then(Value::as_str) else {
        return filter.clone();
    };

    match operator {
        "!in" => {
            report.not_in_filters_rewritten += 1;
            let mut inner = vec![json!("in")];
            inner.extend(items.iter().skip(1).cloned());
            json!(["!", Value::Array(inner)])
        }
        "none" => {
            report.not_in_filters_rewritten += 1;
            let mut inner = vec![json!("any")];
            inner.extend(items.iter().skip(1).map(|i| rewrite_filter(i, report)));
            json!(["!", Value::Array(inner)])
        }
        "all" | "any" => {
            let mut rewritten = vec![json!(operator)];
            rewritten.extend(items.iter().skip(1).map(|i| rewrite_filter(i, report)));
            Value::Array(rewritten)
        }
        _ => filter.clone(),
    }
}

/// Rewrite a `text-field` into an expression the evaluator can answer.
///
/// A literal `"{name}"` is not a token to `walkers::expression::Context`; it is
/// a string, and it evaluates to itself, so the map draws `{name}` under every
/// city. Every `text-field` in both CARTO inputs is either a single `{name}` or
/// `{name_en}` token or a zoom stop set over exactly those two, and all of them
/// collapse to the same coalesce.
///
/// `name_en` is genuinely present in OpenMapTiles and genuinely localised --
/// the corpus carries it on all nine name-bearing layers, alongside `name:en`
/// and roughly eighty other `name:xx` variants.
fn rewrite_text_field(value: &Value, layer_id: &str) -> Result<Value, Error> {
    let name_coalesce = json!(["coalesce", ["get", "name_en"], ["get", "name"]]);

    let tokens: BTreeSet<String> = match value {
        Value::String(text) => {
            single_token(text)
                .map(|t| BTreeSet::from([t]))
                .ok_or_else(|| Error::UnhandledTextField {
                    layer: layer_id.to_owned(),
                    found: value.to_string(),
                })?
        }
        Value::Object(object) if object.contains_key("stops") => {
            let stops = object
                .get("stops")
                .and_then(Value::as_array)
                .ok_or_else(|| Error::UnhandledTextField {
                    layer: layer_id.to_owned(),
                    found: value.to_string(),
                })?;
            let mut found = BTreeSet::new();
            for stop in stops {
                let output = stop
                    .as_array()
                    .and_then(|pair| pair.get(1))
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::UnhandledTextField {
                        layer: layer_id.to_owned(),
                        found: value.to_string(),
                    })?;
                found.insert(
                    single_token(output).ok_or_else(|| Error::UnhandledTextField {
                        layer: layer_id.to_owned(),
                        found: value.to_string(),
                    })?,
                );
            }
            found
        }
        // Already an expression. Leave it alone.
        Value::Array(_) => return Ok(value.clone()),
        _ => {
            return Err(Error::UnhandledTextField {
                layer: layer_id.to_owned(),
                found: value.to_string(),
            });
        }
    };

    let names: BTreeSet<&str> = BTreeSet::from(["name", "name_en"]);
    if tokens.iter().all(|t| names.contains(t.as_str())) {
        return Ok(name_coalesce);
    }
    if tokens.len() == 1 {
        let only = tokens
            .iter()
            .next()
            .expect("a one-element set has one element");
        return Ok(json!(["get", only]));
    }
    Err(Error::UnhandledTextField {
        layer: layer_id.to_owned(),
        found: value.to_string(),
    })
}

/// The single `{token}` a string consists of, if that is all it consists of.
fn single_token(text: &str) -> Option<String> {
    let inner = text.strip_prefix('{')?.strip_suffix('}')?;
    if inner.is_empty() || inner.contains('{') || inner.contains('}') {
        return None;
    }
    Some(inner.to_owned())
}

/// Rewrite every legacy `{"stops": …}` function under `value` into a modern
/// expression, collapsing constant ones to scalars.
///
/// A stop set is a JSON object, and `walkers::style::Float` and
/// `walkers::style::Color` both require the evaluated result to be a number or
/// a string. An object reaches neither: it falls through
/// `walkers::expression::Context::evaluate`'s primitive arm unchanged, fails
/// the type check, and the property silently takes its fallback -- `0.5` for a
/// float, magenta for a colour.
///
/// Every stop set in both CARTO inputs is zoom-driven; none has a `property`.
fn rewrite_stop_sets(
    value: &Value,
    property: &str,
    layer_id: &str,
    report: &mut Report,
) -> Result<Value, Error> {
    match value {
        Value::Object(object) if object.contains_key("stops") => {
            let stops = object
                .get("stops")
                .and_then(Value::as_array)
                .ok_or_else(|| Error::UnhandledStopSet {
                    layer: layer_id.to_owned(),
                    found: value.to_string(),
                })?;

            let mut pairs = Vec::with_capacity(stops.len());
            for stop in stops {
                let pair = stop.as_array().filter(|p| p.len() == 2).ok_or_else(|| {
                    Error::UnhandledStopSet {
                        layer: layer_id.to_owned(),
                        found: value.to_string(),
                    }
                })?;
                // A `property`-driven stop set has a non-numeric input and a
                // different modern form. None exists in either input; refuse
                // rather than mis-translate one that appears later.
                if !pair[0].is_number() || object.contains_key("property") {
                    return Err(Error::UnhandledStopSet {
                        layer: layer_id.to_owned(),
                        found: value.to_string(),
                    });
                }
                pairs.push((pair[0].clone(), pair[1].clone()));
            }
            if pairs.is_empty() {
                return Err(Error::UnhandledStopSet {
                    layer: layer_id.to_owned(),
                    found: value.to_string(),
                });
            }

            // Constant, or a single stop. A one-stop `interpolate` is worse
            // than useless: `walkers::expression` needs two stops to form a
            // window and errors out on exactly one, so the property would take
            // its fallback instead of the value written here.
            let constant = pairs.iter().all(|(_, out)| *out == pairs[0].1);
            if constant {
                report.stop_sets_collapsed_to_scalars += 1;
                return Ok(pairs.swap_remove(0).1);
            }

            report.stop_sets_to_expressions += 1;
            let interpolatable = pairs.iter().all(|(_, out)| {
                out.is_number() || (out.is_string() && is_color_property(property))
            });

            let mut expression = if interpolatable {
                // `base` is MapLibre's exponential interpolation base.
                // `walkers::expression` ignores the interpolation type and
                // always lerps linearly; the type is emitted because it is what
                // the specification wants, not because anything reads it.
                let interpolation = match object.get("base").and_then(Value::as_f64) {
                    Some(base) => json!(["exponential", base]),
                    None => json!(["linear"]),
                };
                vec![json!("interpolate"), interpolation, json!(["zoom"])]
            } else {
                // Arrays and non-colour strings cannot be interpolated. `step`
                // is the specification's answer and matches what MapLibre does
                // with a legacy interval function.
                let mut stepped = vec![json!("step"), json!(["zoom"]), pairs[0].1.clone()];
                for (input, output) in pairs.iter().skip(1) {
                    stepped.push(input.clone());
                    stepped.push(output.clone());
                }
                return Ok(Value::Array(stepped));
            };
            for (input, output) in pairs {
                expression.push(input);
                expression.push(output);
            }
            Ok(Value::Array(expression))
        }
        // Recurse: a stop set can sit inside an array-valued property.
        Value::Array(items) => {
            let mut rewritten = Vec::with_capacity(items.len());
            for item in items {
                rewritten.push(rewrite_stop_sets(item, property, layer_id, report)?);
            }
            Ok(Value::Array(rewritten))
        }
        _ => Ok(value.clone()),
    }
}

/// Whether a paint property's string values are colours, and so interpolatable.
///
/// `text-transform` is the counter-example that makes this necessary: its stop
/// outputs are strings like `"uppercase"`, and lerping those as colours is a
/// parse error.
fn is_color_property(property: &str) -> bool {
    property.ends_with("-color")
}

// ── The checker ─────────────────────────────────────────────────────────────

/// One thing wrong with a finished style document.
#[derive(Debug, PartialEq, Eq)]
pub struct Finding(pub String);

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// What a converted style is expected to contain.
pub struct Expectation {
    /// How many layers the upstream document had.
    pub input_layers: usize,
    /// Every `(source-layer, reason)` deliberately dropped, enumerated by name.
    pub deliberate_drops: Vec<(String, String)>,
}

/// Judge a finished style document on its own terms.
///
/// Deliberately knows nothing about the converter: it re-derives the expected
/// layer count from `expectation`, re-reads every `source-layer` out of the
/// output, and checks the phase tags are present and well-formed. Everything it
/// concludes it concludes from the document in front of it.
///
/// Returns every finding, not just the first -- a report naming one problem
/// invites a fix-and-rerun loop that hides the rest.
pub fn check(style: &Value, expectation: &Expectation) -> Vec<Finding> {
    let mut findings = Vec::new();

    let Some(layers) = style.get("layers").and_then(Value::as_array) else {
        findings.push(Finding("the style has no `layers` array".into()));
        return findings;
    };

    // 1. Every layer is accounted for: `upstream - dropped + added`.
    //
    //    The original form was `upstream - dropped`, which assumed our styles
    //    are a strict subset of CARTO's. They stopped being one when the first
    //    layer CARTO never styled was written by hand. The intent -- nothing is
    //    silently lost, and now nothing is silently gained either -- survives
    //    the change, and this form is strictly stronger: an addition that does
    //    not declare itself in `squallar:added-layers` makes the count
    //    disagree, so a hand edit cannot quietly enlarge the style.
    //
    //    A layer that fills an upstream slot with our own styling is neither a
    //    drop nor an addition -- it is recorded in `squallar:restored-layers`
    //    and does not move this count.
    let dropped_layers = layers_drawing_from(style, &expectation.deliberate_drops);
    let added_layers = declared_additions(style);
    let expected = expectation.input_layers - dropped_layers + added_layers;
    if layers.len() != expected {
        let dropped_detail = if expectation.deliberate_drops.is_empty() {
            "none".to_owned()
        } else {
            expectation
                .deliberate_drops
                .iter()
                .map(|(sl, why)| format!("`{sl}` ({why})"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        findings.push(Finding(format!(
            "layer count is {}, expected {} ({} upstream minus {} deliberately dropped ({}) \
             plus {} declared in `squallar:added-layers`). An addition that is not declared \
             there, or a declaration with no layer behind it, lands here.",
            layers.len(),
            expected,
            expectation.input_layers,
            dropped_layers,
            dropped_detail,
            added_layers,
        )));
    }

    // 1b. And each of those declarations points at a real layer, with a reason.
    findings.extend(declaration_findings(style, layers));

    for (index, layer) in layers.iter().enumerate() {
        let id = layer
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("<no id>")
            .to_owned();

        // 2. Every `source-layer` is one of the sixteen. Exhaustive, not a
        //    spot check: every layer in the array is visited.
        if let Some(source_layer) = layer.get("source-layer").and_then(Value::as_str) {
            if ABSENT_FROM_OMT.contains(&source_layer) {
                findings.push(Finding(format!(
                    "layer `{id}` (index {index}) draws from `{source_layer}`, which OpenMapTiles \
                     does not carry in any form"
                )));
            } else if !OMT_SOURCE_LAYERS.contains(&source_layer) {
                findings.push(Finding(format!(
                    "layer `{id}` (index {index}) draws from `{source_layer}`, which is not one \
                     of the sixteen OpenMapTiles source layers"
                )));
            }
            // 3. Nothing deliberately dropped came back.
            if let Some((_, why)) = expectation
                .deliberate_drops
                .iter()
                .find(|(sl, _)| sl == source_layer)
            {
                findings.push(Finding(format!(
                    "layer `{id}` (index {index}) draws from `{source_layer}`, which is on the \
                     deliberate-drop list ({why})"
                )));
            }
        }

        // 4. Every layer is tagged with a phase, and the tag matches its type.
        let phase = layer
            .get("metadata")
            .and_then(|m| m.get(PHASE_KEY))
            .and_then(Value::as_str);
        let layer_type = layer.get("type").and_then(Value::as_str).unwrap_or("");
        match phase {
            None => findings.push(Finding(format!(
                "layer `{id}` (index {index}) carries no `{PHASE_KEY}` tag"
            ))),
            Some(tag) if tag != Phase::of_layer_type(layer_type).as_str() => {
                findings.push(Finding(format!(
                    "layer `{id}` (index {index}) is a `{layer_type}` tagged `{tag}`, expected \
                     `{}`",
                    Phase::of_layer_type(layer_type).as_str()
                )));
            }
            Some(_) => {}
        }

        // 5. No legacy construct the expression evaluator cannot answer.
        for legacy in legacy_constructs(layer) {
            findings.push(Finding(format!(
                "layer `{id}` (index {index}) still contains {legacy}"
            )));
        }
    }

    findings
}

/// The metadata key each hand-written addition declares itself under.
pub const ADDED_KEY: &str = "squallar:added-layers";

/// The metadata key a layer filling an upstream slot with our own styling uses.
pub const RESTORED_KEY: &str = "squallar:restored-layers";

/// How many layers the style declares it added beyond the upstream document.
///
/// Read off the document, like every other input to [`check`], so the expected
/// count is derived from what the style says about itself rather than from a
/// number kept somewhere else that can drift away from it.
fn declared_additions(style: &Value) -> usize {
    style
        .get("metadata")
        .and_then(|m| m.get(ADDED_KEY))
        .and_then(Value::as_array)
        .map_or(0, Vec::len)
}

/// Every declared addition and restoration names a layer that exists and says
/// why it is there.
///
/// Without this the count in [`check`] is gameable from the wrong side: a bare
/// `squallar:added-layers` entry raises the expected total by one, so an
/// undeclared layer could be "declared" by an entry naming nothing at all. Each
/// declaration has to point at a real layer and carry a reason.
fn declaration_findings(style: &Value, layers: &[Value]) -> Vec<Finding> {
    let mut findings = Vec::new();
    let ids: Vec<&str> = layers
        .iter()
        .filter_map(|l| l.get("id").and_then(Value::as_str))
        .collect();

    for key in [ADDED_KEY, RESTORED_KEY] {
        let Some(entries) = style
            .get("metadata")
            .and_then(|m| m.get(key))
            .and_then(Value::as_array)
        else {
            continue;
        };
        for entry in entries {
            let id = entry.get("id").and_then(Value::as_str);
            match id {
                None => findings.push(Finding(format!("an entry in `{key}` names no `id`"))),
                Some(id) if !ids.contains(&id) => findings.push(Finding(format!(
                    "`{key}` declares `{id}`, but no layer by that id exists"
                ))),
                Some(_) => {}
            }
            if !entry
                .get("reason")
                .and_then(Value::as_str)
                .is_some_and(|r| !r.trim().is_empty())
            {
                findings.push(Finding(format!(
                    "`{key}` entry `{}` carries no reason",
                    id.unwrap_or("<no id>")
                )));
            }
        }
    }
    findings
}

/// How many layers of the *upstream* document the drop list accounts for.
///
/// Read off the output's own record of where each layer came from, so the
/// expected count is derived rather than asserted.
fn layers_drawing_from(style: &Value, drops: &[(String, String)]) -> usize {
    style
        .get("metadata")
        .and_then(|m| m.get("squallar:dropped-layers"))
        .and_then(Value::as_array)
        .map(|dropped| {
            dropped
                .iter()
                .filter(|d| {
                    d.get("source-layer")
                        .and_then(Value::as_str)
                        .is_some_and(|sl| drops.iter().any(|(name, _)| name == sl))
                })
                .count()
        })
        .unwrap_or(0)
}

/// Legacy constructs left in a layer, named.
fn legacy_constructs(layer: &Value) -> Vec<String> {
    let mut found = Vec::new();
    walk_for_legacy(layer, &mut found);
    found.sort();
    found.dedup();
    found
}

fn walk_for_legacy(value: &Value, found: &mut Vec<String>) {
    match value {
        Value::Object(object) => {
            if object.contains_key("stops") {
                found.push("a legacy `stops` function".to_owned());
            }
            for nested in object.values() {
                walk_for_legacy(nested, found);
            }
        }
        Value::Array(items) => {
            match items.first().and_then(Value::as_str) {
                Some("!in") => found.push("a legacy `!in` filter".to_owned()),
                Some("none") => found.push("a legacy `none` filter".to_owned()),
                _ => {}
            }
            for item in items {
                walk_for_legacy(item, found);
            }
        }
        Value::String(text) if single_token(text).is_some() => {
            found.push(format!("an unexpanded `{text}` token"));
        }
        _ => {}
    }
}
