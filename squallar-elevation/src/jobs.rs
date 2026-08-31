//! The height row: one codec row, so the decode and the resample run where
//! every other heavy job runs.
//!
//! [`TerrainHeightJob`] carries a box, a tile rectangle and the tile bodies
//! that cover it; it answers a [`HeightField`]. Modelled on
//! `squallar_radar::jobs::VoxelJob` in shape — a request that names a box and a
//! reply that is a grid over it — and composed **last** in
//! `squallar_worker::job_registry`, never appended into either of the older
//! registries, because a wire code is an index into the composition and
//! inserting a row renumbers every code after it.
//!
//! **This module is why the crate declares `squallar-source`.** [`JobCodec`] is
//! a `struct` and not a trait, defined in `squallar-source/src/job.rs`, so a
//! registry row cannot be built without it. `tests/charter.rs` pre-announced
//! the arrival and carries the measured closure it costs.
//!
//! **The payload is 1–8 MB of PNG.** Natively it moves; on web it is
//! transferred when the page is cross-origin-isolated and copied once
//! otherwise (`squallar_web::worker_port` tries `shared_loan::lend` and falls
//! back to a whole-body copy before the transfer). Neither arm decodes on the
//! frame thread.
//!
//! **The tiles arrive as bodies, never through an `HttpsTiles`.** That is what
//! keeps heights off `WASM_TILE_DECODES_PER_PUMP`: the budget is spent in
//! `squallar_egui::tile_source`'s `drain_completed_fetches`, which only a
//! decoded-picture source reaches, and a height field is one per box rather
//! than one of ~54 needed this frame. **Nothing in the tree reddens if a caller
//! builds an `HttpsTiles` for heights anyway**, and no pin can live here: the
//! budget is `squallar-egui`'s, and that crate sits ABOVE this one, so a
//! source-scrape from here would be aimed at a caller that does not exist yet.
//! The pin is owed by the work unit that writes the first fetch layer, against
//! its own call site.
//!
//! **Refusals happen in [`TerrainHeightJob::decode`], because further down
//! there is nothing to catch them.** Containment is asymmetric and the
//! asymmetry decides where the guards go: natively `offload::pool`'s `guarded`
//! turns a panicking row into `None`, but an allocation failure is an abort no
//! `catch_unwind` sees; and on web `squallar_web::worker` calls
//! `execute_encoded` with no guard at all, under `panic = "abort"`. So a
//! payload whose *geometry* would allocate must be refused while it is still
//! bytes -- see the ceilings below.

use std::sync::Arc;

use squallar_source::job::{EncodeCtx, JobCodec, JobCost, JobGeometry, JobOutCodec, JobSpec};
use squallar_source::wire::Reader;

use crate::height::HeightField;
use crate::plan::MAX_POSTS_PER_AXIS;
use crate::resample::{TileCover, TilePlane};

/// This crate's one codec row. Chained after the radar and overlay registries,
/// so its index — and therefore its wire code — is the last one allocated.
pub static JOB_CODECS: &[JobCodec] = &[JobCodec::of::<TerrainHeightJob>()];

/// One Terrain-RGB tile body, at the address it was fetched from.
///
/// `Arc<Vec<u8>>` for the reason `squallar_radar::jobs::DecodeJob`'s archive
/// is: the native transport moves the request rather than serialising it, so a
/// tile set handed to the pool costs a pointer and not a copy of 1–8 MB.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeightTile {
    pub x: u32,
    pub y: u32,
    pub png: Arc<Vec<u8>>,
}

/// Resample a rectangle of Terrain-RGB tiles onto a volume box's post grid.
///
/// The [`TileCover`] rides the request rather than being recomputed from the
/// box: the caller fetched a particular set of tiles, and a cover recomputed at
/// run time could name a different one. It is still checked — the run
/// recomputes the *needed* cover from the box and refuses a plane that does not
/// contain it (`TilePlane::resample`), so a fetch round that landed a shrunken
/// tile set answers nothing rather than a believable field built off the
/// sampler's edge clamp.
#[derive(Debug, PartialEq)]
pub struct TerrainHeightJob {
    /// `(latitude, longitude)` of the box's origin, in degrees.
    pub site: (f64, f64),
    /// East extent as `(low, high)` kilometres about the site.
    pub x_km: (f64, f64),
    /// North extent as `(low, high)` kilometres about the site.
    pub y_km: (f64, f64),
    /// Posts along east and north, in that order.
    pub posts: [u32; 2],
    /// The tile rectangle [`Self::tiles`] fills.
    pub cover: TileCover,
    /// One body per address the cover names, in any order.
    pub tiles: Vec<HeightTile>,
}

squallar_source::impl_job_input!(TerrainHeightJob);

/// The request's fixed prefix, before the tile list: six `f64` box terms, two
/// `u32` post counts, the cover's `u8` zoom and five `u32`s, and the tile
/// count. Named so the wire layout is stated once in arithmetic as well as in
/// code.
const REQUEST_PREFIX_BYTES: usize = 6 * 8 + 2 * 4 + 1 + 5 * 4 + 4;

/// A tile's own header: address and body length.
const TILE_HEADER_BYTES: usize = 3 * 4;

/// The reply head: the six `f64` box terms and the two `u32` post counts. The
/// samples do not ride here — they are the row's one nominated tail.
const REPLY_HEAD_BYTES: usize = 6 * 8 + 2 * 4;

/// The largest tile side this row will accept.
///
/// **A refusal ceiling, not a measured budget**, and the distinction matters:
/// nothing here has been profiled, and the number exists so a malformed
/// `tile_px` answers `None` instead of aborting the process. A Terrain-RGB tile
/// is 256 px by construction (`tools/squallar-terrain` writes its raster
/// archive at the standard tile size) and 512 is the largest anyone ships, so
/// 4096 is eight times the widest real tile.
const MAX_TILE_PX: u32 = 4096;

// The most posts one axis of a field may carry is `plan::MAX_POSTS_PER_AXIS`,
// imported above. It used to be spelled here as well; a refusal ceiling and the
// fit that stops short of it are the last two numbers that should be allowed to
// disagree.

/// The most posts one field may carry, 4,194,304 -- 8 MiB of `u16`, against
/// 848,241 for the 921x921 case above.
const MAX_POSTS_TOTAL: u64 = 1 << 22;

/// The most pixels one assembled plane may hold, 134,217,728 -- 512 MiB of the
/// `f32` [`TilePlane`] lays out, against the ~69.2 M px (1056 tiles of 256 px)
/// a 920 km box needs at z10. Under 2x the largest honest case, and the
/// tightest of these ceilings; a real fetch layer should re-derive it from a
/// measurement once one exists.
const MAX_PLANE_PX: u64 = 1 << 27;

impl JobSpec for TerrainHeightJob {
    type In = TerrainHeightJob;
    type Out = HeightField;
    const LABEL: &'static str = "terrain/heights";
    const COST: JobCost = JobCost::Raster;

    /// `[site 2xf64][x_km 2xf64][y_km 2xf64][posts 2xu32]`
    /// `[zoom u8][tile_px u32][tx0 u32][ty0 u32][tx1 u32][ty1 u32]`
    /// `[tiles u32]`, then per tile `[x u32][y u32][len u32][body]`.
    ///
    /// A count or a body longer than a `u32` can express saturates rather than
    /// truncating into a plausible smaller number: [`Self::decode`] then runs
    /// off the end of the buffer and answers `None`, which is a refusal. It is
    /// not reachable — a tile is a PNG and a cover is a rectangle of them — and
    /// `encode` has no error channel to report it through.
    fn encode(input: &TerrainHeightJob, _ctx: &EncodeCtx, out: &mut Vec<u8>) {
        out.reserve(REQUEST_PREFIX_BYTES);
        for term in [
            input.site.0,
            input.site.1,
            input.x_km.0,
            input.x_km.1,
            input.y_km.0,
            input.y_km.1,
        ] {
            out.extend_from_slice(&term.to_le_bytes());
        }
        out.extend_from_slice(&input.posts[0].to_le_bytes());
        out.extend_from_slice(&input.posts[1].to_le_bytes());
        out.push(input.cover.zoom);
        for term in [
            input.cover.tile_px,
            input.cover.tx0,
            input.cover.ty0,
            input.cover.tx1,
            input.cover.ty1,
        ] {
            out.extend_from_slice(&term.to_le_bytes());
        }
        out.extend_from_slice(&saturating_u32(input.tiles.len()).to_le_bytes());
        for tile in &input.tiles {
            out.extend_from_slice(&tile.x.to_le_bytes());
            out.extend_from_slice(&tile.y.to_le_bytes());
            out.extend_from_slice(&saturating_u32(tile.png.len()).to_le_bytes());
            out.extend_from_slice(&tile.png);
        }
    }

    /// The mirror of [`Self::encode`], refusing rather than believing.
    ///
    /// These bytes arrive on a message port from a peer that may be another
    /// build, and this row is the last code in the composition — so the codes
    /// a *previous* build allocated to nothing land here. The guards are what
    /// make a payload written for another kind fail loudly: a non-finite box
    /// term, a zero post count, a zero tile size and an inverted rectangle are
    /// all refused, and the cursor must finish exactly at the end.
    /// `JobRequest::from_bytes` checks trailing bytes only for the `overlay/`
    /// rows, so this row checks its own.
    ///
    /// **The count is not the thing that allocates; the geometry is.** Bounding
    /// the tile count against the bytes present is necessary and nowhere near
    /// sufficient — an 81-byte payload declaring zero tiles over a cover of
    /// `tx1 = u32::MAX - 1` used to decode, and `TilePlane::assemble` then
    /// multiplied its plane size to an overflow panic; one column short of that
    /// it asked for a petabyte and aborted. So the cover is pinned to the tile
    /// list **exactly** — `tiles_x * tiles_y == tiles.len()`, which is the
    /// honest-caller invariant `assemble` enforces one layer down through
    /// `MissingTile`/`UnexpectedTile`/`DuplicateTile` anyway — and the plane and
    /// the post grid are each held under a named ceiling.
    ///
    /// The envelope's `side_ceiling_px` is deliberately **not** consulted: it is
    /// a raster side ceiling, a post grid is not a raster, and it arrives off
    /// the same untrusted wire as everything else here. A row that bounds
    /// itself needs no ceiling handed to it.
    fn decode(r: &mut Reader<'_>, geo: JobGeometry) -> Option<(TerrainHeightJob, JobGeometry)> {
        let site = (r.f64()?, r.f64()?);
        let x_km = (r.f64()?, r.f64()?);
        let y_km = (r.f64()?, r.f64()?);
        let posts = [r.u32()?, r.u32()?];
        let cover = TileCover {
            zoom: r.u8()?,
            tile_px: r.u32()?,
            tx0: r.u32()?,
            ty0: r.u32()?,
            tx1: r.u32()?,
            ty1: r.u32()?,
        };
        if ![site.0, site.1, x_km.0, x_km.1, y_km.0, y_km.1]
            .iter()
            .all(|term| term.is_finite())
        {
            return None;
        }
        if posts[0] == 0 || posts[1] == 0 || cover.tile_px == 0 || cover.is_empty() {
            return None;
        }
        if cover.tile_px > MAX_TILE_PX
            || posts[0] > MAX_POSTS_PER_AXIS
            || posts[1] > MAX_POSTS_PER_AXIS
            || u64::from(posts[0]) * u64::from(posts[1]) > MAX_POSTS_TOTAL
        {
            return None;
        }

        let declared = r.u32()?;
        let count = r.bounded(declared, TILE_HEADER_BYTES)?;
        // The cover names exactly the tiles that follow. This is what bounds the
        // rectangle: `count` is already bounded by the bytes present, so the
        // equality carries that bound onto `tiles_x * tiles_y`, and the plane
        // check below can then be done in `u64` without overflowing.
        let tiles_across = u64::from(cover.tiles_x());
        let tiles_down = u64::from(cover.tiles_y());
        if tiles_across.checked_mul(tiles_down)? != count as u64 {
            return None;
        }
        let px_per_tile = u64::from(cover.tile_px) * u64::from(cover.tile_px);
        if (count as u64).checked_mul(px_per_tile)? > MAX_PLANE_PX {
            return None;
        }
        let mut tiles = Vec::with_capacity(count);
        for _ in 0..count {
            let x = r.u32()?;
            let y = r.u32()?;
            let len = usize::try_from(r.u32()?).ok()?;
            tiles.push(HeightTile {
                x,
                y,
                png: Arc::new(r.take(len)?.to_vec()),
            });
        }
        r.at_end().then_some(())?;

        Some((
            TerrainHeightJob {
                site,
                x_km,
                y_km,
                posts,
                cover,
                tiles,
            },
            geo,
        ))
    }

    /// Assemble one contiguous pixel plane, then resample it onto the box's
    /// posts.
    ///
    /// **One plane before any sampling**, never per tile: Terrain-RGB tiles are
    /// edge-sharing grids whose pixel centres do not coincide across a
    /// boundary, and a per-tile bilinear with a per-tile clamp puts a seam at
    /// every tile edge.
    ///
    /// Every failure answers `None`, the same "nothing to draw" every other row
    /// produces — **and says which failure it was**, which is the whole reason
    /// this crate declares `log`. A refused decode and a genuine absence of
    /// terrain are the same observable at the glass, and that is precisely the
    /// confusion `squallar_geo::min_elevation` spends a whole `i16` code to
    /// avoid one level down. Worse here than there: some failures are not a
    /// `None` at all but a dead worker, since `squallar_web::worker` runs this
    /// unguarded under `panic = "abort"`.
    ///
    /// The geometry is untouched. See [`Self::decode`] on `side_ceiling_px`.
    fn run(input: &TerrainHeightJob, _geo: &JobGeometry) -> Option<HeightField> {
        let bodies: Vec<(u32, u32, &[u8])> = input
            .tiles
            .iter()
            .map(|tile| (tile.x, tile.y, tile.png.as_slice()))
            .collect();
        let plane = match TilePlane::assemble(input.cover, &bodies) {
            Ok(plane) => plane,
            Err(e) => {
                log::warn!(
                    "terrain heights: {} tile body/bodies over {:?} did not assemble: {e}",
                    input.tiles.len(),
                    input.cover,
                );
                return None;
            }
        };
        match plane.resample(input.site, input.x_km, input.y_km, input.posts) {
            Ok(field) => Some(field),
            Err(e) => {
                log::warn!(
                    "terrain heights: the plane did not resample onto {:?} posts about {:?}: {e}",
                    input.posts,
                    input.site,
                );
                None
            }
        }
    }
}

impl JobOutCodec for TerrainHeightJob {
    /// `[site 2xf64][x_km 2xf64][y_km 2xf64][posts 2xu32]` as the head, and the
    /// samples as the row's one **tail**.
    ///
    /// A tail rather than more head to avoid the **concat**, and that is the
    /// whole of the claim: head and tails are pushed into one `buffers` list and
    /// lent identically by `squallar_web::worker`, so nominating a tail does not
    /// buy the samples a different transport from the head. What it buys is not
    /// building a `head + samples` buffer at all — a 129x129 field is 33 KiB and
    /// a 921x921 one is 1.62 MiB, and that copy would be paid at each end for
    /// nothing.
    fn encode_out(v: HeightField, head: &mut Vec<u8>, tails: &mut Vec<Vec<u8>>) {
        head.reserve_exact(REPLY_HEAD_BYTES);
        for term in [v.site.0, v.site.1, v.x_km.0, v.x_km.1, v.y_km.0, v.y_km.1] {
            head.extend_from_slice(&term.to_le_bytes());
        }
        head.extend_from_slice(&v.posts[0].to_le_bytes());
        head.extend_from_slice(&v.posts[1].to_le_bytes());

        let mut samples = Vec::with_capacity(v.samples.len() * 2);
        for sample in &v.samples {
            samples.extend_from_slice(&sample.to_le_bytes());
        }
        tails.push(samples);
    }

    /// Refuses a tail count it did not write, and a sample buffer that is not
    /// exactly the declared grid: the wire is same-build-only, so a length that
    /// disagrees with `posts_x * posts_y` is another build's layout and not a
    /// field to salvage.
    fn decode_out(head: &[u8], mut tails: Vec<Vec<u8>>) -> Option<HeightField> {
        if tails.len() != 1 {
            return None;
        }
        let raw = tails.pop()?;

        let mut r = Reader::new(head);
        let site = (r.f64()?, r.f64()?);
        let x_km = (r.f64()?, r.f64()?);
        let y_km = (r.f64()?, r.f64()?);
        let posts = [r.u32()?, r.u32()?];
        if !r.at_end() || posts[0] == 0 || posts[1] == 0 {
            return None;
        }

        let want = (posts[0] as usize)
            .checked_mul(posts[1] as usize)?
            .checked_mul(2)?;
        if raw.len() != want {
            return None;
        }
        let samples = raw
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        Some(HeightField {
            site,
            x_km,
            y_km,
            posts,
            samples,
        })
    }
}

/// A length as a `u32`, saturating. See [`TerrainHeightJob::encode`] for why
/// saturation is the safe direction here.
fn saturating_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// A height field is numbers, not a picture: it nominates **no** straight-alpha
/// raster for the run funnel to premultiply. Stated rather than defaulted,
/// because `JobOut::straight_rasters_mut` has no default and a new output kind
/// must say which posture it takes.
impl squallar_source::job::JobOut for HeightField {
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
