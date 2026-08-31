//! The building row: one codec row, so the parse, the projection and the
//! tessellation run where every other heavy job runs.
//!
//! [`BuildingMeshJob`] carries a volume box, a vertex budget's ceilings and
//! the MVT bodies that cover the box; it answers a [`BuildingMesh`]. Modelled
//! on `squallar_elevation::jobs::TerrainHeightJob` in shape — a request naming
//! a box plus the tile bodies for it, and a reply that is one flat thing over
//! that box — and composed **after** it in
//! `squallar_worker::job_registry`, never inserted earlier, because a wire code
//! is an index into the composition and a row inserted anywhere but the end
//! renumbers every code after it.
//!
//! **This module is why the crate declares `squallar-source`.** [`JobCodec`] is
//! a `struct` and not a trait, so a registry row cannot be built without it.
//!
//! # The second payload class, costed
//!
//! The height row ships PNG tiles because there is no decoded terrain anywhere
//! on the page. This row ships **MVT tiles that the page has already decoded
//! once**, and that is a real cost rather than a convenience:
//!
//! * the parsed-tile cache is page-side (`squallar_egui::tile_source`'s
//!   `SharedParsedTiles`) and a worker cannot read it — on the browser arm it
//!   is a different address space entirely;
//! * `vendor/walkers`' `ParsedTile` has no accessor that would yield a
//!   feature's geometry even to a caller on the right side of that boundary
//!   (see [`crate::footprint`]).
//!
//! So the bytes go across and are parsed a second time. What that buys is that
//! the second parse happens beside the tessellation, on the worker, instead of
//! the tessellation happening beside the cache, on the frame thread.
//!
//! **The size of what crosses, measured, and it is bigger than an earlier
//! draft of this paragraph said.** `BuildingTile::mvt` is the **decompressed**
//! body, and the five `building`-carrying z14 tiles of
//! `squallar-egui/testdata/monaco.pmtiles` decompress to 8,394 / 60,331 /
//! 60,854 / 88,953 / **185,182** bytes — a mean of 80,743 and a worst of
//! 185,182. A z14 cover of a dollied footprint is of order a hundred tiles, so
//! a round is **about 8 MB typical and up to 18.5 MB over downtown**. That is
//! at and above the top of the 1-8 MB bracket the height row accounts for,
//! not inside it.
//!
//! The earlier figure, "118,009 bytes decompressed", was the tile's
//! **gzip-compressed** length as the PMTiles directory stores it — off by
//! 1.57x, and in the direction that made the conclusion ("single-digit
//! megabytes") true when it is not. `footprint::tests`' own fixture comment
//! had the right number the whole time, so the tree contradicted itself.
//!
//! Natively the bodies move behind an `Arc`; on web they are transferred when
//! the page is cross-origin-isolated and copied once otherwise
//! (`squallar_web::worker_port` tries `shared_loan::lend` and falls back to a
//! whole-body copy before the transfer).
//!
//! # Refusals happen in [`BuildingMeshJob::decode`]
//!
//! For the reason the height row's do: containment is asymmetric. Natively
//! `offload::pool`'s `guarded` turns a panicking row into `None`, but an
//! allocation failure is an abort no `catch_unwind` sees; on web
//! `squallar_web::worker` calls `execute_encoded` with no guard at all, under
//! `panic = "abort"`. A payload whose geometry would allocate has to be
//! refused while it is still bytes.

use std::sync::Arc;

use squallar_source::job::{EncodeCtx, JobCodec, JobCost, JobGeometry, JobOutCodec, JobSpec};
use squallar_source::wire::Reader;

use crate::budget::{PrismBudget, PrismCeilings};
use crate::footprint::{BuildingsError, read_footprints};
use crate::prism::{BuildingMesh, extrude};
use crate::tile::{BoxFrame, MAX_TILE_ZOOM, TileId};

/// This crate's one codec row. Chained after the radar, overlay and elevation
/// registries, so its index — and therefore its wire code — is the last one
/// allocated.
pub static JOB_CODECS: &[JobCodec] = &[JobCodec::of::<BuildingMeshJob>()];

/// One vector tile's bytes, at the address they were fetched from.
///
/// `Arc<Vec<u8>>` for the reason the height row's `HeightTile` is: the native
/// transport moves the request rather than serialising it, so a tile set
/// handed to the pool costs a pointer and not a copy of the bodies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildingTile {
    pub tile: TileId,
    /// The tile's **decompressed** MVT bytes. Whatever the archive wrapped
    /// them in has already come off — this row parses protobuf and nothing
    /// else.
    pub mvt: Arc<Vec<u8>>,
}

/// Build the prism mesh for one volume box out of the vector tiles covering
/// it.
#[derive(Debug, PartialEq)]
pub struct BuildingMeshJob {
    /// The box the mesh is authored in, and the origin its kilometres are
    /// measured from.
    pub frame: BoxFrame,
    /// What the mesh may cost. Runtime figures, read off the caller's adapter.
    pub ceilings: PrismCeilings,
    /// The tiles covering [`frame`](Self::frame), in any order.
    pub tiles: Vec<BuildingTile>,
}

squallar_source::impl_job_input!(BuildingMeshJob);

/// The request's fixed prefix, before the tile list: six `f64` box terms, two
/// `u64` ceilings and the tile count. Named so the wire layout is stated once
/// in arithmetic as well as in code.
const REQUEST_PREFIX_BYTES: usize = 6 * 8 + 2 * 8 + 4;

/// A tile's own header: zoom, column, row and body length.
const TILE_HEADER_BYTES: usize = 1 + 3 * 4;

/// The reply head: three `u32` counters and the two buffer lengths. The
/// buffers themselves are the row's three nominated tails.
const REPLY_HEAD_BYTES: usize = 5 * 4;

/// The most tiles one request may name.
///
/// **A refusal ceiling, not a measured budget.** A z14 cover of the ~24 km
/// patch a fully dollied camera sees is of order 121 tiles, and a whole 920 km
/// box at that zoom is not a request anything would make because the vertex
/// budget would shed all but the first few hundred buildings out of it. This
/// is an order of magnitude past the honest case, and exists so a doctored
/// count answers `None` rather than reserving its way into an abort.
const MAX_TILES: usize = 1024;

/// The most tile bytes one request may carry, 256 MiB.
///
/// Sits beside [`MAX_TILES`] because the two bound different things: a
/// thousand tiles of 118 KB is 118 MB, and a thousand tiles is also a
/// perfectly good way to spell one tile of 256 MB.
const MAX_TILE_BYTES_TOTAL: u64 = 1 << 28;

impl JobSpec for BuildingMeshJob {
    type In = BuildingMeshJob;
    type Out = BuildingMesh;
    const LABEL: &'static str = "buildings/prisms";
    const COST: JobCost = JobCost::Raster;

    /// `[site 2xf64][x_km 2xf64][y_km 2xf64][vram u64][max_buffer u64]`
    /// `[tiles u32]`, then per tile `[z u8][x u32][y u32][len u32][body]`.
    ///
    /// A count or a body longer than a `u32` can express saturates rather than
    /// truncating into a plausible smaller number: [`Self::decode`] then runs
    /// off the end of the buffer and answers `None`, which is a refusal. It is
    /// not reachable — a body is a tile and a request is a cover — and
    /// `encode` has no error channel to report it through.
    fn encode(input: &BuildingMeshJob, _ctx: &EncodeCtx, out: &mut Vec<u8>) {
        out.reserve(REQUEST_PREFIX_BYTES);
        for term in [
            input.frame.site.0,
            input.frame.site.1,
            input.frame.x_km.0,
            input.frame.x_km.1,
            input.frame.y_km.0,
            input.frame.y_km.1,
        ] {
            out.extend_from_slice(&term.to_le_bytes());
        }
        out.extend_from_slice(&input.ceilings.vram_bytes.to_le_bytes());
        out.extend_from_slice(&input.ceilings.max_buffer_bytes.to_le_bytes());
        out.extend_from_slice(&saturating_u32(input.tiles.len()).to_le_bytes());
        for tile in &input.tiles {
            out.push(tile.tile.z);
            out.extend_from_slice(&tile.tile.x.to_le_bytes());
            out.extend_from_slice(&tile.tile.y.to_le_bytes());
            out.extend_from_slice(&saturating_u32(tile.mvt.len()).to_le_bytes());
            out.extend_from_slice(&tile.mvt);
        }
    }

    /// The mirror of [`Self::encode`], refusing rather than believing.
    ///
    /// These bytes arrive on a message port from a peer that may be another
    /// build, and this row is the last code in the composition — so the codes
    /// a *previous* build allocated to nothing land here. A non-finite or
    /// inverted box, an unaddressable tile, a tile count past its ceiling and
    /// a body set past its own are all refused, and the cursor must finish
    /// exactly at the end. `JobRequest::from_bytes` checks trailing bytes only
    /// for the `overlay/` rows, so this row checks its own.
    ///
    /// **The ceilings need no guard of their own**, and that is worth stating
    /// rather than leaving as an omission: they are two numbers handed
    /// straight to [`PrismBudget::fit`], which is total over the rung ladder
    /// and cannot answer more than [`crate::FINEST_VERTEX_CEILING`] whatever
    /// it is given. A `u64::MAX` there buys the sender the finest rung and
    /// nothing else.
    fn decode(r: &mut Reader<'_>, geo: JobGeometry) -> Option<(BuildingMeshJob, JobGeometry)> {
        let frame = BoxFrame {
            site: (r.f64()?, r.f64()?),
            x_km: (r.f64()?, r.f64()?),
            y_km: (r.f64()?, r.f64()?),
        };
        let ceilings = PrismCeilings {
            vram_bytes: u64_le(r)?,
            max_buffer_bytes: u64_le(r)?,
        };
        if !frame.is_drawable() {
            return None;
        }

        let declared = r.u32()?;
        let count = r.bounded(declared, TILE_HEADER_BYTES)?;
        if count > MAX_TILES {
            return None;
        }
        let mut tiles = Vec::with_capacity(count);
        let mut bytes_total: u64 = 0;
        for _ in 0..count {
            let tile = TileId {
                z: r.u8()?,
                x: r.u32()?,
                y: r.u32()?,
            };
            if tile.z > MAX_TILE_ZOOM || !tile.is_addressable() {
                return None;
            }
            let len = usize::try_from(r.u32()?).ok()?;
            bytes_total = bytes_total.checked_add(len as u64)?;
            if bytes_total > MAX_TILE_BYTES_TOTAL {
                return None;
            }
            tiles.push(BuildingTile {
                tile,
                mvt: Arc::new(r.take(len)?.to_vec()),
            });
        }
        r.at_end().then_some(())?;

        Some((
            BuildingMeshJob {
                frame,
                ceilings,
                tiles,
            },
            geo,
        ))
    }

    /// Read every tile, then extrude the whole set against one budget.
    ///
    /// **One extrusion over every tile's footprints, never one per tile.** The
    /// budget sheds by height across the whole box, and a per-tile shed would
    /// keep the tallest building in each tile — which is a skyline of evenly
    /// spaced towers rather than a downtown.
    ///
    /// **A tile that refuses is skipped, not fatal, and it is counted.** Most
    /// tiles on earth carry no `building` layer at all, which is not an error;
    /// bytes that do not decode are, and
    /// [`BuildingMesh::refused_tiles`](crate::prism::BuildingMesh::refused_tiles)
    /// is what keeps a round that lost ten tiles from reporting exactly as a
    /// clean one.
    ///
    /// **The answer is `Some` even when it is empty**, deliberately. `None` in
    /// this funnel means "nothing to draw", and there is no way to tell that
    /// from "there are no buildings in Kansas" — which is the true and common
    /// answer over most of the archive. A caller that needs to know whether
    /// the round went well reads the counters.
    ///
    /// The geometry is untouched: a mesh is not a raster and the envelope's
    /// `side_ceiling_px` has nothing to say about it.
    fn run(input: &BuildingMeshJob, _geo: &JobGeometry) -> Option<BuildingMesh> {
        let budget = PrismBudget::fit(input.ceilings);
        let mut footprints = Vec::new();
        let mut refused_tiles = 0u32;
        for tile in &input.tiles {
            match read_footprints(tile.tile, &tile.mvt, &input.frame) {
                Ok(mut found) => footprints.append(&mut found),
                Err(BuildingsError::NoBuildingLayer) => {}
                Err(e) => {
                    refused_tiles += 1;
                    log::warn!(
                        "buildings: z{}/{}/{} ({} bytes) did not read: {e}",
                        tile.tile.z,
                        tile.tile.x,
                        tile.tile.y,
                        tile.mvt.len(),
                    );
                }
            }
        }
        let mut mesh = extrude(&footprints, &budget);
        mesh.refused_tiles = refused_tiles;
        Some(mesh)
    }
}

impl JobOutCodec for BuildingMeshJob {
    /// `[kept u32][shed u32][refused u32][vertices u32][indices u32]` as the
    /// head, and positions, normals and indices as the row's three **tails**.
    ///
    /// Three tails rather than one concatenated buffer for the reason the
    /// height row nominates one: head and tails are pushed into the same
    /// `buffers` list and lent identically by `squallar_web::worker`, so
    /// nominating a tail buys no different transport. What it buys is not
    /// building the concatenation at all — a 262,144-vertex mesh is 9.44 MB,
    /// and that copy would be paid at each end for nothing.
    fn encode_out(v: BuildingMesh, head: &mut Vec<u8>, tails: &mut Vec<Vec<u8>>) {
        head.reserve_exact(REPLY_HEAD_BYTES);
        for term in [
            v.kept,
            v.shed,
            v.refused_tiles,
            saturating_u32(v.positions.len()),
            saturating_u32(v.indices.len()),
        ] {
            head.extend_from_slice(&term.to_le_bytes());
        }

        let mut positions = Vec::with_capacity(v.positions.len() * 12);
        let mut normals = Vec::with_capacity(v.normals.len() * 12);
        for (position, normal) in v.positions.iter().zip(&v.normals) {
            for axis in 0..3 {
                positions.extend_from_slice(&position[axis].to_le_bytes());
                normals.extend_from_slice(&normal[axis].to_le_bytes());
            }
        }
        let mut indices = Vec::with_capacity(v.indices.len() * 4);
        for index in &v.indices {
            indices.extend_from_slice(&index.to_le_bytes());
        }
        tails.push(positions);
        tails.push(normals);
        tails.push(indices);
    }

    /// Refuses a tail count it did not write, buffers that are not exactly the
    /// declared lengths, and a mesh whose indices do not address its own
    /// vertices.
    ///
    /// The wire is same-build-only, so a length that disagrees with the head
    /// is another build's layout and not a mesh to salvage. The **coherence**
    /// check is the one that is not merely defensive: an index off the end of
    /// the position buffer is an out-of-bounds read on the GPU, which is a
    /// driver's business rather than a `panic!` this side of the boundary can
    /// catch.
    fn decode_out(head: &[u8], tails: Vec<Vec<u8>>) -> Option<BuildingMesh> {
        if tails.len() != 3 {
            return None;
        }
        let mut r = Reader::new(head);
        let kept = r.u32()?;
        let shed = r.u32()?;
        let refused_tiles = r.u32()?;
        let vertices = r.u32()? as usize;
        let index_count = r.u32()? as usize;
        if !r.at_end() {
            return None;
        }

        let mut tails = tails.into_iter();
        let raw_positions = tails.next()?;
        let raw_normals = tails.next()?;
        let raw_indices = tails.next()?;
        if raw_positions.len() != vertices.checked_mul(12)?
            || raw_normals.len() != vertices.checked_mul(12)?
            || raw_indices.len() != index_count.checked_mul(4)?
        {
            return None;
        }

        let mesh = BuildingMesh {
            positions: triples(&raw_positions),
            normals: triples(&raw_normals),
            indices: raw_indices
                .as_chunks::<4>().0.iter()
                .map(|word| u32::from_le_bytes([word[0], word[1], word[2], word[3]]))
                .collect(),
            kept,
            shed,
            refused_tiles,
        };
        mesh.is_coherent().then_some(mesh)
    }
}

/// A `u64` off the cursor. `Reader` has no `u64` of its own and this row wants
/// two, so the eight bytes are taken and reassembled rather than routed
/// through `i64` and cast — the cast round-trips, but it spells "signed" in a
/// place where nothing is.
fn u64_le(r: &mut Reader<'_>) -> Option<u64> {
    Some(u64::from_le_bytes(r.take(8)?.try_into().ok()?))
}

/// A flat little-endian `f32` buffer as triples.
fn triples(raw: &[u8]) -> Vec<[f32; 3]> {
    raw.as_chunks::<12>().0.iter()
        .map(|chunk| {
            let mut out = [0.0f32; 3];
            for (axis, word) in chunk.as_chunks::<4>().0.iter().enumerate() {
                out[axis] = f32::from_le_bytes([word[0], word[1], word[2], word[3]]);
            }
            out
        })
        .collect()
}

/// A length as a `u32`, saturating. See [`BuildingMeshJob::encode`] for why
/// saturation is the safe direction here.
fn saturating_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// A mesh is geometry, not a picture: it nominates **no** straight-alpha
/// raster for the run funnel to premultiply. Stated rather than defaulted,
/// because `JobOut::straight_rasters_mut` has no default and a new output kind
/// must say which posture it takes.
impl squallar_source::job::JobOut for BuildingMesh {
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
