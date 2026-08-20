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
}

/// A render's geometry with the numbers it painted, row-major `radials ×
/// gates`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PolarField {
    geometry: PolarGeometry,
    values: Vec<f32>,
}

impl PolarField {
    /// The geometry alone — the half a loop frame keeps.
    pub fn geometry(&self) -> &PolarGeometry {
        &self.geometry
    }

    /// Give up the numbers and keep the geometry, for a render whose caller
    /// asked for the picture and not the values.
    pub fn strip_values(&mut self) {
        self.values = Vec::new();
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
        let taken = self.values.get(index..=index)?;
        #[cfg(test)]
        note_gate_reads(taken.len() as u64);
        let v = *taken.last()?;
        (!v.is_nan()).then_some(v)
    }

    /// What holding this costs, bytes — what the render cache bounds itself by.
    pub fn resident_bytes(&self) -> usize {
        self.geometry.resident_bytes() + self.values.len() * std::mem::size_of::<f32>()
    }

    /// Build one directly, for tests and for callers holding a polar grid.
    pub fn from_parts(geometry: PolarGeometry, values: Vec<f32>) -> Self {
        debug_assert!(
            values.is_empty() || values.len() == geometry.radials() * geometry.gates(),
            "a polar field is exactly radials × gates, or nothing"
        );
        Self { geometry, values }
    }
}

impl PolarField {
    /// The header this field's byte form opens with: three counts and two
    /// ranges.
    const HEADER: usize = 4 * 4 + 8 * 3;

    /// This field as bytes, little-endian, for the one boundary that can only
    /// carry buffers.
    pub fn to_bytes(&self) -> Vec<u8> {
        let g = &self.geometry;
        let mut out = Vec::with_capacity(Self::HEADER + g.wedges.len() * 8 + self.values.len() * 4);
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
        for v in &self.values {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out
    }

    /// The inverse of [`Self::to_bytes`], or `None` for anything this build did
    /// not write.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
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

        let wedge_bytes = radials.checked_mul(8)?;
        let value_bytes = n_values.checked_mul(4)?;
        if bytes.len()
            != Self::HEADER
                .checked_add(wedge_bytes)?
                .checked_add(value_bytes)?
        {
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
        let mut values = Vec::with_capacity(n_values);
        for _ in 0..n_values {
            values.push(f32::from_le_bytes(
                bytes[at..at + 4].try_into().expect("checked"),
            ));
            at += 4;
        }
        Some(Self {
            geometry: PolarGeometry {
                wedges,
                first_gate_slant_km,
                gate_interval_slant_km,
                elevation_deg,
                gates,
                reach_gates,
            },
            values,
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
            values,
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
