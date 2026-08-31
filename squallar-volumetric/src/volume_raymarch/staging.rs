//! Where a voxel grid's plane waits while it crosses PCIe.

use egui_wgpu::wgpu;

use super::{GRID_BYTES_PER_CELL, label};
use squallar_gpu::staging_ring::Ring;

/// Re-exported so the four GPU suites and `AppState::new` keep naming the same
/// path they always have. The definitions are [`squallar_gpu::staging_ring`]'s.
pub use squallar_gpu::staging_ring::{STAGING_RING_DEPTH, STAGING_RING_FEATURE};

/// The host memory an upload borrows: a ring of GPU-readable staging buffers
/// where the device has them, and the plain widening buffer everywhere else.
pub struct VolumeStaging {
    /// The host-side buffer the fallback widens into. See
    /// [`super::coverage_premultiplied_into`] for why the caller owns it.
    widening: Vec<u8>,
    /// `None` until the first upload, and forever on a device without
    /// [`STAGING_RING_FEATURE`].
    ring: Option<Ring>,
    /// Whether this device could have a ring at all — read once, at
    /// construction, so the hot path is a `bool` and not a feature-set test.
    capable: bool,
}

impl Default for VolumeStaging {
    /// Host memory only — no ring, ever.
    fn default() -> Self {
        Self {
            widening: Vec::new(),
            ring: None,
            capable: false,
        }
    }
}

impl VolumeStaging {
    /// Staging for `device`, with a ring if it can have one.
    pub fn new(device: &wgpu::Device) -> Self {
        let capable = squallar_gpu::staging_ring::device_has_ring(device);
        Self {
            widening: Vec::new(),
            ring: None,
            capable,
        }
    }

    /// Whether this device can stage through host memory at all.
    pub fn has_ring(&self) -> bool {
        self.capable
    }

    /// Host bytes this is holding: the ring's slots plus the widening buffer.
    pub fn host_bytes(&self) -> usize {
        let ring = self.ring.as_ref().map_or(0, Ring::host_bytes);
        ring.saturating_add(self.widening.len())
    }

    /// The widening buffer, for the `write_texture` fallback.
    pub(super) fn widening(&mut self) -> &mut Vec<u8> {
        &mut self.widening
    }

    /// Widen `indices` into a staging slot and start the copy into `z_count`
    /// depth layers of `grid`'s mip 0 starting at `z_from`, or say `false` and
    /// leave the caller to `write_texture`.
    ///
    /// `indices` is the band's own slice of the grid's index plane, not the
    /// whole of it: the ring is sized to a band and the widening pass walks a
    /// band, which is what keeps both off the frame thread's critical path.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn write_band(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        grid: &wgpu::Texture,
        cells: [u32; 3],
        z_from: u32,
        z_count: u32,
        indices: &[u8],
    ) -> bool {
        if !self.capable {
            return false;
        }
        let Some(layout) = PlaneLayout::of([cells[0], cells[1], z_count]) else {
            return false;
        };
        if indices.len() != layout.cells {
            return false;
        }

        let ring = self
            .ring
            .get_or_insert_with(|| Ring::new(device, layout.bytes, &label("grid.staging")));
        ring.grow(device, layout.bytes);
        let Some(slot) = ring.claim(device) else {
            return false;
        };

        widen_into_mapping(slot.buffer(), &layout, indices);
        slot.buffer().unmap();

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some(&label("grid.staging")),
        });
        encoder.copy_buffer_to_texture(
            wgpu::TexelCopyBufferInfo {
                buffer: slot.buffer(),
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    // The padded stride, not the plane's own: unlike
                    // `write_texture`, which repacks internally, a buffer copy
                    // is held to `COPY_BYTES_PER_ROW_ALIGNMENT`.
                    bytes_per_row: Some(layout.padded_row),
                    rows_per_image: Some(cells[1]),
                },
            },
            wgpu::TexelCopyTextureInfo {
                texture: grid,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: 0,
                    z: z_from,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: cells[0],
                height: cells[1],
                depth_or_array_layers: z_count,
            },
        );
        queue.submit(Some(encoder.finish()));

        // Ask for it back, and only now that the copy above has been submitted.
        // See `Slot::remap`.
        slot.remap();
        true
    }
}

/// How a grid's plane sits inside a buffer a copy can read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PlaneLayout {
    /// Bytes of real texels in a row: `cells[0] * GRID_BYTES_PER_CELL`.
    row: u32,
    /// That, rounded up to the copy alignment.
    padded_row: u32,
    /// Rows in the whole plane — `cells[1] * cells[2]`, not `cells[1]`.
    rows: usize,
    /// Cells in the whole plane, which is the index count this expects.
    cells: usize,
    /// Bytes a buffer must be to hold it.
    bytes: wgpu::BufferAddress,
}

impl PlaneLayout {
    /// `None` for a shape whose buffer would not fit in the address space, which
    /// is a shape `upload_refusal` will have turned away in any case.
    fn of(cells: [u32; 3]) -> Option<Self> {
        let row = cells[0].checked_mul(GRID_BYTES_PER_CELL)?;
        let padded_row = row
            .checked_next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            .filter(|_| row > 0)?;
        let rows = (cells[1] as usize).checked_mul(cells[2] as usize)?;
        let count = (cells[0] as usize).checked_mul(rows)?;
        let bytes = (padded_row as u64).checked_mul(rows as u64)?;
        Some(Self {
            row,
            padded_row,
            rows,
            cells: count,
            bytes,
        })
    }
}

/// Widen `indices` into `buffer`'s mapping, one padded row at a time.
fn widen_into_mapping(buffer: &wgpu::Buffer, layout: &PlaneLayout, indices: &[u8]) {
    const STRIDE: usize = GRID_BYTES_PER_CELL as usize;

    let texels = super::coverage_texels();
    let row = layout.row as usize;
    let width = row / STRIDE;

    let mut view = buffer.get_mapped_range_mut(..layout.bytes);
    let mut rest = view.slice(..);
    for index in 0..layout.rows {
        let (this, next) = rest.split_at(layout.padded_row as usize);
        // The padding past `row` is left exactly as it was. A buffer copy reads
        // none of it, and wgpu zero-initialised the whole allocation once at
        // creation, so there is no uninitialised memory here for the tail to be.
        let (out, _padding) = this.into_slice(..row).into_chunks::<STRIDE>();
        let source = &indices[index * width..(index + 1) * width];
        out.write_iter(source.iter().map(|&byte| texels[byte as usize]));
        rest = next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every shape a device can be handed has aligned rows, so the ring pads
    /// nothing.
    #[test]
    fn the_shipped_grid_shapes_need_no_row_padding_and_the_odd_ones_do() {
        let budgets = [
            squallar_device_profile::constants::WASM_VOLUME_GRID_CELLS,
            squallar_device_profile::constants::MOBILE_VOLUME_GRID_CELLS,
            squallar_device_profile::constants::DESKTOP_VOLUME_GRID_CELLS,
        ];
        let derived = budgets.into_iter().flat_map(|budget| {
            [256usize, 512, 704, 1024, 2048].map(|limit| {
                let shape = squallar_radar::voxel::shape_for_budget(
                    squallar_radar::voxel::VoxelShape {
                        nx: budget[0] as usize,
                        ny: budget[1] as usize,
                        nz: budget[2] as usize,
                    },
                    limit,
                );
                [shape.nx as u32, shape.ny as u32, shape.nz as u32]
            })
        });
        for cells in budgets.into_iter().chain(derived) {
            let layout = PlaneLayout::of(cells).expect("a shipped rung is expressible");
            assert_eq!(
                layout.row,
                layout.padded_row,
                "the {cells:?} rung's {}-byte row is no longer a multiple of \
                 {}, so its staging buffer is now bigger than its plane",
                layout.row,
                wgpu::COPY_BYTES_PER_ROW_ALIGNMENT,
            );
            assert_eq!(
                layout.bytes,
                super::super::grid_bytes(cells).expect("a shipped rung fits") as u64,
                "the {cells:?} rung's staging buffer is not its mip-0 plane — \
                 and the figure it is held to is `grid_bytes`, the packed \
                 payload, never `grid_bytes_at`, which is what the *device* \
                 reserves and is larger by the mip tail and the tiling",
            );
        }

        for cells in [[7u32, 5, 3], [65, 3, 2], [1, 1, 1]] {
            let layout = PlaneLayout::of(cells).expect("an odd shape is expressible");
            assert!(
                layout.padded_row > layout.row,
                "{cells:?} was picked to exercise the row padding and does not",
            );
            assert_eq!(layout.padded_row % wgpu::COPY_BYTES_PER_ROW_ALIGNMENT, 0);
        }
    }

    /// A shape whose buffer cannot be addressed is refused, not wrapped.
    #[test]
    fn a_plane_too_large_to_address_has_no_layout() {
        assert_eq!(PlaneLayout::of([u32::MAX, 2, 2]), None);
        assert_eq!(PlaneLayout::of([0, 4, 4]), None);
    }

    /// Host-only staging holds nothing and never claims a ring.
    #[test]
    fn staging_with_no_device_has_no_ring_and_holds_nothing() {
        let staging = VolumeStaging::default();
        assert!(!staging.has_ring());
        assert_eq!(staging.host_bytes(), 0);
    }

    /// The ring puts **the same bytes in the same texels** as `write_texture`.
    ///
    /// ```text
    /// cargo test -p squallar-volumetric --lib \
    ///     volume::raymarch::staging::tests::the_two_routes_write_the_same_plane \
    ///     -- --ignored --exact --nocapture
    /// ```
    #[test]
    #[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
    #[cfg(not(target_arch = "wasm32"))]
    fn the_two_routes_write_the_same_plane() {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::default(),
            force_fallback_adapter: false,
            compatible_surface: None,
        }))
        .expect("no wgpu adapter; this test is ignored by default for that reason");
        eprintln!("wgpu adapter: {:?}", adapter.get_info());

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("squallar.volume.staging.test"),
            required_features: adapter.features() & STAGING_RING_FEATURE,
            required_limits: adapter.limits(),
            memory_hints: Default::default(),
            experimental_features: Default::default(),
            trace: Default::default(),
        }))
        .expect("could not create a device on an adapter that was found");

        let mut staging = VolumeStaging::new(&device);
        eprintln!("staging ring available: {}", staging.has_ring());

        // Two turns of each shape, so the ring wraps and every slot is both
        // written and handed back at least once. A ring that never came back
        // would pass a single-shot test and starve on the second turn.
        let mut high_water = 0;
        let mut grows = 0;
        for turn in 0..2 {
            for cells in [
                [1u32, 1, 1],   // build the ring at its smallest
                [128, 128, 64], // grow — the wasm32 rung, one 4.00 MiB band
                [7, 5, 3],      // shrink onto that tail
                [65, 3, 2],     // shrink again, at the awkward stride
                [192, 192, 96], // the mobile rung: 4 bands, none of them wider
                [128, 128, 64], // and fall back, which must not shrink it
            ] {
                let count = (cells[0] as usize) * (cells[1] as usize) * (cells[2] as usize);
                let indices: Vec<u8> = (0..count)
                    .map(|i| (i.wrapping_add(cells[2] as usize + turn)) as u8)
                    .collect();

                let through_write_texture = texture(&device, cells);
                let plane = super::super::coverage_premultiplied_into(
                    VolumeStaging::default().widening(),
                    &indices,
                )
                .to_vec();
                queue.write_texture(
                    through_write_texture.as_image_copy(),
                    &plane,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(cells[0] * GRID_BYTES_PER_CELL),
                        rows_per_image: Some(cells[1]),
                    },
                    extent(cells),
                );

                // In bands, exactly as `advance_volume` walks it — so this
                // compares the routes production takes, not a whole-grid call
                // no frame makes. Both of them: the ring, and the banded
                // `write_texture` a device without one falls back to.
                let through_ring = texture(&device, cells);
                let through_banded_write = texture(&device, cells);
                let planes = super::super::band_planes(cells);
                let plane_cells = (cells[0] as usize) * (cells[1] as usize);
                let mut fallback = VolumeStaging::default();
                let mut took_the_ring = true;
                let mut bands = 0;
                let mut z = 0;
                while z < cells[2] {
                    let count = planes.min(cells[2] - z);
                    let from = z as usize * plane_cells;
                    let band = &indices[from..from + count as usize * plane_cells];
                    took_the_ring &=
                        staging.write_band(&device, &queue, &through_ring, cells, z, count, band);
                    assert!(
                        !fallback.write_band(
                            &device,
                            &queue,
                            &through_banded_write,
                            cells,
                            z,
                            count,
                            band,
                        ),
                        "host-only staging claimed a ring it cannot have",
                    );
                    queue.write_texture(
                        wgpu::TexelCopyTextureInfo {
                            texture: &through_banded_write,
                            mip_level: 0,
                            origin: wgpu::Origin3d { x: 0, y: 0, z },
                            aspect: wgpu::TextureAspect::All,
                        },
                        super::super::coverage_premultiplied_into(fallback.widening(), band),
                        wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(cells[0] * GRID_BYTES_PER_CELL),
                            rows_per_image: Some(cells[1]),
                        },
                        wgpu::Extent3d {
                            width: cells[0],
                            height: cells[1],
                            depth_or_array_layers: count,
                        },
                    );
                    bands += 1;
                    z += count;
                }
                let banded_write = read_back(&device, &queue, &through_banded_write, cells);
                let whole_write = read_back(&device, &queue, &through_write_texture, cells);
                assert!(
                    whole_write.iter().any(|&b| b != 0),
                    "turn {turn}, {cells:?}: the reference plane is all zeroes, \
                     so an upload that wrote nothing at all would pass",
                );
                assert_eq!(
                    banded_write, whole_write,
                    "turn {turn}, {cells:?}: the grid written {bands} bands at a \
                     time is not the grid written in one call — a band boundary \
                     is losing, duplicating or misplacing a depth plane",
                );
                assert_eq!(
                    bands,
                    cells[2].div_ceil(planes),
                    "turn {turn}, {cells:?}: the walk took {bands} bands, not the \
                     {} its shape asks for",
                    cells[2].div_ceil(planes),
                );
                assert_eq!(
                    took_the_ring,
                    staging.has_ring(),
                    "turn {turn}, {cells:?}: the ring's answer disagrees with \
                     whether this device has one — either a capable device \
                     starved its own ring on a walk that uploads one plane at a \
                     time, or an incapable one claimed to have written a plane \
                     it cannot",
                );
                if !took_the_ring {
                    continue;
                }

                // The ring only ever grows, and it grew exactly when this shape
                // was the widest yet.
                let held = staging.host_bytes();
                if held > high_water {
                    grows += 1;
                    assert_eq!(
                        turn, 0,
                        "turn {turn}, {cells:?}: the ring resized on a replay of \
                         a walk it has already been through, so it is not \
                         grow-only after all and the pages this module says are \
                         bought once are being bought again",
                    );
                    high_water = held;
                }
                assert_eq!(
                    held, high_water,
                    "turn {turn}, {cells:?}: the ring shrank to fit a smaller \
                     plane, so the next larger grid pays for its pages again",
                );

                let expected = read_back(&device, &queue, &through_write_texture, cells);
                let got = read_back(&device, &queue, &through_ring, cells);
                assert_eq!(
                    expected.len(),
                    count * GRID_BYTES_PER_CELL as usize,
                    "turn {turn}, {cells:?}: the readback is not the plane's own \
                     length, so the comparison below is over the wrong bytes",
                );
                assert!(
                    expected.iter().any(|&b| b != 0),
                    "turn {turn}, {cells:?}: the reference plane is all zeroes, \
                     so an upload that wrote nothing at all would pass",
                );
                assert_eq!(
                    got, expected,
                    "turn {turn}, {cells:?}: the staging ring wrote different \
                     texels from `write_texture` — the grid the raymarch samples \
                     now depends on which route its plane took",
                );
            }
        }

        if staging.has_ring() {
            assert_eq!(
                grows, 2,
                "the walk above resized the ring {grows} times, not the two the \
                 shape order was built to force (build at [1,1,1], then the \
                 first shape whose band is a whole one) — so `Ring::grow`'s \
                 body is going unchecked and a session that widens its region \
                 box is relying on code no test runs",
            );
            assert_eq!(
                staging.host_bytes(),
                usize::try_from(
                    PlaneLayout::of([128, 128, super::super::band_planes([128, 128, 64])])
                        .expect("a band of the wasm32 rung")
                        .bytes
                )
                .expect("a band fits")
                    * STAGING_RING_DEPTH,
                "the ring did not settle at one band a slot — and a band, not a \
                 grid, is the whole point: the mobile rung's 13.50 MiB plane \
                 walked past here in four bands and never widened it",
            );
        }
    }

    /// What a ring-capable device holds once it has also taken the fallback.
    ///
    /// ```text
    /// cargo test -p squallar-volumetric --lib \
    ///     volume::raymarch::staging::tests::the_worst_case_residency_is_the_ring_and_the_widening_together \
    ///     -- --ignored --exact --nocapture
    /// ```
    #[test]
    #[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
    #[cfg(not(target_arch = "wasm32"))]
    fn the_worst_case_residency_is_the_ring_and_the_widening_together() {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::default(),
            force_fallback_adapter: false,
            compatible_surface: None,
        }))
        .expect("no wgpu adapter; this test is ignored by default for that reason");
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("squallar.volume.staging.residency"),
            required_features: adapter.features() & STAGING_RING_FEATURE,
            required_limits: adapter.limits(),
            memory_hints: Default::default(),
            experimental_features: Default::default(),
            trace: Default::default(),
        }))
        .expect("could not create a device on an adapter that was found");

        let mut staging = VolumeStaging::new(&device);
        if !staging.has_ring() {
            eprintln!(
                "no staging ring on this adapter; the fallback holds one plane and that is all"
            );
            return;
        }

        let cells = squallar_device_profile::constants::DESKTOP_VOLUME_GRID_CELLS;
        let planes = super::super::band_planes(cells);
        let band = PlaneLayout::of([cells[0], cells[1], planes])
            .expect("a band of the desktop rung")
            .bytes as usize;
        let plane_cells = (cells[0] as usize) * (cells[1] as usize);
        let count = plane_cells * cells[2] as usize;
        let indices = vec![7u8; count];

        assert_eq!(staging.host_bytes(), 0, "a fresh staging holds nothing");

        let grid = texture(&device, cells);
        assert!(staging.write_band(
            &device,
            &queue,
            &grid,
            cells,
            0,
            planes,
            &indices[..planes as usize * plane_cells],
        ));
        let steady = staging.host_bytes();
        assert_eq!(
            steady,
            band * STAGING_RING_DEPTH,
            "the steady state on the desktop rung is not the {STAGING_RING_DEPTH} \
             × 4.00 MiB a banded upload asks for",
        );
        assert_eq!(steady, 8 << 20, "…and that is 8.00 MiB");

        // Now the fallback, exactly as `advance_volume` takes it: one band, not
        // the grid.
        super::super::coverage_premultiplied_into(
            staging.widening(),
            &indices[..planes as usize * plane_cells],
        );
        let worst = staging.host_bytes();
        assert_eq!(
            worst,
            steady + band,
            "one starved band did not add a whole widening buffer, so the worst \
             case the docs quote is not what the code reaches",
        );
        assert_eq!(
            worst,
            12 << 20,
            "the worst-case desktop residency is not the 12.00 MiB a banded \
             upload holds — it was 96.00 MiB while a whole 32.00 MiB plane \
             crossed in one call",
        );

        // And it is permanent: a later ring upload does not give it back.
        assert!(staging.write_band(
            &device,
            &queue,
            &grid,
            cells,
            0,
            planes,
            &indices[..planes as usize * plane_cells],
        ));
        assert_eq!(
            staging.host_bytes(),
            worst,
            "the widening buffer was released once the ring recovered — which \
             would be a smaller worst case than the docs promise, but the `Vec` \
             only grows, so this failing means the accounting is wrong",
        );
    }

    /// A 3D grid texture a test can read back out of.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    fn texture(device: &wgpu::Device, cells: [u32; 3]) -> wgpu::Texture {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some("squallar.volume.staging.test.grid"),
            size: extent(cells),
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D3,
            format: crate::VOLUME_TEXTURE_FORMAT,
            // `COPY_SRC` beside the production pair, and only here: the shipped
            // grid does not carry it, so this is a texture of the test's own
            // rather than one borrowed from `upload_volume_at`.
            usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        })
    }

    #[cfg(all(test, not(target_arch = "wasm32")))]
    fn extent(cells: [u32; 3]) -> wgpu::Extent3d {
        wgpu::Extent3d {
            width: cells[0],
            height: cells[1],
            depth_or_array_layers: cells[2],
        }
    }

    /// The texels of a 3D grid, row padding stripped, in the plane's own order.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    fn read_back(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        texture: &wgpu::Texture,
        cells: [u32; 3],
    ) -> Vec<u8> {
        let layout = PlaneLayout::of(cells).expect("a test shape is expressible");
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("squallar.volume.staging.test.readback"),
            size: layout.bytes,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(layout.padded_row),
                    rows_per_image: Some(cells[1]),
                },
            },
            extent(cells),
        );
        queue.submit(Some(encoder.finish()));
        readback.slice(..).map_async(wgpu::MapMode::Read, |result| {
            result.expect("mapping the readback buffer failed");
        });
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("polling the device failed");

        let mapped = readback.slice(..).get_mapped_range();
        let row = layout.row as usize;
        let mut plane = Vec::with_capacity(row * layout.rows);
        for index in 0..layout.rows {
            let at = index * layout.padded_row as usize;
            plane.extend_from_slice(&mapped[at..at + row]);
        }
        plane
    }
}
