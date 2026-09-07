//! Render `packaging/icon/squallar.svg` into every form a platform asks for.
//!
//! One SVG is the definitive drawing. Nothing derived from it is committed:
//! `squallar/build.rs` calls this for the window icon and the Windows `.ico`,
//! and the packaging steps call the binary for the rest.
//!
//! The container formats here — ICO and ICNS in this file, Apple's compiled
//! asset catalog in `squallar-car` — are written directly rather than handed to a
//! converter. That is not enthusiasm for byte-twiddling: the converter this
//! replaced silently wrote a **one-image** `.icns` out of ten inputs, which
//! would have left Finder a single size to scale everything from, and the
//! catalog's compiler ships only inside Xcode. ICO and ICNS are a header and a
//! directory in front of PNG payloads, so owning them costs less than checking
//! whether a tool got them right, and the two rules that actually bite are
//! pinned by the tests at the bottom of this file.

use resvg::tiny_skia::{Pixmap, Transform};
use resvg::usvg::{Options, Tree};

/// A parsed icon, ready to render at any size.
pub struct Icon {
    tree: Tree,
}

impl Icon {
    /// Parse the SVG source.
    pub fn parse(svg: &[u8]) -> Result<Self, String> {
        Tree::from_data(svg, &Options::default())
            .map(|tree| Self { tree })
            .map_err(|e| format!("parsing the icon SVG: {e}"))
    }

    /// Render square at `px`, on the drawing's own opaque ground.
    ///
    /// Nothing composites a background here because the SVG is full bleed: the
    /// first thing it paints is an opaque rect over the whole viewBox. That is
    /// a property the drawing has to keep, and `renders_fully_opaque` below is
    /// what stops it quietly losing it — iOS refuses an app icon that has an
    /// alpha channel at all.
    pub fn render(&self, px: u32) -> Pixmap {
        let mut pixmap = Pixmap::new(px, px).expect("icon size is never zero");
        let scale = px as f32 / self.tree.size().width();
        resvg::render(
            &self.tree,
            Transform::from_scale(scale, scale),
            &mut pixmap.as_mut(),
        );
        pixmap
    }

    /// Render square at `px` and encode as an RGB PNG, with **no alpha
    /// channel at all**.
    ///
    /// Not merely opaque: Apple refuses an app icon whose PNG *has* an alpha
    /// channel, however fully opaque every pixel in it is. tiny-skia only
    /// writes RGBA, so the channel is dropped here rather than trusted to a
    /// later step, and `png_has_no_alpha_channel` pins it.
    ///
    /// The drawing is full bleed and opaque, so dropping the channel discards
    /// nothing; `renders_fully_opaque` is what keeps that true.
    pub fn png(&self, px: u32) -> Vec<u8> {
        let pixmap = self.render(px);
        // Demultiply first: tiny-skia stores premultiplied pixels, and at
        // alpha 255 that is a no-op, but reading the raw buffer as if it were
        // straight colour is the kind of thing that is right by accident.
        let rgb: Vec<u8> = pixmap
            .pixels()
            .iter()
            .flat_map(|p| {
                let c = p.demultiply();
                [c.red(), c.green(), c.blue()]
            })
            .collect();

        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, px, px);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().expect("writing the png header");
            writer.write_image_data(&rgb).expect("writing png pixels");
        }
        out
    }

    /// Render at `px` as raw RGBA8, the layout `winit::window::Icon::from_rgba`
    /// takes. Raw rather than PNG so the runtime needs no decoder.
    pub fn rgba(&self, px: u32) -> Vec<u8> {
        self.render(px).data().to_vec()
    }

    /// Render at `px` as raw BGRA8, straight colour, rows top-down: the pixel
    /// layout an asset catalog's `ARGB` rendition holds (see `squallar_car`).
    pub fn bgra(&self, px: u32) -> Vec<u8> {
        self.render(px)
            .pixels()
            .iter()
            .flat_map(|p| {
                let c = p.demultiply();
                [c.blue(), c.green(), c.red(), c.alpha()]
            })
            .collect()
    }

    /// Render into a `px`-square canvas with the drawing inset to `fraction` of
    /// the edge, the padding left TRANSPARENT.
    ///
    /// This is what Android's adaptive foreground needs: the layer is 108dp but
    /// only the middle 72dp is guaranteed visible, and every launcher masks it
    /// to a different shape. Scaling the drawing down inside a full-size canvas
    /// keeps the subject inside that safe zone; cropping to it would cut the
    /// scope's rim off.
    ///
    /// The padding is transparent rather than filled with the ground colour,
    /// which is the one place in this crate where an alpha channel is wanted.
    /// An adaptive icon is two layers the launcher masks and parallaxes
    /// independently: an opaque foreground hides the background layer entirely
    /// and the parallax has nothing to move against. Filled with the ground it
    /// would look identical standing still and wrong in motion.
    pub fn inset(&self, px: u32, fraction: f32) -> Pixmap {
        let inner = ((px as f32 * fraction).round() as u32).max(1);
        let mut canvas = Pixmap::new(px, px).expect("icon size is never zero");
        let drawn = self.render(inner);
        let offset = ((px - inner) / 2) as i32;
        canvas.draw_pixmap(
            offset,
            offset,
            drawn.as_ref(),
            &resvg::tiny_skia::PixmapPaint::default(),
            Transform::identity(),
            None,
        );
        canvas
    }
}

/// The sizes a Windows `.ico` carries, and what each is for.
pub const ICO_SIZES: [u32; 6] = [16, 24, 32, 48, 128, 256];

/// Assemble a Windows `.ico` around already-encoded PNGs.
///
/// Header (reserved, type, count), one 16-byte directory entry per image, then
/// the payloads. **A 256-pixel edge is stored as 0**, because the field is one
/// byte and 256 does not fit in it; writing it literally truncates to 0 by
/// accident on some encoders and produces a file Explorer rejects on others.
/// That rule is the reason this function is tested.
pub fn ico(images: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_le_bytes()); // reserved
    out.extend_from_slice(&1u16.to_le_bytes()); // 1 = icon, 2 = cursor
    out.extend_from_slice(&(images.len() as u16).to_le_bytes());

    let mut offset = 6 + 16 * images.len() as u32;
    for (size, png) in images {
        let edge = if *size >= 256 { 0u8 } else { *size as u8 };
        out.push(edge); // width
        out.push(edge); // height
        out.push(0); // palette entries: 0 for true colour
        out.push(0); // reserved
        out.extend_from_slice(&1u16.to_le_bytes()); // colour planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        out.extend_from_slice(&(png.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += png.len() as u32;
    }
    for (_, png) in images {
        out.extend_from_slice(png);
    }
    out
}

/// The canonical macOS icon set, as `iconutil` lays it out: each Finder size at
/// 1x and 2x, paired with the four-character type the container names it by.
pub const ICNS_MEMBERS: [(u32, &[u8; 4]); 10] = [
    (16, b"icp4"),   // 16x16
    (32, b"ic11"),   // 16x16@2x
    (32, b"icp5"),   // 32x32
    (64, b"ic12"),   // 32x32@2x
    (128, b"ic07"),  // 128x128
    (256, b"ic13"),  // 128x128@2x
    (256, b"ic08"),  // 256x256
    (512, b"ic14"),  // 256x256@2x
    (512, b"ic09"),  // 512x512
    (1024, b"ic10"), // 512x512@2x
];

/// Assemble a macOS `.icns` around already-encoded PNGs.
///
/// `"icns"`, the total length, then per member: the type, the member's length,
/// and the PNG. **Both length fields count their own header**, which is the
/// detail that produces a file every reader rejects if it is written as the
/// payload length alone.
pub fn icns(members: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
    let mut body = Vec::new();
    for (ostype, png) in members {
        body.extend_from_slice(*ostype);
        body.extend_from_slice(&((png.len() + 8) as u32).to_be_bytes());
        body.extend_from_slice(png);
    }
    let mut out = Vec::with_capacity(body.len() + 8);
    out.extend_from_slice(b"icns");
    out.extend_from_slice(&((body.len() + 8) as u32).to_be_bytes());
    out.extend_from_slice(&body);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SVG: &[u8] = include_bytes!("../../packaging/icon/squallar.svg");

    fn icon() -> Icon {
        Icon::parse(SVG).expect("the committed icon SVG must parse")
    }

    /// The one property iOS enforces and no other target does: an app icon with
    /// an alpha channel is refused outright, so the drawing has to be opaque
    /// corner to corner rather than merely look it.
    #[test]
    fn renders_fully_opaque() {
        for px in [16, 180, 1024] {
            let pixmap = icon().render(px);
            let clear = pixmap.pixels().iter().filter(|p| p.alpha() != 255).count();
            assert_eq!(
                clear, 0,
                "{px}px render has {clear} pixels that are not fully opaque; \
                 the SVG has stopped covering its whole viewBox and iOS \
                 rejects an app icon with an alpha channel"
            );
        }
    }

    /// Apple refuses an app icon whose PNG carries an alpha channel, and it
    /// refuses it for HAVING the channel, not for using it. `renders_fully_opaque`
    /// above asserts every pixel is opaque, which is a different claim and the
    /// one that reads green while the upload is rejected — the encoder wrote
    /// RGBA with every alpha at 255 and that is still an alpha channel.
    ///
    /// Colour type 2 is RGB; 6 is RGBA. Read out of the IHDR rather than out of
    /// the encoder's configuration, so it is the file that is checked.
    #[test]
    fn png_has_no_alpha_channel() {
        for px in [16, 120, 152, 1024] {
            let bytes = icon().png(px);
            assert_eq!(&bytes[..4], b"\x89PNG", "{px}px output is not a PNG");
            assert_eq!(
                &bytes[12..16],
                b"IHDR",
                "{px}px PNG does not start with IHDR"
            );
            let colour_type = bytes[25];
            assert_eq!(
                colour_type, 2,
                "the {px}px PNG has colour type {colour_type} (6 = RGBA); an \
                 app icon must have no alpha channel at all, and Apple rejects \
                 it for carrying one even when every pixel is opaque"
            );
        }
    }

    /// Every raster is square at the size asked for. A non-square viewBox would
    /// letterbox instead, which reads as "the icon has margins" rather than as
    /// a defect.
    #[test]
    fn renders_square_at_the_requested_size() {
        for px in [16, 57, 1024] {
            let pixmap = icon().render(px);
            assert_eq!((pixmap.width(), pixmap.height()), (px, px));
        }
    }

    /// 256 is stored as 0 in a one-byte field. Getting this wrong is the
    /// classic corrupt-.ico bug and it is invisible until Explorer renders
    /// nothing, so it is pinned rather than trusted.
    #[test]
    fn ico_stores_256_as_zero_and_smaller_sizes_literally() {
        let images: Vec<(u32, Vec<u8>)> = vec![(16, vec![0xAA; 4]), (256, vec![0xBB; 6])];
        let out = ico(&images);

        assert_eq!(&out[0..2], &0u16.to_le_bytes(), "reserved must be zero");
        assert_eq!(&out[2..4], &1u16.to_le_bytes(), "type must be 1 (icon)");
        assert_eq!(&out[4..6], &2u16.to_le_bytes(), "count must be 2");

        assert_eq!(out[6], 16, "a 16px edge is stored literally");
        assert_eq!(out[6 + 16], 0, "a 256px edge is stored as 0, not 256");
    }

    /// Every directory entry has to point at its own payload. An offset that is
    /// right for the first image and wrong for the rest is the shape this
    /// catches, and it is why the assertion walks every entry rather than
    /// checking the header.
    #[test]
    fn ico_offsets_address_the_payload_they_name() {
        let images: Vec<(u32, Vec<u8>)> = ICO_SIZES
            .iter()
            .enumerate()
            .map(|(i, &s)| (s, vec![i as u8; 10 + i]))
            .collect();
        let out = ico(&images);

        for (i, (_, png)) in images.iter().enumerate() {
            let entry = 6 + 16 * i;
            let len = u32::from_le_bytes(out[entry + 8..entry + 12].try_into().unwrap()) as usize;
            let off = u32::from_le_bytes(out[entry + 12..entry + 16].try_into().unwrap()) as usize;
            assert_eq!(len, png.len(), "entry {i} declares the wrong length");
            assert_eq!(
                &out[off..off + len],
                png.as_slice(),
                "entry {i} offset {off} does not address its own payload"
            );
        }
        assert_eq!(
            out.len(),
            6 + 16 * images.len() + images.iter().map(|(_, p)| p.len()).sum::<usize>(),
            "the file has bytes in it that no directory entry names"
        );
    }

    /// Both ICNS length fields count their own 8-byte header. Writing the
    /// payload length alone produces a file every reader refuses.
    #[test]
    fn icns_lengths_include_their_own_header() {
        let members: Vec<(&[u8; 4], Vec<u8>)> =
            vec![(b"ic07", vec![1; 20]), (b"ic08", vec![2; 30])];
        let out = icns(&members);

        assert_eq!(&out[0..4], b"icns");
        let total = u32::from_be_bytes(out[4..8].try_into().unwrap()) as usize;
        assert_eq!(
            total,
            out.len(),
            "declared total length must be the file length"
        );

        let mut i = 8;
        for (ostype, png) in &members {
            assert_eq!(&out[i..i + 4], ostype.as_slice());
            let len = u32::from_be_bytes(out[i + 4..i + 8].try_into().unwrap()) as usize;
            assert_eq!(
                len,
                png.len() + 8,
                "member length must count its own header"
            );
            assert_eq!(&out[i + 8..i + len], png.as_slice());
            i += len;
        }
        assert_eq!(i, out.len(), "the container has trailing bytes");
    }

    /// The real `.icns`, assembled the way the binary assembles it, parsed back
    /// by something that is not the writer. This is the assertion that would
    /// have caught the converter writing one image out of ten.
    #[test]
    fn the_real_icns_carries_every_declared_member() {
        let icon = icon();
        let members: Vec<(&[u8; 4], Vec<u8>)> = ICNS_MEMBERS
            .iter()
            .map(|(px, ostype)| (*ostype, icon.png(*px)))
            .collect();
        let out = icns(&members);

        let mut seen = Vec::new();
        let mut i = 8;
        while i < out.len() {
            let ostype: [u8; 4] = out[i..i + 4].try_into().unwrap();
            let len = u32::from_be_bytes(out[i + 4..i + 8].try_into().unwrap()) as usize;
            let png = &out[i + 8..i + len];
            assert_eq!(&png[..4], b"\x89PNG", "{ostype:?} payload is not a PNG");
            let w = u32::from_be_bytes(png[16..20].try_into().unwrap());
            let h = u32::from_be_bytes(png[20..24].try_into().unwrap());
            seen.push((w, ostype));
            assert_eq!(w, h, "{ostype:?} payload is not square");
            i += len;
        }
        assert_eq!(
            seen.len(),
            ICNS_MEMBERS.len(),
            "the container holds {} members, not the {} declared",
            seen.len(),
            ICNS_MEMBERS.len()
        );
        for ((want_px, want_type), (got_px, got_type)) in ICNS_MEMBERS.iter().zip(&seen) {
            assert_eq!((*want_px, *want_type), (*got_px, got_type));
        }
    }

    /// The adaptive foreground keeps its subject inside Android's safe zone,
    /// stays centred, and leaves the padding transparent so the background
    /// layer under it is not hidden.
    ///
    /// The centring is asserted because the first version of this pipeline
    /// pushed the drawing into the bottom-right corner and nothing but a person
    /// looking at it noticed.
    #[test]
    fn inset_centres_the_drawing_and_leaves_the_padding_transparent() {
        let px = 432;
        let pixmap = icon().inset(px, 72.0 / 108.0);

        let corner = pixmap.pixel(0, 0).unwrap();
        assert_eq!(
            corner.alpha(),
            0,
            "the padding is opaque, so it covers the adaptive background layer \
             and the launcher has nothing to parallax the foreground against"
        );

        let differs = |x: u32, y: u32| pixmap.pixel(x, y).unwrap().alpha() != 0;
        let (mut min_x, mut max_x) = (px, 0);
        for y in (0..px).step_by(2) {
            for x in (0..px).step_by(2) {
                if differs(x, y) {
                    min_x = min_x.min(x);
                    max_x = max_x.max(x);
                }
            }
        }
        let left = min_x;
        let right = px - 1 - max_x;
        assert!(
            left.abs_diff(right) <= 2,
            "the drawing is not centred: {left}px of ground on the left and \
             {right}px on the right"
        );
        assert!(
            left > 0,
            "the drawing reaches the canvas edge, so it is not inset into the \
             safe zone at all"
        );
    }
}
