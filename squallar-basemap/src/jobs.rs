//! The basemap row: one codec row, so a pass's vector tiles are parsed and
//! tessellated where every other heavy job runs.
//!
//! [`BasemapTilesJob`] carries a style key and the MVT bodies of one pump
//! pass; it answers a [`BasemapTiles`]. Modelled on
//! `squallar_buildings::jobs::BuildingMeshJob` in shape — a request naming a
//! set of tile bodies, a reply that is flat tails over them — and composed
//! **after** it in `squallar_worker::job_registry`, never earlier, because a
//! wire code is an index into the composition and a row inserted anywhere but
//! the end renumbers every code after it.
//!
//! # One job per pass, not one per tile
//!
//! `squallar_web::worker` runs `execute_encoded` synchronously in `onmessage`,
//! one job at a time, and a radar rasterization is 160-190 ms. A per-tile job
//! would queue behind one and make tiles appear LATER, which is the whole
//! objection this row exists to answer. So a pass's tiles cross as ONE job and
//! [`BasemapTilesJob::run`] `par_iter`s across them: eight tiles serial on the
//! frame thread become eight tiles wide on the worker's pool.
//!
//! That amortises the round trip; it does not remove head-of-line blocking,
//! and nothing here can. What removes it is the dispatch gate in
//! `squallar_egui::tile_source`, which posts only when the funnel is idle and
//! otherwise decodes inline exactly as it does today.
//!
//! # The style does not cross
//!
//! The worker runs the *same wasm module* the page runs, so it already has
//! `www/styles/{dark,light}.json` compiled in ([`crate::style`]). Only
//! [`StyleKey`] crosses — a theme bit and the disabled source-layers — and
//! [`style_for`] memoises the built `Style` against it, because
//! `committed_filtered` re-parses ~95 internally-tagged layers whenever the
//! filter is non-empty (0.44-0.70 ms, release) and **the shipping default is
//! non-empty**: `building` and `poi` are off. Per batch that is fine; per tile
//! it would not be.
//!
//! # This row does not time itself, and the reason is a rule
//!
//! An earlier draft had `run` stamp each tile's parse and style micros and
//! carry them home, so that `squallar_egui`'s `VectorPhase` families could not
//! go silent. `offload::tests::no_run_body_reads_a_clock` refused it, and the
//! refusal is right: a `run` that reads a clock produces an output the direct
//! call and the wire cannot agree on, which is exactly what every
//! `..._is_byte_identical_direct_and_via_the_wire` gate exists to prove. A
//! duration is as nondeterministic as an instant.
//!
//! Carrying them anyway would also have been the WRONG figure. The page's
//! phase ledger's denominator is frame-thread cost; folding the worker's
//! microseconds into it would inflate a frame-thread number with work that is
//! no longer on the frame thread — a misattribution worse than a quiet family.
//!
//! So the phases stay page-side and keep their meaning: the page still parses
//! and styles every tile the dispatch gate declines to offload, and charges
//! those to the same families, whose `n` becomes an honest "tiles this frame
//! decoded itself". What makes a zero there readable rather than ambiguous is
//! a COUNT, not a timing family — `squallar_egui::tile_source` reports tiles
//! offloaded beside tiles decoded inline, and a count cannot stop printing.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, OnceLock};

use rayon::prelude::*;
use squallar_source::job::{EncodeCtx, JobCodec, JobCost, JobGeometry, JobOutCodec, JobSpec};
use squallar_source::wire::Reader;
use walkers::{ShapeOrText, Style};

use crate::wire::{TailCursors, Tails, decode_shapes, encode_shapes};

/// This crate's one codec row. Chained after the buildings registry, so its
/// index — and therefore its wire code — is the last one allocated.
pub static JOB_CODECS: &[JobCodec] = &[JobCodec::of::<BasemapTilesJob>()];

/// Which committed style a batch is to be tessellated against.
///
/// Not a `Style` and never a `Style`: the worker has the JSON compiled in, so
/// what crosses is the two inputs that select one. See the module doc.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StyleKey {
    pub is_dark: bool,
    /// OMT source-layers the style excludes — `squallar_egui`'s layer-global
    /// disabled set, whose shipping default is non-empty.
    pub disabled: BTreeSet<String>,
}

/// One vector tile's bytes, at the address they were fetched from.
///
/// `Arc<Vec<u8>>` for the reason `squallar_buildings`' `BuildingTile` is: the
/// native transport moves the request rather than serialising it, so a batch
/// handed to the pool costs a pointer and not a copy of the bodies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TileBody {
    pub z: u8,
    pub x: u32,
    pub y: u32,
    /// The tile's **decompressed** MVT bytes: whatever the archive wrapped
    /// them in has already come off, exactly as `squallar_buildings` takes
    /// them. This row parses protobuf and nothing else.
    pub mvt: Arc<Vec<u8>>,
}

/// Parse and tessellate one pump pass's vector tiles.
#[derive(Debug, PartialEq)]
pub struct BasemapTilesJob {
    pub style: StyleKey,
    /// The pass's tiles, in any order — replies are keyed back by coordinate,
    /// so ordering costs nothing and buys nothing.
    pub tiles: Vec<TileBody>,
}

squallar_source::impl_job_input!(BasemapTilesJob);

/// One tile's styling, or the fact that its body did not parse.
#[derive(Debug)]
pub struct StyledTile {
    pub z: u8,
    pub x: u32,
    pub y: u32,
    /// `None` is a body that did not parse. It is NOT "nothing to draw": the
    /// page re-decodes such a tile through its own inline path rather than
    /// caching a blank, so a refusal here costs a slower tile and never a
    /// wrong one.
    pub shapes: Option<Vec<ShapeOrText>>,
}

/// One batch's answer.
#[derive(Debug)]
pub struct BasemapTiles {
    pub tiles: Vec<StyledTile>,
}

/// The most tiles one request may name.
///
/// A refusal ceiling, not a measured budget. `tile_rx` is
/// `channel(MAX_PARALLEL_DOWNLOADS)` with a single sender and only one batch is
/// outstanding at a time, so staging holds of order thirteen. This is an order
/// of magnitude past the honest case and exists so a doctored count answers
/// `None` rather than reserving its way into an abort on a 1 GiB wasm ceiling.
const MAX_TILES: usize = 256;

/// The most tile bytes one request may carry, 64 MiB.
///
/// Beside [`MAX_TILES`] because the two bound different things: 256 tiles of
/// 185 KB is 47 MB, and 256 tiles is also a perfectly good way to spell one
/// tile of 64 MB. The committed archive's densest body is 185,182 bytes.
const MAX_TILE_BYTES_TOTAL: u64 = 1 << 26;

/// The most disabled source-layers one request may name, and the most bytes
/// one name may be. `squallar_egui`'s shipped toggle table is of order ten
/// rows and an OMT source-layer name is of order ten bytes.
const MAX_DISABLED: usize = 64;
const MAX_NAME_BYTES: usize = 128;

/// The request's fixed prefix: the theme bit, the disabled count, the tile
/// count. Named so the layout is stated in arithmetic as well as in code.
const REQUEST_PREFIX_BYTES: usize = 1 + 4 + 4;

/// A tile's own request header: zoom, column, row, body length.
const TILE_HEADER_BYTES: usize = 1 + 3 * 4;

/// A tile's reply header: zoom, column, row, the parsed flag, the shape count.
const REPLY_TILE_HEADER_BYTES: usize = 1 + 3 * 4 + 1 + 4;

/// The keys [`style_for`] has already built, beside what they built.
///
/// A `Vec` and not a map, deliberately: the population is the two themes times
/// the filters a session actually selects, which is one or two.
type StyleMemo = Vec<(StyleKey, Arc<Style>)>;

/// The built [`Style`] for a key, memoised.
///
/// See the module doc for why this exists rather than calling
/// [`crate::style::committed_filtered`] per job: the shipping default filter
/// is non-empty, and a non-empty filter re-parses the JSON.
///
/// A `Mutex` and not a `OnceLock` because the key is a parameter. Taken once
/// per **job**, on the thread that runs it, before the `par_iter` — never
/// inside it — so on the browser arm this is an uncontended CAS on a worker
/// thread and never a contended wait on the page's.
fn style_for(key: &StyleKey) -> Arc<Style> {
    static BUILT: OnceLock<Mutex<StyleMemo>> = OnceLock::new();
    let cell = BUILT.get_or_init(|| Mutex::new(StyleMemo::new()));
    let mut built = cell.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, style)) = built.iter().find(|(k, _)| k == key) {
        return Arc::clone(style);
    }
    let style = crate::style::committed_filtered(key.is_dark, &key.disabled);
    // Bounded so a doctored stream of keys cannot grow it without limit; see
    // [`StyleMemo`] for why the honest population is one or two.
    if built.len() < 2 * MAX_DISABLED {
        built.push((key.clone(), Arc::clone(&style)));
    }
    style
}

fn saturating_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

impl JobSpec for BasemapTilesJob {
    type In = BasemapTilesJob;
    type Out = BasemapTiles;
    const LABEL: &'static str = "basemap/tiles";
    const COST: JobCost = JobCost::Raster;

    /// `[is_dark u8][disabled u32]`, then per name `[len u32][utf8]`, then
    /// `[tiles u32]`, then per tile `[z u8][x u32][y u32][len u32][body]`.
    fn encode(input: &BasemapTilesJob, _ctx: &EncodeCtx, out: &mut Vec<u8>) {
        out.reserve(REQUEST_PREFIX_BYTES);
        out.push(u8::from(input.style.is_dark));
        out.extend_from_slice(&saturating_u32(input.style.disabled.len()).to_le_bytes());
        for name in &input.style.disabled {
            out.extend_from_slice(&saturating_u32(name.len()).to_le_bytes());
            out.extend_from_slice(name.as_bytes());
        }
        out.extend_from_slice(&saturating_u32(input.tiles.len()).to_le_bytes());
        for tile in &input.tiles {
            out.push(tile.z);
            out.extend_from_slice(&tile.x.to_le_bytes());
            out.extend_from_slice(&tile.y.to_le_bytes());
            out.extend_from_slice(&saturating_u32(tile.mvt.len()).to_le_bytes());
            out.extend_from_slice(&tile.mvt);
        }
    }

    /// The mirror of [`Self::encode`], refusing rather than believing.
    ///
    /// These bytes arrive on a message port from a peer that may be another
    /// build, and a code a *previous* build allocated to nothing lands on
    /// whichever row now holds it. A theme byte that is not a bool, a name or
    /// tile count past its ceiling, a body set past its own, a zoom that is
    /// not a zoom, and trailing bytes are all refused. `JobRequest::from_bytes`
    /// checks trailing bytes only for the `overlay/` rows, so this row checks
    /// its own.
    fn decode(r: &mut Reader<'_>, geo: JobGeometry) -> Option<(BasemapTilesJob, JobGeometry)> {
        let is_dark = match r.u8()? {
            0 => false,
            1 => true,
            _ => return None,
        };
        let declared = r.u32()?;
        let names = r.bounded(declared, 4)?;
        if names > MAX_DISABLED {
            return None;
        }
        let mut disabled = BTreeSet::new();
        for _ in 0..names {
            let len = usize::try_from(r.u32()?).ok()?;
            if len > MAX_NAME_BYTES {
                return None;
            }
            disabled.insert(std::str::from_utf8(r.take(len)?).ok()?.to_owned());
        }

        let declared = r.u32()?;
        let count = r.bounded(declared, TILE_HEADER_BYTES)?;
        if count > MAX_TILES {
            return None;
        }
        let mut tiles = Vec::with_capacity(count);
        let mut bytes_total: u64 = 0;
        for _ in 0..count {
            let z = r.u8()?;
            let x = r.u32()?;
            let y = r.u32()?;
            // A zoom the mercator grid cannot count, and a coordinate off the
            // grid at its own zoom, are both another build's numbers.
            if z > 30 || u64::from(x) >= 1u64 << z || u64::from(y) >= 1u64 << z {
                return None;
            }
            let len = usize::try_from(r.u32()?).ok()?;
            bytes_total = bytes_total.checked_add(len as u64)?;
            if bytes_total > MAX_TILE_BYTES_TOTAL {
                return None;
            }
            tiles.push(TileBody {
                z,
                x,
                y,
                mvt: Arc::new(r.take(len)?.to_vec()),
            });
        }
        r.at_end().then_some(())?;

        Some((
            BasemapTilesJob {
                style: StyleKey { is_dark, disabled },
                tiles,
            },
            geo,
        ))
    }

    /// Build the style once, then parse and tessellate every tile in parallel.
    ///
    /// **The style is built before the `par_iter` and borrowed into it**, so
    /// the memo's lock is taken once per job on one thread — see [`style_for`].
    ///
    /// **A tile that does not parse is `None`, not fatal and not empty.** Most
    /// refusals here would be a corrupt body, and answering an empty shape
    /// list would cache a blank tile that the page would never re-ask for. The
    /// page treats `None` as "decode this one yourself".
    ///
    /// The answer is `Some` even when every tile refused: `None` in this funnel
    /// means "nothing to draw", which is not the same as "this batch went
    /// badly", and the caller that needs the difference reads the tiles.
    ///
    /// The geometry is untouched: shapes are not a raster and the envelope's
    /// `side_ceiling_px` has nothing to say about them.
    fn run(input: &BasemapTilesJob, _geo: &JobGeometry) -> Option<BasemapTiles> {
        let style = style_for(&input.style);
        let tiles = input
            .tiles
            .par_iter()
            .map(|tile| {
                let parsed = match walkers::mvt::parse(&tile.mvt) {
                    Ok(parsed) => parsed,
                    Err(error) => {
                        log::warn!(
                            "basemap: z{}/{}/{} ({} bytes) did not parse: {error}",
                            tile.z,
                            tile.x,
                            tile.y,
                            tile.mvt.len(),
                        );
                        return StyledTile {
                            z: tile.z,
                            x: tile.x,
                            y: tile.y,
                            shapes: None,
                        };
                    }
                };
                StyledTile {
                    z: tile.z,
                    x: tile.x,
                    y: tile.y,
                    shapes: Some(walkers::mvt::styled(&parsed, &style, tile.z)),
                }
            })
            .collect();
        Some(BasemapTiles { tiles })
    }
}

impl JobOutCodec for BasemapTilesJob {
    /// `[tiles u32]`, then per tile `[z u8][x u32][y u32][ok u8][shapes u32]`
    /// followed by that tile's shape descriptors, and the shapes' bulk arrays
    /// as [`crate::wire::Tails`]' four nominated tails.
    ///
    /// Four tails rather than one concatenated buffer for the reason the
    /// buildings row nominates three: head and tails are pushed into the same
    /// `buffers` list and lent identically by `squallar_web::worker`, so
    /// nominating a tail buys no different transport. What it buys is not
    /// building the concatenation at all.
    fn encode_out(v: BasemapTiles, head: &mut Vec<u8>, tails: &mut Vec<Vec<u8>>) {
        let mut bulk = Tails::default();
        head.reserve(4 + v.tiles.len() * REPLY_TILE_HEADER_BYTES);
        head.extend_from_slice(&saturating_u32(v.tiles.len()).to_le_bytes());
        for tile in &v.tiles {
            head.push(tile.z);
            head.extend_from_slice(&tile.x.to_le_bytes());
            head.extend_from_slice(&tile.y.to_le_bytes());
            head.push(u8::from(tile.shapes.is_some()));
            let shapes = tile.shapes.as_deref().unwrap_or(&[]);
            head.extend_from_slice(&saturating_u32(shapes.len()).to_le_bytes());
            encode_shapes(shapes, head, &mut bulk);
        }
        tails.extend(bulk.into_vec());
    }

    /// Refuses a tail count it did not write, a tile count past its ceiling, a
    /// flag that is not a bool, and any tail left with bytes the head did not
    /// describe.
    ///
    /// The wire is same-build-only, so a length that disagrees with the head is
    /// another build's layout and not a batch to salvage.
    fn decode_out(head: &[u8], tails: Vec<Vec<u8>>) -> Option<BasemapTiles> {
        let mut cursors = TailCursors::new(&tails)?;
        let mut r = Reader::new(head);
        let declared = r.u32()?;
        let count = r.bounded(declared, REPLY_TILE_HEADER_BYTES)?;
        if count > MAX_TILES {
            return None;
        }
        let mut out = Vec::with_capacity(count);
        for _ in 0..count {
            let z = r.u8()?;
            let x = r.u32()?;
            let y = r.u32()?;
            let ok = match r.u8()? {
                0 => false,
                1 => true,
                _ => return None,
            };
            let shapes = decode_shapes(usize::try_from(r.u32()?).ok()?, &mut r, &mut cursors)?;
            out.push(StyledTile {
                z,
                x,
                y,
                shapes: ok.then_some(shapes),
            });
        }
        // Both directions: a head with bytes left over, and a tail with bytes
        // the head never described, are the same defect seen from two ends.
        (r.at_end() && cursors.all_consumed()).then_some(())?;
        Some(BasemapTiles { tiles: out })
    }
}

/// Shapes are geometry, not a picture: this output nominates **no** straight-
/// alpha raster for the run funnel to premultiply. Stated rather than
/// defaulted, because `JobOut::straight_rasters_mut` has no default and a new
/// output kind must say which posture it takes.
impl squallar_source::job::JobOut for BasemapTiles {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn into_any(self: Box<Self>) -> Box<dyn std::any::Any> {
        self
    }

    fn straight_rasters_mut(&mut self) -> Vec<&mut [u8]> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests;
