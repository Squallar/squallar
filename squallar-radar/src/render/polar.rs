//! What a plan-view render painted, in the polar frame it painted *from*, and
//! the one way to ask it what lies under a point.

use std::sync::atomic::{AtomicU32, Ordering};

/// The sky one radial stood for, as the render painted it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Wedge {
    /// The radial's own azimuth, degrees clockwise from true north.
    pub azimuth_deg: f32,
    /// Half the width it was painted at, degrees.
    pub half_width_deg: f32,
}

impl Wedge {
    /// The wedge of a radial that never reached
    /// [`super::MercatorProjection::render_gate`].
    pub const UNPAINTED: Self = Self {
        azimuth_deg: f32::NAN,
        half_width_deg: f32::NAN,
    };

    /// Whether `azimuth_deg` is inside the sky this radial was painted over.
    fn contains(&self, azimuth_deg: f64) -> bool {
        let half = f64::from(self.half_width_deg);
        if !half.is_finite() || !self.azimuth_deg.is_finite() {
            return false;
        }
        let delta = wrap_deg(azimuth_deg - f64::from(self.azimuth_deg));
        delta >= -half && delta < half
    }
}

/// `a` folded onto `(-180, 180]`, so a wedge spanning north is one interval and
/// not two.
fn wrap_deg(a: f64) -> f64 {
    let mut a = a % 360.0;
    if a > 180.0 {
        a -= 360.0;
    } else if a <= -180.0 {
        a += 360.0;
    }
    a
}

/// The azimuth span one radial is **drawn** over, degrees.
///
/// Anchored on the radial's own azimuth and deliberately **not** folded onto
/// any range: a wedge spanning north is one interval whose `lo` is negative,
/// not two intervals with a seam at 0. The fan geometry and the pick both read
/// this, so folding it here would put a seam in the picture.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DrawnEdge {
    /// The clockwise-most edge, degrees. `lo == hi` means the radial draws
    /// nothing — an unpainted radial, or one a neighbour trimmed to zero.
    pub lo_deg: f32,
    /// The counter-clockwise-most edge, degrees.
    pub hi_deg: f32,
}

impl DrawnEdge {
    /// A radial that draws nothing.
    pub const EMPTY: Self = Self {
        lo_deg: 0.0,
        hi_deg: 0.0,
    };

    /// Whether this span is empty.
    ///
    /// Spelled through `partial_cmp` because the answer for a NaN edge has to
    /// be "empty" and not "wide": a negated `>` says that correctly and reads
    /// as a typo, and `<=` would say the opposite for NaN.
    pub fn is_empty(&self) -> bool {
        !matches!(
            self.hi_deg.partial_cmp(&self.lo_deg),
            Some(std::cmp::Ordering::Greater)
        )
    }

    /// Whether `azimuth_deg` falls in this span, on the same half-open
    /// convention [`Wedge::contains`] uses so abutting spans tile without
    /// double-claiming their shared edge.
    ///
    /// Measured from `lo` rather than from a midpoint, and widened to `f64`
    /// **before** any arithmetic. A midpoint form computes `lo + hi` and
    /// `hi - lo`; done in `f32` those round in opposite directions and two
    /// spans that abut exactly both claim their shared edge, which is the one
    /// thing this function exists to prevent. Found by
    /// `the_drawn_sweep_claims_every_azimuth_at_most_once` on a ragged sweep.
    fn contains(&self, azimuth_deg: f64) -> bool {
        if self.is_empty() {
            return false;
        }
        let lo = f64::from(self.lo_deg);
        let width = f64::from(self.hi_deg) - lo;
        (azimuth_deg - lo).rem_euclid(360.0) < width
    }
}

/// **Each radial's drawn azimuth span, with every overlap removed.**
///
/// `docs/radar-polar-design.md` §2.2. [`super::l2_wedge_half_widths_deg`]
/// takes the declared azimuth spacing as a *floor* and widens each radial
/// toward its neighbours, so where the real gap is tighter than the
/// declaration **wedges overlap**. The raster resolves that by ordering:
/// [`PolarGeometry::pick`] scans in reverse and the greatest radial index
/// wins, which is `write_key`'s own ordering.
///
/// Geometry cannot express "greatest wins" without a depth buffer, and egui's
/// pass has none. So this removes the overlap instead of ordering it: where
/// two radials would both claim a point, each is trimmed to the **midpoint of
/// their two azimuths** — the boundary that gives every point to the radial
/// whose beam is nearer it.
///
/// **For non-overlapping input this is the identity**, so the answer is
/// unchanged wherever the raster was unambiguous; it differs only inside the
/// slivers where two radials both claimed a point. Unpainted radials
/// ([`Wedge::UNPAINTED`]) yield [`DrawnEdge::EMPTY`] and are excluded from
/// their neighbours' boundaries entirely, so a sweep with gaps does not have
/// its live radials stretched across them.
pub fn draw_edges(wedges: &[Wedge]) -> Vec<DrawnEdge> {
    let mut edges = vec![DrawnEdge::EMPTY; wedges.len()];

    // Only radials that were actually painted take part. A NaN azimuth has no
    // position to trim against and no sky to claim.
    let mut live: Vec<usize> = (0..wedges.len())
        .filter(|&i| {
            wedges[i].azimuth_deg.is_finite()
                && wedges[i].half_width_deg.is_finite()
                && wedges[i].half_width_deg > 0.0
        })
        .collect();
    if live.is_empty() {
        return edges;
    }
    if live.len() == 1 {
        let w = wedges[live[0]];
        // No neighbour to trim against. The only bound is the circle itself:
        // a half-width past 180 degrees would wrap onto its own far edge.
        let half = f64::from(w.half_width_deg).min(180.0);
        let az = f64::from(w.azimuth_deg);
        edges[live[0]] = DrawnEdge {
            lo_deg: (az - half) as f32,
            hi_deg: (az + half) as f32,
        };
        return edges;
    }

    live.sort_by(|&a, &b| {
        let key = |i: usize| f64::from(wedges[i].azimuth_deg).rem_euclid(360.0);
        key(a)
            .partial_cmp(&key(b))
            .expect("azimuths are finite here")
    });

    // The forward gap from each live radial to the next one round the circle.
    // Cyclic, so the last radial's neighbour is the first.
    for (slot, &i) in live.iter().enumerate() {
        let next = live[(slot + 1) % live.len()];
        let prev = live[(slot + live.len() - 1) % live.len()];
        let az = f64::from(wedges[i].azimuth_deg);
        let half = f64::from(wedges[i].half_width_deg);

        // Forward and backward gaps, each in [0, 360). With two radials the
        // two gaps sum to 360 and each side is bisected independently, which
        // is what keeps the pair from overlapping on the far side too.
        let gap_fwd = (f64::from(wedges[next].azimuth_deg) - az).rem_euclid(360.0);
        let gap_back = (az - f64::from(wedges[prev].azimuth_deg)).rem_euclid(360.0);

        // Trim to the nearer of the wedge's own edge and the bisector. A gap
        // wider than the wedge leaves the wedge untouched, which is the
        // identity the doc promises for non-overlapping input.
        let hi = half.min(gap_fwd / 2.0);
        let lo = half.min(gap_back / 2.0);
        edges[i] = DrawnEdge {
            lo_deg: (az - lo) as f32,
            hi_deg: (az + hi) as f32,
        };
    }
    edges
}

/// One gate of one radial, in the order the render walked them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GateAt {
    /// Index into [`PolarGeometry::wedges`].
    pub radial: usize,
    /// Index along that radial, from gate 0 at
    /// [`PolarGeometry::first_gate_km`].
    pub gate: usize,
}

#[cfg(test)]
thread_local! {
    /// Gate values read out of a picture on this thread since
    /// [`take_gate_reads`] last took the tally.
    static GATE_READS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Note the gates an access actually read.
#[cfg(test)]
pub(crate) fn note_gate_reads(n: u64) {
    GATE_READS.with(|reads| reads.set(reads.get() + n));
}

/// The gate reads since this was last called, and the tally back to zero.
#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) fn take_gate_reads() -> u64 {
    GATE_READS.with(|reads| reads.replace(0))
}

/// Where a render's gates are — everything needed to turn a point into a
/// `(radial, gate)`, and nothing else.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PolarGeometry {
    wedges: Vec<Wedge>,
    /// Gate 0's centre **along the beam**, km.
    first_gate_slant_km: f64,
    /// One gate's depth **along the beam**, km.
    gate_interval_slant_km: f64,
    /// The elevation the sweep was flown at, degrees, or `None` where the two
    /// ranges above are **already ground ranges** and must not be converted at
    /// all.
    elevation_deg: Option<f64>,
    gates: usize,
    reach_gates: usize,
}

impl PolarGeometry {
    /// The gate `render_gate` painted the point at (`azimuth_deg`,
    /// `ground_km`) from, or `None` where it painted no gate there.
    pub fn pick(&self, azimuth_deg: f64, ground_km: f64) -> Option<GateAt> {
        let gate = self.gate_at(ground_km)?;
        // Radial-major, greatest wins — `write_key`'s ordering.
        let radial = (0..self.wedges.len())
            .rev()
            .find(|&i| self.wedges[i].contains(azimuth_deg))?;
        Some(GateAt { radial, gate })
    }

    /// The gate under a point, resolved against the **drawn** edges rather
    /// than the painted wedges.
    ///
    /// [`Self::pick`]'s answer for the raster; this one for the fan. They
    /// agree everywhere the wedges do not overlap, and inside an overlap this
    /// gives the point to the nearer beam where `pick` gives it to the greater
    /// radial index. `edges` is [`draw_edges`] over this geometry's own
    /// wedges, passed in rather than recomputed because it is derived once at
    /// upload and read on every hover — building it per call would put a sort
    /// and an allocation on the frame thread.
    ///
    /// **Nothing calls this yet, and that is deliberate.** The readout must
    /// agree with the picture, and the picture is still the raster, so hover
    /// keeps using [`Self::pick`] until the fan is what draws. Switching the
    /// readout first would make it disagree with every pixel on screen.
    pub fn pick_drawn(
        &self,
        edges: &[DrawnEdge],
        azimuth_deg: f64,
        ground_km: f64,
    ) -> Option<GateAt> {
        let gate = self.gate_at(ground_km)?;
        let radial =
            (0..self.wedges.len().min(edges.len())).find(|&i| edges[i].contains(azimuth_deg))?;
        Some(GateAt { radial, gate })
    }

    /// The gate whose footprint holds `ground_km`, or `None` past either end of
    /// a radial.
    fn gate_at(&self, ground_km: f64) -> Option<usize> {
        if self.gate_interval_slant_km <= 0.0 || self.reach_gates == 0 {
            return None;
        }
        let along_beam_km = match self.elevation_deg {
            Some(e) => crate::beam::slant_range_for_ground_km(ground_km, e),
            None => ground_km,
        };
        let g = ((along_beam_km - self.first_gate_slant_km) / self.gate_interval_slant_km + 0.5)
            .floor();
        (g >= 0.0 && g < self.reach_gates as f64).then_some(g as usize)
    }

    /// The wedge each radial was painted over, in the render's radial order.
    pub fn wedges(&self) -> &[Wedge] {
        &self.wedges
    }

    /// How many radials the render walked.
    pub fn radials(&self) -> usize {
        self.wedges.len()
    }

    /// How many gates each radial's row holds — the stride.
    pub fn gates(&self) -> usize {
        self.gates
    }

    /// How many of them the render actually reached, which is the bound
    /// [`Self::pick`] answers within.
    pub fn reach_gates(&self) -> usize {
        self.reach_gates
    }

    /// Gate 0's centre **along the beam**, km.
    pub fn first_gate_slant_km(&self) -> f64 {
        self.first_gate_slant_km
    }

    /// One gate's depth **along the beam**, km.
    pub fn gate_interval_slant_km(&self) -> f64 {
        self.gate_interval_slant_km
    }

    /// The elevation the sweep was flown at, degrees.
    pub fn elevation_deg(&self) -> Option<f64> {
        self.elevation_deg
    }

    /// The ground range of gate `gate`'s centre, km — the projection of
    /// [`Self::first_gate_slant_km`] + `gate` × [`Self::gate_interval_slant_km`].
    pub fn gate_ground_km(&self, gate: usize) -> f64 {
        let along_beam_km = self.first_gate_slant_km + gate as f64 * self.gate_interval_slant_km;
        match self.elevation_deg {
            Some(e) => crate::beam::ground_range_km(along_beam_km, e),
            None => along_beam_km,
        }
    }

    /// Whether this describes no gates at all, which is what a render that
    /// painted nothing produces.
    pub fn is_empty(&self) -> bool {
        self.wedges.is_empty() || self.reach_gates == 0
    }

    /// What holding this costs, bytes.
    pub fn resident_bytes(&self) -> usize {
        self.wedges.len() * std::mem::size_of::<Wedge>()
    }

    /// Build one directly, for callers that hold a polar layout already and are
    /// not going through a render.
    pub fn from_parts(
        wedges: Vec<Wedge>,
        first_gate_slant_km: f64,
        gate_interval_slant_km: f64,
        elevation_deg: Option<f64>,
        gates: usize,
    ) -> Self {
        Self {
            wedges,
            first_gate_slant_km,
            gate_interval_slant_km,
            elevation_deg,
            gates,
            reach_gates: gates,
        }
    }

    /// [`Self::from_parts`]'s reach, **measured** rather than assumed whole.
    ///
    /// `from_parts` answers `reach_gates == gates` because a caller holding a
    /// layout and no data has nothing else to say. A caller that walked the
    /// gates has: `render_sweep_plane` counts the painted ones, the same
    /// quantity `PolarBuffers::into_field` reads off the raster it just wrote,
    /// and the two must mean the same thing or a fan and a raster of one sweep
    /// draw to two different radii.
    ///
    /// Clamped to the stride, because a reach past it is a caller that
    /// miscounted and `gate_at` would answer gates no row holds.
    pub fn reaching(mut self, reach_gates: usize) -> Self {
        self.reach_gates = reach_gates.min(self.gates);
        self
    }
}

/// The numbers behind a picture, in either of the two forms that answer
/// [`PolarField::at`] with the identical bits.
///
/// `docs/radar-polar-design.md` §6.3 — *"the resident term becomes codes
/// rather than f32"* — for the one place the numbers are actually held. The
/// design states it of the polar representation as a whole; this is the half
/// that needs no renderer and no wire, because the codes are derived from the
/// values the render already painted and are widened again before they are
/// written.
#[derive(Clone, Debug)]
enum Values {
    /// One `f32` a gate, as the render painted it.
    Wide(Vec<f32>),
    /// One byte a gate, indexing the distinct numbers the render actually
    /// painted.
    ///
    /// **Exact, and not a quantisation.** `table` holds the painted numbers
    /// themselves rather than the ends of a range, so a code names one of them
    /// and reading it back is an indexing rather than a rounding. What bounds
    /// this form is the *count* of distinct numbers, and a plane with more of
    /// them than a byte can address stays [`Values::Wide`] instead of losing
    /// some.
    Coded { codes: Vec<u8>, table: Vec<f32> },
}

impl Default for Values {
    fn default() -> Self {
        Self::Wide(Vec::new())
    }
}

/// Elementwise over the numbers, on `f32`'s own comparison, so the two forms
/// answer this the way one `Vec<f32>` did before either existed: a coded plane
/// equals the wide plane it was built from, and a plane holding a NaN still
/// equals nothing, itself included.
impl PartialEq for Values {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len() && self.iter().zip(other.iter()).all(|(a, b)| a == b)
    }
}

impl Values {
    /// The most distinct numbers a coded plane can name — one byte's worth.
    const MAX_CODES: usize = u8::MAX as usize + 1;

    fn len(&self) -> usize {
        match self {
            Self::Wide(values) => values.len(),
            Self::Coded { codes, .. } => codes.len(),
        }
    }

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The number at `index`, or `None` past the end.
    fn get(&self, index: usize) -> Option<f32> {
        match self {
            Self::Wide(values) => values.get(index).copied(),
            Self::Coded { codes, table } => table.get(usize::from(*codes.get(index)?)).copied(),
        }
    }

    /// Every number in order, widened.
    ///
    /// Indexed rather than filtered so the count it yields is exactly
    /// [`Values::len`]: [`PolarField::to_bytes`] writes that count into the
    /// header and then writes this, and an iterator that could quietly yield
    /// fewer would write a header describing bytes it did not write.
    fn iter(&self) -> impl Iterator<Item = f32> + '_ {
        (0..self.len()).map(|i| match self {
            Self::Wide(values) => values[i],
            Self::Coded { codes, table } => table[usize::from(codes[i])],
        })
    }

    /// What holding this costs, bytes.
    fn resident_bytes(&self) -> usize {
        match self {
            Self::Wide(values) => values.len() * size_of::<f32>(),
            Self::Coded { codes, table } => codes.len() + table.len() * size_of::<f32>(),
        }
    }

    /// Re-hold the numbers as codes, or leave them wide where there are more
    /// distinct ones than a byte can name.
    ///
    /// One walk, and it stops at the first number past the ceiling: a plane
    /// whose gates are a computed gradient rather than a wire byte disagrees
    /// within its first few radials, so the walk it does not finish is the
    /// small one.
    fn compact(&mut self) {
        let Self::Wide(values) = self else {
            return;
        };
        if values.is_empty() {
            return;
        }
        let mut assigned = CodeTable::new();
        let mut codes = Vec::with_capacity(values.len());
        for value in values.iter() {
            let Some(code) = assigned.code_for(value.to_bits()) else {
                return;
            };
            codes.push(code);
        }
        *self = Self::Coded {
            codes,
            table: assigned.table,
        };
    }
}

/// A code per distinct bit pattern, assigned in the order the patterns are
/// first seen.
///
/// **Keyed on the bits and not on the number**, because two of the things a
/// render paints are NaNs that mean different things — the unpainted marker
/// and [`super::RANGE_FOLDED_SENTINEL`] — and `f32` equality neither
/// distinguishes those nor separates `-0.0` from `0.0`. The wire has to come
/// back byte for byte, so what is deduplicated is the byte pattern.
///
/// Open-addressed over twice [`Values::MAX_CODES`] slots, so the table is at
/// most half full and a probe always reaches an empty one. Sized to fit a
/// cache: the alternative, a hash map, is a walk over every gate of every
/// still render and this is the same walk without the per-gate hashing cost.
struct CodeTable {
    keys: [u32; Self::SLOTS],
    codes: [u8; Self::SLOTS],
    used: [bool; Self::SLOTS],
    /// The distinct numbers, indexed by the code assigned to each.
    table: Vec<f32>,
}

impl CodeTable {
    const SLOTS: usize = 2 * Values::MAX_CODES;

    fn new() -> Self {
        Self {
            keys: [0; Self::SLOTS],
            codes: [0; Self::SLOTS],
            used: [false; Self::SLOTS],
            table: Vec::new(),
        }
    }

    /// The code for `bits`, assigning a fresh one where this pattern is new,
    /// or `None` at the pattern one past what a byte can address.
    fn code_for(&mut self, bits: u32) -> Option<u8> {
        // Fibonacci hashing: the bits of a float differ in their low end
        // across adjacent gates and in their high end across products, and a
        // multiply mixes both ends into the slot index.
        let mut slot = (bits.wrapping_mul(0x9E37_79B9) >> 23) as usize & (Self::SLOTS - 1);
        loop {
            if !self.used[slot] {
                let code = u8::try_from(self.table.len()).ok()?;
                self.used[slot] = true;
                self.keys[slot] = bits;
                self.codes[slot] = code;
                self.table.push(f32::from_bits(bits));
                return Some(code);
            }
            if self.keys[slot] == bits {
                return Some(self.codes[slot]);
            }
            slot = (slot + 1) & (Self::SLOTS - 1);
        }
    }
}

/// A render's geometry with the numbers it painted, row-major `radials ×
/// gates`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PolarField {
    geometry: PolarGeometry,
    values: Values,
}

impl PolarField {
    /// The geometry alone — the half a loop frame keeps.
    pub fn geometry(&self) -> &PolarGeometry {
        &self.geometry
    }

    /// Give up the numbers and keep the geometry, for a render whose caller
    /// asked for the picture and not the values.
    pub fn strip_values(&mut self) {
        self.values = Values::default();
    }

    /// **Re-hold the numbers as one byte a gate plus a table of the distinct
    /// ones**, for a render whose caller *did* ask for them.
    ///
    /// [`Self::strip_values`]'s counterpart: that one is what a loop frame
    /// does with numbers nobody will read, this is what a still pane does with
    /// numbers a hover will. A surveillance cut's plane is 720 × 1832 gates,
    /// so the fall is `radials × gates × 4` to `radials × gates + table` —
    /// 5,276,160 B to 1,320,064 B at the widest table this form admits, which
    /// is `compacting_falls_the_price_to_a_byte_a_gate_and_the_table` computed
    /// off the shape rather than a figure recorded here.
    ///
    /// **It cannot cost fidelity, by construction.** The table holds the
    /// painted numbers themselves, so [`Self::at`] returns the bit pattern it
    /// was given, and a plane carrying more distinct patterns than a byte can
    /// address is left wide rather than rounded — which is what a computed
    /// field (rotation, storm-relative velocity, a rate) does, and what a wire
    /// byte's 256 reachable values never do. The refusal is measured off the
    /// plane in hand and not read out of a table of products, so a product
    /// that stops being exact stops being compacted on the same day.
    ///
    /// **And it moves the wire only where the wire says so.**
    /// [`Self::to_bytes`] writes the same `f32`s from either form — the
    /// layout this protocol pins is one layout and stays one —, which
    /// `the_wire_is_the_same_bytes_from_either_form` asserts against that
    /// pinned fixture rather than against a second opinion.
    /// [`Self::to_tail`] is the encoder the reply uses, and it writes a coded
    /// plane coded behind a byte that says so, so a page across a worker port
    /// receives and holds what was compacted here instead of a widening of
    /// it.
    ///
    /// One walk of the plane, on whichever thread the render finished on;
    /// never on the frame thread, and never on a plane that is about to be
    /// stripped.
    pub fn compact_values(&mut self) {
        self.values.compact();
    }

    /// Whether the numbers are resident.
    pub fn has_values(&self) -> bool {
        !self.values.is_empty()
    }

    /// The value at a gate this render walked, or `None` where it painted
    /// nothing there.
    pub fn at(&self, at: GateAt) -> Option<f32> {
        if at.gate >= self.geometry.gates {
            return None;
        }
        let index = at.radial * self.geometry.gates + at.gate;
        let v = self.values.get(index)?;
        #[cfg(test)]
        note_gate_reads(1);
        (!v.is_nan()).then_some(v)
    }

    /// What holding this costs, bytes — what the render cache bounds itself by.
    pub fn resident_bytes(&self) -> usize {
        self.geometry.resident_bytes() + self.values.resident_bytes()
    }

    /// Build one directly, for tests and for callers holding a polar grid.
    pub fn from_parts(geometry: PolarGeometry, values: Vec<f32>) -> Self {
        debug_assert!(
            values.is_empty() || values.len() == geometry.radials() * geometry.gates(),
            "a polar field is exactly radials × gates, or nothing"
        );
        Self {
            geometry,
            values: Values::Wide(values),
        }
    }
}

/// **Which of [`PolarField`]'s two forms a polar tail carries**, as the byte
/// that leads it.
///
/// The two payloads are not distinguishable from their own bytes and were
/// never meant to be: both open on a radial count, both are a header then
/// wedges then a block sized by the same `n_values`, and a reader handed one
/// with no way to ask which it holds would read a table of codes as `f32`s
/// and paint numbers nobody measured. A second form could not join a
/// versionless encoding without that ambiguity, which is why one did not
/// until this byte existed.
///
/// **The byte leads the tail rather than joining either payload**, so the
/// wide payload's layout is exactly the layout it was before a second form
/// existed — the same bytes, the same length, the same digest, and
/// `the_polar_wire_layout_is_the_one_this_protocol_ships` still pins them
/// unedited. What that payload gained is a name: it is *form 0*.
///
/// **What keeps a mispaired peer away from this byte is not this byte.** The
/// page and the worker refuse each other's token at the HELLO handshake, and
/// three of `wire_identity::WIRE_FRAME_REPLY_ROWS` are polar tails, so the
/// local token moves when either form's layout does. The form byte is what
/// keeps a *matched* pair from guessing between two payloads; it is not a
/// version negotiation and does not claim to be one. A payload written
/// before it existed leads with a radial count's low byte and would be read
/// as a form or refused as one; nothing here says which, because the
/// handshake is what says such a pair never attaches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PolarWireForm {
    /// One `f32` a gate — the layout this protocol has always shipped.
    Wide,
    /// A table of the distinct numbers, then one byte a gate indexing it.
    Coded,
}

impl PolarWireForm {
    /// This form as the byte that leads a tail.
    pub fn wire_code(self) -> u8 {
        match self {
            Self::Wide => 0,
            Self::Coded => 1,
        }
    }

    /// The inverse of [`wire_code`](Self::wire_code), or `None` for a form
    /// this build does not write.
    pub fn from_wire_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::Wide),
            1 => Some(Self::Coded),
            _ => None,
        }
    }
}

impl PolarField {
    /// The header this field's byte form opens with: three counts and two
    /// ranges.
    const HEADER: usize = 4 * 4 + 8 * 3;

    /// The header and wedges — the prefix **both** forms open with, written
    /// once so the two cannot come to describe the same geometry
    /// differently.
    fn write_geometry(&self, out: &mut Vec<u8>) {
        let g = &self.geometry;
        out.extend_from_slice(&(g.wedges.len() as u32).to_le_bytes());
        out.extend_from_slice(&(g.gates as u32).to_le_bytes());
        out.extend_from_slice(&(g.reach_gates as u32).to_le_bytes());
        out.extend_from_slice(&(self.values.len() as u32).to_le_bytes());
        out.extend_from_slice(&g.first_gate_slant_km.to_le_bytes());
        out.extend_from_slice(&g.gate_interval_slant_km.to_le_bytes());
        // NaN is the wire spelling of `None` — see `PolarGeometry::elevation_deg`.
        out.extend_from_slice(&g.elevation_deg.unwrap_or(f64::NAN).to_le_bytes());
        for w in &g.wedges {
            out.extend_from_slice(&w.azimuth_deg.to_le_bytes());
            out.extend_from_slice(&w.half_width_deg.to_le_bytes());
        }
    }

    /// This field as bytes, little-endian, for the one boundary that can only
    /// carry buffers.
    ///
    /// **Always the wide form**, whichever form the field is in: the numbers
    /// are widened here rather than held wide, so this is the layout it has
    /// always been. [`Self::to_tail`] is what writes a coded plane coded.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(
            Self::HEADER + self.geometry.wedges.len() * 8 + self.values.len() * 4,
        );
        self.write_geometry(&mut out);
        for v in self.values.iter() {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out
    }

    /// Which form [`Self::to_tail`] writes this field in.
    pub fn wire_form(&self) -> PolarWireForm {
        match self.values {
            Values::Wide(_) => PolarWireForm::Wide,
            Values::Coded { .. } => PolarWireForm::Coded,
        }
    }

    /// **This field as a reply tail: the form byte, then that form's
    /// payload.**
    ///
    /// The one encoder the frame reply uses, and the reason a page across a
    /// worker port can hold a still pane's numbers a byte a gate rather than
    /// four. A coded plane is written coded — `1 + header + wedges + 4 +
    /// table + one byte a gate` against `1 + header + wedges + four bytes a
    /// gate` — so the compaction the worker already paid for in
    /// [`Self::compact_values`] is delivered across the port instead of being
    /// widened at the door.
    ///
    /// The tag and the payload are written in the **same match arm** on the
    /// same value, so there is no arrangement of this function in which a
    /// tail says one form and carries the other.
    pub fn to_tail(&self) -> Vec<u8> {
        let wedge_bytes = self.geometry.wedges.len() * 8;
        match &self.values {
            Values::Wide(_) => {
                let mut out =
                    Vec::with_capacity(1 + Self::HEADER + wedge_bytes + self.values.len() * 4);
                out.push(PolarWireForm::Wide.wire_code());
                self.write_geometry(&mut out);
                for v in self.values.iter() {
                    out.extend_from_slice(&v.to_le_bytes());
                }
                out
            }
            Values::Coded { codes, table } => {
                let mut out = Vec::with_capacity(
                    1 + Self::HEADER + wedge_bytes + 4 + table.len() * 4 + codes.len(),
                );
                out.push(PolarWireForm::Coded.wire_code());
                self.write_geometry(&mut out);
                out.extend_from_slice(&(table.len() as u32).to_le_bytes());
                for v in table {
                    out.extend_from_slice(&v.to_le_bytes());
                }
                // Codes are bytes, so they have no order to write them in.
                out.extend_from_slice(codes);
                out
            }
        }
    }

    /// The inverse of [`Self::to_tail`], or `None` for anything this build
    /// did not write — a form byte it does not know included.
    ///
    /// The form is read before a single byte of the payload is interpreted,
    /// so the two layouts are never guessed between.
    pub fn from_tail(bytes: &[u8]) -> Option<Self> {
        let (&form, payload) = bytes.split_first()?;
        match PolarWireForm::from_wire_code(form)? {
            PolarWireForm::Wide => Self::from_bytes(payload),
            PolarWireForm::Coded => Self::from_coded_bytes(payload),
        }
    }

    /// The header and wedges both forms open with: the geometry, the count
    /// the values block is shaped by, and where that block starts.
    ///
    /// The buffer is checked to hold the wedges **before** they are
    /// allocated, so a header declaring four billion radials costs a length
    /// comparison rather than a reservation.
    fn read_geometry(bytes: &[u8]) -> Option<(PolarGeometry, usize, usize)> {
        if bytes.len() < Self::HEADER {
            return None;
        }
        let u32_at = |i: usize| -> usize {
            u32::from_le_bytes(bytes[i..i + 4].try_into().expect("bounds checked")) as usize
        };
        let f64_at = |i: usize| -> f64 {
            f64::from_le_bytes(bytes[i..i + 8].try_into().expect("bounds checked"))
        };
        let radials = u32_at(0);
        let gates = u32_at(4);
        let reach_gates = u32_at(8);
        let n_values = u32_at(12);
        let first_gate_slant_km = f64_at(16);
        let gate_interval_slant_km = f64_at(24);
        let elevation_deg = Some(f64_at(32)).filter(|e| !e.is_nan());

        let values_at = Self::HEADER.checked_add(radials.checked_mul(8)?)?;
        if bytes.len() < values_at {
            return None;
        }
        // A values buffer that is neither empty nor exactly the shape means the
        // two halves disagree about the picture.
        if n_values != 0 && n_values != radials.checked_mul(gates)? {
            return None;
        }

        let mut at = Self::HEADER;
        let mut wedges = Vec::with_capacity(radials);
        for _ in 0..radials {
            let f = |i: usize| f32::from_le_bytes(bytes[i..i + 4].try_into().expect("checked"));
            wedges.push(Wedge {
                azimuth_deg: f(at),
                half_width_deg: f(at + 4),
            });
            at += 8;
        }
        Some((
            PolarGeometry {
                wedges,
                first_gate_slant_km,
                gate_interval_slant_km,
                elevation_deg,
                gates,
                reach_gates,
            },
            n_values,
            values_at,
        ))
    }

    /// The inverse of [`Self::to_bytes`], or `None` for anything this build did
    /// not write.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let (geometry, n_values, mut at) = Self::read_geometry(bytes)?;
        if bytes.len() != at.checked_add(n_values.checked_mul(4)?)? {
            return None;
        }
        let mut values = Vec::with_capacity(n_values);
        for _ in 0..n_values {
            values.push(f32::from_le_bytes(
                bytes[at..at + 4].try_into().expect("checked"),
            ));
            at += 4;
        }
        Some(Self {
            geometry,
            values: Values::Wide(values),
        })
    }

    /// The coded payload — form 1's body — decoded into the form it was
    /// written in, so a page holds one byte a gate and never materializes the
    /// wide plane at all.
    ///
    /// Every code is checked to name a number the table holds. `Values`
    /// indexes that table directly rather than through `get`, and a code past
    /// its end is a message this build did not write; refusing it here is
    /// what keeps a truncated or doctored tail from being an index out of
    /// bounds later, on the thread that reads a hover.
    fn from_coded_bytes(bytes: &[u8]) -> Option<Self> {
        let (geometry, n_values, at) = Self::read_geometry(bytes)?;
        if bytes.len() < at.checked_add(4)? {
            return None;
        }
        let table_len =
            u32::from_le_bytes(bytes[at..at + 4].try_into().expect("bounds checked")) as usize;
        // A table longer than a byte addresses names codes that cannot exist.
        if table_len > Values::MAX_CODES {
            return None;
        }
        let table_at = at + 4;
        let codes_at = table_at.checked_add(table_len.checked_mul(4)?)?;
        if bytes.len() != codes_at.checked_add(n_values)? {
            return None;
        }
        let table: Vec<f32> = bytes[table_at..codes_at]
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes(b.try_into().expect("chunks of four")))
            .collect();
        let codes = &bytes[codes_at..];
        if codes.iter().any(|&code| usize::from(code) >= table_len) {
            return None;
        }
        Some(Self {
            geometry,
            values: Values::Coded {
                codes: codes.to_vec(),
                table,
            },
        })
    }
}

/// The shape a render declares its polar source to have, so the buffer that
/// records it can be sized before the first gate is painted.
#[derive(Clone, Copy, Debug)]
pub(super) struct PolarShape {
    /// How many radials (or grid rows) the fill will walk.
    pub radials: usize,
    /// The most gates any one of them carries.
    pub gates: usize,
    /// Gate 0's centre **along the beam**, km.
    pub first_gate_slant_km: f64,
    /// One gate's depth **along the beam**, km.
    pub gate_interval_slant_km: f64,
    /// The elevation the sweep was flown at, degrees, or `None` for a path whose ranges
    /// are ground ranges already.
    pub elevation_deg: Option<f64>,
}

/// The field under construction, written by
/// [`super::MercatorProjection::render_gate`] as it paints.
pub(super) struct PolarBuffers {
    values: Vec<AtomicU32>,
    azimuth: Vec<AtomicU32>,
    half_width: Vec<AtomicU32>,
    gates: usize,
    first_gate_slant_km: f64,
    gate_interval_slant_km: f64,
    elevation_deg: Option<f64>,
}

/// The bits [`PolarBuffers`] leaves where nothing was painted — `f32::NAN`.
const UNPAINTED_BITS: u32 = 0x7FC0_0000;

impl PolarBuffers {
    /// A field of `shape`, every gate unpainted and every wedge unrecorded.
    pub(super) fn new(shape: PolarShape) -> Self {
        let cells = shape.radials.saturating_mul(shape.gates);
        Self {
            values: (0..cells).map(|_| AtomicU32::new(UNPAINTED_BITS)).collect(),
            azimuth: (0..shape.radials)
                .map(|_| AtomicU32::new(UNPAINTED_BITS))
                .collect(),
            half_width: (0..shape.radials)
                .map(|_| AtomicU32::new(UNPAINTED_BITS))
                .collect(),
            gates: shape.gates,
            first_gate_slant_km: shape.first_gate_slant_km,
            elevation_deg: shape.elevation_deg,
            gate_interval_slant_km: shape.gate_interval_slant_km,
        }
    }

    /// Record one gate as `render_gate` paints it.
    #[inline]
    pub(super) fn paint(
        &self,
        at: super::GateId,
        azimuth_deg: f64,
        half_width_deg: f64,
        value: f32,
    ) {
        debug_assert!(
            at.radial < self.azimuth.len() && at.gate < self.gates,
            "gate ({}, {}) is outside the declared polar shape ({} radials × {} gates)",
            at.radial,
            at.gate,
            self.azimuth.len(),
            self.gates
        );
        if let Some(slot) = self.azimuth.get(at.radial) {
            slot.store((azimuth_deg as f32).to_bits(), Ordering::Relaxed);
        }
        if let Some(slot) = self.half_width.get(at.radial) {
            slot.store((half_width_deg as f32).to_bits(), Ordering::Relaxed);
        }
        if at.gate < self.gates
            && let Some(slot) = self.values.get(at.radial * self.gates + at.gate)
        {
            slot.store(value.to_bits(), Ordering::Relaxed);
        }
    }

    /// The finished field.
    pub(super) fn into_field(mut self) -> PolarField {
        let wedges = self
            .azimuth
            .iter_mut()
            .zip(self.half_width.iter_mut())
            .map(|(a, h)| Wedge {
                azimuth_deg: f32::from_bits(*a.get_mut()),
                half_width_deg: f32::from_bits(*h.get_mut()),
            })
            .collect();
        // Out of what was painted rather than what was declared.
        let mut reach_gates = 0usize;
        let gates = self.gates;
        let values: Vec<f32> = self
            .values
            .iter_mut()
            .enumerate()
            .map(|(i, v)| {
                let bits = *v.get_mut();
                let v = f32::from_bits(bits);
                // On the bits and not on `is_nan`: a range-folded gate is
                // painted and its sentinel is a NaN.
                if bits != UNPAINTED_BITS && gates > 0 {
                    reach_gates = reach_gates.max(i % gates + 1);
                }
                v
            })
            .collect();
        PolarField {
            geometry: PolarGeometry {
                wedges,
                first_gate_slant_km: self.first_gate_slant_km,
                gate_interval_slant_km: self.gate_interval_slant_km,
                elevation_deg: self.elevation_deg,
                gates,
                reach_gates,
            },
            values: Values::Wide(values),
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
