//! The frame reply type — what a rasterizing job produces — beside the
//! renderer that fills it, and its wire form.

/// One rendered raster, in whichever of the two layouts its path produced.
///
/// Which arm a frame carries is provenance, not target: the renderer's own
/// output is [`Bytes`](Self::Bytes) — pooled, recycled, premultiplied in
/// place at the job boundary — and a frame decoded off the worker wire is
/// [`Pixels`](Self::Pixels), egui's own layout materialized once at decode so
/// the consumer's `ColorImage` can take the buffer by move instead of copying
/// megabytes of texture on the thread the reply lands on.
///
/// Equality is content equality: the same raster in the two layouts is the
/// same raster, which is what keeps the codec round-trip a round-trip.
#[derive(Debug, Clone)]
pub enum RasterImage {
    /// Straight from a renderer, RGBA bytes.
    Bytes(Vec<u8>),
    /// Materialized from a wire reply at decode time, premultiplied.
    Pixels(Vec<ecolor::Color32>),
}

impl RasterImage {
    /// The raster as pixels, from premultiplied RGBA bytes — the decode-side
    /// materialization. `None` for a length that is not whole pixels, per the
    /// job boundary's refusal contract.
    pub fn pixels_from_premultiplied(rgba: &[u8]) -> Option<Self> {
        if !rgba.len().is_multiple_of(4) {
            return None;
        }
        Some(Self::Pixels(
            rgba.chunks_exact(4)
                .map(|px| ecolor::Color32::from_rgba_premultiplied(px[0], px[1], px[2], px[3]))
                .collect(),
        ))
    }

    pub fn len_bytes(&self) -> usize {
        match self {
            Self::Bytes(bytes) => bytes.len(),
            Self::Pixels(pixels) => pixels.len() * 4,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len_bytes() == 0
    }

    /// The raster as owned RGBA bytes — the encode side. A `Pixels` arm pays
    /// a conversion here, which no shipped path does: only a renderer's own
    /// `Bytes` output is ever encoded onto the wire.
    pub fn into_bytes(self) -> Vec<u8> {
        match self {
            Self::Bytes(bytes) => bytes,
            Self::Pixels(pixels) => pixels.into_iter().flat_map(|px| px.to_array()).collect(),
        }
    }

    /// [`into_bytes`](Self::into_bytes), borrowing.
    pub fn to_bytes(&self) -> Vec<u8> {
        self.clone().into_bytes()
    }
}

impl PartialEq for RasterImage {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Bytes(a), Self::Bytes(b)) => a == b,
            (Self::Pixels(a), Self::Pixels(b)) => a == b,
            (mixed_a, mixed_b) => mixed_a.to_bytes() == mixed_b.to_bytes(),
        }
    }
}

/// What a rasterizing job produces: the RGBA texture, the half-width it was
/// projected at, and the per-pixel value grid (`NAN` where no gate landed).
#[derive(Debug, Clone, PartialEq)]
pub struct RenderedFrame {
    pub image: RasterImage,
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
    /// **The same sweep's gates as codes, when this frame was produced on the
    /// polar path** — `docs/radar-polar-design.md` §3.
    ///
    /// Mutually exclusive with [`image`](Self::image), which is why the two
    /// travel as separate tails rather than as one tail that is sometimes
    /// RGBA: a tail that changed meaning would carry no discriminant of its
    /// own, so the head would need one anyway, at which point the third tail
    /// costs its framing and nothing else.
    ///
    /// `Some` exactly on the frames `crate::render::render_sweep_plane`
    /// produced — a Level II plan view whose caller asked for the polar
    /// surface and whose sweep could carry one. Every other renderer answers a
    /// raster and leaves this `None`.
    pub codes: Option<crate::render::codes::CodePlane>,
}

impl From<crate::render::SweepRender> for RenderedFrame {
    /// The renderer's own answer, whole. One conversion for all three
    /// rasterizing arms, so a Level III frame and a Level II one cannot come to
    /// describe themselves differently.
    fn from(render: crate::render::SweepRender) -> Self {
        Self {
            image: RasterImage::Bytes(render.image),
            max_range_km: render.max_range_km,
            polar: render.polar,
            nyquist_ms: render.nyquist_ms,
            melting_layer_source: render.melting_layer_source,
            storm_motion: render.storm_motion,
            // Whichever surface the renderer built. `render_sweep_plane` is
            // the one path that fills this, and it leaves `image` empty, so
            // the exclusivity `into_surface_tails` asserts holds by
            // construction of the renderer rather than by a check here.
            codes: render.codes,
        }
    }
}

/// The reply half of the job boundary's erasure seam: a described frame
/// render answers this type through the codec rows in [`crate::jobs`] —
/// erased on the direct path, and on the wire in the head-plus-tails form
/// [`RenderedFrame::write_head`]/[`RenderedFrame::from_parts`] spell.
impl squallar_source::job::JobOut for RenderedFrame {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn into_any(self: Box<Self>) -> Box<dyn std::any::Any> {
        self
    }

    /// The texture is the one raster here, and the rasterizers write it in
    /// straight alpha; the polar grid carries measurements, not pixels.
    ///
    /// Only the renderer's own `Bytes` output can be straight: a `Pixels`
    /// frame exists only past a wire decode, and the wire carries
    /// premultiplied rasters by contract (the premultiply moved to the
    /// producer), so it reports nothing to fix.
    ///
    /// **[`RenderedFrame::codes`] is deliberately not reachable from here**,
    /// and could not be: codes are palette indices, so multiplying one by an
    /// alpha produces a different gate's measurement rather than a dimmer
    /// version of this one. The match below returns the raster arm alone, so
    /// a plane is excluded by construction rather than by a check that could
    /// be forgotten.
    fn straight_rasters_mut(&mut self) -> Vec<&mut [u8]> {
        match &mut self.image {
            RasterImage::Bytes(bytes) => vec![bytes],
            RasterImage::Pixels(_) => Vec::new(),
        }
    }
}

/// [`RenderedFrame::codes`] absent: the frame's surface is the raster in
/// [`RenderedFrame::image`], and the code tail is empty.
pub const SURFACE_RASTER: u8 = 0;

/// [`RenderedFrame::codes`] present: the frame's surface is the code plane,
/// and the image tail is empty.
pub const SURFACE_CODES: u8 = 1;

impl RenderedFrame {
    /// The widest head this codec writes — every optional present *and* a code
    /// plane's block behind them — so the reply's head is one allocation
    /// whatever the frame carries.
    ///
    /// Arithmetic rather than a literal, and derived from the same terms
    /// `a_malformed_frame_reply_is_refused_rather_than_misread` re-derives its
    /// offsets from.
    pub const WIRE_HEAD_MAX_BYTES: usize = 8
        + (1 + 8)
        + (1 + 1)
        + (1 + 1 + 4 + 4)
        + 1
        + crate::render::codes::CodePlane::WIRE_HEAD_MAX_BYTES;

    /// **The two mutually exclusive surface tails, in wire order —
    /// `(codes, image)`.** Exactly one is non-empty.
    ///
    /// By move, because both are the largest buffers in the reply and a
    /// borrowing form would put two copies of one of them in the process at
    /// once — the shape `8b22ca6f2` measured at 75.4 MiB for a single overlay
    /// picture.
    ///
    /// A frame carrying **both** a plane and a raster is a producer bug, not a
    /// wire condition: the surface is whichever one the renderer built, and
    /// nothing can construct a frame where that question has two answers
    /// except code in this crate. The `debug_assert` is where that bug
    /// surfaces; the decode's own refusal is where a *hostile* payload
    /// claiming both is stopped, and the two are deliberately different
    /// mechanisms because they are different failures.
    pub fn into_surface_tails(self) -> (Vec<u8>, Vec<u8>) {
        debug_assert!(
            self.codes.is_none() || self.image.is_empty(),
            "a frame has one surface: a code plane or a raster, never both",
        );
        match self.codes {
            Some(plane) => (plane.into_codes(), Vec::new()),
            None => (Vec::new(), self.image.into_bytes()),
        }
    }

    /// The frame's wire HEAD — the scalar block the reply direction's `OUT`
    /// payload carries for the three frame rows.
    ///
    /// The surface discriminant and the plane's block are written **last**, so
    /// every field that was in this head before keeps the offset it had.
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
        match &self.codes {
            None => out.push(SURFACE_RASTER),
            Some(plane) => {
                out.push(SURFACE_CODES);
                plane.write_wire_head(out);
            }
        }
    }

    /// The inverse of [`write_head`](Self::write_head) plus the three tails,
    /// in the order [`crate::jobs`] pushes them: `[polar, codes, image]`.
    pub fn from_parts(head: &[u8], tails: Vec<Vec<u8>>) -> Option<Self> {
        let Ok([polar_bytes, code_bytes, image]) = <[Vec<u8>; 3]>::try_from(tails) else {
            return None;
        };
        let mut r = squallar_source::wire::Reader::new(head);
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
        // The surface, and with it which of the two exclusive tails carries
        // this frame. Both halves of the exclusion are REFUSED rather than
        // repaired: a reply that names one surface and fills the other tail is
        // one this build did not write, and picking a winner would paint a
        // picture out of whichever buffer happened to be believed.
        let codes = match r.u8()? {
            SURFACE_RASTER => {
                if !code_bytes.is_empty() {
                    return None;
                }
                None
            }
            SURFACE_CODES => {
                if !image.is_empty() {
                    return None;
                }
                Some(crate::render::codes::CodePlane::from_wire(
                    &mut r, code_bytes,
                )?)
            }
            _ => return None,
        };
        if !r.at_end() {
            return None;
        }
        // Through the tail's own form byte and not through `from_bytes`: the
        // reply carries whichever of `PolarField`'s two value forms the worker
        // held, and the byte in front of the payload is what says which.
        let polar = crate::render::polar::PolarField::from_tail(&polar_bytes)?;
        // The one decode-time materialization: the reply's bytes become the
        // pixel vec the consumer's `ColorImage` will take by move.
        let image = RasterImage::pixels_from_premultiplied(&image)?;
        Some(Self {
            image,
            max_range_km,
            polar,
            nyquist_ms,
            melting_layer_source,
            storm_motion,
            codes,
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
            image: RasterImage::Bytes(vec![10, 20, 30, 40, 50, 60, 70, 80]),
            max_range_km: 230.0,
            polar: a_polar_field(),
            nyquist_ms: Some(26.4),
            melting_layer_source: Some(crate::hca::MeltingLayerSource::RadarDetected),
            storm_motion: Some(crate::srv::SrvMotion {
                speed_kt: 33.5,
                direction_deg: 245.0,
                source: crate::srv::StormMotionSource::BunkersRightMover,
            }),
            codes: None,
        }
    }

    /// The bare fixture: every optional absent — a Level III frame's honest
    /// shape — and an empty-geometry polar, which is what a frame with no
    /// gates carries.
    fn a_bare_frame() -> RenderedFrame {
        RenderedFrame {
            image: RasterImage::Bytes(vec![1, 2, 3, 4]),
            max_range_km: 460.0,
            polar: crate::render::polar::PolarField::default(),
            nyquist_ms: None,
            melting_layer_source: None,
            storm_motion: None,
            codes: None,
        }
    }

    /// A tiny code plane, built through the producer's own constructor so this
    /// fixture cannot describe a plane [`crate::render::codes::CodePlane`]
    /// would refuse.
    fn a_code_plane() -> crate::render::codes::CodePlane {
        crate::render::codes::CodePlane::build(
            3,
            4,
            // 0 and 1 are the two sentinels; the rest are measurements, so the
            // mip chain this shape produces is not all-sentinel.
            vec![0, 1, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49],
            crate::render::codes::LutKey {
                product: crate::types::RadarProduct::Reflectivity,
                scale: 2.0,
                offset: 66.0,
            },
            8,
        )
        .expect("the fixture plane is inside every cap and its product is exact on R8")
    }

    /// **The polar fixture: a frame whose surface is a code plane.**
    ///
    /// Its `image` is empty, which is the exclusion the encode side asserts —
    /// and the reason the fixture exists at all is that neither of the two
    /// above reaches the head's surface block, so without it a build could
    /// reorder the plane's shape and key fields and no round-trip and no
    /// pinned row would see it. That is exactly the hole
    /// `wire_identity::WIRE_HEIGHT_REPLY_ROWS` was written to close for the
    /// height reply.
    fn a_polar_frame() -> RenderedFrame {
        RenderedFrame {
            image: RasterImage::Bytes(Vec::new()),
            max_range_km: 230.0,
            polar: a_polar_field(),
            nyquist_ms: Some(26.4),
            melting_layer_source: None,
            storm_motion: None,
            codes: Some(a_code_plane()),
        }
    }

    /// Encode `frame` as the frame reply codec does — head via
    /// [`RenderedFrame::write_head`], tails `[polar, codes, image]` — cloning
    /// where the codec moves, because these tests still hold the frame
    /// afterward.
    fn encode_parts(frame: &RenderedFrame) -> (Vec<u8>, Vec<Vec<u8>>) {
        let mut head = Vec::new();
        frame.write_head(&mut head);
        let (codes, image) = frame.clone().into_surface_tails();
        (head, vec![frame.polar.to_tail(), codes, image])
    }

    #[test]
    fn the_frame_survives_its_own_codec_with_and_without_the_optionals() {
        for frame in [a_full_frame(), a_bare_frame(), a_polar_frame()] {
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
            8 + (1 + 8) + (1 + 1) + (1 + 1 + 4 + 4) + 1,
            "the fixture's head moved; re-derive the offsets. The trailing 1 \
             is the surface discriminant, which a raster frame spends alone",
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

        // The tail-count refusals: 0, 1, 2 and 4 around the valid 3.
        for (count, wrong) in [
            (0usize, Vec::new()),
            (1, vec![tails[0].clone()]),
            (2, vec![tails[0].clone(), tails[1].clone()]),
            (
                4,
                vec![
                    tails[0].clone(),
                    tails[1].clone(),
                    tails[2].clone(),
                    Vec::new(),
                ],
            ),
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

        // The surface discriminant is the head's last byte on a raster frame,
        // and only the two named values decode.
        let surface_at = head.len() - 1;
        assert_eq!(
            head[surface_at], SURFACE_RASTER,
            "premise: a frame with no plane names the raster surface",
        );
        for tag in [SURFACE_CODES, 2, u8::MAX] {
            let mut bad = head.clone();
            bad[surface_at] = tag;
            assert_eq!(
                RenderedFrame::from_parts(&bad, tails.clone()),
                None,
                "surface tag {tag} on a head with no plane block was accepted",
            );
        }

        // A raster frame's code tail is empty, and bytes in it are a reply
        // this build did not write rather than a plane to believe.
        let mut smuggled = tails.clone();
        smuggled[1] = vec![7];
        assert_eq!(
            RenderedFrame::from_parts(&head, smuggled),
            None,
            "a raster-surfaced reply carrying codes in its code tail was accepted",
        );
    }

    /// **The polar surface's own refusals** — the head block a raster frame
    /// never writes, and the exclusion between the two surface tails.
    #[test]
    fn a_malformed_polar_frame_reply_is_refused_rather_than_misread() {
        use crate::render::codes::CodePlane;

        // The refusals below are counted, and the ledger is process-global.
        let _ledger = crate::render::codes::hold_refusal_ledger();

        let frame = a_polar_frame();
        let (head, tails) = encode_parts(&frame);

        // Control first: untouched parts decode, so every refusal below is the
        // mutation's doing.
        assert_eq!(
            RenderedFrame::from_parts(&head, tails.clone()),
            Some(frame.clone()),
            "premise: the polar fixture survives its own codec",
        );
        assert_eq!(
            tails[2],
            Vec::<u8>::new(),
            "premise: a plane's frame puts nothing in the image tail",
        );
        assert_eq!(
            tails[1].len(),
            3 * 4,
            "premise: the code tail is level 0 and nothing else",
        );

        // The surface byte, then the plane's own block: the decode form, the
        // shape, the product, and that form's decode — here the affine one.
        let surface_at = head.len() - 1 - CodePlane::WIRE_HEAD_AFFINE_BYTES;
        assert_eq!(
            head[surface_at], SURFACE_CODES,
            "premise: a frame with a plane names the code surface",
        );
        assert_eq!(
            head.len(),
            // A Nyquist and nothing else, so this fixture's absent-optional
            // tags cost a byte each: the head's length is not the raster
            // fixture's, and both are re-derived rather than copied.
            8 + (1 + 8) + 1 + 1 + 1 + CodePlane::WIRE_HEAD_AFFINE_BYTES,
            "the polar fixture's head moved; re-derive the offsets",
        );

        // Truncating anywhere inside the plane block leaves a head no reader
        // can finish.
        for cut in surface_at + 1..head.len() {
            assert_eq!(
                RenderedFrame::from_parts(&head[..cut], tails.clone()),
                None,
                "the polar head truncated to {cut} bytes was accepted",
            );
        }

        // Naming the raster surface leaves the plane block as trailing bytes,
        // and leaves a non-empty code tail beside it.
        let mut as_raster = head.clone();
        as_raster[surface_at] = SURFACE_RASTER;
        assert_eq!(
            RenderedFrame::from_parts(&as_raster, tails.clone()),
            None,
            "a plane's head relabelled as a raster was accepted",
        );

        // A plane's frame with a picture in the image tail claims two
        // surfaces. Refused, not resolved.
        let mut both = tails.clone();
        both[2] = vec![1, 2, 3, 4];
        assert_eq!(
            RenderedFrame::from_parts(&head, both),
            None,
            "a reply claiming both a code plane and a raster was accepted",
        );

        // The plane's own refusals reach `CodePlane::build`, so they land in
        // the producer's counter rather than in one the wire minted. Each
        // mutation is checked to move it by exactly one.
        // The plane block opens with its own decode-form byte, and the shape
        // follows it — so this is `surface_at + 1` for the surface byte and one
        // more for the form.
        let form_at = surface_at + 1;
        let shape_at = form_at + 1;
        let product_at = shape_at + 8;
        assert_eq!(
            head[form_at],
            CodePlane::WIRE_FORM_AFFINE,
            "premise: this fixture's plane carries the wire's own words",
        );
        for (what, doctor) in [
            (
                "a code tail that is not the declared shape",
                Box::new(|_h: &mut Vec<u8>, t: &mut Vec<Vec<u8>>| {
                    t[1].pop().expect("the fixture's code tail is not empty");
                }) as Box<dyn Fn(&mut Vec<u8>, &mut Vec<Vec<u8>>)>,
            ),
            (
                "a radial count past the cap",
                Box::new(|h: &mut Vec<u8>, _t: &mut Vec<Vec<u8>>| {
                    h[shape_at..shape_at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
                }),
            ),
            (
                "a gate count of zero",
                Box::new(|h: &mut Vec<u8>, _t: &mut Vec<Vec<u8>>| {
                    h[shape_at + 4..shape_at + 8].copy_from_slice(&0u32.to_le_bytes());
                }),
            ),
            (
                "a product an R8 plane cannot carry",
                Box::new(|h: &mut Vec<u8>, _t: &mut Vec<Vec<u8>>| {
                    let lossy = crate::types::RadarProduct::NormalizedRotation.wire_code();
                    h[product_at..product_at + 2].copy_from_slice(&lossy.to_le_bytes());
                }),
            ),
        ] {
            let before = CodePlane::refusals();
            let (mut h, mut t) = (head.clone(), tails.clone());
            doctor(&mut h, &mut t);
            assert_eq!(
                RenderedFrame::from_parts(&h, t),
                None,
                "{what} was accepted",
            );
            assert_eq!(
                CodePlane::refusals(),
                before + 1,
                "{what} was refused somewhere other than the producer's own \
                 constructor, so the refusal counter did not see it",
            );
        }

        // A decode form this build does not write is refused BEFORE a plane is
        // attempted -- there is nothing to build one out of -- so like the
        // unknown product below it is not in that counter.
        let before = CodePlane::refusals();
        let mut unknown_form = head.clone();
        unknown_form[form_at] = u8::MAX;
        assert_eq!(
            RenderedFrame::from_parts(&unknown_form, tails.clone()),
            None,
            "an unknown plane decode form was accepted",
        );
        assert_eq!(
            CodePlane::refusals(),
            before,
            "an unreadable head reached the plane constructor",
        );

        // A product code this build does not know is refused BEFORE a plane is
        // attempted -- there is no key to build one with -- so it is the one
        // malformation that is not in that counter. Named rather than left to
        // be discovered as a hole.
        let before = CodePlane::refusals();
        let mut unknown = head.clone();
        unknown[product_at..product_at + 2].copy_from_slice(&u16::MAX.to_le_bytes());
        assert_eq!(
            RenderedFrame::from_parts(&unknown, tails.clone()),
            None,
            "an unknown product wire code was accepted",
        );
        assert_eq!(
            CodePlane::refusals(),
            before,
            "an unreadable head reached the plane constructor",
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
            21,
            "nothing rides between an absent trio's tag and the surface \
             discriminant that closes the head",
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
