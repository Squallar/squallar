//! The frame reply type — what a rasterizing job produces — beside the
//! renderer that fills it (WO-M7.1), and its wire form (WO-M7c).
//!
//! [`RenderedFrame`] and its `From<SweepRender>` conversion moved here
//! **verbatim** from `rustdar_frontend::offload` at WO-M7.1 — the move is
//! what lets the radar codec rows ([`crate::jobs`]) name their own output
//! type in their `run` bodies. WO-M7c closed the reply direction: the frame
//! stopped crossing the browser's reply port as eight hand-written JS
//! fields and rides the job registry's `OUT` payload in the
//! [`to_bytes`](RenderedFrame::to_bytes)/[`from_bytes`](RenderedFrame::from_bytes)
//! form below, so the two wire newtypes that spell its provenance enums as
//! numbers ([`MeltingLayerWire`]/[`StormMotionWire`]) moved here beside the
//! codec that is now their one consumer.

/// What a rasterizing job produces: the RGBA texture, the half-width it was
/// projected at, and the per-pixel value grid (`NAN` where no gate landed).
///
/// Named fields, as the renderer's own [`crate::render::SweepRender`]
/// has: the two buffers are the same shape to a message port, and transposing
/// them would swap a texture for a value grid somewhere with no type error to
/// catch it. A separate type and not that one because this is what crosses the
/// port.
///
/// The extent and the fold limit are metadata and stay metadata — they say
/// where the pixels *are* and what speed they wrap at, never how many of them
/// there are. How many there are is the buffer's own length, checked rather
/// than believed at each consumer (`constants::raster_side_from_rgba_len`);
/// nothing on this port describes its own shape, which is what keeps a
/// malformed payload from being believed. Adding a second `f64` beside the
/// extent does not weaken that: neither number can be read as a dimension,
/// and the guard that protects a pane from a blank texture reads the length
/// and only the length.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderedFrame {
    pub image: Vec<u8>,
    pub max_range_km: f64,
    /// The gates behind the pixels, at the resolution the radar measured them.
    ///
    /// **The `side²` `f32` raster grid is not here and does not leave the
    /// renderer.** It used to: `7362² × 4` = 206.75 MiB on desktop, and 16 MiB
    /// through the browser's `postMessage` — transferred, but still copied once
    /// into the worker's linear memory and once back out of the page's. This is
    /// the same numbers at the resolution they were measured at, about 5 MiB
    /// for the widest sweep the fleet flies, and it is what a hover reads. See
    /// [`crate::render::polar`].
    pub polar: crate::render::polar::PolarField,
    /// Where the rendered sweep's cut declared its velocity folds, m/s, or
    /// `None` for a raster with no one cut behind it — every Level III
    /// product and every volume product — and for a volume that declared
    /// nothing, which is every Message 1 volume.
    ///
    /// See [`crate::render::SweepRender::nyquist_ms`], which is where
    /// it comes from and which explains what it is a property of.
    pub nyquist_ms: Option<f64>,
    /// Where the melting layer this raster was classified against came from,
    /// or `None` for every raster that classified nothing — which is every
    /// product but the hybrid classification.
    ///
    /// See [`crate::hca::MeltingLayerSource`]. It rides beside
    /// `nyquist_ms` and for the same reason: it is a fact about *this* picture
    /// that the far end cannot recompute, and here it is the difference
    /// between a classification measured for this volume and one standing on a
    /// fleet constant that has been measured 3 km wrong.
    pub melting_layer_source: Option<crate::hca::MeltingLayerSource>,
    /// Where the storm motion vector this raster was shifted by came from, or
    /// `None` for every raster that shifted nothing — which is every product
    /// but storm-relative velocity.
    ///
    /// See [`crate::srv::SrvMotion`]. It rides beside
    /// `melting_layer_source` and for the same reason: it is a fact about
    /// *this* picture that the far end cannot recompute — the projection of
    /// this vector is already inside every gate value, and the two derived
    /// rungs are computed from a wind profile the page never sees.
    ///
    /// The whole vector rather than its provenance byte, because the legend
    /// draws the speed and direction and only apologises for nothing.
    pub storm_motion: Option<crate::srv::SrvMotion>,
}

impl From<crate::render::SweepRender> for RenderedFrame {
    /// The renderer's own answer, whole. One conversion for all three
    /// rasterizing arms, so a Level III frame and a Level II one cannot come to
    /// describe themselves differently.
    fn from(render: crate::render::SweepRender) -> Self {
        // **Where the raster value grid dies, on every path.** It is the
        // rasterizer's own instrument — its tests measure painted ranges and
        // ring bounds off it, and the colouring pass writes through it — and
        // nothing outside that crate has needed it since the readout started
        // reading gates. This is the one conversion all three rasterizing arms
        // come through, so putting it here is what makes "it never leaves the
        // renderer" a property of the type rather than of three call sites.
        //
        // Handed back rather than freed: the slot is waiting for it, and on
        // desktop this is a 206.75 MiB allocation glibc can never recycle. See
        // `crate::render::POOLED_VALUES`.
        crate::render::recycle_values(render.values);
        Self {
            image: render.image,
            max_range_km: render.max_range_km,
            polar: render.polar,
            nyquist_ms: render.nyquist_ms,
            melting_layer_source: render.melting_layer_source,
            storm_motion: render.storm_motion,
        }
    }
}

/// The reply half of the job boundary's erasure seam: a described frame
/// render answers this type through the codec rows in [`crate::jobs`] —
/// erased on the direct path, and on the wire in the form
/// [`RenderedFrame::to_bytes`] writes.
impl rustdar_source::job::JobOut for RenderedFrame {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn into_any(self: Box<Self>) -> Box<dyn std::any::Any> {
        self
    }

    /// The texture is the one raster here, and the rasterizers write it in
    /// straight alpha; the polar grid carries measurements, not pixels.
    fn straight_rasters_mut(&mut self) -> Vec<&mut [u8]> {
        vec![&mut self.image]
    }
}

impl RenderedFrame {
    /// The frame's wire form — the bytes the reply direction's `OUT`
    /// payload carries for the three frame rows (WO-M7c).
    ///
    /// Layout, in the house shape (scalars first, count-prefixed blocks,
    /// the unbounded tail last):
    /// `[max_range_km f64]`
    /// `[nyquist tag u8][nyquist_ms f64 when the tag says so]`
    /// `[melting tag u8][melting code u8 when the tag says so]`
    /// `[storm tag u8][storm code u8 + speed_kt f32 + direction_deg f32 when the tag says so]`
    /// `[polar_len u32][polar bytes]` and then **the image as the rest**.
    ///
    /// Each optional rides behind a one-byte presence tag rather than a
    /// sentinel, because every one of these fields has honest absent states
    /// (a Level III product has no Nyquist, only the hybrid classification
    /// has a melting layer, only storm-relative velocity has a vector) and
    /// a sentinel value would put a fake number where "none" belongs. The
    /// storm trio travels **atomically under one tag**: source, speed and
    /// direction are written together or not at all, so a half-formed
    /// vector — a real source beside a zeroed speed, the confident lie the
    /// old field-per-value reply had to fend off with a `?`-chain at the
    /// reader — is now unrepresentable on the wire.
    ///
    /// The image is deliberately **the rest, with no self-described shape**:
    /// nothing on this port describes its own dimensions, and the guard
    /// that stands between a malformed payload and a wrong-shaped texture
    /// is unchanged — the consumer derives the side from the buffer's own
    /// length and refuses anything that is not a square this build makes
    /// (`raster_side_from_rgba_len`). See the type's own doc.
    ///
    /// One buffer where the browser reply used to transfer the image and
    /// the polar block separately, which costs one concatenating memcpy of
    /// both (up to ~20 MiB for the widest still frame) **where the job ran**
    /// — in the worker, off the frame thread — accepted by design for one
    /// payload shape on the whole reply direction (WO-M7c).
    pub fn to_bytes(&self) -> Vec<u8> {
        let polar = self.polar.to_bytes();
        let mut out = Vec::with_capacity(8 + 2 + 2 + 10 + 4 + polar.len() + self.image.len());
        out.extend_from_slice(&self.max_range_km.to_le_bytes());
        match self.nyquist_ms {
            None => out.push(0),
            Some(nyquist_ms) => {
                out.push(1);
                out.extend_from_slice(&nyquist_ms.to_le_bytes());
            }
        }
        match self.melting_layer_source {
            None => out.push(0),
            Some(source) => {
                out.push(1);
                out.push(MeltingLayerWire(source).wire_code());
            }
        }
        match self.storm_motion {
            None => out.push(0),
            Some(motion) => {
                out.push(1);
                out.push(StormMotionWire(motion.source).wire_code());
                out.extend_from_slice(&motion.speed_kt.to_le_bytes());
                out.extend_from_slice(&motion.direction_deg.to_le_bytes());
            }
        }
        out.extend_from_slice(&(polar.len() as u32).to_le_bytes());
        out.extend_from_slice(&polar);
        out.extend_from_slice(&self.image);
        out
    }

    /// The inverse of [`Self::to_bytes`], or `None` for anything this build
    /// did not write: a buffer truncated anywhere inside the framed prefix,
    /// a presence tag outside `{0, 1}`, a provenance code outside this
    /// build's maps, or a polar block its own codec refuses — including one
    /// whose stated length disagrees with its content, which
    /// `PolarField::from_bytes` checks against the exact slice. All of them
    /// are a clean refusal that reads as "nothing to draw", never a
    /// misparse: on a same-build wire (the M5 token refuses every other
    /// pairing) such bytes can only be a corrupt buffer, and half a frame
    /// believed is worse than none.
    ///
    /// The image takes the rest, so there is no trailing-bytes case to
    /// refuse — every byte after the polar block **is** payload, and its
    /// one guard is the consumer's own length arithmetic, exactly as on
    /// the direct path.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let mut r = rustdar_source::wire::Reader::new(bytes);
        let max_range_km = r.f64()?;
        let nyquist_ms = match r.u8()? {
            0 => None,
            1 => Some(r.f64()?),
            _ => return None,
        };
        let melting_layer_source = match r.u8()? {
            0 => None,
            1 => Some(MeltingLayerWire::from_wire_code(r.u8()?)?.0),
            _ => return None,
        };
        // The trio decodes under its one tag — all three or none, the
        // atomicity `to_bytes` promises, with no reader-side chain needed
        // to enforce it.
        let storm_motion = match r.u8()? {
            0 => None,
            1 => {
                let source = StormMotionWire::from_wire_code(r.u8()?)?.0;
                let speed_kt = r.f32()?;
                let direction_deg = r.f32()?;
                Some(crate::srv::SrvMotion {
                    speed_kt,
                    direction_deg,
                    source,
                })
            }
            _ => return None,
        };
        let polar_len = r.u32()? as usize;
        let polar = crate::render::polar::PolarField::from_bytes(r.take(polar_len)?)?;
        Some(Self {
            image: r.rest().to_vec(),
            max_range_km,
            polar,
            nyquist_ms,
            melting_layer_source,
            storm_motion,
        })
    }
}

/// A [`MeltingLayerSource`](crate::hca::MeltingLayerSource) as a number, for
/// the one boundary that can only carry numbers — which is
/// [`RenderedFrame::to_bytes`], the frame's wire form, since WO-M7c. (The
/// pair spent its earlier life in `rustdar_frontend::offload`, spelling the
/// same bytes into a named JS field on the browser's reply port; the codec
/// is that boundary's one descendant, so the pair moved here beside it.)
///
/// A newtype rather than two free functions so the pair cannot drift apart:
/// [`from_wire_code`](Self::from_wire_code) is exhaustive over the same match
/// arms [`wire_code`](Self::wire_code) writes, so adding a variant upstream
/// fails this build rather than silently encoding as "unknown".
///
/// `None` from `from_wire_code` is a byte this build does not have. Through
/// the codec that refuses the whole frame rather than reading "no source
/// stated": the wire is same-build-only (the build token refuses every
/// cross-build pairing at the handshake), so such a byte is a corrupt
/// buffer, and a frame with one honest field disbelieved is a frame nothing
/// should trust.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeltingLayerWire(pub crate::hca::MeltingLayerSource);

impl MeltingLayerWire {
    pub fn wire_code(self) -> u8 {
        use crate::hca::MeltingLayerSource as S;
        match self.0 {
            S::Rpg => 0,
            S::RadarDetected => 1,
            S::Sounding => 2,
            S::FleetDefault => 3,
        }
    }

    /// The inverse of [`wire_code`](Self::wire_code).
    pub fn from_wire_code(code: u8) -> Option<Self> {
        use crate::hca::MeltingLayerSource as S;
        let source = match code {
            0 => S::Rpg,
            1 => S::RadarDetected,
            2 => S::Sounding,
            3 => S::FleetDefault,
            _ => return None,
        };
        Some(Self(source))
    }
}

/// A [`StormMotionSource`](crate::srv::StormMotionSource) as a number, for
/// the same boundary [`MeltingLayerWire`] crosses — the frame's wire form.
///
/// A newtype rather than two free functions so the pair cannot drift apart:
/// [`from_wire_code`](Self::from_wire_code) is exhaustive over the same match
/// arms [`wire_code`](Self::wire_code) writes, so adding a rung upstream fails
/// this build rather than silently encoding as "unknown" — which for this
/// value would mean an SRV pane reporting a Bunkers prediction as the RPG's own
/// cell average, the one confusion the whole path exists to prevent.
///
/// The numbering **is** the declaration order, which is the fallback order, so
/// a code reads as a rung of the chain. `None` from `from_wire_code` is a byte
/// this build does not have, and through the codec it refuses the whole frame
/// for [`MeltingLayerWire`]'s reason: on a same-build wire it can only be a
/// corrupt buffer, and this byte in particular decides which vector a picture
/// was shifted by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StormMotionWire(pub crate::srv::StormMotionSource);

impl StormMotionWire {
    pub fn wire_code(self) -> u8 {
        use crate::srv::StormMotionSource as S;
        match self.0 {
            S::UserOverride => 0,
            S::RpgScitAverage => 1,
            S::BunkersRightMover => 2,
            S::MeanWind => 3,
        }
    }

    /// The inverse of [`wire_code`](Self::wire_code).
    pub fn from_wire_code(code: u8) -> Option<Self> {
        use crate::srv::StormMotionSource as S;
        let source = match code {
            0 => S::UserOverride,
            1 => S::RpgScitAverage,
            2 => S::BunkersRightMover,
            3 => S::MeanWind,
            _ => return None,
        };
        Some(Self(source))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A polar block with real content, built as bytes and decoded through
    /// the polar codec itself — the layout below is the one
    /// `polar::tests::the_polar_wire_layout_is_the_one_this_protocol_ships`
    /// pins, so this fixture cannot drift from it silently.
    fn a_polar_field() -> crate::render::polar::PolarField {
        let mut bytes = Vec::new();
        // Header: radials, gates, reach_gates, n_values, then first-gate
        // slant, gate interval, elevation.
        bytes.extend_from_slice(&2u32.to_le_bytes());
        bytes.extend_from_slice(&3u32.to_le_bytes());
        bytes.extend_from_slice(&3u32.to_le_bytes());
        bytes.extend_from_slice(&6u32.to_le_bytes());
        bytes.extend_from_slice(&2.125f64.to_le_bytes());
        bytes.extend_from_slice(&0.25f64.to_le_bytes());
        bytes.extend_from_slice(&0.5f64.to_le_bytes());
        // Two wedges, then 2 × 3 finite values (NaN would defeat the
        // round-trip equality this fixture exists for).
        for wedge in [(10.0f32, 0.5f32), (11.0, 0.5)] {
            bytes.extend_from_slice(&wedge.0.to_le_bytes());
            bytes.extend_from_slice(&wedge.1.to_le_bytes());
        }
        for value in [1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        crate::render::polar::PolarField::from_bytes(&bytes)
            .expect("the fixture polar block decodes")
    }

    /// The full fixture: every optional present, so the storm trio, both
    /// provenance codes and the Nyquist all ride the wire.
    fn a_full_frame() -> RenderedFrame {
        RenderedFrame {
            image: vec![10, 20, 30, 40, 50, 60, 70, 80],
            max_range_km: 230.0,
            polar: a_polar_field(),
            nyquist_ms: Some(26.4),
            melting_layer_source: Some(crate::hca::MeltingLayerSource::RadarDetected),
            storm_motion: Some(crate::srv::SrvMotion {
                speed_kt: 33.5,
                direction_deg: 245.0,
                source: crate::srv::StormMotionSource::BunkersRightMover,
            }),
        }
    }

    /// The bare fixture: every optional absent — a Level III frame's honest
    /// shape — and an empty-geometry polar, which is what a frame with no
    /// gates carries.
    fn a_bare_frame() -> RenderedFrame {
        RenderedFrame {
            image: vec![1, 2, 3, 4],
            max_range_km: 460.0,
            polar: crate::render::polar::PolarField::default(),
            nyquist_ms: None,
            melting_layer_source: None,
            storm_motion: None,
        }
    }

    #[test]
    fn the_frame_survives_its_own_codec_with_and_without_the_optionals() {
        for frame in [a_full_frame(), a_bare_frame()] {
            assert_eq!(
                RenderedFrame::from_bytes(&frame.to_bytes()),
                Some(frame.clone()),
                "the frame did not survive its own codec",
            );
        }
    }

    /// The malformed shapes, each a clean refusal — the reply-codec walk
    /// (`a_malformed_overlay_reply_is_refused_rather_than_misread`'s shape):
    /// a truncation walk over everything ahead of the image, then each
    /// judged byte mutated to a *valid different value* first, read back as
    /// a positive control, and only then to the refused one.
    #[test]
    fn a_malformed_frame_reply_is_refused_rather_than_misread() {
        let frame = a_full_frame();
        let encoded = frame.to_bytes();
        // Everything ahead of the image is framed; the image is the rest by
        // design, so a cut inside it still decodes and its guard is the
        // consumer's own length arithmetic (`raster_side_from_rgba_len`).
        let image_at = encoded.len() - frame.image.len();

        // Control first: untouched bytes decode, so every refusal below is
        // the mutation's doing.
        assert!(RenderedFrame::from_bytes(&encoded).is_some());

        // Layout, stated once — the premise behind every offset below:
        // max_range 0..8, nyquist tag 8 (+ f64 9..17), melting tag 17
        // (+ code 18), storm tag 19 (+ code 20, speed 21..25, dir 25..29),
        // polar_len 29..33, polar 33..image_at.
        assert_eq!(
            image_at,
            33 + 40 + 2 * 8 + 6 * 4,
            "the fixture's framed prefix moved; re-derive the offsets",
        );

        for cut in 1..image_at {
            assert_eq!(
                RenderedFrame::from_bytes(&encoded[..cut]),
                None,
                "the frame reply truncated to {cut} bytes was accepted",
            );
        }

        // Presence tags outside {0, 1}. The three offsets are the layout
        // above; the read-back controls prove each one is the tag this test
        // believes, so the refusals are about the tag and not some other
        // byte.
        for (at, what) in [(8usize, "nyquist"), (17, "melting"), (19, "storm")] {
            assert_eq!(encoded[at], 1, "premise: the {what} tag is at {at}");
            let mut bad_tag = encoded.clone();
            bad_tag[at] = 2;
            assert_eq!(
                RenderedFrame::from_bytes(&bad_tag),
                None,
                "a {what} presence tag of 2 was accepted",
            );
        }

        // The melting code: 3 is a valid different rung and reads back
        // moved — the control that byte 18 is the code — then 4, a byte
        // this build does not have, refuses the frame (same-build wire; a
        // foreign byte is a corrupt buffer).
        let mut remapped = encoded.clone();
        remapped[18] = 3;
        assert_eq!(
            RenderedFrame::from_bytes(&remapped)
                .expect("code 3 is a real rung")
                .melting_layer_source,
            Some(crate::hca::MeltingLayerSource::FleetDefault),
            "byte 18 is not the melting code; the refusal below would be \
             about some other field",
        );
        let mut bad_code = encoded.clone();
        bad_code[18] = 4;
        assert_eq!(
            RenderedFrame::from_bytes(&bad_code),
            None,
            "melting-layer code 4 was accepted",
        );

        // The storm code, same pair.
        let mut remapped = encoded.clone();
        remapped[20] = 3;
        assert_eq!(
            RenderedFrame::from_bytes(&remapped)
                .expect("code 3 is a real rung")
                .storm_motion
                .expect("the trio is present")
                .source,
            crate::srv::StormMotionSource::MeanWind,
            "byte 20 is not the storm code; the refusal below would be \
             about some other field",
        );
        let mut bad_code = encoded.clone();
        bad_code[20] = 4;
        assert_eq!(
            RenderedFrame::from_bytes(&bad_code),
            None,
            "storm-motion code 4 was accepted",
        );

        // A polar length that lies into the image: the polar codec checks
        // its stated counts against the exact slice it is handed, so the
        // stolen bytes are a refusal rather than four image bytes read as a
        // value.
        let stated = u32::from_le_bytes(encoded[29..33].try_into().unwrap());
        assert_eq!(
            stated as usize,
            image_at - 33,
            "bytes 29..33 are not the polar length; the refusal below would \
             be about some other field",
        );
        let mut lying = encoded.clone();
        lying[29..33].copy_from_slice(&(stated + 4).to_le_bytes());
        assert_eq!(
            RenderedFrame::from_bytes(&lying),
            None,
            "a polar block that annexed four image bytes was accepted",
        );
    }

    /// The trio travels atomically: one tag, three fields, so a half-formed
    /// vector — the confident lie the old field-per-value reply had to fend
    /// off at the reader — is unrepresentable. What CAN be malformed is the
    /// trio cut short, and the truncation walk above already refuses every
    /// such cut; this pins the equivalence the layout claims, that absent
    /// means absent-together.
    #[test]
    fn the_storm_trio_is_absent_together_or_present_together() {
        let mut frame = a_full_frame();
        frame.storm_motion = None;
        let encoded = frame.to_bytes();
        // With the tag at 0, the code/speed/direction bytes do not exist at
        // all: the polar length follows the tag directly.
        assert_eq!(encoded[19], 0, "the storm tag encodes the absence");
        assert_eq!(
            u32::from_le_bytes(encoded[20..24].try_into().unwrap()) as usize,
            40 + 2 * 8 + 6 * 4,
            "nothing rides between an absent trio's tag and the polar length",
        );
        let back = RenderedFrame::from_bytes(&encoded).expect("the absent form decodes");
        assert_eq!(back.storm_motion, None);
        // And the present form answers the whole vector — the two fixtures'
        // round-trips above are the rest of the claim.
        assert_eq!(
            RenderedFrame::from_bytes(&a_full_frame().to_bytes())
                .expect("the present form decodes")
                .storm_motion,
            a_full_frame().storm_motion,
        );
    }

    /// Both provenance maps: every rung has a stable, distinct byte,
    /// `from_wire_code` is the genuine inverse, and a byte outside the map
    /// answers `None`. The exhaustive walk is what makes it a property of
    /// the enum — a fifth variant upstream fails the match arms in the
    /// newtypes first and these row counts second.
    #[test]
    fn every_provenance_rung_has_a_stable_distinct_wire_code() {
        use crate::hca::MeltingLayerSource as M;
        use crate::srv::StormMotionSource as S;

        const MELTING: [(M, u8); 4] = [
            (M::Rpg, 0),
            (M::RadarDetected, 1),
            (M::Sounding, 2),
            (M::FleetDefault, 3),
        ];
        let mut seen = std::collections::HashSet::new();
        for (source, expected) in MELTING {
            let code = MeltingLayerWire(source).wire_code();
            assert_eq!(code, expected, "{source:?} moved on the wire");
            assert!(seen.insert(code), "{source:?} shares byte {code}");
            assert_eq!(
                MeltingLayerWire::from_wire_code(code),
                Some(MeltingLayerWire(source)),
                "byte {code} did not decode back to {source:?}",
            );
        }
        assert_eq!(seen.len(), 4, "a melting rung was added or removed");
        assert_eq!(MeltingLayerWire::from_wire_code(4), None);
        assert_eq!(MeltingLayerWire::from_wire_code(u8::MAX), None);

        // Declaration order, which is fallback order, which is the numbering.
        const STORM: [(S, u8); 4] = [
            (S::UserOverride, 0),
            (S::RpgScitAverage, 1),
            (S::BunkersRightMover, 2),
            (S::MeanWind, 3),
        ];
        let mut seen = std::collections::HashSet::new();
        for (source, expected) in STORM {
            let code = StormMotionWire(source).wire_code();
            assert_eq!(
                code, expected,
                "{source:?} moved on the wire: a page and a worker built \
                 either side of that change caption one rung with another's \
                 words",
            );
            assert!(seen.insert(code), "{source:?} shares byte {code}");
            assert_eq!(
                StormMotionWire::from_wire_code(code),
                Some(StormMotionWire(source)),
                "byte {code} did not decode back to {source:?}",
            );
        }
        assert_eq!(seen.len(), 4, "a storm rung was added or removed");
        assert_eq!(StormMotionWire::from_wire_code(4), None);
        assert_eq!(StormMotionWire::from_wire_code(u8::MAX), None);
    }
}
