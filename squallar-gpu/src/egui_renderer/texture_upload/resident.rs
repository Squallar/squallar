//! **Bytes the device is holding in egui's texture population, right now.**
//!
//! # Why this exists
//!
//! `squallar_egui::heap_census` says its denominator once: bytes on one wasm
//! instance's linear memory, and "a family whose bytes live on the GPU (a
//! `TextureHandle`'s pixels after upload) is not here either". So a radar
//! raster, an overlay picture, a loop frame's texture and the glyph atlas all
//! stop being counted by every instrument this application has at the moment
//! they upload — the point at which they start costing the device.
//!
//! [`super::UploadTotals`] does not answer it. That is cumulative flow: bytes
//! that have ever crossed, by which route, and it only ever climbs. One Tier-2
//! leg moved 21.7 GB of uploads against a device holding a few hundred MB. A
//! running total cannot say what is resident, and no arithmetic over it can.
//!
//! # The denominator, said once
//!
//! **The pixel bytes of every `wgpu::Texture` alive in egui's `TextureId`
//! population**, at `width * height * 4` — every texture in that population is
//! `Rgba8Unorm`, both where `egui_wgpu::Renderer::update_texture` creates one
//! and where [`super::TextureUploads::allocate`] does.
//!
//! Two things are outside it and neither is an oversight:
//!
//! - **`squallar_volumetric`'s raymarch textures** — the voxel grids, transfer
//!   LUTs, jitter, occluder, colour and depth attachments. They are created by
//!   that crate against its own device, never enter egui's population, and are
//!   named by no census family. The host side of the voxel grids is
//!   `volume store`; their device side is unmeasured and this figure does not
//!   pretend otherwise.
//! - **Buffers.** Vertex, index and uniform bytes are not textures. The tile
//!   meshes have their own GPU family already.
//!
//! # Why two sides
//!
//! One `TextureId` can have **two** textures alive under it, and it is not a
//! bug. Where [`super::TextureUploads`] takes a raster over, it puts a 1x1
//! stand-in under the id through egui's own path so egui has something to paint
//! with, then hands egui a *bind group* built from its own big texture —
//! `update_egui_texture_from_wgpu_texture_with_sampler_options` replaces the
//! bind group and nothing else, so egui's `Texture.texture` stays whatever it
//! was until `free_texture` destroys it. Charging one figure per id would have
//! to pick one of the two and would be wrong in whichever direction it picked.
//!
//! So the ledger keeps a side per owner. Their sum is [`Self::bytes`], and it
//! is a real sum and not an upper bound: the two sides never name the same
//! `wgpu::Texture`, because the one path that puts egui's texture into
//! `TextureUploads::owned` — the large-partial take-over — deliberately files
//! no owned charge for it.
//!
//! # Backends, and why this figure is not one of them
//!
//! **A reading of this level must name the BACKEND as well as the arm.** Every
//! Linux web figure in this campaign is the GL backend; a Mac web figure is
//! WebGPU on Metal on a hardware adapter. Texture upload and residency are
//! backend behaviour, and the *upload* figures — `UploadTotals`, and the
//! `upload pending` census family beside this one — really do differ between
//! them for backend reasons before GPU or OS ones, because which route a delta
//! takes is decided by `device_has_ring`, which is a device property.
//!
//! This level is deliberately not. It is hooked on **egui's delta seam, above
//! wgpu** — [`super::TextureUploads::file`], [`super::TextureUploads::allocate`]
//! and [`super::TextureUploads::free`] — and both routes file their charge
//! through [`texture_bytes`] and nothing else. So neither backend is dark and
//! neither reads a false zero: the same scene holds the same bytes on Vulkan,
//! Metal, GL and WebGL2, and a difference between two arms' readings is a
//! difference in what is on the glass, not in what was instrumented.
//! `the_charge_is_the_same_whichever_route_carried_the_texture` is that
//! property as a test.
//!
//! # What it costs
//!
//! Every mutation is one `HashMap` insert or remove and two `u64` adds. The
//! total is **maintained**, never derived: nothing here walks the population,
//! on the frame thread or anywhere else. Reading it is a field read.

use std::collections::HashMap;

use egui::TextureId;

/// Bytes one texel of egui's population costs.
///
/// Not a guess and not a policy: `egui_wgpu::Renderer::update_texture` creates
/// `Rgba8Unorm` and [`super::TextureUploads::allocate`] carries a comment
/// saying its format must match what `update_texture` would have created.
const TEXEL_BYTES: u64 = 4;

/// Pixel bytes of a `width` x `height` texture in egui's population.
pub(crate) fn texture_bytes(size: [usize; 2]) -> u64 {
    (size[0] as u64)
        .saturating_mul(size[1] as u64)
        .saturating_mul(TEXEL_BYTES)
}

/// The resident level, maintained at the sites that create, replace and free.
#[derive(Debug, Default)]
pub(crate) struct ResidentTextures {
    /// What `egui_wgpu::Renderer` holds under each id: the texture its
    /// `update_texture` created on the last full delta it was given, which is
    /// the 1x1 stand-in for an id this module went on to take over.
    egui_side: HashMap<TextureId, u64>,
    /// What [`super::TextureUploads::allocate`] created for each id. Absent for
    /// every id whose texture is egui's, take-overs included.
    owned_side: HashMap<TextureId, u64>,
    /// The two sides summed, maintained rather than derived.
    bytes: u64,
}

/// Put `bytes` under `id`, taking back whatever was there. `map.insert`
/// returns the displaced charge, so a replace costs one lookup, not two.
fn charge(map: &mut HashMap<TextureId, u64>, total: &mut u64, id: TextureId, bytes: u64) {
    let displaced = map.insert(id, bytes).unwrap_or(0);
    *total = total.saturating_sub(displaced).saturating_add(bytes);
}

/// Take `id`'s charge back out. Saturating rather than wrapping: a level that
/// went negative through a missed charge would read as `u64::MAX` and be
/// mistaken for the instrument rather than for the drift it is.
fn discharge(map: &mut HashMap<TextureId, u64>, total: &mut u64, id: TextureId) {
    if let Some(held) = map.remove(&id) {
        *total = total.saturating_sub(held);
    }
}

impl ResidentTextures {
    /// egui's `update_texture` created a texture of `size` under `id`, dropping
    /// whatever it held there. **Full deltas only**: a delta with a `pos`
    /// writes into the texture that is already there and changes no bytes.
    pub(crate) fn egui_allocated(&mut self, id: TextureId, size: [usize; 2]) {
        charge(
            &mut self.egui_side,
            &mut self.bytes,
            id,
            texture_bytes(size),
        );
    }

    /// [`super::TextureUploads::allocate`] created a texture of `size` for
    /// `id`, replacing any it had created before.
    pub(crate) fn owned_allocated(&mut self, id: TextureId, size: [usize; 2]) {
        charge(
            &mut self.owned_side,
            &mut self.bytes,
            id,
            texture_bytes(size),
        );
    }

    /// This module let go of its texture for `id` — a full delta arrived for a
    /// raster it owns, so the one it holds is superseded. egui's side of `id`
    /// is untouched: the stand-in is still there and still costs.
    pub(crate) fn owned_dropped(&mut self, id: TextureId) {
        discharge(&mut self.owned_side, &mut self.bytes, id);
    }

    /// egui retired `id`. Both sides go: `Renderer::free_texture` destroys
    /// egui's and [`super::TextureUploads::free`] drops this module's, and they
    /// are the same call in `EguiRenderer::free_textures`.
    pub(crate) fn freed(&mut self, id: TextureId) {
        discharge(&mut self.egui_side, &mut self.bytes, id);
        discharge(&mut self.owned_side, &mut self.bytes, id);
    }

    /// The level. One field read.
    pub(crate) fn bytes(&self) -> u64 {
        self.bytes
    }

    /// The two sides' charge counts, for a test that wants to say which side a
    /// figure came from without being able to see a device.
    #[cfg(test)]
    pub(crate) fn charges(&self) -> (usize, usize) {
        (self.egui_side.len(), self.owned_side.len())
    }

    /// The maintained total against a walk of both maps.
    ///
    /// **A test seam and nothing else** — the walk is exactly what
    /// [`Self::bytes`] exists to avoid, and no shipped path may call this.
    /// What it catches is the one failure mode a maintained total has that a
    /// derived one does not: a mutation site that moves a map without moving
    /// the total, or the reverse.
    #[cfg(test)]
    pub(crate) fn walked_bytes(&self) -> u64 {
        self.egui_side
            .values()
            .chain(self.owned_side.values())
            .sum()
    }
}

#[cfg(test)]
#[path = "resident/tests.rs"]
mod tests;
