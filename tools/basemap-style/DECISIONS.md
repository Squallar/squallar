# Basemap style conversion — decisions

What `www/styles/dark.json` and `www/styles/light.json` are, what they cannot
represent, and why. Written so that "the map has no X" is a recorded decision
rather than a bug someone finds in six months.

Dated 2026-08-27. The conversion ran **once**; the two style files are owned
source from that moment on and are edited directly. There is deliberately no
workflow, no scheduled regeneration and no `cmp` gate — anything that
re-asserted upstream's authorship would undo the ownership this seed exists to
establish.

---

## Provenance

| | |
|---|---|
| Repository | <https://github.com/CartoDB/basemap-styles> (`master`) |
| Commit | `64d082a6bc6039b1a0a0a9fb5312330fedd0bba9` |
| Dark input | `mapboxgl/dark-matter.json` — 70,431 bytes, sha256 `fc5bdb44e1d74c0602dd82bba3837b368fe3a96437d0edbbcf96d9dfe96b8a75` |
| Light input | `mapboxgl/positron.json` — 106,887 bytes, sha256 `e1478ad7c5f6aa567667039779a293483f2df8efcb094d0a222a13994fd7147d` |

**Only style documents were fetched.** Two `raw.githubusercontent.com` requests
and some `api.github.com` metadata. No request was made to
`tiles.basemaps.cartocdn.com` or any other CARTO tile, glyph or sprite
endpoint, and nothing CARTO-rendered is committed.

The licence split matters and is why this is allowed: CARTO's `LICENSE.md`
places the **style code** under BSD-3-Clause and the **visual design** under
CC-BY-4.0, while restricting the **hosted basemap tile service** to enterprise
customers. We converted the code, adopted the design, and take our tiles from
OpenFreeMap.

Attribution the licence asks for, to be surfaced somewhere reachable from the
map (not necessarily on it): **© CARTO**, **© OpenMapTiles**, **©
OpenStreetMap contributors**. OpenFreeMap's own TileJSON asks for the same two
data credits.

The full (labelled) styles were taken, not the `-nolabels` variants, because
labels are now a phase of our own renderer rather than a separate tile source.

---

## Contradictions with the work order

Three things the brief asserted turned out to be false against the tree and the
upstream repository as they stand today. Each was verified, not inferred.

### 1. The renames were already done upstream

The brief called the six Mapbox-Streets-to-OpenMapTiles source-layer renames
"the highest-risk part of this task". **All six fired zero times.** CARTO's
current `dark-matter.json` and `positron.json` already target the OpenMapTiles
schema: their source is
`https://tiles.basemaps.cartocdn.com/vector/carto.streets/v1/tiles.json`, and
the fourteen distinct `source-layer` values they use —

```text
aeroway  boundary  building  housenumber  landcover  landuse  park
place  poi  transportation  transportation_name  water  water_name  waterway
```

— are all already among the sixteen valid OpenMapTiles names. Not one Mapbox
Streets name (`road`, `road_label`, `admin`, `poi_label`, `place_label`,
`airport_label`) appears anywhere in either document. CARTO's own `LICENSE.md`
confirms the intent: it requires products "using maps derived from
OpenMapTiles schema" to credit OpenMapTiles.

The rename table is kept in `SOURCE_LAYER_RENAMES` regardless, and
`the_converter_renames_every_mapbox_streets_source_layer` exercises all six
against synthetic input. Dead code that has never run is not known to work, and
the table's whole purpose is the next person who points this converter at an
older revision.

### 2. Zoom ranges have to be folded into filters

The brief listed "folding `minzoom`/`maxzoom` into filters" among the
transforms that vendoring had made unnecessary. It has not.

`walkers::style::Layer` has **no `minzoom` or `maxzoom` field**, so serde drops
both silently at parse. `walkers::mvt::render` iterates `style.layers` and
consults each layer's `filter` and nothing else — there is no zoom-range gate
anywhere in the vendored crate. Without the fold, all 49 transportation layers,
both building layers and all 15 place-label layers would draw at z0.

So the fold is applied: `minzoom` becomes `[">=", ["zoom"], m]` and `maxzoom`
becomes `["<", ["zoom"], M]`, prepended to the layer's filter inside an `all`.
That is MapLibre's `minzoom <= z < maxzoom` semantics exactly. 84 of 92 layers
carry a range in each theme; the other 8 are unbounded upstream.

`minzoom` and `maxzoom` are **left on the layers as well**. They are correct,
the specification wants them, and if walkers ever honours them the fold merely
re-asserts the same range — the two are idempotent together, not conflicting.

Pinned end-to-end by
`a_folded_zoom_range_gates_the_layer_through_the_real_evaluator`, which
evaluates a real committed filter through `walkers::style::Filter::matches` at a
zoom inside and a zoom below the range, plus a stripped-filter control proving
the rejection came from the zoom clause.

### 3. Nothing can currently read the phase tag

The brief requires each layer tagged `ground` or `label` "so the renderer can
split them", explicitly machine-readable rather than inferred at render time.
The tag is emitted, at `metadata."squallar:phase"` on every layer — but
**`walkers::style::Style` deserialises `layers` and nothing else, and
`walkers::style::Layer` has no `metadata` field**, so serde discards the tag on
the way in. Today nothing can read it.

That is the right place for it anyway: `metadata` is where the MapLibre
specification puts application data, the tag travels with the layer it
describes in the committed source, and it costs the current renderer nothing.
But **the phase split needs a walkers change to become real**, and that crate
was out of scope for this work order. Until then the tag is a well-formed
promise with no consumer.

The split itself is unambiguous and needs no per-layer judgement: `symbol` is
the only MapLibre type that places text, and all 27 symbol layers in both inputs
carry a `text-field` — none is icon-only. So `label` = symbol, `ground` =
everything else, giving **66 ground and 26 label** per theme after the
`housenumber` drop.

---

## Layer accounting

**93 layers in, 92 out**, identically in both themes. One deliberate drop:

| Source layer | Layers | Reason |
|---|---|---|
| `housenumber` | 1 (`housenumber`) | `"text-color"` and `"text-halo-color"` are both `"transparent"` in *both* themes. It costs a full symbol pass over the densest layer in the tile to render nothing. |

The drop is recorded in each style's own `metadata."squallar:dropped-layers"`,
and the verification suite reads the expected output count back off that rather
than being told it.

> **Amended 2026-08-28 — the drop is retired and the styles are 95 layers.**
>
> Everything above is the record of the one-time conversion and stays true of
> it. It is no longer true of the committed styles.
>
> `housenumber` came off `DELIBERATE_DROPS`: the styles carry their own
> housenumber layer, at z17+, in a visible colour. CARTO's `transparent` was a
> minimal-backdrop aesthetic we inherited rather than chose, and these styles
> have been owned source since 2026-08-27. Two layers CARTO never styled were
> added at the same time — `mountain_peak` (the only elevation values in the
> schema) and `aerodrome_label` (METARs and TAFs are issued per aerodrome).
>
> **93 in, 95 out.** The accounting is now `upstream - dropped + added`:
> 93 − 0 + 2 = 95. `housenumber` is not one of the two — CARTO's 93 already
> contained a housenumber layer, so ours fills that slot restyled, recorded
> under `metadata."squallar:restored-layers"`. Phase split is **66 ground and
> 29 label**.
>
> The invariant that replaced "93 in, 92 out" is stronger than it: additions are
> read from the document, so a hand edit that enlarges a style without declaring
> itself fails the check, and so does a declaration with no layer behind it.

### Layers OpenMapTiles cannot satisfy — none were present

The four names the brief flagged as genuinely absent from OpenMapTiles —
`landuse_overlay`, `motorway_junction`, `natural_label`, `structure` — **appear
in neither input**, so no decision about what to do without them was needed.
The converter refuses them rather than dropping them quietly
(`Error::UnsatisfiableSourceLayer`), so if a future edit or a different upstream
introduces one, it fails loudly and this section gets a new row.

### Terrain — nothing expected it

The brief warned that OpenMapTiles carries no `contour`, `hillshade` or DEM
layer, and that any CARTO layer expecting terrain could not be satisfied.
**No such layer exists in either input.** Neither style contains a `hillshade`
or `raster` layer of any kind; the only layer types present are `background`
(1), `fill` (9), `line` (56) and `symbol` (27). The two elevation carriers
OpenMapTiles does have — `mountain_peak` and `aerodrome_label.ele` — are simply
unused by CARTO's styles, and we did not invent layers for them.

Consequence to state plainly: **the basemap has no terrain shading and no
contours.** That is upstream's design, not a loss introduced here.

---

## What cannot be represented

### Typeface

**Out of reach, and not worth what it would cost.** CARTO's styles ask for
Montserrat (`"text-font": ["Montserrat Regular", …]`). Matching it would mean
making Montserrat the app's proportional family, and
`squallar-egui/src/ui_glyphs.rs` pins **34 characters** against that family —
27 in `ICON_GLYPHS` and 7 in `TEXT_GLYPHS` — with a coverage test asserting each
one resolves. Montserrat carries none of the geometric-shape and media-control
glyphs in that inventory, so the swap would redden that test and strip the
chrome's icons.

Labels therefore match CARTO in **colour, size and position, but not
typeface**. `text-font` is left in the committed styles: it is inert (nothing
reads it) and it records what the design intended.

### Glyph PBFs and sprite sheets — none are needed or hosted

This inverts the usual MapLibre expectation, so it is worth stating flatly:
**`walkers::style::Style` deserialises only `layers`.** `glyphs` and `sprite`
are never read and never fetched; text is laid out by egui from a font already
bundled in the binary.

Both top-level keys are therefore **dropped** from the output rather than
carried, because both pointed at `tiles.basemaps.cartocdn.com` — a restricted
CARTO service — and a live URL sitting in a committed file is an invitation for
someone to wire a fetch to it later.
`no_committed_style_references_a_carto_service_url` keeps them out.

Consequence: the 11 `icon-image` references surviving in symbol layouts resolve
to nothing. **No point icons render** — POIs, airports and mountain peaks appear
as text only. Nobody should add a sprite pipeline to fix this without first
deciding they want one.

### Interpolation is always linear, and zoom is an integer

Legacy `{"stops": …}` functions become modern `interpolate` expressions, with
`["exponential", base]` emitted when upstream set a `base` and `["linear"]`
otherwise. But **`walkers::expression` ignores the interpolation type** and
always lerps linearly, so every exponential ramp is flattened in practice.
The type is emitted because the specification wants it, not because anything
reads it.

Each input carries 7 `base`-bearing stop sets, of which 5 reach the output as
`["exponential", …]`; the other two do not survive as interpolations at all —
one is `fill-translate`, whose array values cannot be lerped and which becomes a
`step`, and one is constant and collapses to a scalar.

Separately, `walkers::expression` answers `["zoom"]` with `self.zoom as i64` —
the integer tile zoom. Interpolation between stops is therefore **quantised to
whole zoom levels**; there is no smooth ramp during a pinch.

Neither is worth chasing. Both are properties of the vendored evaluator, and
changing them is a walkers edit.

### Stop sets that cannot be interpolated

`line-dasharray`, `fill-translate` and `text-offset` have array-valued stops,
and `text-transform` has string values that are not colours. None can be lerped.
They are emitted as `["step", ["zoom"], …]`, which is what MapLibre does with a
legacy interval function.

This is cosmetic bookkeeping: **`walkers::style::Paint` reads only**
`background-color`, `fill-color`, `fill-opacity`, `line-width`, `line-color`,
`line-opacity`, `text-color` and `text-halo-color`, and `walkers::style::Layout`
reads only `text-field` and `text-size`. Every other property in the committed
files — including all four above, plus `line-dasharray`, `text-halo-width`,
`symbol-placement` and the rest — is inert. They are kept because the files are
now source that humans read and edit, and deleting the design's own record of
its intent to save bytes is a bad trade.

Consequence: **dashed lines render solid.** Tunnel casings and boundary dashes
lose their pattern.

### Single-stop and constant functions collapse to scalars

Not tidying. `walkers::expression`'s `interpolate` needs two stops to form a
window and errors on exactly one, and a failed `Float` evaluation falls back to
`0.5` (a failed `Color` falls back to magenta). Collapsing is the difference
between the written value and `0.5`. 9 collapsed in dark, 5 in light.

### Zoom-varying label language collapses

Seven `text-field`s per theme were `{"stops": [[8, "{name_en}"], [13,
"{name}"]]}` — English at low zoom, local name at high zoom. All `text-field`s
now become `["coalesce", ["get", "name_en"], ["get", "name"]]`, which is the
brief's specified form and is **always English-preferring**. The zoom-varying
switch is gone.

`name_en` is confirmed present in the data: the 42-tile corpus carries it on all
nine name-bearing layers, alongside `name:en` and roughly eighty other `name:xx`
variants, genuinely localised rather than copied.

### Legacy filter operators

`["!in", …]` becomes `["!", ["in", …]]` and `["none", …]` becomes `["!",
["any", …]]` (one `!in` per theme; no `none`).

**The reason this was written is no longer true, and the rewrite is kept
anyway.** It was a workaround: both legacy forms reached
`walkers::expression::Context::evaluate`'s fallback arm, which errored, and
`walkers::style::Filter::matches` turned that into `false` — so the layer drew
nothing while logging a warning nobody reads. walkers implemented both
operators on 2026-08-27, so that failure mode is gone and a style carrying the
legacy spelling now evaluates correctly.

It stays as **normalisation**, which was always the better justification: this
tool's output is committed source that a human edits by hand, and one spelling
per predicate is worth more than preserving whichever spelling the input
happened to use. Same reasoning as rewriting legacy `{"stops": […]}` to a
modern expression rather than passing it through.

---

## No CI gate reaches this directory, deliberately

`tools/basemap-style` is its own workspace root, so root `cargo
build/test/clippy --workspace` and `cargo fmt --all` all walk straight past it.
**That is not a gap to close.** This tool is run **once, by a human**, and its
output — `www/styles/{dark,light}.json` — is committed as owned source that is
edited by hand from then on. There is no upstream to track and nothing
recurring to re-validate, so a workflow here would be a recurring gate on a
one-time job: cost with no subject.

A `basemap-style.yaml` was written and then deleted for exactly that reason. If
you are about to add one back, the question to answer first is *what changes
that this would catch* — and if the answer is "the converter's own tests", run
them by hand, which is what these four commands are for:

```sh
cargo build --release --manifest-path tools/basemap-style/Cargo.toml
cargo fmt   --manifest-path tools/basemap-style/Cargo.toml -- --check
cargo clippy --all-targets --manifest-path tools/basemap-style/Cargo.toml -- -D warnings
cargo test  --manifest-path tools/basemap-style/Cargo.toml
```

**One thing the deletion does cost, and it is worth knowing.** Those 29 tests
were the only automated check anywhere that the committed `www/styles/*.json`
still deserialise. Nothing replaces that today. If it is wanted, it belongs in
`squallar-egui` — the crate that actually loads them — and not here, on the
principle that a gate belongs at the layer the symptom lives in. A converter's
CI could only ever prove the converter still works, which is not the thing
users would notice breaking.

---

## Reproducing the seed

```sh
curl -sSLO https://raw.githubusercontent.com/CartoDB/basemap-styles/64d082a6bc6039b1a0a0a9fb5312330fedd0bba9/mapboxgl/dark-matter.json
curl -sSLO https://raw.githubusercontent.com/CartoDB/basemap-styles/64d082a6bc6039b1a0a0a9fb5312330fedd0bba9/mapboxgl/positron.json

cargo run --manifest-path tools/basemap-style/Cargo.toml -- \
  --theme dark --name "Squallar Dark" \
  --input dark-matter.json --output www/styles/dark.json \
  --upstream-url  https://github.com/CartoDB/basemap-styles/blob/master/mapboxgl/dark-matter.json \
  --upstream-commit 64d082a6bc6039b1a0a0a9fb5312330fedd0bba9

cargo run --manifest-path tools/basemap-style/Cargo.toml -- \
  --theme light --name "Squallar Light" \
  --input positron.json --output www/styles/light.json \
  --upstream-url  https://github.com/CartoDB/basemap-styles/blob/master/mapboxgl/positron.json \
  --upstream-commit 64d082a6bc6039b1a0a0a9fb5312330fedd0bba9
```

Running this again **overwrites** the owned files and discards every hand-edit
made since. That is the reason it is a documented recipe rather than a workflow.
