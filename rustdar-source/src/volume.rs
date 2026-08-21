//! The 3D output shape a source hands back: [`VolumeGrid`] and the
//! [`TransferTable`] that says how its cells become colour.
//!
//! A volume grid is one scalar field resampled onto an axis-aligned box of
//! palette indices — the shape a raymarcher uploads as one 3D texture and one
//! 1D colour table. Nothing here knows what a radar is: the box is placed by a
//! tangent-plane anchor and three kilometre ranges, the field is a
//! [`FieldId`](crate::product::FieldId), and every physics-derived fact about
//! how the values map to colour is *stored* in the transfer table rather than
//! recomputed from a source's own enum.
//!
//! **The storage rule is binding and it is deliberately not the tidy one.**
//! Placement is stored as the tangent-plane primitives the builder computed —
//! the anchor, the two horizontal kilometre ranges, the vertical range — and
//! [`VolumeGrid::footprint`] *derives* geography from them. It is never the
//! other way round. Storing a [`GeoBounds`] and converting back to kilometres
//! drifts the floats: the wire payload is a byte-exact record of those
//! primitives, and the renderer compares a box built here against a box built
//! for the same target elsewhere with `==`. A round trip through geography
//! breaks both.
//!
//! **The Mesh seam, named and deliberately not built.** A source that answers
//! with a triangle mesh rather than a sampled box would widen this module with
//! its own sibling type beside `VolumeGrid` — a `VolumeOutput` enum, or a
//! second associated output on whatever asks. It is not stubbed here: there is
//! no second consumer, an unused variant is a claim nothing tests, and the
//! shape of the widening is decided by the first real mesh source, not
//! guessed at by the first volume one.

use rustdar_geo::GeoBounds;

use crate::product::FieldId;

/// The palette index meaning "nothing was measured here", and simultaneously
/// the bottom of the affine value ramp.
pub const NO_DATA_INDEX: u8 = 0;

/// Bytes in a [`TransferTable::lut`]: 256 entries × RGBA.
pub const LUT_LEN: usize = 256 * 4;

/// The alpha at or under which a table entry counts as see-through for
/// [`TransferTable::see_through_indices`] — a quarter opacity.
pub const SEE_THROUGH_ALPHA_CEILING: u8 = 64;

/// The largest any axis may be for the wire and the arithmetic — the largest
/// `n` with `n³ ≤ u32::MAX`, since [`VolumeDims::cells`] multiplies three
/// untrusted `u32`s and `usize` is 32 bits on wasm32.
pub const MAX_AXIS: usize = largest_cubable_axis();

/// The largest `n` with `n³ ≤ u32::MAX`.
const fn largest_cubable_axis() -> usize {
    let mut n: u64 = 1;
    while (n + 1) * (n + 1) * (n + 1) <= u32::MAX as u64 {
        n += 1;
    }
    n as usize
}

/// How many cells a grid has along each axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VolumeDims {
    pub nx: usize,
    pub ny: usize,
    pub nz: usize,
}

impl VolumeDims {
    /// Total cells — the length of [`VolumeGrid::indices`].
    pub const fn cells(self) -> usize {
        self.nx * self.ny * self.nz
    }

    /// Whether every axis is between 1 and [`MAX_AXIS`] inclusive.
    pub const fn is_supported(self) -> bool {
        const fn ok(n: usize) -> bool {
            n >= 1 && n <= MAX_AXIS
        }
        ok(self.nx) && ok(self.ny) && ok(self.nz)
    }
}

/// How the colour table itself must be sampled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LutFilter {
    /// The field's scale interpolates between stops, so the table may be
    /// interpolated too.
    Linear,
    /// The field's scale steps.
    Nearest,
}

/// How a field's isosurface threshold reads its scale.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum IsoShape {
    /// The surface of `value >= threshold`: the sequential fields, whose
    /// interesting side is up-scale.
    Sequential,
    /// The surface of `|value − centre| >= threshold`: the diverging fields,
    /// whose interesting surfaces sit on both sides of their background.
    DeviationFrom { centre: f32 },
    /// The surface of `value <= threshold`: a field whose background is the
    /// top of its scale. Implemented as a deviation from the ramp top.
    AtOrBelow,
}

/// Everything about turning a cell's palette index back into a number, a
/// colour, or a surface — computed once by the source that knows the physics
/// and then **stored**, never re-derived from a source-private enum.
///
/// The table is what lets a consumer read a volume without naming the source
/// that built it: before this existed, six of these facts were methods that
/// matched on a radar product, which is why the grid had to carry one.
#[derive(Clone, Debug, PartialEq)]
pub struct TransferTable {
    lut: Vec<u8>,
    filter: LutFilter,
    wraps: bool,
    value_range: (f32, f32),
    iso_shape: IsoShape,
    default_iso_threshold: f32,
    fade_band: u8,
    see_through_indices: u16,
}

impl TransferTable {
    /// Build a table over `lut`, deciding at build time the two facts that are
    /// functions of the table's own bytes — [`fade_band`](Self::fade_band) and
    /// [`see_through_indices`](Self::see_through_indices).
    ///
    /// They are computed here rather than on demand so that a consumer holding
    /// a grid pays nothing to ask, and so that nothing downstream needs the
    /// rule that produced them.
    pub fn new(
        lut: Vec<u8>,
        filter: LutFilter,
        wraps: bool,
        value_range: (f32, f32),
        iso_shape: IsoShape,
        default_iso_threshold: f32,
    ) -> Self {
        let fade_band = match lut.chunks_exact(4).position(|entry| entry[3] != 0) {
            // Entry 0 is forced transparent, so the band under the first
            // opaque entry is `n − 1` wide.
            Some(n) => n.saturating_sub(1) as u8,
            // No opaque entry anywhere: the whole ramp fades.
            None => u8::MAX,
        };
        let see_through_indices = lut
            .chunks_exact(4)
            .skip(1)
            .filter(|entry| entry[3] <= SEE_THROUGH_ALPHA_CEILING)
            .count() as u16;
        Self {
            lut,
            filter,
            wraps,
            value_range,
            iso_shape,
            default_iso_threshold,
            fade_band,
            see_through_indices,
        }
    }

    /// Exactly [`LUT_LEN`] bytes: 256 RGBA entries, entry `i` the colour of
    /// index `i`.
    pub fn lut(&self) -> &[u8] {
        &self.lut
    }

    /// How [`lut`](Self::lut) must be sampled.
    pub fn filter(&self) -> LutFilter {
        self.filter
    }

    /// Whether the field is circular, so that the two ends of the ramp are the
    /// same physical value and a linear filter across the seam returns the
    /// opposite phase rather than a blend.
    pub fn wraps(&self) -> bool {
        self.wraps
    }

    /// The values index 0 and index 255 stand for.
    pub fn value_range(&self) -> (f32, f32) {
        self.value_range
    }

    /// What a threshold over this field *means*.
    pub fn iso_shape(&self) -> IsoShape {
        self.iso_shape
    }

    /// The threshold used when the caller has no finite one of its own.
    pub fn default_iso_threshold(&self) -> f32 {
        self.default_iso_threshold
    }

    /// How many indices above [`NO_DATA_INDEX`] the table is still fully
    /// transparent — the width, in index steps, of the band a `Linear` fetch
    /// fades through when it straddles an echo edge.
    pub fn fade_band(&self) -> u8 {
        self.fade_band
    }

    /// How many of the 255 **data** entries are see-through — at or under
    /// [`SEE_THROUGH_ALPHA_CEILING`] — wherever they sit on the ramp.
    pub fn see_through_indices(&self) -> u16 {
        self.see_through_indices
    }

    /// The value index `i` stands for.
    pub fn index_to_value(&self, index: u8) -> f32 {
        ramp_value(self.value_range, index)
    }

    /// The index a value encodes to.
    pub fn value_to_index(&self, value: f32) -> u8 {
        ramp_index(self.value_range, value)
    }

    /// The isosurface uniform pair `(centre, threshold)` for a user-facing
    /// threshold in the field's own units, both in the shader's 0-1 index
    /// space.
    pub fn iso_uniform_params(&self, user_threshold: f32) -> (f32, f32) {
        let user = if user_threshold.is_finite() {
            user_threshold
        } else {
            self.default_iso_threshold
        };
        let norm = |index: u8| f32::from(index) / 255.0;
        match self.iso_shape {
            IsoShape::Sequential => (-1.0, norm(self.value_to_index(user))),
            IsoShape::DeviationFrom { centre } => {
                let c = self.value_to_index(centre);
                let at = self.value_to_index(centre + user.abs());
                (norm(c), norm(at.saturating_sub(c).max(1)))
            }
            IsoShape::AtOrBelow => {
                let top = 255u8;
                let at = self.value_to_index(user);
                (norm(top), norm(top.saturating_sub(at).max(1)))
            }
        }
    }
}

/// The parts one [`VolumeGrid`] is assembled from.
///
/// A named struct rather than a dozen positional arguments: the builder and
/// the wire decoder both fill it in, and a transposed pair of kilometre ranges
/// would be invisible in a call.
pub struct VolumeParts {
    pub indices: Vec<u8>,
    pub values: Option<Vec<f32>>,
    pub dims: VolumeDims,
    /// `(latitude, longitude)` of the tangent plane's origin — the point the
    /// `x`/`y` ranges are measured from. **Stored, never derived.**
    pub anchor: (f64, f64),
    /// Km east of the anchor at the box's west and east faces.
    pub x_range_km: (f64, f64),
    /// Km north of the anchor at the box's south and north faces.
    pub y_range_km: (f64, f64),
    /// Km MSL at the box's bottom and top faces.
    pub z_range_km_msl: (f64, f64),
    pub field: FieldId,
    pub transfer: TransferTable,
    /// How many source levels the ladder had when this grid was resampled.
    pub levels: usize,
    /// The largest angular step between adjacent levels, degrees.
    pub widest_level_gap_deg: f64,
}

/// **A layer that can build a [`VolumeGrid`].**
///
/// [`SourceHandler::volume`](crate::handler::SourceHandler::volume) is how a
/// layer answers "have I a 3D half at all", and this is what it answers
/// *with*. A pane in Volume mode finds the layer to ask by walking its own
/// stack for the first one that says yes — so the 3D view has no idea which
/// of its layers is radar, which is the whole point of the seam.
///
/// **Minimal on purpose, and the remainder is named.** The job-shaping half —
/// the request the builder is handed and the job envelope it comes back in —
/// moves behind this trait at WO-M14b-2. Today the one member is the question
/// the pane's walk asks and the dispatcher re-asks on the far side of the
/// action channel: can this layer build a volume *of this field*.
pub trait VolumeCapable {
    /// Whether this layer can build a volume of `field`.
    ///
    /// **Defaulted to the field's own registered
    /// [`vertical`](crate::product::ProductSpec::vertical)** — the fact that
    /// already says whether a field has a third dimension to render. A layer
    /// whose 3D answer is exactly "every field of mine with vertical extent"
    /// writes nothing here; one whose answer is *narrower* than its own
    /// registry rows overrides, and the override is the place that says why.
    ///
    /// Takes the whole [`ProductSpec`](crate::product::ProductSpec) rather
    /// than a [`FieldId`]: the caller has already resolved the row, and
    /// handing the id back would make every implementor look it up again in a
    /// table only it can see.
    fn builds(&self, field: &crate::product::ProductSpec) -> bool {
        field.vertical
    }
}

/// A resampled Cartesian volume, ready to become one 3D texture and one 1D
/// colour table.
#[derive(Clone)]
pub struct VolumeGrid {
    indices: Vec<u8>,
    values: Option<Vec<f32>>,
    dims: VolumeDims,
    anchor: (f64, f64),
    x_range_km: (f64, f64),
    y_range_km: (f64, f64),
    z_range_km_msl: (f64, f64),
    field: FieldId,
    transfer: TransferTable,
    levels: usize,
    widest_level_gap_deg: f64,
}

/// One line, never the grid.
impl std::fmt::Debug for VolumeGrid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let filled = self.indices.iter().filter(|&&i| i != NO_DATA_INDEX).count();
        write!(
            f,
            "{} {}x{}x{} x{:?} y{:?} z{:?} km msl, anchor {:?}, range {:?}, \
             {} levels (widest gap {:.2}°), {filled}/{} cells with data, \
             values {}",
            self.field,
            self.dims.nx,
            self.dims.ny,
            self.dims.nz,
            self.x_range_km,
            self.y_range_km,
            self.z_range_km_msl,
            self.anchor,
            self.transfer.value_range,
            self.levels,
            self.widest_level_gap_deg,
            self.indices.len(),
            if self.values.is_some() {
                "kept"
            } else {
                "dropped"
            },
        )
    }
}

/// Equality that compares the value plane **bitwise**.
///
/// Hand-written, not derived, and the reason is the value plane: a cell with
/// no data holds `NaN`, and a derived `PartialEq` makes every such grid unequal
/// to itself — which is exactly what a wire round-trip asserts against.
impl PartialEq for VolumeGrid {
    fn eq(&self, other: &Self) -> bool {
        fn same_values(a: Option<&Vec<f32>>, b: Option<&Vec<f32>>) -> bool {
            match (a, b) {
                (None, None) => true,
                (Some(a), Some(b)) => {
                    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits())
                }
                _ => false,
            }
        }
        self.dims == other.dims
            && self.field == other.field
            && self.levels == other.levels
            && self.widest_level_gap_deg == other.widest_level_gap_deg
            && self.x_range_km == other.x_range_km
            && self.y_range_km == other.y_range_km
            && self.z_range_km_msl == other.z_range_km_msl
            && self.anchor == other.anchor
            && self.transfer.value_range == other.transfer.value_range
            && self.indices == other.indices
            && self.transfer.lut == other.transfer.lut
            && same_values(self.values.as_ref(), other.values.as_ref())
    }
}

/// How many points each edge of the box is sampled at for
/// [`VolumeGrid::footprint`], corners included.
///
/// The box is straight in the tangent plane and curved in latitude/longitude,
/// so its four corners do **not** bound it: the north edge of a wide box bulges
/// polewards between them. `a_footprint_covers_the_whole_curved_perimeter`
/// measures that bulge on a full-width box and shows corner-only bounds
/// missing it.
const FOOTPRINT_EDGE_SAMPLES: usize = 65;

impl VolumeGrid {
    pub fn from_parts(parts: VolumeParts) -> Self {
        let VolumeParts {
            indices,
            values,
            dims,
            anchor,
            x_range_km,
            y_range_km,
            z_range_km_msl,
            field,
            transfer,
            levels,
            widest_level_gap_deg,
        } = parts;
        Self {
            indices,
            values,
            dims,
            anchor,
            x_range_km,
            y_range_km,
            z_range_km_msl,
            field,
            transfer,
            levels,
            widest_level_gap_deg,
        }
    }

    /// One palette index per cell, `nx·ny·nz` of them, ordered
    /// `z·(ny·nx) + y·nx + x`.
    pub fn indices(&self) -> &[u8] {
        &self.indices
    }

    /// The same cells in the field's own units, `NaN` wherever
    /// [`indices`](Self::indices) holds [`NO_DATA_INDEX`].
    pub fn values(&self) -> Option<&[f32]> {
        self.values.as_deref()
    }

    pub fn dims(&self) -> VolumeDims {
        self.dims
    }

    /// Cells in the grid — [`VolumeDims::cells`] of its own dims.
    pub fn cells(&self) -> usize {
        self.dims.cells()
    }

    /// Which field these cells measure.
    pub fn field(&self) -> &FieldId {
        &self.field
    }

    /// How a cell's index becomes a number, a colour or a surface.
    pub fn transfer(&self) -> &TransferTable {
        &self.transfer
    }

    /// The tangent plane's origin as `(latitude, longitude)` — the point the
    /// `x`/`y` ranges are measured from. **This is stored placement**; see the
    /// module doc for why it is never derived from [`footprint`](Self::footprint).
    pub fn anchor(&self) -> (f64, f64) {
        self.anchor
    }

    /// Km east of the anchor at the box's west and east faces.
    pub fn x_range_km(&self) -> (f64, f64) {
        self.x_range_km
    }

    /// Km north of the anchor at the box's south and north faces.
    pub fn y_range_km(&self) -> (f64, f64) {
        self.y_range_km
    }

    /// Km MSL at the box's bottom and top faces.
    pub fn z_range_km_msl(&self) -> (f64, f64) {
        self.z_range_km_msl
    }

    /// Km MSL at the box's bottom face.
    pub fn floor_km(&self) -> f64 {
        self.z_range_km_msl.0
    }

    /// Km MSL at the box's top face.
    pub fn ceil_km(&self) -> f64 {
        self.z_range_km_msl.1
    }

    /// Where this box sits on the earth — **derived** from the stored anchor
    /// and kilometre ranges, every time it is asked for.
    ///
    /// The perimeter is walked at [`FOOTPRINT_EDGE_SAMPLES`] points per edge
    /// rather than at the four corners, because a box that is straight in the
    /// tangent plane is curved in latitude/longitude and its extremes are not
    /// at its corners.
    ///
    /// A consumer that needs the box's *placement* reads
    /// [`anchor`](Self::anchor) and the kilometre ranges. This is the
    /// geographic summary, and it is never fed back the other way.
    pub fn footprint(&self) -> GeoBounds {
        let at = |x_km: f64, y_km: f64| {
            let bearing_deg = x_km.atan2(y_km).to_degrees().rem_euclid(360.0);
            rustdar_geo::great_circle_destination(
                self.anchor.0,
                self.anchor.1,
                bearing_deg,
                x_km.hypot(y_km),
            )
        };
        let lerp = |(lo, hi): (f64, f64), i: usize| {
            lo + (hi - lo) * (i as f64) / ((FOOTPRINT_EDGE_SAMPLES - 1) as f64)
        };
        let perimeter = (0..FOOTPRINT_EDGE_SAMPLES).flat_map(|i| {
            let x = lerp(self.x_range_km, i);
            let y = lerp(self.y_range_km, i);
            [
                at(x, self.y_range_km.0),
                at(x, self.y_range_km.1),
                at(self.x_range_km.0, y),
                at(self.x_range_km.1, y),
            ]
        });
        // `FOOTPRINT_EDGE_SAMPLES` is a non-zero literal, so the fold always
        // sees a point.
        GeoBounds::from_points(perimeter).unwrap_or(GeoBounds {
            min_lat: self.anchor.0,
            max_lat: self.anchor.0,
            min_lon: self.anchor.1,
            max_lon: self.anchor.1,
        })
    }

    /// How many source levels the ladder had when this grid was resampled.
    pub fn levels(&self) -> usize {
        self.levels
    }

    /// The largest angular step between adjacent levels, degrees — `0.0` for a
    /// single-level ladder.
    pub fn widest_level_gap_deg(&self) -> f64 {
        self.widest_level_gap_deg
    }

    /// Exactly [`LUT_LEN`] bytes: 256 RGBA entries, entry `i` the colour of
    /// index `i`.
    pub fn lut(&self) -> &[u8] {
        self.transfer.lut()
    }

    /// The values index 0 and index 255 stand for.
    pub fn value_range(&self) -> (f32, f32) {
        self.transfer.value_range()
    }

    /// How [`lut`](Self::lut) must be sampled.
    pub fn lut_filter(&self) -> LutFilter {
        self.transfer.filter()
    }

    /// Whether the field is circular; see [`TransferTable::wraps`].
    pub fn wraps(&self) -> bool {
        self.transfer.wraps()
    }

    /// The value index `i` stands for.
    pub fn index_to_value(&self, index: u8) -> f32 {
        self.transfer.index_to_value(index)
    }

    /// The index a value encodes to.
    pub fn value_to_index(&self, value: f32) -> u8 {
        self.transfer.value_to_index(value)
    }

    /// See [`TransferTable::fade_band`].
    pub fn fade_band(&self) -> u8 {
        self.transfer.fade_band()
    }

    /// See [`TransferTable::see_through_indices`].
    pub fn see_through_indices(&self) -> u16 {
        self.transfer.see_through_indices()
    }

    /// See [`TransferTable::iso_uniform_params`].
    pub fn iso_uniform_params(&self, user_threshold: f32) -> (f32, f32) {
        self.transfer.iso_uniform_params(user_threshold)
    }

    /// The offset of cell `(x, y, z)` in [`indices`](Self::indices) and
    /// [`values`](Self::values). `None` outside the grid.
    pub fn cell_offset(&self, x: usize, y: usize, z: usize) -> Option<usize> {
        (x < self.dims.nx && y < self.dims.ny && z < self.dims.nz)
            .then(|| z * self.dims.ny * self.dims.nx + y * self.dims.nx + x)
    }

    /// The index at cell `(x, y, z)`, or `None` outside the grid.
    pub fn index_at(&self, x: usize, y: usize, z: usize) -> Option<u8> {
        self.cell_offset(x, y, z).map(|o| self.indices[o])
    }

    /// The value at cell `(x, y, z)`, or `None` outside the grid or with no
    /// value plane. `Some(NaN)` where there is no data.
    pub fn value_at(&self, x: usize, y: usize, z: usize) -> Option<f32> {
        let o = self.cell_offset(x, y, z)?;
        self.values.as_ref().map(|v| v[o])
    }

    /// The centre of cell `(x, y, z)` as `(km east, km north, km MSL)`, all
    /// relative to [`anchor`](Self::anchor) except the last which is MSL.
    pub fn cell_centre_km(&self, x: usize, y: usize, z: usize) -> Option<(f64, f64, f64)> {
        self.cell_offset(x, y, z)?;
        Some((
            axis_centre(self.x_range_km, self.dims.nx, x),
            axis_centre(self.y_range_km, self.dims.ny, y),
            axis_centre(self.z_range_km_msl, self.dims.nz, z),
        ))
    }

    /// Bytes this grid holds: index plane, value plane if present, and table.
    pub fn memory_bytes(&self) -> usize {
        self.indices.len() + self.values.as_ref().map_or(0, |v| v.len() * 4) + self.lut().len()
    }
}

/// The reply half of the job boundary's erasure seam: a described volume
/// carries no straight-alpha raster, so the funnel premultiplies nothing.
impl crate::job::JobOut for VolumeGrid {
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

/// The centre of cell `i` on an axis spanning `range` in `n` cells.
pub fn axis_centre(range: (f64, f64), n: usize, i: usize) -> f64 {
    range.0 + (i as f64 + 0.5) * (range.1 - range.0) / n as f64
}

/// The value palette index `i` stands for, affine over the whole 0..=255.
///
/// The free spelling exists because the ramp is needed to BUILD a table's
/// bytes, before there is a table to ask. A consumer holding one asks it:
/// [`TransferTable::index_to_value`] is this function bound to that table's
/// own range, and is the only spelling that cannot be handed the wrong ramp.
pub fn ramp_value(range: (f32, f32), index: u8) -> f32 {
    let (lo, hi) = range;
    lo + (hi - lo) * (f32::from(index) / 255.0)
}

/// The inverse, clamped to `1..=255` so no finite measurement encodes as
/// [`NO_DATA_INDEX`]. See [`ramp_value`] for why the free spelling exists;
/// [`TransferTable::value_to_index`] is the bound one.
pub fn ramp_index(range: (f32, f32), value: f32) -> u8 {
    if !value.is_finite() {
        return NO_DATA_INDEX;
    }
    let (lo, hi) = (f64::from(range.0), f64::from(range.1));
    let step = (f64::from(value) - lo) / (hi - lo) * 255.0;
    if !step.is_finite() {
        return NO_DATA_INDEX;
    }
    step.round().clamp(1.0, 255.0) as u8
}

#[cfg(test)]
mod tests;
