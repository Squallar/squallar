//! **The polar surface's producer**: one sweep's gates as the codes the wire
//! carried, beside the geometry those codes sit in.
//!
//! [`super::codes`] answers what a code *means* — the table, the reduce
//! operator, which products an eight-bit plane may carry at all. This module
//! answers where the codes come from, and it is the half that was missing:
//! `RenderedFrame::codes` and the wire tail behind it landed with no renderer
//! emitting a plane.
//!
//! # Why this is a copy and not a computation
//!
//! The plane's bytes are `MomentData::raw_values()` verbatim. Nothing here
//! decodes a gate, so nothing here can round one: the raster path's
//! [`super::moment_value_at`] turns a raw word into a number and
//! `get_color_for_value` turns that number into a colour, and
//! [`super::codes::Lut`] is the composition of those two evaluated once per
//! code instead of once per gate. A read-back is therefore an indexing rather
//! than a rounding, and `the_plane_paints_what_the_raster_paints` asserts that
//! as byte equality over every gate of a real sweep rather than as a
//! tolerance.
//!
//! That equality only holds while a code is one byte and means one thing
//! across the whole sweep, so this module refuses every sweep where it would
//! not — see [`PlaneUnavailable`]. A refusal is not a degraded plane; it is
//! the raster, rendered the way it is rendered today.

use nexrad_model::data::{DataMoment as _, Radial};

use super::codes::{BELOW_THRESHOLD_CODE, CodePlane, LutKey, PlaneRefusal, RANGE_FOLDED_CODE};
use crate::types::RadarProduct;

/// **Why a sweep could not become a code plane**, past the refusals
/// [`CodePlane::build`] already owns.
///
/// Every arm is a fall back to the raster and none is a degraded plane. The
/// split from [`PlaneRefusal`] is deliberate and not stylistic: that type is
/// about a *payload* — a shape past the caps, a product an eight-bit plane
/// cannot carry — and is asked again on the decode side, where it guards a
/// hostile reply. This one is about a *sweep*, is asked once at the producer,
/// and names things a decoder has no way to re-check because they are
/// properties of the volume rather than of the bytes that left it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PlaneUnavailable {
    /// No radial in the sweep carries this product's moment.
    NoMoment,
    /// The sweep's moment blocks do not agree about how a code decodes.
    ///
    /// A plane carries **one** [`LutKey`], so two radials with different
    /// scales would be painted through whichever one was read first — a
    /// picture of numbers nobody measured, across half the disc.
    MixedDecode,
    /// The codes are not bytes. A sixteen-bit moment has no byte plane, and
    /// truncating one would be a silent quantiser.
    ///
    /// [`super::codes::r8_fidelity`] answers which products *admit* eight
    /// bits; this is the width the sweep in hand actually carried, which for
    /// differential reflectivity is the narrower question and the one that
    /// decides.
    WideWireWord { word_bits: u8 },
    /// `scale == 0.0`: the format's own "the raw words *are* the values" arm.
    ///
    /// Refused because in that encoding **there is no code that means "no
    /// gate"** — 0 and 1 are ordinary numbers. A radial shorter than the
    /// sweep's stride has to be padded to reach it, and every byte this
    /// module could pad with would paint a measurement.
    RawWordEncoding,
    /// A non-finite scale or offset. [`super::codes::Lut::build`] degrades
    /// such a key to a fully unpainted table, which is a blank disc where the
    /// raster still draws whatever the arithmetic produced.
    NonFiniteDecode { scale: f32, offset: f32 },
    /// Not one gate of the sweep is painted.
    ///
    /// Refused rather than shipped as an empty disc, because a fan has no
    /// reach: `FanSweep::is_well_formed` requires `reach_gates >= 1`, so a
    /// plane with nothing in it would arrive as a frame that draws nothing
    /// *and* has no raster to fall back to.
    NothingPainted,
    /// [`CodePlane::build`] refused the payload this module assembled.
    Refused(PlaneRefusal),
}

/// **A sweep's plane and how far it reaches.**
///
/// The reach is measured off the codes rather than declared off the shape, the
/// same way `PolarBuffers::into_field` measures the raster's: "out of what was
/// painted rather than what was declared". A gate past it is a gate the sweep
/// carried and nothing was above threshold in, and drawing the disc out to it
/// would put a ring of nothing outside the weather.
#[derive(Clone, Debug, PartialEq)]
pub struct SweepPlane {
    pub plane: CodePlane,
    /// The largest painted gate index, plus one. At least 1: a sweep with none
    /// is [`PlaneUnavailable::NothingPainted`].
    pub reach_gates: usize,
}

/// How a sweep's codes decode — the three numbers every radial must agree
/// about before one table can colour all of them.
#[derive(Clone, Copy, PartialEq)]
struct Decode {
    scale: f32,
    offset: f32,
    word_bits: u8,
}

impl Decode {
    /// A moment block's own. Read off the block rather than off the product,
    /// because two sweeps of one product can disagree — which is the whole
    /// reason [`LutKey`] carries the pair.
    fn of(moment: &nexrad_model::data::MomentData) -> Self {
        Self {
            scale: moment.scale(),
            offset: moment.offset(),
            word_bits: moment.data_word_size(),
        }
    }

    /// Whether two blocks decode identically.
    ///
    /// **On the bits, not on the numbers.** `f32` equality would call two NaN
    /// scales different and `-0.0` and `0.0` the same, and both answers are
    /// wrong here: the question is whether one table can serve both blocks,
    /// and one table is built from one bit pattern.
    fn agrees_with(self, other: Self) -> bool {
        self.scale.to_bits() == other.scale.to_bits()
            && self.offset.to_bits() == other.offset.to_bits()
            && self.word_bits == other.word_bits
    }
}

/// **Whether a code paints anything**, for each of the 256 a byte can hold.
///
/// The raster's own predicate, precomputed: `moment_value_at` followed by
/// [`super::painted_moment_value`], which is exactly the pair the fill loop
/// runs per gate. A table rather than a call per gate because the answer
/// depends only on the code and the sweep's one decode, so a surveillance
/// sweep's 1.3 million gates ask 256 questions between them.
///
/// `scale == 0.0` is not a case here: [`PlaneUnavailable::RawWordEncoding`]
/// has already refused that encoding, so the sentinels are sentinels.
fn painted_codes(decode: Decode) -> [bool; 256] {
    let mut painted = [false; 256];
    for (code, slot) in painted.iter_mut().enumerate() {
        let code = code as u8;
        *slot = match code {
            BELOW_THRESHOLD_CODE => false,
            RANGE_FOLDED_CODE => true,
            _ => {
                let value = (f32::from(code) - decode.offset) / decode.scale;
                // `painted_moment_value`'s own bound on a decoded value,
                // spelled through it so the two cannot drift.
                super::painted_moment_value(nexrad_model::data::MomentValue::Value(value)).is_some()
            }
        };
    }
    painted
}

/// **One sweep's gates as a code plane**, radial-major over `gates` columns.
///
/// `gates` is the sweep's stride — the most any radial carrying the product
/// declares, which is `compute_gate_span`'s own figure — so a radial shorter
/// than it is padded out with [`BELOW_THRESHOLD_CODE`], the code the raster
/// leaves such a pixel unclaimed for.
///
/// Refuses rather than approximates; see [`PlaneUnavailable`].
pub fn sweep_code_plane(
    radials: &[Radial],
    product: RadarProduct,
    gates: usize,
) -> Result<SweepPlane, PlaneUnavailable> {
    let mut decode: Option<Decode> = None;
    for radial in radials {
        let Some(moment) = product.get_moment(radial) else {
            continue;
        };
        let this = Decode::of(moment);
        match decode {
            None => decode = Some(this),
            Some(first) if first.agrees_with(this) => {}
            Some(_) => return Err(PlaneUnavailable::MixedDecode),
        }
    }
    let Some(decode) = decode else {
        return Err(PlaneUnavailable::NoMoment);
    };
    if decode.word_bits != 8 {
        return Err(PlaneUnavailable::WideWireWord {
            word_bits: decode.word_bits,
        });
    }
    // Literal zero, and deliberately: it is the format's own encoding rather
    // than a tolerance, the same comparison `moment_value_at` and `Lut::build`
    // make.
    if decode.scale == 0.0 {
        return Err(PlaneUnavailable::RawWordEncoding);
    }
    if !decode.scale.is_finite() || !decode.offset.is_finite() {
        return Err(PlaneUnavailable::NonFiniteDecode {
            scale: decode.scale,
            offset: decode.offset,
        });
    }

    let cells = radials
        .len()
        .checked_mul(gates)
        .ok_or(PlaneUnavailable::Refused(PlaneRefusal::Shape {
            radials: radials.len(),
            gates,
        }))?;
    let painted = painted_codes(decode);
    let mut codes = vec![BELOW_THRESHOLD_CODE; cells];
    let mut reach_gates = 0usize;
    for (radial_idx, radial) in radials.iter().enumerate() {
        let Some(moment) = product.get_moment(radial) else {
            continue;
        };
        // `raw_values()` and not `gate_count()`: the byte buffer is what the
        // plane copies, and `moment_value_at` reads the same authority for the
        // same reason. A block declaring more gates than it carries would
        // otherwise index past its own bytes.
        let row = moment.raw_values();
        let take = row.len().min(gates);
        let at = radial_idx * gates;
        codes[at..at + take].copy_from_slice(&row[..take]);
        for (gate, &code) in row[..take].iter().enumerate() {
            if painted[usize::from(code)] {
                reach_gates = reach_gates.max(gate + 1);
            }
        }
    }
    if reach_gates == 0 {
        return Err(PlaneUnavailable::NothingPainted);
    }

    let plane = CodePlane::build(
        radials.len(),
        gates,
        codes,
        LutKey {
            product,
            scale: decode.scale,
            offset: decode.offset,
        },
        decode.word_bits,
    )
    .map_err(PlaneUnavailable::Refused)?;
    Ok(SweepPlane { plane, reach_gates })
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
