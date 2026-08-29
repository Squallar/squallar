//! The terrain hillshade layer: its identity and toggle, and the remap that
//! turns the archive's gdaldem grey into an overlay the basemap shows through.
//!
//! The layer is a streaming-tile layer like `CityLabels`: the handler here is
//! toggle state and identity only, and the pixels move through the same
//! machinery as the basemap — `tiles::MapTileState` owns the source,
//! `tile_source` fetches and decodes, `ui_map_pane`'s `Terrain` arm draws.
//! What is terrain-specific is the **remap**: the archive stores gdaldem
//! hillshade grey (flat ground 181, shadows darker, lit slopes brighter), and
//! painting that opaquely would replace the basemap's palette with grey. The
//! remap spends alpha only where there is relief, so the basemap stays the
//! ground truth in both themes and the hillshade reads as lighting on top of
//! it. One signed remap, theme-independent by design; if dark and light ever
//! want different treatment, that is a follow-up with its own numbers.

use std::sync::Arc;

use squallar_source::controls::{ControlEffect, ControlItem, ControlUpdate, ControlValue};
use squallar_source::handler::{
    FetchPayload, OverlayItem, PaneMut, PaneRef, PaneToggle, RenderMode, SourceHandler, Surface,
};
use squallar_source::id::{LayerId, known};
use squallar_source::time::TimeAxis;

// ---------------------------------------------------------------------------
// The remap
// ---------------------------------------------------------------------------

/// The grey level gdaldem writes for flat ground. A convention of the tool,
/// not a tunable — and pinned by MEASUREMENT, not formula: the flat bulk of
/// the committed Kansas fixture sits at 180..=182 (VP8 wobble around it), and
/// that agrees with gdaldem's flat-surface value `255 · sin(45°) ≈ 180.3` at
/// the default sun altitude. (An earlier version of this comment derived it as
/// `221 · cos(zenith)`, which gives 156 — a wrong formula beside a right
/// constant.)
pub const HILLSHADE_FLAT: u8 = 181;

/// How far a grey level may sit from [`HILLSHADE_FLAT`] and still be treated
/// as flat (fully transparent).
///
/// **Measured, not defensive.** The archive's tiles are *lossy* VP8 WebP, and
/// the encoder's RGB→YUV→RGB round trip cannot even represent 181: a
/// flat-farmland tile (the committed z10 224/395 fixture, Kansas) decodes with
/// **zero** pixels at 181 — its histogram jumps 180 → 182, with 89.5% of the
/// tile on those two values. Without the tolerance every acre of flat ground
/// would carry a one-level black-or-white speckle at alpha
/// [`HILLSHADE_ALPHA_GAIN`], a uniform grey wash the design promised not to
/// spend. Two levels covers the wobble observed (179..=183 over the flat
/// bulk); real relief starts well past it.
pub const HILLSHADE_FLAT_TOLERANCE: u8 = 2;

/// Alpha per grey level of relief past [`HILLSHADE_FLAT_TOLERANCE`], clamped
/// at 255 — full ink from |v − 181| ≥ 87.
///
/// Chosen against the archive's own range: its global extremes are ~130 (deep
/// shadow) and ~219 (lit slope), which land at alpha 147 (~58%) and 108
/// (~42%) — strong enough to read as terrain, thin enough that the basemap's
/// colours stay legible under it. Steep-canyon tiles reach 0 and 255 and
/// saturate, which is what a canyon wall should do. Gain 4 pushed ordinary
/// mountainsides past 75% ink (paint, not lighting); gain 2 left the ~50-level
/// relief of the Plains states at ~30% (invisible at arm's length).
pub const HILLSHADE_ALPHA_GAIN: u16 = 3;

/// Grey levels at or below this are NODATA, not shadow, and draw nothing.
///
/// `gdaldem hillshade` reserves 0 for nodata and emits 1..=255 for real
/// shading, so 0 is convention. The margin above it absorbs the archive's
/// lossy VP8 ringing at nodata boundaries — an encoder smearing a 0-block
/// against its neighbours produces a few near-zero levels that are just as
/// much nodata as the block itself.
///
/// **Found on the glass, not in review.** The archive's z0-z6 tiles carry
/// horizontal nodata stripes (a build defect in the global mosaic: the
/// sharded VRT's latitude-band shards were combined without a shared target
/// grid, leaving seam rows that widen as each lower zoom downsamples them —
/// a z2 tile measures 72% zeros). Without this guard the remap painted every
/// stripe as OPAQUE BLACK (0 is 181 levels of "relief"), which the user saw
/// as black bars across the continent. The guard makes nodata transparent;
/// what it cannot do is restore the shading the stripes destroyed — that
/// needs the archive rebuilt on a fixed mosaic. The cost of the guard on real
/// data is nil in practice: genuine v <= 5 shadow exists only in the deepest
/// canyon tiles, a handful of already-saturated pixels.
pub const HILLSHADE_NODATA_CEILING: u8 = 5;

/// One hillshade grey level as an overlay pixel, **unmultiplied RGBA**.
///
/// Nodata ([`HILLSHADE_NODATA_CEILING`]) draws nothing. Shadow (v below flat)
/// is black, lit slope (v above flat) is white, and the alpha is the relief
/// itself: |v − 181| less the flat tolerance, scaled by
/// [`HILLSHADE_ALPHA_GAIN`]. Flat ground is fully transparent — the layer
/// spends pixels only where there is relief.
const fn remap_hillshade_pixel(v: u8) -> [u8; 4] {
    if v <= HILLSHADE_NODATA_CEILING {
        return [0, 0, 0, 0];
    }
    let relief = v as i16 - HILLSHADE_FLAT as i16;
    let past_flat = relief
        .unsigned_abs()
        .saturating_sub(HILLSHADE_FLAT_TOLERANCE as u16);
    if past_flat == 0 {
        return [0, 0, 0, 0];
    }
    let alpha = past_flat.saturating_mul(HILLSHADE_ALPHA_GAIN);
    let alpha = if alpha > 255 { 255 } else { alpha as u8 };
    if relief < 0 {
        [0, 0, 0, alpha]
    } else {
        [255, 255, 255, alpha]
    }
}

/// Remap one decoded hillshade tile (RGBA, as `image` hands it over) into the
/// overlay the pane draws.
///
/// The luminance is read from the **red channel**: the tiles are grey by
/// construction, and to within the lossy encoder's chroma rounding (green can
/// sit one level under red — measured on the committed fixture) the channels
/// agree; one channel read deterministically beats averaging three.
///
/// Runs once per tile at decode time — on the IO runtime's blocking pool on
/// native, under the wasm pump's decode budget on the web — never per frame.
pub fn remap_hillshade(size: [usize; 2], rgba: &[u8]) -> egui::ColorImage {
    debug_assert_eq!(size[0] * size[1] * 4, rgba.len());
    let mut remapped = vec![0u8; rgba.len()];
    for (out, px) in remapped.chunks_exact_mut(4).zip(rgba.chunks_exact(4)) {
        out.copy_from_slice(&remap_hillshade_pixel(px[0]));
    }
    egui::ColorImage::from_rgba_unmultiplied(size, &remapped)
}

/// Decode one WebP (or any image the header declared) hillshade tile body and
/// remap it. Split from the texture upload so it is testable without an egui
/// context; `tile_source::decode_archive_tile` does the upload.
pub(crate) fn decode_hillshade_tile(bytes: &[u8]) -> Result<egui::ColorImage, String> {
    let reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|error| format!("reading a hillshade tile: {error}"))?;
    let decoded = reader
        .decode()
        .map_err(|error| format!("decoding a hillshade tile: {error}"))?
        .to_rgba8();
    let size = [decoded.width() as usize, decoded.height() as usize];
    Ok(remap_hillshade(size, decoded.as_flat_samples().as_slice()))
}

// ---------------------------------------------------------------------------
// The handler
// ---------------------------------------------------------------------------

/// Toggle state only: the tiles are fetched, remapped and drawn by this
/// crate's own tile machinery (`tiles::MapTileState::terrain`, the `Terrain`
/// arm of `ui_map_pane`'s layer walk).
pub(crate) struct TerrainHandler {
    pub enabled: bool,
}

impl TerrainHandler {
    pub fn new() -> Self {
        // OFF by default: switching a new layer on for every existing user's
        // map is a product decision nobody made.
        Self { enabled: false }
    }
}

impl SourceHandler for TerrainHandler {
    fn id(&self) -> LayerId {
        known::TERRAIN
    }

    /// The DEM is a 2021 release and the hillshade is a pure function of it:
    /// the ground does not move with the clock.
    fn time_axis(&self) -> TimeAxis {
        TimeAxis::Live
    }
    fn surface(&self) -> Surface {
        Surface::Ground
    }
    /// Immediately above the base tiles (which draw before the layer walk) and
    /// below every registered layer — the lowest weight in the registry,
    /// under Gmgsi's 5.
    fn draw_order_weight(&self) -> u32 {
        2
    }
    fn display_name(&self) -> &str {
        "Terrain"
    }
    fn render_mode(&self) -> RenderMode {
        RenderMode::Tile
    }
    fn default_enabled(&self) -> bool {
        false
    }
    fn is_enabled(&self, pane: &PaneRef<'_>) -> bool {
        PaneToggle::is_on(pane, self.enabled)
    }
    fn set_enabled(&mut self, enabled: bool, pane: &mut PaneMut<'_>) {
        if !PaneToggle::set(pane, enabled) {
            self.enabled = enabled;
        }
    }

    fn data_generation(&self) -> u64 {
        0
    }
    fn has_data(&self, _pane: &PaneRef<'_>) -> bool {
        true
    }
    fn is_fetching(&self) -> bool {
        false
    }
    fn set_fetching(&mut self, _fetching: bool, _pane: &PaneRef<'_>) {}
    fn fetch_time(&self) -> Option<web_time::Instant> {
        None
    }

    fn apply_fetch_result(&mut self, _result: FetchPayload, _pane: &PaneRef<'_>) {}
    fn retain_selections(&self, _selections: &mut Vec<Arc<dyn OverlayItem>>, _pane: &PaneRef<'_>) {}

    fn controls(&self, pane: &PaneRef<'_>) -> Vec<ControlItem> {
        vec![ControlItem::Toggle {
            id: "enabled",
            label: "Terrain".to_string(),
            enabled: self.is_enabled(pane),
        }]
    }

    fn apply_control(&mut self, update: &ControlUpdate, pane: &mut PaneMut<'_>) -> ControlEffect {
        if update.id == "enabled"
            && let ControlValue::Bool(val) = update.value
            && !PaneToggle::set(pane, val)
        {
            self.enabled = val;
        }
        ControlEffect::None
    }

    // Per-pane state: this layer's only per-pane fact is whether the pane
    // draws it, so its state IS the toggle — the same shape as CityLabels.

    fn create_pane_state(&self, enabled: bool) -> Option<FetchPayload> {
        PaneToggle::create(enabled)
    }

    fn deserialize_pane_state(
        &self,
        value: serde_json::Value,
        enabled: bool,
    ) -> Option<FetchPayload> {
        PaneToggle::restore(&value, enabled)
    }

    fn serialize_pane_state(&self, state: &dyn std::any::Any) -> serde_json::Value {
        PaneToggle::save(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One real tile out of the published archive: z10 224/395, flat Kansas
    /// farmland with a river valley through it — flat ground, shadow and lit
    /// slope in 666 bytes. Fetched 2026-08-29 from
    /// `tiles.squallar.app/terrain/4ca64469750e-20260829/`.
    const FIXTURE: &[u8] = include_bytes!("../testdata/terrain-hillshade-z10-224-395.webp");

    fn decoded_fixture_grey() -> (usize, Vec<u8>) {
        let decoded = image::ImageReader::new(std::io::Cursor::new(FIXTURE))
            .with_guessed_format()
            .expect("the fixture has a readable container")
            .decode()
            .expect("the fixture decodes")
            .to_rgba8();
        assert_eq!((decoded.width(), decoded.height()), (256, 256));
        let grey: Vec<u8> = decoded.pixels().map(|p| p.0[0]).collect();
        (decoded.width() as usize, grey)
    }

    /// The committed fixture is not uniform — the non-vacuity half of every
    /// pin below. A uniform tile would make "flat is transparent" pass with a
    /// remap that made *everything* transparent.
    #[test]
    fn the_fixture_carries_flat_ground_shadow_and_lit_slope() {
        let (_, grey) = decoded_fixture_grey();
        let flat = grey
            .iter()
            .filter(|&&v| {
                (i16::from(v) - i16::from(HILLSHADE_FLAT)).unsigned_abs()
                    <= u16::from(HILLSHADE_FLAT_TOLERANCE)
            })
            .count();
        let shadow = grey
            .iter()
            .filter(|&&v| v < HILLSHADE_FLAT - HILLSHADE_FLAT_TOLERANCE)
            .count();
        let lit = grey
            .iter()
            .filter(|&&v| v > HILLSHADE_FLAT + HILLSHADE_FLAT_TOLERANCE)
            .count();
        assert!(flat > 50_000, "flat Kansas is mostly flat: {flat}");
        assert!(shadow > 100, "the river valley shades: {shadow}");
        assert!(lit > 100, "its far slope lights: {lit}");
    }

    /// The remap of the real tile: flat pixels transparent, a known shadow
    /// pixel black-with-alpha, a known lit pixel white-with-alpha. The
    /// coordinates and grey levels are pinned from the `image` crate's own
    /// decode of the committed bytes, so a decoder change moves this test
    /// rather than the map.
    #[test]
    fn the_fixture_remap_is_pinned_pixel_by_pixel() {
        let (width, grey) = decoded_fixture_grey();
        let rgba: Vec<u8> = grey.iter().flat_map(|&v| [v, v, v, 255]).collect();
        let remapped = remap_hillshade([width, grey.len() / width], &rgba);

        let at = |x: usize, y: usize| (grey[y * width + x], remapped.pixels[y * width + x]);

        // A flat pixel: within tolerance of 181, fully transparent.
        let (v, px) = at(0, 0);
        assert!(
            (i16::from(v) - i16::from(HILLSHADE_FLAT)).unsigned_abs()
                <= u16::from(HILLSHADE_FLAT_TOLERANCE),
            "pin drifted: (0,0) is no longer flat (grey {v})"
        );
        assert_eq!(px, egui::Color32::TRANSPARENT);

        // The darkest pixel in the tile: black, with the alpha the gain says.
        let (dark_idx, &dark_v) = grey
            .iter()
            .enumerate()
            .min_by_key(|&(_, &v)| v)
            .expect("the tile has pixels");
        assert_eq!(dark_v, 169, "pin drifted: the darkest grey moved");
        let expected_alpha =
            (u16::from(HILLSHADE_FLAT) - u16::from(dark_v) - u16::from(HILLSHADE_FLAT_TOLERANCE))
                * HILLSHADE_ALPHA_GAIN;
        assert_eq!(
            remapped.pixels[dark_idx],
            egui::Color32::from_black_alpha(expected_alpha as u8),
            "the deepest shadow is black at alpha {expected_alpha}"
        );

        // The brightest pixel: white, with the alpha the gain says.
        let (lit_idx, &lit_v) = grey
            .iter()
            .enumerate()
            .max_by_key(|&(_, &v)| v)
            .expect("the tile has pixels");
        assert_eq!(lit_v, 192, "pin drifted: the brightest grey moved");
        let expected_alpha =
            (u16::from(lit_v) - u16::from(HILLSHADE_FLAT) - u16::from(HILLSHADE_FLAT_TOLERANCE))
                * HILLSHADE_ALPHA_GAIN;
        assert_eq!(
            remapped.pixels[lit_idx],
            egui::Color32::from_white_alpha(expected_alpha as u8),
            "the brightest slope is white at alpha {expected_alpha}"
        );
    }

    /// The control the fixture pin needs: a uniform-181 tile remaps to fully
    /// transparent, every pixel — alongside the fixture being non-uniform
    /// above, this is what makes "flat is transparent" falsifiable.
    #[test]
    fn a_uniform_flat_tile_remaps_to_nothing() {
        let rgba: Vec<u8> = std::iter::repeat_n([181u8, 181, 181, 255], 64)
            .flatten()
            .collect();
        let remapped = remap_hillshade([8, 8], &rgba);
        assert!(
            remapped
                .pixels
                .iter()
                .all(|&px| px == egui::Color32::TRANSPARENT),
            "flat ground must spend no pixels"
        );
    }

    /// The whole transfer curve at its edges: nodata is transparent up to its
    /// ceiling and saturated black one level past it, the flat band is exactly
    /// 181 ± tolerance, the first level past the band inks at the gain, and
    /// the lit extreme clamps short of the gain's ceiling (255 → alpha 216).
    ///
    /// The nodata pins replace an earlier `0 → alpha 255` pin: the archive's
    /// low zooms carry nodata stripes that painted as opaque black bars on the
    /// glass, and 0 was never shadow — gdaldem reserves it. Both sides of the
    /// boundary are pinned so the ceiling can neither widen nor collapse
    /// silently.
    #[test]
    fn the_transfer_curve_edges_are_exact() {
        assert_eq!(remap_hillshade_pixel(0), [0, 0, 0, 0], "nodata");
        assert_eq!(
            remap_hillshade_pixel(HILLSHADE_NODATA_CEILING),
            [0, 0, 0, 0],
            "the ceiling itself is still nodata"
        );
        assert_eq!(
            remap_hillshade_pixel(HILLSHADE_NODATA_CEILING + 1),
            [0, 0, 0, 255],
            "one past the ceiling is real, fully saturated shadow"
        );
        assert_eq!(remap_hillshade_pixel(181), [0, 0, 0, 0]);
        assert_eq!(remap_hillshade_pixel(179), [0, 0, 0, 0]);
        assert_eq!(remap_hillshade_pixel(183), [0, 0, 0, 0]);
        assert_eq!(remap_hillshade_pixel(178), [0, 0, 0, 3]);
        assert_eq!(remap_hillshade_pixel(184), [255, 255, 255, 3]);
        assert_eq!(remap_hillshade_pixel(255), [255, 255, 255, 216]);
    }

    /// `decode_hillshade_tile` accepts the real WebP body end to end, and its
    /// answer is the remap of the decode — the two paths cannot drift.
    #[test]
    fn the_webp_body_decodes_and_remaps_end_to_end() {
        let remapped = decode_hillshade_tile(FIXTURE).expect("the fixture decodes");
        assert_eq!(remapped.size, [256, 256]);
        let transparent = remapped
            .pixels
            .iter()
            .filter(|&&px| px == egui::Color32::TRANSPARENT)
            .count();
        assert!(
            transparent > 50_000 && transparent < 65_536,
            "mostly flat, not all flat: {transparent}"
        );
    }

    /// The handler is the CityLabels shape with Terrain's facts: the ledger
    /// spelling, tile render mode, ground surface, and OFF by default.
    #[test]
    fn the_handler_declares_terrains_facts() {
        let handler = TerrainHandler::new();
        assert_eq!(handler.id(), known::TERRAIN);
        assert_eq!(handler.render_mode(), RenderMode::Tile);
        assert_eq!(handler.surface(), Surface::Ground);
        assert!(!handler.default_enabled(), "OFF for existing users' maps");
        assert!(!handler.is_enabled(&PaneRef::bare(0)));
        assert_eq!(handler.time_axis(), TimeAxis::Live);
    }
}
