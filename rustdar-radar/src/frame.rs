//! The frame reply type — what a rasterizing job produces — beside the
//! renderer that fills it, and its wire form.

/// What a rasterizing job produces: the RGBA texture, the half-width it was
/// projected at, and the per-pixel value grid (`NAN` where no gate landed).
#[derive(Debug, Clone, PartialEq)]
pub struct RenderedFrame {
    pub image: Vec<u8>,
    pub max_range_km: f64,
    /// The gates behind the pixels, at the resolution the radar measured them.
    pub polar: crate::render::polar::PolarField,
    /// Where the rendered sweep's cut declared its velocity folds, m/s, or
    /// `None` for a raster with no one cut behind it — every Level III
    /// product and every volume product — and for a volume that declared
    /// nothing, which is every Message 1 volume.
    pub nyquist_ms: Option<f64>,
    /// Where the melting layer this raster was classified against came from,
    /// or `None` for every raster that classified nothing — which is every
    /// product but the hybrid classification.
    pub melting_layer_source: Option<crate::hca::MeltingLayerSource>,
    /// Where the storm motion vector this raster was shifted by came from, or
    /// `None` for every raster that shifted nothing — which is every product
    /// but storm-relative velocity.
    pub storm_motion: Option<crate::srv::SrvMotion>,
}

impl From<crate::render::SweepRender> for RenderedFrame {
    /// The renderer's own answer, whole. One conversion for all three
    /// rasterizing arms, so a Level III frame and a Level II one cannot come to
    /// describe themselves differently.
    fn from(render: crate::render::SweepRender) -> Self {
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
/// erased on the direct path, and on the wire in the head-plus-tails form
/// [`RenderedFrame::write_head`]/[`RenderedFrame::from_parts`] spell.
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
    /// The frame's wire HEAD — the scalar block the reply direction's `OUT`
    /// payload carries for the three frame rows.
    pub fn write_head(&self, out: &mut Vec<u8>) {
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
    }

    pub fn from_parts(head: &[u8], tails: Vec<Vec<u8>>) -> Option<Self> {
        let Ok([polar_bytes, image]) = <[Vec<u8>; 2]>::try_from(tails) else {
            return None;
        };
        let mut r = rustdar_source::wire::Reader::new(head);
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
        if !r.at_end() {
            return None;
        }
        let polar = crate::render::polar::PolarField::from_bytes(&polar_bytes)?;
        Some(Self {
            image,
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
/// [`RenderedFrame::write_head`], the frame's wire head.
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

    /// Encode `frame` as the frame reply codec does — head via
    /// [`RenderedFrame::write_head`], tails `[polar, image]` — cloning
    /// where the codec moves, because these tests still hold the frame
    /// afterward.
    fn encode_parts(frame: &RenderedFrame) -> (Vec<u8>, Vec<Vec<u8>>) {
        let mut head = Vec::new();
        frame.write_head(&mut head);
        (head, vec![frame.polar.to_bytes(), frame.image.clone()])
    }

    #[test]
    fn the_frame_survives_its_own_codec_with_and_without_the_optionals() {
        for frame in [a_full_frame(), a_bare_frame()] {
            let (head, tails) = encode_parts(&frame);
            assert_eq!(
                RenderedFrame::from_parts(&head, tails),
                Some(frame.clone()),
                "the frame did not survive its own codec",
            );
        }
    }

    #[test]
    fn a_malformed_frame_reply_is_refused_rather_than_misread() {
        let frame = a_full_frame();
        let (head, tails) = encode_parts(&frame);

        // Control first: untouched parts decode, so every refusal below is
        // the mutation's doing.
        assert!(RenderedFrame::from_parts(&head, tails.clone()).is_some());

        assert_eq!(
            head.len(),
            8 + (1 + 8) + (1 + 1) + (1 + 1 + 4 + 4),
            "the fixture's head moved; re-derive the offsets",
        );

        for cut in 1..head.len() {
            assert_eq!(
                RenderedFrame::from_parts(&head[..cut], tails.clone()),
                None,
                "the frame head truncated to {cut} bytes was accepted",
            );
        }
        let mut trailing = head.clone();
        trailing.push(0);
        assert_eq!(
            RenderedFrame::from_parts(&trailing, tails.clone()),
            None,
            "a head with a trailing byte was accepted",
        );

        // Presence tags outside {0, 1}.
        for (at, what) in [(8usize, "nyquist"), (17, "melting"), (19, "storm")] {
            assert_eq!(head[at], 1, "premise: the {what} tag is at {at}");
            let mut bad_tag = head.clone();
            bad_tag[at] = 2;
            assert_eq!(
                RenderedFrame::from_parts(&bad_tag, tails.clone()),
                None,
                "a {what} presence tag of 2 was accepted",
            );
        }

        let mut remapped = head.clone();
        remapped[18] = 3;
        assert_eq!(
            RenderedFrame::from_parts(&remapped, tails.clone())
                .expect("code 3 is a real rung")
                .melting_layer_source,
            Some(crate::hca::MeltingLayerSource::FleetDefault),
            "byte 18 is not the melting code; the refusal below would be \
             about some other field",
        );
        let mut bad_code = head.clone();
        bad_code[18] = 4;
        assert_eq!(
            RenderedFrame::from_parts(&bad_code, tails.clone()),
            None,
            "melting-layer code 4 was accepted",
        );

        // The storm code, same pair.
        let mut remapped = head.clone();
        remapped[20] = 3;
        assert_eq!(
            RenderedFrame::from_parts(&remapped, tails.clone())
                .expect("code 3 is a real rung")
                .storm_motion
                .expect("the trio is present")
                .source,
            crate::srv::StormMotionSource::MeanWind,
            "byte 20 is not the storm code; the refusal below would be \
             about some other field",
        );
        let mut bad_code = head.clone();
        bad_code[20] = 4;
        assert_eq!(
            RenderedFrame::from_parts(&bad_code, tails.clone()),
            None,
            "storm-motion code 4 was accepted",
        );

        // The tail-count refusals: 0, 1 and 3 around the valid 2.
        for (count, wrong) in [
            (0usize, Vec::new()),
            (1, vec![tails[0].clone()]),
            (3, vec![tails[0].clone(), tails[1].clone(), Vec::new()]),
        ] {
            assert_eq!(
                RenderedFrame::from_parts(&head, wrong),
                None,
                "a frame reply with {count} tails was accepted",
            );
        }

        let mut cut_polar = tails.clone();
        cut_polar[0].pop();
        assert_eq!(
            RenderedFrame::from_parts(&head, cut_polar),
            None,
            "a doctored polar tail was accepted",
        );
    }

    /// The trio travels atomically: one tag, three fields, so a half-formed
    /// vector — the confident lie the old field-per-value reply had to fend
    /// off at the reader — is unrepresentable.
    #[test]
    fn the_storm_trio_is_absent_together_or_present_together() {
        let mut frame = a_full_frame();
        frame.storm_motion = None;
        let (head, tails) = encode_parts(&frame);
        assert_eq!(head[19], 0, "the storm tag encodes the absence");
        assert_eq!(
            head.len(),
            20,
            "nothing rides between an absent trio's tag and the head's end",
        );
        let back = RenderedFrame::from_parts(&head, tails).expect("the absent form decodes");
        assert_eq!(back.storm_motion, None);
        // And the present form answers the whole vector — the two fixtures'
        // round-trips above are the rest of the claim.
        let (head, tails) = encode_parts(&a_full_frame());
        assert_eq!(
            RenderedFrame::from_parts(&head, tails)
                .expect("the present form decodes")
                .storm_motion,
            a_full_frame().storm_motion,
        );
    }

    /// Both provenance maps: every rung has a stable, distinct byte,
    /// `from_wire_code` is the genuine inverse, and a byte outside the map
    /// answers `None`.
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
