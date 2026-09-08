//! The picture a rasterizer hands back, in whichever of the two layouts its
//! producer wrote it in.

use ecolor::Color32;
use squallar_source::job::PixelBuf;

/// One rasterized picture's buffer: RGBA bytes, or the same bytes already in
/// the element type an `egui::ColorImage` holds.
///
/// **The arm exists because a `Vec` cannot change its alignment.** A `Vec` must
/// be handed back to the allocator with the `Layout` it was taken with, so a
/// `Vec<u8>` (align 1) can never become a `Vec<Color32>` (`#[repr(align(4))]`)
/// by move — `bytemuck::allocation::try_cast_vec` refuses on exactly that
/// ground. A consumer that needs pixels and is given bytes has no choice but to
/// allocate a second buffer the size of the picture and copy into it. So the
/// choice is made where the buffer is *born*: a producer that can write pixels
/// writes pixels, and the whole picture reaches the texture upload by move.
///
/// **The arm is provenance, not convention.** Whether the channels are straight
/// or premultiplied is [`RasterizeOutput::alpha`] and nothing else; the two
/// questions are independent, and the funnel's premultiply walks
/// [`Self::as_mut_bytes`] whichever arm holds the picture. A `Pixels` buffer
/// under [`AlphaMode::Straight`] is four bytes per pixel that the premultiply
/// has not reached yet — which is what [`crate::render::rasterize::rasterize_gridded`]
/// produces, and it is the same statement its `Bytes` output used to make.
///
/// [`RasterizeOutput::alpha`]: crate::render::rasterize::RasterizeOutput::alpha
/// [`AlphaMode::Straight`]: crate::render::rasterize::AlphaMode::Straight
#[derive(Clone)]
pub enum RasterBuf {
    /// What tiny-skia's `Pixmap::take()` hands back, and what the wire decodes
    /// to.
    Bytes(Vec<u8>),
    /// Written as pixels by a producer that stores whole 4-byte units — the
    /// gridded rasterizer, whose cells carry their colour as one.
    Pixels(Vec<Color32>),
}

impl RasterBuf {
    /// The picture with no bytes in it, which is what a settled blank holds.
    pub const fn empty() -> Self {
        Self::Bytes(Vec::new())
    }

    /// A picture of `pixels` transparent pixels — a transport's copy
    /// destination, and the picture a blank loop frame is given.
    ///
    /// **The one way a transport can allocate this arm without naming
    /// [`Color32`].** The browser port copies a reply's picture straight into
    /// the byte view of one of these and never depends on `ecolor` to do it, so
    /// the element type stays this type's own business — which is the whole
    /// reason the arm can exist above a crate that does not draw.
    pub fn transparent(pixels: usize) -> Self {
        Self::Pixels(Self::transparent_pixels(pixels))
    }

    /// [`Self::transparent`] as the element type an `egui::ColorImage` holds,
    /// for the caller that hands one straight over and never wants the enum.
    ///
    /// **`zeroed_vec`, never `vec![Color32::TRANSPARENT; n]`** — the same rule
    /// [`rasterize_gridded`](crate::render::rasterize::rasterize_gridded)
    /// already writes its picture under, and this is the other allocation it
    /// governs. A foreign element type has no `IsZero` specialisation, so the
    /// macro spelling takes an uninitialised block and then writes forty
    /// megabytes of zeros over memory the allocator was going to hand over
    /// zeroed; `zeroed_vec` is `alloc_zeroed` and writes none of them.
    /// [`Color32::TRANSPARENT`] is four zero bytes, so the two spell the same
    /// picture — `transparent_is_four_zero_bytes` fails if that ever stops
    /// being true, which is the whole precondition.
    ///
    /// Measured natively at the user's own 150 % rung (41,719,488 B,
    /// 10,429,872 px), medians of 30 iterations over three runs: the macro
    /// spelling 14.076 / 14.597 / 15.127 ms, this one 0.007 / 0.008 /
    /// 0.008 ms. **The cost is removed, not relocated** — a later full read of
    /// the picture is 4.043-4.142 ms against 4.288-4.403 ms, so ~0.25 ms of
    /// the ~14 ms comes back when something touches the pages. At the 125 %
    /// and 100 % rungs the two are indistinguishable (medians within
    /// 0.02 ms): glibc mmaps the top rung fresh and reuses a resident block
    /// for the other two, where `alloc_zeroed` must memset like anyone else.
    ///
    /// **That figure is native glibc and is not the browser's.** wasm has no
    /// mmap and no demand paging, and no arm of this change has been measured
    /// on the Tier-2 rig; what carries here is that the write is dead on every
    /// target and the spelling is never the slower of the two.
    pub fn transparent_pixels(pixels: usize) -> Vec<Color32> {
        bytemuck::zeroed_vec(pixels)
    }

    /// **The wire's picture, born as pixels.** The reply's premultiplied RGBA
    /// becomes the element type the consumer's `ColorImage` holds, in the one
    /// allocation the decode was always going to make — so the picture reaches
    /// the texture upload by move rather than through a second buffer its own
    /// size.
    ///
    /// It has to happen *here*, at the birth of the buffer, and cannot be a
    /// cast afterwards: a `Vec` is freed with the `Layout` it was allocated
    /// with, so `Vec<u8>` (align 1) and `Vec<Color32>` (`#[repr(align(4))]`)
    /// are not interconvertible in either direction, and
    /// `bytemuck::allocation::try_cast_vec` refuses on exactly that ground.
    ///
    /// **Premultiplied is the wire's contract, not an assumption**: the funnel
    /// premultiplies in its output stage before `encode_out`, and
    /// `Color32::from_rgba_premultiplied` computes nothing — the four bytes
    /// are stored as they arrived, which is what keeps the picture identical
    /// to the `Bytes` arm this replaced.
    ///
    /// A length that is not whole pixels stays [`Bytes`](Self::Bytes). The
    /// reply codec hands its tail back **unjudged** — only the dispatch knows
    /// the dimensions it must match — and materializing whole pixels out of a
    /// buffer that has none would silently drop the remainder that is the
    /// evidence of the mismatch.
    pub fn from_premultiplied_wire(rgba: &[u8]) -> Self {
        if !rgba.len().is_multiple_of(4) {
            return Self::Bytes(rgba.to_vec());
        }
        Self::Pixels(
            rgba.chunks_exact(4)
                .map(|px| Color32::from_rgba_premultiplied(px[0], px[1], px[2], px[3]))
                .collect(),
        )
    }

    /// The bytes, borrowed — the read every consumer but the texture upload
    /// makes.
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            Self::Bytes(bytes) => bytes,
            Self::Pixels(pixels) => bytemuck::cast_slice(pixels),
        }
    }

    /// The bytes, borrowed mutably: what the funnel's premultiply rewrites in
    /// place. A byte view of a `Pixels` buffer is sound in the direction that
    /// matters — 4-align to 1-align is a weakening, and no reallocation
    /// crosses it.
    pub fn as_mut_bytes(&mut self) -> &mut [u8] {
        match self {
            Self::Bytes(bytes) => bytes,
            Self::Pixels(pixels) => bytemuck::cast_slice_mut(pixels),
        }
    }

    /// **The wire's carrier, and back, both by move.** `squallar_source`'s
    /// [`PixelBuf`] is a `Vec<u32>`: the same size and the same alignment as
    /// `Color32`, so `bytemuck` re-labels the allocation instead of copying it.
    /// That is what lets the funnel carry a picture without naming a colour
    /// type, and it is proved rather than asserted — see
    /// `a_wire_round_trip_keeps_the_same_allocation` in this module's tests.
    ///
    /// The `expect` cannot fire: it needs `Color32` to stop being four bytes at
    /// four-byte alignment, which that test fails on first. A fallback copy
    /// here would be worse than a panic — it would silently reintroduce the
    /// second full-size buffer this whole path exists to remove.
    pub fn into_wire(self) -> PixelBuf {
        PixelBuf::from_words(
            bytemuck::allocation::try_cast_vec(self.into_pixels())
                .expect("Color32 and u32 are both 4 bytes at 4-byte alignment"),
        )
    }

    /// [`Self::into_wire`] reversed, on the same terms.
    pub fn from_wire(wire: PixelBuf) -> Self {
        Self::Pixels(
            bytemuck::allocation::try_cast_vec(wire.into_words())
                .expect("Color32 and u32 are both 4 bytes at 4-byte alignment"),
        )
    }

    /// **The move this type exists for.** A `Pixels` buffer becomes the
    /// `ColorImage`'s own storage with no allocation and no copy; a `Bytes` one
    /// pays the copy it always paid.
    pub fn into_pixels(self) -> Vec<Color32> {
        match self {
            Self::Pixels(pixels) => pixels,
            Self::Bytes(bytes) => bytes
                .chunks_exact(4)
                .map(|px| Color32::from_rgba_premultiplied(px[0], px[1], px[2], px[3]))
                .collect(),
        }
    }

    /// The bytes, owned — the encode side. A `Pixels` arm pays a copy here,
    /// which the shipped wire path never does: the encoder borrows
    /// ([`Self::as_bytes`]), and nothing else asks.
    pub fn into_bytes(self) -> Vec<u8> {
        match self {
            Self::Bytes(bytes) => bytes,
            Self::Pixels(pixels) => bytemuck::cast_slice(&pixels).to_vec(),
        }
    }
}

impl std::ops::Deref for RasterBuf {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        self.as_bytes()
    }
}

impl std::ops::DerefMut for RasterBuf {
    fn deref_mut(&mut self) -> &mut [u8] {
        self.as_mut_bytes()
    }
}

impl From<Vec<u8>> for RasterBuf {
    fn from(bytes: Vec<u8>) -> Self {
        Self::Bytes(bytes)
    }
}

impl From<Vec<Color32>> for RasterBuf {
    fn from(pixels: Vec<Color32>) -> Self {
        Self::Pixels(pixels)
    }
}

/// Content equality, so the same picture in the two layouts is the same
/// picture — which is what keeps a codec round trip a round trip.
impl PartialEq for RasterBuf {
    fn eq(&self, other: &Self) -> bool {
        self.as_bytes() == other.as_bytes()
    }
}

impl PartialEq<Vec<u8>> for RasterBuf {
    fn eq(&self, other: &Vec<u8>) -> bool {
        self.as_bytes() == other.as_slice()
    }
}

impl PartialEq<RasterBuf> for Vec<u8> {
    fn eq(&self, other: &RasterBuf) -> bool {
        self.as_slice() == other.as_bytes()
    }
}

impl PartialEq<[u8]> for RasterBuf {
    fn eq(&self, other: &[u8]) -> bool {
        self.as_bytes() == other
    }
}

/// The bytes, exactly as a `Vec<u8>` prints them: an assertion that fails on a
/// picture shows what it always showed.
impl std::fmt::Debug for RasterBuf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self.as_bytes(), f)
    }
}

#[cfg(test)]
mod tests;
