//! Getting a sweep's code plane onto the GPU without the frame thread pushing
//! it across PCIe itself.
//!
//! [`super::RadarFanStore::ensure`] runs inside `egui_wgpu`'s `prepare`, which
//! `Renderer::update_buffers` dispatches on the frame thread. Until this module
//! it wrote the whole mip chain there with `queue.write_texture`, whose mapping
//! `wgpu-hal` places in the card's host-visible BAR window: every host store is
//! a write down the link and the call blocks until the last one lands.
//! [`crate::staging_ring`] carries the measurement, and
//! [`crate::egui_renderer::texture_upload`] is the same answer for egui's own
//! rasters — a memcpy into cached host memory, with the copy engine pulling it
//! across afterwards.
//!
//! A surveillance sweep is 720 x 1832: 1,758,630 B of chain over eleven levels,
//! and a pane's fan is one of those. **A frame may carry six**, one per pane —
//! six loops stepping together, or six panes retargeted at once — and every one
//! of those bytes went through the window.
//!
//! # One slot a pass, not one a sweep
//!
//! The ring holds [`crate::staging_ring::STAGING_RING_DEPTH`] slots and a slot
//! claimed on this frame cannot come back on this frame, so a claim per sweep
//! would serve two panes and send the other four through the window. So
//! `ensure` creates the textures and **files** the sweep, and the whole pass's
//! filings cross out of one slot in `finish_prepare` — which `egui_wgpu` calls
//! for every callback after every `prepare` and before the render pass, so
//! nothing is ever drawn from a texture whose bytes have not been copied.
//!
//! # Padding
//!
//! A buffer-to-texture copy's stride is held to
//! [`wgpu::COPY_BYTES_PER_ROW_ALIGNMENT`] where `write_texture` repacks
//! internally, so a region is staged at a padded stride and copied at that
//! stride. At 1832 gates that is 2048 B a row against 1832 — paid in cached
//! host memory and on the copy engine, not on the frame thread.
//!
//! # Both arms, one code path
//!
//! [`available`] is a device-feature question and not a target question:
//! nothing here sits behind a `cfg`. A device without
//! [`crate::staging_ring::STAGING_RING_FEATURE`] — which is all of the web —
//! takes [`write_direct`], which writes the bytes `ensure` wrote before this
//! module existed, to the same places. The fallback is therefore not an
//! untested branch but the web arm's production route, and the GPU suite holds
//! the two against each other on a device that can take either.

use std::sync::Arc;

use egui_wgpu::wgpu;
use squallar_device_profile::constants::{POLAR_LUT_BYTES, POLAR_LUT_ENTRIES};

use super::UiSweep;
use crate::staging_ring::{Ring, device_has_ring};

/// The largest single pass this path stages, and so the largest a ring slot may
/// grow to.
///
/// 16 MiB: eight worst-case sweeps against the six a desktop layout's panes can
/// ask for at once. Past it a pass takes [`write_direct`] rather than growing a
/// ring that then holds [`crate::staging_ring::STAGING_RING_DEPTH`] slots of
/// whatever it reached — the refusal
/// [`crate::egui_renderer::geometry_staging::MAX_STAGED_GEOMETRY_BYTES`] makes,
/// for its reason.
pub const MAX_STAGED_CHAIN_BYTES: u64 = 16 << 20;

/// How coarsely a ring slot is sized.
///
/// 4 MiB — two worst-case sweeps, and the granularity
/// [`crate::egui_renderer::geometry_staging::GEOMETRY_SLOT_GRANULARITY`] uses,
/// for its reason: a resize rebuilds every slot on the frame thread, so a slot
/// sized to the pass's exact total would rebuild whenever another pane joined
/// in. A build whose panes hold one sweep apiece settles at one granule.
pub const CHAIN_SLOT_GRANULARITY: u64 = 4 << 20;

/// Whether this device can stage a chain through cached host memory at all.
/// The same capability the raster and geometry paths ask for, asked the same
/// way.
pub fn available(device: &wgpu::Device) -> bool {
    device_has_ring(device)
}

/// **What one store's chain uploads have done.**
///
/// # Denominator
///
/// [`Self::staged`] and [`Self::declined`] count **passes that had a sweep to
/// upload**, never sweeps: one pass moves everything filed on it or none of it.
/// [`Self::bytes`] is the payload bytes that went through the ring, padding
/// excluded, so it is comparable with what the chain and the table hold and not
/// with a slot size.
///
/// `staged == 0 && declined == 0` is a store that has never uploaded.
/// `staged == 0 && declined > 0` is one uploading through the window every
/// time — a device with no ring, or a ring refusing — which reads identically
/// in a frame clock and not at all identically here.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChainStagingTotals {
    pub staged: u64,
    pub declined: u64,
    pub bytes: u64,
}

/// Which texture of a sweep a placed region belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Face {
    /// One level of the code plane's mip chain.
    Codes(u32),
    /// The colour table, one row of a texture of its own.
    Lut,
}

/// **One region of one sweep, and where it sits in the pass's slot.**
///
/// The single authority on which bytes go where: both routes below read this
/// list, so a device with a ring and a device without cannot disagree about a
/// level's shape or its place in the chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Placed {
    face: Face,
    /// Bytes from the slot's base. A multiple of
    /// [`wgpu::COPY_BYTES_PER_ROW_ALIGNMENT`] by construction, because every
    /// region's staged length is one.
    offset: u64,
    /// The stride the copy reads this region at.
    padded_row: u32,
    /// The stride the payload holds it at.
    row_bytes: u32,
    rows: u32,
    /// Texels a row carries — `row_bytes` over the format's texel size, which
    /// differs between the two faces.
    width: u32,
}

impl Placed {
    /// Bytes this region occupies in a slot, padding included.
    fn staged_bytes(&self) -> u64 {
        u64::from(self.padded_row) * u64::from(self.rows)
    }

    /// Bytes of payload it carries.
    fn bytes(&self) -> u64 {
        u64::from(self.row_bytes) * u64::from(self.rows)
    }

    fn mip_level(&self) -> u32 {
        match self.face {
            Face::Codes(level) => level,
            Face::Lut => 0,
        }
    }

    fn extent(&self) -> wgpu::Extent3d {
        wgpu::Extent3d {
            width: self.width,
            height: self.rows,
            depth_or_array_layers: 1,
        }
    }
}

/// A sweep whose textures exist and whose bytes have not crossed yet.
pub(super) struct Pending {
    pub sweep: Arc<UiSweep>,
    pub codes: wgpu::Texture,
    pub lut: wgpu::Texture,
    /// Levels the code texture was created with, which is the depth the plan
    /// covers — never the payload's own claim, which may be deeper.
    pub levels: usize,
}

impl Pending {
    fn texture(&self, region: &Placed) -> &wgpu::Texture {
        match region.face {
            Face::Codes(_) => &self.codes,
            Face::Lut => &self.lut,
        }
    }

    /// The payload bytes a region carries.
    ///
    /// Every region in a plan has them: [`chain`] leaves out any level the
    /// payload cannot answer for, which is the case the bridge's admission
    /// already refuses. `None` rather than a panic all the same, because this
    /// runs inside the renderer's prepare, where a panic takes the frame and
    /// every pane on it.
    fn source(&self, region: &Placed) -> Option<&[u8]> {
        match region.face {
            Face::Codes(level) => self.sweep.level(level as usize),
            Face::Lut => Some(&self.sweep.lut_rgba),
        }
    }

    /// This sweep's plan, laid from `base`, and the offset the next sweep
    /// starts at.
    fn plan(&self, base: u64) -> (Vec<Placed>, u64) {
        place(&chain(&self.sweep, self.levels), base)
    }
}

/// **The levels of `sweep` that can actually be copied**, as
/// `(level, rows, gates)`.
///
/// A level the payload is too short for, or one whose index does not fit a
/// texture's, is left out entirely rather than placed and skipped later — so
/// both routes agree on the whole question by construction.
fn chain(sweep: &UiSweep, levels: usize) -> Vec<(u32, u32, u32)> {
    (0..levels)
        .filter_map(|level| {
            let (rows, gates) = sweep.level_shape(level)?;
            sweep.level(level)?;
            Some((u32::try_from(level).ok()?, rows, gates))
        })
        .collect()
}

/// **Where every region of a chain and its table sits**, laid from `base`, and
/// the offset the next sweep starts at.
fn place(levels: &[(u32, u32, u32)], base: u64) -> (Vec<Placed>, u64) {
    let mut at = base;
    let mut out = Vec::with_capacity(levels.len() + 1);
    let mut push = |face, row_bytes: u32, rows: u32, width: u32, at: &mut u64| {
        let region = Placed {
            face,
            offset: *at,
            padded_row: row_bytes.next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT),
            row_bytes,
            rows,
            width,
        };
        *at += region.staged_bytes();
        out.push(region);
    };
    for &(level, rows, gates) in levels {
        // One byte a code, so a row's texels and its bytes are one number.
        push(Face::Codes(level), gates, rows, gates, &mut at);
    }
    push(
        Face::Lut,
        POLAR_LUT_BYTES as u32,
        1,
        POLAR_LUT_ENTRIES as u32,
        &mut at,
    );
    (out, at)
}

/// Push one sweep's regions across with `queue.write_texture`.
///
/// **The route every device without a ring takes**, which is all of the web,
/// and what `ensure` did before this module existed.
fn write_direct(queue: &wgpu::Queue, pending: &Pending, placed: &[Placed]) {
    for region in placed {
        let Some(source) = pending.source(region) else {
            continue;
        };
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: pending.texture(region),
                mip_level: region.mip_level(),
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            source,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                // Packed, not padded: `write_texture` repacks internally.
                bytes_per_row: Some(region.row_bytes),
                rows_per_image: Some(region.rows),
            },
            region.extent(),
        );
    }
}

/// The chain uploads one [`super::RadarFanStore`] has waiting, and the ring
/// they cross through.
pub(super) struct ChainStaging {
    /// Built on the first staging, so a store that never uploads holds no
    /// pinned host memory.
    ring: Option<Ring>,
    /// Whether this device may stage at all. Read once at construction from
    /// [`available`], so nothing here asks the device per frame.
    capable: bool,
    pending: Vec<Pending>,
    staged: u64,
    declined: u64,
    bytes: u64,
}

impl ChainStaging {
    pub fn new(capable: bool) -> Self {
        Self {
            ring: None,
            capable,
            pending: Vec::new(),
            staged: 0,
            declined: 0,
            bytes: 0,
        }
    }

    /// Hold one sweep's textures until the pass's upload.
    pub fn file(&mut self, pending: Pending) {
        self.pending.push(pending);
    }

    pub fn totals(&self) -> ChainStagingTotals {
        ChainStagingTotals {
            staged: self.staged,
            declined: self.declined,
            bytes: self.bytes,
        }
    }

    /// **Move every filed sweep's bytes**, through the ring where there is one
    /// and through the window where there is not.
    ///
    /// Once a pass, from `finish_prepare`. Every callback of the pass is asked;
    /// the first finds the filings and moves them, the rest find nothing and
    /// count nothing.
    pub fn drain(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        if self.pending.is_empty() {
            return;
        }
        let pending = std::mem::take(&mut self.pending);
        let mut plans = Vec::with_capacity(pending.len());
        let mut total = 0u64;
        for one in &pending {
            let (placed, next) = one.plan(total);
            total = next;
            plans.push(placed);
        }

        if self.stage(device, queue, &pending, &plans, total) {
            self.staged += 1;
            self.bytes += plans
                .iter()
                .flat_map(|placed| placed.iter())
                .map(Placed::bytes)
                .sum::<u64>();
            return;
        }
        self.declined += 1;
        for (one, placed) in pending.iter().zip(&plans) {
            write_direct(queue, one, placed);
        }
    }

    /// Memcpy the pass into one ring slot and start its copies, or say `false`.
    fn stage(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pending: &[Pending],
        plans: &[Vec<Placed>],
        total: u64,
    ) -> bool {
        if !self.capable || total == 0 {
            return false;
        }
        let slot_bytes = total.next_multiple_of(CHAIN_SLOT_GRANULARITY);
        if slot_bytes > MAX_STAGED_CHAIN_BYTES {
            return false;
        }
        let ring = self
            .ring
            .get_or_insert_with(|| Ring::new(device, slot_bytes, "squallar.radar_fan.staging"));
        ring.fit(device, slot_bytes);
        let Some(slot) = ring.claim(device) else {
            return false;
        };

        {
            let mut view = slot.buffer().get_mapped_range_mut(..total);
            for (one, placed) in pending.iter().zip(plans) {
                for region in placed {
                    let Some(source) = one.source(region) else {
                        continue;
                    };
                    let row = region.row_bytes as usize;
                    let mut rest = view.slice(region.offset as usize..);
                    for at in 0..region.rows as usize {
                        let (this, next) = rest.split_at(region.padded_row as usize);
                        // The padding past the real row is left as it was; the
                        // copy reads none of it.
                        this.into_slice(..row)
                            .copy_from_slice(&source[at * row..(at + 1) * row]);
                        rest = next;
                    }
                }
            }
        }
        slot.buffer().unmap();

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("squallar.radar_fan.staging"),
        });
        for (one, placed) in pending.iter().zip(plans) {
            for region in placed {
                if one.source(region).is_none() {
                    continue;
                }
                encoder.copy_buffer_to_texture(
                    wgpu::TexelCopyBufferInfo {
                        buffer: slot.buffer(),
                        layout: wgpu::TexelCopyBufferLayout {
                            offset: region.offset,
                            // The padded stride, not the row's own: a buffer
                            // copy is held to `COPY_BYTES_PER_ROW_ALIGNMENT`.
                            bytes_per_row: Some(region.padded_row),
                            rows_per_image: Some(region.rows),
                        },
                    },
                    wgpu::TexelCopyTextureInfo {
                        texture: one.texture(region),
                        mip_level: region.mip_level(),
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    region.extent(),
                );
            }
        }
        // Submitted here rather than on the frame's own encoder, for the reason
        // the raster and geometry paths submit theirs here: a map asked for
        // against an unsubmitted copy resolves early and panics at submission.
        // Queue order is what makes it land before the draw — this submission
        // is ahead of the one the caller has not made yet.
        queue.submit(Some(encoder.finish()));
        slot.remap();
        true
    }

    /// Pinned host memory this store's ring is holding.
    pub fn host_bytes(&self) -> usize {
        self.ring.as_ref().map_or(0, Ring::host_bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use squallar_device_profile::constants::full_mip_levels;

    /// A whole chain's `(level, rows, gates)`, by the halving a texture's mip
    /// extents are taken at.
    fn chain_of(radials: u32, gates: u32) -> Vec<(u32, u32, u32)> {
        (0..full_mip_levels(radials as usize, gates as usize) as u32)
            .map(|level| {
                (
                    level,
                    radials.checked_shr(level).unwrap_or(0).max(1),
                    gates.checked_shr(level).unwrap_or(0).max(1),
                )
            })
            .collect()
    }

    /// **Every region a chain is staged in is one a buffer copy can read.**
    ///
    /// The alignment is not a detail of the encoding:
    /// `copy_buffer_to_texture` refuses a stride that is not a multiple of
    /// [`wgpu::COPY_BYTES_PER_ROW_ALIGNMENT`], and a plan that packed its rows
    /// would be a device error raised on the frame thread — or, where the
    /// numbers happened to be accepted, one level's rows read at another
    /// level's stride, which is a picture of a radar with every radial sheared
    /// along the beam.
    ///
    /// **The property an input needs to reach that defect**: more than one mip
    /// level, and a level-0 row that is not already a multiple of the
    /// alignment. Both are asserted below rather than assumed. The suite's
    /// long-standing fixture — 360 x 200, ONE level — carries the second and
    /// not the first, so a per-level offset that was wrong from level 1 down
    /// was unreachable from it.
    ///
    /// TAMPER: drop the `next_multiple_of` in [`place`] and every stride row
    /// goes red; lay each level at `base` instead of accumulating and the
    /// offset rows do.
    #[test]
    fn every_level_is_placed_where_a_buffer_copy_can_read_it() {
        // The two real WSR-88D shapes and a square, so nothing here is tuned
        // to one row width.
        let mut checked = 0;
        for (radials, gates) in [(720u32, 1832u32), (720, 1192), (360, 256)] {
            let levels = chain_of(radials, gates);
            assert!(
                levels.len() > 1,
                "a one-level fixture cannot reach a per-level offset at all"
            );
            let (placed, total) = place(&levels, 0);
            assert_eq!(
                placed.len(),
                levels.len() + 1,
                "every level of the chain, and the table"
            );
            let mut at = 0u64;
            for region in &placed {
                assert_eq!(region.offset, at, "the regions are laid end to end");
                assert_eq!(
                    region.offset % u64::from(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT),
                    0,
                    "a region begins off the copy alignment"
                );
                assert_eq!(
                    region.padded_row % wgpu::COPY_BYTES_PER_ROW_ALIGNMENT,
                    0,
                    "a stride a buffer copy would refuse"
                );
                assert!(
                    region.padded_row >= region.row_bytes,
                    "a stride narrower than the row it carries"
                );
                assert!(
                    region.padded_row - region.row_bytes < wgpu::COPY_BYTES_PER_ROW_ALIGNMENT,
                    "a stride padded further than the alignment asked for"
                );
                at += region.staged_bytes();
                checked += 1;
            }
            assert_eq!(total, at, "the total is what the regions occupy");
            // The payload bytes the plan carries are the producer's own chain
            // and one table, so no level was placed at another level's size.
            let carried: u64 = placed.iter().map(Placed::bytes).sum();
            assert_eq!(
                carried,
                super::super::chain_bytes(radials as usize, gates as usize, levels.len()) as u64
                    + POLAR_LUT_BYTES as u64,
                "the plan carries something other than the chain and the table"
            );
        }
        assert!(checked > 20, "only {checked} regions were compared");

        // The premise every alignment row above rests on: a real sweep's rows
        // are NOT already aligned, so those assertions are the padding firing
        // and not an identity.
        let (surveillance, _) = place(&chain_of(720, 1832), 0);
        assert_ne!(
            surveillance[0].row_bytes % wgpu::COPY_BYTES_PER_ROW_ALIGNMENT,
            0,
            "a surveillance sweep's row needs no padding, so this suite proves nothing"
        );
        assert!(
            surveillance.iter().any(|r| r.padded_row > r.row_bytes),
            "no region was padded at all"
        );
    }

    /// A pass's second sweep is laid after the first, so one slot holds both.
    #[test]
    fn a_pass_lays_its_sweeps_end_to_end() {
        let (first, after_first) = place(&chain_of(720, 1832), 0);
        let (second, after_second) = place(&chain_of(720, 1192), after_first);
        assert_eq!(
            second[0].offset, after_first,
            "the second sweep starts where the first ended"
        );
        assert_eq!(
            after_first,
            first.iter().map(Placed::staged_bytes).sum::<u64>(),
            "the first sweep's total is what its regions occupy"
        );
        assert_eq!(
            after_second - after_first,
            second.iter().map(Placed::staged_bytes).sum::<u64>(),
            "the second sweep's total is what its regions occupy"
        );
        // Six of the worst shape is the frame a desktop layout's panes can
        // ask for, and it has to fit a slot this path will build.
        let mut at = 0;
        for _ in 0..6 {
            let (_, next) = place(&chain_of(720, 1832), at);
            at = next;
        }
        assert!(
            at.next_multiple_of(CHAIN_SLOT_GRANULARITY) <= MAX_STAGED_CHAIN_BYTES,
            "six panes' worst case is refused by this path's own ceiling"
        );
    }
}
