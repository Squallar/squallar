//! What the readout under the pointer reads, and where it reads it from.
//!
//! # One rule, two places the numbers can live
//!
//! Picking the gate is [`crate::render::polar::PolarGeometry::pick`] and only
//! ever that — the inverse of `render_gate`'s forward paint, spelt once, so the
//! number the readout prints and the colour under the cursor are the same
//! gate's. What this module adds is the second question, which the geometry
//! does not answer: *who is holding that gate's value*.
//!
//! There are two answers, and which one applies is a memory decision rather
//! than a correctness one.
//!
//!   * A **still** pane's render carries its own numbers, in the polar layout
//!     it painted from — `radials × gates`, about 5 MiB for the widest sweep
//!     the fleet flies. That replaced a `side²` raster grid of the same values
//!     resampled up to 206.75 MiB, which is what this whole arrangement exists
//!     to have stopped doing.
//!   * A **loop** frame carries none. It cannot: a loop holds up to 36 frames
//!     on desktop and 14 in a browser, and 14 × 5.03 MiB is 70 MiB of a
//!     288 MiB budget — worse than the raster grid that path already refuses.
//!     What it carries instead is [`SweepGates`], which is an `Arc` on the
//!     volume the frame was rendered from and two integers. The volume is
//!     resident anyway — `LoopDownloadManager`'s scan cache holds every frame's
//!     for as long as the loop lives — so the readout costs a refcount.
//!
//! # Why this is a type and not a trait object
//!
//! `rustdar-egui` draws the pane and asks the question, and it cannot name a
//! `Scan`: `nexrad-model` is not one of its dependencies and is not going to
//! become one, because a pane has no business holding a decoded volume. So the
//! answer has to arrive as something opaque. A trait object would do it, and
//! costs a vtable and an allocation per render for a closed set of two cases
//! that both live in this crate. [`HoverSource`] is that set, by value.
//!
//! # What it costs to ask
//!
//! [`HoverSource::read`] runs on the frame thread, on every frame the pointer
//! is over a pane. It is a linear scan of the wedges — 720 float comparisons
//! for a full ring, walked from the far end so a point inside exactly one wedge
//! stops at the first hit — one division for the gate, and one indexed read.
//! No allocation, and no decode of anything but the gate asked for — the gate
//! comes out of the volume through [`crate::render::moment_value_at`], which
//! indexes into the moment's bytes.
//!
//! That sentence used to read "`MomentData::iter().nth` is
//! `chunks_exact().map()`, and `nth` on either is a pointer add rather than a
//! walk", and it was **false in its second half**: `Map` does not forward
//! `nth`, so the readout decoded every gate up to the one asked for. It is
//! recorded here rather than quietly deleted because the sentence is what made
//! the walk invisible for as long as it lasted — it read like a citation and
//! was a guess.
//!
//! `the_hover_lookup_does_not_walk_the_gates` asserts that shape as a **count**
//! — one gate read per hover, on a 200-gate field, on an 1832-gate one, and
//! reading back out of the volume — because the property is a traversal count
//! and a count is the same integer on every machine under every load, where a
//! ratio of two `Instant`s taken on a contended box is a reading of the rest of
//! the machine. The figures below are a record rather than a bound, for the
//! same reason: an absolute bound tight enough to mean anything fails for
//! reasons that have nothing to do with the code. Measured on a full ring of
//! 720 wedges over 12,800 calls at spread positions with nothing else on the
//! box:
//! **832 ns** at 200 gates, **832 ns** at 1832, and **851 ns** reading out of
//! the volume, `--release`; 3.03 / 3.03 / 5.33 µs unoptimized. Flat in the gate
//! count in both builds, which is the claim, and the volume-backed path costs
//! 2% more than the resident one — the gate decode is not what this costs, the
//! wedge walk is, and both paths walk the same wedges. Against a 16.7 ms frame
//! that is 0.005% of one, and it happens once per frame rather than once per
//! pointer event.
//!
//! A binary search over the wedges is available if a sweep ever arrives with
//! enough radials to matter, but it is not free to have: the wedges are in the
//! render's own radial order, which is the order `write_key` ranks ties in and
//! *not* sorted by azimuth — a sweep crossing north, or one whose antenna
//! wandered, is not monotone. Sorting them would mean carrying a permutation
//! and resolving overlaps against it, for 900 ns.

use crate::render::polar::{GateAt, PolarField, PolarGeometry};
use crate::types::RadarProduct;
use nexrad_model::data::Scan;
use std::sync::Arc;

/// What the readout can be told about a point.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Reading {
    /// A gate was painted there, and this is its value.
    Value(f32),
    /// The render painted nothing there — off the end of a radial, in the sky
    /// of a radial that painted nothing, below threshold, or range-folded.
    /// The picture is blank under the cursor and so is the readout.
    Unpainted,
    /// A gate *was* painted there and nothing is holding its value.
    ///
    /// The state a loop frame of a derived or Level III product is in: the
    /// render's numbers were not kept, and the volume behind it cannot answer
    /// for a field that was computed rather than measured. Distinct from
    /// [`Unpainted`](Self::Unpainted) because the two are different facts and
    /// the readout says so — "no data here" against "this frame's values are
    /// not resident". A reader who cannot tell them apart is being shown a gap
    /// in the weather that is really a gap in the application.
    NotResident,
}

/// The volume behind a picture, and the sweep of it that was drawn.
///
/// Holds an `Arc` on a scan that is resident for other reasons — the loop's
/// download cache keeps every frame's volume for as long as the loop lives — so
/// what a loop frame pays for its readout is one refcount and two `usize`s.
///
/// # Only for what the radar measured
///
/// Six of this display's products are moments the RDA put on the wire, and a
/// sweep can be asked for those directly. The other eleven are *computed* —
/// azimuthal shear, storm-relative velocity, specific differential phase, the
/// volume grids, the hybrid classification, every Level III field — and the
/// volume holds none of them. Asking it for velocity where the picture shows
/// normalized rotation would print a number in the wrong units under a colour
/// scale that means something else, which is worse than declining. So
/// [`Self::new`] answers `None` for those, and the readout says
/// [`Reading::NotResident`] rather than guessing.
#[derive(Clone)]
pub struct SweepGates {
    scan: Arc<Scan>,
    /// Index into `scan.sweeps()`, resolved once by
    /// [`crate::render::sweep_index_for`] — the render's own sweep selection,
    /// not a second one.
    sweep: usize,
    product: RadarProduct,
}

impl SweepGates {
    /// The gates of the sweep `product` at `elevation_deg` was drawn from, or
    /// `None` where this volume cannot answer for that picture.
    ///
    /// The sweep comes from [`crate::render::sweep_index_for`], which is the
    /// renderer's own `find_sweep_owner`, so the radial the geometry's index
    /// names and the radial this reads are the same radial. Nothing here
    /// re-decides which cut was drawn; that is the failure this whole
    /// arrangement is arranged to make impossible.
    pub fn new(scan: Arc<Scan>, product: RadarProduct, elevation_deg: f32) -> Option<Self> {
        if !product.is_wire_moment() {
            return None;
        }
        let sweep = crate::render::sweep_index_for(&scan, product, elevation_deg)?;
        Some(Self {
            scan,
            sweep,
            product,
        })
    }

    /// The value at a gate, decoded on demand.
    ///
    /// Exactly what `render_radar_to_image_full`'s fill would have painted
    /// there, because it is the same function that decides —
    /// [`crate::render::painted_moment_value`]. A second reading of
    /// `MomentValue` here would be the second spelling of the rule this module
    /// exists to have only one of, so the gate arrives *as* a `MomentValue` and
    /// that call is what turns it into a number.
    ///
    /// The gate is reached by [`crate::render::moment_value_at`], which
    /// indexes. This was `moment.iter().nth(at.gate)`, which reads as O(1) and
    /// is not: `Map` does not forward `nth`, so it decoded every gate up to the
    /// one asked for — 6,477 of them across the 64 probes the hover test makes.
    /// See that function's header for the measurement and for the seam it had
    /// to cross to avoid re-spelling anything.
    fn at(&self, at: GateAt) -> Option<f32> {
        let sweep = self.scan.sweeps().get(self.sweep)?;
        let radial = sweep.radials().get(at.radial)?;
        let moment = self.product.get_moment(radial)?;
        let raw = crate::render::moment_value_at(moment, at.gate)?;
        // A range-folded gate paints a colour and carries no number, and its
        // sentinel is a NaN — so this is the same test `PolarField::at` makes,
        // and the two sources answer a folded gate alike.
        crate::render::painted_moment_value(raw).filter(|v| !v.is_nan())
    }
}

/// Where a pane's readout gets its number: the geometry of the picture on the
/// glass, and whatever is holding the values behind it.
///
/// Carried in the slot the `side²` `f32` raster grid used to occupy — one
/// `Arc` per render, threaded through the render channel, the render cache, the
/// suspend copy and the pane — so the shape of the plumbing did not change,
/// only what travels through it.
pub struct HoverSource {
    /// The picture's polar geometry, always — 5.8 KiB for a full ring — and its
    /// values when the render kept them.
    field: PolarField,
    /// The volume behind it, for a frame whose values were not kept.
    sweep: Option<SweepGates>,
}

impl HoverSource {
    /// A source over a render that kept its numbers — a still pane's.
    pub fn resident(field: PolarField) -> Self {
        Self { field, sweep: None }
    }

    /// A source over a render whose numbers were dropped, reading them back out
    /// of the volume it was drawn from — a loop frame's.
    ///
    /// `field` is expected to have been through
    /// [`PolarField::strip_values`](crate::render::polar::PolarField::strip_values);
    /// one that still has its values is not wrong, it just makes `sweep`
    /// unreachable, because resident numbers are always the cheaper answer.
    pub fn from_volume(field: PolarField, sweep: Option<SweepGates>) -> Self {
        Self { field, sweep }
    }

    /// A source over nothing, for a pane with no picture yet.
    pub fn empty() -> Self {
        Self {
            field: PolarField::default(),
            sweep: None,
        }
    }

    /// What was painted at this point.
    ///
    /// `azimuth_deg` and `ground_km` are [`rustdar_geo::site_bearing_range_km`]'s
    /// answer for the position under the pointer — this crate's one spelling of
    /// "where is this point, from the radar".
    pub fn read(&self, azimuth_deg: f64, ground_km: f64) -> Reading {
        let Some(at) = self.field.geometry().pick(azimuth_deg, ground_km) else {
            return Reading::Unpainted;
        };
        if self.field.has_values() {
            return match self.field.at(at) {
                Some(v) => Reading::Value(v),
                None => Reading::Unpainted,
            };
        }
        match self.sweep.as_ref().and_then(|s| s.at(at)) {
            Some(v) => Reading::Value(v),
            // A gate the geometry found, that nothing is holding. If a volume
            // *is* attached it has already been asked, and a `None` from it is
            // a gate below threshold rather than an absent one — but this
            // cannot tell those apart without asking twice, and the honest
            // answer for a frame with no numbers is that they are not here.
            None if self.sweep.is_none() => Reading::NotResident,
            None => Reading::Unpainted,
        }
    }

    /// The picture's geometry, for callers that need to describe it rather than
    /// sample it.
    pub fn geometry(&self) -> &PolarGeometry {
        self.field.geometry()
    }

    /// What holding this costs, bytes — what the render cache bounds itself by.
    ///
    /// The volume a [`SweepGates`] points at is deliberately not counted. It is
    /// resident because the loop's download cache is holding it, and a render
    /// cache that charged itself for a buffer it does not own would evict its
    /// own entries to free memory that would not be freed.
    pub fn resident_bytes(&self) -> usize {
        self.field.resident_bytes()
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod hover_tests;
